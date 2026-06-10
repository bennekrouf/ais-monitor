//! Async, message-driven app loop.
//!
//! See Phase 2 notes for the architecture (Elm-ish select! over crossterm
//! events + tokio::mpsc<Msg>).
//!
//! Phase 3 adds:
//!   - `View::Picker` — sub → app selection when config is empty
//!   - `View::Browser` — chain list (left) + step detail (right)
//!   - `/` filter, `r` refresh, `R` clear-cache + refresh

use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind};
use futures::StreamExt;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, TableState},
};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use std::collections::HashMap;

use ais_monitor_core::{
    azure::{
        self, ActionInfo, AzLoginState, AzSubscription, EventGridSubscription, EventGridTopic,
        LogicAppSite, RunInfo,
    },
    chain::ChainDetail,
    kpi::{self, ChainKpi},
    names, remote_chain,
};
use ratatui::widgets::{Cell, Gauge, Row, Sparkline, Table};

use crate::{
    config::{CliArgs, Config},
    msg::Msg,
    runs_cache,
    tui::Tui,
};

/// Upper bound on cached action timelines. Each entry is small (5–50
/// ActionInfo × ~100 bytes) but unbounded user drills would grow forever.
/// 32 is generous — covers any realistic "what was I looking at?" workflow.
const ACTIONS_CAP: usize = 32;

// ── Slot ───────────────────────────────────────────────────────────────
/// Generic loading state for an async data slot.
#[derive(Debug, Default)]
pub enum Slot<T> {
    #[default]
    Idle,
    Loading,
    Loaded(T),
    Failed(String),
}

// ── Views ──────────────────────────────────────────────────────────────

#[derive(Debug)]
enum View {
    /// Pick subscription → app. Only entered when config doesn't have both.
    Picker(PickerStep),
    /// Browse chains for the configured (sub, rg, app, dir).
    Browser,
    /// Event Grid topics + subscriptions for the configured resource group.
    EventGrid,
}

/// A modal overlay that captures all keystrokes until dismissed.
#[derive(Debug)]
enum Modal {
    /// Renaming the chain whose original label is `key`.
    Rename { key: String, input: String },
    /// Quick help overlay (read-only).
    Help,
    /// Azure login flow. Two phases: prompt (waiting for user to press L)
    /// and `in_progress` (browser opened, polling `az` for completion).
    Login { in_progress: bool },
}

#[derive(Debug)]
enum PickerStep {
    Subs,
    Apps,
}

/// Where keystrokes go inside the browser. Tab cycles forward, Shift-Tab back.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Focus {
    Chains,
    Steps,
    Runs,
    Actions, // only reachable after drilling into a run
}

// ── App ────────────────────────────────────────────────────────────────

pub struct App {
    pub config: Config,
    pub login: Slot<AzLoginState>,

    // picker state
    subs: Slot<Vec<AzSubscription>>,
    sub_cursor: ListState,
    apps: Slot<Vec<LogicAppSite>>,
    app_cursor: ListState,

    // browser state
    chains: Slot<Vec<ChainDetail>>,
    chain_cursor: ListState,
    step_cursor: ListState,
    run_cursor: TableState,
    action_cursor: ListState,
    /// Runs cached per workflow name. Lets us cycle steps without re-fetching.
    runs: HashMap<String, Slot<Vec<RunInfo>>>,
    /// Actions cached per (workflow, run_id). User-driven drill-ins, so this
    /// grows monotonically until capped. See `ACTIONS_CAP`.
    actions: HashMap<(String, String), Slot<Vec<ActionInfo>>>,
    /// Insertion order for `actions` so we can evict the oldest entry when
    /// `ACTIONS_CAP` is exceeded. Cheap O(n) eviction since N is tiny.
    actions_order: std::collections::VecDeque<(String, String)>,
    /// Workflows / runs currently being fetched. Prevents the watch-mode tick
    /// from piling up duplicate `spawn_blocking` tasks when Azure responses
    /// take longer than the tick interval — the real-world unbounded-growth
    /// case on slow networks / rate-limited tenants.
    inflight_runs: std::collections::HashSet<String>,
    inflight_actions: std::collections::HashSet<(String, String)>,
    /// Run the user drilled into. When `Some`, the right pane becomes the
    /// action timeline. `Esc`/`Backspace` clears.
    drilled: Option<(String, String)>,
    focus: Focus,
    filter: String,
    filtering: bool, // `/` mode

    /// User-defined display names for chains. Stored per (sub, app) under
    /// the user's config dir; loaded on app launch and re-loaded after
    /// picking an app; saved on every rename.
    chain_names: HashMap<String, String>,
    modal: Option<Modal>,
    /// Has the user explicitly dismissed the login modal this session? If
    /// so, we won't re-pop it on every periodic re-check.
    login_modal_dismissed: bool,
    /// `--device-code` sign-in mode — set from CLI. When the login flow
    /// fires, it suspends the TUI and runs `az login --use-device-code` in
    /// the user's regular terminal instead of trying to open a browser.
    device_code_login: bool,
    /// Set by `start_login_flow` when device-code mode is on. The run loop
    /// reads this between frames, leaves the alt-screen, runs `az` inline,
    /// then re-enters. Single bool because there's only one such command
    /// today; promote to an enum if more arrive.
    pending_device_code_login: bool,

    // Watch mode — when on, periodic ticks re-fetch runs for the focused step.
    watch: bool,
    watch_interval_secs: u64,

    // Auto-follow — when a chain has a running workflow, move the step
    // cursor to it so the user sees what's executing without tabbing.
    // Off-limits for `grace_secs` after the user has moved the cursor
    // manually, so investigation isn't interrupted.
    follow_running: bool,
    last_manual_step_move: Option<std::time::Instant>,
    follow_grace_secs: u64,

    // Event Grid panel state.
    eg_topics: Slot<Vec<EventGridTopic>>,
    eg_topic_cursor: ListState,
    eg_subs: HashMap<String, Slot<Vec<EventGridSubscription>>>,
    eg_sub_cursor: ListState,
    eg_focus_subs: bool,

    view: View,
    status: String,
    should_quit: bool,
    tx: UnboundedSender<Msg>,
}

impl App {
    pub fn new(tx: UnboundedSender<Msg>, cli: CliArgs) -> Self {
        let mut config = Config::load();
        // Track whether the caller pinned the app on the command line — that
        // bypasses the picker entirely (scripting / tmux pane use case).
        let cli_pinned_app = cli.subscription.is_some()
            && cli.resource_group.is_some()
            && cli.logic_app.is_some();
        if cli.subscription.is_some() {
            config.subscription = cli.subscription;
        }
        if cli.resource_group.is_some() {
            config.resource_group = cli.resource_group;
        }
        if cli.logic_app.is_some() {
            config.logic_app = cli.logic_app;
        }
        if let Some(s) = cli.watch_interval_secs {
            config.watch_interval_secs = s.max(1);
        }
        config.save();
        // Always show the picker at launch so the user confirms which Logic
        // App they're about to monitor — the cached chain count next to each
        // entry doubles as a "pick the one with the data you remember" hint.
        // Only `--sub --rg --app` skips the picker (scripting path).
        let view = if cli_pinned_app {
            View::Browser
        } else {
            View::Picker(PickerStep::Subs)
        };

        let mut sub_cursor = ListState::default();
        sub_cursor.select(Some(0));
        let mut app_cursor = ListState::default();
        app_cursor.select(Some(0));
        let mut chain_cursor = ListState::default();
        chain_cursor.select(Some(0));
        let mut step_cursor = ListState::default();
        step_cursor.select(Some(0));
        let mut run_cursor = TableState::default();
        run_cursor.select(Some(0));
        let mut action_cursor = ListState::default();
        action_cursor.select(Some(0));
        let mut eg_topic_cursor = ListState::default();
        eg_topic_cursor.select(Some(0));
        let mut eg_sub_cursor = ListState::default();
        eg_sub_cursor.select(Some(0));

        // Chain names are stored per (sub, app) under the user's config dir.
        // No working-tree dependency — `ais-monitor-tui` is an online-only
        // monitor.
        let chain_names = match names_dir(&config) {
            Some(dir) => names::load(&dir),
            None => HashMap::new(),
        };
        let watch_interval_secs = config.watch_interval_secs;

        Self {
            config,
            login: Slot::Idle,
            subs: Slot::Idle,
            sub_cursor,
            apps: Slot::Idle,
            app_cursor,
            chains: Slot::Idle,
            chain_cursor,
            step_cursor,
            run_cursor,
            action_cursor,
            runs: HashMap::new(),
            actions: HashMap::new(),
            actions_order: std::collections::VecDeque::new(),
            inflight_runs: std::collections::HashSet::new(),
            inflight_actions: std::collections::HashSet::new(),
            drilled: None,
            focus: Focus::Chains,
            filter: String::new(),
            filtering: false,
            chain_names,
            modal: None,
            login_modal_dismissed: false,
            device_code_login: cli.device_code,
            pending_device_code_login: false,
            // Watch mode is on by default — that's the whole point of a
            // live monitor. The `w` key toggles it off when you want to
            // freeze the view (e.g. while reading a stack trace).
            watch: true,
            follow_running: true,
            last_manual_step_move: None,
            follow_grace_secs: 10,
            watch_interval_secs,
            eg_topics: Slot::Idle,
            eg_topic_cursor,
            eg_subs: HashMap::new(),
            eg_sub_cursor,
            eg_focus_subs: false,
            view,
            status: String::from("starting…"),
            should_quit: false,
            tx,
        }
    }

    pub async fn run(&mut self, terminal: &mut Tui, mut rx: UnboundedReceiver<Msg>) -> Result<()> {
        self.spawn_login_check();
        match self.view {
            View::Picker(_) => self.spawn_subs(),
            View::Browser => self.spawn_chains(),
            View::EventGrid => {}
        }
        // Tick task — always running, even when watch mode is off. Cheap enough
        // (one message per interval) and lets us toggle watch with no spawn
        // dance.
        spawn_tick_task(self.tx.clone(), self.watch_interval_secs);
        // Fast render tick for spinners. 100 ms gives smooth animation
        // without burning CPU — ratatui is happy at 60 fps, we're at 10.
        spawn_render_tick(self.tx.clone());

        let mut events = EventStream::new();
        while !self.should_quit {
            // External-command escape hatch: must run *between* frames so we
            // can take the terminal back from ratatui without races.
            if self.pending_device_code_login {
                self.run_device_code_login(terminal)?;
            }
            terminal.draw(|f| self.draw(f))?;
            tokio::select! {
                Some(Ok(ev)) = events.next() => {
                    if let Some(msg) = translate_event(ev) {
                        self.handle(msg);
                    }
                }
                Some(msg) = rx.recv() => {
                    self.handle(msg);
                }
            }
        }
        Ok(())
    }

