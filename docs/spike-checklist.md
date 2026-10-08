# Spike checklist (M1)

`term --spike` runs Claude over the whole terminal, with no side panes. Every
check must pass in the three hosts before M2 starts. Record the result in the
table at the end and in ADR-0004 / ADR-0005.

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
2. Prefix. `Ctrl+Q` never reaches Claude. `Ctrl+Q Ctrl+Q` reaches it as a
   literal. `Ctrl+Z` and `Ctrl+S` do nothing harmful.
3. Paste. Pasting three lines shows one `[Pasted text #1 +N lines]`
   placeholder. Dragging an image file onto the terminal attaches it.
4. Dialogs. `/config` borders, the permission prompt (arrows, `y`/`n`), the
   `/model` picker and the `@` file picker render and navigate correctly.
5. Rendering. The `✻ Brewing…` spinner animates without stale glyphs. A line
   containing `日本語 ✓ 🚀 ñ á` aligns with the line above. Colours match the
   host (true colour; Claude's theme detection gets an answer).
6. Resize. Start at 80x24, resize to 160x50 and back: Claude re-flows, no
   leftover rows.
7. Exit. `/exit` inside Claude: `term` notices within 1 s and shows
   `claude exited`. `pgrep -f claude` shows no orphan.
8. Restore. After quitting, `stty -a` equals the output captured before
   launch; no `reset` needed; bracketed paste and keyboard flags are popped.
9. Panic. `term --spike --panic-after 5` restores the terminal and kills the
   child.
10. Renderer. Decide classic (scrolling, scrollback 5000) or
    `CLAUDE_CODE_NO_FLICKER=1` (full screen, mouse tracking on). Record why.

Results

| Host | 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9 | 10 |
|---|---|---|---|---|---|---|---|---|---|---|
| A iTerm2 | | | | | | | | | | |
| B herdr | | | | | | | | | | |
| C tmux | | | | | | | | | | |
