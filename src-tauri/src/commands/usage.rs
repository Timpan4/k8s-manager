#[cfg(target_os = "macos")]
use super::usage_webview::{
    is_macos_orphan_webkit_process, macos_orphan_webview_process_candidate,
    selected_orphan_webview_cohort_start_time,
};
#[cfg(target_os = "windows")]
use super::usage_webview::{is_orphan_webview_start_candidate, is_windows_app_webview_process};
use super::usage_webview::{is_webview_descendant_process, webview_process_role};
#[cfg(target_os = "windows")]
use super::usage_webview::{
    is_windows_app_webview_browser_process, windows_webview_host_exe_names,
};
use crate::models::AppErrorKind;
use crate::models::{AppError, AppUsageMetrics, AppUsageMetricsBreakdown};
use chrono::Utc;
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use sysinfo::{
    get_current_pid, Pid, Process, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind,
};

#[derive(Default)]
pub struct AppUsageMonitor {
    system: Mutex<System>,
}

impl AppUsageMonitor {
    pub fn sample(&self) -> Result<AppUsageMetrics, AppError> {
        let current_pid = get_current_pid()
            .map_err(|message| AppError::new(message, AppErrorKind::UsageMetrics))?;
        let mut system = self.system.lock().map_err(|_| {
            AppError::new(
                "usage metrics monitor is unavailable",
                AppErrorKind::UsageMetrics,
            )
        })?;

        system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing().with_cmd(UpdateKind::Always),
        );

        let pids = usage_process_pids(&system, current_pid);
        let pids_to_refresh: Vec<Pid> = pids.iter().copied().collect();
        if !pids_to_refresh.is_empty() {
            system.refresh_processes_specifics(
                ProcessesToUpdate::Some(&pids_to_refresh),
                true,
                ProcessRefreshKind::nothing()
                    .with_memory()
                    .with_cpu()
                    .with_cmd(UpdateKind::Always),
            );
        }
        let process_count = observed_process_count(&system, &pids);
        let memory_bytes = process_tree_memory_bytes(&system, &pids);
        let raw_cpu_percent = process_tree_cpu_percent(&system, &pids);
        let cpu_count = available_cpu_count();
        let cpu_percent = normalize_cpu_percent(raw_cpu_percent, cpu_count);
        let breakdown = process_tree_breakdown(&system, &pids, current_pid, cpu_count);

        Ok(AppUsageMetrics {
            cpu_percent,
            memory_bytes,
            process_count,
            sampled_at: Utc::now().to_rfc3339(),
            breakdown,
        })
    }
}

#[tauri::command]
pub fn get_app_usage_metrics(
    monitor: tauri::State<'_, AppUsageMonitor>,
) -> Result<AppUsageMetrics, AppError> {
    monitor.sample()
}

fn available_cpu_count() -> usize {
    std::thread::available_parallelism().map_or(1, usize::from)
}

pub(crate) fn normalize_cpu_percent(raw_cpu_percent: f32, cpu_count: usize) -> f32 {
    if !raw_cpu_percent.is_finite() {
        return 0.0;
    }

    let divisor = cpu_count.max(1) as f32;
    (raw_cpu_percent / divisor).clamp(0.0, 100.0)
}

fn process_tree_pids(system: &System, root_pid: Pid) -> HashSet<Pid> {
    let relations = system
        .processes()
        .iter()
        .map(|(pid, process)| (*pid, process.parent()));
    process_tree_pids_from_roots([root_pid], relations)
}

