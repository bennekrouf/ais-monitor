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
        return Ok(Vec::new());
    }

    let mut workflows = Vec::new();

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
                }
            }
            Err(e) => {
                eprintln!("Failed to fetch definition for {name}: {e}");
            }
        }
    }

    if workflows.is_empty() {
        return Ok(Vec::new());
    }

    let graph = ais_chain::graph::build(&workflows, &[]);
    let raw_chains = graph.find_chains();

    let chains = raw_chains
        .iter()
        .filter(|c| c.steps.len() > 1)
        .map(|c| {
            let mut queues = HashSet::new();
            let steps: Vec<StepDetail> = c.steps.iter().map(|s| {
                if s.link_type.starts_with("queue:") {
                    queues.insert(s.link_type.strip_prefix("queue:").unwrap().to_string());
                }
                let trigger_info = workflows.iter()
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
