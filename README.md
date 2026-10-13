# termis

A three-pane terminal workspace for working with a coding agent on macOS:
the current project on the left, a live agent session in the middle, and
the git changes with a commit box on the right. Claude Code is the first
supported agent.

```text
┌─ Projects ─────────┬─ ● claude · termis ──────────────────────────────┬─ Changes · main ↑0 ↓0 ───┐
│▸ termis     main ● │> add the list of modified files on the right,    │4 files     +144 −10      │
│  pico       main   │  with their +/- line counts                      │staged (2)                │
│  comunidad  dev  ● │                                                  │  M src/app.rs    +31 −4  │
│  qodexia    main   │● First I check how the layout is built.          │  M src/ui/lay…    +9 −2  │
│─ current ──────────│                                                  │unstaged (2)              │
│~/…/Personal/termis │● Read(src/ui/layout.rs)                          │▸ M src/ui/cha…  +61 −4   │
│Rust · ratatui 0.30 │  ⎿  Read 58 lines                                │  ? src/git.rs   +43      │
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
│ctx     ▓▓▓▓░░░░ 41%│                                                  │Enter commit · Esc cancel │
│5h      23% 3h12m   │╭────────────────────────────────────────────────╮│                          │
│week    41% 4d6h    ││ >                                              ││s stage u unstage a all   │
│lines   +96 −12     │╰────────────────────────────────────────────────╯│c commit P push d diff    │
│time    1:12:08     │  ? for shortcuts           ⏵⏵ accept edits on    │                          │
└────────────────────┴──────────────────────────────────────────────────┴──────────────────────────┘
 claude   Ctrl+Q termis mode                                               termis · main · +144 −10 
```

The screen above is the target (see [docs/mockup.md](docs/mockup.md)); the
project list on the left and the inline diff come after 0.1.
[ROADMAP.md](ROADMAP.md) says where the project is.

## Status

Pre-alpha, usable for a day of work since `v0.0.1`: Claude Code runs in
the center pane. The right pane holds git: the branch and how it stands
against its upstream in the title, the staged and unstaged files with
their line counts, the day's commits and lines and the last commit; `s`,
`u`, `a` and `c` stage, unstage and commit from it, and `d` shows the diff
of the selected file. The left pane shows the project's path and stack; at
the foot of the pane, in a box of its own, the agent: whether it is
working, and with which tool, waiting for a prompt or asking you
something, its model and effort, the context in use, how much of the
plan's five-hour and weekly limits is used and when each resets (the cost
instead, without a plan), the lines it changed and the session time.

## Install

Requires a stable Rust toolchain (1.88 or newer) and the agent on the
`PATH`: [Claude Code](https://claude.com/claude-code) in 0.1.

```sh
cargo install --locked --path .
```

## Use

Run `termis` inside a git repository; `termis -- -c` resumes the last Claude
session. The agent starts in the center pane with the repository as its
working directory. Claude gets a status line and hooks for the session
from `termis` through `--settings`, merged with a `--settings` of your
own: the status line you had keeps running after `termis`'s own, and no
settings file changes. `termis` runs in a plain terminal such as iTerm2
and
provides the git side; tmux provides persistence when it is needed. The
center pane keeps up to 5000 lines that scrolled off the agent's screen,
read in termis mode or with the mouse wheel. An agent that asks for the
mouse gets the clicks, drags and motion it asks for over the center pane:
Claude's fullscreen view selects text with them and copies it. `termis`
always reports the mouse, so the terminal's own selection takes Option
while dragging in iTerm2, unless its profile reports only wheel events;
kitty, WezTerm and Alacritty take Shift.

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
| `j` `k` `s` `u` `a` `d` | In Changes: move the selection, stage or unstage the selected row, stage all, diff of the selected row (`j`/`k` scroll it, `d` or `Esc` go back) |
| `j` `k` `PgUp` `PgDn` `G` | In the agent pane: scroll its history a line or a page, back to the end |
| wheel | Over the agent pane, scroll its history; an agent that asks for the mouse, such as Claude's fullscreen view, scrolls itself, and one on its alternate screen without it gets arrow keys; over Changes, move the selection or scroll the diff |
| click, drag | Over the agent pane, to an agent that asks for the mouse; Claude's fullscreen view selects and copies text |

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
