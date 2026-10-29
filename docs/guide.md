# User guide

This guide says how to start `termis`, what each pane shows, how
commit and push, sessions, terminals and files work, what each key and
the mouse do, and the known limits. The [README](../README.md) says
what `termis` needs and how to install it.

## Start

```text
termis [--new] [--no-restore] [path] [-- <claude args>]
```

Claude starts in the center pane in `path`, the current directory by
default, with the arguments after `--`. It continues the last
conversation it saved for that directory, as `claude --continue` would,
unless `--new` asks for a new one or the arguments already choose one:
`-c`, `-r`, `-p`, their long forms, `--session-id`, `--from-pr`,
`--teleport` or `--cloud`.

The project is the git repository around that directory. Outside one,
Claude still runs, and the right pane stays hidden until the agent
writes in a repository; it then shows the one written last. A
repository created later around the directory shows at the next start.
The sessions of the last run open again too, unless `--no-restore` says
not to. `--spike` runs the agent over the whole terminal with no side
panes, and neither reopens nor saves sessions.

Keys go to the agent, or to the terminal shown, until `Ctrl+Q` or a
click on a side pane enters termis mode, where they are `termis`'s
commands. The tag at the left of the status bar shows the mode: the
agent's or the terminal's name, `view` on the viewer, `TERMIS`, `COMMIT`
in the commit box, or `OPEN` in the project picker. The highlighted pane
has a frame, the agent's in agent mode; in termis mode the status bar
shows its main keys.

When Claude exits on its own, its last screen stays, with a bar such as
`× claude exited (code 0) · r relaunch · q quit`.

## Panes

The left pane, Projects, shows the project's path and its stack, read
from `Cargo.toml`, `package.json`, `pyproject.toml`, `go.mod` or
`composer.json`. At its foot is the agent: whether it is working, and
with which tool, waiting for a prompt or asking you something; its
model and effort; the context in use, amber at 70% and red at 90%; how
much of the plan's five-hour and weekly limits is used, amber from 80%
and red from 95%, and when each resets (the cost instead, without a
plan); the lines it changed and the session time.

That section comes from a status line and hooks `termis` gives Claude
through `--settings`. No settings file changes, and your own status line
keeps running. They pass on event and tool names, file paths, Claude's
notices and its status line input, never a prompt, a command or a
file's content
([ADR-0003](adr/0003-inject-claude-telemetry-per-session.md)).

The center pane runs Claude Code, with the session's terminals, viewer
and editor in tabs named in its title. At its right end, while the tabs
leave room, the title shows the terminal title the program in the tab
shown set, such as Claude's name for the conversation,
`✳ Orders pagination`. The agent and each terminal keep 5000 lines of
history; the title says how far back the view is, as in
`claude · termis ↑ 120`, and typing in agent mode returns to the end.

The right pane, Changes, holds git: the branch and how it stands
against its upstream, as in `Changes · main ↑0 ↓0`, the staged and
unstaged files with their line counts, your commits and lines since
midnight, and the last commit. `d` shows a file's diff over the whole
column.

The right pane is on screen while the repository has files not
committed, or commits that `p` would push. After 10 s with nothing, it
hides and the center pane takes its width. It shows anyway while
highlighted, while its commit box or a diff is open, and while a
commit, a push or a subject runs. While the side panes are on screen,
the repository is usually read again within about half a second of a
change to one of its files, and every 2 s.

Below 100 columns the side panes fold away; `z` does the same on a wider
terminal.

## Commit and push

With Changes highlighted in termis mode, `c` opens the commit box,
which commits what is staged through `git commit`, so your hooks and
commit signing run. A failure shows its first line in the status bar
and keeps the message.

`Ctrl+G` in the commit box asks Claude for a subject. `termis` runs
`claude -p` with Haiku in the repository's root, with no tools, no MCP
servers and no hooks of your settings and plugins. It skips the
repository's `.claude/settings.json` and `.claude/settings.local.json`,
and saves no session. Claude gets the subjects of the branch's last 10
commits, `git diff --cached --stat` and `git diff --cached` cut at 64
KiB. All of it leaves your machine, as any prompt to Claude does. The
first line Claude prints replaces the draft's subject. When Claude
fails or takes longer than 60 s, the draft stays and the status bar
says `suggestion failed:` and why.

