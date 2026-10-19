//! `DisclosureRow`: a list row whose trailing chevron turns and whose body
//! opens in place (`docs/design-language.md`, "Primitives").
//!
//! The row's height follows the body on the standard disclose motion
//! ([`crate::motion::Disclose`]): the container grows and the content fades
//! in as it is revealed; reduced motion lands at once. Rows that share a
//! [`DisclosureGroup`] keep one open at a time: opening one closes the
//! others.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::icons::Icon;
use crate::motion::{Disclose, Presence};
use crate::paint::PaintCtx;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::{TaffyTreeExt, Widget};

use super::TextStackFace;

/// Rows that keep one open at a time. Clone it into every row of the group.
#[derive(Clone, Debug, Default)]
pub struct DisclosureGroup {
    open: Rc<Cell<Option<u64>>>,
    next: Rc<Cell<u64>>,
}

impl DisclosureGroup {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn join(&self) -> u64 {
        let id = self.next.get();
        self.next.set(id + 1);
        id
    }
}

/// A row that opens its body in place.
pub struct DisclosureRow {
    title: String,
    secondary: String,
    leading: Option<Icon>,
    body: Box<dyn Widget>,
    open: bool,
    reveal: Presence,
    group: Option<(DisclosureGroup, u64)>,
    hovered: bool,
    pressed: bool,
    focused: bool,
    rect: Rect,
    header: Rect,
    /// The body's laid-out height when fully open.
    body_height: f64,
    dirty: bool,
    /// The motion the person allows, from the last layout.
    motion: crate::tokens::EffectiveMotion,
}

impl DisclosureRow {
    /// A closed row titled `title` over `body`.
    #[must_use]
    pub fn new(title: impl Into<String>, body: Box<dyn Widget>) -> Self {
        Self {
            title: title.into(),
            secondary: String::new(),
            leading: None,
            body,
            open: false,
            reveal: Presence::closed(),
            group: None,
            hovered: false,
            pressed: false,
            focused: false,
            rect: Rect::ZERO,
            header: Rect::ZERO,
            body_height: 0.0,
            dirty: true,
            motion: crate::tokens::EffectiveMotion::default(),
        }
    }

    /// A secondary line under the title.
    #[must_use]
    pub fn secondary(mut self, text: impl Into<String>) -> Self {
        self.secondary = text.into();
        self
    }

    /// A leading icon.
    #[must_use]
    pub fn icon(mut self, icon: Icon) -> Self {
        self.leading = Some(icon);
        self
    }

    /// Start open, at rest.
    #[must_use]
    pub fn opened(mut self) -> Self {
        self.open = true;
        self.reveal = Presence::opened();
        self
    }

    /// Keep one row of `group` open at a time.
    #[must_use]
    pub fn in_group(mut self, group: &DisclosureGroup) -> Self {
        let id = group.join();
        if self.open {
            group.open.set(Some(id));
        }
        self.group = Some((group.clone(), id));
        self
    }

    /// Whether the row is open (or opening).
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Open or close the row, on the disclose motion as the last input
    /// asked for it.
    pub fn set_open(&mut self, open: bool) {
        if self.open == open {
            return;
        }
        self.open = open;
        if let Some((group, id)) = &self.group {
            if open {
                group.open.set(Some(*id));
            } else if group.open.get() == Some(*id) {
                group.open.set(None);
            }
        }
        self.reveal.show(
            open,
            Disclose::SPEC,
            self.motion,
            crate::motion::input_source(),
            crate::time::now(),
        );
        if let Some(until) = self.reveal.settles_at() {
            crate::motion::want_frames_until(until);
        }
        self.dirty = true;
    }

    /// Close if another row of the group opened.
    fn follow_group(&mut self) {
        let other_opened = self
            .group
            .as_ref()
            .is_some_and(|(group, id)| self.open && group.open.get() != Some(*id));
        if other_opened {
            self.set_open(false);
        }
    }

    fn header_height(&self, theme: &Theme) -> f64 {
        let m = &theme.tokens.metrics;
        f64::from(if self.secondary.is_empty() {
            m.row_single
        } else {
            m.row_double
        })
    }

    fn progress(&self) -> f64 {
        self.reveal.progress(crate::time::now())
    }

