use clap::Parser;

/// Three-pane terminal workspace: current project, Claude Code, git changes.
#[derive(Parser)]
#[command(name = "term", version = term_tui::VERSION, about)]
struct Cli {}

fn main() {
    let _cli = Cli::parse();
    println!(
        "term {}: nothing to run yet, see ROADMAP.md",
        term_tui::VERSION
    );
}
