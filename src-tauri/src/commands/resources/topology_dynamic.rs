use super::{
    api_resource_from_discovered, dynamic_resource_summary,
    topology::{input_from_metadata, TopologyInputResource},
};
use crate::commands::helpers::{extract_owner_ref_summary, list_params};
use crate::models::AppErrorKind;
use crate::models::{AppError, DiscoveredResourceKind};
use futures_util::{stream, StreamExt};
use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::{
    api::{Api, DynamicObject},
    Client, Error as KubeError,
};
use std::sync::Arc;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

const MAX_TOPOLOGY_LIST_CONCURRENCY: usize = 16;
const MAX_DYNAMIC_TOPOLOGY_KIND_CONCURRENCY: usize = 48;
const MAX_DYNAMIC_TOPOLOGY_LIST_CONCURRENCY: usize = 32;
type DynamicListLimiter = Arc<Semaphore>;

fn inputs_from_metadata<T>(cluster_context: &str, items: Vec<T>) -> Vec<TopologyInputResource>
where
    T: k8s_openapi::Metadata<Ty = ObjectMeta>,
{
    items
        .iter()
        .map(|item| {
            input_from_metadata(
                cluster_context,
                <T as k8s_openapi::Resource>::KIND,
                <T as k8s_openapi::Resource>::API_VERSION,
                item.metadata(),
            )
        })
        .collect()
}

fn is_optional_app_error(error: &AppError) -> bool {
    matches!(error.kind, AppErrorKind::Forbidden | AppErrorKind::NotFound)
}

fn is_optional_topology_list_error(error: &KubeError) -> bool {
    matches!(error, KubeError::Api(api_error) if matches!(api_error.code, 403 | 404))
}

fn push_dynamic_topology_list_warning(
    warnings: &mut Vec<String>,
    resource_kind: &DiscoveredResourceKind,
    namespace: Option<&str>,
    error: &AppError,
) {
    warnings.push(format!(
        "Skipped {}{} in topology: {}",
        resource_kind.kind,
        namespace.map_or_else(
            || " across namespaces".to_string(),
            |namespace| format!(" in namespace {namespace}"),
        ),
        error.message
    ));
}

async fn dynamic_list_permit(
    limiter: &DynamicListLimiter,
) -> Result<OwnedSemaphorePermit, AppError> {
    limiter.clone().acquire_owned().await.map_err(|err| {
        AppError::new(
            format!("dynamic topology limiter closed: {err}"),
            AppErrorKind::Internal,
        )
        .with_source(err)
    })
}

pub(super) async fn list_crd_definition_inputs(
    client: Client,
    cluster_context: &str,
    namespaces: &[String],
    warnings: &mut Vec<String>,
) -> Result<Vec<TopologyInputResource>, AppError> {
    let api: Api<CustomResourceDefinition> = Api::all(client);
    match api.list(&list_params()).await {
        Ok(rows) => Ok(inputs_from_metadata(cluster_context, rows.items)
            .into_iter()
            .filter(|input| {
                namespaces.is_empty()
                    || input.summary.git_ops_owner.is_some()
                    || input.summary.helm_release.is_some()
                    || input.owner.is_some()
            })
            .collect()),
        Err(error) if is_optional_topology_list_error(&error) => {
            warnings.push(format!(
                "Skipped CustomResourceDefinition across namespaces in topology: {error}"
            ));
            Ok(Vec::new())
        }
        Err(error) => Err(AppError::from(error)),
    }
}

fn dynamic_topology_input(
    cluster_context: &str,
    resource_kind: &DiscoveredResourceKind,
    object: &DynamicObject,
) -> TopologyInputResource {
    TopologyInputResource {
        uid: object.metadata.uid.clone().unwrap_or_default(),
        owner: extract_owner_ref_summary(&object.metadata),
        labels: object.metadata.labels.clone().unwrap_or_default(),
        port_hints: Vec::new(),
        summary: dynamic_resource_summary(cluster_context, resource_kind, object),
    }
}

