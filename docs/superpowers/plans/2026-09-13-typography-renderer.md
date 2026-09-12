# Shared Typography Font Handles and Painted Weight Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Paint the selected collection face from shared font bytes and use the weight that cosmic-text actually shaped for every plain/rich glyph run.

**Architecture:** Enable the pinned cosmic-text `peniko` adapter and consume its existing `(fontdb::ID, Weight)` cache instead of maintaining copied qOS font handles. Carry shaped weight through plain cached runs and rich contiguous batches; resolve `wght` from that weight at the final normalization boundary while preserving existing non-weight axis behavior. Verify real Vello scene resources with the explicit controlled catalog.

**Tech Stack:** Rust, cosmic-text 0.19.0, fontdb 0.23.0, harfrust 0.5.2, skrifa 0.40.0, linebender_resource_handle 0.1.1; patched Vello 0.9 in `vendor/vello` and `vendor/vello_encoding`.

**Spec:** `docs/superpowers/specs/2026-09-12-macos-shell-port-design.md`, sections 9 and 11; prerequisite `docs/superpowers/plans/2026-09-13-canonical-font-resources.md`, Task 2; investigation `.superpowers/sdd/2026-09-12-macos-shell-build-foundation/typography-renderer-findings.md`, including its follow-up API review.

## Global Constraints

- “Run both hosts from the same git commit and lockfile, with separate native build directories.”
- “Keep Mac rendering expectations explicit about Metal, fonts, scale and pixel tolerance instead of weakening Linux goldens to make them pass on another backend.”
- “Use the repository's nextest process isolation and GPU scheduling rules. Keep the isolated visual-golden tail so those tests are executed after other GPU work drains.”
- “Preserve Linux CLI defaults and commands.”
- Every runtime test command uses the controller-owned isolated-environment helper `.superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py`, clearing qOS runtime inputs and using temporary XDG paths while preserving HOME/toolchain. The helper is created by the wake-integration plan before execution.
- Worktree `/Users/xaver/dev/qos-macos-shell`; Mac target `/Users/xaver/dev/qos/target/macos-shell`. Fedora and broader cross-host integration are controller-owned.
- The explicit catalog loader prerequisite passed task review and its seven focused tests and library clippy on both hosts at cc2cfd954. Preserve that public loader and its verified resources.
- No default catalog activation, font installation, package changes, new font payload, font subsetting, golden PNG regeneration, metric tolerance relaxation, renderer upgrade, or new unsafe mmap code.
- This is a shared-renderer prerequisite. It neither implements the actual shell/application host port nor establishes final Mac/Linux rendering parity.
- The candidate was produced by source inspection only and adopted after controller self-review. No compilation, tests, VM actions, or git operations were run while drafting it.

## Decisions established from the pinned source

1. `cosmic_text::FontSystem::get_font(&mut self, ID, Weight) -> Option<Arc<Font>>` already caches by face and weight and invokes fontdb's sharing internally. `Font::new` creates `FontData::new(Blob::new(shared_arc), info.index)`. Enabling `peniko = []` exposes `Font::as_peniko(&self) -> FontData`; it adds no dependency or version change. The Vello re-export is the same resource-handle type in this lockfile.
2. Remove qOS's `font_cache`. The alternative minimal cache would preserve `Arc` for Binary/SharedFile, read File safely once per path, and invalidate paths/faces on generation changes. It would also need to handle File→SharedFile transitions during shaping. That duplicates a cache already available from the selected shaped font. The adapter is smaller and retains source ownership without an additional read, mmap, or byte copy.
3. Repeated retrieval of the **same face and weight** shares Blob identity. Distinct faces or weights can have different Blob IDs while sharing exactly the same byte-owner `Arc`. Test both facts; do not assert identical Blob IDs across a TTC's five faces.
4. `effective_weight` already rounds explicit `wght` to the nearest integer, with positive half values rounding upward, then clamps to 1..1000. Cosmic shaping passes `Attrs.weight`, records it in `LayoutGlyph.font_weight`, and constructs its HarfRust instance from the corresponding skrifa `wght` location. Keep that contract. Paint uses `f32::from(glyph.font_weight.0)`, so 675.4 paints 675 and 675.5 paints 676. Face axis limits subsequently clamp through skrifa. No full fractional shaping support is claimed.
5. Rich non-bold spans inherit the effective block weight; `.bold(true)` shapes at 700 even if the block has an explicit weight axis. This existing shaping decision remains authoritative at paint. Weight joins face, color, size and baseline in the batch key. A visible discretionary hyphen inherits its marker's weight via the existing cloned glyph.
6. Arbitrary axes and automatic `opsz` already affect painting but are not passed to this pinned shaper's `Font::new`, which only supplies `wght`. Preserve their current painting behavior and native axis ordering; do not claim complete shaping parity for all axes. Fixing that broader API limitation is outside this defect plan.
7. Public `font_system` replacement still requires `bump_generation` and rebuilding old rich layouts. Removing the qOS font cache means replacement cannot retain an old qOS handle keyed by a reused database ID. Cosmic owns its own cache; mutating/replacing bytes behind an existing face ID is not a supported reload mechanism. Replace the FontSystem for changed font contents.
8. `Scene::encoding().resources` exposes `glyph_runs`, each run's `font.data`, `font.index`, coordinate range and glyph range. Regressions inspect that actual emitted data after `draw_style`/`draw_rich`; no test-only substitute renderer or fabricated glyph runs.

