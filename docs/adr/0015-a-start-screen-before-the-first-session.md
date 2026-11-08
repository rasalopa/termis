# 0015. A start screen before the first session

Status: accepted, 2026-11-07

## Context

`termis` with no argument starts the agent in the current directory
and reopens the sessions of the last run around it (ADR-0008). The
user asked for a screen like Neovim's at start: the name `termis`
and the recent projects, so that the first key chooses what opens.
The project picker already lists the projects the agent ran in, per
account (ADR-0014).

## Decision

`termis` with no path, no option and nothing after `--` shows a start
screen before any session starts, unless `screen = false` in the
config file's `[start]` table. It shows a logo, the version, the
sessions of the last run and up to nine of the picker's recent
projects.

Each key on it ends in a start `termis` already makes. `Enter`
reopens the last run, as `termis <dir>` does for its first directory
that still opens, or the current one when the run has it. `.` is
`termis .`. `1` to `9`, and a choice in the picker that `o` opens,
are `termis --account <account> <dir>`, and `Tab` in the picker adds
`--new`. `q` quits; no agent starts and no file is written.

The screen is a step of the start, not a mode of the main loop, which
keeps at least one session while it runs. It reads the host's input
itself, before the input thread starts. The config file, the accounts,
their file and git for the current directory are read before the
screen is taken. Git for a chosen directory and for the saved ones
runs after.

## Consequences

- `termis` alone takes one key more; `termis .` and `screen = false`
  start as before.
- Whatever opens from the screen joins the last run, as with
  `termis <dir>`: the saved sessions that still open stay saved.
- With the screen shown, git also runs after the screen is taken, for
  the chosen directory and the saved ones; ADR-0008 has it run only
  before.
- `[start]` is a sixth table of the config file, after those of
  ADR-0011 and `[[accounts]]` (ADR-0014).
- A current directory whose remembered account is gone no longer
  stops `termis` alone: `.` says so on the screen.
