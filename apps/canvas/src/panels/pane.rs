//! Wrappers for the panels around the artwork. Layout, input and
//! accessibility are the wrapped widget's own unless said otherwise; the
//! window shell gives each column its ground.

use ui::vello::Scene;
use ui::vello::kurbo::{Affine, Rect};
use ui::widget::{Element, Widget};
use ui::{PresenceCtx, TextRenderer, Theme};

/// `content`, announced as one group: what it holds is read to a person
/// but not offered to an agent one by one, since each control is one of
/// the artwork's own actions (or a view of it) the agent already has.
pub struct Group {
    content: Element,
}

impl Group {
    pub fn new(content: Element) -> Self {
        Self { content }
    }
}

impl ui::widget::Decorator for Group {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.content.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.content.as_mut()
    }

    fn concrete_type_name(&self) -> &'static str {
        "Group"
    }

    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        Some(ui::a11y::AccessNode::new(ui::a11y::Role::Group))
    }
}

/// A toolbar floating over the top of the artwork: the floating material
/// and its one shadow, as any floating thing has, so the artwork does not
/// show through its commands.
pub struct Band {
    content: Element,
}

impl Band {
    pub fn new(content: Element) -> Self {
        Self { content }
    }

    fn ground(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let rect: Rect = self.content.get_rect();
        if rect.width() < 1.0 || rect.height() < 1.0 {
            return;
        }
        let radius = f64::from(theme.tokens.radii.panel);
        ui::surface::shadow(
            scene,
            transform,
            rect,
            radius,
            &theme.tokens.elevation.floating,
        );
        ui::surface::panel(scene, transform, rect, radius, theme);
    }
}

impl ui::widget::Decorator for Band {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.content.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.content.as_mut()
    }

    fn concrete_type_name(&self) -> &'static str {
        "Band"
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
}