`p` on Changes pushes the branch of the session shown at a second `p`
within 3 s; the first says what goes where, such as
`push 2 commits to origin/main · press p again`. A branch with an
upstream goes out through `git push` with no arguments, so your
`push.default` and remotes decide where. One without goes through
`git push -u origin <branch>`. Nothing is forced, and pre-push hooks
run. The status bar shows `pushing`, then `pushed to origin/main` or
`push failed:` and git's reason. A push is killed after 10 minutes.

## Sessions

`Ctrl+Q o` opens the project picker: the open sessions, then the
directories Claude ran in, the last started first. Typing filters them;
text that starts with `/`, `~` or `.`, holds a `/` or matches no row is
a directory, counted from your home directory unless it is absolute. A
session opened from the picker gets none of the arguments given after
`--`. The panes and the status bar follow the session shown, and the
status bar adds its position, such as `2/3`. The agents of the sessions
not shown keep running.

From two sessions on, or while the agent shown has written in another
repository, the left pane lists the sessions, each with its terminals,
its viewer and the repositories its agent wrote in. A session's row has
its agent's state (`✶` working, `○` waiting, `!` asking, `×` ended), the
project's name, and `●` when its repository has uncommitted changes. A
terminal's row has `✶` while a program other than the shell runs in
it, and `▸` marks the tab shown.

`Alt+1` to `Alt+9` show the first to the ninth session, in the list's
order, from agent mode, or from termis mode, which they leave. From two
sessions on, the status bar shows `⌥1-9 session` in agent mode. In
iTerm2, Option and a digit send them once Profiles > Keys > Left Option
key is set to `Esc+`.

### Repositories an agent writes in

Rows such as `↳ api` name the other repositories a session's agent wrote
in, the last first, with `●` when they have changes. They come from the
files Claude's `Edit`, `Write`, `MultiEdit` and `NotebookEdit` tools
wrote, its subagents' included, and from `--add-dir` and `/add-dir`.
Three show, then a row such as `+2` counts the others. `Enter` on one
has the right pane follow it, with its name in the title, as in
`Changes · api · main ↑0 ↓0`; staging, commit, `Ctrl+G` and push then
act on it. `Enter` on the session's own row brings its repository back.

### When an agent asks

An agent that starts asking while its tab is not on screen gets `!` in
the list, a notice such as `! api is asking`, and the terminal bell.
With the list not on screen, the status bar counts them, as
`! 2 asking`.

While the `termis` window does not have the focus, an agent that starts
asking or finishes its turn sends a desktop notification,
`api is asking` or `api is waiting`, through the OSC 9 escape sequence.
iTerm2, WezTerm and Ghostty turn it into a macOS notification; iTerm2
needs Profiles > Terminal > "Send escape sequence-generated alerts".

### Sessions of the last run

At start, `termis` reopens the sessions open when it last ran, in their
order, each continuing its last conversation. The project `termis` is
started for is the one shown, and `--new` and the arguments after `--`
apply to it alone. A saved directory that is gone does not open, and a
notice names it. `--no-restore` starts with that one session and saves
it in place of the others.

Only the directories are saved, each time a session opens or closes, in
`$XDG_STATE_HOME/termis/sessions.json`, else
`~/.local/state/termis/sessions.json`. A crash leaves the last set, and
closing every session with `x` leaves none.

## Terminals

`Ctrl+Q t` starts your shell, `$SHELL` when it is an absolute path, else
`/bin/sh`, as a login shell in the session's directory, in a new tab.
Terminals are named after their shell, as `zsh` and `zsh 2`. A tab
closes when its shell exits; `x` sends its shell and the program in its
foreground a hangup. In agent mode `Ctrl+Z` stops the program in the
foreground. A job sent to the background that ignores the hangup, as
under `nohup`, outlives the tab.

## Files

`f` puts the file tree of the session's directory in the left pane.
Inside a repository it lists the files git tracks and the untracked ones
it does not ignore, without submodules or nested repositories. Outside
one it lists the directory, skipping directories whose names start with
`.`. It holds up to 20,000 files. A changed file has its status letter
at the right, and a directory holding a change a grey `●`.

