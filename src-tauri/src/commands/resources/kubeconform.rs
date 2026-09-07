use crate::models::{
    AppError, AppErrorKind, KubernetesYamlLintDiagnostic, KubernetesYamlLintResult,
    KubernetesYamlLintSeverity, KubernetesYamlLintStatusNote,
};
use serde::Deserialize;
use std::{
    env,
    path::{Path, PathBuf},
    process::Stdio,
};
use tokio::{io::AsyncWriteExt, process::Command};

const KUBECONFORM_VERSION: &str = "v0.7.0";

#[derive(Debug, Deserialize)]
struct KubeconformOutput {
    #[serde(default)]
    resources: Option<Vec<KubeconformResource>>,
}

#[derive(Debug, Deserialize)]
struct KubeconformResource {
    #[serde(default)]
    kind: String,
    #[serde(default)]
    status: KubeconformStatus,
    #[serde(default)]
    msg: String,
    #[serde(default, rename = "validationErrors")]
    validation_errors: Vec<KubeconformValidationError>,
}

#[derive(Debug, Default, Deserialize)]
enum KubeconformStatus {
    #[serde(rename = "statusValid", alias = "VALID")]
    Valid,
    #[serde(rename = "statusInvalid", alias = "INVALID")]
    Invalid,
    #[serde(rename = "statusError", alias = "ERROR")]
    Error,
    #[serde(rename = "statusSkipped", alias = "SKIPPED")]
    Skipped,
    #[serde(rename = "")]
    Empty,
    #[default]
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Deserialize)]
struct KubeconformValidationError {
    #[serde(default)]
    path: String,
    #[serde(default)]
    msg: String,
}

pub(super) async fn append_kubeconform_lint(yaml: &str, result: &mut KubernetesYamlLintResult) {
    let Some(binary) = resolve_kubeconform_path() else {
        result.notes.push(status_note(
            KubernetesYamlLintSeverity::Info,
            "Kubeconform",
            format!("kubeconform {KUBECONFORM_VERSION} sidecar is not available."),
        ));
        return;
    };
    let Some(cache_dir) = kubeconform_cache_dir(result) else {
        return;
    };

    let output = run_kubeconform(&binary, &cache_dir, yaml).await;
    let output = match output {
        Ok(output) => output,
        Err(message) => {
            result.notes.push(status_note(
                KubernetesYamlLintSeverity::Warning,
                "Kubeconform",
                message.message,
            ));
            return;
        }
    };

    collect_kubeconform_output(output, result);
}

fn collect_kubeconform_output(output: std::process::Output, result: &mut KubernetesYamlLintResult) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    if stdout.trim().is_empty() {
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            result.notes.push(status_note(
                KubernetesYamlLintSeverity::Warning,
                "Kubeconform",
                stderr.trim().to_string(),
            ));
        }
        return;
    }

    let parsed = if let Ok(parsed) = serde_json::from_str::<KubeconformOutput>(&stdout) {
        parsed
    } else {
        result.notes.push(status_note(
            KubernetesYamlLintSeverity::Warning,
            "Kubeconform",
            "Kubeconform returned unreadable JSON output.",
        ));
        return;
    };

    for resource in parsed.resources.into_iter().flatten() {
        if matches!(
            resource.status,
            KubeconformStatus::Valid | KubeconformStatus::Empty
        ) {
            continue;
        }
        let message = match resource.msg.trim() {
            "" => "Kubeconform returned no diagnostic message.",
            message => message,
        };
        if !matches!(resource.status, KubeconformStatus::Invalid) {
            result.notes.push(status_note(
                if matches!(resource.status, KubeconformStatus::Skipped) {
                    KubernetesYamlLintSeverity::Info
                } else {
                    KubernetesYamlLintSeverity::Warning
                },
                "Kubeconform",
                format!(
                    "Schema validation incomplete for {}: {message}",
                    resource.kind
                ),
            ));
            continue;
        }
        let validation_error = resource.validation_errors.first();
        let field_path = validation_error
            .and_then(|error| json_pointer_to_field_path(&error.path))
            .or_else(|| Some("apiVersion".to_string()));
        let message = validation_error
            .and_then(|error| concise_validation_message(field_path.as_deref(), &error.msg))
            .unwrap_or_else(|| concise_kubeconform_message(message));
        result.diagnostics.push(KubernetesYamlLintDiagnostic {
            severity: KubernetesYamlLintSeverity::Error,
            source: "Kubeconform".to_string(),
            message,
            field_path,
        });
    }
}

