use crate::models::AppErrorKind;
use std::{fs, io::Read};

use crate::models::AppError;
use tauri::AppHandle;
use tauri_plugin_dialog::{DialogExt, FilePath};

const WORKSPACE_IMPORT_MAX_BYTES: u64 = 2 * 1024 * 1024;

#[tauri::command]
pub fn save_workspace_export_json(
    app: AppHandle,
    file_name: String,
    content: String,
) -> Result<bool, AppError> {
    let Some(file_path) = app
        .dialog()
        .file()
        .set_title("Export KubeCove workspace")
        .set_file_name(file_name)
        .add_filter("JSON", &["json"])
        .blocking_save_file()
    else {
        return Ok(false);
    };

    let path = file_path_to_path(file_path)?;
    fs::write(&path, content).map_err(|err| {
        AppError::new(
            format!("failed to write workspace export: {err}"),
            AppErrorKind::Io,
        )
        .with_source(err)
    })?;
    Ok(true)
}

#[tauri::command]
pub fn pick_workspace_import_json(app: AppHandle) -> Result<Option<String>, AppError> {
    let Some(file_path) = app
        .dialog()
        .file()
        .set_title("Import KubeCove workspace")
        .add_filter("JSON", &["json"])
        .blocking_pick_file()
    else {
        return Ok(None);
    };

    let path = file_path_to_path(file_path)?;
    let file = fs::File::open(&path).map_err(|err| {
        AppError::new(
            format!("failed to open workspace import: {err}"),
            AppErrorKind::Io,
        )
        .with_source(err)
    })?;
    let mut bytes = Vec::new();
    file.take(WORKSPACE_IMPORT_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|err| {
            AppError::new(
                format!("failed to read workspace import: {err}"),
                AppErrorKind::Io,
            )
            .with_source(err)
        })?;
    if bytes.len() > WORKSPACE_IMPORT_MAX_BYTES as usize {
        return Err(AppError::new(
            "workspace import JSON must be 2 MiB or smaller",
            AppErrorKind::Validation,
        ));
    }
    let content = String::from_utf8(bytes).map_err(|error| {
        AppError::new(
            "workspace import JSON must use UTF-8 encoding",
            AppErrorKind::Validation,
        )
        .with_source(error)
    })?;
    Ok(Some(content))
}

fn file_path_to_path(file_path: FilePath) -> Result<std::path::PathBuf, AppError> {
    file_path.into_path().map_err(|error| {
        AppError::new("workspace file path is not available", AppErrorKind::Io).with_source(error)
    })
}
