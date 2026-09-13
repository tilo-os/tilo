# Controlled Typography Fixtures and Rendering Evidence Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this task with its review gate.

**Goal:** Make font-sensitive CPU tests use a fixed checked-in catalog and capture honest Metal/Vulkan comparisons against unchanged historical typography goldens.

**Architecture:** Tests and the gallery shot generator explicitly inject the controlled catalog through the production renderer. The default product policy is unchanged. Typography qualification requires a real adapter and emits optional image/environment receipts; its comparator and expected PNGs remain unchanged.

**Tech stack:** Rust, existing ui, cosmic-text 0.19, patched Vello 0.9, nextest, canonical font catalog.

**Spec:** docs/superpowers/specs/2026-09-12-macos-shell-port-design.md §§9/11. Prerequisites: completed docs/superpowers/plans/2026-09-13-canonical-font-resources.md and both tasks in docs/superpowers/plans/2026-09-13-typography-renderer.md. The latter must pass review before this task runs.

## Global Constraints

- “Run both hosts from the same git commit and lockfile, with separate native build directories.”
- “Keep Mac rendering expectations explicit about Metal, fonts, scale and pixel tolerance instead of weakening Linux goldens to make them pass on another backend.”
- “Benchmarks and tests must not load the user's real state or secrets.”
- Every runtime command uses .superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py. The evidence variable is supplied by `env` INSIDE the isolated subprocess because the helper deliberately removes inherited QOS_ values. Preserve all other isolation.
- Worktree /Users/xaver/dev/qos-macos-shell; Mac CARGO_TARGET_DIR=/Users/xaver/dev/qos/target/macos-shell. Controller alone syncs and runs Fedora at the implementation commit using /home/parallels/qos-target and the observed llvmpipe ARM64 ICD /usr/share/vulkan/icd.d/lvp_icd.aarch64.json.
- Do not alter a metric threshold, expected geometry, font payload, product default, app UI, renderer implementation, golden PNG or comparison tolerance in this task. A failing CPU assertion with the fixed catalog is a defect to diagnose, not permission to weaken it.
- No unrecorded GPU skip: this required typography gate must fail if the adapter is unavailable. GPU runs are serialized after other GPU work drains.
- Native implementer runs CPU checks and commits; the controller then owns Fedora CPU checks, idle Fedora golden comparison first and native Metal comparison second at the same SHA. Task acceptance requires evidence and review, while historical mismatch may remain explicitly open in the parent gate.

## File responsibilities

The task below defines test-only helpers and fixture migrations, then adds evidence to the existing typography integration test. The shot generator deliberately uses that same controlled font catalog for reproducible gallery artifacts. Production default font discovery and installed resources have their own subsequent contract.

### Task 1: Isolate metric fixtures and capture the unchanged golden comparison

**Files:**
- Modify: `ui/src/font_catalog.rs` (test helpers only), `ui/src/text_render.rs` test module, `ui/src/richtext.rs` test module, `ui/src/math/layout.rs` test module, `ui/src/widget/text_input.rs` named-family test.
- Modify: `ui/tests/typography_visual_regression.rs`, `ui/examples/gallery_shots.rs`.
- Leave unchanged: every file in `ui/tests/fixtures/typography/` and all ebook/shell baseline PNGs.

**Interfaces:**
- Unit-only helpers: `fixture_catalog() -> FontCatalog`, `fixture_font_system() -> FontSystem`, `fixture_renderer() -> TextRenderer` under `#[cfg(test)]`, `pub(crate)` visibility.
- Integration/example construction uses the public catalog API explicitly; no `cfg(test)` changes to production default policy.
- Optional evidence directory `QOS_TYPOGRAPHY_EVIDENCE_DIR` affects only artifact output, never font selection.

- [ ] Add these unit helpers to `font_catalog.rs`:

