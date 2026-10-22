# 0008. Several sessions in one process

Status: accepted, 2026-10-13

## Context

M8 lets one `termis` run several projects, each with its own agent, and
shows at a glance which agent is working, waiting or asking. 0.1 keeps
one session in the main loop: one PTY, one emulator, one instance
directory `termis-<pid>`, one git worker, and a quit that, once the loop
has ended, waits up to 3 s for the agent to hang up, drawing nothing.
The panic hook knows one child and one directory, and a second instance
directory would replace the first.

Two sessions can work in the same repository, and one session can write
into repositories other than its own. Git's index belongs to the
repository, not to the session. A session started outside a repository
has no git worker. Git runs on the main thread once, before the screen
is taken, to find the start directory's repository; after that it never
does (ADR-0001).

## Decision

One process owns every session. A session is the agent's PTY, an
emulator with its own scrollback, the lifecycle of its exit and
relaunch, an instance directory and a watch on its telemetry. A
repository is one git worker, shared by every session that uses it, with
the changes pane's state: the snapshot and its poll, the selection, the
diff view, the commit draft and the commit in flight.

Sessions and repositories get ids from counters that only grow. Every
PTY and git message on the one channel (ADR-0002) carries the id of what
sent it, tagged by the thread that produces it: PTY events their session
and relaunch generation, git events their repository. Host input belongs
to no session, and the finder's answers name the directory they answer.
An event with an unknown id is dropped, so a straggler of a closed
session or a released repository reaches nothing.

Hidden sessions keep reading their agent's output, answering its queries
and following resizes. An agent that asked for focus events gets a
focus-out while hidden and a focus-in when shown. Keys, pastes and the
mouse reach the session shown. Only the shown session's output redraws
the screen; a hidden one redraws only when what the list shows of it
changes.

Instance directories nest: `$TMPDIR/termis-<pid>/` for the process,
made, replaced and swept as before, and `<n>/` inside it for each
session, both readable by their owner only. Closing a session removes
its directory; quitting and the panic hook remove the process directory
with everything in it.

Closing a session hangs its agent's process group up, kills the group
after `GRACE` if the agent has not exited, and again after `KILL_GRACE`
if something in it still holds the terminal, through deadlines checked
on each pass of the loop, so the other sessions keep running. Quitting
moves every session through the same deadlines at once, after the loop
has ended, drawing nothing. The panic hook hangs up every agent that has
not exited; an agent's entry lives as long as its PTY.

The shown repository is refreshed every 2 s while the side panes are on
screen, as before. Other repositories run only `git status`, every 10 s,
while the list of sessions is on screen. A `git-finder` thread finds the
repository of a directory opened from the picker; its answers are cached
per directory. PTYs are spawned only from the main thread.

## Consequences

- ADR-0001, ADR-0002 and ADR-0003 stand; the roadmap's runtime model
  follows the code as each M8 commit lands.
- Two sessions in one repository share its commit draft, selection and
  diff.
- Long-lived threads: three per session (`pty-reader`, `pty-waiter`,
  `pty-writer`), one per repository in use, the finder and the input
  reader; a commit adds short-lived ones.
- A session's scrollback holds up to 5000 rows of 32-byte cells, each as
  wide as the screen was when it scrolled off, about 20 MB at 130
  columns; it does not grow while the agent is on its alternate screen.
- Hanging the agents up at quit takes at most `GRACE` plus twice
  `KILL_GRACE`, however many sessions there are; the wait for an exit
  already on its way runs within those deadlines.
- `termis` exits with the shown session's status when its agent exited
  on its own before the quit, which is the 0.1 rule for one session.
- A late git event for a released repository is dropped; a repository
  with a commit in flight is kept until the commit ends.
- `--spike` keeps one session.
