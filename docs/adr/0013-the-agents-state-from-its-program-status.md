# 0013. The agent's state from its program status

Status: accepted, 2026-11-02

## Context

The state at the foot of the projects pane comes from Claude's hooks
(ADR-0003). No hook says that a turn ended when `Esc` interrupts it or
the user denies a permission, and the `PostToolUseFailure` of a tool
that `Esc` stops reads as working. So `working` or `asking` stays
until Claude's next event: a prompt, or the idle notice, which Claude
sends after 60 s by default. With `--bare`, `disableAllHooks` or
`allowManagedHooksOnly` no event comes at all.

Claude Code 2.1.316 asks its terminal at start whether it takes
program status reports, with `OSC 7501 ; ?`, right before a device
attributes query; an answer that comes after the one to that query
counts as none. When an OSC 7501 that starts with `?` comes back,
Claude sends an OSC 7501 report each time its state changes: fields
such as `state=working:app=claude-code`, joined by `:`. The state is
`working`; `blocked`, with a `kind` of `permission`, `question` or
`auth`, or with none while one of Claude's own dialogs, such as
`/config`, is open; `done`, `idle` or `error`. A turn stopped by `Esc`
or by a denied permission reports `idle`. Each background task gets
reports of its own, with an `id` field, and `state=clear` withdraws a
task's report when the task ends. Claude withdraws its own report with
`state=clear` when it starts to shut down, before its `SessionEnd`
hooks, which get at least 1.5 s; the process ends after them. Claude
does not ask when `CLAUDE_CODE_DISABLE_TERMINAL_TITLE` is set. No
specification of the protocol is published.

`claude agents --json` lists the running sessions with `busy`,
`waiting` or `idle`, from the record each one keeps in
`~/.claude/sessions/<pid>.json`. Each look starts a Claude process,
and the command is off with `CLAUDE_CODE_DISABLE_AGENT_VIEW` or the
`disableAgentView` setting.

## Decision

In a session with telemetry, the emulator of the agent's tab answers
`OSC 7501 ; ?` with `OSC 7501 ; ? ST` and keeps the last report that
has no `id` field, until one with `state=clear`. The terminals'
emulators answer nothing and keep nothing. So does the agent's in a
session without telemetry, which shows no state: there Claude keeps
the `✳` or spinner before its title, as before.

Each time `termis` reads the telemetry, every 250 ms, the adapter
turns the agent's last report into a state. The last state a report
gave takes the place of the one the hook events give, until the agent
ends or is relaunched; a withdrawn report leaves it, since Claude
withdraws its own while it shuts down, when its events may still say
working or asking. Claude's adapter reads only reports with
`app=claude-code`: `working` is working, `blocked` of any kind
asking, and `done`, `idle` and `error` waiting. The hooks still name
the tool that runs, and give the state until a report gives one, and
while the last report names a state the adapter does not know.

`termis` does not run `claude agents --json`, read
`~/.claude/sessions` or read the transcript.

## Consequences

- The state leaves `working` or `asking` at the next read after an
  interrupt or a denied permission, within about 250 ms of Claude's
  report, and shows without hooks.
- Once answered, Claude puts no `✳` or spinner before its terminal
  title, so the center pane's title shows the conversation's name
  alone.
- If a later Claude sends no reports, or sends a state the adapter
  does not know, the state comes from the hooks again.
- With `CLAUDE_CODE_DISABLE_TERMINAL_TITLE` set, the state comes from
  the hooks alone, as before.
- A report is kept as sent, up to the emulator's cap on an OSC, 4096
  bytes; the fields the adapter reads come first.
- A relaunched agent gets an empty emulator, which answers again and
  keeps nothing of the last launch's reports, nor the state they gave.
- A Claude started in a terminal tab gets no answer and keeps its
  title as it was.
