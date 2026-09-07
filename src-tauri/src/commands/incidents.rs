use crate::models::AppErrorKind;
mod health;
mod restarts;

use super::{resource_scope_from, ClusterLiveStore};
use crate::commands::{
    helpers::{k8s_timestamp_to_datetime, list_params, resource_age},
    kubeconfig::KubeconfigSource,
};
use crate::models::{
    AppError, IncidentCockpitItem, IncidentCockpitSummary, IncidentSeverity, IncidentSignalState,
    IncidentSignalSummary, ResourceEventSummary, ResourceListRequest, ResourceSummary,
};
use chrono::{DateTime, Utc};
use futures_util::future::join_all;
use k8s_openapi::api::core::v1::Event;
use kube::{api::Api, Client};
use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;
use tauri::State;

use health::{
    apply_incident_health_evidence, current_status_severity, has_current_kubernetes_concern,
    severity_for,
};
use restarts::{
    list_restart_evidence, resource_key, restart_signal_label, restart_signal_message,
    RestartEvidence,
};

const MAX_WARNING_EVENTS_PER_RESOURCE: usize = 3;
const MAX_WARNING_EVENTS_TOTAL: usize = 500;

#[tauri::command]
pub async fn list_incident_cockpit(
    cluster_context: String,
    requests: Vec<ResourceListRequest>,
    kubeconfig_env_var: Option<String>,
    live_store: State<'_, ClusterLiveStore>,
) -> Result<IncidentCockpitSummary, AppError> {
    let started = Instant::now();
    eprintln!(
        "[kubecove:backend] list_incident_cockpit start context={} requests={}",
        cluster_context,
        requests.len()
    );
    let result = incident_cockpit_from(
        cluster_context.clone(),
        requests,
        kubeconfig_env_var,
        live_store.inner().clone(),
    )
    .await;
    match &result {
        Ok(summary) => eprintln!(
            "[kubecove:backend] list_incident_cockpit done context={} items={} warnings={} ms={}",
            cluster_context,
            summary.items.len(),
            summary.warnings.len(),
            started.elapsed().as_millis()
        ),
        Err(err) => eprintln!(
            "[kubecove:backend] list_incident_cockpit error context={} error_kind={} message={} ms={}",
            cluster_context,
            err.kind,
            err.message,
            started.elapsed().as_millis()
        ),
    }
    result
}

pub async fn incident_cockpit_from(
    cluster_context: String,
    requests: Vec<ResourceListRequest>,
    kubeconfig_env_var: Option<String>,
    live_store: ClusterLiveStore,
) -> Result<IncidentCockpitSummary, AppError> {
    let resources = resource_scope_from(
        cluster_context.clone(),
        requests.clone(),
        live_store,
        kubeconfig_env_var.clone(),
    )
    .await?;
    let client = client_for_context(&cluster_context, kubeconfig_env_var).await?;
    let event_scope = event_namespace_scope(&requests, &resources);
    let mut warnings = Vec::new();
    let (restart_result, event_result) = tokio::join!(
        list_restart_evidence(client.clone(), &resources),
        list_warning_events(client, event_scope),
    );
    let restart_result = restart_result?;
    let event_result = match event_result {
        Ok(result) => result,
        Err(err) if is_forbidden_app_error(&err) => {
            warnings.push("Warning events unavailable: forbidden by RBAC.".to_string());
            WarningEventList::default()
        }
        Err(err) => return Err(err),
    };
    if !event_result.denied_namespaces.is_empty() {
        warnings.push(format!(
            "Warning events unavailable in namespaces: {}.",
            event_result.denied_namespaces.join(", ")
        ));
    }
    let events = event_result.events;
    if events.len() >= MAX_WARNING_EVENTS_TOTAL {
        warnings.push(format!(
            "Warning events capped at {MAX_WARNING_EVENTS_TOTAL} most recent matches."
        ));
    }

    Ok(IncidentCockpitSummary {
        cluster: cluster_context,
        generated_at: Utc::now().to_rfc3339(),
        requested_scope: requests,
        items: build_incident_items(resources, events, restart_result.by_resource),
        warnings,
    })
}

fn is_forbidden_app_error(error: &AppError) -> bool {
    error.kind == AppErrorKind::Forbidden
}

