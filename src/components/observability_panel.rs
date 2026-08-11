use dioxus::prelude::*;
use std::collections::VecDeque;
use tokio::io::AsyncBufReadExt;
use crate::services::azure::{self, FunctionApp};
use crate::components::chain_detail::AzConfig;

/// Cap on retained log lines — bounds memory on a long-running tail.
const MAX_LOG_LINES: usize = 500;

#[derive(Props, Clone, PartialEq)]
pub struct ObservabilityPanelProps {
    pub az_config: AzConfig,
}

#[component]
pub fn ObservabilityPanel(props: ObservabilityPanelProps) -> Element {
    let az = props.az_config.clone();

    let mut func_apps: Signal<Vec<FunctionApp>> = use_signal(Vec::new);
    let mut apps_loading: Signal<bool> = use_signal(|| true);
    let mut apps_error: Signal<Option<String>> = use_signal(|| None);

    let mut tailing_app: Signal<Option<String>> = use_signal(|| None);
    let mut log_lines: Signal<VecDeque<String>> = use_signal(VecDeque::new);
    let mut log_error: Signal<Option<String>> = use_signal(|| None);
    // Bumped on every Start click so a stale tail loop from a previous
    // Start/Stop cycle knows to give up even if the process took a moment
    // to actually die.
    let mut tail_generation: Signal<u64> = use_signal(|| 0);

    let mut cost_loading: Signal<bool> = use_signal(|| false);
    let mut cost_result: Signal<Option<Result<(f64, String), String>>> = use_signal(|| None);

    use_effect({
        let az = az.clone();
        move || {
            let az = az.clone();
            apps_loading.set(true);
            spawn(async move {
                let sub = az.subscription.clone();
                let rg = az.resource_group.clone();
                match tokio::task::spawn_blocking(move || azure::list_function_apps(&sub, &rg)).await {
                    Ok(Ok(apps)) => func_apps.set(apps),
                    Ok(Err(e)) => apps_error.set(Some(e)),
                    Err(e) => apps_error.set(Some(format!("{e}"))),
                }
                apps_loading.set(false);
            });
        }
    });

    let start_tail = {
        let az = az.clone();
        move |app_name: String| {
            let az = az.clone();
            let my_generation = *tail_generation.read() + 1;
            tail_generation.set(my_generation);
            tailing_app.set(Some(app_name.clone()));
            log_lines.write().clear();
            log_error.set(None);
            spawn(async move {
                let mut cmd = azure::az_command_tokio(&[
                    "webapp", "log", "tail",
                    "--name", &app_name,
                    "--resource-group", &az.resource_group,
                    "--subscription", &az.subscription,
                ]);
                cmd.stdout(std::process::Stdio::piped());
                cmd.stderr(std::process::Stdio::null());
                let mut child = match cmd.spawn() {
                    Ok(c) => c,
                    Err(e) => {
                        log_error.set(Some(format!("Failed to start log tail: {e}")));
                        tailing_app.set(None);
                        return;
                    }
                };
                let Some(stdout) = child.stdout.take() else {
                    log_error.set(Some("Failed to capture log tail output".into()));
                    tailing_app.set(None);
                    return;
                };
                let mut reader = tokio::io::BufReader::new(stdout).lines();
                loop {
                    if *tail_generation.read() != my_generation { break; }
                    match reader.next_line().await {
                        Ok(Some(line)) => {
                            if *tail_generation.read() != my_generation { break; }
                            let mut lines = log_lines.write();
                            lines.push_back(line);
                            if lines.len() > MAX_LOG_LINES { lines.pop_front(); }
                        }
                        Ok(None) => break,
                        Err(_) => break,
                    }
                }
                let _ = child.kill().await;
                if *tail_generation.read() == my_generation {
                    tailing_app.set(None);
                }
            });
        }
    };

    let stop_tail = move |_| {
        // Bumping the generation makes the running loop above exit on its
        // next iteration and kill the child process itself.
        let next = *tail_generation.read() + 1;
        tail_generation.set(next);
        tailing_app.set(None);
    };

    let mut load_cost = {
        let az = az.clone();
        move || {
            let az = az.clone();
            cost_loading.set(true);
            cost_result.set(None);
            spawn(async move {
                let sub = az.subscription.clone();
                let rg = az.resource_group.clone();
                let result = tokio::task::spawn_blocking(move || azure::get_cost_mtd(&sub, &rg)).await
                    .unwrap_or_else(|e| Err(format!("{e}")));
                cost_result.set(Some(result));
                cost_loading.set(false);
            });
        }
    };

    // Fetch cost as soon as the tab opens rather than waiting for a click —
    // it's slower than the other tabs' loads (whole-subscription usage
    // list, summed client-side) but should still start automatically.
    use_effect({
        let mut load_cost = load_cost.clone();
        move || load_cost()
    });

    let apps = func_apps.read().clone();
    let currently_tailing = tailing_app.read().clone();
    let lines = log_lines.read().clone();
    let cost = cost_result.read().clone();

    rsx! {
        div { class: "func-panel",
            div { class: "func-header",
                h2 { "Observability" }
            }
            div { class: "func-note",
                "Function invocation/error counts and per-function exception drill-down already live on the Functions tab. This tab adds a live log stream and month-to-date cost — nothing here is cached, every view re-fetches from Azure."
            }

            // ── Cost MTD ────────────────────────────────────────────────
            div { class: "func-app-card",
                div { class: "func-app-header",
                    h3 { "Cost (month to date)" }
                    button {
                        class: "icon-refresh-btn",
                        style: "margin-left:auto;",
                        title: "Refresh",
                        disabled: *cost_loading.read(),
                        onclick: move |_| load_cost(),
                        span { class: if *cost_loading.read() { "icon-spin" } else { "" }, "⟳" }
                    }
                }
                match cost {
                    None => rsx! { div { class: "func-loading", "Summing subscription-wide usage for this resource group…" } },
                    Some(Ok((total, currency))) => rsx! {
                        div { class: "func-summary",
                            span { class: "func-summary-item func-success", "{total:.2} {currency}" }
                        }
                    },
                    Some(Err(e)) => rsx! { div { class: "az-error", "{e}" } },
                }
            }

            // ── Log tail ────────────────────────────────────────────────
            div { class: "func-app-card",
                div { class: "func-app-header",
                    h3 { "Live Log Tail" }
                }
                if *apps_loading.read() {
                    div { class: "func-loading", "Discovering function apps…" }
                } else if let Some(e) = apps_error.read().clone() {
                    div { class: "az-error", "{e}" }
                } else if apps.is_empty() {
                    div { class: "func-empty", "No function apps found in this resource group." }
                } else {
                    div { class: "func-metrics-bar",
                        span { style: "font-size:11px; color:var(--text2);", "App:" }
                        for app in &apps {
                            {
                                let app_name = app.name.clone();
                                let app_name2 = app_name.clone();
                                let is_this_one = currently_tailing.as_deref() == Some(app_name.as_str());
                                let mut start_tail = start_tail.clone();
                                rsx! {
                                    button {
                                        class: if is_this_one { "btn btn-small btn-primary" } else { "btn btn-small" },
                                        disabled: currently_tailing.is_some() && !is_this_one,
                                        onclick: move |_| {
                                            if is_this_one {
                                                // handled by the Stop button below
                                            } else {
                                                start_tail(app_name2.clone());
                                            }
                                        },
                                        "{app_name}"
                                    }
                                }
                            }
                        }
                        if currently_tailing.is_some() {
                            button { class: "btn btn-small", onclick: stop_tail, "Stop" }
                        }
                    }
                    if let Some(e) = log_error.read().clone() {
                        div { class: "az-error", "{e}" }
                    }
                    if let Some(ref app) = currently_tailing {
                        div { class: "sb-peek-meta", "Tailing {app} — streaming live, most recent {MAX_LOG_LINES} lines kept" }
                    }
                    {
                        let joined = if lines.is_empty() {
                            if currently_tailing.is_some() { "Waiting for log output…".to_string() } else { "Pick a Function App above to start tailing its logs.".to_string() }
                        } else {
                            lines.iter().cloned().collect::<Vec<_>>().join("\n")
                        };
                        rsx! {
                            pre { class: "log-tail-pane", "{joined}" }
                        }
                    }
                }
            }
        }
    }
}
