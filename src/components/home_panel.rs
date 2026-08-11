use dioxus::prelude::*;
use std::collections::HashMap;
use chrono::{DateTime, Duration, Local, TimeZone, Utc};
use crate::services::{azure, chain, chain_probe, health_cache, history_cache, functions_cache, resource_health_cache};
use crate::components::chain_detail::{AzConfig, ChainHealth, QueueStatus};

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
    chain: String,
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

/// How often the background poll re-checks every chain. This app is meant
/// to sit on a wall display all day, so the numbers have to stay current
/// with nobody clicking anything — but each cycle costs one `az` call per
/// workflow plus one per queue, so the interval is deliberately unhurried.
const POLL_SECS: u64 = 10;

/// How often the driver loop wakes to check whether a sweep is due. Short
/// so chain discovery finishing mid-interval is noticed promptly; the tick
/// itself is a no-op when nothing is due.
const POLL_TICK_SECS: u64 = 2;

/// How many chains to probe concurrently. Each chain costs one `az` process
/// per workflow plus one per queue, and every `az` invocation carries CLI
/// start-up overhead — probed one at a time, a sweep of a real environment
/// takes far longer than `POLL_SECS`, which would collapse the interval into
/// continuous back-to-back sweeps. A modest fan-out keeps a sweep inside its
/// window without flooding the Azure API.
const POLL_CONCURRENCY: usize = 4;

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

    let mut load = {
        let az = az.clone();
        move || {
            let az = az.clone();
            loading.set(true);
            error_msg.set(None);
            spawn(async move {
                let sub = az.subscription.clone();
                let rg = az.resource_group.clone();
                let store = az.app_config_store.trim().to_string();

                // Resource health
                let sub_r = sub.clone();
                let rg_r = rg.clone();
                if let Ok(Ok(rows)) = tokio::task::spawn_blocking(move || azure::list_resource_health(&sub_r, &rg_r)).await {
                    let total = rows.len();
                    let bad = rows.iter().filter(|r| {
                        !matches!(r.state.as_str(), "Running" | "Succeeded" | "Active" | "Enabled")
                            || r.health == "Degraded" || r.health == "Unavailable"
                    }).count();
                    res_unhealthy.set(Some((bad, total)));
                    res_fetched_at.set(epoch_secs());
                }

                // Function apps — shared by the drift and RBAC rollups.
                let sub_a = sub.clone();
                let rg_a = rg.clone();
                let apps = tokio::task::spawn_blocking(move || azure::list_function_apps(&sub_a, &rg_a))
                    .await.unwrap_or(Ok(Vec::new())).unwrap_or_default();

                // Config drift across all function apps
                let expected = if store.is_empty() {
                    None
                } else {
                    let sub_c = sub.clone();
                    let store2 = store.clone();
                    tokio::task::spawn_blocking(move || azure::appconfig_list_kv(&sub_c, &store2)).await
                        .ok().and_then(|r| r.ok())
                };
                let mut drift = 0usize;
                for app in &apps {
                    let sub_d = sub.clone();
                    let rg_d = rg.clone();
                    let name = app.name.clone();
                    let expected2 = expected.clone();
                    let rows = tokio::task::spawn_blocking(move || {
                        let live = azure::get_app_settings(&sub_d, &rg_d, &name).unwrap_or_default();
                        azure::compute_app_settings_drift(&live, expected2.as_ref())
                    }).await.unwrap_or_default();
                    drift += rows.iter().filter(|r| matches!(
                        r.status,
                        azure::DriftStatus::Diff
                            | azure::DriftStatus::LiteralWarn { .. }
                            | azure::DriftStatus::KvFail { .. }
                            | azure::DriftStatus::MissingLive
                    )).count();
                }
                drift_count.set(Some(drift));

                // RBAC gaps — identities with no role assignments at all.
                let mut gaps = 0usize;
                for app in &apps {
                    if app.principal_id.is_empty() { continue; }
                    let pid = app.principal_id.clone();
                    let roles = tokio::task::spawn_blocking(move || azure::list_role_assignments(&pid))
                        .await.unwrap_or(Ok(Vec::new())).unwrap_or_default();
                    if roles.is_empty() { gaps += 1; }
                }
                rbac_gaps.set(Some(gaps));

                // Cost MTD
                let sub_x = sub.clone();
                let rg_x = rg.clone();
                if let Ok(Ok(c)) = tokio::task::spawn_blocking(move || azure::get_cost_mtd(&sub_x, &rg_x)).await {
                    cost.set(Some(c));
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
            log_loading.set(true);
            spawn(async move {
                let sub = az.subscription.clone();
                let rg = az.resource_group.clone();
                let app = az.app_name.clone();
                let result = tokio::task::spawn_blocking(move || {
                    azure::list_actions(&sub, &rg, &app, &workflow, &run_id)
                }).await.unwrap_or_else(|e| Err(format!("{e}")));
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
    let mut poll_errors: Signal<usize> = use_signal(|| 0);
    // A few real error strings from the last sweep. A bare count tells you
    // something broke but not what — and "why is this card empty?" is
    // exactly the question the count can't answer.
    let mut poll_error_samples: Signal<Vec<String>> = use_signal(Vec::new);
    // Wall-clock duration of the last sweep. Surfaced in the header because
    // it's the honest answer to "am I actually getting the interval I asked
    // for?" — if this exceeds POLL_SECS the sweeps are running back-to-back.
    let mut sweep_secs: Signal<f64> = use_signal(|| 0.0);

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
            if *chain_polling.peek() { return; }
            let chains = chains_sig.peek().clone();
            if chains.is_empty() { return; }
            let az = az.clone();
            chain_polling.set(true);
            spawn(async move {
                let sweep_started = std::time::Instant::now();
                let mut errs = 0usize;
                let mut samples: Vec<String> = Vec::new();
                // Resolved once per sweep rather than per chain — it can't
                // change mid-sweep and peeking a signal in a loop is waste.
                let ns = if !az.sb_namespace.is_empty() {
                    az.sb_namespace.clone()
                } else {
                    discovered_ns.peek().clone().unwrap_or_default()
                };

                // Probed in small concurrent batches: results are still
                // written through batch by batch, so the page fills in
                // progressively rather than all at once at the end.
                for group in chains.chunks(POLL_CONCURRENCY) {
                    let mut pending = Vec::with_capacity(group.len());
                    for ch in group {
                        let sub = az.subscription.clone();
                        let rg = az.resource_group.clone();
                        let app = az.app_name.clone();
                        let ns = ns.clone();
                        let steps: Vec<String> = ch.steps.iter().map(|s| s.workflow.clone()).collect();
                        let queues = ch.queues.clone();
                        let label = ch.label.clone();
                        pending.push((label, tokio::task::spawn_blocking(move || {
                            chain_probe::probe_chain(&sub, &rg, &app, &ns, &steps, &queues, POLL_DEPTH)
                        })));
                    }
                    for (label, handle) in pending {
                        let Ok(probe) = handle.await else { errs += 1; continue };
                        errs += probe.errors.len();
                        for e in probe.errors.iter().take(3) {
                            if samples.len() < 5 && !samples.contains(e) {
                                samples.push(e.clone());
                            }
                        }
                        let now = epoch_secs();
                        chain_runs.write().insert(label.clone(), probe.runs);
                        chain_health_sig.write().insert(label.clone(), probe.health);
                        queue_statuses.write().insert(label.clone(), probe.queues);
                        last_checked_sig.write().insert(label, now);
                    }
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
                sweep_secs.set(sweep_started.elapsed().as_secs_f64());
                chain_poll_at.set(epoch_secs());
                chain_polling.set(false);
            });
        }
    };

    // One long-lived driver loop, started once via `use_hook` rather than
    // `use_effect`: an effect reruns whenever a signal it touched changes,
    // which would spawn a second loop each time and stack duplicate polls.
    //
    // It wakes on a short tick and polls only when a sweep is actually due.
    // That way discovery finishing mid-interval is picked up within seconds
    // instead of leaving the dashboard blank for the full POLL_SECS.
    use_hook({
        let mut poll_chains = poll_chains.clone();
        move || {
            spawn(async move {
                loop {
                    let last = *chain_poll_at.peek();
                    let due = last == 0 || epoch_secs().saturating_sub(last) >= POLL_SECS;
                    if due { poll_chains(); }
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
    let workspace_dir = workspace_dir(&az.resource_group, &az.app_name);

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
                let mut dated: Vec<(DateTime<Utc>, &azure::RunInfo)> = runs.iter()
                    .filter_map(|r| DateTime::parse_from_rfc3339(&r.start).ok()
                        .map(|dt| (dt.with_timezone(&Utc), r)))
                    .collect();
                dated.sort_by(|a, b| b.0.cmp(&a.0));

                // Runs still in flight aren't evidence either way, so the
                // verdict comes from the most recent finished run.
                let terminal: Vec<&(DateTime<Utc>, &azure::RunInfo)> = dated.iter()
                    .filter(|(_, r)| r.status == "Succeeded" || r.status == "Failed")
                    .collect();
                let Some((failed_at, newest)) = terminal.first().copied() else { continue };
                if newest.status != "Failed" { continue; }
                if *failed_at < cutoff { continue; }

                let consecutive = terminal.iter()
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
    let fetched_runs: usize = all_runs.values()
        .flat_map(|workflows| workflows.values())
        .map(|runs| runs.len())
        .sum();
    let have_run_data = fetched_runs > 0;
    let chains_polled = all_runs.len();

    // Live activity: every run Azure still reports as Running. Ordered
    // oldest-first so a run that has been going far longer than the rest —
    // the likely stuck one — sits at the top rather than scrolling away.
    let live_runs: Vec<LiveRun> = {
        let mut v: Vec<LiveRun> = all_runs.iter()
            .flat_map(|(chain, workflows)| {
                workflows.iter().flat_map(move |(workflow, runs)| {
                    runs.iter()
                        .filter(|r| r.status == "Running")
                        .filter_map(move |r| {
                            DateTime::parse_from_rfc3339(&r.start).ok().map(|dt| LiveRun {
                                chain: chain.clone(),
                                workflow: workflow.clone(),
                                started: dt.with_timezone(&Utc),
                            })
                        })
                })
            })
            .collect();
        v.sort_by(|a, b| a.started.cmp(&b.started));
        v
    };
    let live_chains: Vec<LiveChain> = {
        let mut by_chain: HashMap<String, (usize, std::collections::HashSet<String>, DateTime<Utc>)> = HashMap::new();
        for r in &live_runs {
            let entry = by_chain.entry(r.chain.clone())
                .or_insert_with(|| (0, std::collections::HashSet::new(), r.started));
            entry.0 += 1;
            entry.1.insert(r.workflow.clone());
            if r.started < entry.2 { entry.2 = r.started; }
        }
        let mut v: Vec<LiveChain> = by_chain.into_iter()
            .map(|(chain, (runs, workflows, oldest))| LiveChain {
                chain, runs, workflows: workflows.len(), oldest,
            })
            .collect();
        v.sort_by(|a, b| a.oldest.cmp(&b.oldest));
        v
    };
    // Dead letters belong to queues, not chains — listing them per queue is
    // what makes the row actionable, since that's the thing you go purge or
    // requeue. The chain is carried alongside only as context.
    // (queue, chain, count)
    let queue_statuses = props.chain_queue_statuses.read().clone();
    let dlq: Vec<(String, String, i64)> = {
        let mut v: Vec<(String, String, i64)> = queue_statuses.iter()
            .flat_map(|(chain, queues)| {
                queues.iter()
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
    let regressions: Vec<(String, f64, f64)> = history.chains.iter().filter_map(|(name, points)| {
        let rates: Vec<f64> = points.iter().filter_map(|p| p.success_rate).collect();
        if rates.len() < 3 { return None; }
        let latest = *rates.last()?;
        let prior = &rates[..rates.len() - 1];
        let avg = prior.iter().sum::<f64>() / prior.len() as f64;
        if avg - latest >= REGRESSION_DROP_PCT { Some((name.clone(), latest, avg)) } else { None }
    }).collect();
    // Timestamp of the newest history point across all chains, so the
    // regression card can say what moment it is describing.
    let history_at = history.chains.values()
        .filter_map(|points| points.last().map(|p| p.ts))
        .max()
        .unwrap_or(0);

    // Function errors from the Functions tab's cached metrics.
    let fn_snap = functions_cache::load_for(&az.resource_group, &az.app_name);
    let fn_errors: i64 = fn_snap.metrics.iter()
        .flat_map(|(_, m)| m.iter())
        .map(|m| m.errors)
        .sum();
    let fn_metrics_at = fn_snap.last_fetched;

    // Resource health falls back to its own cache if the live call hasn't
    // returned yet, so the tile isn't blank on first paint.
    let res_summary = *res_unhealthy.read();
    let res_cached = resource_health_cache::load_for(&az.resource_group, &az.app_name);
    let (res_bad, res_total, res_at) = match res_summary {
        Some((bad, total)) => (Some(bad), total, *res_fetched_at.read()),
        None if !res_cached.rows.is_empty() => {
            let bad = res_cached.rows.iter().filter(|r| {
                !matches!(r.state.as_str(), "Running" | "Succeeded" | "Active" | "Enabled")
                    || r.health == "Degraded" || r.health == "Unavailable"
            }).count();
            (Some(bad), res_cached.rows.len(), res_cached.last_fetched)
        }
        None => (None, 0, 0),
    };

    let chains_checked_at = last_checked.values().max().copied().unwrap_or(0);
    let is_loading = *loading.read();
    let drift_val = *drift_count.read();
    let rbac_val = *rbac_gaps.read();
    let cost_val = cost.read().clone();

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
                    } else {
                        {
                            let last = *sweep_secs.read();
                            // Show the sweep cost next to the interval: when
                            // it approaches POLL_SECS the effective refresh
                            // rate is the sweep, not the configured interval.
                            let suffix = if last > 0.0 { format!(" (sweep {last:.0}s)") } else { String::new() };
                            rsx! { "auto every {POLL_SECS}s{suffix} · " }
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
                }
                button {
                    class: "icon-refresh-btn",
                    title: "Refresh now",
                    disabled: is_loading || *chain_polling.read(),
                    onclick: move |_| { load(); poll_chains(); },
                    span { class: if is_loading || *chain_polling.read() { "icon-spin" } else { "" }, "⟳" }
                }
            }
            // Spelled out rather than left as a count: on a new tenant an
            // empty card is almost always a permission or naming mismatch,
            // and only the message text says which.
            if !poll_error_samples.read().is_empty() {
                div { class: "az-error home-poll-errors",
                    for e in poll_error_samples.read().iter() {
                        div { class: "home-poll-error-line", "{e}" }
                    }
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
                        table { class: "func-table home-table",
                            thead { tr { th { "Workflow" } th { "Chain" } th { "Failed at" } th { "×" } } }
                            tbody {
                                for f in failing.iter().take(12) {
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
                                                    "{f.workflow}"
                                                }
                                                td { style: "color:var(--text3);", "{f.chain}" }
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
                }

                // ── Live activity ───────────────────────────────────────
                // Two independent views of the same in-flight runs: rolled
                // up per chain, and listed per workflow. Deliberately not
                // cross-linked — each answers its own question.
                div { class: "func-app-card home-card",
                    div { class: "func-app-header",
                        h3 { "Chains running" }
                        span { class: "func-app-count", "as of {format_dt(chains_checked_at)}" }
                    }
                    if !have_run_data {
                        div { class: "func-empty-small", "No run data loaded yet." }
                    } else if live_chains.is_empty() {
                        div { class: "func-empty-small", "No chains running right now." }
                    } else {
                        table { class: "func-table home-table",
                            thead { tr { th { "Chain" } th { "Runs" } th { "Workflows" } th { "Oldest" } } }
                            tbody {
                                for c in live_chains.iter().take(12) {
                                    tr { class: "func-row",
                                        td { class: "func-name",
                                            span { class: "home-live-dot" }
                                            "{c.chain}"
                                        }
                                        td { "{c.runs}" }
                                        td { "{c.workflows}" }
                                        td { title: "Started {format_dt_utc(c.oldest)}", "{format_elapsed(c.oldest)}" }
                                    }
                                }
                            }
                        }
                    }
                }

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
                        table { class: "func-table home-table",
                            thead { tr { th { "Workflow" } th { "Chain" } th { "Started" } th { "Elapsed" } } }
                            tbody {
                                for r in live_runs.iter().take(12) {
                                    tr { class: "func-row",
                                        td { class: "func-name",
                                            span { class: "home-live-dot" }
                                            "{r.workflow}"
                                        }
                                        td { style: "color:var(--text3);", "{r.chain}" }
                                        td { "{format_dt_utc(r.started)}" }
                                        td { "{format_elapsed(r.started)}" }
                                    }
                                }
                            }
                        }
                    }
                }

                if !dlq.is_empty() {
                    div { class: "func-app-card home-card",
                        div { class: "func-app-header",
                            h3 { "Dead-letter backlog" }
                            span { class: "func-app-count", "as of {format_dt(chains_checked_at)}" }
                        }
                        table { class: "func-table home-table",
                            thead { tr { th { "Queue" } th { "Chain" } th { "Messages" } } }
                            tbody {
                                for (queue, chain, count) in dlq.iter().take(12) {
                                    tr { class: "func-row",
                                        td { class: "func-name", title: "{queue}", "{queue}" }
                                        td { style: "color:var(--text3);", "{chain}" }
                                        td { span { class: "func-errors has-errors", "{count}" } }
                                    }
                                }
                            }
                        }
                    }
                }

                if !regressions.is_empty() {
                    div { class: "func-app-card home-card",
                        div { class: "func-app-header",
                            h3 { "Success-rate regressions" }
                            span { class: "func-app-count", title: "Chains whose latest success rate is more than {REGRESSION_DROP_PCT:.0} points below their own recent average.", "as of {format_dt(history_at)}" }
                        }
                        table { class: "func-table home-table",
                            thead { tr { th { "Chain" } th { "Now" } th { "Avg" } } }
                            tbody {
                                for (name, latest, avg) in regressions.iter().take(10) {
                                    tr { class: "func-row",
                                        td { class: "func-name", "{name}" }
                                        td { span { class: "func-errors has-errors", "{latest:.0}%" } }
                                        td { "{avg:.0}%" }
                                    }
                                }
                            }
                        }
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
    if s.len() <= 14 { s.to_string() } else { format!("…{}", &s[s.len() - 13..]) }
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

fn workspace_dir(rg: &str, app: &str) -> String {
    dirs::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("ais-monitor")
        .join(format!("{}_{}", rg, app))
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
    if days <= 1 { "today".into() } else { format!("last {days}d") }
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
    if secs < 60 { return format!("{secs}s"); }
    if secs < 3600 { return format!("{}m {}s", secs / 60, secs % 60); }
    format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
}

fn format_ago_utc(dt: DateTime<Utc>) -> String {
    let mins = (Utc::now() - dt).num_minutes();
    if mins < 1 { return "(just now)".into(); }
    if mins < 60 { return format!("({mins}m)"); }
    let hours = mins / 60;
    if hours < 24 { return format!("({hours}h)"); }
    format!("({}d)", hours / 24)
}

/// Absolute local timestamp for an epoch-seconds value.
fn format_dt(ts: u64) -> String {
    if ts == 0 { return "never".into(); }
    match Utc.timestamp_opt(ts as i64, 0).single() {
        Some(dt) => format_dt_utc(dt),
        None => "never".into(),
    }
}

fn format_age(ts: u64) -> String {
    if ts == 0 { return "never".into(); }
    let age = epoch_secs().saturating_sub(ts);
    if age < 60 { format!("{age}s ago") }
    else if age < 3600 { format!("{}m ago", age / 60) }
    else if age < 86400 { format!("{}h ago", age / 3600) }
    else { format!("{}d ago", age / 86400) }
}

fn epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
