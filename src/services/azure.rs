use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::process::Command;

#[derive(Clone, Debug, PartialEq)]
pub enum AzLoginState {
    Checking,
    LoggedIn { account: String, subscription_id: String },
    Expired,
    NotLoggedIn,
    /// Azure CLI binary not found on this machine
    AzNotFound,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct AzAccount {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub id: String,
    #[serde(rename = "tenantId", default)]
    pub tenant_id: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct AzSubscription {
    pub id: String,
    pub name: String,
}

pub fn list_subscriptions() -> Result<Vec<AzSubscription>, String> {
    let output = Command::new("az")
        .args(["account", "list", "--output", "json"])
        .output()
        .map_err(|e| format!("az account list failed: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }
    let body = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(&body).map_err(|e| format!("parse: {e}"))
}

/// A Logic App site discovered directly — includes its resource group.
#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
pub struct LogicAppSite {
    pub name:           String,
    pub resource_group: String,
}

/// List all Logic Apps Standard sites in a subscription directly, without
/// enumerating resource groups first.
///
/// Uses `az resource list --resource-type Microsoft.Web/sites
///   --query "[?contains(kind,'workflowapp')]"`
///
/// This works even with PIM access that only covers specific resource groups
/// (unlike `az group list` which requires subscription-level read access).
pub fn list_logic_app_sites(sub: &str) -> Result<Vec<LogicAppSite>, String> {
    let output = Command::new("az")
        .args([
            "resource", "list",
            "--subscription", sub,
            "--resource-type", "Microsoft.Web/sites",
            "--query", "[?contains(kind, 'workflowapp')].{name:name,resource_group:resourceGroup}",
            "--output", "json",
        ])
        .output()
        .map_err(|e| format!("az resource list failed: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }
    let body = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(&body).map_err(|e| format!("parse: {e}"))
}

#[allow(dead_code)]
pub fn list_logic_apps(sub: &str, rg: &str) -> Result<Vec<String>, String> {
    let output = Command::new("az")
        .args([
            "resource", "list",
            "--subscription", sub,
            "--resource-group", rg,
            "--resource-type", "Microsoft.Web/sites",
            "--output", "json",
        ])
        .output()
        .map_err(|e| format!("az resource list failed: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }
    let body = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&body).map_err(|e| format!("parse: {e}"))?;
    Ok(json.as_array().unwrap_or(&vec![])
        .iter()
        .filter_map(|v| {
            let kind = v["kind"].as_str().unwrap_or("");
            if kind.contains("workflowapp") {
                v["name"].as_str().map(String::from)
            } else {
                None
            }
        })
        .collect())
}

pub fn list_service_bus_namespaces(sub: &str, rg: &str) -> Result<Vec<String>, String> {
    let output = Command::new("az")
        .args([
            "servicebus", "namespace", "list",
            "--subscription", sub,
            "--resource-group", rg,
            "--output", "json",
        ])
        .output()
        .map_err(|e| format!("az servicebus namespace list failed: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }
    let body = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&body).map_err(|e| format!("parse: {e}"))?;
    Ok(json.as_array().unwrap_or(&vec![])
        .iter()
        .filter_map(|v| v["name"].as_str().map(String::from))
        .collect())
}

/// Check current az login status (blocking — call from spawn_blocking)
pub fn check_login() -> AzLoginState {
    // Step 1: get account metadata from local cache
    let account_out = Command::new("az")
        .args(["account", "show", "--output", "json"])
        .output();

    let acc = match account_out {
        Ok(out) if out.status.success() => {
            let body = String::from_utf8_lossy(&out.stdout);
            match serde_json::from_str::<AzAccount>(&body) {
                Ok(a) => a,
                Err(_) => return AzLoginState::NotLoggedIn,
            }
        }
        Ok(_) => return AzLoginState::NotLoggedIn,
        Err(e) => {
            // Command failed to execute — az binary not found
            if e.kind() == std::io::ErrorKind::NotFound {
                return AzLoginState::AzNotFound;
            }
            return AzLoginState::NotLoggedIn;
        }
    };

    // Step 2: validate the token is actually fresh by requesting a new one.
    // `az account show` reads local cache and succeeds even after AADSTS70043 expiry.
    // `az account get-access-token` calls the identity endpoint and fails when the
    // refresh token has expired (conditional access, sign-in frequency policy, etc.).
    let token_out = Command::new("az")
        .args(["account", "get-access-token", "--output", "none"])
        .output();

    match token_out {
        Ok(out) if out.status.success() => AzLoginState::LoggedIn {
            account: acc.name,
            subscription_id: acc.id,
        },
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            if stderr.contains("AADSTS") || stderr.contains("expired") || stderr.contains("refresh token") {
                AzLoginState::Expired
            } else {
                AzLoginState::NotLoggedIn
            }
        }
        Err(e) => {
            if e.kind() == std::io::ErrorKind::NotFound {
                AzLoginState::AzNotFound
            } else {
                AzLoginState::NotLoggedIn
            }
        }
    }
}

/// Open az login in a terminal (non-blocking)
pub fn open_login(tenant: Option<&str>) {
    let mut args = vec!["login".to_string()];
    if let Some(t) = tenant {
        args.push("--tenant".into());
        args.push(t.into());
        args.push("--scope".into());
        args.push("https://management.core.windows.net//.default".into());
    }
    let _ = Command::new("az").args(&args).spawn();
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct WorkflowInfo {
    pub name: String,
    #[serde(default)]
    pub state: Option<String>,
    /// Trigger names from hostruntime metadata, e.g.
    /// "When_messages_are_available_in_ais.foo_(peek-lock)".
    /// Used as a fallback when the ARM definition fetch fails.
    #[serde(skip)]
    pub trigger_names: Vec<String>,
}

/// List workflows deployed on a Logic App (blocking)
pub fn list_deployed_workflows(sub: &str, rg: &str, app: &str) -> Result<Vec<WorkflowInfo>, String> {
    let url = format!(
        "https://management.azure.com/subscriptions/{sub}/resourceGroups/{rg}/providers/Microsoft.Web/sites/{app}/hostruntime/runtime/webhooks/workflow/api/management/workflows?api-version=2024-04-01"
    );

    let output = Command::new("az")
        .args(["rest", "--method", "GET", "--url", &url, "--output", "json"])
        .output()
        .map_err(|e| format!("az rest failed: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("az rest error: {stderr}"));
    }

    let body = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&body)
        .map_err(|e| format!("parse error: {e}"))?;

    // API may return a plain array or {"value": [...]}
    let arr = if json.is_array() {
        json.as_array()
    } else {
        json["value"].as_array()
    };

    let workflows = arr
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|v| {
            let name = v["name"].as_str()?.to_string();
            let state = v["properties"]["state"].as_str().map(String::from);
            // Extract trigger names — the keys of the "triggers" object.
            // These encode queue names in the format:
            //   "When_messages_are_available_in_{queue}_(peek-lock)"
            let trigger_names = v["properties"]["triggers"]
                .as_object()
                .map(|t| t.keys().cloned().collect::<Vec<_>>())
                .unwrap_or_default();
            Some(WorkflowInfo { name, state, trigger_names })
        })
        .collect();

    Ok(workflows)
}

#[derive(Clone, Debug, PartialEq)]
pub struct RunInfo {
    pub id: String,
    pub status: String,
    pub start: String,
    pub end: Option<String>,
}

/// List recent runs for a workflow (blocking)
pub fn list_runs(sub: &str, rg: &str, app: &str, workflow: &str, top: u32) -> Result<Vec<RunInfo>, String> {
    let url = format!(
        "https://management.azure.com/subscriptions/{sub}/resourceGroups/{rg}/providers/Microsoft.Web/sites/{app}/hostruntime/runtime/webhooks/workflow/api/management/workflows/{workflow}/runs?api-version=2024-04-01&$top={top}"
    );

    let output = Command::new("az")
        .args(["rest", "--method", "GET", "--url", &url, "--output", "json"])
        .output()
        .map_err(|e| format!("az rest failed: {e}"))?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }

    let body = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&body)
        .map_err(|e| format!("parse: {e}"))?;

    let runs = json["value"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|v| {
            let props = &v["properties"];
            Some(RunInfo {
                id: v["name"].as_str()?.to_string(),
                status: props["status"].as_str()?.to_string(),
                start: props["startTime"].as_str()?.to_string(),
                end: props["endTime"].as_str().map(String::from),
            })
        })
        .collect();

    Ok(runs)
}

#[derive(Clone, Debug, PartialEq)]
pub struct ActionInfo {
    pub name: String,
    pub status: String,
    pub error: Option<String>,
}

/// List actions for a specific run (blocking)
pub fn list_actions(sub: &str, rg: &str, app: &str, workflow: &str, run_id: &str) -> Result<Vec<ActionInfo>, String> {
    let url = format!(
        "https://management.azure.com/subscriptions/{sub}/resourceGroups/{rg}/providers/Microsoft.Web/sites/{app}/hostruntime/runtime/webhooks/workflow/api/management/workflows/{workflow}/runs/{run_id}/actions?api-version=2024-04-01"
    );

    let output = Command::new("az")
        .args(["rest", "--method", "GET", "--url", &url, "--output", "json"])
        .output()
        .map_err(|e| format!("az rest failed: {e}"))?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }

    let body = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&body)
        .map_err(|e| format!("parse: {e}"))?;

    let actions = json["value"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|v| {
            let props = &v["properties"];
            let error = props["error"]["message"].as_str().map(String::from);
            Some(ActionInfo {
                name: v["name"].as_str()?.to_string(),
                status: props["status"].as_str()?.to_string(),
                error,
            })
        })
        .collect();

    Ok(actions)
}

#[derive(Clone, Debug, PartialEq)]
pub struct QueueInfo {
    pub name: String,
    pub active: i64,
    pub dead_letter: i64,
    pub scheduled: i64,
}

/// List trigger names for a workflow (blocking)
pub fn list_triggers(sub: &str, rg: &str, app: &str, workflow: &str) -> Result<Vec<String>, String> {
    let url = format!(
        "https://management.azure.com/subscriptions/{sub}/resourceGroups/{rg}/providers/Microsoft.Web/sites/{app}/hostruntime/runtime/webhooks/workflow/api/management/workflows/{workflow}/triggers?api-version=2024-04-01"
    );

    let output = Command::new("az")
        .args(["rest", "--method", "GET", "--url", &url, "--output", "json"])
        .output()
        .map_err(|e| format!("az rest failed: {e}"))?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }

    let body = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&body)
        .map_err(|e| format!("parse: {e}"))?;

    let arr = if json.is_array() { json.as_array() } else { json["value"].as_array() };

    Ok(arr
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|v| v["name"].as_str().map(String::from))
        .collect())
}

