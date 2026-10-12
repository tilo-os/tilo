//! Regressions for two bugs incremental rendering can reintroduce:
//!
//! 1. `Column`/`Row::draw` rebuild path must recurse
//!    into children at `Affine::IDENTITY`; vello's `Scene::append`
//!    composes the supplied transform onto each stored primitive
//!    transform, so passing the App-level scale here causes a
//!    double-transform when the cache is later appended at the
//!    outer scale, sending widgets off-screen at any DPI ≠ 1.
//!
//! 2. The App's draw path must not push a damage-region clip layer
//!    over the whole widget tree. vello renders to a fresh target
//!    texture every frame, so a clip outside dirty leaves doesn't
//!    save reusable pixels — it just paints those tiles as
//!    background. The visible failure was: on idle animation frames
//!    only spinners/progress bars were visible; static widgets
//!    came back during scroll (which inflated dirty bounds) and
//!    disappeared again on scroll stop.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::collections::HashMap;
use std::time::{Duration, Instant};

use automerge::AutoCommit;
use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use tilo_ui::Widget;
use tilo_ui::i18n::I18nManager;
use tilo_ui::offscreen::OffscreenRenderer;
use tilo_ui::presence::PresenceCtx;
use tilo_ui::text_render::TextRenderer;
use tilo_ui::theme::Theme;
use tilo_ui::widget::toggle_group::Segment;
use tilo_ui::widget::{
    Avatar, AvatarSize, Badge, Button, Column, ProgressBar, Row, ScrollAxis, Scrollable, Spinner,
    Text, ToggleGroup,
};
use vello::Scene;
use vello::kurbo::{Affine, Point};

const ROWS_PER_PANEL: usize = 40;
const FRAME_INTERVAL: Duration = Duration::from_millis(8);

fn leak(s: &str) -> &'static str {
    Box::leak(s.to_string().into_boxed_str())
}

fn panel(title_key: &'static str, idx: usize) -> Box<dyn Widget> {
    let mut col = Column::new().with_intrinsic_height().push(Box::new(
        Text::new(title_key).with_role(tilo_ui::TextRole::Heading),
    ));
    for i in 0..ROWS_PER_PANEL {
        let row_idx = idx.saturating_mul(ROWS_PER_PANEL).saturating_add(i);
        let label_key = leak(&format!("dash_lbl_{row_idx}"));
        match i % 3 {
            0 => {
                col = col.push(Box::new(
                    Row::new()
                        .push(Box::new(Text::new(label_key)))
                        .push(Box::new(Button::new(
                            leak(&format!("dash_btn_{row_idx}")),
                            leak(&format!("dash_btnlbl_{row_idx}")),
                        ))),
                ));
            }
            1 => {
                col = col.push(Box::new(
                    Row::new()
                        .push(Box::new(Text::new(label_key)))
                        .push(Box::new(Badge::count(
                            u32::try_from(i).unwrap().saturating_mul(3),
                        ))),
                ));
            }
            _ => {
                col = col.push(Box::new(
                    Row::new()
                        .push(Box::new(Text::new(label_key)))
                        .push(Box::new(
                            Avatar::person(format!("Teammate {i}"), format!("teammate-{i}"))
                                .size(AvatarSize::Small),
                        )),
                ));
            }
        }
    }
    Box::new(col)
}

fn build_dashboard_root() -> Box<Scrollable> {
    // 1:1 copy of `examples/dashboard.rs`. If you change that file,
    // mirror the change here so the regression coverage stays
    // honest.
    let header = Row::new()
        .push(Box::new(
            Text::new("dash_title").with_role(tilo_ui::TextRole::LargeTitle),
        ))
        .push(Box::new(ToggleGroup::new(
            vec![
                Segment::new("Day"),
                Segment::new("Week"),
                Segment::new("Month"),
            ],
            1,
        )))
        .push(Box::new(ProgressBar::new(0.4)))
        .push(Box::new(ProgressBar::indeterminate()))
        .push(Box::new(ProgressBar::indeterminate()))
        .push(Box::new(Spinner::with_size(24.0)))
        .push(Box::new(Spinner::with_size(24.0)))
        .push(Box::new(Spinner::with_size(24.0)))
        .push(Box::new(Spinner::with_size(24.0)))
        .push(Box::new(Spinner::with_size(24.0)))
        .push(Box::new(Spinner::with_size(24.0)))
        .push(Box::new(Spinner::with_size(24.0)))
        .push(Box::new(Spinner::with_size(24.0)));

    let mut panels = Row::new();
    for i in 0..8 {
        panels = panels.push(panel(leak(&format!("dash_p{i}")), i));
    }

    let footer = Row::new()
        .push(Box::new(Text::new("dash_title")))
        .push(Box::new(
            Avatar::person("Dana Brooks", "dana.brooks").size(AvatarSize::Regular),
        ))
        .push(Box::new(
            Avatar::person("Quinn Daley", "quinn.daley").size(AvatarSize::Regular),
        ))
        .push(Box::new(
            Avatar::person("Ada Lovelace", "ada.lovelace").size(AvatarSize::Regular),
        ))
        .push(Box::new(
            Avatar::person("Priya Bose", "priya.bose").size(AvatarSize::Regular),
        ))
        .push(Box::new(Badge::count(42)))
        .push(Box::new(Badge::count(7)))
        .push(Box::new(Badge::count(120)));

    Box::new(
        Scrollable::new(ScrollAxis::Vertical).with_child(Box::new(
            Column::new()
                .with_intrinsic_height()
                .push(Box::new(header))
                .push(Box::new(panels))
                .push(Box::new(footer)),
        )),
    )
}

