use crate::commands::gitops_crd::{
    client_for_context, find_api_resource, get_crd_object, list_crd_objects, resource_metadata,
    resource_status, resource_yaml,
};
use crate::commands::helpers::{k8s_creation_timestamp_to_rfc3339, resource_age};
use crate::models::AppErrorKind;
use crate::models::{
    argo_health_assessment, AppError, ArgoApplicationDetails, ArgoApplicationSourceSummary,
    ArgoApplicationSummary, YamlEncoding, YamlViewMode,
};
use chrono::{TimeZone, Utc};
use kube::core::DynamicObject;
use std::collections::BTreeSet;

pub(super) fn application_summary_from_object(
    cluster_context: &str,
    obj: &DynamicObject,
) -> Option<ArgoApplicationSummary> {
    let data = obj.data.as_object()?;
    let destination_namespace = data
        .get("spec")
        .and_then(|spec| spec.get("destination"))
        .and_then(|destination| destination.get("namespace"))
        .and_then(|namespace| namespace.as_str())
        .map(String::from);
    let sources = application_sources(data);

    let health_status = data
        .get("status")
        .and_then(|status| status.get("health"))
        .and_then(|health| health.get("status"))
        .and_then(|status| status.as_str());
    let sync_status = data
        .get("status")
        .and_then(|status| status.get("sync"))
        .and_then(|sync| sync.get("status"))
        .and_then(|status| status.as_str());
    let health_assessment = argo_health_assessment(health_status, sync_status);

    Some(ArgoApplicationSummary {
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
        sync_status: sync_status.map(String::from),
        health_status: health_status.map(String::from),
        health_assessment,
        destination_server: data
            .get("spec")
            .and_then(|spec| spec.get("destination"))
            .and_then(|destination| destination.get("server"))
            .and_then(|server| server.as_str())
            .map(String::from),
        source_repo: primary_source_repo(&sources),
        source_revision: primary_source_revision(&sources),
        source_mode: application_source_mode(&sources),
        source_count: application_source_count(&sources),
        resource_version: obj.metadata.resource_version.clone(),
        uid: obj.metadata.uid.clone(),
        resource_namespaces: application_resource_namespaces(
            data,
            destination_namespace.as_deref(),
        ),
        tracked_resource_count: application_tracked_resource_count(data),
        destination_namespace,
        sources,
    })
}

/// List Argo CD Applications in the cluster.
#[tauri::command]
pub async fn list_argocd_applications(
    cluster_context: String,
    kubeconfig_env_var: Option<String>,
) -> Result<Vec<ArgoApplicationSummary>, AppError> {
    let client = client_for_context(&cluster_context, kubeconfig_env_var).await?;

    let ar = match find_api_resource(&client, "argoproj.io", "Application").await? {
        Some(ar) => ar,
        None => return Ok(vec![]),
    };

    let items = list_crd_objects(client.clone(), &ar).await?;

    let summaries = items
        .iter()
        .filter_map(|obj| application_summary_from_object(&cluster_context, obj))
        .collect();

    Ok(summaries)
}

/// Get detailed Argo CD Application information including YAML, metadata, and status.
#[tauri::command]
pub async fn get_argocd_application_details(
    cluster_context: String,
    name: String,
    namespace: Option<String>,
    kubeconfig_env_var: Option<String>,
    yaml_view_mode: Option<YamlViewMode>,
    yaml_encoding: Option<YamlEncoding>,
) -> Result<ArgoApplicationDetails, AppError> {
    let client = client_for_context(&cluster_context, kubeconfig_env_var).await?;

    let ar = match find_api_resource(&client, "argoproj.io", "Application").await? {
        Some(ar) => ar,
        None => {
            return Err(AppError::new(
                "Application CRD not found",
                AppErrorKind::Cluster,
            ))
        }
    };

    let obj = get_crd_object(client.clone(), &ar, &name, namespace.as_deref()).await?;

    let yaml = resource_yaml(&obj, yaml_view_mode, yaml_encoding)?;
    let metadata = resource_metadata(&obj)?;
    let status = resource_status(&obj);
    let summary = application_summary_from_object(&cluster_context, &obj)
        .ok_or_else(|| AppError::new("invalid application data", AppErrorKind::Cluster))?;

    Ok(ArgoApplicationDetails {
        summary,
        yaml,
        metadata,
        status,
    })
}

fn application_resource_namespaces(
    data: &serde_json::Map<String, serde_json::Value>,
    destination_namespace: Option<&str>,
) -> Vec<String> {
    let mut namespaces = BTreeSet::new();
    if let Some(resources) = data
        .get("status")
        .and_then(|status| status.get("resources"))
        .and_then(|resources| resources.as_array())
    {
        for resource in resources {
            if let Some(namespace) = resource
                .get("namespace")
                .and_then(|namespace| namespace.as_str())
                .map(str::trim)
                .filter(|namespace| !namespace.is_empty())
            {
                namespaces.insert(namespace.to_string());
            }
        }
    }
    if namespaces.is_empty() {
        if let Some(namespace) = destination_namespace
            .map(str::trim)
            .filter(|namespace| !namespace.is_empty())
        {
            namespaces.insert(namespace.to_string());
        }
    }
    namespaces.into_iter().collect()
}

