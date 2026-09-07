use crate::models::{
    evaluate_health, HealthAssessmentEvidence, HealthAssessmentInput, HealthAssessmentSource,
    HealthAssessmentState, ResourceHealth, ResourceSummary,
};
use serde_json::json;

pub(crate) fn pod_restarts(
    statuses: Option<&[k8s_openapi::api::core::v1::ContainerStatus]>,
) -> Option<i32> {
    let total = statuses?.iter().try_fold(0_i32, |total, status| {
        (status.restart_count >= 0).then_some(())?;
        total.checked_add(status.restart_count)
    })?;
    (total > 0).then_some(total)
}

pub(crate) fn update_resource_health(summary: &mut ResourceSummary) {
    let state = classify_resource_health(summary);
    summary.health_assessment = evaluate_health(HealthAssessmentInput {
        recognized_semantics: summary.status.is_some() || summary.ready.is_some(),
        provider_available: true,
        evidence: vec![
            HealthAssessmentEvidence {
                source: HealthAssessmentSource::Kubernetes,
                raw: json!({ "status": summary.status, "ready": summary.ready }),
                state: resource_health_state(state),
                current: true,
                reason: "Kubernetes status and readiness".to_string(),
            },
            HealthAssessmentEvidence {
                source: HealthAssessmentSource::ContainerRestart,
                raw: json!(summary.restarts),
                state: None,
                current: false,
                reason: "Container restart count is evidence, not health state".to_string(),
            },
        ],
    });
    summary.health = legacy_resource_health(summary.health_assessment.state);
}

pub(crate) fn legacy_resource_health(state: HealthAssessmentState) -> ResourceHealth {
    match state {
        HealthAssessmentState::Healthy => ResourceHealth::Healthy,
        HealthAssessmentState::NeedsAttention => ResourceHealth::Attention,
        HealthAssessmentState::Degraded => ResourceHealth::Degraded,
        HealthAssessmentState::Unknown | HealthAssessmentState::NotEvaluated => {
            ResourceHealth::Unknown
        }
    }
}

fn resource_health_state(health: ResourceHealth) -> Option<HealthAssessmentState> {
    match health {
        ResourceHealth::Healthy => Some(HealthAssessmentState::Healthy),
        ResourceHealth::Attention => Some(HealthAssessmentState::NeedsAttention),
        ResourceHealth::Degraded => Some(HealthAssessmentState::Degraded),
        ResourceHealth::Restarted | ResourceHealth::Unknown => Some(HealthAssessmentState::Unknown),
    }
}

pub(crate) fn classify_resource_health(summary: &ResourceSummary) -> ResourceHealth {
    let status = normalized(summary.status.as_deref());
    let ready = normalized(summary.ready.as_deref());
    let successful_terminal = is_successful_terminal_status(&status);

    if matches!(status.as_str(), "pending" | "terminating") {
        return ResourceHealth::Attention;
    }
    if is_degraded_status(&status) || (!successful_terminal && ready == "false") {
        return ResourceHealth::Degraded;
    }
    if !successful_terminal && has_incomplete_ready_ratio(&ready) {
        return ResourceHealth::Attention;
    }
    if successful_terminal
        || matches!(
            status.as_str(),
            "running" | "ready" | "true" | "ready: true"
        )
        || ready == "true"
        || has_complete_ready_ratio(&ready)
    {
        return ResourceHealth::Healthy;
    }

    ResourceHealth::Unknown
}

fn normalized(value: Option<&str>) -> String {
    value.unwrap_or_default().trim().to_ascii_lowercase()
}

fn is_successful_terminal_status(status: &str) -> bool {
    matches!(status, "succeeded" | "complete" | "completed")
}

fn is_degraded_status(status: &str) -> bool {
    matches!(
        status,
        "failed" | "error" | "crashloopbackoff" | "imagepullbackoff"
    )
}

fn has_incomplete_ready_ratio(ready: &str) -> bool {
    ready_ratio(ready).is_some_and(|(ready_count, desired_count)| {
        desired_count > 0 && ready_count < desired_count
    })
}

fn has_complete_ready_ratio(ready: &str) -> bool {
    ready_ratio(ready).is_some_and(|(ready_count, desired_count)| {
        desired_count > 0 && ready_count >= desired_count
    })
}

fn ready_ratio(ready: &str) -> Option<(i32, i32)> {
    let (ready_count, desired_count) = ready.split_once('/')?;
    Some((ready_count.parse().ok()?, desired_count.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pod_restarts_preserves_counts_without_overflow_or_negative_values() {
        use k8s_openapi::api::core::v1::ContainerStatus;
        let statuses = |counts: &[i32]| {
            counts
                .iter()
                .map(|count| ContainerStatus {
                    restart_count: *count,
                    ..Default::default()
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(pod_restarts(Some(&statuses(&[2, 3]))), Some(5));
        assert_eq!(pod_restarts(Some(&statuses(&[0]))), None);
        assert_eq!(pod_restarts(None), None);
        assert_eq!(pod_restarts(Some(&statuses(&[i32::MAX, 1]))), None);
        assert_eq!(pod_restarts(Some(&statuses(&[-1, 2]))), None);
    }

    fn summary(status: &str, ready: &str, restarts: Option<i32>) -> ResourceSummary {
        ResourceSummary {
            kind: "Pod".to_string(),
            cluster: "kind-dev".to_string(),
            name: "job-pod".to_string(),
            namespace: Some("default".to_string()),
            age: "1m".to_string(),
            api_version: Some("v1".to_string()),
            group: None,
            version: Some("v1".to_string()),
            plural: Some("pods".to_string()),
            namespaced: Some(true),
            dynamic: None,
            health: ResourceHealth::Unknown,
            health_assessment: crate::models::HealthAssessment::default(),
            created_at: None,
            status: Some(status.to_string()),
            ready: Some(ready.to_string()),
            restarts,
            owner_ref: None,
            argo_app: None,
            helm_release: None,
            git_ops_owner: None,
            git_ops_ownership_partial: false,
        }
    }

    #[test]
    fn succeeded_pod_with_false_ready_is_healthy() {
        assert_eq!(
            classify_resource_health(&summary("Succeeded", "False", None)),
            ResourceHealth::Healthy
        );
    }

    #[test]
    fn succeeded_pod_with_restart_history_is_healthy() {
        assert_eq!(
            classify_resource_health(&summary("Succeeded", "False", Some(2))),
            ResourceHealth::Healthy
        );
    }

    #[test]
    fn running_pod_with_false_ready_is_degraded() {
        assert_eq!(
            classify_resource_health(&summary("Running", "False", None)),
            ResourceHealth::Degraded
        );
    }

    #[test]
    fn pending_pod_with_false_ready_needs_attention() {
        assert_eq!(
            classify_resource_health(&summary("Pending", "False", None)),
            ResourceHealth::Attention
        );
    }

    #[test]
    fn unknown_status_stays_unknown() {
        assert_eq!(
            classify_resource_health(&summary("Unknown", "", None)),
            ResourceHealth::Unknown
        );
    }

    #[test]
    fn incomplete_ready_ratio_needs_attention() {
        assert_eq!(
            classify_resource_health(&summary("Running", "0/3", None)),
            ResourceHealth::Attention
        );
    }
}
