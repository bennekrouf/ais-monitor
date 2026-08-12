//! Shared per-chain probe: run history for every workflow step plus message
//! counts for every queue, rolled into a `ChainHealth`.
//!
//! Both the Chains tab's "Check all" button and the Home dashboard's
//! background poll go through here, so the two can't drift apart — a fix to
//! how health is computed applies to both by construction.

use std::collections::HashMap;

use crate::components::chain_detail::{ChainHealth, QueueStatus};
use crate::services::{azure, kpi};

/// Why a probe stopped before checking everything it was asked to.
///
/// All variants share a shape: the failure is a property of the app, the
/// service, or the caller — never of the individual workflow — so every
/// remaining call would fail identically. They differ in the remedy, which is
/// why callers distinguish them: two resolve by waiting (at different rates),
/// one may need a human.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeHalt {
    /// Azure is refusing load (429, or connections reset mid-handshake).
    /// Retry later, more slowly.
    Throttled,
    /// Azure failed to serve the request (502/503/504). Nothing to fix on
    /// this side; it clears when the service recovers.
    Unavailable,
    /// Azure refused authorization. Usually a stale token rather than a
    /// missing role, but either way retrying at speed will not help.
    Unauthorized,
}

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
    /// Set when the probe gave up early. `None` means it ran to completion
    /// (with or without ordinary per-workflow errors).
    pub halt: Option<ProbeHalt>,
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
    // Whether the failure is load-related or permission-related, every
    // remaining call in this probe would fail the same way — each one costing
    // another `az` process and another round trip. Stopping at the first turns
    // a storm of near-identical errors (one per workflow, per sweep) into a
    // single actionable one.
    let mut halt: Option<ProbeHalt> = None;

    let mut runs: HashMap<String, Vec<azure::RunInfo>> = HashMap::new();
    for wf in steps {
        match azure::list_runs(sub, rg, app, wf, depth) {
            Ok(r) => { runs.insert(wf.clone(), r); }
            Err(e) => {
                halt = classify(&e);
                errors.push(format!("list_runs {wf}: {e}"));
                if halt.is_some() { break; }
            }
        }
    }

    let mut dl_total: i64 = 0;
    let mut queues: HashMap<String, QueueStatus> = HashMap::new();
    // Queue counts live on a different resource (Service Bus) with its own
    // permissions and its own availability, so a workflow-side auth denial or
    // Microsoft.Web outage says nothing about them — but there is no point
    // starting the pass while being throttled, since the throttle is
    // subscription-wide.
    if !sb_namespace.is_empty() && halt != Some(ProbeHalt::Throttled) {
        for q in queue_names {
            match azure::check_queue(sb_namespace, rg, q) {
                Ok(qi) => {
                    dl_total += qi.dead_letter;
                    queues.insert(q.clone(), QueueStatus {
                        active: qi.active,
                        dead_letter: qi.dead_letter,
                    });
                }
                Err(e) => {
                    let q_halt = classify(&e);
                    errors.push(format!("check_queue {q}: {e}"));
                    if q_halt.is_some() {
                        halt = halt.or(q_halt);
                        break;
                    }
                }
            }
        }
    }

    ChainProbe {
        health: roll_up(&runs, dl_total),
        errors,
        runs,
        queues,
        halt,
    }
}

/// Build a chain's `ChainProbe` from run/queue data already fetched.
///
/// Separated from fetching so a sweep can read each distinct workflow **once**
/// and fan the result out to every chain containing it. Chains overlap heavily
/// in practice — a 38-chain app here covers only 66 distinct workflows across
/// 177 chain slots — so probing chain-by-chain spends roughly two thirds of
/// its calls re-reading the same run history, against an endpoint that
/// throttles per subscription.
///
/// `errors` is left empty: with shared fetching, failures belong to the sweep
/// rather than to any one chain that happened to reference the workflow.
pub fn assemble(
    steps: &[String],
    queue_names: &[String],
    all_runs: &HashMap<String, Vec<azure::RunInfo>>,
    all_queues: &HashMap<String, QueueStatus>,
) -> ChainProbe {
    let runs: HashMap<String, Vec<azure::RunInfo>> = steps
        .iter()
        .filter_map(|s| all_runs.get(s).map(|r| (s.clone(), r.clone())))
        .collect();
    let queues: HashMap<String, QueueStatus> = queue_names
        .iter()
        .filter_map(|q| all_queues.get(q).map(|s| (q.clone(), s.clone())))
        .collect();
    let dl_total: i64 = queues.values().map(|q| q.dead_letter).sum();

    ChainProbe {
        health: roll_up(&runs, dl_total),
        errors: Vec::new(),
        runs,
        queues,
        halt: None,
    }
}

