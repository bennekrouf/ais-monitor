use dioxus::prelude::*;
use crate::components::{
    activity_panel::ActivityPanel,
    chain_list::ChainList,
    chain_detail::{AzConfig, ChainDetailView, ChainHealth, QueueStatus},
    eventgrid_panel::EventGridPanel,
    graph_panel::GraphPanel,
    functions_panel::FunctionsPanel,
    api_test_panel::ApiTestPanel,
};
use crate::services::{activity, azure, azure::EgLink, chain, health_cache, history_cache, kpi, remote_chain};
use std::collections::HashMap;

#[derive(Props, Clone, PartialEq)]
pub struct MainScreenProps {
    pub az_config:        AzConfig,
    pub is_light:         Signal<bool>,
    pub theme_overridden: Signal<bool>,
    pub on_back:          EventHandler<()>,
}

#[component]
pub fn MainScreen(props: MainScreenProps) -> Element {
    let az = props.az_config.clone();

    // ── Signals ──────────────────────────────────────────────────────────
    // is_light is owned by the root App so theme applies to Welcome too.
    // The ☀️/🌙 button writes back into this same signal.
    let mut is_light = props.is_light;
    let mut theme_overridden = props.theme_overridden;
    let mut chains = use_signal(|| Vec::<chain::ChainDetail>::new());
    let mut selected_chain = use_signal(|| Option::<String>::None);
    let mut deployed_workflows = use_signal(|| Vec::<String>::new());
    let chain_names = use_signal(|| HashMap::<String, String>::new());
    let mut chain_health  = use_signal(|| HashMap::<String, ChainHealth>::new());
    // Per-chain, per-workflow raw run lists shared with ChainDetailView so
    // that "Check all" populates the per-workflow KPI columns (Success / Runs /
    // Avg / Streak) the same way an individual "Check" does. Without this the
    // table cells would stay empty after Check all because the component-local
    // `all_runs` signal is empty.
    let mut chain_runs: Signal<HashMap<String, HashMap<String, Vec<azure::RunInfo>>>> =
        use_signal(HashMap::new);
    // Per-chain queue counts so "Check all" can populate Active / Dead-Letter
    // columns identically to individual "Check".
    let mut chain_queue_statuses: Signal<HashMap<String, HashMap<String, QueueStatus>>> =
        use_signal(HashMap::new);
    let mut last_checked: Signal<HashMap<String, u64>> = use_signal(HashMap::new);
    let mut chain_history: Signal<HashMap<String, Vec<history_cache::HealthPoint>>> =
        use_signal(HashMap::new);
    // Discovered site metadata used to build proper Portal deep-links:
    //   • Logic Apps Standard workflow deep-link requires the site `location`.
    //   • Service Bus queue link needs a namespace; profile may not have one set.
    // Both fetched once on mount via az CLI; None until the call returns.
    let mut discovered_location: Signal<Option<String>> = use_signal(|| None);
    let mut discovered_sb_namespace: Signal<Option<String>> = use_signal(|| None);
    let mut view_mode = use_signal(|| ViewMode::Chains);
    // Mirrors `view_mode == Graph` as a plain bool signal so the (always-mounted)
    // GraphPanel can react to becoming visible and re-measure its container.
    let mut graph_visible = use_signal(|| false);
    // Lazy keep-alive: the EventGrid and Functions panels run Azure discovery on
    // mount, so we only mount them once their tab is first opened — then keep them
    // mounted (hidden) so their fetched state survives later tab switches.
    let mut visited_eg = use_signal(|| false);
    let mut visited_fn = use_signal(|| false);
    use_effect(move || {
        match *view_mode.read() {
            ViewMode::Graph     => graph_visible.set(true),
            ViewMode::EventGrid => { graph_visible.set(false); visited_eg.set(true); }
            ViewMode::Functions => { graph_visible.set(false); visited_fn.set(true); }
            ViewMode::Chains    => graph_visible.set(false),
            ViewMode::ApiTest   => graph_visible.set(false),
        }
    });
    let mut loading_chains = use_signal(|| true);
    let mut load_error     = use_signal(|| Option::<String>::None);
    // Gates the Refresh-button confirmation modal. Refresh clears the
    // chain-discovery cache and re-fetches everything from Azure, which can
    // take a while on large Logic Apps — better to ask first.
    let mut confirm_refresh = use_signal(|| false);
    let mut checking_all   = use_signal(|| false);
    let mut check_progress = use_signal(|| (0usize, 0usize)); // (done, total)
    // Shared run-history sample size — both "Check all" and the per-chain
    // detail view read this so their KPIs come from the same sample.
    let run_depth = use_signal(|| 20u32);
    let eg_links: Signal<HashMap<String, EgLink>> = use_signal(HashMap::new);

    // Minute-tick: forces re-render of the freshness dot in the top bar so
    // KPIs visibly age from fresh → stale → old without a user interaction.
    let mut freshness_tick = use_signal(|| 0u64);
    use_effect(move || {
        spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                let prev = *freshness_tick.peek();
                freshness_tick.set(prev.wrapping_add(1));
            }
        });
    });

    // Discover site location + SB namespace once on mount. Both feed the
    // Portal deep-links (workflow URL needs location, queue URL needs ns).
    // Fire-and-forget — links degrade gracefully (workflow → site overview,
    // queue → no link button) until these populate.
    use_effect({
        let az = az.clone();
        move || {
            let az = az.clone();
            spawn(async move {
                let sub = az.subscription.clone();
                let rg  = az.resource_group.clone();
                let app = az.app_name.clone();
                // Site location for the Logic App site.
                let sub_loc = sub.clone();
                let rg_loc  = rg.clone();
                let app_loc = app.clone();
                if let Ok(Ok(loc)) = tokio::task::spawn_blocking(move || {
                    azure::get_site_location(&sub_loc, &rg_loc, &app_loc)
                }).await {
                    discovered_location.set(Some(loc));
                }
                // SB namespace — only discover if profile didn't have one set.
                if az.sb_namespace.is_empty() {
                    let sub2 = sub.clone();
                    let rg2  = rg.clone();
                    if let Ok(Ok(mut list)) = tokio::task::spawn_blocking(move || {
                        azure::list_service_bus_namespaces(&sub2, &rg2)
                    }).await {
                        if let Some(ns) = list.drain(..).next() {
                            discovered_sb_namespace.set(Some(ns));
                        }
                    }
                } else {
                    // Profile already configured one — surface it through the
                    // same signal so call sites have a single read path.
                    discovered_sb_namespace.set(Some(az.sb_namespace.clone()));
                }
            });
        }
    });

    // Per-workspace directory used for cached health/last_checked snapshots and
    // any other on-disk artifacts. Same convention used in chain_detail.rs.
    let workspace_dir: String = dirs::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("ais-monitor")
        .join(format!("{}_{}", az.resource_group, az.app_name))
        .to_string_lossy()
        .to_string();
    // Point the activity log at this workspace, so events from anywhere in the
    // app (including spawn_blocking closures with no UI context) get persisted
    // to {workspace_dir}/activity.jsonl. Loading existing events happens here.
    activity::set_workspace_dir(workspace_dir.clone());

    // Skip-first-save latch: don't write back the snapshot we just loaded.
    let mut hydrated = use_signal(|| false);

    // Load cached KPI snapshot once on mount. Each Azure profile has its own
    // file, so chain_health survives app restarts.
    use_effect({
        let dir = workspace_dir.clone();
        move || {
            let snap = health_cache::load(&dir);
            if !snap.health.is_empty() {
                chain_health.set(snap.health);
            }
            if !snap.last_checked.is_empty() {
                last_checked.set(snap.last_checked);
            }
            // Load sparkline history at the same time.
            let hist = history_cache::load(&dir);
            if !hist.chains.is_empty() {
                chain_history.set(hist.chains);
            }
            hydrated.set(true);
        }
    });

    // Re-read history from disk whenever last_checked changes — that's the
    // signal that a chain check just appended a new point.
    use_effect({
        let dir = workspace_dir.clone();
        move || {
            // Subscribe to last_checked so this effect re-runs on changes.
            let _ = last_checked.read().len();
            let hist = history_cache::load(&dir);
            chain_history.set(hist.chains);
        }
    });

    // Persist whenever chain_health or last_checked change. The hydration guard
    // prevents the initial load from immediately writing back unchanged data.
    use_effect({
        let dir = workspace_dir.clone();
        move || {
            if !*hydrated.read() { return; }
            let snap = health_cache::HealthSnapshot {
                health: chain_health.read().clone(),
                last_checked: last_checked.read().clone(),
            };
            let dir = dir.clone();
            // Write on a blocking task so we never block the UI on slow disks.
            spawn(async move {
                tokio::task::spawn_blocking(move || {
                    health_cache::save(&dir, &snap);
                }).await.ok();
            });
        }
    });

    // ── Resize handle script ────────────────────────────────────────────
    use_effect(move || {
        document::eval(r#"
            (function() {
                if (window.__ais_monitor_resize_init) return;
                window.__ais_monitor_resize_init = true;
                document.body.addEventListener('mousedown', function(e) {
                    var target = e.target;
                    if (!target || target.id !== 'resize-handle') return;
                    e.preventDefault();
                    var list = target.previousElementSibling;
                    if (!list) return;
                    var startX = e.clientX;
                    var startW = list.getBoundingClientRect().width;
                    target.classList.add('dragging');
                    document.body.style.cursor = 'col-resize';
                    document.body.style.userSelect = 'none';
                    document.body.style.webkitUserSelect = 'none';
                    var onMove = function(ev) {
                        var w = startW + (ev.clientX - startX);
                        if (w < 160) w = 160;
                        if (w > 520) w = 520;
                        list.style.width = w + 'px';
                    };
                    var onUp = function() {
                        target.classList.remove('dragging');
                        document.body.style.cursor = '';
                        document.body.style.userSelect = '';
                        document.body.style.webkitUserSelect = '';
                        document.removeEventListener('mousemove', onMove);
                        document.removeEventListener('mouseup', onUp);
                    };
                    document.addEventListener('mousemove', onMove);
                    document.addEventListener('mouseup', onUp);
                });
            })();
        "#);
    });

    // ── Discover chains from Azure on mount ─────────────────────────────
    use_effect({
        let az = az.clone();
        move || {
            let az = az.clone();
            loading_chains.set(true);
            load_error.set(None);
            spawn(async move {
                let sub = az.subscription.clone();
                let rg = az.resource_group.clone();
                let app = az.app_name.clone();
                let local_dir = az.local_dir.clone();
                let result = tokio::task::spawn_blocking(move || {
                    remote_chain::discover_chains_remote(&sub, &rg, &app, &local_dir)
                }).await;

                match result {
                    Ok(Ok(discovered)) => {
                        // Extract deployed workflow names
                        let deployed: Vec<String> = discovered.iter()
                            .flat_map(|c| c.steps.iter().map(|s| s.workflow.clone()))
                            .collect();
                        activity::info(
                            "Discovered chains",
                            format!("{} chain(s), {} workflow(s)", discovered.len(), deployed.len()),
                        );
                        deployed_workflows.set(deployed);
                        chains.set(discovered);
                    }
                    Ok(Err(e)) => {
                        activity::error("Chain discovery failed", "", e.clone());
                        load_error.set(Some(e));
                    }
                    Err(e) => {
                        let s = format!("{e}");
                        activity::error("Chain discovery failed", "", s.clone());
                        load_error.set(Some(s));
                    }
                }
                loading_chains.set(false);
            });
        }
    });

    // ── Fetch Event Grid links (queue → topic mapping) ────────────────────
    use_effect({
        let rg = az.resource_group.clone();
        let mut eg_links = eg_links;
        move || {
            let rg = rg.clone();
            spawn(async move {
                let result = tokio::task::spawn_blocking(move || {
                    azure::build_eg_links(&rg)
                }).await;
                if let Ok(links) = result {
                    eg_links.set(links);
                }
            });
        }
    });

    // Content-first: keep a chain selected at all times so the user never
    // lands on an empty detail pane. Re-runs whenever the chain list changes;
    // picks the first chain when no valid selection exists. Also recovers
    // from a stale selection (chain removed after a refresh).
    use_effect(move || {
        let chains_now = chains.read();
        if chains_now.is_empty() { return; }
        // `peek()` reads without subscribing — the effect should re-run on
        // chain-list changes, not on its own write to `selected_chain`.
        let needs_default = match selected_chain.peek().as_ref() {
            None => true,
            Some(label) => !chains_now.iter().any(|c| &c.label == label),
        };
        if needs_default {
            selected_chain.set(Some(chains_now[0].label.clone()));
        }
    });

    // ── Render ────────────────────────────────────────────────────────────
    let sel = selected_chain.read().clone();
    let selected_chain_detail = sel.as_ref().and_then(|label| {
        chains.read().iter().find(|c| c.label == *label).cloned()
    });

    let app_label = format!("{} / {}", az.resource_group, az.app_name);

    // Principal ID of the Logic App's managed identity — fetched once and shown
    // in the topbar so the user can copy it for RBAC role assignments.
    let mut principal_id: Signal<Option<String>> = use_signal(|| None);
    let mut copied_pid: Signal<bool>             = use_signal(|| false);
    use_effect({
        let az = az.clone();
        move || {
            if principal_id.read().is_some() { return; }
            let az = az.clone();
            spawn(async move {
                let result = tokio::task::spawn_blocking(move || {
                    azure::get_principal_id(&az.subscription, &az.resource_group, &az.app_name)
                }).await;
                if let Ok(Ok(pid)) = result {
                    if !pid.is_empty() {
                        principal_id.set(Some(pid));
                    }
                }
            });
        }
    });

    // Derive an environment colour from the label or resource/app names
    let env_source = if !az.label.is_empty() { az.label.to_lowercase() }
                     else { format!("{} {}", az.resource_group, az.app_name).to_lowercase() };
    let (env_label, env_color) = if env_source.contains("prod") {
        (if !az.label.is_empty() { az.label.clone() } else { "PROD".into() }, "#e07070")
    } else if env_source.contains("stg") || env_source.contains("staging") {
        (if !az.label.is_empty() { az.label.clone() } else { "STG".into()  }, "#d29922")
    } else if env_source.contains("dev") {
        (if !az.label.is_empty() { az.label.clone() } else { "DEV".into()  }, "#3fb950")
    } else if env_source.contains("uat") || env_source.contains("test") {
        (if !az.label.is_empty() { az.label.clone() } else { "UAT".into()  }, "#bc8cff")
    } else if !az.label.is_empty() {
        (az.label.clone(), "#58a6ff")   // custom label, blue
    } else {
        (String::new(), "")             // no label, no badge
    };

    rsx! {
        div { class: "app",
            // ── Top bar ──────────────────────────────────────────────
            div { class: "topbar",
                button {
                    class: "btn btn-back",
                    onclick: move |_| props.on_back.call(()),
                    "‹ Back"
                }
                // Environment badge — colour-coded by profile label or resource name
                if !env_label.is_empty() {
                    span {
                        style: "padding:2px 9px; border-radius:10px; font-size:11px; font-weight:700; \
                                letter-spacing:.05em; color:#0d1117; background:{env_color}; \
                                flex-shrink:0; white-space:nowrap;",
                        "{env_label}"
                    }
                }
                span { class: "topbar-dir", title: "{app_label}", "{app_label}" }
                {
                    let pid = principal_id.read().clone();
                    if let Some(p) = pid {
                        let p_full   = p.clone();
                        let p_short  = if p.len() > 8 { format!("{}…", &p[..8]) } else { p.clone() };
                        let tooltip  = format!("Logic App managed identity\nPrincipal ID: {}\nClick to copy", p_full);
                        rsx! {
                            button {
                                style: "background:none; border:1px solid #30363d; border-radius:4px; \
                                        padding:1px 6px; font-size:10px; opacity:0.65; cursor:pointer; \
                                        font-family:monospace; white-space:nowrap;",
                                title: "{tooltip}",
                                onclick: move |_| {
                                    if let Ok(mut cb) = arboard::Clipboard::new() {
                                        let _ = cb.set_text(p_full.clone());
                                        copied_pid.set(true);
                                        spawn(async move {
                                            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                                            copied_pid.set(false);
                                        });
                                    }
                                },
                                if *copied_pid.read() { "✅ copied" } else { "🆔 {p_short}" }
                            }
                        }
                    } else {
                        rsx! {}
                    }
                }
                span {
                    style: "font-size:10px; opacity:0.4; white-space:nowrap;",
                    { concat!("v", env!("CARGO_PKG_VERSION")) }
                }
                {
                    let is_chains = *view_mode.read() == ViewMode::Chains;
                    let is_eg     = *view_mode.read() == ViewMode::EventGrid;
                    let is_graph  = *view_mode.read() == ViewMode::Graph;
                    let is_funcs  = *view_mode.read() == ViewMode::Functions;
                    let is_api    = *view_mode.read() == ViewMode::ApiTest;
                    // Pick the most-recent last_checked timestamp across all chains
                    // and translate it into a freshness state. Subscribe to the
                    // minute-tick so the colour ages without user interaction.
                    let _ = *freshness_tick.read();
                    let now = epoch_now();
                    let freshest: Option<u64> = last_checked.read().values().max().copied();
                    let (dot_class, dot_title) = match freshest {
                        None => ("freshness-dot freshness-none", "No KPIs collected yet".to_string()),
                        Some(ts) => {
                            let age = now.saturating_sub(ts);
                            if age < 5 * 60 {
                                ("freshness-dot freshness-fresh",
                                 format!("KPIs fresh ({}s ago)", age))
                            } else if age < 60 * 60 {
                                ("freshness-dot freshness-stale",
                                 format!("KPIs stale — last check {}m ago", age / 60))
                            } else {
                                ("freshness-dot freshness-old",
                                 format!("KPIs very stale — last check {}h ago", age / 3600))
                            }
                        }
                    };
                    rsx! {
                        div { class: "topbar-tabs",
                            button {
                                class: if is_chains { "topbar-tab active" } else { "topbar-tab" },
                                onclick: move |_| view_mode.set(ViewMode::Chains),
                                "Chains"
                                span { class: "{dot_class}", title: "{dot_title}" }
                            }
                            button {
                                class: if is_eg { "topbar-tab active" } else { "topbar-tab" },
                                onclick: move |_| view_mode.set(ViewMode::EventGrid),
                                "EventGrid"
                            }
                            button {
                                class: if is_funcs { "topbar-tab active" } else { "topbar-tab" },
                                onclick: move |_| view_mode.set(ViewMode::Functions),
                                "Functions"
                            }
                            button {
                                class: if is_graph { "topbar-tab active" } else { "topbar-tab" },
                                onclick: move |_| view_mode.set(ViewMode::Graph),
                                "Graph"
                            }
                            button {
                                class: if is_api { "topbar-tab active" } else { "topbar-tab" },
                                onclick: move |_| view_mode.set(ViewMode::ApiTest),
                                "API Test"
                            }
                        }
                    }
                }
                // Refresh button — opens a confirm modal before doing the work,
                // since clearing the cache and re-fetching every workflow can
                // be a multi-minute round trip on a large Logic App.
                button {
                    class: "btn btn-small",
                    disabled: *loading_chains.read(),
                    onclick: move |_| confirm_refresh.set(true),
                    if *loading_chains.read() { "Refreshing…" } else { "Refresh" }
                }

                // ── Check all chains ─────────────────────────────────
                {
                    let is_busy = *checking_all.read() || *loading_chains.read();
                    let (done, total) = *check_progress.read();
                    let az2 = az.clone();
                    rsx! {
                        button {
                            class: "btn btn-small",
                            disabled: is_busy || chains.read().is_empty(),
                            title: "Run health checks on all chains (success rate, dead letters, stuck runs)",
                            onclick: {
                                let workspace_dir_outer = workspace_dir.clone();
                                move |_| {
                                let az      = az2.clone();
                                let all     = chains.read().clone();
                                let total_n = all.len();
                                let depth   = *run_depth.read();
                                let workspace_dir_history = workspace_dir_outer.clone();
                                checking_all.set(true);
                                check_progress.set((0, total_n));
                                activity::info(
                                    "Check all started",
                                    format!("{} chain(s), depth {}", total_n, depth),
                                );

                                spawn(async move {
                                    let mut failed_chains: Vec<(String, String)> = Vec::new();
                                    // Check each chain sequentially to avoid hammering the API
                                    for (idx, ch) in all.iter().enumerate() {
                                        let sub  = az.subscription.clone();
                                        let rg   = az.resource_group.clone();
                                        let app  = az.app_name.clone();
                                        // Fall back to the namespace MainScreen discovered post-mount
                                        // when the profile doesn't have one configured.
                                        let ns = if !az.sb_namespace.is_empty() {
                                            az.sb_namespace.clone()
                                        } else {
                                            discovered_sb_namespace.read().clone().unwrap_or_default()
                                        };
                                        let steps  = ch.steps.iter().map(|s| s.workflow.clone()).collect::<Vec<_>>();
                                        let queues = ch.queues.clone();
                                        let label  = ch.label.clone();
                                        let label_for_log = label.clone();

                                        let (health, errors, runs_map, q_statuses) = tokio::task::spawn_blocking(move || {
                                            let mut errors: Vec<String> = Vec::new();
                                            // Run history for each workflow step
                                            let mut runs_map: HashMap<String, Vec<azure::RunInfo>> = HashMap::new();
                                            for wf in &steps {
                                                match azure::list_runs(&sub, &rg, &app, wf, depth) {
                                                    Ok(runs) => { runs_map.insert(wf.clone(), runs); }
                                                    Err(e) => errors.push(format!("list_runs {wf}: {e}")),
                                                }
                                            }
                                            // Queue counts — collect per-queue so the detail view's
                                            // Active / Dead-Letter columns populate too, not just the
                                            // aggregated dead-letter total.
                                            let mut dl_total: i64 = 0;
                                            let mut q_statuses: HashMap<String, QueueStatus> = HashMap::new();
                                            if !ns.is_empty() {
                                                for q in &queues {
                                                    match azure::check_queue(&ns, &rg, q) {
                                                        Ok(qi) => {
                                                            dl_total += qi.dead_letter;
                                                            q_statuses.insert(q.clone(), QueueStatus {
                                                                active: qi.active,
                                                                dead_letter: qi.dead_letter,
                                                            });
                                                        }
                                                        Err(e) => errors.push(format!("check_queue {q}: {e}")),
                                                    }
                                                }
                                            }
                                            // Compute KPIs
                                            let all_kpis: Vec<kpi::ChainKpi> = runs_map.values()
                                                .map(|r| kpi::compute_workflow_kpi(r))
                                                .collect();
                                            let total_runs: usize = all_kpis.iter().map(|k| k.total_runs).sum();
                                            let succeeded:  usize = all_kpis.iter().map(|k| k.succeeded).sum();
                                            let rate = if total_runs > 0 {
                                                Some((succeeded as f64 / total_runs as f64) * 100.0)
                                            } else { None };
                                            let stuck   = all_kpis.iter().map(|k| k.stuck_runs.len()).sum();
                                            let streak  = all_kpis.iter().map(|k| k.failure_streak).max().unwrap_or(0);
                                            (ChainHealth { success_rate: rate, dead_letters: dl_total, stuck_count: stuck, failure_streak: streak }, errors, runs_map, q_statuses)
                                        }).await.unwrap_or((ChainHealth::default(), vec!["spawn_blocking panic".into()], HashMap::new(), HashMap::new()));

                                        // Share the per-workflow runs with ChainDetailView so its
                                        // per-workflow KPI columns can render after "Check all".
                                        {
                                            let mut map = chain_runs.read().clone();
                                            map.insert(label.clone(), runs_map);
                                            chain_runs.set(map);
                                        }
                                        // Same write-through for Active / Dead-Letter queue counts.
                                        {
                                            let mut map = chain_queue_statuses.read().clone();
                                            map.insert(label.clone(), q_statuses);
                                            chain_queue_statuses.set(map);
                                        }

                                        if !errors.is_empty() {
                                            failed_chains.push((label_for_log.clone(), errors.join("\n")));
                                        }

                                        // Append a history point so the chain list can draw a sparkline.
                                        let point = history_cache::HealthPoint {
                                            ts: epoch_now(),
                                            success_rate: health.success_rate,
                                            dead_letters: health.dead_letters,
                                            stuck_count: health.stuck_count,
                                            failure_streak: health.failure_streak,
                                        };
                                        let dir_for_history = workspace_dir_history.clone();
                                        let label_for_history = label_for_log.clone();
                                        tokio::task::spawn_blocking(move || {
                                            history_cache::append(&dir_for_history, &label_for_history, point);
                                        }).await.ok();

                                        // Write result into the shared health map
                                        let mut map = chain_health.read().clone();
                                        map.insert(label.clone(), health);
                                        chain_health.set(map);
                                        let mut lc = last_checked.read().clone();
                                        lc.insert(label, epoch_now());
                                        last_checked.set(lc);
                                        check_progress.set((idx + 1, total_n));
                                    }
                                    checking_all.set(false);
                                    if failed_chains.is_empty() {
                                        activity::ok("Check all completed", format!("{} chain(s)", total_n));
                                    } else {
                                        let summary = format!(
                                            "{} of {} chain(s) had per-step errors",
                                            failed_chains.len(), total_n,
                                        );
                                        let detail = failed_chains.iter()
                                            .map(|(label, errs)| format!("• {label}\n{errs}"))
                                            .collect::<Vec<_>>()
                                            .join("\n\n");
                                        activity::warn("Check all completed with errors", summary, detail);
                                    }
                                });
                                }
                            },
                            if *checking_all.read() {
                                "Checking {done}/{total}…"
                            } else {
                                "⚡ Check all"
                            }
                        }
                    }
                }

                div { class: "topbar-spacer" }
                button {
                    class: "btn-theme",
                    onclick: move |_| {
                        theme_overridden.set(true);
                        let new_light = !*is_light.read();
                        is_light.set(new_light);
                    },
                    if *is_light.read() { "🌙" } else { "☀️" }
                }
                div { class: "login-banner",
                    span { class: "dot ok" }
                    span { class: "account-name", "Azure" }
                }
            }

            // ── Main content ─────────────────────────────────────────
            {
                let is_loading = *loading_chains.read();
                let err = load_error.read().clone();
                let mode = view_mode.read().clone();

                if is_loading {
                    rsx! {
                        div { class: "detail-pane",
                            div { class: "detail-empty",
                                p { "Fetching workflow definitions from Azure..." }
                            }
                        }
                    }
                } else if let Some(e) = err {
                    let auth_kind = azure::classify_auth_error(&e);
                    if let Some(kind) = auth_kind {
                        let detail = e.clone();
                        let tenant = az.tenant.clone();
                        let app_scope = format!(
                            "/subscriptions/{}/resourceGroups/{}/providers/Microsoft.Web/sites/{}",
                            az.subscription, az.resource_group, az.app_name
                        );
                        let scope_for_copy = app_scope.clone();
                        let is_rbac = matches!(kind, azure::AzAuthKind::MissingPermission);
                        let title = if is_rbac { "Missing Azure RBAC role" } else { "Azure session needs re-authentication" };
                        let blurb = if is_rbac {
                            "Your account is signed in, but it doesn't have the role required to read this Logic App's workflows. Re-signing in as the same user won't help — an Owner or User Access Administrator needs to assign you a role on this resource (or the resource group)."
                        } else {
                            "Your token expired or was revoked. Sign in again to refresh credentials."
                        };
                        let accent_bg = if is_rbac { "rgba(220,80,80,0.06)" } else { "rgba(255,170,0,0.05)" };

                        rsx! {
                            div { class: "detail-pane",
                                div { class: "detail-empty",
                                    div {
                                        style: "max-width:620px; padding:24px; border:1px solid var(--border, #444); border-radius:10px; background:{accent_bg};",
                                        h3 { style: "margin:0 0 8px; font-size:18px;", "{title}" }
                                        p { style: "margin:0 0 16px; font-size:13px; opacity:0.85; line-height:1.5;", "{blurb}" }

                                        if is_rbac {
                                            div { style: "margin-bottom:16px; font-size:12px; line-height:1.6; padding:12px; background:rgba(0,0,0,0.15); border-radius:6px;",
                                                div { style: "font-weight:600; margin-bottom:6px;", "Suggested roles (any one is enough):" }
                                                ul { style: "margin:0 0 8px 18px; padding:0;",
                                                    li { code { "Logic App Standard Operator" } " — read + run workflows" }
                                                    li { code { "Logic App Contributor" } " — full management" }
                                                    li { code { "Reader" } " + " code { "Logic App Standard Reader" } " — read-only" }
                                                }
                                                div { style: "font-weight:600; margin-top:10px; margin-bottom:4px;", "Scope to grant on:" }
                                                code { style: "display:block; word-break:break-all; font-size:11px; padding:6px; background:rgba(0,0,0,0.25); border-radius:4px;",
                                                    "{app_scope}"
                                                }
                                            }
                                        }

                                        div { style: "display:flex; gap:8px; align-items:center; flex-wrap:wrap;",
                                            if is_rbac {
                                                button {
                                                    style: "padding:8px 14px; font-size:13px; background:transparent; color:inherit; border:1px solid var(--border, #555); border-radius:6px; cursor:pointer;",
                                                    onclick: move |_| {
                                                        let _ = arboard::Clipboard::new().and_then(|mut cb| cb.set_text(scope_for_copy.clone()));
                                                    },
                                                    "Copy scope"
                                                }
                                                button {
                                                    style: "padding:8px 14px; font-size:13px; font-weight:600; background:#0078D4; color:white; border:none; border-radius:6px; cursor:pointer;",
                                                    onclick: {
                                                        let tenant_for_link = tenant.clone();
                                                        let sub = az.subscription.clone();
                                                        let rg  = az.resource_group.clone();
                                                        let app = az.app_name.clone();
                                                        move |_| {
                                                            // Deep-link straight to the Logic App's
                                                            // Access Control (IAM) blade so an admin
                                                            // can assign a role one click in.
                                                            let frag = if tenant_for_link.is_empty() {
                                                                "#".to_string()
                                                            } else {
                                                                format!("#@{}/", tenant_for_link)
                                                            };
                                                            let url = format!(
                                                                "https://portal.azure.com/{frag}resource/subscriptions/{sub}/resourceGroups/{rg}/providers/Microsoft.Web/sites/{app}/users",
                                                            );
                                                            crate::services::portal_links::open_in_browser(&url);
                                                        }
                                                    },
                                                    "Open IAM in Portal"
                                                }
                                                button {
                                                    style: "padding:8px 14px; font-size:13px; background:transparent; color:inherit; border:1px solid var(--border, #555); border-radius:6px; cursor:pointer;",
                                                    onclick: move |_| {
                                                        let t = if tenant.is_empty() { None } else { Some(tenant.clone()) };
                                                        let _ = azure::open_login(t.as_deref());
                                                        load_error.set(None);
                                                    },
                                                    title: "Try a different account that has the role",
                                                    "Switch account"
                                                }
                                            } else {
                                                button {
                                                    style: "padding:8px 14px; font-size:13px; font-weight:600; background:#0078D4; color:white; border:none; border-radius:6px; cursor:pointer;",
                                                    onclick: move |_| {
                                                        let t = if tenant.is_empty() { None } else { Some(tenant.clone()) };
                                                        let _ = azure::open_login(t.as_deref());
                                                        load_error.set(None);
                                                    },
                                                    "Sign in again"
                                                }
                                            }
                                            button {
                                                style: "padding:8px 14px; font-size:13px; background:transparent; color:inherit; border:1px solid var(--border, #555); border-radius:6px; cursor:pointer;",
                                                onclick: move |_| { load_error.set(None); },
                                                "Dismiss"
                                            }
                                        }
                                        details {
                                            style: "margin-top:14px; font-size:11px; opacity:0.6;",
                                            summary { style: "cursor:pointer;", "Show technical details" }
                                            pre {
                                                style: "white-space:pre-wrap; word-break:break-all; margin-top:8px; font-family:monospace; font-size:11px;",
                                                "{detail}"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    } else {
                        rsx! {
                            div { class: "detail-pane",
                                div { class: "detail-empty",
                                    div { class: "az-error", "{e}" }
                                }
                            }
                        }
                    }
                } else {
                    // Render ALL view panels and toggle visibility via CSS, so each
                    // panel's component state (e.g. ChainDetailView's fetched runs and
                    // KPI snapshot) survives tab switches instead of being dropped
                    // when an unselected match-arm goes away.
                    let chains_style    = if mode == ViewMode::Chains    { "" } else { "display:none" };
                    let eg_style        = if mode == ViewMode::EventGrid { "" } else { "display:none" };
                    let functions_style = if mode == ViewMode::Functions { "" } else { "display:none" };
                    let graph_style     = if mode == ViewMode::Graph     { "" } else { "display:none" };
                    let api_style       = if mode == ViewMode::ApiTest  { "" } else { "display:none" };
                    let api_save_dir = dirs::config_dir()
                        .unwrap_or_else(|| std::path::PathBuf::from("."))
                        .join("ais-monitor")
                        .join(format!("{}_{}", az.resource_group, az.app_name))
                        .to_string_lossy()
                        .to_string();
                    rsx! {
                        div { class: "view-stack",
                            div { class: "main-content", style: "{chains_style}",
                                ChainList {
                                    chains: chains.read().clone(),
                                    selected: selected_chain.read().clone(),
                                    on_select: move |label: String| selected_chain.set(Some(label)),
                                    chain_names: chain_names.read().clone(),
                                    chain_health: chain_health.read().clone(),
                                    last_checked: last_checked.read().clone(),
                                    chain_history: chain_history.read().clone(),
                                }
                                div { class: "resize-handle", id: "resize-handle" }
                                div { class: "detail-pane",
                                    if let Some(chain) = selected_chain_detail {
                                        ChainDetailView {
                                            // Key by chain label so each chain keeps its own state,
                                            // but switching tabs (which doesn't change the key) preserves it.
                                            key: "{chain.label}",
                                            chain: chain.clone(),
                                            deployed_workflows: deployed_workflows.read().clone(),
                                            az_config: Some(az.clone()),
                                            chain_names: chain_names,
                                            chain_health: Some(chain_health),
                                            chain_runs: Some(chain_runs),
                                            chain_queue_statuses: Some(chain_queue_statuses),
                                            last_checked: Some(last_checked),
                                            eg_links: eg_links,
                                            run_depth: Some(run_depth),
                                            discovered_location: discovered_location,
                                            discovered_sb_namespace: discovered_sb_namespace,
                                        }
                                    } else {
                                        div { class: "detail-empty",
                                            p { "Select a chain to see its details" }
                                        }
                                    }
                                }
                            }
                            div { class: "main-content", style: "{eg_style}",
                                div { class: "detail-pane",
                                    if *visited_eg.read() {
                                        EventGridPanel { az_config: az.clone() }
                                    }
                                }
                            }
                            div { class: "main-content", style: "{functions_style}",
                                div { class: "detail-pane",
                                    if *visited_fn.read() {
                                        FunctionsPanel { az_config: az.clone() }
                                    }
                                }
                            }
                            div { class: "main-content", style: "{graph_style}",
                                GraphPanel {
                                    chains: chains.read().clone(),
                                    is_light: is_light,
                                    visible: graph_visible,
                                }
                            }
                            div { class: "main-content", style: "{api_style}",
                                div { class: "detail-pane",
                                    ApiTestPanel {
                                        save_dir: api_save_dir.clone(),
                                        azure_subscription: az.subscription.clone(),
                                    }
                                }
                            }
                        }
                    }
                }
            }
            // Floating activity log — visible above the main content.
            ActivityPanel {}

            // Refresh confirmation modal — opened by the topbar Refresh button.
            if *confirm_refresh.read() {
                div { class: "modal-backdrop",
                    onclick: move |_| confirm_refresh.set(false),
                    div { class: "modal-card",
                        onclick: move |e: Event<MouseData>| e.stop_propagation(),
                        h3 { class: "modal-title", "Refresh chains?" }
                        p { class: "modal-body",
                            "This clears the chain-discovery cache and re-fetches every workflow from Azure. It can take a while on large Logic Apps."
                        }
                        div { class: "modal-actions",
                            button {
                                class: "btn btn-small",
                                onclick: move |_| confirm_refresh.set(false),
                                "Cancel"
                            }
                            button {
                                class: "btn btn-small btn-primary",
                                onclick: {
                                    let az = az.clone();
                                    move |_| {
                                        confirm_refresh.set(false);
                                        let az = az.clone();
                                        loading_chains.set(true);
                                        load_error.set(None);
                                        spawn(async move {
                                            let sub = az.subscription.clone();
                                            let app = az.app_name.clone();
                                            let sub2 = sub.clone();
                                            let rg = az.resource_group.clone();
                                            let app2 = app.clone();
                                            let local_dir = az.local_dir.clone();
                                            tokio::task::spawn_blocking(move || {
                                                remote_chain::clear_cache(&sub, &app);
                                            }).await.ok();
                                            let result = tokio::task::spawn_blocking(move || {
                                                remote_chain::discover_chains_remote(&sub2, &rg, &app2, &local_dir)
                                            }).await;
                                            match result {
                                                Ok(Ok(discovered)) => {
                                                    let deployed: Vec<String> = discovered.iter()
                                                        .flat_map(|c| c.steps.iter().map(|s| s.workflow.clone()))
                                                        .collect();
                                                    deployed_workflows.set(deployed);
                                                    chains.set(discovered);
                                                }
                                                Ok(Err(e)) => load_error.set(Some(e)),
                                                Err(e) => load_error.set(Some(format!("{e}"))),
                                            }
                                            loading_chains.set(false);
                                        });
                                    }
                                },
                                "Refresh"
                            }
                        }
                    }
                }
            }
        }
    }
}

fn epoch_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[derive(Clone, Debug, PartialEq)]
enum ViewMode {
    Chains,
    EventGrid,
    Functions,
    Graph,
    ApiTest,
}
