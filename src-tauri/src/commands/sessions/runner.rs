use crate::models::AppErrorKind;
use crate::models::{AppError, PortForwardRequest, PortForwardSessionSummary};
use k8s_openapi::api::core::v1::Pod;
use kube::{api::Api, Client};
use std::{
    future::Future,
    net::{Ipv4Addr, SocketAddrV4},
};
use tokio::{
    io::copy_bidirectional,
    net::{TcpListener, TcpStream},
    task::JoinSet,
};

use super::registry::{session_summary, PortForwardRegistry};
use super::target::{
    client_for_context, resolve_port_forward_target, validate_request, PortForwardTarget,
    ValidatedPortForwardRequest,
};

const MAX_CONSECUTIVE_ACCEPT_FAILURES: u32 = 5;
pub(crate) const ACCEPT_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(200);

pub(crate) fn should_retry_accept(consecutive_failures: u32) -> bool {
    consecutive_failures < MAX_CONSECUTIVE_ACCEPT_FAILURES
}

fn port_forward_error(err: kube::Error) -> AppError {
    let mut error = AppError::from(err);
    if error.kind == AppErrorKind::Forbidden {
        error.message = format!(
            "port-forward forbidden by Kubernetes RBAC: {}",
            error.message
        );
    }
    error
}

// kube's Portforwarder does not abort its task on drop.
struct OwnedPortforwarder(kube::api::Portforwarder);

