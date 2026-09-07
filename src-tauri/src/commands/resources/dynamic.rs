use crate::commands::diagnostics::record_backend_result;
use crate::commands::helpers::{
    extract_argo_app, extract_git_ops_owner, extract_helm_release, extract_owner_ref,
    k8s_creation_timestamp_to_rfc3339, legacy_resource_health, list_params,
    normalize_k8s_yaml_value, resource_age, serialize_json_value_document,
};
use crate::commands::{
    diagnostic_field,
    kubeconfig::{kubeconfig_source_key, KubeconfigSource},
    record_backend_error, record_backend_success, BackendCancellationRegistry, ClusterLiveStore,
};
use crate::models::AppErrorKind;
use crate::models::{
    argo_health_assessment, evaluate_health, AppError, DiscoveredResourceKind, HealthAssessment,
    HealthAssessmentEvidence, HealthAssessmentInput, HealthAssessmentSource, HealthAssessmentState,
    ResourceDetailsFull, ResourceHealth, ResourceSummary, YamlEncoding, YamlViewMode,
};
use chrono::{TimeZone, Utc};
use kube::{
    api::{Api, ApiResource, DynamicObject},
    Client,
};
use serde_json::Value;
use std::time::Instant;
use tauri::State;

pub(crate) fn api_resource_from_discovered(
    resource_kind: &DiscoveredResourceKind,
) -> Result<ApiResource, AppError> {
    if resource_kind.version.trim().is_empty()
        || resource_kind.kind.trim().is_empty()
        || resource_kind.plural.trim().is_empty()
    {
        return Err(AppError::new(
            "invalid discovered resource kind",
            AppErrorKind::Cluster,
        ));
    }

    let resource = ApiResource {
        group: resource_kind.group.clone(),
        version: resource_kind.version.clone(),
        api_version: resource_kind.api_version.clone(),
        kind: resource_kind.kind.clone(),
        plural: resource_kind.plural.clone(),
    };
    crate::commands::helpers::validate_api_resource(&resource)?;
    Ok(resource)
}

pub(crate) fn dynamic_status_from_data(data: &Value) -> Option<String> {
    let status = data.get("status")?;
    if let Some(phase) = status.get("phase").and_then(Value::as_str) {
        return Some(phase.to_string());
    }

    let conditions = status.get("conditions").and_then(Value::as_array)?;
    if let Some(condition) = conditions.iter().find(|condition| {
        condition
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|condition_type| condition_type == "Ready")
    }) {
        let condition_status = condition.get("status").and_then(Value::as_str)?;
        return Some(format!("Ready: {condition_status}"));
    }

    conditions.first().and_then(|condition| {
        let condition_type = condition.get("type").and_then(Value::as_str)?;
        let condition_status = condition.get("status").and_then(Value::as_str)?;
        Some(format!("{condition_type}: {condition_status}"))
    })
}

fn dynamic_status_value(data: &Value) -> Option<Value> {
    data.get("status").cloned()
}

pub(crate) fn dynamic_resource_summary(
    cluster_context: &str,
    resource_kind: &DiscoveredResourceKind,
    object: &DynamicObject,
) -> ResourceSummary {
    let mut summary = ResourceSummary {
        kind: resource_kind.kind.clone(),
        cluster: cluster_context.to_string(),
        name: object.metadata.name.clone().unwrap_or_default(),
        namespace: object.metadata.namespace.clone(),
        age: resource_age(object.metadata.creation_timestamp.clone().map(|t| {
            Utc.timestamp_opt(t.0.as_second(), 0)
                .single()
                .unwrap_or_else(Utc::now)
        })),
        api_version: Some(resource_kind.api_version.clone()),
        group: Some(resource_kind.group.clone()),
        version: Some(resource_kind.version.clone()),
        plural: Some(resource_kind.plural.clone()),
        namespaced: Some(resource_kind.namespaced),
        dynamic: Some(true),
        health: ResourceHealth::default(),
        health_assessment: crate::models::HealthAssessment::default(),
        created_at: k8s_creation_timestamp_to_rfc3339(&object.metadata.creation_timestamp),
        status: dynamic_status_from_data(&object.data),
        ready: None,
        restarts: None,
        owner_ref: extract_owner_ref(&object.metadata),
        argo_app: extract_argo_app(&object.metadata),
        helm_release: extract_helm_release(&object.metadata),
        git_ops_owner: extract_git_ops_owner(&object.metadata),
        git_ops_ownership_partial: false,
    };
    if resource_kind.group == "argoproj.io" && resource_kind.kind == "Application" {
        summary.health_assessment = argo_health_assessment(
            object
                .data
                .pointer("/status/health/status")
                .and_then(Value::as_str),
            object
                .data
                .pointer("/status/sync/status")
                .and_then(Value::as_str),
        );
        summary.health = legacy_resource_health(summary.health_assessment.state);
    } else {
        summary.health_assessment = dynamic_health_assessment(&object.data);
        summary.health = legacy_resource_health(summary.health_assessment.state);
    }
    summary
}

