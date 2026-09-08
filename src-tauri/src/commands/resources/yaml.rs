use crate::commands::diagnostics::record_backend_result;
use crate::commands::{
    diagnostic_field,
    helpers::{
        fetch_and_serialize_cluster_with_encoding, fetch_and_serialize_with_encoding,
        redact_secret, serialize_resource_document,
    },
    kubeconfig::KubeconfigSource,
    BackendCancellationRegistry,
};
use crate::models::AppErrorKind;
use crate::models::{AppError, YamlEncoding, YamlViewMode};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use kube::Api;
use std::time::Instant;
use tauri::State;

#[tauri::command]
pub async fn reveal_secret_data_value(
    cluster_context: String,
    name: String,
    namespace: String,
    key: String,
    kubeconfig_env_var: Option<String>,
) -> Result<String, AppError> {
    let source = KubeconfigSource::new(kubeconfig_env_var)?;
    crate::commands::helpers::validate_namespace(Some(&namespace))?;
    crate::commands::helpers::validate_path_segment(&name, "Secret name")?;
    let secret = Api::<k8s_openapi::api::core::v1::Secret>::namespaced(
        source.client_for_context(&cluster_context).await?,
        &namespace,
    )
    .get(&name)
    .await
    .map_err(AppError::from)?;
    let value = secret
        .data
        .as_ref()
        .and_then(|data| data.get(&key))
        .ok_or_else(|| AppError::new("selected Secret key was not found", AppErrorKind::Cluster))?;
    Ok(STANDARD.encode(&value.0))
}

pub async fn resource_yaml_from(
    cluster_context: String,
    kind: String,
    name: String,
    namespace: Option<String>,
    kubeconfig_env_var: Option<String>,
    yaml_view_mode: Option<YamlViewMode>,
    yaml_encoding: Option<YamlEncoding>,
) -> Result<String, AppError> {
    let source = KubeconfigSource::new(kubeconfig_env_var)?;
    let client = source.client_for_context(&cluster_context).await?;
    let mode = yaml_view_mode.unwrap_or_default();
    let encoding = yaml_encoding.unwrap_or_default();

    resource_yaml_with_client(client, kind, name, namespace, mode, encoding).await
}

