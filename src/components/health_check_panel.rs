use crate::components::chain_detail::AzConfig;
use crate::services::azure::{self, AppSettingDrift, DriftStatus, FunctionApp, RoleAssignment};
use dioxus::prelude::*;

#[derive(Clone, Debug, PartialEq)]
struct AuthCheckRow {
    app_name: String,
    principal_id: String,
    roles: Vec<RoleAssignment>,
}

#[derive(Props, Clone, PartialEq)]
pub struct HealthCheckPanelProps {
    pub az_config: AzConfig,
}

#[component]
pub fn HealthCheckPanel(props: HealthCheckPanelProps) -> Element {
    let az = props.az_config.clone();
    let has_store = !az.app_config_store.trim().is_empty();

    let mut settings_running: Signal<bool> = use_signal(|| false);
    let mut settings_results: Signal<Option<Vec<(String, Vec<AppSettingDrift>)>>> =
        use_signal(|| None);
    let mut settings_error: Signal<Option<String>> = use_signal(|| None);

    let mut auth_running: Signal<bool> = use_signal(|| false);
    let mut auth_results: Signal<Option<Vec<AuthCheckRow>>> = use_signal(|| None);
    let mut auth_error: Signal<Option<String>> = use_signal(|| None);

    let mut run_settings_check = {
        let az = az.clone();
        move || {
            let az = az.clone();
            settings_running.set(true);
            settings_error.set(None);
            spawn(async move {
                let sub = az.subscription.clone();
                let rg = az.resource_group.clone();
                let store = az.app_config_store.trim().to_string();

                let sub2 = sub.clone();
                let rg2 = rg.clone();
                let apps = match tokio::task::spawn_blocking(move || {
                    azure::list_function_apps(&sub2, &rg2)
                })
                .await
                {
                    Ok(Ok(apps)) => apps,
                    Ok(Err(e)) => {
                        settings_error.set(Some(e));
                        settings_running.set(false);
                        return;
                    }
                    Err(e) => {
                        settings_error.set(Some(format!("{e}")));
                        settings_running.set(false);
                        return;
                    }
                };

                let expected = if store.is_empty() {
                    None
                } else {
                    let sub3 = sub.clone();
                    let store2 = store.clone();
                    tokio::task::spawn_blocking(move || azure::appconfig_list_kv(&sub3, &store2))
                        .await
                        .ok()
                        .and_then(|r| r.ok())
                };

                let mut results = Vec::new();
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
                    results.push((name2, rows));
                }
                settings_results.set(Some(results));
                settings_running.set(false);
            });
        }
    };

    let mut run_auth_check = {
        let az = az.clone();
        move || {
            let az = az.clone();
            auth_running.set(true);
            auth_error.set(None);
            spawn(async move {
                let sub = az.subscription.clone();
                let rg = az.resource_group.clone();

                let sub2 = sub.clone();
                let rg2 = rg.clone();
                let apps: Vec<FunctionApp> = match tokio::task::spawn_blocking(move || {
                    azure::list_function_apps(&sub2, &rg2)
                })
                .await
                {
                    Ok(Ok(apps)) => apps,
                    Ok(Err(e)) => {
                        auth_error.set(Some(e));
                        auth_running.set(false);
                        return;
                    }
                    Err(e) => {
                        auth_error.set(Some(format!("{e}")));
                        auth_running.set(false);
                        return;
                    }
                };

                let mut results = Vec::new();
                for app in &apps {
                    let pid = app.principal_id.clone();
                    let pid2 = pid.clone();
                    let roles =
                        tokio::task::spawn_blocking(move || azure::list_role_assignments(&pid2))
                            .await
                            .unwrap_or(Ok(Vec::new()))
                            .unwrap_or_default();
                    results.push(AuthCheckRow {
                        app_name: app.name.clone(),
                        principal_id: pid,
                        roles,
                    });
                }
                auth_results.set(Some(results));
                auth_running.set(false);
            });
        }
    };

    // Run both checks automatically as soon as the tab opens — no click
    // needed for the first look, matching every other tab in the app.
    // The buttons below stay for an explicit re-run.
    use_effect({
        let mut run_settings_check = run_settings_check.clone();
        let mut run_auth_check = run_auth_check.clone();
        move || {
            run_settings_check();
            run_auth_check();
        }
    });

    rsx! {
        div { class: "func-panel",
            div { class: "func-header",
                h2 { "Health Checks" }
            }
            div { class: "func-note",
                "Runs live against Azure — no local scripts, no cached expectations. Re-fetches current state on open and on demand."
            }

            // ── App Settings Check ─────────────────────────────────────
            div { class: "func-app-card",
                div { class: "func-app-header",
                    h3 { "App Settings Check" }
                    button {
                        class: "icon-refresh-btn",
                        title: "Re-run",
                        disabled: *settings_running.read(),
                        onclick: move |_| run_settings_check(),
                        span { class: if *settings_running.read() { "icon-spin" } else { "" }, "⟳" }
                    }
                }
                if !has_store {
                    div { class: "func-note",
                        "No App Configuration store set on this profile — this check will only flag Key Vault resolution failures and partial-literal connection strings, not value drift."
                    }
                }
                if let Some(e) = settings_error.read().clone() {
                    div { class: "az-error", "{e}" }
                }
                if let Some(results) = settings_results.read().clone() {
                    {
                        let mut pass = 0usize;
                        let mut fail = 0usize;
                        let mut failures: Vec<(String, AppSettingDrift)> = Vec::new();
                        for (app_name, rows) in &results {
                            for row in rows {
                                if is_settings_failure(&row.status) {
                                    fail += 1;
                                    failures.push((app_name.clone(), row.clone()));
                                } else {
                                    pass += 1;
                                }
                            }
                        }
                        rsx! {
                            div { class: "func-summary",
                                span { class: "func-summary-item func-success", "Passed: {pass}" }
                                span {
                                    class: if fail > 0 { "func-summary-item func-errors has-errors" } else { "func-summary-item func-errors" },
                                    "Failed: {fail}"
                                }
                            }
                            if !failures.is_empty() {
                                table { class: "func-table",
                                    thead {
                                        tr { th { "App" } th { "Key" } th { "Issue" } th { "Fix" } }
                                    }
                                    tbody {
                                        for (app_name, row) in &failures {
                                            tr { class: "func-row",
                                                td { class: "func-name", "{app_name}" }
                                                td { "{row.key}" }
                                                td { "{settings_issue_text(&row.status)}" }
                                                td { style: "font-family: var(--mono, monospace); font-size: 11px;",
                                                    "{settings_fix_command(&az.resource_group, app_name, row)}"
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

            // ── Auth / RBAC Check ──────────────────────────────────────
            div { class: "func-app-card",
                div { class: "func-app-header",
                    h3 { "Managed Identity Auth Check" }
                    button {
                        class: "icon-refresh-btn",
                        title: "Re-run",
                        disabled: *auth_running.read(),
                        onclick: move |_| run_auth_check(),
                        span { class: if *auth_running.read() { "icon-spin" } else { "" }, "⟳" }
                    }
                }
                if let Some(e) = auth_error.read().clone() {
                    div { class: "az-error", "{e}" }
                }
                if let Some(results) = auth_results.read().clone() {
                    {
                        let with_identity: Vec<&AuthCheckRow> = results.iter().filter(|r| !r.principal_id.is_empty()).collect();
                        let pass = with_identity.iter().filter(|r| !r.roles.is_empty()).count();
                        let fail = with_identity.iter().filter(|r| r.roles.is_empty()).count();
                        let no_identity: Vec<&AuthCheckRow> = results.iter().filter(|r| r.principal_id.is_empty()).collect();
                        rsx! {
                            div { class: "func-summary",
                                span { class: "func-summary-item func-success", "Passed: {pass}" }
                                span {
                                    class: if fail > 0 { "func-summary-item func-errors has-errors" } else { "func-summary-item func-errors" },
                                    "Failed: {fail}"
                                }
                                if !no_identity.is_empty() {
                                    span { class: "func-summary-item", "No managed identity: {no_identity.len()}" }
                                }
                            }
                            if fail > 0 {
                                table { class: "func-table",
                                    thead {
                                        tr { th { "App" } th { "Principal ID" } th { "Issue" } th { "Fix" } }
                                    }
                                    tbody {
                                        for row in with_identity.iter().filter(|r| r.roles.is_empty()) {
                                            tr { class: "func-row",
                                                td { class: "func-name", "{row.app_name}" }
                                                td { style: "font-family: var(--mono, monospace); font-size: 11px;", "{row.principal_id}" }
                                                td { "No RBAC role assignments found" }
                                                td { style: "font-family: var(--mono, monospace); font-size: 11px;",
                                                    "az role assignment create --assignee {row.principal_id} --role <RoleName> --scope <resourceId>"
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

fn is_settings_failure(status: &DriftStatus) -> bool {
    matches!(
        status,
        DriftStatus::Diff
            | DriftStatus::LiteralWarn { .. }
            | DriftStatus::KvFail { .. }
            | DriftStatus::MissingLive
    )
}

fn settings_issue_text(status: &DriftStatus) -> String {
    match status {
        DriftStatus::Diff => "Live value differs from App Configuration".to_string(),
        DriftStatus::LiteralWarn { missing } => {
            format!("Partial connection string — missing '{missing}='")
        }
        DriftStatus::KvFail { error } => format!("Key Vault reference failed: {error}"),
        DriftStatus::MissingLive => "Expected in App Configuration but not set live".to_string(),
        _ => String::new(),
    }
}

fn settings_fix_command(rg: &str, app: &str, row: &AppSettingDrift) -> String {
    match &row.status {
        DriftStatus::Diff | DriftStatus::MissingLive => {
            let val = row.expected_value.clone().unwrap_or_default();
            format!("az webapp config appsettings set --resource-group {rg} --name {app} --settings {}={}", row.key, val)
        }
        DriftStatus::LiteralWarn { missing } => {
            format!("Add the missing '{missing}=' segment, or replace with a Key Vault reference")
        }
        DriftStatus::KvFail { .. } => {
            "Verify vault/secret name and grant this identity 'Key Vault Secrets User' on the vault"
                .to_string()
        }
        _ => String::new(),
    }
}
