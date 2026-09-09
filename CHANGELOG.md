# Changelog

What changed in each release of **AIS Monitor**, the operational console for
Azure Logic Apps Standard workflow chains — and of its terminal companion,
`ais-monitor-tui`.

The public version of this page — with the download for each release — lives at
<https://mayorana.ch/en/apps/ais-monitor/releases>. It is generated from this
file by `scripts/changelog_to_json.py`, so this file is the only place a
release note is written.

Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versioning: [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Each heading is dated on the day its tag was pushed. Releases that carried only
build or packaging work say so rather than being hidden: the version numbers a
user sees in the update prompt should all be accounted for.

## [0.3.32] - 2026-09-08

### Changed

- Dead-letter messages: handling reworked and the panel's layout with it, so a
  queue with a large dead-letter backlog stays readable and stays responsive.

## [0.3.31] - 2026-09-06

### Changed

- Internal: the services the desktop app and `ais-monitor-tui` both use moved
  into a shared `ais-monitor-core` crate, so the two editions read Azure
  through one implementation instead of two that drift.

## [0.3.30] - 2026-09-05

### Added

- Delivery summary on Event Grid subscriptions: how many events were retried
  and how many were dead-lettered, next to the subscription they belong to.

## [0.3.29] - 2026-09-03

### Changed

- CI pipeline only — no user-visible change.

## [0.3.28] - 2026-09-03

### Fixed

- The update banner spells the product name the way the product does.

## [0.3.27] - 2026-09-02

### Changed

- The update check sends a versioned User-Agent, which is what makes
  per-version adoption visible in the download logs — the number that says how
  many people are still on a build with a bug that is already fixed.

## [0.3.26] - 2026-09-02

### Added

- The update check resolves the artifact for the platform it is running on, so
  the banner offers the macOS, Windows or Linux build directly instead of the
  download page.

## [0.3.25] - 2026-08-31

### Changed

- Internal: sign-in moved behind one shared hook, so every screen that can
  start an Azure login now does it the same way.

## [0.3.24] - 2026-08-31

### Fixed

- Tenant handling on sign-in: two logins can no longer race each other, and
  the app stops opening a browser window it does not need.

## [0.3.23] - 2026-08-31

### Changed

- Internal: the wait-for-login step was extracted so the desktop app and the
  TUI share it.

## [0.3.22] - 2026-08-31

### Changed

- The Windows installer runs with the lowest privileges by default, so a
  developer can install it per-user without a UAC prompt. IT can still install
  it machine-wide with a command-line option, and the desktop shortcut lands
  in the right place either way.

## [0.3.21] - 2026-08-30

### Fixed

- A profile with its own subscription set now uses it, instead of falling back
  to whatever subscription `az` had selected.

### Added

- The new-profile form validates what you type before saving it.

## [0.3.20] - 2026-08-29

### Fixed

- The app adopts the login shell's `PATH`, so `az` and the other tools it
  shells out to are found when it is launched from Finder or the Start menu
  rather than from a terminal.

## [0.3.19] - 2026-08-29

### Changed

- Release tooling only — no user-visible change.

## [0.3.18] - 2026-08-27

### Changed

- Release safety: the pipeline refuses to tag a release with no version bump,
  and fails outright when the deploy secrets are missing rather than reporting
  success for a release nobody can download.

## [0.3.17] - 2026-08-27

### Changed

- Builds are distributed from mayorana.ch instead of GitHub Releases. Every
  release publishes a `latest.json` carrying the `sha256` of each artifact, so
  a download can be verified against a source that does not deliver it.
- Licensed under PolyForm Noncommercial 1.0.0: free for personal, educational
  and non-profit use; commercial use requires a licence.

---

Releases before 0.3.17 predate this file. Their tags remain on
[GitHub](https://github.com/Bennekrouf/ais-monitor/releases).
