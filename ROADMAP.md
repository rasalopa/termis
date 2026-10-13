# Roadmap

`termis` is a three-pane terminal workspace for working with a coding agent:
the current project on the left, a live agent session in the middle, and
the git changes (files, lines, diff, commit) on the right. Claude Code is
the first supported agent; the core does not depend on it (ADR-0004).
See [docs/mockup.md](docs/mockup.md) for the target screen.

This file is the plan for the first usable release, v0.1.0, and for the
milestone after it. The release is ordered by risk:
the one thing that can sink the project (hosting Claude Code inside our own
terminal pane) is proven first; the first daily-usable build is tagged as
soon as committing from the right pane works; everything else is added
while using it.

## Daily use

The setup this plan targets, outermost first:

```
iTerm2  →  termis (Ctrl+Q for termis mode)  →  claude
```

`termis` runs directly in the terminal and gives the git side and the project
card; nothing multiplexes it. Consequences:

- `termis` is the terminal Claude talks to. It answers Claude's startup
  queries itself (kitty keyboard, colours, device attributes) and sets the
  child's `TERM`; nothing from the host is relayed verbatim.
- Two sets of keys coexist: `Ctrl+Q` and termis mode for `termis`, `Esc` and
  `Ctrl+C` for Claude. `Ctrl+Z` is dropped in every mode: Claude would try
  to suspend itself and wait forever.
- The center pane keeps up to 5000 lines that scrolled off the agent's
  screen, read with the wheel or in termis mode. An agent that asks for
  the mouse gets the wheel and the clicks and drags it asks for over the
  center pane instead: Claude's fullscreen view scrolls, selects and
  copies with them. Where the agent does not take the mouse, copying is
  the host's job: in iTerm2, hold Option while dragging. Claude's
  transcript lives under `~/.claude/projects`.
- Launch: `termis [path] [-- <claude args>]`. `termis -- -c` continues the
  last conversation.
- Outside a git repository Claude still runs and the right pane says so. If
  `claude` is not on the `PATH`, `termis` restores the terminal and exits
  with status 1; stderr says `Error: start claude`, then the causes, down
  to the `PATH` that was searched.

## The MVP (v0.1.0)

A single binary, `termis`, for macOS:

- **Projects pane** (left): a card for the current project (path, stack)
  and, in a box at the foot of the pane, the agent session: its state or
  the tool it runs, model, effort, context %, the plan's usage limits or
  else the cost, lines changed and session time, as far as the adapter
  provides.
- **Agent pane** (center): a real PTY running the agent, `claude` in 0.1,
  unmodified. In agent mode every key but `Ctrl+Q` and `Ctrl+Z` reaches
  the agent; it keeps rendering while another pane has focus; resizes
  propagate.
- **Changes pane** (right): the branch and its upstream, staged and
  unstaged files with `+/-` per file, lines committed today, the last
  commit, the diff of the selected file, a commit box. Stage, unstage,
  stage all, commit. Commits run the `git` binary, so hooks and signing
  run as in the shell, except that nothing can prompt in the terminal: a
  hook that reads it stops until the 60 s timeout kills it, and a curses
  pinentry cannot take the screen.
- **Status bar**: the mode, the keys that apply, result toasts, and the
  project at its right end.
- **Keys**: `Ctrl+Q` and `Ctrl+Z` never reach the agent. `Ctrl+Q` enters
  termis mode, where keys are commands, and leaves it; `Ctrl+Z` is
  dropped. In termis mode `1/2/3` focus a pane, `Tab` and `Shift+Tab` the
  next and previous one, `c` opens the commit box, `z` zooms, `?` lists
  the keys, `q` quits, `Esc` closes the key list, else the diff, else
  returns to Claude; with the agent pane focused, `j`/`k` and
  `PgUp`/`PgDn` scroll back and `G` returns to the end; with the changes
  pane focused, `j`/`k` move, `s`/`u`/`a` stage, unstage, stage all, `d`
  diff. After the agent exits, `r` relaunches it. The status bar tag shows
  the mode.

### Not in 0.1

Agents other than Claude Code (the adapter boundary exists from the start,
the second adapter comes later), a list of other projects and switching
between them, clicks on `termis`'s own panes, multiple agent sessions,
hunk or line staging, push, branch/stash/rebase operations, a config
file, themes, desktop notifications, session persistence (running
`termis` inside tmux is not tested), Linux and Windows. Several projects
with an agent each come in M8.

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

