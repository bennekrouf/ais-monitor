use std::collections::HashSet;
use std::path::Path;

/// A chain with its steps and the queues involved
#[derive(Clone, Debug, PartialEq)]
pub struct ChainDetail {
    pub label: String,
    pub steps: Vec<StepDetail>,
    pub queues: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StepDetail {
    pub workflow: String,
    pub link_type: String, // empty for root
    pub trigger_info: String,
}

/// Discover all chains from a logic_apps directory (blocking)
pub fn discover_chains(logic_apps_dir: &str) -> Vec<ChainDetail> {
    let base = Path::new(logic_apps_dir);
    let path = if base.join("logic_apps").exists() {
        base.join("logic_apps")
    } else if base.join("logic-apps").exists() {
        base.join("logic-apps")
    } else {
        base.to_path_buf()
    };

    let workflows = ais_chain::parser::parse_all(&path);
    if workflows.is_empty() {
        return Vec::new();
    }

    // Load manual links
    let mut manual_links = Vec::new();
    let auto_links_path = path.join(".ais-chain");
    if auto_links_path.exists() {
        if let Ok(content) = std::fs::read_to_string(&auto_links_path) {
            for line in content.lines() {
                let trimmed = line.trim();
                if !trimmed.is_empty() && !trimmed.starts_with('#') {
                    manual_links.push(trimmed.to_string());
                }
            }
        }
    }

    let graph = ais_chain::graph::build(&workflows, &manual_links);
    let raw_chains = graph.find_chains();

    raw_chains
        .iter()
        .filter(|c| c.steps.len() > 1)
        .map(|c| {
            let mut queues = HashSet::new();
            let steps: Vec<StepDetail> = c.steps.iter().map(|s| {
                // Extract queue names from link types
                if s.link_type.starts_with("queue:") {
                    queues.insert(s.link_type.strip_prefix("queue:").unwrap().to_string());
                }
                // Find trigger info
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
        .collect()
}
