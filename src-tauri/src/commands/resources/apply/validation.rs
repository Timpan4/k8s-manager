use crate::models::AppErrorKind;
use crate::models::{AppError, YamlApplyRequest, YamlApplyTarget};
use kube::api::ApiResource;
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug)]
pub(super) struct ValidatedApply {
    pub(super) request: YamlApplyRequest,
    pub(super) manifest: Value,
    pub(super) api_resource: ApiResource,
    pub(super) namespaced: bool,
    pub(super) target: YamlApplyTarget,
}

pub(super) fn validate_yaml_apply(request: YamlApplyRequest) -> Result<ValidatedApply, AppError> {
    crate::commands::helpers::validate_path_segment(&request.name, "name")?;
    crate::commands::helpers::validate_namespace(request.namespace.as_deref())?;
    let expected_api_version = request_api_version(&request)?;
    if expected_api_version == "v1"
        && (request.kind == "Secret" || request.plural.as_deref() == Some("secrets"))
    {
        return Err(AppError::new(
            "YAML apply is disabled for v1 Secrets because redacted values can corrupt data",
            AppErrorKind::Validation,
        ));
    }

    let manifest = parse_single_document(&request.yaml)?;
    let api_version = string_field(&manifest, "apiVersion")?;
    let kind = string_field(&manifest, "kind")?;
    let metadata = manifest
        .get("metadata")
        .and_then(Value::as_object)
        .ok_or_else(|| AppError::new("metadata is required", AppErrorKind::Validation))?;
    let name = metadata
        .get("name")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| AppError::new("metadata.name is required", AppErrorKind::Validation))?;
    let manifest_namespace = metadata
        .get("namespace")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty());

    if api_version != expected_api_version {
        return Err(identity_error(
            "apiVersion",
            &expected_api_version,
            api_version,
        ));
    }
    if kind != request.kind {
        return Err(identity_error("kind", &request.kind, kind));
    }
    if name != request.name {
        return Err(identity_error("metadata.name", &request.name, name));
    }

    let (api_resource, namespaced) = api_resource_for_request(&request, api_version)?;
    crate::commands::helpers::validate_api_resource(&api_resource)?;
    if namespaced {
        let expected_namespace = request.namespace.as_deref().ok_or_else(|| {
            AppError::new(
                "metadata.namespace is required for namespaced resources",
                AppErrorKind::Validation,
            )
        })?;
        let Some(actual_namespace) = manifest_namespace else {
            return Err(AppError::new(
                "metadata.namespace is required for namespaced resources",
                AppErrorKind::Validation,
            ));
        };
        if actual_namespace != expected_namespace {
            return Err(identity_error(
                "metadata.namespace",
                expected_namespace,
                actual_namespace,
            ));
        }
    } else if let Some(actual_namespace) = manifest_namespace {
        return Err(AppError::new(
            format!(
                "cluster-scoped {} must not include metadata.namespace ({actual_namespace})",
                request.kind
            ),
            AppErrorKind::Validation,
        ));
    }

    let target = YamlApplyTarget {
        cluster_context: request.cluster_context.clone(),
        kind: request.kind.clone(),
        api_version: Some(api_version.to_string()),
        name: request.name.clone(),
        namespace: request.namespace.clone(),
    };

    Ok(ValidatedApply {
        request,
        manifest,
        api_resource,
        namespaced,
        target,
    })
}

pub(super) fn parse_single_document(yaml: &str) -> Result<Value, AppError> {
    let mut manifest = None;
    for document in serde_yaml::Deserializer::from_str(yaml) {
        let yaml_value = serde_yaml::Value::deserialize(document)
            .map_err(|e| AppError::new(e.to_string(), AppErrorKind::Validation).with_source(e))?;
        if matches!(yaml_value, serde_yaml::Value::Null) {
            continue;
        }
        if manifest.replace(yaml_value).is_some() {
            return Err(AppError::new(
                "YAML apply accepts exactly one document",
                AppErrorKind::Validation,
            ));
        }
    }

    match manifest {
        None => Err(AppError::new(
            "YAML document is empty",
            AppErrorKind::Validation,
        )),
        Some(manifest) => serde_json::to_value(manifest)
            .map_err(|e| AppError::new(e.to_string(), AppErrorKind::Validation).with_source(e)),
    }
}

