#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "in a test these are the assertion, not a hazard; pixel probes round f64 rects to u32"
)]

use std::collections::HashMap;

use taffy::{AvailableSpace, Dimension, Style};
use taffy::{NodeId, Size, TaffyTree};
use tilo_ui::TextRole;
use tilo_ui::offscreen::OffscreenRenderer;
use tilo_ui::overlay::{FocusTrap, Modal, OverlayStack, Popover};
use tilo_ui::presence::PresenceCtx;
use tilo_ui::text_render::TextRenderer;
use tilo_ui::theme::Theme;
use tilo_ui::widget::permission_prompt::{PROMPT_HEIGHT, PermissionPrompt};
use tilo_ui::widget::popover_widget::PopoverSide;
use tilo_ui::widget::{ModalDialog, PopoverWidget, Text, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Vec2};
use vello::peniko::Color;

#[test]
fn overlay_stack_orders_layers_and_traps_focus() {
    // Case `overlay_stack_push_pop_order`.
    {
        let mut s = OverlayStack::new();
        let a = s.push_modal(Modal::new("a")).expect("initial ID");
        let b = s
            .push_popover(Popover::new("b", Point::new(100.0, 100.0)))
            .expect("second ID");
        assert_eq!(s.len(), 2);
        assert_eq!(s.top_id(), Some(b));
        s.dismiss(a);
        assert_eq!(s.len(), 1);
        assert_eq!(s.top_id(), Some(b));
    }

    // Case `focus_trap_cycles_only_within_members`.
    {
        let trap = FocusTrap::new(vec![1, 2, 3]);
        assert_eq!(trap.next_from(1), Some(2));
        assert_eq!(trap.next_from(3), Some(1));
        assert_eq!(trap.prev_from(1), Some(3));
    }
}

// ---------------------------------------------------------------------------
// Rest edges of the two overlay Panels (HIG §9.5), rendered at 1× on the dark
// and the light preset. A Panel's edge is derived against the ground and has
// to visibly move against the panel's own fill; the declared `border_standard`
// both used to stroke sat within rounding distance of `raised` on the dark
// preset and read as no edge at all. `TILO_UI_SCREENSHOT_DIR` dumps
// `modal_dialog_{dark,light}.png` and `popover_widget_{dark,light}.png`.
// ---------------------------------------------------------------------------

const CANVAS_W: u32 = 640;
const CANVAS_H: u32 = 420;

fn luminance_of(px: (u8, u8, u8, u8)) -> f64 {
    tilo_ui::a11y::relative_luminance(Color::from_rgba8(px.0, px.1, px.2, px.3))
}

fn dump(name: &str, rgba: &[u8]) {
    let Ok(dir) = tilo_ui::env::var("TILO_UI_SCREENSHOT_DIR") else {
        return;
    };
    std::fs::create_dir_all(&dir).unwrap();
    let path = format!("{dir}/{name}.png");
    tilo_ui::png::write_rgba8(&path, CANVAS_W, CANVAS_H, rgba).unwrap();
    eprintln!("wrote {path}");
}

/// The edge pixel one step inside `panel`'s left side, mid-height, against
/// the fill six pixels further in — clear of the corner radius and of any
/// title text.
fn assert_edge_registers(rgba: &[u8], panel: Rect, name: &str, surface: &str) {
    let y = panel.center().y.round() as u32;
    let x_edge = panel.x0.round() as u32;
    let edge = OffscreenRenderer::sample(rgba, CANVAS_W, x_edge, y);
    let inside = OffscreenRenderer::sample(rgba, CANVAS_W, x_edge.saturating_add(6), y);
    assert_eq!(edge.3, 0xff, "{surface}/{name}: edge pixel is opaque");
    assert_eq!(inside.3, 0xff, "{surface}/{name}: fill pixel is opaque");
    let delta = (luminance_of(edge) - luminance_of(inside)).abs();
    eprintln!("{surface}/{name}: edge {edge:?} vs fill {inside:?} → Δlum={delta:.4}");
    assert!(
        delta >= tilo_ui::tokens::colors::HAIRLINE_MIN_DELTA,
        "{surface}/{name}: edge {edge:?} does not register against fill {inside:?} \
         (Δlum={delta:.4} < {})",
        tilo_ui::tokens::colors::HAIRLINE_MIN_DELTA
    );
}

