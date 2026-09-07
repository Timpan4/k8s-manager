use crate::{
    commands::{
        diagnostic_field,
        kubeconfig::{cluster_contexts_from_source, KubeconfigSource},
        record_backend_error, record_backend_success,
    },
    models::{AppError, ClusterContext},
};
use std::time::Instant;

pub fn get_cluster_contexts(
    kubeconfig_env_var: Option<String>,
) -> Result<Vec<ClusterContext>, AppError> {
    let source = KubeconfigSource::new(kubeconfig_env_var)?;
    cluster_contexts_from_source(&source)
}

#[tauri::command]
pub fn list_kube_contexts(
    kubeconfig_env_var: Option<String>,
) -> Result<Vec<ClusterContext>, AppError> {
    let started = Instant::now();
    let result = get_cluster_contexts(kubeconfig_env_var);
    match &result {
        Ok(rows) => record_backend_success(
            "list_kube_contexts",
            started,
            vec![diagnostic_field("rows", rows.len())],
        ),
        Err(err) => record_backend_error("list_kube_contexts", started, err.kind.as_str()),
    }
    result
}
