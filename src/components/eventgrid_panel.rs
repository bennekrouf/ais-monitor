use dioxus::prelude::*;
use crate::services::azure::{self, EventGridTopic, EventGridSubscription};
use crate::components::chain_detail::AzConfig;

#[derive(Props, Clone, PartialEq)]
pub struct EventGridPanelProps {
    pub az_config: AzConfig,
}

#[component]
pub fn EventGridPanel(props: EventGridPanelProps) -> Element {
    let mut topics = use_signal(|| Vec::<TopicWithSubs>::new());
    let mut loading = use_signal(|| false);
    let mut error_msg = use_signal(|| Option::<String>::None);
    let mut expanded_topic = use_signal(|| Option::<String>::None);

    let az = props.az_config.clone();

    // Fetch on mount
    use_effect({
        let az = az.clone();
        move || {
            let rg = az.resource_group.clone();
            loading.set(true);
            error_msg.set(None);
            spawn(async move {
                let rg2 = rg.clone();
                let result = tokio::task::spawn_blocking(move || {
                    azure::list_eventgrid_topics(&rg2)
                }).await;

                match result {
                    Ok(Ok(topic_list)) => {
                        // Fetch subscriptions for each topic
                        let mut all = Vec::new();
                        for t in &topic_list {
                            let tid = t.id.clone();
                            let subs_result = tokio::task::spawn_blocking(move || {
                                azure::list_eventgrid_subscriptions(&tid)
                            }).await;
                            let subs = match subs_result {
                                Ok(Ok(s)) => s,
                                _ => Vec::new(),
                            };
                            all.push(TopicWithSubs {
                                topic: t.clone(),
                                subscriptions: subs,
                            });
                        }
                        topics.set(all);
                    }
                    Ok(Err(e)) => error_msg.set(Some(e)),
                    Err(e) => error_msg.set(Some(format!("{e}"))),
                }
                loading.set(false);
            });
        }
    });

    rsx! {
        div { class: "eg-panel",
            div { class: "eg-header",
                h3 { "EventGrid Topology" }
                span { class: "eg-rg", "{az.resource_group}" }
            }

            if *loading.read() {
                div { class: "eg-loading", "Loading EventGrid topics..." }
            }

            {
                let err = error_msg.read().clone();
                if let Some(e) = err {
                    rsx! { div { class: "eg-error", "{e}" } }
                } else {
                    rsx! {}
                }
            }

            {
                let topic_list = topics.read().clone();
                if !*loading.read() && topic_list.is_empty() && error_msg.read().is_none() {
                    rsx! { div { class: "eg-empty", "No EventGrid topics found" } }
                } else {
                    rsx! {
                        for tws in topic_list.iter() {
                            {
                                let topic_name = tws.topic.name.clone();
                                let topic_name_click = topic_name.clone();
                                let is_expanded = expanded_topic.read().as_ref() == Some(&topic_name);
                                let sub_count = tws.subscriptions.len();
                                let subs = tws.subscriptions.clone();

                                rsx! {
                                    div { class: "eg-topic",
                                        div {
                                            class: "eg-topic-header",
                                            onclick: move |_| {
                                                let cur = expanded_topic.read().clone();
                                                if cur.as_ref() == Some(&topic_name_click) {
                                                    expanded_topic.set(None);
                                                } else {
                                                    expanded_topic.set(Some(topic_name_click.clone()));
                                                }
                                            },
                                            span { class: "eg-topic-arrow",
                                                if is_expanded { "▼" } else { "▶" }
                                            }
                                            span { class: "eg-topic-icon", "⚡" }
                                            span { class: "eg-topic-name", "{topic_name}" }
                                            span { class: "eg-topic-badge", "{sub_count} subscriptions" }
                                        }

                                        if is_expanded {
                                            div { class: "eg-subs",
                                                for sub in subs.iter() {
                                                    {
                                                        let sub_name = sub.name.clone();
                                                        let dest_type = sub.destination_type.clone();
                                                        let dest_queue = sub.destination_queue.clone();
                                                        let filters = sub.filters.clone();
                                                        let dest_icon = if dest_type.contains("ServiceBus") { "📨" }
                                                            else if dest_type.contains("WebHook") { "🌐" }
                                                            else if dest_type.contains("StorageQueue") { "📦" }
                                                            else { "📌" };

                                                        rsx! {
                                                            div { class: "eg-sub",
                                                                div { class: "eg-sub-header",
                                                                    span { class: "eg-sub-icon", "{dest_icon}" }
                                                                    span { class: "eg-sub-name", "{sub_name}" }
                                                                    span { class: "eg-sub-arrow", "→" }
                                                                    span { class: "eg-sub-dest", "{dest_queue}" }
                                                                }
                                                                if !filters.is_empty() {
                                                                    div { class: "eg-filters",
                                                                        for f in filters.iter() {
                                                                            {
                                                                                let key = f.key.clone();
                                                                                let op = f.operator.clone();
                                                                                let vals = f.values.join(", ");
                                                                                rsx! {
                                                                                    div { class: "eg-filter",
                                                                                        span { class: "eg-filter-key", "{key}" }
                                                                                        span { class: "eg-filter-op", "{op}" }
                                                                                        span { class: "eg-filter-val", "{vals}" }
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
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct TopicWithSubs {
    topic: EventGridTopic,
    subscriptions: Vec<EventGridSubscription>,
}
