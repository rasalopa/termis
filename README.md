# termis

A three-pane terminal workspace for working with a coding agent on macOS:
the current project on the left, a live agent session in the middle, and
the git changes with a commit box on the right. Claude Code is the first
supported agent.

```text
  Projects          ╭─ claude · termis   zsh ─────────────────────────────╮  Changes · main ↑0 ↓0
  ▸ ✶ termis     ●  │> add the list of modified files on the right,       │  4 files      +144 −10
      ✶ zsh         │  with their +/- line counts                         │
    ! api asking ●  │                                                     │  staged              2
    ○ web        ●  │● First I check how the layout is built.             │    M src/app.rs +31 −4
                    │                                                     │    M src/ui/lay… +9 −2
  current           │● Read(src/ui/layout.rs)                             │
  ~/src/termis      │  ⎿  Read 58 lines                                   │  unstaged            2
  Rust · ratatui …  │                                                     │  ▸ M src/ui/ch… +61 −4
                    │● Write(src/git.rs)                                  │    ? src/git.rs    +43
                    │  ⎿  Wrote 97 lines to src/git.rs                    │  today ───────────────
                    │                                                     │  3 commits    +312 −58
                    │● Update(src/ui/changes.rs)                          │  ▁▂▃▅▇█▆▃  09→17h
                    │  ⎿  Added 61 lines, removed 4 lines                 │  last a3f9c1e       2h
                    │                                                     │  feat(ui): add the ca…
                    │● Bash(cargo check)                                  │  diff changes.rs ─────
                    │  ⎿  Finished `dev` profile in 2.41s                 │  @@ -12,6 +12,9 @@
                    │                                                     │  -    files: Vec<Stri…
                    │● Done. The right pane reads git status and          │  +    staged: Vec<Ent…
                    │  numstat every 2 s. Shall I add the diff view?      │  +    unstaged: Vec<E…
                    │                                                     │  ↓ 3 more hunks
  claude ─────────  │> yes, and use the theme colours                     │  commit
  state  ✶ working  │                                                     │  ╭───────────────────╮
  model  Fable 5.1  │✻ Brewing… (esc to interrupt · 8s)                   │  │feat(ui): changes …│
  effort high       │                                                     │  ╰───────────────────╯
  ctx    ━━─── 41%  │╭───────────────────────────────────────────────────╮│  Enter commit
  5h     23% 3h12m  ││ >                                                 ││  Esc cancel
  week   41% 4d6h   │╰───────────────────────────────────────────────────╯│
  lines  +96 −12    │  ? for shortcuts           ⏵⏵ accept edits on       │  s stage u unstage
  time   1:12:08    │                                                     │  a all p push d diff
                    ╰─────────────────────────────────────────────────────╯
 claude   Ctrl+Q termis mode                                           termis 1/3 · main · +144 −10 
```

The screen above is the target, drawn in [docs/mockup.md](docs/mockup.md),
which also says how it is coloured. 0.4.0 draws less:

- no hourly sparkline, and no push: `p` came after 0.4.0;
- `d` shows a file's diff over the whole right column, not under the
  lists;
- the commit box shows only while it is open;
- the changes pane draws no keys: with it highlighted in termis mode,
  the status bar lists them, all but `d`;
- the status bar's right end shows `termis · <project>`, followed from
  two sessions on by the session's position, such as `1/3`, and no
  branch or line counts.

[ROADMAP.md](ROADMAP.md) says what comes next.

## Status

0.4.0 shows files: `f` puts a tree of the session's directory in the
left pane, `Enter` shows a file from it in a viewer tab of the center
pane, and `e` opens it in your editor. Since 0.3.0 a session can hold
terminals, your shell in a tab beside its Claude Code. Since 0.2.0 one
`termis` runs several projects, each in a session with its own Claude
Code; from two sessions on, the left pane lists them. 0.1.0 was the
first release with a changelog, after the v0.0.1 build.
Claude Code runs in the center pane, with `termis` as its terminal. The
right pane holds git: the branch and how it stands against its upstream in
the title, the staged and unstaged files with their line counts, your
commits and lines since midnight and the last commit; from it you stage,
unstage, commit and push, and read a file's diff. The left pane shows the
project's path and its stack, read from `Cargo.toml`, `package.json`,
`pyproject.toml`, `go.mod` or `composer.json`. At its foot, under a
heading with its name, is the agent: whether it is working, and with which
tool, waiting for a prompt or asking you something; its model and effort;
the context in use, as a gauge that turns amber at 70% and red at 90%; how
much of the plan's five-hour and weekly limits is used, amber from 80% and
red from 95%, and when each resets (the cost instead, without a plan); the
lines it changed and the session time.

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

