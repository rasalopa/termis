# Changelog

All notable changes to this project are documented in this file, written at
release time from the commit log.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/)
and the project follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
While the version is 0.x, a change to the command line, the keys or the
`statusline` and `hook` subcommands bumps the minor version.

## [Unreleased]

## [0.5.0] - 2026-10-27

### Added

- `p` in termis mode pushes the branch, at a second `p` within 3 s.
- `Ctrl+G` in the commit box asks Claude for a subject.
- The sessions of the last run open again at start; `--no-restore`
  starts with the one session asked for.
- The list of sessions shows the other repositories an agent writes in,
  as `↳ api`; `Enter` on one has the changes pane follow it, and `x`
  takes it off the list.
- A session outside any repository shows in the changes pane the
  repository its agent wrote in last.
- A desktop notification through OSC 9 when an agent asks or finishes
  its turn while the window does not have the focus.
- `Ctrl+D`, `Ctrl+U`, `Ctrl+F`, `Ctrl+B`, `g` and `G` page and jump
  wherever `j` and `k` move or scroll; the diff also takes `PgUp` and
  `PgDn`.
- `i` in termis mode goes back to agent mode.

### Changed

- The changes pane hides while the repository has nothing to commit or
  push.
- The status bar shows `p push`, and `Ctrl+G suggest` in the commit
  box; the key list (`?`) has the new keys.
- The user guide moved from the README to `docs/guide.md`.
- `termis hook` keeps a tool's `notebook_path`, as it keeps `file_path`.

### Fixed

- An untracked link to a pipe no longer stops the changes pane.
- The key list (`?`) fits every row in 80 columns.

## [0.4.0] - 2026-10-25

### Added

- A file tree: `f` in termis mode, while the side panes are shown, puts
  the file tree of the session shown in the projects pane, in place of
  the list of sessions and the project's path and stack, under a
  heading with the project's name; `f` again brings them back, and each
  session keeps its choice. The tree lists the files of the directory
  the session's agent runs in: inside a repository, the files git
  tracks that are still there and the untracked ones it does not
  ignore, as `git ls-files --cached --others --exclude-standard` lists
  them. Outside one, a walk of the directory lists its files, skipping
  the directories whose names start with `.` and not following links to
  directories.
- The tree holds up to 20,000 files; a longer list keeps those with the
  fewest directories above them, and its last row says
  `cut at 20000 files`. In each directory, directories come first and
  start folded, then files, each by name. A changed file shows its
  status letter, drawn as in the changes pane, and a directory that
  holds a change a grey `●`. The tree says `reading files` until its
  first answer, `no files` when there are none, and `files unavailable:`
  with the reason when the files cannot be listed.
- The files are read when the tree comes on screen and, inside a
  repository, again after a refresh finds the status changed, at most
  once every 2 s after the last answer and never while the tree is
  hidden. A list read again keeps the folds and the cursor.
- With the projects pane highlighted and the tree shown, `j`/`k` and
  `Up`/`Down` move the tree's cursor, `l` or `Enter` opens a directory,
  and `h` folds it or goes to the directory above. While the tree is
  shown, the status bar counts the sessions not shown that are asking,
  as it does with the side panes hidden.
- A viewer: `Enter` on a file in the tree shows it in the session's
  viewer tab, read-only, with line numbers, as it was when read, and
  termis mode stays; `Enter` on it again reads it again. The first file
  viewed opens the tab after the session's other tabs, and the next
  ones take its place. The center pane's title and the list of sessions
  name it after its file, and `Enter` on its row in the list shows it.
- The viewer shows a regular file, once links are followed, of up to
  1 MiB. In place of the text it says `binary file, not shown` when the
  first 8,000 bytes hold a NUL byte, `larger than 1 MiB, not shown`,
  `not a regular file, not shown`, `empty file`, or `file unavailable:`
  with the reason. Tabs show as four spaces, control characters and
  bytes that are not UTF-8 as `�`, and long lines are cut.
- While the viewer is shown, in termis mode with the agent pane
  highlighted and in agent mode, `j`/`k` and `Up`/`Down` scroll the
  file a line, `PgUp`/`PgDn` a page, `G` to its end and `g` to its top;
  the wheel scrolls it too. In agent mode the status bar's tag is
  `view`, and other keys and pastes are dropped.
- `e` in termis mode opens a file in the user's editor: the file under
  the tree's cursor with the projects pane highlighted, else the one the
  viewer shows. A terminal tab after the session's other tabs runs
  `$VISUAL`, else `$EDITOR`, through `/bin/sh -c`. The variable is read
  as shell words, as git reads it, so `code --wait` works, and the
  file's absolute path follows as one argument, whatever it holds. The
  tab runs in the session's directory and is named after the editor and
  the file, as `nvim main.rs`, and `termis` goes back to agent mode.
  With neither variable set, a toast says
  `neither VISUAL nor EDITOR is set`.
- The editor's tab closes when the editor exits. An exit status other
  than 0, as from an editor that cannot start, shows in a toast, such as
  `api: nvim main.rs exited (code 127)`, and so does a kill by a signal,
  as `api: nvim main.rs was killed by SIGSEGV`. While the editor runs,
  its row in the list has `✶`, and `x` on its tab or its session, and
  `q`, take a second press within 3 s; `Ctrl+Z` does not reach it.
- An agent behind the viewer of the session shown that starts asking
  gets the toast naming its project and the bell, as one behind a
  terminal does.

### Changed

- The termis-mode hints show `f files` while the side panes are shown,
  `f sessions` while the tree is, and `e edit` while `e` has a file to
  open.
- `,` and `.` in termis mode show the previous and the next tab while
  the session has any besides the agent's, the viewer included; they
  needed a terminal before.
