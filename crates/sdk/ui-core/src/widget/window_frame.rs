//! A window standing on a desktop, as a described component: what a shell draws for a place on
//! a floating desk (`shell.md` §4, `frame_classic`).
//!
//! Its ground is the window level of the design language ([`SurfaceLevel::Window`]): the app's
//! content colour, the window edge and the window's soft shadow, at the theme's window radius,
//! and what it holds is clipped to that shape. The window in front stands higher than the ones
//! behind it, a floating shadow under its own, as a desktop shows which window has the
//! keyboard.
//!
//! A window has one bar, never two stacked: the frame offers its window bar to the content's
//! header row ([`WindowHeader::set_window_bar`](crate::widget::WindowHeader)), as a desktop
//! window does, and its controls stand at that row's trailing end, so the app's title and
//! actions and the frame's controls read as one bar. Content with no header row of its own (a
//! foreign window in its well) gets the frame's own bar instead: the window's name, with the
//! controls at its end. A view says which window is in front and what its controls are; the
//! theme decides everything the frame is made of.

use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Vec2};
use vello::peniko::{BlendMode, Fill};

use crate::chrome::HeaderRowMetrics;
use crate::component::{Component, Described, SlotViews};
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::{SpacingStep, TextRole};
use crate::widget::arrange::{Align, At, Fit, Layers, Length, Slot, Stack};
use crate::widget::primitives::{SurfaceLevel, paint_surface};
use crate::widget::{Decorator, Element, Spacer, Text, Widget, WindowBar};

/// A window standing on a desktop: the window surface with its edge and shadow, holding the
/// window's content and its controls, clipped to its radius.
#[derive(Component, Default)]
#[component(name = "WindowFrame", role = "Group", label = "name")]
pub struct WindowFrameProps {
    /// The window's name: what assistive technology reads, and the title of the frame's own
    /// bar when the content has no header row to hold the controls.
    pub name: Option<String>,
    /// Whether it is the window in front, the one the keyboard goes to.
    pub focused: bool,
    /// The shell lists this window's actions where the person reaches them (the desktop
    /// bar's window menu): its header's actions that do not fit give way with no door of
    /// their own. Without it what does not fit goes behind the header's door.
    pub lists_actions: bool,
    /// The frame's controls (close, and what else the shell offers on the window), at the
    /// trailing end of the window's bar.
    #[component(slot)]
    pub controls: Option<Element>,
    /// The window's content, filling the window.
    #[component(slot(default))]
    pub body: Option<Element>,
}

/// [`WindowFrameProps`] built: layers on the window surface, the content (under the frame's
/// own bar while it needs one) and the controls over its bar's trailing end.
pub struct WindowFrame {
    name: Option<String>,
    focused: bool,
    lists_actions: bool,
    layers: Layers,
    /// The slot a host is changing: 0 for the controls, 1 for the content.
    editing: usize,
    /// The content's header row holds the window's bar, so the frame shows no bar of its own.
    merged: bool,
    /// How wide the controls were laid out, the room the content's header row keeps for
    /// them.
    controls_width: f64,
    /// The controls' width changed since the header row was told: lay out again.
    relayout: bool,
    /// The header row's measures in the theme of the last layout.
    metrics: Option<HeaderRowMetrics>,
    rect: Rect,
}

/// The layer holding the content, and the one holding the controls.
const CONTENT: usize = 0;
const CONTROLS: usize = 1;
/// In the content's column: the frame's own bar, then the content.
const OWN_BAR: usize = 0;
const BODY: usize = 1;

fn empty() -> Element {
    Box::new(Spacer::token(SpacingStep::S0))
}

fn slot(element: Element, fit: Fit) -> Slot {
    let mut slot = Slot::new(element);
    slot.fit = fit;
    slot
}

impl Described for WindowFrame {
    type Props = WindowFrameProps;

