use crate::commands::helpers::{
    base_resource_summary, fmt_ready, list_params, resource_age, update_resource_health,
};
use crate::models::{AppError, ResourceSummary};
use chrono::{TimeZone, Utc};
use kube::{api::Api, Client};

use super::ingress_status::apply_ingress_status;

fn age_from_metadata(
    metadata: &k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta,
) -> String {
    resource_age(metadata.creation_timestamp.clone().map(|t| {
        Utc.timestamp_opt(t.0.as_second(), 0)
            .single()
            .unwrap_or_else(Utc::now)
    }))
}

pub(super) async fn workload_resource_summaries(
    client: Client,
    cluster_context: &str,
    kind: &str,
    namespace: Option<&str>,
) -> Result<Option<Vec<ResourceSummary>>, AppError> {
    let summaries = match kind {
        "Deployment" => deployment_summaries(client, cluster_context, namespace).await?,
        "ReplicaSet" => replicaset_summaries(client, cluster_context, namespace).await?,
        "StatefulSet" => statefulset_summaries(client, cluster_context, namespace).await?,
        "DaemonSet" => daemonset_summaries(client, cluster_context, namespace).await?,
        "Ingress" => ingress_summaries(client, cluster_context, namespace).await?,
        "Job" => job_summaries(client, cluster_context, namespace).await?,
        "CronJob" => cronjob_summaries(client, cluster_context, namespace).await?,
        _ => return Ok(None),
    };

    Ok(Some(summaries))
}

async fn deployment_summaries(
    client: Client,
    cluster_context: &str,
    namespace: Option<&str>,
) -> Result<Vec<ResourceSummary>, AppError> {
    let api: Api<k8s_openapi::api::apps::v1::Deployment> = if let Some(ns) = namespace {
        Api::namespaced(client, ns)
    } else {
        Api::all(client)
    };
    Ok(api
        .list(&list_params())
        .await
        .map_err(AppError::from)?
        .iter()
        .map(|deploy| {
            let mut summary = base_resource_summary(
                "Deployment",
                cluster_context,
                &deploy.metadata,
                age_from_metadata(&deploy.metadata),
            );
            if let Some(ref status) = deploy.status {
                let available = status.available_replicas.unwrap_or(0);
                let ready = status.ready_replicas.unwrap_or(0);
                let desired = status.replicas.unwrap_or(0);
                summary.ready = Some(format!("{ready}/{desired}"));
                if available > 0 || ready > 0 || desired > 0 {
                    summary.status = Some(format!("Available: {available}"));
                }
            }
            update_resource_health(&mut summary);
            summary
        })
        .collect())
}

async fn replicaset_summaries(
    client: Client,
    cluster_context: &str,
    namespace: Option<&str>,
) -> Result<Vec<ResourceSummary>, AppError> {
    let api: Api<k8s_openapi::api::apps::v1::ReplicaSet> = if let Some(ns) = namespace {
        Api::namespaced(client, ns)
    } else {
        Api::all(client)
    };
    Ok(api
        .list(&list_params())
        .await
        .map_err(AppError::from)?
        .iter()
        .map(|rs| {
            let mut summary = base_resource_summary(
                "ReplicaSet",
                cluster_context,
                &rs.metadata,
                age_from_metadata(&rs.metadata),
            );
            if let Some(ref status) = rs.status {
                summary.ready = Some(fmt_ready(status.ready_replicas, status.replicas));
                summary.status = Some(format!(
                    "Available: {}",
                    status.available_replicas.unwrap_or(0)
                ));
            }
            update_resource_health(&mut summary);
            summary
        })
        .collect())
}

async fn statefulset_summaries(
    client: Client,
    cluster_context: &str,
    namespace: Option<&str>,
) -> Result<Vec<ResourceSummary>, AppError> {
    let api: Api<k8s_openapi::api::apps::v1::StatefulSet> = if let Some(ns) = namespace {
        Api::namespaced(client, ns)
    } else {
        Api::all(client)
    };
    Ok(api
        .list(&list_params())
        .await
        .map_err(AppError::from)?
        .iter()
        .map(|ss| {
            let mut summary = base_resource_summary(
                "StatefulSet",
                cluster_context,
                &ss.metadata,
                age_from_metadata(&ss.metadata),
            );
            if let Some(ref status) = ss.status {
                summary.ready = Some(fmt_ready(status.ready_replicas, status.replicas));
            }
            update_resource_health(&mut summary);
            summary
        })
        .collect())
}

