use crate::models::AppErrorKind;
use crate::{
    commands::helpers::list_params,
    commands::{
        diagnostic_field,
        kubeconfig::{kubeconfig_source_key, KubeconfigSource},
        record_backend_error, record_backend_success, ClusterLiveStore,
    },
    models::{AppError, DiscoveredResourceKind},
};
use futures_util::{stream, StreamExt};
use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::{
    CustomResourceDefinition, CustomResourceDefinitionVersion,
};
use kube::{
    api::{Api, ApiResource, DynamicObject},
    Client,
};
use std::time::Instant;
use tauri::State;

const MAX_PRESENT_KIND_CONCURRENCY: usize = 48;

pub async fn resource_kinds_from(
    cluster_context: String,
    kubeconfig_env_var: Option<String>,
) -> Result<Vec<DiscoveredResourceKind>, AppError> {
    let source = KubeconfigSource::new(kubeconfig_env_var)?;
    let client = source.client_for_context(&cluster_context).await?;
    crd_resource_kinds(client).await
}

pub(crate) async fn crd_resource_kinds(
    client: Client,
) -> Result<Vec<DiscoveredResourceKind>, AppError> {
    let api: Api<CustomResourceDefinition> = Api::all(client);
    let mut kinds: Vec<_> = api
        .list(&list_params())
        .await
        .map_err(AppError::from)?
        .items
        .into_iter()
        .filter_map(|crd| discovered_kind_from_crd(&crd))
        .collect();

    sort_and_dedup_kinds(&mut kinds);
    Ok(kinds)
}

fn discovered_kind_from_crd(crd: &CustomResourceDefinition) -> Option<DiscoveredResourceKind> {
    let version = preferred_crd_version(&crd.spec.versions)?;
    let group = crd.spec.group.clone();
    let api_version = format!("{group}/{}", version.name);
    Some(DiscoveredResourceKind {
        group,
        version: version.name.clone(),
        api_version,
        kind: crd.spec.names.kind.clone(),
        plural: crd.spec.names.plural.clone(),
        short_names: crd.spec.names.short_names.clone().unwrap_or_default(),
        namespaced: crd.spec.scope == "Namespaced",
    })
}

fn preferred_crd_version(
    versions: &[CustomResourceDefinitionVersion],
) -> Option<&CustomResourceDefinitionVersion> {
    versions
        .iter()
        .find(|version| version.served && version.storage)
        .or_else(|| versions.iter().find(|version| version.served))
}

fn api_resource_from_kind(resource_kind: &DiscoveredResourceKind) -> ApiResource {
    ApiResource {
        group: resource_kind.group.clone(),
        version: resource_kind.version.clone(),
        api_version: resource_kind.api_version.clone(),
        kind: resource_kind.kind.clone(),
        plural: resource_kind.plural.clone(),
    }
}

fn should_probe_present_kind(
    resource_kind: &DiscoveredResourceKind,
    namespaces: &[String],
) -> bool {
    resource_kind.namespaced || namespaces.is_empty()
}

pub(crate) fn sort_and_dedup_kinds(kinds: &mut Vec<DiscoveredResourceKind>) {
    kinds.sort_by(|a, b| {
        a.group
            .cmp(&b.group)
            .then(a.kind.cmp(&b.kind))
            .then(a.version.cmp(&b.version))
            .then(a.plural.cmp(&b.plural))
    });
    kinds.dedup_by(|a, b| {
        a.group == b.group && a.version == b.version && a.kind == b.kind && a.plural == b.plural
    });
}

