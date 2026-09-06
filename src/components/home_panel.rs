use crate::components::chain_detail::{AzConfig, ChainHealth, QueueStatus};
use crate::services::{
    azure, chain, chain_probe, functions_cache, health_cache, history_cache, resource_health_cache,
};
use chrono::{DateTime, Duration, Local, TimeZone, Utc};
use dioxus::prelude::*;
use std::collections::HashMap;

/// A chain whose success rate has dropped meaningfully below its own recent
/// average — caught by comparing the latest history point against the mean
/// of the points before it, so a chain that's always been at 80% doesn't
/// read as a regression while one that fell from 99% to 80% does.
const REGRESSION_DROP_PCT: f64 = 10.0;

/// A workflow whose most recent *terminal* run failed and that hasn't
/// succeeded since — i.e. it is still broken right now, as opposed to a
/// workflow that failed earlier and has already recovered. This is the
/// signal worth acting on in the morning; a raw consecutive-failure count
/// says nothing about whether the problem is still live.
#[derive(Clone, Debug, PartialEq)]
struct UnrecoveredFailure {
    chain: String,
    workflow: String,
    failed_at: DateTime<Utc>,
    /// How many failures in a row lead up to now, for a sense of severity.
    consecutive: usize,
    /// The failed run itself, so the row can pull that run's action-level
    /// detail on click without re-listing runs.
    run_id: String,
}

/// A run that is in flight right now. Azure reports this as a run whose
/// status is still `Running` — the same signal the TUI uses to drive its
/// live spinner (`running_count` in `crates/tui/src/app.rs`).
#[derive(Clone, Debug, PartialEq)]
struct LiveRun {
    /// Every chain containing this workflow. A workflow can belong to several
    /// chains, and the poll stores runs per chain, so one Azure run is seen
    /// once per chain — this collects those rather than repeating the run.
    chains: Vec<String>,
    workflow: String,
    started: DateTime<Utc>,
}

/// Per-chain rollup of in-flight runs.
#[derive(Clone, Debug, PartialEq)]
struct LiveChain {
    chain: String,
    runs: usize,
    workflows: usize,
    /// Start of the longest-running run in this chain.
    oldest: DateTime<Utc>,
}

/// Starting poll interval, before the environment is known. This app is meant
/// to sit on a wall display all day, so the numbers have to stay current with
/// nobody clicking anything — but each cycle costs one `az` call per distinct
/// workflow plus one per distinct queue.
///
/// Only the initial value: once discovery lands, `recommended_interval` sizes
/// this to the actual workload (and the user can override it in the picker).
/// Ten seconds is right for a handful of workflows and wildly wrong for
/// several dozen, which is how the subscription throttle got tripped.
const POLL_SECS: u64 = 10;

/// How often the driver loop wakes to check whether a sweep is due. Short
/// so chain discovery finishing mid-interval is noticed promptly; the tick
/// itself is a no-op when nothing is due.
const POLL_TICK_SECS: u64 = 2;

/// How many chains to probe concurrently. Each chain costs one `az` process
/// per workflow plus one per queue, and every `az` invocation carries CLI
/// start-up overhead — probed one at a time, a sweep of a real environment
/// takes far longer than `POLL_SECS`.
///
/// Kept deliberately low. The hostruntime endpoint
/// (`Microsoft.Web/sites/{name}/hostruntime/...`) throttles on *burst* per
/// subscription (error 51020), and a wide fan-out trips it even when the
/// average rate would be fine. Two is enough to overlap CLI start-up cost
/// without looking like a flood.
const POLL_CONCURRENCY: usize = 2;

/// Extra delay added on top of `POLL_SECS` after a sweep that hit Azure
/// throttling (429), doubling each consecutive throttled sweep up to this
/// cap. Without it, a throttled subscription gets hammered with the exact
/// same full-chain workload every cycle forever, which only prolongs the
/// throttling and keeps spawning `az` subprocesses for nothing.
const MAX_POLL_BACKOFF_SECS: u64 = 300;

/// First backoff step after a 429, overriding the usual "start at
/// `POLL_SECS` and double". A subscription-wide hostruntime throttle does
/// not clear in ten seconds, and probing again that soon is itself part of
/// what sustains it — so the first response has to be a real pause rather
/// than a token one.
const THROTTLE_MIN_BACKOFF_SECS: u64 = 60;

/// Rows a grid card shows before it needs "Show more". The cards sit side by
/// side, so they are given one fixed height rather than each growing to its
/// own content — otherwise a card with 40 failures drags the whole row down
/// and pushes everything below it off screen.
const HOME_CARD_ROWS: usize = 5;

/// Run-history depth per workflow for the poll. Much shallower than the
/// Chains tab's default: the dashboard only needs enough history to tell
/// whether the latest run failed and what's in flight, not a KPI sample.
const POLL_DEPTH: u32 = 10;

#[derive(Props, Clone, PartialEq)]
pub struct HomePanelProps {
    pub az_config: AzConfig,
    /// The chains to poll. Empty until MainScreen finishes discovery — a
    /// signal rather than a plain Vec so the long-lived poll loop always
    /// sees the current set instead of a snapshot taken before discovery.
    pub chains: Signal<Vec<chain::ChainDetail>>,
    /// Shared with MainScreen and written through by the poll, so a refresh
    /// here also populates the Chains tab rather than duplicating state.
    pub chain_health: Signal<HashMap<String, ChainHealth>>,
    pub last_checked: Signal<HashMap<String, u64>>,
    /// Raw per-chain, per-workflow run lists — needed to tell an unrecovered
    /// failure from one that already succeeded again, and to spot in-flight
    /// runs, neither of which the aggregated `ChainHealth` can express.
    pub chain_runs: Signal<HashMap<String, HashMap<String, Vec<azure::RunInfo>>>>,
    pub chain_queue_statuses: Signal<HashMap<String, HashMap<String, QueueStatus>>>,
    /// Namespace discovered by MainScreen, used when the profile has none.
    pub discovered_sb_namespace: Signal<Option<String>>,
    /// Logic App region discovered by MainScreen — needed to build a deep
    /// link straight to a workflow's run history; without it the portal link
    /// can only reach the parent Logic App.
    pub discovered_location: Signal<Option<String>>,
    /// User-set display-name overlay, keyed by the chain's stable `label`.
    /// Every chain identifier used elsewhere in this component (poll maps,
    /// history, queue stats) is still the raw `label` — this is consulted
    /// only at render time so a rename shows up immediately without
    /// invalidating any of that keyed state.
    pub chain_names: Signal<HashMap<String, String>>,
}