#[derive(Debug, Clone)]
enum EventNamespaceScope {
    All,
    Namespaces(Vec<String>),
    None,
}

fn event_namespace_scope(
    requests: &[ResourceListRequest],
    resources: &[ResourceSummary],
) -> EventNamespaceScope {
    if requests.iter().any(|request| request.namespace.is_none()) {
        return EventNamespaceScope::All;
    }
    let namespaces = resources
        .iter()
        .filter_map(|resource| resource.namespace.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    if namespaces.is_empty() {
        EventNamespaceScope::None
    } else {
        EventNamespaceScope::Namespaces(namespaces)
    }
}

async fn client_for_context(
    cluster_context: &str,
    kubeconfig_env_var: Option<String>,
) -> Result<Client, AppError> {
    let source = KubeconfigSource::new(kubeconfig_env_var)?;
    source.client_for_context(cluster_context).await
}

async fn list_warning_events(
    client: Client,
    scope: EventNamespaceScope,
) -> Result<WarningEventList, AppError> {
    let namespace_scopes = match scope {
        EventNamespaceScope::All => vec![None],
        EventNamespaceScope::Namespaces(namespaces) => namespaces.into_iter().map(Some).collect(),
        EventNamespaceScope::None => return Ok(WarningEventList::default()),
    };
    let mut events = Vec::new();
    let mut denied_namespaces = Vec::new();
    let fetches = namespace_scopes.into_iter().map(|namespace| {
        let client = client.clone();
        async move {
            let api: Api<Event> = if let Some(namespace) = &namespace {
                Api::namespaced(client, namespace)
            } else {
                Api::all(client)
            };
            (namespace, api.list(&list_params()).await)
        }
    });
    for (namespace, result) in join_all(fetches).await {
        let list = match result {
            Ok(list) => list,
            Err(err) => {
                let app_error = AppError::from(err);
                if let Some(namespace) = namespace.as_ref() {
                    if is_forbidden_app_error(&app_error) {
                        denied_namespaces.push(namespace.clone());
                        continue;
                    }
                }
                return Err(app_error);
            }
        };
        events.extend(list.into_iter().filter_map(event_match));
    }
    events.sort_by(event_match_sort);
    events.truncate(MAX_WARNING_EVENTS_TOTAL);
    Ok(WarningEventList {
        events,
        denied_namespaces,
    })
}

#[derive(Debug, Clone, Default)]
struct WarningEventList {
    events: Vec<ResourceEventMatch>,
    denied_namespaces: Vec<String>,
}

#[derive(Debug, Clone)]
struct ResourceEventMatch {
    key: String,
    summary: ResourceEventSummary,
}

fn event_timestamp(event: &Event) -> Option<DateTime<Utc>> {
    event
        .event_time
        .as_ref()
        .and_then(|t| k8s_timestamp_to_datetime(&t.0))
        .or_else(|| {
            event
                .last_timestamp
                .as_ref()
                .and_then(|t| k8s_timestamp_to_datetime(&t.0))
        })
        .or_else(|| {
            event
                .first_timestamp
                .as_ref()
                .and_then(|t| k8s_timestamp_to_datetime(&t.0))
        })
        .or_else(|| {
            event
                .metadata
                .creation_timestamp
                .as_ref()
                .and_then(|t| k8s_timestamp_to_datetime(&t.0))
        })
}

fn event_source(event: &Event) -> String {
    if let Some(component) = &event.reporting_component {
        if !component.is_empty() {
            return component.clone();
        }
    }
    event
        .source
        .as_ref()
        .and_then(|source| source.component.clone())
        .unwrap_or_else(|| "unknown".to_string())
}

fn event_match(event: Event) -> Option<ResourceEventMatch> {
    if event.type_.as_deref() != Some("Warning") {
        return None;
    }
    let kind = event.involved_object.kind.clone()?;
    let name = event.involved_object.name.clone()?;
    let api_version = event.involved_object.api_version.clone();
    let namespace = event.involved_object.namespace.clone();
    let timestamp = event_timestamp(&event);
    let last_seen_at = timestamp.as_ref().map(DateTime::to_rfc3339);
    let last_seen = resource_age(timestamp);
    let source = event_source(&event);
    let summary = ResourceEventSummary {
        event_type: event.type_.unwrap_or_else(|| "Warning".to_string()),
        reason: event.reason.unwrap_or_else(|| "Event".to_string()),
        message: event.message.unwrap_or_default(),
        count: event.count.unwrap_or(1),
        last_seen,
        last_seen_at,
        source,
        namespace: event.metadata.namespace.clone().or(namespace.clone()),
    };
    Some(ResourceEventMatch {
        key: resource_match_key(&kind, api_version.as_deref(), namespace.as_deref(), &name),
        summary,
    })
}

fn event_match_sort(a: &ResourceEventMatch, b: &ResourceEventMatch) -> std::cmp::Ordering {
    event_time_ms(&b.summary)
        .cmp(&event_time_ms(&a.summary))
        .then_with(|| b.summary.count.cmp(&a.summary.count))
        .then_with(|| a.summary.reason.cmp(&b.summary.reason))
}

fn event_time_ms(event: &ResourceEventSummary) -> i64 {
    event
        .last_seen_at
        .as_deref()
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|dt| dt.timestamp_millis())
        .unwrap_or_default()
}

