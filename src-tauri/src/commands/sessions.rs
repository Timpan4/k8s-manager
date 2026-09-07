use crate::models::AppErrorKind;
use crate::models::{AppError, PortForwardRequest, PortForwardSessionSummary};
use tauri::State;

mod registry;
pub(crate) mod runner;
pub(crate) mod service;
pub(crate) mod target;

pub use registry::PortForwardRegistry;
#[cfg(test)]
use registry::LOCAL_ADDRESS;
use runner::start_pod_port_forward_in_registry;
#[cfg(test)]
use runner::{run_port_forward_session, should_retry_accept};
#[cfg(test)]
use target::{validate_request, PortForwardTarget, PortForwardTargetKind};

#[tauri::command]
pub async fn start_pod_port_forward(
    request: PortForwardRequest,
    registry: State<'_, PortForwardRegistry>,
) -> Result<PortForwardSessionSummary, AppError> {
    start_pod_port_forward_in_registry(request, registry.inner()).await
}

#[tauri::command]
pub async fn stop_port_forward(
    session_id: String,
    registry: State<'_, PortForwardRegistry>,
) -> Result<bool, AppError> {
    if session_id.trim().is_empty() {
        return Err(AppError::new(
            "port-forward session id is required",
            AppErrorKind::Validation,
        ));
    }
    Ok(registry.stop(session_id.trim()))
}

#[tauri::command]
pub async fn list_port_forwards(
    registry: State<'_, PortForwardRegistry>,
) -> Result<Vec<PortForwardSessionSummary>, AppError> {
    Ok(registry.list())
}

#[cfg(test)]
mod tests;
