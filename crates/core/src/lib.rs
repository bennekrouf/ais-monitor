//! Shared, UI-agnostic services for AIS Monitor.
//!
//! Files are included from the existing `src/services/` tree via `#[path]`
//! so the Dioxus desktop app keeps working unchanged. We expose a `services`
//! module so the original `crate::services::azure::RunInfo` paths inside the
//! included files still resolve.

pub mod services;
pub use services::{azure, chain, kpi, msg_template, names, payload, remote_chain};
