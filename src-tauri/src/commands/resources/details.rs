use crate::models::AppErrorKind;
mod cluster;
mod core;
mod workloads;

use crate::commands::diagnostics::record_backend_result;
use crate::commands::{
    diagnostic_field, kubeconfig::KubeconfigSource, BackendCancellationRegistry,
};
use crate::models::{AppError, ResourceDetailsFull, YamlEncoding, YamlViewMode};
use std::time::Instant;
use tauri::State;

pub async fn resource_details_from(
    cluster_context: String,
    kind: String,
    name: String,
    namespace: Option<String>,
    kubeconfig_env_var: Option<String>,
) -> Result<ResourceDetailsFull, AppError> {
    let source = KubeconfigSource::new(kubeconfig_env_var)?;
    let client = source.client_for_context(&cluster_context).await?;

    match kind.as_str() {
        "Pod" => core::pod_details(client, cluster_context, name, namespace).await,
        "Service" => core::service_details(client, cluster_context, name, namespace).await,
        "ConfigMap" => core::configmap_details(client, cluster_context, name, namespace).await,
        "Secret" => core::secret_details(client, cluster_context, name, namespace).await,
        "PersistentVolumeClaim" => {
            core::pvc_details(client, cluster_context, name, namespace).await
        }
        "Deployment" => {
            workloads::deployment_details(client, cluster_context, name, namespace).await
        }
        "StatefulSet" => {
            workloads::statefulset_details(client, cluster_context, name, namespace).await
        }
        "DaemonSet" => workloads::daemonset_details(client, cluster_context, name, namespace).await,
        "Ingress" => workloads::ingress_details(client, cluster_context, name, namespace).await,
        "Job" => workloads::job_details(client, cluster_context, name, namespace).await,
        "CronJob" => workloads::cronjob_details(client, cluster_context, name, namespace).await,
        "Node" => cluster::node_details(client, cluster_context, name, namespace).await,
        "StorageClass" => {
            cluster::storageclass_details(client, cluster_context, name, namespace).await
        }
        "PersistentVolume" => cluster::pv_details(client, cluster_context, name, namespace).await,
        "CustomResourceDefinition" => {
            cluster::crd_details(client, cluster_context, name, namespace).await
        }
        _ => Err(AppError::new(
            format!("unsupported resource kind: {kind}"),
            AppErrorKind::Cluster,
        )),
    }
}

#[tauri::command]
pub async fn get_resource_details(
    cluster_context: String,
    kind: String,
    name: String,
    namespace: Option<String>,
    kubeconfig_env_var: Option<String>,
    _yaml_view_mode: Option<YamlViewMode>,
    _yaml_encoding: Option<YamlEncoding>,
    request_id: Option<String>,
    cancel_scope: Option<String>,
    cancellations: State<'_, BackendCancellationRegistry>,
) -> Result<ResourceDetailsFull, AppError> {
    let started = Instant::now();
    let namespace_label = namespace.as_deref().unwrap_or("<cluster>");
    eprintln!(
        "[kubecove:backend] get_resource_details start context={cluster_context} kind={kind} namespace={namespace_label} name={name}"
    );
    let result = cancellations
        .execute(
            cancel_scope,
            request_id,
            resource_details_from(
                cluster_context.clone(),
                kind.clone(),
                name.clone(),
                namespace.clone(),
                kubeconfig_env_var,
            ),
        )
        .await;
    match &result {
        Ok(details) => {
            eprintln!("[kubecove:backend] get_resource_details done context={} kind={} namespace={} name={} yaml_bytes={} status={} ms={}", cluster_context, kind, namespace_label, name, details.yaml.len(), details.status.is_some(), started.elapsed().as_millis());
        }
        Err(err) if err.kind == AppErrorKind::Cancelled => {
            eprintln!("[kubecove:backend] get_resource_details cancelled context={} kind={} namespace={} name={} ms={}", cluster_context, kind, namespace_label, name, started.elapsed().as_millis());
        }
        Err(err) => {
            eprintln!("[kubecove:backend] get_resource_details error context={} kind={} namespace={} name={} error_kind={} message={} ms={}", cluster_context, kind, namespace_label, name, err.kind, err.message, started.elapsed().as_millis());
        }
    }
    record_backend_result("get_resource_details", started, &result, |details| {
        vec![
            diagnostic_field("kind", &kind),
            diagnostic_field("yamlBytes", details.yaml.len()),
            diagnostic_field("hasStatus", details.status.is_some()),
        ]
    });
    result
}
