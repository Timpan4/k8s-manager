use super::{
    topology::{input_from_metadata, TopologyInputResource, DEPLOYMENT_REVISION_LABEL},
    topology_dynamic::{list_crd_definition_inputs, list_dynamic_topology_inputs},
    topology_network::{
        NetworkEndpointSlice, NetworkIngressBackend, NetworkService, NetworkTopologyInputs,
    },
};
use crate::commands::helpers::{
    enrich_resource_summaries_with_flux_inventory, fmt_ready, list_params,
    read_flux_ownership_index, update_resource_health,
};
use crate::models::{AppError, DiscoveredResourceKind};
use futures_util::{stream, StreamExt};
use k8s_openapi::api::{
    apps::v1::{DaemonSet, Deployment, ReplicaSet, StatefulSet},
    batch::v1::{CronJob, Job},
    core::v1::{ConfigMap, PersistentVolumeClaim, Pod, Secret, Service},
    discovery::v1::EndpointSlice,
    networking::v1::Ingress,
    storage::v1::StorageClass,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use k8s_openapi::{ClusterResourceScope, NamespaceResourceScope};
use kube::{Api, Client, Error as KubeError};

const MAX_TOPOLOGY_LIST_CONCURRENCY: usize = 16;
const DEPLOYMENT_REVISION_ANNOTATION: &str = "deployment.kubernetes.io/revision";

fn attach_deployment_revision(input: &mut TopologyInputResource, metadata: &ObjectMeta) {
    let Some(revision) = metadata
        .annotations
        .as_ref()
        .and_then(|annotations| annotations.get(DEPLOYMENT_REVISION_ANNOTATION))
    else {
        return;
    };
    input
        .labels
        .insert(DEPLOYMENT_REVISION_LABEL.to_string(), revision.clone());
}

pub(super) struct TopologyInputCollection {
    pub resources: Vec<TopologyInputResource>,
    pub warnings: Vec<String>,
}

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

fn is_optional_topology_list_error(error: &KubeError) -> bool {
    matches!(error, KubeError::Api(api_error) if matches!(api_error.code, 403 | 404))
}

fn push_topology_list_warning<T>(
    warnings: &mut Vec<String>,
    namespace: Option<&str>,
    error: &KubeError,
) where
    T: k8s_openapi::Resource,
{
    warnings.push(format!(
        "Skipped {}{} in topology: {}",
        <T as k8s_openapi::Resource>::KIND,
        namespace.map_or_else(
            || " across namespaces".to_string(),
            |namespace| format!(" in namespace {namespace}"),
        ),
        error
    ));
}

async fn list_namespaced<T>(
    client: Client,
    namespaces: &[String],
    warnings: &mut Vec<String>,
) -> Result<Vec<T>, AppError>
where
    T: Clone
        + std::fmt::Debug
        + serde::de::DeserializeOwned
        + k8s_openapi::Metadata<Ty = ObjectMeta>
        + kube::Resource<DynamicType = (), Scope = NamespaceResourceScope>
        + Send
        + Sync
        + 'static,
{
    let mut out = Vec::new();
    if namespaces.is_empty() {
        let api: Api<T> = Api::all(client);
        match api.list(&list_params()).await {
            Ok(rows) => out.extend(rows.items),
            Err(error) if is_optional_topology_list_error(&error) => {
                push_topology_list_warning::<T>(warnings, None, &error);
            }
            Err(error) => return Err(AppError::from(error)),
        }
        return Ok(out);
    }
    let outcomes = stream::iter(namespaces.to_vec())
        .map(|namespace| {
            let api: Api<T> = Api::namespaced(client.clone(), &namespace);
            async move {
                (
                    namespace,
                    api.list(&list_params()).await.map(|rows| rows.items),
                )
            }
        })
        .buffered(MAX_TOPOLOGY_LIST_CONCURRENCY)
        .collect::<Vec<_>>()
        .await;

    for (namespace, outcome) in outcomes {
        match outcome {
            Ok(rows) => out.extend(rows),
            Err(error) if is_optional_topology_list_error(&error) => {
                push_topology_list_warning::<T>(warnings, Some(&namespace), &error);
            }
            Err(error) => return Err(AppError::from(error)),
        }
    }
    Ok(out)
}

async fn list_namespaced_with_warnings<T>(
    client: Client,
    namespaces: &[String],
) -> Result<(Vec<T>, Vec<String>), AppError>
where
    T: Clone
        + std::fmt::Debug
        + serde::de::DeserializeOwned
        + k8s_openapi::Metadata<Ty = ObjectMeta>
        + kube::Resource<DynamicType = (), Scope = NamespaceResourceScope>
        + Send
        + Sync
        + 'static,
{
    let mut warnings = Vec::new();
    let rows = list_namespaced::<T>(client, namespaces, &mut warnings).await?;
    Ok((rows, warnings))
}

async fn list_cluster_with_warnings<T>(client: Client) -> Result<(Vec<T>, Vec<String>), AppError>
where
    T: Clone
        + std::fmt::Debug
        + serde::de::DeserializeOwned
        + k8s_openapi::Metadata<Ty = ObjectMeta>
        + kube::Resource<DynamicType = (), Scope = ClusterResourceScope>
        + Send
        + Sync
        + 'static,
{
    let api: Api<T> = Api::all(client);
    match api.list(&list_params()).await {
        Ok(rows) => Ok((rows.items, Vec::new())),
        Err(error) if is_optional_topology_list_error(&error) => {
            let mut warnings = Vec::new();
            push_topology_list_warning::<T>(&mut warnings, None, &error);
            Ok((Vec::new(), warnings))
        }
        Err(error) => Err(AppError::from(error)),
    }
}

pub(super) fn cluster_scoped_input_visible_in_scope(
    input: &TopologyInputResource,
    namespaces: &[String],
) -> bool {
    namespaces.is_empty()
        || input.summary.git_ops_owner.is_some()
        || input.summary.helm_release.is_some()
        || input.owner.is_some()
}

pub(super) async fn collect_topology_inputs(
    client: Client,
    cluster_context: &str,
    namespaces: &[String],
    custom_resource_kinds: Vec<DiscoveredResourceKind>,
    custom_resource_kinds_are_present: bool,
) -> Result<TopologyInputCollection, AppError> {
    let (mut workload_inputs, mut support_inputs, mut custom_inputs) = futures_util::try_join!(
        collect_workload_topology_inputs(client.clone(), cluster_context, namespaces),
        collect_support_topology_inputs(client.clone(), cluster_context, namespaces),
        collect_custom_topology_inputs(
            client.clone(),
            cluster_context,
            namespaces,
            custom_resource_kinds,
            custom_resource_kinds_are_present,
        ),
    )?;

    workload_inputs
        .resources
        .append(&mut support_inputs.resources);
    workload_inputs
        .resources
        .append(&mut custom_inputs.resources);
    workload_inputs
        .warnings
        .append(&mut support_inputs.warnings);
    workload_inputs.warnings.append(&mut custom_inputs.warnings);
    let index = read_flux_ownership_index(client, namespaces).await?;
    let mut summary_rows = workload_inputs
        .resources
        .iter()
        .map(|input| input.summary.clone())
        .collect::<Vec<_>>();
    enrich_resource_summaries_with_flux_inventory(&mut summary_rows, &index);
    for (input, summary) in workload_inputs.resources.iter_mut().zip(summary_rows) {
        input.summary = summary;
    }
    workload_inputs.resources.retain(|input| {
        input.summary.kind != "StorageClass"
            || cluster_scoped_input_visible_in_scope(input, namespaces)
    });
    Ok(workload_inputs)
}

async fn collect_workload_topology_inputs(
    client: Client,
    cluster_context: &str,
    namespaces: &[String],
) -> Result<TopologyInputCollection, AppError> {
    let (
        (deployments, mut deployment_warnings),
        (daemonsets, mut daemonset_warnings),
        (replicasets, mut replicaset_warnings),
        (statefulsets, mut statefulset_warnings),
        (cronjobs, mut cronjob_warnings),
        (jobs, mut job_warnings),
        (pods, mut pod_warnings),
    ) = futures_util::try_join!(
        list_namespaced_with_warnings::<Deployment>(client.clone(), namespaces),
        list_namespaced_with_warnings::<DaemonSet>(client.clone(), namespaces),
        list_namespaced_with_warnings::<ReplicaSet>(client.clone(), namespaces),
        list_namespaced_with_warnings::<StatefulSet>(client.clone(), namespaces),
        list_namespaced_with_warnings::<CronJob>(client.clone(), namespaces),
        list_namespaced_with_warnings::<Job>(client.clone(), namespaces),
        list_namespaced_with_warnings::<Pod>(client, namespaces),
    )?;

    let mut inputs = Vec::new();
    let mut warnings = Vec::new();
    warnings.append(&mut deployment_warnings);
    warnings.append(&mut daemonset_warnings);
    warnings.append(&mut replicaset_warnings);
    warnings.append(&mut statefulset_warnings);
    warnings.append(&mut cronjob_warnings);
    warnings.append(&mut job_warnings);
    warnings.append(&mut pod_warnings);

    for deploy in deployments {
        let mut input =
            input_from_metadata(cluster_context, "Deployment", "apps/v1", &deploy.metadata);
        attach_deployment_revision(&mut input, &deploy.metadata);
        if let Some(status) = deploy.status {
            let ready = status.ready_replicas.unwrap_or(0);
            let desired = status.replicas.unwrap_or(0);
            input.summary.ready = Some(format!("{ready}/{desired}"));
            input.summary.status = Some(format!(
                "Available: {}",
                status.available_replicas.unwrap_or(0)
            ));
        }
        update_resource_health(&mut input.summary);
        inputs.push(input);
    }

    for ds in daemonsets {
        let mut input = input_from_metadata(cluster_context, "DaemonSet", "apps/v1", &ds.metadata);
        if let Some(status) = ds.status {
            input.summary.ready = Some(format!(
                "{}/{}",
                status.number_ready, status.desired_number_scheduled
            ));
            input.summary.status = Some(format!(
                "Available: {}",
                status.number_available.unwrap_or(0)
            ));
        }
        update_resource_health(&mut input.summary);
        inputs.push(input);
    }

    for rs in replicasets {
        let mut input = input_from_metadata(cluster_context, "ReplicaSet", "apps/v1", &rs.metadata);
        attach_deployment_revision(&mut input, &rs.metadata);
        if let Some(status) = rs.status {
            input.summary.ready = Some(fmt_ready(status.ready_replicas, status.replicas));
            input.summary.status = Some(format!(
                "Available: {}",
                status.available_replicas.unwrap_or(0)
            ));
        }
        update_resource_health(&mut input.summary);
        inputs.push(input);
    }

    for ss in statefulsets {
        let mut input =
            input_from_metadata(cluster_context, "StatefulSet", "apps/v1", &ss.metadata);
        if let Some(status) = ss.status {
            input.summary.ready = Some(fmt_ready(status.ready_replicas, status.replicas));
        }
        update_resource_health(&mut input.summary);
        inputs.push(input);
    }

    for cj in cronjobs {
        let mut input = input_from_metadata(cluster_context, "CronJob", "batch/v1", &cj.metadata);
        if let Some(status) = cj.status {
            let active = status.active.as_ref().map_or(0, std::vec::Vec::len);
            if active > 0 {
                input.summary.status = Some(format!("{active} active"));
            }
        }
        update_resource_health(&mut input.summary);
        inputs.push(input);
    }

    for job in jobs {
        let mut input = input_from_metadata(cluster_context, "Job", "batch/v1", &job.metadata);
        if let Some(status) = job.status {
            let active = status.active.unwrap_or(0);
            let failed = status.failed.unwrap_or(0);
            let succeeded = status.succeeded.unwrap_or(0);
            input.summary.status = if failed > 0 {
                Some("Failed".to_string())
            } else if succeeded > 0 {
                Some("Complete".to_string())
            } else if active > 0 {
                Some("Active".to_string())
            } else {
                Some("Pending".to_string())
            };
            input.summary.ready = Some(format!(
                "{}/{}",
                succeeded,
                job.spec
                    .as_ref()
                    .and_then(|spec| spec.completions)
                    .unwrap_or(1)
            ));
        }
        update_resource_health(&mut input.summary);
        inputs.push(input);
    }

    for pod in pods {
        inputs.push(pod_input(cluster_context, pod));
    }

    Ok(TopologyInputCollection {
        resources: inputs,
        warnings,
    })
}

async fn collect_support_topology_inputs(
    client: Client,
    cluster_context: &str,
    namespaces: &[String],
) -> Result<TopologyInputCollection, AppError> {
    let (
        (pvcs, mut pvc_warnings),
        (services, mut service_warnings),
        (ingresses, mut ingress_warnings),
        (configmaps, mut configmap_warnings),
        (secrets, mut secret_warnings),
        (storageclasses, mut storageclass_warnings),
    ) = futures_util::try_join!(
        list_namespaced_with_warnings::<PersistentVolumeClaim>(client.clone(), namespaces),
        list_namespaced_with_warnings::<Service>(client.clone(), namespaces),
        list_namespaced_with_warnings::<Ingress>(client.clone(), namespaces),
        list_namespaced_with_warnings::<ConfigMap>(client.clone(), namespaces),
        list_namespaced_with_warnings::<Secret>(client.clone(), namespaces),
        list_cluster_with_warnings::<StorageClass>(client),
    )?;

    let mut inputs = Vec::new();
    let mut warnings = Vec::new();
    warnings.append(&mut pvc_warnings);
    warnings.append(&mut service_warnings);
    warnings.append(&mut ingress_warnings);
    warnings.append(&mut configmap_warnings);
    warnings.append(&mut secret_warnings);
    warnings.append(&mut storageclass_warnings);

    inputs.extend(inputs_from_metadata(cluster_context, pvcs));
    inputs.extend(inputs_from_metadata(cluster_context, services));
    inputs.extend(inputs_from_metadata(cluster_context, ingresses));
    inputs.extend(inputs_from_metadata(cluster_context, configmaps));
    inputs.extend(inputs_from_metadata(cluster_context, secrets));
    inputs.extend(inputs_from_metadata(cluster_context, storageclasses));

    Ok(TopologyInputCollection {
        resources: inputs,
        warnings,
    })
}

async fn collect_custom_topology_inputs(
    client: Client,
    cluster_context: &str,
    namespaces: &[String],
    custom_resource_kinds: Vec<DiscoveredResourceKind>,
    custom_resource_kinds_are_present: bool,
) -> Result<TopologyInputCollection, AppError> {
    let dynamic_client = client.clone();
    let definition_client = client;
    let dynamic = async {
        let mut warnings = Vec::new();
        let rows = list_dynamic_topology_inputs(
            dynamic_client,
            cluster_context,
            namespaces,
            &mut warnings,
            custom_resource_kinds,
            custom_resource_kinds_are_present,
        )
        .await?;
        Ok::<_, AppError>((rows, warnings))
    };
    let definitions = async {
        let mut warnings = Vec::new();
        let rows = list_crd_definition_inputs(
            definition_client,
            cluster_context,
            namespaces,
            &mut warnings,
        )
        .await?;
        Ok::<_, AppError>((rows, warnings))
    };
    let ((mut resources, mut warnings), (mut crds, mut crd_warnings)) =
        futures_util::try_join!(dynamic, definitions)?;
    resources.append(&mut crds);
    warnings.append(&mut crd_warnings);
    Ok(TopologyInputCollection {
        resources,
        warnings,
    })
}

fn pod_input(cluster_context: &str, pod: Pod) -> TopologyInputResource {
    let mut input = input_from_metadata(cluster_context, "Pod", "v1", &pod.metadata);
    if let Some(spec) = &pod.spec {
        input.port_hints = spec
            .containers
            .iter()
            .flat_map(|container| {
                container.ports.iter().flatten().map(|port| {
                    let number = port.container_port;
                    let name = port.name.as_deref();
                    match name {
                        Some(name) if !name.is_empty() => format!("{name}:{number}"),
                        _ => number.to_string(),
                    }
                })
            })
            .collect();
        input.port_hints.sort();
        input.port_hints.dedup();
    }
    if let Some(status) = pod.status {
        input.summary.status = status.phase.filter(|phase| !phase.is_empty());
        input.summary.ready = status
            .conditions
            .as_ref()
            .and_then(|conds| conds.iter().find(|condition| condition.type_ == "Ready"))
            .map(|condition| condition.status.clone());
        input.summary.restarts =
            crate::commands::helpers::pod_restarts(status.container_statuses.as_deref());
    }
    update_resource_health(&mut input.summary);
    input
}

pub(super) async fn collect_network_topology_inputs(
    client: Client,
    cluster_context: &str,
    namespaces: &[String],
) -> Result<NetworkTopologyInputs, AppError> {
    let (
        (pods, mut pod_warnings),
        (services, mut service_warnings),
        (ingresses, mut ingress_warnings),
        (endpoint_slices, mut endpoint_slice_warnings),
    ) = futures_util::try_join!(
        list_namespaced_with_warnings::<Pod>(client.clone(), namespaces),
        list_namespaced_with_warnings::<Service>(client.clone(), namespaces),
        list_namespaced_with_warnings::<Ingress>(client.clone(), namespaces),
        list_namespaced_with_warnings::<EndpointSlice>(client, namespaces),
    )?;

    let mut warnings = Vec::new();
    warnings.append(&mut pod_warnings);
    warnings.append(&mut service_warnings);
    warnings.append(&mut ingress_warnings);
    warnings.append(&mut endpoint_slice_warnings);
    let mut resources = Vec::new();
    let mut service_flows = Vec::new();
    let mut ingress_backends = Vec::new();
    let mut endpoint_slice_flows = Vec::new();

    for pod in pods {
        resources.push(pod_input(cluster_context, pod));
    }
    for service in services {
        let mut input = input_from_metadata(cluster_context, "Service", "v1", &service.metadata);
        let namespace = service.metadata.namespace.clone().unwrap_or_default();
        let name = service.metadata.name.clone().unwrap_or_default();
        let selector = service
            .spec
            .as_ref()
            .and_then(|spec| spec.selector.clone())
            .unwrap_or_default();
        let service_type = service
            .spec
            .as_ref()
            .and_then(|spec| spec.type_.clone())
            .unwrap_or_else(|| "ClusterIP".to_string());
        if let Some(spec) = &service.spec {
            input.summary.status = Some(service_type.clone());
            input.port_hints = spec
                .ports
                .iter()
                .flatten()
                .map(|port| service_port_hint(port.name.as_deref(), port.port))
                .collect();
            input.port_hints.sort();
            input.port_hints.dedup();
        }
        update_resource_health(&mut input.summary);
        service_flows.push(NetworkService {
            namespace,
            name,
            service_type,
            selector,
        });
        resources.push(input);
    }
    for ingress in ingresses {
        let namespace = ingress.metadata.namespace.clone().unwrap_or_default();
        let ingress_name = ingress.metadata.name.clone().unwrap_or_default();
        if let Some(spec) = &ingress.spec {
            if let Some(default_backend) = &spec.default_backend {
                if let Some(service_name) = backend_service_name(default_backend) {
                    ingress_backends.push(NetworkIngressBackend {
                        namespace: namespace.clone(),
                        ingress_name: ingress_name.clone(),
                        service_name,
                    });
                }
            }
            for rule in spec.rules.iter().flatten() {
                let Some(http) = &rule.http else {
                    continue;
                };
                for path in &http.paths {
                    if let Some(service_name) = backend_service_name(&path.backend) {
                        ingress_backends.push(NetworkIngressBackend {
                            namespace: namespace.clone(),
                            ingress_name: ingress_name.clone(),
                            service_name,
                        });
                    }
                }
            }
        }
        resources.push(input_from_metadata(
            cluster_context,
            "Ingress",
            "networking.k8s.io/v1",
            &ingress.metadata,
        ));
    }
    for slice in endpoint_slices {
        let namespace = slice.metadata.namespace.clone().unwrap_or_default();
        let name = slice.metadata.name.clone().unwrap_or_default();
        let service_name = slice
            .metadata
            .labels
            .as_ref()
            .and_then(|labels| labels.get("kubernetes.io/service-name").cloned());
        let target_pods: Vec<String> = slice
            .endpoints
            .iter()
            .filter_map(|endpoint| {
                let target = endpoint.target_ref.as_ref()?;
                (target.kind.as_deref() == Some("Pod")).then(|| target.name.clone())?
            })
            .collect();
        let mut input = input_from_metadata(
            cluster_context,
            "EndpointSlice",
            "discovery.k8s.io/v1",
            &slice.metadata,
        );
        input.port_hints = slice
            .ports
            .iter()
            .flatten()
            .filter_map(|port| {
                port.port
                    .map(|number| service_port_hint(port.name.as_deref(), number))
            })
            .collect();
        input.port_hints.sort();
        input.port_hints.dedup();
        endpoint_slice_flows.push(NetworkEndpointSlice {
            namespace,
            name,
            service_name,
            target_pods,
        });
        resources.push(input);
    }

    Ok(NetworkTopologyInputs {
        resources,
        services: service_flows,
        ingress_backends,
        endpoint_slices: endpoint_slice_flows,
        warnings,
    })
}

fn backend_service_name(
    backend: &k8s_openapi::api::networking::v1::IngressBackend,
) -> Option<String> {
    backend.service.as_ref().map(|service| service.name.clone())
}

fn service_port_hint(name: Option<&str>, port: i32) -> String {
    match name {
        Some(name) if !name.is_empty() => format!("{name}:{port}"),
        _ => port.to_string(),
    }
}