Done when: `cargo run -- --version` prints `termis 0.0.0`; `cargo fmt --check`,
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
  `TERM=xterm-256color`, `COLORTERM=truecolor`, `TERMIS_DIR`,
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
iTerm2 and the results table is filled in; tmux is optional.

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
- `feat(app): keep the agent running` - Ctrl+Z is not forwarded: the agent
  would try to suspend itself, the kernel discards the stop for its orphaned
  process group, and it would wait forever for SIGCONT.
- `feat(app): pass trailing arguments to claude` - `term -- -c`.
- `feat(ui): show empty states for no repository and an exited agent` -
  `not a git repository`; `claude exited (code N) · r relaunch`, with the
  adapter's display name.
- `test(ui): snapshot the frame at 100 columns and collapsed`.
- `test(app): cover focus transitions and prefix handling`.
- `feat(app): replace the prefix with a sticky term mode` - `Ctrl+Q` enters
  and leaves the mode, keys inside are commands, the status bar tag shows
  the mode; no literal `Ctrl+Q`, no timeout.

Done when, in iTerm2: `Ctrl+Q` then `3` and `1` move the highlight and
`Esc` returns to Claude; `z` twice in termis mode restores the layout;
`/config` opens and closes in Claude; shrinking the pane below 100 columns
collapses the sides; `q` in termis mode exits and `pgrep -f claude` is empty.

### M3 - Changes pane: lists, stage, commit (M) → v0.0.1

Goal: commit from the right pane. This is the first build worth using every
day.

Commits:
- `feat(git): parse git status --porcelain=v2 --branch -z` - staged,
  unstaged, untracked, renames, branch, ahead/behind; `--no-optional-locks`.
- `feat(git): parse git diff --numstat -z for index and worktree` -
  binaries shown as `bin`; untracked files measured with
  `git diff --no-index --numstat -z /dev/null <file>`.
- `feat(git): run commands on a worker thread with a 2 s poll` - `GitJob`
  in, `GitResult` out; polls only while the pane is visible.
- `feat(ui): add changes pane with staged and unstaged file lists`.
- `feat(git): stage, unstage and stage all through the git binary`.
- `feat(git): commit with a subject and optional body` - `git commit -F -`,
  `GIT_TERMINAL_PROMPT=0`, closed stdin, 60 s timeout, short hash from
  `git rev-parse --short HEAD`.
- `feat(ui): add commit box with Enter, Ctrl+J and Esc handling` - `c` in
  termis mode opens it; draft kept on cancel; refuses to open with an
  empty index.
- `feat(ui): show toasts for commit results and git errors` - first
  non-empty stderr line on failure.
- `test(git): replay recorded git output and commit in a temporary repo` -
  fixtures under `tests/fixtures/git/`; the repository test pins locally
  what the developer's git configuration could change.
- `chore(release): v0.0.1`.

Done when: stage two files, type a subject, `Enter`; the commit is in
`git log`, the lists refresh within 2 s, the toast shows the hash; a
`pre-commit` hook that fails shows its first stderr line and keeps the
message; files edited by Claude appear in the list within 2 s.

Tag `v0.0.1` and use `termis` every evening from here on.

### M4 - Pane extras (S)

Commits:
- `feat(ui): list every key with ?` - a box over the screen in termis mode.
- `feat(ui): show the diff of the selected file with d` - the right column
  shows the diff full height with `j`/`k` scrolling, `d` or `Esc` returns;
  the small inline preview from the mockup waits until widths are known.
- `feat(ui): scroll the agent pane back in termis mode` - `j`/`k`,
  `PgUp`/`PgDn` and `G` with the agent pane highlighted; the pane title
  says how far back the view is; it returns to the end when keys go back
  to the agent.
- `feat(git): count commits and lines made today` - `git log
  --since=midnight --no-merges --numstat`, author from
  `git var GIT_AUTHOR_IDENT`, lockfiles excluded by name.
- `feat(ui): add today section to the changes pane`.

Done when: `d` on a modified file shows the same hunks as `git diff -- file`;
`k` with the agent pane highlighted shows earlier output and `Esc` returns
to the prompt; the today line matches `git log --since=midnight --no-merges
--author=<me> --numstat` summed with lockfiles and binaries left out.

### M5 - Current project card (S)

Commits:
- `feat(git): read the commit HEAD points to` - hash, time and subject
  from `git log -1`, read once per commit; the upstream state is in the
  status already.
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
- `feat(agent): add statusline subcommand that tees and chains` - the user's `statusLine` command is copied to the
  instance directory at launch and run after the tee with the same input.
