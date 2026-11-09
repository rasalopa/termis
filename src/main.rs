use std::io::Read;
use std::path::PathBuf;

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

    /// Directory to work in; without one, termis shows the start screen,
    /// or with an option starts in the current one.
    path: Option<PathBuf>,

    /// Start a new conversation instead of continuing the directory's last
    /// one.
    #[arg(long)]
    new: bool,

    /// Open only the directory to work in, not the sessions open when
    /// termis last ran, and save it in their place.
    #[arg(long)]
    no_restore: bool,

    /// Run the agent of this account of the config file, as the account,
    /// in the directory to work in, and remember it for the directory.
    #[arg(long, value_name = "NAME")]
    account: Option<String>,

    /// Run the agent over the whole terminal, with no side panes; the
    /// sessions of the last run are neither reopened nor saved.
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
    /// Hook shim: appends the agent's hook event to the instance's events
    /// file.
    #[command(hide = true)]
    Hook,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Statusline) => std::process::exit(statusline()?),
        Some(Command::Hook) => {
            hook();
            std::process::exit(0);
        }
        None => {}
    }
    let start_screen = shows_start_screen(&cli);
    let cwd = match cli.path {
        Some(path) => path
            .canonicalize()
            .with_context(|| format!("open {}", path.display()))?,
        None => std::env::current_dir()?,
    };
    let status = app::run(Options {
        cwd: &cwd,
        args: &cli.agent_args,
        new_conversation: cli.new,
        restore: !cli.no_restore,
        account: cli.account.as_deref(),
        full_screen: cli.spike,
        panic_after: cli.panic_after,
        start_screen,
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

/// Whether `termis` shows the start screen for `cli`: with no path, no
/// option and nothing after `--`. The debug aid `--panic-after` does not
/// count.
fn shows_start_screen(cli: &Cli) -> bool {
    cli.path.is_none()
        && !cli.new
        && !cli.no_restore
        && cli.account.is_none()
        && !cli.spike
        && cli.agent_args.is_empty()
}

/// The `hook` subcommand: reads the agent's hook input from stdin and
/// appends it to the events of the instance named by the environment.
/// Never fails the agent: a problem goes to stderr and the exit code
/// stays zero. Without an instance there is nothing to do.
fn hook() {
    let mut input = Vec::new();
    if let Err(error) = std::io::stdin().lock().read_to_end(&mut input) {
        eprintln!("termis hook: read the hook input: {error}");
        return;
    }
    if let Some(dir) = telemetry::instance_dir()
        && let Err(error) = telemetry::hook(&dir, &input)
    {
        eprintln!("termis hook: {error:#}");
    }
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
    match telemetry::instance_dir() {
        Some(dir) => telemetry::statusline(&dir, &input),
        None => Ok(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_an_option_or_agent_arguments_skip_the_start_screen() {
        let shows = |args: &[&str]| {
            let all = ["termis"].iter().chain(args);
            let cli = Cli::try_parse_from(all).expect("parses");
            shows_start_screen(&cli)
        };
        assert!(shows(&[]));
        assert!(shows(&["--panic-after", "5"]));
        for args in [
            &["."][..],
            &["--new"],
            &["--no-restore"],
            &["--account", "work"],
            &["--spike"],
            &["--", "-x"],
        ] {
            assert!(!shows(args), "{args:?}");
        }
    }
}