/// Get the callback URL for a workflow trigger (blocking).
/// Tries each trigger name until one returns a callback URL.
pub fn get_trigger_url(sub: &str, rg: &str, app: &str, workflow: &str) -> Result<String, String> {
    let triggers = list_triggers(sub, rg, app, workflow)
        .unwrap_or_else(|_| vec!["manual".into()]);

    let trigger_names = if triggers.is_empty() { vec!["manual".into()] } else { triggers };

    for trigger_name in &trigger_names {
        let url = format!(
            "https://management.azure.com/subscriptions/{sub}/resourceGroups/{rg}/providers/Microsoft.Web/sites/{app}/hostruntime/runtime/webhooks/workflow/api/management/workflows/{workflow}/triggers/{trigger_name}/listCallbackUrl?api-version=2024-04-01"
        );

        let output = Command::new("az")
            .args(["rest", "--method", "POST", "--url", &url, "--output", "json"])
            .output()
            .map_err(|e| format!("az rest failed: {e}"))?;

        if !output.status.success() {
            continue;
        }

        let body = String::from_utf8_lossy(&output.stdout);
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&body) {
            if let Some(val) = json["value"].as_str() {
                return Ok(val.to_string());
            }
        }
    }

    Err(format!("No callback URL found. Triggers: {}. This workflow may use a non-HTTP trigger (Service Bus, Timer, etc.)", trigger_names.join(", ")))
}

