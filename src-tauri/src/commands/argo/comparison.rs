use super::scope::scoped_connection;
use super::{
    connected::{
        api_get, connected_application_path, inspector_from_application, kubernetes_application,
        managed_resource, redact_secret_fields, state, text,
    },
    ArgoConnectionStore,
};
use crate::models::AppErrorKind;
use crate::{
    commands::BackendCancellationRegistry,
    models::{AppError, ArgoApplicationRef, ArgoManagedResource, ArgoResourceComparison},
};
use serde_json::Value;

async fn argo_resource_comparison(
    store: &ArgoConnectionStore,
    cluster_context: String,
    kubeconfig_env_var: Option<String>,
    connection_id: Option<String>,
    transport: String,
    application: ArgoApplicationRef,
    resource: ArgoManagedResource,
) -> Result<ArgoResourceComparison, AppError> {
    if transport == "kubernetes" {
        let inspector = inspector_from_application(
            &kubernetes_application(
                &cluster_context,
                application.namespace.as_deref(),
                &application.name,
                kubeconfig_env_var,
            )
            .await?,
        )?;
        let resource = inspector
            .resources
            .into_iter()
            .find(|candidate| {
                candidate.group == resource.group
                    && candidate.version == resource.version
                    && candidate.kind == resource.kind
                    && candidate.namespace == resource.namespace
                    && candidate.name == resource.name
            })
            .ok_or_else(|| AppError::new("managed resource not found", AppErrorKind::NotFound))?;
        return Ok(ArgoResourceComparison {
            resource,
            exact: Some(false),
            provenance: Some("kubernetes-status-no-diff".into()),
            ..Default::default()
        });
    }
    if transport != "connected" {
        return Err(AppError::new(
            "invalid Argo CD transport",
            AppErrorKind::ArgoConnection,
        ));
    }
    let connection = scoped_connection(
        store,
        &connection_id.ok_or_else(|| {
            AppError::new("Argo CD connection required", AppErrorKind::ArgoConnection)
        })?,
        &cluster_context,
        application.workspace_id.as_deref(),
        kubeconfig_env_var.as_deref(),
    )?;
    let mut value = api_get(
        &connection,
        &connected_application_path(
            &connection.profile.url,
            &application.name,
            application.namespace.as_deref(),
            None,
            true,
        )?,
    )
    .await?;
    redact_secret_fields(&mut value);
    let item = value
        .get("items")
        .or_else(|| value.get("managedResources"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .find(|item| {
            text(item, "group") == resource.group
                && text(item, "version") == resource.version
                && text(item, "kind") == resource.kind
                && text(item, "namespace") == resource.namespace
                && text(item, "name") == resource.name
        })
        .ok_or_else(|| AppError::new("managed resource not found", AppErrorKind::NotFound))?;
    let resource = managed_resource(item);
    let available_actions = match actions_path(&application, &resource) {
        Some(path) => api_get(&connection, &path)
            .await?
            .get("actions")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        None => vec![],
    };
    Ok(ArgoResourceComparison {
        resource,
        target_state: state(item.get("targetState"), true),
        live_state: state(item.get("liveState"), true),
        normalized_live_state: state(item.get("normalizedLiveState"), true),
        predicted_live_state: state(item.get("predictedLiveState"), true),
        modified: item.get("modified").and_then(Value::as_bool),
        exact: Some(true),
        provenance: Some("argocd-managed-resource".into()),
        available_actions,
    })
}

#[tauri::command]
pub async fn get_argo_resource_comparison(
    store: tauri::State<'_, ArgoConnectionStore>,
    cancellations: tauri::State<'_, BackendCancellationRegistry>,
    cluster_context: String,
    kubeconfig_env_var: Option<String>,
    connection_id: Option<String>,
    transport: String,
    application: ArgoApplicationRef,
    resource: ArgoManagedResource,
    _redact_secrets: Option<bool>,
    request_id: Option<String>,
    cancel_scope: Option<String>,
) -> Result<ArgoResourceComparison, AppError> {
    cancellations
        .execute(
            cancel_scope,
            request_id,
            argo_resource_comparison(
                &store,
                cluster_context,
                kubeconfig_env_var,
                connection_id,
                transport,
                application,
                resource,
            ),
        )
        .await
}

fn required_identity(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.trim().is_empty())
}

fn actions_path(
    application: &ArgoApplicationRef,
    resource: &ArgoManagedResource,
) -> Option<String> {
    let version = required_identity(resource.version.as_deref())?;
    let kind = required_identity(resource.kind.as_deref())?;
    let name = required_identity(resource.name.as_deref())?;
    let mut url =
        reqwest::Url::parse("https://argo.invalid/api/v1/applications").expect("static URL");
    url.path_segments_mut()
        .expect("static URL")
        .push(&application.name);
    url.set_path(&format!("{}/resource/actions", url.path()));
    let mut query = url.query_pairs_mut();
    for (key, value) in [
        ("appNamespace", application.namespace.as_deref()),
        ("project", application.project.as_deref()),
        ("group", resource.group.as_deref()),
        ("version", Some(version)),
        ("kind", Some(kind)),
        ("namespace", resource.namespace.as_deref()),
        ("resourceName", Some(name)),
    ] {
        if let Some(value) = value {
            query.append_pair(key, value);
        }
    }
    drop(query);
    Some(format!(
        "{}?{}",
        url.path(),
        url.query().unwrap_or_default()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actions_path_encodes_application_and_resource_identity() {
        let path = actions_path(
            &ArgoApplicationRef {
                name: "app/a?#".into(),
                namespace: Some("app ns/&?#".into()),
                project: Some("project/&?#".into()),
                ..Default::default()
            },
            &ArgoManagedResource {
                group: Some("apps/&?#".into()),
                version: Some("v1/?.".into()),
                kind: Some("Deployment/#".into()),
                namespace: Some("ns/&?#".into()),
                name: Some("resource/&?#".into()),
                ..Default::default()
            },
        );
        assert_eq!(
            path.as_deref(),
            Some("/api/v1/applications/app%2Fa%3F%23/resource/actions?appNamespace=app+ns%2F%26%3F%23&project=project%2F%26%3F%23&group=apps%2F%26%3F%23&version=v1%2F%3F.&kind=Deployment%2F%23&namespace=ns%2F%26%3F%23&resourceName=resource%2F%26%3F%23")
        );
    }

    #[test]
    fn actions_path_requires_server_action_identity() {
        let application = ArgoApplicationRef {
            name: "app".into(),
            ..Default::default()
        };
        let complete = ArgoManagedResource {
            version: Some("v1".into()),
            kind: Some("ConfigMap".into()),
            name: Some("settings".into()),
            ..Default::default()
        };

        assert!(actions_path(&application, &complete).is_some());
        for incomplete in [
            ArgoManagedResource {
                version: None,
                ..complete.clone()
            },
            ArgoManagedResource {
                kind: Some(" ".into()),
                ..complete.clone()
            },
            ArgoManagedResource {
                name: Some(String::new()),
                ..complete.clone()
            },
        ] {
            assert!(actions_path(&application, &incomplete).is_none());
        }
    }
}
