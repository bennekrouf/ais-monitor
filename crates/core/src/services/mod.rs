//! Shared, UI-agnostic services: Azure CLI access, chain discovery, KPIs,
//! payload/message templating, and the on-disk store the caches sit on.
//!
//! These modules used to live in the desktop app's own `src/services/` and be
//! pulled in here with `#[path]`, which meant every one of them was compiled
//! twice — once into the binary crate, once into this one — producing two
//! unrelated sets of types with the same names. That doubled build time and
//! left a permanent trap: a `core::azure::RunInfo` and the binary's own
//! `services::azure::RunInfo` would never unify, and the compiler error when
//! they finally met would name the same path twice.
pub mod activity;
pub mod azure;
pub mod chain;
pub mod kpi;
pub mod msg_template;
pub mod names;
pub mod payload;
pub mod remote_chain;
pub mod store;
pub mod text;
