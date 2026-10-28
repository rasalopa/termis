# Target screen

The screen `termis` is built towards, at 100 columns; project names,
numbers and the conversation are examples. 0.5.0 draws less: no hourly
sparkline, no inline diff (`d` shows a file's diff over the whole right
column), and the commit box only while it is open. The changes pane
draws no keys: with it highlighted in termis mode, the status bar lists
them, all but `d`. The bar's right end shows `termis · <project>`,
followed from two sessions on by the session's position, such as `1/3`,
and no branch or line counts.

```text
  Projects          ╭─ claude · termis   zsh ─────────────────────────────╮  Changes · main ↑0 ↓0
  ▸ ✶ termis     ●  │> add the list of modified files on the right,       │  4 files      +144 −10
      ✶ zsh         │  with their +/- line counts                         │
    ! api asking ●  │                                                     │  staged              2
    ○ web        ●  │● First I check how the layout is built.             │    M src/app.rs +31 −4
                    │                                                     │    M src/ui/lay… +9 −2
  current           │● Read(src/ui/layout.rs)                             │
  ~/src/termis      │  ⎿  Read 58 lines                                   │  unstaged            2
  Rust · ratatui …  │                                                     │  ▸ M src/ui/ch… +61 −4
                    │● Write(src/git.rs)                                  │    ? src/git.rs    +43
                    │  ⎿  Wrote 97 lines to src/git.rs                    │  today ───────────────
                    │                                                     │  3 commits    +312 −58
                    │● Update(src/ui/changes.rs)                          │  ▁▂▃▅▇█▆▃  09→17h
                    │  ⎿  Added 61 lines, removed 4 lines                 │  last a3f9c1e       2h
                    │                                                     │  feat(ui): add the ca…
                    │● Bash(cargo check)                                  │  diff changes.rs ─────
                    │  ⎿  Finished `dev` profile in 2.41s                 │  @@ -12,6 +12,9 @@
                    │                                                     │  -    files: Vec<Stri…
                    │● Done. The right pane reads git status and          │  +    staged: Vec<Ent…
                    │  numstat every 2 s. Shall I add the diff view?      │  +    unstaged: Vec<E…
                    │                                                     │  ↓ 3 more hunks
  claude ─────────  │> yes, and use the theme colours                     │  commit
  state  ✶ working  │                                                     │  ╭───────────────────╮
  model  Fable 5.1  │✻ Brewing… (esc to interrupt · 8s)                   │  │feat(ui): changes …│
  effort high       │                                                     │  ╰───────────────────╯
  ctx    ━━─── 41%  │╭───────────────────────────────────────────────────╮│  Enter commit
  5h     23% 3h12m  ││ >                                                 ││  Esc cancel
  week   41% 4d6h   │╰───────────────────────────────────────────────────╯│
  lines  +96 −12    │  ? for shortcuts           ⏵⏵ accept edits on       │  s stage u unstage
  time   1:12:08    │                                                     │  a all p push d diff
                    ╰─────────────────────────────────────────────────────╯
 claude   ⌥1-9 session · Ctrl+Q termis mode                            termis 1/3 · main · +144 −10 
```

Only the highlighted pane has a frame, rounded and teal, with its title on
it; the others keep the same one-cell margin, so moving the highlight
moves nothing, and a faint rule parts two neighbours that have no frame.
The side panes keep a column of padding inside the margin.

Most text is in the terminal's own colour. The colours mark, among other
things:

- teal: the frame and name of the highlighted pane, the frames of the
  picker, the key list and the commit box, the `TERMIS`, `COMMIT` and
  `OPEN` tags, the `▸` markers of the lists, the file tree's cursor, a
  working agent, a program running in a terminal, the context gauge under
  70%, a notice that something worked;
- amber: an agent that asks (its row in the list, its state, the count
  in the status bar, its notice), and a share running out: the context
  from 70%, a plan's limit from 80%;
- red: a failure, a conflict (`U`), the context from 90%, a limit from
  95%;
- green and red: lines added and removed, `A`, `?` and the check of
  `✓ no changes`, `D`;
- blue: `M`, `R`, `T`, `C`, and the dot of uncommitted changes;
- grey: labels, headings, the inactive panes' names, the stack, zero
  counts, a limit's time left, the last commit's hash and age, an ended
  session, the tabs not shown in the center title, the terminals and
  viewers not shown in the list, the `↳` of a repository an agent wrote
  in, its name while the changes pane does not show it, the count of
  those left out, the file tree's fold markers and the dot of a
  directory holding changes, the viewer's line numbers, the picker's
  paths but the chosen one, what the hints' keys do, the tag in agent
  mode, the `pushing` and `suggesting` chips and the line over an agent
  that ended.

## Panes

- **Projects** (left, 20%): from two sessions on, or with one whose
  agent wrote in another repository, one row per session, with its
  agent's state and a dot for uncommitted changes, and under it one row
  per terminal, with `✶` while a program runs, one for its viewer, and
  one for each of the first three repositories its agent wrote in, `↳`
  and the name with the same dot, the one Changes follows in place of
  the third when it comes later, then one counting the others; the row
  of the tab shown is marked. Under `current`, the shown project's path
  and stack; at the foot, under a heading with the agent's name, its
  Claude session. `f` puts the session's file tree in place of the list
  and the path and stack: directories first and folded, each changed
  file with its status letter.