`Enter` on a file shows it in the session's viewer tab, one per session,
read-only and with line numbers, as it was when read; `Enter` again
reads it again. The viewer shows regular files up to 1 MiB, and says
why for a binary file, a larger one or one it cannot read.

`e` opens the file under the tree's cursor, or the one viewed, in
`$VISUAL`, else `$EDITOR`, read as shell words, so `code --wait` works.
The editor runs in a terminal tab named as in `nvim main.rs`, which
closes when it exits; an exit status other than 0 leaves a notice.

## Keys

`?` in termis mode lists the keys of every mode but the project picker;
while the picker is open, the status bar shows its main keys.

### Every mode

| Key | Action |
|---|---|
| `Ctrl+Q` | Enter termis mode with the agent pane highlighted; from termis mode, the commit box or the project picker, back to agent mode |
| `Alt+1` … `Alt+9` | In agent and termis mode, show the first to the ninth session, in the list's order, and go back to agent mode; past the last session they do nothing. Not in the commit box or the project picker; with `--spike` they reach the agent |
| `Ctrl+Z` | In agent mode, reaches the terminal shown unless it runs the editor; never reaches the agent |
| `j` `k` `Up` `Down` `Ctrl+D` `Ctrl+U` `Ctrl+F` `Ctrl+B` `PgUp` `PgDn` `G` `g` | In agent mode with the viewer shown, scroll the file as in termis mode; other keys and pastes are dropped |

### Termis mode

| Key | Action |
|---|---|
| `?` | List the keys by mode; `?` or `Esc` closes the list, and any other command closes it and runs |
| `1` / `2` / `3` | Highlight Projects / agent / Changes, which shows while highlighted; `1` and `3` only while the side panes are shown |
| `Tab` / `Shift+Tab` | Highlight the next / previous pane, Changes included when hidden, while the side panes are shown |
| `o` | Open the project picker; not with `--spike` |
| `t` | Open a terminal in the session shown and go back to agent mode; not with `--spike` |
| `e` | Open in your editor, in a terminal tab, the file under the file tree's cursor on Projects with the tree shown, else the file of the viewer shown, and go back to agent mode; nothing on a directory; not with `--spike` |
| `f` | Show the file tree of the session shown in the projects pane, or the list of sessions again, while the side panes are shown |
| `,` / `.` | Show the previous / next tab of the session shown, while it has another tab |
| `n` / `N` | Show the next / previous session, from two on |
| `x` | Close the terminal or the viewer shown, else the session shown, or on Projects with the list shown the session, terminal or viewer under the list's cursor, quitting on the last session; a second `x` within 3 s while its agent works or asks, or a program runs in a terminal it closes. On a repository's row, take it off the list |
| `z` | Zoom: hide the side panes, or bring them back when the width allows |
| `q` | Quit; a second `q` within 3 s while an agent works or asks, or a program runs in a terminal |
| `Esc` | Close the key list, else the diff, else back to agent mode |
| `i` | Back to agent mode, as `Ctrl+Q`, closing the diff with the mode; nothing while the key list is open |
| `j` `k` `Up` `Down` `Enter` | On Projects with the list of sessions shown, from two sessions on or with a repository the agent shown wrote in: move the list's cursor, show the session, terminal or viewer under it and go back to agent mode, or have the changes pane follow the repository under it and highlight that pane |
| `j` `k` `Up` `Down` `l` `h` `Enter` | On Projects with the file tree shown: move the tree's cursor; `l` or `Enter` opens the directory under it and `h` folds it or goes to the one above; `Enter` on a file shows it in the viewer |
| `j` `k` `Up` `Down` `PgUp` `PgDn` | On the agent pane: scroll the history of the tab shown a line or a page; the alternate screen has no history. On the viewer, scroll the file |
| `j` `k` `Up` `Down` `s` `u` `a` `d` | On Changes: move the selection, stage or unstage the selected row, stage all, diff of the selected row |
| `c` | On Changes: open the commit box; needs a repository with something staged, and no commit running |
| `p` | On Changes: push the branch of the session shown, at a second `p` within 3 s with no other key between; needs something to push, and no push running from that repository |
| `j` `k` `Up` `Down` `PgUp` `PgDn` `d` `Esc` | In the diff: scroll it a line or a page; `d` or `Esc` back to the lists |
| `Ctrl+D` / `Ctrl+U` | Where `j` `k` move or scroll, in the list of sessions, the file tree, the selection on Changes, the diff, the history or the viewer: half of what the pane shows down / up, no further than the ends |
| `Ctrl+F` / `Ctrl+B` | The same by all of what the pane shows, as `PgDn` / `PgUp` on the agent pane and in the diff |
| `g` / `G` | Where `j` `k` move or scroll: the first / last row of a list, the top / end of the diff or the viewer, the oldest line kept / the live screen of the history |

