# Target screen

The screen `term` is built towards, at 100 columns. Labels are the real
ones; project names, numbers and the conversation are examples.

```text
┌─ Projects ─────────┬─ ● claude · term ────────────────────────────────┬─ Changes · main ─────────┐
│▸ term       main ● │> add the list of modified files on the right,    │4 files     +144 −10      │
│  pico       main   │  with their +/- line counts                      │staged (2)                │
│  comunidad  dev  ● │                                                  │  M src/app.rs    +31 −4  │
│  qodexia    main   │● First I check how the layout is built.          │  M src/ui/lay…    +9 −2  │
│─ current ──────────│                                                  │unstaged (2)              │
│~/…/Personal/term   │● Read(src/ui/layout.rs)                          │▸ M src/ui/cha…  +61 −4   │
│Rust · ratatui 0.30 │  ⎿  Read 58 lines                                │  ? src/git.rs   +43      │
│branch  main        │                                                  │─ today ──────────────────│
│origin  ↑0 ↓0       │● Write(src/git.rs)                               │3 commits · +312 −58      │
│today   +212 −38    │  ⎿  Wrote 97 lines to src/git.rs                 │▁▂▃▅▇█▆▃  09→17h          │
│last    a3f9c1e 2h  │                                                  │─ diff src/ui/changes.rs ─│
│─ claude ───────────│● Update(src/ui/changes.rs)                       │@@ -12,6 +12,9 @@         │
│state   ✶ working   │  ⎿  Added 61 lines, removed 4 lines              │ pub struct ChangesPane { │
│model   Fable 5.1   │                                                  │-    files: Vec<String>,  │
│ctx     ▓▓▓▓░░░░ 41%│● Bash(cargo check)                               │+    staged: Vec<Entry>,  │
│cost    $1.84       │  ⎿  Finished `dev` profile in 2.41s              │+    unstaged: Vec<Entry>,│
│time    00:42       │                                                  │↓ 3 more hunks · d diff   │
│                    │● Done. The right pane reads git status and       │─ commit ─────────────────│
│                    │  numstat every 2 s. Shall I add the diff view?   │┌────────────────────────┐│
│                    │                                                  ││ feat(ui): changes pane…││
│                    │> yes, and use the theme colours                  │└────────────────────────┘│
│                    │                                                  │Enter commit · Esc cancel │
│                    │✻ Brewing… (esc to interrupt · 8s)                │                          │
│                    │                                                  │s stage u unstage a all   │
│                    │╭────────────────────────────────────────────────╮│c commit P push d diff    │
│                    ││ >                                              ││                          │
│                    │╰────────────────────────────────────────────────╯│                          │
│                    │  ? for shortcuts           ⏵⏵ accept edits on    │                          │
└────────────────────┴──────────────────────────────────────────────────┴──────────────────────────┘
 focus: Claude   Ctrl+Q then 1/2/3 pane · c commit · z zoom · q quit         term · main · +144 −10 
```

## Panes

- **Projects** (left, 20%): repositories where `claude` has been used, the
  current one marked; a card for the current repository; the Claude session.
- **claude** (center, 55%): a real PTY running `claude`. While focused, every
  keystroke is forwarded as raw bytes. It keeps rendering without focus.
- **Changes** (right, 25%): staged and unstaged files with `+/-`, lines
  committed today, the diff of the selected file, the commit box.
- **Status bar**: which pane has focus, the keys that apply, toasts.

## Keys

| Key | Action |
|---|---|
| `Ctrl+Q` | Prefix. The only key the app keeps; everything else goes to Claude. `Ctrl+Q Ctrl+Q` sends a literal `Ctrl+Q`. |
| `Ctrl+Q 1` / `2` / `3` | Focus Projects / claude / Changes |
| `Ctrl+Q c` | Jump to the commit box |
| `Ctrl+Q z` | Zoom the center pane |
| `Ctrl+Q q` | Quit (asks if `claude` is still running) |
| `Esc` | From a side pane, back to Claude. In the commit box, cancel and keep the draft |
| `j` `k` | Move in lists, scroll the diff |
| `s` `u` `a` | Stage, unstage, stage all |
| `c` | Open the commit box (needs a non-empty index) |
| `d` | Full diff of the selected file |
| `r` | Refresh |

## States

1. **Focus on Claude** (default): center border highlighted, side panes dim
   their key hints, bar shows the prefix shortcuts.
2. **Focus on Changes**: right border highlighted, the center keeps rendering
   but hides its cursor, bar shows `j/k s u a c P d Esc`.
3. **Commit open**: a one-line box under the diff, `Enter` commits what is
   staged, `Ctrl+J` adds a body line, `Esc` cancels keeping the draft.