/// Trigger a workflow by POSTing a JSON payload to its callback URL (blocking)
pub fn trigger_workflow(callback_url: &str, payload: &str) -> Result<TriggerResult, String> {
    let output = Command::new("curl")
        .args([
            "-s", "-w", "\n%{http_code}",
            "-X", "POST",
            "-H", "Content-Type: application/json",
            "-d", payload,
            callback_url,
        ])
        .output()
        .map_err(|e| format!("curl failed: {e}"))?;

    let full = String::from_utf8_lossy(&output.stdout).to_string();
    let lines: Vec<&str> = full.trim().rsplitn(2, '\n').collect();
    let status_code = lines[0].parse::<u16>().unwrap_or(0);
    let response_body = if lines.len() > 1 { lines[1].to_string() } else { String::new() };

    Ok(TriggerResult {
        status_code,
        body: response_body,
    })
}

#[derive(Clone, Debug, PartialEq)]
pub struct TriggerResult {
    pub status_code: u16,
    pub body: String,
}

/// Fetch the app's published configuration as a key→value map (blocking).
/// Used to resolve @appsetting('VAR') references in queue names.
pub fn get_app_settings(sub: &str, rg: &str, app: &str) -> Result<std::collections::HashMap<String, String>, String> {
    let output = Command::new("az")
        .args([
            "webapp", "config", "appsettings", "list",
            "--subscription", sub,
            "--resource-group", rg,
            "--name", app,
            "--output", "json",
        ])
        .output()
        .map_err(|e| format!("az appsettings failed: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }
    let body = String::from_utf8_lossy(&output.stdout);
    let arr: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap_or_default();
    Ok(arr.iter().filter_map(|v| {
        let k = v["name"].as_str()?.to_string();
        let val = v["value"].as_str().unwrap_or("").to_string();
        Some((k, val))
    }).collect())
}

/// Fetch the full workflow definition (blocking).
///
/// Uses the ARM `Microsoft.Web/sites/workflows` resource endpoint which returns
/// `{ "properties": { "files": { "workflow.json": { "definition": {...}, "kind": "..." } } } }`.
///
/// NOTE: the hostruntime management endpoint returns only METADATA (trigger names
/// but no parameters), so we use the ARM resource endpoint instead.
pub fn get_workflow_definition(sub: &str, rg: &str, app: &str, workflow: &str) -> Result<serde_json::Value, String> {
    // ARM resource endpoint — returns the files including workflow.json content
    let uri = format!(
        "https://management.azure.com/subscriptions/{sub}/resourceGroups/{rg}\
         /providers/Microsoft.Web/sites/{app}/workflows/{workflow}?api-version=2023-12-01"
    );

    let output = Command::new("az")
        .args(["rest", "--method", "GET", "--uri", &uri, "--output", "json"])
        .output()
        .map_err(|e| format!("az rest failed: {e}"))?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }

    let body = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&body).map_err(|e| format!("parse: {e}"))?;

    // Extract the workflow.json content from properties.files["workflow.json"]
    // That value IS the workflow.json file (has "definition" key at root)
    if let Some(wf) = json.get("properties")
        .and_then(|p| p.get("files"))
        .and_then(|f| f.get("workflow.json"))
    {
        return Ok(wf.clone());
    }

    // Fallback: return the whole response and let parse_workflow_json handle it
    Ok(json)
}

