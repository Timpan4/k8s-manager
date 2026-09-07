use crate::models::{
    AppError, PodExecSessionMessage, PodExecSessionRequest, PodExecSessionSummary,
    PodExecTerminalSize,
};
use tauri::{ipc::Channel, State};

mod registry;
mod runner;
mod validation;

use registry::ExecCommand;
pub use registry::PodExecRegistry;
use runner::start_pod_exec_session_in_registry;
#[cfg(test)]
use validation::validate_request;
use validation::validate_terminal_size;

#[tauri::command]
pub async fn start_pod_exec_session(
    request: PodExecSessionRequest,
    channel: Channel<PodExecSessionMessage>,
    registry: State<'_, PodExecRegistry>,
) -> Result<PodExecSessionSummary, AppError> {
    start_pod_exec_session_in_registry(request, channel, registry.inner()).await
}

#[tauri::command]
pub async fn write_pod_exec_stdin(
    session_id: String,
    data: String,
    registry: State<'_, PodExecRegistry>,
) -> Result<bool, AppError> {
    registry.write_stdin(&session_id, data).await?;
    Ok(true)
}

#[tauri::command]
pub async fn resize_pod_exec_terminal(
    session_id: String,
    size: PodExecTerminalSize,
    registry: State<'_, PodExecRegistry>,
) -> Result<bool, AppError> {
    validate_terminal_size(&size)?;
    registry.send_command(&session_id, ExecCommand::Resize(size))?;
    Ok(true)
}

#[tauri::command]
pub async fn stop_pod_exec_session(
    session_id: String,
    registry: State<'_, PodExecRegistry>,
) -> Result<bool, AppError> {
    Ok(registry.stop(&session_id))
}

#[tauri::command]
pub async fn list_pod_exec_sessions(
    registry: State<'_, PodExecRegistry>,
) -> Result<Vec<PodExecSessionSummary>, AppError> {
    Ok(registry.list())
}

#[cfg(test)]
mod tests;
