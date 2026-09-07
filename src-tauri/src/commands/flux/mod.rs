use crate::commands::gitops_crd::{
    client_for_context, discover_api_resources, find_discovered_api_resource, get_crd_object,
    list_crd_objects, resource_metadata, resource_status, resource_yaml,
};
use crate::commands::helpers::{k8s_creation_timestamp_to_rfc3339, resource_age};
use crate::models::AppErrorKind;
use crate::models::{
    evaluate_health, AppError, FluxDetectionSummary, FluxInventoryResource, FluxResourceDetails,
    FluxResourceKind, FluxResourceSummary, HealthAssessment, HealthAssessmentEvidence,
    HealthAssessmentInput, HealthAssessmentSource, HealthAssessmentState, YamlEncoding,
    YamlViewMode,
};
use chrono::{TimeZone, Utc};
use kube::api::{ApiResource, DynamicObject};
use kube::Client;
use serde_json::Value;

pub(crate) const MAX_FLUX_INVENTORY_ENTRIES: usize = 2048;

pub(crate) const FLUX_KINDS: &[(&str, &str, &str, &str, bool, &str)] = &[
    (
        "source.toolkit.fluxcd.io",
        "v1",
        "GitRepository",
        "gitrepositories",
        true,
        "Sources",
    ),
    (
        "source.toolkit.fluxcd.io",
        "v1",
        "OCIRepository",
        "ocirepositories",
        true,
        "Sources",
    ),
    (
        "source.toolkit.fluxcd.io",
        "v1",
        "HelmRepository",
        "helmrepositories",
        true,
        "Sources",
    ),
    (
        "source.toolkit.fluxcd.io",
        "v1",
        "HelmChart",
        "helmcharts",
        true,
        "Sources",
    ),
    (
        "source.toolkit.fluxcd.io",
        "v1",
        "Bucket",
        "buckets",
        true,
        "Sources",
    ),
    (
        "kustomize.toolkit.fluxcd.io",
        "v1",
        "Kustomization",
        "kustomizations",
        true,
        "Kustomize",
    ),
    (
        "helm.toolkit.fluxcd.io",
        "v2",
        "HelmRelease",
        "helmreleases",
        true,
        "Helm",
    ),
    (
        "notification.toolkit.fluxcd.io",
        "v1beta3",
        "Provider",
        "providers",
        true,
        "Notifications",
    ),
    (
        "notification.toolkit.fluxcd.io",
        "v1beta3",
        "Alert",
        "alerts",
        true,
        "Notifications",
    ),
    (
        "notification.toolkit.fluxcd.io",
        "v1",
        "Receiver",
        "receivers",
        true,
        "Notifications",
    ),
    (
        "image.toolkit.fluxcd.io",
        "v1",
        "ImageRepository",
        "imagerepositories",
        true,
        "Image Automation",
    ),
    (
        "image.toolkit.fluxcd.io",
        "v1",
        "ImagePolicy",
        "imagepolicies",
        true,
        "Image Automation",
    ),
    (
        "image.toolkit.fluxcd.io",
        "v1",
        "ImageUpdateAutomation",
        "imageupdateautomations",
        true,
        "Image Automation",
    ),
];

#[tauri::command]
pub async fn detect_flux(
    cluster_context: String,
    kubeconfig_env_var: Option<String>,
) -> Result<FluxDetectionSummary, AppError> {
    let client = client_for_context(&cluster_context, kubeconfig_env_var).await?;
    let installed = installed_flux_kinds(&client).await?;
    let missing_kinds = flux_kinds()
        .into_iter()
        .filter(|kind| {
            !installed
                .iter()
                .any(|candidate| same_flux_kind(candidate, kind))
        })
        .collect::<Vec<_>>();

    Ok(FluxDetectionSummary {
        detected: !installed.is_empty(),
        kinds: installed,
        missing_kinds,
    })
}

#[tauri::command]
pub async fn list_flux_resources(
    cluster_context: String,
    resource_kind: FluxResourceKind,
    kubeconfig_env_var: Option<String>,
) -> Result<Vec<FluxResourceSummary>, AppError> {
    let client = client_for_context(&cluster_context, kubeconfig_env_var).await?;
    let Some(resource_kind) = installed_flux_kind(&client, &resource_kind).await? else {
        return Ok(vec![]);
    };
    let api_resource = api_resource_from_flux_kind(&resource_kind);
    let items = list_crd_objects(client, &api_resource).await?;

    Ok(items
        .iter()
        .map(|object| flux_summary(&cluster_context, &resource_kind, object))
        .collect())
}

