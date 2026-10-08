# term

A three-pane terminal workspace for working with Claude Code on macOS:
the current project on the left, a live `claude` session in the middle, and
the git changes with a commit box on the right.

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

The screen above is the target (see [docs/mockup.md](docs/mockup.md)); the
project list on the left and the inline diff come after 0.1.
[ROADMAP.md](ROADMAP.md) says where the project is.

## Status

Pre-alpha. Nothing runs yet; the roadmap is being executed milestone by
milestone. The first daily-usable build is tagged `v0.0.1`.

## Install

Requires a stable Rust toolchain (1.85 or newer) and
[Claude Code](https://claude.com/claude-code) on the `PATH`.

```sh
cargo install --locked --path .
```

## Use

Run `term` inside a git repository; `term -- -c` resumes the last Claude
session. Claude starts in the center pane with the repository as its working
directory. `term` is meant to run inside herdr or tmux, which provide
persistence; `term` provides the git side.

| Key | Action |
|---|---|
| `Ctrl+Q` | Prefix. The only key `term` keeps; everything else goes to Claude. `Ctrl+Q Ctrl+Q` sends a literal |
| `Ctrl+Q 1` / `2` / `3` | Focus Projects / claude / Changes |
| `Ctrl+Q c` | Jump to the commit box |
| `Ctrl+Q z` | Zoom the center pane |
| `Ctrl+Q q` | Quit |
| `Esc` | Back to Claude from a side pane |
| `j` `k` `s` `u` `a` `c` `d` `r` | In Changes: move, stage, unstage, stage all, commit, diff, refresh |

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
