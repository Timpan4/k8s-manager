use crate::commands::helpers::{
    extract_argo_app, extract_git_ops_owner, extract_helm_release, extract_owner_ref,
    fetch_and_serialize, k8s_creation_timestamp_to_rfc3339, redact_secret, resource_age,
    update_resource_health,
};
use crate::models::AppErrorKind;
use crate::models::{AppError, ResourceDetailsFull, ResourceHealth, ResourceSummary};
use chrono::{TimeZone, Utc};
use kube::Client;

pub(super) async fn pod_details(
    client: Client,
    cluster_context: String,
    name: String,
    namespace: Option<String>,
) -> Result<ResourceDetailsFull, AppError> {
    let (pod, yaml) =
        fetch_and_serialize::<k8s_openapi::api::core::v1::Pod>(client, namespace.as_deref(), &name)
            .await?;
    let metadata = serde_json::to_value(&pod.metadata)
        .map_err(|e| AppError::new(e.to_string(), AppErrorKind::Serialization).with_source(e))?;
    let status = pod
        .status
        .as_ref()
        .and_then(|s| serde_json::to_value(s).ok());
    let mut summary = ResourceSummary {
        kind: "Pod".to_string(),
        cluster: cluster_context.clone(),
        name: name.clone(),
        namespace: namespace.clone(),
        age: resource_age(pod.metadata.creation_timestamp.clone().map(|t| {
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
        created_at: k8s_creation_timestamp_to_rfc3339(&pod.metadata.creation_timestamp),
        status: pod.status.as_ref().and_then(|s| s.phase.clone()),
        ready: pod.status.as_ref().and_then(|s| {
            s.conditions
                .as_ref()
                .and_then(|conds| conds.iter().find(|c| c.type_ == "Ready"))
                .map(|c| c.status.clone())
        }),
        restarts: pod
            .status
            .as_ref()
            .and_then(|s| crate::commands::helpers::pod_restarts(s.container_statuses.as_deref())),
        owner_ref: extract_owner_ref(&pod.metadata),
        argo_app: extract_argo_app(&pod.metadata),
        helm_release: extract_helm_release(&pod.metadata),
        git_ops_owner: extract_git_ops_owner(&pod.metadata),
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

pub(super) async fn service_details(
    client: Client,
    cluster_context: String,
    name: String,
    namespace: Option<String>,
) -> Result<ResourceDetailsFull, AppError> {
    let (svc, yaml) = fetch_and_serialize::<k8s_openapi::api::core::v1::Service>(
        client,
        namespace.as_deref(),
        &name,
    )
    .await?;
    let metadata = serde_json::to_value(&svc.metadata)
        .map_err(|e| AppError::new(e.to_string(), AppErrorKind::Serialization).with_source(e))?;
    let status = svc
        .status
        .as_ref()
        .and_then(|s| serde_json::to_value(s).ok());
    let summary = ResourceSummary {
        kind: "Service".to_string(),
        cluster: cluster_context.clone(),
        name: name.clone(),
        namespace: namespace.clone(),
        age: resource_age(svc.metadata.creation_timestamp.clone().map(|t| {
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
        created_at: k8s_creation_timestamp_to_rfc3339(&svc.metadata.creation_timestamp),
        status: None,
        ready: None,
        restarts: None,
        owner_ref: extract_owner_ref(&svc.metadata),
        argo_app: extract_argo_app(&svc.metadata),
        helm_release: extract_helm_release(&svc.metadata),
        git_ops_owner: extract_git_ops_owner(&svc.metadata),
        git_ops_ownership_partial: false,
    };
    Ok(ResourceDetailsFull {
        summary,
        yaml,
        metadata,
        status,
    })
}

pub(super) async fn configmap_details(
    client: Client,
    cluster_context: String,
    name: String,
    namespace: Option<String>,
) -> Result<ResourceDetailsFull, AppError> {
    let (cm, yaml) = fetch_and_serialize::<k8s_openapi::api::core::v1::ConfigMap>(
        client,
        namespace.as_deref(),
        &name,
    )
    .await?;
    let metadata = serde_json::to_value(&cm.metadata)
        .map_err(|e| AppError::new(e.to_string(), AppErrorKind::Serialization).with_source(e))?;
    let summary = ResourceSummary {
        kind: "ConfigMap".to_string(),
        cluster: cluster_context.clone(),
        name: name.clone(),
        namespace: namespace.clone(),
        age: resource_age(cm.metadata.creation_timestamp.clone().map(|t| {
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
        created_at: k8s_creation_timestamp_to_rfc3339(&cm.metadata.creation_timestamp),
        status: None,
        ready: None,
        restarts: None,
        owner_ref: extract_owner_ref(&cm.metadata),
        argo_app: extract_argo_app(&cm.metadata),
        helm_release: extract_helm_release(&cm.metadata),
        git_ops_owner: extract_git_ops_owner(&cm.metadata),
        git_ops_ownership_partial: false,
    };
    Ok(ResourceDetailsFull {
        summary,
        yaml,
        metadata,
        status: None,
    })
}

pub(super) async fn secret_details(
    client: Client,
    cluster_context: String,
    name: String,
    namespace: Option<String>,
) -> Result<ResourceDetailsFull, AppError> {
    let (mut sec, _yaml) = fetch_and_serialize::<k8s_openapi::api::core::v1::Secret>(
        client,
        namespace.as_deref(),
        &name,
    )
    .await?;
    redact_secret(&mut sec);
    let yaml = serde_yaml::to_string(&sec)
        .map_err(|e| AppError::new(e.to_string(), AppErrorKind::Serialization).with_source(e))?;
    let metadata = serde_json::to_value(&sec.metadata)
        .map_err(|e| AppError::new(e.to_string(), AppErrorKind::Serialization).with_source(e))?;
    let summary = ResourceSummary {
        kind: "Secret".to_string(),
        cluster: cluster_context.clone(),
        name: name.clone(),
        namespace: namespace.clone(),
        age: resource_age(sec.metadata.creation_timestamp.clone().map(|t| {
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
        created_at: k8s_creation_timestamp_to_rfc3339(&sec.metadata.creation_timestamp),
        status: None,
        ready: None,
        restarts: None,
        owner_ref: extract_owner_ref(&sec.metadata),
        argo_app: extract_argo_app(&sec.metadata),
        helm_release: extract_helm_release(&sec.metadata),
        git_ops_owner: extract_git_ops_owner(&sec.metadata),
        git_ops_ownership_partial: false,
    };
    Ok(ResourceDetailsFull {
        summary,
        yaml,
        metadata,
        status: None,
    })
}

pub(super) async fn pvc_details(
    client: Client,
    cluster_context: String,
    name: String,
    namespace: Option<String>,
) -> Result<ResourceDetailsFull, AppError> {
    let (pvc, yaml) = fetch_and_serialize::<k8s_openapi::api::core::v1::PersistentVolumeClaim>(
        client,
        namespace.as_deref(),
        &name,
    )
    .await?;
    let metadata = serde_json::to_value(&pvc.metadata)
        .map_err(|e| AppError::new(e.to_string(), AppErrorKind::Serialization).with_source(e))?;
    let status = pvc
        .status
        .as_ref()
        .and_then(|s| serde_json::to_value(s).ok());
    let mut summary = ResourceSummary {
        kind: "PersistentVolumeClaim".to_string(),
        cluster: cluster_context.clone(),
        name: name.clone(),
        namespace: namespace.clone(),
        age: resource_age(pvc.metadata.creation_timestamp.clone().map(|t| {
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
        created_at: k8s_creation_timestamp_to_rfc3339(&pvc.metadata.creation_timestamp),
        status: status
            .as_ref()
            .and_then(|s| s.get("phase").and_then(|p| p.as_str().map(str::to_owned))),
        ready: None,
        restarts: None,
        owner_ref: extract_owner_ref(&pvc.metadata),
        argo_app: extract_argo_app(&pvc.metadata),
        helm_release: extract_helm_release(&pvc.metadata),
        git_ops_owner: extract_git_ops_owner(&pvc.metadata),
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
