//! Shared, UI-agnostic services for AIS Monitor.
//!
//! Both frontends — the Dioxus desktop app at the workspace root and the
//! TUI in `crates/tui` — depend on this crate, so there is exactly one
//! definition of every shared type.

pub mod services;
pub use services::{
    activity, azure, chain, kpi, msg_template, names, payload, remote_chain, store, text,
};