- `feat(agent): add hook subcommand that appends events as JSON lines`.
- `feat(agent): launch claude with --settings for status line and hooks` - `statusLine` pointing at the running executable, async command
  hooks for `UserPromptSubmit`, `Stop` and `Notification`; after the
  subcommands exist, so no session ever points at a missing one.
- `feat(agent): read status and events from the instance directory` -
  model, context %, cost, lines; state from the last event of the
  session: a prompt or a main-agent tool that ran or failed → `working`,
  a stop, a stop by an API error or the idle notice → `waiting`, a
  permission or a question → `needs attention`. `PostToolUse`,
  `PostToolUseFailure` and `StopFailure` hooks join the three. An
  interrupt has no event; its state holds until the idle notice.
- `feat(ui): add agent section and state indicator to the projects pane` -
  `✶ working`, `· waiting`, `! asking`; model, context bar, cost, and a
  session time that runs between the agent's updates.

Done when: the section shows the same model, context % and cost as Claude's
own status line; the user's previous status line still renders; the state
flips within 2 s of Claude finishing a turn; two `termis` windows show their
own sessions.

### M7 - Release v0.1.0 (S)

Commits:
- `fix(emu): keep the screen at least two cells in each direction` -
  vt100 0.16 panics when a line wraps on a one-row screen or a wide
  character lands on a one-column one.
- `fix(emu): clear a wide character that a narrowing resize splits` -
  vt100 vendored with the fix (ADR-0007).
- `docs: write README with install, usage and key reference` - the key
  table is checked against `src/strings.rs`.
- `docs: align the roadmap and mockup with 0.1.0`.
- `ci: add fmt, clippy, test and MSRV workflow` - `macos-latest`, dormant
  until a remote exists; exercised on a throwaway private repository
  before the first real push.
- `docs: add changelog entry for 0.1.0` - under `Unreleased`, everything
  up to 0.1.0, as `v0.0.x` tags get no section of their own.
- `chore(release): v0.1.0` - `Unreleased` becomes the dated 0.1.0
  section; bump `Cargo.toml` and `Cargo.lock`, annotated tag.

Done when: `git clone` into a temporary directory, `cargo install --locked
--path .`, `termis --version` prints `termis 0.1.0`; the README key table matches
the status bar hints.

### M8 - Several projects in one window (L)

After the v0.1.0 release; it ends with v0.2.0, tagged locally.

Goal: open more projects without leaving `termis`, each with its own agent,
and see at a glance which agent is working, waiting or asking. Another
`termis` in a second iTerm2 tab already runs a second project; the list of
states is what it lacks.

Commits:
- `docs(adr): record several sessions in one process` - one `termis` owns
  every session; each has its own PTY, emulator, scrollback, instance
  directory and telemetry watch; each repository one git worker, shared
  by its sessions, with the changes pane's state; ids that are never
  reused tag every PTY and git message. The runtime model below changes
  with the commits that change the code it describes.
- `refactor(app): move the agent session into a struct` - the agent with
  its launch arguments and exit state, the emulator and scrollback, the
  instance directory and telemetry watch; PTY messages carry the session
  id. No visible change.
- `refactor(pty): tag events where they are produced` - `pty-forward`
  goes.
- `perf(app): write the agent's replies only when there are some`.
- `refactor(git): let the worker's owner tag its events`.
- `refactor(app): move the repository's state into a struct` - the git
  worker with its poll and snapshot, the selection, the diff view, the
  commit draft and the commit in flight; git messages carry the
  repository id. No visible change.
- `refactor(app): let the panic hook hang up every agent`.
- `refactor(app): hang up agents through deadlines in the loop` - quit
  hangs up every agent at once.
- `refactor(app): run every session's timers, resizes and output`.
- `fix(app): keep running when a relaunch or resize fails`.
- `feat(agent): give each session its own instance directory` -
  `termis-<pid>/<n>`; the sweep is unchanged.
- `feat(git): find repository roots off the main thread`.
- `feat(agent): list the agent's recent projects` - for Claude, the
  `projects` keys in `~/.claude.json` (in `CLAUDE_CONFIG_DIR` when set)
  that still exist on disk.
- `feat(ui): add a project picker` - the recent projects and the open
  sessions, filtered as you type, a directory with an open session listed
  once, as that session; a typed path is taken as is.
