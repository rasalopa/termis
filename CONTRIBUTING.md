# Contributing

`termis` is developed as if it were public even while it is not. These are the
rules the repository follows, so that the day it gets a remote nothing has to
be cleaned up. Today they are the maintainer's rules; the last section is for
the day someone else sends a change.

## Language

- Everything is in English: code, comments, commit messages,
  documentation and the user interface.
- The UI's titles, labels, hints and notices, and the line printed when
  the agent exits, live in `src/strings.rs` as `pub const` items and
  `pub fn`s, so the key tables of `docs/guide.md` can be checked against
  them. Error messages stay with the code that raises them, and names
  stay with the code that knows them: languages and frameworks in
  `src/projects/`, the agent's name and tools in its adapter, git's
  status letters in `src/git/`. No i18n machinery.

## Repository layout

```
src/lib.rs        pub mod for every module, so a commit can add a module
                  with tests before the UI uses it (no dead-code warnings)
src/main.rs       thin binary: parse arguments, call the library
src/pty.rs        spawn, environment, resize, reap
src/emu.rs        vt100 parser, callbacks, mode tracking, query replies
src/keys.rs       key, mouse and paste events → bytes for the child
src/input.rs      host event thread
src/app/          state, focus, main loop
src/ui/           layout, widgets, status bar
src/git/          commands, parsers, worker
src/agent/        Agent trait and adapters; src/agent/claude.rs first
src/instance.rs   the session's instance directory, watch of its files,
                  sweep of stale ones
src/telemetry.rs  statusline and hook subcommands the agent runs
src/projects/     the project card's stack, the walk of a directory
                  outside a repository, the read of a file to view
src/strings.rs    the UI's titles, labels, hints and notices, in English
tests/            integration tests and fixtures
docs/             user guide, mockup, spike checklist, ADRs
.githooks/        commit-msg and pre-commit hooks
.github/          CI: the checks below, the tests and a build with the
                  `rust-version`
vendor/           vt100 with a resize fix, see vendor/README.md
```

Tracked: `Cargo.lock` (binary crate). Ignored: `target/`, logs. Files that
other tools drop into the working tree are kept out through the local
`.git/info/exclude`, not the shared `.gitignore`.

## Commits

Every commit is atomic: one logical change that builds and passes the checks
on its own. A feature that needs three steps is three commits. A module that
nothing calls yet lands as `pub mod` in `src/lib.rs` with at least one unit
test.

Nothing lands untested: before committing, the change is read in full and
exercised by hand (run it, not only `cargo test`). What was tried goes in the
commit body when it is not obvious.

