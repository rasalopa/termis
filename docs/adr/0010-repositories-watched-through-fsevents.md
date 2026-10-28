# 0010. Repositories watched through FSEvents

Status: accepted, 2026-10-28

## Context

A file the agent writes, or a stage or commit made in a terminal tab,
reaches the changes pane at the next refresh, polled every 2 s
(ADR-0001, ADR-0008): 1.2 s after the write at the median and 1.8 s at
most in 12 trials. A faster `git status` poll would not see an edit to
a file that is already modified, since the status carries no line
counts, and a whole refresh runs seven git processes. On macOS, kqueue
needs a file descriptor per watched file; FSEvents watches a tree with
one stream. `unsafe` is forbidden in this crate, so the binding comes
from a crate: notify, as the backlog named.

The reads wrote the index: `git --no-optional-locks diff`
refreshes `.git/index` when a tracked file's stat data changed and its
content did not, as after `touch` or an editor saving the same bytes.
The next poll after such a change rewrote the index, and a user's `git
add` could fail on `index.lock`.

## Decision

notify 8, with only its `macos_fsevent` feature. The git worker of each
repository the poll refreshes watches it while it does: the work tree,
and the git directory and common directory that `git rev-parse` names,
canonicalized; each recursively, leaving out one inside another.

Every path counts as a change but these. Anywhere: a name starting
with `.watchman-cookie-`, the files watchman makes on each query of
git's `fsmonitor-watchman` hook. Under the git directory: a name
ending in `.lock`, what is under `worktrees/` or `fsmonitor--daemon*`,
and, outside `refs/`, what is under a component named `objects` or
`logs`. Under the common directory of a linked worktree: all but
`refs/`, `reftable/`, `packed-refs`, `config` and `info/`, and their
`.lock` files. Under the work tree: what is under a component named
`target`, `node_modules` or `.git`. An event with no path, a rescan
notice and a watcher error count as a change. The watcher sends the
worker's `Changed` once, then nothing until the worker begins its next
refresh.

The main loop reads the repository 100 ms after the first change it
sees (`AFTER_CHANGE`), once no read is in flight, and no sooner than a
gap after the last refresh ended. The gap is 250 ms
(`CHANGE_GAP`) and doubles, up to the poll's 2 s, after each refresh a
change started that read nothing new. A refresh that read something new
sets it back, and so does a poll refresh sent with no change seen since
the last refresh began. The 2 s poll stays as it is: a watch that
cannot start leaves the repository on the poll alone, with no notice.

The reads no longer write the index: the two porcelain `git diff`
reads run with `-c diff.autoRefreshIndex=false`.

## Consequences

- ADR-0001, ADR-0002 and ADR-0008 stand: their 2 s refresh still runs,
  and the reads after a change come on top of it.
- One more thread per watched repository, notify's run loop: two at
  most, and a third while the worker of one no longer watched finishes
  a long job, such as a commit's hooks.
- A change shows about 250 to 300 ms after it in a small repository,
  up to about 500 ms when it lands just after a refresh ended.
- Noise git ignores, such as a dev server's cache, a test run, linked
  worktrees nested in the work tree or ignored directories other than
  `target` and `node_modules`, costs reads that find nothing new; the
  doubling gap keeps that near the cost of the 2 s poll, and while the
  noise lasts a change can wait up to 2 s, as with the poll alone.
- A `target` or `node_modules` directory that git tracks waits for the
  2 s poll.
- A stage or a commit from `termis` costs one refresh that finds
  nothing new: its events arrive after the refresh that follows it
  began.
- A file touched with the same bytes is hashed again on each read until
  a git command of the user's refreshes the index.
- A linked worktree is watched through its own git directory and the
  refs and configuration of the common one.
- The `unsafe` of the FFI lives in notify and fsevent-sys, as the PTY's
  does in portable-pty. notify 8 aborts the process on a path that is
  not UTF-8, which APFS and HFS+ names are not, and on an FSEvents flag
  it does not know, of which the macOS 27 SDK has none; a panic in the
  handler would abort it too, so the handler only checks paths and
  sends. It does not report a stream that fails to start, nor dropped
  events FSEvents reports at a parent of the watched directory; the 2 s
  poll covers both.
