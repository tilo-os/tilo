# Text Widget Exactly-Once Scale Implementation Plan V2

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make retained `Text` apply accessibility text scale exactly once in measurement and encoded glyph runs while preserving effective Theme snapshots, base-style renderer behavior, modifier order, cache correctness, and pluggable layout engines.

**Architecture:** Theme typography stays effective and baked, and `TextRenderer` stays the single glyph-boundary owner of accessibility scaling for legacy low-level callers. `Text` privately retains the sanitized Theme factor last observed by `build_layout`/`draw`, divides only a Theme-baked font size in a renderer-input clone, and passes that same clone through default and pluggable measure/draw paths. Any observed factor change invalidates retained layout and paint, including base styles whose stored `TextStyle` does not change.

**Tech Stack:** Rust 2024, Taffy, cosmic-text 0.19.0, Vello 0.9 scene encoding, existing controlled `FontCatalog`, cargo-nextest.

**Spec:** `docs/superpowers/specs/2026-09-12-macos-shell-port-design.md`, especially shared widget theme/scale behavior at lines 316-332 and controlled font/theme rendering at lines 365-420; `.superpowers/sdd/2026-09-13-common-real-app-framework/task-5-effective-presentation-ruling.md`; investigation `.superpowers/sdd/2026-09-13-typography-renderer/prepared-text-scale-boundary-audit.md` (SHA-256 `87f4ba323d7262f8c1373fe6f61ec9b00cfdc75320a0264a5e4e3b891dcfd47e`).

**Candidate:** `.superpowers/sdd/2026-09-14-text-widget-scale/2026-09-14-text-widget-scale-v2.md`  
**Eventual adoption path:** `docs/superpowers/plans/2026-09-14-text-widget-scale.md`

## Global constraints

- Root adopts/schedules this candidate only after the Notes source checkpoint. Use `/Users/user/dev/qos-macos-shell`; root owns later adoption, push, and merge.
- Modify only `ui/src/widget/text.rs`, including its colocated tests. Do not change Theme, `TextRenderer`, `TextLayoutEngine`, public defaults, font resources, manifests, Cargo files, goldens, or native output code.
- Preserve Task5 effective-presentation semantics: effective size replacement remains supplied effective size / supplied Theme text factor × requested renderer factor; supplied UI scale remains baked.
- Normalize only `TextStyle.size` when `follows_role_style || scales_with_theme`. Theme does not bake `letter_spacing`; the renderer continues scaling it once.
- Preserve public `text_style()` as the effective view. Preserve current modifier order and every existing `use_explicit_style`, `dirty`, and `layout_dirty` transition.
- Keep `Text::new`, `with_size`, and `with_style` on the base-style path. Keep low-level renderer APIs unchanged for custom canvas callers.
- Existing Typography Tasks 1-2 (`f574ac742`, `b9105a23b`) and controlled isolation (`114345a25`) are accepted. Audit/list their test population and run it only in the final combined selection; do not reopen those sources.
- Run Mac and Fedora from the same committed SHA and lockfile with separate target directories. Every `cargo nextest run` below includes `--retries 0 --status-level all --final-status-level all`.
- Every Mac Cargo command uses the known FFmpeg environment `PKG_CONFIG_PATH=/opt/homebrew/opt/ffmpeg@8/lib/pkgconfig`, the isolated helper, and `CARGO_TARGET_DIR=/Users/user/dev/qos/target/macos-shell`. Fedora uses `/home/parallels/qos-target` and its controller-established system FFmpeg pkg-config selection; do not inject the Homebrew path there.
- This is CPU logical-text qualification. Tests use `Affine::IDENTITY`; `FrameViewport` output scale remains a separate physical transform. No GUI, GPU, VM, service, golden, or online work belongs here.

## File boundary

| File | Responsibility |
| --- | --- |
| `ui/src/widget/text.rs` | Retain observed Theme text scale, normalize renderer-input font size, invalidate caches, adapt three ambient test constructors, and add controlled real-scene regressions. |

No new public API or source file is required. `crate::font_catalog::fixture_renderer()` and `Driver::new_with_controlled_inputs(State, Box<dyn Widget>, (u32,u32), Theme, TextRenderer, UserId)` already provide the controlled inputs.