    // ── message handling ────────────────────────────────────────────────

    fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Quit => self.should_quit = true,
            Msg::Resize(_, _) => {}
            Msg::Key(k) => self.handle_key(k),

            Msg::LoginChecked(state) => {
                self.login = Slot::Loaded(state.clone());
                self.react_to_login(&state);
            }

            Msg::SubsLoaded(Ok(s)) => {
                self.status = format!("{} subscription(s)", s.len());
                // Move the cursor onto the previously-used subscription so
                // Enter confirms in one keystroke for returning users.
                if let Some(saved) = self.config.subscription.as_deref() {
                    if let Some(idx) = s.iter().position(|x| x.id == saved) {
                        self.sub_cursor.select(Some(idx));
                    }
                }
                self.subs = Slot::Loaded(s);
            }
            Msg::SubsLoaded(Err(e)) => {
                self.status = format!("subs: {e}");
                self.subs = Slot::Failed(e);
            }

            Msg::AppsLoaded(Ok(a)) => {
                self.status = format!("{} Logic App(s)", a.len());
                if let Some(saved) = self.config.logic_app.as_deref() {
                    if let Some(idx) = a.iter().position(|x| x.name == saved) {
                        self.app_cursor.select(Some(idx));
                    }
                }
                self.apps = Slot::Loaded(a);
            }
            Msg::AppsLoaded(Err(e)) => {
                self.status = format!("apps: {e}");
                self.apps = Slot::Failed(e);
            }

            Msg::ChainsLoaded(Ok(c)) => {
                self.status = format!("{} chain(s)", c.len());
                self.chains = Slot::Loaded(c);
                self.ensure_runs_for_focused_step();
            }
            Msg::ChainsLoaded(Err(e)) => {
                self.status = format!("chains: {e}");
                self.chains = Slot::Failed(e);
            }

            Msg::RunsLoaded { workflow, result } => {
                self.inflight_runs.remove(&workflow);
                match &result {
                    Ok(rs) => {
                        self.status = format!("{} run(s) for {workflow}", rs.len());
                        // Write-through cache. Only on success — we don't want
                        // to overwrite a good cache with a transient Azure
                        // error.
                        if let (Some(sub), Some(app)) =
                            (&self.config.subscription, &self.config.logic_app)
                        {
                            runs_cache::save(sub, app, &workflow, rs);
                        }
                    }
                    Err(e) => self.status = format!("runs {workflow}: {e}"),
                }
                // Don't overwrite cached data with a Failed slot — keep showing
                // the cache and surface the error in the status line.
                let prev_was_cached = matches!(self.runs.get(&workflow), Some(Slot::Loaded(_)));
                match (&result, prev_was_cached) {
                    (Err(_), true) => { /* keep the cached Slot::Loaded */ }
                    _ => {
                        self.runs.insert(workflow, slot_from(result));
                        self.run_cursor.select(Some(0));
                    }
                }
                // Fresh data may have changed who's running — re-evaluate
                // whether the step cursor should jump.
                self.maybe_follow_running();
            }

            Msg::ActionsLoaded { workflow, run_id, result } => {
                let key = (workflow.clone(), run_id.clone());
                self.inflight_actions.remove(&key);
                match &result {
                    Ok(a) => self.status = format!("{} action(s)", a.len()),
                    Err(e) => self.status = format!("actions: {e}"),
                }
                self.insert_actions(key, slot_from(result));
                self.action_cursor.select(Some(0));
            }

            Msg::WorkflowsLoaded(_) => {}

            Msg::EgTopicsLoaded(r) => {
                match &r {
                    Ok(t) => self.status = format!("{} EG topic(s)", t.len()),
                    Err(e) => self.status = format!("eg topics: {e}"),
                }
                self.eg_topics = slot_from(r);
                self.eg_topic_cursor.select(Some(0));
                self.ensure_eg_subs_for_focused_topic();
            }
            Msg::EgSubsLoaded { topic_id, result } => {
                self.eg_subs.insert(topic_id, slot_from(result));
                self.eg_sub_cursor.select(Some(0));
            }

            Msg::RenderTick => {
                // No-op — the loop redraws after every handle() call, so the
                // mere arrival of this message animates the spinner.
            }

