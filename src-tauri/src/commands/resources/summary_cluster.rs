use crate::commands::helpers::{
    base_resource_summary, list_params, resource_age, update_resource_health,
};
use crate::models::{AppError, ResourceSummary};
use chrono::{TimeZone, Utc};
use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
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

pub(super) async fn cluster_resource_summaries(
    client: Client,
    cluster_context: &str,
    kind: &str,
) -> Result<Option<Vec<ResourceSummary>>, AppError> {
    let summaries = match kind {
        "Node" => node_summaries(client, cluster_context).await?,
        "StorageClass" => storageclass_summaries(client, cluster_context).await?,
        "PersistentVolume" => persistentvolume_summaries(client, cluster_context).await?,
        "CustomResourceDefinition" => crd_summaries(client, cluster_context).await?,
        _ => return Ok(None),
    };

    Ok(Some(summaries))
}

async fn node_summaries(
    client: Client,
    cluster_context: &str,
) -> Result<Vec<ResourceSummary>, AppError> {
    let api: Api<k8s_openapi::api::core::v1::Node> = Api::all(client);
    Ok(api
        .list(&list_params())
        .await
        .map_err(AppError::from)?
        .iter()
        .map(|node| {
            let mut summary = base_resource_summary(
                "Node",
                cluster_context,
                &node.metadata,
                age_from_metadata(&node.metadata),
            );
            if let Some(ref status) = node.status {
                if let Some(ref conditions) = status.conditions {
                    let ready_count = conditions
                        .iter()
                        .filter(|c| c.type_ == "Ready" && c.status == "True")
                        .count();
                    summary.ready = Some(format!("{}/{}", ready_count, 1));
                    summary.status = conditions
                        .iter()
                        .find(|c| c.type_ == "Ready")
                        .map(|c| c.status.clone());
                }
            }
            update_resource_health(&mut summary);
            summary
        })
        .collect())
}

async fn storageclass_summaries(
    client: Client,
    cluster_context: &str,
) -> Result<Vec<ResourceSummary>, AppError> {
    let api: Api<k8s_openapi::api::storage::v1::StorageClass> = Api::all(client);
    Ok(api
        .list(&list_params())
        .await
        .map_err(AppError::from)?
        .iter()
        .map(|sc| {
            base_resource_summary(
                "StorageClass",
                cluster_context,
                &sc.metadata,
                age_from_metadata(&sc.metadata),
            )
        })
        .collect())
}

async fn persistentvolume_summaries(
    client: Client,
    cluster_context: &str,
) -> Result<Vec<ResourceSummary>, AppError> {
    let api: Api<k8s_openapi::api::core::v1::PersistentVolume> = Api::all(client);
    Ok(api
        .list(&list_params())
        .await
        .map_err(AppError::from)?
        .iter()
        .map(|pv| {
            let mut summary = base_resource_summary(
                "PersistentVolume",
                cluster_context,
                &pv.metadata,
                age_from_metadata(&pv.metadata),
            );
            summary.status = pv.status.as_ref().and_then(|s| s.phase.as_ref()).cloned();
            update_resource_health(&mut summary);
            summary
        })
        .collect())
}

async fn crd_summaries(
    client: Client,
    cluster_context: &str,
) -> Result<Vec<ResourceSummary>, AppError> {
    let api: Api<CustomResourceDefinition> = Api::all(client);
    Ok(api
        .list(&list_params())
        .await
        .map_err(AppError::from)?
        .iter()
        .map(|crd| {
            let mut summary = base_resource_summary(
                "CustomResourceDefinition",
                cluster_context,
                &crd.metadata,
                age_from_metadata(&crd.metadata),
            );
            summary.api_version = Some("apiextensions.k8s.io/v1".to_string());
            summary.group = Some("apiextensions.k8s.io".to_string());
            summary.version = Some("v1".to_string());
            summary.plural = Some("customresourcedefinitions".to_string());
            summary.namespaced = Some(false);
            summary.status = crd.status.as_ref().and_then(|status| {
                status
                    .conditions
                    .as_ref()?
                    .iter()
                    .find(|condition| condition.type_ == "Established")
                    .or_else(|| status.conditions.as_ref()?.first())
                    .map(|condition| format!("{}: {}", condition.type_, condition.status))
            });
            update_resource_health(&mut summary);
            summary
        })
        .collect())
}
