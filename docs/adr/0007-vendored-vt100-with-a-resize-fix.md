# 0007. vt100 vendored with a resize fix

Status: accepted, 2026-10-13

## Context

vt100 0.16.2, the latest release, panics after a resize that narrows the
screen across a wide character (CJK text, an emoji such as one Claude
Code prints): `Row::resize` cuts the row and leaves the character's first
half in the new last column, and the next write or erase over that cell
indexes past the row. In `termis` a narrower window or `z` bringing the
side panes back narrows the center pane, so the agent's next redraw could
end the app. Fuzzing with random output and resizes found 322 panics in
60,000 runs on vt100 0.16.2.

## Decision

Keep vt100 (ADR-0005) and vendor 0.16.2 in `vendor/vt100`, wired in with
`[patch.crates-io]`. The only change is in `Row::resize`: a wide
character the cut split is cleared, as `Row::truncate` already does. The
vendored copy keeps the upstream license, readme and changelog; its
manifest leaves out the dev-dependencies. A test in `src/emu.rs` fails on
the published crate and passes on the vendored one.

## Consequences

- The same fuzzing finds no panic on the vendored copy.
- About 4,000 lines of third-party code live in the repository; `cargo
  fmt` and `cargo clippy` do not touch them.
- A vt100 upgrade means moving the fix to the new version, or dropping
  the vendored copy once upstream ships it. The fix is offered upstream
  when the repository is public.
