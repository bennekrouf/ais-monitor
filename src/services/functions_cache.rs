//! On-disk cache for the Functions tab — discovered Function Apps, their
//! functions, the linked Application Insights resource, and the last fetched
//! metrics. Keyed by subscription + resource group + app name so each
//! workspace has its own file, even across tenants that reuse names.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::services::azure::{FunctionApp, FunctionDetail, FunctionMetrics};

const FILENAME: &str = "functions_cache.json";

#[derive(Default, Debug, Clone, Serialize, Deserialize)]
pub struct FunctionsSnapshot {
    #[serde(default)]
    pub func_apps: Vec<FunctionApp>,
    /// (app_name, functions)
    #[serde(default)]
    pub functions: Vec<(String, Vec<FunctionDetail>)>,
    #[serde(default)]
    pub app_insights_name: Option<String>,
    /// (app_name, metrics)
    #[serde(default)]
    pub metrics: Vec<(String, Vec<FunctionMetrics>)>,
    /// Epoch-seconds at which the snapshot was last written.
    #[serde(default)]
    pub last_fetched: u64,
}

fn cache_path(workspace_dir: &str) -> PathBuf {
    Path::new(workspace_dir).join(FILENAME)
}

pub fn load(workspace_dir: &str) -> FunctionsSnapshot {
    if workspace_dir.is_empty() {
        return FunctionsSnapshot::default();
    }
    let path = cache_path(workspace_dir);
    let content = std::fs::read_to_string(path).unwrap_or_default();
    serde_json::from_str(&content).unwrap_or_default()
}

pub fn save(workspace_dir: &str, snapshot: &FunctionsSnapshot) {
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

/// Convenience wrapper for callers that don't already have the workspace path.
pub fn load_for(sub: &str, rg: &str, app: &str) -> FunctionsSnapshot {
    load(&workspace_dir(sub, rg, app))
}

/// Convenience wrapper for callers that don't already have the workspace path.
pub fn save_for(sub: &str, rg: &str, app: &str, snapshot: &FunctionsSnapshot) {
    save(&workspace_dir(sub, rg, app), snapshot);
}
