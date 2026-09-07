use super::error::AppError;
use super::HealthAssessment;
use crate::models::AppErrorKind;
use serde::{Deserialize, Serialize};
use serde_json::Value;
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArgoApplicationSourceSummary {
    pub repo_url: Option<String>,
    pub target_revision: Option<String>,
    pub resolved_revision: Option<String>,
    pub path: Option<String>,
    pub chart: Option<String>,
    pub source_mode: Option<String>,
    pub reference: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArgoApplicationSummary {
    pub cluster: String,
    pub name: String,
    pub age: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    pub namespace: Option<String>,
    pub project: Option<String>,
    pub sync_status: Option<String>,
    pub health_status: Option<String>,
    pub health_assessment: HealthAssessment,
    pub destination_namespace: Option<String>,
    pub destination_server: Option<String>,
    pub source_repo: Option<String>,
    pub source_revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_count: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uid: Option<String>,
    #[serde(default)]
    pub sources: Vec<ArgoApplicationSourceSummary>,
    #[serde(default)]
    pub resource_namespaces: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracked_resource_count: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArgoApplicationDetails {
    pub summary: ArgoApplicationSummary,
    pub yaml: String,
    pub metadata: serde_json::Value,
    pub status: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArgoApplicationSetSummary {
    pub cluster: String,
    pub name: String,
    pub age: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    pub namespace: Option<String>,
    pub project: Option<String>,
    pub status: Option<String>,
    pub sync_status: Option<String>,
    pub health_status: Option<String>,
    pub health_assessment: HealthAssessment,
    pub destination_namespace: Option<String>,
    pub destination_server: Option<String>,
    pub source_repo: Option<String>,
    pub source_revision: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArgoApplicationSetDetails {
    pub summary: ArgoApplicationSetSummary,
    pub yaml: String,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArgoAppProjectSummary {
    pub cluster: String,
    pub name: String,
    pub age: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    pub namespace: Option<String>,
    pub description: Option<String>,
    pub status: Option<String>,
    pub health_assessment: HealthAssessment,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArgoAppProjectDetails {
    pub summary: ArgoAppProjectSummary,
    pub yaml: String,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ArgoServerEndpoint {
    ExternalHttps {
        url: String,
    },
    ServiceTunnel {
        namespace: String,
        service_name: String,
        service_port: u16,
        scheme: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        root_path: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tls_server_name: Option<String>,
    },
}

pub(crate) fn normalize_service_tunnel_endpoint(
    namespace: String,
    service_name: String,
    service_port: u16,
    scheme: String,
    root_path: Option<String>,
    tls_server_name: Option<String>,
) -> Result<(ArgoServerEndpoint, String), AppError> {
    let namespace = required_dns_label(&namespace, "namespace")?;
    let service_name = required_dns_label(&service_name, "name")?;
    if service_port == 0 {
        return Err(AppError::new(
            "invalid Argo CD Service port",
            AppErrorKind::ArgoConnection,
        ));
    }
    let scheme = scheme.trim().to_ascii_lowercase();
    if !matches!(scheme.as_str(), "http" | "https") {
        return Err(AppError::new(
            "Argo CD Service tunnel must use HTTP or HTTPS",
            AppErrorKind::ArgoConnection,
        ));
    }
    let root_path = normalized_root_path(root_path.as_deref())?;
    let tls_server_name =
        normalized_tls_server_name(tls_server_name.as_deref(), &service_name, &namespace)?;
    let url = reqwest::Url::parse(&format!("{scheme}://{tls_server_name}{root_path}"))
        .map_err(|error| {
            AppError::new(
                "invalid Argo CD Service endpoint",
                AppErrorKind::ArgoConnection,
            )
            .with_source(error)
        })?
        .to_string()
        .trim_end_matches('/')
        .to_string();
    Ok((
        ArgoServerEndpoint::ServiceTunnel {
            namespace,
            service_name,
            service_port,
            scheme,
            root_path: (root_path != "/").then_some(root_path),
            tls_server_name: Some(tls_server_name),
        },
        url,
    ))
}

fn required_dns_label(value: &str, field: &str) -> Result<String, AppError> {
    let value = value.trim().to_string();
    let valid = !value.is_empty()
        && value.len() <= 63
        && value.bytes().enumerate().all(|(index, byte)| match byte {
            b'a'..=b'z' | b'0'..=b'9' => true,
            b'-' => index != 0 && index + 1 != value.len(),
            _ => false,
        });
    valid.then_some(value).ok_or_else(|| {
        AppError::new(
            format!("invalid Argo CD Service {field}"),
            AppErrorKind::ArgoConnection,
        )
    })
}

fn normalized_root_path(value: Option<&str>) -> Result<String, AppError> {
    let value = value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("/");
    if !value.starts_with('/') {
        return Err(AppError::new(
            "Argo CD root path must start with '/'",
            AppErrorKind::ArgoConnection,
        ));
    }
    let url = reqwest::Url::parse(&format!("https://argo.invalid{value}")).map_err(|error| {
        AppError::new("invalid Argo CD root path", AppErrorKind::ArgoConnection).with_source(error)
    })?;
    if url.query().is_some() || url.fragment().is_some() {
        return Err(AppError::new(
            "Argo CD root path must not contain a query or fragment",
            AppErrorKind::ArgoConnection,
        ));
    }
    Ok(match url.path().trim_end_matches('/') {
        "" => "/".into(),
        path => path.into(),
    })
}

fn normalized_tls_server_name(
    value: Option<&str>,
    service_name: &str,
    namespace: &str,
) -> Result<String, AppError> {
    let value = value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map_or_else(|| format!("{service_name}.{namespace}.svc"), str::to_string);
    let url = reqwest::Url::parse(&format!("https://{value}")).map_err(|error| {
        AppError::new(
            "invalid Argo CD TLS server name",
            AppErrorKind::ArgoConnection,
        )
        .with_source(error)
    })?;
    if url.host_str().is_none()
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
    {
        return Err(AppError::new(
            "invalid Argo CD TLS server name",
            AppErrorKind::ArgoConnection,
        ));
    }
    Ok(url.host_str().expect("validated host").to_string())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArgoConnectionProfile {
    pub id: String,
    // Kept during the frontend migration so saved external profiles still display correctly.
    pub url: String,
    pub endpoint: ArgoServerEndpoint,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cluster_context: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kubeconfig_source_key: Option<String>,
    pub transport: String,
    pub remember_credential: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ArgoConnectionProfileWire {
    id: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    endpoint: Option<ArgoServerEndpoint>,
    #[serde(default)]
    cluster_context: Option<String>,
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    kubeconfig_source_key: Option<String>,
    transport: String,
    remember_credential: bool,
}

impl<'de> Deserialize<'de> for ArgoConnectionProfile {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = ArgoConnectionProfileWire::deserialize(deserializer)?;
        let endpoint = wire
            .endpoint
            .unwrap_or_else(|| ArgoServerEndpoint::ExternalHttps {
                url: wire.url.clone(),
            });
        let (endpoint, url) = match endpoint {
            ArgoServerEndpoint::ExternalHttps { url } => {
                (ArgoServerEndpoint::ExternalHttps { url: url.clone() }, url)
            }
            ArgoServerEndpoint::ServiceTunnel {
                namespace,
                service_name,
                service_port,
                scheme,
                root_path,
                tls_server_name,
            } => normalize_service_tunnel_endpoint(
                namespace,
                service_name,
                service_port,
                scheme,
                root_path,
                tls_server_name,
            )
            .map_err(|error| serde::de::Error::custom(error.message))?,
        };
        Ok(Self {
            id: wire.id,
            url,
            endpoint,
            cluster_context: wire.cluster_context,
            workspace_id: wire.workspace_id,
            kubeconfig_source_key: wire.kubeconfig_source_key,
            transport: wire.transport,
            remember_credential: wire.remember_credential,
        })
    }
}

impl Default for ArgoConnectionProfile {
    fn default() -> Self {
        Self {
            id: String::new(),
            url: String::new(),
            endpoint: ArgoServerEndpoint::ExternalHttps { url: String::new() },
            cluster_context: None,
            workspace_id: None,
            kubeconfig_source_key: None,
            transport: String::new(),
            remember_credential: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ArgoConnectionStatus {
    pub profile: Option<ArgoConnectionProfile>,
    pub connected: bool,
    pub username: Option<String>,
    pub unavailable_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ArgoServiceTunnelUnavailableReason {
    ExternalName,
    SelectorRequired,
    NoTcpPorts,
    NoReadyPod,
    TargetUnavailable,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ArgoServerCapability {
    pub id: String,
    pub name: String,
    pub namespace: Option<String>,
    pub url: Option<String>,
    pub transport: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<ArgoServerEndpoint>,
    pub unavailable_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<ArgoServiceTunnelUnavailableReason>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ArgoApplicationRef {
    pub name: String,
    pub namespace: Option<String>,
    pub project: Option<String>,
    pub resource_version: Option<String>,
    pub uid: Option<String>,
    pub api_version: Option<String>,
    pub context: Option<String>,
    pub workspace_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ArgoApplicationHistory {
    pub id: Option<i64>,
    pub revision: Option<String>,
    pub revisions: Vec<String>,
    pub deployed_at: Option<String>,
    pub initiated_by: Option<String>,
    pub source: Option<Value>,
    pub sources: Vec<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ArgoManagedResource {
    pub group: Option<String>,
    pub version: Option<String>,
    pub kind: Option<String>,
    pub namespace: Option<String>,
    pub name: Option<String>,
    pub status: Option<String>,
    pub health: Option<String>,
    pub hook: Option<bool>,
    pub requires_pruning: Option<bool>,
    pub target_state: Option<Value>,
    pub live_state: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ArgoResourceComparison {
    pub resource: ArgoManagedResource,
    pub target_state: Option<Value>,
    pub live_state: Option<Value>,
    pub normalized_live_state: Option<Value>,
    pub predicted_live_state: Option<Value>,
    pub modified: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exact: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<String>,
    #[serde(default)]
    pub available_actions: Vec<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ArgoInspectionFailure {
    pub kind: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ArgoConnectedFallback {
    pub transport: String,
    pub failure: ArgoInspectionFailure,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ArgoApplicationInspector {
    pub application: ArgoApplicationRef,
    pub status: Option<Value>,
    pub history: Vec<ArgoApplicationHistory>,
    pub resources: Vec<ArgoManagedResource>,
    pub comparisons: Vec<ArgoResourceComparison>,
    pub conditions: Vec<Value>,
    pub operation_state: Option<Value>,
    pub connected: bool,
    pub transport: String,
    pub provenance: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connected_fallback: Option<ArgoConnectedFallback>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ArgoOperationRequest {
    pub connection_id: Option<String>,
    pub transport: String,
    pub application: ArgoApplicationRef,
    pub action: String,
    pub revision: Option<String>,
    pub resources: Vec<ArgoManagedResource>,
    pub prune: Option<bool>,
    pub dry_run: Option<bool>,
    pub force: Option<bool>,
    pub history_id: Option<i64>,
    pub resource_action: Option<String>,
    pub resource_action_parameters: Option<Value>,
    pub resource_version: Option<String>,
    pub cluster_context: Option<String>,
    pub kubeconfig_env_var: Option<String>,
    pub sync_payload: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ArgoOperationPreflight {
    pub allowed: bool,
    pub transport: String,
    pub action: String,
    pub reason: Option<String>,
    pub session_id: Option<String>,
    pub expires_at: Option<u64>,
    pub reviewed_request: Option<ArgoOperationRequest>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ArgoOperationConfirmation {
    pub session_id: String,
    pub confirmation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ArgoOperationResult {
    pub accepted: bool,
    pub transport: String,
    pub message: String,
    pub operation: Option<Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tunnel(overrides: Value) -> Value {
        let mut value = json!({"id":"test","endpoint":{"kind":"serviceTunnel","namespace":"argocd","serviceName":"argocd-server","servicePort":443,"scheme":"https"},"transport":"connected","rememberCredential":false});
        if let (Some(base), Some(extra)) = (value.get_mut("endpoint"), overrides.as_object()) {
            base.as_object_mut().unwrap().extend(extra.clone());
        }
        value
    }

    #[test]
    fn service_tunnel_deserialization_rejects_invalid_endpoint_values() {
        for overrides in [
            json!({"scheme":"ftp"}),
            json!({"tlsServerName":"user@argo.example"}),
            json!({"tlsServerName":"argo.example/path"}),
            json!({"rootPath":"/argo?token=secret"}),
        ] {
            assert!(serde_json::from_value::<ArgoConnectionProfile>(tunnel(overrides)).is_err());
        }
    }

    #[test]
    fn service_tunnel_deserialization_normalizes_valid_endpoint() {
        let profile =
            serde_json::from_value::<ArgoConnectionProfile>(tunnel(json!({"rootPath":"/argo/"})))
                .expect("valid service tunnel");
        assert_eq!(profile.url, "https://argocd-server.argocd.svc/argo");
        let endpoint =
            serde_json::to_value(profile).expect("serialize profile")["endpoint"].clone();
        assert!(endpoint.get("serviceName").is_some());
        assert!(endpoint.get("servicePort").is_some());
        assert!(endpoint.get("rootPath").is_some());
        assert!(endpoint.get("tlsServerName").is_some());
        assert!(endpoint.get("service_name").is_none());
    }
}
