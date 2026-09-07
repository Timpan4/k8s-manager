use super::{
    client_for_context,
    kinds::{api_resource_from_kind, normalize_resource_kind},
    registry::StreamBroadcaster,
};
use crate::{
    commands::ClusterLiveStore,
    models::{WatchResourceKey, WatchResourceTarget},
};
use futures_util::StreamExt;
use k8s_openapi::api::core::v1::Event;
use kube::{
    api::{Api, DynamicObject, WatchEvent, WatchParams},
    core::Status,
    Client,
};
use std::time::Duration;

fn event_action<K>(event: &WatchEvent<K>) -> String {
    match event {
        WatchEvent::Added(_) => "added",
        WatchEvent::Modified(_) => "modified",
        WatchEvent::Deleted(_) => "deleted",
        WatchEvent::Bookmark(_) => "bookmark",
        WatchEvent::Error(_) => "error",
    }
    .to_string()
}

fn watch_status_message(status: &Status) -> String {
    let reason = if status.reason.is_empty() {
        "unknown"
    } else {
        &status.reason
    };
    let message = if status.message.is_empty() {
        "Kubernetes watch returned an error"
    } else {
        &status.message
    };

    match status.code {
        0 => format!("Kubernetes watch error ({reason}): {message}"),
        code => format!("Kubernetes watch error {code} ({reason}): {message}"),
    }
}

fn reset_resource_version_for_error<K>(
    resource_version: &mut String,
    event: &WatchEvent<K>,
) -> Option<String> {
    let WatchEvent::Error(status) = event else {
        return None;
    };
    if status.code == 410 {
        *resource_version = "0".to_string();
    }
    Some(watch_status_message(status))
}

fn reset_resource_version_for_api_error(resource_version: &mut String, error: &kube::Error) {
    if matches!(error, kube::Error::Api(status) if status.code == 410) {
        *resource_version = "0".to_string();
    }
}

fn dynamic_event_target(
    cluster_context: &str,
    kind: &str,
    event: &WatchEvent<DynamicObject>,
) -> WatchResourceTarget {
    let object = match event {
        WatchEvent::Added(object) | WatchEvent::Modified(object) | WatchEvent::Deleted(object) => {
            Some(object)
        }
        WatchEvent::Bookmark(_) | WatchEvent::Error(_) => None,
    };

    WatchResourceTarget {
        cluster: cluster_context.to_string(),
        kind: kind.to_string(),
        namespace: object.and_then(|object| object.metadata.namespace.clone()),
        name: object.and_then(|object| object.metadata.name.clone()),
    }
}

fn dynamic_event_resource_version(event: &WatchEvent<DynamicObject>) -> Option<String> {
    match event {
        WatchEvent::Added(object) | WatchEvent::Modified(object) | WatchEvent::Deleted(object) => {
            object.metadata.resource_version.clone()
        }
        WatchEvent::Bookmark(bookmark) => Some(bookmark.metadata.resource_version.clone()),
        WatchEvent::Error(_) => None,
    }
}

fn event_resource_version(event: &WatchEvent<Event>) -> Option<String> {
    match event {
        WatchEvent::Added(object) | WatchEvent::Modified(object) | WatchEvent::Deleted(object) => {
            object.metadata.resource_version.clone()
        }
        WatchEvent::Bookmark(bookmark) => Some(bookmark.metadata.resource_version.clone()),
        WatchEvent::Error(_) => None,
    }
}

fn scoped_dynamic_api(
    client: Client,
    key: &WatchResourceKey,
    namespaced: bool,
    api_resource: &kube::api::ApiResource,
) -> Api<DynamicObject> {
    if !namespaced {
        return Api::all_with(client, api_resource);
    }
    match key.namespace.as_deref() {
        Some(namespace) if !namespace.is_empty() => {
            Api::namespaced_with(client, namespace, api_resource)
        }
        _ => Api::all_with(client, api_resource),
    }
}

