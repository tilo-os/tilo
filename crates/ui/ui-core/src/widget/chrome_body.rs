//! The body of the system's chrome — a bar or a dock — as a described component.

use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Color;

use tilo_ui_base::{component::Component, component::Described, component::PartViews};

use crate::widget::Container;
use tilo_ui_base::text_render::TextRenderer;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::widget::DrawChild;
use tilo_ui_base::{tokens::ChromeMaterial, tokens::Rhythm, tokens::SurfaceRole};
use tilo_ui_base::{widget::Decorator, widget::Element, widget::Widget};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ChromeShape {
    #[default]
    Square,
    Rounded,
}

tilo_ui_base::choice!(ChromeShape, "ChromeShape", {
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

tilo_ui_base::choice!(ChromeEdge, "ChromeEdge", {
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
    /// Keep the band the body rests in, and let what it holds grow over it: a dock's item under
    /// the pointer stands above the band's edge rather than pushing the band, and the windows
    /// under it, up.
    pub reserve: bool,
    /// What the body holds.
    #[component(part(default))]
    pub body: Option<Element>,
}

pub struct ChromeBody {
    name: Option<String>,
    shape: ChromeShape,
    edge: ChromeEdge,
    material: ChromeMaterial,
    /// The band the body keeps while what it holds magnifies: its across size at rest (height on
    /// a top or foot edge, width up a side).
    reserve: bool,
    rest_across: Option<f64>,
    /// The style the container was built with, kept while the band is held so it comes back when
    /// the growth settles.
    resting_style: Option<taffy::Style>,
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
            reserve,
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
            reserve,
            rest_across: None,
            resting_style: None,
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
        if self.reserve != props.reserve {
            self.reserve = props.reserve;
            self.resting_style = None;
        }
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
    flush: tilo_ui_base::part_paint::Sides,
) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    let body = RoundedRect::from_rect(rect, 0.0);
    // The sides that stand on the display's own edge are flush: the body
    // draws no rim, edge or hairline there, so no pixel of another colour
    // shows between it and the screen's edge.
    tilo_ui_base::part_paint::backdrop::fill_role_flush(
        scene,
        transform,
        &body,
        tilo_ui_base::tokens::SurfaceRole::Bar,
        theme.finish(tilo_ui_base::tokens::SurfaceRole::Bar),
        theme,
        ground,
        flush,
    );
    let (width, ink) = tilo_ui_base::surface::border(theme).unwrap_or_else(|| {
        (
            1.0,
            if theme
                .finish(tilo_ui_base::tokens::SurfaceRole::Bar)
                .hairline()
            {
                tilo_ui_base::chrome::chrome_hairline(theme)
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
    /// The body's size across the edge it rides: its height on a top or foot edge, its width up
    /// a side.
    fn across(&self, rect: Rect) -> f64 {
        match self.edge {
            ChromeEdge::Top | ChromeEdge::Bottom => rect.height(),
            ChromeEdge::Leading | ChromeEdge::Trailing => rect.width(),
        }
    }

    fn flush_sides(&self) -> tilo_ui_base::part_paint::Sides {
        use tilo_ui_base::part_paint::Sides;
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
        let Some(bounds) = tilo_ui_base::surface::floating_lift(theme)
            .iter()
            .filter_map(|token| tilo_ui_base::surface::shadow_bounds(rect, token))
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
            tilo_ui_base::surface::floating_shadow(scene, transform, rect, radius, 1.0, theme);
        } else {
            // A square body stands on its screen edge: its shadow falls only past its open
            // side, onto what it covers (below a top bar, beside a side rail).
            let cast = self.open_side_reach(rect, theme);
            scene.push_clip_layer(vello::peniko::Fill::NonZero, transform, &cast);
            tilo_ui_base::surface::floating_shadow(scene, transform, rect, 0.0, 1.0, theme);
            scene.pop_layer();
        }
        let finish = theme.chrome_material(self.material);
        tilo_ui_base::part_paint::backdrop::fill_role_flush(
            scene,
            transform,
            &body,
            SurfaceRole::Bar,
            &finish,
            theme,
            tilo_ui_base::chrome::panel_ground(theme),
            self.flush_sides(),
        );
        let (width, ink) = tilo_ui_base::surface::border(theme).unwrap_or_else(|| {
            (
                1.0,
                if finish.hairline() {
                    tilo_ui_base::chrome::chrome_hairline(theme)
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
        let node = self.inner_mut().build_layout(taffy, theme, node_map);
        // The node is this body's: the hooks it overrides (the band it reserves) are reached
        // through the node map, where the inner body would otherwise stand.
        tilo_ui_base::widget::register_node(node_map, node, self);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.container.apply_layout(taffy, node, origin);
        self.rect = self.container.get_rect();
        // Laid with nothing magnified, this is the band the body rests in.
        if self.container.magnification() <= 1.0 + 1e-4 {
            self.rest_across = Some(self.across(self.rect));
        }
    }

    /// Hold the rested band while what the body holds magnifies, so the items grow over the band
    /// (and the windows under it) rather than pushing it. Released as soon as the growth settles,
    /// so a size setting that changes the band still moves it.
    ///
    /// Asked at every solve, because a layout pass builds the body's style again: the band is set
    /// on each solve it is held, and put back on each solve it is not.
    fn adjust_layout_style(
        &mut self,
        taffy: &mut TaffyTree<()>,
        node: NodeId,
        _text: &mut TextRenderer,
    ) -> bool {
        if !self.reserve {
            return false;
        }
        let Ok(current) = taffy.style(node).cloned() else {
            return false;
        };
        if self.container.magnification() > 1.0 + 1e-4 {
            let Some(across) = self.rest_across else {
                return false;
            };
            let mut held = current.clone();
            let band = taffy::Dimension::length(tilo_ui_base::num::f32_of_f64(across));
            match self.edge {
                ChromeEdge::Top | ChromeEdge::Bottom => {
                    held.size.height = band;
                    held.min_size.height = band;
                    held.max_size.height = band;
                    held.align_items = Some(if self.edge == ChromeEdge::Bottom {
                        taffy::AlignItems::FlexEnd
                    } else {
                        taffy::AlignItems::FlexStart
                    });
                }
                ChromeEdge::Leading | ChromeEdge::Trailing => {
                    held.size.width = band;
                    held.min_size.width = band;
                    held.max_size.width = band;
                    held.align_items = Some(if self.edge == ChromeEdge::Leading {
                        taffy::AlignItems::FlexStart
                    } else {
                        taffy::AlignItems::FlexEnd
                    });
                }
            }
            self.resting_style.get_or_insert(current.clone());
            if held == current {
                return false;
            }
            taffy.set_style(node, held).is_ok()
        } else {
            let Some(resting) = self.resting_style.as_ref() else {
                return false;
            };
            if *resting == current {
                self.resting_style = None;
                return false;
            }
            let resting = resting.clone();
            self.resting_style = None;
            taffy.set_style(node, resting).is_ok()
        }
    }

    /// The body paints only where its content changed: a bar at rest draws nothing.
    fn needs_redraw(&self) -> bool {
        self.container.needs_redraw() || self.container.needs_animation_frame()
    }

    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        self.container.collect_damage(regions);
    }

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
        self.paint_body(scene, transform, theme);
        // What the body holds stands on the body's own ground: a control's hover plate and its
        // ink are measured against what is under it, not the window ground behind the chrome.
        let inside = theme.on_ground(tilo_ui_base::chrome::panel_ground(theme));
        self.container
            .draw_child(scene, transform, text, &inside, &env);
    }

    fn take_structure_dirty(&mut self) -> bool {
        self.container.take_structure_dirty()
    }

    fn take_structure_change(&mut self) -> tilo_ui_base::widget::StructureChange {
        Widget::take_structure_change(&mut self.container)
    }

    fn name(&self) -> &'static str {
        "Chrome"
    }

    fn accessibility(&self) -> Option<tilo_ui_base::a11y::AccessNode> {
        self.name.as_ref().map(|name| {
            tilo_ui_base::a11y::AccessNode::new(tilo_ui_base::a11y::Role::Group)
                .with_optional_name(Some(name.clone()))
        })
    }
}