#[tauri::command]
pub async fn list_resource_kinds(
    cluster_context: String,
    kubeconfig_env_var: Option<String>,
    live_store: State<'_, ClusterLiveStore>,
) -> Result<Vec<DiscoveredResourceKind>, AppError> {
    let started = Instant::now();
    eprintln!("[kubecove:backend] list_resource_kinds start context={cluster_context}");
    let source_key = kubeconfig_source_key(kubeconfig_env_var.as_deref())?;
    let result = live_store
        .resource_kinds(source_key, cluster_context.clone(), {
            let cluster_context = cluster_context.clone();
            let kubeconfig_env_var = kubeconfig_env_var.clone();
            move || resource_kinds_from(cluster_context, kubeconfig_env_var)
        })
        .await;
    match &result {
        Ok(rows) => {
            eprintln!(
                "[kubecove:backend] list_resource_kinds done context={} rows={} ms={}",
                cluster_context,
                rows.len(),
                started.elapsed().as_millis()
            );
            record_backend_success(
                "list_resource_kinds",
                started,
                vec![diagnostic_field("rows", rows.len())],
            );
        }
        Err(err) => {
            eprintln!(
                "[kubecove:backend] list_resource_kinds error context={} kind={} message={} ms={}",
                cluster_context,
                err.kind,
                err.message,
                started.elapsed().as_millis()
            );
            record_backend_error("list_resource_kinds", started, err.kind.as_str());
        }
    }
    result
}

pub async fn present_custom_resource_kinds_from(
    cluster_context: String,
    namespaces: Vec<String>,
    kubeconfig_env_var: Option<String>,
) -> Result<Vec<DiscoveredResourceKind>, AppError> {
    let source = KubeconfigSource::new(kubeconfig_env_var)?;
    let client = source.client_for_context(&cluster_context).await?;
    let catalog = crd_resource_kinds(client.clone()).await?;
    present_custom_resource_kinds_for_catalog(client, catalog, namespaces).await
}

pub(crate) async fn present_custom_resource_kinds_for_catalog(
    client: Client,
    catalog: Vec<DiscoveredResourceKind>,
    namespaces: Vec<String>,
) -> Result<Vec<DiscoveredResourceKind>, AppError> {
    let namespaces: Vec<_> = namespaces
        .into_iter()
        .filter(|namespace| !namespace.trim().is_empty())
        .collect();

    for namespace in &namespaces {
        crate::commands::helpers::validate_namespace(Some(namespace))?;
    }

    let outcomes = stream::iter(
        catalog
            .into_iter()
            .filter(|kind| should_probe_present_kind(kind, &namespaces)),
    )
    .map(|resource_kind| {
        let client = client.clone();
        let namespaces = namespaces.clone();
        async move {
            match custom_resource_kind_present(client, &resource_kind, &namespaces).await {
                Ok(true) => Ok(Some(resource_kind)),
                Ok(false) => Ok(None),
                Err(err) if err.kind == AppErrorKind::NotFound => Ok(None),
                Err(err) if err.kind == AppErrorKind::Forbidden && !namespaces.is_empty() => {
                    Ok(None)
                }
                Err(err) => Err(err),
            }
        }
    })
    .buffer_unordered(MAX_PRESENT_KIND_CONCURRENCY)
    .collect::<Vec<Result<Option<DiscoveredResourceKind>, AppError>>>()
    .await;

    let mut present = Vec::new();
    for outcome in outcomes {
        if let Some(kind) = outcome? {
            present.push(kind);
        }
    }
    sort_and_dedup_kinds(&mut present);
    Ok(present)
}

async fn custom_resource_kind_present(
    client: Client,
    resource_kind: &DiscoveredResourceKind,
    namespaces: &[String],
) -> Result<bool, AppError> {
    let api_resource = api_resource_from_kind(resource_kind);
    crate::commands::helpers::validate_api_resource(&api_resource)?;
    let params = list_params().limit(1);
    if namespaces.is_empty() {
        let api: Api<DynamicObject> = Api::all_with(client, &api_resource);
        return api
            .list_metadata(&params)
            .await
            .map(|list| !list.items.is_empty())
            .map_err(AppError::from);
    }

    for namespace in namespaces {
        let api: Api<DynamicObject> =
            Api::namespaced_with(client.clone(), namespace, &api_resource);
        match api.list_metadata(&params).await {
            Ok(list) if !list.items.is_empty() => return Ok(true),
            Ok(_) => {}
            Err(err) => {
                let app_error = AppError::from(err);
                if app_error.kind == AppErrorKind::Forbidden
                    || app_error.kind == AppErrorKind::NotFound
                {
                    continue;
                }
                return Err(app_error);
            }
        }
    }
    Ok(false)
}

