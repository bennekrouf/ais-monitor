use dioxus::prelude::*;
use crate::services::azure::{self, FunctionApp, FunctionDetail, FunctionMetrics, FunctionError};
use crate::services::functions_cache;
use crate::components::chain_detail::AzConfig;

#[derive(Clone, Debug, PartialEq)]
enum LifecycleAction {
    Start,
    Stop,
    Restart,
    SyncTriggers,
}

impl LifecycleAction {
    fn label(&self) -> &'static str {
        match self {
            LifecycleAction::Start => "Start",
            LifecycleAction::Stop => "Stop",
            LifecycleAction::Restart => "Restart",
            LifecycleAction::SyncTriggers => "Sync triggers",
        }
    }
    fn az_command(&self, rg: &str, app: &str) -> String {
        let verb = match self {
            LifecycleAction::Start => "functionapp start",
            LifecycleAction::Stop => "functionapp stop",
            LifecycleAction::Restart => "functionapp restart",
            LifecycleAction::SyncTriggers => "functionapp sync-function-triggers",
        };
        format!("az {verb} --resource-group {rg} --name {app}")
    }
}

#[derive(Clone, Debug, PartialEq)]
struct PendingLifecycleAction {
    app_name: String,
    action: LifecycleAction,
}

#[derive(Props, Clone, PartialEq)]
pub struct FunctionsPanelProps {
    pub az_config: AzConfig,
}

