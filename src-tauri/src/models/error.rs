use serde::{Deserialize, Serialize};
use std::{error::Error, fmt, sync::Arc};

#[derive(Debug, thiserror::Error)]
#[error("workspace request cancelled")]
pub(crate) struct WorkspaceRequestCancelled;

/// Stable categories serialized at the frontend boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AppErrorKind {
    AdmissionDenied,
    ArgoApi,
    ArgoConnection,
    ArgoInspection,
    ArgoOperationUnavailable,
    ArgoTunnel,
    ArgoTunnelForbidden,
    Cancelled,
    Cluster,
    ConfirmationRequired,
    CredentialUnavailable,
    FieldManagerConflict,
    Forbidden,
    ImmutableField,
    Internal,
    InvalidResource,
    Io,
    Kubeconfig,
    LiveSessionTargetUnavailable,
    Logs,
    Network,
    NotFound,
    ProviderDiscoveryUnavailable,
    Serialization,
    Session,
    Transport,
    UnsupportedOperation,
    #[serde(rename = "usage_metrics")]
    UsageMetrics,
    Validation,
}

impl AppErrorKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AdmissionDenied => "admissionDenied",
            Self::ArgoApi => "argoApi",
            Self::ArgoConnection => "argoConnection",
            Self::ArgoInspection => "argoInspection",
            Self::ArgoOperationUnavailable => "argoOperationUnavailable",
            Self::ArgoTunnel => "argoTunnel",
            Self::ArgoTunnelForbidden => "argoTunnelForbidden",
            Self::Cancelled => "cancelled",
            Self::Cluster => "cluster",
            Self::ConfirmationRequired => "confirmationRequired",
            Self::CredentialUnavailable => "credentialUnavailable",
            Self::FieldManagerConflict => "fieldManagerConflict",
            Self::Forbidden => "forbidden",
            Self::ImmutableField => "immutableField",
            Self::Internal => "internal",
            Self::InvalidResource => "invalidResource",
            Self::Io => "io",
            Self::Kubeconfig => "kubeconfig",
            Self::LiveSessionTargetUnavailable => "liveSessionTargetUnavailable",
            Self::Logs => "logs",
            Self::Network => "network",
            Self::NotFound => "notFound",
            Self::ProviderDiscoveryUnavailable => "providerDiscoveryUnavailable",
            Self::Serialization => "serialization",
            Self::Session => "session",
            Self::Transport => "transport",
            Self::UnsupportedOperation => "unsupportedOperation",
            Self::UsageMetrics => "usage_metrics",
            Self::Validation => "validation",
        }
    }
}

impl fmt::Display for AppErrorKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppError {
    pub message: String,
    pub kind: AppErrorKind,
    // Sources stay in Rust, including when errors are cloned into read caches.
    #[serde(skip)]
    source: Option<Arc<dyn Error + Send + Sync>>,
}

impl AppError {
    pub fn new(message: impl Into<String>, kind: AppErrorKind) -> Self {
        Self {
            message: message.into(),
            kind,
            source: None,
        }
    }

    #[must_use]
    pub fn with_source(mut self, source: impl Error + Send + Sync + 'static) -> Self {
        self.source = Some(Arc::new(source));
        self
    }

    pub fn cancelled() -> Self {
        Self::new("request cancelled", AppErrorKind::Cancelled)
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for AppError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn Error + 'static))
    }
}

impl From<kube::Error> for AppError {
    fn from(error: kube::Error) -> Self {
        let kind = kube_error_kind(&error);
        Self::new(error.to_string(), kind).with_source(error)
    }
}

impl From<kube::config::KubeconfigError> for AppError {
    fn from(error: kube::config::KubeconfigError) -> Self {
        Self::new(error.to_string(), AppErrorKind::Kubeconfig).with_source(error)
    }
}