## File ownership and task interfaces

| File | Responsibility |
| --- | --- |
| `ui/Cargo.toml` | Enable cosmic-text's existing adapter, unchanged versions. |
| `ui/src/text_render.rs` | Shaped font/weight plumbing, shared handles, plain and rich batching, normalized painting weight. |
| `ui/src/text_render/renderer_regressions.rs` (new) | CPU-only controlled-catalog tests of actual emitted Scene resources. |
| `ui/src/richtext.rs` | Clarify that stored block axes do not override each shaped glyph's weight; no shaping policy change. |

Task 1 produces `GlyphRun.font_weight: Weight` and private `TextRenderer::vello_font(&mut self, face_id: fontdb::ID, weight: Weight) -> Option<Font>`, plus reusable regression utilities. Task 2 consumes those interfaces and changes `normalized_coords` to require a shaped weight. The tasks are separately reviewable: correct collection identity/backing ownership does not depend on changing painted weight.

---

### Task 1: Preserve selected collection faces and reuse shaped-font backing data

**Files:** Modify `ui/Cargo.toml`; modify `ui/src/text_render.rs` imports, `GlyphRun`, `TextRenderer`, `with_font_system`, `flatten_glyph_runs`, `paint_runs`, `vello_font`, `flush_glyphs`; create `ui/src/text_render/renderer_regressions.rs`.

**Interfaces:**
- Consumes `FontCatalog::controlled_fixture(root: impl Into<PathBuf>, locale: impl Into<String>) -> FontCatalog` and `TextRenderer::with_font_catalog(&FontCatalog) -> Result<TextRenderer, FontCatalogError>` from the loader task.
- Produces `GlyphRun { face_id: fontdb::ID, font_weight: Weight, glyphs: Vec<vello::Glyph>, normalized_coords: Vec<vello::NormalizedCoord> }`.
- Produces `fn vello_font(&mut self, face_id: fontdb::ID, weight: Weight) -> Option<Font>` using cosmic's selected font.
- Missing font resolution emits no glyphs instead of constructing an empty invalid font and passing it to Vello's `FontRef::from_index(...).unwrap()`.

- [ ] Add the regression module beside the existing `mod tests` (the existing tests remain):

```rust
#[cfg(test)]
mod renderer_regressions;
```

Create `ui/src/text_render/renderer_regressions.rs` with this complete initial content:

```rust
use super::*;
use crate::font_catalog::{FontCatalog, configured_font_system_with_catalog};
use cosmic_text::fontdb::{ID, Source};
use std::path::PathBuf;

fn catalog() -> FontCatalog {
    FontCatalog::controlled_fixture(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/fonts"),
        "en-US",
    )
}

fn renderer() -> TextRenderer {
    TextRenderer::with_font_catalog(&catalog()).expect("verified controlled catalog")
}

fn named_style(family: &str) -> TextStyle {
    TextStyle::new(24.0)
        .with_family(FontChoice::Named(family.to_owned()))
        .with_optical_sizing(OpticalSizing::None)
}

fn draw(renderer: &mut TextRenderer, text: &str, style: &TextStyle) -> Scene {
    let mut scene = Scene::new();
    renderer.draw_style(
        &mut scene, text, Color::WHITE, Affine::IDENTITY, None, style,
    );
    assert!(!scene.encoding().resources.glyph_runs.is_empty());
    scene
}

fn one_run(scene: &Scene) -> &vello_encoding::GlyphRun {
    let runs = &scene.encoding().resources.glyph_runs;
    assert_eq!(runs.len(), 1);
    assert!(!runs[0].glyphs.is_empty());
    &runs[0]
}

fn face_id(renderer: &TextRenderer, family: &str) -> ID {
    renderer.font_system.db().query(&cosmic_text::fontdb::Query {
        families: &[Family::Name(family)],
        ..cosmic_text::fontdb::Query::default()
    }).expect("catalog family")
}

fn assert_source_owner(renderer: &TextRenderer, id: ID, font: &Font) {
    let (owner, _) = font.data.clone().into_raw_parts();
    let face = renderer.font_system.db().face(id).expect("shaped face");
    match &face.source {
        Source::Binary(data) | Source::SharedFile(_, data) => {
            assert!(Arc::ptr_eq(&owner, data), "paint must retain the shaping owner");
        }
        Source::File(_) => panic!("shaping must have resolved the file source"),
    }
}

#[test]
fn collection_indices_and_shared_owners_reach_the_real_scene() {
    let mut renderer = renderer();
    let mut fonts = Vec::new();
    for (family, index) in [
        ("Noto Sans CJK JP", 0), ("Noto Sans CJK KR", 1),
        ("Noto Sans CJK SC", 2), ("Noto Sans CJK TC", 3),
        ("Noto Sans CJK HK", 4),
    ] {
        let id = face_id(&renderer, family);
        assert_eq!(renderer.font_system.db().face(id).unwrap().index, index);
        let style = named_style(family);
        let scene = draw(&mut renderer, "漢", &style);
        let font = one_run(&scene).font.clone();
        assert_eq!(font.index, index, "{family}");
        assert_source_owner(&renderer, id, &font);
        let repeat = draw(&mut renderer, "漢", &style);
        assert_eq!(one_run(&repeat).font.data.id(), font.data.id());
        fonts.push(font);
    }
    let (owner, _) = fonts[0].data.clone().into_raw_parts();
    for font in &fonts[1..] {
        let (other, _) = font.data.clone().into_raw_parts();
        assert!(Arc::ptr_eq(&owner, &other), "TTC faces share one byte owner");
    }
}

#[test]
fn binary_font_source_is_shared_with_emitted_runs() {
    let mut renderer = renderer();
    // Keep the explicit fixture database, replacing only this known TTC's
    // representation to cover Binary as well as the loader's File source.
    let ids: Vec<_> = renderer.font_system.db().faces()
        .filter(|face| face.families.iter().any(|(name, _)| name.starts_with("Noto Sans CJK ")))
        .map(|face| face.id).collect();
    assert_eq!(ids.len(), 5);
    for id in ids {
        renderer.font_system.db_mut().remove_face(id);
    }
    let bytes = std::fs::read(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/fonts/NotoSansCJK-VF.ttc"),
    ).unwrap();
    let owner: Arc<dyn AsRef<[u8]> + Send + Sync> = Arc::new(bytes);
    assert_eq!(renderer.font_system.db_mut()
        .load_font_source(Source::Binary(owner.clone())).len(), 5);
    renderer.bump_generation();
    let style = named_style("Noto Sans CJK SC");
    let scene = draw(&mut renderer, "漢", &style);
    let font = &one_run(&scene).font;
    assert_eq!(font.index, 2);
    let (emitted_owner, _) = font.data.clone().into_raw_parts();
    assert!(Arc::ptr_eq(&owner, &emitted_owner));
}

#[test]
fn different_shaped_weights_reuse_bytes_and_system_replacement_refreshes_handles() {
    let mut renderer = renderer();
    let style = named_style("Noto Sans");
    let regular = draw(&mut renderer, "A", &style);
    let bold = draw(&mut renderer, "A", &style.clone().with_weight(TextWeight::Bold));
    let (regular_owner, _) = one_run(&regular).font.data.clone().into_raw_parts();
    let (bold_owner, _) = one_run(&bold).font.data.clone().into_raw_parts();
    assert!(Arc::ptr_eq(&regular_owner, &bold_owner));
    renderer.font_system = configured_font_system_with_catalog(&catalog()).unwrap();
    renderer.bump_generation();
    let replacement = draw(&mut renderer, "A", &style);
    let new_font = &one_run(&replacement).font;
    assert_source_owner(&renderer, face_id(&renderer, "Noto Sans"), new_font);
    let (new_owner, _) = new_font.data.clone().into_raw_parts();
    assert!(!Arc::ptr_eq(&regular_owner, &new_owner));
    assert_ne!(one_run(&regular).font.data.id(), new_font.data.id());
    // The old scene remains readable while its own handle keeps bytes alive.
    assert_eq!(one_run(&regular).font.data.len(), new_font.data.len());
}
```

