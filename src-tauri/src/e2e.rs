use crate::models::AppError;
use crate::models::AppErrorKind;
use kube::config::Kubeconfig;
use std::{env, fs, path::PathBuf};

const E2E_ENV: &str = "KUBECOVE_E2E";
const KUBECONFIG_ENV: &str = "KUBECOVE_KUBECONFIG";
const DATA_DIR_ENV: &str = "KUBECOVE_DATA_DIR";
const CLUSTER_ENV: &str = "KUBECOVE_E2E_CLUSTER";

pub(crate) struct E2eConfig {
    pub(crate) data_dir: PathBuf,
}

pub(crate) fn is_enabled() -> bool {
    env::var(E2E_ENV).as_deref() == Ok("1")
}

pub(crate) fn kubeconfig_path() -> Result<PathBuf, AppError> {
    required_absolute_path(KUBECONFIG_ENV, false)
}

pub(crate) fn startup_config() -> Result<E2eConfig, AppError> {
    if !is_enabled() {
        return Err(AppError::new(
            "e2e feature requires KUBECOVE_E2E=1",
            AppErrorKind::Validation,
        ));
    }
    let kubeconfig = kubeconfig_path()?;
    let data_dir = required_absolute_path(DATA_DIR_ENV, true)?;
    let cluster = required_value(CLUSTER_ENV)?;
    let config = Kubeconfig::read_from(&kubeconfig).map_err(|error| {
        AppError::new("failed to load E2E kubeconfig", AppErrorKind::Validation).with_source(error)
    })?;
    validate_kubeconfig(&config, &cluster)?;
    Ok(E2eConfig { data_dir })
}

fn required_value(name: &str) -> Result<String, AppError> {
    env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            AppError::new(
                format!("{name} is required for E2E"),
                AppErrorKind::Validation,
            )
        })
}

fn required_absolute_path(name: &str, directory: bool) -> Result<PathBuf, AppError> {
    let path = PathBuf::from(required_value(name)?);
    canonical_existing_path(name, path, directory)
}

fn canonical_existing_path(
    name: &str,
    path: PathBuf,
    directory: bool,
) -> Result<PathBuf, AppError> {
    if !path.is_absolute() {
        return Err(AppError::new(
            format!("{name} must be an absolute path"),
            AppErrorKind::Validation,
        ));
    }
    let metadata = fs::metadata(&path).map_err(|error| {
        AppError::new(
            format!("{name} path is unavailable"),
            AppErrorKind::Validation,
        )
        .with_source(error)
    })?;
    if (directory && !metadata.is_dir()) || (!directory && !metadata.is_file()) {
        let kind = if directory {
            "directory"
        } else {
            "regular file"
        };
        return Err(AppError::new(
            format!("{name} must be a {kind}"),
            AppErrorKind::Validation,
        ));
    }
    let canonical = fs::canonicalize(&path).map_err(|error| {
        AppError::new(
            format!("{name} path is unavailable"),
            AppErrorKind::Validation,
        )
        .with_source(error)
    })?;
    Ok(canonical)
}