#[tauri::command]
pub async fn get_flux_resource_details(
    cluster_context: String,
    resource_kind: FluxResourceKind,
    name: String,
    namespace: Option<String>,
    kubeconfig_env_var: Option<String>,
    yaml_view_mode: Option<YamlViewMode>,
    yaml_encoding: Option<YamlEncoding>,
) -> Result<FluxResourceDetails, AppError> {
    let client = client_for_context(&cluster_context, kubeconfig_env_var).await?;
    let resource_kind = installed_flux_kind(&client, &resource_kind)
        .await?
        .ok_or_else(|| AppError::new("Flux resource kind not found", AppErrorKind::Cluster))?;
    let api_resource = api_resource_from_flux_kind(&resource_kind);
    let namespace = if resource_kind.namespaced {
        Some(namespace.as_deref().ok_or_else(|| {
            AppError::new(
                "Namespace required for namespaced Flux resource",
                AppErrorKind::Validation,
            )
        })?)
    } else {
        None
    };
    let object = get_crd_object(client, &api_resource, &name, namespace).await?;

    let yaml = resource_yaml(&object, yaml_view_mode, yaml_encoding)?;
    let metadata = resource_metadata(&object)?;
    let summary = flux_summary(&cluster_context, &resource_kind, &object);

    Ok(FluxResourceDetails {
        summary,
        yaml,
        metadata,
        status: bounded_status(&object),
    })
}

pub(crate) fn flux_kinds() -> Vec<FluxResourceKind> {
    FLUX_KINDS
        .iter()
        .map(
            |(group, version, kind, plural, namespaced, category)| FluxResourceKind {
                group: (*group).to_string(),
                version: (*version).to_string(),
                api_version: format!("{group}/{version}"),
                kind: (*kind).to_string(),
                plural: (*plural).to_string(),
                namespaced: *namespaced,
                category: (*category).to_string(),
            },
        )
        .collect()
}

fn installed_flux_kinds_from_resources(resources: &[ApiResource]) -> Vec<FluxResourceKind> {
    flux_kinds()
        .into_iter()
        .filter_map(|catalog_kind| {
            let resource =
                find_discovered_api_resource(resources, &catalog_kind.group, &catalog_kind.kind)?;
            Some(FluxResourceKind {
                group: resource.group,
                version: resource.version,
                api_version: resource.api_version,
                kind: resource.kind,
                plural: resource.plural,
                namespaced: catalog_kind.namespaced,
                category: catalog_kind.category,
            })
        })
        .collect()
}

async fn installed_flux_kinds(client: &Client) -> Result<Vec<FluxResourceKind>, AppError> {
    let resources = discover_api_resources(client).await?;
    Ok(installed_flux_kinds_from_resources(&resources))
}

async fn installed_flux_kind(
    client: &Client,
    kind: &FluxResourceKind,
) -> Result<Option<FluxResourceKind>, AppError> {
    Ok(installed_flux_kinds(client)
        .await?
        .into_iter()
        .find(|candidate| same_flux_kind(candidate, kind)))
}

fn same_flux_kind(left: &FluxResourceKind, right: &FluxResourceKind) -> bool {
    left.group == right.group && left.kind == right.kind
}

fn api_resource_from_flux_kind(kind: &FluxResourceKind) -> ApiResource {
    ApiResource {
        group: kind.group.clone(),
        version: kind.version.clone(),
        api_version: kind.api_version.clone(),
        kind: kind.kind.clone(),
        plural: kind.plural.clone(),
    }
}

pub(crate) fn ready_status(data: &Value) -> Option<String> {
    data.get("status")
        .and_then(|status| status.get("conditions"))
        .and_then(Value::as_array)?
        .iter()
        .find(|condition| {
            condition
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|value| value == "Ready")
        })
        .and_then(|condition| {
            condition
                .get("status")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| {
                    condition
                        .get("reason")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                })
        })
}

