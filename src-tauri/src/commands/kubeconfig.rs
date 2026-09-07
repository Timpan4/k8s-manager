use crate::models::AppErrorKind;
use crate::models::{AppError, ClusterContext};
use kube::{
    config::{KubeConfigOptions, Kubeconfig},
    Config,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{hash_map::DefaultHasher, HashSet},
    env,
    ffi::OsString,
    fs,
    hash::{Hash, Hasher},
    path::PathBuf,
    sync::{OnceLock, RwLock},
    time::Duration,
};
use tauri::AppHandle;
use tauri_plugin_dialog::{DialogExt, FilePath};

pub const DEFAULT_KUBECONFIG_ENV_VAR: &str = "KUBECONFIG";
const SETTINGS_FILE_NAME: &str = "kubeconfig-sources.json";
#[cfg(any(feature = "e2e", test))]
const E2E_KUBECONFIG_SOURCE_ENV_VAR: &str = "KUBECOVE_E2E_KUBECONFIG_SOURCE";
pub(super) const FINITE_REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(30);

static SETTINGS_PATH: OnceLock<PathBuf> = OnceLock::new();
// In-memory copy of the persisted sources file. Every command constructs a
// KubeconfigSource, so reading the settings JSON from disk each time is pure
// overhead; the file is only ever written through save_persisted_sources.
static SETTINGS_CACHE: RwLock<Option<PersistedKubeconfigSources>> = RwLock::new(None);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KubeconfigPathEntry {
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KubeconfigSourceWarning {
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KubeconfigSourcesSummary {
    pub kubeconfig_env_var: String,
    pub paths: Vec<KubeconfigPathEntry>,
    pub source_key: String,
    pub source_label: String,
    pub show_source_labels: bool,
    pub warnings: Vec<KubeconfigSourceWarning>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedKubeconfigSources {
    kubeconfig_env_var: String,
    paths: Vec<String>,
    show_source_labels: bool,
}

impl Default for PersistedKubeconfigSources {
    fn default() -> Self {
        Self {
            kubeconfig_env_var: default_kubeconfig_env_var(),
            paths: Vec::new(),
            show_source_labels: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KubeconfigSource {
    env_var: String,
    app_paths: Vec<PathBuf>,
    show_source_labels: bool,
    read_env: bool,
}

impl KubeconfigSource {
    pub fn new(env_var: Option<String>) -> Result<Self, AppError> {
        #[cfg(feature = "e2e")]
        if crate::e2e::is_enabled() {
            return Ok(Self::from_e2e_path(crate::e2e::kubeconfig_path()?));
        }
        let settings = load_persisted_sources()?;
        let env_var = env_var
            .as_deref()
            .filter(|value| !is_source_key(value))
            .or(Some(settings.kubeconfig_env_var.as_str()));
        Self::from_settings(env_var, settings.paths, settings.show_source_labels)
    }

    pub fn from_env_var(env_var: Option<&str>) -> Result<Self, AppError> {
        Self::from_settings(env_var, Vec::new(), default_show_source_labels())
    }

    fn from_settings(
        env_var: Option<&str>,
        paths: Vec<String>,
        show_source_labels: bool,
    ) -> Result<Self, AppError> {
        let env_var = normalize_env_var(env_var);
        validate_env_var_name(&env_var)?;
        Ok(Self {
            env_var,
            app_paths: normalize_paths(paths)
                .into_iter()
                .map(PathBuf::from)
                .collect(),
            show_source_labels,
            read_env: true,
        })
    }

    #[cfg(any(feature = "e2e", test))]
    fn from_e2e_path(path: PathBuf) -> Self {
        Self {
            env_var: E2E_KUBECONFIG_SOURCE_ENV_VAR.to_string(),
            app_paths: vec![path],
            show_source_labels: false,
            read_env: false,
        }
    }

    pub fn key(&self) -> String {
        let mut hasher = DefaultHasher::new();
        self.env_var.hash(&mut hasher);
        let selected_env_value = self.read_env.then(|| env::var_os(&self.env_var)).flatten();
        selected_env_value
            .as_ref()
            .map(|value| value.to_string_lossy().into_owned())
            .hash(&mut hasher);
        if self.read_env
            && selected_env_value
                .clone()
                .and_then(split_non_empty_paths)
                .is_none()
        {
            for configured in standard_kubeconfig_paths() {
                configured.path.hash(&mut hasher);
            }
        }
        for path in &self.app_paths {
            path.to_string_lossy().hash(&mut hasher);
        }
        format!("kubeconfigSource={:016x}", hasher.finish())
    }

    pub fn label(&self) -> String {
        let count = self.configured_path_count();
        if count == 0 {
            self.env_var.clone()
        } else {
            format!(
                "{} + {} {}",
                self.env_var,
                count,
                if count == 1 { "path" } else { "paths" }
            )
        }
    }

    pub fn env_var(&self) -> &str {
        &self.env_var
    }

    pub fn show_source_labels(&self) -> bool {
        self.show_source_labels
    }

    fn configured_path_count(&self) -> usize {
        let env_count = self
            .read_env
            .then(|| env::var_os(&self.env_var))
            .flatten()
            .and_then(split_non_empty_paths)
            .map_or(0, |paths| paths.len());
        env_count + self.app_paths.len()
    }

    pub fn read_kubeconfig(&self) -> Result<Kubeconfig, AppError> {
        let (kubeconfig, _) = self.read_configured_kubeconfig()?;
        Ok(kubeconfig)
    }

    pub async fn config_for_context(&self, cluster_context: &str) -> Result<Config, AppError> {
        let options = KubeConfigOptions {
            context: Some(cluster_context.to_string()),
            ..Default::default()
        };

        match self.configured_paths()? {
            Some(_) => {
                let (kubeconfig, _) = self.read_configured_kubeconfig()?;
                Config::from_custom_kubeconfig(kubeconfig, &options)
                    .await
                    .map_err(AppError::from)
            }
            None => Config::from_kubeconfig(&options)
                .await
                .map_err(AppError::from),
        }
    }

    // The kubeconfig files that back this source, in the same order
    // config_for_context resolves them: selected env paths, or standard
    // discovery when that selection is empty, followed by app-added paths.
    pub(super) fn effective_kubeconfig_paths(&self) -> Result<Vec<PathBuf>, AppError> {
        if let Some(paths) = self.configured_paths()? {
            return Ok(paths
                .into_iter()
                .map(|configured| configured.path)
                .collect());
        }
        Ok(standard_kubeconfig_paths()
            .into_iter()
            .map(|configured| configured.path)
            .collect())
    }

    fn custom_env_value_paths(&self) -> Result<Option<Vec<PathBuf>>, AppError> {
        if !self.read_env {
            return Ok(None);
        }
        let Some(value) = env::var_os(&self.env_var) else {
            return Ok(None);
        };
        Ok(split_non_empty_paths(value))
    }

    fn configured_paths(&self) -> Result<Option<Vec<ConfiguredKubeconfigPath>>, AppError> {
        let mut paths = Vec::new();
        if let Some(env_paths) = self.custom_env_value_paths()? {
            paths.extend(env_paths.into_iter().map(|path| ConfiguredKubeconfigPath {
                source: ConfiguredKubeconfigPathSource::Env(self.env_var.clone()),
                path,
            }));
        } else if self.read_env && !self.app_paths.is_empty() {
            paths.extend(standard_kubeconfig_paths());
        }
        paths.extend(
            self.app_paths
                .iter()
                .cloned()
                .map(|path| ConfiguredKubeconfigPath {
                    source: ConfiguredKubeconfigPathSource::AppPath,
                    path,
                }),
        );
        if paths.is_empty() {
            Ok(None)
        } else {
            Ok(Some(paths))
        }
    }

    fn read_configured_kubeconfig(
        &self,
    ) -> Result<(Kubeconfig, Vec<KubeconfigSourceWarning>), AppError> {
        let Some(paths) = self.configured_paths()? else {
            return Kubeconfig::read()
                .map(|kubeconfig| (kubeconfig, Vec::new()))
                .map_err(AppError::from);
        };

        let mut merged = Kubeconfig::default();
        let mut loaded = 0usize;
        let mut warnings = Vec::new();
        for configured in paths {
            match Kubeconfig::read_from(&configured.path) {
                Ok(next) => {
                    merged = merged.merge(next).map_err(|err| {
                        AppError::new(
                            format_kubeconfig_error(&self.env_var),
                            AppErrorKind::Kubeconfig,
                        )
                        .with_source(err)
                    })?;
                    loaded += 1;
                }
                Err(err) => {
                    if let Some(warning) = configured.warning(err) {
                        warnings.push(warning);
                    }
                }
            }
        }

        if loaded > 0 {
            return Ok((merged, warnings));
        }

        #[cfg(feature = "e2e")]
        if crate::e2e::is_enabled() {
            return Err(AppError::new(
                "the isolated E2E kubeconfig became unavailable",
                AppErrorKind::Validation,
            ));
        }

        read_default_kubeconfig_without_env()
            .map(|kubeconfig| (kubeconfig, warnings))
            .map_err(|err| {
                AppError::new(
                    format!("no usable kubeconfig sources and default discovery failed: {err}"),
                    AppErrorKind::Kubeconfig,
                )
                .with_source(err)
            })
    }
}

pub fn kubeconfig_source_key(kubeconfig_env_var: Option<&str>) -> Result<String, AppError> {
    if let Some(value) = kubeconfig_env_var.filter(|value| is_source_key(value)) {
        return Ok(value.to_string());
    }
    Ok(KubeconfigSource::new(kubeconfig_env_var.map(str::to_string))?.key())
}

pub fn cluster_contexts_from_source(
    source: &KubeconfigSource,
) -> Result<Vec<ClusterContext>, AppError> {
    let kubeconfig = source.read_kubeconfig()?;
    let current_context = kubeconfig.current_context.as_deref();

    Ok(kubeconfig
        .contexts
        .iter()
        .map(|ctx| ClusterContext {
            name: ctx.name.clone(),
            is_current: current_context == Some(ctx.name.as_str()),
        })
        .collect())
}

fn normalize_env_var(env_var: Option<&str>) -> String {
    env_var
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_KUBECONFIG_ENV_VAR)
        .to_string()
}

pub fn init_kubeconfig_settings_path(app_config_dir: PathBuf) {
    let _ = SETTINGS_PATH.set(app_config_dir.join(SETTINGS_FILE_NAME));
}

#[tauri::command]
pub fn get_kubeconfig_sources() -> Result<KubeconfigSourcesSummary, AppError> {
    kubeconfig_sources_summary()
}

#[tauri::command]
pub fn set_kubeconfig_env_var(env_var: String) -> Result<KubeconfigSourcesSummary, AppError> {
    let mut settings = load_persisted_sources()?;
    let normalized = normalize_env_var(Some(&env_var));
    validate_env_var_name(&normalized)?;
    settings.kubeconfig_env_var = normalized;
    save_persisted_sources(&settings)?;
    kubeconfig_sources_summary()
}

#[tauri::command]
pub fn set_show_kubeconfig_source_labels(show: bool) -> Result<KubeconfigSourcesSummary, AppError> {
    let mut settings = load_persisted_sources()?;
    settings.show_source_labels = show;
    save_persisted_sources(&settings)?;
    kubeconfig_sources_summary()
}

#[tauri::command]
pub fn add_kubeconfig_paths(paths: Vec<String>) -> Result<KubeconfigSourcesSummary, AppError> {
    let mut settings = load_persisted_sources()?;
    settings.paths = append_unique_paths(settings.paths, parse_path_input(paths));
    save_persisted_sources(&settings)?;
    kubeconfig_sources_summary()
}

#[tauri::command]
pub fn remove_kubeconfig_path(path: String) -> Result<KubeconfigSourcesSummary, AppError> {
    let mut settings = load_persisted_sources()?;
    settings.paths.retain(|candidate| candidate != &path);
    save_persisted_sources(&settings)?;
    kubeconfig_sources_summary()
}

#[tauri::command]
pub fn reorder_kubeconfig_paths(paths: Vec<String>) -> Result<KubeconfigSourcesSummary, AppError> {
    let mut settings = load_persisted_sources()?;
    let existing = settings.paths.iter().cloned().collect::<HashSet<_>>();
    let mut reordered = normalize_paths(paths)
        .into_iter()
        .filter(|path| existing.contains(path))
        .collect::<Vec<_>>();
    for path in settings.paths {
        if !reordered.contains(&path) {
            reordered.push(path);
        }
    }
    settings.paths = reordered;
    save_persisted_sources(&settings)?;
    kubeconfig_sources_summary()
}

#[tauri::command]
pub fn pick_kubeconfig_paths(app: AppHandle) -> Result<KubeconfigSourcesSummary, AppError> {
    let Some(paths) = app
        .dialog()
        .file()
        .set_title("Add kubeconfig files")
        .add_filter("Kubeconfig", &["yaml", "yml", "conf", "config"])
        .blocking_pick_files()
    else {
        return kubeconfig_sources_summary();
    };

    let picked = paths
        .into_iter()
        .filter_map(file_path_to_string)
        .collect::<Vec<_>>();
    add_kubeconfig_paths(picked)
}

fn validate_env_var_name(env_var: &str) -> Result<(), AppError> {
    let mut chars = env_var.chars();
    let Some(first) = chars.next() else {
        return Err(invalid_env_var_error());
    };
    if !(first == '_' || first.is_ascii_alphabetic()) {
        return Err(invalid_env_var_error());
    }
    if chars.any(|ch| !(ch == '_' || ch.is_ascii_alphanumeric())) {
        return Err(invalid_env_var_error());
    }
    Ok(())
}

fn invalid_env_var_error() -> AppError {
    AppError::new(
        "kubeconfig env var name must contain only ASCII letters, numbers, and underscores, and cannot start with a number",
        AppErrorKind::Validation,
    )
}

fn default_kubeconfig_env_var() -> String {
    DEFAULT_KUBECONFIG_ENV_VAR.to_string()
}

fn default_show_source_labels() -> bool {
    true
}

fn split_non_empty_paths(value: OsString) -> Option<Vec<PathBuf>> {
    let paths = env::split_paths(&value)
        .filter(|path| !path.as_os_str().is_empty())
        .collect::<Vec<_>>();
    if paths.is_empty() {
        None
    } else {
        Some(paths)
    }
}

fn standard_kubeconfig_paths() -> Vec<ConfiguredKubeconfigPath> {
    if let Some(paths) = env::var_os(DEFAULT_KUBECONFIG_ENV_VAR).and_then(split_non_empty_paths) {
        return paths
            .into_iter()
            .map(|path| ConfiguredKubeconfigPath {
                source: ConfiguredKubeconfigPathSource::Env(DEFAULT_KUBECONFIG_ENV_VAR.to_string()),
                path,
            })
            .collect();
    }
    default_kubeconfig_path()
        .into_iter()
        .map(|path| ConfiguredKubeconfigPath {
            source: ConfiguredKubeconfigPathSource::Default,
            path,
        })
        .collect()
}

fn format_kubeconfig_error(env_var: &str) -> String {
    format!("failed to load kubeconfig from {env_var}")
}

fn is_source_key(value: &str) -> bool {
    value.starts_with("kubeconfigSource=")
}

fn settings_path() -> Option<PathBuf> {
    SETTINGS_PATH.get().cloned()
}

fn load_persisted_sources() -> Result<PersistedKubeconfigSources, AppError> {
    let Some(path) = settings_path() else {
        return Ok(PersistedKubeconfigSources::default());
    };
    if let Some(cached) = SETTINGS_CACHE.read().expect("settings cache lock").clone() {
        return Ok(cached);
    }
    let settings = if path.exists() {
        let content = fs::read_to_string(&path).map_err(|err| {
            AppError::new(
                format!("failed to read kubeconfig sources: {err}"),
                AppErrorKind::Io,
            )
        })?;
        serde_json::from_str(&content).map_err(|err| {
            AppError::new(
                format!("failed to parse kubeconfig sources settings: {err}"),
                AppErrorKind::Validation,
            )
            .with_source(err)
        })?
    } else {
        PersistedKubeconfigSources::default()
    };
    *SETTINGS_CACHE.write().expect("settings cache lock") = Some(settings.clone());
    Ok(settings)
}

fn save_persisted_sources(settings: &PersistedKubeconfigSources) -> Result<(), AppError> {
    let Some(path) = settings_path() else {
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            AppError::new(
                format!("failed to create kubeconfig sources directory: {err}"),
                AppErrorKind::Io,
            )
            .with_source(err)
        })?;
    }
    let content = serde_json::to_string_pretty(settings).map_err(|err| {
        AppError::new(
            format!("failed to serialize kubeconfig sources settings: {err}"),
            AppErrorKind::Validation,
        )
        .with_source(err)
    })?;
    fs::write(&path, content).map_err(|err| {
        AppError::new(
            format!("failed to write kubeconfig sources settings: {err}"),
            AppErrorKind::Io,
        )
        .with_source(err)
    })?;
    *SETTINGS_CACHE.write().expect("settings cache lock") = Some(settings.clone());
    Ok(())
}

fn kubeconfig_sources_summary() -> Result<KubeconfigSourcesSummary, AppError> {
    let settings = load_persisted_sources()?;
    let source = KubeconfigSource::from_settings(
        Some(&settings.kubeconfig_env_var),
        settings.paths.clone(),
        settings.show_source_labels,
    )?;
    let warnings = source.read_configured_kubeconfig().map_or_else(
        |err| {
            vec![KubeconfigSourceWarning {
                source: "default".to_string(),
                path: None,
                message: err.message,
            }]
        },
        |(_, warnings)| warnings,
    );

    Ok(KubeconfigSourcesSummary {
        kubeconfig_env_var: source.env_var().to_string(),
        paths: settings
            .paths
            .into_iter()
            .map(|path| KubeconfigPathEntry { path })
            .collect(),
        source_key: source.key(),
        source_label: source.label(),
        show_source_labels: source.show_source_labels(),
        warnings,
    })
}

fn parse_path_input(values: Vec<String>) -> Vec<String> {
    let mut parsed = Vec::new();
    for value in values {
        for line in value.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            parsed.extend(
                env::split_paths(trimmed)
                    .filter(|path| !path.as_os_str().is_empty())
                    .map(|path| path.to_string_lossy().into_owned()),
            );
        }
    }
    normalize_paths(parsed)
}

fn normalize_paths(paths: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut normalized = Vec::new();
    for path in paths {
        let trimmed = path.trim();
        if trimmed.is_empty() || !seen.insert(trimmed.to_string()) {
            continue;
        }
        normalized.push(trimmed.to_string());
    }
    normalized
}

fn append_unique_paths(existing: Vec<String>, additions: Vec<String>) -> Vec<String> {
    let mut paths = normalize_paths(existing);
    for path in additions {
        if !paths.contains(&path) {
            paths.push(path);
        }
    }
    paths
}

fn file_path_to_string(file_path: FilePath) -> Option<String> {
    file_path
        .into_path()
        .ok()
        .map(|path| path.to_string_lossy().into_owned())
}

fn default_kubeconfig_path() -> Option<PathBuf> {
    if let Some(home) = env::var_os("HOME") {
        return Some(PathBuf::from(home).join(".kube").join("config"));
    }
    env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .map(|home| home.join(".kube").join("config"))
}

fn read_default_kubeconfig_without_env() -> Result<Kubeconfig, kube::config::KubeconfigError> {
    if let Some(path) = default_kubeconfig_path().filter(|path| path.exists()) {
        return Kubeconfig::read_from(path);
    }
    Kubeconfig::read()
}

#[derive(Debug, Clone)]
struct ConfiguredKubeconfigPath {
    source: ConfiguredKubeconfigPathSource,
    path: PathBuf,
}

#[derive(Debug, Clone)]
enum ConfiguredKubeconfigPathSource {
    Env(String),
    Default,
    AppPath,
}

impl ConfiguredKubeconfigPath {
    fn warning(&self, _err: kube::config::KubeconfigError) -> Option<KubeconfigSourceWarning> {
        match &self.source {
            ConfiguredKubeconfigPathSource::Env(env_var) => Some(KubeconfigSourceWarning {
                source: "env".to_string(),
                path: None,
                message: format!("A kubeconfig path from {env_var} could not be loaded."),
            }),
            ConfiguredKubeconfigPathSource::Default if !self.path.exists() => None,
            ConfiguredKubeconfigPathSource::Default => Some(KubeconfigSourceWarning {
                source: "default".to_string(),
                path: None,
                message: "The default kubeconfig could not be loaded.".to_string(),
            }),
            ConfiguredKubeconfigPathSource::AppPath => Some(KubeconfigSourceWarning {
                source: "path".to_string(),
                path: Some(self.path.to_string_lossy().into_owned()),
                message: "This kubeconfig path could not be loaded.".to_string(),
            }),
        }
    }
}

#[cfg(test)]
#[path = "kubeconfig_tests.rs"]
mod tests;