Keys go to the agent, or to the terminal shown, until `Ctrl+Q` enters
termis mode, where they are `termis`'s commands; `Ctrl+Q`, `Esc` or
`i` goes back. `Ctrl+Q` reaches neither, and `Ctrl+Z` never reaches
the agent. A file shown in the viewer takes only the scroll keys.
The tag at the left of the status bar shows the mode: the name of the
agent or of the terminal shown, `view` on the viewer, `TERMIS`,
`COMMIT` while the commit box is open, or `OPEN` while the project
picker is. The highlighted pane is
the one with a frame: the agent's in agent mode. In termis mode the
status bar shows the main keys for the highlighted pane, and `?` lists
the keys of every mode but the picker, whose main keys the status bar
shows while it is open.

The right pane reads the repository again every 2 s while it is on
screen. The commit box (`c`) commits what is staged through `git commit`,
so your git hooks and commit signing run; a failure shows its first line
in the status bar and keeps the message for the next `c`. Below 100
columns the side panes fold away and the center pane takes the width;
`z` in termis mode does the same on a wider terminal.

`p` in termis mode pushes the branch of the session shown, at a second
`p` within 3 s. The first one says what goes where, such as
`push 2 commits to origin/main · press p again`, and any other key
cancels it. A branch with an upstream goes out through `git push` with
no arguments, so your `push.default` and remotes decide where. When
`branch.<name>.pushRemote` or `remote.pushDefault` sends it to another
remote than its upstream's, the first `p` names that remote, as in
`push main to fork`, and counts no commits. A branch without an
upstream goes through `git push -u origin <branch>`, which needs a
remote named `origin`, and tracks it from then on. Nothing is forced,
and pre-push hooks run. On a detached `HEAD`, before the first commit,
or when the branch goes to its upstream and the upstream already has
every commit of it, `p` says why there is nothing to push, and the
status bar does not offer it. While the push runs, the right pane keeps
reading the repository, the status bar shows `pushing`, and `p` says
`a push is running`. When it ends, the repository is read again, for
`↑` and `↓`, and the status bar says `pushed to origin/main`, or
`push failed:` and the line that says why, such as
`! [rejected] main -> main (fetch first)` or
`ssh: connect to host example.com port 22: Connection refused`; while
another session is shown, the project's name comes first. A push that
takes 10 minutes is killed.

The center pane keeps up to 5000 lines that scrolled off the agent's
screen. The wheel, or `j`/`k` in termis mode on that pane, scrolls
through them, and the pane title says how far back the view is
(`claude · termis ↑ 120`). Going back to agent mode, or typing to the
agent, returns to the end.

Claude gets a status line and hooks for the session through `--settings`,
merged into a `--settings` of your own; no settings file changes, and the
status line you had keeps running after `termis`'s own. What they report
goes to a private directory for each session, `$TMPDIR/termis-<pid>/<n>`:
event and tool names, file paths, Claude's notices and its status line
input, never a prompt, a command or a file's content. A closed session's
directory goes once its agent and its terminals are hung up, and
`termis-<pid>` with all of them when `termis` quits. If a session's
directory cannot be set up, Claude runs without it in that session, the
left pane shows nothing of the agent for it, and a notice in the status
bar says why.
[ADR-0003](docs/adr/0003-inject-claude-telemetry-per-session.md) has the
details.

`q` in termis mode quits; while Claude is working or asking in any
session, or a program other than the shell runs in a terminal, it takes
a second `q` within 3 s, as a notice in the status bar says. Each
agent's process group gets a hangup, then a kill once that agent
exited, or two seconds later if it is still running, so nothing the
agent started in its group outlives it. Each terminal's shell, and the
program in its foreground, get the hangup too, and a kill if they still
run two seconds later. When Claude exits on its own, its last screen
stays, with a bar on its last row:
`× claude exited (code N) · r relaunch · q quit`, or
`× claude exited (code N) · r relaunch · x close · q close` while other
sessions remain; `r` starts it again with the same arguments, and `x`
or `q` closes its session. After quitting, `termis` prints how the agent
of the session shown ended when it exited before the quit, such as
`agent exited with code 0`, and exits with a non-zero status when that
agent failed.

