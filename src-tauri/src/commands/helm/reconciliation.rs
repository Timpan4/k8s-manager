use super::storage::{client_for_context, get_helm_storage_record};
use crate::commands::{
    helpers::list_params,
    resources::{api_resource_from_discovered, dynamic_resource_summary},
};
use crate::models::{
    AppError, DiscoveredResourceKind, HelmManifestResourceSummary, HelmReconciliationResource,
    HelmReconciliationStatus, HelmReconciliationTotals, HelmReleaseReconciliation,
    HelmReleaseSummary, ResourceSummary,
};
use kube::{
    api::{Api, DynamicObject},
    discovery::{verbs, Discovery, Scope},
    Client, Error as KubeError,
};
use std::collections::{BTreeMap, BTreeSet};

const HELM_RELEASE_LABEL: &str = "helm.sh/release";
const CONSERVATIVE_SCAN_WARNING: &str =
    "Label-only reconciliation scans explicit Helm release labels for manifest kinds and common namespaced resource kinds; it does not exhaustively scan every CRD.";
const COMMON_LABEL_SCAN_KINDS: &[&str] = &[
    "Pod",
    "Deployment",
    "StatefulSet",
    "DaemonSet",
    "Service",
    "Ingress",
    "ConfigMap",
    "Secret",
    "PersistentVolumeClaim",
    "Job",
    "CronJob",
];

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ResourceKey {
    api_version: String,
    kind: String,
    namespace: Option<String>,
    name: String,
}

#[derive(Debug, Clone)]
struct ResourceIdentity {
    key: ResourceKey,
    resource_kind: DiscoveredResourceKind,
}

#[tauri::command]
pub async fn get_helm_release_reconciliation(
    cluster_context: String,
    namespace: String,
    storage_kind: String,
    storage_name: String,
    kubeconfig_env_var: Option<String>,
) -> Result<HelmReleaseReconciliation, AppError> {
    let (client, _) = client_for_context(&cluster_context, kubeconfig_env_var).await?;
    let record = get_helm_storage_record(
        client.clone(),
        &cluster_context,
        &namespace,
        &storage_kind,
        &storage_name,
    )
    .await?;
    let discovered = discovered_resources(client.clone()).await?;
    let (resources, warnings) = reconcile_release(
        client,
        &cluster_context,
        &record.summary,
        &record.manifest_resources,
        &discovered,
    )
    .await;

    Ok(HelmReleaseReconciliation {
        summary: record.summary,
        totals: reconciliation_totals(&resources),
        resources,
        warnings,
    })
}

async fn discovered_resources(client: Client) -> Result<Vec<DiscoveredResourceKind>, AppError> {
    let discovery = Discovery::new(client)
        .run_aggregated()
        .await
        .map_err(AppError::from)?;
    let mut kinds = Vec::new();
    for group in discovery.groups() {
        for (api_resource, capabilities) in group.recommended_resources() {
            if !capabilities.supports_operation(verbs::GET)
                && !capabilities.supports_operation(verbs::LIST)
            {
                continue;
            }
            kinds.push(DiscoveredResourceKind {
                group: api_resource.group,
                version: api_resource.version,
                api_version: api_resource.api_version,
                kind: api_resource.kind,
                plural: api_resource.plural,
                short_names: Vec::new(),
                namespaced: matches!(capabilities.scope, Scope::Namespaced),
            });
        }
    }
    kinds.sort_by(|a, b| {
        a.api_version
            .cmp(&b.api_version)
            .then(a.kind.cmp(&b.kind))
            .then(a.plural.cmp(&b.plural))
    });
    kinds.dedup_by(|a, b| a.api_version == b.api_version && a.kind == b.kind);
    Ok(kinds)
}

async fn reconcile_release(
    client: Client,
    cluster_context: &str,
    release: &HelmReleaseSummary,
    manifest_refs: &[HelmManifestResourceSummary],
    discovered: &[DiscoveredResourceKind],
) -> (Vec<HelmReconciliationResource>, Vec<String>) {
    let discovered_by_ref = discovered_ref_index(discovered);
    let mut rows_by_key = BTreeMap::new();
    let mut unavailable_rows = Vec::new();
    let mut manifest_scan_kinds = BTreeSet::new();

    for manifest_ref in manifest_refs {
        match resource_identity_from_manifest(manifest_ref, &release.namespace, &discovered_by_ref)
        {
            Ok(identity) => {
                manifest_scan_kinds
                    .insert((identity.key.api_version.clone(), identity.key.kind.clone()));
                let row = live_row_for_manifest(
                    client.clone(),
                    cluster_context,
                    release,
                    manifest_ref,
                    &identity,
                )
                .await;
                rows_by_key.insert(identity.key, row);
            }
            Err(message) => unavailable_rows.push(unavailable_row(manifest_ref, true, message)),
        }
    }

    let mut warnings = vec![CONSERVATIVE_SCAN_WARNING.to_string()];
    scan_label_only_resources(
        client,
        cluster_context,
        release,
        discovered,
        &manifest_scan_kinds,
        &mut rows_by_key,
        &mut warnings,
    )
    .await;

    let mut rows: Vec<HelmReconciliationResource> = rows_by_key.into_values().collect();
    rows.extend(unavailable_rows);
    rows.sort_by_key(row_sort_key);
    warnings.sort();
    warnings.dedup();
    (rows, warnings)
}

