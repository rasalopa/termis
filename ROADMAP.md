# Roadmap

`termis` is a three-pane terminal workspace for working with a coding agent:
the current project on the left, a live agent session in the middle, and
the git changes (files, lines, diff, commit) on the right. Claude Code is
the first supported agent; the core does not depend on it (ADR-0004).
See [docs/mockup.md](docs/mockup.md) for the target screen.

This file is the plan for the first usable release, v0.1.0, and for the
milestones after it. The release is ordered by risk:
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
  `Ctrl+C` for Claude. `Ctrl+Z` never reaches the agent: Claude would try
  to suspend itself and wait forever. A terminal gets it, unless it runs
  the editor. `Alt+1` to `Alt+9` show a session and reach neither the
  agent nor a terminal.
- The center pane keeps up to 5000 lines that scrolled off the agent's
  screen, read with the wheel or in termis mode. An agent that asks for
  the mouse gets the wheel and the clicks and drags it asks for over the
  center pane instead: Claude's fullscreen view scrolls, selects and
  copies with them. Where the agent does not take the mouse, a drag over
  the center pane selects text and `termis` copies it with `pbcopy`;
  Option while dragging in iTerm2 still selects with the host. Claude's
  transcript lives under `~/.claude/projects`.
- Launch: `termis [--new] [--no-restore] [path] [-- <claude args>]`.
  Claude continues the last conversation it saved for the directory;
  `--new` starts a new one. The sessions open when `termis` last ran
  open again around it, unless `--no-restore`.
- Outside a git repository Claude still runs, and the right pane stays
  hidden until the agent writes in a repository; it then follows the
  one written last. If `claude` is not on the `PATH`, `termis` restores
  the terminal and exits with status 1; stderr says
  `Error: start claude`, then the causes, down to the `PATH` that was
  searched.

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
- `refactor(app): hang up every agent at once through deadlines` - at
  quit, draining the channel once the loop has ended.
- `refactor(app): run every session's timers, resizes and output`.
- `fix(app): keep running when a relaunch or resize fails`.
- `feat(agent): continue the project's last conversation` - on every
  launch, relaunches included, the adapter adds `--continue` when Claude
  has a conversation saved for the directory and the arguments after
  `--` do not already choose one (`-c`, `--continue`, `-r`, `--resume`,
  `--session-id`, `--from-pr`, `--teleport`, `--cloud`, `-p`,
  `--print`). Claude saves it as a `*.jsonl` file directly in
  `projects/<key>` of its configuration directory, where the key is the
  canonical path with every UTF-16 unit outside `[A-Za-z0-9]` made `-`,
  cut at 200 with a hash after it when longer; the check looks at file
  names and never reads them. `termis --new` starts a new conversation
  on the first launch. A second `termis` or `claude --continue` in the
  same directory continues the same conversation; the README says so.
  The changed default goes under Changed in 0.2.0.
- `feat(agent): give each session its own instance directory` -
  `termis-<pid>/<n>`; the sweep is unchanged.
- `feat(git): find repository roots off the main thread`.
- `feat(agent): list the agent's recent projects` - for Claude, the
  `projects` keys that still exist on disk, from `~/.claude.json` (in
  `CLAUDE_CONFIG_DIR` when set), or from the legacy `.config.json` in its
  configuration directory, which Claude prefers when it is not empty.
- `feat(ui): add a project picker` - the recent projects and the open
  sessions, filtered as you type, a directory with an open session listed
  once, as that session; a typed path is made canonical before it is
  matched or used.
- `feat(app): open a project in a new session with o` - `Ctrl+Q o` opens
  the picker. On an open session `Enter` or `Tab` shows it; elsewhere
  `Enter` launches the agent, continuing the project's last conversation
  when there is one, and `Tab` starts a new one; `Tab` never completes a
  path. The card, the agent's box, the changes pane and the project in the
  status bar follow the session shown. Sessions in the background keep
  reading their agent's output, answering its queries and following
  resizes, and get a focus-out.
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
- `docs: document sessions in the README and the mockup`.
- `docs: add changelog entry for 0.2.0`.
- `chore(release): v0.2.0`.

