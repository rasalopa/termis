# User guide

This guide says how to start `termis`, what each pane shows, how
commit and push, sessions, terminals and files work, what each key and
the mouse do, what the config file sets, and the known limits. The
[README](../README.md) says what `termis` needs and how to install it.

## Start

```text
termis [--new] [--no-restore] [--account <name>] [path] [-- <agent args>]
```

The agent starts in the center pane in `path`, the current directory
by default, with the arguments after `--`: Claude Code, or Codex when
the directory runs as a Codex account (see [Config file](#config-file)).
Claude continues the last conversation it saved for that directory, as
`claude --continue` would, unless `--new` asks for a new one or the
arguments already choose one: `-c`, `-r`, `-p`, their long forms,
`--session-id`, `--from-pr`, `--teleport` or `--cloud`. `--account`
runs `path` as that account of the config file, with its agent, in
place of the one `termis` keeps for the directory, and keeps it. The
arguments after `--` go to whichever agent that is, so give `--account`
with them in a directory that has run both.

Codex starts as `codex --no-daemon resume --last`, which continues the
directory's last Codex conversation or starts one. `--new`, `Tab` in
the picker, or a prompt or a subcommand among the arguments start it
without `resume --last`: a prompt starts a new conversation. `--no-daemon`
keeps the conversation in the session, so closing the session stops
its turn. `termis` gives Codex no other option and writes nothing in
`~/.codex`.

The project is the git repository around that directory. Outside one,
the agent still runs, and the right pane stays hidden until it
writes in a repository; it then shows the one written last. A
repository created later around the directory shows at the next start.
The sessions of the last run open again too, unless `--no-restore` says
not to. `--spike` runs the agent over the whole terminal with no side
panes, and neither reopens nor saves sessions.

With no path, option or arguments, `termis` first shows a start
screen: the version, the sessions of the last run and the first nine
projects of the project picker. `Enter` reopens the last run, `.` opens
the current directory, `1` to `9` a recent project and `o` the project
picker; whatever opens joins the last run. `q` quits. `termis .` starts
without the screen.

Keys go to the agent, or to the terminal shown, until `Ctrl+Q` or a
click on a side pane enters termis mode, where they are `termis`'s
commands. The tag at the left of the status bar shows the mode: `START`
on the start screen, the agent's or the terminal's name, `view` on the
viewer, `TERMIS`, `COMMIT` in the commit box, or `OPEN` in the project
picker. The highlighted pane has a frame, the agent's in agent mode.
In termis mode each pane takes its own keys, which the status bar
shows, and `1`, `2`, `3`, `Tab`, `?`, `Esc` and `q` work from any pane:
the file tree is `1`, then `f`. With the side panes hidden, the agent
pane also takes `o`, `n` and `N`.

When the agent exits on its own, its last screen stays, with a bar such as
`× claude exited (code 0) · r relaunch · q quit`.

## Panes

The left pane, Projects, shows the project's path and its stack, read
from `Cargo.toml`, `package.json`, `pyproject.toml`, `go.mod` or
`composer.json`. At its foot is the agent: whether it is working, and
with which tool, waiting for a prompt or asking you something; its
model and effort; the context in use, amber at 70% and red at 90%; how
much of the plan's five-hour and weekly limits is used, amber from 80%
and red from 95%, and when each resets (the cost instead, without a
plan); the lines it changed and the session time. While the session's
agent has more than one account, the section's heading names the one
the session runs as, as in `Claude · work`.

That section comes from a status line and hooks `termis` gives Claude
through `--settings`. No settings file changes, and your own status line
keeps running. They pass on event and tool names, file paths, Claude's
notices and its status line input, never a prompt, a command or a
file's content
([ADR-0003](adr/0003-inject-claude-telemetry-per-session.md)). Claude
also reports to `termis` whether it is working, waiting or asking, so
the state changes about a quarter of a second after `Esc` or a denied
permission ends a turn
([ADR-0013](adr/0013-the-agents-state-from-its-program-status.md)).
Codex's section shows that state alone, from the reports Codex sends
unasked ([ADR-0016](adr/0016-codex-as-the-agent-of-an-account.md)).

The center pane runs the session's agent, with its terminals, viewer
and editor in tabs named in its title. At its right end, while the tabs
leave room, the title shows the terminal title the program in the tab
shown set, such as Claude's name for the conversation,
`Orders pagination`. The agent and each terminal keep 5000 lines of
history; the title says how far back the view is, as in
`claude · termis ↑ 120`, and typing in agent mode returns to the end.

The right pane, Changes, holds git: the branch and how it stands
against its upstream, as in `Changes · main ↑0 ↓0`, the staged and
unstaged files with their line counts, your commits and lines since
midnight, with a bar for each hour from the first of them to now (the
earliest left out when the column is too narrow), and the last commit.
`d` shows a file's diff over the whole column.

The right pane is on screen while the repository has files not
committed, or commits that `p` would push. After 10 s with nothing, it
hides and the center pane takes its width. It shows anyway while
highlighted, while its commit box or a diff is open, and while a
commit, a push or a subject runs. While the side panes are on screen,
the repository is usually read again within about half a second of a
change to one of its files, and every 2 s, or every `poll-seconds` the
config file sets.

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
says `suggestion failed:` and why. In a Codex session it says
`codex cannot suggest a subject`.

`p` on Changes pushes the branch of the session shown at a second `p`
within 3 s; the first says what goes where, such as
`push 2 commits to origin/main · press p again`. A branch with an
upstream goes out through `git push` with no arguments, so your
`push.default` and remotes decide where. One without goes through
`git push -u origin <branch>`. Nothing is forced, and pre-push hooks
run. The status bar shows `pushing`, then `pushed to origin/main` or
`push failed:` and git's reason. A push is killed after 10 minutes.

`l` on Changes runs [lazygit](https://github.com/jesseduffield/lazygit),
when it is on the `PATH`, in a terminal tab in the root of the
repository the pane shows, for what the pane does not do: hunks and
lines, branches, stash, rebase. The pane follows what lazygit stages
and commits, and the tab closes when lazygit quits.

## Sessions

`Ctrl+Q o` opens the project picker: the open sessions, then the
directories the agents ran in, the last used first. Typing filters them;
text that starts with `/`, `~` or `.`, holds a `/` or matches no row is
a directory, counted from your home directory unless it is absolute. A
session opened from the picker gets none of the arguments given after
`--`. The panes and the status bar follow the session shown, and the
status bar adds its position, such as `2/3`. The agents of the sessions
not shown keep running.

With more than one account, Claude's own included, the picker lists
the accounts under the query, the one the choice opens as marked, and
each row ends with the account it opens as: its session's, the one it
last opened as, else the one an agent last ran there as, else the
first account without `dir`. An account alone with its agent shows as
the agent, as `codex`, a name `--account` takes too. `Left` and
`Right` pick another for the row chosen, which `Enter` or `Tab` then
opens, and `termis` keeps it for that directory; another row, or
typing, drops the pick. An open session keeps its account: close it
with `x` to open it as another. Its
terminal tabs, the editor's and lazygit's too, run as that account, so
`claude` or `codex` typed in one does as well. Each agent keeps an
account's conversations apart, so a directory opened as another account
starts a new one.

From two sessions on, or while the agent shown has written in another
repository, the left pane lists the sessions, each with its terminals,
its viewer and the repositories its agent wrote in. A session's row has
the digit of its `Alt` key, its agent's state (`✶` working, `○` waiting,
`!` asking, `×` ended), the project's name, and `●` when its repository
has uncommitted changes. A terminal's row has `✶` while a program other
than the shell runs in it. A bar, `▌`, runs down the left of the rows of
the session shown, and `▸` marks its terminal or viewer while that tab
is on screen. With the list shown, the project's path and stack sit at
the foot of the pane, under `Current` and over the agent's section.

`Alt+1` to `Alt+9` show the session with that digit in the list, from
agent mode, or from termis mode, which they leave; a session past the
ninth has no digit. From two sessions on, the status bar shows
`⌥1-9 session` in agent mode. In iTerm2, Option and a digit send them
once Profiles > Keys > Left Option key is set to `Esc+`.

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
closing every session with `x` leaves none. On the start screen, `q`
leaves the file as it is.

With more than one account, `termis` also keeps the account each
directory last opened as, in `accounts.json`
beside that file, and a session reopens as it. One whose account the
config file no longer declares does not reopen, and a notice says so;
as the directory `termis` starts in, it stops `termis` until
`--account` names another; on the start screen, `.` says so instead.
With one account the file is not read: every directory runs as it,
whatever account the file keeps for it.

## Terminals

`Ctrl+Q t` starts your shell, `$SHELL` when it is an absolute path, else
`/bin/sh`, as a login shell in the session's directory, in a new tab.
Terminals are named after their shell, as `zsh` and `zsh 2`. A tab
closes when its shell exits; `x` sends its shell and the program in its
foreground a hangup. In agent mode `Ctrl+Z` stops the program in the
foreground. A job sent to the background that ignores the hangup, as
under `nohup`, outlives the tab.

## Files

`f` on Projects puts the file tree of the session's directory in the
left pane, and again brings back the list of sessions. Inside a repository it lists the files git tracks and
the untracked ones it does not ignore, without submodules or nested
repositories. Outside one it lists the directory, skipping directories
whose names start with `.`. It holds up to 20,000 files. A changed file
has its status letter at the right, and a directory holding a change a
grey `●`.

`Enter` on a file shows it in the session's viewer tab, one per session,
read-only and with line numbers, as it was when read, and highlights
the center pane, where `j` and `k` scroll it. `1` goes back to the
tree, where `Enter` reads the file again. The viewer shows regular
files up to 1 MiB, and says why for a binary file, a larger one or one
it cannot read.

`e` opens the file under the tree's cursor on Projects, or the one
viewed on the agent pane, in `$VISUAL`, else `$EDITOR`, read as shell words, so `code --wait` works.
The editor runs in a terminal tab named as in `nvim main.rs`, which
closes when it exits; an exit status other than 0 leaves a notice.

## Keys

`?` in termis mode lists the keys of every mode but the project picker
and the start screen; on those, the status bar shows their main keys.

### Every mode

| Key | Action |
|---|---|
| `Ctrl+Q` | Enter termis mode with the agent pane highlighted; from termis mode, the commit box or the project picker, back to agent mode |
| `Alt+1` … `Alt+9` | In agent and termis mode, show the session with that digit in the list of sessions and go back to agent mode; past the last session they do nothing. Not in the commit box or the project picker; with `--spike` they reach the agent |
| `Ctrl+Z` | In agent mode, reaches the terminal shown unless it runs the editor or lazygit; the agent does not get it |
| `j` `k` `Up` `Down` `Ctrl+D` `Ctrl+U` `Ctrl+F` `Ctrl+B` `PgUp` `PgDn` `G` `g` | In agent mode with the viewer shown, scroll the file as in termis mode; other keys and pastes are dropped |

### Termis mode

A key for one pane does nothing on the others; with the side panes
hidden, the agent pane also takes `o`, `n` and `N`.

| Key | Action |
|---|---|
| `?` | List the keys by mode; `?` or `Esc` closes the list, and any other command closes it and runs |
| `1` / `2` / `3` | Highlight Projects / agent / Changes, which shows while highlighted; `1` and `3` only while the side panes are shown |
| `Tab` / `Shift+Tab` | Highlight the next / previous pane, Changes included when hidden, while the side panes are shown |
| `o` | On Projects without the file tree: open the project picker; not with `--spike` |
| `t` | On the agent pane: open a terminal in the session shown and go back to agent mode; not with `--spike` |
| `e` | Open in your editor, in a terminal tab, the file under the file tree's cursor on Projects, or the file of the viewer shown on the agent pane, and go back to agent mode; nothing on a directory; not with `--spike` |
| `f` | On Projects: show the file tree of the session shown, or the list of sessions again |
| `,` / `.` | On the agent pane: show the previous / next tab of the session shown, while it has another tab |
| `n` / `N` | On Projects: show the next / previous session, from two on |
| `x` | On the agent pane, close the terminal or the viewer shown, else the session shown; on Projects with the list shown, the session, terminal or viewer under the list's cursor, or take a repository's row off the list. Closing the last session quits; a second `x` within 3 s while its agent works or asks, or a program runs in a terminal it closes |
| `z` | On the agent pane, zoom: hide the side panes, or bring them back when the width allows |
| `q` | Quit; a second `q` within 3 s while an agent works or asks, or a program runs in a terminal |
| `Esc` | Close the key list, else the diff, else back to agent mode |
| `i` | Back to agent mode, as `Ctrl+Q`, closing the diff with the mode; nothing while the key list is open |
| `j` `k` `Up` `Down` `Enter` | On Projects with the list of sessions shown, from two sessions on or with a repository the agent shown wrote in: move the list's cursor, show the session, terminal or viewer under it and go back to agent mode, or have the changes pane follow the repository under it and highlight that pane |
| `j` `k` `Up` `Down` `l` `h` `Enter` | On Projects with the file tree shown: move the tree's cursor; `l` or `Enter` opens the directory under it and `h` folds it or goes to the one above; `Enter` on a file shows it in the viewer and highlights the center pane |
| `j` `k` `Up` `Down` `PgUp` `PgDn` | On the agent pane: scroll the history of the tab shown a line or a page; the alternate screen has no history. On the viewer, scroll the file |
| `j` `k` `Up` `Down` `s` `u` `a` `d` | On Changes: move the selection, stage or unstage the selected row, stage all, diff of the selected row |
| `c` | On Changes: open the commit box; needs a repository with something staged, and no commit running |
| `p` | On Changes: push the branch of the session shown, at a second `p` within 3 s with no other key between; needs something to push, and no push running from that repository |
| `l` | On Changes: open lazygit in a terminal tab, in the root of the repository the pane shows, and go back to agent mode; a notice says so when `lazygit` is not on the `PATH` |
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

### Start screen

| Key | Action |
|---|---|
| `Enter` | Reopen the sessions of the last run, showing the current directory's when it is one of them; without a last run, as `.` |
| `.` | Open the current directory, continuing its last conversation, with the last run around it |
| `1` … `9` | Open that recent project as the account its row names, with the last run around it |
| `o` | Open the project picker over the screen; `Esc` or `Ctrl+Q` goes back to the screen |
| `q` / `Ctrl+C` | Quit; nothing starts |

### Project picker

| Key | Action |
|---|---|
| typing / `Backspace` / `Ctrl+H` | Typed text, or the first line of a paste, goes at the end of the query; `Backspace` or `Ctrl+H` deletes its last character |
| `Up` / `Down` | Choose the row above / below |
| `Left` / `Right` | With more than one account, pick the account before / after the one marked under the query, for the row chosen |
| `Enter` / `Tab` | Open the choice, continuing its last conversation / on a new one, and go back to agent mode; on an open session, both show it |
| `Esc` / `Ctrl+Q` | Close the picker, back to termis mode with Projects highlighted, or the agent pane with the side panes hidden / to agent mode |

### After the agent ended

| Key | Action |
|---|---|
| `r` | Relaunch it, in agent mode or on the agent pane in termis mode, with its tab shown |
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
no mode, cursor or tab, and neither does the click that brings the
window forward. On the status bar a click does nothing, but with
`--spike` it goes back to agent mode.

A click on a row also moves the cursor or the selection there, as `j`
and `k` would: a session, terminal, viewer or repository in the list
of sessions, an entry of the file tree, a file on the right pane. The
keys then act on that row. A double click on a row of the list of
sessions or of the file tree acts as `Enter` on it; on a file of the
right pane it shows the file's diff over the column, as `d` does. A
click on a tab's name in the center pane's title shows that tab, as
`,` and `.` do, and goes to agent mode; the agent or the program that
asks for the mouse does not get that click. For 400 ms after it,
another click on the title does nothing, so that a double click shows
one tab.

On the start screen a click on a row does what its key does.

To select with the terminal, hold Option while dragging in iTerm2, or
Shift in kitty, WezTerm and Alacritty.

## Config file

`termis` reads `$XDG_CONFIG_HOME/termis/config.toml`, else
`~/.config/termis/config.toml`, when it starts. Every table and key is
optional; these are the defaults:

```toml
[keys]
termis-mode = "ctrl+q"  # Ctrl and one of a e k l n o p q r s t v w x y

[widths]                # percent of the terminal's width
projects = 20           # 15 to 40, both together at most 60
changes = 25

[git]
poll-seconds = 2        # 1 to 60

[theme]
text = "default"
accent = "#62c3b1"
asking = "#ffb547"
alert = "#e86e6e"
added = "#8ccf7e"
removed = "#e88388"
modified = "#7aa2f7"
muted = "#8b929c"
faint = "#464b53"
chip = "#3a3f47"
chip-text = "#e6e8eb"
ink = "#1a1c20"

[start]
screen = true           # false: termis alone starts as termis . does
```

The key `termis-mode` sets takes the place of `Ctrl+Q` in this guide,
the status bar and the key list, and no longer reaches the agent or a
terminal; `Ctrl+Q` then does. `poll-seconds` is how often the
repository shown is read besides after a change to its files.

A colour is `#rrggbb`, `default` for the terminal's own colour, or one
of the terminal's 16 colours by name, `black` to `white` and
`bright-black` to `bright-white`, which follow its theme on a light
background. In the [mockup's list](mockup.md) of what the colours mark,
`accent` is the teal, `asking` the amber, `alert` the red, `added` and
`removed` the green and red of lines, `modified` the blue and `muted`
the grey; `faint` draws the rules, `chip` and `chip-text` the grey
chips, `ink` the text on the coloured ones, and `text` a title's
subject, the viewer's lines and a limit's use under 80%.

`[[accounts]]` tables name the accounts sessions can run as, each with
its agent, `claude` unless `agent` says `codex`, and the `dir` that
holds its login, settings and conversations, as `CLAUDE_CONFIG_DIR` or
`CODEX_HOME` would name it:

```toml
[[accounts]]
name = "claude"         # no dir: Claude's own, ~/.claude

[[accounts]]
name = "work"
dir = "~/.claude-work"

[[accounts]]
name = "codex"          # Codex's own, ~/.codex
agent = "codex"
```

An agent's account without `dir` runs it as your shell does. Without a
table for Claude's, it is called `default` and comes first, so one
table with a `dir` makes two accounts. A directory no agent ran in, and
with no account kept for it, runs as the first account without `dir`.
A name is up to 16 ASCII letters, digits, `-` or `_`. Write `dir` as
you write `CLAUDE_CONFIG_DIR` in a shell: Claude names each login in the
keychain after that text, so `~/.claude-work` and `~/.claude-work/`
are two logins. `~/.claude` and `~/.codex` are not a `dir`: leave `dir`
out for them. A Codex account's `dir` must exist.
Claude reads an account's `CLAUDE.md`, settings, hooks, skills and
plugins from its `dir`, so those in `~/.claude` do not apply to it;
copy or link the ones you want there. With two accounts or more, the
project picker shows the one each project opens as and lets you pick
another; see [Sessions](#sessions).

A file with a table or key `termis` does not know, or a value out of
range, stops it before it takes the screen, with the line and the
reason.

## Known limits

- Codex's section shows its state alone, and the repositories Codex
  writes in are not listed under its session.
- A selection covers what the center pane shows: a drag past its edge
  does not scroll. The side panes cannot be selected.
- A terminal does not get `Alt+1` to `Alt+9`, which bash and zsh read
  as a repeat count.
- Claude's and Codex's fullscreen views keep no history in `termis`;
  each agent scrolls its own. `tui.alternate_screen = "never"` in
  Codex's `config.toml` keeps Codex inline.
- With `CLAUDE_CODE_DISABLE_TERMINAL_TITLE` set, Claude reports no state
  to `termis`: after an interrupt with `Esc` or a denied permission, the
  agent's state holds until Claude's next event.
- With `--bare`, `disableAllHooks`, `allowManagedHooksOnly` or a managed
  status line, the agent's section shows less, down to its state alone.
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
- The right pane stages whole files and has no branch, stash or rebase
  commands; `l` opens lazygit for them.
- The default colours suit a dark background, and a program that asks
  is told the background is dark.
- Quitting ends every agent and terminal; the next start reopens each
  session with its agent alone.
- A file written through `Bash` or in a terminal does not list its
  repository under the session. A directory looked up once keeps its
  answer until `termis` quits, so a repository created there later is
  not listed.
- Two `termis` running at once share the file of saved sessions; the
  last to change it wins. A second one reopens the same sessions, and
  two agents in one directory continue the same conversation.
- A Codex session cannot continue a conversation another `codex` has
  open, or closed less than a minute ago.
- Credentials in the environment `termis` starts with, such as
  `ANTHROPIC_API_KEY` or `CLAUDE_CODE_OAUTH_TOKEN`, and Codex's
  `OPENAI_API_KEY` or `CODEX_API_KEY`, apply to every account of their
  agent, and so does a Claude Console sign-in without an API key.