---

### Task 1: Apply Text scale exactly once at the widget boundary

**Files:**
- Modify/test: `ui/src/widget/text.rs:30-105,216-351,619-792,926-end`

**Interfaces:**
- Consumes: `Theme::text_scale`, `Theme::tokens.typography.style`, `TextRenderer::{set_text_scale,measure_style,draw_style}`, `TextLayoutEngine::{measure,draw}`, `fixture_renderer`, `Driver::new_with_controlled_inputs`, and Vello encoded glyph resources.
- Produces privately: `observed_theme_text_scale: f32`, `sanitized_theme_text_scale(&Theme) -> f32`, and `renderer_input_style(&self) -> TextStyle`.
- Preserves every public signature.

- [ ] **Step 1: Audit and list the bounded controlled population without running its 42 tests**

```bash
git rev-parse HEAD
shasum -a 256 Cargo.lock ui/assets/fonts/manifest.toml
git diff --check
git diff 114345a25..HEAD -- ui/assets/fonts ui/src/font_catalog.rs ui/src/text_render.rs ui/src/text_render/renderer_regressions.rs
rg -n '^\s*#\[test\]' ui/src/text_render.rs ui/src/text_render/renderer_regressions.rs
rg -n 'fixture_renderer\(\)|controlled_fixture\(' ui/src/text_render.rs ui/src/text_render/renderer_regressions.rs ui/src/font_catalog.rs
rg -n 'TextRenderer::new\(|(?:crate::headless::)?Driver::new\(' ui/src/widget/text.rs
PKG_CONFIG_PATH=/opt/homebrew/opt/ffmpeg@8/lib/pkgconfig CARGO_TARGET_DIR=/Users/user/dev/qos/target/macos-shell python3 .superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py cargo nextest list -p ui --lib --locked -E 'test(widget::text::tests::) or test(text_render::)'
```

At frozen source `5a1cefd76`, `text_render.rs` has 34 direct tests and `renderer_regressions.rs` has eight accepted controlled tests; 29 direct `fixture_renderer()` uses plus the regression module's controlled catalog avoid ambient fonts. The bounded Text audit must find exactly these three ambient constructors before editing:

```text
custom_layout_engine_keeps_text_widget_lifecycle: TextRenderer::new()
locale_switch_remeasures_and_repaints_retained_text_and_button: Driver::new(...)
font_family_override_label_resolves_to_named_family: TextRenderer::new()
```

Review any accepted font/renderer diff before continuing. Do not run the existing 42-test renderer population here or again before Fedora's final selection; source audit plus nonzero `nextest list` is the baseline.

- [ ] **Step 2: Adapt the three existing Text tests to controlled inputs without changing their assertions**

In `custom_layout_engine_keeps_text_widget_lifecycle`, replace only its ambient renderer construction:

```rust
        let mut renderer = crate::font_catalog::fixture_renderer();
```

In `locale_switch_remeasures_and_repaints_retained_text_and_button`, replace `Driver::new` with the existing accepted constructor:

```rust
        let mut driver = crate::headless::Driver::new_with_controlled_inputs(
            crate::State::new(),
            Box::new(root),
            (700, 300),
            Theme::dark(),
            crate::font_catalog::fixture_renderer(),
            crate::permissions::UserId("text-widget-locale-fixture".into()),
        );
```

In `font_family_override_label_resolves_to_named_family`, use a controlled face distinct from the controlled sans default and preserve the existing cache-hit/miss assertions:

```rust
        let mut tr = crate::font_catalog::fixture_renderer();
        let fam = String::from("Adwaita Sans");
        assert!(
            tr.system_families().iter().any(|family| family == &fam),
            "the controlled fixture ships Adwaita Sans",
        );
```

The fixture default is `Noto Sans`; choosing `Adwaita Sans` keeps the final existing assertion that generic sans is a distinct shape-cache miss. Do not alter the lifecycle, locale, family, hit, miss, accessibility, or geometry expectations.

- [ ] **Step 3: Add controlled real-boundary helpers and a delegating engine**

Add to the existing `mod tests`:

```rust
fn assert_close(actual: f32, expected: f32, context: &str) {
    assert!(
        (actual - expected).abs() < 0.001,
        "{context}: expected {expected}, got {actual}",
    );
}

fn build_measure_draw(
    text: &mut Text,
    theme: &Theme,
    renderer: &mut TextRenderer,
) -> (taffy::Size<f32>, Scene) {
    let mut taffy = TaffyTree::new();
    let mut nodes = std::collections::HashMap::new();
    let _node = Widget::build_layout(text, &mut taffy, theme, &mut nodes);
    let measured = Widget::measure(
        text,
        taffy::Size { width: None, height: None },
        taffy::Size {
            width: taffy::AvailableSpace::MaxContent,
            height: taffy::AvailableSpace::MaxContent,
        },
        renderer,
    );
    text.apply_layout_external(vello::kurbo::Rect::new(0.0, 0.0, 800.0, 200.0));
    let mut scene = Scene::new();
    Widget::draw(
        text,
        &mut scene,
        Affine::IDENTITY,
        renderer,
        theme,
        &crate::presence::PresenceCtx::empty(),
    );
    (measured, scene)
}

fn assert_scene_font_size(scene: &Scene, expected: f32, context: &str) {
    let runs = &scene.encoding().resources.glyph_runs;
    assert!(!runs.is_empty(), "{context}: real draw emitted no glyph runs");
    for run in runs {
        assert_close(run.font_size, expected, context);
        assert!(!run.glyphs.is_empty(), "{context}: emitted an empty run");
    }
}

fn second_glyph_x(scene: &Scene) -> f32 {
    let resources = &scene.encoding().resources;
    let run = resources.glyph_runs.first().expect("one controlled Latin run");
    assert!(run.glyphs.len() >= 2, "AA must emit two glyphs");
    resources.glyphs[run.glyphs.start + 1].x
}

struct DelegatingEngine {
    measured: std::rc::Rc<std::cell::RefCell<Vec<TextStyle>>>,
    drawn: std::rc::Rc<std::cell::RefCell<Vec<TextStyle>>>,
}

impl TextLayoutEngine for DelegatingEngine {
    fn measure(
        &mut self,
        renderer: &mut TextRenderer,
        text: &str,
        constraints: TextLayoutConstraints,
        style: &TextStyle,
    ) -> (f32, f32) {
        self.measured.borrow_mut().push(style.clone());
        renderer.measure_style(text, constraints.max_width, style)
    }

    fn draw(
        &mut self,
        renderer: &mut TextRenderer,
        scene: &mut Scene,
        text: &str,
        constraints: TextLayoutConstraints,
        style: &TextStyle,
        color: vello::peniko::Color,
        transform: Affine,
    ) {
        self.drawn.borrow_mut().push(style.clone());
        renderer.draw_style(scene, text, color, transform, constraints.max_width, style);
    }
}
```

These types are source-verified: `taffy::Size<f32>` is the Widget result; Body is `14.0` with `line_height_normal = 1.4`; `TextStyle::new` uses `line_height = 1.25`; `line_height_px` and measured height are `f32`; `vello_encoding::GlyphRun.glyphs` is `Range<usize>` into `resources.glyphs`; emitted `Glyph.x` is `f32`.

- [ ] **Step 4: Add four regressions/controls at 1×, 1.25×, and 2×**

