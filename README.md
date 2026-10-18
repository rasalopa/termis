# termis

A three-pane terminal workspace for working with a coding agent on macOS:
the current project on the left, a live agent session in the middle, and
the git changes with a commit box on the right. Claude Code is the first
supported agent.

```text
┌─ Projects ─────────┬─ ● claude · termis ──────────────────────────────┬─ Changes · main ↑0 ↓0 ───┐
│▸ termis     main ● │> add the list of modified files on the right,    │4 files     +144 −10      │
│  api        main   │  with their +/- line counts                      │staged (2)                │
│  web        dev  ● │                                                  │  M src/app.rs    +31 −4  │
│  notes      main   │● First I check how the layout is built.          │  M src/ui/lay…    +9 −2  │
│─ current ──────────│                                                  │unstaged (2)              │
│~/src/termis        │● Read(src/ui/layout.rs)                          │▸ M src/ui/cha…  +61 −4   │
│Rust · ratatui 0.30 │  ⎿  Read 58 lines                                │  ? src/git.rs   +43      │
│                    │                                                  │─ today ──────────────────│
│                    │● Write(src/git.rs)                               │3 commits · +312 −58      │
│                    │  ⎿  Wrote 97 lines to src/git.rs                 │▁▂▃▅▇█▆▃  09→17h          │
│                    │                                                  │last a3f9c1e 2h           │
│                    │● Update(src/ui/changes.rs)                       │feat(ui): add the card    │
│                    │  ⎿  Added 61 lines, removed 4 lines              │─ diff src/ui/changes.rs ─│
│                    │                                                  │@@ -12,6 +12,9 @@         │
│                    │● Bash(cargo check)                               │ pub struct ChangesPane { │
│                    │  ⎿  Finished `dev` profile in 2.41s              │-    files: Vec<String>,  │
│                    │                                                  │+    staged: Vec<Entry>,  │
│                    │● Done. The right pane reads git status and       │+    unstaged: Vec<Entry>,│
│                    │  numstat every 2 s. Shall I add the diff view?   │↓ 3 more hunks · d diff   │
├─ claude ───────────┤                                                  │─ commit ─────────────────│
│state   ✶ working   │> yes, and use the theme colours                  │┌────────────────────────┐│
│model   Fable 5.1   │                                                  ││ feat(ui): changes pane…││
│effort  high        │✻ Brewing… (esc to interrupt · 8s)                │└────────────────────────┘│
│ctx     ▓▓▓░░░░░ 41%│                                                  │Enter commit · Esc cancel │
│5h      23% 3h12m   │╭────────────────────────────────────────────────╮│                          │
│week    41% 4d6h    ││ >                                              ││s stage u unstage a all   │
│lines   +96 −12     │╰────────────────────────────────────────────────╯│c commit P push d diff    │
│time    1:12:08     │  ? for shortcuts           ⏵⏵ accept edits on    │                          │
└────────────────────┴──────────────────────────────────────────────────┴──────────────────────────┘
 claude   Ctrl+Q termis mode                                               termis · main · +144 −10 
```

The screen above is the target, drawn in [docs/mockup.md](docs/mockup.md).
0.1.0 draws less:

- no list of other projects, no hourly sparkline and no push (`P`);
- `d` shows a file's diff over the whole right column, not under the
  lists;
- the commit box shows only while it is open;
- the changes pane draws no keys: with it highlighted in termis mode,
  the status bar lists them, all but `d`;
- the status bar's right end shows `termis · <project>`, without the
  branch and the line counts.

[ROADMAP.md](ROADMAP.md) says what comes next.

## Status

0.1.0 is the first release with a changelog, after the v0.0.1 build.
Claude Code runs in the center pane, with `termis` as its terminal. The
right pane holds git: the branch and how it stands against its upstream in
the title, the staged and unstaged files with their line counts, your
commits and lines since midnight and the last commit; from it you stage,
unstage and commit, and read the diff of a file. The left pane shows the
project's path and its stack, read from `Cargo.toml`, `package.json`,
`pyproject.toml`, `go.mod` or `composer.json`. At its foot, in a box of
its own, is the agent: whether it is working, and with which tool, waiting
for a prompt or asking you something; its model and effort, the context in
use, how much of the plan's five-hour and weekly limits is used and when
each resets (the cost instead, without a plan), the lines it changed and
the session time.

