# Fixtures

Recorded data that tests replay.

- `claude-startup.bin`: the first eight seconds of output from Claude Code
  2.1.295 started in an empty directory on a 100x30 pty with
  `TERM=xterm-256color`, the host terminal variables removed and the
  terminal title disabled. The recorder answered DA1 with `CSI ? 62 ; 22 c`,
  the kitty query with `CSI ? 0 u` and XTVERSION with `DCS > | termis 0.0.0 ST`,
  so the bytes include the modes the agent sets after those answers.
