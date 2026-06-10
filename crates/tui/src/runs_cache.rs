//! On-disk cache for recent run lists, keyed by (sub, app, workflow).
//!
//! Mirrors the approach `remote_chain` uses for chains — write JSON, read JSON.
//! No TTL: callers decide when to invalidate. `R` (hard reload) in the TUI
//! calls [`clear_app`] to drop every workflow's cache for a Logic App.
//!
//! Lives under `dirs::cache_dir() / ais-monitor / runs / <sub>_<app> /
//! <workflow>.json`.

use ais_monitor_core::azure::RunInfo;
use std::path::PathBuf;

fn base(sub: &str, app: &str) -> PathBuf {
    // `AIS_MONITOR_HOME` overrides the default cache root for environments
    // where the OS default isn't writable / roams unpredictably.
    let root = std::env::var_os("AIS_MONITOR_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::cache_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("ais-monitor")
        });
    let p = root.join("runs").join(format!("{sub}_{app}"));
    let _ = std::fs::create_dir_all(&p);
    p
}

pub fn load(sub: &str, app: &str, workflow: &str) -> Option<Vec<RunInfo>> {
    let path = base(sub, app).join(format!("{workflow}.json"));
    let content = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}

pub fn save(sub: &str, app: &str, workflow: &str, runs: &[RunInfo]) {
    let path = base(sub, app).join(format!("{workflow}.json"));
    if let Ok(json) = serde_json::to_string(runs) {
        let _ = std::fs::write(path, json);
    }
}

/// Drop every workflow's cache for a given Logic App. Called on hard reload.
pub fn clear_app(sub: &str, app: &str) {
    let _ = std::fs::remove_dir_all(base(sub, app));
}