- **claude** (center, 55%, or 80% while Changes is hidden): the agent
  pane, titled with the agent's name and the session's tabs: the
  agent's, named by the project, then the terminals and the viewer in
  the order they opened, the one shown in bold. Each tab but the
  viewer's is a real PTY, running `claude`, a terminal's shell or the
  user's editor; the viewer shows a file read-only, with line numbers.
  In agent mode the tab shown gets every key but `Ctrl+Q` and `Alt+1` to
  `Alt+9`, and `Ctrl+Z` only when it is a terminal running a shell; the
  viewer takes only its scroll keys. It keeps rendering without focus.
- **Changes** (right, 25%): the repository, or one the agent wrote in
  picked in the list, named first in the title then. The branch and its
  upstream in the title; staged and unstaged files with `+/-`, lines
  committed today, the last commit, the diff of the selected file, the
  commit box. On screen while the repository has a file not committed
  or a commit to push, while highlighted, while its commit box or a
  diff is open or a commit, a push or a subject runs, and for 10 s
  after; the rest of the time the center takes its width.
- **Status bar**: the mode, the keys that apply, toasts, and the project
  at its right end, with the session's position from two sessions on,
  after a `pushing` chip while a push runs.

## Keys

Keys go to Claude, or to the terminal shown, until `Ctrl+Q` enters termis
mode, where they are the app's commands; the viewer takes only its scroll
keys. The tag at the left of the status bar shows the mode: `claude` or
the terminal's name, `view` on the viewer, `TERMIS`, `COMMIT` while the
commit box is open, or `OPEN` while the project picker is.

| Key | Action |
|---|---|
| `Ctrl+Q` | Enter termis mode; in termis mode, back to agent mode |
| `Ctrl+Z` | Reaches a terminal in agent mode, unless it runs the editor; never Claude |
| `?` | List every key |
| `1` / `2` / `3` | Highlight Projects / claude / Changes, which shows while highlighted |
| `Tab` / `Shift+Tab` | Next / previous pane |
| `z` | Zoom the center pane |
| `o` | Open the project picker: the open sessions, Claude's recent projects, or a typed directory |
| `n` / `N` | Show the next / previous session |
| `Alt+1` … `Alt+9` | In agent and termis mode, show the first to the ninth session, in the list's order, and go back to agent mode |
| `t` | Open a terminal in the session shown, in a tab of its own |
| `f` | Show the session's file tree in the projects pane, or the list again |
| `e` | Open the file under the tree's cursor, or the one viewed, in `$VISUAL`, else `$EDITOR`, in a terminal tab of its own |
| `,` / `.` | Show the previous / next tab |
| `x` | Close the terminal or the viewer shown, else the session shown, or on Projects with the list shown the session, terminal or viewer under the list's cursor; closing the last session quits. On a repository's row, take it off the list |
| `q` | Quit |
| `Esc` | Close the key list, else the diff, else back to agent mode. In the commit box, cancel and keep the draft; in the picker, close it |
| `i` | In termis mode, back to agent mode, as `Ctrl+Q`; nothing while the key list is open |
| `j` `k` | Move in lists, the list of sessions and the file tree included, scroll the diff, the history of the tab shown or the file viewed |
| `Ctrl+D` `Ctrl+U` | Where `j` `k` move or scroll: half of what the pane shows, down / up |
| `Ctrl+F` `Ctrl+B` | Where `j` `k` move or scroll: all of what the pane shows, down / up |
| `g` `G` | Where `j` `k` move or scroll: to the top / the end; in the history of the tab shown, the oldest line kept / back to the live screen |
| `l` `h` | In the file tree: open a directory / fold it or go up |
| `Enter` | On Projects, show the session, terminal or viewer under the list's cursor and go back to agent mode, or have Changes follow the repository under it; in the file tree, open a directory or view a file |
| `PgUp` `PgDn` | In the agent pane and the diff: a page back or forward |
| `s` `u` `a` | Stage, unstage, stage all |
| `c` | Open the commit box (needs a non-empty index) |
| `p` | Push the branch at a second `p`: with `git push`, or to `origin` with `-u` when it has no upstream |
| `d` | Full diff of the selected file |
| `r` | Relaunch the agent once it ended |
| wheel | Over the agent pane, scroll the history of the tab shown or the file viewed or, when Claude or the program in the terminal shown asks for the mouse, let it scroll; on the alternate screen without the mouse, arrow keys to the tab shown in agent mode; over Changes, move the selection or scroll the diff |
| click, drag | Over the agent pane, to Claude or the program in the terminal shown when it asks for the mouse: Claude selects and copies text with them. Otherwise, and in the history or the file viewed, a drag selects text and copies it with `pbcopy` at the release; a double click takes a word, a triple click a line |

## States

1. **Agent mode** (default): keys go to Claude or the terminal shown,
   the scroll keys to the viewer, the center pane has the frame, side
   panes dim their key hints, bar shows the tag, `claude`, the
   terminal's name or `view`, `⌥1-9` from two sessions on, and `Ctrl+Q`.
2. **Termis mode on Changes**: `TERMIS` tag, the frame on the right pane
   and a faint rule between the other two, the center keeps rendering but
   hides its cursor, bar shows `j/k s u a c p d Esc`.
3. **Commit open**: a box of up to five lines under the diff, `Enter` commits what is
   staged, `Ctrl+J` adds a body line, `Ctrl+G` asks the agent for a subject, with a
   grey `suggesting` chip after the box's title while it runs, `Esc` cancels keeping
   the draft.
