use crate::services::azure;
use crate::services::chain::{ChainDetail, StepDetail};
use std::collections::HashSet;
use std::path::PathBuf;

/// A workflow with no detected edge (queue producer/consumer, EventGrid, or
/// manual link) to any other workflow — invisible in the Chains view since a
/// 1-node "chain" carries no useful topology to show.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct UnlinkedWorkflow {
    pub name: String,
    /// Trigger summary (e.g. "queue:ais.ignite.kyriba.payment") so the user
    /// can see *what* it's waiting on without opening the workflow itself.
    pub trigger_info: String,
}

pub struct ChainDiscovery {
    pub chains: Vec<ChainDetail>,
    pub unlinked: Vec<UnlinkedWorkflow>,
}

/// Discover chains by fetching workflow definitions from Azure.
/// The final chain graph is cached to disk so subsequent launches are instant.
/// Call `clear_cache` (already wired to the Refresh button) to force a re-fetch.
pub fn discover_chains_remote(
    sub: &str,
    rg: &str,
    app: &str,
    local_dir: &str,
) -> Result<ChainDiscovery, String> {
    // Return the cached chain graph immediately if available.
    if let Some(cached) = load_chains_result(sub, app) {
        let unlinked = load_unlinked_result(sub, app).unwrap_or_default();
        return Ok(ChainDiscovery { chains: cached, unlinked });
    }

    let cache_dir = cache_path(sub, app);

    // Fetch deployed workflow list
    let deployed = azure::list_deployed_workflows(sub, rg, app)?;
    if deployed.is_empty() {
        return Err(format!(
            "No workflows found in Logic App '{app}'.\n\
             Check that the app is running and you have access."
        ));
    }

    // Fetch app settings to resolve @appsetting('VAR') references in queue names
    let app_settings = azure::get_app_settings(sub, rg, app).unwrap_or_default();

    let mut workflows = Vec::new();
    // (workflow name, why it produced no usable topology)
    let mut fetch_errors: Vec<(String, String)> = Vec::new();

    for wf_info in &deployed {
        let name = &wf_info.name;

        // Try cache first
        if let Some(cached) = load_cached_definition(&cache_dir, name) {
            if let Some(wf) = ais_chain::parser::parse_workflow_json(name, &cached) {
                workflows.push(wf);
                continue;
            }
            // Cached data is present but unparseable — fall through to re-fetch
        }

        // Fetch full definition from ARM
        let mut fetch_error: Option<String> = None;
        let parsed = match azure::get_workflow_definition(sub, rg, app, name) {
            Ok(def) => {
                save_cached_definition(&cache_dir, name, &def);
                ais_chain::parser::parse_workflow_json(name, &def)
            }
            Err(e) => {
                // Discarding this used to make a total failure unreadable: every
                // workflow reported "no definition" with no hint whether the
                // cause was auth, throttling, or a 404. Keep the az message.
                fetch_error = Some(e.trim().to_string());
                None
            }
        };

        if let Some(wf) = parsed {
            workflows.push(wf);
        } else {
            // Fallback: extract the trigger queue from the trigger name convention.
            // Azure names Service Bus triggers:
            //   "When_messages_are_available_in_{queue_name}_(peek-lock)"
            // This covers the ~36 workflows whose ARM definition endpoint returns
            // no parseable files but whose queue name is visible in the metadata.
            let fallback_triggers: Vec<ais_chain::parser::Link> = wf_info
                .trigger_names
                .iter()
                .filter_map(|t| parse_queue_from_trigger_name(t))
                .map(|q| ais_chain::parser::Link { kind: "queue".into(), target: q })
                .collect();

            if !fallback_triggers.is_empty() {
                workflows.push(ais_chain::parser::Workflow {
                    name: name.clone(),
                    triggers: fallback_triggers,
                    sends: Vec::new(),
                    calls: Vec::new(),
                });
            } else {
                let reason = fetch_error
                    .unwrap_or_else(|| "definition fetched but no trigger/queue found".to_string());
                fetch_errors.push((name.clone(), reason));
            }
        }
    }

    if workflows.is_empty() {
        return Err(format!(
            "Fetched {} workflow(s) but none could be parsed.\n{}",
            deployed.len(),
            summarize_fetch_errors(&fetch_errors)
        ));
    }

    // Resolve @appsetting('VAR') references so queue names match across workflows
    let mut resolved_workflows = workflows;
    for wf in &mut resolved_workflows {
        for link in wf.triggers.iter_mut().chain(wf.sends.iter_mut()) {
            link.target = resolve_appsetting(&link.target, &app_settings);
        }
    }

    // Load manual links (EventGrid, dynamic queue routing not visible in the
    // deployed workflow JSON). ~/.ais/chains/<project-key>.txt via
    // ais_chain::links::load, keyed by local_dir when the user has a
    // workspace, else by the remote sub/app identity so remote-only installs
    // get their own file. A legacy repo .ais-chain is honored read-only and
    // migrated to ~/.ais/chains/ by the shared loader. No topology ships in
    // the binary — an unconfigured tenant gets an empty link set rather than
    // another customer's routing.
    let links_key: std::path::PathBuf = if local_dir.is_empty() {
        std::path::PathBuf::from(format!("/remote/{sub}/{app}"))
    } else {
        let base = std::path::Path::new(local_dir);
        if base.join("logic_apps").exists() { base.join("logic_apps") }
        else if base.join("logic-apps").exists() { base.join("logic-apps") }
        else { base.to_path_buf() }
    };
    let loaded = ais_chain::links::load(&links_key);
    for w in &loaded.warnings {
        eprintln!("[ais-chain links] {w}");
    }

    // Drop manual links pointing at workflows this app doesn't actually have.
    // `graph::build` adds an edge for both endpoints unconditionally, so a
    // stale link (renamed workflow, typo, or a links file carried over from a
    // different tenant) injects a phantom node that the pollers then try to
    // `list_runs` — surfacing as a stream of WorkflowNotFound errors with no
    // hint as to where the name came from. Reporting and skipping beats
    // polling something that cannot exist.
    let known_names: Vec<&str> = resolved_workflows.iter().map(|w| w.name.as_str()).collect();
    for w in ais_chain::links::validate(&loaded.links, &known_names) {
        eprintln!("[ais-chain links] {w}");
    }
    let known: HashSet<&str> = known_names.iter().copied().collect();
    let (manual_links, stale): (Vec<String>, Vec<String>) = loaded.links
        .into_iter()
        .partition(|l| link_endpoints_known(l, &known));
    if !stale.is_empty() {
        crate::services::activity::warn(
            "Stale chain links skipped",
            format!("{} link(s) reference unknown or malformed workflows", stale.len()),
            format!(
                "These links in {} do not name workflows deployed in {app}, so they were \
                 ignored rather than polled:\n{}",
                ais_chain::links::links_path(&links_key).display(),
                stale.join("\n"),
            ),
        );
    }

    let graph = ais_chain::graph::build(&resolved_workflows, &manual_links);
    let raw_chains = graph.find_chains();

    // If no multi-step chains, surface a diagnostic instead of silently returning empty
    let multi_step: Vec<_> = raw_chains.iter().filter(|c| c.steps.len() > 1).collect();

    // Single-node chains are workflows with no detected edge in or out — they
    // used to just vanish from the UI. Surface them instead so a missing
    // manual link (or a genuinely standalone utility workflow) is visible
    // rather than silently dropped.
    let unlinked: Vec<UnlinkedWorkflow> = raw_chains.iter()
        .filter(|c| c.steps.len() == 1)
        .map(|c| {
            let name = c.steps[0].workflow.clone();
            let trigger_info = resolved_workflows.iter()
                .find(|w| w.name == name)
                .map(|w| {
                    w.triggers.iter()
                        .map(|t| format!("{}:{}", t.kind, t.target))
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            UnlinkedWorkflow { name, trigger_info }
        })
        .collect();
    if multi_step.is_empty() && !raw_chains.is_empty() {
        // There are workflows but no connections detected — useful diagnostic
        let wf_names: Vec<_> = resolved_workflows.iter().map(|w| {
            let triggers: Vec<_> = w.triggers.iter().map(|t| format!("{}:{}", t.kind, t.target)).collect();
            let sends: Vec<_> = w.sends.iter().map(|s| format!("{}:{}", s.kind, s.target)).collect();
            format!("  {} → triggers:[{}] sends:[{}]", w.name, triggers.join(","), sends.join(","))
        }).collect();
        return Err(format!(
            "{} workflow(s) parsed but no chains found (no matching queue sender→receiver).\n\
             Workflow graph:\n{}\n\
             Tip: queue names using @appsetting('VAR') must resolve to the same value on sender and receiver.",
            resolved_workflows.len(),
            wf_names.join("\n")
        ));
    }

    let chains: Vec<ChainDetail> = multi_step
        .iter()
        .map(|c| {
            let mut queues = HashSet::new();
            let steps: Vec<StepDetail> = c.steps.iter().map(|s| {
                if s.link_type.starts_with("queue:") {
                    queues.insert(s.link_type.strip_prefix("queue:").unwrap().to_string());
                }
                let trigger_info = resolved_workflows.iter()
                    .find(|w| w.name == s.workflow)
                    .map(|w| {
                        w.triggers.iter()
                            .map(|t| format!("{}:{}", t.kind, t.target))
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .unwrap_or_default();

                StepDetail {
                    workflow: s.workflow.clone(),
                    link_type: s.link_type.clone(),
                    trigger_info,
                }
            }).collect();

            let mut queue_list: Vec<String> = queues.into_iter().collect();
            queue_list.sort();

            ChainDetail {
                label: c.steps[0].workflow.clone(),
                steps,
                queues: queue_list,
                parallel_entries: c.parallel_entries.clone(),
            }
        })
        .collect();

    save_chains_result(sub, app, &chains);
    save_unlinked_result(sub, app, &unlinked);
    Ok(ChainDiscovery { chains, unlinked })
}

/// Collapse per-workflow failures into one line per distinct cause.
///
/// A whole-app failure means the same `az` error repeated ~100 times; printing
/// it once per workflow buried the actual message (auth, throttling, 404) in a
/// wall of names. Group by reason, keep a couple of names as examples.
fn summarize_fetch_errors(errors: &[(String, String)]) -> String {
    let mut groups: Vec<(String, Vec<String>)> = Vec::new();
    for (name, reason) in errors {
        // First line only — az dumps multi-line tracebacks for some failures.
        let key = reason.lines().next().unwrap_or(reason).trim().to_string();
        match groups.iter_mut().find(|(r, _)| *r == key) {
            Some((_, names)) => names.push(name.clone()),
            None => groups.push((key, vec![name.clone()])),
        }
    }

    let mut out = String::from("Errors:");
    for (reason, names) in &groups {
        let examples = names.iter().take(3).cloned().collect::<Vec<_>>().join(", ");
        let more = names.len().saturating_sub(3);
        out.push_str(&format!(
            "\n  {} workflow(s): {reason}\n    e.g. {examples}{}",
            names.len(),
            if more > 0 { format!(", +{more} more") } else { String::new() },
        ));
    }
    out
}

/// Extract the Service Bus queue name encoded in an Azure trigger name.
///
/// Azure auto-names peek-lock triggers using the pattern:
///   `When_messages_are_available_in_{queue_name}_(peek-lock)`
/// or without the suffix:
///   `When_messages_are_available_in_{queue_name}`
///
/// Returns `None` for triggers that don't follow this pattern (HTTP, Recurrence, …).
fn parse_queue_from_trigger_name(trigger_name: &str) -> Option<String> {
    const PREFIX: &str = "When_messages_are_available_in_";
    let rest = trigger_name.strip_prefix(PREFIX)?;
    let queue = rest
        .strip_suffix("_(peek-lock)")
        .or_else(|| rest.strip_suffix("_(receive-and-delete)"))
        .unwrap_or(rest);
    if queue.is_empty() { None } else { Some(queue.to_string()) }
}

/// Resolve `@appsetting('VAR')` and `@appsetting(VAR)` references using
/// the app's published configuration. Returns the original string unchanged
/// if the pattern is not found or the setting key is missing.
fn resolve_appsetting(s: &str, settings: &std::collections::HashMap<String, String>) -> String {
    // Match @{appsetting('KEY')} or @appsetting('KEY') or @appsetting(KEY)
    let patterns: &[(&str, &str, &str)] = &[
        ("@{appsetting('", "')}", ""),
        ("@appsetting('",  "')", ""),
        ("@{appsetting(\"","\")}",""),
        ("@appsetting(\"", "\")", ""),
    ];
    for (prefix, suffix, _) in patterns {
        if s.starts_with(prefix) && s.ends_with(suffix) {
            let key = &s[prefix.len()..s.len() - suffix.len()];
            if let Some(val) = settings.get(key) {
                return val.clone();
            }
        }
    }
    s.to_string()
}

/// True when both endpoints of a `Source->Target:label` manual link name a
/// workflow that actually exists. Parsing mirrors `ais_chain::links::validate`
/// (and `graph::parse_manual_link`, which is private) so a link is judged here
/// exactly as the graph builder would read it. Malformed links count as not
/// known — `graph::build` ignores them anyway, and surfacing them beats
/// leaving a typo silently inert.
fn link_endpoints_known(link: &str, known: &HashSet<&str>) -> bool {
    let Some((from, rest)) = link.split_once("->") else { return false };
    let to = rest.split(':').next().unwrap_or(rest).trim();
    known.contains(from.trim()) && known.contains(to)
}

fn chains_result_path(sub: &str, app: &str) -> PathBuf {
    cache_path(sub, app).join("_chains.json")
}

fn load_chains_result(sub: &str, app: &str) -> Option<Vec<ChainDetail>> {
    let path = chains_result_path(sub, app);
    let content = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}

fn save_chains_result(sub: &str, app: &str, chains: &[ChainDetail]) {
    let path = chains_result_path(sub, app);
    if let Ok(json) = serde_json::to_string(chains) {
        let _ = std::fs::write(path, json);
    }
}

fn unlinked_result_path(sub: &str, app: &str) -> PathBuf {
    cache_path(sub, app).join("_unlinked.json")
}

fn load_unlinked_result(sub: &str, app: &str) -> Option<Vec<UnlinkedWorkflow>> {
    let path = unlinked_result_path(sub, app);
    let content = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}

fn save_unlinked_result(sub: &str, app: &str, unlinked: &[UnlinkedWorkflow]) {
    let path = unlinked_result_path(sub, app);
    if let Ok(json) = serde_json::to_string(unlinked) {
        let _ = std::fs::write(path, json);
    }
}

/// Invalidate the cache for a specific app, forcing a fresh fetch next time.
pub fn clear_cache(sub: &str, app: &str) {
    let dir = cache_path(sub, app);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Invalidate just the computed chain graph (`_chains.json` / `_unlinked.json`),
/// leaving the per-workflow definition cache untouched. `discover_chains_remote`
/// then rebuilds the graph from already-fetched workflow definitions plus a
/// fresh read of the manual-links file, skipping the expensive per-workflow
/// re-fetch (still makes the two cheap list/app-settings calls). Use this
/// after editing `~/.ais/chains/*.txt`; use `clear_cache` when the workflows
/// themselves changed in Azure and need re-fetching.
pub fn recompute_chains(sub: &str, app: &str) {
    let _ = std::fs::remove_file(chains_result_path(sub, app));
    let _ = std::fs::remove_file(unlinked_result_path(sub, app));
}

fn cache_path(sub: &str, app: &str) -> PathBuf {
    // `AIS_MONITOR_HOME` lets locked-down Windows Server / corporate
    // environments override the default cache root — useful when the OS
    // default (`%LOCALAPPDATA%`) is jailed or roaming. Additive: when unset,
    // behavior is identical to the previous build.
    let root = std::env::var_os("AIS_MONITOR_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::cache_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("ais-monitor")
        });
    let base = root.join(format!("{sub}_{app}"));
    let _ = std::fs::create_dir_all(&base);
    base
}

fn load_cached_definition(cache_dir: &PathBuf, workflow: &str) -> Option<serde_json::Value> {
    let path = cache_dir.join(format!("{workflow}.json"));
    if !path.exists() {
        return None;
    }
    // Expire cache after 1 hour
    if let Ok(meta) = path.metadata() {
        if let Ok(modified) = meta.modified() {
            if modified.elapsed().unwrap_or_default() > std::time::Duration::from_secs(3600) {
                return None;
            }
        }
    }
    let content = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}

fn save_cached_definition(cache_dir: &PathBuf, workflow: &str, def: &serde_json::Value) {
    let path = cache_dir.join(format!("{workflow}.json"));
    if let Ok(json) = serde_json::to_string(def) {
        let _ = std::fs::write(path, json);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_queue_from_trigger_name ─────────────────────────────────────

    #[test]
    fn parse_queue_peek_lock() {
        assert_eq!(
            parse_queue_from_trigger_name("When_messages_are_available_in_my-queue_(peek-lock)"),
            Some("my-queue".to_string())
        );
    }

    #[test]
    fn parse_queue_receive_and_delete() {
        assert_eq!(
            parse_queue_from_trigger_name("When_messages_are_available_in_my-queue_(receive-and-delete)"),
            Some("my-queue".to_string())
        );
    }

    #[test]
    fn parse_queue_no_suffix() {
        assert_eq!(
            parse_queue_from_trigger_name("When_messages_are_available_in_my-queue"),
            Some("my-queue".to_string())
        );
    }

    #[test]
    fn parse_queue_http_trigger_returns_none() {
        assert_eq!(parse_queue_from_trigger_name("manual"), None);
        assert_eq!(parse_queue_from_trigger_name("HTTPTrigger"), None);
        assert_eq!(parse_queue_from_trigger_name(""), None);
    }

    #[test]
    fn parse_queue_empty_name_after_prefix_returns_none() {
        assert_eq!(
            parse_queue_from_trigger_name("When_messages_are_available_in_"),
            None
        );
    }

    // ── summarize_fetch_errors ────────────────────────────────────────────

    #[test]
    fn identical_reasons_collapse_into_one_group() {
        let errs: Vec<(String, String)> = ["A", "B", "C", "D"]
            .iter()
            .map(|n| (n.to_string(), "ERROR: AuthorizationFailed".to_string()))
            .collect();
        let out = summarize_fetch_errors(&errs);
        assert!(out.contains("4 workflow(s): ERROR: AuthorizationFailed"), "{out}");
        assert!(out.contains("e.g. A, B, C, +1 more"), "{out}");
        // The reason must appear once, not once per workflow.
        assert_eq!(out.matches("AuthorizationFailed").count(), 1, "{out}");
    }

    #[test]
    fn distinct_reasons_are_listed_separately() {
        let errs = vec![
            ("A".to_string(), "AuthorizationFailed".to_string()),
            ("B".to_string(), "NotFound\nstack trace line".to_string()),
        ];
        let out = summarize_fetch_errors(&errs);
        assert!(out.contains("1 workflow(s): AuthorizationFailed"), "{out}");
        assert!(out.contains("1 workflow(s): NotFound"), "{out}");
        // Multi-line az output is trimmed to its first line.
        assert!(!out.contains("stack trace line"), "{out}");
    }

    // ── resolve_appsetting ────────────────────────────────────────────────

    #[test]
    fn resolve_appsetting_single_quotes() {
        let mut settings = std::collections::HashMap::new();
        settings.insert("MY_QUEUE".to_string(), "orders-queue".to_string());
        assert_eq!(resolve_appsetting("@appsetting('MY_QUEUE')", &settings), "orders-queue");
    }

    #[test]
    fn resolve_appsetting_with_braces() {
        let mut settings = std::collections::HashMap::new();
        settings.insert("MY_QUEUE".to_string(), "orders-queue".to_string());
        assert_eq!(resolve_appsetting("@{appsetting('MY_QUEUE')}", &settings), "orders-queue");
    }

    #[test]
    fn resolve_appsetting_missing_key_returns_original() {
        let settings = std::collections::HashMap::new();
        assert_eq!(
            resolve_appsetting("@appsetting('MISSING')", &settings),
            "@appsetting('MISSING')"
        );
    }

    // ── link_endpoints_known ──────────────────────────────────────────────

    #[test]
    fn link_with_both_endpoints_deployed_is_kept() {
        let known: HashSet<&str> = ["A", "B"].into_iter().collect();
        assert!(link_endpoints_known("A->B:EventGrid", &known));
        assert!(link_endpoints_known("A->B:queue:ais.some.queue", &known));
        // Surrounding whitespace must not change the verdict.
        assert!(link_endpoints_known(" A -> B :EventGrid", &known));
    }

    #[test]
    fn link_naming_an_undeployed_workflow_is_dropped() {
        let known: HashSet<&str> = ["A", "B"].into_iter().collect();
        // This is the real-world case: a links file carried over from another
        // tenant naming workflows this app never had.
        assert!(!link_endpoints_known("Verify-Ignite-Invoice->Pivot-Ignite-Invoice:queue:x", &known));
        assert!(!link_endpoints_known("A->Missing:EventGrid", &known));
        assert!(!link_endpoints_known("Missing->B:EventGrid", &known));
    }

    #[test]
    fn malformed_link_is_dropped() {
        let known: HashSet<&str> = ["A", "B"].into_iter().collect();
        assert!(!link_endpoints_known("junk", &known));
        assert!(!link_endpoints_known("", &known));
    }

    #[test]
    fn resolve_appsetting_plain_string_untouched() {
        let settings = std::collections::HashMap::new();
        assert_eq!(resolve_appsetting("my-literal-queue", &settings), "my-literal-queue");
    }
}