fn resource_match_key(
    kind: &str,
    api_version: Option<&str>,
    namespace: Option<&str>,
    name: &str,
) -> String {
    format!(
        "{}:{}:{}:{}",
        kind,
        api_version.unwrap_or_default(),
        namespace.unwrap_or_default(),
        name
    )
}

fn build_incident_items(
    resources: Vec<ResourceSummary>,
    warning_events: Vec<ResourceEventMatch>,
    restart_evidence: BTreeMap<String, RestartEvidence>,
) -> Vec<IncidentCockpitItem> {
    let mut events_by_resource: BTreeMap<String, Vec<ResourceEventSummary>> = BTreeMap::new();
    for event in warning_events {
        events_by_resource
            .entry(event.key)
            .or_default()
            .push(event.summary);
    }

    let mut items = resources
        .into_iter()
        .filter_map(|resource| {
            let key = resource_key(&resource);
            let events = events_by_resource.remove(&key).unwrap_or_default();
            incident_item(resource, events, restart_evidence.get(&key))
        })
        .collect::<Vec<_>>();
    items.sort_by(incident_item_sort);
    items
}

fn incident_item(
    mut resource: ResourceSummary,
    warning_events: Vec<ResourceEventSummary>,
    restart_evidence: Option<&RestartEvidence>,
) -> Option<IncidentCockpitItem> {
    apply_incident_health_evidence(&mut resource, &warning_events, restart_evidence);
    let mut signals = Vec::new();
    let status_severity = current_status_severity(&resource);
    if status_severity == Some(IncidentSeverity::Degraded) {
        signals.push(IncidentSignalSummary {
            kind: "status".to_string(),
            label: "Degraded".to_string(),
            message: status_message(&resource),
            source: "status".to_string(),
            state: IncidentSignalState::Active,
            last_seen_at: resource.created_at.clone(),
        });
    } else if status_severity == Some(IncidentSeverity::Attention) {
        signals.push(IncidentSignalSummary {
            kind: "status".to_string(),
            label: "Needs attention".to_string(),
            message: status_message(&resource),
            source: "status".to_string(),
            state: IncidentSignalState::Active,
            last_seen_at: resource.created_at.clone(),
        });
    }
    if let Some(evidence) = restart_evidence.cloned() {
        signals.push(IncidentSignalSummary {
            kind: "restart".to_string(),
            label: restart_signal_label(evidence.state).to_string(),
            message: restart_signal_message(&resource, &evidence),
            source: "status".to_string(),
            state: evidence.state,
            last_seen_at: evidence.last_seen_at,
        });
    }
    let warning_state = if has_current_kubernetes_concern(&resource) {
        IncidentSignalState::Active
    } else {
        IncidentSignalState::Historical
    };
    for event in warning_events.iter().take(MAX_WARNING_EVENTS_PER_RESOURCE) {
        signals.push(IncidentSignalSummary {
            kind: "event".to_string(),
            label: event.reason.clone(),
            message: event.message.clone(),
            source: event.source.clone(),
            state: warning_state,
            last_seen_at: event.last_seen_at.clone(),
        });
    }

    let warning_event_count = warning_events.len();
    let latest_warning_event = warning_events.first().cloned();
    let severity = severity_for(
        status_severity,
        latest_warning_event.is_some(),
        restart_evidence.is_some(),
    )?;
    let state = signals
        .iter()
        .map(|signal| signal.state)
        .max_by_key(|state| state_weight(*state))
        .unwrap_or(IncidentSignalState::Historical);
    let latest_signal_at = latest_signal_at(&signals);

    Some(IncidentCockpitItem {
        resource,
        severity,
        state,
        signals,
        warning_event_count,
        latest_signal_at,
        latest_warning_event,
    })
}