    fn from_props(_key: &str, props: WindowFrameProps) -> Self {
        let WindowFrameProps {
            name,
            focused,
            lists_actions,
            controls,
            body,
        } = props;
        let column = Stack::stack()
            .fill_width()
            .fill_height()
            .push_slot(slot(empty(), Fit::hug()))
            .push_slot(slot(body.unwrap_or_else(empty), Fit::fill(1.0)));
        let controls = Stack::row()
            .align(Align::Center)
            .push(controls.unwrap_or_else(empty));
        let layers = Layers::layers()
            .push_slot(slot(Box::new(column), Fit::fill(1.0)))
            .push_at(Box::new(controls), At::TopTrailing);
        Self {
            name,
            focused,
            lists_actions,
            layers,
            editing: 1,
            merged: true,
            controls_width: 0.0,
            relayout: false,
            metrics: None,
            rect: Rect::ZERO,
        }
    }

    fn set_props(&mut self, props: WindowFrameProps) -> bool {
        let changed = self.focused != props.focused
            || self.name != props.name
            || self.lists_actions != props.lists_actions;
        if self.name != props.name && !self.merged {
            // The own bar shows the name: build it again with the new one.
            self.merged = true;
        }
        self.name = props.name;
        self.focused = props.focused;
        self.lists_actions = props.lists_actions;
        changed
    }

    fn slot_views(&mut self, slot: &str) -> Option<&mut dyn SlotViews> {
        self.editing = match slot {
            "controls" => 0,
            "body" => 1,
            _ => return None,
        };
        Some(self)
    }
}

/// The view of the slot being changed: the controls, or the content.
impl SlotViews for WindowFrame {
    fn len(&self) -> usize {
        1
    }

    fn view_mut(&mut self, index: usize) -> Option<&mut Element> {
        if index != 0 {
            return None;
        }
        if self.editing == 0 {
            self.controls_row()?.child_mut(0)
        } else {
            self.column()?.child_mut(BODY)
        }
    }

    fn insert(&mut self, _index: usize, view: Element) {
        self.put(view);
    }

    fn remove(&mut self, _index: usize) {
        self.put(empty());
    }

    fn move_view(&mut self, _from: usize, _to: usize) {}

    fn replace(&mut self, _index: usize, view: Element) {
        self.put(view);
    }
}

impl WindowFrame {
    fn stack_at(&mut self, index: usize) -> Option<&mut Stack> {
        self.layers
            .child_mut(index)?
            .as_any_mut()?
            .downcast_mut::<Stack>()
    }

    /// The column of the frame's own bar over the content.
    fn column(&mut self) -> Option<&mut Stack> {
        self.stack_at(CONTENT)
    }

    /// The row the controls stand in.
    fn controls_row(&mut self) -> Option<&mut Stack> {
        self.stack_at(CONTROLS)
    }

    /// Put `view` in the slot being changed.
    fn put(&mut self, view: Element) {
        if self.editing == 0 {
            if let Some(row) = self.controls_row() {
                let _ = row.replace(0, view);
            }
        } else if let Some(column) = self.column() {
            let _ = column.replace(BODY, view);
            // New content: whether it has a header row to hold the bar is asked again.
            self.merged = true;
        }
    }

