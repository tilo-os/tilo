#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::type_complexity,
    reason = "pixel geometry over bounded fixture canvases"
)]

//! Text is scaled exactly once.
//!
//! Theme roles are resolved (`TextScaling::Resolved`): they already carry
//! the accessibility text scale and the renderer paints them as given, while
//! a size-based `TextRenderer` API scales its 1× base once. This test pins
//! the widgets to that contract two ways:
//!
//! - through the shared `FrameHarness` (theme *and* renderer scaled, as the
//!   live host does): the label ink of each widget grows by the expected
//!   factor between 1× and 2× text scale — 2× for theme-sized labels, never
//!   4×; and 1× for labels that are sized by their own geometry (an avatar's
//!   initials fill a disc the app laid out, so they must not outgrow it);
//! - for the immediate-mode painters, on the glyph runs they encode: every
//!   run's font size at 2× is exactly the expected multiple of its 1× size.
//!
//! With `TILO_UI_SCREENSHOT_DIR` set, each frame is also written under
//! `text_scaled_once/` there as visual evidence.

use tilo_ui::State;
use tilo_ui::TextRole;
use tilo_ui::headless::{FrameHarness, InspectionNode};
use tilo_ui::text_render::TextRenderer;
use tilo_ui::theme::Theme;
use tilo_ui::widget::{
    Avatar, AvatarSize, Breadcrumb, Chip, ChipLead, ChipStyle, Column, Element, ShortcutRecorder,
};
use vello::Scene;
use vello::kurbo::{Affine, Rect};

struct Capture {
    harness: FrameHarness,
    image: image::RgbaImage,
}

/// Stand `root` up at `scale` with the theme and the renderer scaled together,
/// exactly as `FrameHarness::new_with_theme` does for the live host.
fn capture(name: &str, root: Element, viewport: (u32, u32), scale: f32) -> Capture {
    let harness = FrameHarness::new_with_theme(
        State::new(),
        root,
        viewport,
        Theme::dark().with_text_scale(scale),
    );
    let rgba = harness.render_rgba();
    crate::support::evidence::save_png(
        &format!("text_scaled_once/{name}-{scale}x"),
        viewport.0,
        viewport.1,
        &rgba,
    );
    let image =
        image::RgbaImage::from_raw(viewport.0, viewport.1, rgba).expect("viewport-sized frame");
    Capture { harness, image }
}

fn rect(node: &InspectionNode) -> Rect {
    let [x, y, w, h] = node.rect;
    Rect::new(x, y, x + w, y + h)
}

fn find<'a>(node: &'a InspectionNode, widget: &str) -> &'a InspectionNode {
    fn walk<'a>(node: &'a InspectionNode, widget: &str) -> Option<&'a InspectionNode> {
        if node.widget == widget {
            return Some(node);
        }
        node.children.iter().find_map(|child| walk(child, widget))
    }
    walk(node, widget).unwrap_or_else(|| panic!("no {widget} in the inspection tree"))
}

fn luma(p: [u8; 4]) -> f64 {
    0.2126 * f64::from(p[0]) + 0.7152 * f64::from(p[1]) + 0.0722 * f64::from(p[2])
}

fn pixels(
    image: &image::RgbaImage,
    region: Rect,
) -> impl Iterator<Item = (u32, u32, [u8; 4])> + '_ {
    let x0 = region.x0.max(0.0).floor() as u32;
    let y0 = region.y0.max(0.0).floor() as u32;
    let x1 = (region.x1.ceil() as u32).min(image.width());
    let y1 = (region.y1.ceil() as u32).min(image.height());
    (y0..y1).flat_map(move |y| (x0..x1).map(move |x| (x, y, image.get_pixel(x, y).0)))
}

/// Luma above which a dark-theme pixel is label ink. `text_primary` (~230)
/// and `text_dim` (~160) and the on-fill ink are all above it; the accent
/// pills and discs the labels sit on stay below ~130, the ground below 30,
/// so this floor keeps the capsule, disc and hairline out of the box even
/// where a 2× label overhangs its fixed-height pill onto the ground.
const INK_LUMA: f64 = 140.0;

/// Bounding box of the label ink inside `region`.
fn ink(image: &image::RgbaImage, region: Rect) -> Rect {
    let floor = INK_LUMA;
    let mut bbox: Option<Rect> = None;
    for (x, y, p) in pixels(image, region) {
        if luma(p) > floor {
            let px = Rect::new(
                f64::from(x),
                f64::from(y),
                f64::from(x + 1),
                f64::from(y + 1),
            );
            bbox = Some(bbox.map_or(px, |b| b.union(px)));
        }
    }
    bbox.unwrap_or_else(|| panic!("no ink in {region:?}"))
}

/// Ink grows by `expected` between 1× and 2×: ~2× for theme-sized text (the
/// renderer scales once, so never ~4×), ~1× for geometry-sized text.
///
/// The band is wide because a luma floor under-counts the thin strokes of
/// 12 px glyphs by a pixel or two more than it does 24 px ones; a
/// double-scaled label reads ~4×, far outside either band.
fn assert_ink_scaled(what: &str, base: Rect, large: Rect, expected: f64) {
    let ratio = large.height() / base.height();
    let (lo, hi) = (expected * 0.75, expected * 1.45);
    assert!(
        (lo..=hi).contains(&ratio),
        "{what}: ink height {:.1} -> {:.1} (x{ratio:.2}) must grow by ~{expected}x between 1x and 2x text",
        base.height(),
        large.height()
    );
}

