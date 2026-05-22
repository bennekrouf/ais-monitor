use dioxus::prelude::*;
use crate::components::{
    chain_list::ChainList,
    chain_detail::{AzConfig, ChainDetailView},
    login_banner::LoginBanner,
    eventgrid_panel::EventGridPanel,
};
use crate::services::{azure, azure::AzLoginState, chain, names};
use std::collections::HashMap;

#[derive(Props, Clone, PartialEq)]
pub struct MainScreenProps {
    pub logic_apps_dir: String,
    pub on_back: EventHandler<()>,
}

#[component]
pub fn MainScreen(props: MainScreenProps) -> Element {
    let dir = props.logic_apps_dir.clone();

    // ── Signals ──────────────────────────────────────────────────────────
    let mut is_light = use_signal(|| false);
    let mut az_state = use_signal(|| AzLoginState::Checking);
    let mut chains = use_signal(|| Vec::<chain::ChainDetail>::new());
    let mut selected_chain = use_signal(|| Option::<String>::None);
    let mut deployed_workflows = use_signal(|| Vec::<String>::new());
    let mut az_config = use_signal(|| Option::<AzConfig>::None);
    let chain_names = use_signal(|| names::load(&dir));
    let mut view_mode = use_signal(|| ViewMode::Chains);

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

    // ── Check Azure login on mount ───────────────────────────────────────
    use_effect(move || {
        spawn(async move {
            let state = tokio::task::spawn_blocking(azure::check_login)
                .await
                .unwrap_or(AzLoginState::NotLoggedIn);
            az_state.set(state);
        });
    });

    // ── Discover chains on mount ─────────────────────────────────────────
    use_effect({
        let dir = dir.clone();
        move || {
            let d = dir.clone();
            spawn(async move {
                let discovered = tokio::task::spawn_blocking(move || {
                    chain::discover_chains(&d)
                }).await.unwrap_or_default();
                chains.set(discovered);
            });
        }
    });

    // ── Fetch deployed workflows when logged in ──────────────────────────
    use_effect({
        let dir = dir.clone();
        move || {
            let state = az_state.read().clone();
            if let AzLoginState::LoggedIn { subscription_id, .. } = state {
                // Try to detect resource group & app name from dir name
                let (rg, app, sb) = detect_azure_resources(&dir);
                let sub = subscription_id.clone();
                let rg2 = rg.clone();
                let app2 = app.clone();
                az_config.set(Some(AzConfig {
                    subscription: sub.clone(),
                    resource_group: rg.clone(),
                    app_name: app.clone(),
                    sb_namespace: sb.clone(),
                }));

                spawn(async move {
                    let result = tokio::task::spawn_blocking(move || {
                        azure::list_deployed_workflows(&sub, &rg2, &app2)
                    }).await;
                    if let Ok(Ok(wfs)) = result {
                        deployed_workflows.set(wfs.iter().map(|w| w.name.clone()).collect());
                    }
                });
            }
        }
    });

    // ── Render ────────────────────────────────────────────────────────────
    let sel = selected_chain.read().clone();
    let selected_chain_detail = sel.as_ref().and_then(|label| {
        chains.read().iter().find(|c| c.label == *label).cloned()
    });

    rsx! {
        div { class: "app",
            // ── Top bar ──────────────────────────────────────────────
            div { class: "topbar",
                button {
                    class: "btn btn-back",
                    onclick: move |_| props.on_back.call(()),
                    "‹ Back"
                }
                span { class: "topbar-dir", title: "{dir}", "{dir}" }
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
                LoginBanner {
                    state: az_state.read().clone(),
                    on_login: move |_| {
                        azure::open_login(None);
                        az_state.set(AzLoginState::Checking);
                        spawn(async move {
                            for _ in 0..24 {
                                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                                let state = tokio::task::spawn_blocking(azure::check_login)
                                    .await
                                    .unwrap_or(AzLoginState::NotLoggedIn);
                                let done = matches!(state, AzLoginState::LoggedIn { .. });
                                az_state.set(state);
                                if done { break; }
                            }
                        });
                    },
                    on_refresh: move |_| {
                        az_state.set(AzLoginState::Checking);
                        spawn(async move {
                            let state = tokio::task::spawn_blocking(azure::check_login)
                                .await
                                .unwrap_or(AzLoginState::NotLoggedIn);
                            az_state.set(state);
                        });
                    },
                }
            }

            // ── Main content ─────────────────────────────────────────
            {
                let mode = view_mode.read().clone();
                match mode {
                    ViewMode::Chains => rsx! {
                        div { class: "main-content",
                            ChainList {
                                chains: chains.read().clone(),
                                selected: selected_chain.read().clone(),
                                on_select: move |label: String| selected_chain.set(Some(label)),
                                chain_names: chain_names.read().clone(),
                            }
                            div { class: "resize-handle", id: "resize-handle" }
                            div { class: "detail-pane",
                                if let Some(chain) = selected_chain_detail {
                                    ChainDetailView {
                                        chain: chain,
                                        deployed_workflows: deployed_workflows.read().clone(),
                                        az_config: az_config.read().clone(),
                                        logic_apps_dir: dir.clone(),
                                        chain_names: chain_names,
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
                                {
                                    let cfg = az_config.read().clone();
                                    if let Some(az) = cfg {
                                        rsx! {
                                            EventGridPanel { az_config: az }
                                        }
                                    } else {
                                        rsx! {
                                            div { class: "detail-empty",
                                                p { "Log in to Azure to view EventGrid topology" }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    },
                }
            }
        }
    }
}

/// Try to detect Azure resource names from the directory or local.settings.json
fn detect_azure_resources(dir: &str) -> (String, String, String) {
    let base = std::path::Path::new(dir);
    let settings_path = if base.join("logic_apps").exists() {
        base.join("logic_apps").join("local.settings.json")
    } else {
        base.join("local.settings.json")
    };

    // Try to read from local.settings.json
    if let Ok(content) = std::fs::read_to_string(&settings_path) {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&content) {
            let values = &json["Values"];
            // Look for common patterns
            let _sb = values["AzureServiceBusConnectionString"]
                .as_str()
                .or_else(|| values["ServiceBusConnection__fullyQualifiedNamespace"].as_str());
        }
    }

    // Default naming convention: rg-tom-{env}-chn-001 / logic-tom-{env}-chn-001 / sbns-tom-{env}-chn-001
    // Try to infer from directory name
    let dir_name = base.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");

    // Check for a .ais-monitor config file
    let config_path = base.join(".ais-monitor");
    if let Ok(content) = std::fs::read_to_string(&config_path) {
        let mut rg = String::new();
        let mut app = String::new();
        let mut sb = String::new();
        for line in content.lines() {
            let trimmed = line.trim();
            if let Some((key, val)) = trimmed.split_once('=') {
                match key.trim() {
                    "resource_group" => rg = val.trim().to_string(),
                    "app_name" => app = val.trim().to_string(),
                    "sb_namespace" => sb = val.trim().to_string(),
                    _ => {}
                }
            }
        }
        if !rg.is_empty() && !app.is_empty() {
            return (rg, app, sb);
        }
    }

    // Fallback: convention-based
    let env = if dir_name.contains("dev") || dir.contains("dev") { "dev" }
        else if dir_name.contains("stg") || dir.contains("stg") { "stg" }
        else if dir_name.contains("prd") || dir.contains("prd") { "prd" }
        else { "dev" };

    (
        format!("rg-tom-{env}-chn-001"),
        format!("logic-tom-{env}-chn-001"),
        format!("sbns-tom-{env}-chn-001"),
    )
}

#[derive(Clone, Debug, PartialEq)]
enum ViewMode {
    Chains,
    EventGrid,
}
