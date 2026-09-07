use super::connected::{api_delete, api_get, api_post, ConnectedArgo};
use super::scope::{acquire_connection_lease, scoped_connection, ConnectionLease};
use super::session::{consume, issue, peek, OperationSession, SessionSnapshot};
use crate::commands::gitops_crd::{client_for_context, find_api_resource};
use crate::models::AppErrorKind;
use crate::models::{
    AppError, ArgoApplicationRef, ArgoOperationConfirmation, ArgoOperationPreflight,
    ArgoOperationRequest, ArgoOperationResult,
};
use k8s_openapi::api::authorization::v1::{
    ResourceAttributes, SelfSubjectAccessReview, SelfSubjectAccessReviewSpec,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::{
    api::{Api, ApiResource, Patch, PatchParams, PostParams},
    core::DynamicObject,
    Client,
};
use serde_json::{json, Value};
use std::sync::Arc;

/// Kubernetes-transport fallback only supports a subset of the connected
/// actions; enforced at preflight and again at execution so a stale or
/// forged session can never reach the patch builder.
const KUBERNETES_TRANSPORT_ACTIONS: [&str; 4] = ["refresh", "hardRefresh", "sync", "retry"];

fn ensure_kubernetes_transport_action(action: &str) -> Result<(), AppError> {
    if KUBERNETES_TRANSPORT_ACTIONS.contains(&action) {
        Ok(())
    } else {
        Err(AppError::new(
            "operation unavailable in Kubernetes fallback",
            AppErrorKind::ArgoOperationUnavailable,
        ))
    }
}

fn valid(request: &ArgoOperationRequest) -> Result<(), AppError> {
    if request.transport == "kubernetes" {
        crate::commands::helpers::validate_namespace(request.application.namespace.as_deref())?;
        crate::commands::helpers::validate_path_segment(
            &request.application.name,
            "Application name",
        )?;
    }
    if !matches!(request.transport.as_str(), "connected" | "kubernetes") {
        return Err(AppError::new(
            "invalid Argo CD transport",
            AppErrorKind::ArgoOperationUnavailable,
        ));
    }
    if !matches!(
        request.action.as_str(),
        "refresh" | "hardRefresh" | "sync" | "retry" | "rollback" | "terminate" | "resourceAction"
    ) {
        return Err(AppError::new(
            "operation is not allowlisted",
            AppErrorKind::ArgoOperationUnavailable,
        ));
    }
    if request.application.name.trim().is_empty() {
        return Err(AppError::new(
            "application name required",
            AppErrorKind::ArgoOperationUnavailable,
        ));
    }
    if request.action == "rollback" && request.history_id.is_none() {
        return Err(AppError::new(
            "history ID required for rollback",
            AppErrorKind::ArgoOperationUnavailable,
        ));
    }
    if request.action == "resourceAction" {
        let resource = request.resources.first();
        if request.resource_action.as_deref().is_none_or(str::is_empty)
            || request.resources.len() != 1
            || resource.is_none_or(|resource| {
                [
                    resource.version.as_deref(),
                    resource.kind.as_deref(),
                    resource.name.as_deref(),
                ]
                .into_iter()
                .any(|value| value.is_none_or(|value| value.trim().is_empty()))
            })
        {
            return Err(AppError::new(
                "one server-reported resource action and complete resource identity required",
                AppErrorKind::ArgoOperationUnavailable,
            ));
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn preflight_argo_operation(
    store: tauri::State<'_, super::ArgoConnectionStore>,
    request: ArgoOperationRequest,
) -> Result<ArgoOperationPreflight, AppError> {
    valid(&request)?;
    if request.transport == "kubernetes" {
        ensure_kubernetes_transport_action(&request.action)?;
        fallback_allowed(&request).await?;
    }
    let (resolved_request, connection_generation, connection_instance_id) =
        resolve_request(&store, &request).await?;
    if resolved_request.transport == "connected" && resolved_request.action == "resourceAction" {
        let connection = scoped_connection(
            &store,
            resolved_request.connection_id.as_deref().ok_or_else(|| {
                AppError::new("Argo CD connection required", AppErrorKind::ArgoConnection)
            })?,
            resolved_request.cluster_context.as_deref().ok_or_else(|| {
                AppError::new(
                    "clusterContext required",
                    AppErrorKind::ArgoOperationUnavailable,
                )
            })?,
            resolved_request.application.workspace_id.as_deref(),
            resolved_request.kubeconfig_env_var.as_deref(),
        )?;
        validate_resource_action(&connection, &resolved_request).await?;
    }
    let (session_id, session) = issue(
        &store.sessions,
        resolved_request,
        connection_generation,
        connection_instance_id,
    )?;
    Ok(ArgoOperationPreflight {
        allowed: true,
        transport: session.transport,
        action: session.request.action.clone(),
        reason: None,
        session_id: Some(session_id),
        expires_at: Some(session.expires_at),
        reviewed_request: Some(session.request),
    })
}

async fn fallback_allowed(request: &ArgoOperationRequest) -> Result<(), AppError> {
    let context = request
        .cluster_context
        .as_deref()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| {
            AppError::new(
                "clusterContext required for Kubernetes fallback",
                AppErrorKind::ArgoOperationUnavailable,
            )
        })?;
    let namespace = request
        .application
        .namespace
        .as_deref()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| {
            AppError::new(
                "application namespace required for Kubernetes fallback",
                AppErrorKind::ArgoOperationUnavailable,
            )
        })?;
    if request
        .resource_version
        .as_deref()
        .is_none_or(str::is_empty)
    {
        return Err(AppError::new(
            "resourceVersion required for Kubernetes fallback",
            AppErrorKind::ArgoOperationUnavailable,
        ));
    }
    let client = client_for_context(context, request.kubeconfig_env_var.clone()).await?;
    let review = SelfSubjectAccessReview {
        metadata: ObjectMeta::default(),
        spec: SelfSubjectAccessReviewSpec {
            resource_attributes: Some(ResourceAttributes {
                group: Some("argoproj.io".into()),
                resource: Some("applications".into()),
                verb: Some("patch".into()),
                namespace: Some(namespace.into()),
                name: Some(request.application.name.clone()),
                version: None,
                subresource: None,
                field_selector: None,
                label_selector: None,
            }),
            non_resource_attributes: None,
        },
        status: None,
    };
    let status = Api::<SelfSubjectAccessReview>::all(client)
        .create(&PostParams::default(), &review)
        .await?
        .status;
    if !status.is_some_and(|value| value.allowed && !value.denied.unwrap_or(false)) {
        return Err(AppError::new(
            "operation unavailable or not authorized",
            AppErrorKind::ArgoOperationUnavailable,
        ));
    }
    Ok(())
}

enum ExecutionBinding {
    Connected(Arc<ConnectedArgo>),
    Kubernetes {
        client: Client,
        resource: ApiResource,
    },
}

async fn lease_connection(
    store: &super::ArgoConnectionStore,
    session: &OperationSession,
    connection: Arc<ConnectedArgo>,
) -> Result<ConnectionLease, AppError> {
    let connection_id = session.connection_id.as_deref().ok_or_else(|| {
        AppError::new("Argo CD connection required", AppErrorKind::ArgoConnection)
    })?;
    let cluster_context = session.request.cluster_context.as_deref().ok_or_else(|| {
        AppError::new(
            "clusterContext required",
            AppErrorKind::ArgoOperationUnavailable,
        )
    })?;
    let generation = session.connection_generation.as_deref().ok_or_else(|| {
        AppError::new(
            "Argo CD connection was replaced",
            AppErrorKind::ArgoOperationUnavailable,
        )
    })?;
    let instance_id = session.connection_instance_id.as_deref().ok_or_else(|| {
        AppError::new(
            "Argo CD connection was replaced",
            AppErrorKind::ArgoOperationUnavailable,
        )
    })?;
    acquire_connection_lease(
        store,
        connection_id,
        connection,
        cluster_context,
        session.workspace_id.as_deref(),
        session.request.kubeconfig_env_var.as_deref(),
        generation,
        instance_id,
    )
    .await
}

fn reject_reviewed_operation(
    store: &super::session::SessionStore,
    id: &str,
    reviewed: &SessionSnapshot,
    error: AppError,
) -> AppError {
    let _ = consume(store, id, reviewed);
    error
}

async fn run_connected_operation(
    store: &super::ArgoConnectionStore,
    id: &str,
    reviewed: &SessionSnapshot,
    connection: Arc<ConnectedArgo>,
) -> Result<ArgoOperationResult, AppError> {
    let lease = lease_connection(store, &reviewed.session, connection)
        .await
        .map_err(|error| reject_reviewed_operation(&store.sessions, id, reviewed, error))?;
    let request = consume(&store.sessions, id, reviewed)?.request;
    let path = match request.action.as_str() {
        "refresh" => application_path(&request.application, "", Some("normal")),
        "hardRefresh" => application_path(&request.application, "", Some("hard")),
        "sync" | "retry" => application_path(&request.application, "/sync", None),
        "rollback" => application_path(&request.application, "/rollback", None),
        "terminate" => application_path(&request.application, "/operation", None),
        "resourceAction" => application_path(&request.application, "/resource/actions/v2", None),
        _ => {
            return Err(AppError::new(
                "operation is not allowlisted",
                AppErrorKind::ArgoOperationUnavailable,
            ))
        }
    };
    let value = if request.action == "refresh" || request.action == "hardRefresh" {
        api_get(&lease.connection, &path).await?
    } else if request.action == "terminate" {
        api_delete(&lease.connection, &path).await?
    } else {
        api_post(&lease.connection, &path, connected_payload(&request)).await?
    };
    accepted("connected", value)
}

async fn run_argo_operation_for_store(
    store: &super::ArgoConnectionStore,
    confirmation: ArgoOperationConfirmation,
) -> Result<ArgoOperationResult, AppError> {
    if confirmation.confirmation != confirmation.session_id {
        return Err(AppError::new(
            "operation confirmation does not match reviewed session",
            AppErrorKind::ArgoOperationUnavailable,
        ));
    }
    let reviewed = peek(&store.sessions, &confirmation.session_id, None)?;
    valid(&reviewed.session.request)?;
    let binding = revalidate_session(store, &reviewed.session)
        .await
        .map_err(|error| {
            reject_reviewed_operation(&store.sessions, &confirmation.session_id, &reviewed, error)
        })?;
    match binding {
        ExecutionBinding::Kubernetes { client, resource } => {
            let request = consume(&store.sessions, &confirmation.session_id, &reviewed)?.request;
            kubernetes_operation(request, client, resource).await
        }
        ExecutionBinding::Connected(connection) => {
            run_connected_operation(store, &confirmation.session_id, &reviewed, connection).await
        }
    }
}

#[tauri::command]
pub async fn run_argo_operation(
    store: tauri::State<'_, super::ArgoConnectionStore>,
    confirmation: ArgoOperationConfirmation,
) -> Result<ArgoOperationResult, AppError> {
    run_argo_operation_for_store(&store, confirmation).await
}

async fn revalidate_session(
    store: &super::ArgoConnectionStore,
    session: &OperationSession,
) -> Result<ExecutionBinding, AppError> {
    let request = &session.request;
    if request.transport == "connected" {
        let connection = scoped_connection(
            store,
            request.connection_id.as_deref().ok_or_else(|| {
                AppError::new("Argo CD connection required", AppErrorKind::ArgoConnection)
            })?,
            request.cluster_context.as_deref().ok_or_else(|| {
                AppError::new(
                    "clusterContext required",
                    AppErrorKind::ArgoOperationUnavailable,
                )
            })?,
            session.workspace_id.as_deref(),
            request.kubeconfig_env_var.as_deref(),
        )?;
        let connection_id = request.connection_id.as_deref().ok_or_else(|| {
            AppError::new("Argo CD connection required", AppErrorKind::ArgoConnection)
        })?;
        if session.connection_id.as_deref() != Some(connection_id)
            || session.connection_generation.as_deref() != Some(connection.generation.as_str())
            || session.connection_instance_id.as_deref() != Some(connection.instance_id.as_str())
        {
            return Err(AppError::new(
                "Argo CD connection was replaced",
                AppErrorKind::ArgoOperationUnavailable,
            ));
        }
        let current = api_get(
            &connection,
            &application_path(&session.application, "", None),
        )
        .await?;
        verify_application_identity(&current, &session.application)?;
        if request.action == "resourceAction" {
            validate_resource_action(&connection, request).await?;
        }
        return Ok(ExecutionBinding::Connected(connection));
    }
    fallback_allowed(request).await?;
    let context = request.cluster_context.as_deref().expect("validated");
    let client = client_for_context(context, request.kubeconfig_env_var.clone()).await?;
    let resource = find_api_resource(&client, "argoproj.io", "Application")
        .await?
        .ok_or_else(|| AppError::new("Application CRD not found", AppErrorKind::Cluster))?;
    let current = Api::<DynamicObject>::namespaced_with(
        client.clone(),
        request.application.namespace.as_deref().expect("validated"),
        &resource,
    )
    .get(&request.application.name)
    .await?;
    verify_kubernetes_application_identity(&current, &session.application)?;
    Ok(ExecutionBinding::Kubernetes { client, resource })
}

fn verify_kubernetes_application_identity(
    current: &DynamicObject,
    expected: &ArgoApplicationRef,
) -> Result<(), AppError> {
    let value = serde_json::to_value(current).map_err(|error| {
        AppError::new(
            "could not serialize Application identity",
            AppErrorKind::Serialization,
        )
        .with_source(error)
    })?;
    verify_application_identity(&value, expected)
}

fn verify_application_identity(
    value: &Value,
    expected: &ArgoApplicationRef,
) -> Result<(), AppError> {
    let metadata = value.get("metadata").unwrap_or(value);
    let actual = [
        (
            "name",
            metadata.get("name").and_then(Value::as_str),
            Some(expected.name.as_str()),
        ),
        (
            "namespace",
            metadata.get("namespace").and_then(Value::as_str),
            expected.namespace.as_deref(),
        ),
        (
            "project",
            value.pointer("/spec/project").and_then(Value::as_str),
            expected.project.as_deref(),
        ),
        (
            "uid",
            metadata.get("uid").and_then(Value::as_str),
            expected.uid.as_deref(),
        ),
        (
            "resourceVersion",
            metadata.get("resourceVersion").and_then(Value::as_str),
            expected.resource_version.as_deref(),
        ),
    ];
    if actual
        .into_iter()
        .any(|(_, current, expected)| expected.is_some_and(|expected| current != Some(expected)))
    {
        return Err(AppError::new(
            "Application changed since operation review",
            AppErrorKind::ArgoOperationUnavailable,
        ));
    }
    Ok(())
}

fn accepted(transport: &str, operation: Value) -> Result<ArgoOperationResult, AppError> {
    Ok(ArgoOperationResult {
        accepted: true,
        transport: transport.into(),
        message: "Argo CD accepted operation".into(),
        operation: Some(operation),
    })
}
fn application_path(app: &ArgoApplicationRef, suffix: &str, refresh: Option<&str>) -> String {
    let mut url =
        reqwest::Url::parse("https://argo.invalid/api/v1/applications").expect("static URL");
    url.path_segments_mut().expect("static URL").push(&app.name);
    url.set_path(&format!("{}{}", url.path(), suffix));
    let mut query = url.query_pairs_mut();
    if let Some(v) = app.namespace.as_deref() {
        query.append_pair("appNamespace", v);
    }
    if let Some(v) = app.project.as_deref() {
        query.append_pair("project", v);
    }
    if let Some(v) = refresh {
        query.append_pair("refresh", v);
    }
    drop(query);
    format!(
        "{}{}",
        url.path(),
        url.query().map(|q| format!("?{q}")).unwrap_or_default()
    )
}
fn identity(resource: &crate::models::ArgoManagedResource) -> Value {
    json!({"group": resource.group, "kind": resource.kind, "namespace": resource.namespace, "name": resource.name})
}
fn connected_payload(request: &ArgoOperationRequest) -> Value {
    if matches!(request.action.as_str(), "sync" | "retry") {
        if let Some(payload) = &request.sync_payload {
            return payload.clone();
        }
    }
    match request.action.as_str() {
        "sync" | "retry" => {
            json!({"revision":request.revision,"resources":request.resources.iter().map(identity).collect::<Vec<_>>(),"prune":request.prune,"dryRun":request.dry_run,"syncOptions":{"items":if request.force == Some(true) { vec!["Force=true", "Replace=true"] } else { vec![] }}})
        }
        "rollback" => {
            json!({"name":request.application.name,"appNamespace":request.application.namespace,"project":request.application.project,"id":request.history_id,"prune":request.prune,"dryRun":request.dry_run})
        }
        "resourceAction" => {
            let parameters = request
                .resource_action_parameters
                .as_ref()
                .and_then(Value::as_object)
                .into_iter()
                .flatten()
                .filter_map(|(name, value)| {
                    value
                        .as_str()
                        .map(|value| json!({"name":name,"value":value}))
                })
                .collect::<Vec<_>>();
            json!({"appNamespace":request.application.namespace,"project":request.application.project,"group":request.resources[0].group,"version":request.resources[0].version,"kind":request.resources[0].kind,"namespace":request.resources[0].namespace,"resourceName":request.resources[0].name,"action":request.resource_action,"resourceActionParameters":parameters})
        }
        _ => json!({}),
    }
}
fn kubernetes_sync_payload(request: &ArgoOperationRequest) -> Value {
    let http = request
        .sync_payload
        .clone()
        .unwrap_or_else(|| sync_payload(request));
    let mut value = http.as_object().cloned().unwrap_or_default();
    let options = value
        .remove("syncOptions")
        .and_then(|value| value.get("items").and_then(Value::as_array).cloned())
        .unwrap_or_default();
    if let Some(strategy) = value.remove("strategy") {
        value.insert("syncStrategy".into(), strategy);
    }
    value.insert("syncOptions".into(), Value::Array(options));
    Value::Object(value)
}
fn resource_action_path(request: &ArgoOperationRequest) -> String {
    let resource = &request.resources[0];
    let mut url = reqwest::Url::parse(&format!(
        "https://argo.invalid{}",
        application_path(&request.application, "/resource/actions", None)
    ))
    .expect("static URL");
    let mut q = url.query_pairs_mut();
    for (key, value) in [
        ("group", resource.group.as_deref()),
        ("version", resource.version.as_deref()),
        ("kind", resource.kind.as_deref()),
        ("namespace", resource.namespace.as_deref()),
        ("resourceName", resource.name.as_deref()),
    ] {
        if let Some(value) = value {
            q.append_pair(key, value);
        }
    }
    drop(q);
    format!("{}?{}", url.path(), url.query().unwrap_or_default())
}
async fn validate_resource_action(
    connection: &super::connected::ConnectedArgo,
    request: &ArgoOperationRequest,
) -> Result<(), AppError> {
    let actions = api_get(connection, &resource_action_path(request)).await?;
    let action = request.resource_action.as_deref();
    let action_definition = actions
        .get("actions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|item| item.get("name").and_then(Value::as_str) == action || item.as_str() == action);
    if action_definition.is_none()
        || action_definition
            .and_then(|item| item.get("disabled"))
            .and_then(Value::as_bool)
            == Some(true)
    {
        return Err(AppError::new(
            "resource action is not reported by Argo CD",
            AppErrorKind::ArgoOperationUnavailable,
        ));
    }
    let parameters = request
        .resource_action_parameters
        .as_ref()
        .and_then(Value::as_object);
    if request.resource_action_parameters.is_some()
        && parameters.is_none_or(|values| values.values().any(|value| !value.is_string()))
    {
        return Err(AppError::new(
            "resource action parameters must be string values",
            AppErrorKind::ArgoOperationUnavailable,
        ));
    }
    if action_definition
        .unwrap()
        .get("params")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|parameter| {
            parameter.get("required").and_then(Value::as_bool) == Some(true)
                && parameter
                    .get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|name| {
                        parameters
                            .and_then(|values| values.get(name))
                            .and_then(Value::as_str)
                            .is_none_or(str::is_empty)
                    })
        })
    {
        return Err(AppError::new(
            "required resource action parameters missing",
            AppErrorKind::ArgoOperationUnavailable,
        ));
    }
    Ok(())
}
async fn kubernetes_operation(
    request: ArgoOperationRequest,
    client: Client,
    resource: ApiResource,
) -> Result<ArgoOperationResult, AppError> {
    ensure_kubernetes_transport_action(&request.action)?;
    let api = Api::<DynamicObject>::namespaced_with(
        client,
        request.application.namespace.as_deref().expect("validated"),
        &resource,
    );
    let mut metadata = json!({
        "resourceVersion": request.resource_version.clone().expect("validated")
    });
    if let Some(uid) = &request.application.uid {
        metadata["uid"] = Value::String(uid.clone());
    }
    let patch = match request.action.as_str() {
        "refresh" | "hardRefresh" => {
            metadata["annotations"] = json!({"argocd.argoproj.io/refresh":if request.action == "hardRefresh" { "hard" } else { "normal" }});
            json!({"metadata":metadata})
        }
        "sync" | "retry" => {
            json!({"metadata":metadata,"operation":{"sync":kubernetes_sync_payload(&request)}})
        }
        _ => {
            return Err(AppError::new(
                "operation unavailable in Kubernetes fallback",
                AppErrorKind::ArgoOperationUnavailable,
            ))
        }
    };
    api.patch(
        &request.application.name,
        &PatchParams::default(),
        &Patch::Merge(&patch),
    )
    .await?;
    Ok(ArgoOperationResult {
        accepted: true,
        transport: "kubernetes".into(),
        message: "Kubernetes-authorized fallback accepted; Argo RBAC not evaluated".into(),
        operation: None,
    })
}

async fn resolve_request(
    store: &super::ArgoConnectionStore,
    request: &ArgoOperationRequest,
) -> Result<(ArgoOperationRequest, Option<String>, Option<String>), AppError> {
    let (mut resolved, generation, instance_id) = if request.transport == "connected" {
        let connection = scoped_connection(
            store,
            request.connection_id.as_deref().ok_or_else(|| {
                AppError::new("Argo CD connection required", AppErrorKind::ArgoConnection)
            })?,
            request.cluster_context.as_deref().ok_or_else(|| {
                AppError::new(
                    "clusterContext required",
                    AppErrorKind::ArgoOperationUnavailable,
                )
            })?,
            request.application.workspace_id.as_deref(),
            request.kubeconfig_env_var.as_deref(),
        )?;
        let value = api_get(
            &connection,
            &application_path(&request.application, "", None),
        )
        .await?;
        let metadata = value.get("metadata").unwrap_or(&value);
        let mut application = request.application.clone();
        if let Some(name) = metadata.get("name").and_then(Value::as_str) {
            name.clone_into(&mut application.name);
        }
        application.namespace = metadata
            .get("namespace")
            .and_then(Value::as_str)
            .map(str::to_owned);
        application.project = value
            .pointer("/spec/project")
            .and_then(Value::as_str)
            .map(str::to_owned);
        application.uid = metadata
            .get("uid")
            .and_then(Value::as_str)
            .map(str::to_owned);
        application.resource_version = metadata
            .get("resourceVersion")
            .and_then(Value::as_str)
            .map(str::to_owned);
        application
            .workspace_id
            .clone_from(&connection.profile.workspace_id);
        let mut resolved = request.clone();
        resolved.application = application;
        resolved
            .cluster_context
            .clone_from(&connection.profile.cluster_context);
        (
            resolved,
            Some(connection.generation.clone()),
            Some(connection.instance_id.clone()),
        )
    } else {
        (request.clone(), None, None)
    };
    if resolved.action != "retry" {
        if resolved.action == "sync" {
            resolved.sync_payload = Some(sync_payload(&resolved));
        }
        return Ok((resolved, generation, instance_id));
    }
    let sync = if resolved.transport == "connected" {
        let connection = scoped_connection(
            store,
            resolved.connection_id.as_deref().ok_or_else(|| {
                AppError::new("Argo CD connection required", AppErrorKind::ArgoConnection)
            })?,
            resolved.cluster_context.as_deref().ok_or_else(|| {
                AppError::new(
                    "clusterContext required",
                    AppErrorKind::ArgoOperationUnavailable,
                )
            })?,
            resolved.application.workspace_id.as_deref(),
            resolved.kubeconfig_env_var.as_deref(),
        )?;
        api_get(
            &connection,
            &application_path(&resolved.application, "", None),
        )
        .await?
        .pointer("/status/operationState/operation/sync")
        .cloned()
    } else {
        let context = request.cluster_context.as_deref().ok_or_else(|| {
            AppError::new(
                "clusterContext required",
                AppErrorKind::ArgoOperationUnavailable,
            )
        })?;
        let client = client_for_context(context, request.kubeconfig_env_var.clone()).await?;
        let ar = find_api_resource(&client, "argoproj.io", "Application")
            .await?
            .ok_or_else(|| AppError::new("Application CRD not found", AppErrorKind::Cluster))?;
        Api::<DynamicObject>::namespaced_with(
            client,
            request.application.namespace.as_deref().ok_or_else(|| {
                AppError::new(
                    "application namespace required",
                    AppErrorKind::ArgoOperationUnavailable,
                )
            })?,
            &ar,
        )
        .get(&request.application.name)
        .await?
        .data
        .pointer("/status/operationState/operation/sync")
        .cloned()
    }
    .ok_or_else(|| {
        AppError::new(
            "no recorded sync operation to retry",
            AppErrorKind::ArgoOperationUnavailable,
        )
    })?;
    Ok((recorded_retry(&resolved, sync)?, generation, instance_id))
}

fn recorded_retry(
    request: &ArgoOperationRequest,
    sync: Value,
) -> Result<ArgoOperationRequest, AppError> {
    let mut resolved = request.clone();
    resolved.revision = sync
        .get("revision")
        .and_then(Value::as_str)
        .map(str::to_owned);
    resolved.prune = sync.get("prune").and_then(Value::as_bool);
    resolved.dry_run = sync.get("dryRun").and_then(Value::as_bool);
    resolved.force = Some(
        sync.get("syncOptions")
            .and_then(|value| {
                value
                    .get("items")
                    .and_then(Value::as_array)
                    .or_else(|| value.as_array())
            })
            .is_some_and(|items| items.iter().any(|item| item.as_str() == Some("Force=true"))),
    );
    resolved.resources = sync
        .get("resources")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|item| crate::models::ArgoManagedResource {
            group: item.get("group").and_then(Value::as_str).map(str::to_owned),
            kind: item.get("kind").and_then(Value::as_str).map(str::to_owned),
            namespace: item
                .get("namespace")
                .and_then(Value::as_str)
                .map(str::to_owned),
            name: item.get("name").and_then(Value::as_str).map(str::to_owned),
            ..Default::default()
        })
        .collect();
    resolved.sync_payload = Some(recorded_sync_payload(sync)?);
    Ok(resolved)
}

