# 0012. Hunk staging handed to lazygit

Status: accepted, 2026-11-02

## Context

The changes pane stages and unstages whole files, commits and pushes,
through the `git` binary (ADR-0001). Staging part of a file needs a
cursor on the hunks and lines of a diff, a patch built from them for
`git apply --cached`, and care for what breaks such a patch: a file
without a newline at its end, a rename, a binary file, CRLF line ends.
Branches, stash and rebase are missing too.

lazygit stages hunks and lines and has the rest. It runs in a
terminal, and a terminal tab of a session (ADR-0009) already runs a
program in the shell's place: `e` runs the editor there.

Tried with lazygit 0.62.2 in a terminal tab: `lazygit -p <path>` works
in a linked work tree, but it sets `GIT_DIR=<path>/.git` for every
command it runs, the user's editor and custom commands included.
Started with no arguments in the root of the work tree, it finds the
repository as git does and sets nothing. lazygit binds `Ctrl+Z` to
suspend itself; with no shell to resume it the stop is dropped, and
the key does nothing.

## Decision

`termis` stages whole files only. `l` on the changes pane runs lazygit
in a terminal tab of the session shown, with no arguments, in the root
of the repository the pane shows: the session's own, or one its agent
wrote in. `termis` looks `lazygit` up at each `l` in the absolute
directories of the `PATH` and runs the file it finds; a relative
directory would count from inside the repository, so it is skipped.
Without one, a notice says so and no tab opens. No config key names
another program.

The tab runs a program in the shell's place, as the editor's does:
`Ctrl+Z` does not reach it, `x` and `q` ask twice while it runs, it
closes when lazygit exits, and an exit status other than 0 shows in a
notice. Each `l` opens a tab of its own, as each `e` does.

While lazygit runs, the watch on the repository's files (ADR-0010)
shows what it stages and commits in the changes pane. When a tab of
the session shown ends, as lazygit's does when it exits, the
repository its changes pane shows is read again at once.

## Consequences

- lazygit is optional: nothing else needs it, and the README does not
  list it among the requirements.
- `termis` reads the repository with `--no-optional-locks`, so its
  refreshes take no index lock that would make lazygit's writes fail.
- lazygit reads its own config file from the user's home, and gets the
  terminal environment a terminal gets; `termis` passes it nothing
  else.
- Another program, as gitui or tig, would need a config key, which is
  public interface: a key added later stops an older `termis`
  (ADR-0011).