pub(super) async fn resource_yaml_with_client(
    client: kube::Client,
    kind: String,
    name: String,
    namespace: Option<String>,
    mode: YamlViewMode,
    encoding: YamlEncoding,
) -> Result<String, AppError> {
    match kind.as_str() {
        "Pod" => {
            let (_pod, yaml) =
                fetch_and_serialize_with_encoding::<k8s_openapi::api::core::v1::Pod>(
                    client,
                    namespace.as_deref(),
                    &name,
                    mode,
                    encoding,
                )
                .await?;
            Ok(yaml)
        }
        "Deployment" => {
            let (_deploy, yaml) = fetch_and_serialize_with_encoding::<
                k8s_openapi::api::apps::v1::Deployment,
            >(client, namespace.as_deref(), &name, mode, encoding)
            .await?;
            Ok(yaml)
        }
        "ReplicaSet" => {
            let (_rs, yaml) = fetch_and_serialize_with_encoding::<
                k8s_openapi::api::apps::v1::ReplicaSet,
            >(client, namespace.as_deref(), &name, mode, encoding)
            .await?;
            Ok(yaml)
        }
        "Service" => {
            let (_svc, yaml) = fetch_and_serialize_with_encoding::<
                k8s_openapi::api::core::v1::Service,
            >(client, namespace.as_deref(), &name, mode, encoding)
            .await?;
            Ok(yaml)
        }
        "ConfigMap" => {
            let (_cm, yaml) = fetch_and_serialize_with_encoding::<
                k8s_openapi::api::core::v1::ConfigMap,
            >(client, namespace.as_deref(), &name, mode, encoding)
            .await?;
            Ok(yaml)
        }
        "StatefulSet" => {
            let (_ss, yaml) = fetch_and_serialize_with_encoding::<
                k8s_openapi::api::apps::v1::StatefulSet,
            >(client, namespace.as_deref(), &name, mode, encoding)
            .await?;
            Ok(yaml)
        }
        "DaemonSet" => {
            let (_ds, yaml) = fetch_and_serialize_with_encoding::<
                k8s_openapi::api::apps::v1::DaemonSet,
            >(client, namespace.as_deref(), &name, mode, encoding)
            .await?;
            Ok(yaml)
        }
        "Ingress" => {
            let (_ing, yaml) = fetch_and_serialize_with_encoding::<
                k8s_openapi::api::networking::v1::Ingress,
            >(client, namespace.as_deref(), &name, mode, encoding)
            .await?;
            Ok(yaml)
        }
        "Secret" => {
            let (mut sec, _yaml) = fetch_and_serialize_with_encoding::<
                k8s_openapi::api::core::v1::Secret,
            >(
                client, namespace.as_deref(), &name, mode, encoding
            )
            .await?;
            redact_secret(&mut sec);
            let yaml = serialize_resource_document(&sec, mode, encoding)?;
            Ok(yaml)
        }
        "PersistentVolumeClaim" => {
            let (_pvc, yaml) = fetch_and_serialize_with_encoding::<
                k8s_openapi::api::core::v1::PersistentVolumeClaim,
            >(client, namespace.as_deref(), &name, mode, encoding)
            .await?;
            Ok(yaml)
        }
        "Job" => {
            let (_job, yaml) =
                fetch_and_serialize_with_encoding::<k8s_openapi::api::batch::v1::Job>(
                    client,
                    namespace.as_deref(),
                    &name,
                    mode,
                    encoding,
                )
                .await?;
            Ok(yaml)
        }
        "CronJob" => {
            let (_cj, yaml) = fetch_and_serialize_with_encoding::<
                k8s_openapi::api::batch::v1::CronJob,
            >(client, namespace.as_deref(), &name, mode, encoding)
            .await?;
            Ok(yaml)
        }
        "Node" => {
            let (_node, yaml) = fetch_and_serialize_cluster_with_encoding::<
                k8s_openapi::api::core::v1::Node,
            >(client, &name, mode, encoding)
            .await?;
            Ok(yaml)
        }
        "StorageClass" => {
            let (_sc, yaml) = fetch_and_serialize_cluster_with_encoding::<
                k8s_openapi::api::storage::v1::StorageClass,
            >(client, &name, mode, encoding)
            .await?;
            Ok(yaml)
        }
        "PersistentVolume" => {
            let (_pv, yaml) = fetch_and_serialize_cluster_with_encoding::<
                k8s_openapi::api::core::v1::PersistentVolume,
            >(client, &name, mode, encoding)
            .await?;
            Ok(yaml)
        }
        _ => Err(AppError::new(
            format!("unsupported resource kind: {kind}"),
            AppErrorKind::Cluster,
        )),
    }
}

#[tauri::command]
pub async fn get_resource_yaml(
    cluster_context: String,
    kind: String,
    name: String,
    namespace: Option<String>,
    kubeconfig_env_var: Option<String>,
    yaml_view_mode: Option<YamlViewMode>,
    yaml_encoding: Option<YamlEncoding>,
    request_id: Option<String>,
    cancel_scope: Option<String>,
    cancellations: State<'_, BackendCancellationRegistry>,
) -> Result<String, AppError> {
    let started = Instant::now();
    let namespace_label = namespace.as_deref().unwrap_or("<cluster>");
    eprintln!(
        "[kubecove:backend] get_resource_yaml start context={cluster_context} kind={kind} namespace={namespace_label} name={name}"
    );
    let result = cancellations
        .execute(
            cancel_scope,
            request_id,
            resource_yaml_from(
                cluster_context.clone(),
                kind.clone(),
                name.clone(),
                namespace.clone(),
                kubeconfig_env_var,
                yaml_view_mode,
                yaml_encoding,
            ),
        )
        .await;
    match &result {
        Ok(yaml) => {
            eprintln!("[kubecove:backend] get_resource_yaml done context={} kind={} namespace={} name={} bytes={} ms={}", cluster_context, kind, namespace_label, name, yaml.len(), started.elapsed().as_millis());
        }
        Err(err) if err.kind == AppErrorKind::Cancelled => {
            eprintln!("[kubecove:backend] get_resource_yaml cancelled context={} kind={} namespace={} name={} ms={}", cluster_context, kind, namespace_label, name, started.elapsed().as_millis());
        }
        Err(err) => {
            eprintln!("[kubecove:backend] get_resource_yaml error context={} kind={} namespace={} name={} error_kind={} message={} ms={}", cluster_context, kind, namespace_label, name, err.kind, err.message, started.elapsed().as_millis());
        }
    }
    record_backend_result("get_resource_yaml", started, &result, |yaml| {
        vec![
            diagnostic_field("kind", &kind),
            diagnostic_field("bytes", yaml.len()),
        ]
    });
    result
}
