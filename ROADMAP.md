# Roadmap to v0.1.0

`term` is a three-pane terminal workspace for working with a coding agent:
the current project on the left, a live agent session in the middle, and
the git changes (files, lines, diff, commit) on the right. Claude Code is
the first supported agent; the core does not depend on it (ADR-0004).
See [docs/mockup.md](docs/mockup.md) for the target screen.

This file is the plan for the first usable release. It is ordered by risk:
the one thing that can sink the project (hosting Claude Code inside our own
terminal pane) is proven first; the first daily-usable build is tagged as
soon as committing from the right pane works; everything else is added
while using it.

## Daily use

The setup this plan targets, outermost first:

```
iTerm2  →  herdr (prefix Ctrl+B)  →  term (prefix Ctrl+Q)  →  claude
```

herdr gives persistence and multiplexing; `term` gives the git side and the
project card. Consequences:

- `term` is the terminal Claude talks to. It answers Claude's startup
  queries itself (kitty keyboard, colours, device attributes) and sets the
  child's `TERM`; nothing from the host is relayed verbatim.
- Three prefixes coexist: `Ctrl+B` for herdr, `Ctrl+Q` for `term`, `Esc` and
  `Ctrl+C` for Claude.
- Scrollback and copy for the center pane are the host's job in 0.1; Claude's
  transcript lives under `~/.claude/projects`.
- Launch: `term [path] [-- <claude args>]`. `term -- -c` resumes the last
  session.
- Outside a git repository Claude still runs and the right pane says so. If
  `claude` is not on the `PATH`, `term` exits with a one-line error.

## The MVP (v0.1.0)

A single binary, `term`, for macOS:

- **Projects pane** (left): a card for the current repository (path, stack,
  branch, ahead/behind, last commit, lines today) and the agent session
  (state, model, context %, cost, elapsed), as far as the adapter provides.
- **Agent pane** (center): a real PTY running the agent, `claude` in 0.1,
  unmodified. While it has focus every key reaches the agent; it keeps
  rendering while another pane has focus; resizes propagate.
- **Changes pane** (right): staged and unstaged files with `+/-` per file,
  lines committed today, the diff of the selected file, a commit box. Stage,
  unstage, stage all, commit. Commits run the `git` binary, so hooks, signing
  and credential helpers behave as in the shell.
- **Status bar**: focus, the keys that apply, result toasts.
- **Keys**: `Ctrl+Q` is the only reserved key. `Ctrl+Q 1/2/3` focus a pane,
  `Ctrl+Q c` opens the commit box, `Ctrl+Q z` zooms, `Ctrl+Q q` quits,
  `Ctrl+Q Ctrl+Q` sends a literal. In a side pane: `j`/`k` move, `s`/`u`/`a`
  stage, unstage, stage all, `c` commit, `d` diff, `r` refresh, `Esc` back
  to Claude.

### Not in 0.1

Agents other than Claude Code (the adapter boundary exists from the start,
the second adapter comes later), a list of other projects and switching
between them, mouse support, multiple agent sessions, hunk or line staging, push, branch/stash/rebase
operations, a config file, themes, desktop notifications, session persistence (herdr or tmux provide it), Linux and
Windows.

## Milestones

Each milestone is a list of atomic commits (one logical change each, see
[CONTRIBUTING.md](CONTRIBUTING.md)) and ends with a "done when" that takes
minutes to check. Sizes are full-time solo effort: S is a day or less, M two
to four days, L one to two weeks. An evening is about two hours, so M is one
to two weeks of evenings. The first daily-usable tag, v0.0.1, comes after M3,
roughly three weeks of evenings in; v0.1.0 after five to seven.

### M0 - Scaffold (S)

Goal: a repository that already works like a public project.

Commits:
- `chore: scaffold cargo project` - package `term-tui`, binary `term`,
  `src/lib.rs` plus a thin `src/main.rs`, version `0.0.0`, edition 2024,
  `publish = false`, lints in `Cargo.toml`, `.gitignore`, `.editorconfig`,
  `LICENSE`, committed `Cargo.lock`.
- `docs: add README, ROADMAP, CONTRIBUTING and CHANGELOG`.
- `docs: add target screen mockup`.
- `docs: add spike checklist`.
- `docs(adr): record git through the git binary`.
- `docs(adr): record threads and channels, no async runtime`.
- `docs(adr): record per-session injection of Claude telemetry`.
- `chore: add git hooks for commit message and pre-commit checks`.

