# 0011. A config file read at start

Status: accepted, 2026-10-28

## Context

The key that switches to termis mode, the widths of the side panes,
the interval of the repository poll and the colours were constants:
Ctrl+Q, 20% and 25%, 2 s, and a palette chosen for a dark background.
The config file is part of the public interface, as the CLI and the
keys are.

git, tmux, Alacritty and Helix read their files from
`$XDG_CONFIG_HOME/<name>/`, else `~/.config/<name>/`, on macOS too, and
termis keeps its state file under `XDG_STATE_HOME` the same way.

A host without the kitty keyboard protocol sends Ctrl+I as Tab and
Ctrl+M as Enter, and Ctrl+] and Ctrl+\ as control bytes that crossterm
reports as `5` and `4`; under the protocol they arrive as the keys
themselves. Ctrl and any other letter arrives the same way from both.

## Decision

`termis` reads `$XDG_CONFIG_HOME/termis/config.toml`, else
`$HOME/.config/termis/config.toml`, once, at start, before it looks for
git or takes the screen. An empty or relative `XDG_CONFIG_HOME` counts
as unset. No flag or variable names another file. A missing file gives
the defaults; a link to a missing file cannot be read.

The file is TOML, read through serde, so an error names its line and
column. It has four tables, every key optional:

- `[keys] termis-mode`, default `ctrl+q`: Ctrl and one of a, e, k, l,
  n, o, p, q, r, s, t, v, w, x, y. c and z stay with the agent and the
  terminals; b, d, f, g, h, j and u are termis's own Ctrl keys in
  termis mode, the viewer, the commit box and the picker; i and m are
  Tab and Enter on some hosts. The key takes Ctrl+Q's place in the
  status bar and the key list, and Ctrl+Q then reaches the agent and
  the terminals.
- `[widths] projects` and `changes`, defaults 20 and 25, in percent of
  the terminal's width: each from 15 to 40, together at most 60, so at
  100 columns, below which the side panes fold away, each has at least
  15 columns and the center at least 40.
- `[git] poll-seconds`, default 2, from 1 to 60: how often the
  repository the changes pane shows is read besides after a change to
  its files (ADR-0010). The gap after a change still doubles up to 2 s,
  or up to the poll when it is shorter. The status read for the list's
  dots stays at 10 s.
- `[theme]`: `text`, `accent`, `asking`, `alert`, `added`, `removed`,
  `modified`, `muted`, `faint`, `chip`, `chip-text`, `ink`, each
  `#rrggbb`, `default` for the terminal's own colour, or one of the
  terminal's 16 colours by name. The defaults are the colours of 0.6.0.

A file that cannot be read or is not a regular file, larger than 64
KiB, not UTF-8 or not TOML, or with a table, key or value termis does
not take stops `termis` with status 1 and the reason on stderr, before
it takes the screen or starts an agent. The `statusline` and `hook`
subcommands do not read it.

## Consequences

- A key added by a later version stops an older `termis`, with a
  message naming the key.
- A change takes effect at the next start, which reopens the sessions.
- Tests do not read the user's file: the path comes from the
  variables' values passed in, and only `app::run` reads
  `XDG_CONFIG_HOME` and `HOME` for it.
- serde with its derive is a direct dependency; it was in Cargo.lock
  already, and a clean build takes a second longer.
- The drawing code takes its colours from the palette of the frame it
  draws, which the view carries; the main thread draws (ADR-0002).
- ADR-0010 stands: its 2 s are the default poll, and the gap's 2 s stay
  when the poll is longer, so ignored noise costs what the 2 s poll
  costs and a change waits at most 2 s while it lasts.
- A program in the center pane that asks for the terminal's background
  is still told it is dark, whatever `[theme]` says.
