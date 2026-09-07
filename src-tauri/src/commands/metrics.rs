use crate::commands::diagnostics::record_backend_result;
use crate::commands::{
    diagnostic_field, helpers::list_params, kubeconfig::KubeconfigSource,
    BackendCancellationRegistry,
};
use crate::models::AppErrorKind;
use crate::models::{
    AppError, ResourceMetricSummary, ResourceMetricsAvailability,
    ResourceMetricsAvailabilityStatus, ResourceMetricsSummary,
};
use futures_util::{stream, StreamExt};
use k8s_openapi::api::apps::v1::ReplicaSet;
use k8s_openapi::api::core::v1::Pod;
use kube::{
    api::{Api, ApiResource, DynamicObject},
    discovery::{verbs, Discovery},
    Client, Error,
};
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::Instant;
use tauri::State;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MetricsListStatus {
    Forbidden,
    Unavailable,
}

#[derive(Debug, Default)]
struct MetricsObjectList {
    objects: Vec<DynamicObject>,
    failures: Vec<MetricsListStatus>,
}

const MAX_NAMESPACE_CONCURRENCY: usize = 16;

fn metrics_api_resource(kind: &str, plural: &str) -> ApiResource {
    ApiResource {
        group: "metrics.k8s.io".to_string(),
        version: "v1beta1".to_string(),
        api_version: "metrics.k8s.io/v1beta1".to_string(),
        kind: kind.to_string(),
        plural: plural.to_string(),
    }
}

async fn client_for_context(
    cluster_context: &str,
    kubeconfig_env_var: Option<String>,
) -> Result<Client, AppError> {
    let source = KubeconfigSource::new(kubeconfig_env_var)?;
    source.client_for_context(cluster_context).await
}

async fn has_metrics_api(client: Client) -> Result<bool, AppError> {
    let discovery = Discovery::new(client)
        .run_aggregated()
        .await
        .map_err(AppError::from)?;
    let available = discovery.groups().any(|group| {
        group.name() == "metrics.k8s.io"
            && group
                .recommended_resources()
                .iter()
                .any(|(resource, capabilities)| {
                    resource.api_version == "metrics.k8s.io/v1beta1"
                        && matches!(resource.kind.as_str(), "PodMetrics" | "NodeMetrics")
                        && capabilities.supports_operation(verbs::LIST)
                })
    });
    Ok(available)
}

fn classify_metrics_error(error: &Error) -> MetricsListStatus {
    if crate::models::kube_error_kind(error) == AppErrorKind::Forbidden {
        MetricsListStatus::Forbidden
    } else {
        MetricsListStatus::Unavailable
    }
}

fn parse_cpu_millicores(value: &str) -> Option<f64> {
    let value = value.trim();
    let (number, multiplier) = if let Some(raw) = value.strip_suffix('n') {
        (raw, 1.0 / 1_000_000.0)
    } else if let Some(raw) = value.strip_suffix('u') {
        (raw, 1.0 / 1_000.0)
    } else if let Some(raw) = value.strip_suffix('m') {
        (raw, 1.0)
    } else {
        (value, 1_000.0)
    };
    let millicores = number.parse::<f64>().ok()? * multiplier;
    (millicores.is_finite() && millicores >= 0.0).then_some(millicores)
}

fn parse_memory_bytes(value: &str) -> Option<i64> {
    const UNITS: &[(&str, f64)] = &[
        ("Ki", 1024.0),
        ("Mi", 1_048_576.0),
        ("Gi", 1_073_741_824.0),
        ("Ti", 1_099_511_627_776.0),
        ("K", 1000.0),
        ("M", 1_000_000.0),
        ("G", 1_000_000_000.0),
        ("T", 1_000_000_000_000.0),
    ];
    let trimmed = value.trim();
    for (suffix, multiplier) in UNITS {
        if let Some(raw) = trimmed.strip_suffix(suffix) {
            let quantity = raw.parse::<f64>().ok()?;
            if !quantity.is_finite() || quantity < 0.0 {
                return None;
            }
            let bytes = (quantity * multiplier).round();
            // i64::MAX rounds up to 2^63 as f64, so the upper bound is exclusive.
            return if bytes.is_finite() && bytes >= 0.0 && bytes < i64::MAX as f64 {
                Some(bytes as i64)
            } else {
                None
            };
        }
    }
    trimmed.parse::<i64>().ok().filter(|bytes| *bytes >= 0)
}

