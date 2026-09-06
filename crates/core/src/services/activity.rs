//! In-process activity event log.
//!
//! A sparse, structured timeline of user actions (sent a Service Bus message,
//! triggered a workflow), background operations ("Check all" results), and
//! Azure CLI errors — anything that's worth being able to reconstruct after the
//! fact and that the cell-local UI feedback discards. **Not** a tail of
//! subprocess stdout: ais-monitor doesn't own long-running processes.
//!
//! The buffer is a global ring of the most recent N events accessible from any
//! thread (including `spawn_blocking` closures), and is mirrored to
//! `{workspace_dir}/activity.jsonl` so it survives restart. The panel UI polls
//! the in-memory ring on a short timer and surfaces unread-error counts on the
//! floating launcher button.

use chrono::Local;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

const MAX_EVENTS: usize = 500;
const PERSIST_FILE: &str = "activity.jsonl";
/// Above this size, the JSONL file is truncated to the most recent half on the
/// next persistence write. Keeps the file from growing unboundedly.
const PERSIST_MAX_BYTES: u64 = 1_000_000;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActivityLevel {
    Info,
    Ok,
    Warn,
    Error,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ActivityEvent {
    /// Local clock time in HH:MM:SS — what users want to read in the panel.
    pub time: String,
    /// Epoch-millis — used for stable ordering across machines and for the
    /// "X minutes ago" derivation if we add it later.
    #[serde(default)]
    pub ts_ms: i64,
    pub level: ActivityLevel,
    pub action: String,
    pub target: String,
    #[serde(default)]
    pub detail: String,
}

static EVENTS: OnceLock<Mutex<Vec<ActivityEvent>>> = OnceLock::new();
/// Bumped on every push/clear so polling panels can detect changes cheaply.
static TICK: AtomicU64 = AtomicU64::new(0);
/// Errors logged since the user last opened the panel. Drives the FAB badge.
static UNREAD_ERRORS: AtomicU64 = AtomicU64::new(0);
static WORKSPACE_DIR: OnceLock<Mutex<String>> = OnceLock::new();

fn buffer() -> &'static Mutex<Vec<ActivityEvent>> {
    EVENTS.get_or_init(|| Mutex::new(Vec::with_capacity(MAX_EVENTS)))
}

fn workspace_slot() -> &'static Mutex<String> {
    WORKSPACE_DIR.get_or_init(|| Mutex::new(String::new()))
}

fn current_workspace_dir() -> String {
    workspace_slot()
        .lock()
        .map(|d| d.clone())
        .unwrap_or_default()
}

/// Set the workspace directory used for persistence, and hydrate the in-memory
/// ring from any existing on-disk JSONL. Idempotent — safe to call repeatedly
/// when the user switches profiles.
pub fn set_workspace_dir(dir: String) {
    {
        let mut slot = match workspace_slot().lock() {
            Ok(s) => s,
            Err(_) => return,
        };
        if *slot == dir {
            return; // already current
        }
        *slot = dir.clone();
    }
    // Load existing events from disk into the in-memory ring (most-recent
    // suffix only — older lines stay on disk but won't fit our budget).
    let path = std::path::Path::new(&dir).join(PERSIST_FILE);
    if let Ok(content) = std::fs::read_to_string(&path) {
        let loaded: Vec<ActivityEvent> = content
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        if let Ok(mut buf) = buffer().lock() {
            let start = loaded.len().saturating_sub(MAX_EVENTS);
            *buf = loaded[start..].to_vec();
        }
        TICK.fetch_add(1, Ordering::Relaxed);
    }
}

/// Push an event. Thread-safe — call from anywhere, including non-Dioxus code.
pub fn log(
    level: ActivityLevel,
    action: impl Into<String>,
    target: impl Into<String>,
    detail: impl Into<String>,
) {
    let now = Local::now();
    let event = ActivityEvent {
        time: now.format("%H:%M:%S").to_string(),
        ts_ms: now.timestamp_millis(),
        level,
        action: action.into(),
        target: target.into(),
        detail: detail.into(),
    };

    if let Ok(mut buf) = buffer().lock() {
        buf.push(event.clone());
        if buf.len() > MAX_EVENTS {
            let drop = buf.len() - MAX_EVENTS;
            buf.drain(..drop);
        }
    }
    if level == ActivityLevel::Error {
        UNREAD_ERRORS.fetch_add(1, Ordering::Relaxed);
    }
    TICK.fetch_add(1, Ordering::Relaxed);

    // Persist on a detached OS thread so the caller is never blocked on disk.
    let dir = current_workspace_dir();
    if !dir.is_empty() {
        std::thread::spawn(move || persist(&dir, &event));
    }
}

fn persist(dir: &str, event: &ActivityEvent) {
    let path = std::path::Path::new(dir).join(PERSIST_FILE);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    // Rotate if the file has gotten large — keep the most recent half.
    if let Ok(meta) = std::fs::metadata(&path) {
        if meta.len() > PERSIST_MAX_BYTES {
            if let Ok(content) = std::fs::read_to_string(&path) {
                let lines: Vec<&str> = content.lines().collect();
                let keep_from = lines.len() / 2;
                let trimmed = lines[keep_from..].join("\n");
                crate::services::store::write_best_effort(&path, &(trimmed + "\n"));
            }
        }
    }

    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        if let Ok(line) = serde_json::to_string(event) {
            let _ = writeln!(file, "{}", line);
        }
    }
}

// ── Convenience wrappers (keep call sites short) ─────────────────────────────

pub fn info(action: impl Into<String>, target: impl Into<String>) {
    log(ActivityLevel::Info, action, target, "");
}
pub fn ok(action: impl Into<String>, target: impl Into<String>) {
    log(ActivityLevel::Ok, action, target, "");
}
pub fn warn(action: impl Into<String>, target: impl Into<String>, detail: impl Into<String>) {
    log(ActivityLevel::Warn, action, target, detail);
}
pub fn error(action: impl Into<String>, target: impl Into<String>, detail: impl Into<String>) {
    log(ActivityLevel::Error, action, target, detail);
}

// ── Read API for the panel ───────────────────────────────────────────────────

pub fn snapshot() -> Vec<ActivityEvent> {
    buffer().lock().map(|b| b.clone()).unwrap_or_default()
}

pub fn tick() -> u64 {
    TICK.load(Ordering::Relaxed)
}

pub fn unread_errors() -> u64 {
    UNREAD_ERRORS.load(Ordering::Relaxed)
}

pub fn mark_read() {
    UNREAD_ERRORS.store(0, Ordering::Relaxed);
}

pub fn clear() {
    if let Ok(mut buf) = buffer().lock() {
        buf.clear();
    }
    UNREAD_ERRORS.store(0, Ordering::Relaxed);
    TICK.fetch_add(1, Ordering::Relaxed);
}