fn column(children: Vec<Element>) -> Element {
    let mut col = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_gap(16.0);
    for child in children {
        col = col.push(child);
    }
    Box::new(col)
}

/// One migrated retained widget per row: the inspection name the harness
/// reports for it, how to build it, and how its label must grow between 1×
/// and 2× text.
fn retained_widgets() -> Vec<(&'static str, fn() -> Element, f64)> {
    vec![
        (
            "Breadcrumb",
            || {
                Box::new(Breadcrumb::new(vec![
                    "Home".into(),
                    "Documents".into(),
                    "Reports".into(),
                ]))
            },
            2.0,
        ),
        (
            "ShortcutRecorder",
            || Box::new(ShortcutRecorder::new("x1", "shortcut", "Ctrl+K", "Ctrl+K")),
            2.0,
        ),
        // Initials are 0.4× the disc the app laid out: geometry, not type.
        (
            "Avatar",
            || Box::new(Avatar::person("Ada Byron", "ada.byron").size(AvatarSize::Regular)),
            1.0,
        ),
    ]
}

#[test]
fn text_scale_is_applied_once_by_retained_immediate_and_renderer_paths() {
    // Case `retained_widget_labels_follow_the_text_scale_once_through_the_harness`.
    {
        for (widget, build, expected) in retained_widgets() {
            let mut label_ink = Vec::new();
            for scale in [1.0_f32, 2.0] {
                let cap = capture(widget, column(vec![build()]), (360, 200), scale);
                let band = rect(find(&cap.harness.inspection().root, widget));
                assert!(
                    band.width() > 0.0 && band.height() > 0.0,
                    "{widget}: empty layout rect {band:?}"
                );
                // A theme-sized label may overhang a fixed-height pill at 2×; the
                // probe follows the ink, not the box.
                let probe = band.inflate(4.0, 12.0);
                label_ink.push(ink(&cap.image, probe));
            }
            assert_ink_scaled(widget, label_ink[0], label_ink[1], expected);
        }
    }

    // Case `immediate_mode_painters_encode_glyphs_at_the_text_scale_once`.
    {
        let painters: Vec<(&str, Box<Painter>, f32)> = vec![
            (
                // Theme-sized label in a pill the host lays out at the control
                // step: the label follows the text scale once.
                "Chip::draw_at",
                Box::new(|scene, tr, theme| {
                    Chip::draw_at(
                        scene,
                        Affine::IDENTITY,
                        Rect::new(0.0, 0.0, 160.0, 28.0),
                        "Design review",
                        ChipLead::None,
                        ChipStyle::default(),
                        theme,
                        tr,
                    );
                }),
                2.0,
            ),
            (
                // The pill sets its label in the theme's `Label` role, so it
                // paints it scaled once.
                "status_bubble::draw_pill",
                Box::new(|scene, tr, theme| {
                    let (w, h) = tilo_ui::status_bubble::pill_size("Syncing", tr, theme, 0.0);
                    tilo_ui::status_bubble::draw_pill(
                        scene,
                        Affine::IDENTITY,
                        tr,
                        theme,
                        Rect::new(0.0, 0.0, w, h),
                        "Syncing",
                    );
                }),
                2.0,
            ),
            (
                // Initials are sized from the disc, so they stay put when only
                // the text scale changes — they used to grow out of it.
                "AvatarFace::paint",
                Box::new(|scene, tr, theme| {
                    tilo_ui::widget::AvatarFace::new("Ada Byron", "ada.byron").paint(
                        &mut tilo_ui::paint::PaintCtx::new(scene, Affine::IDENTITY, tr, theme),
                        Rect::new(0.0, 0.0, 40.0, 40.0),
                    );
                }),
                1.0,
            ),
        ];
        for (what, paint, expected) in &painters {
            assert_runs_scale(what, paint.as_ref(), *expected);
        }
    }

    // Case `a_resolved_role_measures_the_same_at_every_renderer_scale`.
    {
        let style = Theme::dark().with_text_scale(2.0).text(TextRole::Body);
        let mut one = TextRenderer::new();
        let mut two = TextRenderer::new();
        two.set_text_scale(2.0);
        assert_eq!(
            one.measure_style("Scale once", None, &style),
            two.measure_style("Scale once", None, &style)
        );
    }
}

type Painter = dyn Fn(&mut Scene, &mut TextRenderer, &Theme);

/// Font sizes of every glyph run `paint` encodes at `scale`, theme and
/// renderer scaled together.
fn glyph_sizes(scale: f32, paint: &Painter) -> Vec<f32> {
    let theme = Theme::dark().with_text_scale(scale);
    let mut tr = TextRenderer::new();
    tr.set_text_scale(scale);
    let mut scene = Scene::new();
    paint(&mut scene, &mut tr, &theme);
    let sizes: Vec<f32> = scene
        .encoding()
        .resources
        .glyph_runs
        .iter()
        .map(|run| run.font_size)
        .collect();
    assert!(
        !sizes.is_empty(),
        "the painter encoded no glyph runs at {scale}x"
    );
    sizes
}

fn assert_runs_scale(what: &str, paint: &Painter, expected: f32) {
    let base = glyph_sizes(1.0, paint);
    let large = glyph_sizes(2.0, paint);
    assert_eq!(
        base.len(),
        large.len(),
        "{what}: the same text must be encoded at both scales"
    );
    for (b, l) in base.iter().zip(&large) {
        assert!(
            (l - expected * b).abs() < 0.01,
            "{what}: glyph run {b} px at 1x became {l} px at 2x; expected {}",
            expected * b
        );
    }
}