Done when: reopening a project with `termis` continues its last
conversation; `o` opens a recent project and `n` moves between the two,
each with its own scrollback; a permission prompt in the background session
shows `!` in the list, a toast naming the project and the bell within 2 s
of the dialog (of Claude's permission notice if the hook was dropped);
closing one leaves the other running; after `q` (twice while an agent
works or asks) the agents `pgrep -P <termis pid>` listed before are gone
and no `termis-<pid>` directory is left.

### M9 - Terminals (M)

After v0.2.0; it ends with v0.3.0.

Goal: run commands and servers for a project without leaving `termis`. A
terminal is a tab of a session, next to its agent's tab in the center
pane, running the user's shell in the session's directory; tabs that are
not shown keep running.

Commits:
- `fix(app): kill what is left of an agent's process group` - once the
  agent exited, closing its session, quitting and a relaunch send its
  group SIGKILL, also when its exit closed the terminal, so a child it
  left there that ignores the hangup goes with it.
- `docs(adr): record terminals as tabs of a session` - a session owns its
  agent's tab and its terminal tabs; a terminal is a PTY, an emulator and
  its scrollback, with no agent, telemetry, instance directory or
  relaunch. `n`/`N`, the session list, the count of sessions asking and
  "closing the last one quits" go by session, and closing a session
  closes its terminals. Each terminal costs three threads and up to about
  20 MB of scrollback, and the panic hook hangs up its shell too. A
  terminal is hung up through the shell's process group and the PTY's
  foreground process group, read at the hangup, then both are killed
  after `GRACE`, so a server running in the shell's foreground stops
  with it.
- `refactor(app): move a PTY and its emulator into a tab` - a session's
  agent becomes its first tab. No visible change.
- `feat(app): open a terminal in the project with t` - `Ctrl+Q t` starts
  the user's shell (`$SHELL`, else `/bin/sh`) as a login shell in the
  session's directory, shows its tab and goes back to agent mode so keys
  reach it; `,` and `.` in termis mode show the previous and the next tab,
  and the center pane's title names the tabs, the shown one marked. The
  tab closes when its shell exits. `Ctrl+Z` reaches a terminal, unlike
  the agent.
- `feat(app): close the tab shown with x` - on a terminal it hangs the
  shell up; on the agent's tab it closes the session and its terminals.
  With the projects pane focused, `x` closes the selected row. While a
  program other than the shell runs in a terminal, `x` takes a second
  press, and so does `q`.
- `feat(ui): list terminals under their session` - in the projects pane.
- `docs: document terminals in the README and the mockup` - with
  `Ctrl+Z` reaching terminals.
- `docs: add changelog entry for 0.3.0`.
- `chore(release): v0.3.0`.

Done when: `Ctrl+Q t` opens a shell in the project; a server started there
keeps printing while the agent's tab is shown, and its output is there
when its tab comes back; after closing the tab, and after `q`, `ps -t
<tty>` lists nothing and the server's port is free.

### M10 - Files (M)

After v0.3.0; it ends with v0.4.0.

Goal: browse the project's files in the left pane, read one in the center
pane, and open it in the user's editor.

Commits:
- `feat(git): list the repository's files` - a job on the repository's
  worker: the files git tracks that still exist and the untracked ones it
  does not ignore, up to 20,000.
- `feat(projects): list a directory's files` - outside a repository, a
  walk off the main thread that skips hidden directories, up to the same
  limit.
- `feat(ui): add a file tree to the projects pane` - `f` switches the
  left pane between the list of sessions and the tree; directories fold;
  `j`/`k` move, `l` or `Enter` opens a directory and `h` closes it;
  changed files carry their status letter. While the tree is shown, the
  status bar counts the sessions asking, as when the list is hidden.
- `feat(ui): view a file in the center pane` - a tab next to the agent's,
  read-only, with line numbers and scroll, showing the file as it was
  when opened; control characters show as in the changes pane and tabs
  are expanded; a binary file or one over 1 MiB is refused with a note.
  `x` closes it at once.
- `feat(app): open a file in the editor with e` - a terminal tab (M9)
  runs `$VISUAL`, else `$EDITOR`, through `sh -c`, read as shell words
  as git reads it, so `code --wait` works; the path reaches it only as
  `"$1"`, one argument whatever it holds. With neither variable set, a
  toast says so. The tab closes when the editor exits, and an exit
  status other than 0, as from an editor that cannot start, shows in a
  toast.
- `docs: document the file tree and the viewer in the README and the
  mockup`, which covers the editor too: `e`, `VISUAL` and `EDITOR`, and
  its toasts.
- `docs: add changelog entry for 0.4.0`.
- `chore(release): v0.4.0`.

Done when: `f` shows the project's tree without the ignored files; `Enter`
on a file shows it in the center pane, and a binary file is refused; `e`
opens it in the editor in a terminal tab, and a change saved there shows
in the changes pane.

## After 0.1

The backlog until there is an issue tracker, in no order:

- More agents behind the `Agent` trait (Codex, OpenCode, Gemini CLI), each
  with its launch command and telemetry source, selected with `--agent`.
- Inline diff preview under the file lists, as drawn in the mockup.
- Hour-by-hour sparkline under the today line, as drawn in the mockup.
- File watcher (`notify`) to cut refresh latency below the 2 s poll, with
  `target/` and `.git/objects` filtered out.
- `claude agents --json` as a state source when hooks are unavailable.
- Hunk and line staging, or handing the pane to lazygit.
- Mouse: click to focus a pane.
- Config file: the termis-mode key, pane widths, poll interval, theme colours.
- Terminal title from Claude's OSC 0/2 in the pane header.
- Linux support.
- Emulator alternatives behind tui-term's traits: `alacritty_terminal`,
  `libghostty-vt`.
- Drop `vendor/vt100` once vt100 releases the resize fix (ADR-0007),
  offered upstream when the repository is public.

## Runtime model

Threads; the long-lived ones are named where they start:
- `input-reader` (`src/input.rs`): crossterm `poll` with a 50 ms timeout,
  then `read`; sends host events, resizes included (crossterm handles
  SIGWINCH itself).
- For each tab of a session, the agent's or a terminal's
  (`src/pty.rs`): `pty-reader` sends output in chunks of up to 8 KiB,
  `pty-waiter` waits for the child and marks it exited, `pty-writer`
  writes queued input so the main loop never blocks on the child. The
  reader and the waiter tag their events with the session's id, the
  tab's and the generation of its launch, so an event for a tab no
  longer there reaches nothing and a relaunch ignores the old launch's
  stragglers.
- `git-worker` (`src/git/worker.rs`), one per repository a session
  uses: `Job` in, `Event` out, the jobs queued together run as one
  batch. It tags its events with its repository's id, so an event for a
  repository no longer in use reaches nothing. A commit adds short-lived
  threads that write the message to `git commit` and drain its output.
- `git-push` (`src/git/push.rs`), short-lived: one per push, beside its
  repository's worker so that the reads go on, answering with the
  repository's id; it adds short-lived threads that drain the output of
  `git push`.
- `agent-subject` (`src/agent/subject.rs`), short-lived: one per subject
  `Ctrl+G` asks the agent for, one at a time per repository, answering
  with the repository's id. It reads the last subjects and what is
  staged through git, then runs the agent in a process group of its
  own, killed after 60 s, adding short-lived threads that write the
  prompt to it and drain its output. A commit sent before the answer,
  the repository let go and the quit stop it: the group is killed at
  once.
- `git-finder` (`src/git/find.rs`): finds the repository of a directory
  chosen in the project picker or written in by an agent, answering with
  the directory it was asked about.
- `file-walker` (`src/projects/walk.rs`), short-lived: one per walk of
  the directory of a session outside a repository, for its file tree,
  answering with the directory it walked.
- `file-reader` (`src/projects/read.rs`), short-lived: one per file a
  session's viewer reads, which reads at most 1 MiB and a byte and
  splits the text into lines to draw, answering with the session, the
  viewer's tab and the number of the read, so an answer no viewer waits
  for any more reaches nothing.
- `clipboard-writer` (`src/app/copy.rs`), short-lived: one per text
  selected in the center pane, at the release, which runs
  `/usr/bin/pbcopy` in a process group of its own, killed after 5 s,
  adding short-lived threads that write the text to it and drain its
  errors, and answers with the notice saying what was copied, or why
  not. `pbcopy` gets `LC_ALL=en_US.UTF-8`, whatever the user's locale,
  so it reads the text as UTF-8. One runs at a time; one whose turn
  comes after a later selection ends without a copy or an answer, so
  the last text selected is the one left on the clipboard. A copy still
  running 500 ms after the quit is killed with its group, and those
  waiting end without a copy.

`input-reader`, `pty-reader`, `pty-waiter`, `git-worker`, `git-push`,
`agent-subject`, `git-finder`, `file-walker`, `file-reader` and
`clipboard-writer` send one `enum Msg` over a single `std::sync::mpsc`
channel. There is no signal thread: SIGHUP and SIGTERM to `termis`
take their default action, which restores nothing and leaves the
instance directory for the next start to sweep, and the state file as
it was last written.
`--panic-after` adds an unnamed thread that panics on purpose.

The main thread owns the emulator and PTY of every tab with a child,
the agent's or a terminal's, the text of every viewer, and the host
terminal. Each pass of its loop looks at the telemetry files when 250 ms
passed (`TELEMETRY_EVERY`), with a toast and the terminal bell when an
agent not shown starts asking, and, while the host window does not have
the focus, OSC 9 to the host terminal, a desktop notification, when any
agent starts asking or finishes its turn; queues a refresh of the
repository the shown session's changes pane shows, its own or one its
agent wrote in, and of its own while its file tree is on screen, when
the side panes are on screen, the changes pane among them or not, no
read of it is in flight and 2 s passed since the last refresh finished
(`POLL_EVERY`), at once when the session was just shown,
the pane follows another repository or the side panes
come back, and a read of the status alone of every other
repository whose dot the list of sessions has, when that
list is on screen, none is in flight and 10 s passed
since its status was last read (`STATUS_EVERY`); asks for the files of
the shown session's directory, its repository's worker listing them or
else a walk of the directory, while its file tree is on screen, when the
tree comes on screen and after a refresh that found the status changed,
with none in flight and 2 s since the last answer (`FILES_EVERY`); waits
up to 16 ms for a message (`TICK`); takes up to 256 more already queued
(`DRAIN`); applies them; looks up the repository of each directory an
agent wrote in, in the order written, among those the finder found,
asking it about the first one not found yet, and starts the worker of
one listed that no session used; closes each terminal whose shell exited
once its output ended or had 200 ms, after SIGKILL to what is left of
the shell's process group; moves on the hangup of each session closed
with `x`, or with `q` after its agent ended, and of each terminal closed
with `x`; shows the changes pane while the repository it shows has a
file not committed or a commit `p` would push, or while termis mode
highlights the pane, its commit box or a diff is open, or a commit, a
push or a subject of that repository runs, and for 10 s after
(`HIDE_AFTER`), a session or repository newly shown, or the side panes
back, going by the state of the repository once refreshed and the pane
staying as it was until then, and resizes every tab when that changes
the center's width; writes the state file when the sessions open differ
from what it keeps; and draws one frame when they, the telemetry or a
clock may have changed the screen; after the end of a push or of the
agent asked for a subject, the whole screen, over what a hook, ssh or
the agent may have written on the terminal.
The clocks are the session time, the last commit's age, the time left on
the plan's limits, the 5 s a toast lasts (`TOAST_LASTS`), 3 s for the
one asking for a second `x`, `q` or `p` (`CONFIRM_WITHIN`), the 10 s
the changes pane stays once nothing needs it (`HIDE_AFTER`), and the
200 ms an exited child's output gets to end (`CLOSE_GRACE`). A pass
draws at most once; nothing else limits the frame rate. An agent is
hung up through its process group: SIGHUP, then SIGKILL if the agent
has not exited after 2 s (`GRACE`). Once the agent exited, the group gets
SIGKILL, so nothing the agent left in it outlives the session; when the
agent is still not reaped 500 ms after its kill (`KILL_GRACE`), the
group gets SIGKILL again if something in it still holds the terminal.
Either way the group gets 500 ms more to let go of the terminal. A
relaunch sends SIGKILL to what is left of the last launch's group. A
terminal's shell is hung up the same way. Before that, `tcgetpgrp` on
the PTY's master reads its foreground group; when that is not the
shell's, it gets SIGHUP with the shell and SIGKILL after `GRACE` unless
it is gone, and neither when a process with its id runs in another
session (`getsid`). A closed session, or a terminal closed with `x`,
goes through these deadlines on the passes of the loop while the others
keep running; once done, its PTYs go, with a session's instance
directory, and so does each repository that no session uses and that
has no commit or push in flight, which waits for its end. On quit
every agent and every terminal is hung up at once, once the loop has
ended, draining the channel without drawing and moving each hangup on
by its deadlines; an exit of the shown agent that came before the quit
gets 500 ms within them to arrive. The agents asked for a subject are
stopped first, and their threads get 500 ms from the quit
(`STOP_GRACE`) to kill them. At most 3 s, however many sessions and
terminals there are.

Instance directories: `$TMPDIR/termis-<pid>/` for the process and
`<n>/` in it for the session with id `n`, both readable by their owner
only; a session's is passed to its agent as `TERMIS_DIR`. It holds
`status.json`, Claude's last status line input, written by rename;
`events.jsonl`, one line per hook event with its name, ids and time, the
tool's name and file and Claude's notice text, never a prompt, a command
or a file's content; and `statusline.prev`, the status line command the
user had, when there was one. A relaunch clears the first two. A closed
session's goes once its agent and its terminals are hung up. Quitting
and the panic hook remove the process's directory with everything in
it; at start `termis` replaces one left with its own pid and removes
those of this user's processes that are gone. When the process's
directory cannot be set up, every agent runs without telemetry; when a
session's cannot, its agent does; either way a toast says why.

State file: `$XDG_STATE_HOME/termis/sessions.json`, else
`$HOME/.local/state/termis/sessions.json`, none when neither variable
is set or with `--spike`: a version and the directories of the sessions
open, in the order of the list, as JSON. The main thread reads it once
at start, unless `--no-restore`, and finds the repository of each saved
directory with git before the screen is taken, as it does for the
directory asked for. It writes the file on each pass where the
sessions open differ from what it keeps: on the first when it was not
read, with `--no-restore` or a file it could not read, when a saved
session did not open, or when the one asked for was not saved; then as
the set of sessions changes; and with none once `x` closed the last
session. It writes through a temporary file readable by its owner
only and a rename, without a sync: under a kilobyte for ten sessions, a
fraction of a millisecond. The directories it makes for the file are
readable by their owner only.

Logs: none; nothing is written to a log file. While the screen is up,
git errors, a diff that cannot be read, telemetry that cannot be set
up, a state file that cannot be read or written, a saved session that
cannot open again, an agent that cannot start again or writes no
commit subject, a project that cannot be opened, a repository an agent
wrote in whose worker cannot start, a shell that cannot start in a
terminal, an editor that exits with a status other than 0 or cannot
start, a copy to the clipboard that fails, and a PTY that refuses a
resize show as toasts in the status bar; a status read for the list of
sessions that fails shows nothing, and so does a directory an agent
wrote in whose repository git cannot tell, which is let go; files that
cannot be listed say why in the file tree, and a file that cannot be
read says why in the viewer. An error that ends `termis`, such as an
agent that cannot start the first time, goes to stderr once the
terminal is restored; so does a panic. The `statusline` and `hook`
subcommands, run by Claude, write their own trouble to their stderr.

## Dependencies

| Crate | Role |
|---|---|
| anyhow 1 | errors with context, up to the one `main` prints |
| clap 4 (derive) | CLI: `termis [--new] [--no-restore] [path] [-- <args>]`, `--spike`, `--panic-after`, the hidden `statusline` and `hook` subcommands |
| crossterm 0.29 | host terminal: raw mode, alternate screen, paste, focus, mouse and keyboard flags; host events for `input-reader`; ratatui's backend |
| nix 0.28 (process, signal) | hangup and kill of the process group of the agent or a terminal's shell, and of a terminal's foreground group after `getsid` tells it is not a stranger's; kill of the process groups of `git commit`, `git push`, the agent asked for a commit subject, `pbcopy`, and `git diff --cached` once the patch passes what the agent reads; whether an instance directory's process is gone; through its `libc`, the flag that opens a file for the viewer without waiting for a pipe's writer |
| portable-pty 0.9 | the PTYs of the agents and the terminals: spawn, resize, wait, and the foreground group (`tcgetpgrp`) |
| ratatui 0.30 | layout and drawing of the panes, the status bar and the overlays |
| serde_json 1 | Claude's status line input, hook events and settings; `package.json` and `composer.json` for the stack; the state file |
| toml 1 | `Cargo.toml` and `pyproject.toml` for the stack |
| tui-term 0.3 | draws the emulator's screen in the center pane |
| unicode-segmentation 1 | whole graphemes when text is cut to fit, when a subject the agent wrote is cut to 100, when `Backspace` deletes in the commit box or the picker's query, when the viewer's lines are split into cells to select, and when a copy counts its characters |
| unicode-width 0.2 | column widths for cutting and padding text, and for the cells of the viewer's lines |
| vt100 0.16 | emulation of each tab's screen, the agent's or a terminal's, and its scrollback; vendored with a resize fix (ADR-0007) |
| tempfile 3 (dev) | temporary repositories and directories in tests |

## Decisions

Recorded in [docs/adr](docs/adr/README.md). Made now: git through the `git`
binary (0001), threads and channels without an async runtime (0002), Claude
telemetry injected per session (0003), agent-agnostic core with Claude as
the first adapter (0004). Decided by the spike: emulator (0005) and input
model (0006). Found in use: vt100 vendored with a resize fix (0007).
Made for M8: several sessions in one process (0008). Made for M9:
terminals as tabs of a session (0009). The package and the binary are
`termis`; the package is never published.
