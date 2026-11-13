//! The body of the system's chrome — a bar or a dock — as a described component.
//!
//! A bar and a dock stand over what they cover, so their ground is the theme's bar finish
//! ([`crate::tokens::SurfaceRole::Bar`]) rather than any app's colour: edge to edge and square
//! at the `full` fit, a rounded body inset from the edges at the floating one. A view says
//! which through [`ChromeShape`] and what the body is made of through [`ChromeMaterial`]; the
//! theme resolves that into a finish, and the shape follows the theme's radii so a rounder
//! theme is rounder chrome.

use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Color;

use crate::component::{Component, Described, SlotViews};
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::{ChromeMaterial, Rhythm, SurfaceRole};
use crate::widget::DrawChild;
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

/// The edge of the screen a bar or a dock stands on: the side of its body the content is. A
/// square body casts no rule from it (bars cast none, design-language.md); it says which way a
/// floating, rounded body's hairline faces and how a view lays its content out.
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
    /// The screen edge the body stands on. A square body casts no rule at it: a bar edge to
    /// edge with the screen has no line to mark, and a hairline a pixel inside its body would
    /// start its ground below the display's own edge.
    pub edge: Option<ChromeEdge>,
    /// What the body is made of: the theme's bar body (the default), the theme's glass, or
    /// that glass made calm. A shell's bar and dock say which the person chose; every body
    /// that leaves it out draws the theme's bar body.
    pub material: Option<ChromeMaterial>,
    /// What the body holds.
    #[component(slot(default))]
    pub body: Option<Element>,
}

/// [`ChromeProps`] built: a padded container whose ground the theme paints.
pub struct ChromeBody {
    name: Option<String>,
    shape: ChromeShape,
    edge: ChromeEdge,
    material: ChromeMaterial,
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
            material,
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
            material: material.unwrap_or_default(),
            container,
            rect: Rect::ZERO,
        };
        if let Some(content) = body {
            built.container.set_child(content);
        }
        built
    }

    /// The body it holds changes where it stands: the bar's controls keep what they hold (the
    /// menu a person opened, the field they were typing in) while what they show changes under
    /// them, rather than the body being built again around them.
    fn slot_views(&mut self, slot: &str) -> Option<&mut dyn SlotViews> {
        (slot == "body").then_some(self as &mut dyn SlotViews)
    }

    fn set_props(&mut self, props: ChromeProps) -> bool {
        self.name = props.name;
        self.shape = props.shape.unwrap_or_default();
        self.edge = props.edge.unwrap_or_default();
        self.material = props.material.unwrap_or_default();
        // The body in the slot is reconciled in place; what the body is made of changed, so
        // the container is built again around it by the host when the shape changed.
        true
    }
}

/// The body the chrome holds: one view, which changes where it stands.
impl SlotViews for ChromeBody {
    fn len(&self) -> usize {
        usize::from(self.container.has_child())
    }

    fn view_mut(&mut self, index: usize) -> Option<&mut Element> {
        (index == 0).then(|| self.container.child_mut()).flatten()
    }

    fn insert(&mut self, _index: usize, view: Element) {
        self.container.set_child(view);
    }

    fn remove(&mut self, _index: usize) {
        self.container.take_child();
    }

    fn move_view(&mut self, _from: usize, _to: usize) {}

    fn replace(&mut self, _index: usize, view: Element) {
        self.container.replace_child(view);
    }
}