#[component]
pub fn HomePanel(props: HomePanelProps) -> Element {
    let az = props.az_config.clone();
    let has_store = !az.app_config_store.trim().is_empty();

    // Live-fetched pieces (resource health, drift, RBAC) — the rest comes
    // from caches the other tabs already populate, so opening Home doesn't
    // re-run every expensive sweep in the app.
    let mut res_unhealthy: Signal<Option<(usize, usize)>> = use_signal(|| None);
    let mut drift_count: Signal<Option<usize>> = use_signal(|| None);
    let mut rbac_gaps: Signal<Option<usize>> = use_signal(|| None);
    let mut cost: Signal<Option<(f64, String)>> = use_signal(|| None);
    let mut loading: Signal<bool> = use_signal(|| true);
    let mut error_msg: Signal<Option<String>> = use_signal(|| None);
    let mut res_fetched_at: Signal<u64> = use_signal(|| 0);
    // Lookback window for the failure list, in days. 1 means "today" —
    // since local midnight, not the last 24h, since that's what people
    // mean when they ask what broke today.
    let mut window_days: Signal<i64> = use_signal(|| 3);

    // Failure drill-down: which failed run's action log is open, keyed by
    // (workflow, run_id) so re-selecting the same row closes it again.
    let mut open_log: Signal<Option<(String, String)>> = use_signal(|| None);
    let mut log_actions: Signal<Vec<azure::ActionInfo>> = use_signal(Vec::new);
    let mut log_loading: Signal<bool> = use_signal(|| false);
    let mut log_error: Signal<Option<String>> = use_signal(|| None);

    // The rollup load runs on mount and again on Refresh, and it publishes
    // in four stages as each section resolves — so a superseded run has four
    // separate chances to overwrite the newer one's numbers. Each stage
    // checks first. (`poll_chains` below needs no guard: it already refuses
    // to start while `chain_polling` is set.)
    let mut guard = crate::hooks::fetch_guard::use_fetch_guard();
    // Expanding an action log is keyed to the row that was clicked; a slow
    // fetch for a row the user has since collapsed must not repopulate it.
    let mut log_guard = crate::hooks::fetch_guard::use_fetch_guard();

    let mut load = {
        let az = az.clone();
        move || {
            let az = az.clone();
            let token = guard.begin();
            loading.set(true);
            error_msg.set(None);
            spawn(async move {
                let sub = az.subscription.clone();
                let rg = az.resource_group.clone();
                let store = az.app_config_store.trim().to_string();

                // Resource health
                let sub_r = sub.clone();
                let rg_r = rg.clone();
                if let Ok(Ok(rows)) =
                    tokio::task::spawn_blocking(move || azure::list_resource_health(&sub_r, &rg_r))
                        .await
                {
                    let total = rows.len();
                    let bad = rows
                        .iter()
                        .filter(|r| {
                            !matches!(
                                r.state.as_str(),
                                "Running" | "Succeeded" | "Active" | "Enabled"
                            ) || r.health == "Degraded"
                                || r.health == "Unavailable"
                        })
                        .count();
                    if !guard.is_current(token) {
                        return;
                    }
                    res_unhealthy.set(Some((bad, total)));
                    res_fetched_at.set(epoch_secs());
                }

                // Function apps — shared by the drift and RBAC rollups.
                let sub_a = sub.clone();
                let rg_a = rg.clone();
                let apps =
                    tokio::task::spawn_blocking(move || azure::list_function_apps(&sub_a, &rg_a))
                        .await
                        .unwrap_or(Ok(Vec::new()))
                        .unwrap_or_default();

                // Config drift across all function apps
                let expected = if store.is_empty() {
                    None
                } else {
                    let sub_c = sub.clone();
                    let store2 = store.clone();
                    tokio::task::spawn_blocking(move || azure::appconfig_list_kv(&sub_c, &store2))
                        .await
                        .ok()
                        .and_then(|r| r.ok())
                };
                let mut drift = 0usize;
                for app in &apps {
                    let sub_d = sub.clone();
                    let rg_d = rg.clone();
                    let name = app.name.clone();
                    let expected2 = expected.clone();
                    let rows = tokio::task::spawn_blocking(move || {
                        let live =
                            azure::get_app_settings(&sub_d, &rg_d, &name).unwrap_or_default();
                        azure::compute_app_settings_drift(&live, expected2.as_ref())
                    })
                    .await
                    .unwrap_or_default();
                    drift += rows
                        .iter()
                        .filter(|r| {
                            matches!(
                                r.status,
                                azure::DriftStatus::Diff
                                    | azure::DriftStatus::LiteralWarn { .. }
                                    | azure::DriftStatus::KvFail { .. }
                                    | azure::DriftStatus::MissingLive
                            )
                        })
                        .count();
                }
                if !guard.is_current(token) {
                    return;
                }
                drift_count.set(Some(drift));

                // RBAC gaps — identities with no role assignments at all.
                let mut gaps = 0usize;
                for app in &apps {
                    if app.principal_id.is_empty() {
                        continue;
                    }
                    let pid = app.principal_id.clone();
                    let roles =
                        tokio::task::spawn_blocking(move || azure::list_role_assignments(&pid))
                            .await
                            .unwrap_or(Ok(Vec::new()))
                            .unwrap_or_default();
                    if roles.is_empty() {
                        gaps += 1;
                    }
                }
                if !guard.is_current(token) {
                    return;
                }
                rbac_gaps.set(Some(gaps));

                // Cost MTD
                let sub_x = sub.clone();
                let rg_x = rg.clone();
                if let Ok(Ok(c)) =
                    tokio::task::spawn_blocking(move || azure::get_cost_mtd(&sub_x, &rg_x)).await
                {
                    if !guard.is_current(token) {
                        return;
                    }
                    cost.set(Some(c));
                }

                if !guard.is_current(token) {
                    return;
                }
                loading.set(false);
            });
        }
    };

    use_effect({
        let mut load = load.clone();
        move || load()
    });

    // Toggle the action log for a failed run. Clicking the open row closes
    // it, so the same click target both opens and dismisses the console.
    let toggle_log = {
        let az = az.clone();
        move |workflow: String, run_id: String| {
            let key = (workflow.clone(), run_id.clone());
            if open_log.read().as_ref() == Some(&key) {
                open_log.set(None);
                log_actions.set(Vec::new());
                log_error.set(None);
                return;
            }
            let az = az.clone();
            open_log.set(Some(key));
            log_actions.set(Vec::new());
            log_error.set(None);
            let token = log_guard.begin();
            log_loading.set(true);
            spawn(async move {
                let sub = az.subscription.clone();
                let rg = az.resource_group.clone();
                let app = az.app_name.clone();
                let result = tokio::task::spawn_blocking(move || {
                    azure::list_actions(&sub, &rg, &app, &workflow, &run_id)
                })
                .await
                .unwrap_or_else(|e| Err(format!("{e}")));
                if !log_guard.is_current(token) {
                    return;
                }
                match result {
                    Ok(actions) => log_actions.set(actions),
                    Err(e) => log_error.set(Some(e)),
                }
                log_loading.set(false);
            });
        }
    };

    // ── Background chain poll ───────────────────────────────────────────
    // Re-probes every chain on a loop so the dashboard stays truthful on an
    // unattended display. Chains are probed sequentially and written through
    // one at a time, so the page fills in progressively instead of sitting
    // blank until the whole sweep finishes.
    let mut chain_poll_at: Signal<u64> = use_signal(|| 0);
    let mut chain_polling: Signal<bool> = use_signal(|| false);
    // User-facing controls: on by default at POLL_SECS, but a wallboard
    // still needs a way to stop hammering a throttled tenant or slow down
    // rather than wait out `MAX_POLL_BACKOFF_SECS`. This is the *requested*
    // interval; the throttle backoff above still stacks on top of it.
    let mut poll_enabled: Signal<bool> = use_signal(|| true);
    let mut poll_interval_secs: Signal<u64> = use_signal(|| POLL_SECS);
    let mut poll_errors: Signal<usize> = use_signal(|| 0);
    // A few real error strings from the last sweep. A bare count tells you
    // something broke but not what — and "why is this card empty?" is
    // exactly the question the count can't answer.
    let mut poll_error_samples: Signal<Vec<String>> = use_signal(Vec::new);
    // Why the last sweep stopped early, if it did — drives the one-line
    // banner instead of dumping raw ARM JSON across the dashboard.
    let mut poll_halt: Signal<Option<chain_probe::ProbeHalt>> = use_signal(|| None);
    // True for the whole sign-in wait, so a sign-in button cannot look
    // inert while a browser flow is in progress.
    let signing_in = use_signal(|| false);
    // Wall-clock duration of the last sweep. Surfaced in the header because
    // it's the honest answer to "am I actually getting the interval I asked
    // for?" — if this exceeds POLL_SECS the sweeps are running back-to-back.
    let mut sweep_secs: Signal<f64> = use_signal(|| 0.0);
    // Extra delay stacked on top of POLL_SECS while Azure (or the local
    // network stack) is throttling/resetting connections. Doubles each
    // throttled sweep, capped at MAX_POLL_BACKOFF_SECS, and resets to zero
    // the moment a sweep comes back clean.
    let mut poll_backoff_secs: Signal<u64> = use_signal(|| 0);

    // Size the default interval to the environment once discovery lands. An
    // explicit pick from the picker wins and is never overridden — this only
    // replaces the fixed default, which is far too fast for a large app.
    // Which grid cards the user has opened up. Expanding keeps the card's
    // height — it only turns the clipped body into a scrollable one, so the
    // row of cards stays aligned and the page never jumps.
    let expanded_cards: Signal<std::collections::HashSet<&'static str>> =
        use_signal(std::collections::HashSet::new);

    let mut interval_user_set: Signal<bool> = use_signal(|| false);
    {
        let chains_sig = props.chains;
        use_effect(move || {
            let chains = chains_sig.read();
            if chains.is_empty() || *interval_user_set.peek() {
                return;
            }
            let rec = recommended_interval(calls_per_sweep(&chains));
            if *poll_interval_secs.peek() != rec {
                poll_interval_secs.set(rec);
            }
        });
    }

    let mut poll_chains = {
        let az = az.clone();
        let chains_sig = props.chains;
        let mut chain_runs = props.chain_runs;
        let mut chain_health_sig = props.chain_health;
        let mut last_checked_sig = props.last_checked;
        let mut queue_statuses = props.chain_queue_statuses;
        let discovered_ns = props.discovered_sb_namespace;
        move || {
            // `peek` throughout, never `read`: this closure is called from
            // effects and from the poll loop, and subscribing those scopes to
            // signals we then write would rerun them and re-trigger the poll.
            if *chain_polling.peek() {
                return;
            }
            let chains = chains_sig.peek().clone();
            if chains.is_empty() {
                return;
            }
            let az = az.clone();
            chain_polling.set(true);
            spawn(async move {
                let sweep_started = std::time::Instant::now();
                let mut errs = 0usize;
                let mut samples: Vec<String> = Vec::new();
                let mut halt: Option<chain_probe::ProbeHalt> = None;
                // Resolved once per sweep rather than per chain — it can't
                // change mid-sweep and peeking a signal in a loop is waste.
                let ns = if !az.sb_namespace.is_empty() {
                    az.sb_namespace.clone()
                } else {
                    discovered_ns.peek().clone().unwrap_or_default()
                };

                // Fetch each distinct workflow and queue once, then fan the
                // results out to every chain referencing them. Chains overlap
                // heavily — 38 chains here span 177 slots but only 66 distinct
                // workflows — so probing chain-by-chain spent roughly two
                // thirds of its calls re-reading identical run history, at a
                // sustained rate that is itself what trips the subscription's
                // hostruntime throttle.
                let mut wf_names: Vec<String> = chains
                    .iter()
                    .flat_map(|c| c.steps.iter().map(|s| s.workflow.clone()))
                    .collect();
                wf_names.sort();
                wf_names.dedup();
                let mut queue_names: Vec<String> = chains
                    .iter()
                    .flat_map(|c| c.queues.iter().cloned())
                    .collect();
                queue_names.sort();
                queue_names.dedup();

                let mut all_runs: HashMap<String, Vec<azure::RunInfo>> = HashMap::new();
                'runs: for group in wf_names.chunks(POLL_CONCURRENCY) {
                    let mut pending = Vec::with_capacity(group.len());
                    for wf in group {
                        let sub = az.subscription.clone();
                        let rg = az.resource_group.clone();
                        let app = az.app_name.clone();
                        let wf = wf.clone();
                        pending.push(tokio::task::spawn_blocking(move || {
                            let r = azure::list_runs(&sub, &rg, &app, &wf, POLL_DEPTH);
                            (wf, r)
                        }));
                    }
                    for handle in pending {
                        let Ok((wf, result)) = handle.await else {
                            errs += 1;
                            continue;
                        };
                        match result {
                            Ok(runs) => {
                                all_runs.insert(wf, runs);
                            }
                            Err(e) => {
                                errs += 1;
                                halt = halt.or(chain_probe::classify(&e));
                                let line = format!("list_runs {wf}: {e}");
                                if samples.len() < 5 && !samples.contains(&line) {
                                    samples.push(line);
                                }
                            }
                        }
                    }
                    // App-wide failure: the rest would fail identically.
                    if halt.is_some() {
                        break 'runs;
                    }
                }

                let mut all_queues: HashMap<String, QueueStatus> = HashMap::new();
                if !ns.is_empty() && halt != Some(chain_probe::ProbeHalt::Throttled) {
                    'queues: for group in queue_names.chunks(POLL_CONCURRENCY) {
                        let mut pending = Vec::with_capacity(group.len());
                        for q in group {
                            let rg = az.resource_group.clone();
                            let ns = ns.clone();
                            let q = q.clone();
                            pending.push(tokio::task::spawn_blocking(move || {
                                let r = azure::check_queue(&ns, &rg, &q);
                                (q, r)
                            }));
                        }
                        for handle in pending {
                            let Ok((q, result)) = handle.await else {
                                errs += 1;
                                continue;
                            };
                            match result {
                                Ok(qi) => {
                                    all_queues.insert(
                                        q,
                                        QueueStatus {
                                            active: qi.active,
                                            dead_letter: qi.dead_letter,
                                        },
                                    );
                                }
                                Err(e) => {
                                    errs += 1;
                                    halt = halt.or(chain_probe::classify(&e));
                                    let line = format!("check_queue {q}: {e}");
                                    if samples.len() < 5 && !samples.contains(&line) {
                                        samples.push(line);
                                    }
                                }
                            }
                        }
                        if halt.is_some() {
                            break 'queues;
                        }
                    }
                }

                // Pure assembly from here — no further calls.
                let now = epoch_secs();
                for ch in &chains {
                    let steps: Vec<String> = ch.steps.iter().map(|s| s.workflow.clone()).collect();
                    let probe = chain_probe::assemble(&steps, &ch.queues, &all_runs, &all_queues);
                    chain_runs.write().insert(ch.label.clone(), probe.runs);
                    chain_health_sig
                        .write()
                        .insert(ch.label.clone(), probe.health);
                    queue_statuses
                        .write()
                        .insert(ch.label.clone(), probe.queues);
                    last_checked_sig.write().insert(ch.label.clone(), now);
                }
                if errs > 0 {
                    crate::services::activity::error(
                        "Home poll read errors",
                        format!("{errs} error(s)"),
                        samples.join("\n"),
                    );
                }
                poll_errors.set(errs);
                poll_error_samples.set(samples);
                poll_halt.set(halt);
                sweep_secs.set(sweep_started.elapsed().as_secs_f64());
                chain_poll_at.set(epoch_secs());
                // Both halt reasons back off — an unauthorized sweep will not
                // start succeeding within ten seconds either, and re-running
                // it just refills the log with the same denial.
                let base_interval = *poll_interval_secs.peek();
                match halt {
                    Some(reason) => {
                        let prev = *poll_backoff_secs.peek();
                        // A throttle starts from a real pause; other halts can
                        // start from the user's chosen interval and climb.
                        let floor = if reason == chain_probe::ProbeHalt::Throttled {
                            THROTTLE_MIN_BACKOFF_SECS
                        } else {
                            base_interval
                        };
                        let next = if prev == 0 {
                            floor
                        } else {
                            (prev * 2).max(floor)
                        };
                        poll_backoff_secs.set(next.min(MAX_POLL_BACKOFF_SECS));
                        if reason == chain_probe::ProbeHalt::Throttled {
                            crate::services::activity::warn(
                                "Azure throttled run-history reads",
                                format!("{}/{}", az.resource_group, az.app_name),
                                format!(
                                    "The hostruntime endpoint is throttled for this whole \
                                     subscription (429/51020), so it affects every tool hitting \
                                     it, not just this poll. Backing off to {}s.\n\nThis quota \
                                     is on burst as well as rate: if it keeps recurring, the \
                                     poll interval is too aggressive for the number of workflows \
                                     in this environment — raise the interval from the Home tab \
                                     rather than waiting it out.",
                                    base_interval + next.min(MAX_POLL_BACKOFF_SECS),
                                ),
                            );
                        }
                        if reason == chain_probe::ProbeHalt::Unavailable {
                            crate::services::activity::warn(
                                "Azure could not serve run history",
                                format!("{}/{}", az.resource_group, az.app_name),
                                "Microsoft.Web returned a gateway/availability error (502-504). \
                                 This is an Azure-side fault, not a problem with this app or \
                                 your access — polling has slowed down and will recover on its \
                                 own once the service does."
                                    .to_string(),
                            );
                        }
                        if reason == chain_probe::ProbeHalt::Unauthorized {
                            crate::services::activity::error(
                                "Session expired reading workflow runs",
                                format!("{}/{}", az.resource_group, az.app_name),
                                "Azure refused 'hostruntime/.../workflows/runs/read' on this \
                                 Logic App with a token error — sign-in expired or was revoked. \
                                 Use the 'Sign in again' button on Home, or run `az login`."
                                    .to_string(),
                            );
                        }
                        if reason == chain_probe::ProbeHalt::MissingPermission {
                            crate::services::activity::error(
                                "Missing role reading workflow runs",
                                format!("{}/{}", az.resource_group, az.app_name),
                                "Azure refused 'hostruntime/.../workflows/runs/read' on this \
                                 Logic App with an RBAC denial — you're signed in, but that \
                                 account lacks the role needed here. Signing in again will not \
                                 fix this. Confirm with:\n  az role assignment list --assignee \
                                 <you> --scope <logic app id> --include-inherited\nReader is not \
                                 sufficient for hostruntime calls; Contributor is. Ask an Owner \
                                 or User Access Administrator for a role assignment."
                                    .to_string(),
                            );
                        }
                    }
                    // Decay rather than reset. Dropping straight back to the
                    // floor after a single clean sweep just walks into the
                    // same throttle again, giving a sawtooth that spends half
                    // its life throttled. Halving converges on the fastest
                    // rate the endpoint actually tolerates and stays there.
                    None => {
                        let prev = *poll_backoff_secs.peek();
                        if prev > 0 {
                            poll_backoff_secs.set(if prev <= base_interval { 0 } else { prev / 2 });
                        }
                    }
                }
                chain_polling.set(false);
            });
        }
    };

    // One long-lived driver loop, started once via `use_hook` rather than
    // `use_effect`: an effect reruns whenever a signal it touched changes,
    // which would spawn a second loop each time and stack duplicate polls.
    //
    // It wakes on a short tick and polls only when a sweep is actually due
    // *and* the user hasn't paused it. That way discovery finishing
    // mid-interval, or the user resuming, is picked up within seconds
    // instead of waiting out a full interval.
    use_hook({
        let mut poll_chains = poll_chains.clone();
        move || {
            spawn(async move {
                loop {
                    if *poll_enabled.peek() {
                        let last = *chain_poll_at.peek();
                        let interval = *poll_interval_secs.peek() + *poll_backoff_secs.peek();
                        let due = last == 0 || epoch_secs().saturating_sub(last) >= interval;
                        if due {
                            poll_chains();
                        }
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(POLL_TICK_SECS)).await;
                }
            });
        }
    });

    // Re-render on a timer so "elapsed" columns keep counting up between
    // polls — without this an in-flight run's age would freeze for two
    // minutes at a time. Also `use_hook`, for the same reason as above.
    let mut clock_tick: Signal<u64> = use_signal(|| 0);
    use_hook(move || {
        spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(15)).await;
                let n = *clock_tick.peek();
                clock_tick.set(n + 1);
            }
        });
    });
    // Subscribe the render to the tick so those timers actually repaint.
    let _ = *clock_tick.read();

    // ── Cached rollups (no Azure calls) ─────────────────────────────────
    let workspace_dir = workspace_dir(&az.subscription, &az.resource_group, &az.app_name);

    // Prefer live in-session chain health; fall back to the on-disk snapshot
    // so the wall display shows last-known numbers immediately on startup
    // rather than an empty page until the first poll lands.
    let live_health = props.chain_health.read().clone();
    let (chain_health, last_checked) = if live_health.is_empty() {
        let snap = health_cache::load(&workspace_dir);
        (snap.health, snap.last_checked)
    } else {
        (live_health, props.last_checked.read().clone())
    };
    let all_runs = props.chain_runs.read().clone();

    // Workflows still broken right now: the newest run that reached a
    // terminal state failed, and nothing has succeeded since. Anything that
    // failed and then recovered on its own is deliberately excluded.
    let cutoff = window_cutoff(*window_days.read());
    let failing: Vec<UnrecoveredFailure> = {
        let mut v = Vec::new();
        for (chain, workflows) in &all_runs {
            for (workflow, runs) in workflows {
                // Parse and order newest-first; skip runs with unparseable
                // timestamps rather than guessing their position.
                let mut dated: Vec<(DateTime<Utc>, &azure::RunInfo)> = runs
                    .iter()
                    .filter_map(|r| {
                        DateTime::parse_from_rfc3339(&r.start)
                            .ok()
                            .map(|dt| (dt.with_timezone(&Utc), r))
                    })
                    .collect();
                dated.sort_by(|a, b| b.0.cmp(&a.0));

                // Runs still in flight aren't evidence either way, so the
                // verdict comes from the most recent finished run.
                let terminal: Vec<&(DateTime<Utc>, &azure::RunInfo)> = dated
                    .iter()
                    .filter(|(_, r)| r.status == "Succeeded" || r.status == "Failed")
                    .collect();
                let Some((failed_at, newest)) = terminal.first().copied() else {
                    continue;
                };
                if newest.status != "Failed" {
                    continue;
                }
                if *failed_at < cutoff {
                    continue;
                }

                let consecutive = terminal
                    .iter()
                    .take_while(|(_, r)| r.status == "Failed")
                    .count();
                v.push(UnrecoveredFailure {
                    chain: chain.clone(),
                    workflow: workflow.clone(),
                    failed_at: *failed_at,
                    consecutive,
                    run_id: newest.id.clone(),
                });
            }
        }
        // Most recent breakage first — that's usually the one to look at.
        v.sort_by(|a, b| b.failed_at.cmp(&a.failed_at));
        v
    };
    // Count actual runs, not chain keys: a sweep whose `list_runs` calls all
    // failed still inserts an (empty) entry per chain, so testing the outer
    // map would report "have data" and make the card claim nothing is
    // failing when really nothing could be read.
    let fetched_runs: usize = all_runs
        .values()
        .flat_map(|workflows| workflows.values())
        .map(|runs| runs.len())
        .sum();
    let have_run_data = fetched_runs > 0;
    let chains_polled = all_runs.len();

    // Live activity: every run Azure still reports as Running. Ordered
    // oldest-first so a run that has been going far longer than the rest —
    // the likely stuck one — sits at the top rather than scrolling away.
    // Collapsed by run id: `all_runs` is keyed chain → workflow → runs, so a
    // workflow belonging to several chains yields the same Azure run once per
    // chain. Listed raw, one run reads as several concurrent runs with
    // identical start times — and the "Workflows running" count inflates to
    // match. The run id is the identity that actually distinguishes them.
    let live_runs: Vec<LiveRun> = {
        let mut by_run: HashMap<(String, String), LiveRun> = HashMap::new();
        for (chain, workflows) in &all_runs {
            for (workflow, runs) in workflows {
                for r in runs.iter().filter(|r| r.status == "Running") {
                    let Ok(dt) = DateTime::parse_from_rfc3339(&r.start) else {
                        continue;
                    };
                    by_run
                        .entry((workflow.clone(), r.id.clone()))
                        .or_insert_with(|| LiveRun {
                            chains: Vec::new(),
                            workflow: workflow.clone(),
                            started: dt.with_timezone(&Utc),
                        })
                        .chains
                        .push(chain.clone());
                }
            }
        }
        let mut v: Vec<LiveRun> = by_run.into_values().collect();
        for r in &mut v {
            r.chains.sort();
            r.chains.dedup();
        }
        // Oldest first — the run that has been going far longer than the rest
        // is the likely stuck one, so it sits at the top rather than scrolling
        // away. Ties broken by name so the order doesn't jitter between polls.
        v.sort_by(|a, b| {
            a.started
                .cmp(&b.started)
                .then_with(|| a.workflow.cmp(&b.workflow))
        });
        v
    };
    let live_chains: Vec<LiveChain> = {
        let mut by_chain: HashMap<
            String,
            (usize, std::collections::HashSet<String>, DateTime<Utc>),
        > = HashMap::new();
        // A shared workflow's run counts toward every chain it belongs to —
        // this is the per-chain view, where the run really is in flight for
        // each of them. Only the flat run list above is deduplicated.
        for r in &live_runs {
            for chain in &r.chains {
                let entry = by_chain
                    .entry(chain.clone())
                    .or_insert_with(|| (0, std::collections::HashSet::new(), r.started));
                entry.0 += 1;
                entry.1.insert(r.workflow.clone());
                if r.started < entry.2 {
                    entry.2 = r.started;
                }
            }
        }
        let mut v: Vec<LiveChain> = by_chain
            .into_iter()
            .map(|(chain, (runs, workflows, oldest))| LiveChain {
                chain,
                runs,
                workflows: workflows.len(),
                oldest,
            })
            .collect();
        v.sort_by_key(|a| a.oldest);
        v
    };
    // Dead letters belong to queues, not chains — listing them per queue is
    // what makes the row actionable, since that's the thing you go purge or
    // requeue. The chain is carried alongside only as context.
    // (queue, chain, count)
    let queue_statuses = props.chain_queue_statuses.read().clone();
    let dlq: Vec<(String, String, i64)> = {
        let mut v: Vec<(String, String, i64)> = queue_statuses
            .iter()
            .flat_map(|(chain, queues)| {
                queues
                    .iter()
                    .filter(|(_, s)| s.dead_letter > 0)
                    .map(move |(queue, s)| (queue.clone(), chain.clone(), s.dead_letter))
            })
            .collect();
        v.sort_by(|a, b| b.2.cmp(&a.2));
        v
    };
    // Prefer the per-queue sum; fall back to the aggregated chain figure
    // when queue counts haven't been collected yet (no SB namespace, or the
    // first poll hasn't finished), so the tile isn't wrongly zero.
    let total_dlq: i64 = if queue_statuses.is_empty() {
        chain_health.values().map(|h| h.dead_letters).sum()
    } else {
        dlq.iter().map(|(_, _, n)| n).sum()
    };
    let stuck_total: usize = chain_health.values().map(|h| h.stuck_count).sum();

    // Success-rate regression vs. each chain's own recent average.
    let history = history_cache::load(&workspace_dir);
    let regressions: Vec<(String, f64, f64)> = history
        .chains
        .iter()
        .filter_map(|(name, points)| {
            let rates: Vec<f64> = points.iter().filter_map(|p| p.success_rate).collect();
            if rates.len() < 3 {
                return None;
            }
            let latest = *rates.last()?;
            let prior = &rates[..rates.len() - 1];
            let avg = prior.iter().sum::<f64>() / prior.len() as f64;
            if avg - latest >= REGRESSION_DROP_PCT {
                Some((name.clone(), latest, avg))
            } else {
                None
            }
        })
        .collect();
    // Timestamp of the newest history point across all chains, so the
    // regression card can say what moment it is describing.
    let history_at = history
        .chains
        .values()
        .filter_map(|points| points.last().map(|p| p.ts))
        .max()
        .unwrap_or(0);

    // Function errors from the Functions tab's cached metrics.
    let fn_snap = functions_cache::load_for(&az.subscription, &az.resource_group, &az.app_name);
    let fn_errors: i64 = fn_snap
        .metrics
        .iter()
        .flat_map(|(_, m)| m.iter())
        .map(|m| m.errors)
        .sum();
    let fn_metrics_at = fn_snap.last_fetched;

    // Resource health falls back to its own cache if the live call hasn't
    // returned yet, so the tile isn't blank on first paint.
    let res_summary = *res_unhealthy.read();
    let res_cached =
        resource_health_cache::load_for(&az.subscription, &az.resource_group, &az.app_name);
    let (res_bad, res_total, res_at) = match res_summary {
        Some((bad, total)) => (Some(bad), total, *res_fetched_at.read()),
        None if !res_cached.rows.is_empty() => {
            let bad = res_cached
                .rows
                .iter()
                .filter(|r| {
                    !matches!(
                        r.state.as_str(),
                        "Running" | "Succeeded" | "Active" | "Enabled"
                    ) || r.health == "Degraded"
                        || r.health == "Unavailable"
                })
                .count();
            (Some(bad), res_cached.rows.len(), res_cached.last_fetched)
        }
        None => (None, 0, 0),
    };

    let chains_checked_at = last_checked.values().max().copied().unwrap_or(0);
    let is_loading = *loading.read();
    let drift_val = *drift_count.read();
    let rbac_val = *rbac_gaps.read();
    let cost_val = cost.read().clone();

    // User-set display names (Chains tab rename) are an overlay keyed by the
    // chain's stable `label` — every card below is keyed/computed off that
    // raw label, so a rename is applied only here, at render time.
    let chain_names_map = props.chain_names.read().clone();
    let disp_chain = |label: &str| -> String {
        chain_names_map
            .get(label)
            .cloned()
            .unwrap_or_else(|| label.to_string())
    };

    rsx! {
        // `home-panel` scopes the fluid type/spacing tokens — see main.css.
        div { class: "func-panel home-panel",
            div { class: "func-header",
                h2 { "Overview" }
                // Every number on this page is a snapshot from a different
                // moment, so state plainly when each source was collected
                // rather than implying it's all live.
                span { class: "home-asof",
                    if *chain_polling.read() {
                        span { class: "home-live-dot" }
                        "polling chains… · "
                    } else if !*poll_enabled.read() {
                        "chain polling paused · "
                    } else {
                        {
                            let last = *sweep_secs.read();
                            let backoff = *poll_backoff_secs.read();
                            let base = *poll_interval_secs.read();
                            // Show the sweep cost next to the interval: when
                            // it approaches the interval the effective refresh
                            // rate is the sweep, not the configured interval.
                            let sweep_suffix = if last > 0.0 { format!(" (sweep {last:.0}s)") } else { String::new() };
                            let interval = base + backoff;
                            rsx! { "auto every {interval}s{sweep_suffix} · " }
                        }
                    }
                    "chains {format_dt(chains_checked_at)} · checks {format_dt(*res_fetched_at.read())} · functions {format_dt(fn_metrics_at)}"
                    if *poll_errors.read() > 0 {
                        span {
                            class: "func-errors has-errors",
                            title: "Some workflows or queues failed to read on the last sweep — full detail is in the Activity log.",
                            " · {poll_errors} read error(s)"
                        }
                    }
                    if *poll_backoff_secs.read() > 0 {
                        span {
                            class: "func-errors has-errors",
                            title: "Azure is throttling or resetting connections, so polling has backed off to reduce load.",
                            " · backing off ({poll_backoff_secs}s)"
                        }
                    }
                }
                // Interval picker — the requested rate, before any throttle
                // backoff is layered on top of it.
                {
                    let calls = calls_per_sweep(&props.chains.read());
                    let rec = recommended_interval(calls);
                    let hint = format!(
                        "Chain-poll interval. A sweep costs {calls} Azure call(s); \
                         {rec}s keeps that under {TARGET_CALLS_PER_SEC}/second. \
                         Faster settings risk throttling the whole subscription."
                    );
                    rsx! {
                        div { class: "home-window-picker", title: "{hint}",
                            for (secs, label) in POLL_INTERVAL_CHOICES {
                                button {
                                    key: "{secs}",
                                    class: if *poll_interval_secs.read() == secs { "btn btn-small btn-primary" } else { "btn btn-small" },
                                    onclick: move |_| {
                                        interval_user_set.set(true);
                                        poll_interval_secs.set(secs);
                                    },
                                    "{label}"
                                }
                            }
                        }
                    }
                }
                button {
                    class: "icon-refresh-btn",
                    title: if *poll_enabled.read() { "Pause background polling" } else { "Resume background polling" },
                    onclick: move |_| { let next = !*poll_enabled.read(); poll_enabled.set(next); },
                    if *poll_enabled.read() { "⏸" } else { "▶" }
                }
                button {
                    class: "icon-refresh-btn",
                    title: "Refresh now",
                    disabled: is_loading || *chain_polling.read(),
                    onclick: move |_| { load(); poll_chains(); },
                    span { class: if is_loading || *chain_polling.read() { "icon-spin" } else { "" }, "⟳" }
                }
            }
            // Says *why* rather than just how many: on a new tenant an empty
            // card is almost always throttling, permissions, or a naming
            // mismatch, and a bare count can't distinguish those. Kept to one
            // line — the full ARM payload goes to the Activity log, which is
            // built for reading it.
            {
                let halt = *poll_halt.read();
                let samples = poll_error_samples.read().clone();
                if let Some(reason) = halt {
                    let is_unauthorized = matches!(reason, chain_probe::ProbeHalt::Unauthorized);
                    let is_missing_permission = matches!(reason, chain_probe::ProbeHalt::MissingPermission);
                    rsx! {
                        div { class: "az-error home-poll-errors",
                            div { class: "home-poll-error-line", "{halt_headline(reason)}" }
                            div { class: "home-poll-error-line home-poll-error-hint",
                                "Full detail is in the Activity log."
                            }
                            if is_unauthorized {
                                div { style: "margin-top:8px;",
                                    button {
                                        class: "btn btn-small btn-primary",
                                        onclick: {
                                            let tenant = az.tenant.clone();
                                            let load = load.clone();
                                            let poll_chains = poll_chains.clone();
                                            move |_| {
                                                // Cloned per click: the callback takes ownership,
                                                // which would otherwise make this fire only once.
                                                let mut load = load.clone();
                                                let mut poll_chains = poll_chains.clone();
                                                crate::hooks::signin::sign_in_and_wait(
                                                    &tenant,
                                                    signing_in,
                                                    move |_| {
                                                        poll_halt.set(None);
                                                        poll_error_samples.set(Vec::new());
                                                        load();
                                                        poll_chains();
                                                    },
                                                );
                                            }
                                        },
                                        "Sign in again"
                                    }
                                }
                            }
                            if is_missing_permission {
                                // Re-signing in cannot grant a role, so there is
                                // nothing to click — only what to check and who
                                // to ask.
                                div { class: "home-poll-error-line home-poll-error-hint", style: "margin-top:4px;",
                                    "Check with: "
                                    code { style: "font-family:monospace; background:var(--bg2); padding:1px 5px; border-radius:3px;",
                                        "az role assignment list --assignee <you> --scope <logic app id> --include-inherited"
                                    }
                                    " — Reader is not enough for hostruntime calls; Contributor is."
                                }
                            }
                        }
                    }
                } else if !samples.is_empty() {
                    rsx! {
                        div { class: "az-error home-poll-errors",
                            for e in samples.iter().take(3) {
                                div { class: "home-poll-error-line", "{summarize_error(e)}" }
                            }
                            if samples.len() > 3 {
                                div { class: "home-poll-error-line home-poll-error-hint",
                                    "…and {samples.len() - 3} more — see the Activity log."
                                }
                            }
                        }
                    }
                } else {
                    rsx! {}
                }
            }
            if let Some(e) = error_msg.read().clone() {
                div { class: "az-error", "{e}" }
            }

            // ── What's actually wrong, in detail ────────────────────────
            // Detail first, tiles below: the lists are what you act on, the
            // tiles are the summary you glance at. Laid out side-by-side so
            // all three fit above the fold on a normal window.
            div { class: "home-grid",
                div { class: "func-app-card home-card",
                    div { class: "func-app-header",
                        h3 { "Still failing" }
                        span {
                            class: "func-app-count",
                            title: "Workflows whose most recent finished run failed, with no successful run since. Recovered failures are excluded.",
                            "no success since"
                        }
                        div { class: "home-window-picker",
                            for (days, label) in [(1i64, "Today"), (3, "3d"), (7, "7d")] {
                                button {
                                    key: "{days}",
                                    class: if *window_days.read() == days { "btn btn-small btn-primary" } else { "btn btn-small" },
                                    onclick: move |_| window_days.set(days),
                                    "{label}"
                                }
                            }
                        }
                    }
                    if !have_run_data {
                        // Distinguish "not polled yet" from "polled and got
                        // nothing back" — they look identical on screen but
                        // mean completely different things, and only the
                        // second one is a problem to chase.
                        div { class: "func-empty-small",
                            if *chain_polling.read() {
                                "Loading run history…"
                            } else if *poll_errors.read() > 0 {
                                "Could not read run history — the queue counts below came back fine, so this is specific to the workflow-runs API. Check the errors listed under the header."
                            } else if chains_polled == 0 {
                                "No chains discovered yet."
                            } else {
                                "Azure returned no runs for any of this profile's {chains_polled} chain(s). Either the workflows have never run, or the Logic App name in this profile doesn't match the one deployed in this tenant."
                            }
                        }
                    } else if failing.is_empty() {
                        div { class: "func-empty-small",
                            "Nothing unrecovered in this window — {fetched_runs} run(s) checked across {chains_polled} chain(s)."
                        }
                    } else {
                        div { class: card_body_class(&expanded_cards.read(), "failing"),
                        table { class: "func-table home-table",
                            thead { tr { th { "Workflow" } th { "Failed at" } th { "×" } } }
                            tbody {
                                for f in failing.iter() {
                                    {
                                        let is_open = open_log.read().as_ref()
                                            .map(|(w, r)| w == &f.workflow && r == &f.run_id)
                                            .unwrap_or(false);
                                        let wf = f.workflow.clone();
                                        let rid = f.run_id.clone();
                                        let mut toggle_log = toggle_log.clone();
                                        rsx! {
                                            tr {
                                                class: if is_open { "func-row home-row-clickable home-row-selected" } else { "func-row home-row-clickable" },
                                                title: "Show this run's action log",
                                                onclick: move |_| toggle_log(wf.clone(), rid.clone()),
                                                td { class: "func-name",
                                                    span { class: "home-caret", if is_open { "▾" } else { "▸" } }
                                                    div { class: "home-wf-cell",
                                                        div { class: "home-wf-line",
                                                            span { class: "home-wf-name", "{f.workflow}" }
                                                            // Portal link — stop_propagation so
                                                            // clicking the icon doesn't toggle the row.
                                                            {
                                                                let loc = props.discovered_location.read().clone();
                                                                let url = crate::services::portal_links::workflow(
                                                                    &az.tenant, &az.subscription, &az.resource_group,
                                                                    &az.app_name, &f.workflow, loc.as_deref(),
                                                                );
                                                                rsx! {
                                                                    button {
                                                                        class: "portal-link",
                                                                        title: "Open this workflow in the Azure Portal",
                                                                        onclick: move |e: Event<MouseData>| {
                                                                            e.stop_propagation();
                                                                            crate::services::portal_links::open_in_browser(&url);
                                                                        },
                                                                        "🔗"
                                                                    }
                                                                }
                                                            }
                                                        }
                                                        span { class: "home-wf-chain", "{disp_chain(&f.chain)}" }
                                                    }
                                                }
                                                td {
                                                    span { class: "func-errors has-errors", "{format_dt_utc(f.failed_at)}" }
                                                    span { class: "home-rel", " {format_ago_utc(f.failed_at)}" }
                                                }
                                                td { title: "{f.consecutive} consecutive failure(s)", "{f.consecutive}" }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        }
                        ShowMore { cards: expanded_cards, id: "failing", total: failing.len() }
                    }
                }

                // ── Live activity ───────────────────────────────────────
                div { class: "func-app-card home-card",
                    div { class: "func-app-header",
                        h3 { "Workflows running" }
                        span { class: "func-app-count", "as of {format_dt(chains_checked_at)}" }
                    }
                    if !have_run_data {
                        div { class: "func-empty-small", "No run data loaded yet." }
                    } else if live_runs.is_empty() {
                        div { class: "func-empty-small", "No workflows running right now." }
                    } else {
                        div { class: card_body_class(&expanded_cards.read(), "live_runs"),
                        table { class: "func-table home-table",
                            thead { tr { th { "Workflow" } th { "Started" } th { "Elapsed" } } }
                            tbody {
                                for r in live_runs.iter() {
                                    tr { class: "func-row",
                                        td { class: "func-name",
                                            span { class: "home-live-dot" }
                                            div { class: "home-wf-cell",
                                                div { class: "home-wf-line",
                                                    span { class: "home-wf-name", "{r.workflow}" }
                                                    {
                                                        let loc = props.discovered_location.read().clone();
                                                        let url = crate::services::portal_links::workflow(
                                                            &az.tenant, &az.subscription, &az.resource_group,
                                                            &az.app_name, &r.workflow, loc.as_deref(),
                                                        );
                                                        rsx! {
                                                            button {
                                                                class: "portal-link",
                                                                title: "Open this workflow in the Azure Portal",
                                                                onclick: move |_| crate::services::portal_links::open_in_browser(&url),
                                                                "🔗"
                                                            }
                                                        }
                                                    }
                                                }
                                                {
                                                    // One row per run now, so name every chain the
                                                    // run belongs to instead of implying it is only
                                                    // in the first one.
                                                    let first = r.chains.first().map(|c| disp_chain(c)).unwrap_or_default();
                                                    let label = match r.chains.len() {
                                                        0 | 1 => first,
                                                        n => format!("{first} +{}", n - 1),
                                                    };
                                                    let full = r.chains.iter()
                                                        .map(|c| disp_chain(c))
                                                        .collect::<Vec<_>>()
                                                        .join(", ");
                                                    rsx! {
                                                        span { class: "home-wf-chain", title: "{full}", "{label}" }
                                                    }
                                                }
                                            }
                                        }
                                        td { "{format_dt_utc(r.started)}" }
                                        td { "{format_elapsed(r.started)}" }
                                    }
                                }
                            }
                        }
                        }
                        ShowMore { cards: expanded_cards, id: "live_runs", total: live_runs.len() }
                    }
                }

                if !dlq.is_empty() {
                    div { class: "func-app-card home-card",
                        div { class: "func-app-header",
                            h3 { "Dead-letter backlog" }
                            span { class: "func-app-count", "as of {format_dt(chains_checked_at)}" }
                        }
                        div { class: card_body_class(&expanded_cards.read(), "dlq"),
                            table { class: "func-table home-table",
                                thead { tr { th { "Queue" } th { "Messages" } } }
                                tbody {
                                    for (queue, chain, count) in dlq.iter() {
                                        tr { class: "func-row",
                                            td { class: "func-name", title: "{queue}",
                                                div { class: "home-wf-cell",
                                                    span { class: "home-wf-name", "{queue}" }
                                                    span { class: "home-wf-chain", "{disp_chain(chain)}" }
                                                }
                                            }
                                            td { span { class: "func-errors has-errors", "{count}" } }
                                        }
                                    }
                                }
                            }
                        }
                        ShowMore { cards: expanded_cards, id: "dlq", total: dlq.len() }
                    }
                }

                if !regressions.is_empty() {
                    div { class: "func-app-card home-card",
                        div { class: "func-app-header",
                            h3 { "Success-rate regressions" }
                            span { class: "func-app-count", title: "Chains whose latest success rate is more than {REGRESSION_DROP_PCT:.0} points below their own recent average.", "as of {format_dt(history_at)}" }
                        }
                        div { class: card_body_class(&expanded_cards.read(), "regressions"),
                            table { class: "func-table home-table",
                                thead { tr { th { "Chain" } th { "Now" } th { "Avg" } } }
                                tbody {
                                    for (name, latest, avg) in regressions.iter() {
                                        tr { class: "func-row",
                                            td { class: "func-name", "{disp_chain(name)}" }
                                            td { span { class: "func-errors has-errors", "{latest:.0}%" } }
                                            td { "{avg:.0}%" }
                                        }
                                    }
                                }
                            }
                        }
                        ShowMore { cards: expanded_cards, id: "regressions", total: regressions.len() }
                    }
                }
            }

            // ── Summary tiles ───────────────────────────────────────────
            div { class: "home-section-label", "Indicators" }
            div { class: "home-tiles",
                StatTile {
                    label: "Still failing".to_string(),
                    value: failing.len().to_string(),
                    bad: !failing.is_empty(),
                    sub: format!("{} · {}", window_label(*window_days.read()), format_age(chains_checked_at)),
                }
                StatTile {
                    label: "Running now".to_string(),
                    value: live_runs.len().to_string(),
                    bad: false,
                    sub: format!("{} chain(s)", live_chains.len()),
                }
                StatTile {
                    label: "Dead letters".to_string(),
                    value: total_dlq.to_string(),
                    bad: total_dlq > 0,
                    sub: format!("{} queue(s)", dlq.len()),
                }
                StatTile {
                    label: "Unhealthy res.".to_string(),
                    value: res_bad.map(|b| b.to_string()).unwrap_or_else(|| "—".into()),
                    bad: res_bad.unwrap_or(0) > 0,
                    sub: if res_total > 0 { format!("of {res_total} · {}", format_age(res_at)) } else { "loading…".to_string() },
                }
                StatTile {
                    label: "Function errors".to_string(),
                    value: fn_errors.to_string(),
                    bad: fn_errors > 0,
                    sub: if fn_metrics_at > 0 { format_age(fn_metrics_at) } else { "see Functions tab".to_string() },
                }
                StatTile {
                    label: "Config drift".to_string(),
                    value: drift_val.map(|d| d.to_string()).unwrap_or_else(|| "—".into()),
                    bad: drift_val.unwrap_or(0) > 0,
                    sub: if has_store { "vs App Config".to_string() } else { "no store set".to_string() },
                }
                StatTile {
                    label: "RBAC gaps".to_string(),
                    value: rbac_val.map(|g| g.to_string()).unwrap_or_else(|| "—".into()),
                    bad: rbac_val.unwrap_or(0) > 0,
                    sub: "identities, no roles".to_string(),
                }
                StatTile {
                    label: "Stuck runs".to_string(),
                    value: stuck_total.to_string(),
                    bad: stuck_total > 0,
                    sub: "all chains".to_string(),
                }
                StatTile {
                    label: "Cost MTD".to_string(),
                    value: cost_val.as_ref().map(|(t, c)| format!("{t:.0} {c}")).unwrap_or_else(|| "—".into()),
                    bad: false,
                    sub: "this res. group".to_string(),
                }
            }

            // ── Action log for the selected failed run ──────────────────
            // Anchored at the bottom of the page rather than expanding
            // inline, so opening it never pushes the tables and tiles
            // around — important on an unattended display.
            if let Some((wf, run_id)) = open_log.read().clone() {
                {
                    let actions = log_actions.read().clone();
                    let failed: Vec<&azure::ActionInfo> = actions.iter()
                        .filter(|a| a.status == "Failed")
                        .collect();
                    rsx! {
                        div { class: "func-app-card home-card home-console",
                            div { class: "func-app-header",
                                h3 { "Run log — {wf}" }
                                span { class: "func-app-count", title: "{run_id}", "run {short_id(&run_id)}" }
                                button {
                                    class: "btn btn-small",
                                    style: "margin-left:auto;",
                                    onclick: move |_| {
                                        open_log.set(None);
                                        log_actions.set(Vec::new());
                                        log_error.set(None);
                                    },
                                    "Close"
                                }
                            }
                            if *log_loading.read() {
                                div { class: "func-loading", "Loading run actions…" }
                            } else if let Some(e) = log_error.read().clone() {
                                div { class: "az-error", "{e}" }
                            } else if actions.is_empty() {
                                div { class: "func-empty-small", "No actions reported for this run." }
                            } else {
                                // Failed actions carry the actual cause, so
                                // they lead; the full sequence follows for
                                // context on where it broke.
                                if !failed.is_empty() {
                                    div { class: "home-section-label", "Failed actions" }
                                    for a in failed.iter() {
                                        div { class: "home-console-err",
                                            div { class: "home-console-name", "{a.name}" }
                                            pre { class: "home-console-body",
                                                { a.error.clone().unwrap_or_else(|| "(no error message returned)".into()) }
                                            }
                                        }
                                    }
                                }
                                div { class: "home-section-label", "All actions" }
                                table { class: "func-table home-table",
                                    thead { tr { th { "Action" } th { "Status" } th { "Detail" } } }
                                    tbody {
                                        for a in actions.iter() {
                                            tr { class: "func-row",
                                                td { class: "func-name", "{a.name}" }
                                                td {
                                                    span {
                                                        class: match a.status.as_str() {
                                                            "Failed" => "func-errors has-errors",
                                                            "Succeeded" => "func-badge-active",
                                                            _ => "func-no-data",
                                                        },
                                                        "{a.status}"
                                                    }
                                                }
                                                td { class: "home-console-detail",
                                                    { a.error.clone().unwrap_or_else(|| "—".into()) }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Logic Apps run IDs are long; show enough of the tail to disambiguate.
fn short_id(s: &str) -> String {
    if s.len() <= 14 {
        s.to_string()
    } else {
        format!("…{}", &s[s.len() - 13..])
    }
}

#[derive(Props, Clone, PartialEq)]
struct StatTileProps {
    label: String,
    value: String,
    bad: bool,
    sub: String,
}

#[component]
fn StatTile(props: StatTileProps) -> Element {
    rsx! {
        div { class: if props.bad { "home-tile home-tile-bad" } else { "home-tile" },
            div { class: "home-tile-label", "{props.label}" }
            div { class: "home-tile-value", "{props.value}" }
            div { class: "home-tile-sub", "{props.sub}" }
        }
    }
}

/// `az` calls one sweep costs: each distinct workflow and each distinct queue
/// is read once, regardless of how many chains reference them.
fn calls_per_sweep(chains: &[chain::ChainDetail]) -> usize {
    let mut wf: Vec<&str> = chains
        .iter()
        .flat_map(|c| c.steps.iter().map(|s| s.workflow.as_str()))
        .collect();
    wf.sort_unstable();
    wf.dedup();
    let mut q: Vec<&str> = chains
        .iter()
        .flat_map(|c| c.queues.iter().map(|s| s.as_str()))
        .collect();
    q.sort_unstable();
    q.dedup();
    wf.len() + q.len()
}

/// Sustained request rate a sweep is allowed to average, in calls/second.
///
/// The hostruntime endpoint throttles per *subscription* (429 / 51020) on
/// burst as well as rate, and that budget is shared with every other tool and
/// person using the subscription — so this app should be a modest tenant of
/// it, not the whole thing. Two per second leaves headroom.
const TARGET_CALLS_PER_SEC: usize = 2;

/// Smallest offered interval that keeps a sweep of this size under
/// [`TARGET_CALLS_PER_SEC`].
///
/// A fixed default cannot be right for both a 3-workflow app and a
/// 38-chain one: ten seconds is fine for the first and roughly 12 calls/second
/// for the second. Sizing the default to the environment is what stops the
/// poll from provoking the throttle it then has to back off from.
fn recommended_interval(calls: usize) -> u64 {
    let needed = calls.div_ceil(TARGET_CALLS_PER_SEC) as u64;
    POLL_INTERVAL_CHOICES
        .iter()
        .map(|(secs, _)| *secs)
        .find(|secs| *secs >= needed)
        .unwrap_or_else(|| {
            POLL_INTERVAL_CHOICES
                .last()
                .map(|(s, _)| *s)
                .unwrap_or(POLL_SECS)
        })
}

/// Offered poll intervals, ascending — also the ladder `recommended_interval`
/// snaps to, so the auto-picked value is always one the user can see selected.
const POLL_INTERVAL_CHOICES: [(u64, &str); 4] = [(10, "10s"), (30, "30s"), (60, "1m"), (300, "5m")];

/// Class for a grid card's scrollable body. Height is fixed either way — the
/// expanded state only swaps clipping for a scrollbar, so opening a card never
/// reflows the cards beside it or shifts the page below.
fn card_body_class(expanded: &std::collections::HashSet<&'static str>, id: &str) -> &'static str {
    if expanded.contains(id) {
        "home-card-body expanded"
    } else {
        "home-card-body"
    }
}

/// "Show more / Show less" toggle, rendered only when a card actually has more
/// rows than it can display. Reports the hidden count so the button says how
/// much is out of sight rather than just that something is.
#[component]
fn ShowMore(
    cards: Signal<std::collections::HashSet<&'static str>>,
    id: &'static str,
    total: usize,
) -> Element {
    if total <= HOME_CARD_ROWS {
        return rsx! {};
    }
    let mut cards = cards;
    let open = cards.read().contains(id);
    rsx! {
        button {
            class: "btn btn-small home-show-more",
            onclick: move |_| {
                let mut set = cards.write();
                if !set.remove(id) { set.insert(id); }
            },
            if open {
                "Show less"
            } else {
                "Show all {total}"
            }
        }
    }
}

/// Headline for a sweep that stopped early — what happened and what, if
/// anything, the reader should do. The underlying ARM payload is a page of
/// JSON; it belongs in the Activity log, not across the top of a dashboard.
fn halt_headline(reason: chain_probe::ProbeHalt) -> &'static str {
    match reason {
        chain_probe::ProbeHalt::Throttled =>
            "Azure is throttling this subscription — polling has slowed down and will speed back up on its own.",
        chain_probe::ProbeHalt::Unavailable =>
            "Azure could not serve run history (gateway error). An Azure-side fault, not a problem with this app or your access.",
        chain_probe::ProbeHalt::Unauthorized =>
            "Azure refused authorization reading run history — your sign-in expired. Try `az login`.",
        chain_probe::ProbeHalt::MissingPermission =>
            "Azure refused authorization reading run history — this account lacks the role needed on this Logic App. Signing in again will not fix this.",
    }
}

/// One-line, human-sized rendering of an `az` error, for failures that have no
/// classification. Keeps the first line and clips it: the raw text is often a
/// full Python traceback, and printing it whole hides the dashboard behind it.
fn summarize_error(raw: &str) -> String {
    const MAX: usize = 160;
    let first = raw.lines().next().unwrap_or(raw).trim();
    if first.chars().count() <= MAX {
        first.to_string()
    } else {
        format!("{}…", first.chars().take(MAX).collect::<String>())
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

/// Start of the lookback window. `1` means "today" in the user's own
/// timezone (since local midnight) rather than a rolling 24h, since that's
/// what people mean when they ask what broke today.
fn window_cutoff(days: i64) -> DateTime<Utc> {
    if days <= 1 {
        let midnight = Local::now().date_naive().and_hms_opt(0, 0, 0);
        if let Some(m) = midnight {
            if let Some(local) = Local.from_local_datetime(&m).single() {
                return local.with_timezone(&Utc);
            }
        }
    }
    Utc::now() - Duration::days(days.max(1))
}

fn window_label(days: i64) -> String {
    if days <= 1 {
        "today".into()
    } else {
        format!("last {days}d")
    }
}

/// Absolute local timestamp for a UTC instant, e.g. "11 Aug 09:42".
/// Same-day values drop the date since the time alone is unambiguous.
fn format_dt_utc(dt: DateTime<Utc>) -> String {
    let local = dt.with_timezone(&Local);
    if local.date_naive() == Local::now().date_naive() {
        local.format("%H:%M").to_string()
    } else {
        local.format("%d %b %H:%M").to_string()
    }
}

/// How long an in-flight run has been going, as a compact duration.
fn format_elapsed(started: DateTime<Utc>) -> String {
    let secs = (Utc::now() - started).num_seconds().max(0);
    if secs < 60 {
        return format!("{secs}s");
    }
    if secs < 3600 {
        return format!("{}m {}s", secs / 60, secs % 60);
    }
    format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
}

fn format_ago_utc(dt: DateTime<Utc>) -> String {
    let mins = (Utc::now() - dt).num_minutes();
    if mins < 1 {
        return "(just now)".into();
    }
    if mins < 60 {
        return format!("({mins}m)");
    }
    let hours = mins / 60;
    if hours < 24 {
        return format!("({hours}h)");
    }
    format!("({}d)", hours / 24)
}

/// Absolute local timestamp for an epoch-seconds value.
fn format_dt(ts: u64) -> String {
    if ts == 0 {
        return "never".into();
    }
    match Utc.timestamp_opt(ts as i64, 0).single() {
        Some(dt) => format_dt_utc(dt),
        None => "never".into(),
    }
}

fn format_age(ts: u64) -> String {
    if ts == 0 {
        return "never".into();
    }
    let age = epoch_secs().saturating_sub(ts);
    if age < 60 {
        format!("{age}s ago")
    } else if age < 3600 {
        format!("{}m ago", age / 60)
    } else if age < 86400 {
        format!("{}h ago", age / 3600)
    } else {
        format!("{}d ago", age / 86400)
    }
}

fn epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interval_scales_to_environment_size() {
        // Real sizes from this user's tenants (distinct workflows + queues).
        // Both were previously polled at 10s — ~12 calls/second sustained.
        assert_eq!(recommended_interval(118), 60, "ipaas-dev-chn-002");
        assert_eq!(recommended_interval(106), 60, "tom-dev-chn-001");
        // A tiny app should still poll briskly.
        assert_eq!(recommended_interval(3), 10);
    }

    #[test]
    fn interval_never_exceeds_the_offered_ladder() {
        // Enormous environments clamp to the slowest offered choice rather
        // than inventing a value the picker cannot display as selected.
        let slowest = POLL_INTERVAL_CHOICES.last().unwrap().0;
        assert_eq!(recommended_interval(100_000), slowest);
        assert!(POLL_INTERVAL_CHOICES
            .iter()
            .any(|(s, _)| *s == recommended_interval(118)));
    }

    #[test]
    fn summarize_keeps_short_errors_intact() {
        assert_eq!(summarize_error("list_runs X: boom"), "list_runs X: boom");
    }

    #[test]
    fn summarize_clips_arm_json_to_one_line() {
        // The real payload: a single enormous line of ARM JSON.
        let raw = format!(
            "Too Many Requests({{\"Code\":\"429\",\"Message\":\"{}\"}})",
            "x".repeat(4000)
        );
        let out = summarize_error(&raw);
        assert!(
            out.chars().count() <= 161,
            "got {} chars",
            out.chars().count()
        );
        assert!(out.ends_with('\u{2026}'));
    }

    #[test]
    fn summarize_drops_traceback_tail() {
        // az reports connection failures as a multi-line Python traceback;
        // only the first line is meaningful at a glance.
        let raw = "ERROR: ('Connection aborted.', ConnectionResetError(54))\nTraceback (most recent call last):\n  File \"x.py\", line 1";
        assert_eq!(
            summarize_error(raw),
            "ERROR: ('Connection aborted.', ConnectionResetError(54))"
        );
    }
}
