//! `FloatingPanel`: the one body of a floating surface with a header slot, a body and a footer
//! slot (`docs/design-language.md`, "Primitives").

use std::collections::HashMap;

use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;

use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::{TaffyTreeExt, Widget};

use super::SurfaceLevel;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FloatingPanelLayout {
    pub panel: Rect,
    pub header: Rect,
    pub rule: Option<Rect>,
    pub body: Rect,
    pub footer: Rect,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FloatingPanelFace {
    header: f64,
    footer: f64,
    body_empty: bool,
}

impl FloatingPanelFace {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A header slot whose content is `height` tall (a field: its control
    /// height).
    #[must_use]
    pub fn header(mut self, height: f64) -> Self {
        self.header = height.max(0.0);
        self
    }

    /// A footer slot whose content is `height` tall.
    #[must_use]
    pub fn footer(mut self, height: f64) -> Self {
        self.footer = height.max(0.0);
        self
    }

    #[must_use]
    pub fn body_empty(mut self, empty: bool) -> Self {
        self.body_empty = empty;
        self
    }

    #[must_use]
    pub fn slot_inset(theme: &Theme) -> f64 {
        f64::from(theme.tokens.spacing.rhythm.related)
    }

    #[must_use]
    pub fn body_inset(theme: &Theme) -> f64 {
        crate::widget::floating_surface::menu_inset(theme)
    }

    #[must_use]
    pub fn header_extent(&self, theme: &Theme) -> f64 {
        if self.header > 0.0 {
            self.header + 2.0 * Self::slot_inset(theme)
        } else {
            0.0
        }
    }

    #[must_use]
    pub fn footer_extent(&self, theme: &Theme) -> f64 {
        if self.footer > 0.0 {
            self.footer + 2.0 * Self::slot_inset(theme)
        } else {
            0.0
        }
    }

    #[must_use]
    pub fn height_for(&self, body: f64, theme: &Theme) -> f64 {
        let body = if self.body_empty || body <= 0.0 {
            0.0
        } else {
            body + 2.0 * Self::body_inset(theme)
        };
        self.header_extent(theme) + body + self.footer_extent(theme)
    }

    /// Lay the parts out in `panel`.
    #[must_use]
    pub fn layout(&self, panel: Rect, theme: &Theme) -> FloatingPanelLayout {
        let slot = Self::slot_inset(theme);
        let inset = Self::body_inset(theme);
        let header_h = self.header_extent(theme);
        let footer_h = self.footer_extent(theme);
        let header = if header_h > 0.0 {
            Rect::new(
                panel.x0 + slot,
                panel.y0 + slot,
                panel.x1 - slot,
                panel.y0 + slot + self.header,
            )
        } else {
            Rect::ZERO
        };
        let body_top = panel.y0 + header_h;
        let body_bottom = panel.y1 - footer_h;
        let rule = (header_h > 0.0 && !self.body_empty && body_bottom > body_top)
            .then(|| Rect::new(panel.x0, body_top - 0.5, panel.x1, body_top + 0.5));
        let body = if self.body_empty {
            Rect::new(panel.x0, body_top, panel.x1, body_top)
        } else {
            Rect::new(
                panel.x0 + inset,
                body_top + inset,
                panel.x1 - inset,
                (body_bottom - inset).max(body_top + inset),
            )
        };
        let footer = if footer_h > 0.0 {
            Rect::new(
                panel.x0 + slot,
                panel.y1 - footer_h + slot,
                panel.x1 - slot,
                panel.y1 - slot,
            )
        } else {
            Rect::ZERO
        };
        FloatingPanelLayout {
            panel,
            header,
            rule,
            body,
            footer,
        }
    }

    /// Paint the panel in `panel` (its shadow, floating ground, inner hairline and the rule
    /// under the header) and return its layout and the theme its contents paint with.
    pub fn paint(
        &self,
        scene: &mut Scene,
        transform: Affine,
        panel: Rect,
        shadow: f32,
        theme: &Theme,
    ) -> (FloatingPanelLayout, Theme) {
        let radius = SurfaceLevel::Floating
            .radius(theme)
            .min(panel.height() / 2.0);
        crate::surface::floating_shadow(scene, transform, panel, radius, shadow, theme);
        self.paint_body(scene, transform, panel, theme)
    }

    /// [`Self::paint`] without the shadow, for a host that paints and fades
    /// the shadow itself (the shell's palette arriving).
    pub fn paint_body(
        &self,
        scene: &mut Scene,
        transform: Affine,
        panel: Rect,
        theme: &Theme,
    ) -> (FloatingPanelLayout, Theme) {
        let layout = self.layout(panel, theme);
        let radius = SurfaceLevel::Floating
            .radius(theme)
            .min(panel.height() / 2.0);
        crate::surface::panel(scene, transform, panel, radius, theme);
        let ground = SurfaceLevel::Floating.color(theme);
        if let Some(rule) = layout.rule {
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                crate::surface::edge_on(ground),
                None,
                &rule,
            );
        }
        (layout, theme.on_ground(ground))
    }
}

#[crate::component::not_a_component(
    part,
    "the panel `ui.FloatingPanel` is built from, around the view's header, body and footer"
)]
pub struct FloatingPanel {
    header: Option<Box<dyn Widget>>,
    body: Box<dyn Widget>,
    footer: Option<Box<dyn Widget>>,
    width: Option<f32>,
    fill_width: bool,
    fill_height: bool,
    /// The height the panel's window should be, when it is the whole of a surface that fits its
    /// content ([`Self::fit_height`]): what its parts need, followed on the expand/collapse motion.
    fit: Option<Fit>,
    rect: Rect,
    header_rect: Rect,
    body_rect: Rect,
    /// The panel arriving where it stands (`motion::Placed`): a panel a view places because the
    /// person asked for it comes in, and one the window's first paint shows rests.
    placed: crate::motion::Placed,
}

