//! URL builders + opener for the Azure portal.
//!
//! Every chain step, queue, function app, etc. gets a one-click 🔗 button that
//! deep-links into the Portal so users can jump straight into the official UI
//! for the deep-dive operations ais-monitor doesn't cover (cost, app settings,
//! peek-DL messages in Service Bus Explorer, etc.).
//!
//! The links use the portal's `#@{tenant}/resource/<resource-id>` fragment so
//! they open the correct directory automatically. `tenant` falls back to
//! "default" — the portal will still resolve a logged-in session correctly,
//! it just doesn't pre-select the directory.

const PORTAL_BASE: &str = "https://portal.azure.com";

/// Returns either `#@{tenant}/` (with trailing slash) or `#` — the use sites
/// concatenate `{frag}resource/...` so a missing tenant produces a clean
/// `#resource/...` instead of the bad `#/resource/...` URL that silently
/// routes to the Portal home page.
fn tenant_fragment(tenant: &str) -> String {
    if tenant.is_empty() {
        "#".to_string()
    } else {
        format!("#@{tenant}/")
    }
}

/// Logic App Standard site (the app, not a specific workflow).
///
/// Lands on the site Overview blade; the user clicks "Workflows" from the
/// left nav to drill in. Tried `.../sites/{app}/workflows` but the trailing
/// `/workflows` segment isn't a standard resource sub-path and the Portal
/// silently dropped it.
pub fn logic_app(tenant: &str, subscription: &str, rg: &str, app: &str) -> String {
    format!(
        "{base}/{frag}resource/subscriptions/{sub}/resourceGroups/{rg}/providers/Microsoft.Web/sites/{app}",
        base = PORTAL_BASE,
        frag = tenant_fragment(tenant),
        sub = subscription,
        rg = rg,
        app = app,
    )
}

/// A specific workflow inside a Logic App Standard site.
///
/// Logic Apps Standard workflows are NOT a first-class ARM resource type, so
/// the generic `#@{tenant}/resource/<id>` form falls through to the Portal
/// landing page. They live inside the EMA blade extension and are reached
/// via a deep-link of the form:
///
///   https://portal.azure.com/#view/Microsoft_Azure_EMA/WorkflowMenuBlade/
///     ~/runHistory/resourceId/<id>/location/<region>/isReadOnly~/false/
///     kind/Stateful
///
/// The Portal validates `location` server-side and refuses to render the
/// blade without it (`ErrorInitializing: missing 'location'`). The caller
/// passes it in once we've discovered the parent site's region.
///
/// If `location` is `None` we fall back to the site Overview URL — better
/// to land somewhere navigable than to throw an error page at the user.
///
/// The tenant fragment is intentionally dropped — the `#view/...` form
/// doesn't support `#@{tenant}/`; Azure resolves the active session's tenant
/// automatically.
pub fn workflow(
    tenant: &str,
    subscription: &str,
    rg: &str,
    app: &str,
    workflow: &str,
    location: Option<&str>,
) -> String {
    let Some(loc) = location.filter(|l| !l.is_empty()) else {
        return logic_app(tenant, subscription, rg, app);
    };
    let resource_id = format!(
        "/subscriptions/{sub}/resourceGroups/{rg}/providers/Microsoft.Web/sites/{app}/workflows/{wf}",
        sub = subscription, rg = rg, app = app, wf = workflow,
    );
    let encoded_id = urlencoding::encode(&resource_id);
    let encoded_loc = urlencoding::encode(loc);
    format!(
        "{base}/#view/Microsoft_Azure_EMA/WorkflowMenuBlade/~/runHistory/resourceId/{encoded_id}/location/{encoded_loc}/isReadOnly~/false/kind/Stateful",
        base = PORTAL_BASE,
    )
}

/// A Service Bus queue blade.
///
/// Landed on `/explorer` (the Service Bus Explorer tab) instead of the
/// default Overview, because monitor users coming from a DL alert almost
/// always want to peek/repair messages — saving the extra click into
/// "Service Bus Explorer" in the left nav.
pub fn sb_queue(
    tenant: &str,
    subscription: &str,
    rg: &str,
    namespace: &str,
    queue: &str,
) -> String {
    format!(
        "{base}/{frag}resource/subscriptions/{sub}/resourceGroups/{rg}/providers/Microsoft.ServiceBus/namespaces/{ns}/queues/{q}/explorer",
        base = PORTAL_BASE,
        frag = tenant_fragment(tenant),
        sub = subscription,
        rg = rg,
        ns = namespace,
        q = queue,
    )
}

/// Service Bus namespace overview.
#[allow(dead_code)]
pub fn sb_namespace(tenant: &str, subscription: &str, rg: &str, namespace: &str) -> String {
    format!(
        "{base}/{frag}resource/subscriptions/{sub}/resourceGroups/{rg}/providers/Microsoft.ServiceBus/namespaces/{ns}",
        base = PORTAL_BASE,
        frag = tenant_fragment(tenant),
        sub = subscription,
        rg = rg,
        ns = namespace,
    )
}

