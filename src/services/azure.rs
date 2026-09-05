use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::process::Command;

/// On Windows, `az` ships as a `.cmd` batch file (not an `.exe`), and Dioxus
/// desktop apps don't inherit the full shell PATH that resolves it. We must
/// invoke it via `cmd /c az ...` and explicitly add the CLI install dir to
/// PATH for `cmd.exe` to find `az.cmd`.
#[cfg(target_os = "windows")]
fn resolve_az_windows() -> String {
    let candidates: &[(&str, &str)] = &[
        (
            "ProgramFiles(x86)",
            r"Microsoft SDKs\Azure\CLI2\wbin\az.cmd",
        ),
        ("ProgramFiles", r"Microsoft SDKs\Azure\CLI2\wbin\az.cmd"),
        ("LOCALAPPDATA", r"Programs\Azure CLI\wbin\az.cmd"),
    ];
    for (env_var, suffix) in candidates {
        if let Ok(base) = std::env::var(env_var) {
            let full = std::path::Path::new(&base).join(suffix);
            if full.is_file() {
                return full.to_string_lossy().to_string();
            }
        }
    }
    "az".to_string()
}

/// On macOS, apps launched from Finder/Dock inherit a minimal PATH
/// (`/usr/bin:/bin:/usr/sbin:/sbin`) and miss Homebrew's bin dirs. On Linux,
/// pip-user / snap installs also live outside the GUI PATH. Probe known
/// install locations and return the first hit so `Command::new` works.
#[cfg(not(target_os = "windows"))]
fn resolve_az_unix() -> String {
    let mut candidates: Vec<std::path::PathBuf> = vec![
        "/opt/homebrew/bin/az".into(), // brew on Apple Silicon
        "/usr/local/bin/az".into(),    // brew on Intel / manual
        "/usr/bin/az".into(),          // distro package
        "/snap/bin/az".into(),         // snap
    ];
    if let Ok(home) = std::env::var("HOME") {
        candidates.push(std::path::Path::new(&home).join(".local/bin/az"));
    }
    for c in &candidates {
        if c.is_file() {
            return c.to_string_lossy().to_string();
        }
    }
    "az".to_string()
}

fn az_not_found_message() -> String {
    #[cfg(target_os = "macos")]
    {
        "Azure CLI not found. Install with `brew install azure-cli` then restart the app."
            .to_string()
    }
    #[cfg(target_os = "linux")]
    {
        "Azure CLI not found. Install from https://aka.ms/installazurecli-linux then restart the app.".to_string()
    }
    #[cfg(target_os = "windows")]
    {
        "Azure CLI not found. Install it from https://aka.ms/installazurecliwindows then restart the app.".to_string()
    }
}

/// True when an `az` failure means "the service or the local network stack is
/// refusing load right now", as opposed to a real problem with the resource.
///
/// Covers explicit ARM throttling (HTTP 429 / `51020`) and the transport-level
/// failures the CLI reports when connections are being refused or torn down
/// mid-handshake — `Connection aborted`, `Connection reset by peer`, and
/// `OSError(22, 'Invalid argument')`. They share a cause (too much request
/// volume in too short a window) and the same remedy (stop, wait, retry
/// later), so callers treat them identically.
pub fn is_throttling_error(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    lower.contains("429")
        || lower.contains("too many requests")
        || lower.contains("throttl")
        || lower.contains("connection aborted")
        || lower.contains("connection reset")
        || lower.contains("invalid argument")
}

/// True when an `az` failure means "this identity is not allowed to read
/// this", rather than a transient fault. Superseded by
/// [`classify_auth_error`] wherever the caller needs to tell a stale token
/// (fixed by `az login`) apart from a genuine RBAC denial (not fixed by
/// it) — kept here, and covered by tests, as the coarser "is this auth at
/// all" check for any future caller that only needs that.
#[allow(dead_code)]
pub fn is_authorization_error(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    lower.contains("authorizationfailed")
        || lower.contains("does not have authorization")
        || lower.contains("forbidden")
}

/// True when Azure itself failed to serve the request — a gateway or
/// availability fault (502/503/504, `BadGatewayConnection`) rather than
/// anything about the caller or the resource. The Logic App runtime is
/// reached through a shared front end, so when it is unreachable every
/// workflow in the app fails identically until it recovers on its own.
///
/// Deliberately not matched by [`is_throttling_error`]: the remedy (wait and
/// retry) is the same, but conflating them would report an Azure-side outage
/// as the app sending too many requests.
pub fn is_service_unavailable_error(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    lower.contains("badgateway")
        || lower.contains("bad gateway")
        || lower.contains("service unavailable")
        || lower.contains("serviceunavailable")
        || lower.contains("gateway timeout")
        || lower.contains("gatewaytimeout")
        || lower.contains("network connectivity issue")
}

/// Build a `Command` that invokes the Azure CLI cross-platform. On both
/// platforms this resolves `az` against known install dirs that GUI apps
/// don't otherwise see in PATH, then hands the path straight to
/// `Command::new` — including on Windows, where `az` is a `.cmd` batch
/// file; Rust's std library (1.77.2+) detects that and applies its own
/// correct, security-hardened escaping, so no manual `cmd /c` wrapping is
/// needed or safe to hand-roll (see the comment in the Windows branch).
pub fn az_command(args: &[&str]) -> Command {
    #[cfg(target_os = "windows")]
    {
        let az_path = resolve_az_windows();
        // Manually wrapping with `cmd /c az ...` and hand-escaping metachars
        // was tried and reverted twice: `az` is a `.cmd` batch file, and
        // batch files re-parse their own body (including `%*` parameter
        // expansion) through cmd.exe a second time, so a single layer of
        // `^`-escaping gets consumed by the outer `cmd /c` and the bare
        // metachar (e.g. `&` in `...?api-version=X&$top=20`) reappears and
        // splits the command again at the inner layer. This exact class of
        // bug — safely invoking `.bat`/`.cmd` files with argument content
        // cmd.exe treats specially — is what Rust's std library fixed for
        // real in 1.77.2 (the "BatBadBut" advisory, GHSA-q455-hj7f-vrqg):
        // `Command::new`/`.args()` now detects a `.bat`/`.cmd` target and
        // applies correct, security-hardened escaping itself. So just hand
        // it the path and args directly — no manual `cmd /c` needed.
        let mut cmd = Command::new(&az_path);
        cmd.args(args);
        if az_path != "az" {
            if let Some(dir) = std::path::Path::new(&az_path).parent() {
                let current = std::env::var("PATH").unwrap_or_default();
                cmd.env("PATH", format!("{};{}", dir.display(), current));
            }
        }
        cmd
    }
    #[cfg(not(target_os = "windows"))]
    {
        let az_path = resolve_az_unix();
        let mut cmd = Command::new(&az_path);
        cmd.args(args);
        if az_path != "az" {
            if let Some(dir) = std::path::Path::new(&az_path).parent() {
                let current = std::env::var("PATH").unwrap_or_default();
                cmd.env("PATH", format!("{}:{}", dir.display(), current));
            }
        }
        cmd
    }
}

