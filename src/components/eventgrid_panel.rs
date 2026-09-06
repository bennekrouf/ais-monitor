use crate::components::chain_detail::AzConfig;
use crate::services::azure::{self, EventGridSubscription, EventGridSystemTopic, EventGridTopic};
use dioxus::prelude::*;

#[derive(Props, Clone, PartialEq)]
pub struct EventGridPanelProps {
    pub az_config: AzConfig,
}

#[component]
pub fn EventGridPanel(props: EventGridPanelProps) -> Element {
    let az = props.az_config.clone();

    // Primary env state
    let mut topics = use_signal(Vec::<TopicWithSubs>::new);
    let mut sys_topics = use_signal(Vec::<SysTopicWithSubs>::new);
    let mut loading = use_signal(|| false);
    let mut error_msg: Signal<Option<String>> = use_signal(|| None);
    let expanded_topic = use_signal(|| None);

    // Compare env state
    let mut cmp_topics = use_signal(Vec::<TopicWithSubs>::new);
    let mut cmp_sys_topics = use_signal(Vec::<SysTopicWithSubs>::new);
    let mut cmp_loading = use_signal(|| false);
    let mut cmp_error: Signal<Option<String>> = use_signal(|| None);
    let cmp_expanded = use_signal(|| None);
    let mut cmp_picker_open = use_signal(|| false);
    let mut cmp_profile: Signal<Option<AzConfig>> = use_signal(|| None);

    // Bumped on every fetch so a slower in-flight one cannot land after it.
    // Without this, clicking Refresh twice races two `fetch_eg` calls and the
    // panel shows whichever happens to finish last.
    let mut fetch_generation: Signal<u64> = use_signal(|| 0);

    // Read once per mount, not once per render. This is a file read plus a
    // JSON parse; in the render body it ran on every keystroke and every
    // signal write in the panel.
    let other_profiles = use_signal({
        let az = az.clone();
        move || {
            load_profiles()
                .into_iter()
                .filter(|p| {
                    p.resource_group != az.resource_group || p.subscription != az.subscription
                })
                .collect::<Vec<AzConfig>>()
        }
    });

    // Load from cache immediately, then fetch fresh in background
    let mut start_fetch = move |rg: String| {
        let generation = *fetch_generation.read() + 1;
        fetch_generation.set(generation);
        loading.set(true);
        error_msg.set(None);
        spawn(async move {
            let mut fresh_topics = Vec::new();
            let mut fresh_sys = Vec::new();
            let mut fresh_err = None;
            fetch_eg_into(&rg, &mut fresh_topics, &mut fresh_sys, &mut fresh_err).await;
            // A superseded fetch reports nothing at all — not its data, not
            // its error, not even that loading finished.
            if *fetch_generation.read() != generation {
                return;
            }
            topics.set(fresh_topics);
            sys_topics.set(fresh_sys);
            error_msg.set(fresh_err);
            loading.set(false);
        });
    };

    use_effect({
        let az = az.clone();
        move || {
            let rg = az.resource_group.clone();
            // Show cache instantly if available
            if let Some(cached) = load_eg_cache(&rg) {
                topics.set(cached.topics);
                sys_topics.set(cached.sys_topics);
            }
            start_fetch(rg);
        }
    });

    // Fetch compare env when profile is picked
    #[allow(unused_mut)]
    let mut trigger_cmp = move |profile: AzConfig| {
        cmp_profile.set(Some(profile.clone()));
        cmp_loading.set(true);
        cmp_error.set(None);
        cmp_topics.set(vec![]);
        cmp_sys_topics.set(vec![]);
        let rg = profile.resource_group.clone();
        spawn(async move {
            fetch_eg(&rg, &mut cmp_topics, &mut cmp_sys_topics, &mut cmp_error).await;
            cmp_loading.set(false);
        });
    };

    let has_cmp = cmp_profile.read().is_some();

    rsx! {
        div { class: "eg-panel",
            // ── Header ──────────────────────────────────────────────
            div { class: "eg-header",
                h3 { "EventGrid Topology" }
                div { style: "display:flex;gap:8px;align-items:center",
                    span { class: "eg-rg", "{az.resource_group}" }
                    button {
                        class: "btn btn-small",
                        disabled: *loading.read(),
                        title: "Clear cache and reload from Azure",
                        onclick: {
                            let rg = az.resource_group.clone();
                            move |_| {
                                clear_eg_cache(&rg);
                                topics.set(vec![]);
                                sys_topics.set(vec![]);
                                start_fetch(rg.clone());
                            }
                        },
                        if *loading.read() { "Refreshing…" } else { "Refresh" }
                    }
                    if !other_profiles.read().is_empty() {
                        button {
                            class: "btn btn-small",
                            onclick: move |_| cmp_picker_open.set(!cmp_picker_open()),
                            if has_cmp { "↔ Change env" } else { "↔ Compare" }
                        }
                    }
                }
            }

            // ── Env picker dropdown ─────────────────────────────────
            if *cmp_picker_open.read() {
                div { class: "ns-dropdown",
                    for profile in other_profiles.read().iter() {
                        {
                            let p = profile.clone();
                            let label = if p.label.is_empty() {
                                format!("{} / {}", p.resource_group, p.app_name)
                            } else {
                                p.label.clone()
                            };
                            let rg_display = p.resource_group.clone();
                            rsx! {
                                div {
                                    class: "ns-option",
                                    onclick: {
                                        let p = p.clone();
                                        move |_| {
                                            cmp_picker_open.set(false);
                                            trigger_cmp(p.clone());
                                        }
                                    },
                                    span { style: "color:var(--text1)", "{label}" }
                                    span { style: "color:var(--text3);font-size:11px;margin-left:8px", "{rg_display}" }
                                }
                            }
                        }
                    }
                }
            }

            if *loading.read() {
                div { class: "eg-loading", "Loading EventGrid topics..." }
            }
            { if let Some(ref e) = *error_msg.read() { rsx! { div { class: "eg-error", "{e}" } } } else { rsx! {} } }

            // ── Side-by-side or single view ──────────────────────────
            if has_cmp {
                // ── Two columns ──────────────────────────────────────
                div { class: "eg-compare",
                    div { class: "eg-compare-col",
                        div { class: "eg-env-label", "📍 {az.resource_group}" }
                        { render_topics_section(&topics.read(), &sys_topics.read(), expanded_topic, *loading.read()) }
                    }
                    div { class: "eg-compare-col",
                        {
                            let cmp_rg = cmp_profile.read().as_ref().map(|p| p.resource_group.clone()).unwrap_or_default();
                            rsx! { div { class: "eg-env-label", "📍 {cmp_rg}" } }
                        }
                        if *cmp_loading.read() {
                            div { class: "eg-loading", "Loading..." }
                        }
                        { if let Some(ref e) = *cmp_error.read() { rsx! { div { class: "eg-error", "{e}" } } } else { rsx! {} } }
                        { render_topics_section(&cmp_topics.read(), &cmp_sys_topics.read(), cmp_expanded, *cmp_loading.read()) }
                    }
                }
            } else {
                // ── Single column ────────────────────────────────────
                { render_topics_section(&topics.read(), &sys_topics.read(), expanded_topic, *loading.read()) }
            }
        }
    }
}

