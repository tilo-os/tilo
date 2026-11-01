//! The body of the system's chrome — a bar or a dock — as a described component.
//!
//! A bar and a dock stand over what they cover, so their ground is the theme's bar finish
//! ([`crate::tokens::SurfaceRole::Bar`]) rather than any app's colour: edge to edge and square
//! at the `full` fit, a rounded body inset from the edges at the floating one. A view says
//! which through [`ChromeShape`]; the theme decides what the body is made of, and the shape
//! follows the theme's radii so a rounder theme is rounder chrome.

use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};

use crate::component::{Component, Described};
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::{Rhythm, SurfaceRole};
use crate::widget::{Container, Decorator, Element, Widget};

/// How a bar or a dock's body sits in its edge.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ChromeShape {
    /// Edge to edge and square: a classic bar, a taskbar.
    #[default]
    Square,
    /// Inset from the edges and rounded: a floating bar, a macOS-style dock.
    Rounded,
}

crate::component::choice!(ChromeShape, "ChromeShape", {
    Square => "square",
    Rounded => "rounded",
});

/// The edge of the screen a bar or a dock stands on: the side of its body the content is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ChromeEdge {
    Top,
    #[default]
    Bottom,
    Leading,
    Trailing,
}

crate::component::choice!(ChromeEdge, "ChromeEdge", {
    Top => "top",
    Bottom => "bottom",
    Leading => "leading",
    Trailing => "trailing",
});

/// A bar or a dock's body: the theme's bar finish behind what the body holds.
#[derive(Component, Default)]
#[component(name = "Chrome", role = "Group", label = "name")]
pub struct ChromeProps {
    /// What the body is, for assistive technology.
    pub name: Option<String>,
    /// How the body sits in its edge: edge to edge and square (the default), or an inset,
    /// rounded body.
    pub shape: Option<ChromeShape>,
    /// The screen edge the body stands on, where a square body draws its rule.
    pub edge: Option<ChromeEdge>,
    /// What the body holds.
    #[component(slot(default))]
    pub body: Option<Element>,
}

/// [`ChromeProps`] built: a padded container whose ground the theme paints.
pub struct ChromeBody {
    name: Option<String>,
    shape: ChromeShape,
    edge: ChromeEdge,
    container: Container,
    rect: Rect,
}

impl Described for ChromeBody {
    type Props = ChromeProps;

    fn from_props(key: &str, props: ChromeProps) -> Self {
        let ChromeProps {
            name,
            shape,
            edge,
            body,
        } = props;
        let shape = shape.unwrap_or_default();
        let container = match shape {
            ChromeShape::Square => Container::new()
                .with_inset(Rhythm::Tight)
                .narrows()
                .with_id(key.to_owned()),
            ChromeShape::Rounded => Container::new()
                .hug()
                .with_inset(Rhythm::Related)
                .narrows()
                .with_id(key.to_owned()),
        };
        let mut built = Self {
            name,
            shape,
            edge: edge.unwrap_or_default(),
            container,
            rect: Rect::ZERO,
        };
        if let Some(content) = body {
            built.container.set_child(content);
        }
        built
    }

    fn set_props(&mut self, props: ChromeProps) -> bool {
        self.name = props.name;
        self.shape = props.shape.unwrap_or_default();
        self.edge = props.edge.unwrap_or_default();
        // The body in the slot is reconciled in place; what the body is made of changed, so
        // the container is built again around it by the host when the shape changed.
        true
    }
}

impl ChromeBody {
    /// The body's ground: the theme's bar finish over its own panel ground.
    fn paint_body(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let rect = self.inner().get_rect();
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return;
        }
        let radius = match self.shape {
            ChromeShape::Square => 0.0,
            ChromeShape::Rounded => f64::from(theme.tokens.radii.panel).min(rect.height() / 2.0),
        };
        let body = RoundedRect::from_rect(rect, radius);
        if radius > 0.0 {
            // A rounded body floats over what it covers: its shadow falls outside it only,
            // so what shows through is not darkened.
            crate::surface::floating_shadow(scene, transform, rect, radius, 1.0, theme);
        }
        crate::backdrop::fill_role(
            scene,
            transform,
            &body,
            SurfaceRole::Bar,
            theme,
            crate::chrome::panel_ground(theme),
        );
        let (width, ink) = crate::surface::border(theme).unwrap_or_else(|| {
            (
                1.0,
                if theme.finish(SurfaceRole::Bar).hairline() {
                    crate::chrome::chrome_hairline(theme)
                } else {
                    Color::TRANSPARENT
                },
            )
        });
        if ink.components[3] <= 0.0 {
            return;
        }
        match self.shape {
            // A square body's rule is along the edge its content is on, as a bar's is.
            ChromeShape::Square => {
                let width = width.max(1.0);
                let rule = match self.edge {
                    ChromeEdge::Top => Rect::new(rect.x0, rect.y0, rect.x1, rect.y0 + width),
                    ChromeEdge::Bottom => Rect::new(rect.x0, rect.y1 - width, rect.x1, rect.y1),
                    ChromeEdge::Leading => Rect::new(rect.x0, rect.y0, rect.x0 + width, rect.y1),
                    ChromeEdge::Trailing => Rect::new(rect.x1 - width, rect.y0, rect.x1, rect.y1),
                };
                scene.fill(Fill::NonZero, transform, ink, None, &rule);
            }
            ChromeShape::Rounded => {
                scene.stroke(
                    &Stroke::new(width),
                    transform,
                    ink,
                    None,
                    &RoundedRect::from_rect(rect.inset(-width / 2.0), radius + width / 2.0),
                );
            }
        }
    }
}

impl Decorator for ChromeBody {
    type Inner = Container;

    fn inner(&self) -> &Container {
        &self.container
    }

    fn inner_mut(&mut self) -> &mut Container {
        &mut self.container
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.inner_mut().build_layout(taffy, theme, node_map)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.container.apply_layout(taffy, node, origin);
        self.rect = self.container.get_rect();
    }

    /// The body paints only where its content changed: a bar at rest draws nothing.
    fn needs_redraw(&self) -> bool {
        self.container.needs_redraw()
    }

    /// The body's ground repaints where its content changed: the container carries its
    /// child's damage.
    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        self.container.collect_damage(regions);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.paint_body(scene, transform, theme);
        self.container.draw(scene, transform, text, theme, presence);
    }

    /// The body adds no structure of its own: what its content reports is the body's.
    fn take_structure_dirty(&mut self) -> bool {
        self.container.take_structure_dirty()
    }

    fn take_structure_change(&mut self) -> crate::widget::StructureChange {
        Widget::take_structure_change(&mut self.container)
    }

    fn name(&self) -> &'static str {
        "Chrome"
    }

    /// The body is a group named for what it is, and what it holds is read inside it.
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        self.name.as_ref().map(|name| {
            crate::a11y::AccessNode::new(crate::a11y::Role::Group)
                .with_optional_name(Some(name.clone()))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rect_of_an_unlaid_out_body_is_empty() {
        let props = ChromeProps::default();
        let body = ChromeBody::from_props("c", props);
        assert_eq!(body.rect, Rect::ZERO);
        assert_eq!(body.shape, ChromeShape::Square);
    }
}
