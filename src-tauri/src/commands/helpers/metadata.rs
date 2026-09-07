use crate::commands::helpers::k8s_timestamp_to_datetime;
use crate::models::{OwnerReferenceSummary, ResourceHealth, ResourceSummary};

mod flux_inventory;
mod ownership;

pub(crate) use flux_inventory::{
    enrich_resource_summaries_with_flux_inventory, fetch_flux_ownership_index,
    filter_flux_ownership_index, read_flux_ownership_index, FluxOwnershipIndex,
};
pub(crate) use ownership::{extract_argo_app, extract_git_ops_owner};

const LABEL_HELM_RELEASE_NAME: &str = "helm.sh/release";

pub(crate) fn extract_owner_ref(
    metadata: &k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta,
) -> Option<String> {
    metadata
        .owner_references
        .as_ref()
        .and_then(|refs| refs.iter().next())
        .map(|r| r.name.clone())
}

pub(crate) fn extract_owner_ref_summary(
    metadata: &k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta,
) -> Option<OwnerReferenceSummary> {
    metadata
        .owner_references
        .as_ref()
        .and_then(|refs| {
            refs.iter()
                .find(|reference| reference.controller == Some(true))
        })
        .map(|r| OwnerReferenceSummary {
            api_version: r.api_version.clone(),
            kind: r.kind.clone(),
            name: r.name.clone(),
            uid: r.uid.clone(),
        })
}

pub(crate) fn extract_helm_release(
    metadata: &k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta,
) -> Option<String> {
    metadata
        .labels
        .as_ref()
        .and_then(|labels| labels.get(LABEL_HELM_RELEASE_NAME))
        .cloned()
}

pub(crate) fn base_resource_summary(
    kind: &str,
    cluster: &str,
    metadata: &k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta,
    age: String,
) -> ResourceSummary {
    let api_version = match kind {
        "Deployment" | "ReplicaSet" | "StatefulSet" | "DaemonSet" => Some("apps/v1"),
        "Ingress" => Some("networking.k8s.io/v1"),
        "Job" | "CronJob" => Some("batch/v1"),
        "StorageClass" => Some("storage.k8s.io/v1"),
        "Pod"
        | "Service"
        | "ConfigMap"
        | "Secret"
        | "PersistentVolumeClaim"
        | "Node"
        | "PersistentVolume" => Some("v1"),
        _ => None,
    }
    .map(str::to_string);
    ResourceSummary {
        kind: kind.to_string(),
        cluster: cluster.to_string(),
        name: metadata.name.clone().unwrap_or_default(),
        namespace: metadata.namespace.clone(),
        age,
        api_version,
        group: None,
        version: None,
        plural: None,
        namespaced: None,
        dynamic: None,
        health: ResourceHealth::Unknown,
        health_assessment: crate::models::HealthAssessment::default(),
        created_at: metadata
            .creation_timestamp
            .as_ref()
            .and_then(|t| k8s_timestamp_to_datetime(&t.0).map(|dt| dt.to_rfc3339())),
        status: None,
        ready: None,
        restarts: None,
        owner_ref: extract_owner_ref(metadata),
        argo_app: extract_argo_app(metadata),
        helm_release: extract_helm_release(metadata),
        git_ops_owner: extract_git_ops_owner(metadata),
        git_ops_ownership_partial: false,
    }
}

fn opt_i32_to_str(opt: Option<i32>) -> String {
    opt.map_or_else(|| "0".to_string(), |v| v.to_string())
}

pub(crate) fn fmt_ready(ready: Option<i32>, desired: i32) -> String {
    format!("{}/{}", opt_i32_to_str(ready), desired)
}

#[cfg(test)]
mod tests {
    use super::*;
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::{ObjectMeta, OwnerReference};
    use std::collections::BTreeMap;

    #[test]
    fn extracts_owner_and_helm_metadata() {
        let mut labels = BTreeMap::new();
        labels.insert(
            LABEL_HELM_RELEASE_NAME.to_string(),
            "payments-api".to_string(),
        );
        let metadata = ObjectMeta {
            labels: Some(labels),
            owner_references: Some(vec![OwnerReference {
                api_version: "apps/v1".to_string(),
                kind: "Deployment".to_string(),
                name: "api".to_string(),
                uid: "uid-1".to_string(),
                controller: Some(true),
                ..Default::default()
            }]),
            ..Default::default()
        };

        assert_eq!(extract_owner_ref(&metadata), Some("api".to_string()));
        assert_eq!(
            extract_owner_ref_summary(&metadata),
            Some(crate::models::OwnerReferenceSummary {
                api_version: "apps/v1".to_string(),
                kind: "Deployment".to_string(),
                name: "api".to_string(),
                uid: "uid-1".to_string(),
            })
        );
        assert_eq!(
            extract_helm_release(&metadata),
            Some("payments-api".to_string())
        );
    }

    #[test]
    fn owner_ref_summary_is_absent_without_owner_references() {
        let metadata = ObjectMeta {
            name: Some("api".to_string()),
            ..Default::default()
        };

        assert_eq!(extract_owner_ref_summary(&metadata), None);
    }

    #[test]
    fn owner_ref_summary_selects_controller_reference() {
        let metadata = ObjectMeta {
            owner_references: Some(vec![
                OwnerReference {
                    api_version: "example/v1".to_string(),
                    kind: "Observer".to_string(),
                    name: "audit".to_string(),
                    uid: "uid-observer".to_string(),
                    controller: Some(false),
                    ..Default::default()
                },
                OwnerReference {
                    api_version: "apps/v1".to_string(),
                    kind: "ReplicaSet".to_string(),
                    name: "api-abc".to_string(),
                    uid: "uid-controller".to_string(),
                    controller: Some(true),
                    ..Default::default()
                },
            ]),
            ..Default::default()
        };

        assert_eq!(extract_owner_ref(&metadata), Some("audit".to_string()));
        assert_eq!(
            extract_owner_ref_summary(&metadata),
            Some(crate::models::OwnerReferenceSummary {
                api_version: "apps/v1".to_string(),
                kind: "ReplicaSet".to_string(),
                name: "api-abc".to_string(),
                uid: "uid-controller".to_string(),
            })
        );
    }

    #[test]
    fn owner_ref_summary_is_absent_without_controller_reference() {
        let metadata = ObjectMeta {
            owner_references: Some(vec![OwnerReference {
                api_version: "example/v1".to_string(),
                kind: "Observer".to_string(),
                name: "audit".to_string(),
                uid: "uid-observer".to_string(),
                controller: Some(false),
                ..Default::default()
            }]),
            ..Default::default()
        };

        assert_eq!(extract_owner_ref(&metadata), Some("audit".to_string()));
        assert_eq!(extract_owner_ref_summary(&metadata), None);
    }
}
