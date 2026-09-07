use super::{client_for_context, logs::read_log_line, send};
use crate::models::AppErrorKind;
use crate::models::{AggregatedLogStreamRequest, AppError, LogLineSource, StreamMessage};
use futures_util::StreamExt;
use k8s_openapi::{
    api::{
        apps::v1::Deployment,
        core::v1::{Pod, Service},
    },
    apimachinery::pkg::apis::meta::v1::LabelSelector,
};
use kube::{
    api::{Api, ListParams, LogParams, WatchEvent, WatchParams},
    Client,
};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    time::Duration,
};
use tauri::{async_runtime::JoinHandle, ipc::Channel};

const MAX_AGGREGATED_LOG_SOURCES: usize = 50;

#[derive(Clone)]
struct LogStreamOptions {
    tail_lines: Option<i64>,
    since_seconds: Option<i64>,
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
struct LogSourceKey {
    pod_name: String,
    container: String,
}

#[derive(Default)]
struct SourceStreams {
    handles: HashMap<LogSourceKey, JoinHandle<()>>,
    /// Sources spawned in the current target set. Respawns follow live output only,
    /// so a completed container does not re-replay its tail every relist.
    attached: BTreeSet<LogSourceKey>,
}

impl Drop for SourceStreams {
    fn drop(&mut self) {
        for handle in self.handles.values() {
            handle.abort();
        }
    }
}

pub(super) async fn run_aggregated_log_stream(
    stream_id: String,
    request: AggregatedLogStreamRequest,
    channel: Channel<StreamMessage>,
) {
    let client = match client_for_context(
        &request.cluster_context,
        request.kubeconfig_env_var.clone(),
    )
    .await
    {
        Ok(client) => client,
        Err(err) => {
            send_error(&channel, stream_id, err.message);
            return;
        }
    };

    let selector = match resolve_target_selector(client.clone(), &request).await {
        Ok(selector) => selector,
        Err(err) => {
            send_error(&channel, stream_id, err.message);
            return;
        }
    };
    let pods: Api<Pod> = Api::namespaced(client.clone(), &request.namespace);
    let options = LogStreamOptions {
        tail_lines: Some(request.tail_lines.unwrap_or(200)),
        since_seconds: request.since_seconds,
    };
    let mut source_streams = SourceStreams::default();
    loop {
        let list = match pods.list(&ListParams::default().labels(&selector)).await {
            Ok(list) => list,
            Err(err) => {
                if !send_status(
                    &channel,
                    &stream_id,
                    "reconnecting",
                    format!("Log pod list failed: {err}"),
                ) {
                    return;
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };
        let resource_version = list
            .metadata
            .resource_version
            .clone()
            .unwrap_or_else(|| "0".to_string());
        let (sources, total_sources) = log_sources_from_pods(&list.items);
        reconcile_sources(
            &mut source_streams,
            sources,
            client.clone(),
            request.namespace.clone(),
            options.clone(),
            channel.clone(),
            stream_id.clone(),
        );
        if !send_status(
            &channel,
            &stream_id,
            "connected",
            aggregate_status_message(total_sources, source_streams.handles.len()),
        ) {
            return;
        }

        let params = WatchParams::default().labels(&selector).timeout(30);
        match pods.watch(&params, &resource_version).await {
            Ok(stream) => {
                let mut stream = stream.boxed();
                while let Some(event) = stream.next().await {
                    match event {
                        Ok(event) => {
                            if matches!(
                                event,
                                WatchEvent::Added(_)
                                    | WatchEvent::Modified(_)
                                    | WatchEvent::Deleted(_)
                            ) {
                                break;
                            }
                            if let WatchEvent::Error(status) = event {
                                let message = if status.message.is_empty() {
                                    "Kubernetes pod watch returned an error".to_string()
                                } else {
                                    status.message
                                };
                                if !send_status(&channel, &stream_id, "reconnecting", message) {
                                    return;
                                }
                                break;
                            }
                        }
                        Err(err) => {
                            if !send_status(
                                &channel,
                                &stream_id,
                                "reconnecting",
                                format!("Log pod watch failed: {err}"),
                            ) {
                                return;
                            }
                            break;
                        }
                    }
                }
            }
            Err(err) => {
                if !send_status(
                    &channel,
                    &stream_id,
                    "reconnecting",
                    format!("Log pod watch failed: {err}"),
                ) {
                    return;
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn resolve_target_selector(
    client: Client,
    request: &AggregatedLogStreamRequest,
) -> Result<String, AppError> {
    match request.target_kind.as_str() {
        "Deployment" => {
            let deployments: Api<Deployment> = Api::namespaced(client, &request.namespace);
            let deployment = deployments
                .get(&request.target_name)
                .await
                .map_err(AppError::from)?;
            let spec = deployment.spec.ok_or_else(|| {
                AppError::new("Deployment spec is unavailable", AppErrorKind::Logs)
            })?;
            selector_from_label_selector(&spec.selector)
        }
        "Service" => {
            let services: Api<Service> = Api::namespaced(client, &request.namespace);
            let service = services
                .get(&request.target_name)
                .await
                .map_err(AppError::from)?;
            service_label_selector(&service)
        }
        _ => Err(AppError::new(
            "aggregated logs support Deployment and selector-backed Service targets",
            AppErrorKind::Validation,
        )),
    }
}

fn selector_from_label_selector(selector: &LabelSelector) -> Result<String, AppError> {
    let mut parts = Vec::new();
    if let Some(labels) = &selector.match_labels {
        parts.extend(labels.iter().map(|(key, value)| format!("{key}={value}")));
    }
    if let Some(expressions) = &selector.match_expressions {
        for expression in expressions {
            let values = expression.values.clone().unwrap_or_default();
            match expression.operator.as_str() {
                "In" => {
                    if values.is_empty() {
                        return Err(AppError::new(
                            "Deployment selector In expression needs values",
                            AppErrorKind::Validation,
                        ));
                    }
                    parts.push(format!("{} in ({})", expression.key, values.join(",")));
                }
                "NotIn" => {
                    if values.is_empty() {
                        return Err(AppError::new(
                            "Deployment selector NotIn expression needs values",
                            AppErrorKind::Validation,
                        ));
                    }
                    parts.push(format!("{} notin ({})", expression.key, values.join(",")));
                }
                "Exists" => parts.push(expression.key.clone()),
                "DoesNotExist" => parts.push(format!("!{}", expression.key)),
                operator => {
                    return Err(AppError::new(
                        format!("unsupported Deployment selector operator: {operator}"),
                        AppErrorKind::Validation,
                    ));
                }
            }
        }
    }
    if parts.is_empty() {
        return Err(AppError::new(
            "Deployment selector is empty",
            AppErrorKind::Validation,
        ));
    }
    Ok(parts.join(","))
}

fn service_label_selector(service: &Service) -> Result<String, AppError> {
    let spec = service
        .spec
        .as_ref()
        .ok_or_else(|| AppError::new("service spec is unavailable", AppErrorKind::Logs))?;
    if matches!(spec.type_.as_deref(), Some("ExternalName")) {
        return Err(AppError::new(
            "ExternalName Services do not have pod logs",
            AppErrorKind::Validation,
        ));
    }
    let selector = spec
        .selector
        .as_ref()
        .filter(|selector| !selector.is_empty())
        .ok_or_else(|| {
            AppError::new(
                "aggregated Service logs require a selector-backed Service",
                AppErrorKind::Validation,
            )
        })?;
    Ok(map_label_selector(selector))
}

fn map_label_selector(selector: &BTreeMap<String, String>) -> String {
    selector
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join(",")
}

fn log_sources_from_pods(pods: &[Pod]) -> (Vec<LogSourceKey>, usize) {
    let mut sources = pods
        .iter()
        .filter(|pod| pod.metadata.deletion_timestamp.is_none())
        .flat_map(log_sources_from_pod)
        .collect::<Vec<_>>();
    sources.sort();
    let total = sources.len();
    sources.truncate(MAX_AGGREGATED_LOG_SOURCES);
    (sources, total)
}

fn log_sources_from_pod(pod: &Pod) -> Vec<LogSourceKey> {
    let Some(pod_name) = pod
        .metadata
        .name
        .as_ref()
        .map(|name| name.trim())
        .filter(|name| !name.is_empty())
    else {
        return Vec::new();
    };
    let Some(spec) = &pod.spec else {
        return Vec::new();
    };
    spec.containers
        .iter()
        .map(|container| container.name.trim())
        .filter(|container| !container.is_empty())
        .map(|container| LogSourceKey {
            pod_name: pod_name.to_string(),
            container: container.to_string(),
        })
        .collect()
}

fn reconcile_sources(
    source_streams: &mut SourceStreams,
    sources: Vec<LogSourceKey>,
    client: Client,
    namespace: String,
    options: LogStreamOptions,
    channel: Channel<StreamMessage>,
    stream_id: String,
) {
    let desired = sources.into_iter().collect::<BTreeSet<_>>();
    source_streams
        .attached
        .retain(|source| desired.contains(source));
    let stale = source_streams
        .handles
        .keys()
        .filter(|source| !desired.contains(*source))
        .cloned()
        .collect::<Vec<_>>();
    for source in stale {
        if let Some(handle) = source_streams.handles.remove(&source) {
            handle.abort();
        }
    }
    source_streams
        .handles
        .retain(|_, handle| !handle.inner().is_finished());
    for source in desired {
        if source_streams.handles.contains_key(&source) {
            continue;
        }
        let mut spawn_options = options.clone();
        if source_streams.attached.contains(&source) {
            // Respawn after a stream break: follow new output only. Omitting
            // tailLines would replay the container's full history, and a
            // repeated tail would replay the same lines every relist.
            spawn_options.tail_lines = Some(0);
        } else {
            source_streams.attached.insert(source.clone());
        }
        let handle = tauri::async_runtime::spawn(stream_log_source(
            client.clone(),
            namespace.clone(),
            source.clone(),
            spawn_options,
            channel.clone(),
            stream_id.clone(),
        ));
        source_streams.handles.insert(source, handle);
    }
}

async fn stream_log_source(
    client: Client,
    namespace: String,
    source: LogSourceKey,
    options: LogStreamOptions,
    channel: Channel<StreamMessage>,
    stream_id: String,
) {
    let pods: Api<Pod> = Api::namespaced(client, &namespace);
    let params = LogParams {
        container: Some(source.container.clone()),
        follow: true,
        tail_lines: options.tail_lines,
        since_seconds: options.since_seconds,
        timestamps: true,
        ..LogParams::default()
    };
    match pods.log_stream(&source.pod_name, &params).await {
        Ok(logs) => {
            let mut logs = logs;
            loop {
                match read_log_line(&mut logs).await {
                    Ok(Some(line)) => {
                        if !send(
                            &channel,
                            StreamMessage::LogLine {
                                stream_id: stream_id.clone(),
                                line,
                                source: Some(log_line_source(&source)),
                            },
                        ) {
                            return;
                        }
                    }
                    Ok(None) => return,
                    Err(err) => {
                        send_source_error(&channel, &stream_id, &source, err.to_string());
                        return;
                    }
                }
            }
        }
        Err(err) => send_source_error(&channel, &stream_id, &source, err.to_string()),
    }
}

fn log_line_source(source: &LogSourceKey) -> LogLineSource {
    LogLineSource {
        pod_name: source.pod_name.clone(),
        container: Some(source.container.clone()),
    }
}

fn send_source_error(
    channel: &Channel<StreamMessage>,
    stream_id: &str,
    source: &LogSourceKey,
    message: String,
) {
    send(
        channel,
        StreamMessage::LogLine {
            stream_id: stream_id.to_string(),
            line: format!("log stream error: {message}"),
            source: Some(log_line_source(source)),
        },
    );
}

fn send_error(channel: &Channel<StreamMessage>, stream_id: String, message: String) {
    send(channel, StreamMessage::Error { stream_id, message });
}

fn send_status(
    channel: &Channel<StreamMessage>,
    stream_id: &str,
    status: &str,
    message: String,
) -> bool {
    send(
        channel,
        StreamMessage::Status {
            stream_id: stream_id.to_string(),
            status: status.to_string(),
            message,
        },
    )
}

fn aggregate_status_message(total_sources: usize, active_sources: usize) -> String {
    match (total_sources, active_sources) {
        (0, _) => "Watching for matching Pods".to_string(),
        (total, active) if total > active => {
            format!("Streaming first {active} of {total} pod containers")
        }
        (_, 1) => "Streaming 1 pod container".to_string(),
        (_, active) => format!("Streaming {active} pod containers"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn audit_departed_log_sources_release_replay_bookkeeping() {
        let source = LogSourceKey {
            pod_name: "old-pod".into(),
            container: "app".into(),
        };
        let mut streams = SourceStreams::default();
        streams.attached.insert(source);
        let (service, _handle) = tower_test::mock::pair::<
            http::Request<kube::client::Body>,
            http::Response<kube::client::Body>,
        >();
        reconcile_sources(
            &mut streams,
            Vec::new(),
            Client::new(service, "default"),
            "default".into(),
            LogStreamOptions {
                tail_lines: Some(10),
                since_seconds: None,
            },
            Channel::new(|_| Ok(())),
            "logs".into(),
        );
        assert!(streams.attached.is_empty());
    }
    use k8s_openapi::{
        api::core::v1::{Container, PodSpec, ServiceSpec},
        apimachinery::pkg::apis::meta::v1::{LabelSelectorRequirement, Time},
    };
    use kube::core::ObjectMeta;

    fn pod(name: &str, containers: &[&str]) -> Pod {
        Pod {
            metadata: ObjectMeta {
                name: Some(name.to_string()),
                ..Default::default()
            },
            spec: Some(PodSpec {
                containers: containers
                    .iter()
                    .map(|name| Container {
                        name: (*name).to_string(),
                        ..Default::default()
                    })
                    .collect(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn deployment_selector_supports_match_labels_and_expressions() {
        let selector = LabelSelector {
            match_labels: Some(BTreeMap::from([(
                "app.kubernetes.io/name".to_string(),
                "api".to_string(),
            )])),
            match_expressions: Some(vec![
                LabelSelectorRequirement {
                    key: "tier".to_string(),
                    operator: "In".to_string(),
                    values: Some(vec!["web".to_string(), "worker".to_string()]),
                },
                LabelSelectorRequirement {
                    key: "canary".to_string(),
                    operator: "DoesNotExist".to_string(),
                    values: None,
                },
            ]),
        };

        assert_eq!(
            selector_from_label_selector(&selector).expect("selector"),
            "app.kubernetes.io/name=api,tier in (web,worker),!canary",
        );
    }

    #[test]
    fn deployment_selector_rejects_empty_or_invalid_expressions() {
        assert_eq!(
            selector_from_label_selector(&LabelSelector::default())
                .expect_err("empty selector")
                .message,
            "Deployment selector is empty",
        );
        let selector = LabelSelector {
            match_labels: None,
            match_expressions: Some(vec![LabelSelectorRequirement {
                key: "tier".to_string(),
                operator: "In".to_string(),
                values: None,
            }]),
        };
        assert_eq!(
            selector_from_label_selector(&selector)
                .expect_err("missing values")
                .message,
            "Deployment selector In expression needs values",
        );
    }

    #[test]
    fn service_selector_requires_selector_backed_service() {
        let selector = BTreeMap::from([
            ("app".to_string(), "api".to_string()),
            ("tier".to_string(), "web".to_string()),
        ]);
        let service = Service {
            spec: Some(ServiceSpec {
                selector: Some(selector),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            service_label_selector(&service).expect("selector"),
            "app=api,tier=web",
        );

        let empty = Service {
            spec: Some(ServiceSpec {
                selector: None,
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(
            service_label_selector(&empty)
                .expect_err("selectorless")
                .kind,
            AppErrorKind::Validation,
        );
    }

    #[test]
    fn log_sources_skip_deleting_pods_and_cap_results() {
        let mut deleting = pod("api-old", &["app"]);
        deleting.metadata.deletion_timestamp =
            Some(Time(k8s_openapi::jiff::Timestamp::from_second(0).unwrap()));
        let pods = std::iter::once(deleting)
            .chain((0..60).map(|index| pod(&format!("api-{index:02}"), &["app"])))
            .collect::<Vec<_>>();

        let (sources, total) = log_sources_from_pods(&pods);

        assert_eq!(total, 60);
        assert_eq!(sources.len(), MAX_AGGREGATED_LOG_SOURCES);
        assert_eq!(sources[0].pod_name, "api-00");
        assert!(sources.iter().all(|source| source.container == "app"));
    }
}