async fn run_kubeconform(
    binary: &Path,
    cache_dir: &Path,
    yaml: &str,
) -> Result<std::process::Output, AppError> {
    let child = Command::new(binary)
        .arg("-strict")
        .arg("-output")
        .arg("json")
        .arg("-summary")
        .arg("-cache")
        .arg(cache_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| {
            AppError::new(
                format!("Failed to start kubeconform: {error}"),
                AppErrorKind::Io,
            )
            .with_source(error)
        })?;
    communicate_with_child(child, yaml).await
}

async fn communicate_with_child(
    mut child: tokio::process::Child,
    yaml: &str,
) -> Result<std::process::Output, AppError> {
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| AppError::new("Failed to open kubeconform stdin.", AppErrorKind::Io))?;
    let write = async move {
        stdin.write_all(yaml.as_bytes()).await.map_err(|error| {
            AppError::new(
                format!("Failed to send YAML to kubeconform: {error}"),
                AppErrorKind::Io,
            )
            .with_source(error)
        })?;
        drop(stdin);
        Ok::<(), AppError>(())
    };
    let read = async move {
        child.wait_with_output().await.map_err(|error| {
            AppError::new(
                format!("Failed to read kubeconform output: {error}"),
                AppErrorKind::Io,
            )
            .with_source(error)
        })
    };
    // Drain stdout/stderr while writing so neither side can fill the other's pipe.
    let ((), output) = tokio::try_join!(write, read)?;
    Ok(output)
}

fn kubeconform_cache_dir(result: &mut KubernetesYamlLintResult) -> Option<PathBuf> {
    let cache_dir =
        env::var_os("KUBECOVE_KUBECONFORM_CACHE").map_or_else(default_cache_dir, PathBuf::from);
    match std::fs::create_dir_all(&cache_dir) {
        Ok(()) => Some(cache_dir),
        Err(error) => {
            result.notes.push(status_note(
                KubernetesYamlLintSeverity::Warning,
                "Kubeconform",
                format!("Could not create schema cache: {error}"),
            ));
            None
        }
    }
}

fn default_cache_dir() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        if let Some(local_app_data) = env::var_os("LOCALAPPDATA") {
            return PathBuf::from(local_app_data)
                .join("KubeCove")
                .join("kubeconform-cache");
        }
    }
    #[cfg(target_os = "macos")]
    {
        if let Some(home) = env::var_os("HOME") {
            return PathBuf::from(home)
                .join("Library")
                .join("Caches")
                .join("KubeCove")
                .join("kubeconform");
        }
    }
    if let Some(xdg_cache_home) = env::var_os("XDG_CACHE_HOME") {
        return PathBuf::from(xdg_cache_home)
            .join("kubecove")
            .join("kubeconform");
    }
    if let Some(home) = env::var_os("HOME") {
        return PathBuf::from(home)
            .join(".cache")
            .join("kubecove")
            .join("kubeconform");
    }
    env::temp_dir().join("kubecove-kubeconform-cache")
}

fn resolve_kubeconform_path() -> Option<PathBuf> {
    if let Some(path) = env::var_os("KUBECOVE_KUBECONFORM") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }

    let mut candidates = Vec::new();
    if let Ok(exe) = env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join(binary_name("kubeconform")));
            candidates.push(dir.join(binary_name(&format!("kubeconform-{}", target_triple()))));
        }
    }
    candidates.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("bin")
            .join(binary_name(&format!("kubeconform-{}", target_triple()))),
    );
    candidates.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("bin")
            .join(binary_name("kubeconform")),
    );

    candidates.into_iter().find(|candidate| candidate.is_file())
}

fn binary_name(stem: &str) -> String {
    if cfg!(target_os = "windows") {
        format!("{stem}.exe")
    } else {
        stem.to_string()
    }
}

fn target_triple() -> &'static str {
    if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        "x86_64-pc-windows-msvc"
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "x86_64-unknown-linux-gnu"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "x86_64-apple-darwin"
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "aarch64-apple-darwin"
    } else {
        "unknown"
    }
}

fn json_pointer_to_field_path(path: &str) -> Option<String> {
    let trimmed = path.trim().trim_start_matches('/');
    if trimmed.is_empty() {
        return None;
    }
    Some(
        trimmed
            .split('/')
            .map(|part| part.replace("~1", "/").replace("~0", "~"))
            .collect::<Vec<_>>()
            .join("."),
    )
}

fn concise_validation_message(field_path: Option<&str>, message: &str) -> Option<String> {
    let message = message.trim();
    if message.is_empty() {
        return None;
    }
    let field = field_path
        .and_then(|path| path.rsplit('.').next())
        .filter(|field| !field.is_empty())
        .unwrap_or("value");
    Some(format!("{field}: {}", concise_kubeconform_message(message)))
}