```rust
#[test]
fn theme_baked_sizes_reach_default_widget_draw_exactly_once() {
    for scale in [1.0, 1.25, 2.0] {
        let theme = Theme::dark().with_text_scale(scale);
        let mut role_renderer = crate::font_catalog::fixture_renderer();
        role_renderer.set_text_scale(scale);
        let mut role = Text::new("Scale probe").with_role(TextRole::Body);
        let (role_measure, role_scene) =
            build_measure_draw(&mut role, &theme, &mut role_renderer);
        assert_close(role.text_style().size, 14.0 * scale, "effective role style");
        assert_close(role_measure.height, 14.0 * 1.4 * scale, "role measure");
        assert_scene_font_size(&role_scene, 14.0 * scale, "role draw");

        let mut scaled_renderer = crate::font_catalog::fixture_renderer();
        scaled_renderer.set_text_scale(scale);
        let mut scaled = Text::new("Scale probe").with_scaled_size(16.0);
        let (scaled_measure, scaled_scene) =
            build_measure_draw(&mut scaled, &theme, &mut scaled_renderer);
        assert_close(scaled.text_style().size, 16.0 * scale, "effective explicit style");
        assert_close(scaled_measure.height, 16.0 * 1.25 * scale, "scaled measure");
        assert_scene_font_size(&scaled_scene, 16.0 * scale, "scaled draw");
    }
}

#[test]
fn pluggable_engine_receives_base_font_size_and_delegates_real_runs() {
    for scale in [1.0, 1.25, 2.0] {
        let measured = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let drawn = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut text = Text::new("Scale probe")
            .with_role(TextRole::Body)
            .with_letter_spacing(2.0)
            .with_layout_engine(DelegatingEngine {
                measured: std::rc::Rc::clone(&measured),
                drawn: std::rc::Rc::clone(&drawn),
            });
        let theme = Theme::dark().with_text_scale(scale);
        let mut renderer = crate::font_catalog::fixture_renderer();
        renderer.set_text_scale(scale);
        let (size, scene) = build_measure_draw(&mut text, &theme, &mut renderer);
        assert_close(size.height, 14.0 * 1.4 * scale, "engine measure");
        assert_scene_font_size(&scene, 14.0 * scale, "engine draw");
        let measured_styles = measured.borrow();
        let drawn_styles = drawn.borrow();
        for style in measured_styles.iter().chain(drawn_styles.iter()) {
            assert_close(style.size, 14.0, "engine renderer-input size");
            assert_close(style.letter_spacing, 2.0, "engine letter spacing");
        }
        assert_eq!(measured_styles.len(), 1);
        assert_eq!(drawn_styles.len(), 1);
    }
}

#[test]
fn base_sizes_and_letter_spacing_remain_renderer_owned() {
    for scale in [1.0, 1.25, 2.0] {
        let theme = Theme::dark().with_text_scale(scale);
        let mut plain_renderer = crate::font_catalog::fixture_renderer();
        plain_renderer.set_text_scale(scale);
        let mut plain = Text::new("AA").with_size(18.0);
        let (plain_measure, plain_scene) =
            build_measure_draw(&mut plain, &theme, &mut plain_renderer);

        let mut spaced_renderer = crate::font_catalog::fixture_renderer();
        spaced_renderer.set_text_scale(scale);
        let mut spaced = Text::new("AA").with_size(18.0).with_letter_spacing(2.0);
        let (spaced_measure, spaced_scene) =
            build_measure_draw(&mut spaced, &theme, &mut spaced_renderer);
        assert_scene_font_size(&plain_scene, 18.0 * scale, "base draw");
        assert_scene_font_size(&spaced_scene, 18.0 * scale, "spaced base draw");
        assert_close(plain_measure.height, 18.0 * 1.25 * scale, "base measure");
        assert_close(spaced_measure.height, plain_measure.height, "spacing height control");
        assert_close(
            second_glyph_x(&spaced_scene) - second_glyph_x(&plain_scene),
            2.0 * scale,
            "letter spacing is scaled once",
        );
    }
}

#[test]
fn observed_theme_scale_invalidates_role_and_base_caches() {
    let initial = Theme::dark();
    let scaled = Theme::dark().with_text_scale(2.0);
    let mut role = Text::new("Scale probe")
        .with_role(TextRole::Body)
        .with_letter_spacing(2.0);
    let mut role_renderer = crate::font_catalog::fixture_renderer();
    let _ = build_measure_draw(&mut role, &initial, &mut role_renderer);
    Widget::mark_drawn(&mut role);
    role_renderer.set_text_scale(2.0);
    let (role_measure, role_scene) =
        build_measure_draw(&mut role, &scaled, &mut role_renderer);
    assert!(Widget::needs_redraw(&role));
    assert!(Widget::is_layout_dirty(&role));
    assert_close(role.text_style().size, 28.0, "public effective style");
    assert_close(role.text_style().letter_spacing, 2.0, "modifier preserved");
    assert_close(role_measure.height, 28.0 * 1.4, "role remeasure");
    assert_scene_font_size(&role_scene, 28.0, "role cache redraw");

    let mut base = Text::new("Base cache").with_size(18.0);
    let mut base_renderer = crate::font_catalog::fixture_renderer();
    let _ = build_measure_draw(&mut base, &initial, &mut base_renderer);
    Widget::mark_drawn(&mut base);
    base_renderer.set_text_scale(2.0);
    let (base_measure, base_scene) =
        build_measure_draw(&mut base, &scaled, &mut base_renderer);
    assert!(Widget::needs_redraw(&base));
    assert!(Widget::is_layout_dirty(&base));
    assert_close(base.text_style().size, 18.0, "public base style");
    assert_close(base_measure.height, 45.0, "base remeasure");
    assert_scene_font_size(&base_scene, 36.0, "base cache redraw");
}
```