/// Async counterpart to `az_command`, for long-running/streaming commands
/// (currently just log tail) that need non-blocking stdout — same PATH
/// resolution logic, `tokio::process::Command` instead of `std::process`.
pub fn az_command_tokio(args: &[&str]) -> tokio::process::Command {
    #[cfg(target_os = "windows")]
    {
        let az_path = resolve_az_windows();
        let mut cmd = tokio::process::Command::new(&az_path);
        cmd.args(args);
        if az_path != "az" {
            if let Some(dir) = std::path::Path::new(&az_path).parent() {
                let current = std::env::var("PATH").unwrap_or_default();
                cmd.env("PATH", format!("{};{}", dir.display(), current));
            }
        }
        cmd
    }
    #[cfg(not(target_os = "windows"))]
    {
        let az_path = resolve_az_unix();
        let mut cmd = tokio::process::Command::new(&az_path);
        cmd.args(args);
        if az_path != "az" {
            if let Some(dir) = std::path::Path::new(&az_path).parent() {
                let current = std::env::var("PATH").unwrap_or_default();
                cmd.env("PATH", format!("{}:{}", dir.display(), current));
            }
        }
        cmd
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum AzLoginState {
    Checking,
    LoggedIn {
        account: String,
        subscription_id: String,
    },
    Expired,
    NotLoggedIn,
    /// Azure CLI binary not found on this machine
    AzNotFound,
}

/// Classification of Azure-side errors that the UI surfaces specially.
/// Distinguishing these matters because the remedies are different:
/// re-logging in only fixes `TokenExpired`; `MissingPermission` requires an
/// RBAC role assignment from a subscription/resource owner.
#[derive(Clone, Debug, PartialEq)]
pub enum AzAuthKind {
    /// The access token is expired, revoked, or the user isn't signed in.
    /// Recovery: `az login`.
    TokenExpired,
    /// The signed-in principal is authenticated but lacks the required RBAC
    /// role on the target scope. Recovery: ask an Owner / User Access
    /// Administrator to grant a role on the Logic App or its resource group.
    MissingPermission,
}

/// Classify a raw `az rest` stderr (or any wrapped error) into a
/// re-authable category. Returns `None` for unrelated errors.
///
/// Note: `AuthorizationFailed` is *RBAC* in Azure — distinct from
/// `AuthenticationFailed` / `Expired…Token` which are token issues.
/// We previously lumped both as "auth" which sent users in circles
/// re-signing-in for what was a permissions problem.
pub fn classify_auth_error(s: &str) -> Option<AzAuthKind> {
    // Token / sign-in problems — re-login fixes these.
    const TOKEN_NEEDLES: &[&str] = &[
        "ExpiredAuthenticationToken",
        "InvalidAuthenticationToken",
        "TokenExpired",
        "access token has expired",
        "Please run 'az login'",
        "AuthenticationFailed",
        "AADSTS70043",  // token expired
        "AADSTS50173",  // session expired due to inactivity
        "AADSTS700082", // refresh token expired
        "AADSTS50058",  // silent sign-in failed
        "AADSTS50076",  // MFA required
    ];
    if TOKEN_NEEDLES.iter().any(|n| s.contains(n)) {
        return Some(AzAuthKind::TokenExpired);
    }

    // RBAC denial — re-login won't help; user needs a role assignment.
    const RBAC_NEEDLES: &[&str] = &[
        "AuthorizationFailed",
        "does not have authorization to perform action",
        "\"code\":\"Forbidden\"",
        "Forbidden(",
        "InsufficientPrivileges",
    ];
    if RBAC_NEEDLES.iter().any(|n| s.contains(n)) {
        return Some(AzAuthKind::MissingPermission);
    }

    None
}

/// Convenience wrapper kept for any caller that just needs "is this auth-ish?".
#[allow(dead_code)]
pub fn is_auth_error(s: &str) -> bool {
    classify_auth_error(s).is_some()
}

/// Turn a raw `az rest` stderr blob into a short, user-readable message.
/// Falls back to the raw text if no known pattern is recognised. Always
/// keeps the underlying detail so the diagnostic log/tooltip can show it.
fn friendly_az_error(stderr: &str) -> String {
    match classify_auth_error(stderr) {
        Some(AzAuthKind::TokenExpired) => {
            let detail = extract_inner_message(stderr).unwrap_or_else(|| stderr.trim().to_string());
            format!("Azure session expired or invalid — sign in again.\n\nDetails: {detail}")
        }
        Some(AzAuthKind::MissingPermission) => {
            let detail = extract_inner_message(stderr).unwrap_or_else(|| stderr.trim().to_string());
            format!(
                "Signed-in account is missing the role required to read this Logic App.\n\nDetails: {detail}"
            )
        }
        None => format!("az rest error: {}", stderr.trim()),
    }
}

/// Pull the inner `"message"` value out of Azure's wrapped JSON error so the
/// friendly banner doesn't show the surrounding `Forbidden({...})` envelope.
fn extract_inner_message(stderr: &str) -> Option<String> {
    let line = stderr.lines().find(|l| l.contains("\"message\""))?;
    let start = line.find("\"message\":\"")? + "\"message\":\"".len();
    let rest = &line[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
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
    let output = az_command(&["account", "list", "--output", "json"])
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
    pub name: String,
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
    let output = az_command(&[
        "resource",
        "list",
        "--subscription",
        sub,
        "--resource-type",
        "Microsoft.Web/sites",
        "--query",
        "[?contains(kind, 'workflowapp')].{name:name,resource_group:resourceGroup}",
        "--output",
        "json",
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
    let output = az_command(&[
        "resource",
        "list",
        "--subscription",
        sub,
        "--resource-group",
        rg,
        "--resource-type",
        "Microsoft.Web/sites",
        "--output",
        "json",
    ])
    .output()
    .map_err(|e| format!("az resource list failed: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }
    let body = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&body).map_err(|e| format!("parse: {e}"))?;
    Ok(json
        .as_array()
        .unwrap_or(&vec![])
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

/// Returns the Azure region of a `Microsoft.Web/sites` resource, e.g.
/// `"Switzerland North"` or `"westeurope"`. Required for the Logic Apps
/// `WorkflowMenuBlade` deep-link — without it the Portal fails the blade
/// init with `ErrorInitializing: missing 'location'`.
pub fn get_site_location(sub: &str, rg: &str, site: &str) -> Result<String, String> {
    let output = az_command(&[
        "webapp",
        "show",
        "--subscription",
        sub,
        "--resource-group",
        rg,
        "--name",
        site,
        "--query",
        "location",
        "--output",
        "tsv",
    ])
    .output()
    .map_err(|e| format!("az webapp show failed: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    let loc = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if loc.is_empty() {
        Err("empty location returned by az".into())
    } else {
        Ok(loc)
    }
}

pub fn list_service_bus_namespaces(sub: &str, rg: &str) -> Result<Vec<String>, String> {
    let output = az_command(&[
        "servicebus",
        "namespace",
        "list",
        "--subscription",
        sub,
        "--resource-group",
        rg,
        "--output",
        "json",
    ])
    .output()
    .map_err(|e| format!("az servicebus namespace list failed: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }
    let body = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&body).map_err(|e| format!("parse: {e}"))?;
    Ok(json
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|v| v["name"].as_str().map(String::from))
        .collect())
}

/// Check current az login status (blocking — call from spawn_blocking)
/// The tenant the CLI session is currently on, if any.
///
/// Cheap: `az account show` reads the local token cache, no network call. Used
/// to decide whether a profile in another tenant needs a switch at all —
/// without it the only options are "always open a browser" or "never".
pub fn current_tenant() -> Option<String> {
    let out = az_command(&["account", "show", "--query", "tenantId", "--output", "tsv"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let tenant = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!tenant.is_empty()).then_some(tenant)
}

/// Whether a profile's tenant differs from the one the CLI is signed in to.
///
/// A profile with no tenant set, or a session whose tenant cannot be read,
/// counts as "no switch needed": guessing wrong here would open a browser on
/// every profile open, which is what this replaced.
pub fn needs_tenant_switch(profile_tenant: &str, session_tenant: Option<&str>) -> bool {
    let wanted = profile_tenant.trim();
    match session_tenant {
        _ if wanted.is_empty() => false,
        Some(current) => !current.trim().eq_ignore_ascii_case(wanted),
        None => false,
    }
}

pub fn check_login() -> AzLoginState {
    // Step 1: get account metadata from local cache
    let account_out = az_command(&["account", "show", "--output", "json"]).output();

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
    let token_out = az_command(&["account", "get-access-token", "--output", "none"]).output();

    match token_out {
        Ok(out) if out.status.success() => AzLoginState::LoggedIn {
            account: acc.name,
            subscription_id: acc.id,
        },
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            if stderr.contains("AADSTS")
                || stderr.contains("expired")
                || stderr.contains("refresh token")
            {
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

/// Open `az login` (non-blocking). `az login` opens the browser for OAuth on
/// all platforms, so we don't need a visible terminal. Returns Ok if the
/// child process spawned successfully, Err with a human-readable message
/// otherwise — surface this in the UI so the user gets feedback instead of
/// silently staying on "Not logged in".
pub fn open_login(tenant: Option<&str>) -> Result<(), String> {
    let mut args: Vec<&str> = vec!["login"];
    let tenant_owned;
    if let Some(t) = tenant.filter(|t| !t.is_empty()) {
        tenant_owned = t.to_string();
        args.extend_from_slice(&["--tenant", &tenant_owned]);
        args.extend_from_slice(&["--scope", "https://management.core.windows.net//.default"]);
    }
    az_command(&args).spawn().map(|_| ()).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            az_not_found_message()
        } else {
            format!("Failed to start 'az login': {e}")
        }
    })
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
pub fn list_deployed_workflows(
    sub: &str,
    rg: &str,
    app: &str,
) -> Result<Vec<WorkflowInfo>, String> {
    let url = format!(
        "https://management.azure.com/subscriptions/{sub}/resourceGroups/{rg}/providers/Microsoft.Web/sites/{app}/hostruntime/runtime/webhooks/workflow/api/management/workflows?api-version=2024-04-01"
    );

    let output = az_command(&["rest", "--method", "GET", "--url", &url, "--output", "json"])
        .output()
        .map_err(|e| format!("az rest failed: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(friendly_az_error(&stderr));
    }

    let body = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("parse error: {e}"))?;

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
            // The hostruntime management API returns each workflow flat —
            // `{name, kind, isDisabled, health:{state}, triggers:{...}}` — with
            // no `properties` envelope (unlike the ARM resource endpoints).
            // Reading `properties.*` here silently yielded None/empty for every
            // workflow, which killed the trigger-name fallback below in
            // `remote_chain`. Keep the `properties.*` lookups as a fallback so
            // an envelope-shaped response still works.
            let state = v["health"]["state"]
                .as_str()
                .or_else(|| v["properties"]["state"].as_str())
                .map(String::from);
            // Extract trigger names — the keys of the "triggers" object.
            // These encode queue names in the format:
            //   "When_messages_are_available_in_{queue}_(peek-lock)"
            let trigger_names = v["triggers"]
                .as_object()
                .or_else(|| v["properties"]["triggers"].as_object())
                .map(|t| t.keys().cloned().collect::<Vec<_>>())
                .unwrap_or_default();
            Some(WorkflowInfo {
                name,
                state,
                trigger_names,
            })
        })
        .collect();

    Ok(workflows)
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RunInfo {
    pub id: String,
    pub status: String,
    pub start: String,
    pub end: Option<String>,
}

/// List recent runs for a workflow (blocking)
pub fn list_runs(
    sub: &str,
    rg: &str,
    app: &str,
    workflow: &str,
    top: u32,
) -> Result<Vec<RunInfo>, String> {
    let url = format!(
        "https://management.azure.com/subscriptions/{sub}/resourceGroups/{rg}/providers/Microsoft.Web/sites/{app}/hostruntime/runtime/webhooks/workflow/api/management/workflows/{workflow}/runs?api-version=2024-04-01&$top={top}"
    );

    let output = az_command(&["rest", "--method", "GET", "--url", &url, "--output", "json"])
        .output()
        .map_err(|e| format!("az rest failed: {e}"))?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }

    let body = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&body).map_err(|e| format!("parse: {e}"))?;

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
pub fn list_actions(
    sub: &str,
    rg: &str,
    app: &str,
    workflow: &str,
    run_id: &str,
) -> Result<Vec<ActionInfo>, String> {
    let url = format!(
        "https://management.azure.com/subscriptions/{sub}/resourceGroups/{rg}/providers/Microsoft.Web/sites/{app}/hostruntime/runtime/webhooks/workflow/api/management/workflows/{workflow}/runs/{run_id}/actions?api-version=2024-04-01"
    );

    let output = az_command(&["rest", "--method", "GET", "--url", &url, "--output", "json"])
        .output()
        .map_err(|e| format!("az rest failed: {e}"))?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }

    let body = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&body).map_err(|e| format!("parse: {e}"))?;

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
pub fn list_triggers(
    sub: &str,
    rg: &str,
    app: &str,
    workflow: &str,
) -> Result<Vec<String>, String> {
    let url = format!(
        "https://management.azure.com/subscriptions/{sub}/resourceGroups/{rg}/providers/Microsoft.Web/sites/{app}/hostruntime/runtime/webhooks/workflow/api/management/workflows/{workflow}/triggers?api-version=2024-04-01"
    );

    let output = az_command(&["rest", "--method", "GET", "--url", &url, "--output", "json"])
        .output()
        .map_err(|e| format!("az rest failed: {e}"))?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }

    let body = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&body).map_err(|e| format!("parse: {e}"))?;

    let arr = if json.is_array() {
        json.as_array()
    } else {
        json["value"].as_array()
    };

    Ok(arr
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|v| v["name"].as_str().map(String::from))
        .collect())
}

/// Get the callback URL for a workflow trigger (blocking).
/// Tries each trigger name until one returns a callback URL.
pub fn get_trigger_url(sub: &str, rg: &str, app: &str, workflow: &str) -> Result<String, String> {
    let triggers = list_triggers(sub, rg, app, workflow).unwrap_or_else(|_| vec!["manual".into()]);

    let trigger_names = if triggers.is_empty() {
        vec!["manual".into()]
    } else {
        triggers
    };

    for trigger_name in &trigger_names {
        let url = format!(
            "https://management.azure.com/subscriptions/{sub}/resourceGroups/{rg}/providers/Microsoft.Web/sites/{app}/hostruntime/runtime/webhooks/workflow/api/management/workflows/{workflow}/triggers/{trigger_name}/listCallbackUrl?api-version=2024-04-01"
        );

        let output = az_command(&[
            "rest", "--method", "POST", "--url", &url, "--output", "json",
        ])
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
            "-s",
            "-w",
            "\n%{http_code}",
            "-X",
            "POST",
            "-H",
            "Content-Type: application/json",
            "-d",
            payload,
            callback_url,
        ])
        .output()
        .map_err(|e| format!("curl failed: {e}"))?;

    let full = String::from_utf8_lossy(&output.stdout).to_string();
    let lines: Vec<&str> = full.trim().rsplitn(2, '\n').collect();
    let status_code = lines[0].parse::<u16>().unwrap_or(0);
    let response_body = if lines.len() > 1 {
        lines[1].to_string()
    } else {
        String::new()
    };

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
pub fn get_app_settings(
    sub: &str,
    rg: &str,
    app: &str,
) -> Result<std::collections::HashMap<String, String>, String> {
    let output = az_command(&[
        "webapp",
        "config",
        "appsettings",
        "list",
        "--subscription",
        sub,
        "--resource-group",
        rg,
        "--name",
        app,
        "--output",
        "json",
    ])
    .output()
    .map_err(|e| format!("az appsettings failed: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }
    let body = String::from_utf8_lossy(&output.stdout);
    let arr: Vec<serde_json::Value> = serde_json::from_str(&body).unwrap_or_default();
    Ok(arr
        .iter()
        .filter_map(|v| {
            let k = v["name"].as_str()?.to_string();
            let val = v["value"].as_str().unwrap_or("").to_string();
            Some((k, val))
        })
        .collect())
}

/// Fetch every key-value pair from an Azure App Configuration store
/// (blocking). Used as the source of truth for the app-settings drift view —
/// this is the *expected* side of the comparison, always read live from
/// Azure (never a local file), per the "pure remote app" design.
///
/// `--auth-mode login` uses the signed-in principal's RBAC (App Configuration
/// Data Reader) rather than requiring a connection string.
pub fn appconfig_list_kv(sub: &str, store_name: &str) -> Result<HashMap<String, String>, String> {
    let output = az_command(&[
        "appconfig",
        "kv",
        "list",
        "--subscription",
        sub,
        "--name",
        store_name,
        "--auth-mode",
        "login",
        "--query",
        "[].{key:key,value:value}",
        "-o",
        "json",
    ])
    .output()
    .map_err(|e| format!("az appconfig kv list: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    let body = String::from_utf8_lossy(&output.stdout);
    let arr: Vec<serde_json::Value> =
        serde_json::from_str(&body).map_err(|e| format!("parse: {e}"))?;
    Ok(arr
        .iter()
        .filter_map(|v| {
            let k = v["key"].as_str()?.to_string();
            let val = v["value"].as_str().unwrap_or("").to_string();
            Some((k, val))
        })
        .collect())
}

/// Resolve a Key Vault secret's current value (blocking). Used to verify a
/// `@Microsoft.KeyVault(...)` app-setting reference actually resolves,
/// rather than just checking the reference syntax is well-formed.
pub fn keyvault_resolve_secret(vault_name: &str, secret_name: &str) -> Result<String, String> {
    let output = az_command(&[
        "keyvault",
        "secret",
        "show",
        "--vault-name",
        vault_name,
        "--name",
        secret_name,
        "--query",
        "value",
        "-o",
        "tsv",
    ])
    .output()
    .map_err(|e| format!("az keyvault secret show: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Drift status of one app setting compared against its expected App
/// Configuration value.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum DriftStatus {
    /// Live value matches the expected value exactly.
    Match,
    /// Live value differs from the expected value.
    Diff,
    /// Live value is a Key Vault reference and resolves to a non-empty secret.
    KvOk,
    /// Live value is a Key Vault reference but resolution failed (no
    /// permission, bad vault/secret name, or empty value).
    KvFail { error: String },
    /// Live value looks like a partial connection string — has some
    /// `Key=Value;` segments but is missing one that's normally required
    /// alongside the ones present (e.g. `AccountEndpoint=` without
    /// `AccountKey=`).
    LiteralWarn { missing: String },
    /// The setting exists live but has no corresponding key in App
    /// Configuration, so there's nothing to compare against.
    NoExpected,
    /// The setting is expected (present in App Configuration) but missing
    /// from the live app settings entirely.
    MissingLive,
}

/// One row of the app-settings drift table.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AppSettingDrift {
    pub key: String,
    pub live_value: String,
    pub expected_value: Option<String>,
    pub status: DriftStatus,
}

/// Connection-string-like values are made of `Key=Value;` segments. Certain
/// key prefixes imply a required companion key — if we see one without the
/// other, the value is very likely a partial literal that silently
/// overrides an intended Key Vault reference (the STG Cosmos incident this
/// view exists to catch).
const REQUIRED_COMPANION_KEYS: &[(&str, &str)] = &[
    ("AccountEndpoint", "AccountKey"),
    ("AccountKey", "AccountEndpoint"),
    ("Endpoint", "SharedAccessKey"),
    ("SharedAccessKey", "Endpoint"),
    ("DefaultEndpointsProtocol", "AccountKey"),
];

/// Detect a partial connection-string literal: parses `Key=Value;` segments
/// and checks that every key with a known required companion also has that
/// companion present. Returns the name of the first missing companion key,
/// if any.
fn detect_partial_connection_string(value: &str) -> Option<String> {
    if !value.contains('=') || !value.contains(';') {
        return None;
    }
    let present: Vec<&str> = value
        .split(';')
        .filter_map(|seg| seg.split_once('=').map(|(k, _)| k.trim()))
        .collect();
    for (key, companion) in REQUIRED_COMPANION_KEYS {
        if present.contains(key) && !present.contains(companion) {
            return Some((*companion).to_string());
        }
    }
    None
}

/// Whether an app-setting value is a Key Vault reference
/// (`@Microsoft.KeyVault(SecretUri=...)` or `(VaultName=...;SecretName=...)`).
fn is_kv_reference(value: &str) -> bool {
    value.trim_start().starts_with("@Microsoft.KeyVault(")
}

/// Extract `(vault_name, secret_name)` from a Key Vault reference value.
/// Supports both the `SecretUri=` form and the `VaultName=;SecretName=` form.
fn parse_kv_reference(value: &str) -> Option<(String, String)> {
    let inner = value
        .trim_start()
        .strip_prefix("@Microsoft.KeyVault(")?
        .strip_suffix(')')?;
    let mut vault_name = None;
    let mut secret_name = None;
    let mut secret_uri = None;
    for part in inner.split(';') {
        if let Some(v) = part.strip_prefix("VaultName=") {
            vault_name = Some(v.to_string());
        }
        if let Some(v) = part.strip_prefix("SecretName=") {
            secret_name = Some(v.to_string());
        }
        if let Some(v) = part.strip_prefix("SecretUri=") {
            secret_uri = Some(v.to_string());
        }
    }
    if let (Some(vn), Some(sn)) = (vault_name, secret_name) {
        return Some((vn, sn));
    }
    if let Some(uri) = secret_uri {
        // https://<vault>.vault.azure.net/secrets/<name>[/<version>]
        let trimmed = uri.trim_end_matches('/');
        let parts: Vec<&str> = trimmed.split('/').collect();
        let vault = trimmed.split("//").nth(1)?.split('.').next()?.to_string();
        let name = parts
            .get(parts.len().saturating_sub(1))
            .copied()
            .unwrap_or("")
            .to_string();
        if !vault.is_empty() && !name.is_empty() {
            return Some((vault, name));
        }
    }
    None
}

/// Compare live app settings against expected App Configuration values and
/// classify each row's drift status (blocking — resolves Key Vault
/// references as needed).
pub fn compute_app_settings_drift(
    live: &HashMap<String, String>,
    expected: Option<&HashMap<String, String>>,
) -> Vec<AppSettingDrift> {
    let mut rows: Vec<AppSettingDrift> = live
        .iter()
        .map(|(key, live_value)| {
            let expected_value = expected.and_then(|e| e.get(key)).cloned();

            let status = if is_kv_reference(live_value) {
                match parse_kv_reference(live_value) {
                    Some((vault, secret)) => match keyvault_resolve_secret(&vault, &secret) {
                        Ok(v) if !v.is_empty() => DriftStatus::KvOk,
                        Ok(_) => DriftStatus::KvFail {
                            error: "secret resolved to an empty value".to_string(),
                        },
                        Err(e) => DriftStatus::KvFail { error: e },
                    },
                    None => DriftStatus::KvFail {
                        error: "malformed Key Vault reference".to_string(),
                    },
                }
            } else if let Some(missing) = detect_partial_connection_string(live_value) {
                DriftStatus::LiteralWarn { missing }
            } else {
                match &expected_value {
                    None => DriftStatus::NoExpected,
                    Some(exp) if exp == live_value => DriftStatus::Match,
                    Some(_) => DriftStatus::Diff,
                }
            };

            AppSettingDrift {
                key: key.clone(),
                live_value: live_value.clone(),
                expected_value,
                status,
            }
        })
        .collect();

    if let Some(expected) = expected {
        for (key, exp_value) in expected {
            if !live.contains_key(key) {
                rows.push(AppSettingDrift {
                    key: key.clone(),
                    live_value: String::new(),
                    expected_value: Some(exp_value.clone()),
                    status: DriftStatus::MissingLive,
                });
            }
        }
    }

    rows.sort_by(|a, b| a.key.cmp(&b.key));
    rows
}

/// Set a single app setting on a Function App / Logic App (blocking). Used
/// by the drift view's "Reset to App Configuration value" action — merges
/// the one key in without touching any other setting.
pub fn set_app_setting(
    sub: &str,
    rg: &str,
    app: &str,
    key: &str,
    value: &str,
) -> Result<(), String> {
    let setting = format!("{key}={value}");
    let output = az_command(&[
        "webapp",
        "config",
        "appsettings",
        "set",
        "--subscription",
        sub,
        "--resource-group",
        rg,
        "--name",
        app,
        "--settings",
        &setting,
    ])
    .output()
    .map_err(|e| format!("az appsettings set: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(())
}

/// Start a stopped Function App (blocking).
pub fn functionapp_start(sub: &str, rg: &str, app: &str) -> Result<(), String> {
    functionapp_lifecycle(&["functionapp", "start"], sub, rg, app)
}

/// Stop a running Function App (blocking).
pub fn functionapp_stop(sub: &str, rg: &str, app: &str) -> Result<(), String> {
    functionapp_lifecycle(&["functionapp", "stop"], sub, rg, app)
}

/// Restart a Function App (blocking).
pub fn functionapp_restart(sub: &str, rg: &str, app: &str) -> Result<(), String> {
    functionapp_lifecycle(&["functionapp", "restart"], sub, rg, app)
}

/// Force the Functions host to re-read trigger bindings without a full
/// restart — useful after a deployment or a binding-affecting app-setting
/// change (blocking).
pub fn functionapp_sync_triggers(sub: &str, rg: &str, app: &str) -> Result<(), String> {
    functionapp_lifecycle(&["functionapp", "sync-function-triggers"], sub, rg, app)
}

fn functionapp_lifecycle(verb: &[&str], sub: &str, rg: &str, app: &str) -> Result<(), String> {
    let mut args: Vec<&str> = verb.to_vec();
    args.extend_from_slice(&["--subscription", sub, "--resource-group", rg, "--name", app]);
    let output = az_command(&args)
        .output()
        .map_err(|e| format!("{}: {e}", verb.join(" ")))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(())
}

/// Fetch the full workflow definition (blocking).
///
/// Uses the ARM `Microsoft.Web/sites/workflows` resource endpoint which returns
/// `{ "properties": { "files": { "workflow.json": { "definition": {...}, "kind": "..." } } } }`.
///
/// NOTE: the hostruntime management endpoint returns only METADATA (trigger names
/// but no parameters), so we use the ARM resource endpoint instead.
pub fn get_workflow_definition(
    sub: &str,
    rg: &str,
    app: &str,
    workflow: &str,
) -> Result<serde_json::Value, String> {
    // ARM resource endpoint — returns the files including workflow.json content
    let uri = format!(
        "https://management.azure.com/subscriptions/{sub}/resourceGroups/{rg}\
         /providers/Microsoft.Web/sites/{app}/workflows/{workflow}?api-version=2023-12-01"
    );

    let output = az_command(&["rest", "--method", "GET", "--uri", &uri, "--output", "json"])
        .output()
        .map_err(|e| format!("az rest failed: {e}"))?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }

    let body = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&body).map_err(|e| format!("parse: {e}"))?;

    // Extract the workflow.json content from properties.files["workflow.json"]
    // That value IS the workflow.json file (has "definition" key at root)
    if let Some(wf) = json
        .get("properties")
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
    let output = az_command(&[
        "servicebus",
        "queue",
        "show",
        "--namespace-name",
        sb_namespace,
        "--resource-group",
        rg,
        "--name",
        queue_name,
        "--output",
        "json",
    ])
    .output()
    .map_err(|e| format!("az sb failed: {e}"))?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }

    let body = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&body).map_err(|e| format!("parse: {e}"))?;

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
    let output = az_command(&[
        "servicebus",
        "namespace",
        "authorization-rule",
        "keys",
        "list",
        "--resource-group",
        rg,
        "--namespace-name",
        namespace,
        "--name",
        "RootManageSharedAccessKey",
        "--query",
        "primaryConnectionString",
        "-o",
        "tsv",
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
        return Err(format!(
            "Invalid connection string (endpoint={}, key_name={}, key_len={})",
            endpoint.is_empty(),
            key_name,
            key.len()
        ));
    }

    eprintln!(
        "[SB Send] endpoint='{}' key_name='{}' key_len={}",
        endpoint,
        key_name,
        key.len()
    );

    let url = format!("https://{}/{}/messages", endpoint, queue);

    // Generate SAS token (valid 5 minutes)
    // Azure SB SAS spec:
    //   StringToSign = URL_ENCODE(lowercase(resource_uri)) + "\n" + expiry
    //   resource_uri = "https://<fqdn>/<queue>"
    let expiry = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 300;

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
    let mut mac = HmacSha256::new_from_slice(&decoded_key).map_err(|e| format!("hmac: {e}"))?;
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
        .header(
            "Content-Type",
            "application/atom+xml;type=entry;charset=utf-8",
        )
        .body(body.to_string())
        .send()
        .await
        .map_err(|e| format!("HTTP error: {e}"))?;

    let status = resp.status();
    let text = resp.text().await.unwrap_or_default();
    eprintln!(
        "[SB Send] response: {} {}",
        status,
        &text[..200.min(text.len())]
    );
    if status.is_success() || status.as_u16() == 201 {
        Ok(())
    } else if status.as_u16() == 401 && text.is_empty() {
        // Empty 401 = network-level rejection (SB firewall / IP not allowlisted)
        Err("401 — your IP is not in the Service Bus firewall allowlist. Connect to VPN or add your IP in the Azure portal (SB namespace → Networking).".into())
    } else {
        Err(format!("SB returned {}: {}", status, text))
    }
}

/// One peeked dead-letter message.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct DeadLetterMessage {
    /// Message body — typically the JSON envelope. We truncate at fetch time
    /// to 8 KB; longer payloads are best inspected via Service Bus Explorer.
    pub body: String,
    pub message_id: String,
    pub enqueued_time: String,
    /// `DeadLetterReason` from the BrokerProperties — e.g. "MessageProcessingFailed",
    /// "MaxDeliveryCountExceeded", "MessageLockTokenInvalid".
    pub dead_letter_reason: String,
    /// `DeadLetterErrorDescription` — typically the exception message from the
    /// consumer that abandoned the message.
    pub dead_letter_description: String,
    /// How many times the message was attempted before being dead-lettered.
    pub delivery_count: i64,
}

/// Peek (non-destructively) up to `max` dead-letter messages from a queue's
/// `$DeadLetterQueue` sub-queue. Each call to the SB REST `POST /head` endpoint
/// returns a single locked message which we don't acknowledge — the lock auto-
/// releases after the queue's default lock duration (typically 60s) so this is
/// safe to call repeatedly for browsing.
pub async fn sb_peek_dead_letters(
    conn_str: &str,
    queue: &str,
    max: usize,
) -> Result<Vec<DeadLetterMessage>, String> {
    // Parse connection string (same logic as sb_send_message)
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
        return Err("Invalid Service Bus connection string".into());
    }

    let resource_path = format!("{}/$DeadLetterQueue", queue);
    let url = format!(
        "https://{}/{}/messages/head?timeout=5",
        endpoint, resource_path
    );

    // SAS token covering the whole DLQ path (same shape as the send path).
    let expiry = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 300;
    let resource_uri = format!("https://{}/{}", endpoint, resource_path).to_lowercase();
    let encoded_resource = lowercase_url_encode(&resource_uri);
    let to_sign = format!("{}\n{}", encoded_resource, expiry);

    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    type HmacSha256 = Hmac<Sha256>;
    let decoded_key = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, key)
        .map_err(|e| format!("base64 decode: {e}"))?;
    let mut mac = HmacSha256::new_from_slice(&decoded_key).map_err(|e| format!("hmac: {e}"))?;
    mac.update(to_sign.as_bytes());
    let sig_bytes = mac.finalize().into_bytes();
    let signature = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, sig_bytes);
    let encoded_sig = lowercase_url_encode(&signature);
    let token = format!(
        "SharedAccessSignature sr={}&sig={}&se={}&skn={}",
        encoded_resource, encoded_sig, expiry, key_name
    );

    let client = reqwest::Client::new();
    let mut out = Vec::with_capacity(max);

    // We peek up to `max` messages. Each request locks the next one in the queue
    // — Azure rotates internal cursors so repeated calls advance through the
    // visible messages. (Locked messages are skipped on subsequent peek-locks.)
    for _ in 0..max {
        let resp = client
            .post(&url)
            .header("Authorization", &token)
            // No Content-Type since this is an empty POST. Explicit
            // Content-Length: 0 — reqwest otherwise sends Transfer-Encoding:
            // chunked for an empty body, and the Service Bus gateway rejects
            // that with HTTP 411 "Length Required".
            .header("Content-Length", "0")
            .body(Vec::<u8>::new())
            .send()
            .await
            .map_err(|e| format!("HTTP error: {e}"))?;

        let status = resp.status();
        // 204 No Content = empty (no more messages currently visible).
        if status.as_u16() == 204 {
            break;
        }

        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            if status.as_u16() == 401 && text.is_empty() {
                return Err("401 — your IP is not in the Service Bus firewall allowlist. Connect to VPN or add your IP in the Azure portal (SB namespace → Networking).".into());
            }
            return Err(format!("SB returned {}: {}", status, text));
        }

        // BrokerProperties is sent back as a JSON header — that's where the
        // dead-letter reason and delivery count live.
        let broker_props = resp
            .headers()
            .get("BrokerProperties")
            .and_then(|h| h.to_str().ok())
            .map(|s| s.to_string())
            .unwrap_or_default();
        // DeadLetterReason / DeadLetterErrorDescription are custom properties
        // promoted to top-level headers when present.
        let dl_reason = resp
            .headers()
            .get("DeadLetterReason")
            .and_then(|h| h.to_str().ok())
            .unwrap_or("")
            .to_string();
        let dl_desc = resp
            .headers()
            .get("DeadLetterErrorDescription")
            .and_then(|h| h.to_str().ok())
            .unwrap_or("")
            .to_string();

        let body_bytes = resp.bytes().await.unwrap_or_default();
        // Truncate at 8 KB to keep the UI snappy and the memory bounded.
        let body = String::from_utf8_lossy(&body_bytes[..body_bytes.len().min(8192)]).to_string();

        let bp: serde_json::Value =
            serde_json::from_str(&broker_props).unwrap_or(serde_json::json!({}));
        out.push(DeadLetterMessage {
            body,
            message_id: bp
                .get("MessageId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            enqueued_time: bp
                .get("EnqueuedTimeUtc")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            dead_letter_reason: dl_reason,
            dead_letter_description: dl_desc,
            delivery_count: bp
                .get("DeliveryCount")
                .and_then(|v| v.as_i64())
                .unwrap_or(0),
        });
    }

    Ok(out)
}

/// Parse `endpoint`, `key_name`, `key` out of a Service Bus connection
/// string. Shared by every SB REST call below (send, peek, purge, requeue).
fn sb_parse_conn_str(conn_str: &str) -> Result<(String, String, String), String> {
    let mut endpoint = String::new();
    let mut key_name = String::new();
    let mut key = String::new();
    for part in conn_str.split(';') {
        let part = part.trim();
        if let Some(v) = part.strip_prefix("Endpoint=sb://") {
            endpoint = v.trim_end_matches('/').to_string();
        } else if let Some(v) = part.strip_prefix("SharedAccessKeyName=") {
            key_name = v.to_string();
        } else if let Some(v) = part.strip_prefix("SharedAccessKey=") {
            key = v.to_string();
        }
    }
    if endpoint.is_empty() || key.is_empty() {
        return Err("Invalid Service Bus connection string".into());
    }
    Ok((endpoint, key_name, key))
}

/// Build a 5-minute SAS token authorizing `resource_path` under `endpoint`.
/// Same signing scheme used by `sb_send_message` / `sb_peek_dead_letters`.
fn sb_sas_token(
    endpoint: &str,
    key_name: &str,
    key: &str,
    resource_path: &str,
) -> Result<String, String> {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    type HmacSha256 = Hmac<Sha256>;

    let expiry = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 300;
    let resource_uri = format!("https://{}/{}", endpoint, resource_path).to_lowercase();
    let encoded_resource = lowercase_url_encode(&resource_uri);
    let to_sign = format!("{}\n{}", encoded_resource, expiry);

    let decoded_key = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, key)
        .map_err(|e| format!("base64 decode: {e}"))?;
    let mut mac = HmacSha256::new_from_slice(&decoded_key).map_err(|e| format!("hmac: {e}"))?;
    mac.update(to_sign.as_bytes());
    let sig_bytes = mac.finalize().into_bytes();
    let signature = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, sig_bytes);
    let encoded_sig = lowercase_url_encode(&signature);

    Ok(format!(
        "SharedAccessSignature sr={}&sig={}&se={}&skn={}",
        encoded_resource, encoded_sig, expiry, key_name
    ))
}

/// Destructively drain up to `max` messages from a queue (or, via
/// `resource_path` pointing at `{queue}/$DeadLetterQueue`, from its
/// dead-letter sub-queue). Uses the SB REST API's "receive and delete" mode
/// (`DELETE .../messages/head`), which removes each message permanently —
/// unlike `sb_peek_dead_letters`'s non-destructive peek-lock.
async fn sb_receive_and_delete(
    conn_str: &str,
    resource_path: &str,
    max: usize,
) -> Result<usize, String> {
    let (endpoint, key_name, key) = sb_parse_conn_str(conn_str)?;
    let token = sb_sas_token(&endpoint, &key_name, &key, resource_path)?;
    let url = format!(
        "https://{}/{}/messages/head?timeout=5",
        endpoint, resource_path
    );
    let client = reqwest::Client::new();
    let mut deleted = 0usize;
    for _ in 0..max {
        let resp = client
            .delete(&url)
            .header("Authorization", &token)
            .send()
            .await
            .map_err(|e| format!("HTTP error: {e}"))?;
        let status = resp.status();
        if status.as_u16() == 204 {
            break;
        }
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            if status.as_u16() == 401 && text.is_empty() {
                return Err("401 — your IP is not in the Service Bus firewall allowlist.".into());
            }
            return Err(format!("SB returned {}: {}", status, text));
        }
        deleted += 1;
    }
    Ok(deleted)
}

/// Purge (permanently delete) up to `max` active messages from a queue.
pub async fn sb_purge_queue(conn_str: &str, queue: &str, max: usize) -> Result<usize, String> {
    sb_receive_and_delete(conn_str, queue, max).await
}

/// Send-and-receive round-trip probe: pushes a small test message onto
/// `queue`, then immediately receive-and-deletes one message back off it,
/// returning the round-trip latency in milliseconds. Verifies both send and
/// receive permissions plus actual network reachability to the namespace —
/// the closest thing to a live connectivity check this app can do without a
/// deployed in-Azure probe function. Only safe to point at a queue that's
/// either empty or dedicated to probing — on a busy queue, the "received"
/// message may be someone else's, not the one just sent.
pub async fn sb_probe_roundtrip(conn_str: &str, queue: &str) -> Result<u128, String> {
    let started = std::time::Instant::now();
    let probe_id = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let body = format!("{{\"probe\":true,\"id\":{probe_id}}}");
    sb_send_message(conn_str, queue, &body).await?;
    let received = sb_receive_and_delete(conn_str, queue, 1).await?;
    if received == 0 {
        return Err("Sent a probe message but didn't receive anything back — check receive permissions and that the queue isn't being drained by another consumer.".into());
    }
    Ok(started.elapsed().as_millis())
}

/// Purge (permanently delete) up to `max` messages from a queue's
/// dead-letter sub-queue.
pub async fn sb_purge_dead_letters(
    conn_str: &str,
    queue: &str,
    max: usize,
) -> Result<usize, String> {
    let dlq_path = format!("{}/$DeadLetterQueue", queue);
    sb_receive_and_delete(conn_str, &dlq_path, max).await
}

/// Move up to `max` dead-lettered messages back onto the main queue:
/// receive-and-delete each one from `$DeadLetterQueue`, then resubmit its
/// body to the main queue. If resubmission fails partway through, the
/// already-removed message is dropped — the failure is surfaced immediately
/// so remaining messages are left untouched rather than also drained.
pub async fn sb_requeue_dead_letters(
    conn_str: &str,
    queue: &str,
    max: usize,
) -> Result<usize, String> {
    let (endpoint, key_name, key) = sb_parse_conn_str(conn_str)?;
    let dlq_path = format!("{}/$DeadLetterQueue", queue);
    let dlq_token = sb_sas_token(&endpoint, &key_name, &key, &dlq_path)?;
    let dlq_url = format!("https://{}/{}/messages/head?timeout=5", endpoint, dlq_path);
    let send_token = sb_sas_token(&endpoint, &key_name, &key, queue)?;
    let send_url = format!("https://{}/{}/messages", endpoint, queue);

    let client = reqwest::Client::new();
    let mut requeued = 0usize;
    for _ in 0..max {
        let resp = client
            .delete(&dlq_url)
            .header("Authorization", &dlq_token)
            .send()
            .await
            .map_err(|e| format!("HTTP error: {e}"))?;
        let status = resp.status();
        if status.as_u16() == 204 {
            break;
        }
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(format!("SB returned {}: {}", status, text));
        }
        let body = resp.bytes().await.unwrap_or_default();

        let send_resp = client
            .post(&send_url)
            .header("Authorization", &send_token)
            .header(
                "Content-Type",
                "application/atom+xml;type=entry;charset=utf-8",
            )
            .body(body.to_vec())
            .send()
            .await
            .map_err(|e| format!("HTTP error resubmitting message {}: {e}", requeued + 1))?;
        let send_status = send_resp.status();
        if !send_status.is_success() {
            let text = send_resp.text().await.unwrap_or_default();
            return Err(format!(
                "Message removed from dead-letter queue but resubmission failed ({} {}) after requeuing {} message(s) — it is now lost. Fix the underlying issue before retrying.",
                send_status, text, requeued
            ));
        }
        requeued += 1;
    }
    Ok(requeued)
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
    let output = az_command(&[
        "eventgrid",
        "system-topic",
        "list",
        "--resource-group",
        rg,
        "--output",
        "json",
    ])
    .output()
    .map_err(|e| format!("az eventgrid failed: {e}"))?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }

    let body = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&body).map_err(|e| format!("parse: {e}"))?;

    let arr = if json.is_array() {
        json.as_array()
    } else {
        json["value"].as_array()
    };

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
pub fn list_eventgrid_system_topic_subscriptions(
    rg: &str,
    topic_name: &str,
) -> Result<Vec<EventGridSubscription>, String> {
    let output = az_command(&[
        "eventgrid",
        "system-topic",
        "event-subscription",
        "list",
        "--resource-group",
        rg,
        "--system-topic-name",
        topic_name,
        "--output",
        "json",
    ])
    .output()
    .map_err(|e| format!("az eventgrid failed: {e}"))?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }

    let body = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&body).map_err(|e| format!("parse: {e}"))?;

    let arr = if json.is_array() {
        json.as_array()
    } else {
        json["value"].as_array()
    };

    let subs = arr
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|v| {
            let name = v["name"].as_str()?.to_string();
            let dest = eg_destination(v);
            let dest_type = dest["endpointType"].as_str().unwrap_or("").to_string();
            let dest_queue = eg_field(dest, "resourceId")
                .map(eg_leaf)
                .or_else(|| eg_field(dest, "endpointUrl"))
                .unwrap_or("")
                .to_string();
            let delivery = parse_eg_delivery(v);

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
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|v| v.as_str().map(String::from))
                                    .collect()
                            })
                            .or_else(|| af["value"].as_str().map(|s| vec![s.to_string()]))
                            .unwrap_or_default();
                        filters.push(EventGridFilter {
                            key,
                            operator: op,
                            values,
                        });
                    }
                }
            }

            Some(EventGridSubscription {
                name,
                destination_type: dest_type,
                destination_queue: dest_queue,
                filters,
                dead_letter: delivery.dead_letter,
                max_delivery_attempts: delivery.max_delivery_attempts,
                event_ttl_minutes: delivery.event_ttl_minutes,
                advanced_filtering_on_arrays: delivery.advanced_filtering_on_arrays,
            })
        })
        .collect();

    Ok(subs)
}

