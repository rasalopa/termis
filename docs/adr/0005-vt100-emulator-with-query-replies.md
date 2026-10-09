# 0005. vt100 behind tui-term, with query replies in our layer

Status: accepted, 2026-10-08

## Context

The center pane needs a terminal emulator. ADR-0002 (threads) and the M1
spike narrowed the choice to `vt100` with `tui-term`, which other ratatui
projects already use to host Claude Code, against `alacritty_terminal`,
which is more complete but needs its own widget. `vt100` does not answer
terminal queries, does not track the kitty keyboard flags or xterm
modifyOtherKeys, and ignores synchronized output.

## Decision

Use `vt100` 0.16 rendered through `tui-term` 0.3. The emulator module
answers the queries the agent sends (DA1, DA2, DSR, cursor position,
DECRQM, XTVERSION, XTQMODKEYS, the kitty flags query, OSC 10/11 colours)
from vt100's unhandled-sequence callbacks, and tracks the modes vt100
does not: kitty flag stacks per screen, modifyOtherKeys, focus events.
The public surface is tui-term's `Screen` trait, so `alacritty_terminal`
can replace vt100 behind an adapter if a check ever fails.

Evidence from the spike: Claude Code 2.1.295 in iTerm2 passed every check
of `docs/spike-checklist.md`; a recorded startup is replayed in a test.

## Consequences

- Colour query replies always end in ST: vt100 does not pass the query's
  terminator on. Claude Code accepts it.
- Known rendering quirks to watch: vt100 gives emoji with a variation
  selector width 1 while ratatui measures 2, and bold plus dim renders as
  dim only.
- A full reset by the child (`ESC c`) does not clear the tracked modes;
  vt100 handles it without a callback.
- The herdr and tmux columns of the checklist are still to be run.