    /// Offer the content's header row the window's bar, and show the frame's own bar when
    /// no row takes it.
    fn offer_bar(&mut self, m: &HeaderRowMetrics) {
        let bar = WindowBar {
            controls_width: self.controls_width + m.inset,
            revealed: true,
            top_inset: 0.0,
            lists_actions: self.lists_actions,
        };
        let merged = self
            .column()
            .and_then(|column| column.child_mut(BODY))
            .is_some_and(|body| {
                crate::widget::window_header::offer_window_bar(body.as_mut(), Some(bar))
            });
        if merged == self.merged {
            return;
        }
        self.merged = merged;
        let own: Element = if merged {
            empty()
        } else {
            let mut title =
                Text::new(self.name.as_deref().unwrap_or_default()).with_role(TextRole::Label);
            if !self.focused {
                title = title.dimmed();
            }
            #[allow(
                clippy::cast_possible_truncation,
                reason = "a header row measure fits f32"
            )]
            let (gutter, height) = (m.gutter as f32, m.height as f32);
            let mut row = Stack::row()
                .align(Align::Center)
                .push(Box::new(Spacer::fixed(gutter)))
                .push(Box::new(title));
            row = row.push_slot(slot(empty(), Fit::fill(1.0)));
            let mut fit = Fit::fill(1.0);
            fit.height.fill = None;
            fit.height.min = Some(Length::Dp(height));
            fit.height.max = Some(Length::Dp(height));
            let mut placed = Stack::stack().fill_width();
            placed = placed.push_slot(slot(Box::new(row), fit));
            Box::new(placed)
        };
        if let Some(column) = self.column() {
            let _ = column.replace(OWN_BAR, own);
        }
    }

    /// The bar's row: the content's header row when it holds the bar, else the frame's own.
    fn bar_row(&self, m: &HeaderRowMetrics) -> Rect {
        self.merged
            .then(|| {
                self.layers
                    .children()
                    .first()
                    .and_then(|column| crate::widget::window_header::window_bar_slot(*column))
                    .map(|slot| slot.row)
            })
            .flatten()
            .unwrap_or_else(|| {
                Rect::new(
                    self.rect.x0,
                    self.rect.y0,
                    self.rect.x1,
                    self.rect.y0 + m.height,
                )
            })
    }

    /// How far the controls move from the frame's top trailing corner, where the layers put
    /// them, to stand on the bar's row, `inset` in from its trailing end, as the header row's
    /// own buttons stand from theirs. Both ends are laid out on device pixels, so the move is
    /// too.
    fn controls_offset(&self, m: &HeaderRowMetrics) -> Vec2 {
        let Some(controls) = self.layers.children().get(CONTROLS).map(|c| c.get_rect()) else {
            return Vec2::ZERO;
        };
        let row = self.bar_row(m);
        Vec2::new(
            row.x1.min(self.rect.x1) - m.inset - controls.x1,
            row.y0 - controls.y0,
        )
    }
}

impl Decorator for WindowFrame {
    type Inner = Layers;

    fn inner(&self) -> &Layers {
        &self.layers
    }