The line-height constants are not fixture guesses: Body selects the default type-scale base `14.0` and normal multiplier `1.4`; explicit/default `TextStyle` uses `1.25`; single-line `measure_style` returns `line_count as f32 * style.line_height_px()`. The letter-spacing assertion follows the pinned path: qOS converts px to em in `cosmic_attrs`, `TextRenderer::scaled_style` multiplies both size and spacing once, cosmic-text adds the em spacing to the first glyph advance, and Vello stores the next glyph's `x: f32` in the encoded resource array. For `"AA"`, the second-position delta is therefore `2.0 * scale`.

- [ ] **Step 5: Review the complete test delta, then run one meaningful RED**

```bash
git diff --check
git diff -- ui/src/widget/text.rs
rg -n 'TextRenderer::new\(|(?:crate::headless::)?Driver::new\(' ui/src/widget/text.rs
git status --short ui/src/widget/text.rs ui/assets/fonts ui/src/font_catalog.rs ui/src/text_render.rs ui/src/text_render/renderer_regressions.rs Cargo.toml Cargo.lock
```

Require zero ambient constructors in the Text test module, real `fixture_renderer` use, the six-argument controlled Driver constructor, real renderer delegation, encoded glyph-resource assertions, and no font/renderer/Theme/Cargo changes. Then run the four new tests once:

```bash
PKG_CONFIG_PATH=/opt/homebrew/opt/ffmpeg@8/lib/pkgconfig CARGO_TARGET_DIR=/Users/user/dev/qos/target/macos-shell python3 .superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py cargo nextest run -p ui --lib --locked --profile ci --retries 0 --status-level all --final-status-level all --no-tests fail -E 'test(widget::text::tests::theme_baked_sizes_reach_default_widget_draw_exactly_once) or test(widget::text::tests::pluggable_engine_receives_base_font_size_and_delegates_real_runs) or test(widget::text::tests::base_sizes_and_letter_spacing_remain_renderer_owned) or test(widget::text::tests::observed_theme_scale_invalidates_role_and_base_caches)'
```

Expected: the base/spacing control passes; the default and pluggable tests fail at 1.25/2 with squared sizes; the cache test fails on the squared role run and stale 18px base run. A compile/type/fixture failure is not the intended RED: correct the test source against the verified APIs before implementation.

- [ ] **Step 6: Retain the sanitized Theme factor and normalize only baked size**

Add beside `explicit_base_size` and initialize in `Text::new`:

```rust
    /// Sanitized Theme factor that produced any effective size in `style`.
    /// `measure` has no Theme parameter, so build/draw retain it here.
    observed_theme_text_scale: f32,
```

```rust
            observed_theme_text_scale: 1.0,
```

Add beside `merged_semantic_style`:

```rust
    fn sanitized_theme_text_scale(theme: &Theme) -> f32 {
        if theme.text_scale.is_finite() {
            theme.text_scale.clamp(0.5, 4.0)
        } else {
            1.0
        }
    }

    fn renderer_input_style(&self) -> TextStyle {
        let mut style = self.style.clone();
        if self.follows_role_style || self.scales_with_theme {
            style.size /= self.observed_theme_text_scale;
        }
        style
    }
```

Begin `observe_theme_role` with exact change detection and invalidate every Text, because even a base style changes inside the renderer:

