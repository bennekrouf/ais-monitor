use crate::services::azure;
use crate::services::chain::{ChainDetail, StepDetail};
use std::collections::HashSet;
use std::path::PathBuf;

/// Discover chains by fetching workflow definitions from Azure.
/// Results are cached locally for faster subsequent loads.
pub fn discover_chains_remote(
    sub: &str,
    rg: &str,
    app: &str,
) -> Result<Vec<ChainDetail>, String> {
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
        }

        // Fetch from Azure
        match azure::get_workflow_definition(sub, rg, app, name) {
            Ok(def) => {
                save_cached_definition(&cache_dir, name, &def);
                if let Some(wf) = ais_chain::parser::parse_workflow_json(name, &def) {
                    workflows.push(wf);
                } else {
                    fetch_errors.push(format!("{name}: could not parse definition"));
                }
            }
            Err(e) => {
                fetch_errors.push(format!("{name}: {e}"));
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

    let chains = multi_step
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
            }
        })
        .collect();

    Ok(chains)
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