#[derive(Default)]
struct Fit {
    wanted: f64,
    extent: crate::motion::Extent,
}

fn embed(widget: &mut dyn Widget) {
    if let Some(any) = widget.as_any_mut() {
        if let Some(field) = any.downcast_mut::<crate::widget::SearchField>() {
            field.set_embedded(true);
        } else if let Some(input) = any.downcast_mut::<crate::widget::TextInput>() {
            input.set_plain_chrome(true);
        }
    }
}

impl FloatingPanel {
    /// A panel around `body`.
    #[must_use]
    pub fn new(body: Box<dyn Widget>) -> Self {
        let placed = if crate::motion::window_has_shown() {
            crate::motion::Placed::closed()
        } else {
            crate::motion::Placed::opened()
        };
        Self {
            header: None,
            body,
            footer: None,
            width: None,
            fill_width: false,
            fill_height: false,
            fit: None,
            rect: Rect::ZERO,
            header_rect: Rect::ZERO,
            body_rect: Rect::ZERO,
            placed,
        }
    }

    /// Put `header` in the header slot. A field there renders embedded.
    #[must_use]
    pub fn header(mut self, mut header: Box<dyn Widget>) -> Self {
        embed(header.as_mut());
        self.header = Some(header);
        self
    }

    /// Put `footer` in the footer slot.
    #[must_use]
    pub fn footer(mut self, footer: Box<dyn Widget>) -> Self {
        self.footer = Some(footer);
        self
    }

    #[must_use]
    pub fn width(mut self, px: f32) -> Self {
        self.set_width(Some(px));
        self
    }

    /// [`Self::width`] after construction; `None` takes its content's width.
    pub fn set_width(&mut self, px: Option<f32>) {
        self.width = px;
    }

    /// Take the width of the room it is offered rather than the width of
    /// its content: a panel that is the whole of its surface, as a popover's
    /// is, so a long row never stands past the window's edge.
    #[must_use]
    pub fn fill_width(mut self) -> Self {
        self.fill_width = true;
        self
    }

    /// Take the height of the room it is offered rather than the height of
    /// its content: a panel whose surface fixes its footprint, so a page that
    /// shows less never moves the panel's edges and one that shows more
    /// scrolls inside the body it was given.
    #[must_use]
    pub fn fill_height(mut self) -> Self {
        self.fill_height = true;
        self
    }

    /// Be the whole of a surface whose height follows the panel's content: the panel takes the
    /// height it is offered, as [`Self::fill_height`] does, and asks the window for the height
    /// its parts need ([`Widget::preferred_window_size`]), moving there on the expand/collapse
    /// motion when its page changes.
    #[must_use]
    pub fn fit_height(mut self) -> Self {
        self.fill_height = true;
        self.fit = Some(Fit::default());
        self
    }

    pub(crate) fn header_mut(&mut self) -> &mut Option<Box<dyn Widget>> {
        &mut self.header
    }

