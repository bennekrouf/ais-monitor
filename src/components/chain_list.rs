use dioxus::prelude::*;
use crate::services::chain::ChainDetail;
use std::collections::HashMap;

#[derive(Props, Clone, PartialEq)]
pub struct ChainListProps {
    pub chains: Vec<ChainDetail>,
    pub selected: Option<String>,
    pub on_select: EventHandler<String>,
    #[props(default)]
    pub chain_names: HashMap<String, String>,
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

    rsx! {
        div { class: "chain-list",
            h3 { "Chains ({props.chains.len()})" }
            for chain in props.chains.iter() {
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
                    rsx! {
                        div {
                            key: "{label}",
                            class: if is_sel { "chain-item selected" } else { "chain-item" },
                            onclick: move |_| props.on_select.call(lbl.clone()),
                            div { class: "chain-header",
                                span { class: "chain-name", "{display_name}" }
                                span { class: "chain-badge", "{steps} steps" }
                                if queues > 0 {
                                    span { class: "chain-badge queue-badge", "{queues} queues" }
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
