#[cfg(test)]
use crate::models::AppErrorKind;
use crate::{
    commands::{helpers::client_cache, ArgoConnectionStore, ClusterLiveStore},
    models::{AppError, CancelBackendRequestsResult, CancelWorkspaceRequestsResult},
};
use std::{
    collections::BTreeMap,
    future::Future,
    sync::{Arc, Mutex},
};
use tauri::State;
use tokio_util::sync::CancellationToken;

struct RegisteredCancellation {
    generation: u64,
    token: CancellationToken,
}

type RequestMap = BTreeMap<String, RegisteredCancellation>;
type ScopeMap = BTreeMap<String, RequestMap>;

#[derive(Default)]
struct CancellationRegistryState {
    next_generation: u64,
    scopes: ScopeMap,
}

#[derive(Clone, Default)]
pub struct BackendCancellationRegistry {
    inner: Arc<Mutex<CancellationRegistryState>>,
}

pub struct BackendCancellationGuard {
    registry: BackendCancellationRegistry,
    scope: Option<String>,
    request_id: Option<String>,
    generation: Option<u64>,
    token: CancellationToken,
}

fn normalized(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

impl BackendCancellationRegistry {
    pub fn register(
        &self,
        cancel_scope: Option<String>,
        request_id: Option<String>,
    ) -> BackendCancellationGuard {
        let scope = normalized(cancel_scope);
        let request_id = normalized(request_id);
        let token = CancellationToken::new();
        let mut generation = None;

        if let (Some(scope), Some(request_id)) = (&scope, &request_id) {
            let mut state = self
                .inner
                .lock()
                .expect("backend cancellation registry mutex poisoned");
            let next_generation = state.next_generation;
            state.next_generation = state.next_generation.saturating_add(1);
            generation = Some(next_generation);
            let previous = state.scopes.entry(scope.clone()).or_default().insert(
                request_id.clone(),
                RegisteredCancellation {
                    generation: next_generation,
                    token: token.clone(),
                },
            );
            if let Some(previous) = previous {
                previous.token.cancel();
            }
        }

        BackendCancellationGuard {
            registry: self.clone(),
            scope,
            request_id,
            generation,
            token,
        }
    }

    pub fn cancel_scope(&self, cancel_scope: &str) -> usize {
        let tokens = self
            .inner
            .lock()
            .expect("backend cancellation registry mutex poisoned")
            .scopes
            .remove(cancel_scope)
            .unwrap_or_default()
            .into_values()
            .map(|entry| entry.token)
            .collect::<Vec<_>>();
        let count = tokens.len();
        for token in tokens {
            token.cancel();
        }
        count
    }

    pub fn cancel_all(&self) -> usize {
        let tokens = {
            let mut state = self
                .inner
                .lock()
                .expect("backend cancellation registry mutex poisoned");
            std::mem::take(&mut state.scopes)
                .into_values()
                .flat_map(BTreeMap::into_values)
                .map(|entry| entry.token)
                .collect::<Vec<_>>()
        };
        let count = tokens.len();
        for token in tokens {
            token.cancel();
        }
        count
    }

    pub async fn execute<T>(
        &self,
        cancel_scope: Option<String>,
        request_id: Option<String>,
        future: impl Future<Output = Result<T, AppError>>,
    ) -> Result<T, AppError> {
        self.register(cancel_scope, request_id).run(future).await
    }

    fn unregister(&self, scope: &str, request_id: &str, generation: u64) {
        let mut state = self
            .inner
            .lock()
            .expect("backend cancellation registry mutex poisoned");
        let Some(requests) = state.scopes.get_mut(scope) else {
            return;
        };
        if requests
            .get(request_id)
            .is_some_and(|entry| entry.generation == generation)
        {
            requests.remove(request_id);
        }
        if requests.is_empty() {
            state.scopes.remove(scope);
        }
    }
}

impl BackendCancellationGuard {
    pub async fn run<T>(
        &self,
        future: impl Future<Output = Result<T, AppError>>,
    ) -> Result<T, AppError> {
        tokio::select! {
            result = future => result,
            () = self.token.cancelled() => Err(AppError::cancelled()),
        }
    }
}

impl Drop for BackendCancellationGuard {
    fn drop(&mut self) {
        if let (Some(scope), Some(request_id), Some(generation)) =
            (&self.scope, &self.request_id, self.generation)
        {
            self.registry.unregister(scope, request_id, generation);
        }
    }
}

#[tauri::command]
pub fn cancel_backend_requests(
    cancel_scope: String,
    registry: State<'_, BackendCancellationRegistry>,
) -> CancelBackendRequestsResult {
    CancelBackendRequestsResult {
        cancelled: registry.cancel_scope(cancel_scope.trim()),
    }
}

#[tauri::command]
pub async fn cancel_workspace_requests(
    registry: State<'_, BackendCancellationRegistry>,
    live_store: State<'_, ClusterLiveStore>,
    argo_connections: State<'_, ArgoConnectionStore>,
) -> Result<CancelWorkspaceRequestsResult, AppError> {
    let cancelled_requests = registry.cancel_all();
    let client_generation = client_cache::rotate_client_generation();
    let cancelled_loads = live_store.cancel_loading();
    argo_connections.close_all().await?;
    Ok(CancelWorkspaceRequestsResult {
        cancelled_requests,
        cancelled_loads,
        client_generation,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancel_scope_only_cancels_matching_requests() {
        let registry = BackendCancellationRegistry::default();
        let first = registry.register(Some("scope-a".to_string()), Some("one".to_string()));
        let second = registry.register(Some("scope-a".to_string()), Some("two".to_string()));
        let third = registry.register(Some("scope-b".to_string()), Some("one".to_string()));

        assert_eq!(registry.cancel_scope("scope-a"), 2);
        assert!(first.token.is_cancelled());
        assert!(second.token.is_cancelled());
        assert!(!third.token.is_cancelled());
    }

    #[test]
    fn dropped_guard_unregisters_request() {
        let registry = BackendCancellationRegistry::default();
        let guard = registry.register(Some("scope-a".to_string()), Some("one".to_string()));
        drop(guard);

        assert_eq!(registry.cancel_scope("scope-a"), 0);
    }

    #[test]
    fn stale_guard_does_not_unregister_replacement_request() {
        let registry = BackendCancellationRegistry::default();
        let first = registry.register(Some("scope-a".to_string()), Some("one".to_string()));
        let second = registry.register(Some("scope-a".to_string()), Some("one".to_string()));

        drop(first);

        assert_eq!(registry.cancel_scope("scope-a"), 1);
        assert!(second.token.is_cancelled());
    }

    #[tokio::test]
    async fn cancel_all_interrupts_pending_guards() {
        let registry = BackendCancellationRegistry::default();
        let guard = registry.register(Some("scope-a".to_string()), Some("one".to_string()));
        let waiting = guard.run(std::future::pending::<Result<(), AppError>>());
        let cancellation = async {
            tokio::task::yield_now().await;
            assert_eq!(registry.cancel_all(), 1);
        };

        let (result, ()) = tokio::join!(waiting, cancellation);

        let error = result.expect_err("pending request should be cancelled");
        assert_eq!(error.kind, AppErrorKind::Cancelled);
    }

    #[tokio::test]
    async fn execute_unregisters_completed_requests() {
        let registry = BackendCancellationRegistry::default();
        let result = registry
            .execute(
                Some("scope-a".to_string()),
                Some("one".to_string()),
                async { Ok::<_, AppError>(42) },
            )
            .await;

        assert_eq!(result.expect("completed request"), 42);
        assert_eq!(registry.cancel_scope("scope-a"), 0);
    }
}