/// A resource discovered in the resource group (any type) — the identity
/// half of a resource-health-dashboard row.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResourceInfo {
    pub name: String,
    /// ARM resource type, e.g. "Microsoft.Web/sites" or "Microsoft.DocumentDB/databaseAccounts".
    pub resource_type: String,
    pub id: String,
}

/// List every resource in a resource group (blocking) — used to populate the
/// resource health dashboard without requiring the user to name each
/// Cosmos/SQL/Storage/etc. resource individually in the profile.
pub fn list_resources(sub: &str, rg: &str) -> Result<Vec<ResourceInfo>, String> {
    let output = az_command(&[
        "resource",
        "list",
        "--subscription",
        sub,
        "--resource-group",
        rg,
        "--query",
        "[].{name:name,type:type,id:id}",
        "-o",
        "json",
    ])
    .output()
    .map_err(|e| format!("az resource list: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    let body = String::from_utf8_lossy(&output.stdout);
    let arr: Vec<serde_json::Value> =
        serde_json::from_str(&body).map_err(|e| format!("parse: {e}"))?;
    Ok(arr
        .iter()
        .map(|v| ResourceInfo {
            name: v["name"].as_str().unwrap_or("").to_string(),
            resource_type: v["type"].as_str().unwrap_or("").to_string(),
            id: v["id"].as_str().unwrap_or("").to_string(),
        })
        .collect())
}

/// Fetch a resource's lifecycle state (blocking). Different resource types
/// expose this under different property names (`state` for Web/sites,
/// `provisioningState` for most everything else) — the JMESPath `||`
/// fallback tries both rather than needing a per-type command.
pub fn get_resource_state(resource_id: &str) -> Result<String, String> {
    let output = az_command(&[
        "resource",
        "show",
        "--ids",
        resource_id,
        "--query",
        "properties.state || properties.provisioningState || 'Unknown'",
        "-o",
        "tsv",
    ])
    .output()
    .map_err(|e| format!("az resource show: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Fetch a resource's platform health status (blocking) via the
/// Microsoft.ResourceHealth provider — works uniformly across resource
/// types, unlike `state`/`provisioningState` which only reflect the last
/// control-plane operation, not actual runtime availability.
pub fn get_resource_availability(resource_id: &str) -> Result<String, String> {
    let uri = format!(
        "https://management.azure.com{resource_id}/providers/Microsoft.ResourceHealth/availabilityStatuses/current?api-version=2020-05-01"
    );
    let output = az_command(&[
        "rest",
        "--method",
        "GET",
        "--uri",
        &uri,
        "--query",
        "properties.availabilityState",
        "-o",
        "tsv",
    ])
    .output()
    .map_err(|e| format!("az rest failed: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// One row of the resource health dashboard.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ResourceHealthRow {
    pub name: String,
    pub resource_type: String,
    pub state: String,
    /// "Available" / "Degraded" / "Unavailable" / "Unknown" (Unknown means
    /// Microsoft.ResourceHealth didn't return a usable answer — not
    /// necessarily a problem, e.g. the provider may not be registered).
    pub health: String,
    pub last_checked: u64,
}

/// Discover every resource in the group and check its state + platform
/// health (blocking — does one or two `az` calls per resource, so this is
/// meant to be run on a background poll interval, not on every render).
pub fn list_resource_health(sub: &str, rg: &str) -> Result<Vec<ResourceHealthRow>, String> {
    let resources = list_resources(sub, rg)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    Ok(resources
        .into_iter()
        .map(|r| {
            let state = get_resource_state(&r.id).unwrap_or_else(|_| "Unknown".to_string());
            let health = get_resource_availability(&r.id).unwrap_or_else(|_| "Unknown".to_string());
            ResourceHealthRow {
                name: r.name,
                resource_type: r.resource_type,
                state,
                health,
                last_checked: now,
            }
        })
        .collect())
}

/// Month-to-date cost for a resource group (blocking). `az consumption
/// usage list` returns usage for the whole subscription with no
/// resource-group filter of its own, so we fetch everything for the month
/// and sum client-side — filtering case-insensitively on the resource ID,
/// since Azure resource IDs aren't consistently cased across services.
pub fn get_cost_mtd(sub: &str, rg: &str) -> Result<(f64, String), String> {
    let now = chrono::Utc::now();
    let start = now.format("%Y-%m-01").to_string();
    let end = now.format("%Y-%m-%d").to_string();
    let output = az_command(&[
        "consumption",
        "usage",
        "list",
        "--subscription",
        sub,
        "--start-date",
        &start,
        "--end-date",
        &end,
        "--query",
        "[].{id:instanceId,cost:pretaxCost,currency:currency}",
        "-o",
        "json",
    ])
    .output()
    .map_err(|e| format!("az consumption usage list: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    let body = String::from_utf8_lossy(&output.stdout);
    let arr: Vec<serde_json::Value> =
        serde_json::from_str(&body).map_err(|e| format!("parse: {e}"))?;
    let needle = format!("/resourcegroups/{}/", rg.to_lowercase());
    let mut total = 0.0f64;
    let mut currency = String::from("USD");
    for row in &arr {
        let id = row["id"].as_str().unwrap_or("").to_lowercase();
        if !id.contains(&needle) {
            continue;
        }
        let cost: f64 = row["cost"]
            .as_str()
            .and_then(|s| s.parse().ok())
            .or_else(|| row["cost"].as_f64())
            .unwrap_or(0.0);
        total += cost;
        if let Some(c) = row["currency"].as_str() {
            currency = c.to_string();
        }
    }
    Ok((total, currency))
}

/// List EventGrid topics in a resource group (blocking)
pub fn list_eventgrid_topics(rg: &str) -> Result<Vec<EventGridTopic>, String> {
    let output = az_command(&[
        "eventgrid",
        "topic",
        "list",
        "--resource-group",
        rg,
        "--output",
        "json",
    ])
    .output()
    .map_err(|e| format!("az eventgrid failed: {e}"))?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }

    let body = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&body).map_err(|e| format!("parse: {e}"))?;

    let arr = if json.is_array() {
        json.as_array()
    } else {
        json["value"].as_array()
    };

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
    /// Where undeliverable events go once retries are exhausted. `None` means
    /// Event Grid *discards* them silently — no queue, no alert, no trace.
    #[serde(default)]
    pub dead_letter: Option<String>,
    #[serde(default)]
    pub max_delivery_attempts: Option<i64>,
    #[serde(default)]
    pub event_ttl_minutes: Option<i64>,
    #[serde(default)]
    pub advanced_filtering_on_arrays: bool,
}

/// Delivery-guarantee fields shared by the custom-topic and system-topic
/// subscription parsers — factored out so both stay in sync instead of
/// duplicating the `dest`/`retryPolicy`/`deadLetterDestination` reads a third
/// time whenever a new field is needed.
struct EgDelivery {
    dead_letter: Option<String>,
    max_delivery_attempts: Option<i64>,
    event_ttl_minutes: Option<i64>,
    advanced_filtering_on_arrays: bool,
}

fn parse_eg_delivery(v: &serde_json::Value) -> EgDelivery {
    // Same two shapes as the delivery destination. Getting this wrong is worse
    // than a blank column: the panel reports "dropped", so a subscription that
    // *is* dead-lettering would still be flagged as silently discarding events.
    let dead_letter = eg_field(&v["deadLetterDestination"], "resourceId")
        .or_else(|| {
            eg_field(
                &v["deadLetterWithResourceIdentity"]["deadLetterDestination"],
                "resourceId",
            )
        })
        .map(String::from);
    let retry = &v["retryPolicy"];
    EgDelivery {
        dead_letter,
        max_delivery_attempts: retry["maxDeliveryAttempts"].as_i64(),
        event_ttl_minutes: retry["eventTimeToLiveInMinutes"].as_i64(),
        advanced_filtering_on_arrays: v["filter"]["enableAdvancedFilteringOnArrays"]
            .as_bool()
            .unwrap_or(false),
    }
}

/// A subscription delivering under a managed identity carries a null
/// `destination` and puts the real target under
/// `deliveryWithResourceIdentity.destination` instead — reading only the
/// former left those rows with an empty destination queue.
fn eg_destination(v: &serde_json::Value) -> &serde_json::Value {
    if v["destination"].is_object() {
        &v["destination"]
    } else {
        &v["deliveryWithResourceIdentity"]["destination"]
    }
}

/// A field on an Event Grid destination, whichever shape it arrives in.
///
/// The ARM REST API wraps these in a `properties` envelope; the `az` CLI
/// flattens it away and puts them directly on the destination. We read `az`,
/// so every `["properties"]["resourceId"]` here was reading `null` — which is
/// why no subscription in any environment showed the queue it delivers to.
/// Both shapes are accepted rather than just swapping one for the other, so a
/// switch to the REST API, or an `az` version that stops flattening, does not
/// silently blank the column again.
fn eg_field<'a>(dest: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    dest["properties"][key]
        .as_str()
        .or_else(|| dest[key].as_str())
        .filter(|s| !s.is_empty())
}

/// The last segment of an ARM resource id — the queue, topic or container
/// name, which is the part worth showing.
fn eg_leaf(resource_id: &str) -> &str {
    resource_id.rsplit('/').next().unwrap_or(resource_id)
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
                        map.insert(
                            s.destination_queue.clone(),
                            EgLink {
                                topic_name: t.name.clone(),
                                subscription_name: s.name.clone(),
                                filters: s.filters.clone(),
                            },
                        );
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
                        map.insert(
                            s.destination_queue.clone(),
                            EgLink {
                                topic_name: st.name.clone(),
                                subscription_name: s.name.clone(),
                                filters: s.filters.clone(),
                            },
                        );
                    }
                }
            }
        }
    }

    map
}

/// List EventGrid subscriptions for a topic (blocking)
pub fn list_eventgrid_subscriptions(topic_id: &str) -> Result<Vec<EventGridSubscription>, String> {
    let output = az_command(&[
        "eventgrid",
        "event-subscription",
        "list",
        "--source-resource-id",
        topic_id,
        "--output",
        "json",
    ])
    .output()
    .map_err(|e| format!("az eventgrid failed: {e}"))?;

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).to_string());
    }

    let body = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&body).map_err(|e| format!("parse: {e}"))?;

    let arr = if json.is_array() {
        json.as_array()
    } else {
        json["value"].as_array()
    };

    let subs = arr
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|v| {
            let name = v["name"].as_str()?.to_string();
            let dest = eg_destination(v);
            let dest_type = dest["endpointType"].as_str().unwrap_or("").to_string();
            // Extract queue name from resourceId — last segment
            // The `endpointUrl` fallback is what a WebHook destination has
            // instead of a resource id; the system-topic parser already had it
            // and this one did not, so webhook rows here showed nothing even
            // once the resource-id path was right.
            let dest_queue = eg_field(dest, "resourceId")
                .map(eg_leaf)
                .or_else(|| eg_field(dest, "endpointUrl"))
                .unwrap_or("")
                .to_string();
            let delivery = parse_eg_delivery(v);

            // Parse advanced filters
            let mut filters = Vec::new();
            if let Some(filter) = v["filter"].as_object() {
                if let Some(adv) = filter.get("advancedFilters").and_then(|f| f.as_array()) {
                    for af in adv {
                        let key = af["key"].as_str().unwrap_or("").to_string();
                        let op = af["operatorType"].as_str().unwrap_or("").to_string();
                        let values: Vec<String> = af["values"]
                            .as_array()
                            .map(|arr| {
                                arr.iter()
                                    .filter_map(|v| v.as_str().map(String::from))
                                    .collect()
                            })
                            .or_else(|| af["value"].as_str().map(|s| vec![s.to_string()]))
                            .unwrap_or_default();
                        filters.push(EventGridFilter {
                            key,
                            operator: op,
                            values,
                        });
                    }
                }
            }

            Some(EventGridSubscription {
                name,
                destination_type: dest_type,
                destination_queue: dest_queue,
                filters,
                dead_letter: delivery.dead_letter,
                max_delivery_attempts: delivery.max_delivery_attempts,
                event_ttl_minutes: delivery.event_ttl_minutes,
                advanced_filtering_on_arrays: delivery.advanced_filtering_on_arrays,
            })
        })
        .collect();

    Ok(subs)
}