/// Aggregate per-workflow run history into the chain-level KPIs.
fn roll_up(runs: &HashMap<String, Vec<azure::RunInfo>>, dead_letters: i64) -> ChainHealth {
    let all_kpis: Vec<kpi::ChainKpi> = runs.values()
        .map(|r| kpi::compute_workflow_kpi(r))
        .collect();
    let total_runs: usize = all_kpis.iter().map(|k| k.total_runs).sum();
    let succeeded: usize = all_kpis.iter().map(|k| k.succeeded).sum();
    ChainHealth {
        success_rate: if total_runs > 0 {
            Some((succeeded as f64 / total_runs as f64) * 100.0)
        } else {
            None
        },
        dead_letters,
        stuck_count: all_kpis.iter().map(|k| k.stuck_runs.len()).sum(),
        failure_streak: all_kpis.iter().map(|k| k.failure_streak).max().unwrap_or(0),
    }
}

/// Map an `az` error string onto a halt reason, if it is one.
///
/// Order matters. Throttling is checked first because a throttled response can
/// mention "Forbidden" in passing, and reporting that as a permission problem
/// would send the user chasing a role assignment for a transient fault.
/// Service faults are checked before authorization for the same reason: a 502
/// body can carry incidental wording that the broader auth matcher would
/// otherwise claim.
pub fn classify(err: &str) -> Option<ProbeHalt> {
    if azure::is_throttling_error(err) {
        Some(ProbeHalt::Throttled)
    } else if azure::is_service_unavailable_error(err) {
        Some(ProbeHalt::Unavailable)
    } else if azure::is_authorization_error(err) {
        Some(ProbeHalt::Unauthorized)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const THROTTLE: &str = "('Connection aborted.', ConnectionResetError(54, 'Connection reset by peer'))";
    const DENIED: &str = r#"Forbidden({"error":{"code":"AuthorizationFailed","message":"does not have authorization"}})"#;

    #[test]
    fn throttling_wins_over_authorization() {
        // A throttled response can mention "Forbidden" incidentally. Calling
        // that a permission problem would send the user chasing a role
        // assignment for what is really a transient fault.
        assert_eq!(classify(THROTTLE), Some(ProbeHalt::Throttled));
        assert_eq!(
            classify("Too Many Requests - Forbidden"),
            Some(ProbeHalt::Throttled)
        );
    }

    #[test]
    fn azure_side_fault_is_unavailable() {
        assert_eq!(
            classify(r#"Bad Gateway({"error":{"code":"BadGatewayConnection","message":"The network connectivity issue encountered for 'Microsoft.Web'; cannot fulfill the request."}})"#),
            Some(ProbeHalt::Unavailable)
        );
        assert_eq!(classify("503 Service Unavailable"), Some(ProbeHalt::Unavailable));
    }

    #[test]
    fn rbac_denial_is_unauthorized() {
        assert_eq!(classify(DENIED), Some(ProbeHalt::Unauthorized));
    }

    #[test]
    fn ordinary_errors_do_not_halt_the_probe() {
        // WorkflowNotFound affects one workflow, not the whole app, so the
        // probe must carry on and report the rest.
        assert_eq!(classify(r#"Not Found({"code":"WorkflowNotFound"})"#), None);
        assert_eq!(classify("some transient parse failure"), None);
    }
}