## Sessions

`Ctrl+Q o` opens the project picker: the open sessions, then the
directories Claude ran in, from its configuration, the last started
first; a directory with a session open is listed once, as that session.
Typing filters the list by name or path, and `Up`/`Down` choose a row.
Text that starts with `/`, `~` or `.`, holds a `/` or matches no row is
a directory, counted from the home directory unless it is absolute.
`Enter` starts Claude there in a new session, continuing the last
conversation it saved for that directory, and `Tab` starts it on a new
one; on a directory with a session open, both show that session. Either
way the picker closes and keys go to the session shown, to its agent or
to the terminal it shows. A session opened this way gets none of the
arguments given after `--`: they may be flags meant for the first
project, such as `--add-dir` or `--mcp-config`. When a directory cannot
be opened, or Claude cannot start there, a notice in the status bar
says why.

The project's path and stack, the agent's section, the changes pane and
the project in the status bar follow the session shown, and the status bar
adds its position, such as `2/3`. `n` and `N` in termis mode show the
next and the previous session, wrapping around. Each session keeps its
own scrollback, and the agents not shown keep running.

From two sessions on, the projects pane lists them, each with its
terminals and its viewer under it, then `current` and the shown
project's path and stack; [Files](#files) says how `f` puts the file
tree in their place. A session's row has its agent's state (`✶`
working, `○` waiting, `!` asking, `×` ended, a blank without
telemetry), the project's name, and `●` when its repository has
uncommitted changes; a terminal's row has `✶` while a program other
than the shell runs in it, and its name; the viewer's row has the name
of its file.
`▸` marks the row of the tab shown: the session's own for the agent's
tab. The name of a session whose agent asks is in amber, followed by
`asking` when there is room. With the projects pane highlighted and the
list shown, `j`/`k` or `Up`/`Down` move a cursor through it, and `Enter`
shows the session, terminal or viewer under it and goes back to agent
mode. The repository of the session shown is read every 2 s while the
side panes are on screen; the others get only `git status`, every 10 s,
while the list is on screen.

An agent that starts asking while its tab is not on screen, in a session
not shown or behind a terminal or the viewer, gets `!` in the list, a
notice in the status bar naming its project, such as `! api is asking`,
and the terminal bell. With the side panes hidden, or the file tree in
the list's place, the status bar counts the sessions not shown that are
asking, as `! 2 asking` on amber.

`x` in termis mode closes the session shown, or, with the projects pane
highlighted and the list shown, the session, terminal or viewer under
the list's cursor; with a terminal's tab or the viewer's shown, it
closes that tab, and the tab before it takes its place.
A session's agent and terminals are hung up while the others keep
running; when it was the session shown, the next one, else the previous
one, takes its place. Closing the last session quits. While the agent
it closes is working or asking, or a program other than the shell runs
in a terminal it closes, `x` takes a second press within 3 s.

## Terminals

`Ctrl+Q t` starts your shell in the session shown: `$SHELL` when it is
an absolute path, else `/bin/sh`, as a login shell in the directory the
session's agent runs in. It runs in a tab of the center pane after the
session's other tabs, and `termis` goes back to agent mode, so keys go
to it. The
shell gets `TERM=xterm-256color` and `COLORTERM=truecolor`, as Claude
does, and none of the host terminal's variables, such as `TERM_PROGRAM`
and `ITERM_SESSION_ID`. When it cannot start, a notice in the status bar
says why.

A terminal is named after its shell, as `zsh`; when another terminal of
the session has that name, it gets the lowest number from 2 on that none
has, as `zsh 2`. The center pane's title names the tabs, the agent's by
its project, as in `claude · termis   zsh   zsh 2`, the one shown in
bold; when they do not fit, the tabs not shown but the agent's give way
to a count, such as `+2`. In agent mode the tag at the left of the
status bar names the terminal shown. `,` and `.` in termis mode show the
previous and the next tab, wrapping around. Tabs not shown keep running,
and a session shown again with `n`, `N` or the picker shows the tab it
showed last.
Each terminal keeps its own 5000 lines of history, which the wheel and
the scroll keys move through as they do the agent's; the title then says
how far back after its name, as in `zsh ↑ 120`.

A tab closes when its shell exits, and the tab before it takes its
place. `x` in termis mode closes the terminal shown: its shell and the
program in its foreground get a hangup, and a kill if they still run
two seconds later. While a program other than the shell runs in a
terminal, `x` on that terminal or its session, and `q`, take a second
press within 3 s, as a notice says, such as
`api · zsh runs a program · press x again to close it`.