    pub(crate) fn body_mut(&mut self) -> &mut Box<dyn Widget> {
        &mut self.body
    }

    pub(crate) fn footer_mut(&mut self) -> &mut Option<Box<dyn Widget>> {
        &mut self.footer
    }

    pub(crate) const fn has_header(&self) -> bool {
        self.header.is_some()
    }

    pub(crate) const fn has_footer(&self) -> bool {
        self.footer.is_some()
    }

    pub(crate) fn embed_header(header: &mut dyn Widget) {
        embed(header);
    }

    /// How its body's page is drawn taller than laid out, while a band opens or closes in it
    /// ([`crate::widget::Disclosing`]); the body's scroll view keeps its own box, so the page is
    /// asked through it.
    fn body_growth(&self) -> Option<crate::widget::Disclosing> {
        match self
            .body
            .as_any()
            .and_then(|any| any.downcast_ref::<crate::widget::Scrollable>())
        {
            Some(scroll) => scroll.content_disclosing(),
            None => self.body.disclosing(),
        }
    }

    fn body_is_empty(&self) -> bool {
        self.body_rect.height() <= 0.5
    }

    fn moving(&self) -> bool {
        let now = crate::motion::frame_now();
        self.placed.is_moving(now) || self.refitting(now)
    }

    /// Whether the height the panel fits is on its way from one page's to another's: its
    /// window then stands at the taller of the two ([`Widget::preferred_window_size`]) and the
    /// card moves inside it ([`Self::card`]).
    fn refitting(&self, now: crate::time::Instant) -> bool {
        self.fit
            .as_ref()
            .is_some_and(|fit| fit.extent.is_moving(now) || self.body_growth().is_some())
    }

    /// The card as it is drawn this frame: the whole of the panel, but in a window that stands
    /// taller than the page it fits (while it refits, or until the window takes the page's
    /// height), the card's height on its motion.
    fn card(&self, now: crate::time::Instant) -> Rect {
        match &self.fit {
            Some(fit)
                if fit.extent.is_known() && fit.extent.get(now) < self.rect.height() - 0.5 =>
            {
                let height = fit.extent.get(now).max(0.0);
                Rect::new(
                    self.rect.x0,
                    self.rect.y0,
                    self.rect.x1,
                    self.rect.y0 + height,
                )
            }
            _ => self.rect,
        }
    }

    /// How far above its laid-out place the footer is drawn: it stands at the card's foot.
    fn footer_shift(&self, card: Rect) -> f64 {
        card.y1 - self.rect.y1
    }

    fn slots_mut(&mut self) -> impl Iterator<Item = &mut Box<dyn Widget>> {
        self.header
            .iter_mut()
            .chain(std::iter::once(&mut self.body))
            .chain(self.footer.iter_mut())
    }

    fn slots_frame(&self) -> bool {
        self.header
            .iter()
            .chain(std::iter::once(&self.body))
            .chain(self.footer.iter())
            .any(|slot| slot.needs_animation_frame())
    }

    fn slots_redraw(&self) -> bool {
        self.header
            .iter()
            .chain(std::iter::once(&self.body))
            .chain(self.footer.iter())
            .any(|slot| slot.needs_redraw())
    }

    /// The panel itself, drawn where `rect` says: what [`Widget::draw`] draws once the panel is
    /// posed where its arrival has reached.
    fn paint_panel(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        card: Rect,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let header = if self.header.is_some() {
            self.header_rect.height()
        } else {
            0.0
        };
        let footer = self.footer.as_ref().map_or(0.0, |f| f.get_rect().height());
        let face = FloatingPanelFace::new()
            .header(header)
            .footer(footer)
            .body_empty(self.body_is_empty());
        let (_, inner) = face.paint(scene, transform, card, 1.0, theme);
        let footer_at = transform * Affine::translate((0.0, self.footer_shift(card)));
        let footer = self.footer.is_some();
        let slots = self.slots_mut().count();
        for (index, slot) in self.slots_mut().enumerate() {
            let at = if footer && index + 1 == slots {
                footer_at
            } else {
                transform
            };
            crate::widget::draw_child(slot.as_mut(), scene, at, text_renderer, &inner, presence);
        }
    }
}

