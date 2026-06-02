use crate::services::azure;
use crate::services::chain::{ChainDetail, StepDetail};
use std::collections::HashSet;
use std::path::PathBuf;

/// Discover chains by fetching workflow definitions from Azure.
/// The final chain graph is cached to disk so subsequent launches are instant.
/// Call `clear_cache` (already wired to the Refresh button) to force a re-fetch.
pub fn discover_chains_remote(
    sub: &str,
    rg: &str,
    app: &str,
) -> Result<Vec<ChainDetail>, String> {
    // Return the cached chain graph immediately if available.
    if let Some(cached) = load_chains_result(sub, app) {
        return Ok(cached);
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
    let mut fetch_errors: Vec<String> = Vec::new();

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
        let parsed = match azure::get_workflow_definition(sub, rg, app, name) {
            Ok(def) => {
                save_cached_definition(&cache_dir, name, &def);
                ais_chain::parser::parse_workflow_json(name, &def)
            }
            Err(_) => None,
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
                fetch_errors.push(format!("{name}: no definition and no trigger name pattern"));
            }
        }
    }

    if workflows.is_empty() {
        return Err(format!(
            "Fetched {} workflow(s) but none could be parsed.\nErrors:\n{}",
            deployed.len(),
            fetch_errors.join("\n")
        ));
    }

    // Resolve @appsetting('VAR') references so queue names match across workflows
    let mut resolved_workflows = workflows;
    for wf in &mut resolved_workflows {
        for link in wf.triggers.iter_mut().chain(wf.sends.iter_mut()) {
            link.target = resolve_appsetting(&link.target, &app_settings);
        }
    }

    let graph = ais_chain::graph::build(&resolved_workflows, &[]);
    let raw_chains = graph.find_chains();

    // If no multi-step chains, surface a diagnostic instead of silently returning empty
    let multi_step: Vec<_> = raw_chains.iter().filter(|c| c.steps.len() > 1).collect();
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
    Ok(chains)
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

/// Invalidate the cache for a specific app, forcing a fresh fetch next time.
pub fn clear_cache(sub: &str, app: &str) {
    let dir = cache_path(sub, app);
    let _ = std::fs::remove_dir_all(&dir);
}

fn cache_path(sub: &str, app: &str) -> PathBuf {
    let base = dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("ais-monitor")
        .join(format!("{sub}_{app}"));
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

    #[test]
    fn resolve_appsetting_plain_string_untouched() {
        let settings = std::collections::HashMap::new();
        assert_eq!(resolve_appsetting("my-literal-queue", &settings), "my-literal-queue");
    }
}
