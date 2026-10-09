# Spike checklist (M1)

`term --spike` runs Claude over the whole terminal, with no side panes. Every
check must pass in the three hosts before M2 starts. Record the result in the
table at the end and in ADR-0005 / ADR-0006.

Hosts

- A: iTerm2 directly.
- B: inside a herdr pane (the daily setup).
- C: inside tmux with `set -s extended-keys on` and
  `set -as terminal-features 'xterm*:extkeys'`.

Checks

1. Keys. `Enter` submits. `Shift+Enter` and `Ctrl+J` insert a newline. `Esc`
   interrupts a running turn. `Ctrl+C` twice exits Claude. `Tab` completes an
   `@` path; `Shift+Tab` cycles permission modes. Arrow keys move through
   prompt history. `Option+←/→` move by word.
2. Term mode. `Ctrl+Q` never reaches Claude: it enters term mode, shown by
   the `TERM` tag on the last row, and `Esc` returns. `Ctrl+Z` and `Ctrl+S`
   do nothing harmful.
3. Paste. Pasting three lines shows one `[Pasted text #1 +N lines]`
   placeholder. Dragging an image file onto the terminal attaches it.
4. Dialogs. `/config` borders, the permission prompt (arrows, `y`/`n`), the
   `/model` picker and the `@` file picker render and navigate correctly.
5. Rendering. The `✻ Brewing…` spinner animates without stale glyphs. A line
   containing `日本語 ✓ 🚀 ñ á` aligns with the line above. Colours match the
   host (true colour; Claude's theme detection gets an answer). Known vt100
   quirks to look for: an emoji with a variation selector such as `❤️` may
   swallow the cell after it, and bold plus dim renders as dim only.
6. Resize. Start at 80x24, resize to 160x50 and back: Claude re-flows, no
   leftover rows.
7. Exit. `/exit` inside Claude: `term` notices within 1 s, exits and prints
   the agent's exit code. `pgrep -f claude` shows no orphan.
8. Restore. After quitting, `stty -a` equals the output captured before
   launch; no `reset` needed; bracketed paste and keyboard flags are popped.
9. Panic. `term --spike --panic-after 5` restores the terminal and kills the
   child.
10. Renderer. Decide classic (scrolling, scrollback 5000) or
    `CLAUDE_CODE_NO_FLICKER=1` (full screen, mouse tracking on). Record why.

Results

Pass, fail, or a note number; blank is not run yet.

| Host | 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9 | 10 |
|---|---|---|---|---|---|---|---|---|---|---|
| A iTerm2 | pass (a) | note 1 | pass | pass | pass | pass | pass | pass (b) | pass (b) | classic (c) |
| B herdr | | | | | | | | | | |
| C tmux | | | | | | | | | | |

Run on 2026-10-08 with Claude Code 2.1.295 and the `spike/claude-pane`
branch at commit 60257ba. Check 2 then read "`Ctrl+Q Ctrl+Q` reaches it as a
literal"; term mode replaced that prefix afterwards.

- (a) Enter, Shift+Enter, Esc and Ctrl+C checked by hand; the agent enabled
  kitty flags 5 and modifyOtherKeys 2 after the emulator answered its
  queries, and Shift+Enter arrived as `CSI 13;2u`.
- (b) Checked under a scripted pseudo-terminal instead of by hand: raw mode
  and the alternate screen are restored, `--panic-after 3` exits with code
  101, no agent process is left behind.
- (c) The classic renderer is what the branch runs; `CLAUDE_CODE_NO_FLICKER=1`
  was not tried. Revisit when the side panes exist.

Notes

1. Ctrl+Z: the agent handles the key itself: it leaves raw mode, sends its
   own process group SIGTSTP and waits for SIGCONT. Inside a pane its group
   is orphaned, so the kernel discards the SIGTSTP; the process keeps
   running with its screen torn down until Ctrl+Q q, which succeeds after
   the two-second grace through SIGKILL. Ctrl+S did nothing, as expected.
   Planned for M2: do not forward Ctrl+Z.