            Msg::Tick => {
                // Live refresh — pulls fresh runs for *every* step of the
                // currently-focused chain, not just the focused step. So when
                // the user Tabs around they see up-to-date data immediately,
                // and a new run appearing anywhere in the pipeline shows up
                // by the next tick.
                //
                // Cost: one az call per step per interval. For a 15-step
                // chain at the 5s default that's ~3 calls/s — well below
                // ARM throttling (~100 req/s).
                if !self.watch || !matches!(self.view, View::Browser) {
                    return;
                }
                let Some(chain) = self.current_chain() else { return };
                let (Some(sub), Some(rg), Some(app)) = (
                    self.config.subscription.clone(),
                    self.config.resource_group.clone(),
                    self.config.logic_app.clone(),
                ) else {
                    return;
                };
                for step in &chain.steps {
                    let tx = self.tx.clone();
                    let (sub, rg, app, wf) =
                        (sub.clone(), rg.clone(), app.clone(), step.workflow.clone());
                    tokio::task::spawn_blocking(move || {
                        let r = azure::list_runs(&sub, &rg, &app, &wf, 20);
                        let _ = tx.send(Msg::RunsLoaded { workflow: wf, result: r });
                    });
                }
            }
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }
        // Modal first — swallows everything until dismissed.
        if self.modal.is_some() {
            self.handle_modal_key(key);
            return;
        }
        // Filter-input mode swallows most keys.
        if self.filtering {
            match key.code {
                KeyCode::Esc => {
                    self.filtering = false;
                    self.filter.clear();
                }
                KeyCode::Enter => self.filtering = false,
                KeyCode::Backspace => {
                    self.filter.pop();
                }
                KeyCode::Char(c) => self.filter.push(c),
                _ => {}
            }
            return;
        }

        // Global keys
        match key.code {
            KeyCode::Char('q') => {
                self.should_quit = true;
                return;
            }
            KeyCode::Esc => {
                self.should_quit = true;
                return;
            }
            KeyCode::Char('?') => {
                self.modal = Some(Modal::Help);
                return;
            }
            _ => {}
        }

        match &self.view {
            View::Picker(step) => self.handle_picker_key(step_clone(step), key),
            View::Browser => self.handle_browser_key(key),
            View::EventGrid => self.handle_eg_key(key),
        }
    }

    fn handle_modal_key(&mut self, key: KeyEvent) {
        let Some(modal) = self.modal.as_mut() else { return };
        match modal {
            Modal::Help => match key.code {
                KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') | KeyCode::Enter => {
                    self.modal = None;
                }
                _ => {}
            },
            Modal::Rename { input, .. } => match key.code {
                KeyCode::Esc => self.modal = None,
                KeyCode::Enter => {
                    let (key_label, new_name) = match self.modal.take().unwrap() {
                        Modal::Rename { key, input } => (key, input),
                        _ => unreachable!(),
                    };
                    self.apply_rename(key_label, new_name);
                }
                KeyCode::Backspace => {
                    input.pop();
                }
                KeyCode::Char(c) => input.push(c),
                _ => {}
            },
            Modal::Login { in_progress } => match key.code {
                KeyCode::Esc => {
                    // Dismiss; remember so we don't re-pop on the next tick.
                    self.modal = None;
                    self.login_modal_dismissed = true;
                }
                KeyCode::Char('q') => self.should_quit = true,
                KeyCode::Char('l') | KeyCode::Char('L') | KeyCode::Enter if !*in_progress => {
                    self.start_login_flow();
                }
                _ => {}
            },
        }
    }

    fn apply_rename(&mut self, key: String, new_name: String) {
        let new_name = new_name.trim().to_string();
        if new_name.is_empty() {
            self.chain_names.remove(&key);
        } else {
            self.chain_names.insert(key.clone(), new_name.clone());
        }
        if let Some(dir) = names_dir(&self.config) {
            names::save(&dir, &self.chain_names);
        }
        self.status = if new_name.is_empty() {
            format!("cleared name for {key}")
        } else {
            format!("renamed: {key} → {new_name}")
        };
    }

    fn handle_picker_key(&mut self, step: PickerStep, key: KeyEvent) {
        match step {
            PickerStep::Subs => match key.code {
                KeyCode::Down | KeyCode::Char('j') => move_cursor(&mut self.sub_cursor, &self.subs, 1),
                KeyCode::Up | KeyCode::Char('k') => move_cursor(&mut self.sub_cursor, &self.subs, -1),
                KeyCode::Enter => self.pick_sub(),
                _ => {}
            },
            PickerStep::Apps => match key.code {
                KeyCode::Down | KeyCode::Char('j') => move_cursor(&mut self.app_cursor, &self.apps, 1),
                KeyCode::Up | KeyCode::Char('k') => move_cursor(&mut self.app_cursor, &self.apps, -1),
                KeyCode::Enter => self.pick_app(),
                KeyCode::Backspace => {
                    self.view = View::Picker(PickerStep::Subs);
                }
                _ => {}
            },
        }
    }

    fn handle_browser_key(&mut self, key: KeyEvent) {
        // Global browser keys (work regardless of focus).
        match key.code {
            KeyCode::Char('/') => {
                self.filtering = true;
                self.filter.clear();
                return;
            }
            KeyCode::Char('r') => {
                self.refresh_focused();
                return;
            }
            KeyCode::Char('R') => {
                if let (Some(sub), Some(app)) = (&self.config.subscription, &self.config.logic_app) {
                    remote_chain::clear_cache(sub, app);
                    runs_cache::clear_app(sub, app);
                }
                self.runs.clear();
                self.actions.clear();
                self.actions_order.clear();
                self.inflight_runs.clear();
                self.inflight_actions.clear();
                self.drilled = None;
                self.spawn_chains();
                return;
            }
            KeyCode::Char('c') => {
                self.view = View::Picker(PickerStep::Subs);
                self.spawn_subs();
                return;
            }
            KeyCode::Tab => {
                self.cycle_focus(1);
                return;
            }
            KeyCode::BackTab => {
                self.cycle_focus(-1);
                return;
            }
            KeyCode::Char('m') => {
                // Rename the focused chain.
                if let Some(c) = self.current_chain() {
                    let key = c.label.clone();
                    let input = self.chain_names.get(&key).cloned().unwrap_or_default();
                    self.modal = Some(Modal::Rename { key, input });
                }
                return;
            }
            KeyCode::Char('w') => {
                self.watch = !self.watch;
                self.status = if self.watch {
                    format!("live: on ({}s refresh)", self.watch_interval_secs)
                } else {
                    "live: paused (press w to resume)".into()
                };
                return;
            }
            KeyCode::Char('f') => {
                self.follow_running = !self.follow_running;
                self.status = if self.follow_running {
                    self.last_manual_step_move = None; // re-arm immediately
                    "follow: on — cursor tracks running workflow".into()
                } else {
                    "follow: off".into()
                };
                return;
            }
            KeyCode::Char('g') => {
                self.view = View::EventGrid;
                if matches!(self.eg_topics, Slot::Idle) {
                    self.spawn_eg_topics();
                }
                return;
            }
            _ => {}
        }

        // Focus-specific keys.
        match self.focus {
            Focus::Chains => self.handle_chains_key(key),
            Focus::Steps => self.handle_steps_key(key),
            Focus::Runs => self.handle_runs_key(key),
            Focus::Actions => self.handle_actions_key(key),
        }
    }

    fn handle_chains_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => {
                let n = self.filtered_chains().len();
                move_cursor_n(&mut self.chain_cursor, n, 1);
                self.on_chain_changed();
            }
            KeyCode::Up | KeyCode::Char('k') => {
                let n = self.filtered_chains().len();
                move_cursor_n(&mut self.chain_cursor, n, -1);
                self.on_chain_changed();
            }
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                self.focus = Focus::Steps;
            }
            _ => {}
        }
    }

    fn handle_steps_key(&mut self, key: KeyEvent) {
        let step_count = self.current_chain().map(|c| c.steps.len()).unwrap_or(0);
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => {
                move_cursor_n(&mut self.step_cursor, step_count, 1);
                // User just took manual control — pause auto-follow.
                self.last_manual_step_move = Some(std::time::Instant::now());
                self.ensure_runs_for_focused_step();
            }
            KeyCode::Up | KeyCode::Char('k') => {
                move_cursor_n(&mut self.step_cursor, step_count, -1);
                self.last_manual_step_move = Some(std::time::Instant::now());
                self.ensure_runs_for_focused_step();
            }
            KeyCode::Left | KeyCode::Char('h') => self.focus = Focus::Chains,
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => self.focus = Focus::Runs,
            _ => {}
        }
    }

    fn handle_runs_key(&mut self, key: KeyEvent) {
        let n = self.current_runs().map(|s| s.len()).unwrap_or(0);
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => move_cursor_n(&mut self.run_cursor, n, 1),
            KeyCode::Up | KeyCode::Char('k') => move_cursor_n(&mut self.run_cursor, n, -1),
            KeyCode::Left | KeyCode::Char('h') => self.focus = Focus::Steps,
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => self.drill_into_run(),
            _ => {}
        }
    }

    fn handle_actions_key(&mut self, key: KeyEvent) {
        let n = self.current_actions().map(|s| s.len()).unwrap_or(0);
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => move_cursor_n(&mut self.action_cursor, n, 1),
            KeyCode::Up | KeyCode::Char('k') => move_cursor_n(&mut self.action_cursor, n, -1),
            KeyCode::Left | KeyCode::Char('h') | KeyCode::Backspace => {
                self.drilled = None;
                self.focus = Focus::Runs;
            }
            _ => {}
        }
    }

    fn cycle_focus(&mut self, delta: i32) {
        // Actions is only reachable when drilled — skip it otherwise.
        let order: &[Focus] = if self.drilled.is_some() {
            &[Focus::Chains, Focus::Steps, Focus::Runs, Focus::Actions]
        } else {
            &[Focus::Chains, Focus::Steps, Focus::Runs]
        };
        let i = order.iter().position(|f| *f == self.focus).unwrap_or(0) as i32;
        let n = order.len() as i32;
        let next = (i + delta).rem_euclid(n) as usize;
        self.focus = order[next];
    }

    fn on_chain_changed(&mut self) {
        self.step_cursor.select(Some(0));
        self.run_cursor.select(Some(0));
        self.drilled = None;
        // Fresh chain — clear the auto-follow grace window so we land on a
        // running workflow immediately (or stay on step 1 if all idle).
        self.last_manual_step_move = None;
        self.ensure_runs_for_focused_step();
    }

    /// If a workflow inside the focused chain is running, move the step
    /// cursor to it so the user sees what's executing. Honors a short grace
    /// after manual cursor moves and never fires during action drill-in.
    fn maybe_follow_running(&mut self) {
        if !self.follow_running || !matches!(self.view, View::Browser) {
            return;
        }
        if self.drilled.is_some() {
            return;
        }
        if let Some(t) = self.last_manual_step_move {
            if t.elapsed() < std::time::Duration::from_secs(self.follow_grace_secs) {
                return;
            }
        }
        let Some(chain) = self.current_chain() else { return };
        // Earliest-in-chain-order — most natural reading direction. If two
        // steps are running, we land on the upstream one.
        let target = chain
            .steps
            .iter()
            .position(|s| running_count(&self.runs, &s.workflow) > 0);
        let Some(idx) = target else { return };
        if self.step_cursor.selected() != Some(idx) {
            self.step_cursor.select(Some(idx));
            self.ensure_runs_for_focused_step();
        }
    }

    fn refresh_focused(&mut self) {
        match self.focus {
            Focus::Chains | Focus::Steps => self.spawn_chains(),
            Focus::Runs => {
                if let Some(wf) = self.focused_workflow() {
                    self.runs.remove(&wf);
                    self.spawn_runs(wf);
                }
            }
            Focus::Actions => {
                if let Some((wf, run)) = self.drilled.clone() {
                    self.actions.remove(&(wf.clone(), run.clone()));
                    self.spawn_actions(wf, run);
                }
            }
        }
    }

    fn drill_into_run(&mut self) {
        let Some(wf) = self.focused_workflow() else { return };
        let runs = self.runs.get(&wf);
        let Some(Slot::Loaded(rs)) = runs else { return };
        let Some(idx) = self.run_cursor.selected() else { return };
        let Some(run) = rs.get(idx) else { return };
        let run_id = run.id.clone();
        self.drilled = Some((wf.clone(), run_id.clone()));
        self.focus = Focus::Actions;
        self.action_cursor.select(Some(0));
        if !self.actions.contains_key(&(wf.clone(), run_id.clone())) {
            self.spawn_actions(wf, run_id);
        }
    }

    /// Workflow name for the currently focused step, if any.
    fn focused_workflow(&self) -> Option<String> {
        let chain = self.current_chain()?;
        let i = self.step_cursor.selected().unwrap_or(0);
        chain.steps.get(i).map(|s| s.workflow.clone())
    }

    fn current_chain(&self) -> Option<ChainDetail> {
        let visible = self.filtered_chains();
        let i = self.chain_cursor.selected()?;
        visible.get(i).map(|c| (*c).clone())
    }

    fn current_runs(&self) -> Option<&[RunInfo]> {
        let wf = self.focused_workflow()?;
        match self.runs.get(&wf)? {
            Slot::Loaded(v) => Some(v.as_slice()),
            _ => None,
        }
    }

    fn current_actions(&self) -> Option<&[ActionInfo]> {
        let key = self.drilled.clone()?;
        match self.actions.get(&key)? {
            Slot::Loaded(v) => Some(v.as_slice()),
            _ => None,
        }
    }

    fn ensure_runs_for_focused_step(&mut self) {
        let Some(wf) = self.focused_workflow() else { return };
        if !self.runs.contains_key(&wf) {
            self.spawn_runs(wf);
        }
    }

    fn pick_sub(&mut self) {
        let Slot::Loaded(subs) = &self.subs else { return };
        let Some(i) = self.sub_cursor.selected() else { return };
        let Some(sub) = subs.get(i) else { return };
        self.config.subscription = Some(sub.id.clone());
        self.status = format!("subscription: {}", sub.name);
        self.view = View::Picker(PickerStep::Apps);
        self.spawn_apps();
    }

    fn pick_app(&mut self) {
        let Slot::Loaded(apps) = &self.apps else { return };
        let Some(i) = self.app_cursor.selected() else { return };
        let Some(site) = apps.get(i) else { return };
        self.config.resource_group = Some(site.resource_group.clone());
        self.config.logic_app = Some(site.name.clone());
        self.config.save();
        // Reload chain names now that sub+app are settled.
        if let Some(dir) = names_dir(&self.config) {
            self.chain_names = names::load(&dir);
        }
        self.status = format!("logic app: {}", site.name);
        self.view = View::Browser;
        self.spawn_chains();
    }

    // ── task spawners ───────────────────────────────────────────────────

    /// Adjust the modal in response to a fresh login state. Opens the login
    /// prompt automatically when the token is expired / missing (unless the
    /// user already dismissed it). Closes the modal once login succeeds.
    fn react_to_login(&mut self, state: &AzLoginState) {
        match state {
            AzLoginState::LoggedIn { .. } => {
                if matches!(self.modal, Some(Modal::Login { .. })) {
                    self.modal = None;
                    self.status = "signed in — welcome back".into();
                }
            }
            AzLoginState::Expired | AzLoginState::NotLoggedIn => {
                if !self.login_modal_dismissed
                    && !matches!(self.modal, Some(Modal::Login { .. }))
                {
                    self.modal = Some(Modal::Login { in_progress: false });
                }
            }
            AzLoginState::AzNotFound | AzLoginState::Checking => {}
        }
    }

    /// Kick off the sign-in flow. Two modes:
    ///   - browser flow (default): spawns `az login`, polls in the background
    ///   - device-code flow (`--device-code`): defers to the run loop, which
    ///     suspends the TUI and runs `az login --use-device-code` interactively
    fn start_login_flow(&mut self) {
        if let Some(Modal::Login { in_progress }) = self.modal.as_mut() {
            *in_progress = true;
        }
        if self.device_code_login {
            // The run loop performs the actual work — we can't leave the
            // alt-screen from inside a tokio task that doesn't own the
            // terminal handle.
            self.pending_device_code_login = true;
            self.status = "preparing device-code sign-in…".into();
            return;
        }

        self.status = "signing in… (browser will open)".into();
        let tx = self.tx.clone();

        // Stage 1: logout (kills the expired refresh token) + spawn `az login`.
        tokio::task::spawn_blocking(move || {
            use std::process::Command;
            let _ = Command::new("az").args(["logout"]).output();
            if let Err(e) = azure::open_login(None) {
                let _ = tx.send(Msg::LoginChecked(AzLoginState::NotLoggedIn));
                eprintln!("az login spawn error: {e}");
                return;
            }
        });

        // Stage 2: poll `check_login` until success or timeout.
        let tx = self.tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            for _ in 0..40 {
                let result = tokio::task::spawn_blocking(azure::check_login).await;
                if let Ok(state) = result {
                    let done = matches!(state, AzLoginState::LoggedIn { .. });
                    let _ = tx.send(Msg::LoginChecked(state));
                    if done {
                        return;
                    }
                }
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
            }
        });
    }

    /// Run `az login --use-device-code` *inline* in the user's normal
    /// terminal so they can read the URL + 8-character code that `az` prints
    /// (which would otherwise be invisible behind our alt-screen). Called by
    /// the run loop after observing `pending_device_code_login`.
    fn run_device_code_login(&mut self, terminal: &mut Tui) -> Result<()> {
        self.pending_device_code_login = false;
        // Leave the alt-screen + raw mode so `az` writes to the real tty.
        crate::tui::restore()?;
        use std::process::Command;
        let _ = Command::new("az").args(["logout"]).status();
        eprintln!();
        eprintln!("─── ais-monitor-tui · device-code sign-in ───────────────────────");
        eprintln!("Follow the instructions below; this terminal resumes after sign-in.");
        eprintln!();
        let status = Command::new("az")
            .args(["login", "--use-device-code"])
            .status();
        eprintln!();
        match status {
            Ok(s) if s.success() => eprintln!("Sign-in complete. Resuming ais-monitor-tui…"),
            _ => eprintln!("Sign-in did not complete. Resuming anyway."),
        }
        // Brief pause so the user sees the final line before alt-screen
        // takes over.
        std::thread::sleep(std::time::Duration::from_millis(500));
        // Re-enter alt-screen + raw mode and replace the terminal handle.
        *terminal = crate::tui::init()?;
        // Refresh login state — the next check tells us if we're in.
        self.spawn_login_check();
        Ok(())
    }

    fn spawn_login_check(&mut self) {
        self.login = Slot::Loading;
        let tx = self.tx.clone();
        tokio::task::spawn_blocking(move || {
            let _ = tx.send(Msg::LoginChecked(azure::check_login()));
        });
    }

    fn spawn_subs(&mut self) {
        self.subs = Slot::Loading;
        let tx = self.tx.clone();
        tokio::task::spawn_blocking(move || {
            let _ = tx.send(Msg::SubsLoaded(azure::list_subscriptions()));
        });
    }

    fn spawn_apps(&mut self) {
        let Some(sub) = self.config.subscription.clone() else { return };
        self.apps = Slot::Loading;
        let tx = self.tx.clone();
        tokio::task::spawn_blocking(move || {
            let _ = tx.send(Msg::AppsLoaded(azure::list_logic_app_sites(&sub)));
        });
    }

    fn spawn_eg_topics(&mut self) {
        let Some(rg) = self.config.resource_group.clone() else { return };
        self.eg_topics = Slot::Loading;
        let tx = self.tx.clone();
        tokio::task::spawn_blocking(move || {
            let _ = tx.send(Msg::EgTopicsLoaded(azure::list_eventgrid_topics(&rg)));
        });
    }

    fn spawn_eg_subs(&mut self, topic_id: String) {
        self.eg_subs.insert(topic_id.clone(), Slot::Loading);
        let tx = self.tx.clone();
        let tid = topic_id.clone();
        tokio::task::spawn_blocking(move || {
            let r = azure::list_eventgrid_subscriptions(&tid);
            let _ = tx.send(Msg::EgSubsLoaded { topic_id: tid, result: r });
        });
    }

    fn ensure_eg_subs_for_focused_topic(&mut self) {
        let Slot::Loaded(topics) = &self.eg_topics else { return };
        let Some(i) = self.eg_topic_cursor.selected() else { return };
        let Some(t) = topics.get(i) else { return };
        let id = t.id.clone();
        if !self.eg_subs.contains_key(&id) {
            self.spawn_eg_subs(id);
        }
    }

    fn handle_eg_key(&mut self, key: KeyEvent) {
        // Globals first.
        match key.code {
            KeyCode::Char('q') => {
                self.should_quit = true;
                return;
            }
            KeyCode::Esc | KeyCode::Char('g') => {
                self.view = View::Browser;
                return;
            }
            KeyCode::Char('r') => {
                self.eg_topics = Slot::Idle;
                self.eg_subs.clear();
                self.spawn_eg_topics();
                return;
            }
            KeyCode::Tab => {
                self.eg_focus_subs = !self.eg_focus_subs;
                return;
            }
            _ => {}
        }
        if self.eg_focus_subs {
            let n = self
                .current_eg_subs()
                .map(|s| s.len())
                .unwrap_or(0);
            match key.code {
                KeyCode::Down | KeyCode::Char('j') => move_cursor_n(&mut self.eg_sub_cursor, n, 1),
                KeyCode::Up | KeyCode::Char('k') => move_cursor_n(&mut self.eg_sub_cursor, n, -1),
                KeyCode::Left | KeyCode::Char('h') => self.eg_focus_subs = false,
                _ => {}
            }
        } else {
            let n = match &self.eg_topics {
                Slot::Loaded(v) => v.len(),
                _ => 0,
            };
            match key.code {
                KeyCode::Down | KeyCode::Char('j') => {
                    move_cursor_n(&mut self.eg_topic_cursor, n, 1);
                    self.ensure_eg_subs_for_focused_topic();
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    move_cursor_n(&mut self.eg_topic_cursor, n, -1);
                    self.ensure_eg_subs_for_focused_topic();
                }
                KeyCode::Right | KeyCode::Char('l') | KeyCode::Enter => {
                    self.eg_focus_subs = true
                }
                _ => {}
            }
        }
    }

    fn current_eg_subs(&self) -> Option<&[EventGridSubscription]> {
        let Slot::Loaded(topics) = &self.eg_topics else { return None };
        let i = self.eg_topic_cursor.selected()?;
        let topic = topics.get(i)?;
        match self.eg_subs.get(&topic.id)? {
            Slot::Loaded(v) => Some(v.as_slice()),
            _ => None,
        }
    }

    fn spawn_runs(&mut self, workflow: String) {
        // Dedup — if a fetch for this workflow is already in flight, skip.
        // Prevents the watch-mode tick from queueing duplicate tasks when
        // Azure responds slower than the tick interval.
        if self.inflight_runs.contains(&workflow) {
            return;
        }
        let (Some(sub), Some(rg), Some(app)) = (
            self.config.subscription.clone(),
            self.config.resource_group.clone(),
            self.config.logic_app.clone(),
        ) else {
            return;
        };
        // Read-through cache: show last-known data immediately while the
        // fresh fetch happens in the background. Makes relaunch feel instant.
        if let Some(cached) = runs_cache::load(&sub, &app, &workflow) {
            self.runs.insert(workflow.clone(), Slot::Loaded(cached));
        } else if !self.runs.contains_key(&workflow) {
            // Only flash Loading on cold start — silent refresh otherwise so
            // watch mode doesn't strobe the panel.
            self.runs.insert(workflow.clone(), Slot::Loading);
        }
        self.inflight_runs.insert(workflow.clone());
        let tx = self.tx.clone();
        let wf = workflow.clone();
        tokio::task::spawn_blocking(move || {
            let r = azure::list_runs(&sub, &rg, &app, &wf, 20);
            let _ = tx.send(Msg::RunsLoaded { workflow: wf, result: r });
        });
    }

    fn spawn_actions(&mut self, workflow: String, run_id: String) {
        let key = (workflow.clone(), run_id.clone());
        if self.inflight_actions.contains(&key) {
            return;
        }
        let (Some(sub), Some(rg), Some(app)) = (
            self.config.subscription.clone(),
            self.config.resource_group.clone(),
            self.config.logic_app.clone(),
        ) else {
            return;
        };
        self.insert_actions(key.clone(), Slot::Loading);
        self.inflight_actions.insert(key);
        let tx = self.tx.clone();
        let wf = workflow.clone();
        let rid = run_id.clone();
        tokio::task::spawn_blocking(move || {
            let r = azure::list_actions(&sub, &rg, &app, &wf, &rid);
            let _ = tx.send(Msg::ActionsLoaded {
                workflow: wf,
                run_id: rid,
                result: r,
            });
        });
    }

    /// Bounded `actions` insert. Evicts the oldest entry when over cap.
    fn insert_actions(&mut self, key: (String, String), slot: Slot<Vec<ActionInfo>>) {
        // If we're refreshing an existing key, update the order so it
        // counts as "fresh" — most-recently-touched is what we want to keep.
        if self.actions.contains_key(&key) {
            self.actions_order.retain(|k| k != &key);
        }
        self.actions_order.push_back(key.clone());
        self.actions.insert(key, slot);
        while self.actions_order.len() > ACTIONS_CAP {
            if let Some(old) = self.actions_order.pop_front() {
                self.actions.remove(&old);
            }
        }
    }

    fn spawn_chains(&mut self) {
        let (Some(sub), Some(rg), Some(app)) = (
            self.config.subscription.clone(),
            self.config.resource_group.clone(),
            self.config.logic_app.clone(),
        ) else {
            self.status = "missing config — pick a subscription + app first".into();
            return;
        };
        self.chains = Slot::Loading;
        let tx = self.tx.clone();
        tokio::task::spawn_blocking(move || {
            // Empty local dir — no `.ais-chain` manual-links file to load
            // (online-only tool). `discover_chains_remote` early-returns
            // empty manual links on "".
            let r = remote_chain::discover_chains_remote(&sub, &rg, &app, "");
            let _ = tx.send(Msg::ChainsLoaded(r));
        });
    }

    fn filtered_chains(&self) -> Vec<&ChainDetail> {
        let Slot::Loaded(chains) = &self.chains else { return vec![] };
        if self.filter.is_empty() {
            return chains.iter().collect();
        }
        let needle = self.filter.to_ascii_lowercase();
        chains
            .iter()
            .filter(|c| {
                c.label.to_ascii_lowercase().contains(&needle)
                    || c.steps.iter().any(|s| s.workflow.to_ascii_lowercase().contains(&needle))
            })
            .collect()
    }

    // ── render ──────────────────────────────────────────────────────────

    fn draw(&mut self, frame: &mut ratatui::Frame) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // top bar
                Constraint::Min(0),    // body
                Constraint::Length(1), // footer
            ])
            .split(frame.area());

        self.draw_top_bar(frame, chunks[0]);
        match &self.view {
            View::Picker(step) => {
                let s = step_clone(step);
                self.draw_picker(frame, chunks[1], s);
            }
            View::Browser => self.draw_browser(frame, chunks[1]),
            View::EventGrid => self.draw_event_grid(frame, chunks[1]),
        }
        self.draw_footer(frame, chunks[2]);

        // Modal overlay rendered last so it sits on top of everything.
        if self.modal.is_some() {
            self.draw_modal(frame, frame.area());
        }
    }

    fn draw_top_bar(&self, frame: &mut ratatui::Frame, area: Rect) {
        let login_span = match &self.login {
            Slot::Loading | Slot::Idle => Span::styled("login: …", Style::default().fg(Color::Yellow)),
            Slot::Loaded(AzLoginState::LoggedIn { account, .. }) => {
                Span::styled(format!("{account}"), Style::default().fg(Color::Green))
            }
            Slot::Loaded(AzLoginState::Expired) => {
                Span::styled("token expired", Style::default().fg(Color::Red))
            }
            Slot::Loaded(AzLoginState::NotLoggedIn) => {
                Span::styled("not logged in", Style::default().fg(Color::Red))
            }
            Slot::Loaded(AzLoginState::AzNotFound) => {
                Span::styled("az not found", Style::default().fg(Color::Red))
            }
            Slot::Loaded(AzLoginState::Checking) => Span::raw("checking…"),
            Slot::Failed(e) => Span::styled(format!("err: {e}"), Style::default().fg(Color::Red)),
        };
        let app_span = self
            .config
            .logic_app
            .as_deref()
            .map(|a| Span::styled(format!("  ·  {a}"), Style::default().fg(Color::Cyan)))
            .unwrap_or(Span::raw(""));
        // Live indicator — always present, color signals state. Pulses
        // visually when watch is on, dim grey when paused.
        let watch_span = if self.watch {
            Span::styled(
                format!("  ·  ● live {}s", self.watch_interval_secs),
                Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(
                "  ·  ○ paused",
                Style::default().fg(Color::DarkGray),
            )
        };
        let follow_span = if self.follow_running {
            Span::styled(
                "  ·  ▶ follow",
                Style::default().fg(Color::Cyan),
            )
        } else {
            Span::styled(
                "  ·  ▷ manual",
                Style::default().fg(Color::DarkGray),
            )
        };
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    " ais-monitor-tui ",
                    Style::default().add_modifier(Modifier::BOLD).bg(Color::DarkGray),
                ),
                Span::raw("  "),
                login_span,
                app_span,
                watch_span,
                follow_span,
            ])),
            area,
        );
    }

    fn draw_picker(&mut self, frame: &mut ratatui::Frame, area: Rect, step: PickerStep) {
        match step {
            PickerStep::Subs => {
                let saved_sub = self.config.subscription.as_deref();
                let (items, title): (Vec<ListItem>, String) = match &self.subs {
                    Slot::Loaded(subs) => {
                        let items = subs
                            .iter()
                            .map(|s| {
                                let mut spans = vec![];
                                if saved_sub == Some(s.id.as_str()) {
                                    spans.push(Span::styled(
                                        "★ ",
                                        Style::default().fg(Color::Yellow),
                                    ));
                                } else {
                                    spans.push(Span::raw("  "));
                                }
                                spans.push(Span::styled(
                                    s.name.clone(),
                                    Style::default().add_modifier(Modifier::BOLD),
                                ));
                                spans.push(Span::styled(
                                    format!("  ({})", s.id),
                                    Style::default().fg(Color::DarkGray),
                                ));
                                ListItem::new(Line::from(spans))
                            })
                            .collect();
                        (items, format!(" subscriptions ({}) — Enter to select ", subs.len()))
                    }
                    _ => slot_to_items(&self.subs, "subscriptions", |s| {
                        format!("{}  ({})", s.name, s.id)
                    }),
                };
                let list = List::new(items)
                    .block(Block::default().borders(Borders::ALL).title(title))
                    .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD))
                    .highlight_symbol("▶ ");
                frame.render_stateful_widget(list, area, &mut self.sub_cursor);
            }
            PickerStep::Apps => {
                // Custom item building so we can annotate each app with the
                // cached chain count + a star next to the previously-used one.
                let saved_app = self.config.logic_app.as_deref();
                let sub = self.config.subscription.as_deref().unwrap_or("");
                let (items, title): (Vec<ListItem>, String) = match &self.apps {
                    Slot::Loaded(apps) => {
                        let items = apps
                            .iter()
                            .map(|a| {
                                let count = cached_chain_count(sub, &a.name);
                                let count_span = match count {
                                    Some(0) => Span::styled(
                                        "  · empty cache",
                                        Style::default().fg(Color::DarkGray),
                                    ),
                                    Some(n) => Span::styled(
                                        format!("  · {n} chain(s) cached"),
                                        Style::default().fg(Color::Green),
                                    ),
                                    None => Span::styled(
                                        "  · no cache yet",
                                        Style::default().fg(Color::DarkGray),
                                    ),
                                };
                                let mut spans = vec![];
                                if saved_app == Some(a.name.as_str()) {
                                    spans.push(Span::styled(
                                        "★ ",
                                        Style::default().fg(Color::Yellow),
                                    ));
                                } else {
                                    spans.push(Span::raw("  "));
                                }
                                spans.push(Span::styled(
                                    a.name.clone(),
                                    Style::default().add_modifier(Modifier::BOLD),
                                ));
                                spans.push(Span::styled(
                                    format!("  [{}]", a.resource_group),
                                    Style::default().fg(Color::DarkGray),
                                ));
                                spans.push(count_span);
                                ListItem::new(Line::from(spans))
                            })
                            .collect();
                        (items, format!(" logic apps ({}) — Enter to select ", apps.len()))
                    }
                    _ => slot_to_items(&self.apps, "logic apps", |a| {
                        format!("{}  [{}]", a.name, a.resource_group)
                    }),
                };
                let list = List::new(items)
                    .block(Block::default().borders(Borders::ALL).title(title))
                    .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD))
                    .highlight_symbol("▶ ");
                frame.render_stateful_widget(list, area, &mut self.app_cursor);
            }
        }
    }

    fn draw_browser(&mut self, frame: &mut ratatui::Frame, area: Rect) {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(35), Constraint::Min(0)])
            .split(area);

        // Snapshot everything we need from `self` (which holds borrows into
        // `self.chains`) before any `&mut self` reborrow.
        let (left_items, title, detail): (Vec<ListItem>, String, Option<ChainDetail>) = {
            let visible = self.filtered_chains();
            let title = if self.filtering || !self.filter.is_empty() {
                format!(" chains [/{}] ", self.filter)
            } else {
                format!(" chains ({}) ", visible.len())
            };
            let items: Vec<ListItem> = match &self.chains {
                Slot::Idle => vec![ListItem::new("idle")],
                Slot::Loading => vec![ListItem::new(Span::styled(
                    "loading chains…",
                    Style::default().fg(Color::Yellow),
                ))],
                Slot::Failed(e) => vec![ListItem::new(Span::styled(
                    format!("error: {e}"),
                    Style::default().fg(Color::Red),
                ))],
                Slot::Loaded(_) => visible
                    .iter()
                    .map(|c| {
                        let custom = self.chain_names.get(&c.label);
                        let display = custom.cloned().unwrap_or_else(|| c.label.clone());
                        let mut spans = vec![Span::styled(
                            display,
                            Style::default().add_modifier(Modifier::BOLD),
                        )];
                        if custom.is_some() {
                            spans.push(Span::styled(
                                "  *",
                                Style::default().fg(Color::Cyan),
                            ));
                        }
                        // Surface live activity directly in the chain list:
                        // any step running anywhere in the pipeline → yellow
                        // dot + count. Lets the user see at a glance which
                        // pipelines have traffic without focusing them.
                        let running: usize = c
                            .steps
                            .iter()
                            .map(|s| running_count(&self.runs, &s.workflow))
                            .sum();
                        if running > 0 {
                            spans.push(Span::styled(
                                format!("  {} {running}", spinner_glyph()),
                                Style::default()
                                    .fg(Color::Yellow)
                                    .add_modifier(Modifier::BOLD),
                            ));
                        }
                        spans.push(Span::raw(format!("  ({} steps)", c.steps.len())));
                        ListItem::new(Line::from(spans))
                    })
                    .collect(),
            };
            let detail = self
                .chain_cursor
                .selected()
                .and_then(|i| visible.get(i).map(|c| (*c).clone()));
            (items, title, detail)
        };

        let list = List::new(left_items)
            .block(focused_block(&title, self.focus == Focus::Chains))
            .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD))
            .highlight_symbol("▶ ");
        frame.render_stateful_widget(list, cols[0], &mut self.chain_cursor);

        // Right pane: drilled view replaces everything when set.
        if self.drilled.is_some() {
            self.draw_actions_pane(frame, cols[1]);
            return;
        }

        let detail = detail.as_ref();
        let Some(c) = detail else {
            frame.render_widget(
                Paragraph::new("  (no chain selected)")
                    .block(focused_block(" detail ", false)),
                cols[1],
            );
            return;
        };

        // Right side: vertical split — Steps | KPI | Runs
        let step_count = c.steps.len() as u16;
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                // 1 border top + 1 border bottom + 2 lines per step = 2 + 2n
                Constraint::Length((2 + step_count * 2).max(5).min(14)),
                Constraint::Length(3),  // KPI strip
                Constraint::Min(0),     // Runs table
            ])
            .split(cols[1]);

        self.draw_steps(frame, rows[0], c);
        self.draw_kpi(frame, rows[1]);
        self.draw_runs(frame, rows[2]);
    }

    fn draw_steps(&mut self, frame: &mut ratatui::Frame, area: Rect, chain: &ChainDetail) {
        let items: Vec<ListItem> = chain
            .steps
            .iter()
            .enumerate()
            .map(|(idx, step)| {
                let running = running_count(&self.runs, &step.workflow);
                let mut first = vec![
                    Span::styled(
                        format!("{:>2}. ", idx + 1),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(
                        step.workflow.clone(),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                ];
                if running > 0 {
                    first.push(Span::styled(
                        format!("  {} {running} running", spinner_glyph()),
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ));
                }
                ListItem::new(vec![
                    Line::from(first),
                    Line::from(vec![
                        Span::raw("    "),
                        Span::styled(
                            format!("trigger: {}", step.trigger_info),
                            Style::default().fg(Color::Green),
                        ),
                        Span::raw("   "),
                        Span::styled(
                            format!("link: {}", step.link_type),
                            Style::default().fg(Color::Magenta),
                        ),
                    ]),
                ])
            })
            .collect();

        let total_running: usize = chain
            .steps
            .iter()
            .map(|s| running_count(&self.runs, &s.workflow))
            .sum();
        let title = if total_running > 0 {
            format!(
                " {} — {} step(s) · {} {} running ",
                chain.label,
                chain.steps.len(),
                spinner_glyph(),
                total_running
            )
        } else {
            format!(" {} — {} step(s) ", chain.label, chain.steps.len())
        };
        let list = List::new(items)
            .block(focused_block(&title, self.focus == Focus::Steps))
            .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD))
            .highlight_symbol("▶ ");
        frame.render_stateful_widget(list, area, &mut self.step_cursor);
    }

    fn draw_kpi(&self, frame: &mut ratatui::Frame, area: Rect) {
        let wf = self.focused_workflow();
        let runs_slot = wf.as_ref().and_then(|w| self.runs.get(w));
        let (label, gauge_pct, summary): (String, u16, String) = match runs_slot {
            None | Some(Slot::Idle) => (
                "no runs loaded".into(),
                0,
                "—".into(),
            ),
            Some(Slot::Loading) => ("loading…".into(), 0, "fetching runs from Azure".into()),
            Some(Slot::Failed(e)) => ("error".into(), 0, e.clone()),
            Some(Slot::Loaded(runs)) => {
                let k = kpi::compute_workflow_kpi(runs);
                (
                    format!("success {:.0}%", k.success_rate),
                    k.success_rate.round() as u16,
                    summarize_kpi(&k),
                )
            }
        };
        let inner = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(28),  // gauge
                Constraint::Min(20),     // summary text
                Constraint::Length(24),  // duration sparkline
            ])
            .split(area);
        let gauge_color = match gauge_pct {
            p if p >= 95 => Color::Green,
            p if p >= 80 => Color::Yellow,
            _ => Color::Red,
        };
        frame.render_widget(
            Gauge::default()
                .block(focused_block(" KPI ", false))
                .gauge_style(Style::default().fg(gauge_color))
                .percent(gauge_pct)
                .label(label),
            inner[0],
        );
        frame.render_widget(
            Paragraph::new(summary).block(Block::default().borders(Borders::ALL)),
            inner[1],
        );

        // Duration sparkline — only when we have data. Most-recent on the
        // right (so it reads like a chart timeline).
        let durations: Vec<u64> = match runs_slot {
            Some(Slot::Loaded(runs)) => durations_for_sparkline(runs),
            _ => Vec::new(),
        };
        if durations.is_empty() {
            frame.render_widget(
                Paragraph::new("  no durations").block(Block::default().borders(Borders::ALL)),
                inner[2],
            );
        } else {
            frame.render_widget(
                Sparkline::default()
                    .block(Block::default().borders(Borders::ALL).title(" duration "))
                    .data(&durations)
                    .style(Style::default().fg(Color::Cyan)),
                inner[2],
            );
        }
    }

    fn draw_runs(&mut self, frame: &mut ratatui::Frame, area: Rect) {
        let wf = self.focused_workflow();
        let title = match &wf {
            Some(w) => format!(" runs — {w} "),
            None => " runs ".into(),
        };
        let focused = self.focus == Focus::Runs;

        let runs_slot = wf.as_ref().and_then(|w| self.runs.get(w));
        match runs_slot {
            None | Some(Slot::Idle) => {
                frame.render_widget(
                    Paragraph::new("  (select a step)").block(focused_block(&title, focused)),
                    area,
                );
                return;
            }
            Some(Slot::Loading) => {
                frame.render_widget(
                    Paragraph::new(Span::styled(
                        "  loading runs…",
                        Style::default().fg(Color::Yellow),
                    ))
                    .block(focused_block(&title, focused)),
                    area,
                );
                return;
            }
            Some(Slot::Failed(e)) => {
                frame.render_widget(
                    Paragraph::new(Span::styled(
                        format!("  error: {e}"),
                        Style::default().fg(Color::Red),
                    ))
                    .block(focused_block(&title, focused)),
                    area,
                );
                return;
            }
            Some(Slot::Loaded(_)) => {}
        }
        // Take owned snapshot so the &mut self.run_cursor reborrow is clean.
        let runs: Vec<RunInfo> = match runs_slot {
            Some(Slot::Loaded(v)) => v.clone(),
            _ => return,
        };

        let rows: Vec<Row> = runs
            .iter()
            .map(|r| {
                let row_style = status_style(&r.status);
                Row::new(vec![
                    Cell::from(Span::styled(status_glyph(&r.status), row_style)),
                    Cell::from(Span::styled(short_time(&r.start), row_style)),
                    Cell::from(Span::styled(
                        duration_str(&r.start, r.end.as_deref()),
                        row_style,
                    )),
                    Cell::from(Span::styled(short_id(&r.id), row_style)),
                ])
            })
            .collect();

        let table = Table::new(
            rows,
            [
                Constraint::Length(12),
                Constraint::Length(20),
                Constraint::Length(12),
                Constraint::Min(0),
            ],
        )
        .header(
            Row::new(vec!["status", "start", "duration", "id"])
                .style(Style::default().fg(Color::DarkGray)),
        )
        .block(focused_block(&title, focused))
        .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD))
        .highlight_symbol("▶ ");

        frame.render_stateful_widget(table, area, &mut self.run_cursor);
    }

    fn draw_actions_pane(&mut self, frame: &mut ratatui::Frame, area: Rect) {
        let Some((wf, run_id)) = self.drilled.clone() else { return };
        let title = format!(" {wf} · run {} ", short_id(&run_id));
        let slot = self.actions.get(&(wf.clone(), run_id.clone()));
        match slot {
            None | Some(Slot::Idle) | Some(Slot::Loading) => {
                frame.render_widget(
                    Paragraph::new(Span::styled(
                        "  loading actions…",
                        Style::default().fg(Color::Yellow),
                    ))
                    .block(focused_block(&title, true)),
                    area,
                );
                return;
            }
            Some(Slot::Failed(e)) => {
                frame.render_widget(
                    Paragraph::new(Span::styled(
                        format!("  error: {e}"),
                        Style::default().fg(Color::Red),
                    ))
                    .block(focused_block(&title, true)),
                    area,
                );
                return;
            }
            Some(Slot::Loaded(_)) => {}
        }
        let actions: Vec<ActionInfo> = match slot {
            Some(Slot::Loaded(v)) => v.clone(),
            _ => return,
        };

        let items: Vec<ListItem> = actions
            .iter()
            .map(|a| {
                let mut lines = vec![Line::from(vec![
                    Span::raw(status_glyph(&a.status)),
                    Span::raw(" "),
                    Span::styled(a.name.clone(), Style::default().add_modifier(Modifier::BOLD)),
                ])];
                if let Some(err) = &a.error {
                    lines.push(Line::from(vec![
                        Span::raw("    "),
                        Span::styled(err.clone(), Style::default().fg(Color::Red)),
                    ]));
                }
                ListItem::new(lines)
            })
            .collect();

        let list = List::new(items)
            .block(focused_block(&title, self.focus == Focus::Actions))
            .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD))
            .highlight_symbol("▶ ");
        frame.render_stateful_widget(list, area, &mut self.action_cursor);
    }

    fn draw_event_grid(&mut self, frame: &mut ratatui::Frame, area: Rect) {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(40), Constraint::Min(0)])
            .split(area);

        // Left — topics
        let (topic_items, topic_title): (Vec<ListItem>, String) =
            slot_to_items(&self.eg_topics, "EG topics", |t| {
                format!("{}  {}", t.name, t.endpoint)
            });
        let list = List::new(topic_items)
            .block(focused_block(&topic_title, !self.eg_focus_subs))
            .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD))
            .highlight_symbol("▶ ");
        frame.render_stateful_widget(list, cols[0], &mut self.eg_topic_cursor);

        // Right — subscriptions for focused topic
        let topic_id = match &self.eg_topics {
            Slot::Loaded(ts) => self
                .eg_topic_cursor
                .selected()
                .and_then(|i| ts.get(i))
                .map(|t| t.id.clone()),
            _ => None,
        };
        let sub_title = " subscriptions ".to_string();
        let focused = self.eg_focus_subs;
        match topic_id.as_ref().and_then(|id| self.eg_subs.get(id)) {
            None | Some(Slot::Idle) => {
                frame.render_widget(
                    Paragraph::new("  (no topic selected)")
                        .block(focused_block(&sub_title, focused)),
                    cols[1],
                );
            }
            Some(Slot::Loading) => {
                frame.render_widget(
                    Paragraph::new(Span::styled(
                        "  loading subscriptions…",
                        Style::default().fg(Color::Yellow),
                    ))
                    .block(focused_block(&sub_title, focused)),
                    cols[1],
                );
            }
            Some(Slot::Failed(e)) => {
                frame.render_widget(
                    Paragraph::new(Span::styled(
                        format!("  error: {e}"),
                        Style::default().fg(Color::Red),
                    ))
                    .block(focused_block(&sub_title, focused)),
                    cols[1],
                );
            }
            Some(Slot::Loaded(subs)) => {
                let items: Vec<ListItem> = subs
                    .iter()
                    .map(|s| {
                        let dest = if s.destination_queue.is_empty() {
                            s.destination_type.clone()
                        } else {
                            format!("{} → {}", s.destination_type, s.destination_queue)
                        };
                        ListItem::new(vec![
                            Line::from(Span::styled(
                                s.name.clone(),
                                Style::default().add_modifier(Modifier::BOLD),
                            )),
                            Line::from(vec![
                                Span::raw("    "),
                                Span::styled(dest, Style::default().fg(Color::DarkGray)),
                            ]),
                        ])
                    })
                    .collect();
                let sub_title_owned = format!(" subscriptions ({}) ", subs.len());
                let list = List::new(items)
                    .block(focused_block(&sub_title_owned, focused))
                    .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD))
                    .highlight_symbol("▶ ");
                frame.render_stateful_widget(list, cols[1], &mut self.eg_sub_cursor);
            }
        }
    }

    fn draw_modal(&self, frame: &mut ratatui::Frame, area: Rect) {
        let Some(modal) = self.modal.as_ref() else { return };
        let popup = centered_rect(60, 30, area);
        // Clear underlying cells so the modal sits on a clean background.
        frame.render_widget(ratatui::widgets::Clear, popup);

        match modal {
            Modal::Help => {
                let body: Vec<Line> = vec![
                    Line::raw(""),
                    Line::raw("  Browser:"),
                    Line::raw("    ↑/↓ j/k    move cursor"),
                    Line::raw("    Tab/h/l    cycle focus (chains→steps→runs→[actions])"),
                    Line::raw("    Enter      drill into run"),
                    Line::raw("    /          filter chains"),
                    Line::raw("    m          rename focused chain"),
                    Line::raw("    w          pause / resume live refresh"),
                    Line::raw("    f          toggle auto-follow (jump cursor to running step)"),
                    Line::raw("    g          Event Grid panel"),
                    Line::raw("    r / R      refresh focused / hard reload"),
                    Line::raw("    c          change subscription / logic app"),
                    Line::raw(""),
                    Line::raw("  Modal:"),
                    Line::raw("    Esc / Enter / ?    dismiss"),
                    Line::raw(""),
                    Line::raw("  q / Esc    quit  (from browser; in modals: Esc cancels)"),
                ];
                frame.render_widget(
                    Paragraph::new(body)
                        .block(focused_block(" help — ? to dismiss ", true)),
                    popup,
                );
            }
            Modal::Rename { key, input } => {
                let body = vec![
                    Line::raw(""),
                    Line::from(vec![
                        Span::raw("  Renaming: "),
                        Span::styled(key.clone(), Style::default().add_modifier(Modifier::BOLD)),
                    ]),
                    Line::raw(""),
                    Line::from(vec![
                        Span::raw("  > "),
                        Span::styled(
                            input.clone(),
                            Style::default().add_modifier(Modifier::UNDERLINED),
                        ),
                        Span::styled(
                            "_",
                            Style::default().add_modifier(Modifier::SLOW_BLINK).fg(Color::Cyan),
                        ),
                    ]),
                    Line::raw(""),
                    Line::styled(
                        "  Enter to save · empty + Enter clears · Esc to cancel",
                        Style::default().fg(Color::DarkGray),
                    ),
                ];
                frame.render_widget(
                    Paragraph::new(body).block(focused_block(" rename chain ", true)),
                    popup,
                );
            }
            Modal::Login { in_progress } => {
                let reason = match &self.login {
                    Slot::Loaded(AzLoginState::Expired) => {
                        "Your Azure sign-in has expired (sign-in frequency policy)."
                    }
                    Slot::Loaded(AzLoginState::NotLoggedIn) => "You're not signed in to Azure.",
                    Slot::Loaded(AzLoginState::AzNotFound) => {
                        "`az` CLI not found on PATH — install Azure CLI and restart."
                    }
                    _ => "Azure sign-in required.",
                };
                let body = if *in_progress {
                    if self.device_code_login {
                        vec![
                            Line::raw(""),
                            Line::styled(
                                "  Switching to your terminal for device-code sign-in…",
                                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                            ),
                            Line::raw(""),
                            Line::raw("  Watch this terminal — `az` will print a short URL"),
                            Line::raw("  and an 8-character code. Open the URL on any device,"),
                            Line::raw("  paste the code, and complete sign-in."),
                            Line::raw(""),
                            Line::raw("  The TUI will resume automatically when sign-in finishes."),
                        ]
                    } else {
                        vec![
                            Line::raw(""),
                            Line::styled(
                                "  Signing in…",
                                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
                            ),
                            Line::raw(""),
                            Line::raw("  A browser window should have opened."),
                            Line::raw("  Complete the Azure sign-in there; this dialog will close"),
                            Line::raw("  automatically once the new token is detected."),
                            Line::raw(""),
                            Line::styled(
                                "  Esc to dismiss · q to quit",
                                Style::default().fg(Color::DarkGray),
                            ),
                        ]
                    }
                } else {
                    let how = if self.device_code_login {
                        "  Press L (or Enter) for device-code sign-in. We'll suspend the TUI,\n  \
                         run `az login --use-device-code` in this terminal so you can read the\n  \
                         URL + code `az` prints, then resume once sign-in completes."
                    } else {
                        "  Press L (or Enter) to sign in via your browser.\n  \
                         We'll run `az logout` then `az login` for you, then wait\n  \
                         for the token to refresh — no extra steps on your side."
                    };
                    let mut lines = vec![
                        Line::raw(""),
                        Line::from(Span::styled(
                            format!("  {reason}"),
                            Style::default().fg(Color::Red),
                        )),
                        Line::raw(""),
                    ];
                    for l in how.lines() {
                        lines.push(Line::raw(l.to_string()));
                    }
                    lines.push(Line::raw(""));
                    lines.push(Line::styled(
                        "  L  sign in     Esc  dismiss     q  quit",
                        Style::default().fg(Color::DarkGray),
                    ));
                    lines
                };
                frame.render_widget(
                    Paragraph::new(body).block(focused_block(" Azure sign-in ", true)),
                    popup,
                );
            }
        }
    }

    fn draw_footer(&self, frame: &mut ratatui::Frame, area: Rect) {
        let hints: Vec<Span> = match (&self.view, self.filtering) {
            (_, true) => vec![
                Span::styled("Enter", Style::default().fg(Color::Yellow)),
                Span::raw(" apply  "),
                Span::styled("Esc", Style::default().fg(Color::Yellow)),
                Span::raw(" cancel"),
            ],
            (View::Picker(_), _) => vec![
                Span::styled("↑↓/jk", Style::default().fg(Color::Yellow)),
                Span::raw(" move  "),
                Span::styled("Enter", Style::default().fg(Color::Yellow)),
                Span::raw(" pick  "),
                Span::styled("q", Style::default().fg(Color::Yellow)),
                Span::raw(" quit"),
            ],
            (View::Browser, _) => vec![
                Span::styled("Tab/hl", Style::default().fg(Color::Yellow)),
                Span::raw(" focus  "),
                Span::styled("Enter", Style::default().fg(Color::Yellow)),
                Span::raw(" drill  "),
                Span::styled("/", Style::default().fg(Color::Yellow)),
                Span::raw(" filter  "),
                Span::styled("m", Style::default().fg(Color::Yellow)),
                Span::raw(" rename  "),
                Span::styled("w", Style::default().fg(Color::Yellow)),
                Span::raw(" pause/resume  "),
                Span::styled("f", Style::default().fg(Color::Yellow)),
                Span::raw(" follow  "),
                Span::styled("g", Style::default().fg(Color::Yellow)),
                Span::raw(" eg  "),
                Span::styled("?", Style::default().fg(Color::Yellow)),
                Span::raw(" help  "),
                Span::styled("q", Style::default().fg(Color::Yellow)),
                Span::raw(" quit"),
            ],
            (View::EventGrid, _) => vec![
                Span::styled("↑↓/jk", Style::default().fg(Color::Yellow)),
                Span::raw(" move  "),
                Span::styled("Tab/hl", Style::default().fg(Color::Yellow)),
                Span::raw(" focus  "),
                Span::styled("r", Style::default().fg(Color::Yellow)),
                Span::raw(" refresh  "),
                Span::styled("g/Esc", Style::default().fg(Color::Yellow)),
                Span::raw(" back  "),
                Span::styled("q", Style::default().fg(Color::Yellow)),
                Span::raw(" quit"),
            ],
        };
        let mut line = Line::from(hints);
        if !self.status.is_empty() {
            line.spans.push(Span::raw("    "));
            line.spans
                .push(Span::styled(self.status.clone(), Style::default().fg(Color::DarkGray)));
        }
        frame.render_widget(Paragraph::new(line), area);
    }
}

