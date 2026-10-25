# 0009. Terminals as tabs of a session

Status: accepted, 2026-10-23

## Context

M9 runs commands and servers for a project without leaving `termis`.
Since ADR-0008 a session is its agent's PTY, an emulator with its own
scrollback, the lifecycle of the agent's exit and relaunch, an instance
directory and a watch on its telemetry, and the center pane shows the
agent of the session shown. A shell needs a PTY and an emulator of its
own and none of the rest: it has no telemetry to read, no conversation
to continue and nothing to relaunch.

An interactive shell does job control. It runs each command in a
process group of its own, which it makes the terminal's foreground group
while the command runs, and `Ctrl+Z`, which `termis` keeps from the
agent, stops that command and gives the terminal back to the shell.
Signals sent only to the shell's process group, as an agent's are,
would not reach a server running in the foreground.

## Decision

A session owns tabs: its agent's tab first, then the terminal tabs
opened in it and, from M10, the viewer's, in the order they opened. A
tab with a child, the agent's or a terminal's, is a PTY with an
emulator, its scrollback and the lifecycle of its child's launch; the
viewer's has no PTY and shows a file that a short-lived `file-reader`
thread read. What only the agent has stays with the session: its
instance directory, the watch on its telemetry and the status read from
it, and the arguments a relaunch reuses. A terminal has no agent,
telemetry, instance directory or relaunch.

A terminal runs the user's shell, `$SHELL`, else `/bin/sh`, as a login
shell in the session's directory, with the terminal environment the
agent gets: `TERM` and `COLORTERM` as the agent has them, and the host
terminal's variables removed. `Ctrl+Z` reaches it. When the shell exits,
its tab closes. From M10 a terminal can run the user's editor on a file
instead, through `sh -c`, with the same environment; `sh -c` has no job
control to resume a stopped editor, so `Ctrl+Z` does not reach it, and
the editor counts as a program running in the terminal until it exits.

The center pane shows one tab of the session shown. Keys, pastes and
the mouse reach that tab's child, and only its output redraws the
screen; the viewer, without one, takes the scroll keys and the wheel
and drops the rest. Tabs not shown keep reading their output, answering
their queries and following resizes, as hidden sessions do, and a tab
that asked for focus events gets a focus-out while hidden and a
focus-in when shown.
PTY events carry their tab as well as their session, under ids from
counters that only grow, so a straggler of a closed terminal is
dropped.

`n` and `N`, the list of sessions, the count of sessions asking and the
rule that closing the last one quits go by session. Closing a session
closes its terminals.

A terminal is hung up through two process groups: the shell's, and the
PTY's foreground group, which `tcgetpgrp` on the master reads at the
hangup. The PTY has no foreground group once the shell exited, so the
one read then is kept for the kill. Both get SIGHUP, then SIGKILL after
`GRACE`, so a server running in the shell's foreground stops with it.
`termis` did not start the foreground group and sees no exit of its
leader, so before the kill it looks for a process with the group's id.
If one runs in a session other than the shell's (`getsid`), a stranger
took the id and the kill is skipped. If there is none, the id stays
reserved while the group has members, and the kill reaches no one else.
When a shell exits on its own, its group gets SIGKILL, as an agent's
does once it exited. The panic hook hangs up every terminal's shell
too.

## Consequences

- ADR-0008 stands, with the session's PTY, emulator and scrollback in
  its agent's tab; the roadmap's runtime model follows the code as each
  M9 commit lands.
- Long-lived threads: three more per terminal (`pty-reader`,
  `pty-waiter`, `pty-writer`); the viewer adds a short-lived
  `file-reader` for each file it reads.
- A terminal's scrollback holds up to 5000 rows, as an agent's does:
  about 20 MB at 130 columns.
- `Ctrl+Z` in a terminal stops the command in its foreground, as in any
  terminal; on the agent's tab, and on an editor's, it does nothing.
- A job the shell runs in the background, started with `&` or moved
  there with `bg`, has a process group of its own and gets only the
  hangup the shell passes on, so one that ignores it, as under `nohup`,
  outlives the tab.
- A terminal whose shell exits leaves no screen behind and has nothing
  to relaunch.