fn string_field<'a>(manifest: &'a Value, key: &str) -> Result<&'a str, AppError> {
    manifest
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| AppError::new(format!("{key} is required"), AppErrorKind::Validation))
}

pub(super) fn request_api_version(request: &YamlApplyRequest) -> Result<String, AppError> {
    if let Some(api_version) = request
        .api_version
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        return Ok(api_version.to_string());
    }
    match request
        .group
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        Some(group) => Ok(format!(
            "{group}/{}",
            request
                .version
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| AppError::new("version is required", AppErrorKind::Validation))?
        )),
        None => request
            .version
            .clone()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| builtin_api_version_for_kind(&request.kind).map(str::to_string))
            .ok_or_else(|| AppError::new("apiVersion is required", AppErrorKind::Validation)),
    }
}

fn builtin_api_version_for_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "Pod"
        | "Service"
        | "ConfigMap"
        | "Secret"
        | "PersistentVolumeClaim"
        | "Node"
        | "PersistentVolume" => Some("v1"),
        "Deployment" | "StatefulSet" | "DaemonSet" => Some("apps/v1"),
        "Ingress" => Some("networking.k8s.io/v1"),
        "Job" | "CronJob" => Some("batch/v1"),
        "StorageClass" => Some("storage.k8s.io/v1"),
        _ => None,
    }
}

fn api_resource_for_request(
    request: &YamlApplyRequest,
    api_version: &str,
) -> Result<(ApiResource, bool), AppError> {
    if let Some(plural) = request
        .plural
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        let namespaced = request
            .namespaced
            .ok_or_else(|| AppError::new("namespaced is required", AppErrorKind::Validation))?;
        let (group, version) = split_api_version(api_version);
        return Ok((
            ApiResource {
                group,
                version,
                api_version: api_version.to_string(),
                kind: request.kind.clone(),
                plural: plural.to_string(),
            },
            namespaced,
        ));
    }

    builtin_api_resource(&request.kind, api_version)
}

fn builtin_api_resource(kind: &str, api_version: &str) -> Result<(ApiResource, bool), AppError> {
    let (group, version, plural, namespaced) = match (kind, api_version) {
        ("Pod", "v1") => ("", "v1", "pods", true),
        ("Service", "v1") => ("", "v1", "services", true),
        ("ConfigMap", "v1") => ("", "v1", "configmaps", true),
        ("Secret", "v1") => ("", "v1", "secrets", true),
        ("PersistentVolumeClaim", "v1") => ("", "v1", "persistentvolumeclaims", true),
        ("Node", "v1") => ("", "v1", "nodes", false),
        ("PersistentVolume", "v1") => ("", "v1", "persistentvolumes", false),
        ("Deployment", "apps/v1") => ("apps", "v1", "deployments", true),
        ("StatefulSet", "apps/v1") => ("apps", "v1", "statefulsets", true),
        ("DaemonSet", "apps/v1") => ("apps", "v1", "daemonsets", true),
        ("Ingress", "networking.k8s.io/v1") => ("networking.k8s.io", "v1", "ingresses", true),
        ("Job", "batch/v1") => ("batch", "v1", "jobs", true),
        ("CronJob", "batch/v1") => ("batch", "v1", "cronjobs", true),
        ("StorageClass", "storage.k8s.io/v1") => ("storage.k8s.io", "v1", "storageclasses", false),
        _ => {
            return Err(AppError::new(
                format!("unsupported apply target: {api_version} {kind}"),
                AppErrorKind::Validation,
            ));
        }
    };

    Ok((
        ApiResource {
            group: group.to_string(),
            version: version.to_string(),
            api_version: api_version.to_string(),
            kind: kind.to_string(),
            plural: plural.to_string(),
        },
        namespaced,
    ))
}

fn split_api_version(api_version: &str) -> (String, String) {
    api_version.split_once('/').map_or_else(
        || (String::new(), api_version.to_string()),
        |(group, version)| (group.to_string(), version.to_string()),
    )
}