fn usage_process_pids(system: &System, root_pid: Pid) -> HashSet<Pid> {
    let mut pids = process_tree_pids(system, root_pid);
    add_related_orphan_webview_pids(system, root_pid, &mut pids);
    pids
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn add_related_orphan_webview_pids(_system: &System, _root_pid: Pid, _pids: &mut HashSet<Pid>) {}

#[cfg(target_os = "windows")]
fn add_related_orphan_webview_pids(system: &System, root_pid: Pid, pids: &mut HashSet<Pid>) {
    let Some(root_process) = system.process(root_pid) else {
        return;
    };
    let root_start_time = root_process.start_time();
    let host_exe_names = windows_webview_host_exe_names(root_process);
    if host_exe_names.is_empty() {
        return;
    }

    let webview_roots: HashSet<Pid> = system
        .processes()
        .iter()
        .filter_map(|(pid, process)| {
            if !is_windows_app_webview_process(process, &host_exe_names) {
                return None;
            }
            if is_windows_app_webview_browser_process(process, &host_exe_names)
                || is_orphan_webview_start_candidate(root_start_time, process.start_time())
            {
                Some(*pid)
            } else {
                None
            }
        })
        .collect();
    if webview_roots.is_empty() {
        return;
    }

    let relations = system
        .processes()
        .iter()
        .map(|(pid, process)| (*pid, process.parent()));
    pids.extend(process_tree_pids_from_roots(webview_roots, relations));
}

#[cfg(target_os = "macos")]
fn add_related_orphan_webview_pids(system: &System, root_pid: Pid, pids: &mut HashSet<Pid>) {
    let Some(root_process) = system.process(root_pid) else {
        return;
    };
    let root_start_time = root_process.start_time();
    let candidate_start_time = selected_orphan_webview_cohort_start_time(
        root_start_time,
        system
            .processes()
            .iter()
            .filter(|(pid, process)| !pids.contains(pid) && is_macos_orphan_webkit_process(process))
            .filter_map(|(_, process)| macos_orphan_webview_process_candidate(process)),
    );

    let Some(candidate_start_time) = candidate_start_time else {
        return;
    };

    pids.extend(
        system
            .processes()
            .iter()
            .filter(|(_, process)| {
                is_macos_orphan_webkit_process(process)
                    && process.start_time() == candidate_start_time
            })
            .map(|(pid, _)| *pid),
    );
}

// Test helper for process-tree traversal without constructing sysinfo snapshots.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn process_tree_pids_from_relations(
    root_pid: Pid,
    relations: impl IntoIterator<Item = (Pid, Option<Pid>)>,
) -> HashSet<Pid> {
    process_tree_pids_from_roots([root_pid], relations)
}

fn process_tree_pids_from_roots(
    root_pids: impl IntoIterator<Item = Pid>,
    relations: impl IntoIterator<Item = (Pid, Option<Pid>)>,
) -> HashSet<Pid> {
    let relations: Vec<(Pid, Option<Pid>)> = relations.into_iter().collect();
    let mut pids: HashSet<Pid> = root_pids.into_iter().collect();
    let mut changed = true;

    while changed {
        changed = false;
        for (pid, parent) in &relations {
            if parent.is_some_and(|parent_pid| pids.contains(&parent_pid)) && pids.insert(*pid) {
                changed = true;
            }
        }
    }

    pids
}

fn observed_process_count(system: &System, pids: &HashSet<Pid>) -> usize {
    pids.iter()
        .filter(|pid| system.process(**pid).is_some())
        .count()
}

fn process_tree_memory_bytes(system: &System, pids: &HashSet<Pid>) -> u64 {
    pids.iter()
        .filter_map(|pid| system.process(*pid))
        .map(sysinfo::Process::memory)
        .sum()
}

fn process_tree_cpu_percent(system: &System, pids: &HashSet<Pid>) -> f32 {
    pids.iter()
        .filter_map(|pid| system.process(*pid))
        .map(sysinfo::Process::cpu_usage)
        .sum()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UsageProcessGroup {
    App,
    WebView,
    OtherChildren,
}

impl UsageProcessGroup {
    fn label(self) -> &'static str {
        match self {
            Self::App => "KubeCove",
            Self::WebView => "WebView",
            Self::OtherChildren => "Other children",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::App => "Rust/Tauri host process",
            Self::WebView => "Embedded WebView browser runtime",
            Self::OtherChildren => "Non-WebView child processes",
        }
    }

    fn child_label(self, index: usize) -> String {
        match self {
            Self::App => "Host process".to_string(),
            Self::WebView => format!("WebView process {index}"),
            Self::OtherChildren => format!("Child process {index}"),
        }
    }
}

#[derive(Default)]
struct UsageProcessGroupTotals {
    raw_cpu_percent: f32,
    memory_bytes: u64,
    process_count: usize,
    children: Vec<UsageProcessSample>,
}

struct UsageProcessSample {
    label: String,
    description: String,
    raw_cpu_percent: f32,
    memory_bytes: u64,
}

impl UsageProcessGroupTotals {
    fn add_process(&mut self, process: &Process, include_child_sample: bool) {
        self.raw_cpu_percent += process.cpu_usage();
        self.memory_bytes += process.memory();
        self.process_count += 1;
        if include_child_sample {
            self.children.push(UsageProcessSample {
                label: usage_process_child_label(process),
                description: usage_process_child_description(process),
                raw_cpu_percent: process.cpu_usage(),
                memory_bytes: process.memory(),
            });
        }
    }
}

