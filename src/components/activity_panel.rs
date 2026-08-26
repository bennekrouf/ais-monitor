//! Floating Activity log button + slide-up drawer.
//!
//! Reads from the global `activity` ring on a 750ms poll. The badge shows the
//! number of error-level events since the user last opened the drawer.

use crate::services::activity::{self, ActivityEvent, ActivityLevel};
use dioxus::prelude::*;

#[component]
pub fn ActivityPanel() -> Element {
    let mut open = use_signal(|| false);
    let mut events: Signal<Vec<ActivityEvent>> = use_signal(activity::snapshot);
    let mut unread = use_signal(activity::unread_errors);
    let mut filter_level: Signal<Option<ActivityLevel>> = use_signal(|| None);

    // Poll the global tick. We don't want to render every tick, only when the
    // counter actually changes — re-reading the snapshot is cheap (a Mutex
    // clone of a bounded Vec) but updating signals triggers Dioxus diffs.
    use_effect(move || {
        spawn(async move {
            let mut last_tick = activity::tick();
            // Initial render already covered by the use_signal defaults.
            loop {
                tokio::time::sleep(std::time::Duration::from_millis(750)).await;
                let t = activity::tick();
                if t != last_tick {
                    last_tick = t;
                    events.set(activity::snapshot());
                    unread.set(activity::unread_errors());
                }
            }
        });
    });

    let is_open = *open.read();
    let unread_n = *unread.read();
    let all = events.read().clone();
    let level_filter = filter_level.read().clone();
    let filtered: Vec<ActivityEvent> = all
        .iter()
        .rev() // newest first
        .filter(|e| level_filter.as_ref().map_or(true, |l| &e.level == l))
        .cloned()
        .collect();

    let fab_class = if unread_n > 0 {
        "activity-fab activity-fab-alert"
    } else {
        "activity-fab"
    };

    rsx! {
        // Floating launcher button (bottom-right)
        button {
            class: "{fab_class}",
            title: if unread_n > 0 {
                format!("{} new error(s) — click to view", unread_n)
            } else {
                "Activity log".to_string()
            },
            onclick: move |_| {
                let next = !*open.peek();
                open.set(next);
                if next {
                    activity::mark_read();
                    unread.set(0);
                }
            },
            "📋"
            if unread_n > 0 {
                span { class: "activity-fab-badge", "{unread_n}" }
            }
        }

        // Slide-up drawer
        if is_open {
            div { class: "activity-drawer",
                div { class: "activity-drawer-header",
                    span { class: "activity-drawer-title", "Activity" }
                    span { class: "activity-drawer-count", "{filtered.len()} / {all.len()}" }
                    div { class: "activity-filters",
                        {
                            let all_cls = if level_filter.is_none() { "activity-filter-pill active" } else { "activity-filter-pill" };
                            let err_cls = if matches!(level_filter, Some(ActivityLevel::Error)) { "activity-filter-pill active error" } else { "activity-filter-pill error" };
                            let warn_cls = if matches!(level_filter, Some(ActivityLevel::Warn)) { "activity-filter-pill active warn" } else { "activity-filter-pill warn" };
                            let ok_cls = if matches!(level_filter, Some(ActivityLevel::Ok)) { "activity-filter-pill active ok" } else { "activity-filter-pill ok" };
                            rsx! {
                                button { class: "{all_cls}",  onclick: move |_| filter_level.set(None),                          "All" }
                                button { class: "{err_cls}",  onclick: move |_| filter_level.set(Some(ActivityLevel::Error)),    "Errors" }
                                button { class: "{warn_cls}", onclick: move |_| filter_level.set(Some(ActivityLevel::Warn)),     "Warns" }
                                button { class: "{ok_cls}",   onclick: move |_| filter_level.set(Some(ActivityLevel::Ok)),       "Ok" }
                            }
                        }
                    }
                    div { style: "flex:1" }
                    button {
                        class: "btn btn-small",
                        title: "Copy visible events as text",
                        onclick: {
                            let to_copy = filtered.clone();
                            move |_| {
                                let text: String = to_copy.iter().map(|e| {
                                    let lvl = match e.level {
                                        ActivityLevel::Info => "info",
                                        ActivityLevel::Ok => "ok",
                                        ActivityLevel::Warn => "warn",
                                        ActivityLevel::Error => "error",
                                    };
                                    if e.detail.is_empty() {
                                        format!("[{}] {} — {} {}", e.time, lvl, e.action, e.target)
                                    } else {
                                        format!("[{}] {} — {} {}\n    {}", e.time, lvl, e.action, e.target, e.detail)
                                    }
                                }).collect::<Vec<_>>().join("\n");
                                let _ = document::eval(&format!(
                                    "navigator.clipboard.writeText({:?})",
                                    text
                                ));
                            }
                        },
                        "Copy"
                    }
                    button {
                        class: "btn btn-small",
                        title: "Clear in-memory log (file on disk is preserved)",
                        onclick: move |_| {
                            activity::clear();
                            events.set(Vec::new());
                            unread.set(0);
                        },
                        "Clear"
                    }
                    button {
                        class: "btn-icon activity-close-btn",
                        title: "Close",
                        onclick: move |_| open.set(false),
                        "✕"
                    }
                }
                div { class: "activity-drawer-body",
                    if filtered.is_empty() {
                        div { class: "activity-empty",
                            if level_filter.is_some() { "No events match this filter." }
                            else { "No activity yet — actions and errors will appear here." }
                        }
                    } else {
                        for ev in filtered.iter() {
                            ActivityRow { event: ev.clone() }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn ActivityRow(event: ActivityEvent) -> Element {
    let mut expanded = use_signal(|| false);
    let level_class = match event.level {
        ActivityLevel::Info => "activity-row activity-info",
        ActivityLevel::Ok => "activity-row activity-ok",
        ActivityLevel::Warn => "activity-row activity-warn",
        ActivityLevel::Error => "activity-row activity-error",
    };
    let icon = match event.level {
        ActivityLevel::Info => "·",
        ActivityLevel::Ok => "✓",
        ActivityLevel::Warn => "!",
        ActivityLevel::Error => "✕",
    };
    let has_detail = !event.detail.is_empty();
    let is_exp = *expanded.read();
    let row_class = if has_detail {
        format!("{} clickable", level_class)
    } else {
        level_class.to_string()
    };

    rsx! {
        div {
            class: "{row_class}",
            onclick: move |_| if has_detail { let v = !*expanded.peek(); expanded.set(v); },
            span { class: "activity-time", "{event.time}" }
            span { class: "activity-icon", "{icon}" }
            span { class: "activity-action", "{event.action}" }
            span { class: "activity-target", "{event.target}" }
            if has_detail {
                span { class: "activity-expand-hint", if is_exp { "▼" } else { "▶" } }
            }
        }
        if is_exp && has_detail {
            div { class: "activity-detail",
                pre { "{event.detail}" }
            }
        }
    }
}
