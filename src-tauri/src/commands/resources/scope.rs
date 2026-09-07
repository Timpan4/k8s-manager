use super::{dynamic_resources_summary_from, resources_summary_from};
use crate::commands::diagnostics::record_backend_result;
use crate::commands::helpers::{
    enrich_resource_summaries_with_flux_inventory, fetch_flux_ownership_index,
    filter_flux_ownership_index,
};
use crate::models::AppErrorKind;
use crate::{
    commands::{
        diagnostic_field,
        kubeconfig::{kubeconfig_source_key, KubeconfigSource},
        BackendCancellationRegistry, ClusterLiveStore,
    },
    models::{AppError, DiscoveredResourceKind, ResourceListRequest, ResourceSummary},
};
use futures_util::future::join_all;
use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;
use tauri::State;

#[derive(Debug, Clone)]
enum ResourceScopeKind {
    Typed(String),
    Dynamic(DiscoveredResourceKind),
}

#[derive(Debug)]
struct ResourceScopeGroup {
    kind: ResourceScopeKind,
    namespaces: BTreeSet<String>,
    all_namespaces: bool,
}

impl ResourceScopeGroup {
    fn new(kind: ResourceScopeKind) -> Self {
        Self {
            kind,
            namespaces: BTreeSet::new(),
            all_namespaces: false,
        }
    }

    fn add_namespace(&mut self, namespace: Option<String>) {
        match namespace {
            Some(namespace) if !namespace.trim().is_empty() => {
                self.namespaces.insert(namespace);
            }
            _ => self.all_namespaces = true,
        }
    }
}

fn request_kind(request: &ResourceListRequest) -> Result<ResourceScopeKind, AppError> {
    match (&request.kind, &request.resource_kind) {
        (Some(kind), None) if !kind.trim().is_empty() => Ok(ResourceScopeKind::Typed(kind.clone())),
        (None, Some(resource_kind)) => Ok(ResourceScopeKind::Dynamic(resource_kind.clone())),
        _ => Err(AppError::new(
            "resource scope request must include exactly one kind",
            AppErrorKind::Validation,
        )),
    }
}

fn kind_key(kind: &ResourceScopeKind) -> String {
    match kind {
        ResourceScopeKind::Typed(kind) => format!("typed:{kind}"),
        ResourceScopeKind::Dynamic(kind) => {
            format!("dynamic:{}:{}:{}", kind.api_version, kind.plural, kind.kind)
        }
    }
}

fn group_requests(
    requests: Vec<ResourceListRequest>,
) -> Result<BTreeMap<String, ResourceScopeGroup>, AppError> {
    let mut groups = BTreeMap::new();
    for request in requests {
        let kind = request_kind(&request)?;
        let key = kind_key(&kind);
        groups
            .entry(key)
            .or_insert_with(|| ResourceScopeGroup::new(kind))
            .add_namespace(request.namespace);
    }
    Ok(groups)
}

fn should_promote_to_all(group: &ResourceScopeGroup) -> bool {
    group.all_namespaces || group.namespaces.is_empty()
}

fn fetch_namespaces(group: &ResourceScopeGroup) -> Vec<Option<String>> {
    if should_promote_to_all(group) {
        return vec![None];
    }
    group.namespaces.iter().cloned().map(Some).collect()
}

fn ownership_namespaces(groups: &BTreeMap<String, ResourceScopeGroup>) -> Vec<String> {
    if groups.values().any(should_promote_to_all) {
        Vec::new()
    } else {
        groups
            .values()
            .flat_map(|group| group.namespaces.iter().cloned())
            .collect()
    }
}

fn dedupe_rows(rows: Vec<ResourceSummary>) -> Vec<ResourceSummary> {
    let mut by_key = BTreeMap::new();
    for row in rows {
        by_key.insert(
            format!(
                "{}:{}:{}:{}:{}:{}",
                row.cluster,
                row.kind,
                row.api_version.as_deref().unwrap_or(""),
                row.plural.as_deref().unwrap_or(""),
                row.namespace.as_deref().unwrap_or(""),
                row.name
            ),
            row,
        );
    }
    by_key.into_values().collect()
}