pub async fn run(terminal: &mut Tui, cli: CliArgs) -> Result<()> {
    let (tx, rx) = mpsc::unbounded_channel::<Msg>();
    let mut app = App::new(tx, cli);
    app.run(terminal, rx).await
}

// ── helpers ────────────────────────────────────────────────────────────

fn translate_event(ev: Event) -> Option<Msg> {
    match ev {
        Event::Key(k) => Some(Msg::Key(k)),
        Event::Resize(w, h) => Some(Msg::Resize(w, h)),
        _ => None,
    }
}

fn step_clone(s: &PickerStep) -> PickerStep {
    match s {
        PickerStep::Subs => PickerStep::Subs,
        PickerStep::Apps => PickerStep::Apps,
    }
}

fn move_cursor<T>(state: &mut ListState, slot: &Slot<Vec<T>>, delta: i32) {
    let Slot::Loaded(v) = slot else { return };
    if v.is_empty() {
        return;
    }
    let n = v.len() as i32;
    let cur = state.selected().unwrap_or(0) as i32;
    let next = ((cur + delta).rem_euclid(n)) as usize;
    state.select(Some(next));
}

/// Cheap chain-count probe used to annotate picker rows. Reads the cached
/// chain graph that `discover_chains_remote` writes after every successful
/// fetch. `None` if there's no cache — the user has never run a discovery
/// for this (sub, app).
fn cached_chain_count(sub: &str, app: &str) -> Option<usize> {
    let root = std::env::var_os("AIS_MONITOR_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| dirs::cache_dir().map(|d| d.join("ais-monitor")))?;
    let path = root.join(format!("{sub}_{app}")).join("_chains.json");
    let content = std::fs::read_to_string(path).ok()?;
    let arr: serde_json::Value = serde_json::from_str(&content).ok()?;
    arr.as_array().map(|a| a.len())
}

/// Where to store/load custom chain names. One directory per (sub, app), so
/// switching apps doesn't mix names. We piggyback on `core::names`, which
/// writes `<dir>/.ais-monitor-names`.
fn names_dir(config: &Config) -> Option<String> {
    let (sub, app) = match (&config.subscription, &config.logic_app) {
        (Some(s), Some(a)) => (s, a),
        _ => return None,
    };
    let root = std::env::var_os("AIS_MONITOR_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| dirs::config_dir().map(|d| d.join("ais-monitor")))?;
    let dir = root.join("names").join(format!("{sub}_{app}"));
    let _ = std::fs::create_dir_all(&dir);
    Some(dir.to_string_lossy().into_owned())
}

/// Spawn the periodic tick that drives watch mode. One task per process.
fn spawn_tick_task(tx: UnboundedSender<Msg>, interval_secs: u64) {
    tokio::spawn(async move {
        let mut iv = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
        // Skip the immediate first tick — we don't want to refresh before the
        // user has even pressed `w`.
        iv.tick().await;
        loop {
            iv.tick().await;
            if tx.send(Msg::Tick).is_err() {
                break; // receiver dropped → app exiting
            }
        }
    });
}

/// Fast redraw tick at 10 Hz so spinners animate. Cheap: each Msg::RenderTick
/// just triggers a `terminal.draw()` next loop iteration.
fn spawn_render_tick(tx: UnboundedSender<Msg>) {
    tokio::spawn(async move {
        let mut iv = tokio::time::interval(std::time::Duration::from_millis(100));
        iv.tick().await;
        loop {
            iv.tick().await;
            if tx.send(Msg::RenderTick).is_err() {
                break;
            }
        }
    });
}

/// Animated spinner glyph derived from wall-clock time, so the frame is the
/// same regardless of who's rendering. Braille-style 10-frame cycle reads
/// smoothly in any terminal font that ships modern Unicode coverage.
fn spinner_glyph() -> char {
    const FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    FRAMES[((ms / 100) as usize) % FRAMES.len()]
}

/// Center a popup of (pct_w, pct_h) inside `area`.
fn centered_rect(pct_w: u16, pct_h: u16, area: Rect) -> Rect {
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - pct_h) / 2),
            Constraint::Percentage(pct_h),
            Constraint::Percentage((100 - pct_h) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - pct_w) / 2),
            Constraint::Percentage(pct_w),
            Constraint::Percentage((100 - pct_w) / 2),
        ])
        .split(v[1])[1]
}