While the version is 0.x, a change to the command line, the keys or the
`statusline` and `hook` subcommands bumps the minor version.

## Requirements

- macOS. Linux and Windows are not supported.
- Rust 1.88 or newer, to build it.
- `git` on the `PATH`. `termis` runs it for every git operation and does
  not start without it.
- [Claude Code](https://claude.com/claude-code) as `claude` on the
  `PATH`. Without it `termis` stops with an error.
- A terminal at least 100 columns wide for the side panes. Tested in
  iTerm2; other terminals and tmux are untested.

## Install

From a clone of the repository:

```sh
cargo install --locked --path .
termis --version
```

Cargo installs `termis` in its `bin` directory, `~/.cargo/bin` by
default, which must be on the `PATH`.

## Use

```text
termis [--new] [path] [-- <claude args>]
```

Claude starts in the center pane in `path`, the current directory by
default, with the arguments after `--`. It continues the last
conversation it saved for that directory, as `claude --continue` would,
unless `--new` asks for a new one or the arguments already choose one:
`-c`, `-r`, `-p`, their long forms, `--session-id`, `--from-pr`,
`--teleport` or `--cloud`. A relaunch with `r` continues too, and two
`termis` in the same directory continue the same conversation. The
project is the git repository around that directory: the left pane shows
its root, the right pane its changes. Outside a repository Claude still
runs and the right pane says `not a git repository`; a repository
created later is picked up at the next start.

Keys go to the agent until `Ctrl+Q` enters termis mode, where they are
`termis`'s commands; `Ctrl+Q` or `Esc` goes back. `Ctrl+Q` and `Ctrl+Z`
never reach the agent. The tag at the left of the status bar shows the
mode: the agent's name, `TERMIS`, or `COMMIT` while the commit box is
open. In termis mode the status bar shows the main keys for the
highlighted pane, and `?` lists them all by mode.

The right pane reads the repository again every 2 s while it is on
screen. The commit box (`c`) commits what is staged through `git commit`,
so your git hooks and commit signing run; a failure shows its first line
in the status bar and keeps the message for the next `c`. Below 100
columns the side panes fold away and the center pane takes the width;
`z` in termis mode does the same on a wider terminal.

The center pane keeps up to 5000 lines that scrolled off the agent's
screen. The wheel, or `j`/`k` in termis mode on that pane, scrolls
through them, and the pane title says how far back the view is
(`claude · termis ↑ 120`). Going back to agent mode, or typing to the
agent, returns to the end.

Claude gets a status line and hooks for the session through `--settings`,
merged into a `--settings` of your own; no settings file changes, and the
status line you had keeps running after `termis`'s own. What they report
goes to a private directory under `$TMPDIR`, removed when `termis` quits:
event and tool names, file paths, Claude's notices and its status line
input, never a prompt, a command or a file's content. If the directory
cannot be set up, Claude runs without it, the left pane has no agent
box, and a notice in the status bar says why.
[ADR-0003](docs/adr/0003-inject-claude-telemetry-per-session.md) has the
details.

`q` in termis mode quits at once, with no confirmation: Claude's process
group gets a hangup, and a kill two seconds later if Claude is still
running. When Claude exits on its own, its last screen stays, with a bar
on its last row: `claude exited (code N) · r relaunch · q quit`; `r`
starts it again with the same arguments. After quitting, `termis` prints
how the agent ended when it exited before the quit, such as
`agent exited with code 0`, and exits with a non-zero status when the
agent failed.

## Keys

### Every mode

| Key | Action |
|---|---|
| `Ctrl+Q` | Enter termis mode with the agent pane highlighted; from termis mode or the commit box, back to the agent |
| `Ctrl+Z` | Never reaches the agent |

### Termis mode

| Key | Action |
|---|---|
| `?` | List the keys by mode; `?` or `Esc` closes the list, and any other command closes it and runs |
| `1` / `2` / `3` | Highlight Projects / agent / Changes; `1` and `3` only while the side panes are shown |
| `Tab` / `Shift+Tab` | Highlight the next / previous pane, while the side panes are shown |
| `c` | Open the commit box from any pane; needs the side panes shown, a repository with something staged, and no commit running |
| `z` | Zoom: hide the side panes, or bring them back when the width allows |
| `q` | Quit |
| `Esc` | Close the key list, else the diff, else back to the agent |
| `j` `k` `Up` `Down` `PgUp` `PgDn` `G` | On the agent pane: scroll its history a line or a page, `G` back to the end; the alternate screen has no history |
| `j` `k` `Up` `Down` `s` `u` `a` `d` | On Changes: move the selection, stage or unstage the selected row, stage all, diff of the selected row |
| `j` `k` `Up` `Down` `d` `Esc` | In the diff: scroll it; `d` or `Esc` back to the lists |

### Commit box

| Key | Action |
|---|---|
| typing / `Backspace` / `Ctrl+H` | Typed or pasted text goes at the end of the draft; `Backspace` or `Ctrl+H` deletes its last character |
| `Ctrl+J` / `Enter` | Body line / commit, which needs a subject |
| `Esc` / `Ctrl+Q` | Cancel, back to Changes / to the agent; the draft is kept |

### After the agent ended

| Key | Action |
|---|---|
| `r` / `q` | Relaunch / quit, in agent or termis mode |

## Mouse

`termis` turns on mouse reporting for its whole window. The wheel over
the center pane scrolls the agent's history; over the right pane it moves
the selection or scrolls an open diff. An agent that asks for the mouse
gets the wheel and the clicks, drags and motion it asks for over the
center pane instead: Claude's fullscreen view scrolls, selects and copies
text with them. On the agent's alternate screen without the mouse, the
wheel sends it arrow keys in agent mode. Other clicks do nothing.

To select with the terminal instead, hold Option while dragging in
iTerm2, or Shift in kitty, WezTerm and Alacritty. An iTerm2 profile that
reports only wheel events leaves plain dragging to the terminal.

## Known limits

- One agent, Claude Code, and one project per window. Several projects
  with an agent each come in 0.2; meanwhile another `termis` in another
  tab runs another project.
- No text selection of its own: where the agent does not take the mouse,
  the terminal's selection takes the screen as drawn, side panes and
  borders included.
- Claude's fullscreen view draws on the alternate screen, which keeps no
  history in `termis`; Claude scrolls it itself.
- The agent's state can lag. An interrupt with `Esc` or a denied
  permission sends no event, so the state and the tool shown hold until
  the next event, Claude's idle notice at the latest. A tool waiting for
  permission shows as running until Claude's permission notice, a few
  seconds later.
- With `--bare`, `disableAllHooks`, `allowManagedHooksOnly` or a managed
  status line, Claude leaves out some or all of what `termis` injects,
  and the agent's box shows less or nothing.
- Your status line runs without the variables `termis` withholds from
  Claude, such as `TERM_PROGRAM`, `LC_TERMINAL` and `ITERM_SESSION_ID`.
- A commit runs in the background with no terminal input: a hook that
  reads the terminal stops until the 60 s timeout kills it, and a curses
  pinentry cannot ask for a passphrase.
- No push, no hunk or line staging, no branch, stash or rebase commands,
  no config file, no themes, no desktop notifications.
- No session persistence: when `termis` quits, the agent ends.

## Develop

```sh
git config core.hooksPath .githooks   # once per clone
cargo test
cargo run -- --version
```

The hooks check the commit message and run `cargo fmt --check` and
`cargo clippy` before each commit; `cargo test` runs by hand. CI runs
`cargo fmt --check`, `cargo clippy`, `cargo test` and a build with the
`rust-version` of `Cargo.toml` on every push and pull request to `main`;
it does not check commit messages.

`termis --spike` runs the agent over the whole terminal with no side
panes, and `--panic-after <SECONDS>` panics a thread after that many
seconds, for the checks in
[docs/spike-checklist.md](docs/spike-checklist.md).

Conventions, commit format and release steps are in
[CONTRIBUTING.md](CONTRIBUTING.md). Design decisions are in
[docs/adr](docs/adr/README.md).

## License

[MIT](LICENSE)
