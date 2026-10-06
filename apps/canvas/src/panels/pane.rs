//! The chrome around the artwork: flush panes of the theme's raised surface,
//! each parted from the canvas by one hairline on the side that faces it,
//! so the artwork on its darker table is what the eye goes to. A pane is
//! the widget it wraps plus that ground: layout, input and accessibility
//! are the wrapped widget's own.

use ui::vello::Scene;
use ui::vello::kurbo::{Affine, Line, Rect, Stroke};
use ui::vello::peniko::Fill;
use ui::widget::{Element, Widget};
use ui::{PresenceCtx, TextRenderer, Theme};

/// The side of a pane that faces the canvas.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    Left,
    Right,
    Bottom,
}

/// `content` on the raised surface, with a hairline on `edge`.
pub struct Pane {
    content: Element,
    edge: Edge,
    group: bool,
}

impl Pane {
    pub fn new(content: Element, edge: Edge) -> Self {
        Self {
            content,
            edge,
            group: false,
        }
    }

    /// Group what the pane holds without a key of its own, as a scrolling
    /// panel does: its controls are read to a person but not offered to an
    /// agent one by one, since each is one of the artwork's own actions
    /// (or a view of it) the agent already has.
    #[must_use]
    pub fn grouping(mut self) -> Self {
        self.group = true;
        self
    }

    fn ground(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let rect: Rect = self.content.get_rect();
        if rect.width() < 1.0 || rect.height() < 1.0 {
            return;
        }
        scene.fill(Fill::NonZero, transform, theme.bg_primary, None, &rect);
        // Half a pixel in, so the hairline is one crisp device pixel.
        let line = match self.edge {
            Edge::Left => Line::new((rect.x0 + 0.5, rect.y0), (rect.x0 + 0.5, rect.y1)),
            Edge::Right => Line::new((rect.x1 - 0.5, rect.y0), (rect.x1 - 0.5, rect.y1)),
            Edge::Bottom => Line::new((rect.x0, rect.y1 - 0.5), (rect.x1, rect.y1 - 0.5)),
        };
        scene.stroke(
            &Stroke::new(1.0),
            transform,
            theme.tokens.colors.overlay_border_subtle,
            None,
            &line,
        );
    }
}

impl ui::widget::Decorator for Pane {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.content.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.content.as_mut()
    }

    fn concrete_type_name(&self) -> &'static str {
        "Pane"
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        if self.group {
            Some(ui::a11y::AccessNode::new(ui::a11y::Role::Group))
        } else {
            self.content.accessibility()
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.ground(scene, transform, theme);
        self.content
            .draw(scene, transform, text_renderer, theme, presence);
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        permissions: &ui::permissions::PermissionContext,
    ) {
        self.ground(scene, transform, theme);
        self.content.draw_with_presence(
            scene,
            transform,
            text_renderer,
            theme,
            presence,
            permissions,
        );
    }
}