fn register_i18n() {
    use std::fmt::Write as _;

    I18nManager::init("en-US");
    let mut ftl = String::new();
    ftl.push_str("dash_title = Tilo Desktop Dashboard\n");
    for i in 0..8 {
        writeln!(ftl, "dash_p{i} = Panel {i}").unwrap();
    }
    for row_idx in 0..(8 * ROWS_PER_PANEL) {
        writeln!(ftl, "dash_lbl_{row_idx} = Metric {row_idx}").unwrap();
        writeln!(ftl, "dash_btn_{row_idx} = btn_{row_idx}").unwrap();
        writeln!(ftl, "dash_btnlbl_{row_idx} = Open").unwrap();
    }
    I18nManager::add_resource("en-US", ftl);
}

/// Single shared init so multiple tests in this file don't race on
/// `I18nManager::init`.
fn ensure_i18n() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(register_i18n);
}

/// Pixels whose RGB differs from `bg` by a summed channel distance above 12.
///
/// Called on every frame of 3.84-megapixel scroll walks. A run of 16 pixels
/// that equals opaque background byte for byte (most of any frame) contains
/// no counted pixel and is rejected with one slice comparison; every other run
/// is measured pixel by pixel.
fn count_non_bg(rgba: &[u8], bg: vello::peniko::Color) -> usize {
    const RUN: usize = 16;
    let [br, bgc, bb, _] = bg.to_rgba8().to_u8_array();
    let (pixels, rest) = rgba.as_chunks::<4>();
    assert!(rest.is_empty(), "RGBA8 frames are whole pixels");
    let background_run = [[br, bgc, bb, u8::MAX]; RUN];
    let mut count = 0_usize;
    for run in pixels.chunks(RUN) {
        if run == &background_run[..run.len()] {
            continue;
        }
        for &[r, g, b, _] in run {
            if u32::from(r.abs_diff(br))
                .saturating_add(u32::from(g.abs_diff(bgc)))
                .saturating_add(u32::from(b.abs_diff(bb)))
                > 12
            {
                count = count.saturating_add(1);
            }
        }
    }
    count
}

struct Harness {
    root: Box<Scrollable>,
    taffy: TaffyTree<()>,
    root_node: NodeId,
    node_map: HashMap<NodeId, *mut dyn Widget>,
    text_renderer: TextRenderer,
    presence: PresenceCtx,
    theme: Theme,
    scene: Scene,
    renderer: OffscreenRenderer,
    logical_w: f64,
    logical_h: f64,
    transform: Affine,
    last_draw: Option<Instant>,
    _doc: AutoCommit,
}

