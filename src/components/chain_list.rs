use dioxus::prelude::*;
use crate::services::chain::ChainDetail;
use crate::services::history_cache::HealthPoint;
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
    /// Per-chain success-rate history for sparkline rendering. Missing key →
    /// no sparkline rendered, which is fine — first check populates it.
    #[props(default)]
    pub chain_history: HashMap<String, Vec<HealthPoint>>,
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
                                    // Sparkline of the last N success-rate values.
                                    {
                                        let series = props.chain_history.get(&label);
                                        if let Some(s) = series {
                                            if s.len() >= 2 {
                                                rsx! { Sparkline { points: s.clone() } }
                                            } else { rsx! {} }
                                        } else { rsx! {} }
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

/// Inline 60×16 SVG sparkline of success-rate history. Missing points (no
/// runs in that sample) render at 0% with a paler stroke — distinct from
/// real samples with rate.
#[derive(Props, Clone, PartialEq)]
struct SparklineProps {
    points: Vec<HealthPoint>,
}

#[component]
fn Sparkline(props: SparklineProps) -> Element {
    const W: f64 = 60.0;
    const H: f64 = 16.0;
    let n = props.points.len();
    if n < 2 { return rsx! {}; }

    // Map each point's success_rate (0-100) to a y coordinate. Missing
    // rate ⇒ treat as 0 (no data) and the dot will sit at baseline.
    let step_x = W / (n as f64 - 1.0);
    let to_y = |rate: Option<f64>| -> f64 {
        let r = rate.unwrap_or(0.0).clamp(0.0, 100.0);
        // Pad top/bottom by 1px so the line never touches the edges.
        H - 1.0 - ((r / 100.0) * (H - 2.0))
    };

    let path_d: String = props.points.iter().enumerate().map(|(i, p)| {
        let x = i as f64 * step_x;
        let y = to_y(p.success_rate);
        if i == 0 { format!("M{x:.1},{y:.1}") } else { format!("L{x:.1},{y:.1}") }
    }).collect::<Vec<_>>().join(" ");

    // Trend colour: compare the average of the latest 3 to the oldest 3.
    fn avg(xs: &[Option<f64>]) -> f64 {
        let xs: Vec<f64> = xs.iter().filter_map(|x| *x).collect();
        if xs.is_empty() { 0.0 } else { xs.iter().sum::<f64>() / xs.len() as f64 }
    }
    let len = props.points.len();
    let head: Vec<Option<f64>> = props.points.iter().take(len.min(3)).map(|p| p.success_rate).collect();
    let tail: Vec<Option<f64>> = props.points.iter().rev().take(len.min(3)).map(|p| p.success_rate).collect();
    let old_avg = avg(&head);
    let new_avg = avg(&tail);
    let delta = new_avg - old_avg;
    let trend_class = if delta >= -0.5      { "spark-up" }
                       else if delta >= -5.0 { "spark-flat" }
                       else                  { "spark-down" };

    let title = format!("{} samples · trend {:+.1}% (oldest {:.0}% → newest {:.0}%)",
        n, delta, old_avg, new_avg);

    let cx = W;
    let cy = to_y(props.points.last().and_then(|p| p.success_rate));

    rsx! {
        // Wrap in a span so we can use the HTML `title` attribute for a hover
        // tooltip — `<svg title="...">` clashes with Dioxus's SVG `<title>`
        // child-element binding.
        span {
            class: "chain-sparkline-wrap",
            title: "{title}",
            svg {
                class: "chain-sparkline {trend_class}",
                width: "{W}",
                height: "{H}",
                view_box: "0 0 {W} {H}",
                preserve_aspect_ratio: "none",
                line {
                    x1: "0", y1: "{H - 1.0}", x2: "{W}", y2: "{H - 1.0}",
                    class: "spark-baseline",
                }
                path {
                    d: "{path_d}",
                    class: "spark-line",
                    fill: "none",
                }
                circle {
                    cx: "{cx}", cy: "{cy}", r: "1.6",
                    class: "spark-dot",
                }
            }
        }
    }
}
