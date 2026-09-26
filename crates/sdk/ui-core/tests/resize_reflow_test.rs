//! Resize reflow + shaped-glyph coalescing.
//!
//! The compositor drives a redraw per drag-step while a window is being
//! resized. Two properties keep that smooth and correct, and both are
//! asserted here against the real layout + draw path: **reflow** —
//! recomputing layout at a new available width re-positions content, wraps
//! text and pushes what's below it down, rather than clipping or freezing —
//! and **coalescing** — once a resize settles at a size, further redraws
//! must not re-shape a single label (the shaped-glyph cache turns a held
//! resize into a pure cache hit), while a real width change still MUST
//! re-shape wrapped text so the user never sees stale glyph positions.

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

use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point};

use tilo_ui::i18n::I18nManager;
use tilo_ui::permissions::PermissionContext;
use tilo_ui::presence::PresenceCtx;
use tilo_ui::text_render::TextRenderer;
use tilo_ui::theme::Theme;
use tilo_ui::widget::{Button, Column, Row, Text, Widget};

fn init_i18n() {
    I18nManager::init("en-US");
    I18nManager::add_resource(
        "en-US",
        [
            "para = the quick brown fox jumps over the lazy dog and then keeps on running",
            "seven = 7",
            "eight = 8",
            "nine = 9",
        ]
        .join("\n"),
    );
}

/// Column(intrinsic height) { wrapping Text; Row { three fixed buttons } }.
/// The Text wraps as the width shrinks, which moves the Row below it —
/// a clean, layout-driven reflow signal.
fn build_tree() -> Box<dyn Widget> {
    Box::new(
        Column::new()
            .with_intrinsic_height()
            .push(Box::new(Text::new("para")))
            .push(Box::new(
                Row::new()
                    .push(Box::new(Button::new("b7", "seven", None)))
                    .push(Box::new(Button::new("b8", "eight", None)))
                    .push(Box::new(Button::new("b9", "nine", None))),
            )),
    )
}

fn layout_at(
    root: &mut Box<dyn Widget>,
    taffy: &mut TaffyTree<()>,
    node_map: &HashMap<NodeId, *mut dyn Widget>,
    root_node: NodeId,
    tr: &mut TextRenderer,
    width: f32,
    height: f32,
) {
    let nm = node_map;
    let trr = &mut *tr;
    taffy
        .compute_layout_with_measure(
            root_node,
            Size {
                width: AvailableSpace::Definite(width),
                height: AvailableSpace::Definite(height),
            },
            |known, available, node_id, _, _| {
                if let Some(&ptr) = nm.get(&node_id) {
                    unsafe { (*ptr).measure(known, available, trr) }
                } else {
                    Size::ZERO
                }
            },
        )
        .unwrap();
    root.apply_layout(taffy, root_node, Point::ZERO);
}

/// y0 of the button Row (second child of the Column).
fn row_top(root: &dyn Widget) -> f64 {
    root.children()[1].get_rect().y0
}

struct Harness {
    root: Box<dyn Widget>,
    taffy: TaffyTree<()>,
    node_map: HashMap<NodeId, *mut dyn Widget>,
    root_node: NodeId,
    tr: TextRenderer,
    theme: Theme,
    presence: PresenceCtx,
    perms: PermissionContext,
    scene: Scene,
}

impl Harness {
    fn new() -> Self {
        init_i18n();
        let theme = Theme::dark();
        let mut root = build_tree();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let root_node = root.build_layout(&mut taffy, &theme, &mut node_map);
        Self {
            root,
            taffy,
            node_map,
            root_node,
            tr: TextRenderer::new(),
            theme,
            presence: PresenceCtx::default(),
            perms: PermissionContext::permissive(),
            scene: Scene::new(),
        }
    }

    fn layout(&mut self, w: f32) {
        layout_at(
            &mut self.root,
            &mut self.taffy,
            &self.node_map,
            self.root_node,
            &mut self.tr,
            w,
            2000.0,
        );
    }

    /// One full draw at the current layout; returns `(hits, misses)`
    /// from the shaped-glyph cache for THIS draw only.
    fn draw(&mut self) -> (u64, u64) {
        self.scene.reset();
        self.root.draw_with_presence(
            &mut self.scene,
            Affine::IDENTITY,
            &mut self.tr,
            &self.theme,
            &self.presence,
            &self.perms,
        );
        let (h, m, _ns) = self.tr.take_shape_stats();
        (h, m)
    }
}

#[test]
fn resize_reflows_and_reshapes_only_when_width_changes() {
    // Case `narrowing_window_reflows_content_downward`.
    // Narrowing the window wraps the paragraph and pushes the button row
    // down: content reflows to the new width instead of clipping.
    {
        let mut h = Harness::new();

        h.layout(900.0);
        let wide_row_top = row_top(h.root.as_ref());

        h.layout(160.0);
        let narrow_row_top = row_top(h.root.as_ref());

        assert!(
            narrow_row_top > wide_row_top,
            "narrower width must wrap the paragraph and push the row down \
         (reflow): wide row_top={wide_row_top}, narrow row_top={narrow_row_top}",
        );
    }

    // Case `settled_size_redraws_do_not_reshape`.
    // Redraws at an unchanged size (a held / settled resize, and every
    // static frame) must not reshape a single label — the property that
    // makes a settled resize and idle content cost ~zero text work.
    {
        let mut h = Harness::new();
        h.layout(900.0);

        // First draw warms the shape cache (misses expected).
        let (_h0, m0) = h.draw();
        assert!(m0 > 0, "cold first draw shapes its labels");

        // Every subsequent draw at the SAME size is a pure cache hit.
        for i in 0..4 {
            let (hits, misses) = h.draw();
            assert_eq!(
                misses, 0,
                "settled-size redraw #{i} must not reshape any label",
            );
            assert!(
                hits > 0,
                "settled-size redraw #{i} must reuse cached glyphs"
            );
        }
    }

    // Case `width_change_reshapes_wrapped_text`.
    // When the width actually changes, wrapped text re-shapes so the user
    // never sees stale glyph positions — the cache adapts, it does not
    // pin old geometry.
    {
        let mut h = Harness::new();

        h.layout(900.0);
        let _ = h.draw(); // warm at wide width
        let (_h_settled, m_settled) = h.draw();
        assert_eq!(m_settled, 0, "wide width is settled before the resize");

        // Drag the window narrower: the paragraph's wrap width changed, so
        // its shaped run must be recomputed (miss), proving reflow reaches
        // the glyph layer and isn't served stale from the cache.
        h.layout(160.0);
        let (_hits, misses) = h.draw();
        assert!(
            misses > 0,
            "a real width change must reshape the wrapped paragraph",
        );
    }
}