- [ ] Run the initial red regression selection, preserving process isolation:

```bash
CARGO_TARGET_DIR=/Users/xaver/dev/qos/target/macos-shell python3 .superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py cargo nextest run -p ui --lib --locked --profile ci -E 'test(text_render::renderer_regressions::)'
```

Expected: failure on actual source-owner/index assertions. Resolve unrelated prerequisite compilation errors with the loader owner before interpreting a result as a regression failure. Do not enable the feature before observing the behavioral failures.

- [ ] Enable the adapter with this exact dependency entry; no version or lockfile change is expected:

```toml
cosmic-text = { version = "0.19.0", default-features = false, features = ["fontconfig", "peniko", "swash", "std"] }
```

- [ ] Remove `use std::fs;`, the `font_cache: HashMap<cosmic_text::fontdb::ID, Font>` field and its initializer. Replace `GlyphRun` and `vello_font` with:

```rust
#[derive(Clone)]
struct GlyphRun {
    face_id: cosmic_text::fontdb::ID,
    font_weight: Weight,
    glyphs: Vec<vello::Glyph>,
    normalized_coords: Vec<vello::NormalizedCoord>,
}

/// Retain the exact face and shared bytes already used by shaping.
fn vello_font(&mut self, face_id: cosmic_text::fontdb::ID, weight: Weight) -> Option<Font> {
    self.font_system.get_font(face_id, weight).map(|font| font.as_peniko())
}
```

Update `shape_cache`'s field comment to end “while the font system is accessed.” Replace the old `paint_runs` cache documentation with “Resolve font handles through cosmic-text's existing face-and-weight cache.”

- [ ] Replace `flatten_glyph_runs` with the following implementation. It carries the actual weight required for `get_font`; paint-coordinate policy is intentionally unchanged until Task 2.

```rust
fn flatten_glyph_runs(&mut self, buffer: &Buffer, style: &TextStyle) -> Vec<GlyphRun> {
    let variations = style.resolved_variations();
    let mut runs = Vec::new();
    for run in buffer.layout_runs() {
        let exercised_hyphen = run.glyphs.last()
            .filter(|glyph| run.text.get(glyph.start..glyph.end) == Some("\u{ad}"));
        let mut current: Option<(cosmic_text::fontdb::ID, Weight)> = None;
        let mut glyphs: Vec<vello::Glyph> = Vec::new();
        for g in run.glyphs {
            if run.text.get(g.start..g.end) == Some("\u{ad}") {
                continue;
            }
            let key = (g.font_id, g.font_weight);
            if let Some((face_id, font_weight)) = current
                && current != Some(key)
                && !glyphs.is_empty()
            {
                runs.push(GlyphRun {
                    face_id,
                    font_weight,
                    glyphs: std::mem::take(&mut glyphs),
                    normalized_coords: self.normalized_coords(face_id, &variations),
                });
            }
            current = Some(key);
            glyphs.push(vello::Glyph {
                id: u32::from(g.glyph_id),
                x: hanging_glyph_x(&run, g, style.hanging_punctuation),
                y: run.line_y + g.y,
            });
        }
        if let Some((face_id, font_weight)) = current
            && !glyphs.is_empty()
        {
            runs.push(GlyphRun {
                face_id,
                font_weight,
                glyphs,
                normalized_coords: self.normalized_coords(face_id, &variations),
            });
        }
        if let Some(marker) = exercised_hyphen
            && let Some(hyphen) = self.visible_discretionary_hyphen(marker)
        {
            runs.push(GlyphRun {
                face_id: hyphen.font_id,
                font_weight: hyphen.font_weight,
                glyphs: vec![vello::Glyph {
                    id: u32::from(hyphen.glyph_id),
                    x: hyphen.x,
                    y: run.line_y + hyphen.y,
                }],
                normalized_coords: self.normalized_coords(hyphen.font_id, &variations),
            });
        }
    }
    runs
}
```

- [ ] In `paint_runs`, replace its font lookup with this guard and keep the existing `.draw_glyphs` builder:

```rust
let Some(font) = self.vello_font(run.face_id, run.font_weight) else {
    continue;
};
```

In `flush_glyphs`, replace its initial font lookup with these guards; the remainder stays unchanged in Task 1:

```rust
let Some(first) = glyphs.first() else {
    return;
};
let Some(vello_font) = self.vello_font(face_id, first.font_weight) else {
    return;
};
```

