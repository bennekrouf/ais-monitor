# AIS Monitor

An operational console for **Azure Logic Apps Standard workflow chains** — chain health, config drift, RBAC, logs, and live diagnostics in one place, plus a lightweight terminal companion for SSH sessions.

Two editions, same Azure backend, shared on-disk cache:

| Edition | Best for | Built with |
|---|---|---|
| **AIS Monitor** (Desktop / GUI) | Day-to-day monitoring and troubleshooting — the full console (12 tabs: dashboard, resource health, config drift, RBAC, observability, diagnostics, and more) | [Dioxus](https://dioxuslabs.com/) (Rust) — runs as a native app via WebView |
| **`ais-monitor-tui`** (Terminal / TUI) | SSH sessions, jumpboxes, Windows Server — chain browsing, run history, and triggering without a GUI | [ratatui](https://ratatui.rs/) (Rust) — single static binary, no installer |

The TUI is deliberately the smaller of the two — a fast chain browser and trigger tool for when there's no desktop available, not a shrunk copy of every Desktop tab.

Both: macOS · Windows · Linux. Both read live data from Azure via the `az` CLI.

<!-- screenshots:
     docs/screenshots/desktop-main.png   — desktop main view
     docs/screenshots/tui-browser.png    — TUI chain browser
     docs/screenshots/tui-actions.png    — TUI action timeline drill-in
-->

---

## Install

### Desktop (GUI)

| Platform | One-liner / link |
|---|---|
| **macOS** (Apple Silicon) | Download [`ais-monitor-macos-arm64.dmg`](https://github.com/Bennekrouf/ais-monitor/releases/latest/download/ais-monitor-macos-arm64.dmg), open it, drag **AIS Monitor** to Applications. Signed with Apple Developer ID + notarized — opens with a normal double-click. |
| **Windows** | Download [`ais-monitor-setup.exe`](https://github.com/Bennekrouf/ais-monitor/releases/latest) and run the installer. |
| **Linux** (x86_64) | `curl -L https://github.com/Bennekrouf/ais-monitor/releases/latest/download/ais-monitor-linux-x86_64.tar.gz \| tar xz && cd ais-monitor-linux-x86_64 && sudo ./setup-linux.sh && ./ais-monitor` |

### Terminal (TUI)

**Windows** — paste into PowerShell (Windows Terminal / `pwsh` / `powershell`):

```powershell
Invoke-WebRequest https://raw.githubusercontent.com/Bennekrouf/ais-monitor/master/scripts/install-tui.ps1 -UseBasicParsing | Invoke-Expression
```

Drops the binary into `%USERPROFILE%\bin`, adds it to your PATH, clears the SmartScreen mark, checks for `az`. No admin rights. On `cmd.exe`, run the same script via `powershell -ExecutionPolicy Bypass -File install-tui.ps1` after downloading it.

No script at all: [download the `.exe` directly](https://github.com/Bennekrouf/ais-monitor/releases/latest/download/ais-monitor-tui-x86_64-pc-windows-msvc.exe), rename to `ais-monitor-tui.exe`, put it on your PATH, then right-click → Properties → **Unblock** (or `Unblock-File ais-monitor-tui.exe`).

**macOS / Linux — paste into your shell:**

```bash
curl -fsSL https://raw.githubusercontent.com/Bennekrouf/ais-monitor/master/scripts/install-tui.sh | sh
```

Detects platform, installs to `/usr/local/bin` (or `~/.local/bin` if unprivileged), strips macOS Gatekeeper quarantine.

**Any platform via `cargo binstall`** (no compile):

```bash
cargo binstall ais-monitor-tui
```

**Any platform from source** (Rust toolchain — ~5 min compile):

```bash
cargo install --git https://github.com/Bennekrouf/ais-monitor ais-monitor-tui
```

---

## Quick start

**Sign in to Azure** (one-time per machine — both editions reuse this):

```bash
az login
```

**Run whichever edition you installed:**

```bash
# Desktop
open -a "AIS Monitor"          # macOS
ais-monitor                    # Linux
# (Windows: Start menu → AIS Monitor)

# Terminal
ais-monitor-tui
```

On first launch a picker walks you through subscription → Logic App. Choices are remembered. Inside the TUI press `?` for the full keymap.

**Headless / no browser available** (Windows Server Core, SSH, locked-down jumpbox):

```bash
ais-monitor-tui --device-code
```

The TUI suspends, runs `az login --use-device-code` in your terminal (prints a URL + 8-char code), resumes once sign-in completes on any device with a browser.

---

## What it does

### Chains — the foundation, both editions

AIS Monitor scans your `logic_apps/` folder (Desktop) or your Azure subscription (both editions) and maps how workflows connect to each other via Service Bus queues — a **chain graph** powered by [ais-chain](https://github.com/Bennekrouf/ais-chain).

Each chain shows the ordered workflow steps, the queues linking them, each step's trigger type, and live deployment status. Selecting a chain drills into run history, a per-run action timeline, and KPIs (success rate, avg/p95 duration, failure streak — color-coded, with a sparkline in the TUI). Chains can be renamed locally (TUI: `m`, Desktop: rename button); manual links for workflows not connected by a shared queue go in a `.ais-chain` file (see [Usage notes](#ais-chain--manual-link-hints) below).

### The Desktop console — 12 tabs, grouped

Everything past Chains is Desktop-only. The tab bar mirrors this grouping:

- **Monitor** — **Home** (dashboard: failing workflows, live runs, dead letters, drift, RBAC gaps, cost, all in one glance), **Resources** (health across every resource in the group), **Health Check** (app-settings and managed-identity checks)
- **Inspect** — **App Settings** (live values vs. App Configuration, drift detection), **Functions** (function apps, metrics, errors), **EventGrid** (topics and subscriptions), **RBAC** (managed identity role assignments)
- **Tools** — **Observability** (live log tail, month-to-date cost), **Diagnostics** (connectivity probes), **API Test** (send test requests), **Graph** (interactive chain dependency graph)
- **Admin** — **Var Groups** (DevOps variable group cleanup)

Hover any tab icon for what it does; the app opens to **Home** by default. The Home dashboard polls in the background (10s by default, adaptive backoff if Azure throttles) so it stays current unattended — useful as a wall display. Poll rate and pause are controlled from the Home tab itself.

### Trigger panel (Desktop only)

Fetch the callback URL for any HTTP-triggered workflow, edit the JSON payload in-browser, save/reload named payloads per workflow, and watch the resulting run status appear in real time.

### Watch mode (TUI only)

Press `w` to enable 5-second auto-refresh of the focused step's runs — handy for tailing a problem workflow. Toggle off with `w` again. Configurable: `ais-monitor-tui --watch-interval 10`.

---

## Azure requirements

Both editions call the Azure management API (`az` CLI under the hood). You need `az login` completed either way.

**Read-only usage** (Chains, KPIs, run/action detail, Resources, Observability, Diagnostics) needs **Reader** on the resource group.

**Anything that changes Azure state** needs more — the Desktop console can trigger workflows, reset app settings, purge/requeue Service Bus messages, and assign RBAC roles, each gated behind a confirm dialog that shows the exact `az` command it's about to run. Contributor (plus User Access Administrator for the RBAC tab specifically) covers all of it.

Run history in particular goes through the Logic App's `hostruntime` API, which needs more than Reader and is throttled per-subscription — the Home dashboard's background poll backs off automatically if it gets rate-limited.

---

## TUI keymap essentials

Arrow keys / `j k` move, `Tab` cycles focus, `Enter` drills into a run's action timeline, `/` filters chains, `w` toggles watch mode. Press **`?`** in-app for the full keymap — it's the source of truth, not this list.

---

## Production / locked-down environments

Override config + cache locations when `%LOCALAPPDATA%` (Windows) or `~/.cache` (Linux/macOS) isn't writable or roams unpredictably:

```bash
export AIS_MONITOR_HOME=/var/opt/ais-monitor   # bash / zsh
$env:AIS_MONITOR_HOME = "D:\ais-monitor"        # PowerShell
```

Both editions honor this; everything (config, chain cache, runs cache, renames) roots under it.

---

## Usage notes

### `.ais-chain` — manual link hints

If two workflows are connected through a mechanism that `ais-chain` can't auto-detect (e.g. a dynamic queue name), drop a hint file:

```
# .ais-chain — one link per line: source-workflow -> target-workflow
Workflow-A -> Workflow-B
```

Place it at `<your-platform>/logic_apps/.ais-chain`. The Desktop edition reads it directly. The TUI relies on the shared chain cache that the Desktop populates, so once the Desktop has discovered chains with hints, the TUI sees the same graph on the same machine.

---

## Contributing

### Prerequisites

| Tool | Install |
|------|---------|
| **Rust** | `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \| sh` |
| **MSYS2 + MinGW-w64** (Windows desktop build) | `scripts\setup-windows-dev.ps1` |

### Build from source

```bash
git clone https://github.com/Bennekrouf/ais-monitor.git
cd ais-monitor

# Desktop
cargo build --release
./target/release/ais-monitor

# Terminal
cargo build --release -p ais-monitor-tui
./target/release/ais-monitor-tui
```

### Project layout

```
src/                          Desktop (Dioxus) frontend
  components/                 One file per tab (home_panel, chain_detail, rbac_panel, …) plus shared UI
  screens/                    welcome, main_screen
  services/                   chain, azure, kpi, chain_probe, remote_chain, and the rest
                              ↑ core chain/Azure logic shared with the TUI via crates/core

crates/
  core/                       ais-monitor-core — service layer (re-exports src/services)
  tui/                        ais-monitor-tui — ratatui frontend
    src/
      main.rs                 tokio runtime + CLI dispatch
      app.rs                  App state, render, event loop
      msg.rs                  Msg enum — every state mutation
      config.rs               persistent config + CLI parser
      runs_cache.rs           on-disk per-workflow runs cache
      tui.rs                  terminal init/restore
      bin/probe.rs            no-TUI service smoke test

scripts/
  release.sh                  Cut a release (bump version, tag, push)
  install-tui.ps1             Windows TUI installer (one-liner)
  install-tui.sh              macOS / Linux TUI installer (one-liner)
  setup-linux.sh              Linux runtime dependency installer

.github/workflows/
  release.yml                 Build all platforms (Desktop + TUI), publish Release
```

### Releasing

```bash
./scripts/release.sh            # auto-bump patch, confirm, push
./scripts/release.sh --minor    # bump minor
./scripts/release.sh 1.0.0      # explicit version
./scripts/release.sh --dry-run  # preview only
```

The tagged release workflow builds both the Desktop bundle (DMG / installer / tarball) and the TUI binary for all three platforms.

---

## Related

- **[ais-runner](https://github.com/Bennekrouf/ais-runner)** — run and test Logic Apps locally (Azurite, func start, DevOps)
- **[ais-chain](https://github.com/Bennekrouf/ais-chain)** — workflow chain parser library used by both editions

---

## Tech stack

| | |
|--|--|
| Desktop UI | [Dioxus 0.6](https://dioxuslabs.com/) — Rust, native WebView |
| Terminal UI | [ratatui 0.28](https://ratatui.rs/) + [crossterm](https://github.com/crossterm-rs/crossterm) |
| Async runtime | [Tokio](https://tokio.rs/) |
| HTTP client | [reqwest](https://github.com/seanmonstar/reqwest) |
| JSON | [serde / serde_json](https://serde.rs/) |
| Chain analysis | [ais-chain](https://github.com/Bennekrouf/ais-chain) |
| File picker (Desktop) | [rfd](https://github.com/PolyMeilex/rfd) |
