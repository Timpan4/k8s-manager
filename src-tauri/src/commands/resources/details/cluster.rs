use crate::commands::helpers::{
    base_resource_summary, fetch_and_serialize_cluster, k8s_creation_timestamp_to_rfc3339,
    resource_age, update_resource_health,
};
use crate::models::AppErrorKind;
use crate::models::{AppError, ResourceDetailsFull, ResourceHealth, ResourceSummary};
use chrono::{TimeZone, Utc};
use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use kube::Client;

pub(super) async fn node_details(
    client: Client,
    cluster_context: String,
    name: String,
    _namespace: Option<String>,
) -> Result<ResourceDetailsFull, AppError> {
    let (node, yaml) =
        fetch_and_serialize_cluster::<k8s_openapi::api::core::v1::Node>(client, &name).await?;
    let metadata = serde_json::to_value(&node.metadata)
        .map_err(|e| AppError::new(e.to_string(), AppErrorKind::Serialization).with_source(e))?;
    let status = node
        .status
        .as_ref()
        .and_then(|s| serde_json::to_value(s).ok());
    let mut summary = ResourceSummary {
        kind: "Node".to_string(),
        cluster: cluster_context.clone(),
        name: name.clone(),
        namespace: None,
        age: resource_age(node.metadata.creation_timestamp.clone().map(|t| {
            Utc.timestamp_opt(t.0.as_second(), 0)
                .single()
                .unwrap_or_else(Utc::now)
        })),
        api_version: None,
        group: None,
        version: None,
        plural: None,
        namespaced: None,
        dynamic: None,
        health: ResourceHealth::default(),
        health_assessment: crate::models::HealthAssessment::default(),
        created_at: k8s_creation_timestamp_to_rfc3339(&node.metadata.creation_timestamp),
        status: node.status.as_ref().and_then(|s| {
            s.conditions
                .as_ref()?
                .iter()
                .find(|c| c.type_ == "Ready")
                .map(|c| c.status.clone())
        }),
        ready: node.status.as_ref().and_then(|s| {
            s.conditions.as_ref().map(|conds| {
                format!(
                    "{}/{}",
                    conds
                        .iter()
                        .filter(|c| c.type_ == "Ready" && c.status == "True")
                        .count(),
                    1
                )
            })
        }),
        restarts: None,
        owner_ref: None,
        argo_app: None,
        helm_release: None,
        git_ops_owner: None,
        git_ops_ownership_partial: false,
    };
    update_resource_health(&mut summary);
    Ok(ResourceDetailsFull {
        summary,
        yaml,
        metadata,
        status,
    })
}

pub(super) async fn storageclass_details(
    client: Client,
    cluster_context: String,
    name: String,
    _namespace: Option<String>,
) -> Result<ResourceDetailsFull, AppError> {
    let (sc, yaml) =
        fetch_and_serialize_cluster::<k8s_openapi::api::storage::v1::StorageClass>(client, &name)
            .await?;
    let metadata = serde_json::to_value(&sc.metadata)
        .map_err(|e| AppError::new(e.to_string(), AppErrorKind::Serialization).with_source(e))?;
    let summary = ResourceSummary {
        kind: "StorageClass".to_string(),
        cluster: cluster_context.clone(),
        name: name.clone(),
        namespace: None,
        age: resource_age(sc.metadata.creation_timestamp.clone().map(|t| {
            Utc.timestamp_opt(t.0.as_second(), 0)
                .single()
                .unwrap_or_else(Utc::now)
        })),
        api_version: None,
        group: None,
        version: None,
        plural: None,
        namespaced: None,
        dynamic: None,
        health: ResourceHealth::default(),
        health_assessment: crate::models::HealthAssessment::default(),
        created_at: k8s_creation_timestamp_to_rfc3339(&sc.metadata.creation_timestamp),
        status: None,
        ready: None,
        restarts: None,
        owner_ref: None,
        argo_app: None,
        helm_release: None,
        git_ops_owner: None,
        git_ops_ownership_partial: false,
    };
    Ok(ResourceDetailsFull {
        summary,
        yaml,
        metadata,
        status: None,
    })
}

pub(super) async fn pv_details(
    client: Client,
    cluster_context: String,
    name: String,
    _namespace: Option<String>,
) -> Result<ResourceDetailsFull, AppError> {
    let (pv, yaml) =
        fetch_and_serialize_cluster::<k8s_openapi::api::core::v1::PersistentVolume>(client, &name)
            .await?;
    let metadata = serde_json::to_value(&pv.metadata)
        .map_err(|e| AppError::new(e.to_string(), AppErrorKind::Serialization).with_source(e))?;
    let status = pv
        .status
        .as_ref()
        .and_then(|s| serde_json::to_value(s).ok());
    let mut summary = ResourceSummary {
        kind: "PersistentVolume".to_string(),
        cluster: cluster_context.clone(),
        name: name.clone(),
        namespace: None,
        age: resource_age(pv.metadata.creation_timestamp.clone().map(|t| {
            Utc.timestamp_opt(t.0.as_second(), 0)
                .single()
                .unwrap_or_else(Utc::now)
        })),
        api_version: None,
        group: None,
        version: None,
        plural: None,
        namespaced: None,
        dynamic: None,
        health: ResourceHealth::default(),
        health_assessment: crate::models::HealthAssessment::default(),
        created_at: k8s_creation_timestamp_to_rfc3339(&pv.metadata.creation_timestamp),
        status: pv.status.as_ref().and_then(|s| s.phase.as_ref()).cloned(),
        ready: None,
        restarts: None,
        owner_ref: None,
        argo_app: None,
        helm_release: None,
        git_ops_owner: None,
        git_ops_ownership_partial: false,
    };
    update_resource_health(&mut summary);
    Ok(ResourceDetailsFull {
        summary,
        yaml,
        metadata,
        status,
    })
}

pub(super) async fn crd_details(
    client: Client,
    cluster_context: String,
    name: String,
    _namespace: Option<String>,
) -> Result<ResourceDetailsFull, AppError> {
    let (crd, yaml) =
        fetch_and_serialize_cluster::<CustomResourceDefinition>(client, &name).await?;
    let metadata = serde_json::to_value(&crd.metadata)
        .map_err(|e| AppError::new(e.to_string(), AppErrorKind::Serialization).with_source(e))?;
    let status = crd
        .status
        .as_ref()
        .and_then(|s| serde_json::to_value(s).ok());
    let mut summary = base_resource_summary(
        "CustomResourceDefinition",
        &cluster_context,
        &crd.metadata,
        resource_age(crd.metadata.creation_timestamp.clone().map(|t| {
            Utc.timestamp_opt(t.0.as_second(), 0)
                .single()
                .unwrap_or_else(Utc::now)
        })),
    );
    summary.api_version = Some("apiextensions.k8s.io/v1".to_string());
    summary.group = Some("apiextensions.k8s.io".to_string());
    summary.version = Some("v1".to_string());
    summary.plural = Some("customresourcedefinitions".to_string());
    summary.namespaced = Some(false);
    summary.health = ResourceHealth::default();
    summary.created_at = k8s_creation_timestamp_to_rfc3339(&crd.metadata.creation_timestamp);
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
    Ok(ResourceDetailsFull {
        summary,
        yaml,
        metadata,
        status,
    })
}