fn themes() -> [(&'static str, Theme); 2] {
    [("dark", Theme::dark()), ("light", Theme::light())]
}

/// A fixed-size popover body that paints one label. `PopoverWidget` sizes
/// its panel from `Widget::measure`, and the bare taffy pass used here has
/// no text measure, so a `Text` alone would collapse the panel around its
/// glyphs.
struct FixedContent {
    rect: Rect,
    size: taffy::Size<f32>,
    label: &'static str,
}

impl FixedContent {
    fn new(w: f32, h: f32, label: &'static str) -> Self {
        Self {
            rect: Rect::ZERO,
            size: taffy::Size {
                width: w,
                height: h,
            },
            label,
        }
    }
}

impl Widget for FixedContent {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::length(self.size.width),
                    height: Dimension::length(self.size.height),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn measure(
        &mut self,
        _known: taffy::Size<Option<f32>>,
        _available: taffy::Size<AvailableSpace>,
        _text_renderer: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        self.size
    }

    fn apply_layout(&mut self, _taffy: &TaffyTree<()>, _node: NodeId, origin: Point) {
        self.rect = Rect::new(
            origin.x,
            origin.y,
            origin.x + f64::from(self.size.width),
            origin.y + f64::from(self.size.height),
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let style = theme.text(TextRole::Caption);
        let (_, h) = text_renderer.measure_style(self.label, None, &style);
        let y = self.rect.y0 + (self.rect.height() - f64::from(h)) / 2.0;
        text_renderer.draw_style(
            scene,
            self.label,
            theme.tokens.colors.text_primary,
            transform.pre_translate(Vec2::new(self.rect.x0, y)),
            None,
            &style,
        );
    }

    fn name(&self) -> &'static str {
        "FixedContent"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

fn lay_out(widget: &mut dyn Widget, theme: &Theme) {
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let node = widget.build_layout(&mut taffy, theme, &mut map);
    taffy.compute_layout(node, Size::max_content()).unwrap();
    widget.apply_layout(&taffy, node, Point::ZERO);
}

#[test]
fn overlay_panels_register_their_edge_in_both_themes() {
    // Case `modal_dialog_panel_edge_registers_in_both_themes`.
    {
        let viewport = Rect::new(0.0, 0.0, f64::from(CANVAS_W), f64::from(CANVAS_H));
        for (name, theme) in themes() {
            let mut dialog = ModalDialog::new(
                "Discard changes?",
                Text::new("Your edits to the shared page will be lost."),
            )
            .open(true)
            .with_viewport(viewport);
            let mut tr = TextRenderer::new();
            dialog.layout_in(viewport, &theme, &mut tr);
            let panel = dialog.get_rect();
            assert!(panel.width() > 100.0, "{name}: dialog laid out ({panel:?})");

            let mut renderer = OffscreenRenderer::new(CANVAS_W, CANVAS_H);
            let mut scene = Scene::new();
            let presence = PresenceCtx::default();
            dialog.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
            let rgba = renderer.render(&scene, theme.tokens.colors.surfaces.content);
            dump(&format!("modal_dialog_{name}"), &rgba);
            assert_edge_registers(&rgba, panel, name, "modal-dialog");
        }
    }

    // Case `popover_widget_panel_edge_registers_in_both_themes`.
    {
        let viewport = Rect::new(0.0, 0.0, f64::from(CANVAS_W), f64::from(CANVAS_H));
        let anchor = Rect::new(240.0, 60.0, 400.0, 92.0);
        for (name, theme) in themes() {
            let content = FixedContent::new(200.0, 40.0, "Shared with 3 collaborators");
            let mut popover = PopoverWidget::new(anchor, content, PopoverSide::Below)
                .open(true)
                .with_viewport(viewport);
            lay_out(&mut popover, &theme);
            let mut tr = TextRenderer::new();
            let mut renderer = OffscreenRenderer::new(CANVAS_W, CANVAS_H);
            let mut scene = Scene::new();
            let presence = PresenceCtx::default();
            popover.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
            let panel = popover.get_rect();
            assert!(panel.width() > 40.0, "{name}: popover laid out ({panel:?})");
            let rgba = renderer.render(&scene, theme.tokens.colors.surfaces.content);
            dump(&format!("popover_widget_{name}"), &rgba);
            assert_edge_registers(&rgba, panel, name, "popover-widget");
        }
    }

    // Case `permission_prompt_panel_edge_registers_in_both_themes`.
    // The permission prompt is a Panel too (HIG §9.5): its edge derives against
    // the ground and has to register against the prompt's `surfaces.overlay`
    // fill in both themes. `TILO_UI_SCREENSHOT_DIR` dumps
    // `permission_prompt_{dark,light}.png`.
    {
        // Wide enough for the label and both consent buttons; the height is the
        // prompt's own fixed band so the probe lands on its centre line.
        let panel = Rect::new(20.0, 40.0, 620.0, 40.0 + PROMPT_HEIGHT);
        for (name, theme) in themes() {
            let mut prompt = PermissionPrompt::new(
                "org.tilo.Photos".to_string(),
                "read your Pictures folder".to_string(),
                true,
            );
            prompt.set_rect(panel);
            let mut tr = TextRenderer::new();
            let mut renderer = OffscreenRenderer::new(CANVAS_W, CANVAS_H);
            let mut scene = Scene::new();
            let presence = PresenceCtx::default();
            prompt.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
            let rgba = renderer.render(&scene, theme.tokens.colors.surfaces.content);
            dump(&format!("permission_prompt_{name}"), &rgba);
            assert_edge_registers(&rgba, prompt.get_rect(), name, "permission-prompt");
        }
    }
}