fn process_tree_breakdown(
    system: &System,
    pids: &HashSet<Pid>,
    root_pid: Pid,
    cpu_count: usize,
) -> Vec<AppUsageMetricsBreakdown> {
    let mut buckets = [
        (UsageProcessGroup::App, UsageProcessGroupTotals::default()),
        (
            UsageProcessGroup::WebView,
            UsageProcessGroupTotals::default(),
        ),
        (
            UsageProcessGroup::OtherChildren,
            UsageProcessGroupTotals::default(),
        ),
    ];

    for pid in pids {
        let Some(process) = system.process(*pid) else {
            continue;
        };
        let group = usage_process_group(*pid, root_pid, process);
        let bucket_index = match group {
            UsageProcessGroup::App => 0,
            UsageProcessGroup::WebView => 1,
            UsageProcessGroup::OtherChildren => 2,
        };
        buckets[bucket_index]
            .1
            .add_process(process, group != UsageProcessGroup::App);
    }

    buckets
        .into_iter()
        .filter(|(_, totals)| totals.process_count > 0)
        .map(|(group, totals)| AppUsageMetricsBreakdown {
            label: group.label().to_string(),
            description: group.description().to_string(),
            cpu_percent: normalize_cpu_percent(totals.raw_cpu_percent, cpu_count),
            memory_bytes: totals.memory_bytes,
            process_count: totals.process_count,
            children: usage_process_children(group, totals.children, cpu_count),
        })
        .collect()
}

fn usage_process_children(
    group: UsageProcessGroup,
    mut children: Vec<UsageProcessSample>,
    cpu_count: usize,
) -> Vec<AppUsageMetricsBreakdown> {
    children.sort_by_key(|child| std::cmp::Reverse(child.memory_bytes));
    let mut label_counts: HashMap<String, usize> = HashMap::new();
    children
        .into_iter()
        .enumerate()
        .map(|(index, child)| AppUsageMetricsBreakdown {
            label: if child.label.is_empty() {
                group.child_label(index + 1)
            } else {
                let count = label_counts.entry(child.label.clone()).or_insert(0);
                *count += 1;
                if *count == 1 {
                    child.label
                } else {
                    format!("{} {}", child.label, count)
                }
            },
            description: child.description,
            cpu_percent: normalize_cpu_percent(child.raw_cpu_percent, cpu_count),
            memory_bytes: child.memory_bytes,
            process_count: 1,
            children: Vec::new(),
        })
        .collect()
}

fn usage_process_child_label(process: &Process) -> String {
    webview_process_role(process).unwrap_or_default()
}

fn usage_process_child_description(process: &Process) -> String {
    if is_webview_descendant_process(process) {
        "WebView child process".to_string()
    } else {
        "Child process".to_string()
    }
}

fn usage_process_group(pid: Pid, root_pid: Pid, process: &Process) -> UsageProcessGroup {
    if pid == root_pid {
        return UsageProcessGroup::App;
    }

    if is_webview_descendant_process(process) {
        UsageProcessGroup::WebView
    } else {
        UsageProcessGroup::OtherChildren
    }
}

#[cfg(test)]
mod tests {
    use super::{
        normalize_cpu_percent, process_tree_pids_from_relations, process_tree_pids_from_roots,
    };
    use sysinfo::Pid;

    fn pid(value: u32) -> Pid {
        Pid::from_u32(value)
    }

    #[test]
    fn normalizes_process_tree_cpu_to_host_percentage() {
        assert!((normalize_cpu_percent(240.0, 8) - 30.0).abs() < f32::EPSILON);
        assert!((normalize_cpu_percent(250.0, 0) - 100.0).abs() < f32::EPSILON);
        assert!(normalize_cpu_percent(f32::NAN, 8).abs() < f32::EPSILON);
    }

    #[test]
    fn collects_recursive_child_processes() {
        let pids = process_tree_pids_from_relations(
            pid(10),
            [
                (pid(10), None),
                (pid(11), Some(pid(10))),
                (pid(12), Some(pid(11))),
                (pid(20), None),
            ],
        );

        assert!(pids.contains(&pid(10)));
        assert!(pids.contains(&pid(11)));
        assert!(pids.contains(&pid(12)));
        assert!(!pids.contains(&pid(20)));
    }

    #[test]
    fn ignores_processes_without_parent_link_to_host() {
        let pids = process_tree_pids_from_relations(
            pid(10),
            [
                (pid(10), None),
                (pid(11), Some(pid(10))),
                (pid(30), None),
                (pid(31), Some(pid(30))),
            ],
        );

        assert_eq!(pids, [pid(10), pid(11)].into_iter().collect());
    }

    #[test]
    fn collects_multiple_process_roots_for_reused_webview_browser() {
        let pids = process_tree_pids_from_roots(
            [pid(10), pid(30)],
            [
                (pid(10), None),
                (pid(30), Some(pid(99))),
                (pid(31), Some(pid(30))),
                (pid(32), Some(pid(30))),
                (pid(40), None),
                (pid(41), Some(pid(40))),
            ],
        );

        assert_eq!(
            pids,
            [pid(10), pid(30), pid(31), pid(32)].into_iter().collect()
        );
    }
}
