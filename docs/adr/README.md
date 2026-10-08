# Architecture decision records

One short file per decision, numbered, with sections Status, Context,
Decision, Consequences. A decision that changes gets a new record; the old
one is kept and its Status line becomes `Superseded by ADR-NNNN`.

| # | Title | Status |
|---|---|---|
| [0001](0001-git-through-the-git-binary.md) | Git through the `git` binary, not libgit2 | accepted |
| [0002](0002-threads-and-channels-no-async-runtime.md) | Threads and channels, no async runtime | accepted |
| [0003](0003-inject-claude-telemetry-per-session.md) | Claude telemetry injected per session, never by editing user settings | accepted |

The terminal emulator and the input model are decided by the M1 spike and
recorded as ADR-0004 and ADR-0005 at its end.
