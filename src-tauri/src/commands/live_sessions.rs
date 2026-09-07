use crate::commands::{PodExecRegistry, PortForwardRegistry};
use crate::models::AppErrorKind;
use crate::models::{AppError, LiveSessionCleanupRequest, LiveSessionCleanupResult};
use std::collections::HashSet;
use tauri::State;

#[tauri::command]
pub async fn stop_live_sessions_outside_scope(
    request: LiveSessionCleanupRequest,
    port_forwards: State<'_, PortForwardRegistry>,
    pod_execs: State<'_, PodExecRegistry>,
) -> Result<LiveSessionCleanupResult, AppError> {
    let kubeconfig_source_key = request.kubeconfig_source_key.trim();
    if kubeconfig_source_key.is_empty() {
        return Err(AppError::new(
            "kubeconfig source key is required",
            AppErrorKind::Validation,
        ));
    }

    let allowed_cluster_contexts = request
        .allowed_cluster_contexts
        .into_iter()
        .map(|context| context.trim().to_string())
        .filter(|context| !context.is_empty())
        .collect::<HashSet<_>>();

    let stopped_port_forward_ids =
        port_forwards.stop_outside_scope(&allowed_cluster_contexts, kubeconfig_source_key);
    let stopped_pod_exec_ids =
        pod_execs.stop_outside_scope(&allowed_cluster_contexts, kubeconfig_source_key);

    Ok(LiveSessionCleanupResult {
        stopped_port_forwards: stopped_port_forward_ids.len(),
        stopped_pod_exec_sessions: stopped_pod_exec_ids.len(),
        stopped_port_forward_ids,
        stopped_pod_exec_ids,
    })
}