fn status_message(resource: &ResourceSummary) -> String {
    [
        resource
            .status
            .as_ref()
            .map(|status| format!("Status {status}")),
        resource
            .ready
            .as_ref()
            .map(|ready| format!("Ready {ready}")),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ")
}

fn severity_weight(severity: IncidentSeverity) -> u8 {
    match severity {
        IncidentSeverity::Degraded => 4,
        IncidentSeverity::Attention => 3,
        IncidentSeverity::Restarted => 2,
        IncidentSeverity::Warning => 1,
    }
}

fn state_weight(state: IncidentSignalState) -> u8 {
    match state {
        IncidentSignalState::Active => 3,
        IncidentSignalState::Resolved => 2,
        IncidentSignalState::Historical => 1,
    }
}

fn incident_item_sort(a: &IncidentCockpitItem, b: &IncidentCockpitItem) -> std::cmp::Ordering {
    state_weight(b.state)
        .cmp(&state_weight(a.state))
        .then_with(|| severity_weight(b.severity).cmp(&severity_weight(a.severity)))
        .then_with(|| latest_item_signal_time_ms(b).cmp(&latest_item_signal_time_ms(a)))
        .then_with(|| a.resource.namespace.cmp(&b.resource.namespace))
        .then_with(|| a.resource.kind.cmp(&b.resource.kind))
        .then_with(|| a.resource.name.cmp(&b.resource.name))
}

fn latest_item_signal_time_ms(item: &IncidentCockpitItem) -> i64 {
    item.latest_signal_at
        .as_deref()
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|dt| dt.timestamp_millis())
        .unwrap_or_default()
}

fn signal_time_ms(signal: &IncidentSignalSummary) -> i64 {
    signal
        .last_seen_at
        .as_deref()
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|dt| dt.timestamp_millis())
        .unwrap_or_default()
}