/// Get queue message counts (blocking)
pub fn check_queue(sb_namespace: &str, rg: &str, queue_name: &str) -> Result<QueueInfo, String> {
    let output = Command::new("az")
        .args([
            "servicebus", "queue", "show",
            "--namespace-name", sb_namespace,
            "--resource-group", rg,
            "--name", queue_name,
            "--output", "json",
        ])
        .output()
        .map_err(|e| format!("az sb failed: {e}"))?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }

    let body = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&body)
        .map_err(|e| format!("parse: {e}"))?;

    let cd = &json["countDetails"];
    Ok(QueueInfo {
        name: queue_name.to_string(),
        active: cd["activeMessageCount"].as_i64().unwrap_or(0),
        dead_letter: cd["deadLetterMessageCount"].as_i64().unwrap_or(0),
        scheduled: cd["scheduledMessageCount"].as_i64().unwrap_or(0),
    })
}

/// Fetch the primary connection string for a Service Bus namespace via az CLI.
pub fn sb_get_connection_string(rg: &str, namespace: &str) -> Result<String, String> {
    let output = Command::new("az")
        .args([
            "servicebus", "namespace", "authorization-rule", "keys", "list",
            "--resource-group", rg,
            "--namespace-name", namespace,
            "--name", "RootManageSharedAccessKey",
            "--query", "primaryConnectionString",
            "-o", "tsv",
        ])
        .output()
        .map_err(|e| format!("az failed: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// URL-encode with lowercase hex digits to match C#'s HttpUtility.UrlEncode,
/// which Azure SAS validation uses server-side.
fn lowercase_url_encode(s: &str) -> String {
    let mut result = String::with_capacity(s.len() * 3);
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                result.push(byte as char);
            }
            _ => {
                result.push('%');
                result.push_str(&format!("{:02x}", byte)); // lowercase hex
            }
        }
    }
    result
}

