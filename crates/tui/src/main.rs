//! AIS Monitor — terminal frontend.

mod app;
mod config;
mod msg;
mod runs_cache;
mod tui;

use anyhow::Result;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = config::CliArgs::parse(std::env::args().skip(1));
    if cli.help {
        eprintln!("{}", config::usage());
        std::process::exit(0);
    }

    let mut terminal = tui::init()?;
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = tui::restore();
        original_hook(info);
    }));

    let result = app::run(&mut terminal, cli).await;
    tui::restore()?;
    result
}
