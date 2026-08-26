use crate::components::chain_detail::AzConfig;
use crate::services::azure::{self, ResourceHealthRow};
use crate::services::resource_health_cache;
use dioxus::prelude::*;

/// Background refresh cadence for the dashboard — matches the 30-60s window
/// from the proposal; picked the middle of that range.
const REFRESH_SECS: u64 = 45;

/// Cap on the extra backoff stacked onto REFRESH_SECS when Azure is
/// throttling or resetting connections.
const MAX_BACKOFF_SECS: u64 = 300;

#[derive(Props, Clone, PartialEq)]
pub struct ResourceHealthPanelProps {
    pub az_config: AzConfig,
}

#[component]
pub fn ResourceHealthPanel(props: ResourceHealthPanelProps) -> Element {
    let az = props.az_config.clone();

    let mut rows: Signal<Vec<ResourceHealthRow>> = use_signal(Vec::new);
    let mut loading: Signal<bool> = use_signal(|| true);
    let mut error_msg: Signal<Option<String>> = use_signal(|| None);
    let mut last_fetched: Signal<u64> = use_signal(|| 0);
    // Extra delay stacked on top of REFRESH_SECS while throttled; doubles
    // each throttled refresh, capped, resets to zero on a clean fetch.
    let mut backoff_secs: Signal<u64> = use_signal(|| 0);

    let mut refresh = {
        let az = az.clone();
        move || {
            let az = az.clone();
            error_msg.set(None);
            spawn(async move {
                let sub = az.subscription.clone();
                let rg = az.resource_group.clone();
                let app = az.app_name.clone();
                let sub2 = sub.clone();
                let rg2 = rg.clone();
                let result =
                    tokio::task::spawn_blocking(move || azure::list_resource_health(&sub2, &rg2))
                        .await;
                match result {
                    Ok(Ok(new_rows)) => {
                        rows.set(new_rows.clone());
                        let now = epoch_secs();
                        last_fetched.set(now);
                        let snap = resource_health_cache::ResourceHealthSnapshot {
                            rows: new_rows,
                            last_fetched: now,
                        };
                        tokio::task::spawn_blocking(move || {
                            resource_health_cache::save_for(&sub, &rg, &app, &snap)
                        })
                        .await
                        .ok();
                        backoff_secs.set(0);
                    }
                    Ok(Err(e)) => {
                        if azure::is_throttling_error(&e) {
                            let prev = *backoff_secs.peek();
                            let next = if prev == 0 { REFRESH_SECS } else { prev * 2 };
                            backoff_secs.set(next.min(MAX_BACKOFF_SECS));
                        }
                        error_msg.set(Some(e));
                    }
                    Err(e) => error_msg.set(Some(format!("{e}"))),
                }
                loading.set(false);
            });
        }
    };

    // Hydrate from disk, then start a background poll loop that keeps
    // refreshing every REFRESH_SECS for as long as this panel stays mounted
    // (it's kept alive-but-hidden across tab switches, same as the other
    // panels, so this loop effectively runs for the life of the session).
    use_effect({
        let az = az.clone();
        let mut refresh = refresh.clone();
        move || {
            let snap =
                resource_health_cache::load_for(&az.subscription, &az.resource_group, &az.app_name);
            if !snap.rows.is_empty() {
                rows.set(snap.rows);
                last_fetched.set(snap.last_fetched);
                loading.set(false);
            }
            refresh();
        }
    });

    // The repeating loop lives in `use_hook`, not the effect above: an
    // effect reruns whenever a signal it touched changes, which would spawn
    // a second loop each time and double the polling rate. `use_hook` runs
    // exactly once for the life of the component.
    use_hook({
        let mut refresh = refresh.clone();
        move || {
            spawn(async move {
                loop {
                    let interval = REFRESH_SECS + *backoff_secs.peek();
                    tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
                    refresh();
                }
            });
        }
    });

    let is_loading = *loading.read();
    let err = error_msg.read().clone();
    let all_rows = rows.read().clone();
    let fetched_at = *last_fetched.read();

    rsx! {
        div { class: "func-panel",
            div { class: "func-header",
                h2 { "Resource Health" }
                div { style: "display:flex; align-items:center; gap:10px;",
                    if fetched_at > 0 {
                        {
                            let interval = REFRESH_SECS + *backoff_secs.read();
                            rsx! { span { style: "font-size:11px; color:var(--text2);", "{format_age(fetched_at)} · auto-refreshes every {interval}s" } }
                        }
                    }
                    if *backoff_secs.read() > 0 {
                        span {
                            style: "font-size:11px; color:var(--text2);",
                            title: "Azure is throttling or resetting connections, so polling has backed off to reduce load.",
                            "backing off"
                        }
                    }
                    button {
                        class: "icon-refresh-btn",
                        title: "Refresh now",
                        disabled: is_loading,
                        onclick: move |_| refresh(),
                        span { class: if is_loading { "icon-spin" } else { "" }, "⟳" }
                    }
                }
            }

            if is_loading && all_rows.is_empty() {
                div { class: "func-loading", "Discovering resources…" }
            } else if let Some(e) = err {
                div { class: "az-error", "{e}" }
            } else if all_rows.is_empty() {
                div { class: "func-empty", "No resources found in this resource group." }
            } else {
                table { class: "func-table",
                    thead {
                        tr {
                            th { "Name" }
                            th { "Type" }
                            th { "State" }
                            th { "Health" }
                        }
                    }
                    tbody {
                        for row in &all_rows {
                            {
                                let (state_class, state_text) = state_badge(&row.state);
                                let (health_class, health_text) = health_badge(&row.health);
                                let short_type = row.resource_type.rsplit('/').next().unwrap_or(&row.resource_type);
                                rsx! {
                                    tr { class: "func-row",
                                        td { class: "func-name", "{row.name}" }
                                        td { title: "{row.resource_type}", "{short_type}" }
                                        td { span { class: "{state_class}", "{state_text}" } }
                                        td { span { class: "{health_class}", "{health_text}" } }
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

fn state_badge(state: &str) -> (&'static str, String) {
    match state {
        "Running" | "Succeeded" | "Active" | "Enabled" => ("func-badge-active", state.to_string()),
        "Stopped" | "Disabled" | "Paused" => ("func-badge-disabled", state.to_string()),
        "Unknown" | "" => ("func-no-data", "Unknown".to_string()),
        other => ("func-errors has-errors", other.to_string()),
    }
}

fn health_badge(health: &str) -> (&'static str, String) {
    match health {
        "Available" => ("func-badge-active", "Up".to_string()),
        "Degraded" => ("func-errors has-errors", "Degraded".to_string()),
        "Unavailable" => ("func-errors has-errors", "Down".to_string()),
        "Unknown" | "" => ("func-no-data", "Unknown".to_string()),
        other => ("func-no-data", other.to_string()),
    }
}

fn format_age(ts: u64) -> String {
    let now = epoch_secs();
    let age = now.saturating_sub(ts);
    if age < 60 {
        format!("Updated {age}s ago")
    } else {
        format!("Updated {}m ago", age / 60)
    }
}

fn epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