```rust
        let text_scale = Self::sanitized_theme_text_scale(theme);
        if self.observed_theme_text_scale.to_bits() != text_scale.to_bits() {
            self.observed_theme_text_scale = text_scale;
            self.dirty = true;
            self.layout_dirty = true;
        }
```

Use the sanitized local for the existing explicit scaled size:

```rust
                let size = self.explicit_base_size * text_scale;
```

Keep semantic role resolution unchanged so `self.style`/`text_style()` remains effective. Do not divide letter spacing or change modifiers.

- [ ] **Step 7: Pass one renderer-input clone through default and pluggable measure/draw**

Replace the two style arguments in `Text::measure`:

```rust
        let renderer_style = self.renderer_input_style();
        let (w, h) = if let Some(engine) = self.layout_engine.as_deref_mut() {
            engine.measure(text_renderer, &label, constraints, &renderer_style)
        } else {
            text_renderer.measure_style(&label, max_w, &renderer_style)
        };
```

In the dirty cache-rebuild branch of `Text::draw`, derive the clone before borrowing `layout_engine` and pass it to both branches:

```rust
            let renderer_style = self.renderer_input_style();
            if let Some(engine) = self.layout_engine.as_deref_mut() {
                engine.draw(
                    text_renderer,
                    &mut self.cached,
                    &label,
                    constraints,
                    &renderer_style,
                    color,
                    origin,
                );
            } else {
                text_renderer.draw_style(
                    &mut self.cached,
                    &label,
                    color,
                    origin,
                    constraints.max_width,
                    &renderer_style,
                );
            }
```

The owned clone avoids aliasing `self` across the mutable engine/cache borrow. All non-size style fields remain byte-for-byte cloned.

- [ ] **Step 8: Rerun only the newly failing tests for GREEN, then run the final combined population once on Mac**

Rerun the three tests that meaningfully failed; do not repeat the already-green base/spacing control by itself:

```bash
PKG_CONFIG_PATH=/opt/homebrew/opt/ffmpeg@8/lib/pkgconfig CARGO_TARGET_DIR=/Users/user/dev/qos/target/macos-shell python3 .superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py cargo nextest run -p ui --lib --locked --profile ci --retries 0 --status-level all --final-status-level all --no-tests fail -E 'test(widget::text::tests::theme_baked_sizes_reach_default_widget_draw_exactly_once) or test(widget::text::tests::pluggable_engine_receives_base_font_size_and_delegates_real_runs) or test(widget::text::tests::observed_theme_scale_invalidates_role_and_base_caches)'
```

Before the full Text selection, repeat the bounded source audit and require zero `TextRenderer::new`/`Driver::new` matches in `ui/src/widget/text.rs`. Then run Text plus the accepted renderer population exactly once:

```bash
rg -n 'TextRenderer::new\(|(?:crate::headless::)?Driver::new\(' ui/src/widget/text.rs
PKG_CONFIG_PATH=/opt/homebrew/opt/ffmpeg@8/lib/pkgconfig CARGO_TARGET_DIR=/Users/user/dev/qos/target/macos-shell python3 .superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py cargo nextest run -p ui --lib --locked --profile ci --retries 0 --status-level all --final-status-level all --no-tests fail -E 'test(widget::text::tests::) or test(text_render::)'
```

Record discovered/executed counts. Repeat a run only after a new failure-driven source change, and then repeat only the affected new selection before the final combined gate.

- [ ] **Step 9: Run Mac formatting and complete UI all-target compile/lint gates**

```bash
rustfmt --edition 2024 ui/src/widget/text.rs
PKG_CONFIG_PATH=/opt/homebrew/opt/ffmpeg@8/lib/pkgconfig CARGO_TARGET_DIR=/Users/user/dev/qos/target/macos-shell python3 .superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py cargo fmt --all --check
PKG_CONFIG_PATH=/opt/homebrew/opt/ffmpeg@8/lib/pkgconfig CARGO_TARGET_DIR=/Users/user/dev/qos/target/macos-shell python3 .superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py cargo check -p ui --all-targets --locked
PKG_CONFIG_PATH=/opt/homebrew/opt/ffmpeg@8/lib/pkgconfig CARGO_TARGET_DIR=/Users/user/dev/qos/target/macos-shell python3 .superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py cargo clippy -p ui --all-targets --locked -- -D warnings
```

