use crate::commands::gitops_crd::{
    client_for_context, find_api_resource, get_crd_object, list_crd_objects, resource_metadata,
    resource_yaml,
};
use crate::commands::helpers::{k8s_creation_timestamp_to_rfc3339, resource_age};
use crate::models::AppErrorKind;
use crate::models::{
    argo_application_set_health_assessment, AppError, ArgoApplicationSetDetails,
    ArgoApplicationSetSummary, YamlEncoding, YamlViewMode,
};
use chrono::{TimeZone, Utc};
use kube::core::DynamicObject;

fn application_set_summary_from_object(
    cluster_context: &str,
    obj: &DynamicObject,
) -> Option<ArgoApplicationSetSummary> {
    let data = obj.data.as_object()?;
    let generator_param = data
        .get("spec")
        .and_then(|spec| spec.get("generatorParams"))
        .and_then(|params| params.as_array())
        .and_then(|params| params.first());

    let health_status = data
        .get("status")
        .and_then(|status| status.get("health"))
        .and_then(|health| health.get("status"))
        .and_then(|status| status.as_str());
    let conditions = data
        .get("status")
        .and_then(|status| status.get("conditions"))
        .and_then(|conditions| conditions.as_array());
    let health_assessment =
        argo_application_set_health_assessment(health_status, conditions.map(Vec::as_slice));

    Some(ArgoApplicationSetSummary {
        cluster: cluster_context.to_string(),
        name: obj.metadata.name.clone().unwrap_or_default(),
        age: resource_age(obj.metadata.creation_timestamp.clone().map(|timestamp| {
            Utc.timestamp_opt(timestamp.0.as_second(), 0)
                .single()
                .unwrap_or_else(Utc::now)
        })),
        created_at: k8s_creation_timestamp_to_rfc3339(&obj.metadata.creation_timestamp),
        namespace: obj.metadata.namespace.clone(),
        project: data
            .get("spec")
            .and_then(|spec| spec.get("project"))
            .and_then(|project| project.as_str())
            .map(String::from),
        status: data
            .get("status")
            .and_then(|status| status.get("conditions"))
            .and_then(|conditions| conditions.as_array())
            .and_then(|conditions| conditions.first())
            .and_then(|condition| condition.get("type"))
            .and_then(|kind| kind.as_str())
            .map(String::from),
        sync_status: None,
        health_status: health_status.map(String::from),
        health_assessment,
        destination_namespace: generator_param
            .and_then(|param| param.get("dest-namespace"))
            .and_then(|namespace| namespace.as_str())
            .map(String::from),
        destination_server: generator_param
            .and_then(|param| param.get("dest-server"))
            .and_then(|server| server.as_str())
            .map(String::from),
        source_repo: data
            .get("spec")
            .and_then(|spec| spec.get("source"))
            .and_then(|source| source.get("repoURL"))
            .and_then(|repo| repo.as_str())
            .map(String::from),
        source_revision: data
            .get("spec")
            .and_then(|spec| spec.get("source"))
            .and_then(|source| source.get("targetRevision"))
            .and_then(|revision| revision.as_str())
            .map(String::from),
    })
}

/// List Argo CD `ApplicationSets` in the cluster.
#[tauri::command]
pub async fn list_argocd_appsets(
    cluster_context: String,
    kubeconfig_env_var: Option<String>,
) -> Result<Vec<ArgoApplicationSetSummary>, AppError> {
    let client = client_for_context(&cluster_context, kubeconfig_env_var).await?;

    let ar = match find_api_resource(&client, "argoproj.io", "ApplicationSet").await? {
        Some(ar) => ar,
        None => return Ok(vec![]),
    };

    let items = list_crd_objects(client.clone(), &ar).await?;

    let summaries = items
        .iter()
        .filter_map(|obj| application_set_summary_from_object(&cluster_context, obj))
        .collect();

    Ok(summaries)
}

/// Get detailed Argo CD `ApplicationSet` information including YAML and metadata.
#[tauri::command]
pub async fn get_argocd_appset_details(
    cluster_context: String,
    name: String,
    namespace: Option<String>,
    kubeconfig_env_var: Option<String>,
    yaml_view_mode: Option<YamlViewMode>,
    yaml_encoding: Option<YamlEncoding>,
) -> Result<ArgoApplicationSetDetails, AppError> {
    let client = client_for_context(&cluster_context, kubeconfig_env_var).await?;

    let ar = match find_api_resource(&client, "argoproj.io", "ApplicationSet").await? {
        Some(ar) => ar,
        None => {
            return Err(AppError::new(
                "ApplicationSet CRD not found",
                AppErrorKind::Cluster,
            ))
        }
    };

    let obj = get_crd_object(client.clone(), &ar, &name, namespace.as_deref()).await?;
    let yaml = resource_yaml(&obj, yaml_view_mode, yaml_encoding)?;
    let metadata = resource_metadata(&obj)?;
    let summary = application_set_summary_from_object(&cluster_context, &obj)
        .ok_or_else(|| AppError::new("invalid ApplicationSet data", AppErrorKind::Cluster))?;
    Ok(ArgoApplicationSetDetails {
        summary,
        yaml,
        metadata,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use kube::core::{ApiResource, DynamicObject};
    use serde_json::json;

    fn application_set(data: serde_json::Value) -> DynamicObject {
        DynamicObject::new(
            "tenants",
            &ApiResource {
                group: "argoproj.io".to_string(),
                version: "v1alpha1".to_string(),
                api_version: "argoproj.io/v1alpha1".to_string(),
                kind: "ApplicationSet".to_string(),
                plural: "applicationsets".to_string(),
            },
        )
        .within("argocd")
        .data(data)
    }

    #[test]
    fn projects_application_set_summary_from_dynamic_object() {
        let object = application_set(json!({
            "spec": {
                "project": "platform",
                "generatorParams": [{ "dest-namespace": "tenant-a", "dest-server": "in-cluster" }],
                "source": { "repoURL": "https://git.example/platform", "targetRevision": "main" }
            },
            "status": {
                "health": { "status": "Healthy" },
                "conditions": [{ "type": "ResourcesUpToDate", "status": "True" }]
            }
        }));

        let summary = application_set_summary_from_object("kind-dev", &object).expect("summary");

        assert_eq!(summary.cluster, "kind-dev");
        assert_eq!(summary.project.as_deref(), Some("platform"));
        assert_eq!(summary.status.as_deref(), Some("ResourcesUpToDate"));
        assert_eq!(
            summary.health_assessment.state,
            crate::models::HealthAssessmentState::Healthy
        );
        assert_eq!(summary.sync_status, None);
        assert_eq!(summary.destination_namespace.as_deref(), Some("tenant-a"));
        assert_eq!(
            summary.source_repo.as_deref(),
            Some("https://git.example/platform")
        );
    }

    #[test]
    fn rejects_non_object_application_set_data() {
        assert!(
            application_set_summary_from_object("kind-dev", &application_set(json!(null)))
                .is_none()
        );
    }
}
