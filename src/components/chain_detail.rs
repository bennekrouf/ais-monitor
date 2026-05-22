use dioxus::prelude::*;
use crate::services::chain::ChainDetail;
use crate::services::{azure, names};
use crate::components::trigger_panel::TriggerPanel;
use std::collections::HashMap;

#[derive(Props, Clone, PartialEq)]
pub struct ChainDetailProps {
    pub chain: ChainDetail,
    pub deployed_workflows: Vec<String>,
    pub az_config: Option<AzConfig>,
    #[props(default)]
    pub logic_apps_dir: String,
    #[props(default)]
    pub chain_names: Signal<HashMap<String, String>>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AzConfig {
    pub subscription: String,
    pub resource_group: String,
    pub app_name: String,
    pub sb_namespace: String,
}

#[component]
pub fn ChainDetailView(props: ChainDetailProps) -> Element {
    let chain = &props.chain;
    let deployed: std::collections::HashSet<&str> = props.deployed_workflows.iter().map(|s| s.as_str()).collect();

    let mut run_statuses = use_signal(|| HashMap::<String, RunStatus>::new());
    let mut queue_statuses = use_signal(|| HashMap::<String, QueueStatus>::new());
    let mut loading = use_signal(|| false);
    let mut show_trigger = use_signal(|| false);
    let mut editing_name = use_signal(|| false);
    let mut name_input = use_signal(|| String::new());

    // Phase D: expanded step + actions
    let mut expanded_step = use_signal(|| Option::<String>::None);
    let mut step_actions = use_signal(|| HashMap::<String, Vec<ActionDetail>>::new());
    let mut loading_actions = use_signal(|| Option::<String>::None);

    let az = props.az_config.clone();
    let chain_steps: Vec<String> = chain.steps.iter().map(|s| s.workflow.clone()).collect();
    let chain_queues: Vec<String> = chain.queues.clone();
    let chain_label = chain.label.clone();

    let display_name = props.chain_names.read()
        .get(&chain_label)
        .cloned()
        .unwrap_or_else(|| chain_label.clone());

    let mut chain_names = props.chain_names;
    let dir = props.logic_apps_dir.clone();

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
                span { class: "detail-subtitle", "{chain.steps.len()} workflows · {chain.queues.len()} queues" }
                if az.is_some() {
                    button {
                        class: "btn",
                        onclick: move |_| {
                            let cur = *show_trigger.read();
                            show_trigger.set(!cur);
                        },
                        if *show_trigger.read() { "Hide Trigger" } else { "Trigger" }
                    }
                    button {
                        class: "btn btn-primary",
                        disabled: *loading.read(),
                        onclick: {
                            let az = az.clone().unwrap();
                            let steps = chain_steps.clone();
                            let queues = chain_queues.clone();
                            move |_| {
                                let az = az.clone();
                                let steps = steps.clone();
                                let queues = queues.clone();
                                loading.set(true);
                                spawn(async move {
                                    let mut statuses = HashMap::new();
                                    for wf in &steps {
                                        let az = az.clone();
                                        let wf_name = wf.clone();
                                        let wf_key = wf.clone();
                                        let result = tokio::task::spawn_blocking(move || {
                                            azure::list_runs(&az.subscription, &az.resource_group, &az.app_name, &wf_name, 1)
                                        }).await;
                                        let status = match result {
                                            Ok(Ok(runs)) if !runs.is_empty() => {
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
                                    queue_statuses.set(q_statuses);
                                    loading.set(false);
                                });
                            }
                        },
                        if *loading.read() { "Checking…" } else { "Check" }
                    }
                }
            }

            // Trigger panel
            if *show_trigger.read() {
                if let Some(ref az) = az {
                    TriggerPanel {
                        workflow: chain.steps[0].workflow.clone(),
                        az_config: az.clone(),
                        payloads_dir: props.logic_apps_dir.clone(),
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

            // Queue status
            if !chain.queues.is_empty() {
                div { class: "queue-section",
                    h3 { "Queues" }
                    for q in chain.queues.iter() {
                        {
                            let qs = queue_statuses.read().get(q).cloned();
                            let (active, dl) = qs.map(|s| (s.active, s.dead_letter)).unwrap_or((-1, -1));
                            let dl_class = if dl > 0 { "queue-dl warn" } else { "queue-dl" };
                            rsx! {
                                div { class: "queue-row",
                                    span { class: "queue-name", "{q}" }
                                    if active >= 0 {
                                        span { class: "queue-active", "active: {active}" }
                                        span { class: "{dl_class}", "dead-letter: {dl}" }
                                    } else {
                                        span { class: "queue-pending", "—" }
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
