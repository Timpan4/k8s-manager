use crate::commands::gitops_crd::{
    client_for_context, find_api_resource, get_crd_object, list_crd_objects, resource_metadata,
    resource_yaml,
};
use crate::commands::helpers::{k8s_creation_timestamp_to_rfc3339, resource_age};
use crate::models::AppErrorKind;
use crate::models::{
    evaluate_health, AppError, ArgoAppProjectDetails, ArgoAppProjectSummary, HealthAssessmentInput,
    YamlEncoding, YamlViewMode,
};
use chrono::{TimeZone, Utc};
use kube::core::DynamicObject;

fn app_project_summary_from_object(
    cluster_context: &str,
    obj: &DynamicObject,
) -> Option<ArgoAppProjectSummary> {
    let data = obj.data.as_object()?;
    let health_assessment = evaluate_health(HealthAssessmentInput {
        recognized_semantics: false,
        provider_available: true,
        evidence: vec![],
    });

    Some(ArgoAppProjectSummary {
        cluster: cluster_context.to_string(),
        name: obj.metadata.name.clone().unwrap_or_default(),
        age: resource_age(obj.metadata.creation_timestamp.clone().map(|timestamp| {
            Utc.timestamp_opt(timestamp.0.as_second(), 0)
                .single()
                .unwrap_or_else(Utc::now)
        })),
        created_at: k8s_creation_timestamp_to_rfc3339(&obj.metadata.creation_timestamp),
        namespace: obj.metadata.namespace.clone(),
        description: data
            .get("spec")
            .and_then(|spec| spec.get("description"))
            .and_then(|description| description.as_str())
            .map(String::from),
        status: data
            .get("status")
            .and_then(|status| status.get("conditions"))
            .and_then(|conditions| conditions.as_array())
            .and_then(|conditions| conditions.first())
            .and_then(|condition| condition.get("type"))
            .and_then(|kind| kind.as_str())
            .map(String::from),
        health_assessment,
    })
}

/// Get detailed Argo CD `AppProject` information including YAML and metadata.
#[tauri::command]
pub async fn get_argocd_appproject_details(
    cluster_context: String,
    name: String,
    namespace: Option<String>,
    kubeconfig_env_var: Option<String>,
    yaml_view_mode: Option<YamlViewMode>,
    yaml_encoding: Option<YamlEncoding>,
) -> Result<ArgoAppProjectDetails, AppError> {
    let client = client_for_context(&cluster_context, kubeconfig_env_var).await?;

    let ar = match find_api_resource(&client, "argoproj.io", "AppProject").await? {
        Some(ar) => ar,
        None => {
            return Err(AppError::new(
                "AppProject CRD not found",
                AppErrorKind::Cluster,
            ))
        }
    };

    let obj = get_crd_object(client.clone(), &ar, &name, namespace.as_deref()).await?;
    let yaml = resource_yaml(&obj, yaml_view_mode, yaml_encoding)?;
    let metadata = resource_metadata(&obj)?;
    let summary = app_project_summary_from_object(&cluster_context, &obj)
        .ok_or_else(|| AppError::new("invalid AppProject data", AppErrorKind::Cluster))?;
    Ok(ArgoAppProjectDetails {
        summary,
        yaml,
        metadata,
    })
}

/// List Argo CD `AppProjects` in the cluster.
#[tauri::command]
pub async fn list_argocd_appprojects(
    cluster_context: String,
    kubeconfig_env_var: Option<String>,
) -> Result<Vec<ArgoAppProjectSummary>, AppError> {
    let client = client_for_context(&cluster_context, kubeconfig_env_var).await?;

    let ar = match find_api_resource(&client, "argoproj.io", "AppProject").await? {
        Some(ar) => ar,
        None => return Ok(vec![]),
    };

    let items = list_crd_objects(client.clone(), &ar).await?;

    let summaries = items
        .iter()
        .filter_map(|obj| app_project_summary_from_object(&cluster_context, obj))
        .collect();

    Ok(summaries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use kube::core::{ApiResource, DynamicObject};
    use serde_json::json;

    fn app_project(data: serde_json::Value) -> DynamicObject {
        DynamicObject::new(
            "payments",
            &ApiResource {
                group: "argoproj.io".to_string(),
                version: "v1alpha1".to_string(),
                api_version: "argoproj.io/v1alpha1".to_string(),
                kind: "AppProject".to_string(),
                plural: "appprojects".to_string(),
            },
        )
        .within("argocd")
        .data(data)
    }

    #[test]
    fn projects_app_project_summary_from_dynamic_object() {
        let object = app_project(json!({
            "spec": { "description": "Payment workloads" },
            "status": { "conditions": [{ "type": "InvalidSpecError" }] }
        }));

        let summary = app_project_summary_from_object("kind-dev", &object).expect("summary");

        assert_eq!(summary.cluster, "kind-dev");
        assert_eq!(summary.name, "payments");
        assert_eq!(summary.namespace.as_deref(), Some("argocd"));
        assert_eq!(summary.description.as_deref(), Some("Payment workloads"));
        assert_eq!(summary.status.as_deref(), Some("InvalidSpecError"));
    }

    #[test]
    fn rejects_non_object_app_project_data() {
        assert!(app_project_summary_from_object("kind-dev", &app_project(json!(null))).is_none());
    }
}