fn slot_from<T>(r: Result<T, String>) -> Slot<T> {
    match r {
        Ok(v) => Slot::Loaded(v),
        Err(e) => Slot::Failed(e),
    }
}

/// Tiny abstraction so the same `move_cursor_n` helper drives both
/// `ListState` and `TableState` cursors.
trait Cursor {
    fn get(&self) -> Option<usize>;
    fn set(&mut self, i: Option<usize>);
}
impl Cursor for ListState {
    fn get(&self) -> Option<usize> { self.selected() }
    fn set(&mut self, i: Option<usize>) { self.select(i) }
}
impl Cursor for TableState {
    fn get(&self) -> Option<usize> { self.selected() }
    fn set(&mut self, i: Option<usize>) { self.select(i) }
}

fn move_cursor_n<C: Cursor>(state: &mut C, len: usize, delta: i32) {
    if len == 0 {
        return;
    }
    let n = len as i32;
    let cur = state.get().unwrap_or(0) as i32;
    let next = ((cur + delta).rem_euclid(n)) as usize;
    state.set(Some(next));
}

/// Bordered block whose title/border highlights when the pane has focus.
fn focused_block(title: &str, focused: bool) -> Block<'_> {
    let style = if focused {
        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    Block::default()
        .borders(Borders::ALL)
        .border_style(style)
        .title(Span::styled(title.to_string(), style))
}

