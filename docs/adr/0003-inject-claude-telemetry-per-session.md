# 0003. Claude telemetry injected per session, never by editing user settings

Status: accepted, 2026-10-08

## Context

Session telemetry (model, context, cost, lines) comes from the JSON Claude
Code pipes to its status line command; activity comes from hooks. A user's
`~/.claude/settings.json` may already define a status line and hooks owned by
other tools, and those tools may rewrite the file. Parsing the pane's screen
to guess state is fragile and ties the project to Claude's rendering.

## Decision

`term` launches `claude` itself, so it passes its configuration on the command
line with `claude --settings '<json>'`, which applies to that session only:
`statusLine` overrides the user's for the session, `hooks` lists merge. Before
launching, `term` copies the user's current `statusLine` command into its
instance directory; `term statusline` tees the JSON to `status.json` and then
runs that previous command with the same input, so the user's own status line
keeps rendering. `term hook` appends hook events as JSON lines. The instance
directory is passed to the child as `TERM_TUI_DIR`, which hooks and the status
line inherit, so two `term` windows never mix.

`term` never writes `~/.claude/settings.json`.

## Consequences

- Telemetry needs no setup; without `--settings` support the pane degrades to
  git-only data.
- The shim must be fast: Claude debounces the status line at 300 ms.
- Files are written atomically (write, then rename) and removed on quit.
- If a future Claude release changes `--settings` merge rules, this ADR is
  superseded rather than patched.
