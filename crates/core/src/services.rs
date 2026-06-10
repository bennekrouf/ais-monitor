//! Re-host the existing service files in-place via `#[path]`. No files moved;
//! the desktop app keeps its `crate::services::*` paths working.

#[path = "../../../src/services/azure.rs"]
pub mod azure;
#[path = "../../../src/services/chain.rs"]
pub mod chain;
#[path = "../../../src/services/kpi.rs"]
pub mod kpi;
#[path = "../../../src/services/names.rs"]
pub mod names;
#[path = "../../../src/services/payload.rs"]
pub mod payload;
#[path = "../../../src/services/remote_chain.rs"]
pub mod remote_chain;
