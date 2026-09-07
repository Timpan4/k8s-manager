use crate::models::AppErrorKind;
use crate::models::{AppError, YamlEncoding, YamlViewMode};
use k8s_openapi::{ClusterResourceScope, NamespaceResourceScope};
use kube::{
    api::{Api, Resource},
    Client,
};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;

pub(crate) fn redact_secret(secret: &mut k8s_openapi::api::core::v1::Secret) {
    redact_secret_metadata(&mut secret.metadata);
    if let Some(ref mut data) = secret.data {
        for value in data.values_mut() {
            *value = k8s_openapi::ByteString(b"REDACTED".to_vec());
        }
    }
    if let Some(ref mut string_data) = secret.string_data {
        for value in string_data.values_mut() {
            *value = "REDACTED".to_string();
        }
    }
}

pub(crate) fn redact_secret_metadata(metadata: &mut kube::api::ObjectMeta) {
    if let Some(annotations) = &mut metadata.annotations {
        // kubectl stores the complete previous document, including Secret data.
        if let Some(value) = annotations.get_mut("kubectl.kubernetes.io/last-applied-configuration")
        {
            *value = "REDACTED".into();
        }
    }
}

pub(crate) fn normalize_k8s_yaml_value(value: &mut Value, mode: YamlViewMode) {
    if let Value::Object(root) = value {
        if let Some(Value::Object(metadata)) = root.get_mut("metadata") {
            metadata.remove("managedFields");
            if matches!(mode, YamlViewMode::ApplyClean) {
                metadata.remove("uid");
                metadata.remove("resourceVersion");
                metadata.remove("generation");
                metadata.remove("creationTimestamp");
                metadata.remove("selfLink");
            }
        }

        if matches!(mode, YamlViewMode::ApplyClean) {
            root.remove("status");
        }
    }
}

pub(crate) fn serialize_resource_document<T: Serialize>(
    resource: &T,
    mode: YamlViewMode,
    encoding: YamlEncoding,
) -> Result<String, AppError> {
    let mut value = serde_json::to_value(resource)
        .map_err(|e| AppError::new(e.to_string(), AppErrorKind::Serialization).with_source(e))?;
    normalize_k8s_yaml_value(&mut value, mode);
    serialize_json_value_document(&value, encoding)
}

pub(crate) fn serialize_json_value_document(
    value: &Value,
    encoding: YamlEncoding,
) -> Result<String, AppError> {
    match encoding {
        YamlEncoding::Yaml => serde_yaml::to_string(value)
            .map_err(|e| AppError::new(e.to_string(), AppErrorKind::Serialization).with_source(e)),
        YamlEncoding::Kyaml => Ok(format!("{}\n", format_kyaml_value(value, 0))),
    }
}

fn format_kyaml_value(value: &Value, indent: usize) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string()),
        Value::Array(values) => format_kyaml_array(values, indent),
        Value::Object(map) => format_kyaml_object(map, indent),
    }
}