Messages follow [Conventional Commits](https://www.conventionalcommits.org):

```
<type>(<scope>)!: <subject>

<body: what changed and why, wrapped at 72 columns>

<footer: trailers>
```

- `type`: `feat`, `fix`, `docs`, `refactor`, `perf`, `test`, `build`, `ci`,
  `chore`, `revert`.
- `scope` (optional): `pty`, `emu`, `keys`, `input`, `app`, `ui`, `git`,
  `agent`, `projects`, `release`, `adr`, `deps`. One scope per module;
  `instance.rs` and `telemetry.rs` go under `agent`, and `strings.rs` under
  the module whose text changes.
- `subject`: imperative ("add", "parse", "render"), starts with a lower-case
  letter, proper nouns and acronyms keep their case (Claude, JSON, PTY), no
  trailing period, aim for 50 characters, hard limit 72. The 50/72 numbers
  are this project's rule, not part of the Conventional Commits spec.
- `body`: optional, one to three short factual sentences stating what the
  commit contains when the subject is not enough. No narrative and no
  justification; reasoning belongs in ADRs. Wrap at 72.
- `footer`: trailers only. Allowed: `BREAKING CHANGE: <what and how to
  migrate>` (or `BREAKING-CHANGE:`), `Refs: <sha or id>`, `Closes:`,
  `Fixes:`. Nothing else; the hook rejects any other trailer.
- Breaking changes: `!` after the type or scope (`feat(app)!: change the
  prefix key`) and a `BREAKING CHANGE:` footer. While 0.x, a breaking change
  bumps the minor version.
- Reverts: `git revert --no-edit <sha>`, then reword the subject to
  `revert: <original subject>` with `Refs: <sha>` in the footer.

The `commit-msg` hook enforces the subject format, the length limits and the
trailer allowlist.

## Branches

Work on `main`, which always builds and passes `cargo test`. A throwaway
`spike/<name>` branch is fine for experiments that may be discarded (the M1
spike); what survives is squashed or cherry-picked into atomic commits on
`main`. History is linear; `main` is never rewritten.

## Checks

The `pre-commit` hook runs the fast checks:

```
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
```

Enable the hooks once per clone:

```
git config core.hooksPath .githooks
```

`cargo test` runs by hand before every commit and before every tag; it is not
in the hook so that commits stay quick once tests spawn processes. CI
runs the same checks, the tests and a build with the `rust-version` on
every push and pull request to `main`; it does not check commit
messages. Its actions are pinned to commits and bumped by hand.
`SKIP_CHECKS=1` and `--no-verify` are only for `fixup!` commits on a spike
branch, never on `main`. `#[allow(...)]` is fine during a spike and is
removed before the tag. `unsafe` code is forbidden by the `[lints]` table
in `Cargo.toml`.

## Tests

- Parsers (`git status`, `numstat`, `git log --numstat`) are unit tests
  against recorded output in `tests/fixtures/`; the status line JSON
  against a sample in `src/agent/claude.rs`.
- Git operations run against a temporary repository made by `init` in
  `src/git/mod.rs`. The `git` helper there runs git with
  `GIT_CONFIG_GLOBAL=/dev/null`, `GIT_CONFIG_NOSYSTEM=1` and a fixed identity
  in `GIT_AUTHOR_*` and `GIT_COMMITTER_*`. The code under test still reads
  the developer's configuration, so `init` overrides keys of it in the
  repository's local configuration: `commit.gpgsign=false`,
  `core.hooksPath=/dev/null`, `hook.<name>.enabled=false` for each hook
  defined with `hook.<name>.command`, and the others listed there.
- The emulator is tested by replaying byte streams recorded from a real
  session and asserting modes and query replies.
- UI: `TestBackend` frames of the three panes, of the collapsed view and
  of the full-screen view; the projects and changes panes are also
  rendered on their own from a sample state.
- The embedded Claude session is verified with the manual matrix in
  `docs/spike-checklist.md`; what proves stable is automated.
  `termis --spike` runs the agent over the whole terminal with no side
  panes, and `--panic-after <SECONDS>` panics a thread after that many
  seconds, for the checks there.

## Versioning and changelog

- [SemVer](https://semver.org). The public interface is the CLI, the key
  bindings, the status line and hook subcommands and, later, the config
  file. While 0.x, changing any of them bumps the minor version.
- `v0.0.x` tags mark builds that are usable for a day of work; they need no
  changelog section.
- `CHANGELOG.md` follows [Keep a Changelog](https://keepachangelog.com) and
  is written at release time from `git log` since the last tag with a
  section, the whole history for 0.1.0 (`feat` → Added, `fix` → Fixed,
  everything else under Changed when user-visible). Feature and fix
  commits do not touch it: `docs: add changelog entry for X.Y.Z` fills
  `Unreleased` right before the release. Compare links are added in the
  commit that sets `repository` in `Cargo.toml`.
- A release is one commit, `chore(release): vX.Y.Z`: move `Unreleased` to a
  dated section, bump `Cargo.toml`, `cargo build` to refresh `Cargo.lock`,
  commit, then `git tag -a vX.Y.Z -m "termis X.Y.Z"`.

## Decisions

Architecture decisions live in `docs/adr/` as short numbered files with
Status, Context, Decision and Consequences. A changed decision gets a new
record; the old one is kept and only its Status line is edited to
`Superseded by ADR-NNNN`.

## Backlog

Without a remote there are no issues. The "After 0.1" section of
`ROADMAP.md` is the backlog; items move into a milestone when planned. When
a remote exists, the list migrates to issues.

## Before pushing to a remote

1. `git log --all -p | grep -inE "co-authored[-]by|generated[ ]with|/Users/[a-z][a-z0-9._-]{2,}|sk[-]ant-|gh[p]_|BEGIN (RSA|OPENSSH) PRIVATE" | grep -v 'grep -inE'`
   must print nothing. The brackets keep this line from matching itself,
   the last `grep` drops the earlier versions of it, and the user part of
   the path pattern skips the one-letter homes in the tests.
2. `git log --all --format='%an <%ae>' | sort -u` shows exactly the identity
   that should be public. It cannot be changed later without rewriting every
   hash (`git filter-repo --mailmap`), so settle it before the first commit.
3. `git ls-files | grep -E '\.env|local\.json'` must print nothing.
4. Push to a throwaway private repository first and watch the CI workflow go
   green.
5. Fill `repository` in `Cargo.toml` and add the changelog compare links in
   the same commit; add `CODE_OF_CONDUCT.md` (Contributor Covenant 2.1) and
   a two-line `SECURITY.md` in that commit too.
6. Require the `fmt`, `clippy`, `test` and `msrv` checks on `main` but
   allow the owner to push directly, so the linear workflow keeps working;
   revisit when a second contributor appears.
7. `git push --follow-tags`.

If step 1 or 3 finds something, fix history with `git filter-repo` on a fresh
clone before the first push; after a push, rewriting means force-pushing a
public ref and re-creating tags.

## For contributors, when the repository is public

Open an issue before a feature. One logical change per pull request, rebased
on `main`, CI green, commits in the format above. No CLA, no sign-off.
