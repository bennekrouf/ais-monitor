use dioxus::prelude::*;
use crate::components::{
    chain_list::ChainList,
    chain_detail::{AzConfig, ChainDetailView, ChainHealth},
    eventgrid_panel::EventGridPanel,
};
use crate::services::{azure, chain, kpi, remote_chain};
use std::collections::HashMap;

#[derive(Props, Clone, PartialEq)]
pub struct MainScreenProps {
    pub az_config: AzConfig,
    pub is_light:  Signal<bool>,
    pub on_back:   EventHandler<()>,
}

#[component]
pub fn MainScreen(props: MainScreenProps) -> Element {
    let az = props.az_config.clone();

    // ── Signals ──────────────────────────────────────────────────────────
    // is_light is owned by the root App so theme applies to Welcome too.
    // The ☀️/🌙 button writes back into this same signal.
    let mut is_light = props.is_light;
    let mut chains = use_signal(|| Vec::<chain::ChainDetail>::new());
    let mut selected_chain = use_signal(|| Option::<String>::None);
    let mut deployed_workflows = use_signal(|| Vec::<String>::new());
    let chain_names = use_signal(|| HashMap::<String, String>::new());
    let mut chain_health = use_signal(|| HashMap::<String, ChainHealth>::new());
    let mut view_mode = use_signal(|| ViewMode::Chains);
    let mut loading_chains = use_signal(|| true);
    let mut load_error     = use_signal(|| Option::<String>::None);
    let mut checking_all   = use_signal(|| false);
    let mut check_progress = use_signal(|| (0usize, 0usize)); // (done, total)

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
                let result = tokio::task::spawn_blocking(move || {
                    remote_chain::discover_chains_remote(&sub, &rg, &app)
                }).await;

                match result {
                    Ok(Ok(discovered)) => {
                        // Extract deployed workflow names
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
    });

    // ── Render ────────────────────────────────────────────────────────────
    let sel = selected_chain.read().clone();
    let selected_chain_detail = sel.as_ref().and_then(|label| {
        chains.read().iter().find(|c| c.label == *label).cloned()
    });

    let app_label = format!("{} / {}", az.resource_group, az.app_name);

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
                    let is_chains = *view_mode.read() == ViewMode::Chains;
                    let is_eg = *view_mode.read() == ViewMode::EventGrid;
                    rsx! {
                        div { class: "topbar-tabs",
                            button {
                                class: if is_chains { "topbar-tab active" } else { "topbar-tab" },
                                onclick: move |_| view_mode.set(ViewMode::Chains),
                                "Chains"
                            }
                            button {
                                class: if is_eg { "topbar-tab active" } else { "topbar-tab" },
                                onclick: move |_| view_mode.set(ViewMode::EventGrid),
                                "EventGrid"
                            }
                        }
                    }
                }
                // Refresh button to clear cache and reload
                button {
                    class: "btn btn-small",
                    disabled: *loading_chains.read(),
                    onclick: {
                        let az = az.clone();
                        move |_| {
                            let az = az.clone();
                            loading_chains.set(true);
                            load_error.set(None);
                            spawn(async move {
                                let sub = az.subscription.clone();
                                let app = az.app_name.clone();
                                let sub2 = sub.clone();
                                let rg = az.resource_group.clone();
                                let app2 = app.clone();
                                tokio::task::spawn_blocking(move || {
                                    remote_chain::clear_cache(&sub, &app);
                                }).await.ok();
                                let result = tokio::task::spawn_blocking(move || {
                                    remote_chain::discover_chains_remote(&sub2, &rg, &app2)
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
                            onclick: move |_| {
                                let az      = az2.clone();
                                let all     = chains.read().clone();
                                let total_n = all.len();
                                checking_all.set(true);
                                check_progress.set((0, total_n));

                                spawn(async move {
                                    // Check each chain sequentially to avoid hammering the API
                                    for (idx, ch) in all.iter().enumerate() {
                                        let sub  = az.subscription.clone();
                                        let rg   = az.resource_group.clone();
                                        let app  = az.app_name.clone();
                                        let ns   = az.sb_namespace.clone();
                                        let steps  = ch.steps.iter().map(|s| s.workflow.clone()).collect::<Vec<_>>();
                                        let queues = ch.queues.clone();
                                        let label  = ch.label.clone();

                                        let health = tokio::task::spawn_blocking(move || {
                                            // Run history for each workflow step
                                            let mut runs_map: HashMap<String, Vec<azure::RunInfo>> = HashMap::new();
                                            for wf in &steps {
                                                if let Ok(runs) = azure::list_runs(&sub, &rg, &app, wf, 20) {
                                                    runs_map.insert(wf.clone(), runs);
                                                }
                                            }
                                            // Queue dead-letter counts
                                            let mut dl_total: i64 = 0;
                                            if !ns.is_empty() {
                                                for q in &queues {
                                                    if let Ok(qi) = azure::check_queue(&ns, &rg, q) {
                                                        dl_total += qi.dead_letter;
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
                                            ChainHealth { success_rate: rate, dead_letters: dl_total, stuck_count: stuck, failure_streak: streak }
                                        }).await.unwrap_or_default();

                                        // Write result into the shared health map
                                        let mut map = chain_health.read().clone();
                                        map.insert(label, health);
                                        chain_health.set(map);
                                        check_progress.set((idx + 1, total_n));
                                    }
                                    checking_all.set(false);
                                });
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
                        let new_light = !*is_light.read();
                        is_light.set(new_light);
                        let js = if new_light {
                            "document.body.classList.add('light')"
                        } else {
                            "document.body.classList.remove('light')"
                        };
                        document::eval(js);
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
                    rsx! {
                        div { class: "detail-pane",
                            div { class: "detail-empty",
                                div { class: "az-error", "{e}" }
                            }
                        }
                    }
                } else {
                    match mode {
                        ViewMode::Chains => rsx! {
                            div { class: "main-content",
                                ChainList {
                                    chains: chains.read().clone(),
                                    selected: selected_chain.read().clone(),
                                    on_select: move |label: String| selected_chain.set(Some(label)),
                                    chain_names: chain_names.read().clone(),
                                    chain_health: chain_health.read().clone(),
                                }
                                div { class: "resize-handle", id: "resize-handle" }
                                div { class: "detail-pane",
                                    if let Some(chain) = selected_chain_detail {
                                        ChainDetailView {
                                            chain: chain,
                                            deployed_workflows: deployed_workflows.read().clone(),
                                            az_config: Some(az.clone()),
                                            chain_names: chain_names,
                                            chain_health: Some(chain_health),
                                        }
                                    } else {
                                        div { class: "detail-empty",
                                            p { "Select a chain to see its details" }
                                        }
                                    }
                                }
                            }
                        },
                        ViewMode::EventGrid => rsx! {
                            div { class: "main-content",
                                div { class: "detail-pane",
                                    EventGridPanel { az_config: az.clone() }
                                }
                            }
                        },
                    }
                }
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum ViewMode {
    Chains,
    EventGrid,
}