fn dynamic_health_assessment(data: &Value) -> HealthAssessment {
    let conditions = data.pointer("/status/conditions").and_then(Value::as_array);
    let evidence: Vec<HealthAssessmentEvidence> = conditions
        .into_iter()
        .flatten()
        .filter_map(|condition| {
            let kind = condition.get("type")?.as_str()?;
            let status = condition.get("status")?.as_str()?;
            let state = match (kind, status) {
                ("Stalled", "True") | ("Ready" | "Healthy", "False") => {
                    HealthAssessmentState::Degraded
                }
                ("Reconciling", "True") => HealthAssessmentState::NeedsAttention,
                ("Ready" | "Healthy", "True") | ("Reconciling", "False") => {
                    HealthAssessmentState::Healthy
                }
                _ => HealthAssessmentState::Unknown,
            };
            matches!(kind, "Ready" | "Healthy" | "Reconciling" | "Stalled").then(|| {
                HealthAssessmentEvidence {
                    source: HealthAssessmentSource::Kubernetes,
                    raw: condition.clone(),
                    state: Some(state),
                    current: true,
                    reason: format!("Kubernetes {kind} condition is {status}"),
                }
            })
        })
        .collect();
    evaluate_health(HealthAssessmentInput {
        recognized_semantics: !evidence.is_empty(),
        provider_available: true,
        evidence,
    })
}

fn is_core_v1_secret(resource_kind: &DiscoveredResourceKind) -> bool {
    resource_kind.group.is_empty()
        && resource_kind.version == "v1"
        && resource_kind.plural == "secrets"
}

fn redact_dynamic_secret_fields(value: &mut Value) {
    if let Some(annotation) =
        value.pointer_mut("/metadata/annotations/kubectl.kubernetes.io~1last-applied-configuration")
    {
        *annotation = Value::String("REDACTED".into());
    }
    let Value::Object(root) = value else {
        return;
    };

    for field in ["data", "stringData"] {
        if let Some(Value::Object(values)) = root.get_mut(field) {
            for value in values.values_mut() {
                *value = Value::String("REDACTED".to_string());
            }
        }
    }
}

fn serialize_dynamic_resource_document(
    resource_kind: &DiscoveredResourceKind,
    object: &DynamicObject,
    mode: YamlViewMode,
    encoding: YamlEncoding,
) -> Result<String, AppError> {
    let mut value = serde_json::to_value(object)
        .map_err(|e| AppError::new(e.to_string(), AppErrorKind::Serialization).with_source(e))?;
    normalize_k8s_yaml_value(&mut value, mode);
    if is_core_v1_secret(resource_kind) {
        redact_dynamic_secret_fields(&mut value);
    }
    serialize_json_value_document(&value, encoding)
}

async fn client_for_context(
    cluster_context: &str,
    kubeconfig_env_var: Option<String>,
) -> Result<Client, AppError> {
    let source = KubeconfigSource::new(kubeconfig_env_var)?;
    source.client_for_context(cluster_context).await
}

pub async fn dynamic_resources_summary_from(
    cluster_context: String,
    resource_kind: DiscoveredResourceKind,
    namespace: Option<String>,
    kubeconfig_env_var: Option<String>,
) -> Result<Vec<ResourceSummary>, AppError> {
    crate::commands::helpers::validate_namespace(namespace.as_deref())?;
    let client = client_for_context(&cluster_context, kubeconfig_env_var).await?;
    let api_resource = api_resource_from_discovered(&resource_kind)?;
    let api: Api<DynamicObject> = if resource_kind.namespaced {
        if let Some(namespace) = namespace.as_deref() {
            Api::namespaced_with(client, namespace, &api_resource)
        } else {
            Api::all_with(client, &api_resource)
        }
    } else {
        Api::all_with(client, &api_resource)
    };

    let objects = api.list(&list_params()).await.map_err(AppError::from)?;

    Ok(objects
        .iter()
        .map(|object| dynamic_resource_summary(&cluster_context, &resource_kind, object))
        .collect())
}