- `feat(app): open a project in a new session with o` - `Ctrl+Q o` opens
  the picker. On an open session `Enter` or `Tab` shows it; elsewhere
  `Enter` launches the agent with a new conversation and `Tab` continues
  the last one there (`claude -c`). The card, the agent's box, the
  changes pane and the project in the status bar follow the session
  shown. Sessions in the background keep reading their agent's output,
  answering its queries and following resizes, and get a focus-out.
- `feat(app): switch sessions with n and N` - the status bar shows the
  position (`2/3`).
- `feat(git): read only the status on request`.
- `feat(ui): list sessions in the projects pane` - one row per session
  with the state glyph, the project name and a dot when it has
  uncommitted changes. Repositories in the background run only
  `git status`, every
  10 s, while the list is on screen. When the list is hidden (under 100
  columns, zoomed) the status bar counts the sessions asking.
- `feat(app): show a session from the projects pane` - `j`/`k` and
  `Enter` with the projects pane focused.
- `feat(app): close a session with x` - hangs up its agent without
  blocking the others; closing the last one quits. While an agent is
  working or asking, `x` and `q` take a second press within a few
  seconds. After an agent exits, the banner's `q` closes its session
  (`q close` while others remain) and quits only when it was the last;
  `q` in termis mode still quits.
- `feat(agent): show asking as soon as a permission dialog opens` - a
  `PermissionRequest` hook, if a new check in `docs/spike-checklist.md`
  shows it neither delays nor answers the dialog.
- `feat(app): tell when a background agent asks` - a toast naming the
  project and the terminal bell.
- `feat(agent): report the files the agent writes` - the paths of `Edit`,
  `Write` and `MultiEdit` tools that ran, from the `tool_name` and
  `tool_input.file_path` the hook shim keeps of `PostToolUse` events, and
  the status line's `workspace.added_dirs`.
- `feat(app): find the repositories an agent writes in` - `git rev-parse
  --show-toplevel` on the finder thread, cached per directory.
- `feat(ui): list touched repositories under their session` - with the
  same dot; the changes pane follows the repository selected in the list,
  else the session's own, else the one touched last.
- `docs: document sessions in the README and the mockup`.
- `docs: add changelog entry for 0.2.0`.
- `chore(release): v0.2.0`.