fn discovered_ref_index(
    discovered: &[DiscoveredResourceKind],
) -> BTreeMap<(String, String), DiscoveredResourceKind> {
    discovered
        .iter()
        .map(|kind| ((kind.api_version.clone(), kind.kind.clone()), kind.clone()))
        .collect()
}

fn resource_identity_from_manifest(
    manifest_ref: &HelmManifestResourceSummary,
    release_namespace: &str,
    discovered_by_ref: &BTreeMap<(String, String), DiscoveredResourceKind>,
) -> Result<ResourceIdentity, String> {
    let api_version = required_ref_field(manifest_ref.api_version.as_deref(), "apiVersion")?;
    let kind = required_ref_field(manifest_ref.kind.as_deref(), "kind")?;
    let name = required_ref_field(manifest_ref.name.as_deref(), "name")?;
    let resource_kind = discovered_by_ref
        .get(&(api_version.clone(), kind.clone()))
        .cloned()
        .ok_or_else(|| format!("{api_version} {kind} is not available through discovery"))?;
    let namespace = if resource_kind.namespaced {
        Some(
            manifest_ref
                .namespace
                .clone()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| release_namespace.to_string()),
        )
    } else {
        None
    };
    crate::commands::helpers::validate_namespace(namespace.as_deref())
        .map_err(|error| error.message)?;
    crate::commands::helpers::validate_path_segment(&name, "manifest resource name")
        .map_err(|error| error.message)?;
    Ok(ResourceIdentity {
        key: ResourceKey {
            api_version,
            kind,
            namespace,
            name,
        },
        resource_kind,
    })
}

fn required_ref_field(value: Option<&str>, label: &str) -> Result<String, String> {
    value
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("manifest resource is missing {label}"))
}

async fn live_row_for_manifest(
    client: Client,
    cluster_context: &str,
    release: &HelmReleaseSummary,
    manifest_ref: &HelmManifestResourceSummary,
    identity: &ResourceIdentity,
) -> HelmReconciliationResource {
    let api_resource = match api_resource_from_discovered(&identity.resource_kind) {
        Ok(api_resource) => api_resource,
        Err(err) => {
            return unavailable_row(manifest_ref, true, err.message);
        }
    };
    let api: Api<DynamicObject> = if identity.resource_kind.namespaced {
        Api::namespaced_with(
            client,
            identity
                .key
                .namespace
                .as_deref()
                .expect("namespaced identity has namespace"),
            &api_resource,
        )
    } else {
        Api::all_with(client, &api_resource)
    };

    match api.get(&identity.key.name).await {
        Ok(object) => {
            let summary =
                dynamic_resource_summary(cluster_context, &identity.resource_kind, &object);
            let explicit = has_explicit_helm_label(&summary, release);
            let status = if explicit {
                HelmReconciliationStatus::Tracked
            } else {
                HelmReconciliationStatus::UnlabeledLive
            };
            HelmReconciliationResource {
                api_version: Some(identity.key.api_version.clone()),
                kind: Some(identity.key.kind.clone()),
                namespace: identity.key.namespace.clone(),
                name: Some(identity.key.name.clone()),
                status,
                status_message: if explicit {
                    "Manifest resource exists and carries the explicit Helm release label."
                        .to_string()
                } else {
                    "Manifest resource exists, but the explicit Helm release label is missing or different."
                        .to_string()
                },
                in_manifest: true,
                explicit_helm_label: explicit,
                live_resource: Some(summary),
            }
        }
        Err(err) if is_not_found(&err) => HelmReconciliationResource {
            api_version: Some(identity.key.api_version.clone()),
            kind: Some(identity.key.kind.clone()),
            namespace: identity.key.namespace.clone(),
            name: Some(identity.key.name.clone()),
            status: HelmReconciliationStatus::Missing,
            status_message: "Manifest resource was not found in the live cluster.".to_string(),
            in_manifest: true,
            explicit_helm_label: false,
            live_resource: None,
        },
        Err(err) => unavailable_row(manifest_ref, true, err.to_string()),
    }
}

