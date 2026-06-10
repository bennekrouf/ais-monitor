# AIS Monitor

Visualise, trigger, and monitor **Azure Logic Apps Standard workflow chains** — locally and in production.

Two ways to use it, same Azure backend, shared on-disk cache:

| Edition | Best for | Built with |
|---|---|---|
| **AIS Monitor** (Desktop / GUI) | Day-to-day work on a workstation, exploring chains visually, triggering workflows | [Dioxus](https://dioxuslabs.com/) (Rust) — runs as a native app via WebView |
| **`ais-monitor-tui`** (Terminal / TUI) | SSH sessions, jumpboxes, Windows Server, anyone who lives in the terminal | [ratatui](https://ratatui.rs/) (Rust) — single static binary, no installer |

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

**Windows — paste into Windows Terminal / PowerShell:**

```powershell
iwr https://raw.githubusercontent.com/Bennekrouf/ais-monitor/master/scripts/install-tui.ps1 | iex
```

Downloads the latest binary into `%USERPROFILE%\bin`, adds it to your user PATH, clears the SmartScreen mark, checks for `az`. No admin rights.

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

### Chain discovery

AIS Monitor scans your `logic_apps/` folder (Desktop edition) or your Azure subscription (both editions) and maps how workflows connect to each other via Service Bus queues — a **chain graph** powered by [ais-chain](https://github.com/Bennekrouf/ais-chain).

Each chain shows:
- The ordered list of workflows (step → step → step)
- The Service Bus queues that link them
- The trigger type per step (HTTP, SB, Timer, Blob…)
- Which workflows are currently **deployed** in Azure (live status badge)

Manual links can be added via a `.ais-chain` file in `logic_apps/` for workflows not connected by a shared queue.

### Chain detail

Select a chain to see:
- **Step list** — each workflow with its trigger and link type
- **Run history** — recent runs per workflow, pulled live from Azure
- **Action timeline** — drill into any run to see every action and its status
- **KPI dashboard** — at-a-glance health metrics (success rate, p95 duration, failure streak)
- **Trigger panel** (Desktop only) — fire any HTTP-triggered workflow with a saved or custom JSON payload, then watch the run appear in real time
- **Rename** — give chains friendly display names (stored locally; the TUI's `m` key, the Desktop's rename button)

### KPI dashboard

For each workflow, the last 20 runs feed:

| Metric | Description |
|--------|-------------|
| **Success rate** | % of runs that succeeded, color-coded: green (≥95 %), yellow (≥80 %), red (<80 %) |
| **Avg / p95 duration** | Mean and 95th-percentile end-to-end run time |
| **Failure streak** | Consecutive recent failures — highlighted when >0 |

The TUI also renders a sparkline of recent run durations next to the KPI strip.

### Event Grid panel

Browse Event Grid topics and subscriptions linked to your Azure resource group — useful for understanding how chains are triggered from upstream producers. Open in the Desktop via the side panel; in the TUI press `g`.

### Trigger panel (Desktop only)

- Fetch the callback URL for any HTTP-triggered workflow directly from Azure
- Edit the JSON payload in-browser
- Save and reload named payloads per workflow
- See the HTTP response and the resulting run status side by side

### Watch mode (TUI only)

Press `w` to enable 5-second auto-refresh of the focused step's runs — handy for tailing a problem workflow. Toggle off with `w` again. Configurable: `ais-monitor-tui --watch-interval 10`.

---

## Azure requirements

Both editions call the Azure management API (`az` CLI under the hood) to:
- List deployed workflows and their states
- Fetch run history and action details
- Retrieve HTTP trigger callback URLs (Desktop)
- Browse Event Grid topics and subscriptions

You need `az login` completed and at least **Reader** access on the Logic App resource.

---

## TUI keymap (cheat sheet)

| Key | Action |
|---|---|
| `↑ ↓` / `j k` | Move cursor |
| `Tab` / `BackTab` | Cycle focus (chains → steps → runs → actions) |
| `h l` / `← →` | Lateral focus, drill in/out |
| `Enter` | Drill into selected run's action timeline |
| `/` | Filter chains by label / step name |
| `m` | Rename focused chain (persisted) |
| `w` | Toggle watch mode (auto-refresh runs) |
| `g` | Event Grid panel |
| `r` / `R` | Refresh focused pane / hard reload (clear caches) |
| `c` | Re-pick subscription / Logic App |
| `?` | Help overlay |
| `q` / `Esc` | Quit |

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
  components/                 UI components (chain_list, chain_detail, trigger_panel, …)
  screens/                    welcome, main_screen
  services/                   chain, azure, kpi, names, payload, remote_chain
                              ↑ shared with the TUI via crates/core

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