- [ ] Run the Task 1 regression command again. Expected: all three tests pass, including nonzero face indices, source-owner identity, repeat identity, Binary ownership and handle refresh on FontSystem replacement. The same family/weight must hit cosmic's existing font cache; there is no new qOS byte cache to invalidate.

- [ ] Format touched Rust files and run scoped lint validation:

```bash
rustfmt --edition 2024 ui/src/text_render.rs ui/src/text_render/renderer_regressions.rs
CARGO_TARGET_DIR=/Users/xaver/dev/qos/target/macos-shell cargo clippy -p ui --lib --locked -- -D warnings
```

If the repository's pedantic lints require a narrow test-only allowance for these explicit assertions, fix the exact diagnostic at the item; do not broadly disable linting. The nextest selection already compiles the regression tests; library clippy keeps the lint gate scoped to the changed library.

- [ ] Review and commit Task 1 independently:

```bash
git add ui/Cargo.toml ui/src/text_render.rs ui/src/text_render/renderer_regressions.rs
git commit -m "fix(ui): reuse shaped font handles and preserve collection indices"
```

---

### Task 2: Paint each plain/rich batch at its actual shaped weight

**Files:** Modify `ui/src/text_render.rs` normalization, its call sites, rich batch key, weight documentation and the affected existing axis-order test; modify `ui/src/richtext.rs` field documentation; append tests to `ui/src/text_render/renderer_regressions.rs`.

**Interfaces:**
- Consumes Task 1's `GlyphRun.font_weight` and `vello_font(face_id, weight) -> Option<Font>`.
- Produces `fn normalized_coords(&self, face_id: fontdb::ID, variations: &[FontVariation], font_weight: Weight) -> Vec<vello::NormalizedCoord>`.
- `variations` supplies non-weight axes. `font_weight` always replaces all `wght` entries; it is the exact `Weight` on the shaped glyphs. Duplicate axis settings supplied through public fields cannot make painting ignore this weight.
- No changes to `RichSpan` or public shaping constructors. `RichTextLayout::font_variations` remains a block-level list, with weight overridden at emission by each shaped batch.

- [ ] Append these tests to the regression module. Expected locations come from pinned font metadata and literal requested integer weights, independently of the production normalization function. Every result asserted is from an actual emitted Vello run.