fn status_glyph(status: &str) -> String {
    match status {
        "Succeeded" => "  ok ".into(),
        "Failed" => " FAIL".into(),
        // Animated spinner — refreshed by the render-tick task at 10 Hz.
        "Running" => format!(" {} run", spinner_glyph()),
        "Cancelled" => " canc".into(),
        "Skipped" => " skip".into(),
        s => format!(" {:<4}", s.chars().take(4).collect::<String>()),
    }
}

/// Color for a run-status glyph. Running pops yellow+bold so an active run
/// catches the eye even in a packed table.
fn status_style(status: &str) -> Style {
    match status {
        "Running" => Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
        "Failed" => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        "Succeeded" => Style::default().fg(Color::Green),
        "Cancelled" | "Skipped" => Style::default().fg(Color::DarkGray),
        _ => Style::default(),
    }
}

/// Count how many runs are currently in flight for `workflow` according to
/// our cached snapshot. Watch-mode keeps this fresh every tick.
fn running_count(runs_cache: &HashMap<String, Slot<Vec<RunInfo>>>, workflow: &str) -> usize {
    match runs_cache.get(workflow) {
        Some(Slot::Loaded(v)) => v.iter().filter(|r| r.status == "Running").count(),
        _ => 0,
    }
}

/// Render an Azure RFC3339 timestamp in the user's local timezone, trimmed
/// to "MM-DD HH:MM:SS". Azure always returns UTC; without conversion the
/// user sees clock-skewed times (e.g. CEST shown as UTC = 2 h behind).
fn short_time(s: &str) -> String {
    use chrono::{DateTime, Local};
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return dt.with_timezone(&Local).format("%m-%d %H:%M:%S").to_string();
    }
    // Fallback if Azure ever returns something we can't parse — keep the
    // raw string but at least strip the T and trailing fractional seconds.
    let s = s.replace('T', " ");
    let cut = s.find('.').or_else(|| s.find('+')).or_else(|| s.find('Z'));
    let trimmed = cut.map(|i| &s[..i]).unwrap_or(&s);
    trimmed.get(5..).unwrap_or(trimmed).to_string()
}

