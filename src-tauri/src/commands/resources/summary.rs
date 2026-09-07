use super::{
    summary_cluster::cluster_resource_summaries, summary_core::core_resource_summaries,
    summary_workloads::workload_resource_summaries,
};
use crate::commands::{
    diagnostic_field,
    kubeconfig::{kubeconfig_source_key, KubeconfigSource},
    record_backend_error, record_backend_success, ClusterLiveStore,
};
use crate::models::AppErrorKind;
use crate::models::{AppError, ResourceSummary};
use std::time::Instant;
use tauri::State;

pub async fn resources_summary_from(
    cluster_context: String,
    kind: String,
    namespace: Option<String>,
    kubeconfig_env_var: Option<String>,
) -> Result<Vec<ResourceSummary>, AppError> {
    crate::commands::helpers::validate_namespace(namespace.as_deref())?;
    let source = KubeconfigSource::new(kubeconfig_env_var)?;
    let client = source.client_for_context(&cluster_context).await?;

    if let Some(rows) = core_resource_summaries(
        client.clone(),
        &cluster_context,
        &kind,
        namespace.as_deref(),
    )
    .await?
    {
        return Ok(rows);
    }
    if let Some(rows) = workload_resource_summaries(
        client.clone(),
        &cluster_context,
        &kind,
        namespace.as_deref(),
    )
    .await?
    {
        return Ok(rows);
    }
    if let Some(rows) = cluster_resource_summaries(client, &cluster_context, &kind).await? {
        return Ok(rows);
    }

    Err(AppError::new(
        format!("unsupported resource kind: {kind}"),
        AppErrorKind::Cluster,
    ))
}

#[tauri::command]
pub async fn list_resources(
    cluster_context: String,
    kind: String,
    namespace: Option<String>,
    kubeconfig_env_var: Option<String>,
    live_store: State<'_, ClusterLiveStore>,
) -> Result<Vec<ResourceSummary>, AppError> {
    let started = Instant::now();
    let namespace_label = namespace.as_deref().unwrap_or("<all>");
    eprintln!(
        "[kubecove:backend] list_resources start context={cluster_context} kind={kind} namespace={namespace_label}"
    );
    let source_key = kubeconfig_source_key(kubeconfig_env_var.as_deref())?;
    let result = live_store
        .typed_resources(
            source_key,
            cluster_context.clone(),
            kind.clone(),
            namespace.clone(),
            {
                let cluster_context = cluster_context.clone();
                let kind = kind.clone();
                let namespace = namespace.clone();
                let kubeconfig_env_var = kubeconfig_env_var.clone();
                move || resources_summary_from(cluster_context, kind, namespace, kubeconfig_env_var)
            },
        )
        .await;
    match &result {
        Ok(rows) => {
            eprintln!("[kubecove:backend] list_resources done context={} kind={} namespace={} rows={} ms={}", cluster_context, kind, namespace_label, rows.len(), started.elapsed().as_millis());
            record_backend_success(
                "list_resources",
                started,
                vec![
                    diagnostic_field("kind", &kind),
                    diagnostic_field("rows", rows.len()),
                ],
            );
        }
        Err(err) => {
            eprintln!("[kubecove:backend] list_resources error context={} kind={} namespace={} error_kind={} message={} ms={}", cluster_context, kind, namespace_label, err.kind, err.message, started.elapsed().as_millis());
            record_backend_error("list_resources", started, err.kind.as_str());
        }
    }
    result
}