The developer enables the hooks by hand once: `git config core.hooksPath
.githooks`.

Done when: `cargo run -- --version` prints `term 0.0.0`; `cargo fmt --check`,
`cargo clippy --all-targets -- -D warnings` and `cargo test` pass; a commit
with the subject `Added stuff.` is rejected by the hook.

### M1 - Spike: Claude inside a pane (L, go/no-go)

Goal: prove that Claude Code runs correctly inside our emulator before any
layout work. Only this milestone may change the stack. Work on a
`spike/claude-pane` branch; squash what survives into the commits below.

Commits:
- `feat(pty): spawn a child on a pty and stream its output` - `portable-pty`,
  reader thread sending chunks, drop the slave after spawn so EOF arrives.
- `feat(pty): build the child environment` - inherit, then set
  `TERM=xterm-256color`, `COLORTERM=truecolor`, `TERM_TUI_DIR`,
  `CLAUDE_CODE_DISABLE_TERMINAL_TITLE=1`; remove `TERM_PROGRAM`,
  `TERM_PROGRAM_VERSION`, `ITERM_SESSION_ID`, `TMUX`, `CLAUDECODE`,
  `CLAUDE_CODE_CHILD_SESSION`; keep `LANG`, default `LC_CTYPE=UTF-8`.
- `feat(pty): resize the child when the host terminal resizes` - emulator
  size first, then `MasterPty::resize`.
- `feat(pty): detect child exit and reap it` - waiter thread plus reader EOF
  or EIO.
- `feat(input): read host events on a thread and send them over a channel` -
  crossterm `poll` and `read` on a thread, a final event on failure.
  Terminal modes (raw, bracketed paste, keyboard flags) are set by app.
- `feat(keys): encode key events into bytes for the child` - legacy,
  modifyOtherKeys and kitty CSI u encodings selected by the mode Claude last
  requested; application cursor keys; pastes re-wrapped when the child asked
  for bracketed paste.
- `feat(emu): track modes and answer queries through vt100 callbacks` -
  kitty push/pop and query, DA1, OSC 10/11 with a hard-coded dark theme,
  DECSET 1004 and 2004; replies written to the pty.
- `feat(emu): render the screen with tui-term` - scrollback per the renderer
  decision in the checklist.
- `feat(app): restore the host terminal on quit, panic and child exit` - one
  `restore()` that pops flags, disables paste and focus reporting, shows the
  cursor and leaves raw mode; panic hook exits the process so a worker panic
  cannot leave a half-dead UI. Quit sends SIGHUP to the child's process
  group, waits 2 s, then SIGKILL.
- `feat(app): add term --spike for a full-screen Claude pane`.
- `test(keys): cover the encoder in all three encodings` - Enter,
  Shift+Enter, Esc, Ctrl+C, arrows and Option keys.
- `test(emu): replay recorded byte streams and assert modes and replies` -
  fixtures captured from a real session with `script -q`.
- `docs(adr): record the emulator chosen by the spike` (ADR-0005).
- `docs(adr): record the input model chosen by the spike` (ADR-0006).

Done when: [docs/spike-checklist.md](docs/spike-checklist.md) passes in
iTerm2, inside herdr and inside tmux, and the results table is filled in.

If the checklist cannot pass with `vt100`, write an `alacritty_terminal`
adapter for tui-term's `Screen` and `Cell` traits (budget one day) and run
the checklist again before M2.

### M2 - Shell: layout, focus, status bar (M)

Goal: the three-pane frame with the focus model; side panes still empty.

Commits:
- `feat(ui): add three-column layout with bordered panes and status bar`.
- `feat(app): add focus state machine with the Ctrl+Q prefix` - prefix
  mode with timeout, `Ctrl+Q 1/2/3`, `Esc` back to Claude, `Ctrl+Q Ctrl+Q`
  literal.
- `feat(ui): collapse side panes below 100 columns`.
- `feat(ui): highlight the focused pane and show its keys in the status bar`.
- `feat(app): zoom the center pane with Ctrl+Q z`.
- `feat(app): quit with Ctrl+Q q` - SIGHUP, wait, kill; no confirmation.
- `feat(app): keep the agent running` - Ctrl+Z is not forwarded, since the
  agent would suspend itself with nothing to resume it; a stopped child gets
  SIGCONT.