/// A Function App overview.
pub fn function_app(tenant: &str, subscription: &str, rg: &str, app: &str) -> String {
    format!(
        "{base}/{frag}resource/subscriptions/{sub}/resourceGroups/{rg}/providers/Microsoft.Web/sites/{app}",
        base = PORTAL_BASE,
        frag = tenant_fragment(tenant),
        sub = subscription,
        rg = rg,
        app = app,
    )
}

/// A single Function inside a Function App. Lands on the **Invocations** tab,
/// directly equivalent to the workflow run-history view. Like `workflow()`,
/// this uses the blade-extension `#view/...` form (no tenant prefix).
pub fn function(
    _tenant: &str,
    subscription: &str,
    rg: &str,
    app: &str,
    function_name: &str,
) -> String {
    let resource_id = format!(
        "/subscriptions/{sub}/resourceGroups/{rg}/providers/Microsoft.Web/sites/{app}/functions/{fn}",
        sub = subscription, rg = rg, app = app, fn = function_name,
    );
    let encoded = urlencoding::encode(&resource_id);
    format!(
        "{base}/#view/WebsitesExtension/FunctionTabMenuBlade/~/invocations/resourceId/{encoded}",
        base = PORTAL_BASE,
    )
}

/// Open the URL in the user's default browser. Falls back to logging an
/// activity error if the OS open fails (rare but worth surfacing).
/// True for the only two schemes this app ever has cause to open.
///
/// Not every URL reaching here is one we built: the update banner opens a URL
/// that came from a remote `latest.json`, and EventGrid webhook destinations
/// are whatever Azure returns. A scheme check is what stops those from
/// reaching the OS handler for `file:`, `ms-msdt:`, or anything else
/// registered on the machine.
fn is_safe_web_url(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    (lower.starts_with("https://") || lower.starts_with("http://"))
        // A control character in a URL has no legitimate meaning and is how
        // an argument gets split into two.
        && !url.chars().any(|c| c.is_control())
}

pub fn open_in_browser(url: &str) {
    // Spawn so a slow open(1) doesn't block the UI thread.
    let url = url.to_string();
    if !is_safe_web_url(&url) {
        crate::services::activity::error(
            "Refused to open link",
            url.clone(),
            "not an http(s) URL".to_string(),
        );
        return;
    }
    std::thread::spawn(move || {
        if let Err(e) = open_url(&url) {
            crate::services::activity::error("Failed to open Portal link", url.clone(), e);
        } else {
            crate::services::activity::info("Opened Portal link", url);
        }
    });
}

#[cfg(target_os = "macos")]
fn open_url(url: &str) -> Result<(), String> {
    let status = std::process::Command::new("open")
        .arg(url)
        .status()
        .map_err(|e| format!("{e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("`open` exited {}", status))
    }
}

#[cfg(target_os = "windows")]
fn open_url(url: &str) -> Result<(), String> {
    // Deliberately *not* `cmd /c start "" <url>`.
    //
    // `Command::args` quotes for the MSVCRT argv parser. cmd.exe does not use
    // that parser — it re-scans its own command line for `&`, `|`, `^`, `>`
    // and `%`, so a URL containing `&calc` runs calc. (Rust 1.77.2's
    // BatBadBut fix hardens `.bat`/`.cmd` *targets*; it does nothing when the
    // target is cmd.exe itself with an explicit `/c`.) Since not every URL
    // here is one we built, that is a live injection sink.
    //
    // rundll32 is an ordinary executable, so std's escaping is the escaping
    // that actually applies, and no shell ever sees the URL.
    let status = std::process::Command::new("rundll32.exe")
        .args(["url.dll,FileProtocolHandler", url])
        .status()
        .map_err(|e| format!("{e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("`rundll32 url.dll` exited {}", status))
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn open_url(url: &str) -> Result<(), String> {
    let status = std::process::Command::new("xdg-open")
        .arg(url)
        .status()
        .map_err(|e| format!("{e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("`xdg-open` exited {}", status))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_portal_links_are_opened() {
        assert!(is_safe_web_url(
            "https://portal.azure.com/#@/resource/subscriptions/x/overview"
        ));
        assert!(is_safe_web_url("http://localhost:7071/api/health"));
    }

    /// The update banner opens a URL that came from a remote `latest.json`,
    /// so "we built this string" is not an assumption available here.
    #[test]
    fn a_non_web_scheme_never_reaches_the_os_handler() {
        assert!(!is_safe_web_url("file:///etc/passwd"));
        assert!(!is_safe_web_url("ms-msdt:/id PCWDiagnostic"));
        assert!(!is_safe_web_url("javascript:alert(1)"));
        assert!(!is_safe_web_url(""));
    }

    /// A newline is how one argument becomes two.
    #[test]
    fn control_characters_are_rejected() {
        assert!(!is_safe_web_url("https://example.com\n& calc"));
        assert!(!is_safe_web_url("https://example.com\r\nHost: evil"));
    }

    /// `&` is legitimate in a query string and must keep working — it is the
    /// *opener* that was fixed, not the URL that needs sanitising.
    #[test]
    fn a_query_string_with_ampersands_is_still_a_valid_link() {
        assert!(is_safe_web_url(
            "https://portal.azure.com/?api-version=2024-04-01&$top=20"
        ));
    }
}
