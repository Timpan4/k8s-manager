use crate::commands::helpers::{
    extract_argo_app, extract_git_ops_owner, extract_helm_release, extract_owner_ref,
    fetch_and_serialize, fmt_ready, k8s_creation_timestamp_to_rfc3339, resource_age,
    update_resource_health,
};
use crate::models::AppErrorKind;
use crate::models::{AppError, ResourceDetailsFull, ResourceHealth, ResourceSummary};
use chrono::{TimeZone, Utc};
use kube::Client;

use super::super::ingress_status::apply_ingress_status;

pub(super) async fn deployment_details(
    client: Client,
    cluster_context: String,
    name: String,
    namespace: Option<String>,
) -> Result<ResourceDetailsFull, AppError> {
    let (deploy, yaml) = fetch_and_serialize::<k8s_openapi::api::apps::v1::Deployment>(
        client,
        namespace.as_deref(),
        &name,
    )
    .await?;
    let metadata = serde_json::to_value(&deploy.metadata)
        .map_err(|e| AppError::new(e.to_string(), AppErrorKind::Serialization).with_source(e))?;
    let status = deploy
        .status
        .as_ref()
        .and_then(|s| serde_json::to_value(s).ok());
    let mut summary = ResourceSummary {
        kind: "Deployment".to_string(),
        cluster: cluster_context.clone(),
        name: name.clone(),
        namespace: namespace.clone(),
        age: resource_age(deploy.metadata.creation_timestamp.clone().map(|t| {
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
        created_at: k8s_creation_timestamp_to_rfc3339(&deploy.metadata.creation_timestamp),
        status: None,
        ready: None,
        restarts: None,
        owner_ref: extract_owner_ref(&deploy.metadata),
        argo_app: extract_argo_app(&deploy.metadata),
        helm_release: extract_helm_release(&deploy.metadata),
        git_ops_owner: extract_git_ops_owner(&deploy.metadata),
        git_ops_ownership_partial: false,
    };
    if let Some(ref s) = deploy.status {
        summary.ready = Some(format!(
            "{}/{}",
            s.ready_replicas.unwrap_or(0),
            s.replicas.unwrap_or(0)
        ));
    }
    update_resource_health(&mut summary);
    Ok(ResourceDetailsFull {
        summary,
        yaml,
        metadata,
        status,
    })
}

pub(super) async fn statefulset_details(
    client: Client,
    cluster_context: String,
    name: String,
    namespace: Option<String>,
) -> Result<ResourceDetailsFull, AppError> {
    let (ss, yaml) = fetch_and_serialize::<k8s_openapi::api::apps::v1::StatefulSet>(
        client,
        namespace.as_deref(),
        &name,
    )
    .await?;
    let metadata = serde_json::to_value(&ss.metadata)
        .map_err(|e| AppError::new(e.to_string(), AppErrorKind::Serialization).with_source(e))?;
    let status = ss
        .status
        .as_ref()
        .and_then(|s| serde_json::to_value(s).ok());
    let mut summary = ResourceSummary {
        kind: "StatefulSet".to_string(),
        cluster: cluster_context.clone(),
        name: name.clone(),
        namespace: namespace.clone(),
        age: resource_age(ss.metadata.creation_timestamp.clone().map(|t| {
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
        created_at: k8s_creation_timestamp_to_rfc3339(&ss.metadata.creation_timestamp),
        status: None,
        ready: None,
        restarts: None,
        owner_ref: extract_owner_ref(&ss.metadata),
        argo_app: extract_argo_app(&ss.metadata),
        helm_release: extract_helm_release(&ss.metadata),
        git_ops_owner: extract_git_ops_owner(&ss.metadata),
        git_ops_ownership_partial: false,
    };
    if let Some(ref s) = ss.status {
        summary.ready = Some(fmt_ready(s.ready_replicas, s.replicas));
    }
    update_resource_health(&mut summary);
    Ok(ResourceDetailsFull {
        summary,
        yaml,
        metadata,
        status,
    })
}

pub(super) async fn daemonset_details(
    client: Client,
    cluster_context: String,
    name: String,
    namespace: Option<String>,
) -> Result<ResourceDetailsFull, AppError> {
    let (ds, yaml) = fetch_and_serialize::<k8s_openapi::api::apps::v1::DaemonSet>(
        client,
        namespace.as_deref(),
        &name,
    )
    .await?;
    let metadata = serde_json::to_value(&ds.metadata)
        .map_err(|e| AppError::new(e.to_string(), AppErrorKind::Serialization).with_source(e))?;
    let status = ds
        .status
        .as_ref()
        .and_then(|s| serde_json::to_value(s).ok());
    let mut summary = ResourceSummary {
        kind: "DaemonSet".to_string(),
        cluster: cluster_context.clone(),
        name: name.clone(),
        namespace: namespace.clone(),
        age: resource_age(ds.metadata.creation_timestamp.clone().map(|t| {
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
        created_at: k8s_creation_timestamp_to_rfc3339(&ds.metadata.creation_timestamp),
        status: None,
        ready: None,
        restarts: None,
        owner_ref: extract_owner_ref(&ds.metadata),
        argo_app: extract_argo_app(&ds.metadata),
        helm_release: extract_helm_release(&ds.metadata),
        git_ops_owner: extract_git_ops_owner(&ds.metadata),
        git_ops_ownership_partial: false,
    };
    if let Some(ref s) = ds.status {
        summary.ready = Some(format!("{}/{}", s.number_ready, s.desired_number_scheduled));
    }
    update_resource_health(&mut summary);
    Ok(ResourceDetailsFull {
        summary,
        yaml,
        metadata,
        status,
    })
}

pub(super) async fn ingress_details(
    client: Client,
    cluster_context: String,
    name: String,
    namespace: Option<String>,
) -> Result<ResourceDetailsFull, AppError> {
    let (ing, yaml) = fetch_and_serialize::<k8s_openapi::api::networking::v1::Ingress>(
        client,
        namespace.as_deref(),
        &name,
    )
    .await?;
    let metadata = serde_json::to_value(&ing.metadata)
        .map_err(|e| AppError::new(e.to_string(), AppErrorKind::Serialization).with_source(e))?;
    let status = ing
        .status
        .as_ref()
        .and_then(|s| serde_json::to_value(s).ok());
    let mut summary = ResourceSummary {
        kind: "Ingress".to_string(),
        cluster: cluster_context.clone(),
        name: name.clone(),
        namespace: namespace.clone(),
        age: resource_age(ing.metadata.creation_timestamp.clone().map(|t| {
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
        created_at: k8s_creation_timestamp_to_rfc3339(&ing.metadata.creation_timestamp),
        status: None,
        ready: None,
        restarts: None,
        owner_ref: extract_owner_ref(&ing.metadata),
        argo_app: extract_argo_app(&ing.metadata),
        helm_release: extract_helm_release(&ing.metadata),
        git_ops_owner: extract_git_ops_owner(&ing.metadata),
        git_ops_ownership_partial: false,
    };
    apply_ingress_status(&mut summary, ing.status.as_ref());
    update_resource_health(&mut summary);
    Ok(ResourceDetailsFull {
        summary,
        yaml,
        metadata,
        status,
    })
}

pub(super) async fn job_details(
    client: Client,
    cluster_context: String,
    name: String,
    namespace: Option<String>,
) -> Result<ResourceDetailsFull, AppError> {
    let (job, yaml) = fetch_and_serialize::<k8s_openapi::api::batch::v1::Job>(
        client,
        namespace.as_deref(),
        &name,
    )
    .await?;
    let metadata = serde_json::to_value(&job.metadata)
        .map_err(|e| AppError::new(e.to_string(), AppErrorKind::Serialization).with_source(e))?;
    let status = job
        .status
        .as_ref()
        .and_then(|s| serde_json::to_value(s).ok());
    let mut summary = ResourceSummary {
        kind: "Job".to_string(),
        cluster: cluster_context.clone(),
        name: name.clone(),
        namespace: namespace.clone(),
        age: resource_age(job.metadata.creation_timestamp.clone().map(|t| {
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
        created_at: k8s_creation_timestamp_to_rfc3339(&job.metadata.creation_timestamp),
        status: None,
        ready: None,
        restarts: None,
        owner_ref: extract_owner_ref(&job.metadata),
        argo_app: extract_argo_app(&job.metadata),
        helm_release: extract_helm_release(&job.metadata),
        git_ops_owner: extract_git_ops_owner(&job.metadata),
        git_ops_ownership_partial: false,
    };
    if let Some(ref s) = job.status {
        summary.ready = Some(format!(
            "{}/{}",
            s.succeeded.unwrap_or(0),
            job.spec
                .as_ref()
                .and_then(|spec| spec.completions)
                .unwrap_or(1)
        ));
        summary.status = if s.failed.unwrap_or(0) > 0 {
            Some("Failed".to_string())
        } else if s.succeeded.unwrap_or(0) > 0 {
            Some("Complete".to_string())
        } else if s.active.unwrap_or(0) > 0 {
            Some("Active".to_string())
        } else {
            Some("Pending".to_string())
        };
    }
    update_resource_health(&mut summary);
    Ok(ResourceDetailsFull {
        summary,
        yaml,
        metadata,
        status,
    })
}

pub(super) async fn cronjob_details(
    client: Client,
    cluster_context: String,
    name: String,
    namespace: Option<String>,
) -> Result<ResourceDetailsFull, AppError> {
    let (cj, yaml) = fetch_and_serialize::<k8s_openapi::api::batch::v1::CronJob>(
        client,
        namespace.as_deref(),
        &name,
    )
    .await?;
    let metadata = serde_json::to_value(&cj.metadata)
        .map_err(|e| AppError::new(e.to_string(), AppErrorKind::Serialization).with_source(e))?;
    let status = cj
        .status
        .as_ref()
        .and_then(|s| serde_json::to_value(s).ok());
    let mut summary = ResourceSummary {
        kind: "CronJob".to_string(),
        cluster: cluster_context.clone(),
        name: name.clone(),
        namespace: namespace.clone(),
        age: resource_age(cj.metadata.creation_timestamp.clone().map(|t| {
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
        created_at: k8s_creation_timestamp_to_rfc3339(&cj.metadata.creation_timestamp),
        status: None,
        ready: None,
        restarts: None,
        owner_ref: extract_owner_ref(&cj.metadata),
        argo_app: extract_argo_app(&cj.metadata),
        helm_release: extract_helm_release(&cj.metadata),
        git_ops_owner: extract_git_ops_owner(&cj.metadata),
        git_ops_ownership_partial: false,
    };
    if let Some(ref s) = cj.status {
        if let Some(ref active) = s.active {
            if !active.is_empty() {
                summary.status = Some(format!("{} active", active.len()));
            }
        }
    }
    update_resource_health(&mut summary);
    Ok(ResourceDetailsFull {
        summary,
        yaml,
        metadata,
        status,
    })
}
