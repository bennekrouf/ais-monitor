//! Phase 1 smoke test — exercise the core data layer with zero TUI.
//!
//! Usage:
//!   probe login                          — show az login state
//!   probe subs                           — list subscriptions
//!   probe apps <sub>                     — list Logic App sites in a subscription
//!   probe workflows <sub> <rg> <app>     — list deployed workflows
//!   probe runs <sub> <rg> <app> <wf>     — list recent runs + computed KPI
//!   probe chains <sub> <rg> <app> <dir>  — discover the chain graph
//!
//! Prints structured-ish text to stdout. If anything here works, the TUI
//! has a solid foundation to render on top of.

use ais_monitor_core::{azure, kpi, remote_chain};
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("");

    let result: Result<(), String> = match cmd {
        "login" => cmd_login(),
        "subs" => cmd_subs(),
        "apps" => cmd_apps(&args),
        "workflows" => cmd_workflows(&args),
        "runs" => cmd_runs(&args),
        "chains" => cmd_chains(&args),
        "" | "help" | "--help" | "-h" => {
            usage();
            return ExitCode::SUCCESS;
        }
        other => Err(format!("unknown command: {other}")),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn usage() {
    eprintln!(
        "ais-monitor probe — exercise core services\n\n\
         commands:\n  \
           login\n  \
           subs\n  \
           apps <sub>\n  \
           workflows <sub> <rg> <app>\n  \
           runs <sub> <rg> <app> <workflow>\n  \
           chains <sub> <rg> <app> <local_dir>"
    );
}

fn cmd_login() -> Result<(), String> {
    use azure::AzLoginState;
    let state = azure::check_login();
    match &state {
        AzLoginState::LoggedIn { account, subscription_id } => {
            println!("logged in: {account}  (sub: {subscription_id})");
        }
        AzLoginState::Expired => {
            println!("token expired — run `az login` to refresh");
        }
        AzLoginState::NotLoggedIn => {
            println!("not logged in — run `az login`");
        }
        AzLoginState::AzNotFound => {
            println!("`az` CLI not found on PATH");
        }
        AzLoginState::Checking => {
            println!("checking…");
        }
    }
    Ok(())
}

fn cmd_subs() -> Result<(), String> {
    let subs = azure::list_subscriptions()?;
    println!("{} subscription(s):", subs.len());
    for s in subs {
        println!("  {s:?}");
    }
    Ok(())
}

fn cmd_apps(args: &[String]) -> Result<(), String> {
    let sub = args.get(1).ok_or("missing <sub>")?;
    let apps = azure::list_logic_app_sites(sub)?;
    println!("{} Logic App site(s) in {sub}:", apps.len());
    for a in apps {
        println!("  {a:?}");
    }
    Ok(())
}

fn cmd_workflows(args: &[String]) -> Result<(), String> {
    let sub = args.get(1).ok_or("missing <sub>")?;
    let rg = args.get(2).ok_or("missing <rg>")?;
    let app = args.get(3).ok_or("missing <app>")?;
    let wfs = azure::list_deployed_workflows(sub, rg, app)?;
    println!("{} workflow(s) deployed in {app}:", wfs.len());
    for w in wfs {
        println!("  {w:?}");
    }
    Ok(())
}

fn cmd_runs(args: &[String]) -> Result<(), String> {
    let sub = args.get(1).ok_or("missing <sub>")?;
    let rg = args.get(2).ok_or("missing <rg>")?;
    let app = args.get(3).ok_or("missing <app>")?;
    let wf = args.get(4).ok_or("missing <workflow>")?;
    let runs = azure::list_runs(sub, rg, app, wf, 20)?;
    println!("{} recent run(s) for {wf}:", runs.len());
    for r in &runs {
        println!("  {r:?}");
    }
    let k = kpi::compute_workflow_kpi(&runs);
    println!("\nKPI: {k:#?}");
    Ok(())
}

fn cmd_chains(args: &[String]) -> Result<(), String> {
    let sub = args.get(1).ok_or("missing <sub>")?;
    let rg = args.get(2).ok_or("missing <rg>")?;
    let app = args.get(3).ok_or("missing <app>")?;
    let dir = args.get(4).ok_or("missing <local_dir>")?;
    let chains = remote_chain::discover_chains_remote(sub, rg, app, dir)?;
    println!("{} chain(s) discovered:", chains.len());
    for c in chains {
        let steps: Vec<&str> = c.steps.iter().map(|s| s.workflow.as_str()).collect();
        println!("  [{}] {}", c.label, steps.join(" -> "));
        if !c.queues.is_empty() {
            println!("    queues: {}", c.queues.join(", "));
        }
        if !c.parallel_entries.is_empty() {
            println!("    parallel: {}", c.parallel_entries.join(", "));
        }
    }
    Ok(())
}
