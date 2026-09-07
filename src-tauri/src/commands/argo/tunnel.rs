use crate::models::AppErrorKind;
use crate::{
    commands::sessions::{
        runner::{forward_pod_connection, should_retry_accept, ACCEPT_RETRY_DELAY},
        service::resolve_service_target,
        target::client_for_context,
    },
    models::AppError,
};
use k8s_openapi::api::core::v1::Pod;
use kube::{api::Api, Client};
use std::{
    net::{Ipv4Addr, SocketAddrV4},
    sync::Arc,
};
use tokio::{
    net::TcpListener,
    sync::oneshot,
    task::{JoinHandle, JoinSet},
};

pub(crate) struct ArgoServiceTunnel {
    local_port: u16,
    state: Arc<TunnelState>,
}

struct TunnelState {
    shutdown: std::sync::Mutex<Option<oneshot::Sender<()>>>,
    task: std::sync::Mutex<Option<JoinHandle<()>>>,
}

impl TunnelState {
    fn close(&self) {
        if let Ok(mut shutdown) = self.shutdown.lock() {
            if let Some(shutdown) = shutdown.take() {
                let _ = shutdown.send(());
            }
        }
        if let Ok(mut task) = self.task.lock() {
            if let Some(task) = task.take() {
                task.abort();
            }
        }
    }
}

impl ArgoServiceTunnel {
    pub(crate) async fn start(
        cluster_context: &str,
        kubeconfig_env_var: Option<String>,
        namespace: String,
        service_name: String,
        service_port: u16,
    ) -> Result<Self, AppError> {
        let client = client_for_context(cluster_context, kubeconfig_env_var).await?;
        let target =
            resolve_service_target(client.clone(), &namespace, &service_name, service_port).await?;
        verify_port_forward(
            client.clone(),
            &target.namespace,
            &target.pod_name,
            target.pod_port,
        )
        .await?;
        let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|error| {
                AppError::new(
                    format!("local tunnel unavailable: {error}"),
                    AppErrorKind::ArgoTunnel,
                )
                .with_source(error)
            })?;
        let local_port = listener
            .local_addr()
            .map_err(|error| {
                AppError::new(
                    format!("local tunnel unavailable: {error}"),
                    AppErrorKind::ArgoTunnel,
                )
                .with_source(error)
            })?
            .port();
        let (shutdown, shutdown_rx) = oneshot::channel();
        let task = tokio::spawn(run_tunnel(
            listener,
            shutdown_rx,
            client,
            Arc::new(ServiceRoute {
                namespace,
                service_name,
                service_port,
            }),
        ));
        Ok(Self {
            local_port,
            state: Arc::new(TunnelState {
                shutdown: std::sync::Mutex::new(Some(shutdown)),
                task: std::sync::Mutex::new(Some(task)),
            }),
        })
    }

    pub(crate) fn local_port(&self) -> u16 {
        self.local_port
    }

    pub(crate) fn close(&self) {
        self.state.close();
    }

    #[cfg(test)]
    pub(crate) fn test_tunnel(shutdown: oneshot::Sender<()>) -> Self {
        Self {
            local_port: 0,
            state: Arc::new(TunnelState {
                shutdown: std::sync::Mutex::new(Some(shutdown)),
                task: std::sync::Mutex::new(None),
            }),
        }
    }
}

impl Drop for ArgoServiceTunnel {
    fn drop(&mut self) {
        self.close();
    }
}

#[derive(Clone)]
struct ServiceRoute {
    namespace: String,
    service_name: String,
    service_port: u16,
}

async fn verify_port_forward(
    client: Client,
    namespace: &str,
    pod_name: &str,
    pod_port: u16,
) -> Result<(), AppError> {
    let pods: Api<Pod> = Api::namespaced(client, namespace);
    let forwarder = pods
        .portforward(pod_name, &[pod_port])
        .await
        .map_err(port_forward_error)?;
    forwarder.abort();
    Ok(())
}

fn port_forward_error(error: kube::Error) -> AppError {
    let message = error.to_string();
    if crate::models::kube_error_kind(&error) == AppErrorKind::Forbidden {
        return AppError::new(
            "Kubernetes RBAC denies pods/portforward for this Argo CD Service",
            AppErrorKind::ArgoTunnelForbidden,
        )
        .with_source(error);
    }
    AppError::new(
        format!("Argo CD Service tunnel could not open: {message}"),
        AppErrorKind::ArgoTunnel,
    )
    .with_source(error)
}

async fn run_tunnel(
    listener: TcpListener,
    mut shutdown: oneshot::Receiver<()>,
    client: Client,
    route: Arc<ServiceRoute>,
) {
    let mut connections = JoinSet::new();
    let mut consecutive_accept_failures = 0;
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            accept = listener.accept() => match accept {
                Ok((stream, _)) => {
                    consecutive_accept_failures = 0;
                    let client = client.clone();
                    let route = route.clone();
                    connections.spawn(async move {
                        let target = resolve_service_target(
                            client.clone(),
                            &route.namespace,
                            &route.service_name,
                            route.service_port,
                        )
                        .await?;
                        forward_pod_connection(
                            client,
                            target.namespace,
                            target.pod_name,
                            target.pod_port,
                            stream,
                        )
                        .await
                    });
                }
                Err(error) => {
                    consecutive_accept_failures += 1;
                    if !should_retry_accept(consecutive_accept_failures) {
                        eprintln!("[kubecove:backend] Argo Service tunnel accept failed: {error}");
                        break;
                    }
                    eprintln!("[kubecove:backend] Argo Service tunnel accept retry {consecutive_accept_failures}: {error}");
                    tokio::time::sleep(ACCEPT_RETRY_DELAY).await;
                }
            },
            connection = connections.join_next(), if !connections.is_empty() => {
                match connection {
                    Some(Ok(Err(error))) => {
                        eprintln!("[kubecove:backend] Argo Service tunnel forward failed: {error}");
                    }
                    Some(Err(error)) => {
                        eprintln!("[kubecove:backend] Argo Service tunnel task failed: {error}");
                    }
                    _ => {}
                }
            },
        }
    }
    connections.abort_all();
    while connections.join_next().await.is_some() {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn close_signals_shared_shutdown_once() {
        let (shutdown, receiver) = oneshot::channel();
        let tunnel = ArgoServiceTunnel {
            local_port: 0,
            state: Arc::new(TunnelState {
                shutdown: std::sync::Mutex::new(Some(shutdown)),
                task: std::sync::Mutex::new(None),
            }),
        };
        let state = tunnel.state.clone();

        tunnel.close();
        assert!(receiver.await.is_ok());
        state.close();
    }

    #[test]
    fn forbidden_port_forward_is_actionable_and_redacted() {
        let error = port_forward_error(kube::Error::Api(Box::new(kube::core::Status {
            code: 403,
            reason: "Forbidden".into(),
            message: "pods/portforward is forbidden".into(),
            ..Default::default()
        })));
        assert_eq!(error.kind, AppErrorKind::ArgoTunnelForbidden);
        assert!(!error.message.contains("token"));
    }
}
