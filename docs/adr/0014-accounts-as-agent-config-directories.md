# 0014. Accounts as the agent's configuration directories

Status: accepted, 2026-11-05

## Context

A user with two Claude accounts, a personal one and one for work, wants
each project to run as one of them. Claude Code keeps an account's
login, settings, plugins and conversations under its configuration
directory: the one `CLAUDE_CONFIG_DIR` names, else `~/.claude`. The
account's identity and the projects it ran in are in `.claude.json`,
in that directory, or in the home for `~/.claude`. Claude's
documentation gives each account a directory of its own, as in
`alias claude-work='CLAUDE_CONFIG_DIR=~/.claude-work claude'`.

On macOS, Claude Code 2.1.318 keeps a claude.ai login in a keychain item
named `Claude Code-credentials`. When `CLAUDE_CONFIG_DIR` is set, the
name ends in `-` and the first 8 hex digits of the SHA-256 of the
variable's text, whatever directory that text names: set to
`~/.claude`, the variable finds no login there. When
`CLAUDE_SECURESTORAGE_CONFIG_DIR` is set, its text names the item in
place of the other, and a Claude session hands it on to the background
sessions it starts. `--continue` and `--resume` read `projects/` in the
directory, so each account has its own conversations. Claude also reads
the user's `CLAUDE.md`, `settings.json`, skills and plugins there.

The core does not name Claude (ADR-0004). What an account is for
another agent, a variable or nothing, is up to its adapter.

## Decision

- The config file declares accounts in `[[accounts]]` tables, each a
  `name` and an optional `dir`. The account without `dir` is the
  agent's own, run as before with the environment `termis` has,
  `CLAUDE_CONFIG_DIR` set or not; it is named `default` unless a table
  without `dir` names it. A name is 1 to 16 ASCII letters, digits, `-`
  or `_`. A `dir` is absolute or starts with `~/`, which becomes `HOME`,
  and holds no `..` and no control character. Two accounts may not share
  a name, case aside, or a directory, by its text or once links are
  followed, and no account may name a directory the adapter says holds
  the agent's own account: for Claude, the home, `~/.claude` and the
  `CLAUDE_CONFIG_DIR` `termis` was started with. Any of these stops
  `termis` before it takes the screen, as the rest of the file does
  (ADR-0011).
- The core hands an account's directory to the adapter for the launch,
  the commit subject and the recent projects. Claude's adapter sets
  `CLAUDE_CONFIG_DIR` to the directory as written, but for `~/`, removes
  `CLAUDE_SECURESTORAGE_CONFIG_DIR`, and reads the account's
  `settings.json`, `projects/` and `.claude.json`. The agent's own
  account leaves the environment as it is.
- With more than one account, the project picker shows them under its
  query. `Left` or `Right` picks one for the row chosen, which `Enter`
  or `Tab` then opens; choosing another row or typing drops the pick.
  Each row names the account it opens as: its session's, else the
  one remembered for its directory, else the one whose `.claude.json`
  says the agent started there last, else the agent's own. An open
  session keeps its account. `termis --account <name>` picks one for
  the directory `termis` starts in.
- While there is more than one account, `termis` keeps the account of
  each directory a session opens in, in `accounts.json` beside the file
  of the sessions of the last run: a version, then up to 1000
  directories with their account's name, the last set first, written as
  that file is. It holds names only: the directory the agent gets comes
  from the config file. A file `termis` cannot read stops it before
  it takes the screen. A directory kept with an account the config file
  no longer declares does not reopen at start, opens from the picker
  only once another account is picked, and stops `termis` when it is
  the directory `termis` starts in without `--account`.
- With one account nothing changes: no line in the picker, no name in
  the agent's heading, and the file is neither read nor written, so
  every directory runs as the agent's own.

## Consequences

- The heading of the agent's section reads `Claude · work` while there
  are several accounts.
- The first session as a new account shows Claude's login in the agent
  pane. `termis` neither makes the directory nor logs in, and copies
  nothing into it: an account runs without the `CLAUDE.md`, settings,
  hooks, skills and plugins of `~/.claude` until the user puts them in
  its directory.
- A directory opened as another account starts a new conversation; the
  old one stays with the first account.
- `Ctrl+G` writes subjects as the account of the session shown.
- A terminal tab, the editor's and lazygit's too, gets the variables
  the adapter sets for its session's account and loses those it
  removes, as the agent does, so `claude` typed in it runs as that
  account.
- Credentials in the environment, such as `ANTHROPIC_API_KEY` or
  `CLAUDE_CODE_OAUTH_TOKEN`, apply to every account, as they do to
  `claude` in a shell, and so does a Console sign-in without an API
  key, which Claude keeps outside the directory.
- The config file and the new state file are part of the public
  interface; `--account` is part of the CLI.