    fn face(&self) -> TextStackFace<'_> {
        TextStackFace::new(&self.title).secondary(&self.secondary)
    }

    fn paint_header(&self, ctx: &mut PaintCtx<'_>) {
        let theme = ctx.theme;
        let colors = &theme.tokens.colors;
        let radius = f64::from(theme.tokens.radii.item);
        let bleed = crate::chrome::row_bleed(theme);
        let outer = self.header.inflate(bleed, 0.0);
        if self.hovered || self.pressed {
            let fill = colors.lift_on(
                colors.surfaces.content,
                if self.pressed { 1.0 } else { 0.5 },
            );
            ctx.scene.fill(
                vello::peniko::Fill::NonZero,
                ctx.transform,
                fill,
                None,
                &vello::kurbo::RoundedRect::from_rect(outer, radius),
            );
        }
        let m = &theme.tokens.metrics;
        let icon = f64::from(m.icon_control);
        let gap = f64::from(theme.tokens.spacing.rhythm.sibling);
        let cy = self.header.center().y;
        let mut x = self.header.x0;
        if let Some(leading) = self.leading {
            leading.draw(
                ctx.scene,
                ctx.transform,
                Rect::new(x, cy - icon / 2.0, x + icon, cy + icon / 2.0),
                colors.text_dim,
            );
            x += icon + gap;
        }
        let chevron = Rect::new(
            self.header.x1 - icon,
            cy - icon / 2.0,
            self.header.x1,
            cy + icon / 2.0,
        );
        self.face().paint(
            ctx,
            Rect::new(x, self.header.y0, chevron.x0 - gap, self.header.y1),
        );
        // The chevron turns a quarter, from pointing at the row's end to
        // pointing down, with the reveal.
        let turn = Disclose::reveal(self.progress(), 1.0).turn;
        let centre = chevron.center();
        let rotated = ctx.transform
            * Affine::translate(centre.to_vec2())
            * Affine::rotate(turn * std::f64::consts::FRAC_PI_2)
            * Affine::translate(-centre.to_vec2());
        Icon::ChevronRight.draw(ctx.scene, rotated, chevron, colors.text_dim);
        if self.focused {
            crate::widget::paint_focus_ring(ctx.scene, ctx.transform, outer, radius, theme);
        }
    }
}

