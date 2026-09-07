use crate::models::AppErrorKind;
use crate::{
    commands::{
        diagnostic_field, diagnostics::record_backend_result, kubeconfig::KubeconfigSource,
    },
    models::{
        AppError, RbacAccessReviewIdentity, RbacAccessReviewOutcome, RbacAccessReviewRequest,
        RbacAccessReviewResult, RbacAccessReviewTarget,
    },
};
use k8s_openapi::api::authorization::v1::{
    NonResourceAttributes, ResourceAttributes, SubjectAccessReview, SubjectAccessReviewSpec,
    SubjectAccessReviewStatus,
};
use kube::api::{Api, PostParams};
use std::time::Instant;
use tauri::State;

#[tauri::command]
pub async fn review_rbac_access(
    request: RbacAccessReviewRequest,
    request_id: Option<String>,
    cancel_scope: Option<String>,
    cancellations: State<'_, crate::commands::BackendCancellationRegistry>,
) -> Result<RbacAccessReviewResult, AppError> {
    let started = Instant::now();
    let result = cancellations
        .execute(cancel_scope, request_id, review_rbac_access_from(request))
        .await;
    record_backend_result("review_rbac_access", started, &result, |result| {
        vec![diagnostic_field("outcome", format!("{:?}", result.outcome))]
    });
    result
}

async fn review_rbac_access_from(
    request: RbacAccessReviewRequest,
) -> Result<RbacAccessReviewResult, AppError> {
    let client = KubeconfigSource::new(request.kubeconfig_env_var)?
        .client_for_context(&request.cluster_context)
        .await?;
    let (user, groups) = review_identity(&request.identity)?;
    let review = SubjectAccessReview {
        metadata: k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta::default(),
        spec: SubjectAccessReviewSpec {
            extra: None,
            groups: (!groups.is_empty()).then_some(groups),
            non_resource_attributes: target_non_resource_attributes(&request.target)?,
            resource_attributes: target_resource_attributes(&request.target)?,
            uid: None,
            user,
        },
        status: None,
    };
    let result = Api::<SubjectAccessReview>::all(client)
        .create(&PostParams::default(), &review)
        .await
        .map_err(AppError::from)?;
    review_result(result.status)
}

fn review_result(
    status: Option<SubjectAccessReviewStatus>,
) -> Result<RbacAccessReviewResult, AppError> {
    let status = status.ok_or_else(|| {
        AppError::new(
            "SubjectAccessReview returned no status",
            AppErrorKind::Cluster,
        )
    })?;
    Ok(RbacAccessReviewResult {
        outcome: if status.allowed {
            RbacAccessReviewOutcome::Allowed
        } else if status.denied.unwrap_or(false) {
            RbacAccessReviewOutcome::Denied
        } else {
            RbacAccessReviewOutcome::NoOpinion
        },
        reason: status.reason,
        evaluation_error: status.evaluation_error,
    })
}

fn review_identity(
    identity: &RbacAccessReviewIdentity,
) -> Result<(Option<String>, Vec<String>), AppError> {
    match identity {
        RbacAccessReviewIdentity::ServiceAccount { name, namespace } => {
            required("service account name", name)?;
            required("service account namespace", namespace)?;
            Ok((
                Some(format!("system:serviceaccount:{namespace}:{name}")),
                vec![
                    "system:serviceaccounts".to_string(),
                    format!("system:serviceaccounts:{namespace}"),
                    "system:authenticated".to_string(),
                ],
            ))
        }
        RbacAccessReviewIdentity::User { username, groups } => {
            required("username", username)?;
            Ok((Some(username.trim().to_string()), clean_groups(groups)))
        }
        RbacAccessReviewIdentity::Group { group } => {
            required("group", group)?;
            Ok((None, vec![group.trim().to_string()]))
        }
    }
}

fn target_resource_attributes(
    target: &RbacAccessReviewTarget,
) -> Result<Option<ResourceAttributes>, AppError> {
    let RbacAccessReviewTarget::Resource {
        verb,
        resource,
        api_group,
        namespace,
        subresource,
        name,
    } = target
    else {
        return Ok(None);
    };
    required("resource verb", verb)?;
    required("resource", resource)?;
    Ok(Some(ResourceAttributes {
        group: (!api_group.trim().is_empty()).then_some(api_group.trim().to_string()),
        name: nonempty(name),
        namespace: nonempty(namespace),
        resource: Some(resource.trim().to_string()),
        subresource: nonempty(subresource),
        verb: Some(verb.trim().to_string()),
        version: None,
        field_selector: None,
        label_selector: None,
    }))
}

