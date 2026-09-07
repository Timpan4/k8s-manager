pub(crate) use super::transport::{api_delete, api_get, api_post, redact_secret_fields};
use super::transport::{
    argo_url, http_client, normalize_endpoint, response_json, safe_http_error, url,
};
use super::{scope::scoped_connection, tunnel::ArgoServiceTunnel};
use crate::commands::{
    gitops_crd::{client_for_context, find_api_resource, get_crd_object},
    kubeconfig::KubeconfigSource,
    BackendCancellationRegistry,
};
use crate::models::AppErrorKind;
use crate::models::{
    AppError, ArgoApplicationHistory, ArgoApplicationInspector, ArgoApplicationRef,
    ArgoConnectionProfile, ArgoConnectionStatus, ArgoManagedResource, ArgoResourceComparison,
    ArgoServerCapability, ArgoServerEndpoint, ArgoServiceTunnelUnavailableReason,
};
use k8s_openapi::api::core::v1::Service;
use kube::{
    api::{Api, ListParams},
    core::DynamicObject,
};
use reqwest::Client as HttpClient;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
};
use tokio::sync::Mutex as AsyncMutex;
use uuid::Uuid;

const ARGO_SERVICE_NAMES: [&str; 2] = ["argocd-server", "argo-cd-argocd-server"];

#[derive(Default)]
pub struct ArgoConnectionStore {
    pub(crate) connections: Mutex<HashMap<String, Arc<ConnectedArgo>>>,
    pub(crate) sessions: super::session::SessionStore,
    pub(crate) cleanup_epoch: AtomicU64,
}

pub(crate) struct ConnectedArgo {
    pub(crate) profile: ArgoConnectionProfile,
    pub(crate) token: String,
    pub(crate) username: Option<String>,
    pub(crate) client: HttpClient,
    pub(crate) tunnel: Option<ArgoServiceTunnel>,
    pub(crate) generation: String,
    pub(crate) instance_id: String,
    pub(crate) gate: Arc<AsyncMutex<()>>,
}

impl ConnectedArgo {
    fn close_tunnel(&self) {
        if let Some(tunnel) = &self.tunnel {
            tunnel.close();
        }
    }
}

pub(crate) fn kubeconfig_source_key(value: Option<&str>) -> Result<String, AppError> {
    if let Some(key) = value.filter(|value| value.starts_with("kubeconfigSource=")) {
        return Ok(key.to_string());
    }
    Ok(KubeconfigSource::new(value.map(str::to_string))?.key())
}

fn cleanup_error() -> AppError {
    AppError::new(
        "Argo CD connection was cancelled by workspace cleanup",
        AppErrorKind::ArgoConnection,
    )
}

impl ArgoConnectionStore {
    pub(crate) fn connection_epoch(&self) -> u64 {
        self.cleanup_epoch.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) async fn replace_connection(
        &self,
        id: String,
        connection: Arc<ConnectedArgo>,
    ) -> Result<(), AppError> {
        self.replace_connection_at(id, connection, self.connection_epoch())
            .await
    }

    pub(crate) async fn replace_connection_at(
        &self,
        id: String,
        connection: Arc<ConnectedArgo>,
        expected_epoch: u64,
    ) -> Result<(), AppError> {
        loop {
            let old = self
                .connections
                .lock()
                .map_err(|_| {
                    AppError::new(
                        "Argo CD connection state unavailable",
                        AppErrorKind::ArgoConnection,
                    )
                })?
                .get(&id)
                .cloned();
            let Some(old) = old else {
                let mut connections = self.connections.lock().map_err(|_| {
                    AppError::new(
                        "Argo CD connection state unavailable",
                        AppErrorKind::ArgoConnection,
                    )
                })?;
                if self.connection_epoch() != expected_epoch {
                    return Err(cleanup_error());
                }
                if connections.contains_key(&id) {
                    continue;
                }
                connections.insert(id.clone(), connection);
                return Ok(());
            };
            let _lease = old.gate.clone().lock_owned().await;
            let mut connections = self.connections.lock().map_err(|_| {
                AppError::new(
                    "Argo CD connection state unavailable",
                    AppErrorKind::ArgoConnection,
                )
            })?;
            if self.connection_epoch() != expected_epoch {
                return Err(cleanup_error());
            }
            if connections
                .get(&id)
                .is_some_and(|current| Arc::ptr_eq(current, &old))
            {
                connections.insert(id.clone(), connection);
                drop(connections);
                old.close_tunnel();
                return Ok(());
            }
        }
    }

    pub(crate) async fn close_all(&self) -> Result<(), AppError> {
        self.cleanup_epoch.fetch_add(1, Ordering::AcqRel);
        let ids = self
            .connections
            .lock()
            .map_err(|_| {
                AppError::new(
                    "Argo CD connection state unavailable",
                    AppErrorKind::ArgoConnection,
                )
            })?
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for id in ids {
            self.remove_connection(&id).await?;
        }
        Ok(())
    }

    pub(crate) async fn remove_connection(&self, id: &str) -> Result<(), AppError> {
        loop {
            let old = self
                .connections
                .lock()
                .map_err(|_| {
                    AppError::new(
                        "Argo CD connection state unavailable",
                        AppErrorKind::ArgoConnection,
                    )
                })?
                .get(id)
                .cloned();
            let Some(old) = old else {
                return Ok(());
            };
            let _lease = old.gate.clone().lock_owned().await;
            let removed = {
                let mut connections = self.connections.lock().map_err(|_| {
                    AppError::new(
                        "Argo CD connection state unavailable",
                        AppErrorKind::ArgoConnection,
                    )
                })?;
                if connections
                    .get(id)
                    .is_some_and(|current| Arc::ptr_eq(current, &old))
                {
                    connections.remove(id)
                } else {
                    None
                }
            };
            if removed.is_some() {
                old.close_tunnel();
                return Ok(());
            }
        }
    }
}

