//! On-disk cache for the Resource Health dashboard — the last discovered
//! set of resources and their state/health, so the dashboard paints
//! instantly on tab switch instead of blocking on a fresh `az` sweep.
//! Keyed by subscription + resource group + app name, same convention as
//! `functions_cache.rs`.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::services::azure::ResourceHealthRow;

const FILENAME: &str = "resource_health_cache.json";

#[derive(Default, Debug, Clone, Serialize, Deserialize)]
pub struct ResourceHealthSnapshot {
    #[serde(default)]
    pub rows: Vec<ResourceHealthRow>,
    #[serde(default)]
    pub last_fetched: u64,
}

fn cache_path(workspace_dir: &str) -> PathBuf {
    Path::new(workspace_dir).join(FILENAME)
}

pub fn load(workspace_dir: &str) -> ResourceHealthSnapshot {
    if workspace_dir.is_empty() {
        return ResourceHealthSnapshot::default();
    }
    let path = cache_path(workspace_dir);
    let content = std::fs::read_to_string(path).unwrap_or_default();
    serde_json::from_str(&content).unwrap_or_default()
}

pub fn save(workspace_dir: &str, snapshot: &ResourceHealthSnapshot) {
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

fn workspace_dir(sub: &str, rg: &str, app: &str) -> String {
    dirs::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("ais-monitor")
        .join(format!("{}_{}_{}", sub, rg, app))
        .to_string_lossy()
        .to_string()
}

pub fn load_for(sub: &str, rg: &str, app: &str) -> ResourceHealthSnapshot {
    load(&workspace_dir(sub, rg, app))
}

pub fn save_for(sub: &str, rg: &str, app: &str, snapshot: &ResourceHealthSnapshot) {
    save(&workspace_dir(sub, rg, app), snapshot);
}