fn target_non_resource_attributes(
    target: &RbacAccessReviewTarget,
) -> Result<Option<NonResourceAttributes>, AppError> {
    let RbacAccessReviewTarget::NonResource {
        verb,
        non_resource_url,
    } = target
    else {
        return Ok(None);
    };
    required("non-resource verb", verb)?;
    if !non_resource_url.trim().starts_with('/') {
        return Err(AppError::new(
            "non-resource URL must start with /",
            AppErrorKind::Validation,
        ));
    }
    Ok(Some(NonResourceAttributes {
        path: Some(non_resource_url.trim().to_string()),
        verb: Some(verb.trim().to_string()),
    }))
}

fn required(label: &str, value: &str) -> Result<(), AppError> {
    if value.trim().is_empty() {
        Err(AppError::new(
            format!("{label} is required"),
            AppErrorKind::Validation,
        ))
    } else {
        Ok(())
    }
}

fn nonempty(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn clean_groups(groups: &[String]) -> Vec<String> {
    let mut groups: Vec<_> = groups
        .iter()
        .map(|group| group.trim())
        .filter(|group| !group.is_empty())
        .map(str::to_string)
        .collect();
    groups.sort();
    groups.dedup();
    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_account_uses_canonical_identity_and_groups() {
        let (user, groups) = review_identity(&RbacAccessReviewIdentity::ServiceAccount {
            name: "api".into(),
            namespace: "payments".into(),
        })
        .unwrap();
        assert_eq!(user.as_deref(), Some("system:serviceaccount:payments:api"));
        assert!(groups.contains(&"system:authenticated".to_string()));
    }

    #[test]
    fn group_does_not_invent_user() {
        let (user, groups) = review_identity(&RbacAccessReviewIdentity::Group {
            group: "ops".into(),
        })
        .unwrap();
        assert_eq!(user, None);
        assert_eq!(groups, vec!["ops"]);
    }

    #[test]
    fn resource_allows_explicit_cluster_scope_with_null_namespace() {
        let target = RbacAccessReviewTarget::Resource {
            verb: "get".into(),
            resource: "pods".into(),
            api_group: String::new(),
            namespace: None,
            subresource: None,
            name: None,
        };
        assert!(target_resource_attributes(&target).is_ok());
    }

    #[test]
    fn validates_non_resource_paths_and_preserves_resource_fields() {
        let target = RbacAccessReviewTarget::NonResource {
            verb: "get".into(),
            non_resource_url: "/healthz".into(),
        };
        assert_eq!(
            target_non_resource_attributes(&target)
                .unwrap()
                .unwrap()
                .path
                .as_deref(),
            Some("/healthz")
        );
        let invalid = RbacAccessReviewTarget::NonResource {
            verb: "get".into(),
            non_resource_url: "healthz".into(),
        };
        assert!(target_non_resource_attributes(&invalid).is_err());
        let resource = RbacAccessReviewTarget::Resource {
            verb: "get".into(),
            resource: "pods".into(),
            api_group: "apps".into(),
            namespace: Some("team".into()),
            subresource: Some("log".into()),
            name: Some("api".into()),
        };
        let attrs = target_resource_attributes(&resource).unwrap().unwrap();
        assert_eq!(attrs.group.as_deref(), Some("apps"));
        assert_eq!(attrs.subresource.as_deref(), Some("log"));
        assert_eq!(attrs.name.as_deref(), Some("api"));
    }

    #[test]
    fn users_clean_groups_and_preserve_anonymous_group() {
        let (user, groups) = review_identity(&RbacAccessReviewIdentity::User {
            username: "system:anonymous".into(),
            groups: vec![
                "system:unauthenticated".into(),
                "system:unauthenticated".into(),
                " ".into(),
            ],
        })
        .unwrap();
        assert_eq!(user.as_deref(), Some("system:anonymous"));
        assert_eq!(groups, vec!["system:unauthenticated"]);
    }

    #[test]
    fn maps_allowed_denied_no_opinion_and_missing_status() {
        let status = |allowed, denied| SubjectAccessReviewStatus {
            allowed,
            denied,
            reason: Some("reason".into()),
            evaluation_error: None,
        };
        assert_eq!(
            review_result(Some(status(true, None))).unwrap().outcome,
            RbacAccessReviewOutcome::Allowed
        );
        assert_eq!(
            review_result(Some(status(false, Some(true))))
                .unwrap()
                .outcome,
            RbacAccessReviewOutcome::Denied
        );
        assert_eq!(
            review_result(Some(status(false, None))).unwrap().outcome,
            RbacAccessReviewOutcome::NoOpinion
        );
        assert!(review_result(None).is_err());
    }

    #[test]
    fn preserves_evaluation_error() {
        let result = review_result(Some(SubjectAccessReviewStatus {
            allowed: false,
            denied: None,
            reason: None,
            evaluation_error: Some("authorizer unavailable".into()),
        }))
        .unwrap();
        assert_eq!(result.outcome, RbacAccessReviewOutcome::NoOpinion);
        assert_eq!(
            result.evaluation_error.as_deref(),
            Some("authorizer unavailable")
        );
    }
}
