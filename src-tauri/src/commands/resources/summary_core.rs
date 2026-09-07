use crate::commands::helpers::{
    base_resource_summary, list_params, resource_age, update_resource_health,
};
use crate::models::{AppError, ResourceSummary};
use chrono::{TimeZone, Utc};
use kube::{api::Api, Client};

fn age_from_metadata(
    metadata: &k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta,
) -> String {
    resource_age(metadata.creation_timestamp.clone().map(|t| {
        Utc.timestamp_opt(t.0.as_second(), 0)
            .single()
            .unwrap_or_else(Utc::now)
    }))
}

pub(super) async fn core_resource_summaries(
    client: Client,
    cluster_context: &str,
    kind: &str,
    namespace: Option<&str>,
) -> Result<Option<Vec<ResourceSummary>>, AppError> {
    let summaries = match kind {
        "Pod" => {
            let api: Api<k8s_openapi::api::core::v1::Pod> = if let Some(ns) = namespace {
                Api::namespaced(client, ns)
            } else {
                Api::all(client)
            };
            api.list(&list_params())
                .await
                .map_err(AppError::from)?
                .iter()
                .map(|pod| {
                    let mut summary = base_resource_summary(
                        "Pod",
                        cluster_context,
                        &pod.metadata,
                        age_from_metadata(&pod.metadata),
                    );
                    if let Some(ref status) = pod.status {
                        summary.status = status.phase.clone().filter(|phase| !phase.is_empty());
                        summary.ready = status
                            .conditions
                            .as_ref()
                            .and_then(|conds| conds.iter().find(|c| c.type_ == "Ready"))
                            .map(|c| c.status.clone());
                        summary.restarts = crate::commands::helpers::pod_restarts(
                            status.container_statuses.as_deref(),
                        );
                    }
                    update_resource_health(&mut summary);
                    summary
                })
                .collect()
        }
        "Service" => {
            let api: Api<k8s_openapi::api::core::v1::Service> = if let Some(ns) = namespace {
                Api::namespaced(client, ns)
            } else {
                Api::all(client)
            };
            api.list(&list_params())
                .await
                .map_err(AppError::from)?
                .iter()
                .map(|svc| {
                    base_resource_summary(
                        "Service",
                        cluster_context,
                        &svc.metadata,
                        age_from_metadata(&svc.metadata),
                    )
                })
                .collect()
        }
        "ConfigMap" => {
            let api: Api<k8s_openapi::api::core::v1::ConfigMap> = if let Some(ns) = namespace {
                Api::namespaced(client, ns)
            } else {
                Api::all(client)
            };
            api.list(&list_params())
                .await
                .map_err(AppError::from)?
                .iter()
                .map(|cm| {
                    base_resource_summary(
                        "ConfigMap",
                        cluster_context,
                        &cm.metadata,
                        age_from_metadata(&cm.metadata),
                    )
                })
                .collect()
        }
        "Secret" => {
            let api: Api<k8s_openapi::api::core::v1::Secret> = if let Some(ns) = namespace {
                Api::namespaced(client, ns)
            } else {
                Api::all(client)
            };
            api.list(&list_params())
                .await
                .map_err(AppError::from)?
                .iter()
                .map(|sec| {
                    base_resource_summary(
                        "Secret",
                        cluster_context,
                        &sec.metadata,
                        age_from_metadata(&sec.metadata),
                    )
                })
                .collect()
        }
        "PersistentVolumeClaim" => {
            let api: Api<k8s_openapi::api::core::v1::PersistentVolumeClaim> =
                if let Some(ns) = namespace {
                    Api::namespaced(client, ns)
                } else {
                    Api::all(client)
                };
            api.list(&list_params())
                .await
                .map_err(AppError::from)?
                .iter()
                .map(|pvc| {
                    base_resource_summary(
                        "PersistentVolumeClaim",
                        cluster_context,
                        &pvc.metadata,
                        age_from_metadata(&pvc.metadata),
                    )
                })
                .collect()
        }
        _ => return Ok(None),
    };

    Ok(Some(summaries))
}