impl Drop for OwnedPortforwarder {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn verify_pod_port_forward(
    client: Client,
    target: &PortForwardTarget,
) -> Result<(), AppError> {
    let pods: Api<Pod> = Api::namespaced(client, &target.namespace);
    let forwarder = pods
        .portforward(&target.pod_name, &[target.pod_port])
        .await
        .map_err(|err| {
            let mut error = port_forward_error(err);
            error.kind = AppErrorKind::LiveSessionTargetUnavailable;
            error
        })?;
    forwarder.abort();
    Ok(())
}

pub(crate) async fn forward_pod_connection(
    client: Client,
    namespace: String,
    pod_name: String,
    pod_port: u16,
    mut local_stream: TcpStream,
) -> Result<(), AppError> {
    let pods: Api<Pod> = Api::namespaced(client, &namespace);
    let mut forwarder = OwnedPortforwarder(
        pods.portforward(&pod_name, &[pod_port])
            .await
            .map_err(port_forward_error)?,
    );
    let mut pod_stream = forwarder.0.take_stream(pod_port).ok_or_else(|| {
        AppError::new(
            format!("remote port {pod_port} did not open"),
            AppErrorKind::Session,
        )
    })?;
    let error_future = forwarder.0.take_error(pod_port);
    let copy = copy_bidirectional(&mut local_stream, &mut pod_stream);
    tokio::pin!(copy);
    let result = if let Some(error_future) = error_future {
        tokio::pin!(error_future);
        tokio::select! {
            copy_result = &mut copy => copy_result,
            port_error = &mut error_future => {
                match port_error {
                    Some(message) if !message.trim().is_empty() => return Err(AppError::new(message, AppErrorKind::Session)),
                    // Closing the diagnostic channel does not complete data transfer.
                    _ => copy.await,
                }
            }
        }
    } else {
        copy.await
    };
    result.map(|_| ()).map_err(|error| {
        AppError::new("Port-forward data transfer failed", AppErrorKind::Io).with_source(error)
    })
}

async fn forward_connection(
    client: Client,
    target: PortForwardTarget,
    local_stream: TcpStream,
) -> Result<(), AppError> {
    forward_pod_connection(
        client,
        target.namespace,
        target.pod_name,
        target.pod_port,
        local_stream,
    )
    .await
}

async fn resolve_and_forward_connection(
    client: Client,
    request: ValidatedPortForwardRequest,
    local_stream: TcpStream,
    session_id: String,
    registry: PortForwardRegistry,
) -> Result<(), AppError> {
    registry.mark_status(&session_id, "reconnecting", None);
    let target = resolve_port_forward_target(request).await?;
    registry.mark_resolved_target(&session_id, &target);
    registry.mark_status(&session_id, "connected", None);
    forward_connection(client, target, local_stream).await
}

pub(super) async fn run_port_forward_session<H, F>(
    session_id: String,
    listener: TcpListener,
    registry: PortForwardRegistry,
    handle_connection: H,
) where
    H: Fn(TcpStream) -> F + Send + 'static,
    F: Future<Output = Result<(), AppError>> + Send + 'static,
{
    let mut connections = JoinSet::new();
    let mut consecutive_accept_failures: u32 = 0;
    loop {
        tokio::select! {
            accept_result = listener.accept() => {
                let (local_stream, _) = match accept_result {
                    Ok(accepted) => {
                        consecutive_accept_failures = 0;
                        accepted
                    }
                    Err(err) => {
                        consecutive_accept_failures += 1;
                        if !should_retry_accept(consecutive_accept_failures) {
                            registry.mark_error(&session_id, err.to_string());
                            break;
                        }
                        registry.mark_status(
                            &session_id,
                            "listening",
                            Some(format!("accept retry {consecutive_accept_failures}: {err}")),
                        );
                        tokio::time::sleep(ACCEPT_RETRY_DELAY).await;
                        continue;
                    }
                };
                connections.spawn(handle_connection(local_stream));
            }
            join_result = connections.join_next(), if !connections.is_empty() => {
                match join_result {
                    Some(Ok(Ok(()))) => {
                        if connections.is_empty() {
                            registry.mark_status(&session_id, "listening", None);
                        }
                    }
                    Some(Ok(Err(message))) => {
                        registry.mark_error(&session_id, message.message);
                    }
                    Some(Err(err)) => {
                        registry.mark_error(&session_id, err.to_string());
                    }
                    None => {}
                }
            }
        }
    }
}

pub(super) async fn start_pod_port_forward_in_registry(
    request: PortForwardRequest,
    registry: &PortForwardRegistry,
) -> Result<PortForwardSessionSummary, AppError> {
    let request = validate_request(&request)?;
    if let Some(local_port) = request.local_port {
        if registry.has_local_port(local_port) {
            return Err(AppError::new(
                format!("local port {local_port} is already forwarded"),
                AppErrorKind::LiveSessionTargetUnavailable,
            ));
        }
    }

    let bind_port = request.local_port.unwrap_or(0);
    let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, bind_port))
        .await
        .map_err(|err| {
            AppError::new(
                format!("local port unavailable: {err}"),
                AppErrorKind::LiveSessionTargetUnavailable,
            )
        })?;
    let local_port = listener
        .local_addr()
        .map_err(|err| {
            AppError::new(
                format!("local port unavailable: {err}"),
                AppErrorKind::LiveSessionTargetUnavailable,
            )
            .with_source(err)
        })?
        .port();
    if registry.has_local_port(local_port) {
        return Err(AppError::new(
            format!("local port {local_port} is already forwarded"),
            AppErrorKind::LiveSessionTargetUnavailable,
        ));
    }

    let target = resolve_port_forward_target(request).await?;
    let client =
        client_for_context(&target.cluster_context, target.kubeconfig_env_var.clone()).await?;
    verify_pod_port_forward(client.clone(), &target).await?;

    let session_id = registry.session_id();
    let summary = session_summary(session_id.clone(), &target, local_port);
    let loop_registry = registry.clone();
    let handle_request = ValidatedPortForwardRequest {
        cluster_context: target.cluster_context.clone(),
        kubeconfig_env_var: target.kubeconfig_env_var.clone(),
        kubeconfig_source_key: target.kubeconfig_source_key.clone(),
        kubeconfig_source_label: target.kubeconfig_source_label.clone(),
        namespace: target.namespace.clone(),
        target_kind: target.target_kind,
        target_name: target.target_name.clone(),
        remote_port: target.remote_port,
        local_port: Some(local_port),
    };
    let connection_session_id = session_id.clone();
    let connection_registry = registry.clone();
    let handle_connection = move |local_stream: TcpStream| {
        resolve_and_forward_connection(
            client.clone(),
            handle_request.clone(),
            local_stream,
            connection_session_id.clone(),
            connection_registry.clone(),
        )
    };
    let handle = tauri::async_runtime::spawn(async move {
        run_port_forward_session(session_id, listener, loop_registry, handle_connection).await;
    });
    registry.insert(summary, handle)
}
