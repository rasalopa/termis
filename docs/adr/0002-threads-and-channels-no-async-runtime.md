# 0002. Threads and channels, no async runtime

Status: accepted, 2026-10-08

## Context

The application has a handful of concurrent concerns: host input, the PTY
reader, the child waiter, a git worker and a few timers. None of them needs
thousands of tasks, and an async runtime would colour every library choice.

## Decision

Each concern runs on its own `std::thread` and sends a `Msg` over one
`std::sync::mpsc` channel to the main loop. The main loop owns the terminal
emulator state and the PTY writer (no mutex), blocks on `recv_timeout`, drains
the channel to coalesce bursts, and draws once per iteration when something
changed, with a 16 ms floor. No tokio in 0.x.

## Consequences

- Blocking git commands stay on the worker; a stuck worker never stops the
  center pane from rendering.
- Libraries are chosen for synchronous APIs.
- Timers (prefix timeout, elapsed clock, polls) are deadlines checked on each
  loop iteration, not tasks.