fn format_kyaml_array(values: &[Value], indent: usize) -> String {
    if values.is_empty() {
        return "[]".to_string();
    }

    let child_indent = indent + 2;
    let closing = " ".repeat(indent);
    let lines = values
        .iter()
        .map(|value| {
            format!(
                "{}{},",
                " ".repeat(child_indent),
                format_kyaml_value(value, child_indent)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    format!("[\n{lines}\n{closing}]")
}

fn format_kyaml_object(map: &serde_json::Map<String, Value>, indent: usize) -> String {
    if map.is_empty() {
        return "{}".to_string();
    }

    let child_indent = indent + 2;
    let closing = " ".repeat(indent);
    let lines = map
        .iter()
        .map(|(key, value)| {
            format!(
                "{}{}: {},",
                " ".repeat(child_indent),
                format_kyaml_key(key),
                format_kyaml_value(value, child_indent)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");

    format!("{{\n{lines}\n{closing}}}")
}

fn format_kyaml_key(key: &str) -> String {
    if is_plain_kyaml_key(key) {
        key.to_string()
    } else {
        serde_json::to_string(key).unwrap_or_else(|_| "\"\"".to_string())
    }
}

fn is_plain_kyaml_key(key: &str) -> bool {
    let mut chars = key.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/'))
}

pub(crate) async fn fetch_and_serialize<
    T: Resource<Scope = NamespaceResourceScope>
        + Serialize
        + DeserializeOwned
        + Clone
        + std::fmt::Debug
        + Send
        + Sync,
>(
    client: Client,
    namespace: Option<&str>,
    name: &str,
) -> Result<(T, String), AppError>
where
    <T as Resource>::DynamicType: Default,
{
    fetch_and_serialize_with_mode(client, namespace, name, YamlViewMode::Kubectl).await
}

pub(crate) async fn fetch_and_serialize_with_mode<
    T: Resource<Scope = NamespaceResourceScope>
        + Serialize
        + DeserializeOwned
        + Clone
        + std::fmt::Debug
        + Send
        + Sync,
>(
    client: Client,
    namespace: Option<&str>,
    name: &str,
    mode: YamlViewMode,
) -> Result<(T, String), AppError>
where
    <T as Resource>::DynamicType: Default,
{
    fetch_and_serialize_with_encoding(client, namespace, name, mode, YamlEncoding::Yaml).await
}

pub(crate) async fn fetch_and_serialize_with_encoding<
    T: Resource<Scope = NamespaceResourceScope>
        + Serialize
        + DeserializeOwned
        + Clone
        + std::fmt::Debug
        + Send
        + Sync,
>(
    client: Client,
    namespace: Option<&str>,
    name: &str,
    mode: YamlViewMode,
    encoding: YamlEncoding,
) -> Result<(T, String), AppError>
where
    <T as Resource>::DynamicType: Default,
{
    super::validate_namespace(namespace)?;
    super::validate_path_segment(name, "resource name")?;
    let api: Api<T> = if let Some(ns) = namespace {
        Api::namespaced(client, ns)
    } else {
        return Err(AppError::new(
            "namespace is required for namespaced resources",
            AppErrorKind::Validation,
        ));
    };
    let resource = api.get(name).await.map_err(AppError::from)?;
    let yaml = serialize_resource_document(&resource, mode, encoding)?;
    Ok((resource, yaml))
}

pub(crate) async fn fetch_and_serialize_cluster<
    T: Resource<Scope = ClusterResourceScope>
        + Serialize
        + DeserializeOwned
        + Clone
        + std::fmt::Debug
        + Send
        + Sync,
>(
    client: Client,
    name: &str,
) -> Result<(T, String), AppError>
where
    <T as Resource>::DynamicType: Default,
{
    fetch_and_serialize_cluster_with_mode(client, name, YamlViewMode::Kubectl).await
}

pub(crate) async fn fetch_and_serialize_cluster_with_mode<
    T: Resource<Scope = ClusterResourceScope>
        + Serialize
        + DeserializeOwned
        + Clone
        + std::fmt::Debug
        + Send
        + Sync,
>(
    client: Client,
    name: &str,
    mode: YamlViewMode,
) -> Result<(T, String), AppError>
where
    <T as Resource>::DynamicType: Default,
{
    fetch_and_serialize_cluster_with_encoding(client, name, mode, YamlEncoding::Yaml).await
}

pub(crate) async fn fetch_and_serialize_cluster_with_encoding<
    T: Resource<Scope = ClusterResourceScope>
        + Serialize
        + DeserializeOwned
        + Clone
        + std::fmt::Debug
        + Send
        + Sync,
>(
    client: Client,
    name: &str,
    mode: YamlViewMode,
    encoding: YamlEncoding,
) -> Result<(T, String), AppError>
where
    <T as Resource>::DynamicType: Default,
{
    super::validate_path_segment(name, "resource name")?;
    let api: Api<T> = Api::all(client);
    let resource = api.get(name).await.map_err(AppError::from)?;
    let yaml = serialize_resource_document(&resource, mode, encoding)?;
    Ok((resource, yaml))
}

#[cfg(test)]
mod tests {
    use super::*;
    use k8s_openapi::api::core::v1::Secret;
    use k8s_openapi::ByteString;
    use std::collections::BTreeMap;

    #[test]
    fn redacts_secret_data_and_string_data() {
        let mut data = BTreeMap::new();
        data.insert("password".to_string(), ByteString(b"super-secret".to_vec()));
        let mut string_data = BTreeMap::new();
        string_data.insert("token".to_string(), "plain-secret".to_string());
        let mut secret = Secret {
            data: Some(data),
            string_data: Some(string_data),
            ..Default::default()
        };

        redact_secret(&mut secret);

        assert_eq!(
            secret.data.as_ref().unwrap().get("password").unwrap().0,
            b"REDACTED".to_vec()
        );
        assert_eq!(
            secret.string_data.as_ref().unwrap().get("token").unwrap(),
            "REDACTED"
        );
    }

    #[test]
    fn audit_secret_last_applied_annotation_is_redacted() {
        let mut secret = Secret::default();
        secret.metadata.annotations = Some(BTreeMap::from([
            (
                "kubectl.kubernetes.io/last-applied-configuration".into(),
                "sensitive".into(),
            ),
            ("note".into(), "keep".into()),
        ]));
        redact_secret(&mut secret);
        let serialized = serde_json::to_string(&secret).unwrap();
        assert!(!serialized.contains("sensitive"));
        assert!(serialized.contains("keep"));
    }

    #[test]
    fn kubectl_mode_strips_managed_fields_only() {
        let mut value = serde_json::json!({
            "apiVersion": "v1",
            "kind": "Service",
            "metadata": {
                "name": "redis",
                "namespace": "argocd",
                "managedFields": [{ "manager": "helm" }],
                "uid": "uid-1",
                "resourceVersion": "10",
                "creationTimestamp": "2026-05-20T12:31:12Z",
                "labels": { "app": "redis" },
                "ownerReferences": [{ "name": "owner" }],
                "finalizers": ["example.com/finalizer"]
            },
            "spec": { "ports": [{ "port": 6379 }] },
            "status": { "loadBalancer": {} }
        });

        normalize_k8s_yaml_value(&mut value, YamlViewMode::Kubectl);

        let metadata = value.get("metadata").unwrap();
        assert!(metadata.get("managedFields").is_none());
        assert_eq!(metadata.get("uid").unwrap(), "uid-1");
        assert!(value.get("status").is_some());
        assert!(metadata.get("ownerReferences").is_some());
        assert!(metadata.get("finalizers").is_some());
    }

    #[test]
    fn apply_clean_mode_strips_server_owned_metadata_and_status() {
        let mut value = serde_json::json!({
            "apiVersion": "apps/v1",
            "kind": "Deployment",
            "metadata": {
                "name": "api",
                "namespace": "default",
                "managedFields": [],
                "uid": "uid-1",
                "resourceVersion": "10",
                "generation": 4,
                "creationTimestamp": "2026-05-20T12:31:12Z",
                "selfLink": "/api/v1",
                "annotations": { "note": "keep" },
                "labels": { "app": "api" },
                "ownerReferences": [{ "name": "owner" }],
                "finalizers": ["example.com/finalizer"]
            },
            "spec": { "selector": { "matchLabels": { "app": "api" } } },
            "status": { "readyReplicas": 1 }
        });

        normalize_k8s_yaml_value(&mut value, YamlViewMode::ApplyClean);

        let metadata = value.get("metadata").unwrap();
        for key in [
            "managedFields",
            "uid",
            "resourceVersion",
            "generation",
            "creationTimestamp",
            "selfLink",
        ] {
            assert!(metadata.get(key).is_none(), "{key} should be stripped");
        }
        assert!(value.get("status").is_none());
        assert!(metadata.get("annotations").is_some());
        assert!(metadata.get("labels").is_some());
        assert!(metadata.get("ownerReferences").is_some());
        assert!(metadata.get("finalizers").is_some());
        assert!(value.get("spec").is_some());
    }

    #[test]
    fn kyaml_uses_bare_keys_and_quoted_strings() {
        let value = serde_json::json!({
            "apiVersion": "v1",
            "kind": "ConfigMap",
            "metadata": {
                "name": "app-config",
                "labels": {
                    "app.kubernetes.io/name": "api"
                }
            },
            "data": {
                "country": "NO",
                "enabled": "true",
                "answer": 42,
                "flag": true
            }
        });

        let kyaml = serialize_json_value_document(&value, YamlEncoding::Kyaml).unwrap();

        assert!(kyaml.contains("apiVersion: \"v1\""));
        assert!(kyaml.contains("app.kubernetes.io/name: \"api\""));
        assert!(kyaml.contains("country: \"NO\""));
        assert!(kyaml.contains("enabled: \"true\""));
        assert!(kyaml.contains("answer: 42"));
        assert!(kyaml.contains("flag: true"));
        assert!(!kyaml.contains("\"apiVersion\":"));
    }

    #[test]
    fn kyaml_parses_back_to_equivalent_json() {
        let value = serde_json::json!({
            "apiVersion": "apps/v1",
            "kind": "Deployment",
            "metadata": {
                "name": "api",
                "namespace": "default",
                "managedFields": [{ "manager": "kubecove" }],
                "resourceVersion": "12"
            },
            "spec": {
                "replicas": 2,
                "template": {
                    "spec": {
                        "containers": [{ "name": "api", "image": "nginx:1.27" }]
                    }
                }
            },
            "status": { "readyReplicas": 1 }
        });
        let mut expected = value.clone();
        normalize_k8s_yaml_value(&mut expected, YamlViewMode::ApplyClean);

        let kyaml = serialize_json_value_document(&expected, YamlEncoding::Kyaml).unwrap();
        let parsed: serde_json::Value =
            serde_json::to_value(serde_yaml::from_str::<serde_yaml::Value>(&kyaml).unwrap())
                .unwrap();

        assert_eq!(parsed, expected);
        assert!(parsed.get("status").is_none());
        assert!(parsed.pointer("/metadata/resourceVersion").is_none());
    }
}