/// Classify transport failures by their types, never by remote message text.
pub(crate) fn kube_error_kind(error: &kube::Error) -> AppErrorKind {
    let mut current: Option<&(dyn Error + 'static)> = Some(error);
    while let Some(source) = current {
        if source.is::<WorkspaceRequestCancelled>() {
            return AppErrorKind::Cancelled;
        }
        if let Some(error) = source.downcast_ref::<std::io::Error>() {
            use std::io::ErrorKind;
            if matches!(
                error.kind(),
                ErrorKind::ConnectionRefused
                    | ErrorKind::ConnectionReset
                    | ErrorKind::ConnectionAborted
                    | ErrorKind::NotConnected
                    | ErrorKind::TimedOut
                    | ErrorKind::AddrNotAvailable
                    | ErrorKind::NetworkUnreachable
                    | ErrorKind::HostUnreachable
            ) {
                return AppErrorKind::Network;
            }
        }
        current = source.source();
    }
    match error {
        kube::Error::Api(status) => status_kind(status),
        kube::Error::InferConfig(_) | kube::Error::InferKubeconfig(_) | kube::Error::Auth(_) => {
            AppErrorKind::Kubeconfig
        }
        kube::Error::Discovery(_) => AppErrorKind::ProviderDiscoveryUnavailable,
        kube::Error::SerdeError(_) | kube::Error::FromUtf8(_) => AppErrorKind::Serialization,
        kube::Error::HyperError(_)
        | kube::Error::Service(_)
        | kube::Error::RustlsTls(_)
        | kube::Error::TlsRequired => AppErrorKind::Network,
        _ => AppErrorKind::Cluster,
    }
}

fn status_kind(status: &kube::core::Status) -> AppErrorKind {
    if status.details.as_ref().is_some_and(|details| {
        details
            .causes
            .iter()
            .any(|cause| cause.reason == "FieldManagerConflict")
    }) {
        return AppErrorKind::FieldManagerConflict;
    }
    if status.code == 403 || status.reason == "Forbidden" {
        return AppErrorKind::Forbidden;
    }
    if status.code == 404 || status.reason == "NotFound" {
        return AppErrorKind::NotFound;
    }
    if status.code == 422 || status.reason == "Invalid" {
        return AppErrorKind::InvalidResource;
    }
    AppErrorKind::Cluster
}

#[cfg(test)]
mod tests {
    use super::*;
    use kube::core::Status;

    fn status(code: u16, reason: &str, message: &str) -> kube::Error {
        kube::Error::Api(Box::new(Status {
            code,
            reason: reason.into(),
            message: message.into(),
            ..Default::default()
        }))
    }

    #[test]
    fn classifies_status_without_reading_message() {
        for (code, reason, kind) in [
            (403, "Forbidden", AppErrorKind::Forbidden),
            (404, "NotFound", AppErrorKind::NotFound),
            (422, "Invalid", AppErrorKind::InvalidResource),
        ] {
            assert_eq!(
                AppError::from(status(code, reason, "untrusted description")).kind,
                kind
            );
        }
    }

    #[test]
    fn error_text_cannot_turn_transport_failure_into_forbidden() {
        let error = kube::Error::Service(Box::new(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "cannot connect to forbidden.example:403",
        )));
        assert_eq!(AppError::from(error).kind, AppErrorKind::Network);
    }

    #[test]
    fn cancellation_survives_transport_wrapping_and_clone() {
        let error = AppError::from(kube::Error::Service(Box::new(WorkspaceRequestCancelled)));
        let cloned = error.clone();
        assert_eq!(cloned.kind, AppErrorKind::Cancelled);
        let source = cloned
            .source()
            .unwrap()
            .downcast_ref::<kube::Error>()
            .unwrap();
        assert!(source.source().unwrap().is::<WorkspaceRequestCancelled>());
    }

    #[test]
    fn serialized_error_has_only_stable_message_and_kind() {
        let error = AppError::new("safe description", AppErrorKind::Network)
            .with_source(std::io::Error::other("backend-only detail"));
        let value = serde_json::to_value(&error).unwrap();
        assert_eq!(
            value,
            serde_json::json!({"message":"safe description","kind":"network"})
        );
        let decoded: AppError = serde_json::from_value(value).unwrap();
        assert_eq!(decoded.kind, AppErrorKind::Network);
        assert!(decoded.source().is_none());
    }
}