fn credential_key(profile: &ArgoConnectionProfile) -> String {
    let scope = format!(
        "{}:{}",
        profile.cluster_context.clone().unwrap_or_default(),
        profile.workspace_id.clone().unwrap_or_default()
    );
    match &profile.endpoint {
        // Preserve existing credential lookups for migrated legacy profiles.
        ArgoServerEndpoint::ExternalHttps { .. } => {
            format!("{}:{}:{}", profile.id, profile.url, scope)
        }
        ArgoServerEndpoint::ServiceTunnel {
            namespace,
            service_name,
            service_port,
            scheme,
            root_path,
            tls_server_name,
        } => format!(
            "{}:serviceTunnel:{}:{}:{}:{}:{}:{}:{}:{}",
            profile.id,
            namespace,
            service_name,
            service_port,
            scheme,
            root_path.as_deref().unwrap_or("/"),
            tls_server_name.as_deref().unwrap_or_default(),
            scope,
            profile.kubeconfig_source_key.as_deref().unwrap_or_default(),
        ),
    }
}
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct StoredCredential {
    token: String,
    generation: String,
}

trait CredentialStore {
    fn read(&self, key: &str) -> Result<Option<String>, AppError>;
    fn write(&self, key: &str, value: &str) -> Result<(), AppError>;
    fn delete(&self, key: &str) -> Result<(), AppError>;
}

fn credential_error(error: keyring::Error) -> AppError {
    AppError::new(
        "native credential storage unavailable",
        AppErrorKind::CredentialUnavailable,
    )
    .with_source(error)
}

struct KeyringCredentialStore;
impl CredentialStore for KeyringCredentialStore {
    fn read(&self, key: &str) -> Result<Option<String>, AppError> {
        let entry = keyring::Entry::new("KubeCove Argo CD", key).map_err(credential_error)?;
        match entry.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(credential_error(error)),
        }
    }
    fn write(&self, key: &str, value: &str) -> Result<(), AppError> {
        keyring::Entry::new("KubeCove Argo CD", key)
            .map_err(credential_error)?
            .set_password(value)
            .map_err(credential_error)
    }
    fn delete(&self, key: &str) -> Result<(), AppError> {
        let entry = keyring::Entry::new("KubeCove Argo CD", key).map_err(credential_error)?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(credential_error(error)),
        }
    }
}

fn load_credential(
    store: &dyn CredentialStore,
    profile: &ArgoConnectionProfile,
) -> Result<Option<StoredCredential>, AppError> {
    let value = store.read(&credential_key(profile))?;
    let Some(value) = value else { return Ok(None) };
    let record = match serde_json::from_str::<StoredCredential>(&value) {
        Ok(record) => record,
        Err(_) if !value.is_empty() => {
            let record = StoredCredential {
                token: value,
                generation: Uuid::new_v4().to_string(),
            };
            save_credential(store, profile, &record.token, record.generation.clone())?;
            record
        }
        Err(_) => return Ok(None),
    };
    Ok((!record.token.is_empty()).then_some(record))
}

fn save_credential(
    store: &dyn CredentialStore,
    profile: &ArgoConnectionProfile,
    token: &str,
    generation: String,
) -> Result<(), AppError> {
    let value = serde_json::to_string(&StoredCredential {
        token: token.into(),
        generation,
    })
    .map_err(|error| {
        AppError::new(
            "native credential storage unavailable",
            AppErrorKind::CredentialUnavailable,
        )
        .with_source(error)
    })?;
    store.write(&credential_key(profile), &value)
}

fn connection_generation(
    stored: Option<&StoredCredential>,
    submitted_token: bool,
    remember_credential: bool,
    insecure_tls: bool,
    custom_ca_pem: Option<&[u8]>,
) -> String {
    if submitted_token || insecure_tls || custom_ca_pem.is_some() {
        return Uuid::new_v4().to_string();
    }
    if remember_credential {
        return stored.map_or_else(
            || Uuid::new_v4().to_string(),
            |record| record.generation.clone(),
        );
    }
    Uuid::new_v4().to_string()
}

fn delete_credential(
    store: &dyn CredentialStore,
    profile: &ArgoConnectionProfile,
) -> Result<(), AppError> {
    store.delete(&credential_key(profile))
}
fn unavailable_capability(
    id: String,
    name: String,
    namespace: Option<String>,
    reason: ArgoServiceTunnelUnavailableReason,
    message: impl Into<String>,
) -> ArgoServerCapability {
    ArgoServerCapability {
        id,
        name,
        namespace,
        url: None,
        transport: "serviceTunnel".into(),
        endpoint: None,
        unavailable_reason: Some(message.into()),
        unavailable: Some(reason),
    }
}

fn servicetunnel_capabilities(service: &Service) -> Vec<ArgoServerCapability> {
    let Some(name) = service.metadata.name.clone() else {
        return Vec::new();
    };
    if !ARGO_SERVICE_NAMES.contains(&name.as_str()) {
        return Vec::new();
    }
    let namespace = service.metadata.namespace.clone();
    let id = format!("service:{}:{name}", namespace.clone().unwrap_or_default());
    let Some(spec) = service.spec.as_ref() else {
        return vec![unavailable_capability(
            id,
            name,
            namespace,
            ArgoServiceTunnelUnavailableReason::TargetUnavailable,
            "service spec is unavailable",
        )];
    };
    if matches!(spec.type_.as_deref(), Some("ExternalName")) {
        return vec![unavailable_capability(
            id,
            name,
            namespace,
            ArgoServiceTunnelUnavailableReason::ExternalName,
            "ExternalName Services cannot be port-forwarded",
        )];
    }
    let ports: Vec<_> = spec
        .ports
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter(|port| port.protocol.as_deref().unwrap_or("TCP") == "TCP")
        .filter_map(|port| u16::try_from(port.port).ok())
        .filter(|port| *port != 0)
        .collect();
    if ports.is_empty() {
        return vec![unavailable_capability(
            id,
            name,
            namespace,
            ArgoServiceTunnelUnavailableReason::NoTcpPorts,
            "service has no TCP ports to forward",
        )];
    }
    let selector_missing = spec
        .selector
        .as_ref()
        .is_none_or(std::collections::BTreeMap::is_empty);
    ports
        .into_iter()
        .map(|service_port| {
            let id = format!("{id}:{service_port}");
            if selector_missing {
                return unavailable_capability(
                    id,
                    name.clone(),
                    namespace.clone(),
                    ArgoServiceTunnelUnavailableReason::SelectorRequired,
                    "service port-forwarding requires a selector-backed Service",
                );
            }
            ArgoServerCapability {
                id,
                name: name.clone(),
                namespace: namespace.clone(),
                url: None,
                transport: "serviceTunnel".into(),
                endpoint: Some(ArgoServerEndpoint::ServiceTunnel {
                    namespace: namespace.clone().unwrap_or_default(),
                    service_name: name.clone(),
                    service_port,
                    scheme: "https".into(),
                    root_path: None,
                    tls_server_name: None,
                }),
                unavailable_reason: None,
                unavailable: None,
            }
        })
        .collect()
}