fn duration_str(start: &str, end: Option<&str>) -> String {
    use chrono::DateTime;
    let Some(end) = end else { return "—".into() };
    let (Ok(s), Ok(e)) = (
        DateTime::parse_from_rfc3339(start),
        DateTime::parse_from_rfc3339(end),
    ) else {
        return "?".into();
    };
    let secs = (e - s).num_milliseconds() as f64 / 1000.0;
    if secs < 60.0 {
        format!("{secs:.1}s")
    } else if secs < 3600.0 {
        format!("{:.1}m", secs / 60.0)
    } else {
        format!("{:.1}h", secs / 3600.0)
    }
}

/// Logic Apps run IDs are long — show the last 12 chars, enough to disambiguate.
fn short_id(s: &str) -> String {
    if s.len() <= 14 {
        s.to_string()
    } else {
        format!("…{}", &s[s.len() - 13..])
    }
}

/// Compute durations in seconds for the sparkline. Reversed so the most
/// recent run is on the right, matching reading-order for a time series.
/// Running / no-end runs are skipped.
fn durations_for_sparkline(runs: &[RunInfo]) -> Vec<u64> {
    use chrono::DateTime;
    let mut out: Vec<u64> = runs
        .iter()
        .rev()
        .filter_map(|r| {
            let end = r.end.as_ref()?;
            let (Ok(s), Ok(e)) = (
                DateTime::parse_from_rfc3339(&r.start),
                DateTime::parse_from_rfc3339(end),
            ) else {
                return None;
            };
            let ms = (e - s).num_milliseconds().max(0) as u64;
            Some(ms)
        })
        .collect();
    // Sparkline scales linearly to max — a single huge outlier flattens the
    // rest. Clip to 2× p95 to keep the chart readable.
    if out.len() >= 4 {
        let mut sorted = out.clone();
        sorted.sort_unstable();
        let p95 = sorted[(sorted.len() as f64 * 0.95).floor() as usize];
        let ceiling = (p95 * 2).max(1);
        for v in &mut out {
            if *v > ceiling {
                *v = ceiling;
            }
        }
    }
    out
}

