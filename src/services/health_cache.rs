//! On-disk cache for per-chain KPI snapshots and their last-check timestamps.
//!
//! The snapshot file lives in the per-workspace directory used elsewhere
//! (`~/Library/Application Support/ais-monitor/{rg}_{app}/health_cache.json` on
//! macOS) so each Azure profile keeps its own cached KPIs.
//!
//! Saved data is the same content rendered in the "from last check" placeholder
//! in `ChainDetailView` when no fresh data has been fetched yet — persisting it
//! means that placeholder survives across app restarts instead of disappearing.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::components::chain_detail::ChainHealth;

const FILENAME: &str = "health_cache.json";

#[derive(Default, Debug, Clone, Serialize, Deserialize)]
pub struct HealthSnapshot {
    #[serde(default)]
    pub health: HashMap<String, ChainHealth>,
    /// Epoch-seconds at which each chain was last checked.
    #[serde(default)]
    pub last_checked: HashMap<String, u64>,
}

fn cache_path(workspace_dir: &str) -> PathBuf {
    Path::new(workspace_dir).join(FILENAME)
}

/// Loads the cached snapshot for a workspace. Returns an empty snapshot if the
/// file doesn't exist or is unreadable — callers should still feel free to
/// overwrite it.
pub fn load(workspace_dir: &str) -> HealthSnapshot {
    if workspace_dir.is_empty() {
        return HealthSnapshot::default();
    }
    let path = cache_path(workspace_dir);
    let content = std::fs::read_to_string(path).unwrap_or_default();
    serde_json::from_str(&content).unwrap_or_default()
}

/// Persists the snapshot to disk. Failures are silent — this is a best-effort
/// cache, not authoritative state.
pub fn save(workspace_dir: &str, snapshot: &HealthSnapshot) {
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
}
