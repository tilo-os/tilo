//! The go-to-offset form: a caption over one text field, shown in a
//! [`Popover`](ui::widget::Popover).
//!
//! The field is the shared text input (selection, clipboard, IME and
//! accessibility come with it); the form only places it under its caption
//! and reports Enter. Parsing the offset stays with the view.

use std::collections::HashMap;

use taffy::style_helpers::length;
use taffy::{NodeId, Size, Style, TaffyTree};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::{ControlSize, EventCtx, TaffyTreeExt, TextInput, TextInputKind, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Vec2};
use winit::event::{ElementState, WindowEvent};
use winit::keyboard::{Key, NamedKey};

pub(super) struct GotoForm {
    input: TextInput,
    rect: Rect,
    caption_h: f64,
    gap: f64,
    submitted: bool,
}

impl GotoForm {
    pub fn new() -> Self {
        Self {
            input: TextInput::local()
                .with_kind(TextInputKind::Code)
                .with_id("hex.goto")
                .with_accessible_name("Go to offset")
                .with_placeholder("0x1F or 31"),
            rect: Rect::ZERO,
            caption_h: 0.0,
            gap: 0.0,
            submitted: false,
        }
    }

    /// Focus the field with its text selected, as the popover opens.
    pub fn focus(&mut self) {
        self.input.set_focus(true);
        self.input.set_cursor_pos(self.input.value().len());
    }

    /// What the person typed once they pressed Enter; clears the field.
    pub fn take_submit(&mut self) -> Option<String> {
        if !std::mem::take(&mut self.submitted) {
            return None;
        }
        let value = self.input.value().to_owned();
        self.reset();
        Some(value)
    }

    /// Empty the field and drop its focus.
    pub fn reset(&mut self) {
        self.input.set_text("");
        self.input.set_focus(false);
        self.submitted = false;
    }
}

impl Widget for GotoForm {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let caption = theme.text(TextRole::Subheading);
        self.caption_h = f64::from(caption.line_height_px());
        self.gap = f64::from(theme.tokens.spacing.s2);
        // The popover's own padding on both sides brings the panel to the
        // utility panel width.
        let width = theme.tokens.metrics.panel_width_utility - 2.0 * theme.tokens.spacing.s4;
        let height = self.caption_h + self.gap + ControlSize::Regular.height(theme);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a form's height is a small logical-pixel value"
        )]
        let node = taffy.leaf_node(Style {
            size: Size {
                width: length(width),
                height: length(height as f32),
            },
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let Ok(l) = taffy.layout(node) else {
            return;
        };
        let x0 = origin.x + f64::from(l.location.x);
        let y0 = origin.y + f64::from(l.location.y);
        self.rect = Rect::new(
            x0,
            y0,
            x0 + f64::from(l.size.width),
            y0 + f64::from(l.size.height),
        );
        self.input.apply_layout_external(Rect::new(
            self.rect.x0,
            self.rect.y0 + self.caption_h + self.gap,
            self.rect.x1,
            self.rect.y1,
        ));
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        text.draw_style(
            scene,
            "Go to offset",
            theme.tokens.colors.text_primary,
            transform * Affine::translate(Vec2::new(self.rect.x0, self.rect.y0)),
            None,
            &theme.text(TextRole::Subheading),
        );
        self.input.draw(scene, transform, text, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        if let WindowEvent::KeyboardInput { event: key, .. } = event
            && key.state == ElementState::Pressed
            && matches!(key.logical_key, Key::Named(NamedKey::Enter))
        {
            self.submitted = true;
            *ctx.handled = true;
            return;
        }
        self.input.handle_event(event, ctx);
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.input]
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        visitor(&self.input);
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.input]
    }

    fn ime_cursor_rect(&self) -> Option<Rect> {
        self.input.ime_cursor_rect()
    }

    fn ime_purpose(&self) -> Option<winit::window::ImePurpose> {
        self.input.ime_purpose()
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        self.input.cursor_shape(point)
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        self.input.accessibility()
    }

    fn set_focus(&mut self, focused: bool) {
        self.input.set_focus(focused);
    }

    fn name(&self) -> &'static str {
        "GotoForm"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}
