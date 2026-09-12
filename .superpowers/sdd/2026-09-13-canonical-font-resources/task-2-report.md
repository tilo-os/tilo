# Task 2 report: fallible canonical font loader

## Result

Implemented the manifest-backed `FontCatalog` loader, controlled fixture scope, product host discovery policy, canonical fallback table, typed packaging errors, and explicit `TextRenderer::with_font_catalog` construction. Existing no-argument renderer construction remains unchanged. The loader validates the embedded manifest bytes, every selected file's size and SHA-256, loaded face count and metadata, generic family assignments, and canonical family availability on every load.

## TDD evidence

### RED

Command:

```text
CARGO_TARGET_DIR=/Users/xaver/dev/qos/target/macos-shell cargo nextest run -p ui --lib --profile ci -E 'test(font_catalog::tests::)'
```

Before implementation, the new constructor regression and module tests failed to compile as expected:

```text
error[E0433]: could not find `font_catalog` in `crate`
error[E0599]: no function or associated item named `with_font_catalog`
```

### GREEN

Focused catalog and fallback suite:

```text
CARGO_TARGET_DIR=/Users/xaver/dev/qos/target/macos-shell cargo nextest run -p ui --lib --profile ci -E 'test(font_catalog)' --locked
```

Result: 6 tests passed, 2,835 skipped. The six tests include the five catalog tests and the fallback module test.

Explicit renderer constructor regression:

```text
CARGO_TARGET_DIR=/Users/xaver/dev/qos/target/macos-shell cargo nextest run -p ui --lib --profile ci -E 'test(text_render::tests::explicit_catalog_constructs_the_production_renderer_and_caches)' --locked
```

Result: 1 test passed, 2,840 skipped.

Library lint:

```text
CARGO_TARGET_DIR=/Users/xaver/dev/qos/target/macos-shell cargo clippy -p ui --lib --locked -- -D warnings
```

Result: passed with no clippy diagnostics. Formatting and `git diff --check` also pass.

## Files changed

- `ui/src/font_catalog.rs`: fallible manifest/hash/face validating loader, product and fixture scopes, and catalog provenance API.
- `ui/src/font_catalog/fallback.rs`: cosmic-text-derived Unix script fallback policy with locale-aware Han selection.
- `ui/src/font_catalog/tests.rs`: controlled face count, host alias filtering, failure ordering, script shaping, and Adwaita axis tests.
- `ui/src/text_render.rs`: private shared initializer and explicit catalog constructor plus cache regression.
- `ui/src/lib.rs`, `ui/Cargo.toml`, `Cargo.lock`: module and dependency wiring.

## Self-review and concerns

The default `TextRenderer::new()` path intentionally remains the existing ambient-font path until the later default-integration task. The test binary emits the existing macOS linker warning about `__eh_frame` size; it does not affect test results. No unrelated files or dependencies were changed.

## Commit

The implementation commit before adding this report is `3a4b94f66` (`feat(ui): load shared canonical fonts with explicit fixture policy`).
