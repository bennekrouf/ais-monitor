use crate::components::chain_detail::AzConfig;
use crate::services::azure;
use crate::services::payload::suggest_payload;
use dioxus::prelude::*;

#[derive(Props, Clone, PartialEq)]
pub struct TriggerPanelProps {
    pub workflow: String,
    pub az_config: AzConfig,
    pub payloads_dir: String,
    /// Path to the local logic apps workspace. When set, the suggested payload
    /// is derived from the workflow's JSON schema (ParseJson schema first, then
    /// trigger schema) instead of the generic CloudEvent default.
    #[props(default)]
    pub local_dir: String,
    #[props(default)]
    pub on_triggered: Option<EventHandler<()>>,
}

#[component]
pub fn TriggerPanel(props: TriggerPanelProps) -> Element {
    let initial_payload = if props.local_dir.is_empty() {
        default_payload()
    } else {
        let suggested = suggest_payload(&props.local_dir, &props.workflow);
        if suggested == "{}" {
            default_payload()
        } else {
            suggested
        }
    };
    let mut payload_text = use_signal(|| initial_payload.clone());
    let mut trigger_url = use_signal(|| Option::<String>::None);
    let mut fetching_url = use_signal(|| false);
    let mut trigger_result = use_signal(|| Option::<TriggerState>::None);
    let mut triggering = use_signal(|| false);
    let mut saved_payloads =
        use_signal(|| list_saved_payloads(&props.payloads_dir, &props.workflow));
    let mut save_name = use_signal(|| String::new());

    let az = props.az_config.clone();
    let workflow = props.workflow.clone();
    let payloads_dir = props.payloads_dir.clone();
    let on_triggered = props.on_triggered.clone();

    // Fetch trigger URL on mount
    use_effect({
        let az = az.clone();
        let workflow = workflow.clone();
        move || {
            let az = az.clone();
            let wf = workflow.clone();
            fetching_url.set(true);
            spawn(async move {
                let result = tokio::task::spawn_blocking(move || {
                    azure::get_trigger_url(&az.subscription, &az.resource_group, &az.app_name, &wf)
                })
                .await;
                match result {
                    Ok(Ok(url)) => trigger_url.set(Some(url)),
                    Ok(Err(e)) => trigger_url.set(Some(format!("ERROR: {e}"))),
                    Err(e) => trigger_url.set(Some(format!("ERROR: {e}"))),
                }
                fetching_url.set(false);
            });
        }
    });

    let has_url = trigger_url
        .read()
        .as_ref()
        .map_or(false, |u| !u.starts_with("ERROR"));

    rsx! {
        div { class: "trigger-panel",
            h3 { class: "trigger-title", "Trigger: {workflow}" }

            // URL status
            div { class: "trigger-url-row",
                span { class: "trigger-label", "Callback URL" }
                {
                    let is_fetching = *fetching_url.read();
                    let url_val = trigger_url.read().clone();
                    if is_fetching {
                        rsx! { span { class: "trigger-url fetching", "Fetching..." } }
                    } else if let Some(ref url) = url_val {
                        if url.starts_with("ERROR") {
                            rsx! { span { class: "trigger-url error", "{url}" } }
                        } else {
                            let short = truncate_url(url);
                            rsx! { span { class: "trigger-url ok", title: "{url}", "{short}" } }
                        }
                    } else {
                        rsx! { span { class: "trigger-url", "—" } }
                    }
                }
            }

            // Saved payloads
            {
                let saved_list = saved_payloads.read().clone();
                if !saved_list.is_empty() {
                    rsx! {
                        div { class: "trigger-saved",
                            span { class: "trigger-label", "Saved payloads" }
                            div { class: "trigger-pills",
                                for name in saved_list.iter() {
                                    {
                                        let name_click = name.clone();
                                        let name_display = name.clone();
                                        let dir = payloads_dir.clone();
                                        let wf = workflow.clone();
                                        rsx! {
                                            button {
                                                class: "btn btn-small",
                                                onclick: move |_| {
                                                    if let Some(content) = load_payload(&dir, &wf, &name_click) {
                                                        payload_text.set(content);
                                                    }
                                                },
                                                "{name_display}"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                } else {
                    rsx! {}
                }
            }

            // Payload editor
            div { class: "trigger-editor-wrap",
                div { class: "trigger-editor-header",
                    span { class: "trigger-label", "Payload (JSON)" }
                    div { class: "trigger-editor-actions",
                        button {
                            class: "btn btn-small",
                            onclick: move |_| {
                                // Pretty-print the JSON
                                let text = payload_text.read().clone();
                                if let Ok(val) = serde_json::from_str::<serde_json::Value>(&text) {
                                    if let Ok(pretty) = serde_json::to_string_pretty(&val) {
                                        payload_text.set(pretty);
                                    }
                                }
                            },
                            "Format"
                        }
                        button {
                            class: "btn btn-small",
                            onclick: move |_| payload_text.set(initial_payload.clone()),
                            "Reset"
                        }
                    }
                }
                textarea {
                    class: "trigger-editor",
                    spellcheck: false,
                    value: "{payload_text.read()}",
                    oninput: move |e| payload_text.set(e.value().clone()),
                }
            }

            // Save payload row
            div { class: "trigger-save-row",
                input {
                    class: "trigger-save-input",
                    r#type: "text",
                    placeholder: "payload name...",
                    value: "{save_name.read()}",
                    oninput: move |e| save_name.set(e.value().clone()),
                }
                button {
                    class: "btn btn-small",
                    disabled: save_name.read().is_empty(),
                    onclick: {
                        let dir = payloads_dir.clone();
                        let wf = workflow.clone();
                        move |_| {
                            let name = save_name.read().clone();
                            let text = payload_text.read().clone();
                            save_payload(&dir, &wf, &name, &text);
                            saved_payloads.set(list_saved_payloads(&dir, &wf));
                            save_name.set(String::new());
                        }
                    },
                    "Save"
                }
            }

            // Trigger button
            div { class: "trigger-action-row",
                button {
                    class: "btn-primary trigger-btn",
                    disabled: !has_url || *triggering.read(),
                    onclick: {
                        let url = trigger_url.read().clone();
                        let wf_for_log = workflow.clone();
                        move |_| {
                            if let Some(ref callback) = url {
                                let cb = callback.clone();
                                let body = payload_text.read().clone();
                                let wf_log = wf_for_log.clone();
                                triggering.set(true);
                                trigger_result.set(None);
                                spawn(async move {
                                    let result = tokio::task::spawn_blocking(move || {
                                        azure::trigger_workflow(&cb, &body)
                                    }).await;
                                    let success = match result {
                                        Ok(Ok(r)) => {
                                            let ok = r.status_code >= 200 && r.status_code < 300;
                                            if ok {
                                                crate::services::activity::ok(
                                                    "Triggered workflow",
                                                    format!("{} ({})", wf_log, r.status_code),
                                                );
                                            } else {
                                                crate::services::activity::warn(
                                                    "Trigger returned non-2xx",
                                                    format!("{} ({})", wf_log, r.status_code),
                                                    r.body.clone(),
                                                );
                                            }
                                            trigger_result.set(Some(TriggerState {
                                                status: r.status_code,
                                                body: r.body,
                                            }));
                                            ok
                                        }
                                        Ok(Err(e)) => {
                                            crate::services::activity::error(
                                                "Trigger failed",
                                                wf_log.clone(),
                                                e.clone(),
                                            );
                                            trigger_result.set(Some(TriggerState {
                                                status: 0,
                                                body: e,
                                            }));
                                            false
                                        }
                                        Err(e) => {
                                            let s = format!("{e}");
                                            crate::services::activity::error(
                                                "Trigger panic",
                                                wf_log.clone(),
                                                s.clone(),
                                            );
                                            trigger_result.set(Some(TriggerState {
                                                status: 0,
                                                body: s,
                                            }));
                                            false
                                        }
                                    };
                                    triggering.set(false);
                                    if success {
                                        if let Some(ref cb) = on_triggered {
                                            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                                            cb.call(());
                                        }
                                    }
                                });
                            }
                        }
                    },
                    if *triggering.read() { "Sending..." } else { "Send" }
                }
            }

            // Response
            {
                let result_val = trigger_result.read().clone();
                if let Some(result) = result_val.as_ref() {
                    let status_class = if result.status >= 200 && result.status < 300 { "trigger-status ok" }
                        else if result.status == 0 { "trigger-status error" }
                        else { "trigger-status warn" };
                    let status_text = format!("{}", result.status);
                    let body_text = format_response(&result.body);
                    let copy_text = body_text.clone();
                    rsx! {
                        div { class: "trigger-response",
                            div { class: "trigger-response-header",
                                span { class: "trigger-label", "Response" }
                                span { class: "{status_class}", "{status_text}" }
                                button {
                                    class: "btn btn-small",
                                    title: "Copy response to clipboard",
                                    onclick: move |_| {
                                        if let Ok(mut cb) = arboard::Clipboard::new() {
                                            let _ = cb.set_text(copy_text.clone());
                                        }
                                    },
                                    "⎘ Copy"
                                }
                            }
                            pre { class: "trigger-response-body", "{body_text}" }
                        }
                    }
                } else {
                    rsx! {}
                }
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct TriggerState {
    status: u16,
    body: String,
}

fn default_payload() -> String {
    serde_json::to_string_pretty(&serde_json::json!({
        "specversion": "1.0",
        "type": "com.example.event",
        "source": "ais-monitor",
        "id": "test-001",
        "data": {}
    }))
    .unwrap_or_else(|_| "{}".into())
}

fn truncate_url(url: &str) -> String {
    if url.len() > 80 {
        format!("{}...{}", &url[..40], &url[url.len() - 30..])
    } else {
        url.to_string()
    }
}

fn format_response(body: &str) -> String {
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(body) {
        serde_json::to_string_pretty(&val).unwrap_or_else(|_| body.to_string())
    } else {
        body.to_string()
    }
}

// ── Payload persistence ─────────────────────────────────────────────────────

fn payloads_path(base_dir: &str, workflow: &str) -> std::path::PathBuf {
    std::path::Path::new(base_dir)
        .join(".ais-monitor-payloads")
        .join(workflow)
}

fn list_saved_payloads(base_dir: &str, workflow: &str) -> Vec<String> {
    let dir = payloads_path(base_dir, workflow);
    if !dir.exists() {
        return Vec::new();
    }
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|e| {
            let e = e.ok()?;
            let name = e.file_name().to_string_lossy().to_string();
            if name.ends_with(".json") {
                Some(name.strip_suffix(".json").unwrap().to_string())
            } else {
                None
            }
        })
        .collect();
    names.sort();
    names
}

fn save_payload(base_dir: &str, workflow: &str, name: &str, content: &str) {
    let dir = payloads_path(base_dir, workflow);
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(format!("{name}.json"));
    let _ = std::fs::write(path, content);
}

fn load_payload(base_dir: &str, workflow: &str, name: &str) -> Option<String> {
    let path = payloads_path(base_dir, workflow).join(format!("{name}.json"));
    std::fs::read_to_string(path).ok()
}