fn usage_value(data: &Value, key: &str) -> Option<String> {
    data.get("usage")
        .and_then(|usage| usage.get(key))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn container_usage_totals(data: &Value) -> (Option<f64>, Option<i64>) {
    let containers = data
        .get("containers")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let mut cpu = Some(0.0_f64);
    let mut has_cpu = false;
    let mut memory = Some(0_i64);
    let mut has_memory = false;

    for container in containers {
        if let Some(value) =
            usage_value(container, "cpu").and_then(|value| parse_cpu_millicores(&value))
        {
            cpu = cpu
                .map(|total| total + value)
                .filter(|total| total.is_finite());
            has_cpu = true;
        }
        if let Some(value) =
            usage_value(container, "memory").and_then(|value| parse_memory_bytes(&value))
        {
            memory = memory.and_then(|total| total.checked_add(value));
            has_memory = true;
        }
    }

    (
        has_cpu.then_some(cpu).flatten(),
        has_memory.then_some(memory).flatten(),
    )
}

fn metric_from_object(
    cluster_context: &str,
    kind: &str,
    object: &DynamicObject,
) -> ResourceMetricSummary {
    let (cpu_millicores, memory_bytes) = if kind == "Pod" {
        container_usage_totals(&object.data)
    } else {
        (
            usage_value(&object.data, "cpu").and_then(|value| parse_cpu_millicores(&value)),
            usage_value(&object.data, "memory").and_then(|value| parse_memory_bytes(&value)),
        )
    };

    ResourceMetricSummary {
        kind: kind.to_string(),
        cluster: cluster_context.to_string(),
        name: object.metadata.name.clone().unwrap_or_default(),
        namespace: object.metadata.namespace.clone(),
        cpu_millicores,
        memory_bytes,
        sampled_at: object
            .data
            .get("timestamp")
            .and_then(Value::as_str)
            .map(str::to_string),
        source_pods: Vec::new(),
    }
}

async fn list_pod_metric_objects(client: Client, namespaces: &[String]) -> MetricsObjectList {
    let resource = metrics_api_resource("PodMetrics", "pods");
    let mut result = MetricsObjectList::default();
    if namespaces.is_empty() {
        let api: Api<DynamicObject> = Api::all_with(client, &resource);
        match api.list(&list_params()).await {
            Ok(list) => result.objects = list.items,
            Err(error) => result.failures.push(classify_metrics_error(&error)),
        }
        return result;
    }
    let outcomes = stream::iter(namespaces.to_vec())
        .map(|namespace| {
            let api: Api<DynamicObject> =
                Api::namespaced_with(client.clone(), &namespace, &resource);
            async move { api.list(&list_params()).await }
        })
        .buffered(MAX_NAMESPACE_CONCURRENCY)
        .collect::<Vec<_>>()
        .await;
    for outcome in outcomes {
        match outcome {
            Ok(list) => result.objects.extend(list.items),
            Err(error) => result.failures.push(classify_metrics_error(&error)),
        }
    }
    result
}

async fn list_node_metric_objects(client: Client) -> Result<Vec<DynamicObject>, MetricsListStatus> {
    let resource = metrics_api_resource("NodeMetrics", "nodes");
    let api: Api<DynamicObject> = Api::all_with(client, &resource);
    api.list(&list_params())
        .await
        .map(|list| list.items)
        .map_err(|error| classify_metrics_error(&error))
}

async fn list_pods(client: Client, namespaces: &[String]) -> Result<Vec<Pod>, AppError> {
    let mut out = Vec::new();
    if namespaces.is_empty() {
        let api: Api<Pod> = Api::all(client);
        return api
            .list(&list_params())
            .await
            .map(|list| list.items)
            .map_err(AppError::from);
    }
    let outcomes = stream::iter(namespaces.to_vec())
        .map(|namespace| {
            let api: Api<Pod> = Api::namespaced(client.clone(), &namespace);
            async move { api.list(&list_params()).await }
        })
        .buffered(MAX_NAMESPACE_CONCURRENCY)
        .collect::<Vec<_>>()
        .await;
    for outcome in outcomes {
        out.extend(outcome.map_err(AppError::from)?.items);
    }
    Ok(out)
}

async fn list_replicasets(
    client: Client,
    namespaces: &[String],
) -> Result<Vec<ReplicaSet>, AppError> {
    let mut out = Vec::new();
    if namespaces.is_empty() {
        let api: Api<ReplicaSet> = Api::all(client);
        return api
            .list(&list_params())
            .await
            .map(|list| list.items)
            .map_err(AppError::from);
    }
    let outcomes = stream::iter(namespaces.to_vec())
        .map(|namespace| {
            let api: Api<ReplicaSet> = Api::namespaced(client.clone(), &namespace);
            async move { api.list(&list_params()).await }
        })
        .buffered(MAX_NAMESPACE_CONCURRENCY)
        .collect::<Vec<_>>()
        .await;
    for outcome in outcomes {
        out.extend(outcome.map_err(AppError::from)?.items);
    }
    Ok(out)
}

type WorkloadKey = (String, String, String);

fn controller_owner(
    owners: &[k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference],
) -> Option<&k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference> {
    owners
        .iter()
        .find(|owner| owner.controller == Some(true))
        .or_else(|| owners.first())
}

struct WorkloadMetric {
    summary: ResourceMetricSummary,
    cpu_overflowed: bool,
    memory_overflowed: bool,
}

fn add_workload_metric(
    by_workload: &mut BTreeMap<WorkloadKey, WorkloadMetric>,
    cluster_context: &str,
    key: WorkloadKey,
    pod_name: &str,
    metric: &ResourceMetricSummary,
) {
    let (kind, namespace, name) = key.clone();
    let entry = by_workload.entry(key).or_insert_with(|| WorkloadMetric {
        cpu_overflowed: false,
        memory_overflowed: false,
        summary: ResourceMetricSummary {
            kind,
            cluster: cluster_context.to_string(),
            name,
            namespace: Some(namespace),
            cpu_millicores: None,
            memory_bytes: None,
            sampled_at: metric.sampled_at.clone(),
            source_pods: Vec::new(),
        },
    });
    if !entry.cpu_overflowed {
        if let Some(cpu) = metric.cpu_millicores {
            let total = entry.summary.cpu_millicores.unwrap_or(0.0) + cpu;
            entry.summary.cpu_millicores = total.is_finite().then_some(total);
            entry.cpu_overflowed = entry.summary.cpu_millicores.is_none();
        }
    }
    if !entry.memory_overflowed {
        if let Some(memory) = metric.memory_bytes {
            entry.summary.memory_bytes =
                entry.summary.memory_bytes.unwrap_or(0).checked_add(memory);
            entry.memory_overflowed = entry.summary.memory_bytes.is_none();
        }
    }
    if metric.sampled_at > entry.summary.sampled_at {
        entry.summary.sampled_at.clone_from(&metric.sampled_at);
    }
    entry.summary.source_pods.push(pod_name.to_string());
}

fn aggregate_workload_metrics(
    cluster_context: &str,
    pods: &[Pod],
    replicasets: &[ReplicaSet],
    pod_metrics: &[ResourceMetricSummary],
) -> Vec<ResourceMetricSummary> {
    let metric_by_pod: BTreeMap<_, _> = pod_metrics
        .iter()
        .filter_map(|metric| Some(((metric.namespace.clone()?, metric.name.clone()), metric)))
        .collect();
    let replicaset_owner_by_key: BTreeMap<WorkloadKey, WorkloadKey> = replicasets
        .iter()
        .filter_map(|replicaset| {
            let namespace = replicaset.metadata.namespace.clone()?;
            let name = replicaset.metadata.name.clone()?;
            let owner = controller_owner(replicaset.metadata.owner_references.as_deref()?)?;
            Some((
                ("ReplicaSet".to_string(), namespace.clone(), name),
                (owner.kind.clone(), namespace, owner.name.clone()),
            ))
        })
        .collect();
    let mut by_workload: BTreeMap<WorkloadKey, WorkloadMetric> = BTreeMap::new();

    for pod in pods {
        let namespace = pod.metadata.namespace.clone().unwrap_or_default();
        let name = pod.metadata.name.clone().unwrap_or_default();
        let Some(metric) = metric_by_pod.get(&(namespace.clone(), name.clone())) else {
            continue;
        };
        let Some(owner) = pod.metadata.owner_references.as_ref().and_then(|owners| {
            owners
                .iter()
                .find(|owner| owner.controller == Some(true))
                .or_else(|| owners.first())
        }) else {
            continue;
        };
        let key = (owner.kind.clone(), namespace.clone(), owner.name.clone());
        add_workload_metric(
            &mut by_workload,
            cluster_context,
            key.clone(),
            &name,
            metric,
        );

        if let Some(parent_key) = replicaset_owner_by_key.get(&key) {
            add_workload_metric(
                &mut by_workload,
                cluster_context,
                parent_key.clone(),
                &name,
                metric,
            );
        }
    }

    by_workload
        .into_values()
        .map(|metric| metric.summary)
        .collect()
}

fn availability(
    status: ResourceMetricsAvailabilityStatus,
    message: impl Into<String>,
) -> ResourceMetricsAvailability {
    ResourceMetricsAvailability {
        status,
        message: Some(message.into()),
    }
}

pub async fn resource_metrics_from(
    cluster_context: String,
    namespaces: Vec<String>,
    kubeconfig_env_var: Option<String>,
) -> Result<ResourceMetricsSummary, AppError> {
    for namespace in &namespaces {
        crate::commands::helpers::validate_namespace(Some(namespace))?;
    }
    let client = client_for_context(&cluster_context, kubeconfig_env_var).await?;
    match has_metrics_api(client.clone()).await {
        Ok(true) => {}
        Ok(false) => {
            return Ok(ResourceMetricsSummary {
                cluster: cluster_context,
                availability: availability(
                    ResourceMetricsAvailabilityStatus::Unavailable,
                    "metrics API unavailable",
                ),
                pods: Vec::new(),
                nodes: Vec::new(),
                workloads: Vec::new(),
                warnings: Vec::new(),
            });
        }
        Err(err) => {
            let forbidden = err.kind == AppErrorKind::Forbidden;
            return Ok(ResourceMetricsSummary {
                cluster: cluster_context,
                availability: availability(
                    if forbidden {
                        ResourceMetricsAvailabilityStatus::Forbidden
                    } else {
                        ResourceMetricsAvailabilityStatus::Unavailable
                    },
                    if forbidden {
                        "metrics API forbidden"
                    } else {
                        "metrics API unavailable"
                    },
                ),
                pods: Vec::new(),
                nodes: Vec::new(),
                workloads: Vec::new(),
                warnings: vec![format!("Metrics discovery unavailable: {}", err.message)],
            });
        }
    }

    let pod_result = list_pod_metric_objects(client.clone(), &namespaces).await;
    let node_result = list_node_metric_objects(client.clone()).await;
    let mut warnings = Vec::new();
    let mut had_unavailable_error = false;
    let mut had_forbidden_error = pod_result.failures.contains(&MetricsListStatus::Forbidden);

    if had_forbidden_error {
        warnings.push("Pod metrics forbidden".to_string());
    }
    if pod_result
        .failures
        .contains(&MetricsListStatus::Unavailable)
    {
        had_unavailable_error = true;
        warnings.push("Pod metrics unavailable".to_string());
    }
    let pods: Vec<ResourceMetricSummary> = pod_result
        .objects
        .iter()
        .map(|object| metric_from_object(&cluster_context, "Pod", object))
        .collect();
    let nodes = match node_result {
        Ok(objects) => objects
            .iter()
            .map(|object| metric_from_object(&cluster_context, "Node", object))
            .collect(),
        Err(status) => {
            if status == MetricsListStatus::Forbidden {
                had_forbidden_error = true;
                warnings.push("Node metrics forbidden".to_string());
            } else {
                had_unavailable_error = true;
                warnings.push("Node metrics unavailable".to_string());
            }
            Vec::new()
        }
    };
    let workloads = match list_pods(client.clone(), &namespaces).await {
        Ok(pod_objects) => {
            let replicasets = match list_replicasets(client, &namespaces).await {
                Ok(replicasets) => replicasets,
                Err(err) => {
                    warnings.push(format!(
                        "Workload owner rollup unavailable: {}",
                        err.message
                    ));
                    Vec::new()
                }
            };
            aggregate_workload_metrics(&cluster_context, &pod_objects, &replicasets, &pods)
        }
        Err(err) => {
            warnings.push(format!("Workload metrics unavailable: {}", err.message));
            Vec::new()
        }
    };
    let status = if pods.is_empty() && nodes.is_empty() {
        if had_forbidden_error {
            ResourceMetricsAvailabilityStatus::Forbidden
        } else if had_unavailable_error {
            ResourceMetricsAvailabilityStatus::Unavailable
        } else {
            ResourceMetricsAvailabilityStatus::NoSamples
        }
    } else {
        ResourceMetricsAvailabilityStatus::Available
    };
    let message = match status {
        ResourceMetricsAvailabilityStatus::Available => "metrics available",
        ResourceMetricsAvailabilityStatus::Unavailable => "metrics API unavailable",
        ResourceMetricsAvailabilityStatus::Forbidden => "forbidden",
        ResourceMetricsAvailabilityStatus::NoSamples => "no samples yet",
    };

    Ok(ResourceMetricsSummary {
        cluster: cluster_context,
        availability: availability(status, message),
        pods,
        nodes,
        workloads,
        warnings,
    })
}

#[tauri::command]
pub async fn list_resource_metrics(
    cluster_context: String,
    namespaces: Vec<String>,
    kubeconfig_env_var: Option<String>,
    request_id: Option<String>,
    cancel_scope: Option<String>,
    cancellations: State<'_, BackendCancellationRegistry>,
) -> Result<ResourceMetricsSummary, AppError> {
    let started = Instant::now();
    let namespace_count = namespaces.len();
    eprintln!(
        "[kubecove:backend] list_resource_metrics start context={cluster_context} namespaces={namespace_count}",
    );
    let result = cancellations
        .execute(
            cancel_scope,
            request_id,
            resource_metrics_from(cluster_context.clone(), namespaces, kubeconfig_env_var),
        )
        .await;
    match &result {
        Ok(summary) => {
            eprintln!(
                "[kubecove:backend] list_resource_metrics done context={} status={:?} pods={} nodes={} workloads={} ms={}",
                cluster_context,
                summary.availability.status,
                summary.pods.len(),
                summary.nodes.len(),
                summary.workloads.len(),
                started.elapsed().as_millis()
            );
        }
        Err(err) if err.kind == AppErrorKind::Cancelled => {
            eprintln!(
                "[kubecove:backend] list_resource_metrics cancelled context={} ms={}",
                cluster_context,
                started.elapsed().as_millis()
            );
        }
        Err(err) => {
            eprintln!(
                "[kubecove:backend] list_resource_metrics error context={} kind={} message={} ms={}",
                cluster_context,
                err.kind,
                err.message,
                started.elapsed().as_millis()
            );
        }
    }
    record_backend_result("list_resource_metrics", started, &result, |summary| {
        vec![
            diagnostic_field("status", format!("{:?}", summary.availability.status)),
            diagnostic_field("namespaces", namespace_count),
            diagnostic_field("pods", summary.pods.len()),
            diagnostic_field("nodes", summary.nodes.len()),
            diagnostic_field("workloads", summary.workloads.len()),
        ]
    });
    result
}

#[cfg(test)]
#[path = "metrics_tests.rs"]
mod tests;
