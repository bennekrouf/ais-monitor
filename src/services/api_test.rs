//! Generic HTTP request tester — hits any URL (APIM, arbitrary REST
//! endpoints, etc.) with a chosen method, custom headers, and a body,
//! independent of Azure Resource Manager discovery. Complements
//! `azure::trigger_workflow`, which is scoped to Logic App callback URLs.

use std::process::Command;
use crate::services::azure;

#[derive(Clone, Debug, PartialEq)]
pub struct ApiResponse {
    pub status_code: u16,
    pub body: String,
}

/// Send an HTTP request via curl (blocking). Headers are passed as argv
/// entries, never through a shell, so header/body values cannot inject
/// extra curl flags or commands.
pub fn send_request(
    method: &str,
    url: &str,
    headers: &[(String, String)],
    body: &str,
) -> Result<ApiResponse, String> {
    if url.trim().is_empty() {
        return Err("URL is empty".to_string());
    }

    let method = method.trim().to_uppercase();
    let mut args: Vec<String> = vec![
        "-s".into(),
        "-w".into(),
        "\n%{http_code}".into(),
        "-X".into(),
        method.clone(),
    ];

    let mut has_content_type = false;
    for (k, v) in headers {
        let k = k.trim();
        if k.is_empty() {
            continue;
        }
        if k.eq_ignore_ascii_case("content-type") {
            has_content_type = true;
        }
        args.push("-H".into());
        args.push(format!("{k}: {v}"));
    }

    let send_body = !body.trim().is_empty() && method != "GET" && method != "HEAD";
    if send_body {
        if !has_content_type {
            args.push("-H".into());
            args.push("Content-Type: application/json".into());
        }
        args.push("-d".into());
        args.push(body.to_string());
    }

    args.push(url.to_string());

    let output = Command::new("curl")
        .args(&args)
        .output()
        .map_err(|e| format!("curl failed: {e}"))?;

    let full = String::from_utf8_lossy(&output.stdout).to_string();
    let lines: Vec<&str> = full.trim().rsplitn(2, '\n').collect();
    let status_code = lines[0].parse::<u16>().unwrap_or(0);
    let response_body = if lines.len() > 1 { lines[1].to_string() } else { String::new() };

    if status_code == 0 {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        if !stderr.trim().is_empty() {
            return Err(stderr);
        }
    }

    Ok(ApiResponse { status_code, body: response_body })
}

// ── APIM subscription-key lookup ─────────────────────────────────────────────
//
// A Logic App's callback URL (see `azure::get_trigger_url`) is signed via
// ARM's `listCallbackUrl` action, scoped by RBAC on the Logic App resource.
// An APIM product subscription key lives on a *different* resource
// (`Microsoft.ApiManagement/service/.../subscriptions/...`) with its own
// RBAC — access to one does not imply access to the other. These fetch the
// key the same way, via ARM's `listSecrets` action, for whoever does have
// APIM-side access.

#[derive(Clone, Debug, PartialEq)]
pub struct ApimSubscription {
    pub id: String,
    pub display: String,
}

/// Run a blocking closure with a hard deadline. `az rest` invoked from a
/// GUI-launched process (no attached terminal) can hang indefinitely if it
/// hits a token refresh / device-code prompt with nowhere to render — which
/// otherwise looks identical to "nothing happens" in the UI. The worker
/// thread is left to finish on its own; we just stop waiting on it.
fn run_with_timeout<T, F>(f: F, timeout: std::time::Duration) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    rx.recv_timeout(timeout).unwrap_or_else(|_| {
        Err(format!(
            "az CLI call did not return within {}s — it may be stuck on an interactive prompt \
             (device-code login, MFA) with no terminal to show it. Try running the equivalent \
             `az rest` command directly in a terminal to confirm it completes.",
            timeout.as_secs()
        ))
    })
}