In agent mode `Ctrl+Z` reaches a terminal and stops the program in its
foreground, as in any terminal; it never reaches the agent, nor a
terminal that runs the editor (`e`), where nothing would resume it. A
job sent to the background with `&` or `bg` runs in a process group of
its own and gets only the hangup the shell passes on, so one that
ignores it, as under `nohup`, outlives the tab.

[Sessions](#sessions) says how the projects pane lists a session's
terminals.

## Files

`f` in termis mode, while the side panes are shown, puts the file tree
of the session shown in the projects pane, in place of the list of
sessions and the project's path and stack, under a heading with the
project's name; the agent's section stays at the foot. `f` again brings
them back, and each session keeps its choice.

The tree lists the files of the directory the session's agent runs in.
Inside a repository they are the files git tracks that are still there
and the untracked ones it does not ignore, as
`git ls-files --cached --others --exclude-standard` lists them; a
submodule, or another repository nested in it, is left out with its
files. Outside one, `termis` walks the directory: it lists regular
files and links that do not lead to a directory, does not follow links
to directories, and skips the directories whose names start with `.`
and those it cannot read. The tree holds up to 20,000 files; a longer
list keeps those with the fewest directories above them, and the
tree's last row says `cut at 20000 files`.

In each directory, directories come first, then files, each sorted by
name; a directory starts folded, `▸`, until opened, `▾`. A changed
file has the letter of its change in the work tree, else in the index,
at the right end of its row, drawn as in the changes pane; a directory
that holds a changed file, a deleted one included, has a grey `●`
there. The files are read when the tree comes on screen and, inside a
repository, again after a refresh finds the status changed, at most
once every 2 s after the last answer and never while the tree is
hidden. The tree says `reading files` until the first answer and
`no files` when there are none to list; when the files cannot be
listed, it says `files unavailable:` and why, in red. A list read again
keeps the folds and the cursor.

With the projects pane highlighted and the tree shown, `j`/`k` or
`Up`/`Down` move the tree's cursor, `l` or `Enter` opens the directory
under it, and `h` folds an open directory, or else moves the cursor to
the directory its entry is in. `Enter` on a file shows it in the
viewer, staying in termis mode, and `e` opens it in your editor.

A session has at most one viewer tab: the first file viewed opens it
after the session's other tabs, and each file viewed later takes its
place there. The center pane's title and the list of sessions name it
after its file, as in `claude · termis   zsh   main.rs`. It shows the
file read-only, as it was when read, each line after its number;
`Enter` on the same file reads it again and keeps the view where it
was, while another file starts at its top. Until the read answers, the
viewer says `reading the file`.

The viewer shows a regular file, once links are followed, of up to
1 MiB. In place of the text it says `binary file, not shown` when the
file's first 8,000 bytes hold a NUL byte (git's test for a binary file),
`larger than 1 MiB, not shown`, `not a regular file, not shown` for a
directory or a pipe, `empty file`, or, in red, `file unavailable:` and
why it could not be read. As in the diff, tabs show as four spaces and
control characters as `�`. Bytes that are not UTF-8 show as `�` too,
and a line longer than the pane is cut at its edge.

While the viewer is shown, `j`/`k` or `Up`/`Down` scroll the file a
line, `Ctrl+D`/`Ctrl+U` half a page, `PgUp`/`PgDn` or `Ctrl+B`/`Ctrl+F`
a page, `G` to its end and `g` to its top, in termis mode with the
agent pane highlighted and in agent mode, where the tag says `view` and
other keys and pastes are dropped. The wheel over the center pane
scrolls it too. `x` in termis mode closes the viewer at once, from its
tab or its row in the list; when it was shown, the tab before it takes
its place.

`e` in termis mode opens a file in your editor: with the projects pane
highlighted and the tree shown, the file under the tree's cursor, else
the file the viewer shows while its tab is shown; the status bar shows
`e edit` while there is one. The editor runs in a terminal of the
session, in a tab after its other tabs and in the directory the
session's agent runs in, and `termis` goes back to agent mode, so keys
go to it. It is `$VISUAL`, else `$EDITOR` (an empty one counts as
unset), read as shell words as git reads them, so `code --wait` works;
the file's absolute path follows as one more argument, whatever it
holds. The tab is named after the editor's program and the file, as
`nvim main.rs`. With neither variable set, a notice says
`neither VISUAL nor EDITOR is set`.

