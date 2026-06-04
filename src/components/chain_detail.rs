use dioxus::prelude::*;
use crate::services::chain::ChainDetail;
use crate::services::{azure, azure::EgLink, kpi, names};
use crate::components::trigger_panel::TriggerPanel;
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq, Default)]
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
    #[props(default)]
    pub last_checked: Option<Signal<HashMap<String, u64>>>,
    /// Event Grid links: queue name → EG topic/subscription info
    #[props(default)]
    pub eg_links: Signal<HashMap<String, EgLink>>,
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
    let deployed: std::collections::HashSet<&str> = props.deployed_workflows.iter().map(|s| s.as_str()).collect();

    let mut run_statuses = use_signal(|| HashMap::<String, RunStatus>::new());
    let mut all_runs = use_signal(|| HashMap::<String, Vec<azure::RunInfo>>::new());
    let mut queue_statuses = use_signal(|| HashMap::<String, QueueStatus>::new());
    let mut loading = use_signal(|| false);
    let mut run_depth = use_signal(|| 20u32);
    let mut show_trigger = use_signal(|| false);
    let mut trigger_step_idx = use_signal(|| 0usize);
    let mut auto_poll = use_signal(|| false);
    let mut poll_interval = use_signal(|| 30u64);
    let mut editing_name = use_signal(|| false);
    let mut name_input = use_signal(|| String::new());

    // Send message to queue
    let mut send_queue: Signal<Option<String>> = use_signal(|| None);
    let mut send_body: Signal<String> = use_signal(|| "{}".to_string());
    let mut send_status: Signal<Option<String>> = use_signal(|| None);
    let mut sending: Signal<bool> = use_signal(|| false);
    // Cache the connection string so we only fetch it once per session
    let mut sb_conn_str: Signal<Option<String>> = use_signal(|| None);

    // Phase D: expanded step + actions
    let mut expanded_step = use_signal(|| Option::<String>::None);
    let mut step_actions = use_signal(|| HashMap::<String, Vec<ActionDetail>>::new());
    let mut loading_actions = use_signal(|| Option::<String>::None);

    let chain_health_signal = props.chain_health;
    let last_checked_signal = props.last_checked;

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
        move || {
            let is_on = *auto_poll.read();
            let interval_secs = *poll_interval.read();
            if !is_on { return; }
            let az = match az.clone() {
                Some(a) => a,
                None => return,
            };
            let steps = steps.clone();
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

                    let mut q_statuses = HashMap::new();
                    for q in &queues {
                        let az = az.clone();
                        let q_name = q.clone();
                        let q_key = q.clone();
                        let result = tokio::task::spawn_blocking(move || {
                            azure::check_queue(&az.sb_namespace, &az.resource_group, &q_name)
                        }).await;
                        if let Ok(Ok(info)) = result {
                            q_statuses.insert(q_key, QueueStatus {
                                active: info.active,
                                dead_letter: info.dead_letter,
                            });
                        }
                    }
                    queue_statuses.set(q_statuses.clone());
                    if let Some(mut health_sig) = chain_health_signal {
                        let health = compute_health(&runs_map, &q_statuses);
                        let mut map = health_sig.read().clone();
                        map.insert(lbl.clone(), health);
                        health_sig.set(map);
                    }
                    if let Some(mut lc) = last_checked_signal {
                        let mut map = lc.read().clone();
                        map.insert(lbl.clone(), epoch_now());
                        lc.set(map);
                    }
                    loading.set(false);
                }
            });
        }
    });

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
                span { class: "detail-subtitle",
                    {
                        let s = chain.steps.len();
                        let q = chain.queues.len();
                        let steps_str  = if s == 1 { "1 workflow".to_string()  } else { format!("{s} workflows") };
                        let queues_str = if q == 0 { String::new() }
                                         else if q == 1 { " · 1 queue".to_string() }
                                         else { format!(" · {q} queues") };
                        format!("{steps_str}{queues_str}")
                    }
                }
                if az.is_some() {
                    button {
                        class: "btn",
                        onclick: move |_| {
                            let cur = *show_trigger.read();
                            show_trigger.set(!cur);
                        },
                        if *show_trigger.read() { "Hide Trigger" } else { "Trigger" }
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
                            move |_| {
                                let az = az.clone();
                                let steps = steps.clone();
                                let queues = queues.clone();
                                let top = *run_depth.read();
                                let lbl = label_for_health.clone();
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

                                    let mut q_statuses = HashMap::new();
                                    for q in &queues {
                                        let az = az.clone();
                                        let q_name = q.clone();
                                        let q_key = q.clone();
                                        let result = tokio::task::spawn_blocking(move || {
                                            azure::check_queue(&az.sb_namespace, &az.resource_group, &q_name)
                                        }).await;
                                        if let Ok(Ok(info)) = result {
                                            q_statuses.insert(q_key, QueueStatus {
                                                active: info.active,
                                                dead_letter: info.dead_letter,
                                            });
                                        }
                                    }
                                    queue_statuses.set(q_statuses.clone());
                                    if let Some(mut health_sig) = chain_health_signal {
                                        let health = compute_health(&runs_map, &q_statuses);
                                        let mut map = health_sig.read().clone();
                                        map.insert(lbl.clone(), health);
                                        health_sig.set(map);
                                    }
                                    if let Some(mut lc) = last_checked_signal {
                                        let mut map = lc.read().clone();
                                        map.insert(lbl.clone(), epoch_now());
                                        lc.set(map);
                                    }
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
                                    move |_| {
                                        let az = az.clone();
                                        let steps = steps.clone();
                                        let queues = queues.clone();
                                        let lbl = lbl.clone();
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

                                            let mut q_statuses = HashMap::new();
                                            for q in &queues {
                                                let az = az.clone();
                                                let q_name = q.clone();
                                                let q_key = q.clone();
                                                let result = tokio::task::spawn_blocking(move || {
                                                    azure::check_queue(&az.sb_namespace, &az.resource_group, &q_name)
                                                }).await;
                                                if let Ok(Ok(info)) = result {
                                                    q_statuses.insert(q_key, QueueStatus {
                                                        active: info.active,
                                                        dead_letter: info.dead_letter,
                                                    });
                                                }
                                            }
                                            queue_statuses.set(q_statuses.clone());
                                            if let Some(mut health_sig) = chain_health_signal {
                                                let health = compute_health(&runs_map, &q_statuses);
                                                let mut map = health_sig.read().clone();
                                                map.insert(lbl.clone(), health);
                                                health_sig.set(map);
                                            }
                                            if let Some(mut lc) = last_checked_signal {
                                                let mut map = lc.read().clone();
                                                map.insert(lbl.clone(), epoch_now());
                                                lc.set(map);
                                            }
                                            loading.set(false);
                                        });
                                    }
                                }),
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
                    let total_runs: usize = workflow_kpis.iter().map(|(_, k)| k.total_runs).sum();
                    let total_succeeded: usize = workflow_kpis.iter().map(|(_, k)| k.succeeded).sum();
                    let total_failed: usize = workflow_kpis.iter().map(|(_, k)| k.failed).sum();
                    let overall_rate = if total_runs > 0 { (total_succeeded as f64 / total_runs as f64) * 100.0 } else { 0.0 };
                    let max_streak = workflow_kpis.iter().map(|(_, k)| k.failure_streak).max().unwrap_or(0);
                    let avg_durations: Vec<f64> = workflow_kpis.iter().filter_map(|(_, k)| k.avg_duration_secs).collect();
                    let overall_avg = if avg_durations.is_empty() { None } else {
                        Some(avg_durations.iter().sum::<f64>() / avg_durations.len() as f64)
                    };
                    let p95_durations: Vec<f64> = workflow_kpis.iter().filter_map(|(_, k)| k.p95_duration_secs).collect();
                    let overall_p95 = p95_durations.iter().cloned().reduce(f64::max);

                    let rate_class = if overall_rate >= 95.0 { "kpi-value kpi-good" }
                        else if overall_rate >= 80.0 { "kpi-value kpi-warn" }
                        else { "kpi-value kpi-bad" };
                    let streak_class = if max_streak == 0 { "kpi-value kpi-good" }
                        else if max_streak <= 2 { "kpi-value kpi-warn" }
                        else { "kpi-value kpi-bad" };

                    rsx! {
                        div { class: "kpi-banner",
                            div { class: "kpi-card",
                                div { class: "kpi-label", "Success Rate" }
                                div { class: "{rate_class}", "{overall_rate:.1}%" }
                                div { class: "kpi-sub", "{total_succeeded}/{total_runs} runs" }
                            }
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
                            div { class: "kpi-card",
                                div { class: "kpi-label", "Failure Streak" }
                                div { class: "{streak_class}", "{max_streak}" }
                                div { class: "kpi-sub",
                                    if total_failed > 0 { "{total_failed} failed total" } else { "all clear" }
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
                            // Dead letters
                            {
                                let qs = queue_statuses.read();
                                let total_dl: i64 = qs.values().map(|q| q.dead_letter).sum();
                                let dl_class = if total_dl == 0 { "kpi-value kpi-good" } else { "kpi-value kpi-bad" };
                                if !qs.is_empty() {
                                    rsx! {
                                        div { class: "kpi-card",
                                            div { class: "kpi-label", "Dead Letters" }
                                            div { class: "{dl_class}", "{total_dl}" }
                                            div { class: "kpi-sub",
                                                if total_dl > 0 { "messages need attention" } else { "all clear" }
                                            }
                                        }
                                    }
                                } else {
                                    rsx! {}
                                }
                            }
                            // Per-workflow breakdown for chains with multiple steps
                            if workflow_kpis.len() > 1 {
                                for (wf_name, wk) in workflow_kpis.iter() {
                                    {
                                        let wf_rate_class = if wk.success_rate >= 95.0 { "kpi-value kpi-good" }
                                            else if wk.success_rate >= 80.0 { "kpi-value kpi-warn" }
                                            else { "kpi-value kpi-bad" };
                                        rsx! {
                                            div { class: "kpi-card kpi-card-small",
                                                div { class: "kpi-label", "{wf_name}" }
                                                div { class: "{wf_rate_class}", "{wk.success_rate:.0}%" }
                                                div { class: "kpi-sub",
                                                    {match wk.avg_duration_secs {
                                                        Some(s) => format_duration(s),
                                                        None => "—".into(),
                                                    }}
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                } else {
                    // No fresh data yet — show pre-computed health from "Check all" if available
                    let cached_health = chain_health_signal
                        .and_then(|sig| sig.read().get(&chain_label).cloned());
                    if let Some(h) = cached_health {
                        let rate_val = h.success_rate.unwrap_or(0.0);
                        let rate_class = if rate_val >= 95.0 { "kpi-value kpi-good" }
                            else if rate_val >= 80.0 { "kpi-value kpi-warn" }
                            else { "kpi-value kpi-bad" };
                        let streak_class = if h.failure_streak == 0 { "kpi-value kpi-good" }
                            else if h.failure_streak <= 2 { "kpi-value kpi-warn" }
                            else { "kpi-value kpi-bad" };
                        rsx! {
                            div { class: "kpi-banner",
                                // Faint "cached" indicator
                                div { style: "width:100%; font-size:10px; opacity:0.45; padding:0 4px 4px; font-style:italic;",
                                    "from last check — loading fresh data…"
                                }
                                div { class: "kpi-card",
                                    div { class: "kpi-label", "Success Rate" }
                                    if let Some(rate) = h.success_rate {
                                        div { class: "{rate_class}", "{rate:.1}%" }
                                    } else {
                                        div { class: "kpi-value", "—" }
                                    }
                                }
                                div { class: "kpi-card",
                                    div { class: "kpi-label", "Dead Letters" }
                                    {
                                        let dl_class = if h.dead_letters > 0 { "kpi-value kpi-bad" } else { "kpi-value kpi-good" };
                                        rsx! { div { class: "{dl_class}", "{h.dead_letters}" } }
                                    }
                                }
                                div { class: "kpi-card",
                                    div { class: "kpi-label", "Failure Streak" }
                                    div { class: "{streak_class}", "{h.failure_streak}" }
                                }
                                if h.stuck_count > 0 {
                                    div { class: "kpi-card",
                                        div { class: "kpi-label", "Stuck" }
                                        div { class: "kpi-value kpi-bad", "{h.stuck_count}" }
                                    }
                                }
                            }
                        }
                    } else {
                        rsx! {}
                    }
                }
            }

            // Steps table
            div { class: "steps-table",
                div { class: "steps-header",
                    span { class: "col col-status", "Status" }
                    span { class: "col col-name", "Workflow" }
                    span { class: "col col-link", "Link" }
                    span { class: "col col-deployed", "Deployed" }
                    span { class: "col col-run", "Last Run" }
                }
                for (i, step) in chain.steps.iter().enumerate() {
                    {
                        let wf = step.workflow.clone();
                        let wf_click = wf.clone();
                        let is_deployed = deployed.contains(step.workflow.as_str());
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

                        let az_for_click = az.clone();
                        let run_for_click = run.clone();

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
                                        // Expand and fetch actions if not cached
                                        expanded_step.set(Some(wf_click.clone()));
                                        let already = step_actions.read().contains_key(&wf_click);
                                        if !already {
                                            if let Some(ref az) = az_for_click {
                                                if let Some(ref run) = run_for_click {
                                                    let az = az.clone();
                                                    let wf_name = wf_click.clone();
                                                    let run_id = run.run_id.clone();
                                                    let wf_key = wf_click.clone();
                                                    loading_actions.set(Some(wf_key.clone()));
                                                    spawn(async move {
                                                        let az2 = az.clone();
                                                        let wf2 = wf_name.clone();
                                                        let rid = run_id.clone();
                                                        let result = tokio::task::spawn_blocking(move || {
                                                            azure::list_actions(&az2.subscription, &az2.resource_group, &az2.app_name, &wf2, &rid)
                                                        }).await;
                                                        if let Ok(Ok(actions)) = result {
                                                            let details: Vec<ActionDetail> = actions.iter().map(|a| {
                                                                ActionDetail {
                                                                    name: a.name.clone(),
                                                                    status: a.status.clone(),
                                                                    error: a.error.clone(),
                                                                }
                                                            }).collect();
                                                            let mut map = step_actions.read().clone();
                                                            map.insert(wf_key.clone(), details);
                                                            step_actions.set(map);
                                                        }
                                                        loading_actions.set(None);
                                                    });
                                                }
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
                                span {
                                    class: if is_deployed { "col col-deployed deployed-yes" } else { "col col-deployed deployed-no" },
                                    if is_deployed { "✓" } else { "✗" }
                                }
                                span { class: "col col-run", "{time_short}" }
                            }

                            // Expanded actions panel
                            if is_expanded {
                                {
                                    let loading_wf = loading_actions.read().clone();
                                    let is_loading = loading_wf.as_ref() == Some(&wf);
                                    let actions = step_actions.read().get(&wf).cloned().unwrap_or_default();
                                    rsx! {
                                        div { class: "actions-panel",
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

            // Queue status
            if !chain.queues.is_empty() {
                div { class: "queue-section",
                    h3 { "Queues" }
                    for q in chain.queues.iter() {
                        {
                            let qs = queue_statuses.read().get(q).cloned();
                            let (active, dl) = qs.map(|s| (s.active, s.dead_letter)).unwrap_or((-1, -1));
                            let dl_class = if dl > 0 { "queue-dl warn" } else { "queue-dl" };
                            let q_send = q.clone();
                            let is_open = send_queue.read().as_deref() == Some(q.as_str());
                            rsx! {
                                div { class: "queue-row",
                                    span { class: "queue-name", "{q}" }
                                    if active >= 0 {
                                        span { class: "queue-active", "active: {active}" }
                                        span { class: "{dl_class}", "dead-letter: {dl}" }
                                    } else {
                                        span { class: "queue-pending", "—" }
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
                                }
                            }
                        }
                    }

                    // ── Send message form ─────────────────────────────────
                    if let Some(ref target_q) = *send_queue.read() {
                        {
                            let target_q = target_q.clone();
                            let az_send = az.clone();
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
                                                                        Ok(()) => send_status.set(Some("✅ Message sent".into())),
                                                                        Err(e) => send_status.set(Some(format!("❌ {e}"))),
                                                                    }
                                                                }
                                                                Err(e) => send_status.set(Some(format!("❌ Auth: {e}"))),
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

fn epoch_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
