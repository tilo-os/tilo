# genawaiter-proc-macro provenance

- Package: `genawaiter-proc-macro` 0.99.1 (crates.io)
- Registry checksum: `784f84eebc366e15251c4a8c3acee82a6a6f427949776ecb88377362a9621738`
- Repository: https://github.com/whatisaphone/genawaiter
- Upstream revision: `f48046a9f66fbd2e535d5614fa7fe0d0b1a6a046` (the archive's
  `.cargo_vcs_info.json`; the "v0.99.1" commit)
- Upstream package path: `genawaiter-proc-macro`
- License: upstream declares `MIT/Apache-2.0` in its manifest and nowhere
  else: neither the published archive nor the repository at that revision
  contains a licence file. Tilo takes the MIT option (the vendored manifest
  says `MIT`). `LICENSE-MIT` is therefore **not** an upstream file: it is the
  standard MIT text with a copyright line written when the crate was vendored.
- Vendored: 2026-09-01 (`1fad393c70`)

Local changes (see `README.md`):

- the unmaintained `proc-macro-error` dependency is removed; the same compile
  failures are emitted with `syn::Error::to_compile_error`
  (`src/lib.rs`);
- the macro parses with syn 2, the version the rest of the workspace builds,
  instead of syn 1; `yield_!` is rewritten through `visit_macro_mut`
  (`src/visit.rs`);
- the manifest is rewritten by hand (`publish = false`, the dependencies
  above); `Cargo.toml.orig` and `.cargo_vcs_info.json` are omitted.

The public macro API and successful expansion output are unchanged. Remove the
copy when nothing in the workspace depends on `genawaiter` any more.
