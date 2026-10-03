//! Offscreen visual gallery — renders the 16 widgets added by the
//! 2026-05-22 expansion plan into a single PNG via `OffscreenRenderer`,
//! no window opened. Pairs each widget with a label so the snapshot
//! reads as a quick "do they look right?" reference.
//!
//! Run with:
//! ```
//! cargo run -p ui-examples --example ui-shots -- widget_snapshot
//! ```
//! The PNG is written to `/tmp/quick-ui-widget-gallery.png`. Override
//! the path with `TILO_UI_SNAPSHOT_OUT=…`.

use std::collections::HashMap;
use std::sync::Arc;

use automerge::AutoCommit;
use chrono::NaiveDate;
use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{
    Blob, Color, Fill, ImageAlphaType, ImageData as PenikoImage, ImageFormat as Format,
};

use tilo_ui::offscreen::OffscreenRenderer;
use tilo_ui::presence::PresenceCtx;
use tilo_ui::text_render::TextRenderer;
use tilo_ui::theme::Theme;
use tilo_ui::widget::popover_widget::PopoverSide;
use tilo_ui::widget::resizable_split::SplitOrientation;
use tilo_ui::widget::toggle_group::Segment;
use tilo_ui::widget::toolbar::Orientation as ToolbarOrientation;
use tilo_ui::widget::tree_view::TreeNode;
use tilo_ui::widget::{
    Avatar, Badge, Breadcrumb, ColorPicker, DatePicker, ImageViewer, ModalDialog, PopoverWidget,
    ProgressBar, ResizableSplit, Spinner, TimePicker, ToggleGroup, Toolbar, TreeView, VideoPlayer,
    Widget,
};

/// Narrows a small gallery layout dimension (well under f32's 2^24
/// exact-integer range) from `f64` to `f32` for a widget's `(w, h)` size.
#[allow(
    clippy::cast_possible_truncation,
    reason = "gallery layout dimensions are small (<2000px), far below f32's exact-integer range"
)]
fn px32(v: f64) -> f32 {
    v as f32
}

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
    // A leaf that fills its slot with `flex_grow` (VideoPlayer, ImageViewer)
    // has no flex parent here, so it would resolve to a zero rect and paint
    // its chrome at the cell origin. Give such a root the cell itself, as a
    // parent Row or Column would.
    if let Ok(style) = taffy.style(root) {
        let fills_slot = style.flex_grow > 0.0
            && style.size.width.into_option().is_none()
            && style.size.height.into_option().is_none();
        if fills_slot {
            let mut style = style.clone();
            style.size = Size {
                width: taffy::style_helpers::length(size.0),
                height: taffy::style_helpers::length(size.1),
            };
            let _ = taffy.set_style(root, style);
        }
    }

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
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // `new_leaf` only fails for a child `NodeId` belonging to a
        // different tree, which can't happen for a freshly created leaf
        // with no children.
        #[allow(
            clippy::unwrap_used,
            reason = "new_leaf cannot fail for a childless leaf node"
        )]
        let node = taffy
            .new_leaf(taffy::Style {
                size: Size {
                    width: taffy::Dimension::percent(1.0),
                    height: taffy::Dimension::percent(1.0),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
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
            width: w + 12.0,
            height: h + 10.0,
        }
    }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
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
        // kurbo::Affine * Affine composes two 2D transforms via f64 matrix
        // math; it never panics or overflows, unlike integer +/-/*.
        #[allow(
            clippy::arithmetic_side_effects,
            reason = "Affine * Affine matrix composition"
        )]
        let text_transform =
            transform * Affine::translate((self.rect.x0 + 8.0, self.rect.y0 + 6.0));
        tr.draw_text(
            scene,
            &self.text,
            theme.tokens.typography.scale.sm,
            theme.tokens.colors.text_primary,
            text_transform,
            None,
        );
    }
    fn name(&self) -> &'static str {
        "TinyText"
    }
    fn get_rect(&self) -> Rect {
        self.rect
    }
}

fn placeholder_image() -> PenikoImage {
    let mut buf = vec![0u8; 4 * 16 * 16];
    for (idx, pixel) in buf.chunks_exact_mut(4).enumerate() {
        let x = idx % 16;
        let y = idx / 16;
        let on = (x / 4).saturating_add(y / 4) % 2 == 0;
        let color: [u8; 4] = if on {
            [99, 102, 241, 255]
        } else {
            [30, 41, 59, 255]
        };
        pixel.copy_from_slice(&color);
    }
    PenikoImage {
        data: Blob::new(Arc::new(buf)),
        format: Format::Rgba8,
        alpha_type: ImageAlphaType::Alpha,
        width: 16,
        height: 16,
    }
}