async fn daemonset_summaries(
    client: Client,
    cluster_context: &str,
    namespace: Option<&str>,
) -> Result<Vec<ResourceSummary>, AppError> {
    let api: Api<k8s_openapi::api::apps::v1::DaemonSet> = if let Some(ns) = namespace {
        Api::namespaced(client, ns)
    } else {
        Api::all(client)
    };
    Ok(api
        .list(&list_params())
        .await
        .map_err(AppError::from)?
        .iter()
        .map(|ds| {
            let mut summary = base_resource_summary(
                "DaemonSet",
                cluster_context,
                &ds.metadata,
                age_from_metadata(&ds.metadata),
            );
            if let Some(ref status) = ds.status {
                summary.ready = Some(format!(
                    "{}/{}",
                    status.number_ready, status.desired_number_scheduled
                ));
            }
            update_resource_health(&mut summary);
            summary
        })
        .collect())
}

async fn ingress_summaries(
    client: Client,
    cluster_context: &str,
    namespace: Option<&str>,
) -> Result<Vec<ResourceSummary>, AppError> {
    let api: Api<k8s_openapi::api::networking::v1::Ingress> = if let Some(ns) = namespace {
        Api::namespaced(client, ns)
    } else {
        Api::all(client)
    };
    Ok(api
        .list(&list_params())
        .await
        .map_err(AppError::from)?
        .iter()
        .map(|ing| {
            let mut summary = base_resource_summary(
                "Ingress",
                cluster_context,
                &ing.metadata,
                age_from_metadata(&ing.metadata),
            );
            apply_ingress_status(&mut summary, ing.status.as_ref());
            update_resource_health(&mut summary);
            summary
        })
        .collect())
}

async fn job_summaries(
    client: Client,
    cluster_context: &str,
    namespace: Option<&str>,
) -> Result<Vec<ResourceSummary>, AppError> {
    let api: Api<k8s_openapi::api::batch::v1::Job> = if let Some(ns) = namespace {
        Api::namespaced(client, ns)
    } else {
        Api::all(client)
    };
    Ok(api
        .list(&list_params())
        .await
        .map_err(AppError::from)?
        .iter()
        .map(|job| {
            let mut summary = base_resource_summary(
                "Job",
                cluster_context,
                &job.metadata,
                age_from_metadata(&job.metadata),
            );
            if let Some(ref status) = job.status {
                let active = status.active.unwrap_or(0);
                let failed = status.failed.unwrap_or(0);
                let succeeded = status.succeeded.unwrap_or(0);
                summary.status = if failed > 0 {
                    Some("Failed".to_string())
                } else if succeeded > 0 {
                    Some("Complete".to_string())
                } else if active > 0 {
                    Some("Active".to_string())
                } else {
                    Some("Pending".to_string())
                };
                summary.ready = Some(format!(
                    "{}/{}",
                    succeeded,
                    job.spec.as_ref().and_then(|s| s.completions).unwrap_or(1)
                ));
            }
            update_resource_health(&mut summary);
            summary
        })
        .collect())
}

async fn cronjob_summaries(
    client: Client,
    cluster_context: &str,
    namespace: Option<&str>,
) -> Result<Vec<ResourceSummary>, AppError> {
    let api: Api<k8s_openapi::api::batch::v1::CronJob> = if let Some(ns) = namespace {
        Api::namespaced(client, ns)
    } else {
        Api::all(client)
    };
    Ok(api
        .list(&list_params())
        .await
        .map_err(AppError::from)?
        .iter()
        .map(|cj| {
            let mut summary = base_resource_summary(
                "CronJob",
                cluster_context,
                &cj.metadata,
                age_from_metadata(&cj.metadata),
            );
            if let Some(ref status) = cj.status {
                let active = status.active.as_ref().map_or(0, std::vec::Vec::len);
                if active > 0 {
                    summary.status = Some(format!("{active} active"));
                }
            }
            update_resource_health(&mut summary);
            summary
        })
        .collect())
}