impl Widget for DisclosureRow {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.motion = theme.motion();
        self.follow_group();
        let header = taffy.leaf_node(Style {
            size: taffy::Size {
                width: taffy::Dimension::percent(1.0),
                height: taffy::Dimension::length(crate::num::f32_of_f64(self.header_height(theme))),
            },
            flex_shrink: 0.0,
            ..Default::default()
        });
        let body = self.body.build_layout(taffy, theme, node_map);
        let revealed = Disclose::reveal(self.progress(), self.body_height).height;
        let clip = taffy.parent_node(
            Style {
                display: taffy::Display::Flex,
                flex_direction: taffy::FlexDirection::Column,
                flex_shrink: 0.0,
                size: taffy::Size {
                    width: taffy::Dimension::percent(1.0),
                    height: taffy::Dimension::length(crate::num::f32_of_f64(revealed)),
                },
                overflow: taffy::Point {
                    x: taffy::Overflow::Visible,
                    y: taffy::Overflow::Hidden,
                },
                ..Default::default()
            },
            &[body],
        );
        let style = Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            flex_shrink: 0.0,
            ..Default::default()
        };
        taffy.widget_node(style, &[header, clip], node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        let children = taffy.children(node).unwrap_or_default();
        let origin = Point::new(self.rect.x0, self.rect.y0);
        if let Some(&header) = children.first() {
            self.header = crate::widget::layout_rect(taffy, header, origin);
        }
        if let Some(&clip) = children.get(1) {
            let clip_rect = crate::widget::layout_rect(taffy, clip, origin);
            if let Some(&body) = taffy.children(clip).unwrap_or_default().first() {
                self.body
                    .apply_layout(taffy, body, Point::new(clip_rect.x0, clip_rect.y0));
                let full = self.body.get_rect().height();
                if (full - self.body_height).abs() > 0.5 {
                    self.body_height = full;
                    self.dirty = true;
                }
            }
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
        self.paint_header(&mut PaintCtx::new(scene, transform, text_renderer, theme));
        let progress = self.progress();
        if progress <= 0.0 {
            return;
        }
        let reveal = Disclose::reveal(progress, self.body_height);
        let clip = Rect::new(
            self.rect.x0 - crate::chrome::row_bleed(theme),
            self.header.y1,
            self.rect.x1 + crate::chrome::row_bleed(theme),
            self.header.y1 + reveal.height,
        );
        scene.push_layer(
            vello::peniko::Fill::NonZero,
            vello::peniko::BlendMode::default(),
            reveal.opacity,
            transform,
            &clip,
        );
        crate::widget::draw_child(
            self.body.as_mut(),
            scene,
            transform,
            text_renderer,
            theme,
            presence,
        );
        scene.pop_layer();
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                let over = self.header.contains(p);
                if over != self.hovered {
                    self.hovered = over;
                    self.dirty = true;
                }
                if !over {
                    self.pressed = false;
                }
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } if self.hovered => {
                match state {
                    ElementState::Pressed => self.pressed = true,
                    ElementState::Released if self.pressed => {
                        self.pressed = false;
                        self.set_open(!self.open);
                    }
                    ElementState::Released => {}
                }
                self.dirty = true;
                *ctx.handled = true;
                return;
            }
            WindowEvent::KeyboardInput { event, .. }
                if self.focused && event.state == ElementState::Pressed =>
            {
                let toggle = match &event.logical_key {
                    Key::Named(NamedKey::Enter | NamedKey::Space) => Some(!self.open),
                    Key::Named(NamedKey::ArrowRight) => Some(true),
                    Key::Named(NamedKey::ArrowLeft) => Some(false),
                    _ => None,
                };
                if let Some(open) = toggle {
                    self.set_open(open);
                    *ctx.handled = true;
                    return;
                }
            }
            _ => {}
        }
        if self.open
            && let Some(&clip) = ctx.taffy.children(ctx.node_id).unwrap_or_default().get(1)
            && let Some(&body) = ctx.taffy.children(clip).unwrap_or_default().first()
        {
            self.body
                .handle_event(event, &mut ctx.reborrow(ctx.path, body));
        }
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn set_focus(&mut self, focused: bool) {
        if self.focused != focused {
            self.focused = focused;
            self.dirty = true;
        }
    }

    fn is_focused(&self) -> bool {
        self.focused
    }

    fn needs_animation_frame(&self) -> bool {
        self.reveal.is_moving(crate::time::now()) || self.body.needs_animation_frame()
    }

    fn is_layout_dirty(&self) -> bool {
        self.dirty || self.reveal.is_moving(crate::time::now()) || self.body.is_layout_dirty()
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || self.reveal.is_moving(crate::time::now()) || self.body.needs_redraw()
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        self.body.mark_drawn();
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![self.body.as_ref()]
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        visitor(self.body.as_ref());
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![self.body.as_mut()]
    }

    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        if self.needs_redraw() {
            regions.push(self.rect);
        } else {
            self.body.collect_damage(regions);
        }
    }

    fn take_structure_dirty(&mut self) -> bool {
        self.body.take_structure_dirty()
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn name(&self) -> &'static str {
        "DisclosureRow"
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        use crate::a11y::{AccessNode, AccessState, Role};
        Some(
            AccessNode::new(Role::Button)
                .with_name(self.face().spoken())
                .with_bounds(self.header)
                .with_state(
                    AccessState::new()
                        .focusable(true)
                        .focused(self.focused)
                        .expanded(self.open),
                ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_row_of_a_group_is_open_at_a_time() {
        let _settled = crate::motion::testing::settled();
        let group = DisclosureGroup::new();
        let body = || Box::new(crate::widget::Spacer::fixed(40.0)) as Box<dyn Widget>;
        let mut a = DisclosureRow::new("Wi-Fi", body()).in_group(&group);
        let mut b = DisclosureRow::new("Bluetooth", body()).in_group(&group);
        a.set_open(true);
        assert!(a.is_open());
        b.set_open(true);
        a.follow_group();
        assert!(b.is_open() && !a.is_open(), "opening one closes the other");
        b.set_open(false);
        assert_eq!(group.open.get(), None);
    }

    #[test]
    fn a_row_is_a_single_or_two_line_row_tall() {
        let theme = Theme::dark();
        let body = Box::new(crate::widget::Spacer::fixed(40.0));
        let one = DisclosureRow::new("Wi-Fi", body);
        assert!((one.header_height(&theme) - 36.0).abs() < 1e-6);
        let two = DisclosureRow::new("Wi-Fi", Box::new(crate::widget::Spacer::fixed(40.0)))
            .secondary("Home");
        assert!((two.header_height(&theme) - 52.0).abs() < 1e-6);
        let compact = theme.with_density(crate::tokens::Density::Compact);
        assert!((one.header_height(&compact) - 28.0).abs() < 1e-6);
    }
}