```rust
#[cfg(test)]
pub(crate) fn fixture_catalog() -> FontCatalog {
    FontCatalog::controlled_fixture(Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/fonts"), "en-US")
}
#[cfg(test)]
pub(crate) fn fixture_font_system() -> FontSystem {
    fixture_catalog().load().expect("checked-in canonical font fixture")
}
#[cfg(test)]
pub(crate) fn fixture_renderer() -> crate::text_render::TextRenderer {
    crate::text_render::TextRenderer::with_font_catalog(&fixture_catalog()).expect("checked-in canonical renderer fixture")
}
```

- [ ] Replace `TextRenderer::new()` inside `text_render::tests` and `math::layout::layout_tests` with `crate::font_catalog::fixture_renderer()`. Replace `configured_font_system()` in the generic-family unit test with `crate::font_catalog::fixture_font_system()`. Retain all existing Thai line/height/byte/selection/hit assertions, math cell geometry (`> 22 * 3`), variable-axis assertions, and cache assertions. Change Adwaita's failure message to `"the controlled fixture ships Adwaita Sans"`.
- [ ] In the entire `richtext::tests` module replace **both** `crate::configured_font_system()` and `FontSystem::new()` with `crate::font_catalog::fixture_font_system()`. The `layout()` helper computes the widths used by the attributed Thai test, so changing only the final paragraph font system would leave that test internally inconsistent. Do not modify non-test shaping implementation or any expected geometry.
- [ ] Change the first four lines of `font_family_override_shapes_in_named_family` to this exact fixture choice, leaving the drawn-input and every face-ID assertion unchanged:

```rust
let mut tr = crate::font_catalog::fixture_renderer();
let fam = crate::text_render::UI_SERIF_FAMILY.to_owned();
assert!(tr.system_families().contains(&fam));
```

- [ ] Use this public construction block at the old `TextRenderer::new()` site in both the typography integration test and `gallery_shots::shoot`:

```rust
let catalog = ui::font_catalog::FontCatalog::controlled_fixture(
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/fonts"), "en-US");
let mut text_renderer = TextRenderer::with_font_catalog(&catalog)
    .expect("checked-in typography font catalog");
```

In `gallery_shots.rs` the existing variable is `tr`; use `let mut tr = ...` there. No other statement changes in that function.

- [ ] Run the exact focused selection (Mac first, then controller runs the same on Fedora at the reviewed commit):

```sh
CARGO_TARGET_DIR=/Users/xaver/dev/qos/target/macos-shell python3 .superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py cargo nextest run -p ui --lib --locked --profile ci -E 'test(font_catalog::) or test(text_render::tests::) or test(richtext::tests::) or test(math::layout::layout_tests::) or test(font_family_override_shapes_in_named_family)'
```

Require a pass without altered thresholds. The renderer prerequisite has already moved the Adwaita axis-order test to a controlled catalog; reuse the helper there without undoing its new shaped-weight argument or assertions. Run all-target clippy through the isolated helper with `CARGO_TARGET_DIR=/Users/xaver/dev/qos/target/macos-shell python3 .superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py cargo clippy -p ui --all-targets --locked -- -D warnings`, and compile the changed gallery shot example with `CARGO_TARGET_DIR=/Users/xaver/dev/qos/target/macos-shell cargo check -p ui --example ui-shots --locked`. If the pinned variable fonts expose a genuine normalization/shaping issue, stop that assertion change and report its exact face ID/axis/measurement evidence; do not translate that into a baseline update.

- [ ] Make typography GPU absence a failed prerequisite for this mandatory golden gate. Change `fn render(case: Case) -> Option<Frame>` to `-> Frame`, replace its `OffscreenRenderer::try_new` match with the block below, return `Frame { ... }` rather than `Some(Frame { ... })`, and replace the loop's `let Some(actual) ... else { return; }` with `let actual = render(case);`:

```rust
let mut renderer = OffscreenRenderer::try_new(width, height)
    .expect("typography golden qualification requires an actual offscreen adapter");
let adapter = format!("{:?}", renderer.adapter_info());
```

Add `adapter: String` to `Frame`; assign `adapter` in `render`, `String::new()` in `read_png`, and `actual.adapter.clone()` in `diagnostic_diff`. This reports the selected adapter, not just an environment request.

- [ ] Add this evidence function and call it immediately after `let actual = render(case);`. This leaves the comparison, tolerance and update branch otherwise unchanged:

```rust
fn record_evidence(case: Case, frame: &Frame) {
    let Some(directory) = std::env::var_os("QOS_TYPOGRAPHY_EVIDENCE_DIR") else { return; };
    let directory = PathBuf::from(directory);
    std::fs::create_dir_all(&directory).expect("create typography evidence directory");
    write_png(&directory.join(format!("{}-actual.png", case.name)), frame);
    let receipt = serde_json::json!({
        "case": case.name,
        "width": frame.width,
        "height": frame.height,
        "logical_width": LOGICAL_WIDTH,
        "display_scale": case.display_scale,
        "text_scale": case.text_scale,
        "light": case.light,
        "font_manifest_sha256": ui::font_catalog::catalog_manifest_sha256(),
        "font_scope": "controlled-product-and-fixtures",
        "font_locale": "en-US",
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "adapter": frame.adapter,
        "wgpu_backend_request": std::env::var("WGPU_BACKEND").ok(),
        "vulkan_driver_files_request": std::env::var("VK_DRIVER_FILES").ok(),
        "vulkan_legacy_icd_request": std::env::var("VK_ICD_FILENAMES").ok(),
    });
    std::fs::write(directory.join(format!("{}-receipt.json", case.name)),
        serde_json::to_vec_pretty(&receipt).expect("serialize typography receipt"))
        .expect("write typography receipt");
}
```

- [ ] Controller: run the golden on idle Fedora first with the ARM64 software Vulkan ICD already selected by the controller; run Mac Metal second. Example Mac invocation:

```sh
CARGO_TARGET_DIR=/Users/xaver/dev/qos/target/macos-shell WGPU_BACKEND=metal python3 .superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py env QOS_TYPOGRAPHY_EVIDENCE_DIR=/tmp/qos-typography-canonical-mac cargo nextest run -p ui --test it --locked --profile ci --test-threads=1 -E 'test(typography_visual_regression::multilingual_gallery_matches_scale_matrix)'
```

The controller substitutes Fedora's target root, `WGPU_BACKEND=vulkan`, an **observed existing** `/usr/share/vulkan/icd.d/*aarch64*.json` software ICD, and a distinct evidence directory. Do not invent the installed ICD filename or use `UPDATE_TYPOGRAPHY_BASELINES` here.

- [ ] Preserve diagnostics and add a companion `environment.txt` to each evidence folder with output from these read-only commands, each separately, plus the full command and exit status used:

```sh
git rev-parse HEAD
git status --short
rustc -Vv
shasum -a 256 Cargo.lock ui/assets/fonts/manifest.toml
```

On Fedora also preserve exact Mesa/font RPM NEVRs used in the **unmodified baseline run**. Preserve the old actual/diff PNGs before the new run reuses `/tmp/typography-visual-regression`. Include expected PNG hashes and Vello vendor revision from git. Existing raw `d19f1a0ae` logs are evidence, not a pass condition.

- [ ] Gate decision: unit contracts must pass before default integration. A historical PNG mismatch is retained as an **open visual acceptance gate** with six per-case dimensions, differing-pixel counts, receipts, actuals and diffs. Do not claim equivalence, widen tolerances, or regenerate. The parent rendering qualification resolves it after reviewing preserved captures and historical build provenance. The loader/fixture commit can be reviewed independently as `test(ui): isolate typography fonts and record rendering provenance`; its commit description must state whether the historical visual gate is still failing.