```rust
use crate::richtext::{RichSpan, RichTextLayout, TextAlign};
use skrifa::{FontRef, MetadataProvider, Tag};

fn emitted_coords(scene: &Scene, run: &vello_encoding::GlyphRun) -> Vec<i16> {
    scene.encoding().resources.normalized_coords[run.normalized_coords.clone()].to_vec()
}

fn expected_coords(font: &Font, weight: u16, optical: Option<f32>) -> Vec<i16> {
    let font = FontRef::from_index(font.data.as_ref(), font.index).unwrap();
    let mut settings = vec![(Tag::new(b"wght"), f32::from(weight))];
    if let Some(size) = optical {
        settings.push((Tag::new(b"opsz"), size));
    }
    font.axes().location(settings).coords().iter().map(|coord| coord.to_bits()).collect()
}

fn assert_plain_weight(renderer: &mut TextRenderer, style: &TextStyle, expected: u16) {
    let buffer = renderer.shape_buffer("Hamburgefon", None, style);
    let shaped: Vec<_> = buffer.layout_runs().flat_map(|run| run.glyphs.iter())
        .map(|glyph| (glyph.font_id, glyph.font_weight)).collect();
    assert!(!shaped.is_empty());
    assert!(shaped.iter().all(|(_, weight)| *weight == Weight(expected)));
    for _ in 0..2 { // Includes the cached draw path.
        let scene = draw(renderer, "Hamburgefon", style);
        let run = one_run(&scene);
        assert_eq!(run.font.index, renderer.font_system.db().face(shaped[0].0).unwrap().index);
        assert_source_owner(renderer, shaped[0].0, &run.font);
        assert_eq!(emitted_coords(&scene, run), expected_coords(&run.font, expected, None));
    }
}

#[test]
fn plain_paint_uses_named_and_rounded_explicit_shaped_weights() {
    let mut renderer = renderer();
    // Adwaita has a 100..900 weight range. Noto Sans also covers its narrower
    // 400..700 range, so requests below/above a face's range are exercised.
    // CJK SC also exercises weight coordinates on collection index 2.
    for family in ["Adwaita Sans", "Noto Sans", "Noto Sans CJK SC"] {
        let base = named_style(family);
        for (weight, expected) in [
            (TextWeight::Regular, 400), (TextWeight::Medium, 500),
            (TextWeight::Semibold, 600), (TextWeight::Bold, 700),
        ] {
            assert_plain_weight(&mut renderer, &base.clone().with_weight(weight), expected);
        }
        for (value, expected) in [
            (675.4, 675), (675.5, 676), (400.49, 400),
            (-20.0, 1), (1200.0, 1000),
        ] {
            let style = base.clone().with_weight(TextWeight::Bold)
                .with_font_variation(OpenTypeTag::WEIGHT, value);
            assert_plain_weight(&mut renderer, &style, expected);
        }
    }
}

#[test]
fn weight_fix_preserves_optical_axis_order_in_actual_runs() {
    let mut renderer = renderer();
    let style = named_style("Adwaita Sans").with_weight(TextWeight::Bold)
        .with_optical_sizing(OpticalSizing::Value(32.0));
    let scene = draw(&mut renderer, "A", &style);
    let run = one_run(&scene);
    assert_eq!(emitted_coords(&scene, run), expected_coords(&run.font, 700, Some(32.0)));
    let font = FontRef::from_index(run.font.data.as_ref(), run.font.index).unwrap();
    assert_eq!(font.axes().iter().map(|axis| axis.tag()).collect::<Vec<_>>(),
        [Tag::new(b"opsz"), Tag::new(b"wght")]);
    assert_eq!(emitted_coords(&scene, run)[0], 16384);
}

#[test]
fn rich_same_face_runs_split_at_shaped_weight_and_retain_span_bold() {
    let mut renderer = renderer();
    let style = named_style("Noto Sans")
        .with_font_variation(OpenTypeTag::WEIGHT, 675.4);
    let layout = RichTextLayout::shape_styled(
        &mut renderer.font_system,
        &[RichSpan::new("A"), RichSpan::new("B").bold(true), RichSpan::new("C")],
        &style, None, TextAlign::Left, vec![], vec![],
    );
    let glyphs: Vec<_> = layout.buffer.layout_runs()
        .flat_map(|run| run.glyphs.iter().cloned()).collect();
    assert_eq!(glyphs.len(), 3);
    assert_eq!(glyphs.iter().map(|g| g.font_weight.0).collect::<Vec<_>>(), [675, 700, 675]);
    assert!(glyphs.iter().all(|g| g.font_id == glyphs[0].font_id));
    let mut scene = Scene::new();
    renderer.draw_rich(&mut scene, &layout, Affine::IDENTITY, Color::WHITE);
    let resources = &scene.encoding().resources;
    assert_eq!(resources.glyph_runs.len(), 3);
    for (run, glyph) in resources.glyph_runs.iter().zip(&glyphs) {
        assert_eq!(run.glyphs.len(), 1);
        assert_eq!(resources.glyphs[run.glyphs.start].id, u32::from(glyph.glyph_id));
        assert_eq!(emitted_coords(&scene, run), expected_coords(&run.font, glyph.font_weight.0, None));
        assert_source_owner(&renderer, glyph.font_id, &run.font);
    }
}

#[test]
fn rich_equal_weight_adjacent_spans_stay_batched() {
    let mut renderer = renderer();
    let style = named_style("Noto Sans").with_weight(TextWeight::Bold);
    let layout = RichTextLayout::shape_styled(
        &mut renderer.font_system,
        &[RichSpan::new("A"), RichSpan::new("B").bold(true), RichSpan::new("C")],
        &style, None, TextAlign::Left, vec![], vec![],
    );
    let mut scene = Scene::new();
    renderer.draw_rich(&mut scene, &layout, Affine::IDENTITY, Color::WHITE);
    let run = one_run(&scene);
    assert_eq!(run.glyphs.len(), 3);
    assert_eq!(emitted_coords(&scene, run), expected_coords(&run.font, 700, None));
}

#[test]
fn rich_visible_discretionary_hyphen_uses_its_bold_marker_weight() {
    let mut renderer = renderer();
    let style = named_style("Noto Sans")
        .with_font_variation(OpenTypeTag::WEIGHT, 675.4);
    let bold = named_style("Noto Sans").with_weight(TextWeight::Bold);
    let width = renderer.measure_style("ab-", None, &bold).0 + 0.5;
    let spans = [RichSpan::new("ab\u{ad}cd").bold(true)];
    let layout = RichTextLayout::shape_styled(
        &mut renderer.font_system, &spans, &style, Some(width),
        TextAlign::Left, vec![], vec![],
    );
    let marker = layout.buffer.layout_runs().find_map(|run| {
        run.glyphs.last().filter(|g| run.text.get(g.start..g.end) == Some("\u{ad}"))
            .map(|g| (g.clone(), run.line_y))
    }).expect("the controlled narrow layout exercises the discretionary break");
    assert_eq!(marker.0.font_weight, Weight(700));
    // Resolve the expected visible glyph from the selected face's charmap,
    // independent of the renderer's visible_discretionary_hyphen helper.
    let font = renderer.font_system.get_font(marker.0.font_id, marker.0.font_weight).unwrap();
    let charmap = font.as_swash().charmap();
    let unicode_hyphen = charmap.map('\u{2010}');
    let expected_id = if unicode_hyphen == 0 { charmap.map('-') } else { unicode_hyphen };
    assert_ne!(expected_id, 0);
    let mut scene = Scene::new();
    renderer.draw_rich(&mut scene, &layout, Affine::IDENTITY, Color::WHITE);
    let resources = &scene.encoding().resources;
    let hyphen_runs: Vec<_> = resources.glyph_runs.iter().filter(|run| {
        let glyphs = &resources.glyphs[run.glyphs.clone()];
        glyphs.len() == 1 && glyphs[0].id == u32::from(expected_id)
            && (glyphs[0].x - marker.0.x).abs() < 0.001
            && (glyphs[0].y - (marker.1 + marker.0.y)).abs() < 0.001
    }).collect();
    assert_eq!(hyphen_runs.len(), 1);
    let run = hyphen_runs[0];
    assert_eq!(emitted_coords(&scene, run), expected_coords(&run.font, 700, None));
    assert_source_owner(&renderer, marker.0.font_id, &run.font);
    let wide = RichTextLayout::shape_styled(
        &mut renderer.font_system, &spans, &style, Some(10_000.0),
        TextAlign::Left, vec![], vec![],
    );
    let mut wide_scene = Scene::new();
    renderer.draw_rich(&mut wide_scene, &wide, Affine::IDENTITY, Color::WHITE);
    assert_eq!(wide_scene.encoding().resources.glyphs.len(), 4);
}
```

