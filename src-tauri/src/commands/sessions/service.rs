use super::target::{
    client_for_context, validate_port, PortForwardTarget, PortForwardTargetKind,
    ValidatedPortForwardRequest,
};
use crate::models::AppError;
use crate::models::AppErrorKind;
use k8s_openapi::{
    api::core::v1::{Pod, Service, ServicePort},
    apimachinery::pkg::util::intstr::IntOrString,
};
use kube::{
    api::{Api, ListParams},
    Client,
};
use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub(crate) struct ServiceTarget {
    pub(crate) namespace: String,
    pub(crate) pod_name: String,
    pub(crate) pod_port: u16,
}

pub(crate) async fn resolve_service_target(
    client: Client,
    namespace: &str,
    service_name: &str,
    service_port: u16,
) -> Result<ServiceTarget, AppError> {
    crate::commands::helpers::validate_namespace(Some(namespace))?;
    crate::commands::helpers::validate_path_segment(service_name, "Service name")?;
    let services: Api<Service> = Api::namespaced(client.clone(), namespace);
    let service = services.get(service_name).await.map_err(AppError::from)?;
    let service_spec = service
        .spec
        .ok_or_else(|| AppError::new("service spec is unavailable", AppErrorKind::Session))?;
    if matches!(service_spec.type_.as_deref(), Some("ExternalName")) {
        return Err(AppError::new(
            "ExternalName Services cannot be port-forwarded",
            AppErrorKind::Session,
        ));
    }

    let selected_port = select_service_port(
        service_spec.ports.as_deref().unwrap_or_default(),
        service_port,
    )?;
    let selector = service_spec
        .selector
        .filter(|selector| !selector.is_empty())
        .ok_or_else(|| {
            AppError::new(
                "service port-forwarding requires a selector-backed Service",
                AppErrorKind::Session,
            )
        })?;

    let pods: Api<Pod> = Api::namespaced(client, namespace);
    let pod_list = pods
        .list(&ListParams::default().labels(&label_selector(&selector)))
        .await
        .map_err(AppError::from)?;
    let (pod_name, pod_port) = resolve_ready_service_target(pod_list.items, selected_port)?;

    Ok(ServiceTarget {
        namespace: namespace.to_string(),
        pod_name,
        pod_port,
    })
}

pub(super) async fn resolve_service_request_target(
    request: ValidatedPortForwardRequest,
) -> Result<PortForwardTarget, AppError> {
    let client =
        client_for_context(&request.cluster_context, request.kubeconfig_env_var.clone()).await?;
    let resolved = resolve_service_target(
        client,
        &request.namespace,
        &request.target_name,
        request.remote_port,
    )
    .await?;

    Ok(PortForwardTarget {
        cluster_context: request.cluster_context,
        kubeconfig_env_var: request.kubeconfig_env_var,
        kubeconfig_source_key: request.kubeconfig_source_key,
        kubeconfig_source_label: request.kubeconfig_source_label,
        namespace: resolved.namespace,
        target_kind: PortForwardTargetKind::Service,
        target_name: request.target_name,
        pod_name: resolved.pod_name,
        remote_port: request.remote_port,
        pod_port: resolved.pod_port,
    })
}

fn label_selector(selector: &BTreeMap<String, String>) -> String {
    selector
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join(",")
}