#[tauri::command]
pub async fn list_dynamic_resources(
    cluster_context: String,
    resource_kind: DiscoveredResourceKind,
    namespace: Option<String>,
    kubeconfig_env_var: Option<String>,
    live_store: State<'_, ClusterLiveStore>,
) -> Result<Vec<ResourceSummary>, AppError> {
    let started = Instant::now();
    let namespace_label = namespace.as_deref().unwrap_or("<all>");
    eprintln!(
        "[kubecove:backend] list_dynamic_resources start context={} kind={} api_version={} namespace={}",
        cluster_context, resource_kind.kind, resource_kind.api_version, namespace_label
    );
    let source_key = kubeconfig_source_key(kubeconfig_env_var.as_deref())?;
    let result = live_store
        .dynamic_resources(
            source_key,
            cluster_context.clone(),
            resource_kind.clone(),
            namespace.clone(),
            {
                let cluster_context = cluster_context.clone();
                let resource_kind = resource_kind.clone();
                let namespace = namespace.clone();
                let kubeconfig_env_var = kubeconfig_env_var.clone();
                move || {
                    dynamic_resources_summary_from(
                        cluster_context,
                        resource_kind,
                        namespace,
                        kubeconfig_env_var,
                    )
                }
            },
        )
        .await;
    match &result {
        Ok(rows) => {
            eprintln!(
                "[kubecove:backend] list_dynamic_resources done context={} kind={} namespace={} rows={} ms={}",
                cluster_context,
                resource_kind.kind,
                namespace_label,
                rows.len(),
                started.elapsed().as_millis()
            );
            record_backend_success(
                "list_dynamic_resources",
                started,
                vec![
                    diagnostic_field("kind", &resource_kind.kind),
                    diagnostic_field("rows", rows.len()),
                ],
            );
        }
        Err(err) => {
            eprintln!(
                "[kubecove:backend] list_dynamic_resources error context={} kind={} namespace={} error_kind={} message={} ms={}",
                cluster_context,
                resource_kind.kind,
                namespace_label,
                err.kind,
                err.message,
                started.elapsed().as_millis()
            );
            record_backend_error("list_dynamic_resources", started, err.kind.as_str());
        }
    }
    result
}

pub async fn dynamic_resource_details_from(
    cluster_context: String,
    resource_kind: DiscoveredResourceKind,
    name: String,
    namespace: Option<String>,
    kubeconfig_env_var: Option<String>,
    yaml_view_mode: Option<YamlViewMode>,
    yaml_encoding: Option<YamlEncoding>,
) -> Result<ResourceDetailsFull, AppError> {
    crate::commands::helpers::validate_path_segment(&name, "resource name")?;
    crate::commands::helpers::validate_namespace(namespace.as_deref())?;
    if resource_kind.namespaced && namespace.is_none() {
        return Err(AppError::new(
            format!("namespace required for {}", resource_kind.kind),
            AppErrorKind::Cluster,
        ));
    }

    let client = client_for_context(&cluster_context, kubeconfig_env_var).await?;
    let api_resource = api_resource_from_discovered(&resource_kind)?;
    let api: Api<DynamicObject> = if resource_kind.namespaced {
        Api::namespaced_with(
            client,
            namespace
                .as_deref()
                .expect("namespaced resource has namespace"),
            &api_resource,
        )
    } else {
        Api::all_with(client, &api_resource)
    };

    let mut object = api.get(&name).await.map_err(AppError::from)?;
    if is_core_v1_secret(&resource_kind) {
        crate::commands::helpers::redact_secret_metadata(&mut object.metadata);
    }
    let yaml = serialize_dynamic_resource_document(
        &resource_kind,
        &object,
        yaml_view_mode.unwrap_or_default(),
        yaml_encoding.unwrap_or_default(),
    )?;
    let metadata = serde_json::to_value(&object.metadata)
        .map_err(|e| AppError::new(e.to_string(), AppErrorKind::Serialization).with_source(e))?;
    let status = dynamic_status_value(&object.data);
    let summary = dynamic_resource_summary(&cluster_context, &resource_kind, &object);

    Ok(ResourceDetailsFull {
        summary,
        yaml,
        metadata,
        status,
    })
}