// ── Fetch helper ─────────────────────────────────────────────────────────────

/// Fetch into plain values rather than straight into signals.
///
/// Writing signals from inside the fetch is what made a superseded fetch
/// impossible to discard: by the time the caller could check whether its
/// result was still wanted, half of it was already on screen. Collecting
/// first lets the caller decide whether to publish the result at all.
async fn fetch_eg_into(
    rg: &str,
    topics: &mut Vec<TopicWithSubs>,
    sys_topics: &mut Vec<SysTopicWithSubs>,
    error: &mut Option<String>,
) {
    // Custom topics
    let rg2 = rg.to_string();
    let custom_result =
        tokio::task::spawn_blocking(move || azure::list_eventgrid_topics(&rg2)).await;

    match custom_result {
        Ok(Ok(topic_list)) => {
            let mut all = Vec::new();
            for t in &topic_list {
                let tid = t.id.clone();
                let subs =
                    tokio::task::spawn_blocking(move || azure::list_eventgrid_subscriptions(&tid))
                        .await
                        .ok()
                        .and_then(|r| r.ok())
                        .unwrap_or_default();
                all.push(TopicWithSubs {
                    topic: t.clone(),
                    subscriptions: subs,
                });
            }
            *topics = all;
        }
        Ok(Err(e)) => *error = Some(e),
        Err(e) => *error = Some(format!("{e}")),
    }

    // System topics
    let rg3 = rg.to_string();
    let sys_result =
        tokio::task::spawn_blocking(move || azure::list_eventgrid_system_topics(&rg3)).await;

    if let Ok(Ok(st_list)) = sys_result {
        let mut all = Vec::new();
        for st in &st_list {
            let rg4 = rg.to_string();
            let name = st.name.clone();
            let subs = tokio::task::spawn_blocking(move || {
                azure::list_eventgrid_system_topic_subscriptions(&rg4, &name)
            })
            .await
            .ok()
            .and_then(|r| r.ok())
            .unwrap_or_default();
            all.push(SysTopicWithSubs {
                topic: st.clone(),
                subscriptions: subs,
            });
        }
        *sys_topics = all;
    }

    save_eg_cache(rg, topics, sys_topics);
}

