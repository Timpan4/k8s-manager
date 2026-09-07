use crate::models::AppErrorKind;
use crate::models::{
    AppError, BackendDiagnosticEvent, BackendDiagnosticField, BackendDiagnosticStatus,
};
use chrono::Utc;
use std::{
    collections::VecDeque,
    sync::{Mutex, OnceLock},
    time::Instant,
};

const MAX_BACKEND_DIAGNOSTIC_EVENTS: usize = 500;

#[derive(Default)]
struct BackendDiagnosticsState {
    enabled: bool,
    sequence: u64,
    events: VecDeque<BackendDiagnosticEvent>,
}

fn state() -> &'static Mutex<BackendDiagnosticsState> {
    static STATE: OnceLock<Mutex<BackendDiagnosticsState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(BackendDiagnosticsState::default()))
}

fn with_state<T>(callback: impl FnOnce(&mut BackendDiagnosticsState) -> T) -> T {
    let mut guard = state()
        .lock()
        .expect("backend diagnostics state mutex poisoned");
    callback(&mut guard)
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

pub fn diagnostic_field(key: &str, value: impl ToString) -> BackendDiagnosticField {
    BackendDiagnosticField {
        key: key.to_string(),
        value: value.to_string(),
    }
}

pub fn record_backend_timing(
    command: &str,
    status: BackendDiagnosticStatus,
    duration_ms: u64,
    summary: Vec<BackendDiagnosticField>,
) {
    with_state(|state| {
        if !state.enabled {
            return;
        }
        state.sequence = state.sequence.saturating_add(1);
        state.events.push_back(BackendDiagnosticEvent {
            id: state.sequence,
            recorded_at: Utc::now().to_rfc3339(),
            command: command.to_string(),
            status,
            duration_ms,
            summary,
        });
        while state.events.len() > MAX_BACKEND_DIAGNOSTIC_EVENTS {
            state.events.pop_front();
        }
    });
}

pub fn record_backend_success(
    command: &str,
    started: Instant,
    summary: Vec<BackendDiagnosticField>,
) {
    record_backend_timing(
        command,
        BackendDiagnosticStatus::Ok,
        elapsed_ms(started),
        summary,
    );
}

pub fn record_backend_error(command: &str, started: Instant, error_kind: &str) {
    record_backend_timing(
        command,
        BackendDiagnosticStatus::Error,
        elapsed_ms(started),
        vec![diagnostic_field("errorKind", error_kind)],
    );
}

pub fn record_backend_cancelled(command: &str, started: Instant) {
    record_backend_timing(
        command,
        BackendDiagnosticStatus::Cancelled,
        elapsed_ms(started),
        Vec::new(),
    );
}

pub fn record_backend_result<T>(
    command: &str,
    started: Instant,
    result: &Result<T, AppError>,
    success_summary: impl FnOnce(&T) -> Vec<BackendDiagnosticField>,
) {
    match result {
        Ok(value) => record_backend_success(command, started, success_summary(value)),
        Err(error) if error.kind == AppErrorKind::Cancelled => {
            record_backend_cancelled(command, started);
        }
        Err(error) => record_backend_error(command, started, error.kind.as_str()),
    }
}

#[tauri::command]
pub fn set_backend_diagnostics_enabled(enabled: bool) -> bool {
    with_state(|state| {
        state.enabled = enabled;
    });
    enabled
}

#[tauri::command]
pub fn get_backend_diagnostics() -> Vec<BackendDiagnosticEvent> {
    with_state(|state| state.events.iter().cloned().collect())
}

#[tauri::command]
pub fn clear_backend_diagnostics() {
    with_state(|state| {
        state.events.clear();
        state.sequence = 0;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_store_drops_events_then_records_bounded_events_and_clears() {
        clear_backend_diagnostics();
        set_backend_diagnostics_enabled(false);
        record_backend_timing(
            "list_resource_scope",
            BackendDiagnosticStatus::Ok,
            12,
            vec![diagnostic_field("rows", 5)],
        );
        assert!(get_backend_diagnostics().is_empty());

        set_backend_diagnostics_enabled(true);
        for index in 0..505 {
            record_backend_timing(
                "list_resource_scope",
                BackendDiagnosticStatus::Ok,
                index,
                vec![diagnostic_field("rows", index)],
            );
        }
        let events = get_backend_diagnostics();
        assert_eq!(events.len(), MAX_BACKEND_DIAGNOSTIC_EVENTS);
        assert_eq!(events.first().map(|event| event.id), Some(6));
        assert_eq!(events.last().map(|event| event.duration_ms), Some(504));
        assert_eq!(
            events.last().and_then(|event| event.summary.first()),
            Some(&diagnostic_field("rows", 504))
        );

        clear_backend_diagnostics();
        assert!(get_backend_diagnostics().is_empty());

        record_backend_result(
            "tracked_success",
            Instant::now(),
            &Ok::<_, AppError>(5),
            |rows| vec![diagnostic_field("rows", rows)],
        );
        record_backend_result::<()>(
            "tracked_cancelled",
            Instant::now(),
            &Err(AppError::cancelled()),
            |()| Vec::new(),
        );
        record_backend_result::<()>(
            "tracked_error",
            Instant::now(),
            &Err(AppError::new("boom", AppErrorKind::Transport)),
            |()| Vec::new(),
        );
        let events = get_backend_diagnostics();
        assert_eq!(events.len(), 3);
        assert_eq!(events[0].status, BackendDiagnosticStatus::Ok);
        assert_eq!(events[1].status, BackendDiagnosticStatus::Cancelled);
        assert_eq!(events[2].status, BackendDiagnosticStatus::Error);
        assert_eq!(
            events[2].summary,
            vec![diagnostic_field("errorKind", "transport")]
        );

        clear_backend_diagnostics();
        set_backend_diagnostics_enabled(false);
    }
}
