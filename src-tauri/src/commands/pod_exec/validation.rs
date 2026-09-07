use crate::commands::kubeconfig::KubeconfigSource;
use crate::models::AppErrorKind;
use crate::models::{AppError, PodExecSessionRequest, PodExecTerminalSize};
use kube::Client;

const MIN_TERMINAL_SIZE: u16 = 1;
const MAX_TERMINAL_SIZE: u16 = 500;

#[derive(Debug, Clone)]
pub(super) struct ValidatedPodExecRequest {
    pub(super) cluster_context: String,
    pub(super) kubeconfig_env_var: Option<String>,
    pub(super) kubeconfig_source_key: Option<String>,
    pub(super) kubeconfig_source_label: Option<String>,
    pub(super) namespace: String,
    pub(super) pod_name: String,
    pub(super) container: Option<String>,
    pub(super) command: Vec<String>,
    pub(super) stdin: bool,
    pub(super) tty: bool,
    pub(super) terminal_size: PodExecTerminalSize,
}

fn trimmed(value: &str) -> String {
    value.trim().to_string()
}

pub(super) fn validate_terminal_size(size: &PodExecTerminalSize) -> Result<(), AppError> {
    if !(MIN_TERMINAL_SIZE..=MAX_TERMINAL_SIZE).contains(&size.cols)
        || !(MIN_TERMINAL_SIZE..=MAX_TERMINAL_SIZE).contains(&size.rows)
    {
        return Err(AppError::new(
            "terminal size must be between 1 and 500 columns and rows",
            AppErrorKind::Validation,
        ));
    }
    Ok(())
}

fn exec_target_text(
    cluster_context: &str,
    namespace: &str,
    pod_name: &str,
    container: Option<&str>,
) -> String {
    let container = container.unwrap_or("<default>");
    format!("{cluster_context}/{namespace}/Pod/{pod_name}/container/{container}")
}

fn exec_command_text(command: &[String]) -> String {
    serde_json::to_string(command).expect("pod exec command serializes")
}

pub(super) fn validate_request(
    request: &PodExecSessionRequest,
) -> Result<ValidatedPodExecRequest, AppError> {
    let cluster_context = trimmed(&request.cluster_context);
    let namespace = trimmed(&request.namespace);
    let pod_name = trimmed(&request.pod_name);
    if cluster_context.is_empty() || namespace.is_empty() || pod_name.is_empty() {
        return Err(AppError::new(
            "pod exec target is required",
            AppErrorKind::Validation,
        ));
    }

    crate::commands::helpers::validate_namespace(Some(&namespace))?;
    crate::commands::helpers::validate_path_segment(&pod_name, "Pod name")?;
    let command = &request.command;
    if command
        .first()
        .is_none_or(|executable| executable.trim().is_empty())
    {
        return Err(AppError::new(
            "pod exec command is required",
            AppErrorKind::Validation,
        ));
    }

    validate_terminal_size(&request.terminal_size)?;
    if !request.stdin && request.tty {
        return Err(AppError::new(
            "tty exec sessions require stdin",
            AppErrorKind::Validation,
        ));
    }
    if !request.confirmation.acknowledged {
        return Err(AppError::new(
            "pod exec requires explicit confirmation",
            AppErrorKind::Validation,
        ));
    }
    let container = request
        .container
        .as_ref()
        .map(|container| trimmed(container))
        .filter(|container| !container.is_empty());
    if request.confirmation.target
        != exec_target_text(
            &cluster_context,
            &namespace,
            &pod_name,
            container.as_deref(),
        )
    {
        return Err(AppError::new(
            "pod exec confirmation target does not match the request",
            AppErrorKind::Validation,
        ));
    }
    if request.confirmation.command != exec_command_text(command) {
        return Err(AppError::new(
            "pod exec confirmation command does not match the request",
            AppErrorKind::Validation,
        ));
    }

    let source = KubeconfigSource::new(request.kubeconfig_env_var.clone())?;
    Ok(ValidatedPodExecRequest {
        cluster_context,
        kubeconfig_env_var: request.kubeconfig_env_var.clone(),
        kubeconfig_source_key: Some(source.key()),
        kubeconfig_source_label: source.show_source_labels().then(|| source.label()),
        namespace,
        pod_name,
        container,
        command: command.clone(),
        stdin: request.stdin,
        tty: request.tty,
        terminal_size: request.terminal_size.clone(),
    })
}

pub(super) async fn client_for_context(
    cluster_context: &str,
    kubeconfig_env_var: Option<String>,
) -> Result<Client, AppError> {
    let source = KubeconfigSource::new(kubeconfig_env_var)?;
    source.live_client_for_context(cluster_context).await
}
