//! Lightweight update check.
//!
//! Fetches the `latest.json` published with each GitHub release and compares
//! the version field to this build's `CARGO_PKG_VERSION`. Designed to be
//! cheap and side-effect-free so it can run in the background at startup.

use serde::Deserialize;
use std::collections::HashMap;

/// Served from mayorana.ch alongside the builds it describes, so update
/// checks do not depend on the source repository staying publicly readable.
const LATEST_URL: &str = "https://mayorana.ch/downloads/ais-monitor/latest/latest.json";
/// Fallback when `latest.json` has no entry for this OS (e.g. an Intel Mac —
/// only Apple Silicon is built). Sends the user to pick a build by hand
/// instead of at a link that would 404.
const RELEASES_URL: &str = "https://mayorana.ch/en/apps";

/// Sent on the update check so the download logs can tell a new install
/// (a browser hitting the site) from an existing user updating. Also
/// carries the version, which is what makes per-version adoption
/// visible — the number that says how many people are still on a build
/// with a bug that is already fixed.
const USER_AGENT: &str = concat!("ais-monitor/", env!("CARGO_PKG_VERSION"), " (updater)");

#[derive(Debug, Deserialize)]
struct LatestJson {
    version: String,
    tag: String,
    platforms: Platforms,
}

#[derive(Debug, Deserialize)]
struct Platforms {
    macos: HashMap<String, Artifact>,
    windows: HashMap<String, Artifact>,
    linux: HashMap<String, Artifact>,
}

#[derive(Debug, Deserialize)]
struct Artifact {
    url: String,
    #[serde(default)]
    sha256: String,
}

#[derive(Debug, Clone)]
pub struct UpdateInfo {
    pub latest_version: String,
    #[allow(dead_code)]
    pub latest_tag: String,
    /// Direct link to this OS's build, so the banner's button downloads the
    /// binary itself rather than opening a landing page to pick one from.
    pub release_url: String,
    /// Digest of the build `release_url` points at, as published.
    ///
    /// The banner hands the URL to the user's browser, so this process never
    /// sees the downloaded bytes and cannot verify them itself. Publishing a
    /// digest and then discarding it is worse than not publishing one — show
    /// it, so a user who cares can check what they downloaded. Empty when
    /// `latest.json` did not carry one.
    pub sha256: String,
}

/// Returns `Some(UpdateInfo)` if a newer release is available, else `None`.
/// Any network / parse failure → `None`. Never panics.
pub async fn check() -> Option<UpdateInfo> {
    let current = env!("CARGO_PKG_VERSION");
    let body = reqwest::Client::new()
        .get(LATEST_URL)
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
        .ok()?
        .text()
        .await
        .ok()?;
    let latest: LatestJson = serde_json::from_str(&body).ok()?;
    if is_newer(&latest.version, current) {
        let (release_url, sha256) = platform_artifact(&latest.platforms);
        Some(UpdateInfo {
            latest_version: latest.version,
            latest_tag: latest.tag,
            release_url,
            sha256,
        })
    } else {
        None
    }
}

/// Picks the artifact published for this OS *and this CPU*. Missing or
/// unparseable falls back to the landing page.
///
/// The architecture match is the point. Selecting with `values().next()` — a
/// `HashMap`, whose iteration order Rust randomises per process — meant that
/// the moment `latest.json` listed both an `aarch64` and an `x86_64` macOS
/// build, each launch offered whichever one the hash seed happened to yield.
/// Half the users would be sent an executable that cannot run on their
/// machine, intermittently.
fn platform_artifact(platforms: &Platforms) -> (String, String) {
    let fallback = || (RELEASES_URL.to_string(), String::new());
    let by_os = match std::env::consts::OS {
        "macos" => &platforms.macos,
        "windows" => &platforms.windows,
        "linux" => &platforms.linux,
        _ => return fallback(),
    };
    let Some(artifact) = pick_arch(by_os) else {
        return fallback();
    };
    if artifact.url.is_empty() {
        return fallback();
    }
    // Marks the hit as coming from an existing install. The banner opens
    // this in the user's browser, so the updater's own User-Agent is not
    // what fetches the file — without the marker the request is
    // indistinguishable from a first-time download off the website.
    // nginx serves the file regardless of the query string.
    (
        format!("{}?src=updater", artifact.url),
        artifact.sha256.clone(),
    )
}

/// The entry matching this build's architecture.
///
/// Keys are matched loosely because the publisher names them by target triple
/// (`aarch64-apple-darwin`) as often as by bare arch (`aarch64`), and `arm64`
/// is the same machine as `aarch64`. A single-entry map is taken as-is: a
/// publisher who ships one build for an OS means that build.
fn pick_arch(by_os: &HashMap<String, Artifact>) -> Option<&Artifact> {
    let aliases: &[&str] = match std::env::consts::ARCH {
        "aarch64" => &["aarch64", "arm64"],
        "x86_64" => &["x86_64", "amd64", "x64"],
        other => return by_os.get(other).or_else(|| single(by_os)),
    };
    for alias in aliases {
        if let Some(a) = by_os.get(*alias) {
            return Some(a);
        }
    }
    for alias in aliases {
        if let Some((_, a)) = by_os.iter().find(|(k, _)| k.contains(alias)) {
            return Some(a);
        }
    }
    single(by_os)
}

