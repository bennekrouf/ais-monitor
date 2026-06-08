# AIS Monitor

A desktop tool for **visualising, triggering, and monitoring Azure Logic Apps Standard workflow chains** — locally and in Azure.

Built with [Dioxus](https://dioxuslabs.com/) (Rust) · macOS · Windows · Linux

---

## What it does

### Chain discovery

AIS Monitor scans your `logic_apps/` folder and automatically maps how workflows connect to each other via Service Bus queues, building a **chain graph** powered by [ais-chain](https://github.com/Bennekrouf/ais-chain).

Each chain shows:
- The ordered list of workflows (step → step → step)
- The Service Bus queues that link them
- The trigger type for each step (HTTP, SB, Timer, Blob…)
- Which workflows are currently **deployed** in Azure (live status badge)

Manual links can be added in a `.ais-chain` file in the `logic_apps/` folder for workflows that aren't connected via a shared queue.

### Chain detail

Select a chain to see:
- **Step list** — each workflow with its trigger and link type
- **Run history** — recent runs per workflow, pulled live from Azure
- **Action timeline** — drill into any run to see every action and its status
- **KPI dashboard** — at-a-glance health metrics (see below)
- **Trigger panel** — fire any HTTP-triggered workflow with a saved or custom JSON payload, then watch the run appear in real time
- **Rename** — give chains friendly display names (stored locally)

### KPI dashboard

Click **Check** on any chain to fetch the last 20 runs per workflow and see:

| Metric | Description |
|--------|-------------|
| **Success rate** | % of runs that succeeded, color-coded: green (≥95%), yellow (≥80%), red (<80%) |
| **Avg / p95 duration** | Mean and 95th-percentile end-to-end run time |
| **Failure streak** | Consecutive recent failures — highlighted when >0 |

For multi-step chains, per-workflow mini-cards break down the rate and duration of each step.

### Trigger panel

- Fetch the callback URL for any workflow directly from Azure
- Edit the JSON payload in-browser
- Save and reload named payloads per workflow
- See the HTTP response and the resulting run status side by side

### Event Grid panel

Browse Event Grid topics and subscriptions linked to your Azure resource group — useful for understanding how chains are triggered from upstream producers.

---

## Install

### macOS (Apple Silicon)

Download [`ais-monitor-macos-arm64.dmg`](https://github.com/Bennekrouf/ais-monitor/releases/latest/download/ais-monitor-macos-arm64.dmg), open it, and drag **AIS Monitor** to Applications.
Signed with Apple Developer ID and notarized — opens with a normal double-click.

### Windows

Download [`ais-monitor-setup.exe`](https://github.com/Bennekrouf/ais-monitor/releases/latest) and run the installer.

### Linux (x86\_64)

```bash
curl -L https://github.com/Bennekrouf/ais-monitor/releases/latest/download/ais-monitor-linux-x86_64.tar.gz | tar xz
cd ais-monitor-linux-x86_64
sudo ./setup-linux.sh && ./ais-monitor
```

---

## Usage

1. Launch `ais-monitor`
2. Select your platform folder (the root containing `logic_apps/`)
3. Log in to Azure if prompted — chains pull live run data from the Azure management API
4. The chain list populates automatically; click any chain to explore it

### `.ais-chain` — manual links

If two workflows are connected through a mechanism that `ais-chain` can't detect automatically (e.g. a dynamic queue name), add a hint file:

```
# .ais-chain — one link per line: source-workflow -> target-workflow
Workflow-A -> Workflow-B
```

---

## Azure requirements

AIS Monitor calls the Azure management API (`az` CLI under the hood) to:
- List deployed workflows and their states
- Fetch run history and action details
- Retrieve HTTP trigger callback URLs
- Browse Event Grid topics and subscriptions

You need to be logged in with `az login` and have at least **Reader** access on the Logic App resource.

---

## Contributing

### Prerequisites

| Tool | Install |
|------|---------|
| **Rust** | `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \| sh` |
| **MSYS2 + MinGW-w64** (Windows) | `scripts\setup-windows-dev.ps1` |

### Build from source

```bash
git clone https://github.com/Bennekrouf/ais-monitor.git
cd ais-monitor
cargo build --release
./target/release/ais-monitor
```

### Project layout

```
src/
  components/
    chain_list.rs       Left panel — list of discovered chains
    chain_detail.rs     Right panel — steps, runs, actions, trigger, KPIs
    trigger_panel.rs    Trigger a workflow with a saved payload
    eventgrid_panel.rs  Browse Event Grid topics and subscriptions
    login_banner.rs     Azure login status banner
  screens/
    welcome.rs          Folder picker
    main_screen.rs      Main layout — chain list + detail
  services/
    chain.rs            Chain discovery (wraps ais-chain)
    azure.rs            Azure CLI wrappers (login, runs, actions, triggers…)
    kpi.rs              KPI computation (success rate, duration, streaks)
    names.rs            Local chain rename storage
scripts/
  release.sh            Cut a release (bump version, tag, push → triggers CI)
  setup-linux.sh        Linux runtime dependency installer
  setup-windows.ps1     Windows runtime dependency installer
.github/workflows/
  build-mac.yml         CI build on push/PR (macOS)
  build-windows.yml     CI build on push/PR (Windows)
  build-linux.yml       CI build on push/PR (Linux x86_64 + arm64)
  release.yml           Build all platforms, publish GitHub Release
```

### Releasing

```bash
./scripts/release.sh            # auto-bump patch, confirm, push
./scripts/release.sh --minor    # bump minor
./scripts/release.sh 1.0.0      # explicit version
./scripts/release.sh --dry-run  # preview only
```

---

## Related

- **[ais-runner](https://github.com/Bennekrouf/ais-runner)** — run and test Logic Apps locally (Azurite, func start, DevOps)
- **[ais-chain](https://github.com/Bennekrouf/ais-chain)** — workflow chain parser library used by both tools

---

## Tech stack

| | |
|--|--|
| UI framework | [Dioxus 0.6](https://dioxuslabs.com/) — Rust, renders via WebView |
| Async runtime | [Tokio](https://tokio.rs/) |
| HTTP client | [reqwest](https://github.com/seanmonstar/reqwest) |
| JSON | [serde / serde_json](https://serde.rs/) |
| Chain analysis | [ais-chain](https://github.com/Bennekrouf/ais-chain) |
| File picker | [rfd](https://github.com/PolyMeilex/rfd) |