/// Send a message to a Service Bus queue using the REST API with SAS auth.
/// `conn_str` is the full connection string from `sb_get_connection_string`.
pub async fn sb_send_message(conn_str: &str, queue: &str, body: &str) -> Result<(), String> {
    // Parse connection string
    let mut endpoint = "";
    let mut key_name = "";
    let mut key = "";
    for part in conn_str.split(';') {
        let part = part.trim();
        if let Some(v) = part.strip_prefix("Endpoint=sb://") {
            endpoint = v.trim_end_matches('/');
        } else if let Some(v) = part.strip_prefix("SharedAccessKeyName=") {
            key_name = v;
        } else if let Some(v) = part.strip_prefix("SharedAccessKey=") {
            key = v;
        }
    }
    if endpoint.is_empty() || key.is_empty() {
        return Err(format!("Invalid connection string (endpoint={}, key_name={}, key_len={})",
            endpoint.is_empty(), key_name, key.len()));
    }

    eprintln!("[SB Send] endpoint='{}' key_name='{}' key_len={}", endpoint, key_name, key.len());

    let url = format!("https://{}/{}/messages", endpoint, queue);

    // Generate SAS token (valid 5 minutes)
    // Azure SB SAS spec:
    //   StringToSign = URL_ENCODE(lowercase(resource_uri)) + "\n" + expiry
    //   resource_uri = "https://<fqdn>/<queue>"
    let expiry = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() + 300;

    // The resource URI for signing — use https:// scheme per Azure REST API.
    // URL-encode must use lowercase hex (%3a not %3A) to match Azure's server-side
    // validation (C# HttpUtility.UrlEncode uses lowercase).
    let resource_uri = format!("https://{}/{}", endpoint, queue).to_lowercase();
    let encoded_resource = lowercase_url_encode(&resource_uri);
    let to_sign = format!("{}\n{}", encoded_resource, expiry);

    eprintln!("[SB Send] url: {}", url);
    eprintln!("[SB Send] resource_uri: {}", resource_uri);
    eprintln!("[SB Send] to_sign: {:?}", to_sign);

    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    type HmacSha256 = Hmac<Sha256>;

    let decoded_key = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, key)
        .map_err(|e| format!("base64 decode: {e}"))?;
    let mut mac = HmacSha256::new_from_slice(&decoded_key)
        .map_err(|e| format!("hmac: {e}"))?;
    mac.update(to_sign.as_bytes());
    let sig_bytes = mac.finalize().into_bytes();
    let signature = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, sig_bytes);
    let encoded_sig = lowercase_url_encode(&signature);

    let token = format!(
        "SharedAccessSignature sr={}&sig={}&se={}&skn={}",
        encoded_resource, encoded_sig, expiry, key_name
    );

    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .header("Authorization", &token)
        .header("Content-Type", "application/atom+xml;type=entry;charset=utf-8")
        .body(body.to_string())
        .send()
        .await
        .map_err(|e| format!("HTTP error: {e}"))?;

    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    eprintln!("[SB Send] response: {} {}", status, &text[..200.min(text.len())]);
    if status.is_success() || status.as_u16() == 201 {
        Ok(())
    } else if status.as_u16() == 401 && text.is_empty() {
        // Empty 401 = network-level rejection (SB firewall / IP not allowlisted)
        Err("401 — your IP is not in the Service Bus firewall allowlist. Connect to VPN or add your IP in the Azure portal (SB namespace → Networking).".into())
    } else {
        Err(format!("SB returned {}: {}", status, text))
    }
}