// ── Function Apps ────────────────────────────────────────────────────────────

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FunctionApp {
    pub name: String,
    pub state: String,
    pub resource_group: String,
    /// System-assigned managed identity object/principal ID — empty if MSI not enabled.
    /// Useful for granting Azure RBAC roles (Cosmos, Key Vault, SQL) to the function app.
    #[serde(default)]
    pub principal_id: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FunctionDetail {
    pub name: String,
    pub language: String,
    pub is_disabled: bool,
}

#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
pub struct FunctionMetrics {
    pub function_name: String,
    pub success: i64,
    pub errors: i64,
    pub last_run: String,
}

/// List all Function Apps in a resource group.
pub fn list_function_apps(sub: &str, rg: &str) -> Result<Vec<FunctionApp>, String> {
    let output = az_command(&[
        "functionapp",
        "list",
        "--subscription",
        sub,
        "--resource-group",
        rg,
        "--query",
        "[].{name:name,state:state,principalId:identity.principalId}",
        "-o",
        "json",
    ])
    .output()
    .map_err(|e| format!("az functionapp list: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    let body = String::from_utf8_lossy(&output.stdout);
    let arr: Vec<serde_json::Value> =
        serde_json::from_str(&body).map_err(|e| format!("parse: {e}"))?;
    Ok(arr
        .iter()
        .map(|v| FunctionApp {
            name: v["name"].as_str().unwrap_or("").to_string(),
            state: v["state"].as_str().unwrap_or("Unknown").to_string(),
            resource_group: rg.to_string(),
            principal_id: v["principalId"].as_str().unwrap_or("").to_string(),
        })
        .collect())
}

/// Get the system-assigned managed identity (principal ID) of a Logic App or Function App.
/// Both are App Service sites under the hood — same REST endpoint.
/// Returns Ok("") if the site has no managed identity assigned.
pub fn get_principal_id(sub: &str, rg: &str, app: &str) -> Result<String, String> {
    let output = az_command(&[
        "webapp",
        "identity",
        "show",
        "--subscription",
        sub,
        "--resource-group",
        rg,
        "--name",
        app,
        "--query",
        "principalId",
        "-o",
        "tsv",
    ])
    .output()
    .map_err(|e| format!("az webapp identity show: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// One Azure RBAC role assignment held by a principal.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RoleAssignment {
    pub role_name: String,
    pub scope: String,
}

/// List every RBAC role assignment held by a principal (blocking) — used by
/// the auth health check to flag Function Apps whose managed identity has
/// no roles at all, which is the most common cause of runtime "access
/// denied" failures against Cosmos/Key Vault/SQL.
pub fn list_role_assignments(principal_id: &str) -> Result<Vec<RoleAssignment>, String> {
    if principal_id.is_empty() {
        return Ok(Vec::new());
    }
    let output = az_command(&[
        "role",
        "assignment",
        "list",
        "--assignee",
        principal_id,
        "--all",
        "--query",
        "[].{roleName:roleDefinitionName,scope:scope}",
        "-o",
        "json",
    ])
    .output()
    .map_err(|e| format!("az role assignment list: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    let body = String::from_utf8_lossy(&output.stdout);
    let arr: Vec<serde_json::Value> =
        serde_json::from_str(&body).map_err(|e| format!("parse: {e}"))?;
    Ok(arr
        .iter()
        .map(|v| RoleAssignment {
            role_name: v["roleName"].as_str().unwrap_or("Unknown").to_string(),
            scope: v["scope"].as_str().unwrap_or("").to_string(),
        })
        .collect())
}

/// Grant an ARM RBAC role to a principal at a scope (blocking). Covers most
/// data sources a Function App talks to — Storage, Key Vault, SQL (AAD
/// auth), Service Bus data-plane roles — everything except Cosmos DB's SQL
/// data-plane RBAC, which uses a separate role system (see
/// `assign_cosmos_data_role`).
pub fn assign_role_arm(principal_id: &str, role: &str, scope: &str) -> Result<(), String> {
    let output = az_command(&[
        "role",
        "assignment",
        "create",
        "--assignee",
        principal_id,
        "--role",
        role,
        "--scope",
        scope,
    ])
    .output()
    .map_err(|e| format!("az role assignment create: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(())
}

/// Grant a Cosmos DB SQL data-plane role to a principal (blocking). Cosmos
/// data access (reading/writing items) is governed by its own RBAC system,
/// separate from the ARM roles `assign_role_arm` grants — an identity can
/// have full ARM "Contributor" on the Cosmos account and still get 403s on
/// data operations without one of these. `role_definition_id` is a GUID;
/// the two built-ins are `00000000-0000-0000-0000-000000000001` (Data
/// Reader) and `00000000-0000-0000-0000-000000000002` (Data Contributor).
/// `data_scope` is a Cosmos resource path, e.g. `/` for the whole account.
pub fn assign_cosmos_data_role(
    rg: &str,
    cosmos_account: &str,
    principal_id: &str,
    role_definition_id: &str,
    data_scope: &str,
) -> Result<(), String> {
    let output = az_command(&[
        "cosmosdb",
        "sql",
        "role",
        "assignment",
        "create",
        "--resource-group",
        rg,
        "--account-name",
        cosmos_account,
        "--principal-id",
        principal_id,
        "--role-definition-id",
        role_definition_id,
        "--scope",
        data_scope,
    ])
    .output()
    .map_err(|e| format!("az cosmosdb sql role assignment create: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(())
}

/// One variable inside an Azure DevOps variable group.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VariableGroupVar {
    pub name: String,
    /// `None` for secret variables — Azure DevOps never returns secret
    /// values through the CLI, so there's nothing to compare/drift-check.
    pub value: Option<String>,
    pub is_secret: bool,
}

/// An Azure DevOps variable group with its variables already resolved —
/// `az pipelines variable-group list` returns each group's full definition
/// (including variables) in one call, so there's no need for a second
/// per-group fetch.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VariableGroup {
    pub id: u64,
    pub name: String,
    pub variables: Vec<VariableGroupVar>,
}

/// List every variable group in an Azure DevOps project (blocking).
/// Requires the `azure-devops` CLI extension (`az extension add --name
/// azure-devops`) — surfaces that requirement in the error if missing
/// rather than failing silently.
pub fn list_variable_groups(org: &str, project: &str) -> Result<Vec<VariableGroup>, String> {
    let output = az_command(&[
        "pipelines",
        "variable-group",
        "list",
        "--organization",
        org,
        "--project",
        project,
        "-o",
        "json",
    ])
    .output()
    .map_err(|e| format!("az pipelines variable-group list: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        if stderr.contains("az extension add")
            || stderr.contains("is not a registered")
            || stderr.contains("'pipelines' is misspelled")
        {
            return Err(format!(
                "{stderr}\n\nRun: az extension add --name azure-devops"
            ));
        }
        return Err(stderr);
    }
    let body = String::from_utf8_lossy(&output.stdout);
    let arr: Vec<serde_json::Value> =
        serde_json::from_str(&body).map_err(|e| format!("parse: {e}"))?;
    Ok(arr
        .iter()
        .map(|g| {
            let variables = g["variables"]
                .as_object()
                .map(|obj| {
                    obj.iter()
                        .map(|(name, v)| VariableGroupVar {
                            name: name.clone(),
                            is_secret: v["isSecret"].as_bool().unwrap_or(false),
                            value: if v["isSecret"].as_bool().unwrap_or(false) {
                                None
                            } else {
                                v["value"].as_str().map(|s| s.to_string())
                            },
                        })
                        .collect()
                })
                .unwrap_or_default();
            VariableGroup {
                id: g["id"].as_u64().unwrap_or(0),
                name: g["name"].as_str().unwrap_or("").to_string(),
                variables,
            }
        })
        .collect())
}

/// Delete a single variable from a variable group (blocking) — used by the
/// variable-group cleanup view's bulk-delete action. Never call this on a
/// secret variable from an automated "safe to delete" pass; secrets can't
/// be drift-checked so they should never be auto-suggested for deletion.
pub fn delete_variable_group_variable(
    org: &str,
    project: &str,
    group_id: u64,
    name: &str,
) -> Result<(), String> {
    let group_id_str = group_id.to_string();
    let output = az_command(&[
        "pipelines",
        "variable-group",
        "variable",
        "delete",
        "--group-id",
        &group_id_str,
        "--name",
        name,
        "--organization",
        org,
        "--project",
        project,
        "--yes",
    ])
    .output()
    .map_err(|e| format!("az pipelines variable-group variable delete: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(())
}

/// List functions inside a Function App.
pub fn list_functions(rg: &str, app: &str) -> Result<Vec<FunctionDetail>, String> {
    let output = az_command(&[
        "functionapp",
        "function",
        "list",
        "--resource-group",
        rg,
        "--name",
        app,
        "--query",
        "[].{name:name,language:language,isDisabled:isDisabled}",
        "-o",
        "json",
    ])
    .output()
    .map_err(|e| format!("az functionapp function list: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    let body = String::from_utf8_lossy(&output.stdout);
    let arr: Vec<serde_json::Value> =
        serde_json::from_str(&body).map_err(|e| format!("parse: {e}"))?;
    Ok(arr
        .iter()
        .map(|v| {
            let full = v["name"].as_str().unwrap_or("");
            let short = full.rsplit('/').next().unwrap_or(full);
            FunctionDetail {
                name: short.to_string(),
                language: v["language"].as_str().unwrap_or("").to_string(),
                is_disabled: v["isDisabled"].as_bool().unwrap_or(false),
            }
        })
        .collect())
}

/// Discover Application Insights resource names in a resource group.
pub fn find_app_insights(rg: &str) -> Result<Vec<String>, String> {
    let output = az_command(&[
        "monitor",
        "app-insights",
        "component",
        "show",
        "--resource-group",
        rg,
        "--query",
        "[].name",
        "-o",
        "json",
    ])
    .output()
    .map_err(|e| format!("az monitor app-insights: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    let body = String::from_utf8_lossy(&output.stdout);
    let arr: Vec<String> = serde_json::from_str(&body).unwrap_or_default();
    Ok(arr)
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct FunctionError {
    pub timestamp: String,
    pub operation_name: String,
    pub result_code: String,
    pub message: String,
}

/// Query failed invocation details for a specific function from Application Insights.
pub fn query_function_errors(
    rg: &str,
    app_insights: &str,
    function_app: &str,
    function_name: &str,
    days: u32,
) -> Result<Vec<FunctionError>, String> {
    let query = format!(
        "requests \
         | where timestamp > ago({days}d) \
         | where cloud_RoleName == '{function_app}' \
         | where success == false \
         | where operation_Name == '{function_name}' \
         | project timestamp, operation_Name, resultCode, tostring(customDimensions) \
         | order by timestamp desc \
         | take 50"
    );
    let output = az_command(&[
        "monitor",
        "app-insights",
        "query",
        "--app",
        app_insights,
        "--resource-group",
        rg,
        "--analytics-query",
        &query,
        "-o",
        "json",
    ])
    .output()
    .map_err(|e| format!("az app-insights query: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    let body = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&body).map_err(|e| format!("parse: {e}"))?;
    let rows = json["tables"][0]["rows"]
        .as_array()
        .ok_or_else(|| "No rows in response".to_string())?;
    Ok(rows
        .iter()
        .filter_map(|r| {
            let arr = r.as_array()?;
            Some(FunctionError {
                timestamp: arr.first()?.as_str().unwrap_or("").to_string(),
                operation_name: arr.get(1)?.as_str().unwrap_or("").to_string(),
                result_code: arr.get(2)?.as_str().unwrap_or("").to_string(),
                message: arr.get(3)?.as_str().unwrap_or("").to_string(),
            })
        })
        .collect())
}

/// Query function invocation metrics from Application Insights.
pub fn query_function_metrics(
    rg: &str,
    app_insights: &str,
    function_app: &str,
    days: u32,
) -> Result<Vec<FunctionMetrics>, String> {
    let query = format!(
        "requests | where timestamp > ago({days}d) | where cloud_RoleName == '{function_app}' \
         | summarize success=countif(success==true), errors=countif(success==false), lastRun=max(timestamp) \
         by operation_Name | order by operation_Name asc"
    );
    let output = az_command(&[
        "monitor",
        "app-insights",
        "query",
        "--app",
        app_insights,
        "--resource-group",
        rg,
        "--analytics-query",
        &query,
        "-o",
        "json",
    ])
    .output()
    .map_err(|e| format!("az app-insights query: {e}"))?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    let body = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&body).map_err(|e| format!("parse: {e}"))?;
    let rows = json["tables"][0]["rows"]
        .as_array()
        .ok_or_else(|| "No rows in response".to_string())?;
    Ok(rows
        .iter()
        .filter_map(|r| {
            let arr = r.as_array()?;
            Some(FunctionMetrics {
                function_name: arr.first()?.as_str()?.to_string(),
                success: arr.get(1)?.as_i64().unwrap_or(0),
                errors: arr.get(2)?.as_i64().unwrap_or(0),
                last_run: arr.get(3)?.as_str().unwrap_or("").to_string(),
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No `deadLetterDestination` — Event Grid drops the event once retries
    /// run out. This shape is what every subscription in dev/stg looked like
    /// before dead-lettering was configured.
    #[test]
    fn parse_eg_delivery_flags_missing_dead_letter() {
        let v: serde_json::Value = serde_json::json!({
            "retryPolicy": { "maxDeliveryAttempts": 30, "eventTimeToLiveInMinutes": 1440 },
            "filter": { "enableAdvancedFilteringOnArrays": true }
        });
        let d = parse_eg_delivery(&v);
        assert_eq!(d.dead_letter, None);
        assert_eq!(d.max_delivery_attempts, Some(30));
        assert_eq!(d.event_ttl_minutes, Some(1440));
        assert!(d.advanced_filtering_on_arrays);
    }

    /// The shape `az eventgrid event-subscription list` actually returns —
    /// verbatim from `evgt-Tom-dev-chn-001`, trimmed. Note `resourceId` sits
    /// directly on `destination`: the CLI flattens ARM's `properties` envelope,
    /// and reading only the nested path is why every row showed a blank queue.
    #[test]
    fn eg_field_reads_the_flattened_shape_the_cli_returns() {
        let dest: serde_json::Value = serde_json::json!({
            "deliveryAttributeMappings": null,
            "endpointType": "ServiceBusQueue",
            "resourceId": "/subscriptions/x/resourceGroups/rg-tom-dev-chn-001/providers/Microsoft.ServiceBus/namespaces/sbns-tom-dev-chn-001/queues/ais.event.ignite"
        });
        assert_eq!(
            eg_field(&dest, "resourceId").map(eg_leaf),
            Some("ais.event.ignite")
        );
    }

    /// And the nested one the REST API returns, so switching transport does
    /// not blank the column again.
    #[test]
    fn eg_field_still_reads_the_nested_arm_shape() {
        let dest: serde_json::Value = serde_json::json!({
            "endpointType": "ServiceBusQueue",
            "properties": { "resourceId": "/subscriptions/x/.../queues/ais.event.ignite" }
        });
        assert_eq!(
            eg_field(&dest, "resourceId").map(eg_leaf),
            Some("ais.event.ignite")
        );
    }

    #[test]
    fn eg_field_treats_missing_and_empty_alike() {
        assert_eq!(eg_field(&serde_json::json!({}), "resourceId"), None);
        assert_eq!(eg_field(&serde_json::json!(null), "resourceId"), None);
        // An empty string would render as a destination that is simply blank,
        // which is the bug this whole change is about.
        assert_eq!(
            eg_field(&serde_json::json!({"resourceId": ""}), "resourceId"),
            None
        );
    }

    /// ais-event-jde: delivery under a system-assigned identity, so the real
    /// target hangs off `deliveryWithResourceIdentity` — and is flattened too.
    #[test]
    fn an_identity_delivery_still_names_its_queue() {
        let v: serde_json::Value = serde_json::json!({
            "destination": null,
            "deliveryWithResourceIdentity": {
                "destination": {
                    "endpointType": "ServiceBusQueue",
                    "resourceId": "/subscriptions/x/.../queues/ais.event.jde"
                },
                "identity": { "type": "SystemAssigned" }
            }
        });
        let dest = eg_destination(&v);
        assert_eq!(dest["endpointType"].as_str(), Some("ServiceBusQueue"));
        assert_eq!(
            eg_field(dest, "resourceId").map(eg_leaf),
            Some("ais.event.jde")
        );
    }

    /// A WebHook has no resource id at all; the url is the destination.
    #[test]
    fn a_webhook_destination_falls_back_to_its_url() {
        let dest: serde_json::Value = serde_json::json!({
            "endpointType": "WebHook",
            "endpointUrl": "https://example.invalid/hook"
        });
        assert_eq!(eg_field(&dest, "resourceId"), None);
        assert_eq!(
            eg_field(&dest, "endpointUrl"),
            Some("https://example.invalid/hook")
        );
    }

    /// The dead-letter path had the same nesting mistake, and there it is
    /// worse than a blank: the panel would say "dropped" for a subscription
    /// that is dead-lettering perfectly well.
    #[test]
    fn a_flattened_dead_letter_is_not_reported_as_dropped() {
        let v: serde_json::Value = serde_json::json!({
            "deadLetterDestination": {
                "endpointType": "StorageBlob",
                "resourceId": "/subscriptions/x/.../containers/eg-deadletter"
            },
            "retryPolicy": { "maxDeliveryAttempts": 30, "eventTimeToLiveInMinutes": 1440 }
        });
        assert_eq!(
            parse_eg_delivery(&v).dead_letter.as_deref(),
            Some("/subscriptions/x/.../containers/eg-deadletter")
        );
    }

    #[test]
    fn parse_eg_delivery_reads_configured_dead_letter() {
        let v: serde_json::Value = serde_json::json!({
            "deadLetterDestination": {
                "properties": { "resourceId": "/subscriptions/x/.../containers/eg-deadletter" }
            },
            "retryPolicy": { "maxDeliveryAttempts": 30, "eventTimeToLiveInMinutes": 1440 }
        });
        let d = parse_eg_delivery(&v);
        assert_eq!(
            d.dead_letter.as_deref(),
            Some("/subscriptions/x/.../containers/eg-deadletter")
        );
    }

    /// A subscription delivering under a managed identity (ais-event-jde is
    /// the one example on this platform) carries a null `destination` and
    /// puts the real target under `deliveryWithResourceIdentity` instead —
    /// reading only the former rendered those rows with a blank destination.
    #[test]
    fn eg_destination_falls_back_to_managed_identity_delivery() {
        let v: serde_json::Value = serde_json::json!({
            "destination": null,
            "deliveryWithResourceIdentity": {
                "destination": {
                    "endpointType": "ServiceBusQueue",
                    "properties": { "resourceId": "/subscriptions/x/.../queues/ais.event.jde" }
                }
            }
        });
        let dest = eg_destination(&v);
        assert_eq!(dest["endpointType"].as_str(), Some("ServiceBusQueue"));
        assert_eq!(
            dest["properties"]["resourceId"].as_str(),
            Some("/subscriptions/x/.../queues/ais.event.jde")
        );
    }

    #[test]
    fn throttling_matches_explicit_429() {
        assert!(is_throttling_error(
            r#"Too Many Requests({"Code":"429","Message":"...Endpoint is currently throttled..."})"#
        ));
    }

    #[test]
    fn throttling_matches_transport_failures() {
        // The az CLI reports connection-level refusal as a Python traceback;
        // these are the three shapes seen in practice.
        assert!(is_throttling_error(
            "('Connection aborted.', ConnectionResetError(54, 'Connection reset by peer'))"
        ));
        assert!(is_throttling_error(
            "('Connection aborted.', OSError(22, 'Invalid argument'))"
        ));
        assert!(is_throttling_error("ERROR: Too Many Requests"));
    }

    #[test]
    fn throttling_ignores_real_resource_errors() {
        // A missing workflow is a genuine problem to surface, not a reason
        // to back off — backing off here would hide it behind a slow poll.
        assert!(!is_throttling_error(
            r#"Not Found({"error":{"code":"WorkflowNotFound","message":"..."}})"#
        ));
        assert!(!is_throttling_error("AuthorizationFailed"));
        assert!(!is_throttling_error(""));
    }

    #[test]
    fn authorization_matches_rbac_denial() {
        assert!(is_authorization_error(
            r#"Forbidden({"error":{"code":"AuthorizationFailed","message":"The client 'x@y.com' does not have authorization to perform action 'Microsoft.Web/sites/hostruntime/webhooks/api/workflows/runs/read'..."}})"#
        ));
    }

    #[test]
    fn service_unavailable_matches_gateway_faults() {
        assert!(is_service_unavailable_error(
            r#"Bad Gateway({"error":{"code":"BadGatewayConnection","message":"The network connectivity issue encountered for 'Microsoft.Web'; cannot fulfill the request."}})"#
        ));
        assert!(is_service_unavailable_error("503 Service Unavailable"));
        assert!(is_service_unavailable_error("Gateway Timeout"));
    }

    #[test]
    fn service_unavailable_is_distinct_from_throttling() {
        // A gateway fault is Azure failing, not the app over-sending —
        // reporting it as throttling would blame the wrong side.
        let gw = r#"Bad Gateway({"code":"BadGatewayConnection"})"#;
        assert!(!is_throttling_error(gw));
        assert!(!is_authorization_error(gw));
    }

    #[test]
    fn authorization_ignores_unrelated_errors() {
        assert!(!is_authorization_error(
            r#"Not Found({"error":{"code":"WorkflowNotFound"}})"#
        ));
        assert!(!is_authorization_error(
            "('Connection aborted.', ConnectionResetError(54, 'Connection reset by peer'))"
        ));
        assert!(!is_authorization_error(""));
    }
}

#[cfg(test)]
mod tenant_tests {
    use super::*;

    #[test]
    fn a_profile_on_the_current_tenant_needs_no_switch() {
        assert!(!needs_tenant_switch("abc-123", Some("abc-123")));
    }

    #[test]
    fn a_profile_on_another_tenant_does() {
        assert!(needs_tenant_switch("abc-123", Some("def-456")));
    }

    #[test]
    fn tenant_ids_compare_without_regard_to_case() {
        assert!(!needs_tenant_switch("ABC-123", Some("abc-123")));
    }

    #[test]
    fn a_profile_with_no_tenant_never_switches() {
        // Most profiles. Opening a browser for these was the visible symptom.
        assert!(!needs_tenant_switch("", Some("abc-123")));
        assert!(!needs_tenant_switch("   ", Some("abc-123")));
    }

    #[test]
    fn an_unreadable_session_is_left_alone_rather_than_guessed_at() {
        assert!(!needs_tenant_switch("abc-123", None));
    }
}
