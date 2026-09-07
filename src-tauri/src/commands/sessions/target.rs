use crate::commands::kubeconfig::KubeconfigSource;
use crate::models::AppErrorKind;
use crate::models::{AppError, PortForwardRequest};
use kube::Client;

use super::service;

const MIN_USER_LOCAL_PORT: u16 = 1024;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub(super) enum PortForwardTargetKind {
    Pod,
    Service,
}

impl PortForwardTargetKind {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Pod => "Pod",
            Self::Service => "Service",
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct ValidatedPortForwardRequest {
    pub(super) cluster_context: String,
    pub(super) kubeconfig_env_var: Option<String>,
    pub(super) kubeconfig_source_key: Option<String>,
    pub(super) kubeconfig_source_label: Option<String>,
    pub(super) namespace: String,
    pub(super) target_kind: PortForwardTargetKind,
    pub(super) target_name: String,
    pub(super) remote_port: u16,
    pub(super) local_port: Option<u16>,
}

#[derive(Debug, Clone)]
pub(super) struct PortForwardTarget {
    pub(super) cluster_context: String,
    pub(super) kubeconfig_env_var: Option<String>,
    pub(super) kubeconfig_source_key: Option<String>,
    pub(super) kubeconfig_source_label: Option<String>,
    pub(super) namespace: String,
    pub(super) target_kind: PortForwardTargetKind,
    pub(super) target_name: String,
    pub(super) pod_name: String,
    pub(super) remote_port: u16,
    pub(super) pod_port: u16,
}

pub(super) fn validate_port(value: i64, field: &str) -> Result<u16, AppError> {
    if !(1..=i64::from(u16::MAX)).contains(&value) {
        return Err(AppError::new(
            format!("{field} must be between 1 and 65535"),
            AppErrorKind::Validation,
        ));
    }
    Ok(value as u16)
}

fn target_text(value: Option<&String>) -> Option<String> {
    value
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

fn validate_target_kind(request: &PortForwardRequest) -> Result<PortForwardTargetKind, AppError> {
    let target_kind =
        target_text(request.target_kind.as_ref()).unwrap_or_else(|| "Pod".to_string());
    match target_kind.as_str() {
        "Pod" => Ok(PortForwardTargetKind::Pod),
        "Service" => Ok(PortForwardTargetKind::Service),
        _ => Err(AppError::new(
            "port-forward target kind must be Pod or Service",
            AppErrorKind::Validation,
        )),
    }
}

pub(super) fn validate_request(
    request: &PortForwardRequest,
) -> Result<ValidatedPortForwardRequest, AppError> {
    if request.cluster_context.trim().is_empty() || request.namespace.trim().is_empty() {
        return Err(AppError::new(
            "port-forward target is required",
            AppErrorKind::Validation,
        ));
    }
    let target_kind = validate_target_kind(request)?;
    let target_name = target_text(request.target_name.as_ref())
        .or_else(|| target_text(request.pod_name.as_ref()))
        .ok_or_else(|| {
            AppError::new("port-forward target is required", AppErrorKind::Validation)
        })?;

    crate::commands::helpers::validate_namespace(Some(request.namespace.trim()))?;
    crate::commands::helpers::validate_path_segment(&target_name, "port-forward target")?;
    let remote_port = validate_port(request.remote_port, "remote_port")?;
    let local_port = request
        .local_port
        .map(|port| validate_port(port, "local_port"))
        .transpose()?;
    if matches!(local_port, Some(port) if port < MIN_USER_LOCAL_PORT) {
        return Err(AppError::new(
            "local_port must be 1024 or higher",
            AppErrorKind::Validation,
        ));
    }

    let source = KubeconfigSource::new(request.kubeconfig_env_var.clone())?;
    Ok(ValidatedPortForwardRequest {
        cluster_context: request.cluster_context.trim().to_string(),
        kubeconfig_env_var: request.kubeconfig_env_var.clone(),
        kubeconfig_source_key: Some(source.key()),
        kubeconfig_source_label: source.show_source_labels().then(|| source.label()),
        namespace: request.namespace.trim().to_string(),
        target_kind,
        target_name,
        remote_port,
        local_port,
    })
}

pub(super) async fn resolve_port_forward_target(
    request: ValidatedPortForwardRequest,
) -> Result<PortForwardTarget, AppError> {
    match request.target_kind {
        PortForwardTargetKind::Pod => Ok(PortForwardTarget {
            cluster_context: request.cluster_context,
            kubeconfig_env_var: request.kubeconfig_env_var,
            kubeconfig_source_key: request.kubeconfig_source_key,
            kubeconfig_source_label: request.kubeconfig_source_label,
            namespace: request.namespace,
            target_kind: PortForwardTargetKind::Pod,
            target_name: request.target_name.clone(),
            pod_name: request.target_name,
            remote_port: request.remote_port,
            pod_port: request.remote_port,
        }),
        PortForwardTargetKind::Service => service::resolve_service_request_target(request).await,
    }
}

pub(crate) async fn client_for_context(
    cluster_context: &str,
    kubeconfig_env_var: Option<String>,
) -> Result<Client, AppError> {
    let source = KubeconfigSource::new(kubeconfig_env_var)?;
    source.live_client_for_context(cluster_context).await
}