fn application_tracked_resource_count(
    data: &serde_json::Map<String, serde_json::Value>,
) -> Option<usize> {
    data.get("status")
        .and_then(|status| status.get("resources"))
        .and_then(|resources| resources.as_array())
        .map(Vec::len)
}

fn application_sources(
    data: &serde_json::Map<String, serde_json::Value>,
) -> Vec<ArgoApplicationSourceSummary> {
    let Some(spec) = data.get("spec") else {
        return vec![];
    };
    let resolved_revisions = data
        .get("status")
        .and_then(|status| status.get("sync"))
        .and_then(|sync| sync.get("revisions"))
        .and_then(|revisions| revisions.as_array())
        .map(|revisions| {
            revisions
                .iter()
                .map(|revision| revision.as_str().map(String::from))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let single_resolved_revision = data
        .get("status")
        .and_then(|status| status.get("sync"))
        .and_then(|sync| sync.get("revision"))
        .and_then(|revision| revision.as_str())
        .map(String::from);

    if let Some(sources) = spec
        .get("sources")
        .and_then(|sources| sources.as_array())
        .filter(|sources| !sources.is_empty())
    {
        return sources
            .iter()
            .enumerate()
            .filter_map(|(index, source)| {
                source.as_object().map(|source| {
                    source_summary(source, resolved_revisions.get(index).cloned().flatten())
                })
            })
            .collect();
    }

    spec.get("source")
        .and_then(|source| source.as_object())
        .map(|source| vec![source_summary(source, single_resolved_revision)])
        .unwrap_or_default()
}

fn source_summary(
    source: &serde_json::Map<String, serde_json::Value>,
    resolved_revision: Option<String>,
) -> ArgoApplicationSourceSummary {
    let source_mode = source_mode(source);
    let target_revision = source
        .get("targetRevision")
        .and_then(|revision| revision.as_str())
        .map(String::from)
        .or_else(|| {
            if source_mode.as_deref() == Some("git") {
                Some("HEAD".to_string())
            } else {
                None
            }
        });

    ArgoApplicationSourceSummary {
        repo_url: source
            .get("repoURL")
            .and_then(|repo| repo.as_str())
            .map(String::from),
        target_revision,
        resolved_revision,
        path: source
            .get("path")
            .and_then(|path| path.as_str())
            .map(String::from),
        chart: source
            .get("chart")
            .and_then(|chart| chart.as_str())
            .map(String::from),
        source_mode,
        reference: source
            .get("ref")
            .and_then(|reference| reference.as_str())
            .map(String::from),
    }
}

fn source_mode(source: &serde_json::Map<String, serde_json::Value>) -> Option<String> {
    if source.get("plugin").is_some() {
        return Some("plugin".to_string());
    }
    if source.get("helm").is_some() || source.get("chart").is_some() {
        return Some("helm".to_string());
    }
    if source.get("repoURL").is_some() || source.get("path").is_some() {
        return Some("git".to_string());
    }
    Some("unknown".to_string())
}

fn primary_source_repo(sources: &[ArgoApplicationSourceSummary]) -> Option<String> {
    sources.iter().find_map(|source| source.repo_url.clone())
}

fn primary_source_revision(sources: &[ArgoApplicationSourceSummary]) -> Option<String> {
    let revisions = sources
        .iter()
        .filter_map(|source| {
            source
                .target_revision
                .clone()
                .or_else(|| source.resolved_revision.clone())
        })
        .collect::<BTreeSet<_>>();
    if revisions.len() == 1 {
        return revisions.into_iter().next();
    }
    if revisions.len() > 1 {
        return Some(format!("{} revisions", revisions.len()));
    }
    None
}

fn application_source_mode(sources: &[ArgoApplicationSourceSummary]) -> Option<String> {
    if sources.len() > 1 {
        return Some("multi".to_string());
    }
    sources
        .first()
        .and_then(|source| source.source_mode.clone())
        .or_else(|| Some("unknown".to_string()))
}

fn application_source_count(sources: &[ArgoApplicationSourceSummary]) -> Option<usize> {
    (sources.len() > 1).then_some(sources.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kube::core::{ApiResource, DynamicObject};
    use serde_json::json;

    fn application(data: serde_json::Value) -> DynamicObject {
        DynamicObject::new(
            "checkout",
            &ApiResource {
                group: "argoproj.io".to_string(),
                version: "v1alpha1".to_string(),
                api_version: "argoproj.io/v1alpha1".to_string(),
                kind: "Application".to_string(),
                plural: "applications".to_string(),
            },
        )
        .within("argocd")
        .data(data)
    }

    #[test]
    fn projects_application_summary_from_dynamic_object() {
        let object = application(json!({
            "spec": {
                "project": "payments",
                "destination": { "namespace": "payments", "server": "in-cluster" },
                "source": { "repoURL": "https://git.example/apps", "targetRevision": "main", "path": "checkout" }
            },
            "status": {
                "sync": { "status": "Synced" },
                "health": { "status": "Healthy" },
                "resources": [{ "kind": "Deployment", "namespace": "payments", "name": "checkout" }]
            }
        }));

        let summary = application_summary_from_object("kind-dev", &object).expect("summary");

        assert_eq!(summary.cluster, "kind-dev");
        assert_eq!(summary.name, "checkout");
        assert_eq!(summary.namespace.as_deref(), Some("argocd"));
        assert_eq!(summary.project.as_deref(), Some("payments"));
        assert_eq!(summary.sync_status.as_deref(), Some("Synced"));
        assert_eq!(summary.health_status.as_deref(), Some("Healthy"));
        assert_eq!(
            summary.source_repo.as_deref(),
            Some("https://git.example/apps")
        );
        assert_eq!(summary.resource_namespaces, vec!["payments"]);
    }

    #[test]
    fn rejects_non_object_application_data() {
        assert!(application_summary_from_object("kind-dev", &application(json!(null))).is_none());
    }

    #[test]
    fn extracts_unique_tracked_resource_namespaces() {
        let data = json!({
            "spec": {
                "destination": {
                    "namespace": "fallback"
                }
            },
            "status": {
                "resources": [
                    { "kind": "Deployment", "namespace": "traefik", "name": "traefik" },
                    { "kind": "Service", "namespace": "traefik", "name": "traefik" },
                    { "kind": "ConfigMap", "namespace": "monitoring", "name": "dashboard" },
                    { "kind": "ClusterRole", "name": "traefik" },
                    { "kind": "CustomResourceDefinition", "namespace": "", "name": "ignored" }
                ]
            }
        });
        let data = data.as_object().expect("application data");

        assert_eq!(
            application_resource_namespaces(data, Some("fallback")),
            vec!["monitoring".to_string(), "traefik".to_string()]
        );
        assert_eq!(application_tracked_resource_count(data), Some(5));
    }

    #[test]
    fn falls_back_to_destination_namespace_without_status_resources() {
        let data = json!({
            "spec": {
                "destination": {
                    "namespace": "traefik"
                }
            }
        });
        let data = data.as_object().expect("application data");

        assert_eq!(
            application_resource_namespaces(data, Some("traefik")),
            vec!["traefik".to_string()]
        );
        assert_eq!(application_tracked_resource_count(data), None);
    }

    #[test]
    fn returns_empty_namespaces_for_all_namespace_fallback() {
        let data = json!({
            "status": {
                "resources": [
                    { "kind": "ClusterRole", "name": "traefik" }
                ]
            }
        });
        let data = data.as_object().expect("application data");

        assert_eq!(
            application_resource_namespaces(data, None),
            Vec::<String>::new()
        );
        assert_eq!(application_tracked_resource_count(data), Some(1));
    }

    #[test]
    fn detects_application_source_modes() {
        let multi = json!({
            "spec": {
                "sources": [
                    { "repoURL": "https://git.example/apps", "path": "apps/api" },
                    { "repoURL": "https://charts.example", "chart": "redis" }
                ]
            }
        });
        let helm = json!({
            "spec": {
                "source": {
                    "repoURL": "https://charts.example",
                    "chart": "grafana"
                }
            }
        });
        let plugin = json!({
            "spec": {
                "source": {
                    "repoURL": "https://git.example/apps",
                    "plugin": { "name": "custom" }
                }
            }
        });
        let git = json!({
            "spec": {
                "source": {
                    "repoURL": "https://git.example/apps",
                    "path": "apps/api"
                }
            }
        });

        let multi = multi.as_object().expect("application data");
        let helm = helm.as_object().expect("application data");
        let plugin = plugin.as_object().expect("application data");
        let git = git.as_object().expect("application data");

        let multi_sources = application_sources(multi);
        let helm_sources = application_sources(helm);
        let plugin_sources = application_sources(plugin);
        let git_sources = application_sources(git);

        assert_eq!(
            application_source_mode(&multi_sources).as_deref(),
            Some("multi")
        );
        assert_eq!(application_source_count(&multi_sources), Some(2));
        assert_eq!(
            primary_source_repo(&multi_sources).as_deref(),
            Some("https://git.example/apps")
        );
        assert_eq!(
            primary_source_revision(&multi_sources).as_deref(),
            Some("HEAD")
        );
        assert_eq!(multi_sources[0].path.as_deref(), Some("apps/api"));
        assert_eq!(multi_sources[1].chart.as_deref(), Some("redis"));
        assert_eq!(
            application_source_mode(&helm_sources).as_deref(),
            Some("helm")
        );
        assert_eq!(
            application_source_mode(&plugin_sources).as_deref(),
            Some("plugin")
        );
        assert_eq!(
            application_source_mode(&git_sources).as_deref(),
            Some("git")
        );
    }
}
