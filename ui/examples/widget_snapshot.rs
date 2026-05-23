//! Offscreen visual gallery — renders the 16 widgets added by the
//! 2026-05-22 expansion plan into a single PNG via `OffscreenRenderer`,
//! no window opened. Pairs each widget with a label so the snapshot
//! reads as a quick "do they look right?" reference.
//!
//! Run with:
//! ```
//! cargo run -p quick-ui --example widget_snapshot
//! ```
//! The PNG is written to `/tmp/quick-ui-widget-gallery.png`. Override
//! the path with `QUICK_UI_SNAPSHOT_OUT=…`.

use std::collections::HashMap;
use std::sync::Arc;

use automerge::AutoCommit;
use chrono::NaiveDate;
use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{Blob, Color, Fill, Format, Image as PenikoImage};

use ui::offscreen::OffscreenRenderer;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::popover_widget::PopoverSide;
use ui::widget::resizable_split::SplitOrientation;
use ui::widget::toggle_group::Segment;
use ui::widget::toolbar::Orientation as ToolbarOrientation;
use ui::widget::tree_view::TreeNode;
use ui::widget::{
    Avatar, Badge, Breadcrumb, ColorPicker, DatePicker, ImageViewer, ModalDialog,
    PopoverWidget, ProgressBar, ResizableSplit, Spinner, TimePicker, ToggleGroup,
    Toolbar, TreeView, VideoPlayer, Widget,
};

fn label(scene: &mut Scene, tr: &mut TextRenderer, theme: &Theme, x: f64, y: f64, text: &str) {
    tr.draw_text(
        scene,
        text,
        theme.tokens.typography.scale.sm,
        theme.tokens.colors.text_dim,
        Affine::translate((x, y)),
        None,
    );
}

fn stamp(
    scene: &mut Scene,
    tr: &mut TextRenderer,
    theme: &Theme,
    state: &mut AutoCommit,
    widget: &mut dyn Widget,
    origin: Point,
    size: (f32, f32),
) {
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let root = widget.build_layout(&mut taffy, theme, &mut node_map);

    let _ = taffy.compute_layout_with_measure(
        root,
        Size {
            width: AvailableSpace::Definite(size.0),
            height: AvailableSpace::Definite(size.1),
        },
        |known, available, node_id, _, _| {
            if let Some(&ptr) = node_map.get(&node_id) {
                unsafe { (*ptr).measure(known, available, tr) }
            } else {
                Size::ZERO
            }
        },
    );

    widget.apply_layout(&taffy, root, origin);
    widget.sync(state, &automerge::ObjId::Root);
    let presence = PresenceCtx::default();
    widget.draw(scene, Affine::IDENTITY, tr, theme, &presence);
}

struct TinyText {
    text: String,
    rect: Rect,
}
impl TinyText {
    fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            rect: Rect::ZERO,
        }
    }
}
impl Widget for TinyText {
    fn sync(&mut self, _: &mut automerge::AutoCommit, _: &automerge::ObjId) {}
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(taffy::Style {
                size: Size {
                    width: taffy::Dimension::percent(1.0),
                    height: taffy::Dimension::percent(1.0),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }
    fn measure(
        &mut self,
        _known: Size<Option<f32>>,
        _available: Size<AvailableSpace>,
        tr: &mut TextRenderer,
    ) -> Size<f32> {
        let (w, h) = tr.measure(&self.text, 14.0, None);
        Size {
            width: w as f32 + 12.0,
            height: h as f32 + 10.0,
        }
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + l.location.x as f64,
            origin.y + l.location.y as f64,
            origin.x + l.location.x as f64 + l.size.width as f64,
            origin.y + l.location.y as f64 + l.size.height as f64,
        );
    }
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.raised,
            None,
            &self.rect,
        );
        tr.draw_text(
            scene,
            &self.text,
            theme.tokens.typography.scale.sm,
            theme.tokens.colors.text_primary,
            transform * Affine::translate((self.rect.x0 + 8.0, self.rect.y0 + 6.0)),
            None,
        );
    }
    fn handle_event(
        &mut self,
        _event: &winit::event::WindowEvent,
        _scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut automerge::AutoCommit,
        _path: &automerge::ObjId,
        _handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
    }
    fn name(&self) -> &str {
        "TinyText"
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
}

fn placeholder_image() -> PenikoImage {
    let mut buf = vec![0u8; 4 * 16 * 16];
    for y in 0..16 {
        for x in 0..16 {
            let i = (y * 16 + x) * 4;
            let on = ((x / 4) + (y / 4)) % 2 == 0;
            if on {
                buf[i] = 99;
                buf[i + 1] = 102;
                buf[i + 2] = 241;
                buf[i + 3] = 255;
            } else {
                buf[i] = 30;
                buf[i + 1] = 41;
                buf[i + 2] = 59;
                buf[i + 3] = 255;
            }
        }
    }
    PenikoImage::new(Blob::new(Arc::new(buf)), Format::Rgba8, 16, 16)
}

