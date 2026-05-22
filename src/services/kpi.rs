use crate::services::azure::RunInfo;
use chrono::{DateTime, Utc};

#[derive(Clone, Debug, PartialEq, Default)]
pub struct ChainKpi {
    pub total_runs: usize,
    pub succeeded: usize,
    pub failed: usize,
    pub success_rate: f64,
    pub avg_duration_secs: Option<f64>,
    pub p95_duration_secs: Option<f64>,
    pub failure_streak: usize,
    pub last_success_ago: Option<String>,
}

pub fn compute_workflow_kpi(runs: &[RunInfo]) -> ChainKpi {
    if runs.is_empty() {
        return ChainKpi::default();
    }

    let total = runs.len();
    let succeeded = runs.iter().filter(|r| r.status == "Succeeded").count();
    let failed = runs.iter().filter(|r| r.status == "Failed").count();
    let rate = if total > 0 { (succeeded as f64 / total as f64) * 100.0 } else { 0.0 };

    let mut durations: Vec<f64> = runs.iter().filter_map(|r| {
        let start = DateTime::parse_from_rfc3339(&r.start).ok()?;
        let end = DateTime::parse_from_rfc3339(r.end.as_ref()?).ok()?;
        Some((end - start).num_milliseconds() as f64 / 1000.0)
    }).collect();

    let avg = if durations.is_empty() { None } else {
        Some(durations.iter().sum::<f64>() / durations.len() as f64)
    };

    let p95 = if durations.is_empty() { None } else {
        durations.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let idx = ((durations.len() as f64 * 0.95).ceil() as usize).min(durations.len()) - 1;
        Some(durations[idx])
    };

    // Failure streak: count consecutive failures from most recent
    let streak = runs.iter().take_while(|r| r.status == "Failed").count();

    let last_success_ago = runs.iter()
        .find(|r| r.status == "Succeeded")
        .and_then(|r| DateTime::parse_from_rfc3339(&r.start).ok())
        .map(|dt| format_ago(dt.with_timezone(&Utc)));

    ChainKpi {
        total_runs: total,
        succeeded,
        failed,
        success_rate: rate,
        avg_duration_secs: avg,
        p95_duration_secs: p95,
        failure_streak: streak,
        last_success_ago,
    }
}

fn format_ago(dt: DateTime<Utc>) -> String {
    let now = Utc::now();
    let diff = now - dt;
    let mins = diff.num_minutes();
    if mins < 1 { return "just now".into(); }
    if mins < 60 { return format!("{mins}m ago"); }
    let hours = diff.num_hours();
    if hours < 24 { return format!("{hours}h ago"); }
    let days = diff.num_days();
    format!("{days}d ago")
}
