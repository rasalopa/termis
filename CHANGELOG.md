# Changelog

All notable changes to this project are documented in this file, written at
release time from the commit log.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/)
and the project follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
While the version is 0.x, a change to the command line, the keys or the
`statusline` and `hook` subcommands bumps the minor version.

## [Unreleased]

## [0.2.0] - 2026-10-22

### Added

- Several projects in one `termis`, each in a session with its own
  Claude Code, PTY, scrollback and agent box. The project's path and
  stack, the agent's box, the changes pane and the project in the status
  bar follow the session shown; the agents not shown keep running and
  follow resizes.
- `Ctrl+Q o` opens a project picker, tagged `OPEN` in the status bar:
  the open sessions, then the directories Claude Code ran in that still
  exist, the last started first, from `~/.claude.json` (in
  `CLAUDE_CONFIG_DIR` when set) or from the legacy `.config.json` in its
  configuration directory, which Claude Code prefers while it is not
  empty. Typing filters them by name or path and `Up`/`Down` choose one;
  text that starts with `/`, `~` or `.`, holds a `/` or matches no row is
  a directory, counted from the home directory unless it is absolute.
- In the picker, `Enter` starts Claude Code in a new session in the
  chosen directory, continuing its last conversation, and `Tab` starts it
  on a new one; on a directory with a session open, both show that
  session. Either way the picker closes and keys go to the agent shown.
  A session opened this way gets none of the arguments given after `--`.
  When a directory cannot be opened, or Claude Code cannot start there,
  a toast says why.
- `n` and `N` in termis mode show the next and the previous session,
  wrapping around, and the status bar shows the position, as in
  `termis · api 2/3`. While the window has the focus, the agent switched
  away from gets a focus-out and the one shown a focus-in, when they
  asked for focus events.
- From two sessions on, the projects pane lists them above `current` and
  the shown project's path and stack. A row has `▸` on the session
  shown, its agent's state glyph, the project's name and `•` when its
  repository has uncommitted changes. The shown repository is read every
  2 s; the others get only `git status`, every 10 s, while the list is
  on screen. With the side panes hidden, the status bar counts the
  sessions not shown that are asking, as `! 2 asking`.
- With the projects pane highlighted, `j`/`k` and `Up`/`Down` move a
  cursor through the list, and `Enter` shows the session under it and
  goes back to agent mode.
- `x` in termis mode closes the session shown, or with the projects pane
  highlighted the one under the list's cursor; once its agent ended, `x`
  in agent mode closes its session, and so does `q` while other sessions
  remain, as the bar on the pane's last row says (`x close · q close`).
  The agent is hung up while the others keep running; when it was the
  session shown, the next one, else the previous one, takes its place;
  closing the last one quits. While the agent it closes is working or
  asking, `x` takes a second press within 3 s, as a toast says.
- An agent not shown that starts asking shows `!` in the list, a toast
  naming its project, such as `! api is asking`, and rings the terminal
  bell, once each time it starts.
- Claude Code gets a `PermissionRequest` hook too, run in the
  background: the agent box shows `! asking` as soon as a permission
  dialog opens, before Claude Code's permission notice.
  `docs/spike-checklist.md` records that the hook neither delays nor
  answers the dialog.
- `termis --new` starts the first session on a new conversation.
- Quitting hangs up every agent at once and takes at most 3 s, however
  many sessions run; a panic hangs up every agent too.

### Changed

- At every launch, a relaunch with `r` included, Claude Code continues
  the last conversation it saved for the directory, as
  `claude --continue` would. `--new` and `Tab` in the picker start a new
  one, and arguments after `--` that already choose one keep their
  choice: `-c`, `-r`, `-p`, their long forms, `--session-id`,
  `--from-pr`, `--teleport` or `--cloud`. Only the names of the files in
  Claude Code's `projects` folder are read.
- `q` in termis mode takes a second press within 3 s while an agent is
  working or asking, as a toast says; it quit at once before.
- The private `termis-<pid>` directory holds one directory per session,
  named after the session's id, and `TERMIS_DIR` names the session's
  directory. A closed session's directory goes once its agent is hung
  up.
- When `git` cannot be started to find a directory's repository, at
  start or for the picker, the error says `git is not installed` when
  git is missing and the directory exists, and otherwise names the
  directory, as in `run git in <directory>`; it said `run git` before.

### Fixed

- A relaunch with `r` that cannot start no longer ends `termis`: the
  session stays ended, its agent box as it was, and a toast says why.
- A PTY that refuses a resize no longer ends `termis`; a toast names the
  project.

## [0.1.0] - 2026-10-13

The first release with a changelog: this section covers everything up to
0.1.0, the v0.0.1 build included.

### Added

- `termis [path] [-- <args>]` runs Claude Code on a PTY in the center
  pane, in `path` or the current directory, with the arguments after
  `--`; `termis -- -c` resumes the last session. `--spike` gives the
  agent the whole terminal.
