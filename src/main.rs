use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;
use term_tui::app::{self, SpikeOptions};
use term_tui::strings;

/// Three-pane terminal workspace: current project, coding agent, git changes.
#[derive(Parser)]
#[command(name = "term", version = term_tui::VERSION, about)]
struct Cli {
    /// Directory to work in; defaults to the current one.
    path: Option<PathBuf>,

    /// Run the agent over the whole terminal, with no side panes.
    #[arg(long)]
    spike: bool,

    /// Debug aid: panic from a worker thread after this many seconds.
    #[arg(long, value_name = "SECONDS", requires = "spike")]
    panic_after: Option<u64>,

    /// Arguments passed to the agent, after `--`.
    #[arg(last = true)]
    agent_args: Vec<String>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let cwd = match cli.path {
        Some(path) => path,
        None => std::env::current_dir()?,
    };
    if !cli.spike {
        println!("{}", strings::nothing_to_run(term_tui::VERSION));
        return Ok(());
    }
    let status = app::spike(SpikeOptions {
        cwd: &cwd,
        args: &cli.agent_args,
        panic_after: cli.panic_after,
    })?;
    if let Some(status) = status {
        match status.signal() {
            Some(signal) => println!("{}", strings::agent_killed(signal)),
            None => println!("{}", strings::agent_exited(status.exit_code())),
        }
        if !status.success() {
            std::process::exit(i32::try_from(status.exit_code()).unwrap_or(1));
        }
    }
    Ok(())
}
