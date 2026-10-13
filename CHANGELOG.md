# Changelog

All notable changes to this project are documented in this file, written at
release time from the commit log.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/)
and the project follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
While the version is 0.x, a change to the command line, the keys or the
`statusline` and `hook` subcommands bumps the minor version.

## [Unreleased]

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
