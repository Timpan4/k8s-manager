use super::manifest::{manifest_resources, manifest_summary_from_resources};
use super::redaction::{redact_configmap_release, redact_secret_release};
use super::time::parse_rfc3339;
use super::values::values_summary;
use crate::commands::{
    helpers::{
        k8s_creation_timestamp_to_rfc3339, list_params, resource_age, serialize_resource_document,
    },
    kubeconfig::KubeconfigSource,
};
use crate::models::AppErrorKind;
use crate::models::{
    AppError, HelmManifestResourceSummary, HelmManifestSummary, HelmReleaseDetails,
    HelmReleaseSummary, HelmValuesSummary, YamlEncoding, YamlViewMode,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use flate2::read::GzDecoder;
use k8s_openapi::api::core::v1::{ConfigMap, Namespace, Secret};
use kube::{
    api::{Api, ListParams, ObjectMeta},
    Client,
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, io::Read};

const HELM_OWNER_SELECTOR: &str = "owner=helm";
const HELM_STORAGE_SECRET: &str = "Secret";
const HELM_STORAGE_CONFIGMAP: &str = "ConfigMap";
// Kubernetes limits Secret and ConfigMap data to 1 MiB. The decoded budget is
// the separately approved application limit, including gzip expansion.
const MAX_STORED_RELEASE_BYTES: usize = 1024 * 1024;
const MAX_DECODED_RELEASE_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Debug, Serialize, Deserialize)]
struct DecodedHelmRelease {
    name: Option<String>,
    namespace: Option<String>,
    version: Option<i32>,
    info: Option<DecodedHelmInfo>,
    chart: Option<DecodedHelmChart>,
    config: Option<serde_json::Value>,
    manifest: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct DecodedHelmInfo {
    status: Option<String>,
    last_deployed: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct DecodedHelmChart {
    metadata: Option<DecodedHelmChartMetadata>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DecodedHelmChartMetadata {
    name: Option<String>,
    version: Option<String>,
    app_version: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) struct HelmStorageRecord {
    decode_error: Option<AppError>,
    pub(super) summary: HelmReleaseSummary,
    pub(super) release: Option<serde_json::Value>,
    pub(super) values_summary: HelmValuesSummary,
    pub(super) manifest_summary: HelmManifestSummary,
    pub(super) manifest_resources: Vec<HelmManifestResourceSummary>,
}

#[tauri::command]
pub async fn list_helm_releases(
    cluster_context: String,
    kubeconfig_env_var: Option<String>,
) -> Result<Vec<HelmReleaseSummary>, AppError> {
    let (client, default_namespace) =
        client_for_context(&cluster_context, kubeconfig_env_var).await?;
    let fallback_namespaces = helm_fallback_namespaces(client.clone(), &default_namespace).await;
    let mut records = Vec::new();
    let mut errors = Vec::new();

    match list_secret_releases(client.clone(), &cluster_context, &fallback_namespaces).await {
        Ok(mut releases) => records.append(&mut releases),
        Err(err) => errors.push(err),
    }

    match list_configmap_releases(client, &cluster_context, &fallback_namespaces).await {
        Ok(mut releases) => records.append(&mut releases),
        Err(err) => errors.push(err),
    }

    if records.is_empty() && !errors.is_empty() {
        return Err(storage_errors(errors));
    }

    Ok(latest_releases(records))
}

#[tauri::command]
pub async fn get_helm_release_details(
    cluster_context: String,
    namespace: String,
    storage_kind: String,
    storage_name: String,
    kubeconfig_env_var: Option<String>,
    yaml_view_mode: Option<YamlViewMode>,
    yaml_encoding: Option<YamlEncoding>,
) -> Result<HelmReleaseDetails, AppError> {
    let (client, _) = client_for_context(&cluster_context, kubeconfig_env_var).await?;

    let (record, metadata, yaml) = release_storage_object(
        client,
        &cluster_context,
        &namespace,
        &storage_kind,
        &storage_name,
        yaml_view_mode.unwrap_or_default(),
        yaml_encoding.unwrap_or_default(),
    )
    .await?;
    if let Some(error) = record.decode_error {
        return Err(error);
    }
    Ok(HelmReleaseDetails {
        summary: record.summary,
        yaml,
        metadata,
        values_summary: record.values_summary,
        manifest_summary: record.manifest_summary,
        release: record.release,
    })
}

pub(super) async fn get_helm_storage_record(
    client: Client,
    cluster_context: &str,
    namespace: &str,
    storage_kind: &str,
    storage_name: &str,
) -> Result<HelmStorageRecord, AppError> {
    let (record, _, _) = release_storage_object(
        client,
        cluster_context,
        namespace,
        storage_kind,
        storage_name,
        YamlViewMode::Kubectl,
        YamlEncoding::Yaml,
    )
    .await?;
    if let Some(error) = record.decode_error {
        return Err(error);
    }
    Ok(record)
}

async fn release_storage_object(
    client: Client,
    cluster_context: &str,
    namespace: &str,
    storage_kind: &str,
    storage_name: &str,
    yaml_view_mode: YamlViewMode,
    yaml_encoding: YamlEncoding,
) -> Result<(HelmStorageRecord, serde_json::Value, String), AppError> {
    crate::commands::helpers::validate_namespace(Some(namespace))?;
    crate::commands::helpers::validate_path_segment(storage_name, "Helm storage name")?;
    match storage_kind {
        HELM_STORAGE_SECRET => {
            let api: Api<Secret> = Api::namespaced(client, namespace);
            let mut secret = api.get(storage_name).await.map_err(AppError::from)?;
            if !is_helm_owned(secret.metadata.labels.as_ref()) {
                return Err(AppError::new(
                    "storage object is not a Helm release",
                    AppErrorKind::Validation,
                ));
            }
            let record = secret_record(cluster_context, &mut secret)?;
            let metadata = serde_json::to_value(&secret.metadata).map_err(|e| {
                AppError::new(e.to_string(), AppErrorKind::Serialization).with_source(e)
            })?;
            redact_secret_release(&mut secret);
            let yaml = serialize_resource_document(&secret, yaml_view_mode, yaml_encoding)?;
            Ok((record, metadata, yaml))
        }
        HELM_STORAGE_CONFIGMAP => {
            let api: Api<ConfigMap> = Api::namespaced(client, namespace);
            let mut configmap = api.get(storage_name).await.map_err(AppError::from)?;
            if !is_helm_owned(configmap.metadata.labels.as_ref()) {
                return Err(AppError::new(
                    "storage object is not a Helm release",
                    AppErrorKind::Validation,
                ));
            }
            let record = configmap_record(cluster_context, &mut configmap)?;
            let metadata = serde_json::to_value(&configmap.metadata).map_err(|e| {
                AppError::new(e.to_string(), AppErrorKind::Serialization).with_source(e)
            })?;
            redact_configmap_release(&mut configmap);
            let yaml = serialize_resource_document(&configmap, yaml_view_mode, yaml_encoding)?;
            Ok((record, metadata, yaml))
        }
        _ => Err(AppError::new(
            "unsupported Helm storage kind",
            AppErrorKind::Validation,
        )),
    }
}

pub(super) async fn client_for_context(
    cluster_context: &str,
    kubeconfig_env_var: Option<String>,
) -> Result<(Client, String), AppError> {
    let source = KubeconfigSource::new(kubeconfig_env_var)?;
    source.client_and_default_namespace(cluster_context).await
}

async fn helm_fallback_namespaces(client: Client, default_namespace: &str) -> Vec<String> {
    let api: Api<Namespace> = Api::all(client);
    match api.list(&list_params()).await {
        Ok(items) => {
            let namespaces: Vec<String> = items
                .items
                .into_iter()
                .filter_map(|namespace| namespace.metadata.name)
                .collect();
            if namespaces.is_empty() && !default_namespace.is_empty() {
                vec![default_namespace.to_string()]
            } else {
                namespaces
            }
        }
        Err(_) if !default_namespace.is_empty() => vec![default_namespace.to_string()],
        Err(_) => Vec::new(),
    }
}

async fn list_secret_releases(
    client: Client,
    cluster_context: &str,
    fallback_namespaces: &[String],
) -> Result<Vec<HelmStorageRecord>, AppError> {
    let api: Api<Secret> = Api::all(client.clone());
    let params = helm_list_params();
    let items = match api.list(&params).await {
        Ok(items) => items,
        Err(all_error) => {
            return list_secret_releases_by_namespace(client, cluster_context, fallback_namespaces)
                .await
                .map_err(|namespace_error| {
                    storage_errors(vec![AppError::from(all_error), namespace_error])
                });
        }
    };
    items
        .items
        .into_iter()
        .map(|mut secret| secret_record(cluster_context, &mut secret))
        .collect()
}

async fn list_configmap_releases(
    client: Client,
    cluster_context: &str,
    fallback_namespaces: &[String],
) -> Result<Vec<HelmStorageRecord>, AppError> {
    let api: Api<ConfigMap> = Api::all(client.clone());
    let params = helm_list_params();
    let items = match api.list(&params).await {
        Ok(items) => items,
        Err(all_error) => {
            return list_configmap_releases_by_namespace(
                client,
                cluster_context,
                fallback_namespaces,
            )
            .await
            .map_err(|namespace_error| {
                storage_errors(vec![AppError::from(all_error), namespace_error])
            });
        }
    };
    items
        .items
        .into_iter()
        .map(|mut configmap| configmap_record(cluster_context, &mut configmap))
        .collect()
}

async fn list_secret_releases_by_namespace(
    client: Client,
    cluster_context: &str,
    namespaces: &[String],
) -> Result<Vec<HelmStorageRecord>, AppError> {
    let params = helm_list_params();
    let mut records = Vec::new();
    let mut succeeded = false;
    let mut errors = Vec::new();

    for namespace in namespaces {
        let api: Api<Secret> = Api::namespaced(client.clone(), namespace);
        match api.list(&params).await {
            Ok(items) => {
                succeeded = true;
                for mut secret in items.items {
                    records.push(secret_record(cluster_context, &mut secret)?);
                }
            }
            Err(err) => {
                let mut error = AppError::from(err);
                error.message = format!("{namespace}: {}", error.message);
                errors.push(error);
            }
        }
    }

    if !succeeded && !errors.is_empty() {
        Err(storage_errors(errors))
    } else {
        Ok(records)
    }
}

async fn list_configmap_releases_by_namespace(
    client: Client,
    cluster_context: &str,
    namespaces: &[String],
) -> Result<Vec<HelmStorageRecord>, AppError> {
    let params = helm_list_params();
    let mut records = Vec::new();
    let mut succeeded = false;
    let mut errors = Vec::new();

    for namespace in namespaces {
        let api: Api<ConfigMap> = Api::namespaced(client.clone(), namespace);
        match api.list(&params).await {
            Ok(items) => {
                succeeded = true;
                for mut configmap in items.items {
                    records.push(configmap_record(cluster_context, &mut configmap)?);
                }
            }
            Err(err) => {
                let mut error = AppError::from(err);
                error.message = format!("{namespace}: {}", error.message);
                errors.push(error);
            }
        }
    }

    if !succeeded && !errors.is_empty() {
        Err(storage_errors(errors))
    } else {
        Ok(records)
    }
}

#[derive(Debug)]
struct StorageErrors(Vec<AppError>);

impl std::fmt::Display for StorageErrors {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (index, error) in self.0.iter().enumerate() {
            if index > 0 {
                formatter.write_str("; ")?;
            }
            std::fmt::Display::fmt(error, formatter)?;
        }
        Ok(())
    }
}

impl std::error::Error for StorageErrors {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.0.first().map(|error| error as &dyn std::error::Error)
    }
}

fn storage_errors(errors: Vec<AppError>) -> AppError {
    let kind = errors
        .first()
        .map_or(AppErrorKind::Cluster, |error| error.kind);
    let kind = if errors.iter().all(|error| error.kind == kind) {
        kind
    } else {
        AppErrorKind::Cluster
    };
    let source = StorageErrors(errors);
    AppError::new(source.to_string(), kind).with_source(source)
}

fn helm_list_params() -> ListParams {
    list_params().labels(HELM_OWNER_SELECTOR)
}

fn secret_record(
    cluster_context: &str,
    secret: &mut Secret,
) -> Result<HelmStorageRecord, AppError> {
    let release_data = secret
        .data
        .as_ref()
        .and_then(|data| data.get("release"))
        .map(|data| data.0.as_slice());
    let decoded = release_data.map(decode_helm_release).transpose();
    let (decoded, decode_error) = match decoded {
        Ok(decoded) => (decoded, None),
        Err(error) => (None, Some(error)),
    };
    let release = decoded.as_ref().and_then(safe_release_metadata);
    let values_summary =
        values_summary(decoded.as_ref().and_then(|release| release.config.as_ref()));
    let manifest_resources = manifest_resources(
        decoded
            .as_ref()
            .and_then(|release| release.manifest.as_deref()),
    );
    let manifest_summary = manifest_summary_from_resources(&manifest_resources);
    let storage_name = secret.metadata.name.clone().unwrap_or_default();
    let summary = release_summary(
        cluster_context,
        HELM_STORAGE_SECRET,
        &storage_name,
        &secret.metadata,
        decoded.as_ref(),
    )?;
    Ok(HelmStorageRecord {
        decode_error,
        summary,
        release,
        values_summary,
        manifest_summary,
        manifest_resources,
    })
}

fn configmap_record(
    cluster_context: &str,
    configmap: &mut ConfigMap,
) -> Result<HelmStorageRecord, AppError> {
    let release_data = configmap
        .data
        .as_ref()
        .and_then(|data| data.get("release"))
        .map(std::string::String::as_bytes);
    let decoded = release_data.map(decode_helm_release).transpose();
    let (decoded, decode_error) = match decoded {
        Ok(decoded) => (decoded, None),
        Err(error) => (None, Some(error)),
    };
    let release = decoded.as_ref().and_then(safe_release_metadata);
    let values_summary =
        values_summary(decoded.as_ref().and_then(|release| release.config.as_ref()));
    let manifest_resources = manifest_resources(
        decoded
            .as_ref()
            .and_then(|release| release.manifest.as_deref()),
    );
    let manifest_summary = manifest_summary_from_resources(&manifest_resources);
    let storage_name = configmap.metadata.name.clone().unwrap_or_default();
    let summary = release_summary(
        cluster_context,
        HELM_STORAGE_CONFIGMAP,
        &storage_name,
        &configmap.metadata,
        decoded.as_ref(),
    )?;
    Ok(HelmStorageRecord {
        decode_error,
        summary,
        release,
        values_summary,
        manifest_summary,
        manifest_resources,
    })
}

fn release_summary(
    cluster_context: &str,
    storage_kind: &str,
    storage_name: &str,
    metadata: &ObjectMeta,
    decoded: Option<&DecodedHelmRelease>,
) -> Result<HelmReleaseSummary, AppError> {
    let labels = metadata.labels.as_ref();
    let namespace = decoded
        .and_then(|release| release.namespace.clone())
        .or_else(|| metadata.namespace.clone())
        .ok_or_else(|| {
            AppError::new(
                "Helm release storage object has no namespace",
                AppErrorKind::Cluster,
            )
        })?;
    let name = decoded
        .and_then(|release| release.name.clone())
        .or_else(|| label_value(labels, "name"))
        .unwrap_or_else(|| release_name_from_storage_name(storage_name));
    let revision = decoded
        .and_then(|release| release.version)
        .or_else(|| label_value(labels, "version").and_then(|value| value.parse().ok()));
    let status = decoded
        .and_then(|release| release.info.as_ref())
        .and_then(|info| info.status.clone())
        .or_else(|| label_value(labels, "status"));
    let updated_at = decoded
        .and_then(|release| release.info.as_ref())
        .and_then(|info| info.last_deployed.clone())
        .filter(|value| !value.is_empty());
    let updated = updated_at.as_deref().and_then(parse_rfc3339);
    let created = metadata
        .creation_timestamp
        .as_ref()
        .and_then(|time| crate::commands::helpers::k8s_timestamp_to_datetime(&time.0));
    let chart_metadata = decoded
        .and_then(|release| release.chart.as_ref())
        .and_then(|chart| chart.metadata.as_ref());
    let chart = chart_metadata
        .and_then(|metadata| chart_label(metadata.name.as_deref(), metadata.version.as_deref()));
    let app_version = chart_metadata.and_then(|metadata| metadata.app_version.clone());

    Ok(HelmReleaseSummary {
        cluster: cluster_context.to_string(),
        name,
        namespace,
        age: resource_age(updated.or(created)),
        updated_at,
        created_at: k8s_creation_timestamp_to_rfc3339(&metadata.creation_timestamp),
        chart,
        app_version,
        revision,
        status,
        storage_kind: storage_kind.to_string(),
        storage_name: storage_name.to_string(),
    })
}

fn latest_releases(records: Vec<HelmStorageRecord>) -> Vec<HelmReleaseSummary> {
    let mut latest: BTreeMap<(String, String), HelmReleaseSummary> = BTreeMap::new();
    for record in records {
        let key = (
            record.summary.namespace.clone(),
            record.summary.name.clone(),
        );
        let replace = latest.get(&key).is_none_or(|existing| {
            record.summary.revision.unwrap_or(0) > existing.revision.unwrap_or(0)
        });
        if replace {
            latest.insert(key, record.summary);
        }
    }
    latest.into_values().collect()
}

fn decode_helm_release(data: &[u8]) -> Result<DecodedHelmRelease, AppError> {
    if data.len() > MAX_STORED_RELEASE_BYTES {
        return Err(AppError::new(
            "Helm release storage exceeds the Kubernetes 1 MiB limit",
            AppErrorKind::Validation,
        ));
    }
    let encoded = std::str::from_utf8(data)
        .map_err(|error| {
            AppError::new(
                "Helm release encoding is not UTF-8",
                AppErrorKind::Serialization,
            )
            .with_source(error)
        })?
        .trim();
    let decoded = STANDARD.decode(encoded).map_err(|error| {
        AppError::new(
            "Helm release encoding is not base64",
            AppErrorKind::Serialization,
        )
        .with_source(error)
    })?;
    let json = if decoded.starts_with(&[0x1f, 0x8b, 0x08]) {
        let mut json = Vec::new();
        GzDecoder::new(decoded.as_slice())
            .take(MAX_DECODED_RELEASE_BYTES + 1)
            .read_to_end(&mut json)
            .map_err(|error| {
                AppError::new(
                    "Could not decompress Helm release",
                    AppErrorKind::Serialization,
                )
                .with_source(error)
            })?;
        if json.len() as u64 > MAX_DECODED_RELEASE_BYTES {
            return Err(AppError::new(
                "Decoded Helm release exceeds the 32 MiB limit",
                AppErrorKind::Validation,
            ));
        }
        json
    } else {
        // Helm also accepts its older, uncompressed JSON storage format.
        decoded
    };
    serde_json::from_slice(&json).map_err(|error| {
        AppError::new(
            "Helm release contains invalid JSON",
            AppErrorKind::Serialization,
        )
        .with_source(error)
    })
}

fn safe_release_metadata(release: &DecodedHelmRelease) -> Option<serde_json::Value> {
    Some(serde_json::json!({
        "name": release.name,
        "namespace": release.namespace,
        "version": release.version,
        "info": release.info,
        "chart": release.chart,
    }))
}

fn label_value(labels: Option<&BTreeMap<String, String>>, key: &str) -> Option<String> {
    labels.and_then(|labels| labels.get(key).cloned())
}

fn is_helm_owned(labels: Option<&BTreeMap<String, String>>) -> bool {
    matches!(labels.and_then(|labels| labels.get("owner")), Some(owner) if owner == "helm")
}

fn release_name_from_storage_name(storage_name: &str) -> String {
    storage_name
        .strip_prefix("sh.helm.release.v1.")
        .and_then(|rest| rest.rsplit_once(".v"))
        .map_or_else(|| storage_name.to_string(), |(name, _)| name.to_string())
}

fn chart_label(name: Option<&str>, version: Option<&str>) -> Option<String> {
    match (name, version) {
        (Some(name), Some(version)) if !name.is_empty() && !version.is_empty() => {
            Some(format!("{name}-{version}"))
        }
        (Some(name), _) if !name.is_empty() => Some(name.to_string()),
        _ => None,
    }
}

#[cfg(test)]
#[path = "storage_tests.rs"]
mod storage_tests;
