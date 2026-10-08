# 0001. Git through the `git` binary, not libgit2

Status: accepted, 2026-10-08

## Context

The changes pane reads status, numstat, log and diffs, and writes with add,
restore and commit. `git2` (libgit2) does not run hooks, needs its own
signing and credential setup, and its status semantics differ from the CLI in
edge cases. Commits made from `term` must behave exactly as from the shell.

## Decision

All git access goes through `std::process::Command` running `git` with
machine-readable flags: `--no-optional-locks status --porcelain=v2 --branch
-z`, `--no-optional-locks diff --numstat -z -M`, `log --numstat
--format=...`, `commit -F -`. Output is parsed by small unit-tested
functions. Commands never run on the UI thread. Writes run with
`GIT_TERMINAL_PROMPT=0`, a closed stdin and a timeout, so a prompting hook or
a curses pinentry cannot hang the worker.

## Consequences

- Hooks, signing, credential helpers and worktrees work for free.
- Each refresh spawns processes; refreshes are polled at 2 s and only while
  the pane is visible.
- `--no-verify` exists behind a flag, never a key.
- Tests isolate git from the developer's configuration
  (`GIT_CONFIG_GLOBAL=/dev/null`, `GIT_CONFIG_NOSYSTEM=1`, fixed identity,
  `-c commit.gpgsign=false -c core.hooksPath=/dev/null`).
