//! Regressions for two bugs hidden by the 2026-05-23 incremental
//! rendering work:
//!
//! 1. `Column`/`Row::draw_with_presence` rebuild path must recurse
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

use std::collections::HashMap;
use std::time::Duration;

use automerge::AutoCommit;
use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use ui::Widget;
use ui::i18n::I18nManager;
use ui::offscreen::OffscreenRenderer;
use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::toggle_group::Segment;
use ui::widget::{
    Avatar, Badge, Button, Column, ProgressBar, Row, ScrollAxis, Scrollable, Spinner, Text,
    ToggleGroup,
};
use vello::Scene;
use vello::kurbo::{Affine, Point};

const ROWS_PER_PANEL: usize = 40;

fn leak(s: &str) -> &'static str {
    Box::leak(s.to_string().into_boxed_str())
}

fn panel(title_key: &'static str, idx: usize) -> Box<dyn Widget> {
    let mut col = Column::new()
        .with_intrinsic_height()
        .push(Box::new(Text::new(title_key).with_size(18.0)));
    for i in 0..ROWS_PER_PANEL {
        let row_idx = idx * ROWS_PER_PANEL + i;
        let label_key = leak(&format!("dash_lbl_{row_idx}"));
        match i % 3 {
            0 => col = col.push(Box::new(
                Row::new()
                    .push(Box::new(Text::new(label_key)))
                    .push(Box::new(Button::new(
                        leak(&format!("dash_btn_{row_idx}")),
                        leak(&format!("dash_btnlbl_{row_idx}")),
                        None,
                    ))),
            )),
            1 => col = col.push(Box::new(
                Row::new()
                    .push(Box::new(Text::new(label_key)))
                    .push(Box::new(Badge::count((i as u32) * 3))),
            )),
            _ => col = col.push(Box::new(
                Row::new()
                    .push(Box::new(Text::new(label_key)))
                    .push(Box::new(Avatar::from_initials(&format!("U{i}"), 24.0))),
            )),
        }
    }
    Box::new(col)
}

fn build_dashboard_root() -> Box<dyn Widget> {
    let header = Row::new()
        .push(Box::new(Text::new("dash_title").with_size(24.0)))
        .push(Box::new(ToggleGroup::new(
            vec![Segment::new("Day"), Segment::new("Week"), Segment::new("Month")],
            1,
        )))
        .push(Box::new(ProgressBar::new(0.4)))
        .push(Box::new(ProgressBar::indeterminate()))
        .push(Box::new(Spinner::with_size(24.0)))
        .push(Box::new(Spinner::with_size(24.0)));

    let mut panels = Row::new();
    for i in 0..4 {
        panels = panels.push(panel(leak(&format!("dash_p{i}")), i));
    }

    Box::new(
        Scrollable::new(ScrollAxis::Vertical).with_child(Box::new(
            Column::new()
                .with_intrinsic_height()
                .push(Box::new(header))
                .push(Box::new(panels)),
        )),
    )
}

fn register_i18n() {
    I18nManager::init("en-US");
    let mut ftl = String::new();
    ftl.push_str("dash_title = QuickDesktop Dashboard\n");
    for i in 0..4 {
        ftl.push_str(&format!("dash_p{i} = Panel {i}\n"));
    }
    for row_idx in 0..(4 * ROWS_PER_PANEL) {
        ftl.push_str(&format!("dash_lbl_{row_idx} = Metric {row_idx}\n"));
        ftl.push_str(&format!("dash_btn_{row_idx} = btn_{row_idx}\n"));
        ftl.push_str(&format!("dash_btnlbl_{row_idx} = Open\n"));
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

fn count_non_bg(rgba: &[u8], bg: vello::peniko::Color) -> usize {
    let [br, bgc, bb, _] = bg.to_rgba8().to_u8_array();
    rgba.chunks_exact(4)
        .filter(|p| {
            let dr = p[0] as i32 - br as i32;
            let dg = p[1] as i32 - bgc as i32;
            let db = p[2] as i32 - bb as i32;
            dr.abs() + dg.abs() + db.abs() > 12
        })
        .count()
}

struct Harness {
    root: Box<dyn Widget>,
    text_renderer: TextRenderer,
    presence: PresenceCtx,
    perms: PermissionContext,
    theme: Theme,
    scene: Scene,
    renderer: OffscreenRenderer,
    logical_w: f64,
    logical_h: f64,
    transform: Affine,
    _doc: AutoCommit,
}

impl Harness {
    fn new(canvas_w: u32, canvas_h: u32, scale: f64) -> Self {
        ensure_i18n();
        let theme = Theme::dark();
        let mut text_renderer = TextRenderer::new();
        let presence = PresenceCtx::default();
        let perms = PermissionContext::permissive();
        let mut root = build_dashboard_root();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let root_node = root.build_layout(&mut taffy, &theme, &mut node_map);

        let logical_w = (canvas_w as f64) / scale;
        let logical_h = (canvas_h as f64) / scale;

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
        root.apply_layout(&taffy, root_node, Point::ZERO);

        drop((taffy, root_node, node_map));
        Self {
            root,
            text_renderer,
            presence,
            perms,
            theme,
            scene: Scene::new(),
            renderer: OffscreenRenderer::new(canvas_w, canvas_h),
            logical_w,
            logical_h,
            transform: Affine::scale(scale),
            _doc: AutoCommit::new(),
        }
    }

    fn draw_one_frame(&mut self) -> Vec<u8> {
        self.scene.reset();
        let vp = vello::kurbo::Rect::new(0.0, 0.0, self.logical_w, self.logical_h);
        let prev = self.presence.push_viewport(vp);
        self.root.draw_with_presence(
            &mut self.scene,
            self.transform,
            &mut self.text_renderer,
            &self.theme,
            &self.presence,
            &self.perms,
        );
        self.presence.pop_viewport(prev);
        self.root.mark_drawn();
        // Step spinner phases — they read `Instant::now()` in
        // `draw`, so without a real delay between frames the
        // animation rate doesn't accumulate.
        std::thread::sleep(Duration::from_millis(8));
        self.renderer.render(&self.scene, self.theme.tokens.colors.surfaces.base)
    }
}

#[test]
fn column_row_subtree_cache_renders_correctly_at_dpi_scale_2() {
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

#[test]
fn static_widgets_stay_visible_on_animation_only_frames() {
    let mut h = Harness::new(1600, 1200, 2.0);

    // Frame 0: full repaint (every leaf starts `dirty == true`).
    // Frame 1: only spinners + progress bars remain dirty —
    // exactly the "post-scroll steady state" the user reported
    // broken. If a future change reintroduces an unconditional
    // damage-region clip across the whole `draw_with_presence`,
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
