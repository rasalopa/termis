# termis

A terminal app for macOS that runs Claude Code in the middle of three
panes, with your projects and the agent's state on the left and the
repository's git changes on the right.

```text
  Projects          ╭─ claude · api ───────── ◐ Orders cursor pagination ─╮  Changes · main
  ▸ ✶ api        ●  │> add cursor pagination to GET /orders               │  3 files        +27 −2
    ! web asking ●  │                                                     │
    ○ docs          │● I will page /orders by cursor and keep the old     │  staged              1
                    │  page parameter working.                            │  ▸ M Cargo.toml  +1 −1
  current           │                                                     │
  ~/src/api         │● Read(src/routes.rs)                                │  unstaged            2
  Rust              │  ⎿  Read 20 lines                                   │    M src/routes… +2 −1
                    │                                                     │    ? src/paginati… +24
                    │● Update(Cargo.toml)                                 │
                    │  ⎿  Updated Cargo.toml with 1 addition and 1        │
                    │     removal                                         │
                    │                                                     │
                    │● Write(src/pagination.rs)                           │
                    │  ⎿  Wrote 24 lines to src/pagination.rs             │
                    │                                                     │
                    │● Update(src/routes.rs)                              │
                    │  ⎿  Updated src/routes.rs with 2 additions and 1    │
                    │     removal                                         │
  claude ─────────  │                                                     │
  state  ✶ Bash     │● Bash(cargo test)                                   │
  model  Opus 5.5   │  ⎿  Running…                                        │
  effort high       │                                                     │
  ctx    ━━─── 34%  │✶ Testing… (esc to interrupt · 41s)                  │
  5h     42% 1h30m  │                                                     │  today ───────────────
  week   18% 3d12h  │                                                     │  2 commits      +24 −0
  lines  +128 −31   │                                                     │  last 85197cc      now
  time   12:47      │                                                     │  Add orders route
                    ╰─────────────────────────────────────────────────────╯
 claude   ⌥1-9 session · Ctrl+Q termis mode                                        termis · api 1/3
```

Three projects are open: `api` is shown, and the agent of `web` is
asking for a permission. The conversation is a sample.

## What it does

- Runs several projects in one window, each in a session with its own
  Claude Code. The left pane lists them with their agent's state, and
  `n` and `N` switch between them.
- Opens your shell, a file viewer and your editor in tabs beside a
  session's Claude Code, and the project's file tree in the left pane.
- Stages, commits and pushes from the right pane through `git`, so your
  hooks run. In the commit box, `Ctrl+G` asks Claude for a subject. The
  pane hides while the repository has nothing to commit or push.
- Shows at the foot of the left pane whether the agent is working,
  waiting or asking, its model, the context in use, and how much of the
  plan's five-hour and weekly limits is used.
- Lists under a session the other repositories its agent writes in;
  `Enter` on one shows its changes on the right.
- Asks the terminal for a desktop notification when an agent asks or
  finishes its turn while the `termis` window does not have the focus.
- Reopens the sessions of the last run at the next start.

## Requirements

- macOS. Linux and Windows are not supported.
- Rust 1.88 or newer, to build it.
- `git` on the `PATH`. `termis` runs it for every git operation and does
  not start without it.
- [Claude Code](https://claude.com/claude-code) as `claude` on the
  `PATH`. Without it `termis` stops with an error.
- A terminal at least 100 columns wide for the side panes. Tested in
  iTerm2; other terminals and tmux are untested.

## Install

```sh
cargo install --locked --git https://github.com/rasalopa/termis
termis --version
```

From a clone of the repository, `cargo install --locked --path .` does
the same.

Cargo installs `termis` in its `bin` directory, `~/.cargo/bin` by
default, which must be on the `PATH`.

## Quick start

```sh
termis ~/src/api
```

`termis [path]` starts Claude Code in `path`, the current directory by
default, and continues the last conversation it saved there; `--new`
starts a new one. Keys go to Claude, in agent mode, until `Ctrl+Q`
or a click on a side pane switches to termis mode, where they are
`termis`'s commands. `Ctrl+Q`, `Esc`, `i` or a click on the center
pane goes back. In termis mode, `?` lists the keys by mode and `q`
quits.

## Keys

The keys used most, in termis mode:

| Key | Action |
|---|---|
| `Ctrl+Q` | Switch between agent mode and termis mode; the config file can set another key |
| `?` | List the keys by mode |
| `1` `2` `3` | Highlight the left, center or right pane |
| `j` `k` | Move in a list, or scroll the history, a diff or a file |
| `s` `u` `a` `d` | On the right pane: stage or unstage the selected file, stage all, show the file's diff |
| `c` | On the right pane: open the commit box, where `Enter` commits, `Ctrl+G` asks Claude for a subject and `Esc` closes it |
| `p` | On the right pane: push the branch, at a second `p` |
| `o` | Open another project in a session of its own |
| `n` `N` | Show the next or the previous session |
| `Alt+1` … `Alt+9` | From agent or termis mode, show the first to the ninth session, in the list's order, and go to agent mode; in iTerm2, the left Option key must be set to `Esc+` |
| `t` | Open your shell in a new tab |
| `f` | Show the file tree in the left pane, or the sessions again; `Enter` on a file shows it in a viewer tab |
| `e` | Open the file under the tree's cursor, or the one viewed, in your editor |
| `,` `.` | Show the previous or the next tab |
| `x` | Close the terminal or the viewer shown, else the session |
| `q` | Quit |

## More

- [docs/guide.md](docs/guide.md): what each pane shows, every key and
  mouse action, sessions, terminals and files, the config file, and the
  known limits.
- [CHANGELOG.md](CHANGELOG.md): what each release changed.
- [ROADMAP.md](ROADMAP.md): what comes next.
- [CONTRIBUTING.md](CONTRIBUTING.md): how to build and test, the commit
  format and the release steps.
- [docs/mockup.md](docs/mockup.md): the screen `termis` is built
  towards, and its colours.

## License

[MIT](LICENSE)