Done when: `o` opens a recent project and `n` moves between the two, each
with its own scrollback; a permission prompt in the background session
shows `!` in the list, a toast naming the project and the bell within 2 s
of the dialog (of Claude's permission notice if the hook was dropped);
closing one leaves the other running; after `q` (twice while an agent
works or asks) the agents `pgrep -P <termis pid>` listed before are gone
and no `termis-<pid>` directory is left; a session started outside a
repository that edits a file in one lists that repository, and the
changes pane shows the file.

## After 0.1

The backlog until there is an issue tracker, in no order:

- More agents behind the `Agent` trait (Codex, OpenCode, Gemini CLI), each
  with its launch command and telemetry source, selected with `--agent`.
- Push from the pane, with `-u origin <branch>` when there is no upstream.
- Inline diff preview under the file lists, as drawn in the mockup.
- Hour-by-hour sparkline under the today line, as drawn in the mockup.
- File watcher (`notify`) to cut refresh latency below the 2 s poll, with
  `target/` and `.git/objects` filtered out.
- `claude agents --json` as a state source when hooks are unavailable.
- Desktop notification when Claude is waiting and the window is not focused.
- Hunk and line staging, or handing the pane to lazygit.
- Mouse: click to focus a pane.
- Selecting and copying in the center pane when the agent does not take
  the mouse.
- Config file: the termis-mode key, pane widths, poll interval, theme colours.
- Terminal title from Claude's OSC 0/2 in the pane header.
- Linux support.
- Emulator alternatives behind tui-term's traits: `alacritty_terminal`,
  `libghostty-vt`.
- Ask Claude for a commit subject from the staged diff (`claude -p`).
- Drop `vendor/vt100` once vt100 releases the resize fix (ADR-0007),
  offered upstream when the repository is public.

## Runtime model

Threads; the long-lived ones are named where they start:
- `input-reader` (`src/input.rs`): crossterm `poll` with a 50 ms timeout,
  then `read`; sends host events, resizes included (crossterm handles
  SIGWINCH itself).
- For each agent session (`src/pty.rs`): `pty-reader` sends output in
  chunks of up to 8 KiB, `pty-waiter` waits for the child and marks it
  exited, `pty-writer` writes queued input so the main loop never blocks
  on the child. `pty-forward` (`src/app/mod.rs`) tags the reader's and the
  waiter's events with the session's generation, so a relaunch ignores
  the old session's stragglers.
- `git-worker` (`src/git/worker.rs`), inside a repository only: `Job` in,
  `Event` out, the jobs queued together run as one batch. A commit adds
  short-lived threads that write the message to `git commit` and drain
  its output.

`input-reader`, `pty-forward` and `git-worker` send one `enum Msg` over a
single `std::sync::mpsc` channel. There is no signal thread: SIGHUP and
SIGTERM to `termis` take their default action, which restores nothing
and leaves the instance directory for the next start to sweep.
`--panic-after` adds an unnamed thread that panics on purpose.

The main thread owns the emulator, the agent's PTY session and the host
terminal. Each pass of its loop looks at the telemetry files when 250 ms
passed (`TELEMETRY_EVERY`); queues a git refresh when the side panes are
on screen, none is in flight and 2 s passed since the last one finished
(`POLL_EVERY`); waits up to 16 ms for a message (`TICK`); takes up to 256
more already queued (`DRAIN`); applies them; and draws one frame when
they, the telemetry or a clock may have changed the screen. The clocks
are the session time, the last commit's age, the time left on the plan's
limits, the 5 s a toast lasts (`TOAST_LASTS`) and the 200 ms an exited
agent's output gets to end (`CLOSE_GRACE`). A pass draws at most once;
nothing else limits the frame rate. On quit the agent's process group
gets SIGHUP, then SIGKILL if the agent has not exited after 2 s
(`GRACE`).

Instance directory: `$TMPDIR/termis-<pid>/`, readable by its owner only,
passed to the child as `TERMIS_DIR`. It holds `status.json`, Claude's
last status line input, written by rename; `events.jsonl`, one line per
hook event with its name, ids and time, the tool's name and file and
Claude's notice text, never a prompt, a command or a file's content; and
`statusline.prev`, the status line command the user had, when there was
one. A relaunch clears the first two. Quitting and the panic hook remove
the directory; at start `termis` replaces one left with its own pid and
removes those of this user's processes that are gone. When it cannot be
set up, the agent runs without telemetry and a toast says why.

Logs: none; nothing is written to a log file. While the screen is up,
git errors, a diff that cannot be read and telemetry that cannot be set
up show as toasts in the status bar. An error that ends `termis`, such
as an agent that cannot start, goes to stderr once the terminal is
restored; so does a panic. The `statusline` and `hook` subcommands, run
by Claude, write their own trouble to their stderr.

## Dependencies

| Crate | Role |
|---|---|
| anyhow 1 | errors with context, up to the one `main` prints |
| clap 4 (derive) | CLI: `termis [path] [-- <args>]`, `--spike`, `--panic-after`, the hidden `statusline` and `hook` subcommands |
| crossterm 0.29 | host terminal: raw mode, alternate screen, paste, focus, mouse and keyboard flags; host events for `input-reader`; ratatui's backend |
| nix 0.28 (signal) | hangup and kill of the agent's process group, kill of `git commit`'s; whether an instance directory's process is gone |
| portable-pty 0.9 | the agent's PTY: spawn, resize, wait |
| ratatui 0.30 | layout and drawing of the panes, the status bar and the overlays |
| serde_json 1 | Claude's status line input, hook events and settings; `package.json` and `composer.json` for the stack |
| toml 1 | `Cargo.toml` and `pyproject.toml` for the stack |
| tui-term 0.3 | draws the emulator's screen in the center pane |
| unicode-segmentation 1 | whole graphemes when text is cut to fit and when `Backspace` deletes in the commit box |
| unicode-width 0.2 | column widths for cutting and padding text |
| vt100 0.16 | emulation of the agent's terminal and its scrollback; vendored with a resize fix (ADR-0007) |
| tempfile 3 (dev) | temporary repositories and directories in tests |

## Decisions

Recorded in [docs/adr](docs/adr/README.md). Made now: git through the `git`
binary (0001), threads and channels without an async runtime (0002), Claude
telemetry injected per session (0003), agent-agnostic core with Claude as
the first adapter (0004). Decided by the spike: emulator (0005) and input
model (0006). Found in use: vt100 vendored with a resize fix (0007).
Made for M8: several sessions in one process (0008). The package and the
binary are `termis`; the package is never published.
