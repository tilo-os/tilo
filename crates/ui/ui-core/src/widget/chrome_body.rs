//! The body of the system's chrome — a bar or a dock — as a described component.

use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Color;

use crate::component::{Component, Described, PartViews};

use crate::theme::Theme;
use crate::tokens::{ChromeMaterial, Rhythm, SurfaceRole};
use crate::widget::DrawChild;
use crate::widget::{Container, Decorator, Element, Widget};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ChromeShape {
    #[default]
    Square,
    Rounded,
}

crate::component::choice!(ChromeShape, "ChromeShape", {
    Square => "square",
    Rounded => "rounded",
});

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
    /// How the body sits in its edge: edge to edge and square (the default), or an inset, rounded body.
    pub shape: Option<ChromeShape>,
    /// The screen edge the body stands on.
    pub edge: Option<ChromeEdge>,
    /// What the body is made of: the theme's bar body (the default), the theme's glass, or that glass made
    /// calm.
    pub material: Option<ChromeMaterial>,
    /// What the body holds.
    #[component(part(default))]
    pub body: Option<Element>,
}

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

    fn part_views(&mut self, part: &str) -> Option<&mut dyn PartViews> {
        (part == "body").then_some(self as &mut dyn PartViews)
    }

    fn set_props(&mut self, props: ChromeProps) -> bool {
        self.name = props.name;
        self.shape = props.shape.unwrap_or_default();
        self.edge = props.edge.unwrap_or_default();
        self.material = props.material.unwrap_or_default();
        // The body in the part is reconciled in place; what the body is made of changed, so
        // the container is built again around it by the host when the shape changed.
        true
    }
}

impl PartViews for ChromeBody {
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

    /// The band past a square body's open side its shadow may fall in: the side facing away from
    /// the screen edge it stands on, as far as the body's own lift can paint. The lift's
    /// [`shadow_bounds`] are the shadow's real extent, so the band never guesses a reach that
    /// clips a larger shadow or leaves a smaller one painting where it may not.
    fn open_side_reach(&self, rect: Rect, theme: &Theme) -> Rect {
        let Some(bounds) = crate::surface::floating_lift(theme)
            .iter()
            .filter_map(|token| crate::surface::shadow_bounds(rect, token))
            .reduce(|one, two| one.union(two))
        else {
            return Rect::new(rect.x0, rect.y1, rect.x1, rect.y1);
        };
        match self.edge {
            ChromeEdge::Top => {
                let reach = (bounds.y1 - rect.y1).max(0.0);
                Rect::new(rect.x0, rect.y1, rect.x1, rect.y1 + reach)
            }
            ChromeEdge::Bottom => {
                let reach = (rect.y0 - bounds.y0).max(0.0);
                Rect::new(rect.x0, rect.y0 - reach, rect.x1, rect.y0)
            }
            ChromeEdge::Leading => {
                let reach = (bounds.x1 - rect.x1).max(0.0);
                Rect::new(rect.x1, rect.y0, rect.x1 + reach, rect.y1)
            }
            ChromeEdge::Trailing => {
                let reach = (rect.x0 - bounds.x0).max(0.0);
                Rect::new(rect.x0 - reach, rect.y0, rect.x0, rect.y1)
            }
        }
    }

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
        } else {
            // A square body stands on its screen edge: its shadow falls only past its open
            // side, onto what it covers (below a top bar, beside a side rail).
            let cast = self.open_side_reach(rect, theme);
            scene.push_clip_layer(vello::peniko::Fill::NonZero, transform, &cast);
            crate::surface::floating_shadow(scene, transform, rect, 0.0, 1.0, theme);
            scene.pop_layer();
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

    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        self.container.collect_damage(regions);
    }

    fn draw(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        self.paint_body(scene, transform, theme);
        self.container
            .draw_child(scene, transform, text, theme, &env);
    }

    fn take_structure_dirty(&mut self) -> bool {
        self.container.take_structure_dirty()
    }

    fn take_structure_change(&mut self) -> crate::widget::StructureChange {
        Widget::take_structure_change(&mut self.container)
    }

    fn name(&self) -> &'static str {
        "Chrome"
    }

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

    tilo_test_env::scenario! {
        /// What a chrome body remembers.
        /// Each phase was a test of its own and builds its own fixture.
        fn a_chrome_body_keeps_its_rect_and_its_material;

        phase the_rect_of_an_unlaid_out_body_is_empty;
        {
            let props = ChromeProps::default();
            let body = ChromeBody::from_props("c", props);
            assert_eq!(body.rect, Rect::ZERO);
            assert_eq!(body.shape, ChromeShape::Square);
            assert_eq!(body.material, ChromeMaterial::Bar);
        }

        phase a_body_remembered_the_material_it_was_built_with;
        {
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

    #[test]
    fn a_square_bar_on_the_top_edge_casts_its_shadow_below_it_only() {
        let theme = Theme::light();
        let mut bar = ChromeBody::from_props(
            "bar",
            ChromeProps {
                edge: Some(ChromeEdge::Top),
                ..ChromeProps::default()
            },
        );
        crate::testing::lay_out_in(&mut bar, 400.0, 40.0, &theme).unwrap();
        let rect = bar.inner().get_rect();
        assert!(rect.height() > 0.0, "the bar has a body: {rect:?}");
        let mut scene = Scene::new();
        let top = 60.0;
        bar.paint_body(&mut scene, Affine::translate((0.0, top)), &theme);
        let rgba = crate::offscreen::OffscreenRenderer::new(400, 240).render(&scene, Color::WHITE);
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "test pixels"
        )]
        let at = |y: f64| crate::offscreen::OffscreenRenderer::sample(&rgba, 400, 200, y as u32).0;
        let below = at(top + rect.y1 + 3.0);
        let above = at(top + rect.y0 - 4.0);
        let far = at(top + rect.y1 + 150.0);
        assert!(
            below + 4 < far,
            "a shadow falls under the bar: {below} against {far}"
        );
        assert_eq!(
            above, far,
            "none falls above it, onto the screen edge it stands on"
        );
    }
}
