use super::connected::{kubeconfig_source_key, ArgoConnectionStore, ConnectedArgo};
use crate::models::AppError;
use crate::models::AppErrorKind;
use std::sync::Arc;
use tokio::sync::OwnedMutexGuard;

pub(crate) struct ConnectionLease {
    pub(crate) connection: Arc<ConnectedArgo>,
    _gate: OwnedMutexGuard<()>,
}

fn required_workspace_id(workspace_id: Option<&str>) -> Result<&str, AppError> {
    workspace_id
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            AppError::new(
                "workspaceId required for connected Argo CD",
                AppErrorKind::ArgoConnection,
            )
        })
}

fn in_scope(
    connection: &ConnectedArgo,
    cluster_context: &str,
    workspace_id: &str,
    kubeconfig_env_var: Option<&str>,
) -> Result<bool, AppError> {
    Ok(
        connection.profile.cluster_context.as_deref() == Some(cluster_context)
            && connection.profile.workspace_id.as_deref() == Some(workspace_id)
            && connection.profile.kubeconfig_source_key.as_deref()
                == Some(kubeconfig_source_key(kubeconfig_env_var)?.as_str()),
    )
}

fn scope_error() -> AppError {
    AppError::new(
        "Argo CD connection is outside current workspace scope",
        AppErrorKind::ArgoConnection,
    )
}

pub(crate) fn scoped_connection(
    store: &ArgoConnectionStore,
    id: &str,
    cluster_context: &str,
    workspace_id: Option<&str>,
    kubeconfig_env_var: Option<&str>,
) -> Result<Arc<ConnectedArgo>, AppError> {
    let workspace_id = required_workspace_id(workspace_id)?;
    let connection = store
        .connections
        .lock()
        .map_err(|_| {
            AppError::new(
                "Argo CD connection state unavailable",
                AppErrorKind::ArgoConnection,
            )
        })?
        .get(id)
        .cloned()
        .ok_or_else(|| {
            AppError::new("Argo CD connection not found", AppErrorKind::ArgoConnection)
        })?;
    if !in_scope(
        &connection,
        cluster_context,
        workspace_id,
        kubeconfig_env_var,
    )? {
        return Err(scope_error());
    }
    Ok(connection)
}

pub(crate) async fn acquire_connection_lease(
    store: &ArgoConnectionStore,
    id: &str,
    connection: Arc<ConnectedArgo>,
    cluster_context: &str,
    workspace_id: Option<&str>,
    kubeconfig_env_var: Option<&str>,
    expected_generation: &str,
    expected_instance_id: &str,
) -> Result<ConnectionLease, AppError> {
    let workspace_id = required_workspace_id(workspace_id)?;
    let gate = connection.gate.clone().lock_owned().await;
    let current = store
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
    let current_matches = current.is_some_and(|current| Arc::ptr_eq(&current, &connection));
    let scope_matches = in_scope(
        &connection,
        cluster_context,
        workspace_id,
        kubeconfig_env_var,
    )?;
    let generation_matches = connection.generation == expected_generation;
    let instance_matches = connection.instance_id == expected_instance_id;
    if !current_matches || !generation_matches || !instance_matches {
        return Err(AppError::new(
            "Argo CD connection was replaced",
            AppErrorKind::ArgoOperationUnavailable,
        ));
    }
    if !scope_matches {
        return Err(scope_error());
    }
    Ok(ConnectionLease {
        connection,
        _gate: gate,
    })
}