/// Signal-writing wrapper, for the compare-environment pane — it has its own
/// signals and only ever runs one fetch at a time.
async fn fetch_eg(
    rg: &str,
    topics: &mut Signal<Vec<TopicWithSubs>>,
    sys_topics: &mut Signal<Vec<SysTopicWithSubs>>,
    error: &mut Signal<Option<String>>,
) {
    let mut fresh_topics = Vec::new();
    let mut fresh_sys = Vec::new();
    let mut fresh_err = None;
    fetch_eg_into(rg, &mut fresh_topics, &mut fresh_sys, &mut fresh_err).await;
    topics.set(fresh_topics);
    sys_topics.set(fresh_sys);
    error.set(fresh_err);
}

// ── Render helpers ───────────────────────────────────────────────────────────

fn render_topics_section(
    topics: &[TopicWithSubs],
    sys_topics: &[SysTopicWithSubs],
    expanded: Signal<Option<String>>,
    is_loading: bool,
) -> Element {
    let nt = topics.len();
    let ns = sys_topics.len();
    rsx! {
        div { class: "eg-section-label",
            "⚡ Topics"
            span { class: "eg-count", " ({nt})" }
        }
        if !is_loading && topics.is_empty() {
            div { class: "eg-empty", "No custom topics" }
        }
        for tws in topics.iter() {
            { render_topic_block(tws, expanded) }
        }

        div { class: "eg-section-label", style: "margin-top: 16px",
            "🔗 System Topics"
            span { class: "eg-count", " ({ns})" }
        }
        if !is_loading && sys_topics.is_empty() {
            div { class: "eg-empty", "No system topics" }
        }
        for stws in sys_topics.iter() {
            { render_sys_topic_block(stws, expanded) }
        }
    }
}

fn render_topic_block(tws: &TopicWithSubs, mut expanded: Signal<Option<String>>) -> Element {
    let topic_name = tws.topic.name.clone();
    let topic_name_click = topic_name.clone();
    let is_expanded = expanded.read().as_ref() == Some(&topic_name);
    let sub_count = tws.subscriptions.len();
    let subs = tws.subscriptions.clone();
    let endpoint = tws.topic.endpoint.clone();

    rsx! {
        div { class: "eg-topic",
            div {
                class: "eg-topic-header",
                onclick: move |_| {
                    let cur = expanded.read().clone();
                    if cur.as_ref() == Some(&topic_name_click) {
                        expanded.set(None);
                    } else {
                        expanded.set(Some(topic_name_click.clone()));
                    }
                },
                span { class: "eg-topic-arrow", if is_expanded { "▼" } else { "▶" } }
                span { class: "eg-topic-icon", "⚡" }
                span { class: "eg-topic-name", "{topic_name}" }
                span { class: "eg-topic-badge", "{sub_count} subscriptions" }
            }
            if !endpoint.is_empty() {
                div { class: "eg-topic-endpoint", "{endpoint}" }
            }
            if is_expanded {
                { render_subs(&subs) }
            }
        }
    }
}

fn render_sys_topic_block(
    stws: &SysTopicWithSubs,
    mut expanded: Signal<Option<String>>,
) -> Element {
    let topic_name = stws.topic.name.clone();
    let key = format!("sys:{}", topic_name);
    let key_click = key.clone();
    let is_expanded = expanded.read().as_ref() == Some(&key);
    let sub_count = stws.subscriptions.len();
    let subs = stws.subscriptions.clone();
    let source = stws.topic.source.clone();
    let topic_type = stws.topic.topic_type.clone();

    rsx! {
        div { class: "eg-topic",
            div {
                class: "eg-topic-header",
                onclick: move |_| {
                    let cur = expanded.read().clone();
                    if cur.as_ref() == Some(&key_click) {
                        expanded.set(None);
                    } else {
                        expanded.set(Some(key_click.clone()));
                    }
                },
                span { class: "eg-topic-arrow", if is_expanded { "▼" } else { "▶" } }
                span { class: "eg-topic-icon", "🔗" }
                span { class: "eg-topic-name", "{topic_name}" }
                span { class: "eg-topic-type-badge", "{topic_type}" }
                span { class: "eg-topic-badge", "{sub_count} subscriptions" }
            }
            div { class: "eg-topic-source", "Source: {source}" }
            if is_expanded {
                { render_subs(&subs) }
            }
        }
    }
}

