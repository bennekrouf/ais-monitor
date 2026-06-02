use dioxus::prelude::*;
use crate::services::chain::ChainDetail;
use crate::components::chain_detail::ChainHealth;
use std::collections::HashMap;

#[derive(Props, Clone, PartialEq)]
pub struct ChainListProps {
    pub chains: Vec<ChainDetail>,
    pub selected: Option<String>,
    pub on_select: EventHandler<String>,
    #[props(default)]
    pub chain_names: HashMap<String, String>,
    #[props(default)]
    pub chain_health: HashMap<String, ChainHealth>,
    #[props(default)]
    pub last_checked: HashMap<String, u64>,
}

#[component]
pub fn ChainList(props: ChainListProps) -> Element {
    if props.chains.is_empty() {
        return rsx! {
            div { class: "chain-list empty",
                p { "No chains discovered" }
            }
        };
    }

    let mut sorted = props.chains.clone();
    sorted.sort_by(|a, b| {
        let a_ts = props.last_checked.get(&a.label).copied().unwrap_or(0);
        let b_ts = props.last_checked.get(&b.label).copied().unwrap_or(0);
        if a_ts != b_ts {
            return b_ts.cmp(&a_ts);
        }
        a.label.to_lowercase().cmp(&b.label.to_lowercase())
    });

    rsx! {
        div { class: "chain-list",
            h3 {
                {
                    let n = props.chains.len();
                    if n == 1 { "1 chain".to_string() } else { format!("{n} chains") }
                }
            }
            for chain in sorted.iter() {
                {
                    let label = chain.label.clone();
                    let display_name = props.chain_names
                        .get(&label)
                        .cloned()
                        .unwrap_or_else(|| label.clone());
                    let is_sel = props.selected.as_ref() == Some(&label);
                    let steps = chain.steps.len();
                    let queues = chain.queues.len();
                    let lbl = label.clone();
                    let has_custom = props.chain_names.contains_key(&label);
                    let trigger = chain.steps.first()
                        .map(|s| s.trigger_info.clone())
                        .unwrap_or_default();
                    let health = props.chain_health.get(&label).cloned();
                    rsx! {
                        div {
                            key: "{label}",
                            class: if is_sel { "chain-item selected" } else { "chain-item" },
                            onclick: move |_| props.on_select.call(lbl.clone()),
                            div { class: "chain-header",
                                {
                                    if let Some(ref h) = health {
                                        let dot_class = if h.dead_letters > 0 || h.stuck_count > 0 || h.failure_streak > 2 {
                                            "dot error"
                                        } else if let Some(rate) = h.success_rate {
                                            if rate >= 95.0 { "dot ok" }
                                            else if rate >= 80.0 { "dot warn" }
                                            else { "dot error" }
                                        } else {
                                            "dot ok"
                                        };
                                        rsx! { span { class: "{dot_class}" } }
                                    } else {
                                        rsx! {}
                                    }
                                }
                                span { class: "chain-name", "{display_name}" }
                                span { class: "chain-badge",
                                    if steps == 1 { "1 step" } else { "{steps} steps" }
                                }
                                if queues > 0 {
                                    span { class: "chain-badge queue-badge",
                                        if queues == 1 { "1 queue" } else { "{queues} queues" }
                                    }
                                }
                            }
                            if let Some(ref h) = health {
                                div { class: "chain-health-badges",
                                    if let Some(rate) = h.success_rate {
                                        {
                                            let rate_class = if rate >= 95.0 { "health-badge health-good" }
                                                else if rate >= 80.0 { "health-badge health-warn" }
                                                else { "health-badge health-bad" };
                                            rsx! { span { class: "{rate_class}", "{rate:.0}%" } }
                                        }
                                    }
                                    if h.dead_letters > 0 {
                                        span { class: "health-badge health-bad", "DL:{h.dead_letters}" }
                                    }
                                    if h.stuck_count > 0 {
                                        span { class: "health-badge health-bad", "stuck:{h.stuck_count}" }
                                    }
                                }
                            }
                            if has_custom {
                                div { class: "chain-trigger chain-original", "{label}" }
                            }
                            div { class: "chain-trigger", "{trigger}" }
                        }
                    }
                }
            }
        }
    }
}