fn tunnel_target_unavailable(error: &AppError) -> ArgoServiceTunnelUnavailableReason {
    match error.kind {
        AppErrorKind::LiveSessionTargetUnavailable => {
            ArgoServiceTunnelUnavailableReason::NoReadyPod
        }
        _ => ArgoServiceTunnelUnavailableReason::TargetUnavailable,
    }
}

#[tauri::command]
pub async fn discover_argo_servers(
    cancellations: tauri::State<'_, BackendCancellationRegistry>,
    cluster_context: String,
    kubeconfig_env_var: Option<String>,
    request_id: Option<String>,
    cancel_scope: Option<String>,
) -> Result<Vec<ArgoServerCapability>, AppError> {
    cancellations
        .execute(
            cancel_scope,
            request_id,
            Box::pin(async move {
                let client = client_for_context(&cluster_context, kubeconfig_env_var).await?;
                let services: Api<Service> = Api::all(client.clone());
                let list = services
                    .list(&ListParams::default())
                    .await
                    .map_err(AppError::from)?;
                let mut capabilities = Vec::new();
                for service in list.items {
                    for mut capability in servicetunnel_capabilities(&service) {
                        if let Some(ArgoServerEndpoint::ServiceTunnel {
                            namespace,
                            service_name,
                            service_port,
                            ..
                        }) = capability.endpoint.as_ref()
                        {
                            if let Err(error) =
                                crate::commands::sessions::service::resolve_service_target(
                                    client.clone(),
                                    namespace,
                                    service_name,
                                    *service_port,
                                )
                                .await
                            {
                                capability.unavailable = Some(tunnel_target_unavailable(&error));
                                capability.unavailable_reason = Some(error.message);
                            }
                        }
                        capabilities.push(capability);
                    }
                }
                Ok(capabilities)
            }),
        )
        .await
}