/// The only artifact, when there is exactly one — never an arbitrary pick
/// from several.
fn single(by_os: &HashMap<String, Artifact>) -> Option<&Artifact> {
    (by_os.len() == 1).then(|| by_os.values().next())?
}

fn is_newer(a: &str, b: &str) -> bool {
    let parse = |s: &str| -> Option<(u32, u32, u32)> {
        let mut parts = s.trim_start_matches('v').split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        let patch = parts.next()?.split(['-', '+']).next()?.parse().ok()?;
        Some((major, minor, patch))
    };
    match (parse(a), parse(b)) {
        (Some(av), Some(bv)) => av > bv,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artifact(url: &str) -> Artifact {
        Artifact {
            url: url.to_string(),
            sha256: "abc123".to_string(),
        }
    }

    fn map(entries: &[(&str, &str)]) -> HashMap<String, Artifact> {
        entries
            .iter()
            .map(|(k, u)| (k.to_string(), artifact(u)))
            .collect()
    }

    /// The bug this replaced: with two builds in the map, `values().next()`
    /// returned whichever the randomised hash order yielded, so an Apple
    /// Silicon user was offered an Intel binary about half the time — and
    /// differently on each launch.
    #[test]
    fn the_build_for_this_cpu_is_chosen_not_an_arbitrary_one() {
        let both = map(&[
            ("aarch64", "https://x.test/arm.dmg"),
            ("x86_64", "https://x.test/intel.dmg"),
        ]);
        let expected = match std::env::consts::ARCH {
            "aarch64" => "https://x.test/arm.dmg",
            "x86_64" => "https://x.test/intel.dmg",
            _ => return,
        };
        // Repeated because the failure mode was intermittent by construction.
        for _ in 0..50 {
            assert_eq!(pick_arch(&both).map(|a| a.url.as_str()), Some(expected));
        }
    }

    #[test]
    fn a_target_triple_key_still_matches() {
        let by_triple = map(&[
            ("aarch64-apple-darwin", "https://x.test/arm.dmg"),
            ("x86_64-apple-darwin", "https://x.test/intel.dmg"),
        ]);
        let expected = match std::env::consts::ARCH {
            "aarch64" => "https://x.test/arm.dmg",
            "x86_64" => "https://x.test/intel.dmg",
            _ => return,
        };
        assert_eq!(
            pick_arch(&by_triple).map(|a| a.url.as_str()),
            Some(expected)
        );
    }

    /// `arm64` and `aarch64` name the same machine.
    #[cfg(target_arch = "aarch64")]
    #[test]
    fn arm64_is_accepted_as_a_spelling_of_aarch64() {
        let m = map(&[("arm64", "https://x.test/arm.dmg")]);
        assert_eq!(
            pick_arch(&m).map(|a| a.url.as_str()),
            Some("https://x.test/arm.dmg")
        );
    }

    /// One published build for an OS is unambiguous, whatever it is keyed by.
    #[test]
    fn a_lone_build_is_offered_even_under_an_unrecognised_key() {
        let m = map(&[("universal", "https://x.test/universal.dmg")]);
        assert_eq!(
            pick_arch(&m).map(|a| a.url.as_str()),
            Some("https://x.test/universal.dmg")
        );
    }

    /// Several builds, none of which match: send the user to pick by hand
    /// rather than guess. Guessing is what this replaced.
    #[test]
    fn several_non_matching_builds_yield_nothing_rather_than_a_guess() {
        let m = map(&[
            ("mips", "https://x.test/a"),
            ("sparc", "https://x.test/b"),
            ("riscv64", "https://x.test/c"),
        ]);
        assert!(pick_arch(&m).is_none());
    }

    #[test]
    fn an_unknown_os_falls_back_to_the_landing_page() {
        let platforms = Platforms {
            macos: HashMap::new(),
            windows: HashMap::new(),
            linux: HashMap::new(),
        };
        let (url, sha) = platform_artifact(&platforms);
        assert_eq!(url, RELEASES_URL);
        assert!(sha.is_empty());
    }

    #[test]
    fn version_comparison_ignores_prerelease_suffixes() {
        assert!(is_newer("0.3.31", "0.3.30"));
        assert!(is_newer("v0.4.0", "0.3.30"));
        assert!(!is_newer("0.3.30", "0.3.30"));
        assert!(!is_newer("0.3.29", "0.3.30"));
        assert!(!is_newer("garbage", "0.3.30"));
        assert!(is_newer("0.3.31-rc1", "0.3.30"));
    }
}