- `termis` is the terminal Claude Code talks to: it answers its queries
  (device attributes, cursor position, kitty keyboard flags, colours),
  encodes keys as it asked (legacy, modifyOtherKeys or kitty), and
  passes on pastes, focus changes and resizes. Claude Code runs with
  `TERM=xterm-256color` and `COLORTERM=truecolor`, without the host
  terminal's, tmux's or a parent Claude session's variables, such as
  `TERM_PROGRAM`, `ITERM_SESSION_ID` and `CLAUDECODE`, and does not set
  the terminal title.
- Three panes over a status bar with the mode, the main keys for the
  highlighted pane, toasts and the project. Below 100 columns the side
  panes fold away; on a wider terminal `z` does the same.
- `Ctrl+Q` enters termis mode and leaves it. In termis mode `1`/`2`/`3`,
  `Tab` and `Shift+Tab` highlight a pane, `q` quits, `Esc` returns to
  the agent, and `?` lists every key, grouped by mode and pane. `Ctrl+Z`
  never reaches the agent, which would try to suspend itself and wait
  forever.
- After the agent exits, its pane keeps the last screen with a line
  saying how it ended; `r` relaunches the agent and `q` quits. `termis`
  then prints how the agent ended and exits with its code, or 1 when a
  signal ended it. Quitting hangs up the agent's process group and kills
  it after 2 s. The host terminal is restored on quit, on an error and on
  a panic.
- The changes pane: the branch in the title, with the upstream's remote
  and how many commits the branch is ahead and behind in the longest
  form that fits; the staged and unstaged files with their change letter
  and the lines added and removed, read again every 2 s while the side
  panes show. Outside a repository it says `not a git repository` and
  the agent runs anyway.
- With the changes pane highlighted in termis mode, `j`/`k` move the
  selection; `s`, `u` and `a` stage, unstage and stage everything with
  the `git` binary; `d` shows the diff of the selected file over the
  whole column, its staged or unstaged side or an untracked file whole,
  which `j`/`k` scroll and `d` or `Esc` close.
- `c` opens a commit box while the side panes show and something is
  staged: the first line is the subject, `Ctrl+J` starts a body line,
  `Enter` commits with `git commit`, hooks included, with no terminal
  and a 60 s limit, and `Esc` closes the box keeping the draft. A failed
  commit brings the draft back.
- A today section in the changes pane: your commits since midnight and
  their lines, with merges, lockfiles and binaries left out; then the
  last commit with its hash, age and subject.
- Toasts in the status bar for five seconds: the short hash of a commit,
  the line git or a hook printed when a commit, a stage or unstage, a
  diff or a refresh failed, or why `c` or `Enter` was refused.
- The agent pane's history, up to 5000 lines: in termis mode with the
  pane highlighted, `j`/`k` scroll a line, `PgUp`/`PgDn` a page and `G`
  goes back to the end, and the title says how far back the view is.
  There is none while the agent is on its alternate screen, such as
  Claude Code's fullscreen view, which scrolls itself.
- The mouse wheel scrolls the agent pane's history or, in agent mode,
  sends arrow keys to an agent on its alternate screen that did not ask
  for the mouse. Over the changes pane it moves the selection or scrolls
  the diff.
- An agent that asks for the mouse, such as Claude Code's fullscreen
  view, gets the wheel, clicks, drags and motion over the agent pane, so
  it scrolls its transcript and selects text itself.
- `termis` turns on mouse reporting in the host terminal: selecting text
  with the terminal takes Option while dragging in iTerm2, unless the
  profile does not report clicks and drags, and Shift in kitty, WezTerm
  and Alacritty.
- The projects pane shows the project's path and its stack from
  `Cargo.toml`, `pyproject.toml`, `go.mod`, `composer.json` or
  `package.json`: the language, and a known framework with the version
  the manifest asks for.
- An agent box at the foot of the projects pane: the state (`✶ working`,
  or `✶` and the tool it runs with its file, such as `✶ Edit main.rs`;
  `· waiting`; `! asking`; `× ended`), model, effort, context in use, the
  plan's five-hour and weekly limits with the time to reset (the cost
  without a plan), lines changed and session time.
- Claude Code gets a status line and hooks for the session through one
  `--settings`, merged with a `--settings` given after `--`. They run
  the hidden `termis statusline` and `termis hook` subcommands; the
  status line configured before still runs, and no settings file
  changes.
- The session's status and events live in a private `termis-<pid>`
  directory under the temporary directory, removed on quit and on a
  panic; those of dead processes go at the next start. When it cannot be
  made or prepared, the agent runs without its box and a toast says why.
- An exported `GIT_DIR`, `GIT_WORK_TREE` or `GIT_INDEX_FILE` does not
  redirect the git commands `termis` runs.

### Fixed

Since the v0.0.1 build:

- Quitting while the agent runs no longer reports the exit that the
  quit's own hangup caused, such as `agent exited with code 129`, nor
  ends `termis` with its code.
- A terminal that reports no size (0x0), or a center pane one row high
  or one column wide, no longer makes `termis` panic at the agent's
  output.
- Narrowing the center pane across a wide character, such as an emoji,
  by resizing the window or with `z`, no longer makes `termis` panic
  when the agent writes there next.
