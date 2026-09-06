use crate::components::chain_detail::AzConfig;
use crate::services::azure;
use dioxus::prelude::*;

#[derive(Clone, Debug, PartialEq)]
enum ProbeResult {
    Ok { latency_ms: u128 },
    Err(String),
}

#[derive(Props, Clone, PartialEq)]
pub struct DiagnosticsPanelProps {
    pub az_config: AzConfig,
}

#[component]
pub fn DiagnosticsPanel(props: DiagnosticsPanelProps) -> Element {
    let az = props.az_config.clone();

    let mut kv_vault: Signal<String> = use_signal(String::new);
    let mut kv_secret: Signal<String> = use_signal(String::new);
    let mut kv_running: Signal<bool> = use_signal(|| false);
    let mut kv_result: Signal<Option<ProbeResult>> = use_signal(|| None);

    let mut sb_queue: Signal<String> = use_signal(String::new);
    let mut sb_running: Signal<bool> = use_signal(|| false);
    let mut sb_result: Signal<Option<ProbeResult>> = use_signal(|| None);
    // The probe receive-and-deletes, so on a queue that is not idle it
    // permanently destroys a real message. Every other destructive queue
    // action in this app is behind a modal; a note above the button was not
    // the same promise.
    let mut sb_confirm: Signal<bool> = use_signal(|| false);

    let run_kv_probe = move |_| {
        let vault = kv_vault.read().trim().to_string();
        let secret = kv_secret.read().trim().to_string();
        if vault.is_empty() || secret.is_empty() {
            return;
        }
        kv_running.set(true);
        kv_result.set(None);
        spawn(async move {
            let started = std::time::Instant::now();
            let result = tokio::task::spawn_blocking(move || {
                azure::keyvault_resolve_secret(&vault, &secret)
            })
            .await;
            let outcome = match result {
                Ok(Ok(v)) if !v.is_empty() => ProbeResult::Ok {
                    latency_ms: started.elapsed().as_millis(),
                },
                Ok(Ok(_)) => ProbeResult::Err("Secret resolved to an empty value".into()),
                Ok(Err(e)) => ProbeResult::Err(e),
                Err(e) => ProbeResult::Err(format!("{e}")),
            };
            kv_result.set(Some(outcome));
            kv_running.set(false);
        });
    };

    let run_sb_probe = {
        let az = az.clone();
        move |_| {
            sb_confirm.set(false);
            let az = az.clone();
            let queue = sb_queue.read().trim().to_string();
            if queue.is_empty() {
                return;
            }
            sb_running.set(true);
            sb_result.set(None);
            spawn(async move {
                let rg = az.resource_group.clone();
                let ns = az.sb_namespace.clone();
                if ns.is_empty() {
                    sb_result.set(Some(ProbeResult::Err(
                        "No Service Bus namespace configured for this profile".into(),
                    )));
                    sb_running.set(false);
                    return;
                }
                let rg2 = rg.clone();
                let ns2 = ns.clone();
                let queue_for_conn = queue.clone();
                let conn = tokio::task::spawn_blocking(move || {
                    azure::sb_get_connection_string_for(&rg2, &ns2, Some(&queue_for_conn))
                })
                .await
                .unwrap_or_else(|e| Err(format!("{e}")));
                let cs = match conn {
                    Ok(cs) => cs,
                    Err(e) => {
                        sb_result.set(Some(ProbeResult::Err(format!("Auth: {e}"))));
                        sb_running.set(false);
                        return;
                    }
                };
                let outcome = match azure::sb_probe_roundtrip(&cs, &queue).await {
                    Ok(latency_ms) => ProbeResult::Ok { latency_ms },
                    Err(e) => ProbeResult::Err(e),
                };
                sb_result.set(Some(outcome));
                sb_running.set(false);
            });
        }
    };

    rsx! {
        div { class: "func-panel",
            div { class: "func-header",
                h2 { "Diagnostic Probes" }
            }
            div { class: "func-note",
                "These run from this machine's current az session, not from inside the Function/Logic App itself — they check whether the credential and network path work at all, not the app's exact runtime network path. Cosmos, SQL, and true intra-app connectivity probes aren't implemented here: the original design calls for a tiny probe function deployed inside the Function App, which this tool doesn't provision."
            }

            // ── Key Vault probe ─────────────────────────────────────────
            div { class: "func-app-card",
                div { class: "func-app-header", h3 { "Key Vault: resolve a secret" } }
                div { class: "az-field",
                    label { "Vault name" }
                    input {
                        r#type: "text",
                        value: "{kv_vault.read()}",
                        oninput: move |e| kv_vault.set(e.value().clone()),
                    }
                }
                div { class: "az-field",
                    label { "Secret name" }
                    input {
                        r#type: "text",
                        value: "{kv_secret.read()}",
                        oninput: move |e| kv_secret.set(e.value().clone()),
                    }
                }
                button {
                    class: "btn btn-small btn-primary",
                    disabled: *kv_running.read(),
                    onclick: run_kv_probe,
                    if *kv_running.read() { "Probing…" } else { "Run probe" }
                }
                {
                    let result = kv_result.read().clone();
                    match result {
                        None => rsx! {},
                        Some(ProbeResult::Ok { latency_ms }) => rsx! {
                            div { class: "func-summary",
                                span { class: "func-summary-item func-success", "✅ Resolved in {latency_ms}ms" }
                            }
                        },
                        Some(ProbeResult::Err(e)) => rsx! { div { class: "az-error", "❌ {e}" } },
                    }
                }
            }

            // ── Service Bus round-trip probe ────────────────────────────
            div { class: "func-app-card",
                div { class: "func-app-header", h3 { "Service Bus: send/receive round trip" } }
                div { class: "func-note",
                    "Sends a small test message to the queue below, then immediately receives it back and deletes it. Only point this at an empty or dedicated probe queue — on a busy queue it may receive someone else's message instead of its own."
                }
                div { class: "az-field",
                    label { "Queue name" }
                    input {
                        r#type: "text",
                        placeholder: "probe-queue",
                        value: "{sb_queue.read()}",
                        oninput: move |e| sb_queue.set(e.value().clone()),
                    }
                }
                button {
                    class: "btn btn-small btn-primary",
                    disabled: *sb_running.read() || sb_queue.read().trim().is_empty(),
                    onclick: move |_| sb_confirm.set(true),
                    if *sb_running.read() { "Probing…" } else { "Run probe" }
                }
                {
                    let result = sb_result.read().clone();
                    match result {
                        None => rsx! {},
                        Some(ProbeResult::Ok { latency_ms }) => rsx! {
                            div { class: "func-summary",
                                span { class: "func-summary-item func-success", "✅ Round trip in {latency_ms}ms" }
                            }
                        },
                        Some(ProbeResult::Err(e)) => rsx! { div { class: "az-error", "❌ {e}" } },
                    }
                }
            }

            if *sb_confirm.read() {
                {
                    let queue = sb_queue.read().trim().to_string();
                    rsx! {
                        div { class: "modal-backdrop",
                            onclick: move |_| sb_confirm.set(false),
                            div { class: "modal-card",
                                onclick: move |e: Event<MouseData>| e.stop_propagation(),
                                h3 { class: "modal-title", "Run the round-trip probe on '{queue}'?" }
                                p { class: "modal-body",
                                    "This sends a test message, then receives and "
                                    strong { "permanently deletes" }
                                    " one message from the queue. If anything else is already \
                                     sitting in '"
                                    strong { "{queue}" }
                                    "', the message destroyed may be that one rather than the \
                                     probe's. Only run this against an empty or dedicated probe \
                                     queue."
                                }
                                div { class: "modal-actions",
                                    button {
                                        class: "btn btn-small",
                                        onclick: move |_| sb_confirm.set(false),
                                        "Cancel"
                                    }
                                    button {
                                        class: "btn btn-small btn-primary",
                                        onclick: run_sb_probe,
                                        "Run probe"
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
