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
    let mut filter = use_signal(String::new);

    if props.chains.is_empty() {
        return rsx! {
            div { class: "chain-list empty",
                p { "No chains discovered" }
            }
        };
    }

    let query = filter.read().to_lowercase();
    let mut sorted: Vec<ChainDetail> = props.chains.iter()
        .filter(|c| {
            if query.is_empty() { return true; }
            let display = props.chain_names.get(&c.label).unwrap_or(&c.label);
            display.to_lowercase().contains(&query)
                || c.label.to_lowercase().contains(&query)
                || c.steps.iter().any(|s| s.workflow.to_lowercase().contains(&query))
                || c.queues.iter().any(|q| q.to_lowercase().contains(&query))
        })
        .cloned()
        .collect();
    sorted.sort_by(|a, b| {
        let a_ts = props.last_checked.get(&a.label).copied().unwrap_or(0);
        let b_ts = props.last_checked.get(&b.label).copied().unwrap_or(0);
        if a_ts != b_ts {
            return b_ts.cmp(&a_ts);
        }
        a.label.to_lowercase().cmp(&b.label.to_lowercase())
    });

    let total = props.chains.len();
    let shown = sorted.len();
    // Header label hoisted into outer scope so the rsx `"{header_label}"`
    // interpolation actually substitutes the variable.
    let header_label = if shown == total {
        if total == 1 { "1 chain".to_string() } else { format!("{total} chains") }
    } else {
        format!("{shown}/{total} chains")
    };

    rsx! {
        div { class: "chain-list",
            div { class: "chain-list-header",
                h3 { "{header_label}" }
                input {
                    class: "chain-filter-input",
                    placeholder: "Filter…",
                    value: "{filter}",
                    oninput: move |e| filter.set(e.value()),
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
