use crate::models::AppErrorKind;
mod cluster;
mod core;
mod workloads;

use crate::commands::diagnostics::record_backend_result;
use crate::commands::{
    diagnostic_field, kubeconfig::KubeconfigSource, BackendCancellationRegistry,
};
use crate::models::{AppError, ResourceDetailsFull, YamlEncoding, YamlViewMode};
use std::time::Instant;
use tauri::State;

#[cfg(test)]
mod tests {
    use super::*;
    use http::{Request, Response};
    use kube::{client::Body, Client};
    use serde_json::json;

    #[tokio::test]
    async fn replicaset_details_load_from_namespaced_api() {
        for status in [
            Some(json!({"replicas": 3, "readyReplicas": 2, "availableReplicas": 1})),
            None,
        ] {
            let object = json!({
                "apiVersion": "apps/v1",
                "kind": "ReplicaSet",
                "metadata": {
                    "name": "calagopus-5f554d575f",
                    "namespace": "calagopus",
                    "ownerReferences": [{
                        "apiVersion": "apps/v1", "kind": "Deployment",
                        "name": "calagopus", "uid": "deployment-uid", "controller": true
                    }]
                },
                "status": status
            });
            let service = tower::service_fn(move |request: Request<Body>| {
                assert_eq!(request.method(), http::Method::GET);
                assert_eq!(
                    request.uri().path(),
                    "/apis/apps/v1/namespaces/calagopus/replicasets/calagopus-5f554d575f"
                );
                let body = object.to_string().into_bytes();
                async move { Ok::<_, std::convert::Infallible>(Response::new(Body::from(body))) }
            });
            let client = Client::new(service, "default");
            let details = resource_details_with_client(
                client.clone(),
                "test-cluster".into(),
                "ReplicaSet".into(),
                "calagopus-5f554d575f".into(),
                Some("calagopus".into()),
            )
            .await
            .expect("ReplicaSet details should load");
            assert_eq!(details.summary.kind, "ReplicaSet");
            assert_eq!(details.summary.cluster, "test-cluster");
            assert_eq!(details.summary.name, "calagopus-5f554d575f");
            assert_eq!(details.summary.namespace.as_deref(), Some("calagopus"));
            assert_eq!(details.summary.owner_ref.as_deref(), Some("calagopus"));
            assert_eq!(
                details.summary.ready.as_deref(),
                status.as_ref().map(|_| "2/3")
            );
            assert_eq!(
                details.summary.status.as_deref(),
                status.as_ref().map(|_| "Available: 1")
            );
            assert_eq!(details.status, status);
            assert_eq!(details.metadata["name"], "calagopus-5f554d575f");
            let yaml: serde_json::Value = serde_yaml::from_str(&details.yaml).unwrap();
            assert_eq!(yaml["kind"], "ReplicaSet");
            let document = super::super::yaml::resource_yaml_with_client(
                client,
                "ReplicaSet".into(),
                "calagopus-5f554d575f".into(),
                Some("calagopus".into()),
                YamlViewMode::default(),
                YamlEncoding::default(),
            )
            .await
            .expect("ReplicaSet YAML should load");
            let document: serde_json::Value = serde_yaml::from_str(&document).unwrap();
            assert_eq!(document, yaml);
        }
    }
}

pub async fn resource_details_from(
    cluster_context: String,
    kind: String,
    name: String,
    namespace: Option<String>,
    kubeconfig_env_var: Option<String>,
) -> Result<ResourceDetailsFull, AppError> {
    let source = KubeconfigSource::new(kubeconfig_env_var)?;
    let client = source.client_for_context(&cluster_context).await?;

    resource_details_with_client(client, cluster_context, kind, name, namespace).await
}

async fn resource_details_with_client(
    client: kube::Client,
    cluster_context: String,
    kind: String,
    name: String,
    namespace: Option<String>,
) -> Result<ResourceDetailsFull, AppError> {
    match kind.as_str() {
        "Pod" => core::pod_details(client, cluster_context, name, namespace).await,
        "Service" => core::service_details(client, cluster_context, name, namespace).await,
        "ConfigMap" => core::configmap_details(client, cluster_context, name, namespace).await,
        "Secret" => core::secret_details(client, cluster_context, name, namespace).await,
        "PersistentVolumeClaim" => {
            core::pvc_details(client, cluster_context, name, namespace).await
        }
        "Deployment" => {
            workloads::deployment_details(client, cluster_context, name, namespace).await
        }
        "ReplicaSet" => {
            workloads::replicaset_details(client, cluster_context, name, namespace).await
        }
        "StatefulSet" => {
            workloads::statefulset_details(client, cluster_context, name, namespace).await
        }
        "DaemonSet" => workloads::daemonset_details(client, cluster_context, name, namespace).await,
        "Ingress" => workloads::ingress_details(client, cluster_context, name, namespace).await,
        "Job" => workloads::job_details(client, cluster_context, name, namespace).await,
        "CronJob" => workloads::cronjob_details(client, cluster_context, name, namespace).await,
        "Node" => cluster::node_details(client, cluster_context, name, namespace).await,
        "StorageClass" => {
            cluster::storageclass_details(client, cluster_context, name, namespace).await
        }
        "PersistentVolume" => cluster::pv_details(client, cluster_context, name, namespace).await,
        "CustomResourceDefinition" => {
            cluster::crd_details(client, cluster_context, name, namespace).await
        }
        _ => Err(AppError::new(
            format!("unsupported resource kind: {kind}"),
            AppErrorKind::Cluster,
        )),
    }
}

#[tauri::command]
pub async fn get_resource_details(
    cluster_context: String,
    kind: String,
    name: String,
    namespace: Option<String>,
    kubeconfig_env_var: Option<String>,
    _yaml_view_mode: Option<YamlViewMode>,
    _yaml_encoding: Option<YamlEncoding>,
    request_id: Option<String>,
    cancel_scope: Option<String>,
    cancellations: State<'_, BackendCancellationRegistry>,
) -> Result<ResourceDetailsFull, AppError> {
    let started = Instant::now();
    let namespace_label = namespace.as_deref().unwrap_or("<cluster>");
    eprintln!(
        "[kubecove:backend] get_resource_details start context={cluster_context} kind={kind} namespace={namespace_label} name={name}"
    );
    let result = cancellations
        .execute(
            cancel_scope,
            request_id,
            resource_details_from(
                cluster_context.clone(),
                kind.clone(),
                name.clone(),
                namespace.clone(),
                kubeconfig_env_var,
            ),
        )
        .await;
    match &result {
        Ok(details) => {
            eprintln!("[kubecove:backend] get_resource_details done context={} kind={} namespace={} name={} yaml_bytes={} status={} ms={}", cluster_context, kind, namespace_label, name, details.yaml.len(), details.status.is_some(), started.elapsed().as_millis());
        }
        Err(err) if err.kind == AppErrorKind::Cancelled => {
            eprintln!("[kubecove:backend] get_resource_details cancelled context={} kind={} namespace={} name={} ms={}", cluster_context, kind, namespace_label, name, started.elapsed().as_millis());
        }
        Err(err) => {
            eprintln!("[kubecove:backend] get_resource_details error context={} kind={} namespace={} name={} error_kind={} message={} ms={}", cluster_context, kind, namespace_label, name, err.kind, err.message, started.elapsed().as_millis());
        }
    }
    record_backend_result("get_resource_details", started, &result, |details| {
        vec![
            diagnostic_field("kind", &kind),
            diagnostic_field("yamlBytes", details.yaml.len()),
            diagnostic_field("hasStatus", details.status.is_some()),
        ]
    });
    result
}