#[component]
pub fn FunctionsPanel(props: FunctionsPanelProps) -> Element {
    let az = props.az_config.clone();

    let mut func_apps: Signal<Vec<FunctionApp>> = use_signal(Vec::new);
    let mut functions: Signal<Vec<(String, Vec<FunctionDetail>)>> = use_signal(Vec::new);
    let mut metrics: Signal<Vec<(String, Vec<FunctionMetrics>)>> = use_signal(Vec::new);
    let mut app_insights_name: Signal<Option<String>> = use_signal(|| None);
    let mut loading: Signal<bool> = use_signal(|| true);
    let mut error_msg: Signal<Option<String>> = use_signal(|| None);
    let mut metrics_loading: Signal<bool> = use_signal(|| false);
    let mut days_range: Signal<u32> = use_signal(|| 30);
    // Error drill-down: (app_name, function_name) → error rows
    let mut error_key: Signal<Option<(String, String)>> = use_signal(|| None);
    let mut error_details: Signal<Vec<FunctionError>> = use_signal(Vec::new);
    let mut error_details_loading: Signal<bool> = use_signal(|| false);
    let mut pending_action: Signal<Option<PendingLifecycleAction>> = use_signal(|| None);
    let mut action_running: Signal<bool> = use_signal(|| false);
    let mut action_error: Signal<Option<String>> = use_signal(|| None);

    // Auto-discover on mount: paint cached snapshot instantly, then refresh.
    use_effect({
        let az = az.clone();
        move || {
            let az = az.clone();
            error_msg.set(None);

            // Hydrate from disk first so the user sees something immediately.
            let snap = functions_cache::load_for(&az.resource_group, &az.app_name);
            let has_cache = !snap.func_apps.is_empty();
            if has_cache {
                func_apps.set(snap.func_apps);
                functions.set(snap.functions);
                app_insights_name.set(snap.app_insights_name);
                metrics.set(snap.metrics);
                loading.set(false);
            } else {
                loading.set(true);
            }

            spawn(async move {
                let sub = az.subscription.clone();
                let rg = az.resource_group.clone();
                let app = az.app_name.clone();

                // 1. List function apps
                let rg2 = rg.clone();
                let sub2 = sub.clone();
                let apps_result = tokio::task::spawn_blocking(move || {
                    azure::list_function_apps(&sub2, &rg2)
                }).await;

                match apps_result {
                    Ok(Ok(apps)) => {
                        func_apps.set(apps.clone());

                        // 2. List functions for each app
                        let mut all_funcs = Vec::new();
                        for app_info in &apps {
                            let rg3 = rg.clone();
                            let name = app_info.name.clone();
                            let name2 = name.clone();
                            if let Ok(Ok(fns)) = tokio::task::spawn_blocking(move || {
                                azure::list_functions(&rg3, &name)
                            }).await {
                                all_funcs.push((name2, fns));
                            }
                        }
                        functions.set(all_funcs.clone());

                        // 3. Discover App Insights
                        let rg4 = rg.clone();
                        let ai_name: Option<String> = tokio::task::spawn_blocking(move || {
                            azure::find_app_insights(&rg4)
                        }).await
                            .ok()
                            .and_then(|r| r.ok())
                            .and_then(|list| list.into_iter().next());
                        app_insights_name.set(ai_name.clone());

                        // 4. Persist what we just discovered. Metrics are kept
                        //    from the previous snapshot (or empty) — they only
                        //    get refreshed on explicit "Fetch Metrics" click.
                        let snap = functions_cache::FunctionsSnapshot {
                            func_apps: apps,
                            functions: all_funcs,
                            app_insights_name: ai_name,
                            metrics: metrics.read().clone(),
                            last_fetched: epoch_secs(),
                        };
                        tokio::task::spawn_blocking(move || {
                            functions_cache::save_for(&rg, &app, &snap);
                        }).await.ok();
                    }
                    Ok(Err(e)) => error_msg.set(Some(e)),
                    Err(e) => error_msg.set(Some(format!("{e}"))),
                }
                loading.set(false);
            });
        }
    });

    // Fetch metrics handler
    let fetch_metrics = {
        let az = az.clone();
        move |_| {
            let az = az.clone();
            let ai = app_insights_name.read().clone();
            let apps = func_apps.read().clone();
            let days = *days_range.read();
            if ai.is_none() || apps.is_empty() { return; }
            let ai_name = ai.unwrap();
            metrics_loading.set(true);
            spawn(async move {
                let mut all_metrics = Vec::new();
                for app in &apps {
                    let rg = az.resource_group.clone();
                    let ai = ai_name.clone();
                    let app_name = app.name.clone();
                    let app_name2 = app_name.clone();
                    if let Ok(Ok(m)) = tokio::task::spawn_blocking(move || {
                        azure::query_function_metrics(&rg, &ai, &app_name, days)
                    }).await {
                        all_metrics.push((app_name2, m));
                    }
                }
                metrics.set(all_metrics.clone());
                metrics_loading.set(false);

                // Persist the new metrics into the cached snapshot.
                let snap = functions_cache::FunctionsSnapshot {
                    func_apps: func_apps.read().clone(),
                    functions: functions.read().clone(),
                    app_insights_name: app_insights_name.read().clone(),
                    metrics: all_metrics,
                    last_fetched: epoch_secs(),
                };
                let rg = az.resource_group.clone();
                let app = az.app_name.clone();
                tokio::task::spawn_blocking(move || {
                    functions_cache::save_for(&rg, &app, &snap);
                }).await.ok();
            });
        }
    };

    // Fetch error details for a specific function
    let fetch_errors = {
        let az = az.clone();
        move |app_name: String, fn_name: String| {
            let az = az.clone();
            let ai = app_insights_name.read().clone();
            let days = *days_range.read();
            let key = (app_name.clone(), fn_name.clone());
            // Toggle off if same key clicked again
            if error_key.read().as_ref() == Some(&key) {
                error_key.set(None);
                return;
            }
            let Some(ai_name) = ai else { return };
            error_key.set(Some(key));
            error_details.set(Vec::new());
            error_details_loading.set(true);
            spawn(async move {
                let rg = az.resource_group.clone();
                let result = tokio::task::spawn_blocking(move || {
                    azure::query_function_errors(&rg, &ai_name, &app_name, &fn_name, days)
                }).await;
                match result {
                    Ok(Ok(errs)) => error_details.set(errs),
                    _ => error_details.set(Vec::new()),
                }
                error_details_loading.set(false);
            });
        }
    };

    // Run a confirmed lifecycle action, then re-list function apps so the
    // state dot / running badge reflects the new state.
    let run_action = {
        let az = az.clone();
        move |pending: PendingLifecycleAction| {
            let az = az.clone();
            action_running.set(true);
            action_error.set(None);
            spawn(async move {
                let sub = az.subscription.clone();
                let rg = az.resource_group.clone();
                let app_name = pending.app_name.clone();
                let action = pending.action.clone();
                let rg2 = rg.clone();
                let sub2 = sub.clone();
                let app2 = app_name.clone();
                let result: Result<(), String> = tokio::task::spawn_blocking(move || {
                    match action {
                        LifecycleAction::Start => azure::functionapp_start(&sub2, &rg2, &app2),
                        LifecycleAction::Stop => azure::functionapp_stop(&sub2, &rg2, &app2),
                        LifecycleAction::Restart => azure::functionapp_restart(&sub2, &rg2, &app2),
                        LifecycleAction::SyncTriggers => azure::functionapp_sync_triggers(&sub2, &rg2, &app2),
                    }
                }).await.unwrap_or_else(|e| Err(format!("{e}")));

                match &result {
                    Ok(()) => {
                        crate::services::activity::info(
                            format!("Function App {}", pending.action.label()),
                            app_name.clone(),
                        );
                        pending_action.set(None);
                    }
                    Err(e) => {
                        crate::services::activity::error(
                            format!("Function App {} failed", pending.action.label()),
                            app_name.clone(),
                            e.clone(),
                        );
                        action_error.set(Some(e.clone()));
                    }
                }

                // Re-list so the state badge picks up the change.
                let sub3 = sub.clone();
                let rg3 = rg.clone();
                if let Ok(Ok(apps)) = tokio::task::spawn_blocking(move || azure::list_function_apps(&sub3, &rg3)).await {
                    func_apps.set(apps);
                }
                action_running.set(false);
            });
        }
    };

    let is_loading = *loading.read();
    let err = error_msg.read().clone();
    let apps = func_apps.read().clone();
    let funcs = functions.read().clone();
    let mets = metrics.read().clone();
    let has_ai = app_insights_name.read().is_some();
    let days = *days_range.read();
    let active_error_key = error_key.read().clone();
    let err_details = error_details.read().clone();
    let err_loading = *error_details_loading.read();

    rsx! {
        div { class: "func-panel",
            // Header
            div { class: "func-header",
                h2 { "Function Apps" }
                if has_ai && !apps.is_empty() {
                    div { class: "func-metrics-bar",
                        span { style: "font-size:11px; color:var(--text2);", "Metrics range:" }
                        select {
                            class: "eg-select",
                            value: "{days}",
                            onchange: move |e: Event<FormData>| {
                                if let Ok(d) = e.value().parse::<u32>() {
                                    days_range.set(d);
                                }
                            },
                            option { value: "1", "Last 24h" }
                            option { value: "7", "Last 7 days" }
                            option { value: "30", "Last 30 days" }
                            option { value: "90", "Last 90 days" }
                        }
                        button {
                            class: "btn btn-small",
                            disabled: *metrics_loading.read(),
                            onclick: fetch_metrics,
                            if *metrics_loading.read() { "Loading..." } else { "Fetch Metrics" }
                        }
                    }
                }
            }

            if is_loading {
                div { class: "func-loading", "Discovering function apps..." }
            } else if let Some(e) = err {
                div { class: "az-error", "{e}" }
            } else if apps.is_empty() {
                div { class: "func-empty", "No function apps found in this resource group." }
            } else {
                for app in &apps {
                    {
                        let app_name = app.name.clone();
                        let app_funcs = funcs.iter()
                            .find(|(n, _)| n == &app_name)
                            .map(|(_, f)| f.as_slice())
                            .unwrap_or(&[]);
                        let app_metrics = mets.iter()
                            .find(|(n, _)| n == &app_name)
                            .map(|(_, m)| m.as_slice())
                            .unwrap_or(&[]);
                        let state_class = if app.state == "Running" { "func-state running" } else { "func-state stopped" };
                        rsx! {
                            div { class: "func-app-card",
                                div { class: "func-app-header",
                                    span { class: "{state_class}" }
                                    h3 { "{app_name}" }
                                    span { class: "func-app-count",
                                        "{app_funcs.len()} functions"
                                    }
                                    {
                                        let url = crate::services::portal_links::function_app(
                                            &az.tenant, &az.subscription, &az.resource_group, &app_name,
                                        );
                                        rsx! {
                                            button {
                                                class: "portal-link",
                                                title: "Open Function App in Azure Portal",
                                                onclick: move |_| crate::services::portal_links::open_in_browser(&url),
                                                "🔗"
                                            }
                                        }
                                    }
                                    div { class: "func-lifecycle-actions",
                                        {
                                            let is_running_state = app.state == "Running";
                                            let an1 = app_name.clone();
                                            let an2 = app_name.clone();
                                            let an3 = app_name.clone();
                                            let an4 = app_name.clone();
                                            rsx! {
                                                button {
                                                    class: "btn btn-small",
                                                    disabled: is_running_state,
                                                    onclick: move |_| pending_action.set(Some(PendingLifecycleAction { app_name: an1.clone(), action: LifecycleAction::Start })),
                                                    "Start"
                                                }
                                                button {
                                                    class: "btn btn-small",
                                                    disabled: !is_running_state,
                                                    onclick: move |_| pending_action.set(Some(PendingLifecycleAction { app_name: an2.clone(), action: LifecycleAction::Stop })),
                                                    "Stop"
                                                }
                                                button {
                                                    class: "btn btn-small",
                                                    onclick: move |_| pending_action.set(Some(PendingLifecycleAction { app_name: an3.clone(), action: LifecycleAction::Restart })),
                                                    "Restart"
                                                }
                                                button {
                                                    class: "btn btn-small",
                                                    title: "Force the host to re-read trigger bindings without a full restart",
                                                    onclick: move |_| pending_action.set(Some(PendingLifecycleAction { app_name: an4.clone(), action: LifecycleAction::SyncTriggers })),
                                                    "Sync triggers"
                                                }
                                            }
                                        }
                                    }
                                }

                                if app_funcs.is_empty() {
                                    div { class: "func-empty-small", "No functions discovered" }
                                } else {
                                    table { class: "func-table",
                                        thead {
                                            tr {
                                                th { "Function" }
                                                th { "Language" }
                                                th { "Status" }
                                                if !app_metrics.is_empty() {
                                                    th { class: "func-th-num", "Success" }
                                                    th { class: "func-th-num", "Errors" }
                                                    th { "Last Run" }
                                                }
                                            }
                                        }
                                        tbody {
                                            for func in app_funcs {
                                                {
                                                    let fn_name = func.name.clone();
                                                    let m = app_metrics.iter().find(|m| m.function_name == fn_name);
                                                    let disabled_class = if func.is_disabled { " func-disabled" } else { "" };
                                                    let is_error_open = active_error_key.as_ref()
                                                        .map(|(a, f)| a == &app_name && f == &fn_name)
                                                        .unwrap_or(false);
                                                    let col_span = if app_metrics.is_empty() { 3usize } else { 6usize };
                                                    let mut fetch_errors = fetch_errors.clone();
                                                    let an = app_name.clone();
                                                    let fn2 = fn_name.clone();
                                                    let portal_fn_url = crate::services::portal_links::function(
                                                        &az.tenant, &az.subscription, &az.resource_group,
                                                        &app_name, &fn_name,
                                                    );
                                                    rsx! {
                                                        tr { class: "func-row{disabled_class}",
                                                            td { class: "func-name",
                                                                "{fn_name}"
                                                                button {
                                                                    class: "portal-link",
                                                                    title: "Open this function's invocations in Azure Portal",
                                                                    onclick: move |_| crate::services::portal_links::open_in_browser(&portal_fn_url),
                                                                    "🔗"
                                                                }
                                                            }
                                                            td { class: "func-lang",
                                                                span { class: "func-lang-badge", "{func.language}" }
                                                            }
                                                            td {
                                                                if func.is_disabled {
                                                                    span { class: "func-badge-disabled", "Disabled" }
                                                                } else {
                                                                    span { class: "func-badge-active", "Active" }
                                                                }
                                                            }
                                                            if !app_metrics.is_empty() {
                                                                td { class: "func-td-num",
                                                                    if let Some(met) = m {
                                                                        span { class: "func-success", "{met.success}" }
                                                                    } else {
                                                                        span { class: "func-no-data", "—" }
                                                                    }
                                                                }
                                                                td { class: "func-td-num",
                                                                    if let Some(met) = m {
                                                                        {
                                                                            let errors = met.errors;
                                                                            let cls = if errors > 0 { "func-errors has-errors func-errors-btn" } else { "func-errors" };
                                                                            let title = if errors > 0 { "Click to view error details" } else { "" };
                                                                            rsx! {
                                                                                span {
                                                                                    class: "{cls}",
                                                                                    title: "{title}",
                                                                                    onclick: move |_| {
                                                                                        if errors > 0 {
                                                                                            fetch_errors(an.clone(), fn2.clone());
                                                                                        }
                                                                                    },
                                                                                    "{errors}"
                                                                                    if errors > 0 { " ▾" }
                                                                                }
                                                                            }
                                                                        }
                                                                    } else {
                                                                        span { class: "func-no-data", "—" }
                                                                    }
                                                                }
                                                                td { class: "func-last-run",
                                                                    if let Some(met) = m {
                                                                        { format_last_run(&met.last_run) }
                                                                    } else {
                                                                        "—"
                                                                    }
                                                                }
                                                            }
                                                        }
                                                        if is_error_open {
                                                            tr { class: "func-error-detail-row",
                                                                td { colspan: "{col_span}",
                                                                    div { class: "func-error-detail-panel",
                                                                        if err_loading {
                                                                            div { class: "func-error-loading", "Fetching error details…" }
                                                                        } else if err_details.is_empty() {
                                                                            div { class: "func-error-empty", "No error details found." }
                                                                        } else {
                                                                            table { class: "func-error-table",
                                                                                thead {
                                                                                    tr {
                                                                                        th { "Time" }
                                                                                        th { "Code" }
                                                                                        th { "Details" }
                                                                                        th { }
                                                                                    }
                                                                                }
                                                                                tbody {
                                                                                    for e in &err_details {
                                                                                        {
                                                                                        let copy_text = format!("[{}] {} {}", e.timestamp, e.result_code, e.message);
                                                                                        rsx! {
                                                                                        tr {
                                                                                            td { class: "func-error-ts", { format_last_run(&e.timestamp) } }
                                                                                            td { class: "func-error-code", "{e.result_code}" }
                                                                                            td { class: "func-error-msg", "{e.message}" }
                                                                                            td { class: "func-error-copy-cell",
                                                                                                button {
                                                                                                    class: "func-error-copy-btn",
                                                                                                    title: "Copy line",
                                                                                                    onclick: move |_| {
                                                                                                        let _ = document::eval(&format!(
                                                                                                            "navigator.clipboard.writeText({:?})",
                                                                                                            copy_text
                                                                                                        ));
                                                                                                    },
                                                                                                    "⎘"
                                                                                                }
                                                                                            }
                                                                                        }
                                                                                        }}
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

                                // Summary metrics row when metrics are loaded
                                if !app_metrics.is_empty() {
                                    {
                                        let total_success: i64 = app_metrics.iter().map(|m| m.success).sum();
                                        let total_errors: i64 = app_metrics.iter().map(|m| m.errors).sum();
                                        let total = total_success + total_errors;
                                        let rate = if total > 0 { format!("{:.1}%", total_success as f64 / total as f64 * 100.0) } else { "N/A".into() };
                                        rsx! {
                                            div { class: "func-summary",
                                                span { class: "func-summary-item",
                                                    "Total: {total}"
                                                }
                                                span { class: "func-summary-item func-success",
                                                    "Success: {total_success}"
                                                }
                                                span { class: if total_errors > 0 { "func-summary-item func-errors has-errors" } else { "func-summary-item func-errors" },
                                                    "Errors: {total_errors}"
                                                }
                                                span { class: "func-summary-item",
                                                    "Rate: {rate}"
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                if !has_ai {
                    div { class: "func-note",
                        "No Application Insights found in this resource group. Metrics are unavailable."
                    }
                }
            }

            if let Some(pending) = pending_action.read().clone() {
                {
                    let command = pending.action.az_command(&az.resource_group, &pending.app_name);
                    let running = *action_running.read();
                    let err = action_error.read().clone();
                    let confirm_label = pending.action.label();
                    let app_name = pending.app_name.clone();
                    rsx! {
                        div { class: "modal-backdrop",
                            onclick: move |_| if !running { pending_action.set(None); },
                            div { class: "modal-card",
                                onclick: move |e: Event<MouseData>| e.stop_propagation(),
                                h3 { class: "modal-title", "{confirm_label} {app_name}?" }
                                p { class: "modal-body",
                                    "This runs:"
                                    br {}
                                    code { "{command}" }
                                }
                                if let Some(e) = err {
                                    div { class: "az-error", "{e}" }
                                }
                                div { class: "modal-actions",
                                    button {
                                        class: "btn btn-small",
                                        disabled: running,
                                        onclick: move |_| pending_action.set(None),
                                        "Cancel"
                                    }
                                    button {
                                        class: "btn btn-small btn-primary",
                                        disabled: running,
                                        onclick: {
                                            let pending = pending.clone();
                                            let mut run_action = run_action.clone();
                                            move |_| {
                                                let pending = pending.clone();
                                                run_action(pending);
                                            }
                                        },
                                        if running { "Running…" } else { "{confirm_label}" }
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

fn format_last_run(ts: &str) -> String {
    if ts.is_empty() { return "—".into(); }
    // Try to parse and show relative time
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(ts) {
        let now = chrono::Utc::now();
        let diff = now.signed_duration_since(dt);
        if diff.num_minutes() < 60 {
            format!("{}m ago", diff.num_minutes())
        } else if diff.num_hours() < 24 {
            format!("{}h ago", diff.num_hours())
        } else {
            format!("{}d ago", diff.num_days())
        }
    } else {
        // Fallback: show first 19 chars (datetime without fractional seconds)
        ts.get(..19).unwrap_or(ts).to_string()
    }
}

fn epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