pub(super) async fn run_resource_watch(
    source_key: String,
    cluster_context: String,
    key: WatchResourceKey,
    kubeconfig_env_var: Option<String>,
    broadcaster: StreamBroadcaster,
    live_store: ClusterLiveStore,
) {
    let normalized_kind = match normalize_resource_kind(&key.resource_kind) {
        Ok(kind) => kind,
        Err(err) => {
            broadcaster.error(err.message);
            return;
        }
    };
    let api_resource = match api_resource_from_kind(&normalized_kind) {
        Ok(api_resource) => api_resource,
        Err(err) => {
            broadcaster.error(err.message);
            return;
        }
    };
    let namespaced = normalized_kind.namespaced.unwrap_or(true);
    let kind_label = normalized_kind.kind.clone();
    let mut resource_version = "0".to_string();

    loop {
        let client = match client_for_context(&cluster_context, kubeconfig_env_var.clone()).await {
            Ok(client) => client,
            Err(err) => {
                if !broadcaster.error(err.message) {
                    return;
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        };
        let api = scoped_dynamic_api(client, &key, namespaced, &api_resource);
        let params = WatchParams::default().timeout(30);

        if !broadcaster.status("connected", format!("Watching {kind_label}")) {
            return;
        }

        match api.watch(&params, &resource_version).await {
            Ok(stream) => {
                let mut stream = stream.boxed();
                while let Some(event) = stream.next().await {
                    match event {
                        Ok(event) => {
                            if let Some(message) =
                                reset_resource_version_for_error(&mut resource_version, &event)
                            {
                                if !broadcaster.error(message) {
                                    return;
                                }
                                break;
                            }
                            if let Some(next_resource_version) =
                                dynamic_event_resource_version(&event)
                            {
                                resource_version = next_resource_version;
                            }
                            if matches!(event, WatchEvent::Bookmark(_)) {
                                continue;
                            }
                            let target =
                                dynamic_event_target(&cluster_context, &kind_label, &event);
                            live_store.mark_watch_resource_dirty(
                                &source_key,
                                &cluster_context,
                                &normalized_kind,
                                target.namespace.as_deref(),
                            );
                            if !broadcaster.resource_changed(target, event_action(&event)) {
                                return;
                            }
                        }
                        Err(err) => {
                            reset_resource_version_for_api_error(&mut resource_version, &err);
                            if !broadcaster.error(err.to_string()) {
                                return;
                            }
                            break;
                        }
                    }
                }
            }
            Err(err) => {
                reset_resource_version_for_api_error(&mut resource_version, &err);
                if !broadcaster.error(err.to_string()) {
                    return;
                }
            }
        }

        if !broadcaster.status("reconnecting", format!("Reconnecting {kind_label}")) {
            return;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

fn event_target(
    cluster_context: &str,
    kind: &str,
    name: &str,
    namespace: Option<&str>,
) -> WatchResourceTarget {
    WatchResourceTarget {
        cluster: cluster_context.to_string(),
        kind: kind.to_string(),
        namespace: namespace.map(ToString::to_string),
        name: Some(name.to_string()),
    }
}

pub(super) async fn run_event_watch(
    cluster_context: String,
    kind: String,
    name: String,
    namespace: Option<String>,
    kubeconfig_env_var: Option<String>,
    broadcaster: StreamBroadcaster,
) {
    let field_selector = match namespace.as_deref() {
        Some(ns) => format!(
            "involvedObject.kind={kind},involvedObject.name={name},involvedObject.namespace={ns}",
        ),
        None => format!("involvedObject.kind={kind},involvedObject.name={name}"),
    };
    let mut resource_version = "0".to_string();

    loop {
        let client = match client_for_context(&cluster_context, kubeconfig_env_var.clone()).await {
            Ok(client) => client,
            Err(err) => {
                if !broadcaster.error(err.message) {
                    return;
                }
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            }
        };
        let api: Api<Event> = if let Some(namespace) = &namespace {
            Api::namespaced(client, namespace)
        } else {
            Api::all(client)
        };
        let params = WatchParams::default().fields(&field_selector).timeout(30);

        if !broadcaster.status("connected", "Watching events".to_string()) {
            return;
        }

        match api.watch(&params, &resource_version).await {
            Ok(stream) => {
                let mut stream = stream.boxed();
                while let Some(event) = stream.next().await {
                    match event {
                        Ok(event) => {
                            if let Some(message) =
                                reset_resource_version_for_error(&mut resource_version, &event)
                            {
                                if !broadcaster.error(message) {
                                    return;
                                }
                                break;
                            }
                            if let Some(next_resource_version) = event_resource_version(&event) {
                                resource_version = next_resource_version;
                            }
                            if matches!(event, WatchEvent::Bookmark(_)) {
                                continue;
                            }
                            if !broadcaster.events_changed(
                                event_target(&cluster_context, &kind, &name, namespace.as_deref()),
                                event_action(&event),
                            ) {
                                return;
                            }
                        }
                        Err(err) => {
                            reset_resource_version_for_api_error(&mut resource_version, &err);
                            if !broadcaster.error(err.to_string()) {
                                return;
                            }
                            break;
                        }
                    }
                }
            }
            Err(err) => {
                reset_resource_version_for_api_error(&mut resource_version, &err);
                if !broadcaster.error(err.to_string()) {
                    return;
                }
            }
        }

        if !broadcaster.status("reconnecting", "Reconnecting events".to_string()) {
            return;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::{header::CONTENT_TYPE, Request, Response};
    use kube::{api::ApiResource, client::Body};
    use serde_json::json;

    fn watch_error_response() -> Response<Body> {
        let line = serde_json::to_vec(&json!({
            "type": "ERROR",
            "object": {
                "apiVersion": "v1",
                "kind": "Status",
                "status": "Failure",
                "reason": "Expired",
                "message": "too old resource version",
                "code": 410
            }
        }))
        .expect("watch event");
        Response::builder()
            .status(200)
            .header(CONTENT_TYPE, "application/json")
            .body(Body::from([line, vec![b'\n']].concat()))
            .expect("response")
    }

    #[test]
    fn watch_status_message_includes_api_error_context() {
        let status = Status {
            code: 410,
            reason: "Expired".to_string(),
            message: "too old resource version".to_string(),
            ..Status::default()
        };

        assert_eq!(
            watch_status_message(&status),
            "Kubernetes watch error 410 (Expired): too old resource version",
        );
    }

    #[test]
    fn non_gone_error_preserves_resource_version() {
        let event = WatchEvent::<DynamicObject>::Error(Box::new(Status {
            code: 403,
            reason: "Forbidden".to_string(),
            message: "watch is forbidden".to_string(),
            ..Status::default()
        }));
        let mut resource_version = "55".to_string();

        assert!(reset_resource_version_for_error(&mut resource_version, &event).is_some());
        assert_eq!(resource_version, "55");
    }

    #[test]
    fn http_gone_resets_version_without_matching_error_text() {
        for (code, expected) in [(410, "0"), (403, "55")] {
            let mut version = "55".to_string();
            let error = kube::Error::Api(Box::new(Status {
                code,
                message: "410 Gone text must not control recovery".into(),
                ..Status::default()
            }));
            reset_resource_version_for_api_error(&mut version, &error);
            assert_eq!(version, expected);
        }
    }

    #[tokio::test]
    async fn gone_response_resets_and_reconnects_from_zero() {
        let (service, mut handle) = tower_test::mock::pair::<Request<Body>, Response<Body>>();
        let client = Client::new(service, "default");
        let resource = ApiResource {
            group: String::new(),
            version: "v1".to_string(),
            api_version: "v1".to_string(),
            kind: "ConfigMap".to_string(),
            plural: "configmaps".to_string(),
        };
        let api: Api<DynamicObject> = Api::namespaced_with(client, "e2e-watch", &resource);
        let operation = async move {
            let mut resource_version = "55".to_string();
            for _ in 0..2 {
                let stream = api
                    .watch(&WatchParams::default().timeout(30), &resource_version)
                    .await
                    .expect("watch response");
                let event = stream
                    .boxed()
                    .next()
                    .await
                    .expect("watch item")
                    .expect("decoded event");
                assert!(reset_resource_version_for_error(&mut resource_version, &event).is_some());
            }
            resource_version
        };
        let responder = async move {
            let (first, send) = handle.next_request().await.expect("first watch");
            assert!(first
                .uri()
                .query()
                .unwrap_or_default()
                .contains("resourceVersion=55"));
            send.send_response(watch_error_response());
            let (second, send) = handle.next_request().await.expect("reconnect watch");
            assert!(second
                .uri()
                .query()
                .unwrap_or_default()
                .contains("resourceVersion=0"));
            send.send_response(watch_error_response());
        };

        let (resource_version, ()) = tokio::join!(operation, responder);

        assert_eq!(resource_version, "0");
    }
}
