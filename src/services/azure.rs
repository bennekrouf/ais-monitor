use serde::Deserialize;
use std::process::Command;

#[derive(Clone, Debug, PartialEq)]
pub enum AzLoginState {
    Checking,
    LoggedIn { account: String, subscription_id: String },
    Expired,
    NotLoggedIn,
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
    let output = Command::new("az")
        .args(["account", "show", "--output", "json"])
        .output();

    match output {
        Ok(out) if out.status.success() => {
            let body = String::from_utf8_lossy(&out.stdout);
            match serde_json::from_str::<AzAccount>(&body) {
                Ok(acc) => AzLoginState::LoggedIn {
                    account: acc.name,
                    subscription_id: acc.id,
                },
                Err(_) => AzLoginState::NotLoggedIn,
            }
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            if stderr.contains("AADSTS70043") || stderr.contains("expired") {
                AzLoginState::Expired
            } else {
                AzLoginState::NotLoggedIn
            }
        }
        Err(_) => AzLoginState::NotLoggedIn,
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
            Some(WorkflowInfo { name, state })
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

// ── EventGrid ────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq)]
pub struct EventGridTopic {
    pub name: String,
    pub id: String,
    pub endpoint: String,
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

#[derive(Clone, Debug, PartialEq)]
pub struct EventGridSubscription {
    pub name: String,
    pub destination_type: String,
    pub destination_queue: String,
    pub filters: Vec<EventGridFilter>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EventGridFilter {
    pub key: String,
    pub operator: String,
    pub values: Vec<String>,
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