impl Harness {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "logical_w/logical_h are small test canvas dimensions (a few thousand px at \
                  most) divided by a small scale factor; f32 has ample precision at that \
                  magnitude, matching the live App's own f64->f32 compositing cast"
    )]
    fn new(canvas_w: u32, canvas_h: u32, scale: f64) -> Self {
        ensure_i18n();
        let theme = Theme::dark();
        let mut text_renderer = TextRenderer::new();
        let presence = PresenceCtx::default();
        let mut root = build_dashboard_root();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let root_node = {
            let root: &mut dyn Widget = root.as_mut();
            root.build_layout(&mut taffy, &theme, &mut node_map)
        };

        let logical_w = f64::from(canvas_w) / scale;
        let logical_h = f64::from(canvas_h) / scale;

        // Initial layout — matches the live App which only runs
        // compute_layout when `app.layout_dirty` is set.
        taffy
            .compute_layout_with_measure(
                root_node,
                Size {
                    width: AvailableSpace::Definite(logical_w as f32),
                    height: AvailableSpace::Definite(logical_h as f32),
                },
                |known, available, node_id, _, _| {
                    if let Some(&ptr) = node_map.get(&node_id) {
                        unsafe { (*ptr).measure(known, available, &mut text_renderer) }
                    } else {
                        Size::ZERO
                    }
                },
            )
            .unwrap();
        {
            let root: &mut dyn Widget = root.as_mut();
            root.apply_layout(&taffy, root_node, Point::ZERO);
        }

        Self {
            root,
            taffy,
            root_node,
            node_map,
            text_renderer,
            presence,
            theme,
            scene: Scene::new(),
            renderer: OffscreenRenderer::new(canvas_w, canvas_h),
            logical_w,
            logical_h,
            transform: Affine::scale(scale),
            last_draw: None,
            _doc: AutoCommit::new(),
        }
    }

    /// Re-run `compute_layout` + `apply_layout` — the live App
    /// fires this on any frame where `layout_dirty` is set
    /// (scroll, resize, text change, …).
    #[allow(
        clippy::cast_possible_truncation,
        reason = "logical_w/logical_h are small test canvas dimensions (a few thousand px at \
                  most) divided by a small scale factor; f32 has ample precision at that \
                  magnitude, matching the live App's own f64->f32 compositing cast"
    )]
    fn relayout(&mut self) {
        let node_map = &self.node_map;
        let text_renderer = &mut self.text_renderer;
        self.taffy
            .compute_layout_with_measure(
                self.root_node,
                Size {
                    width: AvailableSpace::Definite(self.logical_w as f32),
                    height: AvailableSpace::Definite(self.logical_h as f32),
                },
                |known, available, node_id, _, _| {
                    if let Some(&ptr) = node_map.get(&node_id) {
                        unsafe { (*ptr).measure(known, available, text_renderer) }
                    } else {
                        Size::ZERO
                    }
                },
            )
            .unwrap();
        let root: &mut dyn Widget = self.root.as_mut();
        root.apply_layout(&self.taffy, self.root_node, Point::ZERO);
    }

    fn draw_one_frame(&mut self) -> Vec<u8> {
        // Step spinner phases — they read `Instant::now()` in `draw`, so
        // consecutive draws must be at least one 8 ms frame apart or the
        // animation rate doesn't accumulate. Rendering and pixel counting
        // usually take longer than that already; sleep only for the rest.
        if let Some(previous) = self.last_draw
            && let Some(rest) = FRAME_INTERVAL.checked_sub(previous.elapsed())
        {
            std::thread::sleep(rest);
        }
        self.last_draw = Some(Instant::now());
        self.scene.reset();
        let vp = vello::kurbo::Rect::new(0.0, 0.0, self.logical_w, self.logical_h);
        let prev = self.presence.push_viewport(vp);
        let root: &mut dyn Widget = self.root.as_mut();
        root.draw(
            &mut self.scene,
            self.transform,
            &mut self.text_renderer,
            &self.theme,
            &self.presence,
        );
        self.presence.pop_viewport(prev);
        root.mark_drawn();
        self.renderer
            .render(&self.scene, self.theme.tokens.colors.surfaces.base)
    }
}