// ── EventGrid ────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EventGridTopic {
    pub name: String,
    pub id: String,
    pub endpoint: String,
}

// ── System Topics ────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EventGridSystemTopic {
    pub name: String,
    pub id: String,
    pub source: String,
    pub topic_type: String,
}

/// List EventGrid system topics in a resource group (blocking)
pub fn list_eventgrid_system_topics(rg: &str) -> Result<Vec<EventGridSystemTopic>, String> {
    let output = Command::new("az")
        .args([
            "eventgrid", "system-topic", "list",
            "--resource-group", rg,
            "--output", "json",
        ])
        .output()
        .map_err(|e| format!("az eventgrid failed: {e}"))?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }

    let body = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&body)
        .map_err(|e| format!("parse: {e}"))?;

    let arr = if json.is_array() { json.as_array() } else { json["value"].as_array() };

    let topics = arr
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|v| {
            Some(EventGridSystemTopic {
                name: v["name"].as_str()?.to_string(),
                id: v["id"].as_str()?.to_string(),
                source: v["source"].as_str().unwrap_or("").to_string(),
                topic_type: v["topicType"].as_str().unwrap_or("").to_string(),
            })
        })
        .collect();

    Ok(topics)
}

/// List event subscriptions under a system topic (blocking)
pub fn list_eventgrid_system_topic_subscriptions(rg: &str, topic_name: &str) -> Result<Vec<EventGridSubscription>, String> {
    let output = Command::new("az")
        .args([
            "eventgrid", "system-topic", "event-subscription", "list",
            "--resource-group", rg,
            "--system-topic-name", topic_name,
            "--output", "json",
        ])
        .output()
        .map_err(|e| format!("az eventgrid failed: {e}"))?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }

    let body = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&body)
        .map_err(|e| format!("parse: {e}"))?;

    let arr = if json.is_array() { json.as_array() } else { json["value"].as_array() };

    let subs = arr
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|v| {
            let name = v["name"].as_str()?.to_string();
            let dest = &v["destination"];
            let dest_type = dest["endpointType"].as_str().unwrap_or("").to_string();
            let dest_queue = dest["properties"]["resourceId"]
                .as_str()
                .and_then(|rid| rid.rsplit('/').next())
                .or_else(|| dest["properties"]["endpointUrl"].as_str())
                .unwrap_or("")
                .to_string();

            let mut filters = Vec::new();
            if let Some(filter) = v["filter"].as_object() {
                // Subject filters
                if let Some(s) = filter.get("subjectBeginsWith").and_then(|v| v.as_str()) {
                    if !s.is_empty() {
                        filters.push(EventGridFilter {
                            key: "Subject".into(),
                            operator: "BeginsWith".into(),
                            values: vec![s.to_string()],
                        });
                    }
                }
                if let Some(s) = filter.get("subjectEndsWith").and_then(|v| v.as_str()) {
                    if !s.is_empty() {
                        filters.push(EventGridFilter {
                            key: "Subject".into(),
                            operator: "EndsWith".into(),
                            values: vec![s.to_string()],
                        });
                    }
                }
                // Advanced filters
                if let Some(adv) = filter.get("advancedFilters").and_then(|f| f.as_array()) {
                    for af in adv {
                        let key = af["key"].as_str().unwrap_or("").to_string();
                        let op = af["operatorType"].as_str().unwrap_or("").to_string();
                        let values: Vec<String> = af["values"]
                            .as_array()
                            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                            .or_else(|| af["value"].as_str().map(|s| vec![s.to_string()]))
                            .unwrap_or_default();
                        filters.push(EventGridFilter { key, operator: op, values });
                    }
                }
            }

            Some(EventGridSubscription {
                name,
                destination_type: dest_type,
                destination_queue: dest_queue,
                filters,
            })
        })
        .collect();

    Ok(subs)
}