#[tauri::command]
pub async fn get_dynamic_resource_details(
    cluster_context: String,
    resource_kind: DiscoveredResourceKind,
    name: String,
    namespace: Option<String>,
    kubeconfig_env_var: Option<String>,
    yaml_view_mode: Option<YamlViewMode>,
    yaml_encoding: Option<YamlEncoding>,
    request_id: Option<String>,
    cancel_scope: Option<String>,
    cancellations: State<'_, BackendCancellationRegistry>,
) -> Result<ResourceDetailsFull, AppError> {
    let started = Instant::now();
    let namespace_label = namespace.as_deref().unwrap_or("<cluster>");
    eprintln!(
        "[kubecove:backend] get_dynamic_resource_details start context={} kind={} api_version={} namespace={} name={}",
        cluster_context, resource_kind.kind, resource_kind.api_version, namespace_label, name
    );
    let result = cancellations
        .execute(
            cancel_scope,
            request_id,
            dynamic_resource_details_from(
                cluster_context.clone(),
                resource_kind.clone(),
                name.clone(),
                namespace.clone(),
                kubeconfig_env_var,
                yaml_view_mode,
                yaml_encoding,
            ),
        )
        .await;
    match &result {
        Ok(details) => {
            eprintln!(
                "[kubecove:backend] get_dynamic_resource_details done context={} kind={} namespace={} name={} yaml_bytes={} status={} ms={}",
                cluster_context,
                resource_kind.kind,
                namespace_label,
                name,
                details.yaml.len(),
                details.status.is_some(),
                started.elapsed().as_millis()
            );
        }
        Err(err) if err.kind == AppErrorKind::Cancelled => {
            eprintln!(
                "[kubecove:backend] get_dynamic_resource_details cancelled context={} kind={} namespace={} name={} ms={}",
                cluster_context,
                resource_kind.kind,
                namespace_label,
                name,
                started.elapsed().as_millis()
            );
        }
        Err(err) => {
            eprintln!(
                "[kubecove:backend] get_dynamic_resource_details error context={} kind={} namespace={} name={} error_kind={} message={} ms={}",
                cluster_context,
                resource_kind.kind,
                namespace_label,
                name,
                err.kind,
                err.message,
                started.elapsed().as_millis()
            );
        }
    }
    record_backend_result(
        "get_dynamic_resource_details",
        started,
        &result,
        |details| {
            vec![
                diagnostic_field("kind", &resource_kind.kind),
                diagnostic_field("yamlBytes", details.yaml.len()),
                diagnostic_field("hasStatus", details.status.is_some()),
            ]
        },
    );
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn discovered_kind_cannot_redirect_the_resource_path() {
        for plural in ["secrets/token?ignored=", "../secrets", "%2e%2e", "pods/log"] {
            let mut kind = widget_kind();
            kind.plural = plural.into();
            assert!(api_resource_from_discovered(&kind).is_err());
        }
    }

    fn widget_kind() -> DiscoveredResourceKind {
        DiscoveredResourceKind {
            group: "example.com".to_string(),
            version: "v1".to_string(),
            api_version: "example.com/v1".to_string(),
            kind: "Widget".to_string(),
            plural: "widgets".to_string(),
            short_names: Vec::new(),
            namespaced: true,
        }
    }

    fn secret_kind() -> DiscoveredResourceKind {
        DiscoveredResourceKind {
            group: String::new(),
            version: "v1".to_string(),
            api_version: "v1".to_string(),
            kind: "Secret".to_string(),
            plural: "secrets".to_string(),
            short_names: Vec::new(),
            namespaced: true,
        }
    }

    #[test]
    fn rejects_invalid_discovered_kind() {
        let mut kind = widget_kind();
        kind.plural = String::new();

        let err = api_resource_from_discovered(&kind).unwrap_err();
        assert_eq!(err.kind, AppErrorKind::Cluster);
        assert_eq!(err.message, "invalid discovered resource kind");
    }

    #[test]
    fn extracts_dynamic_status_from_phase_or_conditions() {
        assert_eq!(
            dynamic_status_from_data(&json!({ "status": { "phase": "Running" } })),
            Some("Running".to_string())
        );
        assert_eq!(
            dynamic_status_from_data(&json!({
                "status": {
                    "conditions": [
                        { "type": "Available", "status": "False" },
                        { "type": "Ready", "status": "True" }
                    ]
                }
            })),
            Some("Ready: True".to_string())
        );
    }

    #[test]
    fn builds_dynamic_resource_summary_from_metadata_and_status() {
        let resource_kind = widget_kind();
        let api_resource = api_resource_from_discovered(&resource_kind).unwrap();
        let object = DynamicObject::new("sample-widget", &api_resource)
            .within("default")
            .data(json!({
                "status": {
                    "phase": "Running"
                }
            }));

        let summary = dynamic_resource_summary("kind-kind", &resource_kind, &object);

        assert_eq!(summary.kind, "Widget");
        assert_eq!(summary.cluster, "kind-kind");
        assert_eq!(summary.name, "sample-widget");
        assert_eq!(summary.namespace, Some("default".to_string()));
        assert_eq!(summary.status, Some("Running".to_string()));
        assert_eq!(summary.api_version, Some("example.com/v1".to_string()));
        assert_eq!(summary.group, Some("example.com".to_string()));
        assert_eq!(summary.version, Some("v1".to_string()));
        assert_eq!(summary.plural, Some("widgets".to_string()));
        assert_eq!(summary.namespaced, Some(true));
        assert_eq!(summary.dynamic, Some(true));
        assert_eq!(
            summary.health_assessment.state,
            HealthAssessmentState::NotEvaluated
        );
    }

    #[test]
    fn assesses_argocd_application_with_same_argo_contract() {
        let mut resource_kind = widget_kind();
        resource_kind.group = "argoproj.io".to_string();
        resource_kind.api_version = "argoproj.io/v1".to_string();
        resource_kind.plural = "applications".to_string();
        resource_kind.kind = "Application".to_string();
        let api_resource = api_resource_from_discovered(&resource_kind).unwrap();
        let object = DynamicObject::new("payments", &api_resource).data(json!({
            "status": { "health": { "status": "Missing" }, "sync": { "status": "OutOfSync" } }
        }));

        let summary = dynamic_resource_summary("kind-kind", &resource_kind, &object);

        assert_eq!(
            summary.health_assessment.state,
            HealthAssessmentState::Degraded
        );
        assert_eq!(summary.health, ResourceHealth::Degraded);
    }

    #[test]
    fn ignores_unrecognized_dynamic_conditions() {
        let resource_kind = widget_kind();
        let api_resource = api_resource_from_discovered(&resource_kind).unwrap();
        let object = DynamicObject::new("sample-widget", &api_resource).data(json!({
            "status": { "conditions": [{ "type": "ObservedGeneration", "status": "False" }] }
        }));

        assert_eq!(
            dynamic_resource_summary("kind-kind", &resource_kind, &object)
                .health_assessment
                .state,
            HealthAssessmentState::NotEvaluated
        );
    }

    #[test]
    fn redacts_core_v1_secret_dynamic_yaml() {
        let resource_kind = secret_kind();
        let api_resource = api_resource_from_discovered(&resource_kind).unwrap();
        let object = DynamicObject::new("api-token", &api_resource)
            .within("default")
            .data(json!({
                "data": {
                    "password": "c3VwZXItc2VjcmV0LXBhc3N3b3Jk"
                },
                "stringData": {
                    "token": "plain-secret"
                },
                "type": "Opaque"
            }));

        let yaml = serialize_dynamic_resource_document(
            &resource_kind,
            &object,
            YamlViewMode::Kubectl,
            YamlEncoding::Yaml,
        )
        .unwrap();

        assert!(yaml.contains("REDACTED"));
        assert!(!yaml.contains("c3VwZXItc2VjcmV0LXBhc3N3b3Jk"));
        assert!(!yaml.contains("plain-secret"));
    }

    #[test]
    fn audit_secret_endpoint_is_redacted_despite_forged_kind() {
        let mut resource_kind = secret_kind();
        resource_kind.kind = "Widget".into();
        resource_kind.api_version = "example.com/v1".into();
        let object = DynamicObject::new(
            "token",
            &ApiResource::erase::<k8s_openapi::api::core::v1::Secret>(&()),
        )
        .data(json!({"data":{"password":"sensitive"}}));
        let yaml = serialize_dynamic_resource_document(
            &resource_kind,
            &object,
            YamlViewMode::Kubectl,
            YamlEncoding::Yaml,
        )
        .unwrap();
        assert!(!yaml.contains("sensitive"));
    }
}