The editor's tab closes when the editor exits. An exit status other
than 0, as from an editor that cannot start, leaves a notice such as
`api: nvim main.rs exited (code 127)`; an editor killed by a signal
leaves one such as `api: nvim main.rs was killed by SIGSEGV`. While the
editor runs, its terminal counts as one where a program runs: its row
in the list has `✶`, and `x` on its tab or its session, and `q`, take a
second press within 3 s. `Ctrl+Z` does not reach it. A change saved in
the editor shows in the changes pane after its next refresh, and in the
viewer once `Enter` reads the file again.

## Keys

### Every mode

| Key | Action |
|---|---|
| `Ctrl+Q` | Enter termis mode with the agent pane highlighted; from termis mode, the commit box or the project picker, back to agent mode |
| `Ctrl+Z` | In agent mode, reaches the terminal shown unless it runs the editor; never reaches the agent |
| `j` `k` `Up` `Down` `Ctrl+D` `Ctrl+U` `Ctrl+F` `Ctrl+B` `PgUp` `PgDn` `G` `g` | In agent mode with the viewer shown, scroll the file as in termis mode; other keys and pastes are dropped |

### Termis mode

| Key | Action |
|---|---|
| `?` | List the keys by mode; `?` or `Esc` closes the list, and any other command closes it and runs |
| `1` / `2` / `3` | Highlight Projects / agent / Changes; `1` and `3` only while the side panes are shown |
| `Tab` / `Shift+Tab` | Highlight the next / previous pane, while the side panes are shown |
| `c` | Open the commit box from any pane; needs the side panes shown, a repository with something staged, and no commit running |
| `p` | Push the branch of the session shown from any pane, at a second `p` within 3 s with no other key between; needs the side panes shown, something to push, and no push running from that repository |
| `o` | Open the project picker; not with `--spike` |
| `t` | Open a terminal in the session shown and go back to agent mode; not with `--spike` |
| `e` | Open in your editor, in a terminal tab, the file under the file tree's cursor on Projects with the tree shown, else the file of the viewer shown, and go back to agent mode; nothing on a directory; not with `--spike` |
| `f` | Show the file tree of the session shown in the projects pane, or the list of sessions again, while the side panes are shown |
| `,` / `.` | Show the previous / next tab of the session shown, while it has another tab |
| `n` / `N` | Show the next / previous session, from two on |
| `x` | Close the terminal or the viewer shown, else the session shown, or on Projects with the list shown the session, terminal or viewer under the list's cursor, quitting on the last session; a second `x` within 3 s while its agent works or asks, or a program runs in a terminal it closes |
| `z` | Zoom: hide the side panes, or bring them back when the width allows |
| `q` | Quit; a second `q` within 3 s while an agent works or asks, or a program runs in a terminal |
| `Esc` | Close the key list, else the diff, else back to agent mode |
| `i` | Back to agent mode, as `Ctrl+Q`, closing the diff with the mode; nothing while the key list is open |
| `j` `k` `Up` `Down` `Enter` | On Projects with the list of sessions shown, from two sessions on: move the list's cursor, show the session, terminal or viewer under it and go back to agent mode |
| `j` `k` `Up` `Down` `l` `h` `Enter` | On Projects with the file tree shown: move the tree's cursor; `l` or `Enter` opens the directory under it and `h` folds it or goes to the one above; `Enter` on a file shows it in the viewer |
| `j` `k` `Up` `Down` `PgUp` `PgDn` `G` `g` | On the agent pane: scroll the history of the tab shown a line or a page, `G` back to the end and `g` to the oldest line kept; the alternate screen has no history. On the viewer, scroll the file, `G` to its end and `g` to its top |
| `j` `k` `Up` `Down` `s` `u` `a` `d` | On Changes: move the selection, stage or unstage the selected row, stage all, diff of the selected row |
| `j` `k` `Up` `Down` `PgUp` `PgDn` `d` `Esc` | In the diff: scroll it a line or a page; `d` or `Esc` back to the lists |
| `Ctrl+D` / `Ctrl+U` | Where `j` `k` move or scroll, in the list of sessions, the file tree, the selection on Changes, the diff, the history or the viewer: half of what the pane shows down / up, no further than the ends |
| `Ctrl+F` / `Ctrl+B` | The same by all of what the pane shows, as `PgDn` / `PgUp` on the agent pane and in the diff |
| `g` / `G` | Where `j` `k` move or scroll: the first / last row of a list, the top / end of the diff or the viewer, the oldest line kept / the live screen of the history |

