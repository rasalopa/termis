# 0004. Agent-agnostic core, Claude Code as the first adapter

Status: accepted, 2026-10-08

## Context

The center pane runs Claude Code today, and other coding agents (Codex,
OpenCode, Gemini CLI) are expected later. Each agent has its own launch
command, its own way of exposing session state, and its own key
expectations. Spreading that knowledge through the UI and the app state
would make the second agent a rewrite.

## Decision

The core knows only "a command running on a PTY". Everything specific to
an agent lives behind an `Agent` trait in `src/agent/`: launch command and
arguments, environment, telemetry source, mapping of events to the states
`working`, `waiting` and `needs attention`, and the display name shown in
the pane title. `src/agent/claude.rs` is the only implementation in 0.1 and
the default. No module outside `src/agent/` names Claude.

## Consequences

- Adding an agent is one adapter file plus a CLI option to select it.
- Telemetry depth varies by agent; the projects pane shows what the adapter
  provides and leaves the rest blank.
- The spike checklist is run per adapter; the M1 results cover Claude.
- User-facing text says "agent" or the adapter's display name, never a
  hard-coded product name.
