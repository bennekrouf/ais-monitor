//! Per-chain success-rate history.
//!
//! Every time the user runs a chain check (individual or "Check all"), one
//! snapshot is appended for that chain. The chain list draws a small inline
//! sparkline from the recent values so users see "this chain has been getting
//! worse" trends without leaving the dashboard.
//!
//! Snapshots are bounded at `MAX_PER_CHAIN` per chain (default 30) and
//! persisted to `{workspace_dir}/history.json`.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const FILENAME: &str = "history.json";
const MAX_PER_CHAIN: usize = 30;

#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
pub struct HealthPoint {
    /// Epoch-seconds at which this snapshot was taken.
    pub ts: u64,
    /// Success rate 0-100, or None if there were no runs in the sample.
    pub success_rate: Option<f64>,
    pub dead_letters: i64,
    pub stuck_count: usize,
    pub failure_streak: usize,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct HistorySnapshot {
    /// chain_label → bounded Vec of points (oldest first).
    #[serde(default)]
    pub chains: HashMap<String, Vec<HealthPoint>>,
}

fn cache_path(workspace_dir: &str) -> PathBuf {
    Path::new(workspace_dir).join(FILENAME)
}

/// Cached load result so the disk only gets read once per session per workspace.
static CACHE: Mutex<Option<(String, HistorySnapshot)>> = Mutex::new(None);

pub fn load(workspace_dir: &str) -> HistorySnapshot {
    if workspace_dir.is_empty() {
        return HistorySnapshot::default();
    }
    // Fast path: same workspace, already cached.
    if let Ok(guard) = CACHE.lock() {
        if let Some((dir, snap)) = guard.as_ref() {
            if dir == workspace_dir {
                return snap.clone();
            }
        }
    }
    let path = cache_path(workspace_dir);
    let content = std::fs::read_to_string(path).unwrap_or_default();
    let snap: HistorySnapshot = serde_json::from_str(&content).unwrap_or_default();
    if let Ok(mut guard) = CACHE.lock() {
        *guard = Some((workspace_dir.to_string(), snap.clone()));
    }
    snap
}

pub fn save(workspace_dir: &str, snapshot: &HistorySnapshot) {
    if workspace_dir.is_empty() {
        return;
    }
    let path = cache_path(workspace_dir);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(snapshot) {
        crate::services::store::write_best_effort(&path, &json);
    }
    if let Ok(mut guard) = CACHE.lock() {
        *guard = Some((workspace_dir.to_string(), snapshot.clone()));
    }
}

/// Append a single snapshot to a chain's history, trimming to MAX_PER_CHAIN.
/// Convenience wrapper for callers that don't manipulate the full HistorySnapshot.
pub fn append(workspace_dir: &str, chain_label: &str, point: HealthPoint) {
    if workspace_dir.is_empty() {
        return;
    }
    let mut snap = load(workspace_dir);
    let series = snap.chains.entry(chain_label.to_string()).or_default();
    series.push(point);
    if series.len() > MAX_PER_CHAIN {
        let drop = series.len() - MAX_PER_CHAIN;
        series.drain(..drop);
    }
    save(workspace_dir, &snap);
}
