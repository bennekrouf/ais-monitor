use dioxus::prelude::*;
use crate::services::chain::ChainDetail;
use crate::services::{azure, azure::EgLink, history_cache, kpi, names};
use crate::components::trigger_panel::TriggerPanel;
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct ChainHealth {
    pub success_rate: Option<f64>,
    pub dead_letters: i64,
    pub stuck_count: usize,
    pub failure_streak: usize,
}

#[derive(Props, Clone, PartialEq)]
pub struct ChainDetailProps {
    pub chain: ChainDetail,
    pub deployed_workflows: Vec<String>,
    pub az_config: Option<AzConfig>,
    #[props(default)]
    pub chain_names: Signal<HashMap<String, String>>,
    #[props(default)]
    pub chain_health: Option<Signal<HashMap<String, ChainHealth>>>,
    /// Per-chain, per-workflow raw run lists shared with MainScreen so
    /// "Check all" populates the per-workflow KPI columns the same way an
    /// individual "Check" does.
    #[props(default)]
    pub chain_runs: Option<Signal<HashMap<String, HashMap<String, Vec<azure::RunInfo>>>>>,
    #[props(default)]
    pub last_checked: Option<Signal<HashMap<String, u64>>>,
    /// Event Grid links: queue name → EG topic/subscription info
    #[props(default)]
    pub eg_links: Signal<HashMap<String, EgLink>>,
    /// Run-history sample size, shared with the parent screen so "Check all"
    /// and individual chain checks use identical depth and produce identical KPIs.
    #[props(default)]
    pub run_depth: Option<Signal<u32>>,
    /// Site location discovered by MainScreen — feeds the workflow Portal
    /// deep-link (which Azure rejects without an explicit region).
    #[props(default)]
    pub discovered_location: Signal<Option<String>>,
    /// SB namespace — falls back from `az_config.sb_namespace` to a value
    /// discovered by MainScreen if the profile didn't have one configured.
    #[props(default)]
    pub discovered_sb_namespace: Signal<Option<String>>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AzConfig {
    pub subscription: String,
    pub resource_group: String,
    pub app_name: String,
    pub sb_namespace: String,
    #[serde(default)]
    pub tenant: String,
    #[serde(default)]
    pub label: String,
    /// Optional path to the local logic apps workspace — used to suggest
    /// workflow-specific payloads in the trigger panel.
    #[serde(default)]
    pub local_dir: String,
}

#[component]
pub fn ChainDetailView(props: ChainDetailProps) -> Element {
    let chain = &props.chain;
    // `deployed_workflows` is currently every workflow that appears in the
    // discovered chains, so it adds no information per-row — kept on props
    // for future use (e.g. flagging steps referenced but not deployed).
    let _deployed_unused = &props.deployed_workflows;

    let mut run_statuses = use_signal(|| HashMap::<String, RunStatus>::new());
    let mut all_runs = use_signal(|| HashMap::<String, Vec<azure::RunInfo>>::new());
    let mut queue_statuses = use_signal(|| HashMap::<String, QueueStatus>::new());
    // Per-queue error message from the last check_queue() attempt. Lets the UI
    // tell the user *why* the active / dead-letter counts are missing (IP
    // restriction, RBAC, namespace gone, etc.) instead of silently showing "—".
    let mut queue_errors = use_signal(|| HashMap::<String, String>::new());
    let mut loading = use_signal(|| false);
    // Prefer the parent-provided run_depth signal so "Check all" sees the same
    // value. Fall back to a local signal if no parent passed one (e.g. tests).
    let local_run_depth = use_signal(|| 20u32);
    let mut run_depth = props.run_depth.unwrap_or(local_run_depth);
    let mut show_trigger = use_signal(|| false);
    let mut trigger_step_idx = use_signal(|| 0usize);
    let mut auto_poll = use_signal(|| false);
    let mut poll_interval = use_signal(|| 30u64);
    let mut editing_name = use_signal(|| false);
    let mut name_input = use_signal(|| String::new());

    // Send message to queue
    let mut send_queue: Signal<Option<String>> = use_signal(|| None);
    // Peek-DL state — which queue's DL preview is open, the loaded messages,
    // and a loading/error flag.
    let mut peek_queue: Signal<Option<String>> = use_signal(|| None);
    let mut peek_messages: Signal<Vec<azure::DeadLetterMessage>> = use_signal(Vec::new);
    let mut peek_loading: Signal<bool> = use_signal(|| false);
    let mut peek_error: Signal<Option<String>> = use_signal(|| None);
    let mut send_body: Signal<String> = use_signal(|| "{}".to_string());
    let mut send_status: Signal<Option<String>> = use_signal(|| None);
    let mut sending: Signal<bool> = use_signal(|| false);
    // Cache the connection string so we only fetch it once per session
    let mut sb_conn_str: Signal<Option<String>> = use_signal(|| None);

    // Phase D: expanded step + actions
    let mut expanded_step = use_signal(|| Option::<String>::None);
    // Actions cache keyed by (workflow, run_id) so we can browse any past run.
    let mut step_actions = use_signal(|| HashMap::<(String, String), Vec<ActionDetail>>::new());
    // Which run the user is currently viewing for each expanded workflow.
    let mut selected_run = use_signal(|| HashMap::<String, String>::new());
    // Loading flag keyed by (workflow, run_id).
    let mut loading_actions = use_signal(|| Option::<(String, String)>::None);

    let chain_health_signal = props.chain_health;
    let chain_runs_signal = props.chain_runs;
    let last_checked_signal = props.last_checked;
    // Signal is Copy — capture once and re-read inside each spawn closure so
    // queue checks pick up the namespace discovered after mount even when the
    // profile didn't have `sb_namespace` configured.
    let discovered_ns_sig = props.discovered_sb_namespace;

    // Hydrate local `all_runs` from the parent-owned `chain_runs` map (populated
    // by "Check all") so the per-workflow KPI columns render without the user
    // having to press the per-chain "Check" again. Re-runs whenever the parent
    // map updates or the user switches chain.
    {
        let label_for_hydrate = chain.label.clone();
        use_effect(move || {
            if let Some(parent) = chain_runs_signal {
                if all_runs.read().is_empty() {
                    if let Some(runs_for_chain) = parent.read().get(&label_for_hydrate).cloned() {
                        if !runs_for_chain.is_empty() {
                            all_runs.set(runs_for_chain);
                        }
                    }
                }
            }
        });
    }


    let az = props.az_config.clone();
    let chain_steps: Vec<String> = chain.steps.iter().map(|s| s.workflow.clone()).collect();
    let chain_queues: Vec<String> = chain.queues.clone();
    let chain_label = chain.label.clone();

    let display_name = props.chain_names.read()
        .get(&chain_label)
        .cloned()
        .unwrap_or_else(|| chain_label.clone());

    let mut chain_names = props.chain_names;
    let dir = az.as_ref()
        .map(|a| {
            dirs::config_dir()
                .unwrap_or_else(|| std::path::PathBuf::from("."))
                .join("ais-monitor")
                .join(format!("{}_{}", a.resource_group, a.app_name))
                .to_string_lossy()
                .to_string()
        })
        .unwrap_or_default();

    // Auto-poll effect
    use_effect({
        let az = az.clone();
        let steps = chain_steps.clone();
        let queues = chain_queues.clone();
        let label_for_health = chain_label.clone();
        let dir_for_history = dir.clone();
        move || {
            let is_on = *auto_poll.read();
            let interval_secs = *poll_interval.read();
            if !is_on { return; }
            let az = match az.clone() {
                Some(a) => a,
                None => return,
            };
            let steps = steps.clone();
            let dir_poll = dir_for_history.clone();
            let queues = queues.clone();
            let top = *run_depth.read();
            let lbl = label_for_health.clone();
            spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(interval_secs)).await;
                    if !*auto_poll.read() { break; }
                    loading.set(true);
                    let mut statuses = HashMap::new();
                    let mut runs_map = HashMap::new();
                    for wf in &steps {
                        let az = az.clone();
                        let wf_name = wf.clone();
                        let wf_key = wf.clone();
                        let result = tokio::task::spawn_blocking(move || {
                            azure::list_runs(&az.subscription, &az.resource_group, &az.app_name, &wf_name, top)
                        }).await;
                        let status = match result {
                            Ok(Ok(ref runs)) if !runs.is_empty() => {
                                runs_map.insert(wf_key.clone(), runs.clone());
                                RunStatus {
                                    run_id: runs[0].id.clone(),
                                    last_status: runs[0].status.clone(),
                                    last_time: runs[0].start.clone(),
                                }
                            }
                            _ => RunStatus {
                                run_id: String::new(),
                                last_status: "no runs".into(),
                                last_time: String::new(),
                            },
                        };
                        statuses.insert(wf_key, status);
                    }
                    run_statuses.set(statuses);
                    all_runs.set(runs_map.clone());
                    // Share with the parent so "Check all" and the chain list see it too.
                    if let Some(mut parent) = chain_runs_signal {
                        let mut m = parent.read().clone();
                        m.insert(lbl.clone(), runs_map.clone());
                        parent.set(m);
                    }

                    let mut q_statuses = HashMap::new();
                    let mut q_errors: HashMap<String, String> = HashMap::new();
                    for q in &queues {
                        let az = az.clone();
                        let q_name = q.clone();
                        let q_key = q.clone();
                        let ns = if !az.sb_namespace.is_empty() {
                            az.sb_namespace.clone()
                        } else {
                            discovered_ns_sig.read().clone().unwrap_or_default()
                        };
                        let rg = az.resource_group.clone();
                        let result = if ns.is_empty() {
                            Ok(Err("Service Bus namespace not configured for this profile and not discovered yet".to_string()))
                        } else {
                            tokio::task::spawn_blocking(move || {
                                azure::check_queue(&ns, &rg, &q_name)
                            }).await
                        };
                        match result {
                            Ok(Ok(info)) => {
                                q_statuses.insert(q_key, QueueStatus {
                                    active: info.active,
                                    dead_letter: info.dead_letter,
                                });
                            }
                            Ok(Err(e)) => { q_errors.insert(q_key, e); }
                            Err(e) => { q_errors.insert(q_key, format!("task: {e}")); }
                        }
                    }
                    queue_statuses.set(q_statuses.clone());
                    queue_errors.set(q_errors);
                    let health = compute_health(&runs_map, &q_statuses);
                    if let Some(mut health_sig) = chain_health_signal {
                        let mut map = health_sig.read().clone();
                        map.insert(lbl.clone(), health.clone());
                        health_sig.set(map);
                    }
                    if let Some(mut lc) = last_checked_signal {
                        let mut map = lc.read().clone();
                        map.insert(lbl.clone(), epoch_now());
                        lc.set(map);
                    }
                    // Append a history point for the sparkline.
                    append_history_point(&dir_poll, &lbl, &health);
                    loading.set(false);
                }
            });
        }
    });

    // Shared helper: fetch actions for (workflow, run_id) and cache them.
    // Used by both the initial expand and the failed-run list clicks.
    let fetch_actions = {
        let az = az.clone();
        move |wf: String, run_id: String| {
            // Skip if already cached or no az config
            if step_actions.read().contains_key(&(wf.clone(), run_id.clone())) { return; }
            let Some(az) = az.clone() else { return };
            if run_id.is_empty() { return; }
            let key = (wf.clone(), run_id.clone());
            loading_actions.set(Some(key.clone()));
            spawn(async move {
                let az2 = az.clone();
                let wf2 = wf.clone();
                let rid = run_id.clone();
                let result = tokio::task::spawn_blocking(move || {
                    azure::list_actions(&az2.subscription, &az2.resource_group, &az2.app_name, &wf2, &rid)
                }).await;
                if let Ok(Ok(actions)) = result {
                    let details: Vec<ActionDetail> = actions.iter().map(|a| ActionDetail {
                        name: a.name.clone(),
                        status: a.status.clone(),
                        error: a.error.clone(),
                    }).collect();
                    let mut map = step_actions.read().clone();
                    map.insert(key, details);
                    step_actions.set(map);
                }
                loading_actions.set(None);
            });
        }
    };

    rsx! {
        div { class: "chain-detail",
            // Header
            div { class: "detail-header",
                {
                    let is_editing = *editing_name.read();
                    if is_editing {
                        let label_save = chain_label.clone();
                        let dir_save = dir.clone();
                        rsx! {
                            input {
                                class: "rename-input",
                                r#type: "text",
                                value: "{name_input.read()}",
                                oninput: move |e| name_input.set(e.value().clone()),
                                onkeypress: {
                                    let label = label_save.clone();
                                    let dir = dir_save.clone();
                                    move |e: KeyboardEvent| {
                                        if e.key() == Key::Enter {
                                            let val = name_input.read().trim().to_string();
                                            let mut map = chain_names.read().clone();
                                            if val.is_empty() || val == label {
                                                map.remove(&label);
                                            } else {
                                                map.insert(label.clone(), val);
                                            }
                                            names::save(&dir, &map);
                                            chain_names.set(map);
                                            editing_name.set(false);
                                        }
                                    }
                                },
                            }
                            button {
                                class: "btn btn-small",
                                onclick: {
                                    let label = label_save.clone();
                                    let dir = dir_save.clone();
                                    move |_| {
                                        let val = name_input.read().trim().to_string();
                                        let mut map = chain_names.read().clone();
                                        if val.is_empty() || val == label {
                                            map.remove(&label);
                                        } else {
                                            map.insert(label.clone(), val);
                                        }
                                        names::save(&dir, &map);
                                        chain_names.set(map);
                                        editing_name.set(false);
                                    }
                                },
                                "Save"
                            }
                            button {
                                class: "btn btn-small",
                                onclick: move |_| editing_name.set(false),
                                "Cancel"
                            }
                        }
                    } else {
                        let dn = display_name.clone();
                        let has_custom = props.chain_names.read().contains_key(&chain_label);
                        rsx! {
                            h2 { "{dn}" }
                            if has_custom {
                                span { class: "detail-original", "{chain_label}" }
                            }
                            button {
                                class: "btn btn-small",
                                onclick: {
                                    let dn2 = dn.clone();
                                    move |_| {
                                        name_input.set(dn2.clone());
                                        editing_name.set(true);
                                    }
                                },
                                "Rename"
                            }
                        }
                    }
                }
                {
                    let s = chain.steps.len();
                    let q = chain.queues.len();
                    rsx! {
                        span { class: "detail-stat", title: "Workflows in this chain",
                            "⚙ {s}"
                        }
                        if q > 0 {
                            span { class: "detail-stat", title: "Queues in this chain",
                                "✉ {q}"
                            }
                        }
                        if let Some(ref a) = az {
                            {
                                let url = crate::services::portal_links::logic_app(
                                    &a.tenant, &a.subscription, &a.resource_group, &a.app_name,
                                );
                                rsx! {
                                    button {
                                        class: "portal-link portal-link-header",
                                        title: "Open Logic App in Azure Portal",
                                        onclick: move |_| crate::services::portal_links::open_in_browser(&url),
                                        "🔗 Portal"
                                    }
                                }
                            }
                        }
                        // Inline KPI chips — fill the gap between Portal and Trigger.
                        // Prefer fresh data from `all_runs` / `queue_statuses`;
                        // fall back to the cached health from "Check all".
                        {
                            let runs_data = all_runs.read();
                            let qs = queue_statuses.read();
                            let cached = chain_health_signal
                                .and_then(|sig| sig.read().get(&chain_label).cloned());

                            let (rate_opt, streak, dead_letters): (Option<f64>, usize, i64) =
                                if !runs_data.is_empty() {
                                    let mut total = 0usize;
                                    let mut ok = 0usize;
                                    let mut max_streak = 0usize;
                                    for s in chain.steps.iter() {
                                        if let Some(runs) = runs_data.get(&s.workflow) {
                                            let k = kpi::compute_workflow_kpi(runs);
                                            total += k.total_runs;
                                            ok += k.succeeded;
                                            if k.failure_streak > max_streak { max_streak = k.failure_streak; }
                                        }
                                    }
                                    let rate = if total > 0 { Some((ok as f64 / total as f64) * 100.0) } else { None };
                                    let dl: i64 = qs.values().map(|q| q.dead_letter).sum();
                                    (rate, max_streak, dl)
                                } else if let Some(h) = cached.as_ref() {
                                    (h.success_rate, h.failure_streak, h.dead_letters)
                                } else {
                                    (None, 0, 0)
                                };

                            let have_any = rate_opt.is_some() || !runs_data.is_empty() || cached.is_some();
                            let rate_cls = match rate_opt {
                                Some(r) if r >= 95.0 => "header-kpi-val kpi-good",
                                Some(r) if r >= 80.0 => "header-kpi-val kpi-warn",
                                Some(_) => "header-kpi-val kpi-bad",
                                None => "header-kpi-val",
                            };
                            let dl_cls = if dead_letters == 0 { "header-kpi-val kpi-good" } else { "header-kpi-val kpi-bad" };
                            let streak_cls = if streak == 0 { "header-kpi-val kpi-good" }
                                else if streak <= 2 { "header-kpi-val kpi-warn" }
                                else { "header-kpi-val kpi-bad" };

                            rsx! {
                                if have_any {
                                    div { class: "header-kpis",
                                        span { class: "header-kpi", title: "Success rate across the sampled run history",
                                            span { class: "header-kpi-lbl", "Succ" }
                                            span { class: "{rate_cls}",
                                                {match rate_opt {
                                                    Some(r) => format!("{:.1}%", r),
                                                    None => "—".into(),
                                                }}
                                            }
                                        }
                                        span { class: "header-kpi", title: "Dead-letter messages across this chain's queues",
                                            span { class: "header-kpi-lbl", "DL" }
                                            span { class: "{dl_cls}", "{dead_letters}" }
                                        }
                                        span { class: "header-kpi", title: "Longest consecutive recent-failure streak across the chain's workflows",
                                            span { class: "header-kpi-lbl", "Streak" }
                                            span { class: "{streak_cls}", "{streak}" }
                                        }
                                        if *loading.read() {
                                            span { class: "header-kpi-spinner", title: "Refreshing…", "" }
                                        }
                                    }
                                }
                            }
                        }
                        div { class: "detail-spacer" }
                    }
                }
                if az.is_some() {
                    button {
                        class: if *show_trigger.read() { "btn btn-trigger-toggle active" } else { "btn btn-trigger-toggle" },
                        title: if *show_trigger.read() { "Click to close the trigger panel" } else { "Open the trigger panel" },
                        onclick: move |_| {
                            let cur = *show_trigger.read();
                            show_trigger.set(!cur);
                        },
                        if *show_trigger.read() { "✕ Close Trigger" } else { "▶ Trigger" }
                    }
                    select {
                        class: "select-depth",
                        value: "{run_depth.read()}",
                        onchange: move |e: Event<FormData>| {
                            if let Ok(v) = e.value().parse::<u32>() {
                                run_depth.set(v);
                            }
                        },
                        option { value: "20", "Last 20" }
                        option { value: "50", "Last 50" }
                        option { value: "100", "Last 100" }
                    }
                    button {
                        class: "btn btn-primary",
                        disabled: *loading.read(),
                        onclick: {
                            let az = az.clone().unwrap();
                            let steps = chain_steps.clone();
                            let queues = chain_queues.clone();
                            let label_for_health = chain_label.clone();
                            let dir_for_check = dir.clone();
                            move |_| {
                                let az = az.clone();
                                let steps = steps.clone();
                                let queues = queues.clone();
                                let top = *run_depth.read();
                                let lbl = label_for_health.clone();
                                let dir_check = dir_for_check.clone();
                                loading.set(true);
                                spawn(async move {
                                    let mut statuses = HashMap::new();
                                    let mut runs_map = HashMap::new();
                                    for wf in &steps {
                                        let az = az.clone();
                                        let wf_name = wf.clone();
                                        let wf_key = wf.clone();
                                        let result = tokio::task::spawn_blocking(move || {
                                            azure::list_runs(&az.subscription, &az.resource_group, &az.app_name, &wf_name, top)
                                        }).await;
                                        let status = match result {
                                            Ok(Ok(ref runs)) if !runs.is_empty() => {
                                                runs_map.insert(wf_key.clone(), runs.clone());
                                                RunStatus {
                                                    run_id: runs[0].id.clone(),
                                                    last_status: runs[0].status.clone(),
                                                    last_time: runs[0].start.clone(),
                                                }
                                            }
                                            _ => RunStatus {
                                                run_id: String::new(),
                                                last_status: "no runs".into(),
                                                last_time: String::new(),
                                            },
                                        };
                                        statuses.insert(wf_key, status);
                                    }
                                    run_statuses.set(statuses);
                                    all_runs.set(runs_map.clone());
                                    if let Some(mut parent) = chain_runs_signal {
                                        let mut m = parent.read().clone();
                                        m.insert(lbl.clone(), runs_map.clone());
                                        parent.set(m);
                                    }

                                    let mut q_statuses = HashMap::new();
                                    let mut q_errors: HashMap<String, String> = HashMap::new();
                                    for q in &queues {
                                        let az = az.clone();
                                        let q_name = q.clone();
                                        let q_key = q.clone();
                                        let ns = if !az.sb_namespace.is_empty() {
                                            az.sb_namespace.clone()
                                        } else {
                                            discovered_ns_sig.read().clone().unwrap_or_default()
                                        };
                                        let rg = az.resource_group.clone();
                                        let result = if ns.is_empty() {
                                            Ok(Err("Service Bus namespace not configured for this profile and not discovered yet".to_string()))
                                        } else {
                                            tokio::task::spawn_blocking(move || {
                                                azure::check_queue(&ns, &rg, &q_name)
                                            }).await
                                        };
                                        match result {
                                            Ok(Ok(info)) => {
                                                q_statuses.insert(q_key, QueueStatus {
                                                    active: info.active,
                                                    dead_letter: info.dead_letter,
                                                });
                                            }
                                            Ok(Err(e)) => { q_errors.insert(q_key, e); }
                                            Err(e) => { q_errors.insert(q_key, format!("task: {e}")); }
                                        }
                                    }
                                    queue_statuses.set(q_statuses.clone());
                                    queue_errors.set(q_errors);
                                    let health = compute_health(&runs_map, &q_statuses);
                                    if let Some(mut health_sig) = chain_health_signal {
                                        let mut map = health_sig.read().clone();
                                        map.insert(lbl.clone(), health.clone());
                                        health_sig.set(map);
                                    }
                                    if let Some(mut lc) = last_checked_signal {
                                        let mut map = lc.read().clone();
                                        map.insert(lbl.clone(), epoch_now());
                                        lc.set(map);
                                    }
                                    append_history_point(&dir_check, &lbl, &health);
                                    loading.set(false);
                                });
                            }
                        },
                        if *loading.read() { "Checking…" } else { "Check" }
                    }
                    button {
                        class: if *auto_poll.read() { "btn btn-poll active" } else { "btn btn-poll" },
                        onclick: move |_| {
                            let cur = *auto_poll.read();
                            auto_poll.set(!cur);
                        },
                        if *auto_poll.read() { "Auto: ON" } else { "Auto: OFF" }
                    }
                    if *auto_poll.read() {
                        select {
                            class: "select-depth",
                            value: "{poll_interval.read()}",
                            onchange: move |e: Event<FormData>| {
                                if let Ok(v) = e.value().parse::<u64>() {
                                    poll_interval.set(v);
                                }
                            },
                            option { value: "15", "15s" }
                            option { value: "30", "30s" }
                            option { value: "60", "1m" }
                            option { value: "300", "5m" }
                        }
                    }
                }
            }

            // Trigger panel
            if *show_trigger.read() {
                if let Some(ref az) = az {
                    {
                        let step_names: Vec<String> = chain.steps.iter().map(|s| s.workflow.clone()).collect();
                        let idx = *trigger_step_idx.read();
                        let selected_wf = step_names.get(idx).cloned().unwrap_or_default();
                        let az_refresh = az.clone();
                        let steps_refresh = chain_steps.clone();
                        let queues_refresh = chain_queues.clone();
                        let top_refresh = *run_depth.read();
                        rsx! {
                            div { class: "trigger-section",
                                // Sticky header always visible at the top of the panel
                                div { class: "trigger-section-header",
                                    span { class: "trigger-section-title", "▶ Trigger" }
                                    div { class: "detail-spacer" }
                                    button {
                                        class: "btn-icon trigger-close-btn",
                                        title: "Close trigger panel (Esc)",
                                        onclick: move |_| show_trigger.set(false),
                                        "✕"
                                    }
                                }
                            if step_names.len() > 1 {
                                div { class: "trigger-step-select",
                                    span { class: "trigger-label", "Step" }
                                    select {
                                        class: "select-depth",
                                        value: "{idx}",
                                        onchange: move |e: Event<FormData>| {
                                            if let Ok(v) = e.value().parse::<usize>() {
                                                trigger_step_idx.set(v);
                                            }
                                        },
                                        for (i, name) in step_names.iter().enumerate() {
                                            option { value: "{i}", "{name}" }
                                        }
                                    }
                                }
                            }
                            TriggerPanel {
                                key: "{selected_wf}",
                                workflow: selected_wf,
                                az_config: az.clone(),
                                payloads_dir: dir.clone(),
                                local_dir: az.local_dir.clone(),
                                on_triggered: EventHandler::new({
                                    let az = az_refresh.clone();
                                    let steps = steps_refresh.clone();
                                    let queues = queues_refresh.clone();
                                    let lbl = chain_label.clone();
                                    let dir_for_refresh = dir.clone();
                                    move |_| {
                                        let az = az.clone();
                                        let steps = steps.clone();
                                        let queues = queues.clone();
                                        let lbl = lbl.clone();
                                        let dir_refresh = dir_for_refresh.clone();
                                        loading.set(true);
                                        spawn(async move {
                                            let mut statuses = HashMap::new();
                                            let mut runs_map = HashMap::new();
                                            for wf in &steps {
                                                let az = az.clone();
                                                let wf_name = wf.clone();
                                                let wf_key = wf.clone();
                                                let result = tokio::task::spawn_blocking(move || {
                                                    azure::list_runs(&az.subscription, &az.resource_group, &az.app_name, &wf_name, top_refresh)
                                                }).await;
                                                let status = match result {
                                                    Ok(Ok(ref runs)) if !runs.is_empty() => {
                                                        runs_map.insert(wf_key.clone(), runs.clone());
                                                        RunStatus {
                                                            run_id: runs[0].id.clone(),
                                                            last_status: runs[0].status.clone(),
                                                            last_time: runs[0].start.clone(),
                                                        }
                                                    }
                                                    _ => RunStatus {
                                                        run_id: String::new(),
                                                        last_status: "no runs".into(),
                                                        last_time: String::new(),
                                                    },
                                                };
                                                statuses.insert(wf_key, status);
                                            }
                                            run_statuses.set(statuses);
                                            all_runs.set(runs_map.clone());
                                            if let Some(mut parent) = chain_runs_signal {
                                                let mut m = parent.read().clone();
                                                m.insert(lbl.clone(), runs_map.clone());
                                                parent.set(m);
                                            }

                                            let mut q_statuses = HashMap::new();
                                            let mut q_errors: HashMap<String, String> = HashMap::new();
                                            for q in &queues {
                                                let az = az.clone();
                                                let q_name = q.clone();
                                                let q_key = q.clone();
                                                let ns = if !az.sb_namespace.is_empty() {
                                                    az.sb_namespace.clone()
                                                } else {
                                                    discovered_ns_sig.read().clone().unwrap_or_default()
                                                };
                                                let rg = az.resource_group.clone();
                                                let result = if ns.is_empty() {
                                                    Ok(Err("Service Bus namespace not configured for this profile and not discovered yet".to_string()))
                                                } else {
                                                    tokio::task::spawn_blocking(move || {
                                                        azure::check_queue(&ns, &rg, &q_name)
                                                    }).await
                                                };
                                                match result {
                                                    Ok(Ok(info)) => {
                                                        q_statuses.insert(q_key, QueueStatus {
                                                            active: info.active,
                                                            dead_letter: info.dead_letter,
                                                        });
                                                    }
                                                    Ok(Err(e)) => { q_errors.insert(q_key, e); }
                                                    Err(e) => { q_errors.insert(q_key, format!("task: {e}")); }
                                                }
                                            }
                                            queue_statuses.set(q_statuses.clone());
                                            queue_errors.set(q_errors);
                                            let health = compute_health(&runs_map, &q_statuses);
                                            if let Some(mut health_sig) = chain_health_signal {
                                                let mut map = health_sig.read().clone();
                                                map.insert(lbl.clone(), health.clone());
                                                health_sig.set(map);
                                            }
                                            if let Some(mut lc) = last_checked_signal {
                                                let mut map = lc.read().clone();
                                                map.insert(lbl.clone(), epoch_now());
                                                lc.set(map);
                                            }
                                            append_history_point(&dir_refresh, &lbl, &health);
                                            loading.set(false);
                                        });
                                    }
                                }),
                            }
                            }
                        }
                    }
                }
            }

            // KPI banner
            {
                let runs_data = all_runs.read();
                let has_data = !runs_data.is_empty();
                if has_data {
                    let workflow_kpis: Vec<(&String, kpi::ChainKpi)> = chain.steps.iter()
                        .filter_map(|s| {
                            runs_data.get(&s.workflow).map(|runs| (&s.workflow, kpi::compute_workflow_kpi(runs)))
                        })
                        .collect();
                    let avg_durations: Vec<f64> = workflow_kpis.iter().filter_map(|(_, k)| k.avg_duration_secs).collect();
                    let overall_avg = if avg_durations.is_empty() { None } else {
                        Some(avg_durations.iter().sum::<f64>() / avg_durations.len() as f64)
                    };
                    let p95_durations: Vec<f64> = workflow_kpis.iter().filter_map(|(_, k)| k.p95_duration_secs).collect();
                    let overall_p95 = p95_durations.iter().cloned().reduce(f64::max);

                    rsx! {
                        div { class: "kpi-banner",
                            div { class: "kpi-card",
                                div { class: "kpi-label", "Avg Duration" }
                                div { class: "kpi-value",
                                    {match overall_avg {
                                        Some(s) => format_duration(s),
                                        None => "—".into(),
                                    }}
                                }
                                div { class: "kpi-sub",
                                    {match overall_p95 {
                                        Some(s) => format!("p95: {}", format_duration(s)),
                                        None => String::new(),
                                    }}
                                }
                            }
                            // Stuck runs
                            {
                                let total_stuck: usize = workflow_kpis.iter().map(|(_, k)| k.stuck_runs.len()).sum();
                                let stuck_class = if total_stuck == 0 { "kpi-value kpi-good" } else { "kpi-value kpi-bad" };
                                rsx! {
                                    div { class: "kpi-card",
                                        div { class: "kpi-label", "Stuck Runs" }
                                        div { class: "{stuck_class}", "{total_stuck}" }
                                        div { class: "kpi-sub",
                                            if total_stuck > 0 { "exceeding p95" } else { "none" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                } else {
                    // Success/DL/Streak now live in the header chips; nothing to show here
                    // when there's no fresh run data yet.
                    rsx! {}
                }
            }

            // Steps table
            div { class: "steps-table",
                div { class: "steps-header",
                    span { class: "col col-status", "Status" }
                    span { class: "col col-name", "Workflow" }
                    span { class: "col col-link", "Link" }
                    span { class: "col col-rate", title: "Success rate over the sampled run history", "Success" }
                    span { class: "col col-runs", title: "Succeeded / total runs in the sample", "Runs" }
                    span { class: "col col-dur", title: "Average run duration", "Avg" }
                    span { class: "col col-streak", title: "Consecutive recent failures", "Streak" }
                    span { class: "col col-run", "Last Run" }
                }
                for (i, step) in chain.steps.iter().enumerate() {
                    {
                        let wf = step.workflow.clone();
                        let wf_click = wf.clone();
                        // Per-workflow KPI for this step (None if no runs fetched yet)
                        let wf_kpi = all_runs.read().get(&wf)
                            .map(|runs| kpi::compute_workflow_kpi(runs));
                        let run = run_statuses.read().get(&wf).cloned();
                        let status_icon = match run.as_ref().map(|r| r.last_status.as_str()) {
                            Some("Succeeded") => "✅",
                            Some("Failed") => "❌",
                            Some("Running") => "⏳",
                            Some("no runs") => "○",
                            None => "○",
                            _ => "⚠",
                        };
                        let has_run = run.as_ref().map_or(false, |r| !r.run_id.is_empty());
                        let link_display = if step.link_type.is_empty() {
                            if i == 0 { "trigger".to_string() } else { String::new() }
                        } else {
                            step.link_type.clone()
                        };
                        let link_class = if step.link_type.starts_with("queue:") { "link-queue" }
                            else if step.link_type == "EventGrid" { "link-eg" }
                            else if step.link_type == "invoke" { "link-invoke" }
                            else { "link-other" };
                        let time_short = run.as_ref()
                            .map(|r| {
                                r.last_time.split('T').nth(1)
                                    .and_then(|t| t.split('.').next())
                                    .unwrap_or(&r.last_time)
                                    .to_string()
                            })
                            .unwrap_or_default();

                        let is_expanded = expanded_step.read().as_ref() == Some(&wf);
                        let row_class = if is_expanded { "step-row step-row-expanded" }
                            else if has_run { "step-row step-row-clickable" }
                            else { "step-row" };

                        let run_for_click = run.clone();
                        let fetch_actions_row = fetch_actions.clone();

                        rsx! {
                            div {
                                class: "{row_class}",
                                onclick: move |_| {
                                    if !has_run { return; }
                                    let cur = expanded_step.read().clone();
                                    if cur.as_ref() == Some(&wf_click) {
                                        // Collapse
                                        expanded_step.set(None);
                                    } else {
                                        // Expand → default to the latest run, fetch its actions.
                                        expanded_step.set(Some(wf_click.clone()));
                                        if let Some(ref run) = run_for_click {
                                            let rid = run.run_id.clone();
                                            if !rid.is_empty() {
                                                let mut sel = selected_run.read().clone();
                                                sel.insert(wf_click.clone(), rid.clone());
                                                selected_run.set(sel);
                                                fetch_actions_row.clone()(wf_click.clone(), rid);
                                            }
                                        }
                                    }
                                },
                                span { class: "col col-status", "{status_icon}" }
                                span { class: "col col-name",
                                    if has_run {
                                        if is_expanded { "▼ " } else { "▶ " }
                                    }
                                    "{step.workflow}"
                                    // Portal link — stop_propagation so the row
                                    // doesn't toggle expansion when the icon is clicked.
                                    if let Some(ref a) = az {
                                        {
                                            let loc = props.discovered_location.read().clone();
                                            let url = crate::services::portal_links::workflow(
                                                &a.tenant, &a.subscription, &a.resource_group,
                                                &a.app_name, &step.workflow,
                                                loc.as_deref(),
                                            );
                                            rsx! {
                                                button {
                                                    class: "portal-link",
                                                    title: "Open this workflow in the Azure Portal",
                                                    onclick: move |e: Event<MouseData>| {
                                                        e.stop_propagation();
                                                        crate::services::portal_links::open_in_browser(&url);
                                                    },
                                                    "🔗"
                                                }
                                            }
                                        }
                                    }
                                }
                                span { class: "col col-link {link_class}", "{link_display}" }
                                {
                                    // Show EG source if this step's trigger queue is fed by Event Grid
                                    let eg_info = step.trigger_info.split(", ")
                                        .filter_map(|t| t.strip_prefix("ServiceBus:"))
                                        .find_map(|q| props.eg_links.read().get(q).cloned());
                                    if let Some(eg) = eg_info {
                                        let filter_summary: String = eg.filters.iter()
                                            .map(|f| format!("{} {} {}", f.key, f.operator, f.values.join(",")))
                                            .collect::<Vec<_>>()
                                            .join(" · ");
                                        rsx! {
                                            span { class: "col col-eg",
                                                title: "Fed by Event Grid: {eg.topic_name} → {eg.subscription_name}\n{filter_summary}",
                                                "⚡ {eg.topic_name}"
                                            }
                                        }
                                    } else { rsx! {} }
                                }
                                // ── Per-workflow KPI columns ────────────────────────
                                {
                                    match wf_kpi.as_ref() {
                                        Some(k) => {
                                            let rate_class = if k.total_runs == 0 { "col col-rate kpi-na" }
                                                else if k.success_rate >= 95.0 { "col col-rate kpi-good" }
                                                else if k.success_rate >= 80.0 { "col col-rate kpi-warn" }
                                                else { "col col-rate kpi-bad" };
                                            let streak_class = if k.failure_streak == 0 { "col col-streak kpi-good" }
                                                else if k.failure_streak <= 2 { "col col-streak kpi-warn" }
                                                else { "col col-streak kpi-bad" };
                                            let avg = k.avg_duration_secs
                                                .map(format_duration)
                                                .unwrap_or_else(|| "—".into());
                                            rsx! {
                                                span { class: "{rate_class}",
                                                    if k.total_runs > 0 { "{k.success_rate:.0}%" } else { "—" }
                                                }
                                                span { class: "col col-runs",
                                                    if k.total_runs > 0 { "{k.succeeded}/{k.total_runs}" } else { "—" }
                                                }
                                                span { class: "col col-dur", "{avg}" }
                                                span { class: "{streak_class}", "{k.failure_streak}" }
                                            }
                                        }
                                        None => rsx! {
                                            span { class: "col col-rate kpi-na", "—" }
                                            span { class: "col col-runs kpi-na", "—" }
                                            span { class: "col col-dur kpi-na", "—" }
                                            span { class: "col col-streak kpi-na", "—" }
                                        },
                                    }
                                }
                                span { class: "col col-run", "{time_short}" }
                            }

                            // Expanded actions panel
                            if is_expanded {
                                {
                                    // All runs in the sample (newest first as returned by list_runs)
                                    let all_runs_snap = all_runs.read();
                                    let runs_for_wf = all_runs_snap.get(&wf).cloned().unwrap_or_default();
                                    let failed_runs: Vec<azure::RunInfo> = runs_for_wf.iter()
                                        .filter(|r| r.status == "Failed")
                                        .cloned()
                                        .collect();
                                    // Currently-viewed run for this workflow
                                    let cur_run_id = selected_run.read().get(&wf).cloned().unwrap_or_default();
                                    let key = (wf.clone(), cur_run_id.clone());
                                    let loading_wf = loading_actions.read().clone();
                                    let is_loading = loading_wf.as_ref() == Some(&key);
                                    let actions = step_actions.read().get(&key).cloned().unwrap_or_default();
                                    // Latest run's id and time, for the "Latest" pill at the start of the list
                                    let latest_run = runs_for_wf.first().cloned();
                                    let fetch_clone = fetch_actions.clone();
                                    rsx! {
                                        div { class: "actions-panel",
                                            // ── Failed-runs picker ────────────────────────
                                            {
                                                let has_failed = !failed_runs.is_empty();
                                                let has_latest = latest_run.is_some();
                                                if has_failed || has_latest {
                                                    rsx! {
                                                        div { class: "failed-runs-bar",
                                                            span { class: "failed-runs-label",
                                                                if has_failed {
                                                                    "{failed_runs.len()} failed run(s) in sample — click to inspect:"
                                                                } else {
                                                                    "No failed runs in sample. Viewing:"
                                                                }
                                                            }
                                                            // Always offer the Latest run as a pill
                                                            if let Some(lr) = latest_run.as_ref() {
                                                                {
                                                                    let lr_id = lr.id.clone();
                                                                    let lr_status = lr.status.clone();
                                                                    let lr_time = run_time_short(&lr.start);
                                                                    let is_sel = cur_run_id == lr_id;
                                                                    let cls = if is_sel { "failed-run-pill failed-run-pill-current" } else { "failed-run-pill" };
                                                                    let icon = match lr_status.as_str() {
                                                                        "Succeeded" => "✅",
                                                                        "Failed" => "❌",
                                                                        "Running" => "⏳",
                                                                        _ => "○",
                                                                    };
                                                                    let wf_p = wf.clone();
                                                                    let lr_id_click = lr_id.clone();
                                                                    let fc = fetch_clone.clone();
                                                                    rsx! {
                                                                        button {
                                                                            class: "{cls}",
                                                                            title: "Latest run ({lr_status})",
                                                                            onclick: move |e: Event<MouseData>| {
                                                                                e.stop_propagation();
                                                                                let mut sel = selected_run.read().clone();
                                                                                sel.insert(wf_p.clone(), lr_id_click.clone());
                                                                                selected_run.set(sel);
                                                                                fc.clone()(wf_p.clone(), lr_id_click.clone());
                                                                            },
                                                                            "{icon} Latest · {lr_time}"
                                                                        }
                                                                    }
                                                                }
                                                            }
                                                            for fr in failed_runs.iter() {
                                                                {
                                                                    // Skip duplicate if latest is already failed
                                                                    let is_latest = latest_run.as_ref().map(|l| l.id == fr.id).unwrap_or(false);
                                                                    if is_latest { rsx! {} } else {
                                                                        let fr_id = fr.id.clone();
                                                                        let fr_time = run_time_short(&fr.start);
                                                                        let is_sel = cur_run_id == fr_id;
                                                                        let cls = if is_sel { "failed-run-pill failed-run-pill-bad failed-run-pill-current" } else { "failed-run-pill failed-run-pill-bad" };
                                                                        let wf_p = wf.clone();
                                                                        let fr_id_click = fr_id.clone();
                                                                        let fc = fetch_clone.clone();
                                                                        rsx! {
                                                                            button {
                                                                                class: "{cls}",
                                                                                title: "Failed run · {fr.start}",
                                                                                onclick: move |e: Event<MouseData>| {
                                                                                    e.stop_propagation();
                                                                                    let mut sel = selected_run.read().clone();
                                                                                    sel.insert(wf_p.clone(), fr_id_click.clone());
                                                                                    selected_run.set(sel);
                                                                                    fc.clone()(wf_p.clone(), fr_id_click.clone());
                                                                                },
                                                                                "❌ {fr_time}"
                                                                            }
                                                                        }
                                                                    }
                                                                }
                                                            }
                                                        }
                                                    }
                                                } else { rsx! {} }
                                            }

                                            // ── Actions for the selected run ─────────────
                                            if is_loading {
                                                div { class: "actions-loading", "Loading actions..." }
                                            } else if actions.is_empty() {
                                                div { class: "actions-loading", "No actions found" }
                                            } else {
                                                div { class: "actions-header",
                                                    span { class: "acol acol-status", "Status" }
                                                    span { class: "acol acol-name", "Action" }
                                                    span { class: "acol acol-error", "Error" }
                                                }
                                                for action in actions.iter() {
                                                    {
                                                        let action_icon = match action.status.as_str() {
                                                            "Succeeded" => "✅",
                                                            "Failed" => "❌",
                                                            "Skipped" => "⊘",
                                                            "Running" => "⏳",
                                                            _ => "○",
                                                        };
                                                        let action_name = action.name.clone();
                                                        let error_text = action.error.clone().unwrap_or_default();
                                                        let action_class = if action.status == "Failed" { "action-row action-failed" } else { "action-row" };
                                                        rsx! {
                                                            div { class: "{action_class}",
                                                                span { class: "acol acol-status", "{action_icon}" }
                                                                span { class: "acol acol-name", "{action_name}" }
                                                                span { class: "acol acol-error", "{error_text}" }
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

            // Parallel entries
            if !chain.parallel_entries.is_empty() {
                div { class: "parallel-section",
                    span { class: "parallel-label", "↳ also fed by" }
                    for entry in chain.parallel_entries.iter() {
                        span { class: "parallel-entry", "{entry}" }
                    }
                }
            }

            // Queue status — bordered table to match the Workflow steps table above.
            if !chain.queues.is_empty() {
                // IP-restriction banner — fires when every queue check failed
                // with a network/firewall-looking error. Most actionable cause
                // of empty Active / Dead-Letter cells, so call it out explicitly.
                {
                    let errs = queue_errors.read();
                    let ip_blocked: Vec<String> = errs.iter()
                        .filter(|(_, e)| looks_like_ip_block(e))
                        .map(|(q, _)| q.clone())
                        .collect();
                    if !ip_blocked.is_empty() {
                        let sample = errs.values().find(|e| looks_like_ip_block(e))
                            .cloned().unwrap_or_default();
                        rsx! {
                            div { class: "queue-ip-warn",
                                title: "{sample}",
                                "⚠ Active / Dead-Letter counts unavailable for {ip_blocked.len()} queue(s) — your client IP looks blocked by the Service Bus namespace's network rules. Add your IP to the namespace firewall allow-list (Azure Portal → Service Bus → Networking) and re-check."
                            }
                        }
                    } else if !errs.is_empty() {
                        // Non-IP errors — show a generic warning with the first error in tooltip.
                        let sample = errs.values().next().cloned().unwrap_or_default();
                        rsx! {
                            div { class: "queue-ip-warn",
                                title: "{sample}",
                                "⚠ Could not read queue counts for {errs.len()} queue(s). Hover for details."
                            }
                        }
                    } else { rsx! {} }
                }
                div { class: "queues-table",
                    div { class: "queues-header",
                        span { class: "qcol qcol-name", "Queue" }
                        span { class: "qcol qcol-active", title: "Active messages", "Active" }
                        span { class: "qcol qcol-dl", title: "Dead-letter messages", "Dead-Letter" }
                        span { class: "qcol qcol-actions" }
                    }
                    for q in chain.queues.iter() {
                        {
                            let qs = queue_statuses.read().get(q).cloned();
                            let (active, dl) = qs.map(|s| (s.active, s.dead_letter)).unwrap_or((-1, -1));
                            let q_send = q.clone();
                            let q_target = q.clone();
                            let q_peek = q.clone();
                            let is_open = send_queue.read().as_deref() == Some(q.as_str());
                            let is_peek_open = peek_queue.read().as_deref() == Some(q.as_str());
                            let az_send = az.clone();
                            // Compose status cells. Until the chain has been
                            // checked, active/dl are -1 → render an em-dash so
                            // the column line-up stays consistent across rows.
                            let active_text = if active >= 0 { active.to_string() } else { "—".into() };
                            let dl_text     = if dl >= 0     { dl.to_string()     } else { "—".into() };
                            let active_cls  = if active >= 0 { "qcol qcol-active" } else { "qcol qcol-active qcol-pending" };
                            let dl_cls      = if dl > 0 { "qcol qcol-dl warn" }
                                              else if dl == 0 { "qcol qcol-dl" }
                                              else { "qcol qcol-dl qcol-pending" };
                            let q_err = queue_errors.read().get(q).cloned();
                            let name_title = match &q_err {
                                Some(e) => format!("{q}\n\n⚠ {e}"),
                                None => q.clone(),
                            };
                            rsx! {
                                div { class: "queue-row",
                                    span { class: "qcol qcol-name", title: "{name_title}",
                                        "{q}"
                                        if q_err.is_some() {
                                            span { class: "queue-err-icon", "⚠" }
                                        }
                                        // Portal link — placed next to the queue name to match
                                        // the workflow rows above.
                                        if let Some(ref a) = az {
                                            {
                                                let ns = if !a.sb_namespace.is_empty() {
                                                    Some(a.sb_namespace.clone())
                                                } else {
                                                    props.discovered_sb_namespace.read().clone()
                                                };
                                                if let Some(ns) = ns {
                                                    let url = crate::services::portal_links::sb_queue(
                                                        &a.tenant, &a.subscription, &a.resource_group,
                                                        &ns, q,
                                                    );
                                                    rsx! {
                                                        button {
                                                            class: "portal-link",
                                                            title: "Open queue in Azure Portal (peek messages, DL contents, …)",
                                                            onclick: move |e: Event<MouseData>| {
                                                                e.stop_propagation();
                                                                crate::services::portal_links::open_in_browser(&url);
                                                            },
                                                            "🔗"
                                                        }
                                                    }
                                                } else { rsx! {} }
                                            }
                                        }
                                    }
                                    span { class: "{active_cls}", "{active_text}" }
                                    span { class: "{dl_cls}", "{dl_text}" }
                                    span { class: "qcol qcol-actions",
                                    // Peek dead-letter button — only shown when DL count > 0
                                    if az.is_some() && dl > 0 {
                                        {
                                            let az_peek = az.clone();
                                            rsx! {
                                                button {
                                                    class: if is_peek_open { "btn-icon sb-peek-btn active" } else { "btn-icon sb-peek-btn" },
                                                    title: "Peek dead-letter messages (non-destructive)",
                                                    onclick: move |_| {
                                                        if is_peek_open {
                                                            peek_queue.set(None);
                                                            peek_messages.set(Vec::new());
                                                            peek_error.set(None);
                                                            return;
                                                        }
                                                        let q_name = q_peek.clone();
                                                        let cached_conn = sb_conn_str.read().clone();
                                                        let az_ref = az_peek.clone();
                                                        peek_queue.set(Some(q_name.clone()));
                                                        peek_messages.set(Vec::new());
                                                        peek_error.set(None);
                                                        peek_loading.set(true);
                                                        spawn(async move {
                                                            if let Some(ref a) = az_ref {
                                                                let rg = a.resource_group.clone();
                                                                // Resolve namespace from config or discover.
                                                                let ns = if !a.sb_namespace.is_empty() { a.sb_namespace.clone() } else {
                                                                    let sub2 = a.subscription.clone();
                                                                    let rg2 = rg.clone();
                                                                    match tokio::task::spawn_blocking(move || azure::list_service_bus_namespaces(&sub2, &rg2)).await {
                                                                        Ok(Ok(mut list)) => list.drain(..).next().unwrap_or_default(),
                                                                        _ => String::new(),
                                                                    }
                                                                };
                                                                if ns.is_empty() {
                                                                    peek_error.set(Some("No Service Bus namespace configured for this profile".into()));
                                                                    peek_loading.set(false);
                                                                    return;
                                                                }
                                                                let conn = if let Some(c) = cached_conn { Ok(c) } else {
                                                                    let rg2 = rg.clone();
                                                                    let ns2 = ns.clone();
                                                                    tokio::task::spawn_blocking(move || azure::sb_get_connection_string(&rg2, &ns2))
                                                                        .await.unwrap_or_else(|e| Err(format!("{e}")))
                                                                };
                                                                match conn {
                                                                    Ok(cs) => {
                                                                        sb_conn_str.set(Some(cs.clone()));
                                                                        match azure::sb_peek_dead_letters(&cs, &q_name, 10).await {
                                                                            Ok(msgs) => {
                                                                                crate::services::activity::info(
                                                                                    "Peeked dead-letter messages",
                                                                                    format!("queue:{} ({} msg)", q_name, msgs.len()),
                                                                                );
                                                                                peek_messages.set(msgs);
                                                                            }
                                                                            Err(e) => {
                                                                                crate::services::activity::error(
                                                                                    "Peek DL failed",
                                                                                    format!("queue:{}", q_name),
                                                                                    e.clone(),
                                                                                );
                                                                                peek_error.set(Some(e));
                                                                            }
                                                                        }
                                                                    }
                                                                    Err(e) => peek_error.set(Some(format!("Auth: {e}"))),
                                                                }
                                                            }
                                                            peek_loading.set(false);
                                                        });
                                                    },
                                                    "🔍"
                                                }
                                            }
                                        }
                                    }
                                    if az.is_some() {
                                        button {
                                            class: if is_open { "btn-icon sb-send-btn active" } else { "btn-icon sb-send-btn" },
                                            title: "Send a message to this queue",
                                            onclick: move |_| {
                                                if is_open {
                                                    send_queue.set(None);
                                                } else {
                                                    send_queue.set(Some(q_send.clone()));
                                                    send_status.set(None);
                                                }
                                            },
                                            "📨"
                                        }
                                    }
                                    } // close qcol-actions
                                }
                                // ── Dead-letter peek panel ─────────────────
                                if is_peek_open {
                                    {
                                        let loading = *peek_loading.read();
                                        let err = peek_error.read().clone();
                                        let msgs = peek_messages.read().clone();
                                        rsx! {
                                            div { class: "sb-peek-panel",
                                                div { class: "sb-peek-header",
                                                    span { "🔍 Dead-letter messages — " }
                                                    strong { "{q}" }
                                                    span { class: "sb-peek-meta", " (peek-lock, non-destructive)" }
                                                }
                                                if loading {
                                                    div { class: "sb-peek-loading", "Peeking…" }
                                                } else if let Some(e) = err {
                                                    div { class: "sb-peek-error", "❌ {e}" }
                                                } else if msgs.is_empty() {
                                                    div { class: "sb-peek-loading",
                                                        "No messages visible right now. Some may be locked by other consumers."
                                                    }
                                                } else {
                                                    for (i, m) in msgs.iter().enumerate() {
                                                        DeadLetterRow { idx: i, msg: m.clone() }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                                // ── Send message form (inline, right under this queue) ──
                                if is_open {
                                    {
                                        let target_q = q_target.clone();
                                        let az_send = az_send.clone();
                                        rsx! {
                                            div { class: "sb-send-panel",
                                    div { class: "sb-send-header",
                                        span { "Send to " }
                                        strong { "{target_q}" }
                                        if let Some(ref a) = az_send {
                                            span { class: "sb-send-ns",
                                                " ({a.sb_namespace})"
                                            }
                                        }
                                    }
                                    textarea {
                                        class: "sb-send-body",
                                        rows: "8",
                                        placeholder: "Message body (JSON)…",
                                        value: "{send_body}",
                                        oninput: move |e| send_body.set(e.value()),
                                    }
                                    div { class: "sb-send-actions",
                                        button {
                                            class: "btn btn-small",
                                            disabled: *sending.read(),
                                            onclick: {
                                                let q = target_q.clone();
                                                let az_ref = az_send.clone();
                                                move |_| {
                                                    let q = q.clone();
                                                    let body = send_body.read().clone();
                                                    let az_ref = az_ref.clone();
                                                    let cached_conn = sb_conn_str.read().clone();
                                                    if let Some(ref a) = az_ref {
                                                        let rg = a.resource_group.clone();
                                                        let sub = a.subscription.clone();
                                                        let ns = a.sb_namespace.clone();
                                                        sending.set(true);
                                                        send_status.set(Some("Sending…".into()));
                                                        spawn(async move {
                                                            // Resolve namespace: use configured, or auto-discover from RG
                                                            let ns_resolved = if !ns.is_empty() {
                                                                Ok(ns)
                                                            } else {
                                                                let sub2 = sub.clone();
                                                                let rg2 = rg.clone();
                                                                send_status.set(Some("Discovering SB namespace…".into()));
                                                                tokio::task::spawn_blocking(move || {
                                                                    azure::list_service_bus_namespaces(&sub2, &rg2)
                                                                }).await
                                                                    .unwrap_or_else(|e| Err(format!("{e}")))
                                                                    .and_then(|list| {
                                                                        list.into_iter().next()
                                                                            .ok_or_else(|| "No SB namespace found in this resource group".into())
                                                                    })
                                                            };

                                                            let ns_name = match ns_resolved {
                                                                Ok(n) => n,
                                                                Err(e) => {
                                                                    send_status.set(Some(format!("❌ {e}")));
                                                                    sending.set(false);
                                                                    return;
                                                                }
                                                            };

                                                            // Get or fetch connection string
                                                            let conn = if let Some(c) = cached_conn {
                                                                Ok(c)
                                                            } else {
                                                                let rg2 = rg.clone();
                                                                let ns2 = ns_name.clone();
                                                                tokio::task::spawn_blocking(move || {
                                                                    azure::sb_get_connection_string(&rg2, &ns2)
                                                                }).await.unwrap_or_else(|e| Err(format!("{e}")))
                                                            };

                                                            match conn {
                                                                Ok(cs) => {
                                                                    sb_conn_str.set(Some(cs.clone()));
                                                                    match azure::sb_send_message(&cs, &q, &body).await {
                                                                        Ok(()) => {
                                                                            send_status.set(Some("✅ Message sent".into()));
                                                                            crate::services::activity::ok("Sent message", format!("queue:{}", q));
                                                                        }
                                                                        Err(e) => {
                                                                            send_status.set(Some(format!("❌ {e}")));
                                                                            crate::services::activity::error(
                                                                                "Send message failed",
                                                                                format!("queue:{}", q),
                                                                                e.clone(),
                                                                            );
                                                                        }
                                                                    }
                                                                }
                                                                Err(e) => {
                                                                    send_status.set(Some(format!("❌ Auth: {e}")));
                                                                    crate::services::activity::error(
                                                                        "Send message auth failed",
                                                                        format!("queue:{}", q),
                                                                        e.clone(),
                                                                    );
                                                                }
                                                            }
                                                            sending.set(false);
                                                        });
                                                    }
                                                }
                                            },
                                            if *sending.read() { "Sending…" } else { "▶ Send" }
                                        }
                                        button {
                                            class: "btn btn-small",
                                            onclick: move |_| send_queue.set(None),
                                            "Cancel"
                                        }
                                        if let Some(ref st) = *send_status.read() {
                                            span { class: "sb-send-status", "{st}" }
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

fn compute_health(
    runs_map: &HashMap<String, Vec<azure::RunInfo>>,
    q_statuses: &HashMap<String, QueueStatus>,
) -> ChainHealth {
    let all_kpis: Vec<kpi::ChainKpi> = runs_map.values()
        .map(|runs| kpi::compute_workflow_kpi(runs))
        .collect();
    let total: usize = all_kpis.iter().map(|k| k.total_runs).sum();
    let succeeded: usize = all_kpis.iter().map(|k| k.succeeded).sum();
    let rate = if total > 0 { Some((succeeded as f64 / total as f64) * 100.0) } else { None };
    let dl: i64 = q_statuses.values().map(|q| q.dead_letter).sum();
    let stuck: usize = all_kpis.iter().map(|k| k.stuck_runs.len()).sum();
    let streak = all_kpis.iter().map(|k| k.failure_streak).max().unwrap_or(0);
    ChainHealth { success_rate: rate, dead_letters: dl, stuck_count: stuck, failure_streak: streak }
}

/// Pretty-prints an ISO-8601 timestamp as a short relative/absolute string
/// for the failed-runs picker. Falls back gracefully on parse failure.
fn run_time_short(ts: &str) -> String {
    if ts.is_empty() { return "—".into(); }
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(ts) {
        let now = chrono::Utc::now();
        let diff = now.signed_duration_since(dt);
        if diff.num_minutes() < 1 {
            return "just now".into();
        }
        if diff.num_minutes() < 60 {
            return format!("{}m ago", diff.num_minutes());
        }
        if diff.num_hours() < 24 {
            return format!("{}h ago", diff.num_hours());
        }
        if diff.num_days() < 7 {
            return format!("{}d ago", diff.num_days());
        }
        // Fall back to a short date for older runs
        return dt.format("%b %d").to_string();
    }
    // Last resort: best-effort time portion of the raw string
    ts.split('T').nth(1)
        .and_then(|t| t.split('.').next())
        .unwrap_or(ts)
        .to_string()
}

fn format_duration(secs: f64) -> String {
    if secs < 1.0 {
        format!("{:.0}ms", secs * 1000.0)
    } else if secs < 60.0 {
        format!("{:.1}s", secs)
    } else if secs < 3600.0 {
        format!("{:.1}m", secs / 60.0)
    } else {
        format!("{:.1}h", secs / 3600.0)
    }
}

#[derive(Clone, Debug, PartialEq)]
struct RunStatus {
    run_id: String,
    last_status: String,
    last_time: String,
}

#[derive(Clone, Debug, PartialEq)]
struct QueueStatus {
    active: i64,
    dead_letter: i64,
}

#[derive(Clone, Debug, PartialEq)]
struct ActionDetail {
    name: String,
    status: String,
    error: Option<String>,
}

/// Sniff a check_queue error message for tell-tale signs that the user's
/// client IP is blocked by the Service Bus namespace's network rules.
/// Returns `true` for both data-plane ("Ip has been prevented…") and
/// control-plane ("Public network access is disabled" / firewall) variants,
/// plus the generic Azure firewall denial wording.
fn looks_like_ip_block(err: &str) -> bool {
    let e = err.to_ascii_lowercase();
    e.contains("ip has been prevented")
        || e.contains("public network access is disabled")
        || e.contains("not allowed to access")
        || (e.contains("firewall") && e.contains("denied"))
        || e.contains("ipnotallowed")
        || e.contains("networkrulesetrestriction")
        || e.contains("forbiddenip")
}

fn epoch_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// One dead-letter row with expand-on-click to reveal the body.
#[derive(Props, Clone, PartialEq)]
struct DeadLetterRowProps {
    idx: usize,
    msg: azure::DeadLetterMessage,
}

#[component]
fn DeadLetterRow(props: DeadLetterRowProps) -> Element {
    let mut expanded = use_signal(|| false);
    let is_exp = *expanded.read();
    let m = &props.msg;
    let reason = if m.dead_letter_reason.is_empty() { "—" } else { m.dead_letter_reason.as_str() };
    let when = if m.enqueued_time.is_empty() { String::new() } else {
        run_time_short(&m.enqueued_time)
    };
    let copy_payload = m.body.clone();

    rsx! {
        div {
            class: if is_exp { "sb-dl-row expanded" } else { "sb-dl-row" },
            onclick: move |_| { let v = !*expanded.peek(); expanded.set(v); },
            span { class: "sb-dl-idx", "#{props.idx + 1}" }
            span { class: "sb-dl-reason", title: "DeadLetterReason", "{reason}" }
            span { class: "sb-dl-when", "{when}" }
            span { class: "sb-dl-delivery", title: "Delivery count", "⤴ {m.delivery_count}" }
            span { class: "sb-dl-expand-hint", if is_exp { "▼" } else { "▶" } }
        }
        if is_exp {
            div { class: "sb-dl-detail",
                if !m.dead_letter_description.is_empty() {
                    div { class: "sb-dl-error-block",
                        div { class: "sb-dl-label", "Error description" }
                        pre { "{m.dead_letter_description}" }
                    }
                }
                div { class: "sb-dl-body-block",
                    div { class: "sb-dl-body-header",
                        span { class: "sb-dl-label", "Body" }
                        if !m.message_id.is_empty() {
                            span { class: "sb-dl-mid", "MessageId: {m.message_id}" }
                        }
                        button {
                            class: "btn btn-small",
                            title: "Copy body to clipboard",
                            onclick: move |e: Event<MouseData>| {
                                e.stop_propagation();
                                let payload = copy_payload.clone();
                                spawn(async move {
                                    tokio::task::spawn_blocking(move || {
                                        if let Ok(mut cb) = arboard::Clipboard::new() {
                                            let _ = cb.set_text(payload);
                                        }
                                    }).await.ok();
                                });
                            },
                            "⎘"
                        }
                    }
                    pre { class: "sb-dl-body", "{m.body}" }
                }
            }
        }
    }
}

/// Append a per-chain history point so the chain list can draw a sparkline.
/// Cheap — bounded to MAX_PER_CHAIN entries on disk.
fn append_history_point(workspace_dir: &str, chain_label: &str, health: &ChainHealth) {
    if workspace_dir.is_empty() { return; }
    let point = history_cache::HealthPoint {
        ts: epoch_now(),
        success_rate: health.success_rate,
        dead_letters: health.dead_letters,
        stuck_count: health.stuck_count,
        failure_streak: health.failure_streak,
    };
    let dir = workspace_dir.to_string();
    let label = chain_label.to_string();
    // Disk write is small but we still detach so we don't block the UI thread.
    std::thread::spawn(move || {
        history_cache::append(&dir, &label, point);
    });
}
