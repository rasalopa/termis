# Target screen

The screen `termis` is built towards, at 100 columns; project names,
numbers and the conversation are examples. 0.2.0 draws less, as the
[README](../README.md) says: no hourly sparkline, no inline diff and no
push (`P`), and the commit box only while it is open. The changes pane
draws no keys: with it highlighted in termis mode, the status bar lists
them, all but `d`. The bar's right end shows `termis · <project>`,
followed from two sessions on by the session's position, such as `1/3`,
and no branch or line counts.

```text
┌─ Projects ─────────┬─ ● claude · termis ──────────────────────────────┬─ Changes · main ↑0 ↓0 ───┐
│▸ ✶ termis        • │> add the list of modified files on the right,    │4 files     +144 −10      │
│  ! api             │  with their +/- line counts                      │staged (2)                │
│  · web           • │                                                  │  M src/app.rs    +31 −4  │
│current             │● First I check how the layout is built.          │  M src/ui/lay…    +9 −2  │
│~/src/termis        │                                                  │unstaged (2)              │
│Rust · ratatui 0.30 │● Read(src/ui/layout.rs)                          │▸ M src/ui/cha…  +61 −4   │
│                    │  ⎿  Read 58 lines                                │  ? src/git.rs   +43      │
│                    │                                                  │─ today ──────────────────│
│                    │● Write(src/git.rs)                               │3 commits · +312 −58      │
│                    │  ⎿  Wrote 97 lines to src/git.rs                 │▁▂▃▅▇█▆▃  09→17h          │
│                    │                                                  │last a3f9c1e 2h           │
│                    │● Update(src/ui/changes.rs)                       │feat(ui): add the card    │
│                    │  ⎿  Added 61 lines, removed 4 lines              │─ diff src/ui/changes.rs ─│
│                    │                                                  │@@ -12,6 +12,9 @@         │
│                    │● Bash(cargo check)                               │ pub struct ChangesPane { │
│                    │  ⎿  Finished `dev` profile in 2.41s              │-    files: Vec<String>,  │
│                    │                                                  │+    staged: Vec<Entry>,  │
│                    │● Done. The right pane reads git status and       │+    unstaged: Vec<Entry>,│
│                    │  numstat every 2 s. Shall I add the diff view?   │↓ 3 more hunks · d diff   │
├─ claude ───────────┤                                                  │─ commit ─────────────────│
│state   ✶ working   │> yes, and use the theme colours                  │┌────────────────────────┐│
│model   Fable 5.1   │                                                  ││ feat(ui): changes pane…││
│effort  high        │✻ Brewing… (esc to interrupt · 8s)                │└────────────────────────┘│
│ctx     ▓▓▓░░░░░ 41%│                                                  │Enter commit · Esc cancel │
│5h      23% 3h12m   │╭────────────────────────────────────────────────╮│                          │
│week    41% 4d6h    ││ >                                              ││s stage u unstage a all   │
│lines   +96 −12     │╰────────────────────────────────────────────────╯│c commit P push d diff    │
│time    1:12:08     │  ? for shortcuts           ⏵⏵ accept edits on    │                          │
└────────────────────┴──────────────────────────────────────────────────┴──────────────────────────┘
 claude   Ctrl+Q termis mode                                           termis 1/3 · main · +144 −10 
```

## Panes

- **Projects** (left, 20%): from two sessions on, one row per session,
  the one shown marked, with its agent's state and a dot for uncommitted
  changes; under `current`, the shown project's path and stack; at the
  foot, in a box of its own, its Claude session.
- **claude** (center, 55%): the agent pane, titled with the agent's name; a
  real PTY running `claude`. In agent mode every key but `Ctrl+Q` and
  `Ctrl+Z` reaches the agent. It keeps rendering without focus.
- **Changes** (right, 25%): the repository. The branch and its upstream
  in the title; staged and unstaged files with `+/-`, lines committed
  today, the last commit, the diff of the selected file, the commit box.
- **Status bar**: the mode, the keys that apply, toasts, and the project
  at its right end, with the session's position from two sessions on.

## Keys

Keys go to Claude until `Ctrl+Q` enters termis mode, where they are the app's
commands. The tag at the left of the status bar shows the mode: `claude`,
`TERMIS`, `COMMIT` while the commit box is open, or `OPEN` while the
project picker is.

| Key | Action |
|---|---|
| `Ctrl+Q` | Enter termis mode; in termis mode, back to Claude |
| `Ctrl+Z` | Never reaches Claude, in any mode |
| `?` | List every key |
| `1` / `2` / `3` | Highlight Projects / claude / Changes |
| `Tab` / `Shift+Tab` | Next / previous pane |
| `z` | Zoom the center pane |
| `o` | Open the project picker: the open sessions, Claude's recent projects, or a typed directory |
| `n` / `N` | Show the next / previous session |
| `x` | Close the session shown, or on Projects the one under the list's cursor; closing the last one quits |
| `q` | Quit |
| `Esc` | Close the key list, else the diff, else back to Claude. In the commit box, cancel and keep the draft; in the picker, close it |
| `j` `k` | Move in lists, the list of sessions included, scroll the diff or the agent's history |
| `Enter` | On Projects, show the session under the list's cursor and go back to Claude |
| `PgUp` `PgDn` `G` | In the agent pane: a page back or forward, back to the end |
| `s` `u` `a` | Stage, unstage, stage all |
| `c` | Open the commit box (needs a non-empty index) |
| `d` | Full diff of the selected file |
| `r` | Relaunch the agent once it ended |
| wheel | Over the agent pane, scroll its history, or let Claude scroll when it asks for the mouse; on its alternate screen without the mouse, arrow keys to Claude in agent mode; over Changes, move the selection or scroll the diff |
| click, drag | Over the agent pane, to Claude when it asks for the mouse: select and copy text |

## States

1. **Agent mode** (default): keys go to Claude, center border highlighted,
   side panes dim their key hints, bar shows the `claude` tag and `Ctrl+Q`.
2. **Termis mode on Changes**: `TERMIS` tag, right border highlighted, the center
   keeps rendering but hides its cursor, bar shows `j/k s u a c P d Esc`.
3. **Commit open**: a box of up to five lines under the diff, `Enter` commits what is
   staged, `Ctrl+J` adds a body line, `Esc` cancels keeping the draft.