- [ ] Run the regression selection before implementing Task 2:

```bash
CARGO_TARGET_DIR=/Users/xaver/dev/qos/target/macos-shell python3 .superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py cargo nextest run -p ui --lib --locked --profile ci -E 'test(text_render::renderer_regressions::)'
```

Expected red assertions: default medium/semibold/bold paints default axis; fractional explicit axis differs from integer shaped weight; rich same-face weights coalesce; bold marker paints block weight. If the controlled narrow fixture does not exercise the break, inspect its emitted layout and correct its deterministic width without changing production line fitting. The `.expect` prohibits silently passing without a visible-hyphen case.

- [ ] Replace `normalized_coords` with:

```rust
fn normalized_coords(
    &self,
    face_id: cosmic_text::fontdb::ID,
    variations: &[FontVariation],
    font_weight: Weight,
) -> Vec<vello::NormalizedCoord> {
    use skrifa::{FontRef, MetadataProvider, Tag};

    self.font_system.db().with_face_data(face_id, |data, index| {
        let font = FontRef::from_index(data, index).ok()?;
        let settings = variations.iter()
            .filter(|axis| axis.tag != OpenTypeTag::WEIGHT)
            .map(|axis| (Tag::new(&axis.tag.0), axis.value))
            .chain(std::iter::once((Tag::new(b"wght"), f32::from(font_weight.0))));
        let location = font.axes().location(settings);
        Some(location.coords().iter().map(|coord| coord.to_bits()).collect())
    }).flatten().unwrap_or_default()
}
```

A font with no variable axes naturally returns an empty vector. Do not keep the old `variations.is_empty()` early return: an otherwise empty list still needs its shaped weight.

- [ ] Pass shaped weight at all call sites. In both ordinary `GlyphRun` constructors in `flatten_glyph_runs`, use:

```rust
normalized_coords: self.normalized_coords(face_id, &variations, font_weight),
```

In its visible-hyphen constructor use:

```rust
normalized_coords: self.normalized_coords(hyphen.font_id, &variations, hyphen.font_weight),
```

In `flush_glyphs`, after the Task 1 `first` and font guards, use:

```rust
debug_assert!(glyphs.iter().all(|glyph| glyph.font_weight == first.font_weight));
let normalized_coords = self.normalized_coords(face_id, variations, first.font_weight);
```