async fn list_dynamic_topology_kind(
    client: Client,
    cluster_context: &str,
    resource_kind: &DiscoveredResourceKind,
    namespaces: &[String],
    warnings: &mut Vec<String>,
    limiter: DynamicListLimiter,
) -> Result<Vec<TopologyInputResource>, AppError> {
    let api_resource = api_resource_from_discovered(resource_kind)?;
    let mut out = Vec::new();
    if !resource_kind.namespaced {
        if !namespaces.is_empty() {
            return Ok(out);
        }
        let api: Api<DynamicObject> = Api::all_with(client, &api_resource);
        let _permit = dynamic_list_permit(&limiter).await?;
        match api.list(&list_params()).await.map_err(AppError::from) {
            Ok(rows) => out.extend(
                rows.iter()
                    .map(|object| dynamic_topology_input(cluster_context, resource_kind, object)),
            ),
            Err(error) if is_optional_app_error(&error) => {
                push_dynamic_topology_list_warning(warnings, resource_kind, None, &error);
            }
            Err(error) => return Err(error),
        }
        return Ok(out);
    }

    if namespaces.is_empty() {
        let api: Api<DynamicObject> = Api::all_with(client, &api_resource);
        let _permit = dynamic_list_permit(&limiter).await?;
        match api.list(&list_params()).await.map_err(AppError::from) {
            Ok(rows) => out.extend(
                rows.iter()
                    .map(|object| dynamic_topology_input(cluster_context, resource_kind, object)),
            ),
            Err(error) if is_optional_app_error(&error) => {
                push_dynamic_topology_list_warning(warnings, resource_kind, None, &error);
            }
            Err(error) => return Err(error),
        }
        return Ok(out);
    }

    let outcomes = stream::iter(namespaces.to_vec())
        .map(|namespace| {
            let api: Api<DynamicObject> =
                Api::namespaced_with(client.clone(), &namespace, &api_resource);
            let limiter = limiter.clone();
            async move {
                let _permit = dynamic_list_permit(&limiter).await?;
                Ok::<_, AppError>((
                    namespace,
                    api.list(&list_params())
                        .await
                        .map(|rows| rows.items)
                        .map_err(AppError::from),
                ))
            }
        })
        .buffered(MAX_TOPOLOGY_LIST_CONCURRENCY)
        .collect::<Vec<_>>()
        .await;

    for outcome in outcomes {
        let (namespace, outcome) = outcome?;
        match outcome {
            Ok(rows) => out.extend(
                rows.iter()
                    .map(|object| dynamic_topology_input(cluster_context, resource_kind, object)),
            ),
            Err(error) if is_optional_app_error(&error) => {
                push_dynamic_topology_list_warning(
                    warnings,
                    resource_kind,
                    Some(&namespace),
                    &error,
                );
            }
            Err(error) => return Err(error),
        }
    }
    Ok(out)
}

async fn dynamic_topology_kind_present(
    client: Client,
    resource_kind: &DiscoveredResourceKind,
    namespaces: &[String],
    warnings: &mut Vec<String>,
    limiter: DynamicListLimiter,
) -> Result<bool, AppError> {
    let api_resource = api_resource_from_discovered(resource_kind)?;
    let params = list_params().limit(1);
    if !resource_kind.namespaced {
        if !namespaces.is_empty() {
            return Ok(false);
        }
        let api: Api<DynamicObject> = Api::all_with(client, &api_resource);
        let _permit = dynamic_list_permit(&limiter).await?;
        return match api.list_metadata(&params).await.map_err(AppError::from) {
            Ok(list) => Ok(!list.items.is_empty()),
            Err(error) if is_optional_app_error(&error) => {
                push_dynamic_topology_list_warning(warnings, resource_kind, None, &error);
                Ok(false)
            }
            Err(error) => Err(error),
        };
    }

    if namespaces.is_empty() {
        let api: Api<DynamicObject> = Api::all_with(client, &api_resource);
        let _permit = dynamic_list_permit(&limiter).await?;
        return match api.list_metadata(&params).await.map_err(AppError::from) {
            Ok(list) => Ok(!list.items.is_empty()),
            Err(error) if is_optional_app_error(&error) => {
                push_dynamic_topology_list_warning(warnings, resource_kind, None, &error);
                Ok(false)
            }
            Err(error) => Err(error),
        };
    }

    for namespace in namespaces {
        let api: Api<DynamicObject> =
            Api::namespaced_with(client.clone(), namespace, &api_resource);
        let _permit = dynamic_list_permit(&limiter).await?;
        match api.list_metadata(&params).await.map_err(AppError::from) {
            Ok(list) if !list.items.is_empty() => return Ok(true),
            Ok(_) => {}
            Err(error) if is_optional_app_error(&error) => {
                push_dynamic_topology_list_warning(
                    warnings,
                    resource_kind,
                    Some(namespace),
                    &error,
                );
            }
            Err(error) => return Err(error),
        }
    }
    Ok(false)
}

pub(super) async fn list_dynamic_topology_inputs(
    client: Client,
    cluster_context: &str,
    namespaces: &[String],
    warnings: &mut Vec<String>,
    kinds: Vec<DiscoveredResourceKind>,
    kinds_are_present: bool,
) -> Result<Vec<TopologyInputResource>, AppError> {
    let limiter = Arc::new(Semaphore::new(MAX_DYNAMIC_TOPOLOGY_LIST_CONCURRENCY));
    let outcomes = stream::iter(kinds)
        .map(|resource_kind| {
            let client = client.clone();
            let limiter = limiter.clone();
            let namespaces = namespaces.to_vec();
            async move {
                let mut warnings = Vec::new();
                if !kinds_are_present
                    && !dynamic_topology_kind_present(
                        client.clone(),
                        &resource_kind,
                        &namespaces,
                        &mut warnings,
                        limiter.clone(),
                    )
                    .await?
                {
                    return Ok::<_, AppError>((Vec::new(), warnings));
                }
                let rows = list_dynamic_topology_kind(
                    client,
                    cluster_context,
                    &resource_kind,
                    &namespaces,
                    &mut warnings,
                    limiter,
                )
                .await?;
                Ok::<_, AppError>((rows, warnings))
            }
        })
        .buffer_unordered(MAX_DYNAMIC_TOPOLOGY_KIND_CONCURRENCY)
        .collect::<Vec<_>>()
        .await;

    let mut inputs = Vec::new();
    for outcome in outcomes {
        let (rows, mut kind_warnings) = outcome?;
        inputs.extend(rows);
        warnings.append(&mut kind_warnings);
    }
    Ok(inputs)
}
