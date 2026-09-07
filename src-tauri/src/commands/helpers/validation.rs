use crate::models::{AppError, AppErrorKind};
use kube::api::ApiResource;

/// kube-rs inserts these values directly into URI paths. Preserve identifiers,
/// but reject characters that can change the endpoint or query.
pub(crate) fn validate_path_segment(value: &str, field: &str) -> Result<(), AppError> {
    if value.is_empty()
        || matches!(value, "." | "..")
        || value.chars().any(|character| {
            character.is_whitespace()
                || character.is_control()
                || matches!(character, '/' | '\\' | '%' | '?' | '#')
        })
    {
        return Err(AppError::new(
            format!("{field} must be a valid Kubernetes path segment"),
            AppErrorKind::Validation,
        ));
    }
    Ok(())
}

pub(crate) fn validate_namespace(namespace: Option<&str>) -> Result<(), AppError> {
    if let Some(namespace) = namespace {
        validate_path_segment(namespace, "namespace")?;
    }
    Ok(())
}

pub(crate) fn validate_api_resource(resource: &ApiResource) -> Result<(), AppError> {
    if !resource.group.is_empty() {
        validate_path_segment(&resource.group, "API group")?;
    }
    validate_path_segment(&resource.version, "API version")?;
    validate_path_segment(&resource.plural, "resource plural")?;
    let expected = if resource.group.is_empty() {
        resource.version.clone()
    } else {
        format!("{}/{}", resource.group, resource.version)
    };
    if resource.api_version != expected {
        return Err(AppError::new(
            "apiVersion does not match group and version",
            AppErrorKind::Validation,
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_names_but_rejects_uri_controls() {
        for name in [
            "default",
            "web-0",
            "example.com",
            "system:aggregate-to-admin",
        ] {
            assert!(validate_path_segment(name, "name").is_ok());
        }
        for name in [
            "", ".", "..", "a/b", "a\\b", "%2f", "a?b", "a#b", "a\nb", " a",
        ] {
            assert!(validate_path_segment(name, "name").is_err(), "{name:?}");
        }
    }
}