fn validate_kubeconfig(config: &Kubeconfig, cluster: &str) -> Result<(), AppError> {
    let expected_contexts = [format!("{cluster}-admin"), format!("{cluster}-restricted")];
    let mut actual_contexts = config
        .contexts
        .iter()
        .map(|context| context.name.as_str())
        .collect::<Vec<_>>();
    actual_contexts.sort_unstable();
    let mut expected_names = expected_contexts
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    expected_names.sort_unstable();
    if actual_contexts != expected_names {
        return Err(AppError::new(
            "E2E kubeconfig must contain exactly admin and restricted contexts for this run",
            AppErrorKind::Validation,
        ));
    }
    if config.clusters.len() != 1 || config.clusters[0].name != cluster {
        return Err(AppError::new(
            "E2E kubeconfig must contain only the exact run cluster",
            AppErrorKind::Validation,
        ));
    }
    let mut actual_users = config
        .auth_infos
        .iter()
        .map(|user| user.name.as_str())
        .collect::<Vec<_>>();
    actual_users.sort_unstable();
    if actual_users != expected_names {
        return Err(AppError::new(
            "E2E kubeconfig must contain exactly admin and restricted users for this run",
            AppErrorKind::Validation,
        ));
    }
    for role in ["admin", "restricted"] {
        let context_name = format!("{cluster}-{role}");
        let context = config
            .contexts
            .iter()
            .find(|context| context.name == context_name)
            .and_then(|context| context.context.as_ref())
            .ok_or_else(|| {
                AppError::new(
                    format!("missing E2E context {context_name}"),
                    AppErrorKind::Validation,
                )
            })?;
        if context.cluster != cluster || context.user.as_deref() != Some(context_name.as_str()) {
            return Err(AppError::new(
                format!("E2E context {context_name} must target its exact cluster and user"),
                AppErrorKind::Validation,
            ));
        }
    }
    if config.current_context.as_deref() != Some(expected_contexts[0].as_str()) {
        return Err(AppError::new(
            "E2E kubeconfig current context must be the run admin context",
            AppErrorKind::Validation,
        ));
    }
    let server = config
        .clusters
        .iter()
        .find(|named| named.name == cluster)
        .and_then(|named| named.cluster.as_ref())
        .and_then(|cluster| cluster.server.as_deref())
        .ok_or_else(|| AppError::new("missing E2E cluster API server", AppErrorKind::Validation))?;
    let uri = server.parse::<http::Uri>().map_err(|error| {
        AppError::new(
            "E2E cluster API server must be a URL",
            AppErrorKind::Validation,
        )
        .with_source(error)
    })?;
    if uri.scheme_str() != Some("https") {
        return Err(AppError::new(
            "E2E cluster API server must use HTTPS",
            AppErrorKind::Validation,
        ));
    }
    let host = uri
        .host()
        .map(|value| value.trim_start_matches('[').trim_end_matches(']'))
        .ok_or_else(|| {
            AppError::new(
                "E2E cluster API server must have a host",
                AppErrorKind::Validation,
            )
        })?;
    if !matches!(host, "127.0.0.1" | "localhost" | "::1") {
        return Err(AppError::new(
            "E2E cluster API server must use loopback",
            AppErrorKind::Validation,
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_config() -> Kubeconfig {
        serde_yaml::from_str(
            "clusters:\n- name: run\n  cluster: { server: https://127.0.0.1:6443 }\ncontexts:\n- name: run-admin\n  context: { cluster: run, user: run-admin }\n- name: run-restricted\n  context: { cluster: run, user: run-restricted }\ncurrent-context: run-admin\nusers:\n- name: run-admin\n  user: { token: admin }\n- name: run-restricted\n  user: { token: restricted }\n",
        )
        .expect("config")
    }

    #[test]
    fn accepts_exact_loopback_config() {
        assert!(validate_kubeconfig(&valid_config(), "run").is_ok());
    }

    #[test]
    fn rejects_context_with_wrong_user() {
        let mut config = valid_config();
        config.contexts[0].context.as_mut().expect("context").user = Some("run-restricted".into());
        assert!(validate_kubeconfig(&config, "run").is_err());
    }

    #[test]
    fn rejects_non_loopback_server() {
        let mut config = valid_config();
        config.clusters[0].cluster.as_mut().expect("cluster").server =
            Some("https://example.com".into());
        assert!(validate_kubeconfig(&config, "run").is_err());
    }

    #[test]
    fn canonicalizes_existing_absolute_path() {
        let root = env::temp_dir().join(format!("kubecove-e2e-path-{}", std::process::id()));
        let nested = root.join("nested");
        fs::create_dir_all(&nested).expect("temporary directory");
        let file = root.join("kubeconfig");
        fs::write(&file, "test").expect("temporary file");

        let result =
            canonical_existing_path(KUBECONFIG_ENV, nested.join("..").join("kubeconfig"), false)
                .expect("canonical path");
        let expected = fs::canonicalize(&file).expect("expected canonical path");

        let _ = fs::remove_dir_all(root);
        assert_eq!(result, expected);
    }
}
