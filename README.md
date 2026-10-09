# termis

A three-pane terminal workspace for working with a coding agent on macOS:
the current project on the left, a live agent session in the middle, and
the git changes with a commit box on the right. Claude Code is the first
supported agent.

```text
┌─ Projects ─────────┬─ ● claude · termis ──────────────────────────────┬─ Changes · main ─────────┐
│▸ termis     main ● │> add the list of modified files on the right,    │4 files     +144 −10      │
│  pico       main   │  with their +/- line counts                      │staged (2)                │
│  comunidad  dev  ● │                                                  │  M src/app.rs    +31 −4  │
│  qodexia    main   │● First I check how the layout is built.          │  M src/ui/lay…    +9 −2  │
│─ current ──────────│                                                  │unstaged (2)              │
│~/…/Personal/termis │● Read(src/ui/layout.rs)                          │▸ M src/ui/cha…  +61 −4   │
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
 claude   Ctrl+Q termis mode                                               termis · main · +144 −10 
```

The screen above is the target (see [docs/mockup.md](docs/mockup.md)); the
project list on the left and the inline diff come after 0.1.
[ROADMAP.md](ROADMAP.md) says where the project is.

## Status

Pre-alpha, usable for a day of work since `v0.0.1`: Claude Code runs in the
center pane, the right pane lists the staged and unstaged files with their
line counts, and `s`, `u`, `a` and `c` stage, unstage and commit from it.
The left pane and the diff view are still to come; the roadmap says in what
order.

## Install

Requires a stable Rust toolchain (1.88 or newer) and the agent on the
`PATH`: [Claude Code](https://claude.com/claude-code) in 0.1.

```sh
cargo install --locked --path .
```

## Use

Run `termis` inside a git repository; `termis -- -c` resumes the last Claude
session. The agent starts in the center pane with the repository as its
working directory. `termis` runs in a plain terminal such as iTerm2 and
provides the git side; tmux provides persistence when it is needed.

Keys go to the agent until `Ctrl+Q` enters termis mode, where they are
`termis`'s commands. The tag at the left of the status bar shows the mode:
the agent's name, or `TERMIS`.

| Key | Action |
|---|---|
| `Ctrl+Q` | Enter termis mode; in termis mode, back to the agent |
| `?` | List every key; `?` or `Esc` closes the list |
| `1` / `2` / `3` | Highlight Projects / agent / Changes |
| `Tab` / `Shift+Tab` | Next / previous pane |
| `c` | Open the commit box; needs something staged |
| `Enter` / `Ctrl+J` / `Backspace` | In the commit box: commit, body line, delete |
| `z` | Zoom the center pane |
| `q` | Quit |
| `Esc` | Back to the agent; in the commit box, back to the pane keeping the draft |
| `r` / `q` | After the agent ended, in either mode: relaunch / quit |
| `j` `k` `s` `u` `a` `d` `r` | In Changes: move the selection, stage or unstage the selected row, stage all, diff, refresh |

## Develop

```sh
git config core.hooksPath .githooks   # once per clone
cargo test
cargo run -- --version
```

Conventions, commit format and release steps are in
[CONTRIBUTING.md](CONTRIBUTING.md). Design decisions are in
[docs/adr](docs/adr/README.md).

## License

[MIT](LICENSE)
