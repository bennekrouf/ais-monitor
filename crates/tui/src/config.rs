//! Persistent user config — remembers the last sub / rg / app / local dir so
//! launch is one keystroke. Lives at `$XDG_CONFIG_HOME/ais-monitor/tui.json`
//! (or the OS equivalent via `dirs`).
//!
//! Read-on-start, write-on-change. Errors are non-fatal: a missing or
//! corrupt file just means "no remembered selection".

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    pub subscription: Option<String>,
    pub resource_group: Option<String>,
    pub logic_app: Option<String>,
    /// Watch-mode polling interval in seconds. Defaults to 5.
    #[serde(default = "default_watch_interval")]
    pub watch_interval_secs: u64,
}

fn default_watch_interval() -> u64 {
    5
}

impl Default for Config {
    fn default() -> Self {
        Self {
            subscription: None,
            resource_group: None,
            logic_app: None,
            watch_interval_secs: default_watch_interval(),
        }
    }
}

/// CLI overrides applied on top of the saved config. Anything passed on the
/// command line wins and is persisted to disk on next save.
#[derive(Default, Debug)]
pub struct CliArgs {
    pub subscription: Option<String>,
    pub resource_group: Option<String>,
    pub logic_app: Option<String>,
    pub watch_interval_secs: Option<u64>,
    /// Use `az login --use-device-code` instead of the browser flow.
    /// Required for headless / SSH-only environments (Server Core, jumpboxes
    /// without a display). When set, the TUI suspends to print the device
    /// code in the user's regular terminal, then resumes after sign-in.
    pub device_code: bool,
    pub help: bool,
}

impl CliArgs {
    /// Parse `argv[1..]`. Unknown flags trigger `help`. Form: `--key value` or
    /// `--key=value`. Long-form only; we don't have enough flags to justify a
    /// dependency on `clap` yet.
    pub fn parse(args: impl IntoIterator<Item = String>) -> Self {
        let mut out = CliArgs::default();
        let mut it = args.into_iter().peekable();
        while let Some(arg) = it.next() {
            let (key, inline_val) = match arg.split_once('=') {
                Some((k, v)) => (k.to_string(), Some(v.to_string())),
                None => (arg, None),
            };
            let take_val = |it: &mut std::iter::Peekable<_>,
                            inline: Option<String>|
             -> Option<String> { inline.or_else(|| it.next()) };
            match key.as_str() {
                "--help" | "-h" => out.help = true,
                "--sub" | "--subscription" => out.subscription = take_val(&mut it, inline_val),
                "--rg" | "--resource-group" => out.resource_group = take_val(&mut it, inline_val),
                "--app" | "--logic-app" => out.logic_app = take_val(&mut it, inline_val),
                "--device-code" => out.device_code = true,
                "--watch-interval" => {
                    if let Some(v) = take_val(&mut it, inline_val).and_then(|s| s.parse().ok()) {
                        out.watch_interval_secs = Some(v);
                    }
                }
                _ => out.help = true,
            }
        }
        out
    }
}

pub fn usage() -> &'static str {
    "ais-monitor-tui — terminal UI for Azure Logic Apps chain monitoring\n\
     \n\
     Just run `ais-monitor-tui` — the picker walks you through choosing a\n\
     subscription and a Logic App on first launch, then remembers them.\n\
     \n\
     usage: ais-monitor-tui [flags]\n\
     \n\
     flags (all optional — skip these unless you already know the values):\n  \
       --sub <id>               Azure subscription id (skips picker step 1)\n  \
       --rg <name>              resource group (paired with --app)\n  \
       --app <name>             logic app name (skips picker step 2)\n  \
       --watch-interval <secs>  watch-mode refresh interval (default 5)\n  \
       --device-code            use `az login --use-device-code` for headless\n  \
                                sign-in (SSH, Server Core, no-browser hosts)\n  \
       -h, --help               show this message\n\
     \n\
     To discover the values yourself:\n  \
       az account list --query \"[].{id:id,name:name}\" -o table\n  \
       az resource list --resource-type Microsoft.Web/sites \\\n    \
         --query \"[?contains(kind,'workflowapp')].{app:name,rg:resourceGroup}\" -o table\n\
     \n\
     keys: press ? inside the app for the full keymap"
}

impl Config {
    pub fn load() -> Self {
        path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    #[allow(dead_code)] // wired in Phase 3 when selections become editable
    pub fn save(&self) {
        let Some(p) = path() else { return };
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(s) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(p, s);
        }
    }
}

fn path() -> Option<PathBuf> {
    // Honor `AIS_MONITOR_HOME` (single root for both config + caches under
    // locked-down profiles); fall back to the OS-standard config dir.
    if let Some(home) = std::env::var_os("AIS_MONITOR_HOME") {
        return Some(PathBuf::from(home).join("tui.json"));
    }
    dirs::config_dir().map(|d| d.join("ais-monitor").join("tui.json"))
}