fn main() {
    let out = std::env::var("QUICK_UI_SNAPSHOT_OUT")
        .unwrap_or_else(|_| "/tmp/quick-ui-widget-gallery.png".to_string());
    let (canvas_w, canvas_h) = (1100u32, 1500u32);

    let mut renderer = OffscreenRenderer::new(canvas_w, canvas_h);
    let theme = Theme::dark();
    let mut tr = TextRenderer::new();
    let mut scene = Scene::new();
    let mut doc = AutoCommit::new();

    let col_w = 320.0_f64;
    let col_gap = 40.0_f64;
    let pad = 24.0_f64;
    let cols: [f64; 3] = [pad, pad + col_w + col_gap, pad + 2.0 * (col_w + col_gap)];
    let mut y = pad;
    let row_label_h = 22.0_f64;
    let row_gap = 28.0_f64;

    // Helper: place a single widget at (col_idx, y) with a label.
    let mut place = |scene: &mut Scene,
                     tr: &mut TextRenderer,
                     doc: &mut AutoCommit,
                     col_idx: usize,
                     y_top: f64,
                     caption: &str,
                     widget: &mut dyn Widget,
                     size: (f32, f32)| {
        label(scene, tr, &theme, cols[col_idx], y_top, caption);
        stamp(
            scene,
            tr,
            &theme,
            doc,
            widget,
            Point::new(cols[col_idx], y_top + row_label_h),
            size,
        );
    };

    // --- Row 1: ProgressBar / ProgressBar (indeterminate) / Spinner --------
    let mut pb_det = ProgressBar::new(0.5);
    let mut pb_indet = ProgressBar::indeterminate();
    let mut sp = Spinner::with_size(32.0);
    place(&mut scene, &mut tr, &mut doc, 0, y, "ProgressBar (50%)", &mut pb_det, (col_w as f32, 8.0));
    place(&mut scene, &mut tr, &mut doc, 1, y, "ProgressBar (indeterminate)", &mut pb_indet, (col_w as f32, 8.0));
    place(&mut scene, &mut tr, &mut doc, 2, y, "Spinner (32 px)", &mut sp, (32.0, 32.0));
    y += row_label_h + 32.0 + row_gap;

    // --- Row 2: Badge variants ---------------------------------------------
    let mut b_dot = Badge::dot();
    let mut b_5 = Badge::count(5);
    let mut b_99 = Badge::count(150);
    place(&mut scene, &mut tr, &mut doc, 0, y, "Badge::dot", &mut b_dot, (16.0, 16.0));
    place(&mut scene, &mut tr, &mut doc, 1, y, "Badge::count(5)", &mut b_5, (28.0, 18.0));
    place(&mut scene, &mut tr, &mut doc, 2, y, "Badge::count(150) → '99+'", &mut b_99, (40.0, 18.0));
    y += row_label_h + 24.0 + row_gap;

    // --- Row 3: Avatar variants --------------------------------------------
    let mut av_qd = Avatar::from_initials("QD", 48.0);
    let mut av_al = Avatar::from_initials("AL", 48.0);
    let mut av_img = Avatar::new(placeholder_image(), 48.0);
    place(&mut scene, &mut tr, &mut doc, 0, y, "Avatar 'QD' (initials)", &mut av_qd, (48.0, 48.0));
    place(&mut scene, &mut tr, &mut doc, 1, y, "Avatar 'AL' (initials)", &mut av_al, (48.0, 48.0));
    place(&mut scene, &mut tr, &mut doc, 2, y, "Avatar (image)", &mut av_img, (48.0, 48.0));
    y += row_label_h + 48.0 + row_gap;

    // --- Row 4: Breadcrumb / ToggleGroup / Toolbar -------------------------
    let mut bc = Breadcrumb::new(vec![
        "home".into(),
        "docs".into(),
        "specs".into(),
        "readme.md".into(),
    ]);
    let mut tg = ToggleGroup::new(
        vec![Segment::new("Day"), Segment::new("Week"), Segment::new("Month")],
        1,
    );
    let mut tb = Toolbar::new(ToolbarOrientation::Horizontal)
        .push_button("Cut", || {})
        .push_separator()
        .push_button("Copy", || {})
        .push_button("Paste", || {});
    place(&mut scene, &mut tr, &mut doc, 0, y, "Breadcrumb", &mut bc, (col_w as f32, 24.0));
    place(&mut scene, &mut tr, &mut doc, 1, y, "ToggleGroup (selected=1)", &mut tg, (240.0, 36.0));
    place(&mut scene, &mut tr, &mut doc, 2, y, "Toolbar", &mut tb, (col_w as f32, 36.0));
    y += row_label_h + 36.0 + row_gap;

    // --- Row 5: DatePicker / TimePicker (×2) -------------------------------
    let mut dp = DatePicker::new(NaiveDate::from_ymd_opt(2026, 5, 22).unwrap());
    let mut tp = TimePicker::new(14, 30);
    let mut tps = TimePicker::new(9, 15).with_seconds(45);
    place(&mut scene, &mut tr, &mut doc, 0, y, "DatePicker (2026-05-22)", &mut dp, (224.0, 244.0));
    place(&mut scene, &mut tr, &mut doc, 1, y, "TimePicker (14:30)", &mut tp, (96.0, 72.0));
    place(&mut scene, &mut tr, &mut doc, 2, y, "TimePicker (with seconds)", &mut tps, (140.0, 72.0));
    y += row_label_h + 244.0 + row_gap;

    // --- Row 6: ColorPicker / TreeView / ResizableSplit --------------------
    let mut cp = ColorPicker::new(Color::from_rgba8(99, 102, 241, 255));
    let tree_root = TreeNode::branch(
        1u64,
        "project",
        (),
        vec![
            TreeNode::branch(
                2u64,
                "src",
                (),
                vec![
                    TreeNode::leaf(3u64, "main.rs", ()),
                    TreeNode::leaf(4u64, "lib.rs", ()),
                ],
            ),
            TreeNode::leaf(5u64, "README.md", ()),
        ],
    );
    let mut tv = TreeView::new(tree_root);
    tv.toggle_expand(1);
    tv.toggle_expand(2);
    let mut rs = ResizableSplit::new(
        TinyText::new("left pane"),
        TinyText::new("right pane"),
        SplitOrientation::Horizontal,
    );
    place(&mut scene, &mut tr, &mut doc, 0, y, "ColorPicker (indigo)", &mut cp, (260.0, 220.0));
    place(&mut scene, &mut tr, &mut doc, 1, y, "TreeView (project tree)", &mut tv, (col_w as f32, 160.0));
    place(&mut scene, &mut tr, &mut doc, 2, y, "ResizableSplit (Horizontal)", &mut rs, (col_w as f32, 160.0));
    y += row_label_h + 220.0 + row_gap;

    // --- Row 7: ImageViewer / VideoPlayer ----------------------------------
    let mut iv = ImageViewer::new(placeholder_image());
    let mut vp = VideoPlayer::new();
    place(&mut scene, &mut tr, &mut doc, 0, y, "ImageViewer (16×16 indigo)", &mut iv, (col_w as f32, 160.0));
    place(&mut scene, &mut tr, &mut doc, 1, y, "VideoPlayer (no frame)", &mut vp, (col_w as f32, 160.0));

    // --- Row 7 col 3: PopoverWidget — set anchor_rect and viewport in
    // scene-space so the flip resolver fires inside the intended cell. ----
    {
        let pop_x = cols[2];
        let pop_y = y;
        let pop_w = col_w;
        let pop_h = 240.0_f64;
        label(&mut scene, &mut tr, &theme, pop_x, pop_y, "PopoverWidget (anchored, Below)");
        // Visualise the anchor we're going to attach to so it reads as a
        // real button rather than a phantom region.
        let anchor = Rect::new(
            pop_x + 40.0,
            pop_y + row_label_h + 20.0,
            pop_x + 200.0,
            pop_y + row_label_h + 50.0,
        );
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            theme.tokens.colors.surfaces.raised,
            None,
            &anchor,
        );
        tr.draw_text(
            &mut scene,
            "anchor",
            theme.tokens.typography.scale.sm,
            theme.tokens.colors.text_primary,
            Affine::translate((anchor.x0 + 12.0, anchor.y0 + 8.0)),
            None,
        );
        let viewport = Rect::new(pop_x, pop_y + row_label_h, pop_x + pop_w, pop_y + row_label_h + pop_h);
        let mut popover = PopoverWidget::new(anchor, TinyText::new("Popover content"), PopoverSide::Below)
            .open(true)
            .with_viewport(viewport);
        stamp(
            &mut scene,
            &mut tr,
            &theme,
            &mut doc,
            &mut popover,
            Point::new(pop_x, pop_y + row_label_h),
            (pop_w as f32, pop_h as f32),
        );
    }
    y += row_label_h + 240.0 + row_gap;

    // --- Row 8: ModalDialog (full-width banner at bottom) ------------------
    {
        let modal_x = pad;
        let modal_y = y;
        let modal_w = (canvas_w as f64) - 2.0 * pad;
        let modal_h = 200.0_f64;
        label(&mut scene, &mut tr, &theme, modal_x, modal_y, "ModalDialog (open) — backdrop + centred box");
        let viewport = Rect::new(modal_x, modal_y + row_label_h, modal_x + modal_w, modal_y + row_label_h + modal_h);
        let mut modal = ModalDialog::new(
            "Save changes?",
            TinyText::new("Discard your unsaved edits to widget_snapshot.rs?"),
        )
        .open(true)
        .primary_action("Save", || {})
        .secondary_action("Discard", || {})
        .with_viewport(viewport);
        stamp(
            &mut scene,
            &mut tr,
            &theme,
            &mut doc,
            &mut modal,
            Point::new(modal_x, modal_y + row_label_h),
            (modal_w as f32, modal_h as f32),
        );
    }

    renderer.render_png(&scene, theme.tokens.colors.surfaces.base, &out);
    println!("wrote widget gallery snapshot to {}", out);
}