#[test]
#[allow(
    clippy::cast_precision_loss,
    reason = "pixel counts are bounded by the test canvas area (at most ~2,000,000), far below \
f64's 2^53 exact-integer range"
)]
fn cached_subtrees_stay_visible_at_scale_and_on_animation_frames() {
    // Case `column_row_subtree_cache_renders_correctly_at_dpi_scale_2`.
    {
        let mut h = Harness::new(1600, 1200, 2.0);

        // Frame 0 — every cache is dirty (cache_rect == Rect::ZERO),
        // so this is the rebuild path. Frame 1 — animated leaves
        // (Spinner/ProgressBar) are still dirty by default and force
        // the outer Column to rebuild, but the panels Row hits the
        // cache. Bug-on-regression: cache primitives would be built
        // with `transform = scale(2)` baked in, then the outer
        // append composes another `transform`, putting them at scale 4
        // off-screen. With the fix, both frames render every widget.
        let rebuild = h.draw_one_frame();
        let cache_hit = h.draw_one_frame();
        let bg = h.theme.tokens.colors.surfaces.base;
        let n_rebuild = count_non_bg(&rebuild, bg);
        let n_cache_hit = count_non_bg(&cache_hit, bg);

        assert!(
            n_rebuild > 1000,
            "rebuild frame has too few non-bg pixels ({n_rebuild}) — widget tree not rendering",
        );
        let ratio = n_cache_hit as f64 / n_rebuild as f64;
        assert!(
            ratio > 0.97,
            "cache-hit frame lost pixels relative to rebuild \
         (cache_hit={n_cache_hit}, rebuild={n_rebuild}, ratio={ratio:.3}); \
         double-transform regression in Column/Row subtree cache?",
        );
    }

    // Case `static_widgets_stay_visible_on_animation_only_frames`.
    {
        let mut h = Harness::new(1600, 1200, 2.0);

        // Frame 0: full repaint (every leaf starts `dirty == true`).
        // Frame 1: only spinners + progress bars remain dirty —
        // exactly the "post-scroll steady state" the user reported
        // broken. If a future change reintroduces an unconditional
        // damage-region clip across the whole `draw`,
        // the second frame will drop all static widgets and this
        // test will fail.
        let _warmup = h.draw_one_frame();
        let steady = h.draw_one_frame();
        let bg = h.theme.tokens.colors.surfaces.base;
        let n_steady = count_non_bg(&steady, bg);

        // Hard floor — the panels alone (4 × 40 rows of Text +
        // Buttons + Avatars + Badges) easily exceed this even after
        // spatial culling clips the off-viewport rows.
        assert!(
            n_steady > 30_000,
            "steady frame has only {n_steady} non-bg pixels; static \
         widgets are being clipped out on animation-only frames \
         (damage-clip regression?)",
        );
    }
}

#[test]
fn scrolled_scrollable_renders_widgets_that_were_offscreen_at_initial_layout() {
    // Focused repro: a Scrollable wrapping a Column with 200 Text
    // rows. Initial layout puts only the top ~25 rows in the
    // 600-px logical viewport. Then we scroll deep, relayout, and
    // assert the now-visible rows render. With the Column/Row
    // subtree cache, if the rebuild path applied spatial culling
    // the cache would only contain rows 0..~25; the post-scroll
    // viewport would be blank.
    ensure_i18n();
    let canvas_w = 800u32;
    let canvas_h = 600u32;
    let scale = 1.0_f64;
    let logical_w = f64::from(canvas_w);
    let logical_h = f64::from(canvas_h);

    let theme = Theme::dark();
    let mut text_renderer = TextRenderer::new();
    let presence = PresenceCtx::default();

    let mut col = Column::new().with_intrinsic_height();
    for i in 0..200 {
        let key = leak(&format!("scroll_row_{i}"));
        I18nManager::add_resource("en-US", format!("{key} = Row {i}\n"));
        col = col.push(Box::new(Text::new(key)));
    }
    let mut root: Box<Scrollable> =
        Box::new(Scrollable::new(ScrollAxis::Vertical).with_child(Box::new(col)));

    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let root_node = {
        let r: &mut dyn Widget = root.as_mut();
        r.build_layout(&mut taffy, &theme, &mut node_map)
    };

    #[allow(
        clippy::cast_possible_truncation,
        reason = "logical_w/logical_h are small test canvas dimensions (a few thousand px at \
                  most); f32 has ample precision at that magnitude, matching the live App's own \
                  f64->f32 compositing cast"
    )]
    let do_layout = |root: &mut Scrollable,
                     taffy: &mut TaffyTree<()>,
                     node_map: &HashMap<NodeId, *mut dyn Widget>,
                     text_renderer: &mut TextRenderer| {
        taffy
            .compute_layout_with_measure(
                root_node,
                Size {
                    width: AvailableSpace::Definite(logical_w as f32),
                    height: AvailableSpace::Definite(logical_h as f32),
                },
                |known, available, node_id, _, _| {
                    if let Some(&ptr) = node_map.get(&node_id) {
                        unsafe { (*ptr).measure(known, available, text_renderer) }
                    } else {
                        Size::ZERO
                    }
                },
            )
            .unwrap();
        let r: &mut dyn Widget = root;
        r.apply_layout(taffy, root_node, Point::ZERO);
    };
    do_layout(&mut root, &mut taffy, &node_map, &mut text_renderer);

    let mut renderer = OffscreenRenderer::new(canvas_w, canvas_h);
    let bg = theme.tokens.colors.surfaces.base;
    let transform = Affine::scale(scale);
    let mut scene = Scene::new();
    let vp = vello::kurbo::Rect::new(0.0, 0.0, logical_w, logical_h);

    let draw = |root: &mut Scrollable,
                scene: &mut Scene,
                text_renderer: &mut TextRenderer,
                presence: &PresenceCtx| {
        scene.reset();
        let prev = presence.push_viewport(vp);
        let r: &mut dyn Widget = root;
        r.draw(scene, transform, text_renderer, &theme, presence);
        presence.pop_viewport(prev);
        r.mark_drawn();
    };

    draw(&mut root, &mut scene, &mut text_renderer, &presence);
    let warm_rgba = renderer.render(&scene, bg);
    let warm_pixels = count_non_bg(&warm_rgba, bg);
    renderer.render_png(&scene, bg, "/tmp/scroll_warmup.png");
    assert!(
        warm_pixels > 1000,
        "warmup frame is blank ({warm_pixels}px) — test setup broken"
    );

    // Scroll to row ~150-ish (each row is ~20 px logical → ~3000 px).
    root.scroll_by(0.0, 3000.0);
    let off = root.offset_y();
    assert!(off > 1000.0, "offset_y={off}, scroll didn't take effect");
    do_layout(&mut root, &mut taffy, &node_map, &mut text_renderer);

    draw(&mut root, &mut scene, &mut text_renderer, &presence);
    let scrolled_rgba = renderer.render(&scene, bg);
    let scrolled_pixels = count_non_bg(&scrolled_rgba, bg);
    renderer.render_png(&scene, bg, "/tmp/scroll_after.png");
    eprintln!("warmup={warm_pixels} scrolled={scrolled_pixels} offset_y={off}");
    assert!(
        scrolled_pixels > 1000,
        "post-scroll frame has only {scrolled_pixels} non-bg pixels — \
         viewport blanked out after scroll; the subtree cache likely \
         captured only the originally-visible primitives.",
    );
}

