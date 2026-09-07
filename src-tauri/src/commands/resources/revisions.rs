use crate::commands::helpers::k8s_creation_timestamp_to_rfc3339;
use crate::commands::kubeconfig::KubeconfigSource;
use crate::models::AppErrorKind;
use crate::models::{AppError, DeploymentRevision};
use k8s_openapi::api::apps::v1::{Deployment, ReplicaSet};
use kube::api::ListParams;
use kube::{Api, Client};

const CHANGE_CAUSE: &str = "kubernetes.io/change-cause";
const REVISION: &str = "deployment.kubernetes.io/revision";

#[tauri::command]
pub async fn list_deployment_revisions(
    cluster_context: String,
    name: String,
    namespace: String,
    kubeconfig_env_var: Option<String>,
) -> Result<Vec<DeploymentRevision>, AppError> {
    let source = KubeconfigSource::new(kubeconfig_env_var)?;
    let client = source.client_for_context(&cluster_context).await?;
    deployment_revisions_from(client, &name, &namespace).await
}

async fn deployment_revisions_from(
    client: Client,
    deployment_name: &str,
    namespace: &str,
) -> Result<Vec<DeploymentRevision>, AppError> {
    crate::commands::helpers::validate_namespace(Some(namespace))?;
    crate::commands::helpers::validate_path_segment(deployment_name, "Deployment name")?;
    let deployments: Api<Deployment> = Api::namespaced(client.clone(), namespace);
    let deployment = deployments
        .get(deployment_name)
        .await
        .map_err(AppError::from)?;
    let deployment_uid = deployment.metadata.uid.as_deref().ok_or_else(|| {
        AppError::new(
            "Deployment is missing its Kubernetes UID",
            AppErrorKind::Cluster,
        )
    })?;
    let replica_sets: Api<ReplicaSet> = Api::namespaced(client, namespace);
    let replica_sets = replica_sets
        .list(&ListParams::default())
        .await
        .map_err(AppError::from)?;

    Ok(deployment_revisions_from_replica_sets(
        replica_sets.items,
        deployment_uid,
    ))
}

fn deployment_revisions_from_replica_sets(
    replica_sets: Vec<ReplicaSet>,
    deployment_uid: &str,
) -> Vec<DeploymentRevision> {
    let mut revisions = replica_sets
        .into_iter()
        .filter(|replica_set| is_owned_by_deployment(replica_set, deployment_uid))
        .filter_map(|replica_set| {
            let name = replica_set.metadata.name?;
            let pod_template_yaml = serde_yaml::to_string(&replica_set.spec?.template).ok()?;
            let annotations = replica_set.metadata.annotations.as_ref();
            Some(DeploymentRevision {
                name,
                revision: annotations
                    .and_then(|values| values.get(REVISION))
                    .and_then(|value| value.parse::<u64>().ok()),
                change_cause: annotations.and_then(|values| values.get(CHANGE_CAUSE).cloned()),
                created_at: k8s_creation_timestamp_to_rfc3339(
                    &replica_set.metadata.creation_timestamp,
                ),
                pod_template_yaml,
            })
        })
        .collect::<Vec<_>>();
    revisions.sort_by(|left, right| {
        right
            .revision
            .cmp(&left.revision)
            .then_with(|| left.name.cmp(&right.name))
    });
    revisions
}

fn is_owned_by_deployment(replica_set: &ReplicaSet, deployment_uid: &str) -> bool {
    replica_set
        .metadata
        .owner_references
        .as_ref()
        .is_some_and(|owners| {
            owners.iter().any(|owner| {
                owner.controller.unwrap_or(false)
                    && owner.kind == "Deployment"
                    && owner.api_version == "apps/v1"
                    && owner.uid == deployment_uid
            })
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use k8s_openapi::api::apps::v1::ReplicaSetSpec;
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference;
    use kube::api::ObjectMeta;

    fn replica_set(name: &str, owner_uid: &str, revision: Option<&str>) -> ReplicaSet {
        let mut annotations = std::collections::BTreeMap::new();
        if let Some(revision) = revision {
            annotations.insert(REVISION.to_string(), revision.to_string());
        }
        ReplicaSet {
            metadata: ObjectMeta {
                name: Some(name.to_string()),
                annotations: Some(annotations),
                owner_references: Some(vec![OwnerReference {
                    api_version: "apps/v1".to_string(),
                    kind: "Deployment".to_string(),
                    name: "web".to_string(),
                    uid: owner_uid.to_string(),
                    controller: Some(true),
                    block_owner_deletion: None,
                }]),
                ..Default::default()
            },
            spec: Some(ReplicaSetSpec::default()),
            status: None,
        }
    }

    #[test]
    fn filters_by_exact_controller_owner_and_sorts_revisions() {
        let mut non_controller = replica_set("web-non-controller", "deployment-uid", Some("9"));
        non_controller.metadata.owner_references.as_mut().unwrap()[0].controller = Some(false);
        let mut wrong_kind = replica_set("web-job", "deployment-uid", Some("8"));
        wrong_kind.metadata.owner_references.as_mut().unwrap()[0].kind = "Job".to_string();
        let revisions = deployment_revisions_from_replica_sets(
            vec![
                replica_set("web-old", "deployment-uid", Some("1")),
                replica_set("web-current", "deployment-uid", Some("2")),
                replica_set("other", "other-uid", Some("10")),
                replica_set("web-invalid", "deployment-uid", Some("oops")),
                non_controller,
                wrong_kind,
            ],
            "deployment-uid",
        );
        assert_eq!(
            revisions
                .iter()
                .map(|revision| revision.name.as_str())
                .collect::<Vec<_>>(),
            vec!["web-current", "web-old", "web-invalid"]
        );
        assert_eq!(revisions[0].revision, Some(2));
        assert_eq!(revisions[2].revision, None);
        let serialized = serde_json::to_value(&revisions[2]).unwrap();
        assert!(serialized.get("revision").is_none());
        assert!(!revisions[0].pod_template_yaml.is_empty());
    }
}