fn summarize_kpi(k: &ChainKpi) -> String {
    let avg = k
        .avg_duration_secs
        .map(|s| format!("avg {s:.1}s"))
        .unwrap_or_else(|| "avg —".into());
    let p95 = k
        .p95_duration_secs
        .map(|s| format!("p95 {s:.1}s"))
        .unwrap_or_else(|| "p95 —".into());
    let streak = if k.failure_streak > 0 {
        format!("streak {}!", k.failure_streak)
    } else {
        "streak 0".into()
    };
    format!(
        "  {}/{} runs   {}   {}   {}",
        k.succeeded, k.total_runs, avg, p95, streak
    )
}

/// Render the three non-loaded `Slot` states as a single-item list so callers
/// don't repeat the boilerplate. Loaded data goes through the caller's mapper.
fn slot_to_items<T, F>(slot: &Slot<Vec<T>>, name: &str, mapper: F) -> (Vec<ListItem<'static>>, String)
where
    F: Fn(&T) -> String,
{
    match slot {
        Slot::Idle => (vec![ListItem::new("idle")], format!(" {name} ")),
        Slot::Loading => (
            vec![ListItem::new(Span::styled(
                format!("loading {name}…"),
                Style::default().fg(Color::Yellow),
            ))],
            format!(" {name} "),
        ),
        Slot::Failed(e) => (
            vec![ListItem::new(Span::styled(
                format!("error: {e}"),
                Style::default().fg(Color::Red),
            ))],
            format!(" {name} (error) "),
        ),
        Slot::Loaded(v) => (
            v.iter()
                .map(|x| ListItem::new(mapper(x)))
                .collect(),
            format!(" {name} ({}) ", v.len()),
        ),
    }
}