### Commit box

| Key | Action |
|---|---|
| typing / `Backspace` / `Ctrl+H` | Typed or pasted text goes at the end of the draft; `Backspace` or `Ctrl+H` deletes its last character |
| `Ctrl+J` / `Enter` | Body line / commit, which needs a subject |
| `Esc` / `Ctrl+Q` | Cancel, back to Changes / to agent mode; the draft is kept |

### Project picker

| Key | Action |
|---|---|
| typing / `Backspace` / `Ctrl+H` | Typed text, or the first line of a paste, goes at the end of the query; `Backspace` or `Ctrl+H` deletes its last character |
| `Up` / `Down` | Choose the row above / below |
| `Enter` / `Tab` | Open the choice, continuing its last conversation / on a new one, and go back to agent mode; on an open session, both show it |
| `Esc` / `Ctrl+Q` | Close the picker, back to termis mode / to agent mode |

### After the agent ended

| Key | Action |
|---|---|
| `r` | Relaunch it, in agent or termis mode, with its tab shown |
| `x` / `q` | In agent mode with its tab shown, close its session, quitting on the last |

## Mouse

`termis` turns on mouse reporting for its whole window. The wheel over
the center pane scrolls the agent's history; over the right pane it moves
the selection or scrolls an open diff. An agent that asks for the mouse
gets the wheel and the clicks, drags and motion it asks for over the
center pane instead: Claude's fullscreen view scrolls, selects and copies
text with them. On the agent's alternate screen without the mouse, the
wheel sends it arrow keys in agent mode. With a terminal shown, the
same goes for the terminal and the program running in it. With the
viewer shown, the wheel over the center pane scrolls its file. Other
clicks do nothing.

To select with the terminal instead, hold Option while dragging in
iTerm2, or Shift in kitty, WezTerm and Alacritty. An iTerm2 profile that
reports only wheel events leaves plain dragging to the terminal.

## Known limits

- Claude Code is the only supported agent.
- No text selection of its own: in the viewer, and where the agent or a
  program in a terminal does not take the mouse, the host terminal's
  selection takes the screen as drawn, side panes, frames, rules and the
  viewer's line numbers included.
- Claude's fullscreen view draws on the alternate screen, which keeps no
  history in `termis`; Claude scrolls it itself.
- The agent's state can lag. An interrupt with `Esc` or a denied
  permission sends no event, so the state and the tool shown hold until
  the next event, Claude's idle notice at the latest. Rarely, a tool
  waiting for permission shows as running until Claude's permission
  notice, a few seconds later.
- With `--bare`, `disableAllHooks`, `allowManagedHooksOnly` or a managed
  status line, Claude leaves out some or all of what `termis` injects,
  and the agent's section shows less or nothing.
- Your status line runs without the variables `termis` withholds from
  Claude, such as `TERM_PROGRAM`, `LC_TERMINAL` and `ITERM_SESSION_ID`.
- A commit runs in the background with no terminal input: a hook that
  reads the terminal stops until the 60 s timeout kills it, and a curses
  pinentry cannot ask for a passphrase.
- A push runs the same way, with a 10-minute timeout. ssh gets its
  passphrase or its answer about an unknown host key from your
  `SSH_ASKPASS` and fails without one, so a key with a passphrase has to
  be in `ssh-agent` or, on macOS, in the keychain (`UseKeychain`), and
  an unknown host is refused. Over HTTPS, git takes the user name and
  password from a credential helper, `GIT_ASKPASS`, `core.askPass` or
  `SSH_ASKPASS`; without one the push fails with git's
  `terminal prompts disabled` line. A hook or an ssh command that opens
  the terminal itself still writes on the screen, and can shift it a
  row, until the push ends and `termis` draws the whole screen again;
  one that reads the terminal stops the push until the timeout kills it.
- No hunk or line staging, no branch, stash or rebase commands, no
  config file, no themes.
- The colours are chosen for a dark background. On a light one the
  amber, green and teal text is hard to read and the faint lines turn
  dark; the chips keep their contrast.
- When `termis` quits, every agent and every terminal ends. The next
  start reopens the sessions, each with its agent alone: no terminal, no
  viewer, and the list in place of the file tree.

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