/// List EventGrid topics in a resource group (blocking)
pub fn list_eventgrid_topics(rg: &str) -> Result<Vec<EventGridTopic>, String> {
    let output = Command::new("az")
        .args([
            "eventgrid", "topic", "list",
            "--resource-group", rg,
            "--output", "json",
        ])
        .output()
        .map_err(|e| format!("az eventgrid failed: {e}"))?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }

    let body = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&body)
        .map_err(|e| format!("parse: {e}"))?;

    let arr = if json.is_array() { json.as_array() } else { json["value"].as_array() };

    let topics = arr
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|v| {
            Some(EventGridTopic {
                name: v["name"].as_str()?.to_string(),
                id: v["id"].as_str()?.to_string(),
                endpoint: v["endpoint"].as_str().unwrap_or("").to_string(),
            })
        })
        .collect();

    Ok(topics)
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EventGridSubscription {
    pub name: String,
    pub destination_type: String,
    pub destination_queue: String,
    pub filters: Vec<EventGridFilter>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EventGridFilter {
    pub key: String,
    pub operator: String,
    pub values: Vec<String>,
}

/// Describes which Event Grid topic+subscription feeds a given queue.
#[derive(Clone, Debug, PartialEq)]
pub struct EgLink {
    pub topic_name: String,
    pub subscription_name: String,
    pub filters: Vec<EventGridFilter>,
}

/// Build a map from destination queue name → EgLink.
/// Fetches all custom topics + system topics and their subscriptions.
pub fn build_eg_links(rg: &str) -> HashMap<String, EgLink> {
    let mut map = HashMap::new();

    // Custom topics
    if let Ok(topics) = list_eventgrid_topics(rg) {
        for t in &topics {
            if let Ok(subs) = list_eventgrid_subscriptions(&t.id) {
                for s in &subs {
                    if !s.destination_queue.is_empty() {
                        map.insert(s.destination_queue.clone(), EgLink {
                            topic_name: t.name.clone(),
                            subscription_name: s.name.clone(),
                            filters: s.filters.clone(),
                        });
                    }
                }
            }
        }
    }

    // System topics
    if let Ok(sys_topics) = list_eventgrid_system_topics(rg) {
        for st in &sys_topics {
            if let Ok(subs) = list_eventgrid_system_topic_subscriptions(rg, &st.name) {
                for s in &subs {
                    if !s.destination_queue.is_empty() {
                        map.insert(s.destination_queue.clone(), EgLink {
                            topic_name: st.name.clone(),
                            subscription_name: s.name.clone(),
                            filters: s.filters.clone(),
                        });
                    }
                }
            }
        }
    }

    map
}

/// List EventGrid subscriptions for a topic (blocking)
pub fn list_eventgrid_subscriptions(topic_id: &str) -> Result<Vec<EventGridSubscription>, String> {
    let output = Command::new("az")
        .args([
            "eventgrid", "event-subscription", "list",
            "--source-resource-id", topic_id,
            "--output", "json",
        ])
        .output()
        .map_err(|e| format!("az eventgrid failed: {e}"))?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }

    let body = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&body)
        .map_err(|e| format!("parse: {e}"))?;

    let arr = if json.is_array() { json.as_array() } else { json["value"].as_array() };

    let subs = arr
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|v| {
            let name = v["name"].as_str()?.to_string();
            let dest = &v["destination"];
            let dest_type = dest["endpointType"].as_str().unwrap_or("").to_string();
            // Extract queue name from resourceId — last segment
            let dest_queue = dest["properties"]["resourceId"]
                .as_str()
                .and_then(|rid| rid.rsplit('/').next())
                .unwrap_or("")
                .to_string();

            // Parse advanced filters
            let mut filters = Vec::new();
            if let Some(filter) = v["filter"].as_object() {
                if let Some(adv) = filter.get("advancedFilters").and_then(|f| f.as_array()) {
                    for af in adv {
                        let key = af["key"].as_str().unwrap_or("").to_string();
                        let op = af["operatorType"].as_str().unwrap_or("").to_string();
                        let values: Vec<String> = af["values"]
                            .as_array()
                            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                            .or_else(|| af["value"].as_str().map(|s| vec![s.to_string()]))
                            .unwrap_or_default();
                        filters.push(EventGridFilter { key, operator: op, values });
                    }
                }
            }

            Some(EventGridSubscription {
                name,
                destination_type: dest_type,
                destination_queue: dest_queue,
                filters,
            })
        })
        .collect();

    Ok(subs)
}