- `feat(app): pass trailing arguments to claude` - `term -- -c`.
- `feat(ui): show empty states for no repository and an exited agent` -
  `not a git repository`; `claude exited (code N) · r relaunch`, with the
  adapter's display name.
- `test(ui): snapshot the frame at 100 columns and collapsed`.
- `test(app): cover focus transitions and prefix handling`.

Done when, inside herdr: `Ctrl+Q 3` and `Ctrl+Q 1` move the highlight and
`Esc` returns; `Ctrl+Q z` twice restores the layout; `Ctrl+Q Ctrl+Q` reaches
Claude as a literal; `/config` opens and closes in Claude; shrinking the pane
below 100 columns collapses the sides; `Ctrl+Q q` exits and `pgrep -f claude`
is empty.

### M3 - Changes pane: lists, stage, commit (M) → v0.0.1

Goal: commit from the right pane. This is the first build worth using every
day.

Commits:
- `feat(git): parse git status --porcelain=v2 --branch -z` - staged,
  unstaged, untracked, renames, branch, ahead/behind; `--no-optional-locks`.
- `feat(git): parse git diff --numstat -z -M for index and worktree` -
  binaries shown as `bin`; untracked files measured with
  `git diff --no-index --numstat -z /dev/null <file>`.
- `feat(git): run commands on a worker thread with a 2 s poll` - `GitJob`
  in, `GitResult` out; polls only while the pane is visible.
- `feat(ui): add changes pane with staged and unstaged file lists`.
- `feat(git): stage, unstage and stage all through the git binary`.
- `feat(git): commit with a subject and optional body` - `git commit -F -`,
  `GIT_TERMINAL_PROMPT=0`, closed stdin, 60 s timeout, short hash from
  `git rev-parse --short HEAD`.
- `feat(ui): add commit box with Enter, Ctrl+J and Esc handling` - draft
  kept on cancel; refuses to open with an empty index.
- `feat(ui): show toasts for commit results and git errors` - first
  non-empty stderr line on failure.
- `feat(app): jump to the commit box with Ctrl+Q c`.
- `test(git): parse recorded status and numstat output` - fixtures under
  `tests/fixtures/git/`.
- `test(git): stage and commit in a temporary repository` - isolated from
  the developer's git configuration.
- `chore(release): v0.0.1`.

Done when: stage two files, type a subject, `Enter`; the commit is in
`git log`, the lists refresh within 2 s, the toast shows the hash; a
`pre-commit` hook that fails shows its first stderr line and keeps the
message; files edited by Claude appear in the list within 2 s.

Tag `v0.0.1` and use `term` every evening from here on.

### M4 - Changes pane extras (S)

Commits:
- `feat(ui): show the diff of the selected file with d` - the right column
  shows the diff full height with `j`/`k` scrolling, `d` or `Esc` returns;
  the small inline preview from the mockup waits until widths are known.
- `feat(git): count commits and lines made today` - `git log
  --since=midnight --no-merges --numstat`, author from
  `git var GIT_AUTHOR_IDENT`, lockfiles excluded by pathspec.
- `feat(ui): add today section to the changes pane`.

Done when: `d` on a modified file shows the same hunks as `git diff -- file`;
the today line matches `git log --since=midnight --shortstat`.

### M5 - Current project card (S)

Commits:
- `feat(git): read last commit and upstream state for the current repo`.
- `feat(projects): detect the stack from manifest files` - Cargo.toml,
  package.json, pyproject.toml, go.mod, composer.json.
- `feat(ui): add current project card to the projects pane`.

Done when: the card matches `git status -sb` and `git log -1` for the repo.

### M6 - Claude adapter: status line and hooks (M)

Goal: the agent section of the left pane, fed by Claude Code's own status
line and hooks, injected per session (ADR-0003), behind the `Agent` trait
(ADR-0004).

Commits:
- `feat(agent): add the Agent trait and the Claude adapter` - launch
  command, environment, display name.
- `feat(agent): launch claude with --settings for status line and hooks` - `statusLine` pointing at the running executable, async command
  hooks for `UserPromptSubmit`, `Stop` and `Notification`.
- `feat(agent): add statusline subcommand that tees and chains` - the user's `statusLine` command is copied to the
  instance directory at launch and run after the tee with the same input.
- `feat(agent): add hook subcommand that appends events as JSON lines`.
- `feat(agent): read status and events from the instance directory` -
  model, context %, cost, lines; state from events: prompt submitted →
  `working`, stop → `waiting`, notification → `needs attention`.