fn select_service_port(
    ports: &[ServicePort],
    requested_port: u16,
) -> Result<&ServicePort, AppError> {
    if ports.is_empty() {
        return Err(AppError::new(
            "service has no ports to forward",
            AppErrorKind::Session,
        ));
    }
    let Some(port) = ports
        .iter()
        .find(|port| port.port == i32::from(requested_port))
    else {
        let available = ports
            .iter()
            .map(|port| port.port.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(AppError::new(
            format!("service port {requested_port} was not found; available ports: {available}"),
            AppErrorKind::Validation,
        ));
    };
    if matches!(port.protocol.as_deref(), Some(protocol) if protocol != "TCP") {
        return Err(AppError::new(
            "only TCP Service ports can be forwarded",
            AppErrorKind::Validation,
        ));
    }
    Ok(port)
}

fn resolve_ready_service_target(
    pods: Vec<Pod>,
    service_port: &ServicePort,
) -> Result<(String, u16), AppError> {
    if let Some(IntOrString::String(port_name)) = service_port.target_port.as_ref() {
        let mut ready_pods = pods
            .into_iter()
            .filter(is_ready_running_pod)
            .collect::<Vec<_>>();
        if ready_pods.is_empty() {
            return Err(AppError::new(
                "no ready Pods matched this Service selector",
                AppErrorKind::LiveSessionTargetUnavailable,
            ));
        }
        ready_pods.sort_by_key(pod_name);
        for pod in ready_pods {
            if let Some(port) = named_container_port(&pod, port_name) {
                return Ok((
                    pod_name(&pod).ok_or_else(|| {
                        AppError::new(
                            "resolved Service target Pod is missing a name",
                            AppErrorKind::Session,
                        )
                    })?,
                    validate_port(i64::from(port), "container port")?,
                ));
            }
        }
        return Err(AppError::new(
            format!("Service targetPort '{port_name}' was not found on the resolved Pod"),
            AppErrorKind::Session,
        ));
    }

    let pod = select_ready_pod(pods).ok_or_else(|| {
        AppError::new(
            "no ready Pods matched this Service selector",
            AppErrorKind::LiveSessionTargetUnavailable,
        )
    })?;
    let pod_name = pod_name(&pod).ok_or_else(|| {
        AppError::new(
            "resolved Service target Pod is missing a name",
            AppErrorKind::Session,
        )
    })?;
    Ok((pod_name, resolve_service_target_port(&pod, service_port)?))
}

fn select_ready_pod(mut pods: Vec<Pod>) -> Option<Pod> {
    pods.sort_by_key(pod_name);
    pods.into_iter().find(is_ready_running_pod)
}

fn is_ready_running_pod(pod: &Pod) -> bool {
    if pod.metadata.deletion_timestamp.is_some() {
        return false;
    }
    let Some(status) = &pod.status else {
        return false;
    };
    if status.phase.as_deref() != Some("Running") {
        return false;
    }
    status.conditions.as_ref().is_some_and(|conditions| {
        conditions
            .iter()
            .any(|condition| condition.type_ == "Ready" && condition.status == "True")
    })
}

fn pod_name(pod: &Pod) -> Option<String> {
    pod.metadata
        .name
        .as_ref()
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
}

fn resolve_service_target_port(pod: &Pod, service_port: &ServicePort) -> Result<u16, AppError> {
    match service_port.target_port.as_ref() {
        Some(IntOrString::Int(port)) => validate_port(i64::from(*port), "service targetPort"),
        Some(IntOrString::String(name)) => find_named_container_port(pod, name),
        None => validate_port(i64::from(service_port.port), "service port"),
    }
}

fn named_container_port(pod: &Pod, port_name: &str) -> Option<i32> {
    pod.spec
        .as_ref()?
        .containers
        .iter()
        .flat_map(|container| container.ports.iter().flatten())
        .find(|port| port.name.as_deref() == Some(port_name))
        .map(|port| port.container_port)
}

fn find_named_container_port(pod: &Pod, port_name: &str) -> Result<u16, AppError> {
    named_container_port(pod, port_name)
        .map(|port| validate_port(i64::from(port), "container port"))
        .transpose()?
        .ok_or_else(|| {
            AppError::new(
                format!("Service targetPort '{port_name}' was not found on the resolved Pod"),
                AppErrorKind::Session,
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use k8s_openapi::api::core::v1::{Container, ContainerPort, PodCondition, PodSpec, PodStatus};
    use kube::core::ObjectMeta;

    fn pod(name: &str, ready: bool, ports: Vec<ContainerPort>) -> Pod {
        Pod {
            metadata: ObjectMeta {
                name: Some(name.to_string()),
                ..Default::default()
            },
            spec: Some(PodSpec {
                containers: vec![Container {
                    name: "app".to_string(),
                    ports: Some(ports),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            status: Some(PodStatus {
                phase: Some("Running".to_string()),
                conditions: Some(vec![PodCondition {
                    type_: "Ready".to_string(),
                    status: if ready { "True" } else { "False" }.to_string(),
                    ..Default::default()
                }]),
                ..Default::default()
            }),
        }
    }

    #[test]
    fn selects_requested_tcp_service_port() {
        let ports = vec![
            ServicePort {
                port: 80,
                ..Default::default()
            },
            ServicePort {
                port: 443,
                protocol: Some("TCP".to_string()),
                ..Default::default()
            },
        ];

        assert_eq!(select_service_port(&ports, 443).expect("port").port, 443);
        assert_eq!(
            select_service_port(&ports, 8080)
                .expect_err("missing port")
                .message,
            "service port 8080 was not found; available ports: 80, 443",
        );
    }

    #[test]
    fn resolves_named_service_target_port_from_selected_pod() {
        let pod = pod(
            "api-0",
            true,
            vec![ContainerPort {
                name: Some("http".to_string()),
                container_port: 8080,
                ..Default::default()
            }],
        );
        let service_port = ServicePort {
            port: 80,
            target_port: Some(IntOrString::String("http".to_string())),
            ..Default::default()
        };

        assert_eq!(
            resolve_service_target_port(&pod, &service_port).expect("target port"),
            8080,
        );
    }

    #[test]
    fn named_target_port_uses_ready_pod_that_declares_it() {
        let service_port = ServicePort {
            port: 80,
            target_port: Some(IntOrString::String("http".to_string())),
            ..Default::default()
        };
        let (pod_name, pod_port) = resolve_ready_service_target(
            vec![
                pod("api-a", true, vec![]),
                pod(
                    "api-b",
                    true,
                    vec![ContainerPort {
                        name: Some("http".to_string()),
                        container_port: 8080,
                        ..Default::default()
                    }],
                ),
            ],
            &service_port,
        )
        .expect("resolved target");

        assert_eq!(pod_name, "api-b");
        assert_eq!(pod_port, 8080);
    }

    #[test]
    fn named_target_port_without_ready_pods_is_unavailable() {
        let service_port = ServicePort {
            port: 80,
            target_port: Some(IntOrString::String("http".to_string())),
            ..Default::default()
        };
        assert_eq!(
            resolve_ready_service_target(vec![pod("api-0", false, vec![])], &service_port)
                .expect_err("missing ready pod")
                .kind,
            AppErrorKind::LiveSessionTargetUnavailable,
        );
    }

    #[test]
    fn selects_ready_running_pod_by_name() {
        let pods = vec![pod("api-b", true, vec![]), pod("api-a", true, vec![])];

        assert_eq!(
            pod_name(&select_ready_pod(pods).expect("pod")).as_deref(),
            Some("api-a"),
        );
    }

    #[test]
    fn ignores_unready_pods_for_service_resolution() {
        assert!(select_ready_pod(vec![pod("api-0", false, vec![])]).is_none());
    }
}