#[tauri::command]
pub async fn connect_argo_server(
    store: tauri::State<'_, ArgoConnectionStore>,
    id: String,
    server_url: String,
    endpoint: Option<ArgoServerEndpoint>,
    token: Option<String>,
    username: Option<String>,
    password: Option<String>,
    insecure_tls: bool,
    custom_ca_pem: Option<Vec<u8>>,
    remember_credential: bool,
    cluster_context: Option<String>,
    kubeconfig_env_var: Option<String>,
    workspace_id: Option<String>,
) -> Result<ArgoConnectionStatus, AppError> {
    let connection_epoch = store.connection_epoch();
    let kubeconfig_source_key = kubeconfig_source_key(kubeconfig_env_var.as_deref())?;
    let (endpoint, normalized_url) = normalize_endpoint(
        endpoint.unwrap_or(ArgoServerEndpoint::ExternalHttps { url: server_url }),
    )?;
    let profile = ArgoConnectionProfile {
        id,
        url: normalized_url,
        endpoint,
        cluster_context,
        workspace_id,
        kubeconfig_source_key: Some(kubeconfig_source_key.clone()),
        transport: "connected".into(),
        remember_credential,
    };
    let mut tunnel = None;
    let resolved_host = if let ArgoServerEndpoint::ServiceTunnel {
        namespace,
        service_name,
        service_port,
        ..
    } = &profile.endpoint
    {
        let cluster_context = profile.cluster_context.as_deref().ok_or_else(|| {
            AppError::new(
                "clusterContext required for an Argo CD Service tunnel",
                AppErrorKind::ArgoConnection,
            )
        })?;
        let started = ArgoServiceTunnel::start(
            cluster_context,
            kubeconfig_env_var,
            namespace.clone(),
            service_name.clone(),
            *service_port,
        )
        .await?;
        let host = argo_url(&profile.url)?
            .host_str()
            .expect("normalized service endpoint has a host")
            .to_string();
        let address = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, started.local_port()));
        tunnel = Some(started);
        Some((host, address))
    } else {
        None
    };
    let client = http_client(
        insecure_tls,
        custom_ca_pem.clone(),
        resolved_host
            .as_ref()
            .map(|(host, address)| (host.as_str(), *address)),
    )?;
    let credential_store = KeyringCredentialStore;
    let submitted_token = token.filter(|value| !value.is_empty());
    let stored = if remember_credential {
        load_credential(&credential_store, &profile)?
    } else {
        None
    };
    let stored_token = stored.as_ref().map(|record| record.token.clone());
    let token = if let Some(token) = submitted_token.clone().or(stored_token) {
        token
    } else {
        let user = username.ok_or_else(|| {
            AppError::new(
                "token or local login credentials required",
                AppErrorKind::ArgoConnection,
            )
        })?;
        let password = password.ok_or_else(|| {
            AppError::new(
                "token or local login credentials required",
                AppErrorKind::ArgoConnection,
            )
        })?;
        let response = client
            .post(url(&profile.url, "/api/v1/session")?)
            .json(&json!({"username": user, "password": password}))
            .send()
            .await
            .map_err(safe_http_error)?;
        if !response.status().is_success() {
            return Err(AppError::new(
                format!("Argo CD login failed ({})", response.status()),
                AppErrorKind::ArgoConnection,
            ));
        }
        response_json(response)
            .await?
            .get("token")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| {
                AppError::new(
                    "Argo CD login returned no token",
                    AppErrorKind::ArgoConnection,
                )
            })?
    };
    let response = client
        .get(url(&profile.url, "/api/v1/session/userinfo")?)
        .bearer_auth(&token)
        .send()
        .await
        .map_err(safe_http_error)?;
    if !response.status().is_success() {
        return Err(AppError::new(
            format!("Argo CD session validation failed ({})", response.status()),
            AppErrorKind::ArgoConnection,
        ));
    }
    let userinfo = response_json(response).await?;
    let user = userinfo
        .get("username")
        .or_else(|| userinfo.get("sub"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let generation = connection_generation(
        stored.as_ref(),
        submitted_token.is_some(),
        remember_credential,
        insecure_tls,
        custom_ca_pem.as_deref(),
    );
    if remember_credential && (submitted_token.is_some() || stored.is_none()) {
        save_credential(&credential_store, &profile, &token, generation.clone())?;
    } else if !remember_credential {
        // A stale remembered credential must not revive on a later reconnect.
        delete_credential(&credential_store, &profile)?;
    }
    store
        .replace_connection_at(
            profile.id.clone(),
            Arc::new(ConnectedArgo {
                profile: profile.clone(),
                token,
                username: user.clone(),
                client,
                tunnel,
                generation,
                instance_id: Uuid::new_v4().to_string(),
                gate: Arc::new(AsyncMutex::new(())),
            }),
            connection_epoch,
        )
        .await?;
    Ok(ArgoConnectionStatus {
        profile: Some(profile),
        connected: true,
        username: user,
        unavailable_reason: None,
    })
}

#[tauri::command]
pub fn get_argo_connection_status(
    store: tauri::State<'_, ArgoConnectionStore>,
    id: String,
) -> Result<ArgoConnectionStatus, AppError> {
    let connection = store
        .connections
        .lock()
        .map_err(|_| {
            AppError::new(
                "Argo CD connection state unavailable",
                AppErrorKind::ArgoConnection,
            )
        })?
        .get(&id)
        .cloned();
    Ok(match connection {
        Some(connection) => ArgoConnectionStatus {
            profile: Some(connection.profile.clone()),
            connected: true,
            username: connection.username.clone(),
            unavailable_reason: None,
        },
        None => ArgoConnectionStatus {
            profile: None,
            connected: false,
            username: None,
            unavailable_reason: None,
        },
    })
}

#[tauri::command]
pub async fn disconnect_argo_server(
    store: tauri::State<'_, ArgoConnectionStore>,
    id: String,
) -> Result<(), AppError> {
    store.remove_connection(&id).await
}

#[tauri::command]
pub fn forget_argo_credential(profile: ArgoConnectionProfile) -> Result<(), AppError> {
    delete_credential(&KeyringCredentialStore, &profile)
}

pub(crate) fn text(object: &Value, field: &str) -> Option<String> {
    object.get(field).and_then(Value::as_str).map(str::to_owned)
}
pub(crate) fn managed_resource(value: &Value) -> ArgoManagedResource {
    ArgoManagedResource {
        group: text(value, "group"),
        version: text(value, "version"),
        kind: text(value, "kind"),
        namespace: text(value, "namespace"),
        name: text(value, "name"),
        status: text(value, "status"),
        health: value.get("health").and_then(|v| text(v, "status")),
        hook: value.get("hook").and_then(Value::as_bool),
        requires_pruning: value.get("requiresPruning").and_then(Value::as_bool),
        target_state: None,
        live_state: None,
    }
}
pub(crate) fn state(value: Option<&Value>, redact: bool) -> Option<Value> {
    value
        .and_then(|state| match state {
            Value::String(text) => serde_json::from_str(text)
                .ok()
                .or_else(|| Some(Value::String(text.clone()))),
            _ => Some(state.clone()),
        })
        .map(|mut state| {
            if redact {
                redact_secret_fields(&mut state);
            }
            state
        })
}
fn history(value: &Value) -> ArgoApplicationHistory {
    ArgoApplicationHistory {
        id: value.get("id").and_then(Value::as_i64),
        revision: text(value, "revision"),
        revisions: value
            .get("revisions")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        deployed_at: text(value, "deployedAt"),
        initiated_by: value.get("initiatedBy").and_then(|v| text(v, "username")),
        source: value.get("source").cloned(),
        sources: value
            .get("sources")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
    }
}

pub(crate) fn inspector_from_application(
    application: &DynamicObject,
) -> Result<ArgoApplicationInspector, AppError> {
    let data = application
        .data
        .as_object()
        .ok_or_else(|| AppError::new("invalid Application data", AppErrorKind::Serialization))?;
    let status = data.get("status").cloned();
    let source = status.as_ref();
    let resources: Vec<_> = source
        .and_then(|value| value.get("resources"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(managed_resource)
        .collect();
    Ok(ArgoApplicationInspector {
        application: ArgoApplicationRef {
            name: application.metadata.name.clone().unwrap_or_default(),
            namespace: application.metadata.namespace.clone(),
            project: data.get("spec").and_then(|value| text(value, "project")),
            resource_version: application.metadata.resource_version.clone(),
            uid: application.metadata.uid.clone(),
            api_version: application
                .types
                .as_ref()
                .map(|types| types.api_version.clone()),
            context: None,
            workspace_id: None,
        },
        history: source
            .and_then(|value| value.get("history"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(history)
            .collect(),
        comparisons: resources
            .iter()
            .cloned()
            .map(kubernetes_comparison)
            .collect(),
        resources,
        conditions: source
            .and_then(|value| value.get("conditions"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        operation_state: source
            .and_then(|value| value.get("operationState"))
            .cloned(),
        status,
        connected: false,
        transport: "kubernetes".into(),
        provenance: "kubernetes-status-no-diff".into(),
        connected_fallback: None,
    })
}

fn kubernetes_comparison(resource: ArgoManagedResource) -> ArgoResourceComparison {
    ArgoResourceComparison {
        resource,
        exact: Some(false),
        provenance: Some("kubernetes-status-no-diff".into()),
        ..Default::default()
    }
}

fn connected_comparison(value: &Value) -> ArgoResourceComparison {
    ArgoResourceComparison {
        resource: managed_resource(value),
        target_state: state(value.get("targetState"), true),
        live_state: state(value.get("liveState"), true),
        normalized_live_state: state(value.get("normalizedLiveState"), true),
        predicted_live_state: state(value.get("predictedLiveState"), true),
        modified: value.get("modified").and_then(Value::as_bool),
        exact: Some(true),
        provenance: Some("argocd-managed-resource".into()),
        ..Default::default()
    }
}

fn connected_inspector(
    application: ArgoApplicationRef,
    mut response: Value,
    mut managed: Value,
) -> ArgoApplicationInspector {
    redact_secret_fields(&mut response);
    redact_secret_fields(&mut managed);
    let status = response.get("status").cloned();
    let comparisons: Vec<_> = managed
        .get("items")
        .or_else(|| managed.get("managedResources"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(connected_comparison)
        .collect();
    let resources = comparisons
        .iter()
        .map(|comparison| comparison.resource.clone())
        .collect();
    ArgoApplicationInspector {
        application: ArgoApplicationRef {
            resource_version: response
                .pointer("/metadata/resourceVersion")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or(application.resource_version),
            uid: response
                .pointer("/metadata/uid")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or(application.uid),
            api_version: response
                .get("apiVersion")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or(application.api_version),
            context: application.context,
            workspace_id: application.workspace_id,
            ..application
        },
        history: status
            .as_ref()
            .and_then(|value| value.get("history"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(history)
            .collect(),
        resources,
        comparisons,
        conditions: status
            .as_ref()
            .and_then(|value| value.get("conditions"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        operation_state: status
            .as_ref()
            .and_then(|value| value.get("operationState"))
            .cloned(),
        status,
        connected: true,
        transport: "connected".into(),
        provenance: "argocd-api".into(),
        connected_fallback: None,
    }
}

pub(crate) async fn kubernetes_application(
    cluster_context: &str,
    namespace: Option<&str>,
    name: &str,
    kubeconfig_env_var: Option<String>,
) -> Result<DynamicObject, AppError> {
    let client = client_for_context(cluster_context, kubeconfig_env_var).await?;
    let ar = find_api_resource(&client, "argoproj.io", "Application")
        .await?
        .ok_or_else(|| AppError::new("Application CRD not found", AppErrorKind::Cluster))?;
    get_crd_object(client, &ar, name, namespace).await
}

async fn kubernetes_inspector(
    cluster_context: &str,
    application: &ArgoApplicationRef,
    kubeconfig_env_var: Option<String>,
) -> Result<ArgoApplicationInspector, AppError> {
    let workspace_id = application.workspace_id.clone();
    let mut object = kubernetes_application(
        cluster_context,
        application.namespace.as_deref(),
        &application.name,
        kubeconfig_env_var,
    )
    .await?;
    redact_secret_fields(&mut object.data);
    let mut inspector = inspector_from_application(&object)?;
    inspector.application.context = Some(cluster_context.to_string());
    inspector.application.workspace_id = workspace_id;
    Ok(inspector)
}

pub(crate) fn connected_application_path(
    base: &str,
    name: &str,
    namespace: Option<&str>,
    project: Option<&str>,
    managed_resources: bool,
) -> Result<String, AppError> {
    argo_url(base)?;
    let mut path = reqwest::Url::parse("https://argo.invalid/")
        .expect("constant Argo CD API base URL is valid");
    {
        let mut segments = path.path_segments_mut().map_err(|()| {
            AppError::new("invalid Argo CD API path", AppErrorKind::ArgoConnection)
        })?;
        segments.extend(["api", "v1", "applications"]).push(name);
        if managed_resources {
            segments.push("managed-resources");
        }
    }
    let mut query = path.query_pairs_mut();
    if let Some(value) = namespace {
        query.append_pair("appNamespace", value);
    }
    if let Some(value) = project {
        query.append_pair("project", value);
    }
    drop(query);
    Ok(format!(
        "{}{}",
        path.path(),
        path.query()
            .map(|value| format!("?{value}"))
            .unwrap_or_default()
    ))
}

async fn connected_inspector_read(
    store: &ArgoConnectionStore,
    cluster_context: &str,
    kubeconfig_env_var: Option<&str>,
    connection_id: Option<&str>,
    application: ArgoApplicationRef,
) -> Result<ArgoApplicationInspector, AppError> {
    let connection = scoped_connection(
        store,
        connection_id.ok_or_else(|| {
            AppError::new("Argo CD connection required", AppErrorKind::ArgoConnection)
        })?,
        cluster_context,
        application.workspace_id.as_deref(),
        kubeconfig_env_var,
    )?;
    let namespace = application.namespace.clone().unwrap_or_default();
    let response = api_get(
        &connection,
        &connected_application_path(
            &connection.profile.url,
            &application.name,
            Some(&namespace),
            application.project.as_deref(),
            false,
        )?,
    )
    .await?;
    let managed = api_get(
        &connection,
        &connected_application_path(
            &connection.profile.url,
            &application.name,
            Some(&namespace),
            None,
            true,
        )?,
    )
    .await?;
    Ok(connected_inspector(application, response, managed))
}

fn inspection_failure(error: &AppError) -> crate::models::ArgoInspectionFailure {
    let message = match error.kind {
        AppErrorKind::Forbidden => "access denied",
        AppErrorKind::NotFound => "Application not found",
        AppErrorKind::Cancelled => "request cancelled",
        AppErrorKind::Kubeconfig => "cluster configuration unavailable",
        AppErrorKind::Network => "network unavailable",
        AppErrorKind::ArgoConnection => "connection unavailable",
        AppErrorKind::ArgoApi => "Argo CD API request failed",
        AppErrorKind::ProviderDiscoveryUnavailable => "Application API unavailable",
        AppErrorKind::Serialization => "invalid response",
        _ => "read failed",
    };
    let kind = match error.kind {
        AppErrorKind::ArgoApi
        | AppErrorKind::ArgoConnection
        | AppErrorKind::Cancelled
        | AppErrorKind::Cluster
        | AppErrorKind::Forbidden
        | AppErrorKind::Kubeconfig
        | AppErrorKind::Network
        | AppErrorKind::NotFound
        | AppErrorKind::ProviderDiscoveryUnavailable
        | AppErrorKind::Serialization => error.kind.as_str(),
        _ => "inspection",
    };
    crate::models::ArgoInspectionFailure {
        kind: kind.into(),
        message: message.into(),
    }
}

fn with_connected_fallback(
    mut inspector: ArgoApplicationInspector,
    error: &AppError,
) -> ArgoApplicationInspector {
    inspector.connected_fallback = Some(crate::models::ArgoConnectedFallback {
        transport: "connected".into(),
        failure: inspection_failure(error),
    });
    inspector
}

fn inspection_error(connected: &AppError, kubernetes: &AppError) -> AppError {
    let connected = inspection_failure(connected);
    let kubernetes = inspection_failure(kubernetes);
    AppError::new(
        format!(
            "Connected inspection failed ({}) {}; Kubernetes inspection failed ({}) {}",
            connected.kind, connected.message, kubernetes.kind, kubernetes.message
        ),
        AppErrorKind::ArgoInspection,
    )
}

async fn application_inspector(
    store: &ArgoConnectionStore,
    cluster_context: String,
    kubeconfig_env_var: Option<String>,
    connection_id: Option<String>,
    transport: String,
    application: ArgoApplicationRef,
) -> Result<ArgoApplicationInspector, AppError> {
    if transport == "kubernetes" {
        return kubernetes_inspector(&cluster_context, &application, kubeconfig_env_var).await;
    }
    if transport != "connected" {
        return Err(AppError::new(
            "invalid Argo CD transport",
            AppErrorKind::ArgoConnection,
        ));
    }
    match connected_inspector_read(
        store,
        &cluster_context,
        kubeconfig_env_var.as_deref(),
        connection_id.as_deref(),
        application.clone(),
    )
    .await
    {
        Ok(inspector) => Ok(inspector),
        Err(connected_error) => {
            match kubernetes_inspector(&cluster_context, &application, kubeconfig_env_var).await {
                Ok(inspector) => Ok(with_connected_fallback(inspector, &connected_error)),
                Err(kubernetes_error) => Err(inspection_error(&connected_error, &kubernetes_error)),
            }
        }
    }
}

#[tauri::command]
pub async fn get_argo_application_inspector(
    store: tauri::State<'_, ArgoConnectionStore>,
    cancellations: tauri::State<'_, BackendCancellationRegistry>,
    cluster_context: String,
    kubeconfig_env_var: Option<String>,
    connection_id: Option<String>,
    transport: String,
    application: ArgoApplicationRef,
    _redact_secrets: Option<bool>,
    request_id: Option<String>,
    cancel_scope: Option<String>,
) -> Result<ArgoApplicationInspector, AppError> {
    cancellations
        .execute(
            cancel_scope,
            request_id,
            application_inspector(
                &store,
                cluster_context,
                kubeconfig_env_var,
                connection_id,
                transport,
                application,
            ),
        )
        .await
}

#[cfg(test)]
mod tests {
    use super::super::scope::acquire_connection_lease;
    use super::*;

    #[test]
    fn url_preserves_configured_base_path() {
        assert_eq!(
            url("https://argo.example/argo-cd", "/api/v1/applications").unwrap(),
            "https://argo.example/argo-cd/api/v1/applications"
        );
    }

    #[derive(Default)]
    struct MemoryCredentialStore(Mutex<HashMap<String, String>>);
    #[test]
    fn credential_failure_keeps_native_source_private() {
        let error = credential_error(keyring::Error::NoEntry);
        assert!(std::error::Error::source(&error)
            .unwrap()
            .is::<keyring::Error>());
        assert_eq!(
            serde_json::to_value(&error).unwrap(),
            json!({"message":"native credential storage unavailable","kind":"credentialUnavailable"})
        );
    }
    impl CredentialStore for MemoryCredentialStore {
        fn read(&self, key: &str) -> Result<Option<String>, AppError> {
            Ok(self.0.lock().unwrap().get(key).cloned())
        }
        fn write(&self, key: &str, value: &str) -> Result<(), AppError> {
            self.0.lock().unwrap().insert(key.into(), value.into());
            Ok(())
        }
        fn delete(&self, key: &str) -> Result<(), AppError> {
            self.0.lock().unwrap().remove(key);
            Ok(())
        }
    }

    fn profile() -> ArgoConnectionProfile {
        ArgoConnectionProfile {
            id: "server".into(),
            url: "https://argo.example/argo-cd".into(),
            endpoint: ArgoServerEndpoint::ExternalHttps {
                url: "https://argo.example/argo-cd".into(),
            },
            cluster_context: Some("cluster".into()),
            workspace_id: Some("workspace".into()),
            kubeconfig_source_key: Some("kubeconfigSource=test".into()),
            transport: "connected".into(),
            remember_credential: true,
        }
    }

    fn connection(generation: &str) -> Arc<ConnectedArgo> {
        connection_with_tunnel(generation, None)
    }

    fn connection_with_tunnel(
        generation: &str,
        tunnel: Option<ArgoServiceTunnel>,
    ) -> Arc<ConnectedArgo> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        Arc::new(ConnectedArgo {
            profile: profile(),
            token: "token".into(),
            username: None,
            client: HttpClient::new(),
            tunnel,
            generation: generation.into(),
            instance_id: Uuid::new_v4().to_string(),
            gate: Arc::new(tokio::sync::Mutex::new(())),
        })
    }

    #[tokio::test]
    async fn lease_rejects_replaced_and_disconnected_connections() {
        let store = ArgoConnectionStore::default();
        let old = connection("old");
        store
            .replace_connection("server".into(), old)
            .await
            .unwrap();
        let selected = scoped_connection(
            &store,
            "server",
            "cluster",
            Some("workspace"),
            Some("kubeconfigSource=test"),
        )
        .unwrap();
        let replacement = connection("replacement");
        store
            .replace_connection("server".into(), replacement.clone())
            .await
            .unwrap();

        let error = acquire_connection_lease(
            &store,
            "server",
            selected,
            "cluster",
            Some("workspace"),
            Some("kubeconfigSource=test"),
            "old",
            "old-instance",
        )
        .await
        .err()
        .expect("replacement must reject the old connection");
        assert_eq!(error.message, "Argo CD connection was replaced");
        assert!(Arc::ptr_eq(
            store.connections.lock().unwrap().get("server").unwrap(),
            &replacement
        ));

        let selected = scoped_connection(
            &store,
            "server",
            "cluster",
            Some("workspace"),
            Some("kubeconfigSource=test"),
        )
        .unwrap();
        store.remove_connection("server").await.unwrap();
        let error = acquire_connection_lease(
            &store,
            "server",
            selected,
            "cluster",
            Some("workspace"),
            Some("kubeconfigSource=test"),
            "replacement",
            "replacement-instance",
        )
        .await
        .err()
        .expect("disconnect must reject the selected connection");
        assert_eq!(error.message, "Argo CD connection was replaced");
    }

    #[tokio::test]
    async fn disconnect_and_replacement_wait_for_active_lease() {
        let store = ArgoConnectionStore::default();
        let old = connection("old");
        store
            .replace_connection("server".into(), old.clone())
            .await
            .unwrap();

        let lease = acquire_connection_lease(
            &store,
            "server",
            scoped_connection(
                &store,
                "server",
                "cluster",
                Some("workspace"),
                Some("kubeconfigSource=test"),
            )
            .unwrap(),
            "cluster",
            Some("workspace"),
            Some("kubeconfigSource=test"),
            "old",
            &old.instance_id,
        )
        .await
        .unwrap();
        let mut removal = Box::pin(store.remove_connection("server"));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), removal.as_mut())
                .await
                .is_err()
        );
        assert!(Arc::ptr_eq(
            store.connections.lock().unwrap().get("server").unwrap(),
            &old
        ));
        drop(lease);
        removal.await.unwrap();
        assert!(store.connections.lock().unwrap().is_empty());

        let old = connection("old");
        store
            .replace_connection("server".into(), old.clone())
            .await
            .unwrap();
        let lease = acquire_connection_lease(
            &store,
            "server",
            scoped_connection(
                &store,
                "server",
                "cluster",
                Some("workspace"),
                Some("kubeconfigSource=test"),
            )
            .unwrap(),
            "cluster",
            Some("workspace"),
            Some("kubeconfigSource=test"),
            "old",
            &old.instance_id,
        )
        .await
        .unwrap();
        let replacement = connection("replacement");
        let mut replace = Box::pin(store.replace_connection("server".into(), replacement.clone()));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), replace.as_mut())
                .await
                .is_err()
        );
        assert!(Arc::ptr_eq(
            store.connections.lock().unwrap().get("server").unwrap(),
            &old
        ));
        drop(lease);
        replace.await.unwrap();
        assert!(Arc::ptr_eq(
            store.connections.lock().unwrap().get("server").unwrap(),
            &replacement
        ));
    }

    #[tokio::test]
    async fn disconnect_removes_replacement_after_waiting_on_old_gate() {
        let store = ArgoConnectionStore::default();
        let old = connection("old");
        store
            .replace_connection("server".into(), old.clone())
            .await
            .unwrap();
        let old_gate = old.gate.clone().lock_owned().await;
        let replacement = connection("replacement");
        let mut replace = Box::pin(store.replace_connection("server".into(), replacement));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), replace.as_mut())
                .await
                .is_err()
        );
        let mut removal = Box::pin(store.remove_connection("server"));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), removal.as_mut())
                .await
                .is_err()
        );

        drop(old_gate);
        replace.await.unwrap();
        removal.await.unwrap();
        assert!(store.connections.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn close_all_removes_connections_and_closes_tunnels() {
        let store = ArgoConnectionStore::default();
        let (first_shutdown, first_closed) = tokio::sync::oneshot::channel();
        let (second_shutdown, second_closed) = tokio::sync::oneshot::channel();
        store
            .replace_connection(
                "first".into(),
                connection_with_tunnel(
                    "first",
                    Some(ArgoServiceTunnel::test_tunnel(first_shutdown)),
                ),
            )
            .await
            .unwrap();
        store
            .replace_connection(
                "second".into(),
                connection_with_tunnel(
                    "second",
                    Some(ArgoServiceTunnel::test_tunnel(second_shutdown)),
                ),
            )
            .await
            .unwrap();

        store.close_all().await.unwrap();

        assert!(store.connections.lock().unwrap().is_empty());
        assert!(first_closed.await.is_ok());
        assert!(second_closed.await.is_ok());
    }

    #[tokio::test]
    async fn source_scope_rejects_same_context_from_another_kubeconfig() {
        let store = ArgoConnectionStore::default();
        store
            .replace_connection("server".into(), connection("generation"))
            .await
            .unwrap();

        let error = scoped_connection(
            &store,
            "server",
            "cluster",
            Some("workspace"),
            Some("kubeconfigSource=other"),
        )
        .err()
        .expect("another kubeconfig source must not reuse the connection");

        assert_eq!(
            error.message,
            "Argo CD connection is outside current workspace scope"
        );
    }

    #[tokio::test]
    async fn workspace_cleanup_rejects_connection_started_before_cleanup() {
        let store = ArgoConnectionStore::default();
        let started_epoch = store.connection_epoch();
        let (shutdown, closed) = tokio::sync::oneshot::channel();
        let late = connection_with_tunnel("late", Some(ArgoServiceTunnel::test_tunnel(shutdown)));

        store.close_all().await.unwrap();
        let error = store
            .replace_connection_at("late".into(), late, started_epoch)
            .await
            .expect_err("cleanup must reject a connection that started earlier");

        assert_eq!(
            error.message,
            "Argo CD connection was cancelled by workspace cleanup"
        );
        assert!(store.connections.lock().unwrap().is_empty());
        assert!(closed.await.is_ok());
    }

    #[test]
    fn service_tunnel_credentials_are_scoped_to_kubeconfig_source() {
        let mut first = profile();
        first.endpoint = ArgoServerEndpoint::ServiceTunnel {
            namespace: "argocd".into(),
            service_name: "argocd-server".into(),
            service_port: 443,
            scheme: "https".into(),
            root_path: None,
            tls_server_name: None,
        };
        let mut second = first.clone();
        second.kubeconfig_source_key = Some("kubeconfigSource=other".into());

        assert_ne!(credential_key(&first), credential_key(&second));
    }

    #[test]
    fn persisted_credential_generation_survives_restart_and_rotates_on_change() {
        let store = MemoryCredentialStore::default();
        let profile = profile();
        let first = Uuid::new_v4().to_string();
        save_credential(&store, &profile, "token-a", first.clone()).unwrap();
        let restarted = load_credential(&store, &profile).unwrap().unwrap();
        assert_eq!(restarted.token, "token-a");
        assert_eq!(restarted.generation, first);
        let rotated = Uuid::new_v4().to_string();
        save_credential(&store, &profile, "token-b", rotated.clone()).unwrap();
        assert_ne!(rotated, restarted.generation);
    }

    #[test]
    fn legacy_raw_token_is_migrated() {
        let store = MemoryCredentialStore::default();
        let profile = profile();
        store
            .0
            .lock()
            .unwrap()
            .insert(credential_key(&profile), "legacy-token".into());

        let migrated = load_credential(&store, &profile).unwrap().unwrap();

        assert_eq!(migrated.token, "legacy-token");
        let stored = store.0.lock().unwrap()[&credential_key(&profile)].clone();
        assert_eq!(
            serde_json::from_str::<StoredCredential>(&stored)
                .unwrap()
                .token,
            "legacy-token"
        );
    }

    #[test]
    fn generation_policy_is_ephemeral_for_submitted_or_tls_credentials() {
        let stored = StoredCredential {
            token: "token".into(),
            generation: "saved-generation".into(),
        };
        assert_eq!(
            connection_generation(Some(&stored), false, true, false, None),
            "saved-generation"
        );
        assert_ne!(
            connection_generation(Some(&stored), true, true, false, None),
            "saved-generation"
        );
        assert_ne!(
            connection_generation(Some(&stored), false, true, true, None),
            "saved-generation"
        );
        assert_ne!(
            connection_generation(Some(&stored), false, true, false, Some(b"ca")),
            "saved-generation"
        );
    }

    #[test]
    fn serialized_credential_contains_no_tls_material() {
        let value = serde_json::to_value(StoredCredential {
            token: "token".into(),
            generation: "generation".into(),
        })
        .unwrap();
        assert_eq!(
            value,
            serde_json::json!({"token": "token", "generation": "generation"})
        );
    }

    #[test]
    fn discovery_only_offers_exact_argo_server_services() {
        let service = |name: &str| Service {
            metadata: kube::core::ObjectMeta {
                name: Some(name.into()),
                ..Default::default()
            },
            spec: Some(k8s_openapi::api::core::v1::ServiceSpec {
                ports: Some(vec![k8s_openapi::api::core::v1::ServicePort {
                    port: 443,
                    ..Default::default()
                }]),
                selector: Some(std::collections::BTreeMap::from([(
                    "app".into(),
                    "argocd-server".into(),
                )])),
                ..Default::default()
            }),
            ..Default::default()
        };

        assert_eq!(
            servicetunnel_capabilities(&service("argocd-server")).len(),
            1
        );
        assert!(servicetunnel_capabilities(&service("argocd-server-metrics")).is_empty());
        assert!(servicetunnel_capabilities(&service("argo-cd-argocd-server-metrics")).is_empty());
    }

    #[test]
    fn connected_paths_encode_application_and_query_identity() {
        let path = connected_application_path(
            "https://argo.example/argo-cd",
            "a/b?#",
            Some("ns &/#?"),
            Some("project&?#/"),
            false,
        )
        .unwrap();
        assert_eq!(
            path,
            "/api/v1/applications/a%2Fb%3F%23?appNamespace=ns+%26%2F%23%3F&project=project%26%3F%23%2F"
        );
    }

    #[test]
    fn connected_paths_preserve_base_path_for_managed_resources() {
        assert_eq!(
            connected_application_path(
                "https://argo.example/argo-cd",
                "demo",
                Some("argocd"),
                None,
                true,
            )
            .unwrap(),
            "/api/v1/applications/demo/managed-resources?appNamespace=argocd"
        );
    }

    #[test]
    fn connected_paths_apply_argo_root_path_once() {
        let path = connected_application_path(
            "https://argo.example/argo",
            "demo",
            Some("argocd"),
            None,
            false,
        )
        .unwrap();

        assert_eq!(
            url("https://argo.example/argo", &path).unwrap(),
            "https://argo.example/argo/api/v1/applications/demo?appNamespace=argocd"
        );
    }

    #[test]
    fn connected_inspection_eagerly_compares_and_redacts_nested_secrets() {
        let inspector = connected_inspector(
            ArgoApplicationRef {
                name: "demo".into(),
                namespace: Some("argocd".into()),
                ..Default::default()
            },
            serde_json::json!({
                "status": {
                    "conditions": [{"nested": {"kind": "Secret", "data": {"key": "value"}}}]
                }
            }),
            serde_json::json!({
                "items": [{
                    "group": "", "version": "v1", "kind": "Secret", "namespace": "default", "name": "db",
                    "targetState": "{\"kind\":\"Secret\",\"data\":{\"password\":\"plaintext\"}}",
                    "liveState": {"kind": "Secret", "stringData": {"password": "plaintext"}},
                    "modified": true
                }]
            }),
        );
        let value = serde_json::to_value(&inspector).unwrap();

        assert_eq!(value["transport"], "connected");
        assert_eq!(value["provenance"], "argocd-api");
        assert_eq!(value["comparisons"][0]["exact"], true);
        assert_eq!(
            value["comparisons"][0]["availableActions"],
            serde_json::json!([])
        );
        assert_eq!(
            value["comparisons"][0]["targetState"]["data"]["password"],
            "[REDACTED]"
        );
        assert_eq!(
            value["conditions"][0]["nested"]["data"]["key"],
            "[REDACTED]"
        );
    }

    #[test]
    fn kubernetes_comparisons_do_not_imply_desired_state() {
        let comparison = kubernetes_comparison(ArgoManagedResource {
            kind: Some("Deployment".into()),
            name: Some("api".into()),
            ..Default::default()
        });

        assert_eq!(comparison.exact, Some(false));
        assert_eq!(
            comparison.provenance.as_deref(),
            Some("kubernetes-status-no-diff")
        );
        assert!(comparison.target_state.is_none());
        assert!(comparison.live_state.is_none());
        assert!(comparison.available_actions.is_empty());
    }

    #[test]
    fn fallback_keeps_complete_kubernetes_provenance_visible() {
        let inspector = ArgoApplicationInspector {
            transport: "kubernetes".into(),
            provenance: "kubernetes-status-no-diff".into(),
            resources: vec![ArgoManagedResource {
                kind: Some("Deployment".into()),
                name: Some("api".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let fallback = with_connected_fallback(
            inspector,
            &AppError::new("token=plaintext", AppErrorKind::ArgoConnection),
        );

        assert_eq!(fallback.transport, "kubernetes");
        assert_eq!(fallback.provenance, "kubernetes-status-no-diff");
        assert_eq!(fallback.resources.len(), 1);
        let connected = fallback.connected_fallback.expect("fallback metadata");
        assert_eq!(connected.transport, "connected");
        assert_eq!(connected.failure.kind, "argoConnection");
        assert_eq!(connected.failure.message, "connection unavailable");
    }

    #[test]
    fn fallback_and_combined_errors_are_sanitized() {
        let connected = AppError::new(
            "token=plaintext https://user:password@argo.example/api kubeconfig /tmp/config",
            AppErrorKind::ArgoConnection,
        );
        let kubernetes = AppError::new("Secret data: plaintext", AppErrorKind::Cluster);
        let fallback = inspection_failure(&connected);
        let error = inspection_error(&connected, &kubernetes);

        assert_eq!(fallback.kind, "argoConnection");
        assert_eq!(fallback.message, "connection unavailable");
        assert_eq!(error.kind, AppErrorKind::ArgoInspection);
        assert!(error.message.contains("argoConnection"));
        assert!(error.message.contains("cluster"));
        assert!(!error.message.contains("plaintext"));
        assert!(!error.message.contains("https://"));
    }
}
