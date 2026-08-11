//! Shared per-chain probe: run history for every workflow step plus message
//! counts for every queue, rolled into a `ChainHealth`.
//!
//! Both the Chains tab's "Check all" button and the Home dashboard's
//! background poll go through here, so the two can't drift apart — a fix to
//! how health is computed applies to both by construction.

use std::collections::HashMap;

use crate::components::chain_detail::{ChainHealth, QueueStatus};
use crate::services::{azure, kpi};

pub struct ChainProbe {
    pub health: ChainHealth,
    /// Non-fatal per-workflow / per-queue failures. A probe returns whatever
    /// it managed to collect rather than failing wholesale, so one broken
    /// queue doesn't blank out an entire chain's KPIs.
    pub errors: Vec<String>,
    /// workflow → run history (newest first, as Azure returns it).
    pub runs: HashMap<String, Vec<azure::RunInfo>>,
    /// queue name → active / dead-letter counts.
    pub queues: HashMap<String, QueueStatus>,
}

/// Blocking — call inside `spawn_blocking`.
///
/// `sb_namespace` may be empty, in which case queue counts are skipped and
/// the dead-letter total reads zero.
pub fn probe_chain(
    sub: &str,
    rg: &str,
    app: &str,
    sb_namespace: &str,
    steps: &[String],
    queue_names: &[String],
    depth: u32,
) -> ChainProbe {
    let mut errors: Vec<String> = Vec::new();

    let mut runs: HashMap<String, Vec<azure::RunInfo>> = HashMap::new();
    for wf in steps {
        match azure::list_runs(sub, rg, app, wf, depth) {
            Ok(r) => { runs.insert(wf.clone(), r); }
            Err(e) => errors.push(format!("list_runs {wf}: {e}")),
        }
    }

    let mut dl_total: i64 = 0;
    let mut queues: HashMap<String, QueueStatus> = HashMap::new();
    if !sb_namespace.is_empty() {
        for q in queue_names {
            match azure::check_queue(sb_namespace, rg, q) {
                Ok(qi) => {
                    dl_total += qi.dead_letter;
                    queues.insert(q.clone(), QueueStatus {
                        active: qi.active,
                        dead_letter: qi.dead_letter,
                    });
                }
                Err(e) => errors.push(format!("check_queue {q}: {e}")),
            }
        }
    }

    let all_kpis: Vec<kpi::ChainKpi> = runs.values()
        .map(|r| kpi::compute_workflow_kpi(r))
        .collect();
    let total_runs: usize = all_kpis.iter().map(|k| k.total_runs).sum();
    let succeeded: usize = all_kpis.iter().map(|k| k.succeeded).sum();
    let rate = if total_runs > 0 {
        Some((succeeded as f64 / total_runs as f64) * 100.0)
    } else {
        None
    };
    let stuck = all_kpis.iter().map(|k| k.stuck_runs.len()).sum();
    let streak = all_kpis.iter().map(|k| k.failure_streak).max().unwrap_or(0);

    ChainProbe {
        health: ChainHealth {
            success_rate: rate,
            dead_letters: dl_total,
            stuck_count: stuck,
            failure_streak: streak,
        },
        errors,
        runs,
        queues,
    }
}
