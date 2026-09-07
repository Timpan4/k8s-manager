use crate::commands::helpers::{list_params, serialize_resource_document};
use crate::models::AppErrorKind;
use crate::{commands::kubeconfig::KubeconfigSource, models::AppError};
use kube::{
    api::{Api, ApiResource, DynamicObject},
    discovery::Discovery,
    Client,
};
use serde_json::Value;

use crate::models::{YamlEncoding, YamlViewMode};

pub(crate) async fn client_for_context(
    cluster_context: &str,
    kubeconfig_env_var: Option<String>,
) -> Result<Client, AppError> {
    let source = KubeconfigSource::new(kubeconfig_env_var)?;
    source.client_for_context(cluster_context).await
}

pub(crate) async fn discover_api_resources(client: &Client) -> Result<Vec<ApiResource>, AppError> {
    let discovery = Discovery::new(client.clone())
        .run_aggregated()
        .await
        .map_err(AppError::from)?;
    let mut resources = Vec::new();
    for group in discovery.groups() {
        for (api_resource, _) in group.recommended_resources() {
            resources.push(api_resource.clone());
        }
    }
    Ok(resources)
}

pub(crate) fn find_discovered_api_resource(
    resources: &[ApiResource],
    group: &str,
    kind: &str,
) -> Option<ApiResource> {
    resources
        .iter()
        .find(|resource| resource.group == group && resource.kind == kind)
        .cloned()
}

pub(crate) async fn find_api_resource(
    client: &Client,
    group: &str,
    kind: &str,
) -> Result<Option<ApiResource>, AppError> {
    let resources = discover_api_resources(client).await?;
    Ok(find_discovered_api_resource(&resources, group, kind))
}

pub(crate) async fn list_crd_objects(
    client: Client,
    api_resource: &ApiResource,
) -> Result<Vec<DynamicObject>, AppError> {
    crate::commands::helpers::validate_api_resource(api_resource)?;
    Api::<DynamicObject>::all_with(client, api_resource)
        .list(&list_params())
        .await
        .map(|list| list.items)
        .map_err(AppError::from)
}

pub(crate) async fn get_crd_object(
    client: Client,
    api_resource: &ApiResource,
    name: &str,
    namespace: Option<&str>,
) -> Result<DynamicObject, AppError> {
    crate::commands::helpers::validate_namespace(namespace)?;
    crate::commands::helpers::validate_path_segment(name, "resource name")?;
    crate::commands::helpers::validate_api_resource(api_resource)?;
    dynamic_api(client, api_resource, namespace)
        .get(name)
        .await
        .map_err(AppError::from)
}

fn dynamic_api(
    client: Client,
    api_resource: &ApiResource,
    namespace: Option<&str>,
) -> Api<DynamicObject> {
    match namespace {
        Some(namespace) => Api::namespaced_with(client, namespace, api_resource),
        None => Api::all_with(client, api_resource),
    }
}

pub(crate) fn resource_yaml(
    object: &DynamicObject,
    yaml_view_mode: Option<YamlViewMode>,
    yaml_encoding: Option<YamlEncoding>,
) -> Result<String, AppError> {
    serialize_resource_document(
        object,
        yaml_view_mode.unwrap_or_default(),
        yaml_encoding.unwrap_or_default(),
    )
}

pub(crate) fn resource_metadata(object: &DynamicObject) -> Result<Value, AppError> {
    serde_json::to_value(&object.metadata)
        .map_err(|err| AppError::new(err.to_string(), AppErrorKind::Serialization).with_source(err))
}

pub(crate) fn resource_status(object: &DynamicObject) -> Option<Value> {
    object.data.get("status").cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kube::core::ObjectMeta;
    use serde_json::json;

    fn test_api_resource(group: &str, kind: &str) -> ApiResource {
        ApiResource {
            group: group.to_string(),
            version: "v1".to_string(),
            api_version: if group.is_empty() {
                "v1".to_string()
            } else {
                format!("{group}/v1")
            },
            kind: kind.to_string(),
            plural: format!("{}s", kind.to_ascii_lowercase()),
        }
    }

    #[test]
    fn finds_discovered_resource_by_group_and_kind() {
        let resources = vec![
            test_api_resource("source.toolkit.fluxcd.io", "GitRepository"),
            test_api_resource("argoproj.io", "Application"),
        ];

        assert!(find_discovered_api_resource(&resources, "argoproj.io", "Application").is_some());
        assert!(find_discovered_api_resource(&resources, "argoproj.io", "Missing").is_none());
    }

    #[test]
    fn serializes_metadata_and_status_from_dynamic_object() {
        let mut object =
            DynamicObject::new("demo", &test_api_resource("argoproj.io", "Application"));
        object.metadata = ObjectMeta {
            namespace: Some("argocd".to_string()),
            ..Default::default()
        };
        object.data = json!({
            "status": { "health": { "status": "Healthy" } }
        });

        assert_eq!(
            resource_metadata(&object)
                .expect("metadata")
                .get("namespace")
                .and_then(Value::as_str),
            Some("argocd")
        );
        assert_eq!(
            resource_status(&object)
                .and_then(|status| status.get("health").cloned())
                .and_then(|health| health.get("status").cloned())
                .and_then(|status| status.as_str().map(str::to_string)),
            Some("Healthy".to_string())
        );
    }
}