/// The square body of a bar standing edge to edge in the screen's own edge,
/// in `rect`: the theme's bar finish over its own panel ground and the edge
/// the theme gives it along its foot. Bars cast no rule at the screen's edge
/// (design-language.md), so a square body paints no outline; a shell's bar
/// paints this and its controls on top.
pub fn paint_bar_body(
    scene: &mut Scene,
    transform: Affine,
    rect: Rect,
    theme: &Theme,
    ground: Color,
    flush: crate::part_paint::Sides,
) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    let body = RoundedRect::from_rect(rect, 0.0);
    // The sides that stand on the display's own edge are flush: the body
    // draws no rim, edge or hairline there, so no pixel of another colour
    // shows between it and the screen's edge.
    crate::backdrop::fill_role_flush(
        scene,
        transform,
        &body,
        crate::tokens::SurfaceRole::Bar,
        theme.finish(crate::tokens::SurfaceRole::Bar),
        theme,
        ground,
        flush,
    );
    let (width, ink) = crate::surface::border(theme).unwrap_or_else(|| {
        (
            1.0,
            if theme.finish(crate::tokens::SurfaceRole::Bar).hairline() {
                crate::chrome::chrome_hairline(theme)
            } else {
                Color::TRANSPARENT
            },
        )
    });
    if ink.components[3] <= 0.0 {
        return;
    }
    let rule = Rect::new(rect.x0, rect.y1 - width, rect.x1, rect.y1);
    scene.fill(vello::peniko::Fill::NonZero, transform, ink, None, &rule);
}

impl ChromeBody {
    /// The sides of a square body that stand on the display's own edge.
    ///
    /// A bar at the `full` fit spans the display edge to edge, so its two
    /// ends and the side it stands on the screen by are the display's own:
    /// its ground reaches the screen there in one colour, with no rim, edge
    /// or hairline a pixel in from it. Its inner side keeps them, which is
    /// where it meets what it covers. A floating body stands inside every
    /// edge, so it has none.
    ///
    /// The edges it stands on are the ends only along the screen's own
    /// width: a bar on the top or the foot of the screen meets the leading
    /// and trailing edges, and one on the leading or trailing edge meets
    /// the top and the foot. Either way a square body spans both of its
    /// perpendicular edges.
    fn flush_sides(&self) -> crate::part_paint::Sides {
        use crate::part_paint::Sides;
        if self.shape != ChromeShape::Square {
            return Sides::NONE;
        }
        match self.edge {
            ChromeEdge::Top => Sides::TOP.union(Sides::LEADING).union(Sides::TRAILING),
            ChromeEdge::Bottom => Sides::BOTTOM.union(Sides::LEADING).union(Sides::TRAILING),
            ChromeEdge::Leading => Sides::LEADING.union(Sides::TOP).union(Sides::BOTTOM),
            ChromeEdge::Trailing => Sides::TRAILING.union(Sides::TOP).union(Sides::BOTTOM),
        }
    }

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
        let finish = theme.chrome_material(self.material);
        crate::backdrop::fill_role_flush(
            scene,
            transform,
            &body,
            SurfaceRole::Bar,
            &finish,
            theme,
            crate::chrome::panel_ground(theme),
            self.flush_sides(),
        );
        let (width, ink) = crate::surface::border(theme).unwrap_or_else(|| {
            (
                1.0,
                if finish.hairline() {
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
            // A bar standing edge to edge in the screen's own edge casts no
            // rule (design-language.md: "Bars, cards and selected controls
            // cast none"): a hairline a pixel into its body left the bar's
            // ground starting one logical pixel below the display's top, which
            // on a dense panel reads as a bar floating a little down the
            // screen. A floating (rounded) body keeps its hairline, which
            // separates it from what shows around it.
            ChromeShape::Square => {}
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
        self.container.needs_redraw() || self.container.needs_animation_frame()
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
        self.container
            .draw_child(scene, transform, text, theme, presence);
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
        assert_eq!(body.material, ChromeMaterial::Bar);
    }

    #[test]
    fn a_body_remembered_the_material_it_was_built_with() {
        let mut body = ChromeBody::from_props(
            "c",
            ChromeProps {
                material: Some(ChromeMaterial::Calm),
                ..ChromeProps::default()
            },
        );
        assert_eq!(body.material, ChromeMaterial::Calm);
        body.set_props(ChromeProps {
            material: Some(ChromeMaterial::Glass),
            ..ChromeProps::default()
        });
        assert_eq!(body.material, ChromeMaterial::Glass);
    }
}
