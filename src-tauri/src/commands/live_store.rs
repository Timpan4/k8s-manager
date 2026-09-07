use crate::commands::helpers::FluxOwnershipIndex;
use crate::models::{
    AppError, DiscoveredResourceKind, NamespaceSummary, ResourceSummary, ResourceTopology,
    WatchResourceKind,
};
use futures_util::future::{BoxFuture, FutureExt, Shared};
use std::{
    collections::HashMap,
    future::Future,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

const COMPLETED_GRACE: Duration = Duration::from_millis(500);
const RESOURCE_FRESHNESS: Duration = Duration::from_secs(30);
/// Flux ownership comes from cluster-wide lists with no watch feed; a short
/// freshness window bounds staleness while absorbing per-request refetches.
const FLUX_INDEX_FRESHNESS: Duration = Duration::from_secs(30);
const MAX_CACHE_ENTRIES: usize = 128;

type SharedLoad<T> = Shared<BoxFuture<'static, Result<T, AppError>>>;

#[derive(Clone, Copy)]
enum CacheMode {
    GraceOnly,
    LiveFor(Duration),
}

struct ReadyValue<T> {
    value: T,
    completed_at: Instant,
    dirty: bool,
}

enum CacheEntry<T> {
    Ready(ReadyValue<T>),
    Loading {
        load_id: u64,
        future: SharedLoad<T>,
        cancellation: CancellationToken,
        previous: Option<ReadyValue<T>>,
        dirty: bool,
    },
}

struct SharedCache<T> {
    label: &'static str,
    next_load_id: AtomicU64,
    entries: Mutex<HashMap<String, CacheEntry<T>>>,
}

impl<T> SharedCache<T>
where
    T: Clone + Send + Sync + 'static,
{
    fn new(label: &'static str) -> Self {
        Self {
            label,
            next_load_id: AtomicU64::new(0),
            entries: Mutex::new(HashMap::new()),
        }
    }

    fn ready_value(value: T) -> ReadyValue<T> {
        ReadyValue {
            value,
            completed_at: Instant::now(),
            dirty: false,
        }
    }

    fn can_reuse(ready: &ReadyValue<T>, mode: CacheMode) -> bool {
        if ready.dirty {
            return false;
        }

        match mode {
            CacheMode::GraceOnly => ready.completed_at.elapsed() <= COMPLETED_GRACE,
            CacheMode::LiveFor(freshness) => ready.completed_at.elapsed() <= freshness,
        }
    }

    fn peek(&self, key: &str, mode: CacheMode) -> Option<T> {
        let entries = self.entries.lock().expect("live store cache lock");
        match entries.get(key) {
            Some(CacheEntry::Ready(ready)) if Self::can_reuse(ready, mode) => {
                Some(ready.value.clone())
            }
            _ => None,
        }
    }

    async fn get_or_load<F, Fut>(
        &self,
        key: String,
        mode: CacheMode,
        loader: F,
    ) -> Result<T, AppError>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<T, AppError>> + Send + 'static,
    {
        let (future, load_id) = {
            let mut entries = self.entries.lock().expect("live store cache lock");
            match entries.get(&key) {
                Some(CacheEntry::Ready(ready)) if Self::can_reuse(ready, mode) => {
                    eprintln!(
                        "[kubecove:backend] live_store cache_hit area={} key={}",
                        self.label, key
                    );
                    return Ok(ready.value.clone());
                }
                Some(CacheEntry::Loading {
                    load_id, future, ..
                }) => {
                    eprintln!(
                        "[kubecove:backend] live_store singleflight_join area={} key={}",
                        self.label, key
                    );
                    (future.clone(), *load_id)
                }
                _ => {
                    eprintln!(
                        "[kubecove:backend] live_store cache_miss area={} key={}",
                        self.label, key
                    );
                    let previous = entries.remove(&key).and_then(|entry| match entry {
                        CacheEntry::Ready(ready) => Some(ready),
                        CacheEntry::Loading { previous, .. } => previous,
                    });
                    let load_id = self.next_load_id.fetch_add(1, Ordering::Relaxed) + 1;
                    let cancellation = CancellationToken::new();
                    let loader_cancellation = cancellation.clone();
                    let future = async move {
                        tokio::select! {
                            result = loader() => result,
                            () = loader_cancellation.cancelled() => Err(AppError::cancelled()),
                        }
                    }
                    .boxed()
                    .shared();
                    entries.insert(
                        key.clone(),
                        CacheEntry::Loading {
                            load_id,
                            future: future.clone(),
                            cancellation,
                            previous,
                            dirty: false,
                        },
                    );
                    (future, load_id)
                }
            }
        };

        let result = future.await;
        let mut entries = self.entries.lock().expect("live store cache lock");
        let Some(CacheEntry::Loading {
            load_id: active_load_id,
            dirty,
            ..
        }) = entries.get(&key)
        else {
            return result;
        };
        if *active_load_id != load_id {
            return result;
        }
        let dirty_while_loading = *dirty;
        if let Ok(value) = &result {
            let mut ready = Self::ready_value(value.clone());
            ready.dirty = dirty_while_loading;
            entries.insert(key, CacheEntry::Ready(ready));
            Self::trim_to_budget(&mut entries);
        } else {
            let previous = entries.remove(&key).and_then(|entry| match entry {
                CacheEntry::Loading {
                    previous, dirty, ..
                } => previous.map(|mut ready| {
                    ready.dirty |= dirty;
                    ready
                }),
                CacheEntry::Ready(ready) => Some(ready),
            });
            if let Some(previous) = previous {
                entries.insert(key, CacheEntry::Ready(previous));
                Self::trim_to_budget(&mut entries);
            }
        }
        result
    }

    fn mark_dirty(&self, key: &str) {
        let mut entries = self.entries.lock().expect("live store cache lock");
        if let Some(entry) = entries.get_mut(key) {
            match entry {
                CacheEntry::Ready(ready) => ready.dirty = true,
                CacheEntry::Loading { dirty, .. } => *dirty = true,
            }
        }
    }

    fn mark_dirty_where(&self, mut matches: impl FnMut(&str) -> bool) {
        let mut entries = self.entries.lock().expect("live store cache lock");
        for (key, entry) in entries.iter_mut() {
            if !matches(key) {
                continue;
            }
            match entry {
                CacheEntry::Ready(ready) => ready.dirty = true,
                CacheEntry::Loading { dirty, .. } => *dirty = true,
            }
        }
    }

    fn cancel_loading(&self) -> usize {
        let mut entries = self.entries.lock().expect("live store cache lock");
        let loading_keys = entries
            .iter()
            .filter(|(_, entry)| matches!(entry, CacheEntry::Loading { .. }))
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        for key in &loading_keys {
            let previous = entries.remove(key).and_then(|entry| match entry {
                CacheEntry::Loading {
                    cancellation,
                    previous,
                    ..
                } => {
                    cancellation.cancel();
                    previous
                }
                CacheEntry::Ready(_) => None,
            });
            if let Some(mut previous) = previous {
                previous.dirty = true;
                entries.insert(key.clone(), CacheEntry::Ready(previous));
            }
        }
        loading_keys.len()
    }

    fn trim_to_budget(entries: &mut HashMap<String, CacheEntry<T>>) {
        let mut ready_entries: Vec<(String, Instant, bool)> = entries
            .iter()
            .filter_map(|(key, entry)| match entry {
                CacheEntry::Ready(ready) => Some((key.clone(), ready.completed_at, ready.dirty)),
                CacheEntry::Loading { .. } => None,
            })
            .collect();
        if ready_entries.len() <= MAX_CACHE_ENTRIES {
            return;
        }

        let mut ready_count = ready_entries.len();
        ready_entries.sort_by_key(|(_, completed_at, dirty)| (*dirty, *completed_at));

        for (key, _, _) in ready_entries {
            if ready_count <= MAX_CACHE_ENTRIES {
                break;
            }
            if entries.remove(&key).is_some() {
                ready_count -= 1;
            }
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.lock().expect("live store cache lock").len()
    }

    #[cfg(test)]
    fn ready_len(&self) -> usize {
        self.entries
            .lock()
            .expect("live store cache lock")
            .values()
            .filter(|entry| matches!(entry, CacheEntry::Ready(_)))
            .count()
    }

    #[cfg(test)]
    fn has_key(&self, key: &str) -> bool {
        self.entries
            .lock()
            .expect("live store cache lock")
            .contains_key(key)
    }
}

#[derive(Clone)]
pub struct ClusterLiveStore {
    namespaces: Arc<SharedCache<Vec<NamespaceSummary>>>,
    resource_kinds: Arc<SharedCache<Vec<DiscoveredResourceKind>>>,
    present_custom_resource_kinds: Arc<SharedCache<Vec<DiscoveredResourceKind>>>,
    resources: Arc<SharedCache<Vec<ResourceSummary>>>,
    topologies: Arc<SharedCache<ResourceTopology>>,
    flux_ownership_indexes: Arc<SharedCache<FluxOwnershipIndex>>,
}

impl Default for ClusterLiveStore {
    fn default() -> Self {
        Self {
            namespaces: Arc::new(SharedCache::new("namespaces")),
            resource_kinds: Arc::new(SharedCache::new("resource_kinds")),
            present_custom_resource_kinds: Arc::new(SharedCache::new(
                "present_custom_resource_kinds",
            )),
            resources: Arc::new(SharedCache::new("resources")),
            topologies: Arc::new(SharedCache::new("topologies")),
            flux_ownership_indexes: Arc::new(SharedCache::new("flux_ownership_indexes")),
        }
    }
}

impl ClusterLiveStore {
    pub fn cancel_loading(&self) -> usize {
        self.namespaces.cancel_loading()
            + self.resource_kinds.cancel_loading()
            + self.present_custom_resource_kinds.cancel_loading()
            + self.resources.cancel_loading()
            + self.topologies.cancel_loading()
            + self.flux_ownership_indexes.cancel_loading()
    }

    /// Caches the cluster-wide Flux ownership index per kubeconfig source and
    /// context. The index comes from two cluster-wide lists, so it is reused
    /// across scope requests within `FLUX_INDEX_FRESHNESS` and re-scoped to
    /// the requested namespaces by the caller.
    pub(crate) async fn flux_ownership_index<F, Fut>(
        &self,
        source_key: String,
        cluster_context: String,
        loader: F,
    ) -> Result<FluxOwnershipIndex, AppError>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<FluxOwnershipIndex, AppError>> + Send + 'static,
    {
        self.flux_ownership_indexes
            .get_or_load(
                context_cache_key(&source_key, &cluster_context),
                CacheMode::LiveFor(FLUX_INDEX_FRESHNESS),
                loader,
            )
            .await
    }

    pub async fn namespaces<F, Fut>(
        &self,
        source_key: String,
        cluster_context: String,
        loader: F,
    ) -> Result<Vec<NamespaceSummary>, AppError>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<Vec<NamespaceSummary>, AppError>> + Send + 'static,
    {
        self.namespaces
            .get_or_load(
                context_cache_key(&source_key, &cluster_context),
                CacheMode::GraceOnly,
                loader,
            )
            .await
    }

    pub async fn resource_kinds<F, Fut>(
        &self,
        source_key: String,
        cluster_context: String,
        loader: F,
    ) -> Result<Vec<DiscoveredResourceKind>, AppError>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<Vec<DiscoveredResourceKind>, AppError>> + Send + 'static,
    {
        self.resource_kinds
            .get_or_load(
                context_cache_key(&source_key, &cluster_context),
                CacheMode::LiveFor(RESOURCE_FRESHNESS),
                loader,
            )
            .await
    }

    pub fn cached_resource_kinds(
        &self,
        source_key: &str,
        cluster_context: &str,
    ) -> Option<Vec<DiscoveredResourceKind>> {
        self.resource_kinds.peek(
            &context_cache_key(source_key, cluster_context),
            CacheMode::LiveFor(RESOURCE_FRESHNESS),
        )
    }

    pub async fn present_custom_resource_kinds<F, Fut>(
        &self,
        source_key: String,
        cluster_context: String,
        namespaces: Vec<String>,
        loader: F,
    ) -> Result<Vec<DiscoveredResourceKind>, AppError>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<Vec<DiscoveredResourceKind>, AppError>> + Send + 'static,
    {
        let key =
            present_custom_resource_kinds_cache_key(&source_key, &cluster_context, &namespaces);
        self.present_custom_resource_kinds
            .get_or_load(key, CacheMode::LiveFor(RESOURCE_FRESHNESS), loader)
            .await
    }

    pub fn cached_present_custom_resource_kinds(
        &self,
        source_key: &str,
        cluster_context: &str,
        namespaces: &[String],
    ) -> Option<Vec<DiscoveredResourceKind>> {
        self.present_custom_resource_kinds.peek(
            &present_custom_resource_kinds_cache_key(source_key, cluster_context, namespaces),
            CacheMode::LiveFor(RESOURCE_FRESHNESS),
        )
    }

    pub async fn typed_resources<F, Fut>(
        &self,
        source_key: String,
        cluster_context: String,
        kind: String,
        namespace: Option<String>,
        loader: F,
    ) -> Result<Vec<ResourceSummary>, AppError>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<Vec<ResourceSummary>, AppError>> + Send + 'static,
    {
        let kind_key = typed_kind_key(&kind);
        let namespace_key = namespace_key_for_typed(&kind, namespace.as_deref());
        self.resources_for_scope(source_key, cluster_context, kind_key, namespace_key, loader)
            .await
    }

    pub async fn dynamic_resources<F, Fut>(
        &self,
        source_key: String,
        cluster_context: String,
        resource_kind: DiscoveredResourceKind,
        namespace: Option<String>,
        loader: F,
    ) -> Result<Vec<ResourceSummary>, AppError>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<Vec<ResourceSummary>, AppError>> + Send + 'static,
    {
        let kind_key = dynamic_kind_key(
            &resource_kind.api_version,
            &resource_kind.plural,
            &resource_kind.kind,
        );
        let namespace_key =
            namespace_key_for_namespaced(resource_kind.namespaced, namespace.as_deref());
        self.resources_for_scope(source_key, cluster_context, kind_key, namespace_key, loader)
            .await
    }

    async fn resources_for_scope<F, Fut>(
        &self,
        source_key: String,
        cluster_context: String,
        kind_key: String,
        namespace_key: ScopeNamespace,
        loader: F,
    ) -> Result<Vec<ResourceSummary>, AppError>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<Vec<ResourceSummary>, AppError>> + Send + 'static,
    {
        if let ScopeNamespace::Named(namespace) = &namespace_key {
            let all_key = resource_cache_key(
                &source_key,
                &cluster_context,
                &kind_key,
                &ScopeNamespace::All,
            );
            if let Some(rows) = self
                .resources
                .peek(&all_key, CacheMode::LiveFor(RESOURCE_FRESHNESS))
            {
                eprintln!(
                    "[kubecove:backend] live_store cache_hit area=resources key={} covered_by={}",
                    resource_cache_key(&source_key, &cluster_context, &kind_key, &namespace_key),
                    all_key
                );
                return Ok(rows
                    .into_iter()
                    .filter(|row| row.namespace.as_deref() == Some(namespace.as_str()))
                    .collect());
            }
        }

        self.resources
            .get_or_load(
                resource_cache_key(&source_key, &cluster_context, &kind_key, &namespace_key),
                CacheMode::LiveFor(RESOURCE_FRESHNESS),
                loader,
            )
            .await
    }

    pub async fn topology<F, Fut>(
        &self,
        source_key: String,
        cluster_context: String,
        namespaces: Vec<String>,
        mode: String,
        loader: F,
    ) -> Result<ResourceTopology, AppError>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<ResourceTopology, AppError>> + Send + 'static,
    {
        let key = topology_cache_key(&source_key, &cluster_context, &namespaces, &mode);
        self.topologies
            .get_or_load(key, CacheMode::LiveFor(RESOURCE_FRESHNESS), loader)
            .await
    }

    pub fn mark_watch_resource_dirty(
        &self,
        source_key: &str,
        cluster_context: &str,
        resource_kind: &WatchResourceKind,
        namespace: Option<&str>,
    ) {
        let namespace_key =
            namespace_key_for_namespaced(resource_kind.namespaced.unwrap_or(true), namespace);
        for kind_key in watch_kind_keys(resource_kind) {
            let exact_key =
                resource_cache_key(source_key, cluster_context, &kind_key, &namespace_key);
            let all_key =
                resource_cache_key(source_key, cluster_context, &kind_key, &ScopeNamespace::All);
            self.resources.mark_dirty(&exact_key);
            self.resources.mark_dirty(&all_key);
        }
        self.topologies.mark_dirty_where(|key| {
            key.starts_with(&format!("{source_key}|context={cluster_context}|"))
        });
        self.present_custom_resource_kinds.mark_dirty_where(|key| {
            key.starts_with(&format!("{source_key}|context={cluster_context}|"))
        });
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ScopeNamespace {
    All,
    Cluster,
    Named(String),
}

fn typed_kind_key(kind: &str) -> String {
    format!("typed:{kind}")
}

fn dynamic_kind_key(api_version: &str, plural: &str, kind: &str) -> String {
    format!("dynamic:{api_version}:{plural}:{kind}")
}

fn watch_kind_key(resource_kind: &WatchResourceKind) -> String {
    match (
        resource_kind.api_version.as_deref(),
        resource_kind.plural.as_deref(),
    ) {
        (Some(api_version), Some(plural)) => {
            dynamic_kind_key(api_version, plural, &resource_kind.kind)
        }
        _ => typed_kind_key(&resource_kind.kind),
    }
}

fn watch_kind_keys(resource_kind: &WatchResourceKind) -> Vec<String> {
    let mut keys = vec![watch_kind_key(resource_kind)];
    if is_known_typed_kind(&resource_kind.kind) {
        let typed_key = typed_kind_key(&resource_kind.kind);
        if !keys.contains(&typed_key) {
            keys.push(typed_key);
        }
    }
    keys
}

fn namespace_key_for_typed(kind: &str, namespace: Option<&str>) -> ScopeNamespace {
    if is_cluster_scoped_kind(kind) {
        ScopeNamespace::Cluster
    } else {
        namespace_key_for_namespaced(true, namespace)
    }
}

fn namespace_key_for_namespaced(namespaced: bool, namespace: Option<&str>) -> ScopeNamespace {
    if !namespaced {
        return ScopeNamespace::Cluster;
    }
    match namespace {
        Some(namespace) if !namespace.trim().is_empty() => {
            ScopeNamespace::Named(namespace.to_string())
        }
        _ => ScopeNamespace::All,
    }
}

fn context_cache_key(source_key: &str, context: &str) -> String {
    format!("{source_key}|context={context}")
}

fn resource_cache_key(
    source_key: &str,
    context: &str,
    kind_key: &str,
    namespace: &ScopeNamespace,
) -> String {
    let namespace = match namespace {
        ScopeNamespace::All => "<all>",
        ScopeNamespace::Cluster => "<cluster>",
        ScopeNamespace::Named(namespace) => namespace,
    };
    format!("{source_key}|context={context}|kind={kind_key}|namespace={namespace}")
}

fn topology_cache_key(
    source_key: &str,
    context: &str,
    namespaces: &[String],
    mode: &str,
) -> String {
    let mut namespaces = namespaces.to_vec();
    namespaces.sort();
    namespaces.dedup();
    format!(
        "{}|context={}|topology={}|namespaces={}",
        source_key,
        context,
        mode,
        namespaces.join(",")
    )
}

pub(crate) fn present_custom_resource_kinds_cache_key(
    source_key: &str,
    context: &str,
    namespaces: &[String],
) -> String {
    let mut namespaces = namespaces.to_vec();
    namespaces.sort();
    namespaces.dedup();
    format!(
        "{}|context={}|present_custom_resource_kinds={}",
        source_key,
        context,
        namespaces.join(",")
    )
}

fn is_cluster_scoped_kind(kind: &str) -> bool {
    matches!(kind, "Node" | "StorageClass" | "PersistentVolume")
}

fn is_known_typed_kind(kind: &str) -> bool {
    matches!(
        kind,
        "Pod"
            | "Deployment"
            | "ReplicaSet"
            | "StatefulSet"
            | "DaemonSet"
            | "Service"
            | "Ingress"
            | "ConfigMap"
            | "Secret"
            | "PersistentVolumeClaim"
            | "Job"
            | "CronJob"
            | "Node"
            | "StorageClass"
            | "PersistentVolume"
    )
}

#[cfg(test)]
#[path = "live_store_tests.rs"]
mod live_store_tests;
