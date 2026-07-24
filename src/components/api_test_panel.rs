use dioxus::prelude::*;
use crate::services::api_test;

/// Generic "Postman-like" panel: hit any URL with any method/headers/body.
/// Unlike TriggerPanel (scoped to a Logic App's ARM-issued callback URL),
/// this targets arbitrary endpoints — APIM gateways, external webhooks, etc.
#[derive(Props, Clone, PartialEq)]
pub struct ApiTestPanelProps {
    /// Directory saved requests are persisted under. Empty disables saving.
    #[props(default)]
    pub save_dir: String,
    /// Azure subscription id — used for the optional APIM subscription-key
    /// lookup (a different RBAC scope than the Logic App itself).
    #[props(default)]
    pub azure_subscription: String,
}

#[component]
pub fn ApiTestPanel(props: ApiTestPanelProps) -> Element {
    // Seed fields from whatever was last typed for this profile (including
    // the subscription-key header), so it survives tab switches and app
    // restarts without an explicit Save.
    let last_state = load_last_state(&props.save_dir);
    let mut method = use_signal({
        let last = last_state.clone();
        move || last.as_ref().map(|r| r.method.clone()).unwrap_or_else(|| "POST".to_string())
    });
    let mut url = use_signal({
        let last = last_state.clone();
        move || last.as_ref().map(|r| r.url.clone()).unwrap_or_default()
    });
    let mut headers = use_signal({
        let last = last_state.clone();
        move || last.as_ref().map(|r| r.headers.clone())
            .unwrap_or_else(|| vec![("Content-Type".to_string(), "application/json".to_string())])
    });
    let mut body_text = use_signal({
        let last = last_state.clone();
        move || last.as_ref().map(|r| r.body.clone()).unwrap_or_else(default_body)
    });
    let mut sending = use_signal(|| false);
    let mut result = use_signal(|| Option::<ApiState>::None);
    let mut saved = use_signal({
        let dir = props.save_dir.clone();
        move || list_saved(&dir)
    });
    let mut save_name = use_signal(|| String::new());

    let save_dir = props.save_dir.clone();
    let azure_subscription = props.azure_subscription.clone();

    // Persist current field values on every change so they're there next
    // time this tab (or the app) is opened for this profile.
    use_effect({
        let dir = save_dir.clone();
        move || {
            let state = SavedRequest {
                method: method.read().clone(),
                url: url.read().clone(),
                headers: headers.read().clone(),
                body: body_text.read().clone(),
            };
            save_last_state(&dir, &state);
        }
    });

    // ── APIM subscription-key lookup ────────────────────────────────────
    // The APIM instance (e.g. apim-ipaas-dev-chn-001) usually lives in a
    // different resource group than the Logic App, so its RG/service name
    // are entered once here and persisted per profile.
    let apim_cfg = load_apim_config(&save_dir);
    let mut apim_rg = use_signal({
        let cfg = apim_cfg.clone();
        move || cfg.resource_group.clone()
    });
    let mut apim_service = use_signal({
        let cfg = apim_cfg.clone();
        move || cfg.service_name.clone()
    });
    let mut apim_subs = use_signal(|| Vec::<api_test::ApimSubscription>::new());
    let mut apim_loading = use_signal(|| false);
    let mut apim_error = use_signal(|| Option::<String>::None);
    // Which subscription's key is currently being fetched (for the per-pill
    // spinner), distinct from `apim_loading` which covers the list call.
    let mut fetching_key = use_signal(|| Option::<String>::None);
    // Distinguishes "never listed yet" from "listed, zero subscriptions" so
    // a successful-but-empty result isn't indistinguishable from the click
    // never having fired.
    let mut apim_fetched = use_signal(|| false);
    let mut apim_rg_error = use_signal(|| Option::<String>::None);
    let mut apim_rg_loading = use_signal(|| false);

    // Suggest the APIM service name from the URL's host as soon as it looks
    // like an Azure API Management gateway — the name is already right
    // there in the URL you pasted, no need to retype it.
    use_effect(move || {
        let current_url = url.read().clone();
        if apim_service.read().is_empty() {
            if let Some(guess) = api_test::guess_apim_service_from_url(&current_url) {
                apim_service.set(guess);
            }
        }
    });

    use_effect({
        let dir = save_dir.clone();
        move || {
            let cfg = ApimConfig {
                resource_group: apim_rg.read().clone(),
                service_name: apim_service.read().clone(),
            };
            save_apim_config(&dir, &cfg);
        }
    });

    rsx! {
        div { class: "trigger-panel",
            // APIM subscription-key lookup — the APIM instance is usually a
            // separate resource (different RG) from the Logic App, so it
            // needs its own coordinates, entered once and persisted here.
            div { class: "api-apim-wrap",
                div { class: "trigger-editor-header",
                    span { class: "trigger-label", "APIM key lookup" }
                }
                div { class: "api-apim-row",
                    input {
                        class: "api-apim-input",
                        r#type: "text",
                        placeholder: "APIM resource group",
                        value: "{apim_rg.read()}",
                        oninput: move |e| apim_rg.set(e.value()),
                    }
                    input {
                        class: "api-apim-input",
                        r#type: "text",
                        placeholder: "APIM service name",
                        value: "{apim_service.read()}",
                        oninput: move |e| apim_service.set(e.value()),
                    }
                    button {
                        class: "btn btn-small",
                        title: "Look up which resource group this APIM service lives in",
                        disabled: apim_service.read().is_empty() || azure_subscription.is_empty()
                            || *apim_rg_loading.read(),
                        onclick: {
                            let az_sub = azure_subscription.clone();
                            move |_| {
                                let az_sub = az_sub.clone();
                                let service = apim_service.read().clone();
                                apim_rg_loading.set(true);
                                apim_rg_error.set(None);
                                spawn(async move {
                                    let service2 = service.clone();
                                    let res = tokio::task::spawn_blocking(move || {
                                        api_test::discover_apim_resource_group(&az_sub, &service2)
                                    }).await;
                                    match res {
                                        Ok(Ok(rg)) => apim_rg.set(rg),
                                        Ok(Err(e)) => apim_rg_error.set(Some(e)),
                                        Err(e) => apim_rg_error.set(Some(format!("{e}"))),
                                    }
                                    apim_rg_loading.set(false);
                                });
                            }
                        },
                        if *apim_rg_loading.read() {
                            span { class: "spinner" }
                            "Finding…"
                        } else {
                            "Find RG"
                        }
                    }
                    button {
                        class: "btn btn-small",
                        disabled: apim_rg.read().is_empty() || apim_service.read().is_empty()
                            || azure_subscription.is_empty() || *apim_loading.read(),
                        onclick: {
                            let az_sub = azure_subscription.clone();
                            move |_| {
                                let az_sub = az_sub.clone();
                                let rg = apim_rg.read().clone();
                                let service = apim_service.read().clone();
                                apim_loading.set(true);
                                apim_error.set(None);
                                spawn(async move {
                                    let rg2 = rg.clone();
                                    let service2 = service.clone();
                                    let res = tokio::task::spawn_blocking(move || {
                                        api_test::list_apim_subscriptions(&az_sub, &rg2, &service2)
                                    }).await;
                                    match res {
                                        Ok(Ok(subs)) => {
                                            crate::services::activity::info(
                                                "APIM subscriptions listed",
                                                format!("{} found ({rg}/{service})", subs.len()),
                                            );
                                            apim_fetched.set(true);
                                            apim_subs.set(subs);
                                        }
                                        Ok(Err(e)) => {
                                            crate::services::activity::error(
                                                "APIM subscription list failed",
                                                format!("{rg}/{service}"),
                                                e.clone(),
                                            );
                                            apim_error.set(Some(e));
                                        }
                                        Err(e) => {
                                            let s = format!("{e}");
                                            crate::services::activity::error(
                                                "APIM subscription list panic",
                                                format!("{rg}/{service}"),
                                                s.clone(),
                                            );
                                            apim_error.set(Some(s));
                                        }
                                    }
                                    apim_loading.set(false);
                                });
                            }
                        },
                        if *apim_loading.read() {
                            span { class: "spinner" }
                            "Loading…"
                        } else {
                            "List subscriptions"
                        }
                    }
                }
                if let Some(err) = apim_rg_error.read().as_ref() {
                    div { class: "api-preview-warn", "{err}" }
                }
                if let Some(err) = apim_error.read().as_ref() {
                    div { class: "api-preview-warn", "{err}" }
                }
                if *apim_fetched.read() && apim_subs.read().is_empty() && apim_error.read().is_none() {
                    div { class: "api-preview-warn", "No subscriptions found for this APIM service." }
                }
                {
                    let subs = apim_subs.read().clone();
                    if !subs.is_empty() {
                        rsx! {
                            div { class: "trigger-pills",
                                for s in subs.iter() {
                                    {
                                        let sub_id = s.id.clone();
                                        let sub_display = s.display.clone();
                                        let az_sub = azure_subscription.clone();
                                        let is_fetching = fetching_key.read().as_deref() == Some(sub_id.as_str());
                                        rsx! {
                                            button {
                                                class: "btn btn-small",
                                                disabled: fetching_key.read().is_some(),
                                                title: "Fetch this subscription's key and fill the Ocp-Apim-Subscription-Key header",
                                                onclick: move |_| {
                                                    let az_sub = az_sub.clone();
                                                    let rg = apim_rg.read().clone();
                                                    let service = apim_service.read().clone();
                                                    let sid = sub_id.clone();
                                                    fetching_key.set(Some(sid.clone()));
                                                    apim_error.set(None);
                                                    spawn(async move {
                                                        let res = tokio::task::spawn_blocking(move || {
                                                            api_test::get_apim_subscription_key(&az_sub, &rg, &service, &sid)
                                                        }).await;
                                                        match res {
                                                            Ok(Ok(key)) => {
                                                                let mut h = headers.read().clone();
                                                                upsert_header(&mut h, "Ocp-Apim-Subscription-Key", key);
                                                                headers.set(h);
                                                            }
                                                            Ok(Err(e)) => apim_error.set(Some(e)),
                                                            Err(e) => apim_error.set(Some(format!("{e}"))),
                                                        }
                                                        fetching_key.set(None);
                                                    });
                                                },
                                                if is_fetching {
                                                    span { class: "spinner" }
                                                }
                                                "{sub_display}"
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
            }

            div { class: "api-url-row",
                select {
                    class: "api-method-select",
                    value: "{method.read()}",
                    onchange: move |e| method.set(e.value()),
                    option { value: "GET", "GET" }
                    option { value: "POST", "POST" }
                    option { value: "PUT", "PUT" }
                    option { value: "PATCH", "PATCH" }
                    option { value: "DELETE", "DELETE" }
                }
                input {
                    class: "api-url-input",
                    r#type: "text",
                    placeholder: "https://host/path/invoke",
                    value: "{url.read()}",
                    oninput: move |e| url.set(e.value()),
                }
            }

            // Saved requests
            {
                let saved_list = saved.read().clone();
                if !saved_list.is_empty() {
                    rsx! {
                        div { class: "trigger-saved",
                            span { class: "trigger-label", "Saved" }
                            div { class: "trigger-pills",
                                for name in saved_list.iter() {
                                    {
                                        let name_click = name.clone();
                                        let name_display = name.clone();
                                        let dir = save_dir.clone();
                                        rsx! {
                                            button {
                                                class: "btn btn-small",
                                                onclick: move |_| {
                                                    if let Some(req) = load_saved(&dir, &name_click) {
                                                        method.set(req.method);
                                                        url.set(req.url);
                                                        headers.set(req.headers);
                                                        body_text.set(req.body);
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

            // Headers editor
            div { class: "api-headers-wrap",
                div { class: "trigger-editor-header",
                    span { class: "trigger-label", "Headers" }
                    button {
                        class: "btn btn-small",
                        onclick: move |_| {
                            let mut h = headers.read().clone();
                            h.push((String::new(), String::new()));
                            headers.set(h);
                        },
                        "+ Header"
                    }
                }
                for (i , (k , v)) in headers.read().clone().into_iter().enumerate() {
                    div { class: "api-header-row", key: "{i}",
                        input {
                            class: "api-header-key",
                            r#type: "text",
                            placeholder: "Header-Name",
                            value: "{k}",
                            oninput: move |e| {
                                let mut h = headers.read().clone();
                                if let Some(pair) = h.get_mut(i) { pair.0 = e.value(); }
                                headers.set(h);
                            },
                        }
                        input {
                            class: "api-header-val",
                            r#type: "text",
                            placeholder: "value",
                            value: "{v}",
                            oninput: move |e| {
                                let mut h = headers.read().clone();
                                if let Some(pair) = h.get_mut(i) { pair.1 = e.value(); }
                                headers.set(h);
                            },
                        }
                        button {
                            class: "btn btn-small api-header-remove",
                            title: "Remove header",
                            onclick: move |_| {
                                let mut h = headers.read().clone();
                                if i < h.len() { h.remove(i); }
                                headers.set(h);
                            },
                            "✕"
                        }
                    }
                }
            }

            // Body editor
            div { class: "trigger-editor-wrap",
                div { class: "trigger-editor-header",
                    span { class: "trigger-label", "Body" }
                    button {
                        class: "btn btn-small",
                        onclick: move |_| {
                            let text = body_text.read().clone();
                            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&text) {
                                if let Ok(pretty) = serde_json::to_string_pretty(&val) {
                                    body_text.set(pretty);
                                }
                            }
                        },
                        "Format"
                    }
                }
                textarea {
                    class: "trigger-editor",
                    spellcheck: false,
                    value: "{body_text.read()}",
                    oninput: move |e| body_text.set(e.value().clone()),
                }
            }

            // Save row
            div { class: "trigger-save-row",
                input {
                    class: "trigger-save-input",
                    r#type: "text",
                    placeholder: "request name...",
                    value: "{save_name.read()}",
                    oninput: move |e| save_name.set(e.value().clone()),
                }
                button {
                    class: "btn btn-small",
                    disabled: save_name.read().is_empty() || save_dir.is_empty(),
                    onclick: {
                        let dir = save_dir.clone();
                        move |_| {
                            let name = save_name.read().clone();
                            let req = SavedRequest {
                                method: method.read().clone(),
                                url: url.read().clone(),
                                headers: headers.read().clone(),
                                body: body_text.read().clone(),
                            };
                            save_request(&dir, &name, &req);
                            saved.set(list_saved(&dir));
                            save_name.set(String::new());
                        }
                    },
                    "Save"
                }
            }

            // Request preview — shows exactly what will be sent, so a header
            // with an accidentally-empty value (e.g. the whole "Key: value"
            // pasted into the Key box) is obvious before clicking Send.
            {
                let m = method.read().clone();
                let u = url.read().clone();
                let h = headers.read().clone();
                let empty_val_keys: Vec<String> = h.iter()
                    .filter(|(k, v)| !k.trim().is_empty() && v.trim().is_empty())
                    .map(|(k, _)| k.clone())
                    .collect();
                rsx! {
                    div { class: "api-preview-wrap",
                        span { class: "trigger-label", "Request preview" }
                        pre { class: "api-preview", "{build_curl_preview(&m, &u, &h)}" }
                        if !empty_val_keys.is_empty() {
                            div { class: "api-preview-warn",
                                "⚠ empty value for: {empty_val_keys.join(\", \")}"
                            }
                        }
                    }
                }
            }

            // Send button
            div { class: "trigger-action-row",
                button {
                    class: "btn-primary trigger-btn",
                    disabled: url.read().is_empty() || *sending.read(),
                    onclick: move |_| {
                        let m = method.read().clone();
                        let u = url.read().clone();
                        let h = headers.read().clone();
                        let b = body_text.read().clone();
                        sending.set(true);
                        result.set(None);
                        spawn(async move {
                            let m2 = m.clone();
                            let u2 = u.clone();
                            let res = tokio::task::spawn_blocking(move || {
                                api_test::send_request(&m2, &u2, &h, &b)
                            }).await;
                            match res {
                                Ok(Ok(r)) => {
                                    let ok = r.status_code >= 200 && r.status_code < 300;
                                    if ok {
                                        crate::services::activity::ok(
                                            "API test",
                                            format!("{m} {u} ({})", r.status_code),
                                        );
                                    } else {
                                        crate::services::activity::warn(
                                            "API test non-2xx",
                                            format!("{m} {u} ({})", r.status_code),
                                            r.body.clone(),
                                        );
                                    }
                                    result.set(Some(ApiState { status: r.status_code, body: r.body }));
                                }
                                Ok(Err(e)) => {
                                    crate::services::activity::error(
                                        "API test failed",
                                        format!("{m} {u}"),
                                        e.clone(),
                                    );
                                    result.set(Some(ApiState { status: 0, body: e }));
                                }
                                Err(e) => {
                                    let s = format!("{e}");
                                    crate::services::activity::error(
                                        "API test panic",
                                        format!("{m} {u}"),
                                        s.clone(),
                                    );
                                    result.set(Some(ApiState { status: 0, body: s }));
                                }
                            }
                            sending.set(false);
                        });
                    },
                    if *sending.read() { "Sending..." } else { "Send" }
                }
            }

            // Response
            {
                let result_val = result.read().clone();
                if let Some(r) = result_val.as_ref() {
                    let status_class = if r.status >= 200 && r.status < 300 { "trigger-status ok" }
                        else if r.status == 0 { "trigger-status error" }
                        else { "trigger-status warn" };
                    let status_text = format!("{}", r.status);
                    let body_fmt = format_response(&r.body);
                    let copy_text = body_fmt.clone();
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
                            pre { class: "trigger-response-body", "{body_fmt}" }
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
struct ApiState {
    status: u16,
    body: String,
}

fn default_body() -> String {
    serde_json::to_string_pretty(&serde_json::json!({}))
        .unwrap_or_else(|_| "{}".into())
}

/// Update an existing header's value (case-insensitive key match), or append
/// a new row if it isn't present yet.
fn upsert_header(headers: &mut Vec<(String, String)>, key: &str, value: String) {
    if let Some(pair) = headers.iter_mut().find(|(k, _)| k.eq_ignore_ascii_case(key)) {
        pair.1 = value;
    } else {
        headers.push((key.to_string(), value));
    }
}

fn build_curl_preview(method: &str, url: &str, headers: &[(String, String)]) -> String {
    let mut s = format!("curl -X {}", if method.is_empty() { "POST" } else { method });
    for (k, v) in headers {
        if k.trim().is_empty() {
            continue;
        }
        s.push_str(&format!(" \\\n  -H \"{k}: {v}\""));
    }
    if url.is_empty() {
        s.push_str(" \\\n  <url>");
    } else {
        s.push_str(&format!(" \\\n  {url}"));
    }
    s
}

fn format_response(body: &str) -> String {
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(body) {
        serde_json::to_string_pretty(&val).unwrap_or_else(|_| body.to_string())
    } else {
        body.to_string()
    }
}

// ── Saved-request persistence ────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct SavedRequest {
    method: String,
    url: String,
    headers: Vec<(String, String)>,
    body: String,
}

fn requests_path(base_dir: &str) -> std::path::PathBuf {
    std::path::Path::new(base_dir).join("_api-requests")
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
struct ApimConfig {
    resource_group: String,
    service_name: String,
}

fn apim_config_path(base_dir: &str) -> std::path::PathBuf {
    std::path::Path::new(base_dir).join("_apim-config.json")
}

fn load_apim_config(base_dir: &str) -> ApimConfig {
    if base_dir.is_empty() {
        return ApimConfig::default();
    }
    std::fs::read_to_string(apim_config_path(base_dir))
        .ok()
        .and_then(|c| serde_json::from_str(&c).ok())
        .unwrap_or_default()
}

fn save_apim_config(base_dir: &str, cfg: &ApimConfig) {
    if base_dir.is_empty() {
        return;
    }
    if let Some(parent) = apim_config_path(base_dir).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(cfg) {
        let _ = std::fs::write(apim_config_path(base_dir), json);
    }
}

fn last_state_path(base_dir: &str) -> std::path::PathBuf {
    std::path::Path::new(base_dir).join("_api-test-last.json")
}

fn load_last_state(base_dir: &str) -> Option<SavedRequest> {
    if base_dir.is_empty() {
        return None;
    }
    let content = std::fs::read_to_string(last_state_path(base_dir)).ok()?;
    serde_json::from_str(&content).ok()
}

fn save_last_state(base_dir: &str, req: &SavedRequest) {
    if base_dir.is_empty() {
        return;
    }
    if let Some(parent) = last_state_path(base_dir).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(req) {
        let _ = std::fs::write(last_state_path(base_dir), json);
    }
}

fn list_saved(base_dir: &str) -> Vec<String> {
    if base_dir.is_empty() {
        return Vec::new();
    }
    let dir = requests_path(base_dir);
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
            name.strip_suffix(".json").map(|n| n.to_string())
        })
        .collect();
    names.sort();
    names
}

fn save_request(base_dir: &str, name: &str, req: &SavedRequest) {
    if base_dir.is_empty() {
        return;
    }
    let dir = requests_path(base_dir);
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(json) = serde_json::to_string_pretty(req) {
        let _ = std::fs::write(dir.join(format!("{name}.json")), json);
    }
}

fn load_saved(base_dir: &str, name: &str) -> Option<SavedRequest> {
    let path = requests_path(base_dir).join(format!("{name}.json"));
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}
