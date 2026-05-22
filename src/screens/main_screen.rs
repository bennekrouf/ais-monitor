use dioxus::prelude::*;
use crate::components::{
    chain_list::ChainList,
    chain_detail::{AzConfig, ChainDetailView, ChainHealth},
    eventgrid_panel::EventGridPanel,
};
use crate::services::{chain, remote_chain};
use std::collections::HashMap;

#[derive(Props, Clone, PartialEq)]
pub struct MainScreenProps {
    pub az_config: AzConfig,
    pub on_back: EventHandler<()>,
}

#[component]
pub fn MainScreen(props: MainScreenProps) -> Element {
    let az = props.az_config.clone();

    // ── Signals ──────────────────────────────────────────────────────────
    let mut is_light = use_signal(|| false);
    let mut chains = use_signal(|| Vec::<chain::ChainDetail>::new());
    let mut selected_chain = use_signal(|| Option::<String>::None);
    let mut deployed_workflows = use_signal(|| Vec::<String>::new());
    let chain_names = use_signal(|| HashMap::<String, String>::new());
    let chain_health = use_signal(|| HashMap::<String, ChainHealth>::new());
    let mut view_mode = use_signal(|| ViewMode::Chains);
    let mut loading_chains = use_signal(|| true);
    let mut load_error = use_signal(|| Option::<String>::None);

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

    rsx! {
        div { class: "app",
            // ── Top bar ──────────────────────────────────────────────
            div { class: "topbar",
                button {
                    class: "btn btn-back",
                    onclick: move |_| props.on_back.call(()),
                    "‹ Back"
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