- `x` in termis mode also closes the viewer, at once, from its tab or
  its row in the list; when it was shown, the tab before it takes its
  place.
- The key list (`?`) has `f`, `e`, `g` and the tree's `l` and `h`, and
  its `x` row names the viewer. `Ctrl+Z` reads
  `reaches a terminal, not the agent or editor`.

## [0.3.0] - 2026-10-25

### Added

- Terminals: `Ctrl+Q t` starts the user's shell (`$SHELL` when it is an
  absolute path, else `/bin/sh`) as a login shell in the directory the
  session's agent runs in. It opens in a tab of the center pane after
  the session's other tabs, and `termis` goes back to agent mode, so
  keys go to it. The shell gets `TERM=xterm-256color` and
  `COLORTERM=truecolor` and none of the host terminal's variables, such
  as `TERM_PROGRAM`. When it cannot start, a toast says why. Tabs not
  shown keep running, and each terminal keeps up to 5000 lines of
  history, scrolled as the agent's are. A program in a terminal that
  asks for the mouse gets the wheel and the clicks over the center pane,
  as the agent does; on its alternate screen without the mouse, the
  wheel sends it arrow keys in agent mode.
- A terminal is named after its shell, as `zsh`; when another terminal
  of its session has that name, it gets the lowest number from 2 on that
  none has, as `zsh 2`. The center pane's title names the session's
  tabs, the agent's by its project, as in `claude · api   zsh   zsh 2`,
  the one shown in bold; when they do not fit, the terminals not shown
  give way to a count, such as `+2`. In agent mode the status bar's tag
  names the terminal shown.
- `,` and `.` in termis mode show the previous and the next tab of the
  session shown, wrapping around. While the window has the focus, the
  tab hidden gets a focus-out and the one shown a focus-in, when they
  asked for focus events. A session shown again with `n`, `N` or the
  picker shows the tab it showed last.
- `Ctrl+Z` in agent mode reaches a terminal, where it stops the program
  in the foreground; it still never reaches the agent.
- A terminal's tab closes when its shell exits, after a kill of what the
  shell left in its process group, and the tab before it takes its
  place.
- `x` in termis mode closes the terminal shown, and with the projects
  pane highlighted the terminal under the list's cursor: its shell and
  the program in its foreground get SIGHUP, and SIGKILL 2 s later if
  they still run. On the agent's tab, `x` closes the session with its
  terminals, and quitting hangs up every terminal the same way. A job
  sent to the background with `&` or `bg` gets only the hangup the shell
  passes on, so one that ignores it, as under `nohup`, outlives the tab.
  A panic hangs up every terminal's shell too.
- While a program other than the shell runs in a terminal, `x` on that
  terminal or on its session takes a second press within 3 s, as a toast
  says, such as `api · zsh runs a program · press x again to close it`.
- From two sessions on, the projects pane lists each session's
  terminals under it, with `✶` while a program other than the shell
  runs, and `▸` marks the row of the tab shown. The list's cursor moves
  over them, and `Enter` shows the terminal under it.
- An agent behind a terminal of the session shown that starts asking
  gets the toast naming its project and the bell, as one in a session
  not shown does.

### Changed

- Only the highlighted pane has a frame, rounded and teal, with its
  title on it; the others show their title in the margin, and a faint
  rule parts two panes without a frame. A title has the pane's name,
  muted unless the pane is highlighted, and its subject in bold. The
  side panes have a column of padding.
- The colours mark what things are: teal the highlighted pane, the
  `TERMIS`, `COMMIT` and `OPEN` tags and a working agent; amber an agent
  that asks and a share running out; red a failure; green and red lines
  added and removed; blue a modified file; grey labels, headings, the
  names of the panes not highlighted and what the hints' keys do. They
  are chosen for a dark background; on a light one the amber, green and
  teal text is hard to read.
- The changes pane colours status letters and line counts by their
  change: `A` and `?` green, `D` red, `M`, `R`, `T` and `C` blue, `U`
  red. Each list has a blank line before it and its length flush right,
  in place of `staged (2)`, and the today section has a heading with a
  rule and its line counts flush right. The diff view's added and
  removed lines take the same green and red. An empty pane says
  `✓ no changes`.
- In the list of sessions, the name of a session whose agent asks is in
  bold amber, followed by `asking` when there is room, and uncommitted
  changes show as a blue `●` in place of `•`. Waiting shows as `○` in
  place of `·`, in the list and in the agent's section.
- The agent's box is a section under a heading with the agent's name,
  as `claude ───`. Its asking state is an amber chip. The context is a
  gauge that turns amber at 70% and red at 90%; a limit's share turns
  amber at 80% and red at 95%, and its time left shortens to its first
  unit, as `5h`, when the row is narrow.
- In the status bar, the mode tags are chips, teal for `TERMIS`,
  `COMMIT` and `OPEN` and grey for the agent's name; the hints' keys are
  in bold, and the count of sessions asking is an amber chip. The right
  end shows `termis` muted, the project in bold and its position muted.
  The line over an agent that ended is a grey bar that starts with `×`,
  as in `× claude exited (code 0)`.
- The project picker and the key list have rounded teal frames with
  padding, and the picker a `›` prompt and a rule under the query. The
  commit box is rounded, and the diff's header in bold.
- `q` in termis mode also takes a second press within 3 s while a
  program other than the shell runs in a terminal, as a toast says.

### Fixed

- A child the agent left in its process group that ignores SIGHUP no
  longer outlives `termis`: once the agent exited, closing its session,
  a relaunch with `r` and quitting send the group SIGKILL. Nothing is
  sent once the agent's pid belongs to a new process.

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
