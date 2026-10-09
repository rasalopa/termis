# Contributing

`termis` is developed as if it were public even while it is not. These are the
rules the repository follows, so that the day it gets a remote nothing has to
be cleaned up. Today they are the maintainer's rules; the last section is for
the day someone else sends a change.

## Language

- Everything is in English: code, comments, commit messages,
  documentation and the user interface.
- User-facing strings live as `pub const` items in `src/strings.rs`, so
  the README key table can be checked against them. No i18n machinery.

## Repository layout

```
src/lib.rs        pub mod for every module, so a commit can add a module
                  with tests before the UI uses it (no dead-code warnings)
src/main.rs       thin binary: parse arguments, call the library
src/pty           spawn, environment, resize, reap
src/emu           vt100 parser, callbacks, mode tracking, query replies
src/keys          key event → bytes for the child
src/input         host event thread
src/app           state, focus, main loop
src/ui            layout, widgets, status bar
src/git           commands, parsers, worker
src/agent         Agent trait and adapters; src/agent/claude.rs first
src/projects      current project card data
src/strings.rs    user-facing strings (one place, English)
tests/            integration tests and fixtures
docs/             mockup, spike checklist, ADRs
.githooks/        commit-msg and pre-commit hooks
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
  `agent`, `projects`, `release`, `adr`, `deps`. One scope per module.
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
in the hook so that commits stay quick once tests spawn processes.
`SKIP_CHECKS=1` and `--no-verify` are only for `fixup!` commits on a spike
branch, never on `main`. `#[allow(...)]` is fine during a spike and is
removed before the tag.

## Tests

- Parsers (`git status`, `numstat`, status line JSON) are unit tests against
  recorded output in `tests/fixtures/`.
- Git operations run against a temporary repository built by a test helper
  that isolates git from the developer's configuration:
  `GIT_CONFIG_GLOBAL=/dev/null`, `GIT_CONFIG_NOSYSTEM=1`, a fixed identity in
  `GIT_AUTHOR_*` and `GIT_COMMITTER_*`, `-c commit.gpgsign=false`,
  `-c core.hooksPath=/dev/null`.
- The emulator is tested by replaying byte streams recorded from a real
  session and asserting modes and query replies.
- UI: `TestBackend::new(100, 20)` snapshots of the empty three-pane frame
  and of the collapsed frame; the changes pane rendered from a fixture
  state.
- The embedded Claude session is verified with the manual matrix in
  `docs/spike-checklist.md`; what proves stable is automated.

## Versioning and changelog

- [SemVer](https://semver.org). The public interface is the CLI, the key
  bindings, the status line and hook subcommands and, later, the config
  file. While 0.x, changing any of them bumps the minor version.
- `v0.0.x` tags mark builds that are usable for a day of work; they need no
  changelog section.
- `CHANGELOG.md` follows [Keep a Changelog](https://keepachangelog.com) and
  is written at release time from `git log <last tag>..` (`feat` → Added,
  `fix` → Fixed, everything else under Changed when user-visible). Commits do
  not touch it. Compare links are added in the commit that sets `repository`
  in `Cargo.toml`.
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

1. `git log --all -p | grep -inE "co-authored[-]by|generated[ ]with|/Users/[a-z]|sk-ant-|ghp_|BEGIN (RSA|OPENSSH) PRIVATE"`
   must print nothing (the brackets keep this line from matching itself).
2. `git log --all --format='%an <%ae>' | sort -u` shows exactly the identity
   that should be public. It cannot be changed later without rewriting every
   hash (`git filter-repo --mailmap`), so settle it before the first commit.
3. `git ls-files | grep -E '\.env|local\.json'` must print nothing.
4. Push to a throwaway private repository first and watch the CI workflow go
   green.
5. Fill `repository` in `Cargo.toml` and add the changelog compare links in
   the same commit; add `CODE_OF_CONDUCT.md` (Contributor Covenant 2.1) and
   a two-line `SECURITY.md` in that commit too.
6. Require the CI check on `main` but allow the owner to push directly, so
   the linear workflow keeps working; revisit when a second contributor
   appears.
7. `git push --follow-tags`.

If step 1 or 3 finds something, fix history with `git filter-repo` on a fresh
clone before the first push; after a push, rewriting means force-pushing a
public ref and re-creating tags.

## For contributors, when the repository is public

Open an issue before a feature. One logical change per pull request, rebased
on `main`, CI green, commits in the format above. No CLA, no sign-off.