    fn inner_mut(&mut self) -> &mut Layers {
        &mut self.layers
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let m = HeaderRowMetrics::resolve(theme);
        self.offer_bar(&m);
        // The controls stand in a row as tall as the bar's, centred in it.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a header row measure fits f32"
        )]
        let height = Length::Dp(m.height as f32);
        let mut fit = Fit::hug();
        fit.height.min = Some(height);
        fit.height.max = Some(height);
        self.layers
            .set_placement(CONTROLS, fit, 1, Some(At::TopTrailing));
        self.metrics = Some(m);
        self.relayout = false;
        self.layers.build_layout(taffy, theme, node_map)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.layers.apply_layout(taffy, node, origin);
        self.rect = self.layers.get_rect();
        if let Some(m) = self.metrics {
            let by = self.controls_offset(&m);
            if by != Vec2::ZERO {
                self.layers.offset_child(taffy, CONTROLS, by);
            }
        }
        let width = self
            .layers
            .children()
            .get(CONTROLS)
            .map_or(0.0, |c| c.get_rect().width());
        if (width - self.controls_width).abs() > 0.5 {
            self.controls_width = width;
            self.relayout = true;
        }
    }

    fn needs_redraw(&self) -> bool {
        self.relayout || self.layers.needs_redraw()
    }

    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        self.layers.collect_damage(regions);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let rect = self.rect;
        if rect.width() <= 0.0 || rect.height() <= 0.0 {
            return;
        }
        let radius = SurfaceLevel::Window.radius(theme).min(rect.height() / 2.0);
        // The window in front stands higher than the ones behind it: the theme's floating
        // shadow under its own window shadow. One behind keeps the window's soft shadow alone,
        // the lighter of the two, as a desktop shows which window has the keyboard.
        if self.focused {
            crate::surface::floating_shadow(scene, transform, rect, radius, 1.0, theme);
        }
        let on = paint_surface(scene, transform, rect, SurfaceLevel::Window, theme);
        scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            transform,
            &RoundedRect::from_rect(rect, radius),
        );
        self.layers.draw(scene, transform, text, &on, presence);
        scene.pop_layer();
        // A shadow barely shows on a dark ground, so the window in front is told apart by its
        // edge too: the theme's edge of the window with the keyboard, heavier than the rest
        // edge the surface drew, on the same outline. A theme that draws its own border keeps
        // that one.
        if self.focused
            && crate::surface::border(theme).is_none()
            && theme.finish(crate::tokens::SurfaceRole::Window).hairline()
        {
            scene.stroke(
                &vello::kurbo::Stroke::new(1.0),
                transform,
                theme.tokens.colors.window_edge_focused(),
                None,
                &RoundedRect::from_rect(rect.inflate(0.5, 0.5), radius + 0.5),
            );
        }
    }

    /// What the window holds is cut at its edge: a list scrolled past its top is not seen over
    /// the window behind it, and a press there does not reach it.
    fn child_clip_rect(&self) -> Option<Rect> {
        (self.rect.width() > 0.0).then_some(self.rect)
    }

    fn take_structure_dirty(&mut self) -> bool {
        let relayout = std::mem::take(&mut self.relayout);
        self.layers.take_structure_dirty() || relayout
    }

    fn take_structure_change(&mut self) -> crate::widget::StructureChange {
        Widget::take_structure_change(&mut self.layers)
    }

    fn name(&self) -> &'static str {
        "WindowFrame"
    }

    /// The frame is a group named for the window, and what it holds is read inside it.
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
    use crate::component::Described;

    /// The window drawn on its ground at `rect`, rendered: what a person sees around its edge.
    fn rendered(theme: &Theme, focused: bool, rect: Rect) -> Vec<u8> {
        let mut frame = WindowFrame::from_props(
            "w",
            WindowFrameProps {
                name: Some("A window".to_owned()),
                focused,
                ..WindowFrameProps::default()
            },
        );
        frame.rect = rect;
        let mut scene = Scene::new();
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            theme.tokens.colors.surfaces.base,
            None,
            &Rect::new(0.0, 0.0, 240.0, 240.0),
        );
        let mut text = crate::font_catalog::fixture_renderer();
        Widget::draw(
            &mut frame,
            &mut scene,
            Affine::IDENTITY,
            &mut text,
            theme,
            &PresenceCtx::empty(),
        );
        crate::offscreen::OffscreenRenderer::new(240, 240)
            .render(&scene, vello::peniko::Color::BLACK)
    }

    /// How far the pixel at (`x`, `y`) stands from the ground, in relative luminance.
    fn from_ground(rgba: &[u8], theme: &Theme, x: u32, y: u32) -> f64 {
        let (r, g, b, _) = crate::offscreen::OffscreenRenderer::sample(rgba, 240, x, y);
        let seen = vello::peniko::Color::from_rgb8(r, g, b);
        (crate::a11y::relative_luminance(seen)
            - crate::a11y::relative_luminance(theme.tokens.colors.surfaces.base))
        .abs()
    }

    #[test]
    fn the_window_in_front_stands_apart_from_the_ground_by_its_edge_and_its_shadow() {
        let rect = Rect::new(40.0, 40.0, 200.0, 160.0);
        for (theme, dark) in [(Theme::dark(), true), (Theme::light(), false)] {
            let front = rendered(&theme, true, rect);
            let behind = rendered(&theme, false, rect);
            // The pixel row just outside the top edge, away from the corners: the edge.
            let edge = |rgba: &[u8]| from_ground(rgba, &theme, 120, 39);
            assert!(
                edge(&front) > edge(&behind) + crate::tokens::colors::HAIRLINE_MIN_DELTA,
                "the window in front has the heavier edge: {} over {}",
                edge(&front),
                edge(&behind)
            );
            assert!(
                edge(&behind) >= crate::tokens::colors::HAIRLINE_MIN_DELTA / 2.0,
                "a window behind still has an edge on the ground: {}",
                edge(&behind)
            );
            // Below the window, past its edge: the shadow, lighter under a window behind (on a
            // dark ground both may be too faint to tell apart, which is why the edge is there).
            let shadow = |rgba: &[u8]| from_ground(rgba, &theme, 120, 172);
            let lighter = if dark {
                shadow(&front) >= shadow(&behind)
            } else {
                shadow(&front) > shadow(&behind)
            };
            assert!(
                lighter,
                "a window behind casts the lighter shadow: {} under {}",
                shadow(&behind),
                shadow(&front)
            );
        }
    }
}