pub async fn resource_scope_from(
    cluster_context: String,
    requests: Vec<ResourceListRequest>,
    live_store: ClusterLiveStore,
    kubeconfig_env_var: Option<String>,
) -> Result<Vec<ResourceSummary>, AppError> {
    if requests.is_empty() {
        return Ok(Vec::new());
    }
    let groups = group_requests(requests)?;
    let requested_namespaces = ownership_namespaces(&groups);
    let source_key = kubeconfig_source_key(kubeconfig_env_var.as_deref())?;
    let futures = groups.into_values().map(|group| {
        let live_store = live_store.clone();
        let cluster_context = cluster_context.clone();
        let source_key = source_key.clone();
        let kubeconfig_env_var = kubeconfig_env_var.clone();
        async move {
            let fetches = fetch_namespaces(&group).into_iter().map(|namespace| {
                let kind = group.kind.clone();
                let live_store = live_store.clone();
                let cluster_context = cluster_context.clone();
                let source_key = source_key.clone();
                let kubeconfig_env_var = kubeconfig_env_var.clone();
                async move {
                    match kind {
                        ResourceScopeKind::Typed(kind) => {
                            live_store
                                .typed_resources(
                                    source_key,
                                    cluster_context.clone(),
                                    kind.clone(),
                                    namespace.clone(),
                                    {
                                        let cluster_context = cluster_context.clone();
                                        let kubeconfig_env_var = kubeconfig_env_var.clone();
                                        move || {
                                            resources_summary_from(
                                                cluster_context,
                                                kind,
                                                namespace,
                                                kubeconfig_env_var,
                                            )
                                        }
                                    },
                                )
                                .await
                        }
                        ResourceScopeKind::Dynamic(resource_kind) => {
                            live_store
                                .dynamic_resources(
                                    source_key,
                                    cluster_context.clone(),
                                    resource_kind.clone(),
                                    namespace.clone(),
                                    {
                                        let cluster_context = cluster_context.clone();
                                        let kubeconfig_env_var = kubeconfig_env_var.clone();
                                        move || {
                                            dynamic_resources_summary_from(
                                                cluster_context,
                                                resource_kind,
                                                namespace,
                                                kubeconfig_env_var,
                                            )
                                        }
                                    },
                                )
                                .await
                        }
                    }
                }
            });
            let mut rows = Vec::new();
            for result in join_all(fetches).await {
                rows.extend(result?);
            }
            Ok::<_, AppError>(rows)
        }
    });

    let results = join_all(futures).await;
    let mut rows = Vec::new();
    for result in results {
        rows.extend(result?);
    }
    let client = KubeconfigSource::new(kubeconfig_env_var)?
        .client_for_context(&cluster_context)
        .await?;
    let index = live_store
        .flux_ownership_index(source_key, cluster_context.clone(), {
            let client = client.clone();
            move || fetch_flux_ownership_index(client)
        })
        .await?;
    let index = filter_flux_ownership_index(index, &requested_namespaces);
    enrich_resource_summaries_with_flux_inventory(&mut rows, &index);
    Ok(dedupe_rows(rows))
}