/// Shared draw target plus gallery layout constants, threaded through every
/// `draw_rowN` helper below — bundled into one struct so `run` stays well
/// under clippy's line-count limit.
struct GalleryCtx<'a> {
    scene: &'a mut Scene,
    tr: &'a mut TextRenderer,
    doc: &'a mut AutoCommit,
    theme: &'a Theme,
    cols: [f64; 3],
    row_label_h: f64,
    row_gap: f64,
    col_w: f64,
}

/// Place a single widget at `(col_idx, y_top)` with a label above it.
fn place(
    ctx: &mut GalleryCtx<'_>,
    col_idx: usize,
    y_top: f64,
    caption: &str,
    widget: &mut dyn Widget,
    size: (f32, f32),
) {
    let x = ctx.cols.get(col_idx).copied().unwrap_or(0.0);
    label(ctx.scene, ctx.tr, ctx.theme, x, y_top, caption);
    stamp(
        ctx.scene,
        ctx.tr,
        ctx.theme,
        ctx.doc,
        widget,
        Point::new(x, y_top + ctx.row_label_h),
        size,
    );
}

/// Row 1: `ProgressBar` / `ProgressBar` (indeterminate) / `Spinner`.
fn draw_row1(ctx: &mut GalleryCtx<'_>, y: f64) -> f64 {
    let mut pb_det = ProgressBar::new(0.5);
    let mut pb_indet = ProgressBar::indeterminate();
    let mut sp = Spinner::with_size(32.0);
    let col_w = ctx.col_w;
    place(
        ctx,
        0,
        y,
        "ProgressBar (50%)",
        &mut pb_det,
        (px32(col_w), 8.0),
    );
    place(
        ctx,
        1,
        y,
        "ProgressBar (indeterminate)",
        &mut pb_indet,
        (px32(col_w), 8.0),
    );
    place(ctx, 2, y, "Spinner (32 px)", &mut sp, (32.0, 32.0));
    y + ctx.row_label_h + 32.0 + ctx.row_gap
}

/// Row 2: `Badge` variants.
fn draw_row2(ctx: &mut GalleryCtx<'_>, y: f64) -> f64 {
    let mut b_dot = Badge::dot();
    let mut b_5 = Badge::count(5);
    let mut b_99 = Badge::count(150);
    place(ctx, 0, y, "Badge::dot", &mut b_dot, (16.0, 16.0));
    place(ctx, 1, y, "Badge::count(5)", &mut b_5, (28.0, 18.0));
    place(
        ctx,
        2,
        y,
        "Badge::count(150) → '99+'",
        &mut b_99,
        (40.0, 18.0),
    );
    y + ctx.row_label_h + 24.0 + ctx.row_gap
}

/// Row 3: `Avatar` variants.
fn draw_row3(ctx: &mut GalleryCtx<'_>, y: f64) -> f64 {
    let mut av_qd = Avatar::from_initials("QD", 48.0);
    let mut av_al = Avatar::from_initials("AL", 48.0);
    let mut av_img = Avatar::new(placeholder_image(), 48.0);
    place(
        ctx,
        0,
        y,
        "Avatar 'QD' (initials)",
        &mut av_qd,
        (48.0, 48.0),
    );
    place(
        ctx,
        1,
        y,
        "Avatar 'AL' (initials)",
        &mut av_al,
        (48.0, 48.0),
    );
    place(ctx, 2, y, "Avatar (image)", &mut av_img, (48.0, 48.0));
    y + ctx.row_label_h + 48.0 + ctx.row_gap
}

