//! Messages flowing into the main loop. Anything that mutates `App` state
//! arrives as a `Msg` — keyboard input is translated to one, and background
//! tokio tasks push them via the shared `mpsc::UnboundedSender`.
//!
//! Keeping this enum exhaustive is the contract that makes Phase 3 screens
//! drop-in: each new fetch adds one `*Loaded` variant.

use ais_monitor_core::{
    azure::{
        ActionInfo, AzLoginState, AzSubscription, EventGridSubscription, EventGridTopic,
        LogicAppSite, RunInfo, WorkflowInfo,
    },
    chain::ChainDetail,
};

#[derive(Debug)]
#[allow(dead_code)] // variants land as Phase 3 / 4 screens come online
pub enum Msg {
    // ── input ──────────────────────────────────────────────────────────
    /// Cooked key event from crossterm — `app` decides what to do with it.
    Key(crossterm::event::KeyEvent),
    /// Terminal resize — ratatui handles redraw automatically; kept for
    /// future use (e.g. recomputing layout caches).
    Resize(u16, u16),
    /// Quit cleanly. Reached from `q`, `Esc`, or fatal errors that ask the
    /// user to confirm and exit.
    Quit,

    // ── async results ──────────────────────────────────────────────────
    LoginChecked(AzLoginState),
    SubsLoaded(Result<Vec<AzSubscription>, String>),
    AppsLoaded(Result<Vec<LogicAppSite>, String>),
    WorkflowsLoaded(Result<Vec<WorkflowInfo>, String>),
    RunsLoaded {
        workflow: String,
        result: Result<Vec<RunInfo>, String>,
    },
    ActionsLoaded {
        workflow: String,
        run_id: String,
        result: Result<Vec<ActionInfo>, String>,
    },
    ChainsLoaded(Result<Vec<ChainDetail>, String>),

    EgTopicsLoaded(Result<Vec<EventGridTopic>, String>),
    EgSubsLoaded {
        topic_id: String,
        result: Result<Vec<EventGridSubscription>, String>,
    },

    /// Periodic tick (watch mode). Fired by a background interval task; the
    /// receiver decides whether to act based on `App::watch`.
    Tick,
    /// Fast redraw tick (~10 Hz) — drives spinner animation without
    /// triggering any data fetch. No-op handler; presence in the channel is
    /// what wakes the main loop so `terminal.draw()` runs again.
    RenderTick,
}