fn sync_payload(request: &ArgoOperationRequest) -> Value {
    json!({"revision":request.revision,"resources":request.resources.iter().map(identity).collect::<Vec<_>>(),"prune":request.prune,"dryRun":request.dry_run,"syncOptions":{"items":if request.force == Some(true) { vec!["Force=true", "Replace=true"] } else { vec![] }}})
}

fn recorded_sync_payload(sync: Value) -> Result<Value, AppError> {
    let mut payload = sync.as_object().cloned().ok_or_else(|| {
        AppError::new(
            "recorded sync operation is invalid",
            AppErrorKind::ArgoOperationUnavailable,
        )
    })?;
    if payload.keys().any(|key| {
        !matches!(
            key.as_str(),
            "revision"
                | "prune"
                | "dryRun"
                | "syncOptions"
                | "resources"
                | "syncStrategy"
                | "strategy"
        )
    }) {
        return Err(AppError::new(
            "recorded sync operation has unsupported fields",
            AppErrorKind::ArgoOperationUnavailable,
        ));
    }
    let resources = match payload.remove("resources") {
        None => Vec::new(),
        Some(Value::Array(resources)) => resources,
        Some(_) => {
            return Err(AppError::new(
                "recorded sync resources are invalid",
                AppErrorKind::ArgoOperationUnavailable,
            ))
        }
    }
    .into_iter()
    .map(|resource| {
        let resource = resource.as_object().ok_or_else(|| {
            AppError::new(
                "recorded sync resource is invalid",
                AppErrorKind::ArgoOperationUnavailable,
            )
        })?;
        if resource.keys().any(|key| {
            !matches!(
                key.as_str(),
                "group" | "version" | "kind" | "namespace" | "name"
            )
        }) {
            return Err(AppError::new(
                "recorded sync resource has unsupported fields",
                AppErrorKind::ArgoOperationUnavailable,
            ));
        }
        if resource.values().any(|value| !value.is_string()) {
            return Err(AppError::new(
                "recorded sync resource fields are invalid",
                AppErrorKind::ArgoOperationUnavailable,
            ));
        }
        let mut selector = serde_json::Map::new();
        for key in ["group", "kind", "namespace", "name"] {
            if let Some(value) = resource.get(key) {
                selector.insert(key.into(), value.clone());
            }
        }
        Ok(Value::Object(selector))
    })
    .collect::<Result<Vec<_>, AppError>>()?;
    let options = match payload.remove("syncOptions") {
        None => Vec::new(),
        Some(Value::Array(options)) => options,
        Some(Value::Object(mut options)) if options.len() == 1 && options.contains_key("items") => {
            options
                .remove("items")
                .and_then(|items| items.as_array().cloned())
                .ok_or_else(|| {
                    AppError::new(
                        "recorded sync options are invalid",
                        AppErrorKind::ArgoOperationUnavailable,
                    )
                })?
        }
        Some(_) => {
            return Err(AppError::new(
                "recorded sync options are invalid",
                AppErrorKind::ArgoOperationUnavailable,
            ))
        }
    };
    if options.iter().any(|option| !option.is_string()) {
        return Err(AppError::new(
            "recorded sync option is invalid",
            AppErrorKind::ArgoOperationUnavailable,
        ));
    }
    let strategy = payload
        .remove("syncStrategy")
        .or_else(|| payload.remove("strategy"));
    payload.insert("resources".into(), Value::Array(resources));
    payload.insert("syncOptions".into(), json!({"items": options}));
    if let Some(strategy) = strategy {
        payload.insert("strategy".into(), strategy);
    }
    Ok(Value::Object(payload))
}

