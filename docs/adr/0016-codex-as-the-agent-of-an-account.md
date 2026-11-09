# 0016. Codex as the agent of an account

Status: accepted, 2026-11-08

## Context

The user runs Claude Code and the OpenAI Codex CLI and wants projects
of both in one `termis`. Each session already runs as an account of the
config file, chosen per directory in the picker, on the start screen
and with `--account`, and kept in `accounts.json` (ADR-0014). ADR-0004
and the roadmap planned a CLI option, `--agent`, which would choose one
agent per process.

Codex 0.167 starts its TUI as `codex`. `codex resume --last` continues
the latest interactive conversation of the working directory and
starts a new one when there is none. By default the TUI hands the
conversation to a background server shared by every `codex`, which
unloads a conversation only once it is idle and no client follows it,
60 s later by default; `--no-daemon` keeps the server in the TUI's
process. Only one process at a time writes a conversation: another
that resumes it gets "already has an active writer". From 0.163 Codex
sends its terminal OSC 7501 reports such as `state=working:app=codex`
without asking first, `blocked` while it asks, and `state=clear` when
it exits.

Codex has no status line command, and from 0.129 its hooks run only
once the user trusts them, on a screen that writes its `config.toml`.
Each conversation is a file under `$CODEX_HOME/sessions` whose first
line names its directory. `CODEX_HOME` is `~/.codex` unless set, and
must exist.

## Decision

- An `[[accounts]]` table takes `agent`, `claude` or `codex`, `claude`
  when left out. Each session runs the agent of its account, and
  sessions of both agents run in one process. `--account` chooses;
  there is no `--agent`.
- Each agent has at most one account without `dir`, its own. Claude's
  own is added first, as `default`, when no table declares it. A
  directory nothing chose runs as the first account without `dir`, in
  the file's order. The name `default`, in `accounts.json` and
  `--account`, is Claude's own.
- `sessions.json` and `accounts.json` keep their format: a directory's
  agent follows from the account kept for it.
- The agent's heading and the picker name an account only while its
  agent has more than one; the picker shows the agent's name for an
  account alone with its agent.
- Codex's adapter starts `codex --no-daemon resume --last` and the
  user's arguments, or `codex --no-daemon` and the arguments for a new
  conversation or when the arguments hold a word that is neither an
  option nor an option's value: a subcommand or a prompt, which only
  Codex's subcommand list, different in each release, tells apart.
  `--no-daemon` keeps the conversation in the session's process group,
  so closing the session stops it, as it stops Claude. The adapter
  passes nothing else, so Codex's configuration and hooks apply as
  they do in a shell. It withholds the variables by which Codex would
  take `termis` for the host terminal, `ITERM_PROFILE` among them.
- Codex's state comes from its reports with `app=codex`, read as
  Claude's are (ADR-0013).
- The picker lists the directories of Codex's interactive
  conversations, from the first line of its 1000 newest session files.
- For Codex an account's `dir` is `CODEX_HOME`, and the account's
  session runs without `CODEX_SQLITE_HOME`. `~/.codex` and the
  `CODEX_HOME` `termis` has hold Codex's own account.

## Consequences

- A config file without `agent` runs as before.
- An older `termis` stops at a config file with `agent`.
- Codex's section shows its state alone.
- `termis` needs Codex 0.163.0 or later; an older `codex` stops on
  `--no-daemon` or shows no state.
- Codex's background server and the sessions it shares are not used
  by sessions `termis` starts. A session cannot continue a
  conversation that a `codex` outside `termis` has open, or that the
  background server keeps for up to a minute after it was closed.
- With a prompt in the arguments, Codex starts a new conversation,
  where Claude continues the last one.
- `Ctrl+G` says codex cannot suggest a subject.
- A Codex account's `dir` must exist; `termis` does not make it.
- ADR-0004's "one adapter file plus a CLI option" becomes an adapter
  file and a name in the config file. ADR-0014 stands, with one own
  account per agent.
