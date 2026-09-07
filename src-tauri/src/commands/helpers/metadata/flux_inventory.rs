use crate::commands::flux::FLUX_KINDS;
use crate::models::AppErrorKind;
use crate::models::{AppError, GitOpsOwnerSummary, ResourceSummary};
use kube::{
    api::{Api, ApiResource, DynamicObject, ListParams},
    Client,
};
use std::collections::HashMap;

pub(crate) const MAX_FLUX_OWNER_OBJECTS: usize = 64;
pub(crate) const MAX_FLUX_INVENTORY_ENTRIES: usize = 2048;

#[derive(Debug, Clone, Default)]
pub(crate) struct FluxOwnershipIndex {
    owners: HashMap<ResourceKey, GitOpsOwnerSummary>,
    pub partial: bool,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct ResourceKey {
    namespace: Option<String>,
    api_version: String,
    kind: String,
    name: String,
}

pub(crate) fn parse_inventory_id(id: &str) -> Option<(Option<String>, String, String, String)> {
    let mut parts = id.split('_');
    let namespace = parts.next()?.trim();
    let name = parts.next()?.trim();
    let mut rest: Vec<_> = parts.collect();
    let kind = rest.pop()?.trim();
    let group = rest.join("_");
    if name.is_empty() || kind.is_empty() {
        return None;
    }
    Some((
        (!namespace.is_empty()).then(|| namespace.to_string()),
        name.to_string(),
        group,
        kind.to_string(),
    ))
}

fn normalized_api_version(group: &str, version: Option<&str>) -> Option<String> {
    let version = version?.trim();
    if version.is_empty() {
        return None;
    }
    let group = group.trim();
    Some(if group.is_empty() {
        version.to_string()
    } else {
        format!("{group}/{version}")
    })
}

fn has_continuation_token(token: Option<&str>) -> bool {
    token.is_some_and(|token| !token.is_empty())
}

fn target_namespace_allowed(namespace: Option<&str>, requested: &[String]) -> bool {
    namespace.is_none()
        || requested
            .iter()
            .any(|wanted| Some(wanted.as_str()) == namespace)
}

pub(crate) async fn read_flux_ownership_index(
    client: Client,
    namespaces: &[String],
) -> Result<FluxOwnershipIndex, AppError> {
    let index = fetch_flux_ownership_index(client).await?;
    Ok(filter_flux_ownership_index(index, namespaces))
}

/// Lists Flux owner objects cluster-wide. Namespace scoping happens later in
/// [`filter_flux_ownership_index`] so callers can cache the fetched index and
/// re-scope it per request without re-listing.
pub(crate) async fn fetch_flux_ownership_index(
    client: Client,
) -> Result<FluxOwnershipIndex, AppError> {
    let mut index = FluxOwnershipIndex::default();
    for (group, version, kind, plural, _, _) in FLUX_KINDS
        .iter()
        .filter(|(_, _, kind, _, _, _)| *kind == "Kustomization" || *kind == "HelmRelease")
    {
        let resource = ApiResource {
            group: group.to_string(),
            version: version.to_string(),
            api_version: format!("{group}/{version}"),
            kind: kind.to_string(),
            plural: plural.to_string(),
        };
        let mut objects = Vec::new();
        // Owners are cluster-wide evidence; target namespaces only filter retained inventory identities.
        let api = Api::<DynamicObject>::all_with(client.clone(), &resource);
        for api in [api] {
            match api
                .list(&ListParams::default().limit(MAX_FLUX_OWNER_OBJECTS as u32))
                .await
            {
                Ok(rows) => {
                    index.partial |= has_continuation_token(rows.metadata.continue_.as_deref());
                    objects.extend(rows.items);
                }
                Err(e) => {
                    let error = AppError::from(e);
                    if error.kind == AppErrorKind::Cancelled {
                        return Err(error);
                    }
                    index.partial = true;
                }
            }
        }
        if objects.len() >= MAX_FLUX_OWNER_OBJECTS {
            index.partial = true;
        }
        for object in objects.into_iter().take(MAX_FLUX_OWNER_OBJECTS) {
            let Some(owner_name) = object.metadata.name.clone() else {
                index.partial = true;
                continue;
            };
            let owner_namespace = object.metadata.namespace.clone();
            let entries = object
                .data
                .get("status")
                .and_then(|s| s.get("inventory"))
                .and_then(|i| i.get("entries"))
                .and_then(|e| e.as_array());
            let Some(entries) = entries else {
                index.partial = true;
                continue;
            };
            if entries.len() > MAX_FLUX_INVENTORY_ENTRIES {
                index.partial = true;
            }
            for entry in entries.iter().take(MAX_FLUX_INVENTORY_ENTRIES) {
                let Some(id) = entry.get("id").and_then(|v| v.as_str()) else {
                    index.partial = true;
                    continue;
                };
                let Some((namespace, name, group, target_kind)) = parse_inventory_id(id) else {
                    index.partial = true;
                    continue;
                };
                let Some(api_version) = normalized_api_version(
                    &group,
                    entry
                        .get("version")
                        .or_else(|| entry.get("v"))
                        .and_then(|v| v.as_str()),
                ) else {
                    index.partial = true;
                    continue;
                };
                index
                    .owners
                    .entry(ResourceKey {
                        namespace: namespace.clone(),
                        api_version,
                        kind: target_kind,
                        name,
                    })
                    .or_insert_with(|| GitOpsOwnerSummary {
                        provider: "flux".into(),
                        kind: kind.to_string(),
                        name: owner_name.clone(),
                        namespace: owner_namespace.clone(),
                        confidence: "inventory".into(),
                        provenance: "flux-inventory".into(),
                        partial: true,
                    });
            }
        }
    }
    Ok(index)
}

/// Restricts a fetched index to owners whose inventoried targets live in the
/// requested namespaces and stamps owner visibility with index partialness.
pub(crate) fn filter_flux_ownership_index(
    mut index: FluxOwnershipIndex,
    namespaces: &[String],
) -> FluxOwnershipIndex {
    if !namespaces.is_empty() {
        index
            .owners
            .retain(|key, _| target_namespace_allowed(key.namespace.as_deref(), namespaces));
    }
    for owner in index.owners.values_mut() {
        owner.partial = index.partial;
    }
    index
}

pub(crate) fn enrich_resource_summaries_with_flux_inventory(
    summaries: &mut [ResourceSummary],
    index: &FluxOwnershipIndex,
) {
    for summary in summaries.iter_mut() {
        summary.git_ops_ownership_partial |= index.partial;
        if let Some(owner) = summary.git_ops_owner.as_mut() {
            owner.partial |= index.partial;
            continue;
        }
        let key = ResourceKey {
            namespace: summary.namespace.clone(),
            api_version: summary.api_version.clone().unwrap_or_default(),
            kind: summary.kind.clone(),
            name: summary.name.clone(),
        };
        if let Some(owner) = index.owners.get(&key) {
            let mut owner = owner.clone();
            owner.partial |= index.partial;
            summary.git_ops_owner = Some(owner);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::{header::CONTENT_TYPE, Request, Response};
    use kube::client::Body;
    use serde_json::json;

    type MockHandle = tower_test::mock::Handle<Request<Body>, Response<Body>>;

    fn mock_client() -> (Client, MockHandle) {
        let (service, handle) = tower_test::mock::pair::<Request<Body>, Response<Body>>();
        (Client::new(service, "default"), handle)
    }

    fn json_response(status: u16, body: serde_json::Value) -> Response<Body> {
        Response::builder()
            .status(status)
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap()
    }

    fn inventory_list() -> serde_json::Value {
        json!({
            "apiVersion": "v1",
            "items": [{
                "apiVersion": "apps/v1",
                "kind": "Deployment",
                "metadata": {"name": "api", "namespace": "default"},
                "status": {
                    "inventory": {
                        "entries": [{"id": "default_api_apps_Deployment", "version": "v1"}]
                    }
                }
            }]
        })
    }

    #[tokio::test]
    async fn provider_list_failure_preserves_evidence_from_next_kind() {
        let (client, mut handle) = mock_client();
        let read = read_flux_ownership_index(client, &[]);
        let respond = async move {
            let (_, send) = handle.next_request().await.expect("Kustomization request");
            send.send_response(json_response(
                500,
                json!({
                    "apiVersion": "v1", "kind": "Status", "message": "provider unavailable"
                }),
            ));

            let (_, send) = handle.next_request().await.expect("HelmRelease request");
            send.send_response(json_response(200, inventory_list()));
        };

        let (index, ()) = tokio::join!(read, respond);
        let index = index.expect("partial index");
        assert!(index.partial);
        assert!(index.owners.contains_key(&ResourceKey {
            namespace: Some("default".into()),
            api_version: "apps/v1".into(),
            kind: "Deployment".into(),
            name: "api".into(),
        }));
    }

    #[tokio::test]
    async fn cancelled_provider_list_failure_is_fatal() {
        let (client, mut handle) = mock_client();
        let read = read_flux_ownership_index(client, &[]);
        let respond = async move {
            let (_, send) = handle.next_request().await.expect("Kustomization request");
            send.send_error(crate::models::WorkspaceRequestCancelled);
        };

        let (result, ()) = tokio::join!(read, respond);
        assert_eq!(
            result.expect_err("cancellation should be fatal").kind,
            AppErrorKind::Cancelled
        );
    }

    #[test]
    fn continuation_token_marks_inventory_partial() {
        assert!(has_continuation_token(Some("next-page")));
        assert!(!has_continuation_token(Some("")));
        assert!(!has_continuation_token(None));
    }

    #[test]
    fn parses_defensively() {
        assert_eq!(
            parse_inventory_id("default_api_apps_Deployment"),
            Some((
                Some("default".into()),
                "api".into(),
                "apps".into(),
                "Deployment".into(),
            ))
        );
        assert_eq!(
            parse_inventory_id("_shared_storage.k8s.io_StorageClass"),
            Some((
                None,
                "shared".into(),
                "storage.k8s.io".into(),
                "StorageClass".into(),
            ))
        );
        assert_eq!(parse_inventory_id("bad"), None);
        assert_eq!(parse_inventory_id("__apps_Deployment"), None);
    }

    #[test]
    fn complete_inventory_is_not_partial_and_coverage_marks_unmatched_rows() {
        let mut index = FluxOwnershipIndex::default();
        index.owners.insert(
            ResourceKey {
                namespace: Some("default".into()),
                api_version: "apps/v1".into(),
                kind: "Deployment".into(),
                name: "api".into(),
            },
            GitOpsOwnerSummary {
                provider: "flux".into(),
                kind: "Kustomization".into(),
                name: "apps".into(),
                namespace: Some("flux-system".into()),
                confidence: "inventory".into(),
                provenance: "flux-inventory".into(),
                partial: false,
            },
        );
        let mut summaries = vec![summary("Deployment", "api"), summary("Service", "missing")];

        enrich_resource_summaries_with_flux_inventory(&mut summaries, &index);

        assert!(!summaries[0].git_ops_owner.as_ref().unwrap().partial);
        assert_eq!(summaries[1].git_ops_owner, None);
        assert!(!summaries.iter().any(|row| row.git_ops_ownership_partial));

        index.partial = true;
        enrich_resource_summaries_with_flux_inventory(&mut summaries, &index);
        assert!(summaries.iter().all(|row| row.git_ops_ownership_partial));
        assert!(summaries[0].git_ops_owner.as_ref().unwrap().partial);
    }

    fn summary(kind: &str, name: &str) -> ResourceSummary {
        let mut summary: ResourceSummary = serde_json::from_value(json!({
            "kind": kind,
            "cluster": "kind-dev",
            "name": name,
            "namespace": "default",
            "age": "1m",
            "health": "unknown"
        }))
        .expect("summary");
        summary.api_version = Some(
            match kind {
                "Deployment" => "apps/v1",
                _ => "v1",
            }
            .to_string(),
        );
        summary
    }

    #[test]
    fn api_version_is_part_of_inventory_identity() {
        let owner = GitOpsOwnerSummary {
            provider: "flux".into(),
            kind: "Kustomization".into(),
            name: "apps".into(),
            namespace: Some("flux-system".into()),
            confidence: "inventory".into(),
            provenance: "flux-inventory".into(),
            partial: false,
        };
        let mut index = FluxOwnershipIndex::default();
        index.owners.insert(
            ResourceKey {
                namespace: Some("default".into()),
                api_version: "apps/v1".into(),
                kind: "Deployment".into(),
                name: "api".into(),
            },
            owner,
        );
        let mut summaries = vec![summary("Deployment", "api")];
        enrich_resource_summaries_with_flux_inventory(&mut summaries, &index);
        assert!(summaries[0].git_ops_owner.is_some());

        summaries[0].api_version = Some("example.com/v1".into());
        summaries[0].git_ops_owner = None;
        enrich_resource_summaries_with_flux_inventory(&mut summaries, &index);
        assert!(summaries[0].git_ops_owner.is_none());
    }

    #[test]
    fn core_and_incomplete_inventory_versions_are_handled_safely() {
        assert_eq!(normalized_api_version("", Some("v1")), Some("v1".into()));
        assert_eq!(
            normalized_api_version("apps", Some("v1")),
            Some("apps/v1".into())
        );
        assert_eq!(normalized_api_version("apps", None), None);
        assert_eq!(normalized_api_version("apps", Some("")), None);

        let mut index = FluxOwnershipIndex::default();
        let parsed = parse_inventory_id("default_config__ConfigMap").unwrap();
        let api_version = normalized_api_version(&parsed.2, Some("v1")).unwrap();
        index.owners.insert(
            ResourceKey {
                namespace: parsed.0,
                api_version,
                kind: parsed.3,
                name: parsed.1,
            },
            GitOpsOwnerSummary {
                provider: "flux".into(),
                kind: "Kustomization".into(),
                name: "apps".into(),
                namespace: None,
                confidence: "inventory".into(),
                provenance: "flux-inventory".into(),
                partial: false,
            },
        );
        let mut summaries = vec![summary("ConfigMap", "config")];
        enrich_resource_summaries_with_flux_inventory(&mut summaries, &index);
        assert!(summaries[0].git_ops_owner.is_some());
    }
    #[test]
    fn cluster_scoped_inventory_survives_namespace_filtering_and_enriches_summary() {
        let owner = GitOpsOwnerSummary {
            provider: "flux".into(),
            kind: "Kustomization".into(),
            name: "apps".into(),
            namespace: Some("flux-system".into()),
            confidence: "inventory".into(),
            provenance: "flux-inventory".into(),
            partial: false,
        };
        let mut index = FluxOwnershipIndex::default();
        index.owners.insert(
            ResourceKey {
                namespace: None,
                api_version: "storage.k8s.io/v1".into(),
                kind: "StorageClass".into(),
                name: "shared".into(),
            },
            owner.clone(),
        );
        index.owners.insert(
            ResourceKey {
                namespace: Some("default".into()),
                api_version: "apps/v1".into(),
                kind: "Deployment".into(),
                name: "api".into(),
            },
            owner.clone(),
        );
        index.owners.insert(
            ResourceKey {
                namespace: Some("other".into()),
                api_version: "apps/v1".into(),
                kind: "Deployment".into(),
                name: "api".into(),
            },
            owner.clone(),
        );

        let requested = ["default".to_string()];
        index
            .owners
            .retain(|key, _| target_namespace_allowed(key.namespace.as_deref(), &requested));

        assert!(index.owners.contains_key(&ResourceKey {
            namespace: None,
            api_version: "storage.k8s.io/v1".into(),
            kind: "StorageClass".into(),
            name: "shared".into(),
        }));
        assert!(index.owners.contains_key(&ResourceKey {
            namespace: Some("default".into()),
            api_version: "apps/v1".into(),
            kind: "Deployment".into(),
            name: "api".into(),
        }));
        assert!(!index.owners.contains_key(&ResourceKey {
            namespace: Some("other".into()),
            api_version: "apps/v1".into(),
            kind: "Deployment".into(),
            name: "api".into(),
        }));

        let mut summaries = vec![summary_cluster_scoped("StorageClass", "shared")];
        enrich_resource_summaries_with_flux_inventory(&mut summaries, &index);

        assert_eq!(summaries[0].git_ops_owner, Some(owner));
    }

    fn summary_cluster_scoped(kind: &str, name: &str) -> ResourceSummary {
        let mut summary: ResourceSummary = serde_json::from_value(json!({
            "kind": kind,
            "cluster": "kind-dev",
            "name": name,
            "namespace": null,
            "age": "1m",
            "health": "unknown"
        }))
        .expect("summary");
        summary.api_version = Some(
            match kind {
                "StorageClass" => "storage.k8s.io/v1",
                _ => "v1",
            }
            .to_string(),
        );
        summary
    }
}
