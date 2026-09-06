use crate::components::chain_detail::AzConfig;
use crate::services::azure::{self, AppSettingDrift, DriftStatus, FunctionApp};
use dioxus::prelude::*;
use std::collections::HashMap;

#[derive(Props, Clone, PartialEq)]
pub struct AppSettingsPanelProps {
    pub az_config: AzConfig,
}

#[component]
pub fn AppSettingsPanel(props: AppSettingsPanelProps) -> Element {
    let az = props.az_config.clone();
    let has_store = !az.app_config_store.trim().is_empty();

    let mut func_apps: Signal<Vec<FunctionApp>> = use_signal(Vec::new);
    let mut drift: Signal<Vec<(String, Vec<AppSettingDrift>)>> = use_signal(Vec::new);
    let mut loading: Signal<bool> = use_signal(|| true);
    let mut error_msg: Signal<Option<String>> = use_signal(|| None);
    let mut resetting: Signal<Option<(String, String)>> = use_signal(|| None);

    // Mount and Refresh both call `load`, so two can be in flight at once.
    let mut guard = crate::hooks::fetch_guard::use_fetch_guard();

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

                let sub2 = sub.clone();
                let rg2 = rg.clone();
                let apps_result =
                    tokio::task::spawn_blocking(move || azure::list_function_apps(&sub2, &rg2))
                        .await;

                let apps = match apps_result {
                    Ok(Ok(apps)) => apps,
                    Ok(Err(e)) => {
                        if guard.is_current(token) {
                            error_msg.set(Some(e));
                            loading.set(false);
                        }
                        return;
                    }
                    Err(e) => {
                        if guard.is_current(token) {
                            error_msg.set(Some(format!("{e}")));
                            loading.set(false);
                        }
                        return;
                    }
                };
                if !guard.is_current(token) {
                    return;
                }
                func_apps.set(apps.clone());

                let expected: Option<HashMap<String, String>> = if store.is_empty() {
                    None
                } else {
                    let sub3 = sub.clone();
                    let store2 = store.clone();
                    match tokio::task::spawn_blocking(move || {
                        azure::appconfig_list_kv(&sub3, &store2)
                    })
                    .await
                    {
                        Ok(Ok(kv)) => Some(kv),
                        Ok(Err(e)) => {
                            if guard.is_current(token) {
                                error_msg
                                    .set(Some(format!("App Configuration store '{store}': {e}")));
                            }
                            None
                        }
                        Err(e) => {
                            if guard.is_current(token) {
                                error_msg.set(Some(format!("{e}")));
                            }
                            None
                        }
                    }
                };

                let mut all_drift = Vec::new();
                for app in &apps {
                    let sub4 = sub.clone();
                    let rg4 = rg.clone();
                    let name = app.name.clone();
                    let name2 = name.clone();
                    let expected2 = expected.clone();
                    let rows = tokio::task::spawn_blocking(move || {
                        let live = azure::get_app_settings(&sub4, &rg4, &name).unwrap_or_default();
                        azure::compute_app_settings_drift(&live, expected2.as_ref())
                    })
                    .await
                    .unwrap_or_default();
                    all_drift.push((name2, rows));
                }
                if !guard.is_current(token) {
                    return;
                }
                drift.set(all_drift);
                loading.set(false);
            });
        }
    };

    use_effect({
        let mut load = load.clone();
        move || load()
    });

    let do_reset = {
        let az = az.clone();
        let load = load.clone();
        move |app_name: String, key: String, value: String| {
            let az = az.clone();
            let mut load = load.clone();
            resetting.set(Some((app_name.clone(), key.clone())));
            spawn(async move {
                let sub = az.subscription.clone();
                let rg = az.resource_group.clone();
                let res = tokio::task::spawn_blocking(move || {
                    azure::set_app_setting(&sub, &rg, &app_name, &key, &value)
                })
                .await;
                if let Ok(Err(e)) = res {
                    error_msg.set(Some(e));
                }
                resetting.set(None);
                load();
            });
        }
    };

    let is_loading = *loading.read();
    let err = error_msg.read().clone();
    let apps = func_apps.read().clone();
    let all_drift = drift.read().clone();
    let currently_resetting = resetting.read().clone();

    rsx! {
        div { class: "func-panel",
            div { class: "func-header",
                h2 { "App Settings & Config Drift" }
                button {
                    class: "icon-refresh-btn",
                    title: "Refresh",
                    disabled: is_loading,
                    onclick: move |_| load(),
                    span { class: if is_loading { "icon-spin" } else { "" }, "⟳" }
                }
            }

            if !has_store {
                div { class: "func-note",
                    "No App Configuration store set on this profile — showing live app settings only, no expected-value comparison. Edit the profile to add one."
                }
            }

            if is_loading {
                div { class: "func-loading", "Loading app settings…" }
            } else if let Some(e) = err {
                div { class: "az-error", "{e}" }
            } else if apps.is_empty() {
                div { class: "func-empty", "No function apps found in this resource group." }
            } else {
                for app in &apps {
                    {
                        let app_name = app.name.clone();
                        let rows = all_drift.iter()
                            .find(|(n, _)| n == &app_name)
                            .map(|(_, r)| r.as_slice())
                            .unwrap_or(&[]);
                        rsx! {
                            div { class: "func-app-card",
                                div { class: "func-app-header",
                                    h3 { "{app_name}" }
                                    span { class: "func-app-count", "{rows.len()} settings" }
                                }
                                if rows.is_empty() {
                                    div { class: "func-empty-small", "No app settings found." }
                                } else {
                                    table { class: "func-table app-settings-table",
                                        thead {
                                            tr {
                                                th { style: "width:220px;", title: "Drag the right edge to resize", "Key" }
                                                th { style: "width:320px;", title: "Drag the right edge to resize", "Live Value" }
                                                if has_store { th { style: "width:320px;", title: "Drag the right edge to resize", "Expected Value" } }
                                                th { style: "width:90px;", title: "Drag the right edge to resize", "Status" }
                                                th { style: "width:140px;", "" }
                                            }
                                        }
                                        tbody {
                                            for row in rows {
                                                {
                                                    let (badge_class, badge_text, detail) = drift_badge(&row.status);
                                                    let can_reset = has_store
                                                        && matches!(row.status, DriftStatus::Diff | DriftStatus::MissingLive)
                                                        && row.expected_value.is_some();
                                                    let an = app_name.clone();
                                                    let key = row.key.clone();
                                                    let exp = row.expected_value.clone().unwrap_or_default();
                                                    let mut do_reset = do_reset.clone();
                                                    let is_resetting = currently_resetting.as_ref()
                                                        == Some(&(app_name.clone(), row.key.clone()));
                                                    let live_display = if row.live_value.is_empty() { "—".to_string() } else { row.live_value.clone() };
                                                    let expected_display = row.expected_value.clone().unwrap_or_else(|| "—".to_string());
                                                    rsx! {
                                                        tr { class: "func-row",
                                                            td { class: "func-name", "{row.key}" }
                                                            td {
                                                                style: "font-family: var(--mono, monospace); font-size: 12px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis;",
                                                                title: "{live_display}",
                                                                "{live_display}"
                                                            }
                                                            if has_store {
                                                                td {
                                                                    style: "font-family: var(--mono, monospace); font-size: 12px; white-space: nowrap; overflow: hidden; text-overflow: ellipsis;",
                                                                    title: "{expected_display}",
                                                                    "{expected_display}"
                                                                }
                                                            }
                                                            td {
                                                                span { class: "{badge_class}", title: "{detail}", "{badge_text}" }
                                                            }
                                                            td {
                                                                if can_reset {
                                                                    button {
                                                                        class: "btn btn-small",
                                                                        disabled: is_resetting,
                                                                        title: "az webapp config appsettings set --name {an} --resource-group {az.resource_group} --settings {key}=<expected value>",
                                                                        onclick: move |_| do_reset(an.clone(), key.clone(), exp.clone()),
                                                                        if is_resetting { "Resetting…" } else { "Reset to expected" }
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
                }
            }
        }
    }
}

/// (css class, short badge text, tooltip detail) for a drift status.
fn drift_badge(status: &DriftStatus) -> (&'static str, &'static str, String) {
    match status {
        DriftStatus::Match => (
            "func-badge-active",
            "=",
            "Live value matches App Configuration".to_string(),
        ),
        DriftStatus::Diff => (
            "func-errors has-errors",
            "≠",
            "Live value differs from App Configuration".to_string(),
        ),
        DriftStatus::KvOk => (
            "func-badge-active",
            "KV✓",
            "Key Vault reference resolves successfully".to_string(),
        ),
        DriftStatus::KvFail { error } => (
            "func-errors has-errors",
            "KV✗",
            format!("Key Vault reference failed to resolve: {error}"),
        ),
        DriftStatus::LiteralWarn { missing } => (
            "func-badge-disabled",
            "LITERAL⚠",
            format!("Looks like a partial connection string — missing '{missing}='"),
        ),
        DriftStatus::NoExpected => (
            "func-no-data",
            "—",
            "No corresponding key in App Configuration".to_string(),
        ),
        DriftStatus::MissingLive => (
            "func-errors has-errors",
            "MISSING",
            "Expected in App Configuration but not set live".to_string(),
        ),
    }
}