/// Row 4: `Breadcrumb` / `ToggleGroup` / `Toolbar`.
fn draw_row4(ctx: &mut GalleryCtx<'_>, y: f64) -> f64 {
    let mut bc = Breadcrumb::new(vec![
        "home".into(),
        "docs".into(),
        "specs".into(),
        "readme.md".into(),
    ]);
    let mut tg = ToggleGroup::new(
        vec![
            Segment::new("Day"),
            Segment::new("Week"),
            Segment::new("Month"),
        ],
        1,
    );
    let mut tb = Toolbar::new(ToolbarOrientation::Horizontal)
        .push_button("Cut", || {})
        .push_separator()
        .push_button("Copy", || {})
        .push_button("Paste", || {});
    let col_w = ctx.col_w;
    place(ctx, 0, y, "Breadcrumb", &mut bc, (px32(col_w), 24.0));
    place(
        ctx,
        1,
        y,
        "ToggleGroup (selected=1)",
        &mut tg,
        (240.0, 36.0),
    );
    place(ctx, 2, y, "Toolbar", &mut tb, (px32(col_w), 36.0));
    y + ctx.row_label_h + 36.0 + ctx.row_gap
}

/// Row 5: `DatePicker` / `TimePicker` (×2).
fn draw_row5(ctx: &mut GalleryCtx<'_>, y: f64) -> f64 {
    // 2026-05-22 is a hardcoded, always-valid calendar date.
    #[allow(
        clippy::unwrap_used,
        reason = "2026-05-22 is a literal, always-valid date"
    )]
    let mut dp = DatePicker::new(NaiveDate::from_ymd_opt(2026, 5, 22).unwrap());
    let mut tp = TimePicker::new(14, 30);
    let mut tps = TimePicker::new(9, 15).with_seconds(45);
    place(
        ctx,
        0,
        y,
        "DatePicker (2026-05-22)",
        &mut dp,
        (224.0, 244.0),
    );
    place(ctx, 1, y, "TimePicker (14:30)", &mut tp, (96.0, 72.0));
    place(
        ctx,
        2,
        y,
        "TimePicker (with seconds)",
        &mut tps,
        (140.0, 72.0),
    );
    y + ctx.row_label_h + 244.0 + ctx.row_gap
}

/// Row 6: `ColorPicker` / `TreeView` / `ResizableSplit`.
fn draw_row6(ctx: &mut GalleryCtx<'_>, y: f64) -> f64 {
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
    let col_w = ctx.col_w;
    place(ctx, 0, y, "ColorPicker (indigo)", &mut cp, (260.0, 220.0));
    place(
        ctx,
        1,
        y,
        "TreeView (project tree)",
        &mut tv,
        (px32(col_w), 160.0),
    );
    place(
        ctx,
        2,
        y,
        "ResizableSplit (Horizontal)",
        &mut rs,
        (px32(col_w), 160.0),
    );
    y + ctx.row_label_h + 220.0 + ctx.row_gap
}

/// Row 7: `ImageViewer` / `VideoPlayer` / `PopoverWidget` (the popover sets
/// its own anchor rect and viewport in scene-space, so the flip resolver
/// fires inside the intended cell rather than going through `place`).
fn draw_row7(ctx: &mut GalleryCtx<'_>, y: f64) -> f64 {
    let mut iv = ImageViewer::new(placeholder_image());
    let mut vp = VideoPlayer::new();
    let col_w = ctx.col_w;
    place(
        ctx,
        0,
        y,
        "ImageViewer (16×16 indigo)",
        &mut iv,
        (px32(col_w), 160.0),
    );
    place(
        ctx,
        1,
        y,
        "VideoPlayer (no frame)",
        &mut vp,
        (px32(col_w), 160.0),
    );

    let pop_x = ctx.cols[2];
    let pop_y = y;
    let pop_w = ctx.col_w;
    let pop_h = 240.0_f64;
    let row_label_h = ctx.row_label_h;
    label(
        ctx.scene,
        ctx.tr,
        ctx.theme,
        pop_x,
        pop_y,
        "PopoverWidget (anchored, Below)",
    );
    // Visualise the anchor we're going to attach to so it reads as a
    // real button rather than a phantom region.
    let anchor = Rect::new(
        pop_x + 40.0,
        pop_y + row_label_h + 20.0,
        pop_x + 200.0,
        pop_y + row_label_h + 50.0,
    );
    ctx.scene.fill(
        Fill::NonZero,
        Affine::IDENTITY,
        ctx.theme.tokens.colors.surfaces.raised,
        None,
        &anchor,
    );
    ctx.tr.draw_text(
        ctx.scene,
        "anchor",
        ctx.theme.tokens.typography.scale.sm,
        ctx.theme.tokens.colors.text_primary,
        Affine::translate((anchor.x0 + 12.0, anchor.y0 + 8.0)),
        None,
    );
    let viewport = Rect::new(
        pop_x,
        pop_y + row_label_h,
        pop_x + pop_w,
        pop_y + row_label_h + pop_h,
    );
    let mut popover =
        PopoverWidget::new(anchor, TinyText::new("Popover content"), PopoverSide::Below)
            .open(true)
            .with_viewport(viewport);
    stamp(
        ctx.scene,
        ctx.tr,
        ctx.theme,
        ctx.doc,
        &mut popover,
        Point::new(pop_x, pop_y + row_label_h),
        (px32(pop_w), px32(pop_h)),
    );
    y + ctx.row_label_h + 240.0 + ctx.row_gap
}

