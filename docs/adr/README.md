# Architecture decision records

One short file per decision, numbered, with sections Status, Context,
Decision, Consequences. A decision that changes gets a new record; the old
one is kept and its Status line becomes `Superseded by ADR-NNNN`.

| # | Title | Status |
|---|---|---|
| [0001](0001-git-through-the-git-binary.md) | Git through the `git` binary, not libgit2 | accepted |
| [0002](0002-threads-and-channels-no-async-runtime.md) | Threads and channels, no async runtime | accepted |
| [0003](0003-inject-claude-telemetry-per-session.md) | Claude telemetry injected per session, never by editing user settings | accepted |
| [0004](0004-agent-agnostic-core-claude-first.md) | Agent-agnostic core, Claude Code as the first adapter | accepted |
| [0005](0005-vt100-emulator-with-query-replies.md) | vt100 behind tui-term, with query replies in our layer | accepted |
| [0006](0006-host-events-encoded-per-child-protocol.md) | Host key events encoded per the child's protocol | accepted |