#[test]
fn scrolling_to_the_end_of_dashboard_still_renders_content() {
    // Walks the full dashboard from offset 0 → max via repeated
    // wheel-sized scrolls (60 px logical per tick — the same step
    // a real mouse wheel produces in `Scrollable::handle_event`).
    // At each step it relayouts, draws, mark_drawns, and counts
    // non-bg pixels. Any frame below the threshold is a
    // regression: the user-reported failure was "scrolled all the
    // way down, suddenly all content vanishes — just background
    // remains". The dashboard tree mirrors `examples/dashboard.rs`
    // exactly (8 panels × 40 rows + header + footer); the canvas
    // matches a HiDPI 800×600 logical window.
    const CANVAS_W: u32 = 1600;
    const CANVAS_H: u32 = 1200;
    let mut h = Harness::new(CANVAS_W, CANVAS_H, 2.0);

    // Warmup + a few animation-only frames so the caches settle.
    for _ in 0..4 {
        let _ = h.draw_one_frame();
    }

    let bg = h.theme.tokens.colors.surfaces.base;
    let mut min_pixels = usize::MAX;
    let mut min_offset = 0.0_f32;
    let mut min_frame = Vec::new();
    let mut last_offset: f32 = -1.0;

    // 250 × 60 px = up to 15,000 px of scroll. The dashboard is
    // ~9,800 px logical so this comfortably hits max_y.
    for step in 0..250 {
        h.root.scroll_by(0.0, 60.0);
        let off = h.root.offset_y();
        h.relayout();
        let rgba = h.draw_one_frame();
        let n = count_non_bg(&rgba, bg);
        if n < min_pixels {
            min_pixels = n;
            min_offset = off;
            min_frame = rgba;
        }
        if (off - last_offset).abs() < 0.5 && step > 5 {
            eprintln!("hit max scroll at step={step} offset_y={off}");
            break;
        }
        last_offset = off;
    }

    eprintln!("min non-bg pixels across scroll: {min_pixels} at offset_y={min_offset}");
    // Encode the worst frame once, rather than every time the minimum drops.
    tilo_ui::png::write_rgba8("/tmp/dash_scroll_min.png", CANVAS_W, CANVAS_H, &min_frame).unwrap();
    assert!(
        min_pixels > 30_000,
        "during scroll, the worst frame (offset_y={min_offset}) \
         dropped to {min_pixels} non-bg pixels — content went blank. \
         PNG: /tmp/dash_scroll_min.png",
    );
}