- `feat(ui): add agent section and state indicator to the projects pane`.

Done when: the section shows the same model, context % and cost as Claude's
own status line; the user's previous status line still renders; the state
flips within 2 s of Claude finishing a turn; two `term` windows show their
own sessions.

### M7 - Release v0.1.0 (S)

Commits:
- `docs: write README with install, usage and key reference` - the key
  table is checked against `src/strings.rs`.
- `ci: add fmt, clippy and test workflow` - `macos-latest`, dormant until a
  remote exists; exercised on a throwaway private repository before the
  first real push.
- `docs: add changelog entry for 0.1.0`.
- `chore(release): v0.1.0` - bump `Cargo.toml` and `Cargo.lock`, annotated
  tag.

Done when: `git clone` into a temporary directory, `cargo install --locked
--path .`, `term --version` prints `term 0.1.0`; the README key table matches
the status bar hints.

## After 0.1

The backlog until there is an issue tracker, in no order:

- More agents behind the `Agent` trait (Codex, OpenCode, Gemini CLI), each
  with its launch command and telemetry source, selected with `--agent`.
- List of other projects and switching between them. Paths come from the
  `projects` keys in `~/.claude.json` (the directory names under
  `~/.claude/projects` are not reversible); switching sends SIGHUP to the
  current `claude` and relaunches `claude -c` in the selected repository.
- Push from the pane, with `-u origin <branch>` when there is no upstream.
- Inline diff preview under the file lists, as drawn in the mockup.
- File watcher (`notify`) to cut refresh latency below the 2 s poll, with
  `target/` and `.git/objects` filtered out.
- `claude agents --json` as a state source when hooks are unavailable.
- Desktop notification when Claude is waiting and the window is not focused.
- Hunk and line staging, or handing the pane to lazygit.
- Mouse: click to focus, wheel to scroll side panes.
- Scrollback and copy for the center pane.
- Config file: prefix key, pane widths, poll interval, theme colours.
- Terminal title from Claude's OSC 0/2 in the pane header.
- Linux support.
- Emulator alternatives behind tui-term's traits: `alacritty_terminal`,
  `libghostty-vt`.
- Ask Claude for a commit subject from the staged diff (`claude -p`).

## Runtime model

Threads: host input (crossterm `read()`), PTY reader (byte chunks), child
waiter, git worker (`GitJob` in, `GitResult` out), signal thread (SIGHUP and
SIGTERM to `term` itself). All send one `enum Msg` over a single
`std::sync::mpsc` channel.

The main thread owns the emulator state and the PTY writer. Loop:
`recv_timeout(16 ms)`, then `try_recv` until empty, apply everything, draw
once if anything changed or a deadline (prefix timeout, elapsed clock, poll)
fired. Redraw is on change with a 16 ms floor; a 100x40 grid through tui-term
is a few thousand cells and well under a millisecond.

Instance directory: `$TMPDIR/term-<pid>/`, passed to the child as
`TERM_TUI_DIR`, holds `status.json` (written by rename), `events.jsonl` and
the copied previous status line command; removed on quit.

Logs: `~/Library/Logs/term/term.log` through `tracing` with a non-blocking
appender, level from `TERM_LOG`, default `info`. Nothing is printed to
stdout or stderr after the terminal is initialised.

## Dependencies (planned)

| Crate | Role |
|---|---|
| ratatui 0.30, crossterm 0.29 | UI and host terminal backend |
| portable-pty 0.9 | PTY spawn, resize, child handling |
| vt100 0.16, tui-term 0.3 | emulation and rendering of the center pane |
| serde, serde_json | status line and hook JSON |
| clap 4 | CLI: `term`, `term --spike`, `term statusline`, `term hook` |
| anyhow | errors |
| tracing, tracing-appender | logs to a file |
| signal-hook | SIGHUP and SIGTERM delivered to the main loop |
| tempfile (dev) | temporary repositories in tests |

## Decisions

Recorded in [docs/adr](docs/adr/README.md). Made now: git through the `git`
binary (0001), threads and channels without an async runtime (0002), Claude
telemetry injected per session (0003), agent-agnostic core with Claude as
the first adapter (0004). Decided by the spike: emulator (0005) and input
model (0006). The package is `term-tui` and the binary `term`,
because `term` is an existing crates.io crate; the package is never
published under that name.
