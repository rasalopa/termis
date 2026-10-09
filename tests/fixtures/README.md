# Fixtures

Recorded data that tests replay.

- `claude-startup.bin`: the first eight seconds of output from Claude Code
  2.1.295 started in an empty directory on a 100x30 pty with
  `TERM=xterm-256color`, the host terminal variables removed and the
  terminal title disabled. The recorder answered DA1 with `CSI ? 62 ; 22 c`,
  the kitty query with `CSI ? 0 u` and XTVERSION with `DCS > | termis 0.0.0 ST`,
  so the bytes include the modes the agent sets after those answers.
- `git/*.bin`: output of git 2.54 in a throwaway repository with one commit
  and then a staged binary, a deleted file, a file changed in the index and
  the work tree, a staged rename edited again, and two untracked files, one
  with a space; `status.bin` is `status --porcelain=v2 --branch -z
  --untracked-files=all`, `numstat-index.bin` and `numstat-worktree.bin`
  are `diff --numstat -z --no-relative` with and without `--cached`, and
  `no-index.bin` is `diff --no-index --numstat -z -- /dev/null ./newdir/f.txt`.
  The `*-conflict.bin` pair comes from a second repository in the middle of
  a merge that conflicts on `both.txt`: the status, and the numstat without
  `--cached`, which lists the path twice. The unit tests in `src/git` read
  these files.