#[tauri::command]
pub async fn list_present_custom_resource_kinds(
    cluster_context: String,
    namespaces: Vec<String>,
    kubeconfig_env_var: Option<String>,
    live_store: State<'_, ClusterLiveStore>,
) -> Result<Vec<DiscoveredResourceKind>, AppError> {
    let started = Instant::now();
    eprintln!(
        "[kubecove:backend] list_present_custom_resource_kinds start context={cluster_context} namespaces={}",
        namespaces.len()
    );
    let source_key = kubeconfig_source_key(kubeconfig_env_var.as_deref())?;
    let store = live_store.inner().clone();
    let result = live_store
        .present_custom_resource_kinds(
            source_key.clone(),
            cluster_context.clone(),
            namespaces.clone(),
            {
                let cluster_context = cluster_context.clone();
                let kubeconfig_env_var = kubeconfig_env_var.clone();
                let namespaces = namespaces.clone();
                move || {
                    let source_key = source_key.clone();
                    let store = store.clone();
                    async move {
                        let source = KubeconfigSource::new(kubeconfig_env_var)?;
                        let client = source.client_for_context(&cluster_context).await?;
                        let catalog_client = client.clone();
                        let catalog = store
                            .resource_kinds(source_key, cluster_context.clone(), move || {
                                crd_resource_kinds(catalog_client)
                            })
                            .await?;
                        present_custom_resource_kinds_for_catalog(client, catalog, namespaces).await
                    }
                }
            },
        )
        .await;
    match &result {
        Ok(rows) => {
            eprintln!(
                "[kubecove:backend] list_present_custom_resource_kinds done context={} rows={} ms={}",
                cluster_context,
                rows.len(),
                started.elapsed().as_millis()
            );
            record_backend_success(
                "list_present_custom_resource_kinds",
                started,
                vec![diagnostic_field("rows", rows.len())],
            );
        }
        Err(err) => {
            eprintln!(
                "[kubecove:backend] list_present_custom_resource_kinds error context={} kind={} message={} ms={}",
                cluster_context,
                err.kind,
                err.message,
                started.elapsed().as_millis()
            );
            record_backend_error(
                "list_present_custom_resource_kinds",
                started,
                err.kind.as_str(),
            );
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::{header::CONTENT_TYPE, Request, Response};
    use kube::{client::Body, Client};
    use serde_json::json;

    type MockHandle = tower_test::mock::Handle<Request<Body>, Response<Body>>;

    fn mock_client() -> (Client, MockHandle) {
        let (service, handle) = tower_test::mock::pair::<Request<Body>, Response<Body>>();
        (Client::new(service, "default"), handle)
    }

    fn response(status: u16, body: impl Into<Body>) -> Response<Body> {
        Response::builder()
            .status(status)
            .header(CONTENT_TYPE, "application/json")
            .body(body.into())
            .expect("response")
    }

    fn crd(
        name: &str,
        group: &str,
        kind: &str,
        plural: &str,
        scope: &str,
    ) -> CustomResourceDefinition {
        CustomResourceDefinition {
            metadata: k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta::default(),
            spec: k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinitionSpec {
                group: group.to_string(),
                names: k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinitionNames {
                    kind: kind.to_string(),
                    plural: plural.to_string(),
                    ..Default::default()
                },
                scope: scope.to_string(),
                versions: vec![
                    CustomResourceDefinitionVersion {
                        name: "v1beta1".to_string(),
                        served: true,
                        storage: false,
                        ..Default::default()
                    },
                    CustomResourceDefinitionVersion {
                        name: name.to_string(),
                        served: true,
                        storage: true,
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
            status: None,
        }
    }

    #[test]
    fn maps_crd_to_storage_version_kind() {
        let mut source = crd("v1", "example.com", "Widget", "widgets", "Namespaced");
        source.spec.names.short_names = Some(vec!["wdg".to_string()]);
        let kind = discovered_kind_from_crd(&source).expect("kind");

        assert_eq!(kind.api_version, "example.com/v1");
        assert_eq!(kind.kind, "Widget");
        assert_eq!(kind.plural, "widgets");
        assert_eq!(kind.short_names, ["wdg"]);
        assert!(kind.namespaced);
    }

    #[test]
    fn falls_back_to_first_served_version() {
        let mut source = crd("v1", "example.com", "Widget", "widgets", "Cluster");
        source.spec.versions[1].served = false;

        let kind = discovered_kind_from_crd(&source).expect("kind");

        assert_eq!(kind.version, "v1beta1");
        assert!(!kind.namespaced);
    }

    #[test]
    fn ignores_crd_with_no_served_version() {
        let mut source = crd("v1", "example.com", "Widget", "widgets", "Namespaced");
        for version in &mut source.spec.versions {
            version.served = false;
        }

        assert!(discovered_kind_from_crd(&source).is_none());
    }

    #[test]
    fn sorts_and_dedups_kinds() {
        let mut kinds = vec![
            discovered_kind_from_crd(&crd("v1", "z.example", "Zed", "zeds", "Namespaced")).unwrap(),
            discovered_kind_from_crd(&crd("v1", "a.example", "Alpha", "alphas", "Namespaced"))
                .unwrap(),
            discovered_kind_from_crd(&crd("v1", "a.example", "Alpha", "alphas", "Namespaced"))
                .unwrap(),
        ];

        sort_and_dedup_kinds(&mut kinds);

        assert_eq!(kinds.len(), 2);
        assert_eq!(kinds[0].kind, "Alpha");
        assert_eq!(kinds[1].kind, "Zed");
    }

    #[test]
    fn keeps_cluster_scoped_kinds_for_all_namespace_presence() {
        let cluster_kind = discovered_kind_from_crd(&crd(
            "v1",
            "example.com",
            "ClusterThing",
            "clusterthings",
            "Cluster",
        ))
        .unwrap();
        let namespaced_kind =
            discovered_kind_from_crd(&crd("v1", "example.com", "Widget", "widgets", "Namespaced"))
                .unwrap();

        assert!(should_probe_present_kind(&cluster_kind, &[]));
        assert!(should_probe_present_kind(&namespaced_kind, &[]));
        assert!(!should_probe_present_kind(
            &cluster_kind,
            &[String::from("default")]
        ));
        assert!(should_probe_present_kind(
            &namespaced_kind,
            &[String::from("default")]
        ));
    }

    #[tokio::test]
    async fn malformed_discovery_response_is_reported() {
        let (client, mut handle) = mock_client();
        let operation = crd_resource_kinds(client);
        let responder = async move {
            let (_, send) = handle.next_request().await.expect("CRD list request");
            send.send_response(response(200, Body::from(b"{not-json".to_vec())));
        };

        let (result, ()) = tokio::join!(operation, responder);
        let error = result.expect_err("malformed response must fail");

        assert_eq!(error.kind, AppErrorKind::Serialization);
    }

    #[tokio::test]
    async fn forbidden_discovery_response_preserves_permission_kind() {
        let (client, mut handle) = mock_client();
        let operation = crd_resource_kinds(client);
        let responder = async move {
            let (_, send) = handle.next_request().await.expect("CRD list request");
            send.send_response(response(
                403,
                Body::from(
                    serde_json::to_vec(&json!({
                        "apiVersion": "v1",
                        "kind": "Status",
                        "status": "Failure",
                        "reason": "Forbidden",
                        "message": "customresourcedefinitions is forbidden",
                        "code": 403
                    }))
                    .expect("status JSON"),
                ),
            ));
        };

        let (result, ()) = tokio::join!(operation, responder);
        let error = result.expect_err("forbidden response must fail");

        assert_eq!(error.kind, AppErrorKind::Forbidden);
        assert!(error.message.contains("forbidden"));
    }
}
