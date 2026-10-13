# Vendored crates

## vt100 0.16.2

From [crates.io](https://crates.io/crates/vt100/0.16.2), upstream
[doy/vt100-rust](https://github.com/doy/vt100-rust), MIT (see
`vt100/LICENSE`). `Cargo.toml` points `vt100` here through
`[patch.crates-io]`; the reason is in
[ADR-0007](../docs/adr/0007-vendored-vt100-with-a-resize-fix.md).

Changed from the release:

- `src/row.rs`, `Row::resize`: a wide character whose second half the cut
  dropped is cleared, as `Row::truncate` does.
- `Cargo.toml` is the crate's own manifest (`Cargo.toml.orig` in the
  release) without its dev-dependencies, which only the upstream
  repository's tests use.

Everything else is the published 0.16.2 source.