fn bounded_status(object: &DynamicObject) -> Option<Value> {
    let mut status = resource_status(object)?;
    if let Some(entries) = status
        .get_mut("inventory")
        .and_then(|inventory| inventory.get_mut("entries"))
        .and_then(Value::as_array_mut)
    {
        entries.truncate(MAX_FLUX_INVENTORY_ENTRIES);
    }
    Some(status)
}

pub(crate) fn inventory(data: &Value) -> Vec<FluxInventoryResource> {
    data.get("status")
        .and_then(|status| status.get("inventory"))
        .and_then(|inventory| inventory.get("entries"))
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .take(MAX_FLUX_INVENTORY_ENTRIES)
                .filter_map(|entry| {
                    let id = entry.get("id").and_then(Value::as_str)?.trim();
                    (!id.is_empty()).then(|| FluxInventoryResource {
                        id: id.to_string(),
                        version: entry
                            .get("v")
                            .or_else(|| entry.get("version"))
                            .and_then(Value::as_str)
                            .map(str::to_string),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn inventory_partial(data: &Value) -> bool {
    data.get("status")
        .and_then(|status| status.get("inventory"))
        .and_then(|inventory| inventory.get("entries"))
        .and_then(Value::as_array)
        .is_some_and(|entries| entries.len() > MAX_FLUX_INVENTORY_ENTRIES)
}

fn flux_summary(
    cluster_context: &str,
    resource_kind: &FluxResourceKind,
    object: &DynamicObject,
) -> FluxResourceSummary {
    let ready_status = ready_status(&object.data);
    FluxResourceSummary {
        cluster: cluster_context.to_string(),
        name: object.metadata.name.clone().unwrap_or_default(),
        namespace: object.metadata.namespace.clone(),
        age: resource_age(object.metadata.creation_timestamp.clone().map(|timestamp| {
            Utc.timestamp_opt(timestamp.0.as_second(), 0)
                .single()
                .unwrap_or_else(Utc::now)
        })),
        created_at: k8s_creation_timestamp_to_rfc3339(&object.metadata.creation_timestamp),
        resource_kind: resource_kind.clone(),
        health_assessment: flux_health_assessment(ready_status.as_deref()),
        ready_status,
        suspended: object
            .data
            .get("spec")
            .and_then(|spec| spec.get("suspend"))
            .and_then(Value::as_bool),
        source_kind: source_field(&object.data, "kind"),
        source_name: source_field(&object.data, "name"),
        source_namespace: source_field(&object.data, "namespace"),
        interval: object
            .data
            .get("spec")
            .and_then(|spec| spec.get("interval"))
            .and_then(Value::as_str)
            .map(str::to_string),
        last_applied_revision: revision(&object.data),
        message: ready_message(&object.data),
        inventory: inventory(&object.data),
        inventory_partial: inventory_partial(&object.data),
    }
}

fn flux_health_assessment(ready_status: Option<&str>) -> HealthAssessment {
    let evidence = ready_status
        .map(|status| HealthAssessmentEvidence {
            source: HealthAssessmentSource::Kubernetes,
            raw: Value::String(status.to_string()),
            state: Some(match status {
                "True" => HealthAssessmentState::Healthy,
                "False" => HealthAssessmentState::Degraded,
                _ => HealthAssessmentState::Unknown,
            }),
            current: true,
            reason: format!("Flux Ready condition is {status}"),
        })
        .into_iter()
        .collect();
    evaluate_health(HealthAssessmentInput {
        recognized_semantics: true,
        provider_available: true,
        evidence,
    })
}

fn source_field(data: &Value, field: &str) -> Option<String> {
    data.get("spec")
        .and_then(|spec| spec.get("sourceRef"))
        .and_then(|source_ref| source_ref.get(field))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn revision(data: &Value) -> Option<String> {
    data.get("status")
        .and_then(|status| status.get("lastAppliedRevision"))
        .or_else(|| {
            data.get("status")
                .and_then(|status| status.get("artifact"))
                .and_then(|artifact| artifact.get("revision"))
        })
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn ready_message(data: &Value) -> Option<String> {
    data.get("status")
        .and_then(|status| status.get("conditions"))
        .and_then(Value::as_array)?
        .iter()
        .find(|condition| {
            condition
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|value| value == "Ready")
        })
        .and_then(|condition| condition.get("message"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn flux_catalog_contains_current_api_families() {
        let kinds = flux_kinds();

        assert!(kinds.iter().any(|kind| kind.kind == "GitRepository"));
        assert!(kinds.iter().any(|kind| kind.kind == "Kustomization"));
        assert!(kinds.iter().any(|kind| kind.kind == "HelmRelease"));
        assert!(kinds.iter().any(|kind| kind.kind == "Alert"));
        assert!(kinds
            .iter()
            .any(|kind| kind.kind == "ImageUpdateAutomation"));
    }

    #[test]
    fn installed_flux_kinds_preserve_discovered_api_identity() {
        let resources = vec![
            ApiResource {
                group: "source.toolkit.fluxcd.io".into(),
                version: "v1beta2".into(),
                api_version: "source.toolkit.fluxcd.io/v1beta2".into(),
                kind: "GitRepository".into(),
                plural: "gitrepositories-v1beta2".into(),
            },
            ApiResource {
                group: "example.com".into(),
                version: "v1".into(),
                api_version: "example.com/v1".into(),
                kind: "Unknown".into(),
                plural: "unknowns".into(),
            },
        ];

        let installed = installed_flux_kinds_from_resources(&resources);
        assert_eq!(installed.len(), 1);
        assert_eq!(
            installed[0],
            FluxResourceKind {
                group: "source.toolkit.fluxcd.io".into(),
                version: "v1beta2".into(),
                api_version: "source.toolkit.fluxcd.io/v1beta2".into(),
                kind: "GitRepository".into(),
                plural: "gitrepositories-v1beta2".into(),
                namespaced: true,
                category: "Sources".into(),
            }
        );
        assert!(same_flux_kind(
            &installed[0],
            &flux_kinds()
                .into_iter()
                .find(|kind| kind.kind == "GitRepository")
                .expect("catalog kind")
        ));
    }

    #[test]
    fn extracts_ready_status_and_inventory_entries() {
        let data = json!({
            "status": {
                "conditions": [
                    { "type": "Ready", "status": "True", "message": "Applied revision main@sha1:abc" }
                ],
                "inventory": {
                    "entries": [
                        { "id": "default_api_apps_Deployment", "v": "v1" }
                    ]
                }
            }
        });

        assert_eq!(ready_status(&data), Some("True".to_string()));
        assert_eq!(inventory(&data).len(), 1);
        assert!(!inventory_partial(&data));
        assert_eq!(
            ready_message(&data),
            Some("Applied revision main@sha1:abc".to_string())
        );
    }

    #[test]
    fn classifies_flux_ready_condition_as_kubernetes_health_evidence() {
        let healthy = flux_health_assessment(Some("True"));
        let degraded = flux_health_assessment(Some("False"));
        let unknown = flux_health_assessment(None);

        assert_eq!(healthy.state, HealthAssessmentState::Healthy);
        assert_eq!(degraded.state, HealthAssessmentState::Degraded);
        assert_eq!(unknown.state, HealthAssessmentState::Unknown);
        assert_eq!(
            healthy.winning_sources,
            vec![HealthAssessmentSource::Kubernetes]
        );
    }

    #[test]
    fn reports_bounded_inventory_as_partial() {
        let entries = (0..=MAX_FLUX_INVENTORY_ENTRIES)
            .map(|index| json!({ "id": format!("default_resource-{index}__ConfigMap") }))
            .collect::<Vec<_>>();
        let data = json!({ "status": { "inventory": { "entries": entries } } });
        let object: DynamicObject = serde_json::from_value(json!({
            "apiVersion": "kustomize.toolkit.fluxcd.io/v1",
            "kind": "Kustomization",
            "metadata": { "name": "apps", "namespace": "flux-system" },
            "status": data["status"].clone()
        }))
        .expect("dynamic object");

        assert_eq!(inventory(&data).len(), MAX_FLUX_INVENTORY_ENTRIES);
        assert!(inventory_partial(&data));
        assert_eq!(
            bounded_status(&object)
                .and_then(|status| status.pointer("/inventory/entries").cloned())
                .and_then(|entries| entries.as_array().map(Vec::len)),
            Some(MAX_FLUX_INVENTORY_ENTRIES)
        );
    }
}