fn identity_error(field: &str, expected: &str, actual: &str) -> AppError {
    AppError::new(
        format!("{field} must match selected resource: expected {expected}, got {actual}"),
        AppErrorKind::Validation,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_request(yaml: &str) -> YamlApplyRequest {
        YamlApplyRequest {
            cluster_context: "kind-kind".to_string(),
            kubeconfig_env_var: None,
            kind: "Service".to_string(),
            api_version: Some("v1".to_string()),
            group: None,
            version: None,
            plural: None,
            namespaced: Some(true),
            name: "api".to_string(),
            namespace: Some("default".to_string()),
            yaml: yaml.to_string(),
            yaml_encoding: crate::models::YamlEncoding::default(),
            force_conflicts: false,
        }
    }

    #[test]
    fn rejects_multi_document_yaml() {
        let err = validate_yaml_apply(base_request(
            "apiVersion: v1\nkind: Service\nmetadata:\n  name: api\n  namespace: default\n---\napiVersion: v1\nkind: Service\nmetadata:\n  name: other\n  namespace: default\n",
        ))
        .unwrap_err();

        assert_eq!(err.kind, AppErrorKind::Validation);
        assert!(err.message.contains("exactly one document"));
    }

    #[test]
    fn rejects_identity_mismatch() {
        let err = validate_yaml_apply(base_request(
            "apiVersion: v1\nkind: Service\nmetadata:\n  name: other\n  namespace: default\n",
        ))
        .unwrap_err();

        assert_eq!(err.kind, AppErrorKind::Validation);
        assert!(err.message.contains("metadata.name"));
    }

    #[test]
    fn rejects_missing_namespace_for_namespaced_resource() {
        let err = validate_yaml_apply(base_request(
            "apiVersion: v1\nkind: Service\nmetadata:\n  name: api\n",
        ))
        .unwrap_err();

        assert_eq!(err.kind, AppErrorKind::Validation);
        assert!(err.message.contains("metadata.namespace"));
    }

    #[test]
    fn rejects_v1_secret_apply() {
        let mut request = base_request(
            "apiVersion: v1\nkind: Secret\nmetadata:\n  name: api\n  namespace: default\n",
        );
        request.kind = "Secret".to_string();
        request.name = "api".to_string();

        let err = validate_yaml_apply(request).unwrap_err();

        assert_eq!(err.kind, AppErrorKind::Validation);
        assert!(err.message.contains("Secrets"));
    }

    #[test]
    fn audit_secret_apply_rejects_inferred_api_version() {
        let mut request = base_request(
            "apiVersion: v1\nkind: Secret\nmetadata:\n  name: api\n  namespace: default\n",
        );
        request.kind = "Secret".into();
        request.api_version = Some(" ".into());
        request.version = Some("v1".into());
        assert!(validate_yaml_apply(request).is_err());
    }

    #[test]
    fn accepts_matching_selected_resource() {
        let validated = validate_yaml_apply(base_request(
            "apiVersion: v1\nkind: Service\nmetadata:\n  name: api\n  namespace: default\nspec:\n  selector:\n    app: api\n",
        ))
        .unwrap();

        assert_eq!(validated.target.kind, "Service");
        assert!(validated.namespaced);
        assert_eq!(validated.api_resource.plural, "services");
    }

    #[test]
    fn apply_target_cannot_redirect_the_resource_path() {
        let mut request = base_request(
            "apiVersion: v1\nkind: Service\nmetadata:\n  name: api\n  namespace: default\n",
        );
        request.plural = Some("secrets/token?ignored=".into());
        assert!(validate_yaml_apply(request).is_err());
    }

    #[test]
    fn preserves_force_conflicts_request_flag() {
        let mut request = base_request(
            "apiVersion: v1\nkind: Service\nmetadata:\n  name: api\n  namespace: default\n",
        );
        request.force_conflicts = true;

        let validated = validate_yaml_apply(request).unwrap();

        assert!(validated.request.force_conflicts);
    }
}