/// One-line delivery summary: retries, and — the part that matters — whether
/// anything catches an event that never gets delivered.
fn delivery_summary(sub: &EventGridSubscription) -> (String, bool) {
    let retries = match (sub.max_delivery_attempts, sub.event_ttl_minutes) {
        (Some(a), Some(t)) => format!("{a}× / {t}m"),
        (Some(a), None) => format!("{a}×"),
        _ => "—".to_string(),
    };
    match &sub.dead_letter {
        Some(dl) => {
            let tail = dl.rsplit('/').next().unwrap_or(dl);
            (format!("{retries} → …/{tail}"), false)
        }
        None => (format!("{retries} → dropped"), true),
    }
}

fn render_subs(subs: &[EventGridSubscription]) -> Element {
    rsx! {
        div { class: "eg-subs",
            for sub in subs.iter() {
                {
                    let sub_name = sub.name.clone();
                    let dest_type = sub.destination_type.clone();
                    let dest_queue = sub.destination_queue.clone();
                    let filters = sub.filters.clone();
                    // Only meaningful alongside filters: it changes whether a
                    // filter tests an array's elements or the array itself,
                    // which is the difference between a filter that matches
                    // and one that silently never does.
                    let array_filtering = sub.advanced_filtering_on_arrays && !filters.is_empty();
                    let dest_icon = if dest_type.contains("ServiceBus") { "📨" }
                        else if dest_type.contains("WebHook") { "🌐" }
                        else if dest_type.contains("StorageQueue") { "📦" }
                        else { "📌" };
                    let (delivery_text, delivery_dropped) = delivery_summary(sub);

                    rsx! {
                        div { class: "eg-sub",
                            div { class: "eg-sub-header",
                                span { class: "eg-sub-icon", "{dest_icon}" }
                                span { class: "eg-sub-name", "{sub_name}" }
                                span { class: "eg-sub-arrow", "→" }
                                span { class: "eg-sub-dest", "{dest_queue}" }
                            }
                            div {
                                class: if delivery_dropped { "eg-delivery eg-delivery-warn" } else { "eg-delivery" },
                                title: if delivery_dropped {
                                    "No deadLetterDestination — Event Grid discards the event once retries are exhausted"
                                } else { "Undeliverable events are dead-lettered" },
                                "{delivery_text}"
                            }
                            if !filters.is_empty() {
                                div { class: "eg-filters",
                                    if array_filtering {
                                        div {
                                            class: "eg-filter-note",
                                            title: "enableAdvancedFilteringOnArrays is on — a filter matches if any element of an array value matches, rather than testing the array as a whole",
                                            "matches any array element"
                                        }
                                    }
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

// ── Types ────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct TopicWithSubs {
    topic: EventGridTopic,
    subscriptions: Vec<EventGridSubscription>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct SysTopicWithSubs {
    topic: EventGridSystemTopic,
    subscriptions: Vec<EventGridSubscription>,
}

// ── Cache ─────────────────────────────────────────────────────────────────────

#[derive(serde::Serialize, serde::Deserialize)]
struct EgCache {
    topics: Vec<TopicWithSubs>,
    sys_topics: Vec<SysTopicWithSubs>,
}

fn eg_cache_path(rg: &str) -> std::path::PathBuf {
    let safe = rg.replace(['/', '\\', ':'], "_");
    dirs::cache_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("ais-monitor")
        .join(format!("eg_{safe}.json"))
}

fn load_eg_cache(rg: &str) -> Option<EgCache> {
    let path = eg_cache_path(rg);
    let content = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}

fn save_eg_cache(rg: &str, topics: &[TopicWithSubs], sys_topics: &[SysTopicWithSubs]) {
    let path = eg_cache_path(rg);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let cache = EgCache {
        topics: topics.to_vec(),
        sys_topics: sys_topics.to_vec(),
    };
    if let Ok(json) = serde_json::to_string(&cache) {
        crate::services::store::write_best_effort(&path, &json);
    }
}

fn clear_eg_cache(rg: &str) {
    let _ = std::fs::remove_file(eg_cache_path(rg));
}

// ── Profile persistence (shared with welcome screen) ─────────────────────────

fn config_file() -> std::path::PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("ais-monitor")
        .join("profiles.json")
}

fn load_profiles() -> Vec<AzConfig> {
    let path = config_file();
    let content = std::fs::read_to_string(path).unwrap_or_default();
    serde_json::from_str(&content).unwrap_or_default()
}