pub fn list_apim_subscriptions(
    sub: &str,
    rg: &str,
    service: &str,
) -> Result<Vec<ApimSubscription>, String> {
    let (sub, rg, service) = (sub.to_string(), rg.to_string(), service.to_string());
    run_with_timeout(
        move || list_apim_subscriptions_inner(&sub, &rg, &service),
        std::time::Duration::from_secs(20),
    )
}

fn list_apim_subscriptions_inner(
    sub: &str,
    rg: &str,
    service: &str,
) -> Result<Vec<ApimSubscription>, String> {
    let url = format!(
        "https://management.azure.com/subscriptions/{sub}/resourceGroups/{rg}/providers/Microsoft.ApiManagement/service/{service}/subscriptions?api-version=2022-08-01"
    );
    let output = azure::az_command(&["rest", "--method", "GET", "--url", &url, "--output", "json"])
        .output()
        .map_err(|e| format!("az rest failed: {e}"))?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }

    let body = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&body)
        .map_err(|e| format!("bad JSON from listing APIM subscriptions: {e}"))?;

    let subs = json["value"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|v| {
            let id = v["name"].as_str().unwrap_or_default().to_string();
            let display_name = v["properties"]["displayName"].as_str().map(|s| s.to_string());
            let scope = v["properties"]["scope"].as_str().unwrap_or_default();
            let product = scope.rsplit('/').next().unwrap_or_default().to_string();
            let display = display_name
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| if product.is_empty() { id.clone() } else { product });
            ApimSubscription { id, display }
        })
        .collect();

    Ok(subs)
}

pub fn get_apim_subscription_key(
    sub: &str,
    rg: &str,
    service: &str,
    subscription_id: &str,
) -> Result<String, String> {
    let (sub, rg, service, subscription_id) =
        (sub.to_string(), rg.to_string(), service.to_string(), subscription_id.to_string());
    run_with_timeout(
        move || get_apim_subscription_key_inner(&sub, &rg, &service, &subscription_id),
        std::time::Duration::from_secs(20),
    )
}

fn get_apim_subscription_key_inner(
    sub: &str,
    rg: &str,
    service: &str,
    subscription_id: &str,
) -> Result<String, String> {
    let url = format!(
        "https://management.azure.com/subscriptions/{sub}/resourceGroups/{rg}/providers/Microsoft.ApiManagement/service/{service}/subscriptions/{subscription_id}/listSecrets?api-version=2022-08-01"
    );
    let output = azure::az_command(&["rest", "--method", "POST", "--url", &url, "--output", "json"])
        .output()
        .map_err(|e| format!("az rest failed: {e}"))?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }

    let body = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&body)
        .map_err(|e| format!("bad JSON from listSecrets: {e}"))?;

    json["primaryKey"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| "primaryKey not found in listSecrets response".to_string())
}

/// Extract the APIM service name from a gateway URL, e.g.
/// `https://apim-ipaas-dev-chn-001.azure-api.net/...` → `apim-ipaas-dev-chn-001`.
/// Returns None for non-`*.azure-api.net` hosts (custom domains, other
/// endpoints) since the name can't be inferred there.
pub fn guess_apim_service_from_url(url: &str) -> Option<String> {
    let after_scheme = url.split("://").nth(1)?;
    let host = after_scheme.split('/').next()?;
    host.strip_suffix(".azure-api.net").map(|s| s.to_string())
}

/// Find which resource group an APIM service lives in, given its name —
/// so the RG doesn't have to be typed by hand once the service name is
/// known (either guessed from the URL or entered manually).
pub fn discover_apim_resource_group(sub: &str, service: &str) -> Result<String, String> {
    let query = format!("[?name=='{service}'] | [0].resourceGroup");
    let output = azure::az_command(&[
        "resource", "list",
        "--subscription", sub,
        "--resource-type", "Microsoft.ApiManagement/service",
        "--query", &query,
        "--output", "tsv",
    ])
    .output()
    .map_err(|e| format!("az resource list failed: {e}"))?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }

    let rg = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if rg.is_empty() {
        return Err(format!("No APIM service named '{service}' found in this subscription"));
    }
    Ok(rg)
}