This replaces the old two-argument normalization call, leaving the rest of the draw builder intact.

- [ ] Extend the rich batch key in `draw_rich_run`. Add beside `cur_color`:

```rust
let mut cur_weight: Option<Weight> = None;
```

Use this exact `changed` expression:

```rust
let changed = face != Some(g.font_id)
    || cur_weight != Some(g.font_weight)
    || cur_color != g.color_opt
    || (g.font_size - batch_size).abs() > f32::EPSILON
    || cur_shift != g_shift;
```

After `face = Some(g.font_id);`, add:

```rust
cur_weight = Some(g.font_weight);
```

The existing flush-before-update order is required. Keep the existing marker clone and dedicated hyphen flush: its `first.font_weight` is the marker's weight. Replace the two batching comments in `draw_rich`/`draw_rich_run` with “Glyphs batch by face, shaped weight, color, size and baseline shift.”

- [ ] Document the exact policy at the source boundaries. Above `effective_weight` add:

```rust
/// Resolve the integer weight accepted by cosmic-text: the last explicit
/// `wght` overrides the named tier, rounds to nearest, then clamps to 1..1000.
/// Painting uses each shaped glyph's resulting weight, including span bold.
```

Above `RichTextLayout::font_variations` add:

```rust
/// Block-level paint axes. The renderer replaces `wght` with each batch's
/// shaped glyph weight so an explicit block axis cannot override span bold.
```

- [ ] Update the existing `variable_axes_are_normalized_in_the_faces_native_axis_order` test to use the deterministic loader, replacing its renderer construction with:

```rust
let catalog = crate::font_catalog::FontCatalog::controlled_fixture(
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/fonts"),
    "en-US",
);
let renderer = TextRenderer::with_font_catalog(&catalog).unwrap();
```

Pass `Weight(700)` as the third argument after its existing variation slice. Change its expectation message to `"the controlled catalog supplies Adwaita Sans"`. Keep its existing axis assertions; the new actual-scene optical regression supplements it.

- [ ] Run the complete renderer regression selection again. Expected: all eight tests pass (three Task 1, five Task 2), including actual run boundaries, rounded explicit weights, cache hits, optical axis order and the exercised/unused discretionary break.

- [ ] Run the affected existing CPU tests and scoped linting, with process isolation and without the visual-golden suite:

```bash
rustfmt --edition 2024 ui/src/text_render.rs ui/src/text_render/renderer_regressions.rs ui/src/richtext.rs
CARGO_TARGET_DIR=/Users/xaver/dev/qos/target/macos-shell python3 .superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py cargo nextest run -p ui --lib --locked --profile ci -E 'test(text_render::) or test(richtext::)'
CARGO_TARGET_DIR=/Users/xaver/dev/qos/target/macos-shell cargo clippy -p ui --lib --locked -- -D warnings
```

Existing tests that construct ambient `TextRenderer::new()` may still expose the documented host-font prerequisite. Do not silently skip them or use renderer fixes to weaken their assertions. Record each failure with its source boundary, and let the controller coordinate deterministic-fixture migration where needed. CPU regression failures attributable to these changes block Task 2 acceptance.

- [ ] Review and commit this separately from handle identity work:

```bash
git add ui/src/text_render.rs ui/src/text_render/renderer_regressions.rs ui/src/richtext.rs
git commit -m "fix(ui): paint variable glyph runs at their shaped weight"
```

## Review and handoff evidence

- Task 1 review checks actual nonzero indices 1..4, unchanged index 0, shared source ownership for File→SharedFile and Binary, stable repeated Blob identity, cross-weight byte ownership and fresh handles after FontSystem replacement.
- Task 2 review checks real plain and rich Scene coordinate ranges, all named tiers, explicit override/rounding/clamping, span-bold precedence, unchanged same-weight batching, visible hyphen weight and native `opsz` ordering.
- Report discovered test counts and outcomes, exact failing assertions if any, and scoped clippy status. Feature-only dependency work should preserve `Cargo.lock`; investigate rather than accepting an unrelated resolution change.
- Controller integration still owns unchanged typography golden comparisons on both hosts, broader UI/GPU and workspace gates, regression triage, and later default catalog/runtime packaging activation. These are not waived by CPU tests passing. Existing historical emoji/CJK mismatches may have additional causes; this plan makes no pixel-parity claim before those comparisons.
- No source implementation was performed as part of drafting this candidate. Execute Task 1 then Task 2 with their separate review gates after the current wake integration implementation is committed; only one source implementer runs at a time.