fn latest_signal_at(signals: &[IncidentSignalSummary]) -> Option<String> {
    signals
        .iter()
        .filter(|signal| signal.last_seen_at.is_some())
        .max_by_key(|signal| signal_time_ms(signal))
        .and_then(|signal| signal.last_seen_at.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::helpers::update_resource_health;
    use crate::models::{HealthAssessmentState, ResourceHealth};

    #[test]
    fn permission_classification_uses_kind_not_resource_name() {
        let error = AppError::new("pod forbidden-403 not found", AppErrorKind::NotFound);
        assert!(!is_forbidden_app_error(&error));
    }

    fn resource(name: &str) -> ResourceSummary {
        let mut resource = ResourceSummary {
            kind: "Pod".to_string(),
            cluster: "kind-admin".to_string(),
            name: name.to_string(),
            namespace: Some("default".to_string()),
            age: "1m".to_string(),
            api_version: Some("v1".to_string()),
            group: None,
            version: Some("v1".to_string()),
            plural: Some("pods".to_string()),
            namespaced: Some(true),
            dynamic: None,
            health: ResourceHealth::Healthy,
            health_assessment: crate::models::HealthAssessment::default(),
            created_at: Some("2026-06-04T10:00:00Z".to_string()),
            status: Some("Running".to_string()),
            ready: Some("true".to_string()),
            restarts: None,
            owner_ref: None,
            argo_app: None,
            helm_release: None,
            git_ops_owner: None,
            git_ops_ownership_partial: false,
        };
        update_resource_health(&mut resource);
        resource
    }

    fn warning(key_name: &str, reason: &str, at: &str) -> ResourceEventMatch {
        ResourceEventMatch {
            key: resource_match_key("Pod", Some("v1"), Some("default"), key_name),
            summary: ResourceEventSummary {
                event_type: "Warning".to_string(),
                reason: reason.to_string(),
                message: "Back-off restarting failed container".to_string(),
                count: 3,
                last_seen: "1m".to_string(),
                last_seen_at: Some(at.to_string()),
                source: "kubelet".to_string(),
                namespace: Some("default".to_string()),
            },
        }
    }

    fn build_items(
        resources: Vec<ResourceSummary>,
        warning_events: Vec<ResourceEventMatch>,
    ) -> Vec<IncidentCockpitItem> {
        build_incident_items(resources, warning_events, BTreeMap::new())
    }

    fn build_items_with_restarts(
        resources: Vec<ResourceSummary>,
        restart_evidence: BTreeMap<String, RestartEvidence>,
    ) -> Vec<IncidentCockpitItem> {
        build_incident_items(resources, Vec::new(), restart_evidence)
    }

    #[test]
    fn degraded_status_wins_over_warning_event() {
        let mut pod = resource("api-0");
        pod.status = Some("CrashLoopBackOff".to_string());
        pod.ready = Some("false".to_string());
        update_resource_health(&mut pod);

        let items = build_items(
            vec![pod],
            vec![warning("api-0", "BackOff", "2026-06-04T10:01:00Z")],
        );

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].severity, IncidentSeverity::Degraded);
        assert_eq!(items[0].warning_event_count, 1);
        assert_eq!(
            items[0].latest_signal_at.as_deref(),
            Some("2026-06-04T10:01:00Z")
        );
        assert!(items[0].signals.iter().any(|signal| signal.kind == "event"));
    }

    #[test]
    fn uncorroborated_warning_stays_healthy_and_historical() {
        let items = build_items(
            vec![resource("api-0")],
            vec![warning("api-0", "FailedMount", "2026-06-04T10:01:00Z")],
        );

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].severity, IncidentSeverity::Warning);
        assert_eq!(items[0].state, IncidentSignalState::Historical);
        assert_eq!(
            items[0].resource.health_assessment.state,
            HealthAssessmentState::Healthy
        );
        assert_eq!(items[0].resource.health, ResourceHealth::Healthy);
        assert_eq!(items[0].warning_event_count, 1);
        assert_eq!(
            items[0].latest_signal_at.as_deref(),
            Some("2026-06-04T10:01:00Z")
        );
        assert_eq!(
            items[0].latest_warning_event.as_ref().unwrap().reason,
            "FailedMount"
        );
    }

    #[test]
    fn active_restart_worsens_healthy_resource_to_needs_attention() {
        let resource = resource("api-0");
        let mut restarts = BTreeMap::new();
        restarts.insert(
            resource_key(&resource),
            RestartEvidence {
                state: IncidentSignalState::Active,
                last_seen_at: Some("2026-06-04T10:01:00Z".to_string()),
                container: Some("api".to_string()),
                reason: Some("CrashLoopBackOff".to_string()),
                exit_code: Some(1),
            },
        );

        let items = build_items_with_restarts(vec![resource], restarts);

        assert_eq!(items[0].severity, IncidentSeverity::Attention);
        assert_eq!(
            items[0].resource.health_assessment.state,
            HealthAssessmentState::NeedsAttention
        );
        assert_eq!(items[0].resource.health, ResourceHealth::Attention);
    }

    #[test]
    fn resolved_restart_does_not_worsen_healthy_resource() {
        let resource = resource("api-0");
        let mut restarts = BTreeMap::new();
        restarts.insert(
            resource_key(&resource),
            RestartEvidence {
                state: IncidentSignalState::Resolved,
                last_seen_at: Some("2026-06-04T10:01:00Z".to_string()),
                container: Some("api".to_string()),
                reason: Some("Error".to_string()),
                exit_code: Some(1),
            },
        );

        let items = build_items_with_restarts(vec![resource], restarts);

        assert_eq!(items[0].severity, IncidentSeverity::Restarted);
        assert_eq!(items[0].state, IncidentSignalState::Resolved);
        assert_eq!(
            items[0].resource.health_assessment.state,
            HealthAssessmentState::Healthy
        );
        assert_eq!(items[0].resource.health, ResourceHealth::Healthy);
    }

    #[test]
    fn healthy_resource_without_warning_is_omitted() {
        let items = build_items(vec![resource("api-0")], Vec::new());

        assert!(items.is_empty());
    }

    #[test]
    fn succeeded_pod_with_false_ready_is_omitted_without_warning() {
        let mut pod = resource("job-pod");
        pod.status = Some("Succeeded".to_string());
        pod.ready = Some("False".to_string());
        update_resource_health(&mut pod);

        let items = build_items(vec![pod], Vec::new());

        assert!(items.is_empty());
    }

    #[test]
    fn workload_ready_ratio_below_desired_becomes_attention_item() {
        let mut deployment = resource("api");
        deployment.kind = "Deployment".to_string();
        deployment.api_version = Some("apps/v1".to_string());
        deployment.group = Some("apps".to_string());
        deployment.plural = Some("deployments".to_string());
        deployment.ready = Some("0/3".to_string());
        update_resource_health(&mut deployment);

        let items = build_items(vec![deployment], Vec::new());

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].severity, IncidentSeverity::Attention);
        assert_eq!(items[0].warning_event_count, 0);
        assert_eq!(
            items[0].latest_signal_at.as_deref(),
            Some("2026-06-04T10:00:00Z")
        );
        assert_eq!(items[0].signals[0].label, "Needs attention");
        assert!(items[0].signals[0].message.contains("Ready 0/3"));
    }

    #[test]
    fn items_sort_by_severity_then_recent_signal() {
        let mut restarted = resource("api-1");
        restarted.restarts = Some(2);
        update_resource_health(&mut restarted);
        let mut failed = resource("api-0");
        failed.status = Some("Failed".to_string());
        update_resource_health(&mut failed);

        let items = build_items(
            vec![restarted, failed, resource("api-2"), resource("api-3")],
            vec![
                warning("api-2", "BackOff", "2026-06-04T10:00:00Z"),
                warning("api-3", "FailedMount", "2026-06-04T10:02:00Z"),
            ],
        );

        assert_eq!(items[0].resource.name, "api-0");
        assert_eq!(items[1].resource.name, "api-3");
        assert_eq!(items[2].resource.name, "api-2");
        assert_eq!(items.len(), 3);
    }

    #[test]
    fn warning_event_count_preserves_all_matched_events() {
        let items = build_items(
            vec![resource("api-0")],
            vec![
                warning("api-0", "BackOff", "2026-06-04T10:03:00Z"),
                warning("api-0", "FailedMount", "2026-06-04T10:02:00Z"),
                warning("api-0", "FailedPull", "2026-06-04T10:01:00Z"),
                warning("api-0", "Unhealthy", "2026-06-04T10:00:00Z"),
            ],
        );

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].warning_event_count, 4);
        assert_eq!(
            items[0].latest_signal_at.as_deref(),
            Some("2026-06-04T10:03:00Z")
        );
        assert_eq!(
            items[0]
                .signals
                .iter()
                .filter(|signal| signal.kind == "event")
                .count(),
            MAX_WARNING_EVENTS_PER_RESOURCE
        );
    }

    #[test]
    fn namespace_scoped_empty_resources_do_not_query_all_events() {
        let scope = event_namespace_scope(
            &[ResourceListRequest {
                kind: Some("Pod".to_string()),
                namespace: Some("default".to_string()),
                resource_kind: None,
            }],
            &[],
        );

        assert!(matches!(scope, EventNamespaceScope::None));
    }

    #[test]
    fn warning_events_match_resources_by_api_version() {
        let mut apps = resource("api");
        apps.api_version = Some("apps/v1".to_string());
        let mut batch = resource("api");
        batch.api_version = Some("batch/v1".to_string());
        let events = vec![ResourceEventMatch {
            key: resource_match_key("Pod", Some("batch/v1"), Some("default"), "api"),
            summary: ResourceEventSummary {
                event_type: "Warning".to_string(),
                reason: "FailedCreate".to_string(),
                message: "batch resource warning".to_string(),
                count: 1,
                last_seen: "1m".to_string(),
                last_seen_at: Some("2026-06-04T10:03:00Z".to_string()),
                source: "controller".to_string(),
                namespace: Some("default".to_string()),
            },
        }];

        let items = build_items(vec![apps, batch], events);

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].resource.api_version.as_deref(), Some("batch/v1"));
        assert_eq!(
            items[0].latest_warning_event.as_ref().unwrap().reason,
            "FailedCreate"
        );
    }
}
