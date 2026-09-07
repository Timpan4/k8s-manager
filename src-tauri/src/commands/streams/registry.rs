use crate::models::{StreamMessage, WatchResourceKey, WatchResourceTarget};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};
use tauri::{async_runtime::JoinHandle, ipc::Channel};

#[derive(Clone)]
pub(super) struct StreamBroadcaster {
    subscribers: Arc<Mutex<HashMap<String, Channel<StreamMessage>>>>,
}

impl StreamBroadcaster {
    fn new() -> Self {
        Self {
            subscribers: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn add(&self, stream_id: String, channel: Channel<StreamMessage>) {
        self.subscribers
            .lock()
            .expect("stream subscribers lock")
            .insert(stream_id, channel);
    }

    fn remove(&self, stream_id: &str) -> usize {
        let mut subscribers = self.subscribers.lock().expect("stream subscribers lock");
        subscribers.remove(stream_id);
        subscribers.len()
    }

    fn is_empty(&self) -> bool {
        self.subscribers
            .lock()
            .expect("stream subscribers lock")
            .is_empty()
    }

    fn send(&self, message: impl Fn(&str) -> StreamMessage) -> bool {
        let subscribers = self
            .subscribers
            .lock()
            .expect("stream subscribers lock")
            .clone();
        if subscribers.is_empty() {
            return false;
        }

        let mut failed = Vec::new();
        for (stream_id, channel) in &subscribers {
            if channel.send(message(stream_id)).is_err() {
                failed.push(stream_id.clone());
            }
        }
        if !failed.is_empty() {
            let mut subscribers = self.subscribers.lock().expect("stream subscribers lock");
            for stream_id in failed {
                subscribers.remove(&stream_id);
            }
        }
        !self
            .subscribers
            .lock()
            .expect("stream subscribers lock")
            .is_empty()
    }

    pub(super) fn status(&self, status: &str, message: String) -> bool {
        let status = status.to_string();
        self.send(move |stream_id| StreamMessage::Status {
            stream_id: stream_id.to_string(),
            status: status.clone(),
            message: message.clone(),
        })
    }

    pub(super) fn error(&self, message: String) -> bool {
        self.send(move |stream_id| StreamMessage::Error {
            stream_id: stream_id.to_string(),
            message: message.clone(),
        })
    }

    pub(super) fn resource_changed(&self, target: WatchResourceTarget, action: String) -> bool {
        self.send(move |stream_id| StreamMessage::ResourceChanged {
            stream_id: stream_id.to_string(),
            target: target.clone(),
            action: action.clone(),
        })
    }

    pub(super) fn events_changed(&self, target: WatchResourceTarget, action: String) -> bool {
        self.send(move |stream_id| StreamMessage::ResourceEventsChanged {
            stream_id: stream_id.to_string(),
            target: target.clone(),
            action: action.clone(),
        })
    }
}

struct SharedWatch {
    broadcaster: StreamBroadcaster,
    handle: Option<JoinHandle<()>>,
}

enum StreamSubscription {
    Resources(Vec<String>),
    Events(String),
    Handles(Vec<JoinHandle<()>>),
}

#[derive(Default)]
struct RegistryState {
    resource_watches: HashMap<String, SharedWatch>,
    event_watches: HashMap<String, SharedWatch>,
    streams: HashMap<String, StreamSubscription>,
}

#[derive(Default)]
pub struct StreamRegistry {
    next_id: AtomicU64,
    state: Mutex<RegistryState>,
}

impl StreamRegistry {
    pub(super) fn stream_id(&self, prefix: &str) -> String {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        format!("{prefix}-{id}")
    }

    pub(super) fn subscribe_resource(
        &self,
        stream_id: &str,
        source_key: &str,
        cluster_context: &str,
        key: &WatchResourceKey,
        channel: Channel<StreamMessage>,
    ) -> (String, StreamBroadcaster, bool) {
        let watch_key = resource_watch_key(source_key, cluster_context, key);
        let mut state = self.state.lock().expect("stream registry lock");
        remove_empty_resource_watch(&mut state, &watch_key);
        let mut started = false;
        let watch = state
            .resource_watches
            .entry(watch_key.clone())
            .or_insert_with(|| {
                started = true;
                SharedWatch {
                    broadcaster: StreamBroadcaster::new(),
                    handle: None,
                }
            });
        watch.broadcaster.add(stream_id.to_string(), channel);
        let broadcaster = watch.broadcaster.clone();
        state
            .streams
            .entry(stream_id.to_string())
            .and_modify(|subscription| {
                if let StreamSubscription::Resources(keys) = subscription {
                    keys.push(watch_key.clone());
                }
            })
            .or_insert_with(|| StreamSubscription::Resources(vec![watch_key.clone()]));
        eprintln!(
            "[kubecove:backend] watch_subscribe kind=resource stream={} key={} reused={}",
            stream_id, watch_key, !started
        );
        (watch_key, broadcaster, started)
    }

    pub(super) fn set_resource_handle(
        &self,
        key: &str,
        broadcaster: &StreamBroadcaster,
        handle: JoinHandle<()>,
    ) {
        if let Some(watch) = self
            .state
            .lock()
            .expect("stream registry lock")
            .resource_watches
            .get_mut(key)
        {
            if Arc::ptr_eq(&watch.broadcaster.subscribers, &broadcaster.subscribers)
                && watch.handle.is_none()
            {
                watch.handle = Some(handle);
                return;
            }
        }
        handle.abort();
    }

    pub(super) fn subscribe_event(
        &self,
        stream_id: &str,
        source_key: &str,
        cluster_context: &str,
        kind: &str,
        name: &str,
        namespace: Option<&str>,
        channel: Channel<StreamMessage>,
    ) -> (String, StreamBroadcaster, bool) {
        let watch_key = event_watch_key(source_key, cluster_context, kind, name, namespace);
        let mut state = self.state.lock().expect("stream registry lock");
        remove_empty_event_watch(&mut state, &watch_key);
        let mut started = false;
        let watch = state
            .event_watches
            .entry(watch_key.clone())
            .or_insert_with(|| {
                started = true;
                SharedWatch {
                    broadcaster: StreamBroadcaster::new(),
                    handle: None,
                }
            });
        watch.broadcaster.add(stream_id.to_string(), channel);
        let broadcaster = watch.broadcaster.clone();
        state.streams.insert(
            stream_id.to_string(),
            StreamSubscription::Events(watch_key.clone()),
        );
        eprintln!(
            "[kubecove:backend] watch_subscribe kind=events stream={} key={} reused={}",
            stream_id, watch_key, !started
        );
        (watch_key, broadcaster, started)
    }

    pub(super) fn set_event_handle(
        &self,
        key: &str,
        broadcaster: &StreamBroadcaster,
        handle: JoinHandle<()>,
    ) {
        if let Some(watch) = self
            .state
            .lock()
            .expect("stream registry lock")
            .event_watches
            .get_mut(key)
        {
            if Arc::ptr_eq(&watch.broadcaster.subscribers, &broadcaster.subscribers)
                && watch.handle.is_none()
            {
                watch.handle = Some(handle);
                return;
            }
        }
        handle.abort();
    }

    pub(super) fn insert_handles(&self, stream_id: String, handles: Vec<JoinHandle<()>>) {
        self.state
            .lock()
            .expect("stream registry lock")
            .streams
            .insert(stream_id, StreamSubscription::Handles(handles));
    }

    pub(super) fn stop(&self, stream_id: &str) -> bool {
        let mut state = self.state.lock().expect("stream registry lock");
        let Some(subscription) = state.streams.remove(stream_id) else {
            return false;
        };
        match subscription {
            StreamSubscription::Resources(keys) => {
                for key in keys {
                    let should_remove = state
                        .resource_watches
                        .get(&key)
                        .is_some_and(|watch| watch.broadcaster.remove(stream_id) == 0);
                    if should_remove {
                        if let Some(mut watch) = state.resource_watches.remove(&key) {
                            if let Some(handle) = watch.handle.take() {
                                handle.abort();
                            }
                            eprintln!("[kubecove:backend] watch_stop kind=resource key={key}");
                        }
                    }
                }
            }
            StreamSubscription::Events(key) => {
                let should_remove = state
                    .event_watches
                    .get(&key)
                    .is_some_and(|watch| watch.broadcaster.remove(stream_id) == 0);
                if should_remove {
                    if let Some(mut watch) = state.event_watches.remove(&key) {
                        if let Some(handle) = watch.handle.take() {
                            handle.abort();
                        }
                        eprintln!("[kubecove:backend] watch_stop kind=events key={key}");
                    }
                }
            }
            StreamSubscription::Handles(handles) => {
                for handle in handles {
                    handle.abort();
                }
            }
        }
        true
    }
}

fn remove_empty_resource_watch(state: &mut RegistryState, key: &str) {
    let should_remove = state
        .resource_watches
        .get(key)
        .is_some_and(|watch| watch.broadcaster.is_empty());
    if should_remove {
        if let Some(mut watch) = state.resource_watches.remove(key) {
            if let Some(handle) = watch.handle.take() {
                handle.abort();
            }
            eprintln!("[kubecove:backend] watch_cleanup kind=resource key={key}");
        }
    }
}

fn remove_empty_event_watch(state: &mut RegistryState, key: &str) {
    let should_remove = state
        .event_watches
        .get(key)
        .is_some_and(|watch| watch.broadcaster.is_empty());
    if should_remove {
        if let Some(mut watch) = state.event_watches.remove(key) {
            if let Some(handle) = watch.handle.take() {
                handle.abort();
            }
            eprintln!("[kubecove:backend] watch_cleanup kind=events key={key}");
        }
    }
}

fn resource_watch_key(source_key: &str, cluster_context: &str, key: &WatchResourceKey) -> String {
    format!(
        "{}|context={}|kind={}|api={}|plural={}|namespace={}",
        source_key,
        cluster_context,
        key.resource_kind.kind,
        key.resource_kind
            .api_version
            .as_deref()
            .unwrap_or("<typed>"),
        key.resource_kind.plural.as_deref().unwrap_or("<typed>"),
        key.namespace.as_deref().unwrap_or("<all>")
    )
}

fn event_watch_key(
    source_key: &str,
    cluster_context: &str,
    kind: &str,
    name: &str,
    namespace: Option<&str>,
) -> String {
    format!(
        "{}|context={}|kind={}|namespace={}|name={}",
        source_key,
        cluster_context,
        kind,
        namespace.unwrap_or("<cluster>"),
        name
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending_task() -> (JoinHandle<()>, tokio::sync::oneshot::Receiver<()>) {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let handle = tokio::spawn(async move {
            let _sender = sender;
            std::future::pending::<()>().await;
        });
        (JoinHandle::Tokio(handle), receiver)
    }

    #[tokio::test]
    async fn aborted_or_replaced_watch_cannot_detach_or_overwrite_a_task() {
        for resource in [true, false] {
            let registry = StreamRegistry::default();
            let old = StreamBroadcaster::new();
            let current = StreamBroadcaster::new();
            let set_handle = |broadcaster: &StreamBroadcaster, handle| {
                if resource {
                    registry.set_resource_handle("key", broadcaster, handle);
                } else {
                    registry.set_event_handle("key", broadcaster, handle);
                }
            };
            let (handle, stopped) = pending_task();
            set_handle(&old, handle);
            assert!(
                stopped.await.is_err(),
                "unregistered task must be cancelled"
            );

            {
                let mut state = registry.state.lock().unwrap();
                let watches = if resource {
                    &mut state.resource_watches
                } else {
                    &mut state.event_watches
                };
                watches.insert(
                    "key".into(),
                    SharedWatch {
                        broadcaster: current.clone(),
                        handle: None,
                    },
                );
            }
            let (handle, retained) = pending_task();
            let retained_id = handle.inner().id();
            set_handle(&current, handle);
            for broadcaster in [&old, &current] {
                let (handle, stopped) = pending_task();
                set_handle(broadcaster, handle);
                assert!(
                    stopped.await.is_err(),
                    "stale and duplicate tasks must be cancelled"
                );
            }
            {
                let mut state = registry.state.lock().unwrap();
                let watches = if resource {
                    &mut state.resource_watches
                } else {
                    &mut state.event_watches
                };
                let watch = watches.remove("key").unwrap();
                let handle = watch.handle.unwrap();
                assert_eq!(handle.inner().id(), retained_id);
                handle.abort();
            }
            assert!(retained.await.is_err());
        }
    }

    fn pod_key(namespace: Option<&str>) -> WatchResourceKey {
        WatchResourceKey {
            resource_kind: crate::models::WatchResourceKind {
                kind: "Pod".to_string(),
                group: None,
                version: None,
                api_version: None,
                plural: None,
                namespaced: None,
            },
            namespace: namespace.map(ToString::to_string),
        }
    }

    #[test]
    fn namespaced_watch_key_stays_distinct_from_all_namespace_key() {
        let all = resource_watch_key("kubeconfigEnv=KUBECONFIG", "kind-dev", &pod_key(None));
        let default = resource_watch_key(
            "kubeconfigEnv=KUBECONFIG",
            "kind-dev",
            &pod_key(Some("default")),
        );

        assert_ne!(all, default);
    }

    #[test]
    fn watch_key_includes_kubeconfig_source_without_path() {
        let key = resource_watch_key("kubeconfigEnv=KUBECOVE_CONFIG", "kind-dev", &pod_key(None));

        assert!(key.starts_with("kubeconfigEnv=KUBECOVE_CONFIG|context=kind-dev"));
        assert!(!key.contains('/'));
    }
}
