use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use termis::app::{self, Options};
use termis::{strings, telemetry};

/// Three-pane terminal workspace: current project, coding agent, git changes.
#[derive(Parser)]
#[command(name = "termis", version = termis::VERSION, about, disable_help_subcommand = true)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Directory to work in; defaults to the current one.
    path: Option<PathBuf>,

    /// Run the agent over the whole terminal, with no side panes.
    #[arg(long)]
    spike: bool,

    /// Debug aid: panic from a worker thread after this many seconds.
    #[arg(long, value_name = "SECONDS")]
    panic_after: Option<u64>,

    /// Arguments passed to the agent, after `--`.
    #[arg(last = true)]
    agent_args: Vec<String>,
}

/// What the agent's session runs, not meant for typing and kept out of
/// the help.
#[derive(Subcommand)]
enum Command {
    /// Status line shim: keeps the agent's JSON for the app and runs the
    /// status line the user had before.
    #[command(hide = true)]
    Statusline,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    if let Some(Command::Statusline) = cli.command {
        std::process::exit(statusline()?);
    }
    let cwd = match cli.path {
        Some(path) => path
            .canonicalize()
            .with_context(|| format!("open {}", path.display()))?,
        None => std::env::current_dir()?,
    };
    let status = app::run(Options {
        cwd: &cwd,
        args: &cli.agent_args,
        full_screen: cli.spike,
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

/// The `statusline` subcommand: reads the agent's JSON from stdin into
/// the instance named by the environment, chaining the previous status
/// line. Without an instance there is nothing to do; the input is still
/// read, so the agent never finds the pipe closed early.
fn statusline() -> Result<i32> {
    let mut input = Vec::new();
    std::io::stdin()
        .lock()
        .read_to_end(&mut input)
        .context("read the status line input")?;
    match std::env::var_os(telemetry::INSTANCE_DIR_VAR) {
        Some(dir) => telemetry::statusline(Path::new(&dir), &input),
        None => Ok(0),
    }
}