Use only a narrow test-item lint allowance if a concrete diagnostic requires it; do not suppress production/module linting.

- [ ] **Step 10: Review and commit exactly one tracked file**

```bash
git diff --check
git diff --stat
git diff -- ui/src/widget/text.rs
git status --short
git add ui/src/widget/text.rs
git commit -m "fix(ui): apply Text accessibility scale once"
git show --stat --oneline HEAD
```

Require only `ui/src/widget/text.rs` in the commit. Root reviews/adopts after the Notes checkpoint and owns any push.

- [ ] **Step 11: On Fedora, source-audit/list first, then run one final combined population and all-target gates**

At the exact reviewed commit, record `git rev-parse HEAD`, `rustc -vV`, and SHA-256 for `Cargo.lock` and `ui/assets/fonts/manifest.toml`. Review the one-file diff, verify zero ambient constructors, and list the combined selection. Do not run a separate pre-existing renderer baseline.

```bash
CARGO_TARGET_DIR=/home/parallels/qos-target python3 .superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py cargo nextest list -p ui --lib --locked -E 'test(widget::text::tests::) or test(text_render::)'
CARGO_TARGET_DIR=/home/parallels/qos-target python3 .superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py cargo nextest run -p ui --lib --locked --profile ci --retries 0 --status-level all --final-status-level all --no-tests fail -E 'test(widget::text::tests::) or test(text_render::)'
CARGO_TARGET_DIR=/home/parallels/qos-target python3 .superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py cargo fmt --all --check
CARGO_TARGET_DIR=/home/parallels/qos-target python3 .superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py cargo check -p ui --all-targets --locked
CARGO_TARGET_DIR=/home/parallels/qos-target python3 .superpowers/sdd/2026-09-13-wake-scope-integration/run-isolated.py cargo clippy -p ui --all-targets --locked -- -D warnings
```

Preserve Fedora's established FFmpeg pkg-config environment. Require the same SHA, lockfile, font manifest, test names, and nonzero counts as Mac. Repeat only an affected new selection after a real failure-driven change.

- [ ] **Step 12: Hand off evidence and await automatic CI only if root pushes**

Report commit/diffstat, Mac/Fedora toolchains and hashes, RED/GREEN failures/results, final combined discovered/executed counts, and both-host fmt/check/clippy. After a normal push, await the task's automatic final CI status and use `fgj actions` for relevant failures before merge.

## Self-review and source proof

- **Exactly-once formula:** role/scaled effective size is divided by the retained supplied Theme factor and multiplied once by the renderer's requested factor; UI scale stays baked.
- **Measure boundary:** `Widget::build_layout` observes Theme before normal Taffy measurement, while `draw` observes before cache rebuild. Retention solves the absent Theme parameter without changing public traits.
- **Metrics/types:** Body is 14×1.4; default explicit Text is 1.25 leading; measured height is `f32`; encoded run size and glyph `x` are `f32`; run glyph ranges index the real resource array. Intended RED cannot be a type mismatch if copied exactly.
- **Modifier/cache:** only baked size is normalized. Role + letter spacing proves spacing stays renderer-owned. Theme-factor changes dirty role and base caches; `text_style()` remains effective for role/scaled styles and base for explicit styles.
- **Ambient test edges:** the full Text selection's three ambient constructors are explicitly replaced by existing controlled APIs while all original assertions remain.
- **Runtime economy:** accepted source audit plus list replaces redundant 42-test baselines. New tests run once RED, only failures rerun GREEN, and the final combined Text+renderer population runs once per host unless a subsequent source change requires a targeted repeat.
- **Host discipline:** every nextest run has retries/status/final-status flags; Mac uses the known FFmpeg pkg-config path; both hosts run UI all-target fmt/check/clippy at one SHA/lockfile.
- **Scope/output:** one tracked file; no renderer/trait/Theme/default/font/Cargo/golden changes. Identity transform isolates logical scale; native output scale remains separate.