/// Row 8: `ModalDialog`, a full-width banner at the bottom of the gallery.
fn draw_row8_modal(ctx: &mut GalleryCtx<'_>, pad: f64, canvas_w: u32, y: f64) {
    let modal_x = pad;
    let modal_y = y;
    let modal_w = f64::from(canvas_w) - 2.0 * pad;
    let modal_h = 200.0_f64;
    let row_label_h = ctx.row_label_h;
    label(
        ctx.scene,
        ctx.tr,
        ctx.theme,
        modal_x,
        modal_y,
        "ModalDialog (open) — backdrop + centred box",
    );
    let viewport = Rect::new(
        modal_x,
        modal_y + row_label_h,
        modal_x + modal_w,
        modal_y + row_label_h + modal_h,
    );
    let mut modal = ModalDialog::new(
        "Save changes?",
        TinyText::new("Discard your unsaved edits to widget_snapshot.rs?"),
    )
    .open(true)
    .primary_action("Save", || {})
    .secondary_action("Discard", || {})
    .with_viewport(viewport);
    stamp(
        ctx.scene,
        ctx.tr,
        ctx.theme,
        ctx.doc,
        &mut modal,
        Point::new(modal_x, modal_y + row_label_h),
        (px32(modal_w), px32(modal_h)),
    );
}

pub fn run(_args: &[String]) {
    let out = tilo_ui::env::var("TILO_UI_SNAPSHOT_OUT")
        .unwrap_or_else(|_| "/tmp/quick-ui-widget-gallery.png".to_string());
    let (canvas_w, canvas_h) = (1100u32, 1500u32);

    let mut renderer = OffscreenRenderer::new(canvas_w, canvas_h);
    // `TILO_SHOTS_THEME=light|dark|high-contrast` and `TILO_SHOTS_TEXT_SCALE`
    // are the switches every other capture honours; default stays dark at
    // 1×, and motion is reduced like a real capture so nothing mid-animation
    // lands in the still.
    let text_scale = std::env::var("TILO_SHOTS_TEXT_SCALE").ok();
    let theme = std::env::var("TILO_SHOTS_THEME")
        .ok()
        .and_then(|name| Theme::named(&name))
        .unwrap_or_else(Theme::dark)
        .with_text_scale(tilo_ui::launch::shot_text_scale(
            None,
            text_scale.as_deref(),
        ))
        .with_reduce_motion(true);
    let mut tr = TextRenderer::new();
    let mut scene = Scene::new();
    let mut doc = AutoCommit::new();

    let col_w = 320.0_f64;
    let col_gap = 40.0_f64;
    let pad = 24.0_f64;
    let cols: [f64; 3] = [pad, pad + col_w + col_gap, pad + 2.0 * (col_w + col_gap)];
    let row_label_h = 22.0_f64;
    let row_gap = 28.0_f64;

    let mut ctx = GalleryCtx {
        scene: &mut scene,
        tr: &mut tr,
        doc: &mut doc,
        theme: &theme,
        cols,
        row_label_h,
        row_gap,
        col_w,
    };

    let mut y = pad;
    y = draw_row1(&mut ctx, y);
    y = draw_row2(&mut ctx, y);
    y = draw_row3(&mut ctx, y);
    y = draw_row4(&mut ctx, y);
    y = draw_row5(&mut ctx, y);
    y = draw_row6(&mut ctx, y);
    y = draw_row7(&mut ctx, y);
    draw_row8_modal(&mut ctx, pad, canvas_w, y);

    renderer.render_png(&scene, theme.tokens.colors.surfaces.base, &out);
    println!("wrote widget gallery snapshot to {out}");
}