impl Widget for FloatingPanel {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let slot = crate::num::f32_of_f64(FloatingPanelFace::slot_inset(theme));
        let inset = crate::num::f32_of_f64(FloatingPanelFace::body_inset(theme));
        let wrap = |taffy: &mut TaffyTree<()>, child: NodeId, pad: f32, grow: f32| {
            taffy.parent_node(
                Style {
                    display: taffy::Display::Flex,
                    flex_direction: taffy::FlexDirection::Column,
                    padding: crate::widget::layout::insets_all(pad),
                    flex_shrink: 0.0,
                    flex_grow: grow,
                    ..Default::default()
                },
                &[child],
            )
        };
        let mut children = Vec::new();
        if let Some(header) = &mut self.header {
            let node = header.build_layout(taffy, theme, node_map);
            children.push(wrap(taffy, node, slot, 0.0));
        }
        let body = self.body.build_layout(taffy, theme, node_map);
        children.push(wrap(
            taffy,
            body,
            inset,
            if self.fill_height { 1.0 } else { 0.0 },
        ));
        if let Some(footer) = &mut self.footer {
            let node = footer.build_layout(taffy, theme, node_map);
            children.push(wrap(taffy, node, slot, 0.0));
        }
        let style = Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            flex_shrink: 0.0,
            size: taffy::Size {
                width: if self.fill_width {
                    taffy::Dimension::percent(1.0)
                } else {
                    self.width
                        .map_or(taffy::Dimension::auto(), taffy::Dimension::length)
                },
                height: if self.fill_height {
                    taffy::Dimension::percent(1.0)
                } else {
                    taffy::Dimension::auto()
                },
            },
            ..Default::default()
        };
        taffy.widget_node(style, &children, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        let wrappers = taffy.children(node).unwrap_or_default();
        let has_header = self.header.is_some();
        let origin = Point::new(self.rect.x0, self.rect.y0);
        let mut rects = Vec::new();
        for (slot, wrapper) in self.slots_mut().zip(wrappers) {
            let wrap_rect = crate::widget::layout_rect(taffy, wrapper, origin);
            if let Some(&child) = taffy.children(wrapper).unwrap_or_default().first() {
                slot.apply_layout(taffy, child, Point::new(wrap_rect.x0, wrap_rect.y0));
                rects.push(slot.get_rect());
            }
        }
        let mut rects = rects.into_iter();
        self.header_rect = if has_header {
            rects.next().unwrap_or(Rect::ZERO)
        } else {
            Rect::ZERO
        };
        self.body_rect = rects.next().unwrap_or(Rect::ZERO);
        if let Some(fit) = &mut self.fit
            && let Some(scroll) = self.body.primary_vertical_scroll()
        {
            let beyond = scroll.metrics.content - scroll.metrics.viewport;
            fit.wanted = (self.rect.height() + beyond).max(1.0).ceil();
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
        let now = crate::motion::frame_now();
        let arriving = self.placed.is_moving(now);
        // A section opening or closing in the page is drawn taller or shorter than it is laid
        // out, on its own motion: the card goes with its edge, so what follows it moves down
        // inside a card growing under it, rather than after it on a spring of its own.
        let growth = self.body_growth().map(|moving| moving.follower_offset(0.0));
        if let Some(fit) = &mut self.fit {
            if arriving || growth.is_some() {
                fit.extent.snap(fit.wanted + growth.unwrap_or(0.0));
            } else {
                fit.extent.follow(
                    fit.wanted,
                    theme.motion(),
                    crate::motion::input_source(),
                    now,
                );
            }
        }
        // A window fitted to its page is resized under it, not moved: the panel stands where the
        // window does.
        if self.fit.is_some() {
            self.placed.carry(self.rect);
        }
        self.placed.follow(
            self.rect,
            true,
            theme.motion(),
            crate::motion::input_source(),
            now,
        );
        let card = self.card(now);
        let pose = self.placed.pose(card, now);
        let bounds = card.union(pose.drawn_rect(card));
        pose.paint(scene, transform, bounds, |scene, transform| {
            self.paint_panel(scene, transform, card, text_renderer, theme, presence);
        });
    }

    /// Whether the panel is arriving: what asks for frames while it does, and what its slots
    /// ask for. Nothing at rest.
    fn needs_animation_frame(&self) -> bool {
        self.moving() || self.slots_frame()
    }

    fn needs_redraw(&self) -> bool {
        self.moving() || self.slots_redraw()
    }

    fn preferred_window_size(&self) -> Option<(f64, f64)> {
        let fit = self.fit.as_ref()?;
        let now = crate::motion::frame_now();
        // While the card refits, the window stands at the taller of where it comes from and
        // where it goes, in one step: a window resized each frame would trail the card (a size
        // is a round trip to the desk and a frame drawn whole), cutting off what follows a
        // section opening.
        let height = if !fit.extent.is_known() || self.placed.is_moving(now) {
            fit.wanted
        } else if self.refitting(now) {
            let reach = self.body_growth().map_or(0.0, |moving| moving.reach(0.0));
            fit.extent
                .get(now)
                .max(fit.wanted + reach)
                .max(self.rect.height())
        } else {
            fit.extent.get(now)
        };
        (self.rect.width() > 0.0 && height > 0.0)
            .then(|| (self.rect.width(), height.max(1.0).round()))
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        let wrappers = ctx.taffy.children(ctx.node_id).unwrap_or_default();
        let shift = self.footer_shift(self.card(crate::motion::frame_now()));
        let footer = self.footer.is_some();
        let slots = wrappers.len();
        for (index, (slot, wrapper)) in self.slots_mut().zip(wrappers).enumerate() {
            if *ctx.handled {
                return;
            }
            if let Some(&child) = ctx.taffy.children(wrapper).unwrap_or_default().first() {
                let event = if footer && index + 1 == slots {
                    crate::widget::disclosing::event_offset(
                        event,
                        vello::kurbo::Vec2::new(0.0, shift),
                        ctx.scale_factor,
                    )
                } else {
                    std::borrow::Cow::Borrowed(event)
                };
                slot.handle_event(&event, &mut ctx.reborrow(ctx.path, child));
            }
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        let mut out: Vec<&dyn Widget> = Vec::new();
        if let Some(h) = &self.header {
            out.push(h.as_ref());
        }
        out.push(self.body.as_ref());
        if let Some(f) = &self.footer {
            out.push(f.as_ref());
        }
        out
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        for child in self.children() {
            visitor(child);
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.slots_mut()
            .map(|b| b.as_mut() as &mut (dyn Widget + '_))
            .collect()
    }

    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        // A card refitting inside its window changes its own edge and moves its footer.
        if self.refitting(crate::motion::frame_now()) {
            regions.push(self.rect);
        }
        for child in self.children() {
            child.collect_damage(regions);
        }
    }

    fn mark_drawn(&mut self) {
        for slot in self.slots_mut() {
            slot.mark_drawn();
        }
    }

    fn take_structure_dirty(&mut self) -> bool {
        let mut dirty = false;
        for slot in self.slots_mut() {
            dirty |= slot.take_structure_dirty();
        }
        dirty
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn name(&self) -> &'static str {
        "FloatingPanel"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_header_rule_shows_only_with_a_body() {
        let theme = Theme::dark();
        let panel = Rect::new(0.0, 0.0, 400.0, 300.0);
        let with_body = FloatingPanelFace::new().header(32.0).layout(panel, &theme);
        assert!(
            with_body.rule.is_some(),
            "a body under the header gets a rule"
        );
        let alone = FloatingPanelFace::new()
            .header(32.0)
            .body_empty(true)
            .layout(panel, &theme);
        assert!(alone.rule.is_none(), "a header standing alone has no rule");
        assert!((with_body.header.x0 - 8.0).abs() < 1e-6);
        assert!((with_body.body.x0 - 8.0).abs() < 1e-6);
        assert!((with_body.header.height() - 32.0).abs() < 1e-6);
    }

    #[test]
    fn the_panel_is_as_tall_as_its_parts() {
        let theme = Theme::dark();
        let face = FloatingPanelFace::new().header(32.0).footer(28.0);
        // header 32 + 2·8, body 100 + 2·8, footer 28 + 2·8.
        assert!((face.height_for(100.0, &theme) - (48.0 + 116.0 + 44.0)).abs() < 1e-6);
        let empty = face.body_empty(true);
        assert!((empty.height_for(100.0, &theme) - (48.0 + 44.0)).abs() < 1e-6);
    }

    #[test]
    fn a_field_in_the_header_slot_is_embedded() {
        let field = crate::widget::TextInput::local();
        let panel = FloatingPanel::new(Box::new(crate::widget::Spacer::fixed(20.0)))
            .header(Box::new(field));
        let header = panel.header.as_ref().map(|h| h.as_ref());
        let plain = header
            .and_then(|h| h.as_any())
            .and_then(|a| a.downcast_ref::<crate::widget::TextInput>())
            .is_some_and(crate::widget::TextInput::is_plain);
        assert!(plain, "no fill and no outline of its own");
    }

    /// A panel a view places after the window has shown comes in over the frames between, and
    /// rests asking for no frames; a still theme lands it at once.
    #[test]
    fn a_panel_comes_in_where_it_stands_and_rests() {
        let _frame = crate::motion::pin_frame();
        let mut panel = FloatingPanel::new(Box::new(crate::widget::Spacer::new()));
        let now = crate::motion::frame_now();
        let rect = Rect::new(0.0, 0.0, 360.0, 300.0);
        panel.placed.follow(
            rect,
            true,
            Theme::dark().motion(),
            crate::motion::input_source(),
            now,
        );
        assert!(!panel.placed.is_moving(now));

        panel.placed.snap(rect, false);
        panel.placed.follow(
            rect,
            true,
            Theme::dark().motion(),
            crate::tokens::MotionSource::Pointer,
            now,
        );
        assert!(
            panel.placed.is_moving(now),
            "it comes in over the frames between"
        );
        assert!(
            panel.placed.settles_at().is_some(),
            "and it will arrive at a time of the motion's own"
        );
    }

    /// A card shrinking to a shorter page moves inside its window, which keeps its height until
    /// the card rests and then takes the card's in one step: resized each frame, the window
    /// would trail the card and cut off what it shows.
    #[test]
    fn a_shrinking_card_moves_inside_a_window_resized_once() {
        let start = crate::time::now();
        let room = Rect::new(0.0, 0.0, 380.0, 436.0);
        let mut panel = FloatingPanel::new(Box::new(crate::widget::Spacer::new())).fit_height();
        panel.rect = room;
        panel.placed.snap(room, false);
        let fit = panel.fit.as_mut().expect("it fits its page");
        fit.extent.snap(436.0);
        fit.wanted = 344.0;
        fit.extent.follow(
            344.0,
            Theme::dark().motion(),
            crate::tokens::MotionSource::Pointer,
            start,
        );
        let mut heights = Vec::new();
        for frame in 1..=60 {
            let _frame =
                crate::motion::pin_frame_at(start + std::time::Duration::from_millis(16 * frame));
            let card = panel.card(crate::motion::frame_now());
            let window = Widget::preferred_window_size(&panel).map(|(_, height)| height);
            assert!(
                card.height() <= room.height(),
                "the card stands in its window"
            );
            heights.push((window, card.height()));
        }
        let mut windows: Vec<_> = heights.iter().map(|(window, _)| *window).collect();
        windows.dedup();
        assert_eq!(
            windows,
            [Some(436.0), Some(344.0)],
            "the window keeps its height while the card moves, then takes the card's: {heights:?}"
        );
        assert!(
            heights
                .iter()
                .any(|(_, card)| *card < 436.0 && *card > 344.0),
            "the card moves inside it: {heights:?}"
        );
        assert!(
            heights
                .iter()
                .all(|(window, card)| *window != Some(344.0) || (*card - 344.0).abs() < 1e-6),
            "the window takes the card's height only once the card rests: {heights:?}"
        );
    }

    /// A panel arriving asks for the height of its page on its first frame, not the room it was
    /// given before: the frame a person first sees is fitted.
    #[test]
    fn a_panel_arriving_asks_for_the_height_of_its_page() {
        let _frame = crate::motion::pin_frame();
        let mut panel = FloatingPanel::new(Box::new(crate::widget::Spacer::new())).fit_height();
        let now = crate::motion::frame_now();
        let room = Rect::new(0.0, 0.0, 380.0, 577.0);
        panel.rect = room;
        let fit = panel.fit.as_mut().expect("it fits its page");
        fit.extent.follow(
            room.height(),
            Theme::dark().motion(),
            crate::tokens::MotionSource::Pointer,
            now,
        );
        fit.wanted = 180.0;
        panel.placed.snap(room, false);
        panel.placed.follow(
            room,
            true,
            Theme::dark().motion(),
            crate::tokens::MotionSource::Pointer,
            now,
        );
        assert!(panel.placed.is_moving(now));
        assert_eq!(
            Widget::preferred_window_size(&panel),
            Some((380.0, 180.0)),
            "the window it first shows in is as tall as its page"
        );
    }
}
