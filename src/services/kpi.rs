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
    pub stuck_runs: Vec<StuckRun>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StuckRun {
    pub run_id: String,
    pub elapsed_secs: f64,
    pub p95_secs: f64,
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

    let stuck_runs = if let Some(p95_val) = p95 {
        let now = Utc::now();
        runs.iter()
            .filter(|r| r.status == "Running")
            .filter_map(|r| {
                let start = DateTime::parse_from_rfc3339(&r.start).ok()?;
                let elapsed = (now - start.with_timezone(&Utc)).num_milliseconds() as f64 / 1000.0;
                if elapsed > p95_val {
                    Some(StuckRun { run_id: r.id.clone(), elapsed_secs: elapsed, p95_secs: p95_val })
                } else {
                    None
                }
            })
            .collect()
    } else {
        Vec::new()
    };

    ChainKpi {
        total_runs: total,
        succeeded,
        failed,
        success_rate: rate,
        avg_duration_secs: avg,
        p95_duration_secs: p95,
        failure_streak: streak,
        last_success_ago,
        stuck_runs,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn run(id: &str, status: &str, start: &str, end: Option<&str>) -> RunInfo {
        RunInfo {
            id: id.to_string(),
            status: status.to_string(),
            start: start.to_string(),
            end: end.map(|s| s.to_string()),
        }
    }

    #[test]
    fn empty_runs_returns_default() {
        let kpi = compute_workflow_kpi(&[]);
        assert_eq!(kpi.total_runs, 0);
        assert_eq!(kpi.success_rate, 0.0);
        assert_eq!(kpi.failure_streak, 0);
        assert!(kpi.avg_duration_secs.is_none());
    }

    #[test]
    fn all_succeeded_full_rate_no_streak() {
        let runs = vec![
            run("1", "Succeeded", "2026-01-01T10:00:00Z", Some("2026-01-01T10:00:10Z")),
            run("2", "Succeeded", "2026-01-01T10:01:00Z", Some("2026-01-01T10:01:05Z")),
        ];
        let kpi = compute_workflow_kpi(&runs);
        assert_eq!(kpi.total_runs, 2);
        assert_eq!(kpi.succeeded, 2);
        assert_eq!(kpi.failed, 0);
        assert_eq!(kpi.success_rate, 100.0);
        assert_eq!(kpi.failure_streak, 0);
    }

    #[test]
    fn mixed_runs_correct_rate() {
        let runs = vec![
            run("1", "Failed",    "2026-01-01T10:00:00Z", Some("2026-01-01T10:00:02Z")),
            run("2", "Failed",    "2026-01-01T09:00:00Z", Some("2026-01-01T09:00:02Z")),
            run("3", "Succeeded", "2026-01-01T08:00:00Z", Some("2026-01-01T08:00:10Z")),
            run("4", "Succeeded", "2026-01-01T07:00:00Z", Some("2026-01-01T07:00:10Z")),
        ];
        let kpi = compute_workflow_kpi(&runs);
        assert_eq!(kpi.total_runs, 4);
        assert_eq!(kpi.succeeded, 2);
        assert_eq!(kpi.failed, 2);
        assert!((kpi.success_rate - 50.0).abs() < 0.01);
        // Two leading failures → streak of 2
        assert_eq!(kpi.failure_streak, 2);
    }

    #[test]
    fn duration_avg_and_p95_computed() {
        // 4 runs: 2s, 4s, 6s, 100s
        let runs = vec![
            run("1", "Succeeded", "2026-01-01T10:00:00Z", Some("2026-01-01T10:00:02Z")),
            run("2", "Succeeded", "2026-01-01T10:01:00Z", Some("2026-01-01T10:01:04Z")),
            run("3", "Succeeded", "2026-01-01T10:02:00Z", Some("2026-01-01T10:02:06Z")),
            run("4", "Succeeded", "2026-01-01T10:03:00Z", Some("2026-01-01T10:04:40Z")),
        ];
        let kpi = compute_workflow_kpi(&runs);
        let avg = kpi.avg_duration_secs.unwrap();
        assert!((avg - 28.0).abs() < 0.1, "avg={avg}");
        // sorted: [2,4,6,100], p95 idx = ceil(4*0.95)-1 = ceil(3.8)-1 = 4-1 = 3 → 100s
        assert!((kpi.p95_duration_secs.unwrap() - 100.0).abs() < 0.1);
    }

    #[test]
    fn no_end_time_excluded_from_durations() {
        let runs = vec![
            run("1", "Running",   "2026-01-01T10:00:00Z", None),
            run("2", "Succeeded", "2026-01-01T09:00:00Z", Some("2026-01-01T09:00:30Z")),
        ];
        let kpi = compute_workflow_kpi(&runs);
        // Only the Succeeded run has duration
        assert!((kpi.avg_duration_secs.unwrap() - 30.0).abs() < 0.1);
    }
}