#[tauri::command]
pub async fn list_resource_scope(
    cluster_context: String,
    requests: Vec<ResourceListRequest>,
    kubeconfig_env_var: Option<String>,
    request_id: Option<String>,
    cancel_scope: Option<String>,
    live_store: State<'_, ClusterLiveStore>,
    cancellations: State<'_, BackendCancellationRegistry>,
) -> Result<Vec<ResourceSummary>, AppError> {
    let started = Instant::now();
    let request_count = requests.len();
    let namespace_count = requests
        .iter()
        .filter_map(|request| request.namespace.as_deref())
        .filter(|namespace| !namespace.trim().is_empty())
        .collect::<BTreeSet<_>>()
        .len();
    eprintln!(
        "[kubecove:backend] list_resource_scope start context={cluster_context} requests={request_count}",
    );
    let result = cancellations
        .execute(
            cancel_scope,
            request_id,
            resource_scope_from(
                cluster_context.clone(),
                requests,
                live_store.inner().clone(),
                kubeconfig_env_var,
            ),
        )
        .await;
    match &result {
        Ok(rows) => {
            eprintln!(
                "[kubecove:backend] list_resource_scope done context={} rows={} ms={}",
                cluster_context,
                rows.len(),
                started.elapsed().as_millis()
            );
        }
        Err(err) if err.kind == AppErrorKind::Cancelled => {
            eprintln!(
                "[kubecove:backend] list_resource_scope cancelled context={} ms={}",
                cluster_context,
                started.elapsed().as_millis()
            );
        }
        Err(err) => {
            eprintln!(
                "[kubecove:backend] list_resource_scope error context={} error_kind={} message={} ms={}",
                cluster_context,
                err.kind,
                err.message,
                started.elapsed().as_millis()
            );
        }
    }
    record_backend_result("list_resource_scope", started, &result, |rows| {
        vec![
            diagnostic_field("requests", request_count),
            diagnostic_field("namespaces", namespace_count),
            diagnostic_field("rows", rows.len()),
        ]
    });
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pod_request(namespace: Option<&str>) -> ResourceListRequest {
        ResourceListRequest {
            kind: Some("Pod".to_string()),
            resource_kind: None,
            namespace: namespace.map(ToString::to_string),
        }
    }

    #[tokio::test]
    async fn empty_requests_return_without_kubeconfig_access() {
        let result = resource_scope_from(
            "missing-context".to_string(),
            Vec::new(),
            ClusterLiveStore::default(),
            Some("missing-kubeconfig".to_string()),
        )
        .await;

        assert!(result.expect("empty scope").is_empty());
    }

    #[test]
    fn groups_multiple_namespaces_for_one_kind_without_promoting_to_all() {
        let groups = group_requests(vec![
            pod_request(Some("default")),
            pod_request(Some("argocd")),
        ])
        .expect("groups");
        let group = groups.get("typed:Pod").expect("pod group");

        assert!(!should_promote_to_all(group));
        assert_eq!(
            fetch_namespaces(group),
            vec![Some("argocd".to_string()), Some("default".to_string())]
        );
        assert_eq!(group.namespaces.len(), 2);
    }

    #[test]
    fn keeps_large_namespace_sets_namespace_scoped() {
        let groups = group_requests(vec![
            pod_request(Some("default")),
            pod_request(Some("argocd")),
            pod_request(Some("jobs")),
            pod_request(Some("kube-system")),
            pod_request(Some("monitoring")),
            pod_request(Some("platform")),
            pod_request(Some("staging")),
            pod_request(Some("prod")),
        ])
        .expect("groups");
        let group = groups.get("typed:Pod").expect("pod group");

        assert!(!should_promote_to_all(group));
        assert_eq!(
            fetch_namespaces(group),
            vec![
                Some("argocd".to_string()),
                Some("default".to_string()),
                Some("jobs".to_string()),
                Some("kube-system".to_string()),
                Some("monitoring".to_string()),
                Some("platform".to_string()),
                Some("prod".to_string()),
                Some("staging".to_string()),
            ]
        );
    }

    #[test]
    fn promotes_explicit_all_namespace_request_to_one_fetch() {
        let groups = group_requests(vec![pod_request(None)]).expect("groups");
        let group = groups.get("typed:Pod").expect("pod group");

        assert!(should_promote_to_all(group));
        assert_eq!(fetch_namespaces(group), vec![None]);
    }

    #[test]
    fn all_namespace_group_keeps_ownership_read_cluster_wide() {
        let groups =
            group_requests(vec![pod_request(None), pod_request(Some("default"))]).expect("groups");

        assert!(ownership_namespaces(&groups).is_empty());
    }

    #[test]
    fn rejects_requests_without_one_kind_identity() {
        let err = request_kind(&ResourceListRequest {
            kind: None,
            resource_kind: None,
            namespace: None,
        })
        .expect_err("missing kind");

        assert_eq!(err.kind, AppErrorKind::Validation);
    }
}
