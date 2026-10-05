---
name: ais-monitor
description: Help someone use AIS Monitor, the console for watching Azure Logic Apps Standard workflow chains in production (desktop app and the ais-monitor-tui terminal edition). Use this whenever the user mentions AIS Monitor or ais-monitor-tui, or is monitoring deployed Logic Apps Standard and asks about chain health, run history, failing workflows, dead-letter queues, app settings drift against App Configuration, managed identity RBAC, Function Apps, Event Grid, or a Logic App that won't show up, AuthorizationFailed or throttling errors from the Azure management API, or az login and PIM trouble. Also use it when they want to report an AIS Monitor bug or ask the author for help.
---

# AIS Monitor

AIS Monitor is an operational console for Azure Logic Apps Standard running in
Azure. It maps how workflows hand work to each other through Service Bus
queues ("chains") and shows each chain's health, run history and failures,
alongside the resources around them. There are two editions on the same
backend and the same on-disk cache:

- **AIS Monitor** (desktop, macOS/Windows/Linux): the full console.
- **`ais-monitor-tui`** (terminal): a chain browser and trigger tool for SSH
  sessions, jumpboxes and Windows Server. It is deliberately smaller.

The person you are helping is usually an integration developer or someone on
support duty, looking at a real (often client's) production environment.
Everything AIS Monitor shows comes from Azure through the `az` CLI and the
signed-in user's permissions, so most problems are about sign-in, the
subscription selected, or what that user is allowed to read.

## How it is organised

- **Profiles.** On first launch the user creates a profile: Azure subscription,
  Logic App, and optionally a **Local Workspace** (the project folder, used for
  chain links and payload suggestions), an **App Configuration Store** (the
  source of truth for the app-settings drift view) and an **Azure DevOps**
  organisation and project (for variable group cleanup). A profile with its
  own subscription uses it, whatever `az` has selected.
- **Chains** (both editions): workflows linked through shared Service Bus
  queues, with each step's trigger, the queues between them, deployment
  status, run history, a per-run action timeline, and KPIs (success rate,
  average and p95 duration, failure streak). Workflows linked by something
  the scan can't see (a dynamic queue name) are linked by hand in a
  `.ais-chain` file at `<project>/logic_apps/.ais-chain`, one
  `Source-Workflow -> Target-Workflow` per line. The TUI reads the chain cache
  the desktop app built on the same machine, so hints show up there after the
  desktop app has discovered chains.
- **Desktop tabs**, grouped:
  - **Monitor**: **Home** (failing workflows, runs in progress, dead-letter
    backlog, drift, RBAC gaps, cost; polls every 10 s and backs off when Azure
    throttles), **Resources** (health of everything in the resource group),
    **Health Check** (app settings and managed identity checks).
  - **Inspect**: **App Settings** (live values against App Configuration),
    **Functions** (Function Apps, metrics, errors, and each function's recent
    runs with their log lines; needs Application Insights), **EventGrid**
    (topics, subscriptions, retried and dead-lettered deliveries), **RBAC**
    (managed identity role assignments).
  - **Tools**: **Observability** (live log tail, month-to-date cost),
    **Diagnostics** (connectivity probes), **API Test**, **Graph** (the chain
    dependency graph).
  - **Admin**: **Var Groups** (Azure DevOps variable group cleanup).
- **Home in practice**: click a running workflow to follow that run live in
  the console at the bottom; each dead-letter queue has a 🔗 button that opens
  it in Service Bus Explorer in the Azure Portal, to peek, resubmit or purge.
- **Trigger panel** (desktop): fetch an HTTP-triggered workflow's callback
  URL, edit and save named JSON payloads, and watch the run appear.
- **TUI**: arrows or `j`/`k` to move, `Tab` to cycle focus, `Enter` for a run's
  action timeline, `/` to filter chains, `w` for watch mode (refreshes every
  5 s; `--watch-interval 10` changes it), `m` to rename a chain, `?` for the
  full keymap. `ais-monitor-tui --device-code` signs in with a code on a
  machine without a browser.

## Permissions

What the user can see depends on their Azure role, and this explains many
"empty" or "AuthorizationFailed" views:

- **Reader** on the resource group covers chains, KPIs, resources,
  observability and diagnostics.
- **Run history** goes through the Logic App's `hostruntime` API, which
  **Reader is not enough for**; it needs Contributor on the Logic App. This is
  the most common surprise.
- **Changing anything** (triggering a workflow, resetting app settings,
  purging or requeuing messages, assigning roles) needs Contributor, plus User
  Access Administrator for the RBAC tab. Every such action asks for
  confirmation and shows the exact `az` command first.
- With **PIM** (just-in-time access), the role must be activated before the
  Logic App is visible: activate it, then **↻ Refresh**.

## When something fails

1. **Get the exact message** the app shows (Home, the tab, or the console).
2. **Check sign-in and subscription**: `az account show` says who is signed in
   and which subscription is current. An expired session shows as "Azure
   session expired or invalid — sign in again"; `AADSTS…` codes come from
   Microsoft sign-in, not from AIS Monitor.
3. **Check the permission** the failing view needs (above). The app often
   names it, and the `az role assignment list --assignee <you> --scope <id>
   --include-inherited` command it suggests shows what the user actually has.
4. **Look up the symptom** in `references/troubleshooting.md`.
5. If it's still unexplained, check the version (window title or the update
   banner): the release notes at
   <https://mayorana.ch/en/apps/ais-monitor/releases> say which version fixed
   what. Then offer to draft a report (below).

Treat the production environment with care. Don't suggest purging dead
letters, resetting settings or assigning roles as a way to make an error go
away; those change a live system and are the user's (or their team's) call.

Config, chain cache, run cache and renames live under the cache folder
(`~/.cache` on Linux and macOS, `%LOCALAPPDATA%` on Windows), or under
`AIS_MONITOR_HOME` when that is set, for locked-down machines.

## Reporting a problem to the author

When the problem looks like an AIS Monitor bug, or the user wants to send
feedback, help them write a report they can send. Go through "When something
fails" first, even when the user asks straight for a report: a permission or
sign-in cause on their side wastes their time and the author's, and finding it
is more useful to them than a report. The user sends it, not you: never create
an issue, send an email or submit a form on their behalf.

Screens and messages from a production environment contain subscription and
tenant ids, resource group, Logic App, workflow and queue names, URLs,
callback URLs with signatures (`sig=`), connection strings and message
contents. Before showing the draft, replace anything like that with neutral
placeholders (`<subscription>`, `<resource-group>`, `<logic-app>`,
`<workflow>`, `<queue>`), then tell the user what you replaced and ask them to
check the rest. Keep only what shows the failure.

Use this structure:

~~~markdown
**AIS Monitor version:** 0.3.41 (desktop / TUI)
**OS:** macOS 15.1 / Windows 11 / Windows Server 2022 / Ubuntu 24.04
**Azure role on the resource group:** Reader / Contributor / …

**Where**
Home → "Still failing" / Chains → <chain> / Functions → Runs / …

**What I did**
1. …

**What I expected**
…

**What happened instead**
…

**Message shown**
```
…
```

**Workaround found, if any**
…
~~~

Then give them the two ways to send it:

- A GitHub issue at <https://github.com/bennekrouf/ais-monitor/issues/new>:
  they paste the title and body and submit it themselves. Issues there are
  public, which is one more reason the draft must be scrubbed.
- The contact form at <https://mayorana.ch/en/contact>, for anything they
  would rather not post publicly.