### Commit box

| Key | Action |
|---|---|
| typing / `Backspace` / `Ctrl+H` | Typed or pasted text goes at the end of the draft; `Backspace` or `Ctrl+H` deletes its last character |
| `Ctrl+J` / `Enter` | Body line / commit, which needs a subject |
| `Ctrl+G` | Ask the agent for a subject for what is staged, which takes the place of the draft's first line; the body lines stay. One at a time per repository |
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

The wheel over the center pane scrolls the history or the file viewed;
over the right pane it moves the selection or scrolls the diff. An agent
or a program that asks for the mouse gets the wheel, clicks and drags
over the center pane instead, so Claude's fullscreen view scrolls and
selects text itself. On an alternate screen without the mouse, the wheel
sends arrow keys in agent mode.

Where the agent or the program in the terminal shown does not ask for
the mouse, and in the viewer, a drag over the center pane selects text,
a double click a word and a triple click a line. `termis` copies it with
`pbcopy` when the button goes up, and the status bar says
`copied 3 lines`.

A click on the left or the right pane enters termis mode with that
pane highlighted, as `1` and `3` do. In termis mode a click on the
center pane goes back to agent mode, and still reaches the agent or
the program that asks for the mouse, or starts a selection. With the
commit box, the project picker or the key list open, a click changes
no mode, and neither does the click that brings the window forward.
On the status bar a click does nothing, but with `--spike` it goes back
to agent mode.

To select with the terminal, hold Option while dragging in iTerm2, or
Shift in kitty, WezTerm and Alacritty.

## Known limits

- Claude Code is the only supported agent.
- A selection covers what the center pane shows: a drag past its edge
  does not scroll. The side panes cannot be selected.
- A terminal does not get `Alt+1` to `Alt+9`, which bash and zsh read
  as a repeat count.
- Claude's fullscreen view keeps no history in `termis`; Claude scrolls
  it itself.
- After an interrupt with `Esc` or a denied permission, the agent's state
  holds until Claude's next event.
- With `--bare`, `disableAllHooks`, `allowManagedHooksOnly` or a managed
  status line, the agent's section shows less or nothing.
- Your status line runs without the variables `termis` withholds from
  Claude, such as `TERM_PROGRAM`, `LC_TERMINAL` and `ITERM_SESSION_ID`.
- Commits and pushes get no terminal input: a hook that reads the
  terminal waits for the timeout, 60 s or 10 minutes, and a curses
  pinentry cannot ask for a passphrase. A push hook or ssh command that
  writes to the terminal itself can shift the screen a row until the
  push ends.
- Without `SSH_ASKPASS`, an ssh key with a passphrase has to be in
  `ssh-agent` or the keychain, and an unknown host is refused. Over
  HTTPS, git takes the password from a credential helper or an askpass
  program.
- `Ctrl+G` sends at most 64 KiB of the patch; Claude sees the rest only
  in the stat. Your organization's managed hooks still run for it.
- `p` counts no commits on a branch without an upstream, one whose
  upstream is gone, or one that goes to another remote. The status bar
  shows `p push` for it whatever the remote has, and the right pane
  stays on screen.
- No hunk or line staging, no branch, stash or rebase commands, no
  config file, no themes.
- The colours are chosen for a dark background.
- Quitting ends every agent and terminal; the next start reopens each
  session with its agent alone.
- A file written through `Bash` or in a terminal does not list its
  repository under the session. A directory looked up once keeps its
  answer until `termis` quits, so a repository created there later is
  not listed.
- Two `termis` running at once share the file of saved sessions; the
  last to change it wins. A second one reopens the same sessions, and
  two agents in one directory continue the same conversation.