#[cfg(test)]
mod tests {
    use super::super::session::SecureStore;
    use super::*;
    use crate::models::{ArgoConnectionProfile, ArgoServerEndpoint};
    use std::{
        collections::HashMap,
        sync::{atomic::AtomicU64, Mutex},
    };

    #[derive(Default)]
    struct MemorySessionStore(Mutex<HashMap<String, String>>);
    impl SecureStore for MemorySessionStore {
        fn read(&self, id: &str) -> Result<Option<String>, AppError> {
            Ok(self.0.lock().unwrap().get(id).cloned())
        }
        fn write(&self, id: &str, value: &str) -> Result<(), AppError> {
            self.0.lock().unwrap().insert(id.into(), value.into());
            Ok(())
        }
        fn delete(&self, id: &str) -> Result<(), AppError> {
            self.0.lock().unwrap().remove(id);
            Ok(())
        }
    }

    fn connection(generation: &str) -> Arc<ConnectedArgo> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        Arc::new(ConnectedArgo {
            profile: ArgoConnectionProfile {
                id: "server".into(),
                url: "https://argo.example".into(),
                endpoint: ArgoServerEndpoint::ExternalHttps {
                    url: "https://argo.example".into(),
                },
                cluster_context: Some("cluster".into()),
                workspace_id: Some("workspace".into()),
                kubeconfig_source_key: Some("kubeconfigSource=test".into()),
                transport: "connected".into(),
                remember_credential: true,
            },
            token: "token".into(),
            username: None,
            client: reqwest::Client::new(),
            tunnel: None,
            generation: generation.into(),
            instance_id: uuid::Uuid::new_v4().to_string(),
            gate: Arc::new(tokio::sync::Mutex::new(())),
        })
    }

    fn store() -> super::super::ArgoConnectionStore {
        super::super::ArgoConnectionStore {
            connections: Mutex::new(HashMap::new()),
            sessions: super::super::session::SessionStore::with_secure(Arc::new(
                MemorySessionStore::default(),
            )),
            cleanup_epoch: AtomicU64::default(),
        }
    }

    fn reviewed(store: &super::super::ArgoConnectionStore) -> (String, SessionSnapshot) {
        let connection = scoped_connection(
            store,
            "server",
            "cluster",
            Some("workspace"),
            Some("kubeconfigSource=test"),
        )
        .unwrap();
        let (id, _) = issue(
            &store.sessions,
            ArgoOperationRequest {
                transport: "connected".into(),
                action: "refresh".into(),
                connection_id: Some("server".into()),
                cluster_context: Some("cluster".into()),
                kubeconfig_env_var: Some("kubeconfigSource=test".into()),
                application: ArgoApplicationRef {
                    name: "app".into(),
                    workspace_id: Some("workspace".into()),
                    ..Default::default()
                },
                ..Default::default()
            },
            Some(connection.generation.clone()),
            Some(connection.instance_id.clone()),
        )
        .unwrap();
        let reviewed = peek(&store.sessions, &id, None).unwrap();
        (id, reviewed)
    }

    #[tokio::test]
    async fn reconnect_with_same_generation_rejects_and_tombstones_reviewed_session() {
        let store = store();
        let first = connection("remembered-generation");
        store
            .replace_connection("server".into(), first.clone())
            .await
            .unwrap();
        let (id, _reviewed) = reviewed(&store);

        let replacement = connection("remembered-generation");
        assert_ne!(first.instance_id, replacement.instance_id);
        store
            .replace_connection("server".into(), replacement)
            .await
            .unwrap();
        let error = run_argo_operation_for_store(
            &store,
            ArgoOperationConfirmation {
                session_id: id.clone(),
                confirmation: id.clone(),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error.message, "Argo CD connection was replaced");
        assert!(peek(&store.sessions, &id, None).is_err());

        let (id, _reviewed) = reviewed(&store);
        store.remove_connection("server").await.unwrap();
        let error = run_argo_operation_for_store(
            &store,
            ArgoOperationConfirmation {
                session_id: id.clone(),
                confirmation: id.clone(),
            },
        )
        .await
        .unwrap_err();
        assert_eq!(error.message, "Argo CD connection not found");
        assert!(peek(&store.sessions, &id, None).is_err());
    }

    #[tokio::test]
    async fn rejected_lease_tombstones_the_reviewed_session() {
        let store = store();
        let old = connection("old");
        store
            .replace_connection("server".into(), old.clone())
            .await
            .unwrap();
        let (id, reviewed) = reviewed(&store);
        store
            .replace_connection("server".into(), connection("replacement"))
            .await
            .unwrap();

        let error = run_connected_operation(&store, &id, &reviewed, old)
            .await
            .unwrap_err();
        assert_eq!(error.message, "Argo CD connection was replaced");
        assert!(peek(&store.sessions, &id, None).is_err());
    }

    #[test]
    fn operation_paths_encode_application_and_resource_identity() {
        let application = ArgoApplicationRef {
            name: "app/a?#".into(),
            namespace: Some("app ns/&?#".into()),
            project: Some("project/&?#".into()),
            ..Default::default()
        };
        assert_eq!(
            application_path(&application, "/sync", None),
            "/api/v1/applications/app%2Fa%3F%23/sync?appNamespace=app+ns%2F%26%3F%23&project=project%2F%26%3F%23"
        );
        let path = resource_action_path(&ArgoOperationRequest {
            application,
            resources: vec![crate::models::ArgoManagedResource {
                group: Some("apps/&?#".into()),
                version: Some("v1/?.".into()),
                kind: Some("Deployment/#".into()),
                namespace: Some("ns/&?#".into()),
                name: Some("resource/&?#".into()),
                ..Default::default()
            }],
            ..Default::default()
        });
        assert_eq!(
            path,
            "/api/v1/applications/app%2Fa%3F%23/resource/actions?appNamespace=app+ns%2F%26%3F%23&project=project%2F%26%3F%23&group=apps%2F%26%3F%23&version=v1%2F%3F.&kind=Deployment%2F%23&namespace=ns%2F%26%3F%23&resourceName=resource%2F%26%3F%23"
        );
    }

    #[test]
    fn resource_action_requires_complete_server_identity() {
        let complete = crate::models::ArgoManagedResource {
            version: Some("v1".into()),
            kind: Some("ConfigMap".into()),
            name: Some("settings".into()),
            ..Default::default()
        };
        let request = |resource| ArgoOperationRequest {
            transport: "connected".into(),
            action: "resourceAction".into(),
            application: ArgoApplicationRef {
                name: "app".into(),
                ..Default::default()
            },
            resource_action: Some("restart".into()),
            resources: vec![resource],
            ..Default::default()
        };

        assert!(valid(&request(complete.clone())).is_ok());
        for incomplete in [
            crate::models::ArgoManagedResource {
                version: None,
                ..complete.clone()
            },
            crate::models::ArgoManagedResource {
                kind: Some(" ".into()),
                ..complete.clone()
            },
            crate::models::ArgoManagedResource {
                name: Some(String::new()),
                ..complete
            },
        ] {
            let error = valid(&request(incomplete)).unwrap_err();
            assert_eq!(error.kind, AppErrorKind::ArgoOperationUnavailable);
        }
    }

    #[test]
    fn absent_optional_identity_fields_are_not_mismatches() {
        let mut expected = ArgoApplicationRef {
            name: "app".into(),
            ..Default::default()
        };
        let current = json!({"metadata": {"name": "app"}});

        assert!(verify_application_identity(&current, &expected).is_ok());
        expected.uid = Some("reviewed-uid".into());
        assert!(verify_application_identity(&current, &expected).is_err());
    }

    #[test]
    fn kubernetes_identity_includes_object_metadata() {
        let resource = ApiResource::from_gvk(&kube::core::GroupVersionKind::gvk(
            "argoproj.io",
            "v1alpha1",
            "Application",
        ));
        let mut current = DynamicObject::new("app", &resource)
            .within("argocd")
            .data(json!({"spec":{"project":"default"}}));
        current.metadata.uid = Some("uid-1".into());
        current.metadata.resource_version = Some("42".into());
        let expected = ArgoApplicationRef {
            name: "app".into(),
            namespace: Some("argocd".into()),
            uid: Some("uid-1".into()),
            resource_version: Some("42".into()),
            project: Some("default".into()),
            ..Default::default()
        };
        assert!(verify_kubernetes_application_identity(&current, &expected).is_ok());
        current.metadata.resource_version = Some("43".into());
        assert!(verify_kubernetes_application_identity(&current, &expected).is_err());
    }

    #[test]
    fn connected_refresh_is_allowed_into_review_gate() {
        let request = ArgoOperationRequest {
            transport: "connected".into(),
            action: "refresh".into(),
            application: ArgoApplicationRef {
                name: "app".into(),
                workspace_id: Some("workspace".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        assert!(valid(&request).is_ok());
        assert_eq!(request.transport, "connected");
    }
    #[test]
    fn sync_payload_matches_argocd_request_contract() {
        let request = ArgoOperationRequest {
            action: "sync".into(),
            revision: Some("main".into()),
            force: Some(true),
            resources: vec![crate::models::ArgoManagedResource {
                group: Some("apps".into()),
                version: Some("v1".into()),
                kind: Some("Deployment".into()),
                namespace: Some("default".into()),
                name: Some("web".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(
            connected_payload(&request),
            json!({"revision":"main","resources":[{"group":"apps","kind":"Deployment","namespace":"default","name":"web"}],"prune":null,"dryRun":null,"syncOptions":{"items":["Force=true","Replace=true"]}})
        );
    }

    #[test]
    fn recorded_retry_is_replayed_as_sync_request() {
        let retry = recorded_retry(&ArgoOperationRequest { action: "retry".into(), ..Default::default() }, json!({"revision":"v2","prune":true,"syncOptions":{"items":["Force=true"]},"resources":[{"group":"apps","version":"v1","kind":"Deployment","namespace":"default","name":"web"}]})).expect("recorded sync");
        assert_eq!(
            connected_payload(&retry),
            json!({"revision":"v2","resources":[{"group":"apps","kind":"Deployment","namespace":"default","name":"web"}],"prune":true,"syncOptions":{"items":["Force=true"]}})
        );
    }

    #[test]
    fn resource_action_contract_is_flat_and_server_scoped() {
        let request = ArgoOperationRequest {
            action: "resourceAction".into(),
            application: ArgoApplicationRef {
                namespace: Some("argocd".into()),
                project: Some("default".into()),
                ..Default::default()
            },
            resource_action: Some("restart".into()),
            resources: vec![crate::models::ArgoManagedResource {
                group: Some("apps".into()),
                version: Some("v1".into()),
                kind: Some("Deployment".into()),
                namespace: Some("default".into()),
                name: Some("web".into()),
                ..Default::default()
            }],
            resource_action_parameters: Some(json!({"replicas":"3","mode":"safe"})),
            ..Default::default()
        };
        assert_eq!(
            connected_payload(&request),
            json!({"appNamespace":"argocd","project":"default","group":"apps","version":"v1","kind":"Deployment","namespace":"default","resourceName":"web","action":"restart","resourceActionParameters":[{"name":"mode","value":"safe"},{"name":"replicas","value":"3"}]})
        );
    }

    #[test]
    fn fallback_sync_uses_crd_option_array() {
        let request = ArgoOperationRequest {
            action: "sync".into(),
            force: Some(true),
            ..Default::default()
        };
        assert_eq!(
            kubernetes_sync_payload(&request)["syncOptions"],
            json!(["Force=true", "Replace=true"])
        );
        assert_eq!(
            connected_payload(&request)["syncOptions"],
            json!({"items":["Force=true", "Replace=true"]})
        );
    }

    #[test]
    fn recorded_sync_allowlists_and_maps_strategy() {
        assert_eq!(
            recorded_sync_payload(json!({"revision":"v1","syncStrategy":{"apply":{"force":true}}}))
                .unwrap(),
            json!({"revision":"v1","resources":[],"syncOptions":{"items":[]},"strategy":{"apply":{"force":true}}})
        );
        assert!(recorded_sync_payload(json!({"source":{"repoURL":"secret"}})).is_err());
        assert!(recorded_sync_payload(json!({"autoHealAttemptsCount":1})).is_err());
    }

    #[test]
    fn fallback_retry_keeps_recorded_crd_sync_strategy() {
        let retry = recorded_retry(
            &ArgoOperationRequest {
                action: "retry".into(),
                transport: "kubernetes".into(),
                ..Default::default()
            },
            json!({"syncStrategy":{"apply":{"force":true}}}),
        )
        .unwrap();
        assert_eq!(
            kubernetes_sync_payload(&retry)["syncStrategy"],
            json!({"apply":{"force":true}})
        );
    }

    #[test]
    fn recorded_sync_rejects_malformed_resources_and_options() {
        assert!(recorded_sync_payload(json!({"resources":{}})).is_err());
        assert!(recorded_sync_payload(json!({"resources":["Deployment"]})).is_err());
        assert!(recorded_sync_payload(json!({"resources":[{"name":4}]})).is_err());
        assert!(recorded_sync_payload(json!({"syncOptions":{}})).is_err());
        assert!(recorded_sync_payload(json!({"syncOptions":{"items":"Force=true"}})).is_err());
        assert!(recorded_sync_payload(json!({"syncOptions":[4]})).is_err());
    }

    #[test]
    fn rollback_payload_is_bound_to_application_and_flags() {
        let request = ArgoOperationRequest {
            action: "rollback".into(),
            history_id: Some(4),
            prune: Some(true),
            dry_run: Some(false),
            application: ArgoApplicationRef {
                name: "app".into(),
                namespace: Some("argo".into()),
                project: Some("default".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(
            connected_payload(&request),
            json!({"name":"app","appNamespace":"argo","project":"default","id":4,"prune":true,"dryRun":false})
        );
    }
}

#[cfg(test)]
mod kubernetes_transport_gate_tests {
    use super::*;

    #[test]
    fn kubernetes_transport_allows_only_fallback_actions() {
        for action in KUBERNETES_TRANSPORT_ACTIONS {
            assert!(ensure_kubernetes_transport_action(action).is_ok());
        }
        for action in ["rollback", "terminate", "resourceAction", ""] {
            let error = ensure_kubernetes_transport_action(action).unwrap_err();
            assert_eq!(error.kind, AppErrorKind::ArgoOperationUnavailable);
        }
    }
}