async fn scan_label_only_resources(
    client: Client,
    cluster_context: &str,
    release: &HelmReleaseSummary,
    discovered: &[DiscoveredResourceKind],
    manifest_scan_kinds: &BTreeSet<(String, String)>,
    rows_by_key: &mut BTreeMap<ResourceKey, HelmReconciliationResource>,
    warnings: &mut Vec<String>,
) {
    let scan_kinds = conservative_label_scan_kinds(discovered, manifest_scan_kinds);
    if let Err(error) = crate::commands::helpers::validate_namespace(Some(&release.namespace)) {
        warnings.push(error.message);
        return;
    }
    let label_selector = format!("{HELM_RELEASE_LABEL}={}", release.name);
    for resource_kind in scan_kinds {
        let api_resource = match api_resource_from_discovered(&resource_kind) {
            Ok(api_resource) => api_resource,
            Err(err) => {
                warnings.push(format!(
                    "Could not scan {} {}: {}",
                    resource_kind.api_version, resource_kind.kind, err.message
                ));
                continue;
            }
        };
        let api: Api<DynamicObject> = if resource_kind.namespaced {
            Api::namespaced_with(client.clone(), &release.namespace, &api_resource)
        } else {
            Api::all_with(client.clone(), &api_resource)
        };
        let params = list_params().labels(&label_selector);
        let items = match api.list(&params).await {
            Ok(items) => items,
            Err(err) => {
                warnings.push(format!(
                    "Could not scan {} {} for explicit Helm labels: {}",
                    resource_kind.api_version, resource_kind.kind, err
                ));
                continue;
            }
        };
        for object in items.items {
            let summary = dynamic_resource_summary(cluster_context, &resource_kind, &object);
            if !has_explicit_helm_label(&summary, release) || summary.name.is_empty() {
                continue;
            }
            let key = key_from_summary(&summary);
            rows_by_key
                .entry(key)
                .or_insert_with(|| label_only_row(summary));
        }
    }
}

fn conservative_label_scan_kinds(
    discovered: &[DiscoveredResourceKind],
    manifest_scan_kinds: &BTreeSet<(String, String)>,
) -> Vec<DiscoveredResourceKind> {
    let common: BTreeSet<&str> = COMMON_LABEL_SCAN_KINDS.iter().copied().collect();
    let mut by_key = BTreeMap::new();
    for kind in discovered {
        if manifest_scan_kinds.contains(&(kind.api_version.clone(), kind.kind.clone()))
            || (kind.namespaced && common.contains(kind.kind.as_str()))
        {
            by_key.insert((kind.api_version.clone(), kind.kind.clone()), kind.clone());
        }
    }
    by_key.into_values().collect()
}

fn has_explicit_helm_label(summary: &ResourceSummary, release: &HelmReleaseSummary) -> bool {
    summary.helm_release.as_deref() == Some(release.name.as_str())
}

fn key_from_summary(summary: &ResourceSummary) -> ResourceKey {
    ResourceKey {
        api_version: summary.api_version.clone().unwrap_or_default(),
        kind: summary.kind.clone(),
        namespace: summary.namespace.clone(),
        name: summary.name.clone(),
    }
}

fn label_only_row(summary: ResourceSummary) -> HelmReconciliationResource {
    HelmReconciliationResource {
        api_version: summary.api_version.clone(),
        kind: Some(summary.kind.clone()),
        namespace: summary.namespace.clone(),
        name: Some(summary.name.clone()),
        status: HelmReconciliationStatus::LabelOnly,
        status_message: "Live resource carries the explicit Helm release label but is absent from the decoded manifest."
            .to_string(),
        in_manifest: false,
        explicit_helm_label: true,
        live_resource: Some(summary),
    }
}

fn unavailable_row(
    manifest_ref: &HelmManifestResourceSummary,
    in_manifest: bool,
    message: impl Into<String>,
) -> HelmReconciliationResource {
    HelmReconciliationResource {
        api_version: manifest_ref.api_version.clone(),
        kind: manifest_ref.kind.clone(),
        namespace: manifest_ref.namespace.clone(),
        name: manifest_ref.name.clone(),
        status: HelmReconciliationStatus::Unavailable,
        status_message: message.into(),
        in_manifest,
        explicit_helm_label: false,
        live_resource: None,
    }
}

fn is_not_found(err: &KubeError) -> bool {
    matches!(err, KubeError::Api(api_error) if api_error.code == 404)
}

fn reconciliation_totals(rows: &[HelmReconciliationResource]) -> HelmReconciliationTotals {
    let mut totals = HelmReconciliationTotals::default();
    for row in rows {
        match row.status {
            HelmReconciliationStatus::Tracked => totals.tracked += 1,
            HelmReconciliationStatus::UnlabeledLive => totals.unlabeled_live += 1,
            HelmReconciliationStatus::Missing => totals.missing += 1,
            HelmReconciliationStatus::LabelOnly => totals.label_only += 1,
            HelmReconciliationStatus::Unavailable => totals.unavailable += 1,
        }
    }
    totals
}

fn row_sort_key(row: &HelmReconciliationResource) -> (String, String, String, String) {
    (
        row.namespace.clone().unwrap_or_default(),
        row.kind.clone().unwrap_or_default(),
        row.name.clone().unwrap_or_default(),
        row.api_version.clone().unwrap_or_default(),
    )
}

#[cfg(test)]
#[path = "reconciliation_tests.rs"]
mod reconciliation_tests;
