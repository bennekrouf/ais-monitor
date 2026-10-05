# AIS Monitor troubleshooting

Known failures, grouped by where they show up. Each entry: what the user sees,
why, and what to do. Versions in brackets are where a fix arrived; if the user
is older, updating is often the answer.

## Contents

- [Starting and signing in](#starting-and-signing-in)
- [Nothing, or the wrong thing, shows up](#nothing-or-the-wrong-thing-shows-up)
- [Run history and permissions](#run-history-and-permissions)
- [Throttling](#throttling)
- [Chains](#chains)
- [Other tabs](#other-tabs)
- [Terminal edition (TUI)](#terminal-edition-tui)

## Starting and signing in

**"Azure CLI not found"** on the welcome screen.
Install the Azure CLI. An app started from the Dock, Finder or Start menu
picks up the login shell's PATH (0.3.20); after installing, restart the app,
and on Windows open a new terminal so the PATH change is seen.

**"Not logged in" / "Azure session expired or invalid — sign in again."**
Sign in with **Connect to Azure**, or `az login` in a terminal. If the browser
doesn't open, the screen shows the command to run by hand.

**`AADSTS…` errors during sign-in.**
They come from Microsoft Entra ID, not from AIS Monitor. The common ones:
`AADSTS50076` (multi-factor authentication required: sign in again
interactively), `AADSTS50058` (no signed-in session in the browser),
`AADSTS700082` / `AADSTS70043` (refresh token expired: `az login` again),
`AADSTS50173` (the grant was revoked, for example after a password change).
A conditional-access policy can also block `az` from an unmanaged device;
that's for the tenant's admins.

**Two sign-ins at once, or a browser window opening for no reason.**
Fixed in 0.3.24.

## Nothing, or the wrong thing, shows up

**"No Logic Apps found — activate PIM then click ↻ Refresh"** /
**"No Logic Apps (Standard) in this subscription".**
The user has no active role on any Logic App Standard in that subscription.
With PIM, activate the role in the portal, then **↻ Refresh**. Otherwise
check the subscription in the profile: AIS Monitor only lists Logic Apps
**Standard**, not Consumption.

**The app shows a different subscription from the one expected.**
A profile with its own subscription uses it (0.3.21). Check the profile with
**Edit Profile**; without one set, the app follows `az account show`.

**"No chains discovered."**
Chains are workflows linked by Service Bus queues. A Logic App whose
workflows don't share queues has no chains; a link through a dynamic queue
name needs a `.ais-chain` hint (see SKILL.md). Setting the **Local Workspace**
in the profile lets the desktop app read the project's workflows too.

**A tab shows "Needs Application Insights" or "No Application Insights found
in this resource group."**
Functions metrics and runs read Application Insights; without a component in
the resource group, those parts stay empty. Nothing to fix in AIS Monitor.

## Run history and permissions

**Runs don't load, with "AuthorizationFailed" (often "N workflow(s):
AuthorizationFailed").**
Run history uses the Logic App's `hostruntime` API: **Reader is not enough;
Contributor is.** The app suggests
`az role assignment list --assignee <you> --scope <logic app id>
--include-inherited` to see what the user really has. Ask whoever manages
access; don't work around it.

**An action (trigger, reset a setting, purge or requeue, assign a role) fails
with Forbidden.**
Changes need Contributor; the RBAC tab also needs User Access Administrator.
The confirmation dialog shows the exact `az` command, which can be run by
someone who has the rights.

## Throttling

**"Azure is throttling this subscription — polling has slowed down and will
speed back up on its own."** / "Azure throttled run-history reads" / "Too Many
Requests".
Azure limits management API calls per subscription, and the `hostruntime`
API (run history) hardest. Home backs off automatically. Other tools hitting
the same subscription (pipelines, other dashboards, other people with AIS
Monitor open) share the limit. Lower the poll rate or pause polling on Home if
it keeps happening; it clears on its own.

## Chains

**Two workflows that hand work to each other aren't linked.**
The link goes through something the scan can't follow, typically a queue name
built at runtime. Add `Source -> Target` to `logic_apps/.ais-chain` in the
project and point the profile's **Local Workspace** at that project.

**The TUI shows no chains, or not the same ones as the desktop app.**
The TUI reads the chain cache the desktop app writes on the same machine.
Open the desktop app once with the same profile; on a machine without it,
the TUI discovers what it can from the subscription.

## Other tabs

**A tab is slow the first time it opens.**
From 0.3.39 the other tabs load in the background, one every few seconds,
once chains are discovered; they're usually ready by the time they're opened.

**App Settings drift view is empty.**
It compares live settings with an **App Configuration Store**; set one in the
profile.

**Var Groups is empty.**
It needs the Azure DevOps organisation URL and project in the profile, and
the user signed in with access to that organisation.

## Terminal edition (TUI)

**No browser on the machine** (Windows Server Core, SSH, jumpbox).
`ais-monitor-tui --device-code` prints a URL and a code; sign in on any
device with a browser and the TUI resumes.

**Windows blocks the downloaded `.exe`.**
Right-click → Properties → **Unblock**, or `Unblock-File ais-monitor-tui.exe`.
The PowerShell install script does this for you.

**Config or cache can't be written** (roaming profiles, locked-down
servers).
Set `AIS_MONITOR_HOME` to a writable folder; both editions keep everything
under it.
