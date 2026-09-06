//! The desktop app's services.
//!
//! Everything UI-agnostic lives in `ais-monitor-core` and is re-exported here
//! so the app's own `crate::services::azure::…` paths keep working. The
//! modules declared below are the ones that genuinely belong to this
//! frontend — either because they depend on its components (`chain_probe`,
//! `health_cache`) or because they only make sense for a windowed app
//! (`env`, `portal_links`).
pub use ais_monitor_core::services::{
    activity, azure, chain, kpi, msg_template, names, payload, remote_chain, store, text,
};

pub mod api_test;
pub mod chain_probe;
pub mod env;
pub mod functions_cache;
pub mod health_cache;
pub mod history_cache;
pub mod pipeline_scan;
pub mod portal_links;
pub mod resource_health_cache;