fn concise_kubeconform_message(message: &str) -> String {
    message
        .trim()
        .strip_prefix("problem validating schema. ")
        .unwrap_or(message.trim())
        .split(" - at ")
        .last()
        .unwrap_or(message.trim())
        .trim_matches('\'')
        .to_string()
}

fn status_note(
    severity: KubernetesYamlLintSeverity,
    source: impl Into<String>,
    message: impl Into<String>,
) -> KubernetesYamlLintStatusNote {
    KubernetesYamlLintStatusNote {
        severity,
        source: source.into(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_status_drives_validation_and_null_resources_are_empty() {
        let mut empty = result();
        collect_kubeconform_output(
            std::process::Output {
                status: exit_status(0),
                stdout: br#"{"resources":null}"#.to_vec(),
                stderr: Vec::new(),
            },
            &mut empty,
        );
        assert!(empty.notes.is_empty());
        let mut invalid = result();
        collect_kubeconform_output(std::process::Output { status: exit_status(1), stdout: br#"{"resources":[{"kind":"Widget","status":"statusInvalid","msg":"could not find schema is an invalid field value"}]}"#.to_vec(), stderr: Vec::new() }, &mut invalid);
        assert_eq!(invalid.diagnostics.len(), 1);
        assert!(invalid.notes.is_empty());
    }

    #[test]
    #[ignore = "subprocess fixture launched by drains_output_while_writing_input"]
    fn subprocess_pipe_fixture() {
        use std::io::{Read, Write};
        // Both payloads exceed OS pipe buffers and force concurrent draining.
        std::io::stdout()
            .write_all(&vec![b'x'; 1024 * 1024])
            .unwrap();
        std::io::stdout().flush().unwrap();
        let mut input = String::new();
        std::io::stdin().read_to_string(&mut input).unwrap();
        assert_eq!(input.len(), 1024 * 1024);
    }

    #[tokio::test]
    async fn drains_output_while_writing_input() {
        let child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "commands::resources::kubeconform::tests::subprocess_pipe_fixture",
                "--ignored",
                "--nocapture",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let output = communicate_with_child(child, &"y".repeat(1024 * 1024))
            .await
            .unwrap();
        assert!(output.status.success());
        assert!(output.stdout.len() >= 1024 * 1024);
    }

    fn result() -> KubernetesYamlLintResult {
        KubernetesYamlLintResult {
            diagnostics: Vec::new(),
            notes: Vec::new(),
        }
    }

    #[test]
    fn missing_schema_becomes_status_note() {
        let mut result = result();
        let output = std::process::Output {
            status: exit_status(1),
            stdout: br#"{"resources":[{"kind":"Widget","status":"ERROR","msg":"could not find schema for Widget"}]}"#
                .to_vec(),
            stderr: Vec::new(),
        };

        collect_kubeconform_output(output, &mut result);

        assert!(result.diagnostics.is_empty());
        assert_eq!(result.notes.len(), 1);
    }

    #[test]
    fn schema_download_failure_becomes_status_note() {
        let mut result = result();
        let output = std::process::Output {
            status: exit_status(1),
            stdout: br#"{"resources":[{"kind":"Deployment","status":"statusError","msg":"failed downloading schema from https://raw.githubusercontent.com/example/schema.json"}]}"#
                .to_vec(),
            stderr: Vec::new(),
        };

        collect_kubeconform_output(output, &mut result);

        assert!(result.diagnostics.is_empty());
        assert_eq!(result.notes.len(), 1);
    }

    #[test]
    fn invalid_schema_becomes_diagnostic() {
        let mut result = result();
        let output = std::process::Output {
            status: exit_status(1),
            stdout: br#"{"resources":[{"kind":"Deployment","status":"statusInvalid","msg":"Additional property bad is not allowed","validationErrors":[{"path":"/spec","msg":"additional property"}]}]}"#
                .to_vec(),
            stderr: Vec::new(),
        };

        collect_kubeconform_output(output, &mut result);

        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(result.diagnostics[0].field_path.as_deref(), Some("spec"));
        assert_eq!(result.diagnostics[0].message, "spec: additional property");
        assert_eq!(result.notes.len(), 0);
    }

    #[cfg(unix)]
    fn exit_status(code: i32) -> std::process::ExitStatus {
        use std::os::unix::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(code << 8)
    }

    #[cfg(windows)]
    fn exit_status(code: u32) -> std::process::ExitStatus {
        use std::os::windows::process::ExitStatusExt;
        std::process::ExitStatus::from_raw(code)
    }
}
