//! `FloatingPanel`: the one body of a floating surface with a header slot,
//! a body and a footer slot (`docs/design-language.md`, "Primitives"). The
//! palette, menus, Quick Settings, the collaborator menu and sheets are
//! built on it.
//!
//! - **Surface:** the floating level ([`super::SurfaceLevel::Floating`]):
//!   `radii.panel`, the opaque floating ground, the inner hairline and the
//!   two-layer shadow.
//! - **Header:** a field placed here renders embedded: no fill, no
//!   outline. The panel is its surface. A hairline runs under the header
//!   only while the body has something in it, so a palette with no query
//!   is one clean field.
//! - **Body:** rows inset by the menu inset, so a lifted row's corners run
//!   concentric with the panel's.
//! - **Footer:** separated by space, never boxed.
//!
//! [`FloatingPanelFace`] is the immediate layout and painter (the shell's
//! surfaces); [`FloatingPanel`] the retained widget.

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

/// Where a floating panel's parts stand, from [`FloatingPanelFace::layout`].
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct FloatingPanelLayout {
    /// The whole panel.
    pub panel: Rect,
    /// The header slot's content box (already inset), or zero.
    pub header: Rect,
    /// The hairline under the header, when it shows.
    pub rule: Option<Rect>,
    /// The body's content box: rows go here, edge to edge.
    pub body: Rect,
    /// The footer slot's content box, or zero.
    pub footer: Rect,
}

/// A floating panel's shape: how tall its header and footer are and
/// whether its body holds anything.
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

    /// Whether the body holds nothing: then the header stands alone, with
    /// no hairline under it.
    #[must_use]
    pub fn body_empty(mut self, empty: bool) -> Self {
        self.body_empty = empty;
        self
    }

    /// The inset of the header and footer content from the panel's edge.
    #[must_use]
    pub fn slot_inset(theme: &Theme) -> f64 {
        f64::from(theme.tokens.spacing.rhythm.related)
    }

    /// The inset of body rows from the panel's edge: the menu inset.
    #[must_use]
    pub fn body_inset(theme: &Theme) -> f64 {
        crate::widget::floating_surface::menu_inset(theme)
    }

    /// The height the header slot takes, padding included.
    #[must_use]
    pub fn header_extent(&self, theme: &Theme) -> f64 {
        if self.header > 0.0 {
            self.header + 2.0 * Self::slot_inset(theme)
        } else {
            0.0
        }
    }

    /// The height the footer slot takes, padding included.
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

    /// Paint the panel in `panel` (its shadow, floating ground, inner
    /// hairline and the rule under the header) and return its layout and
    /// the theme its contents paint with. `shadow` scales the shadow, for a
    /// panel still arriving.
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

/// A retained floating panel: a header slot, a body and a footer slot.
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
    /// person asked for it comes in, and one the window's first paint shows rests. Its motion is
    /// the library's (`WindowMotion::OPEN`), and a still theme or reduced motion lands it at once.
    placed: crate::motion::Placed,
}

/// The height a panel that fits its content wants, and the motion its window follows to it.
#[derive(Default)]
struct Fit {
    /// What its parts need, as of its last layout.
    wanted: f64,
    extent: crate::motion::Extent,
}

/// Make a field placed in a header slot embedded: no fill, no outline.
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
        // A panel a view places after the window has shown is one the person asked for (a
        // surface opening): it comes in. One the window's first frame shows rests.
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

    /// A fixed width, in logical pixels (a menu, a utility panel).
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
    /// height it is offered, as [`Self::fill_height`] does, and asks the window for the height its
    /// parts need ([`Widget::preferred_window_size`]), moving there on the expand/collapse motion
    /// when its page changes. A window that cannot be that tall leaves the body scrolling.
    #[must_use]
    pub fn fit_height(mut self) -> Self {
        self.fill_height = true;
        self.fit = Some(Fit::default());
        self
    }

    /// The header slot, to change what it holds in place. A field put there is embedded.
    pub(crate) fn header_mut(&mut self) -> &mut Option<Box<dyn Widget>> {
        &mut self.header
    }

    /// The body, to change it in place.
    pub(crate) fn body_mut(&mut self) -> &mut Box<dyn Widget> {
        &mut self.body
    }

    /// The footer slot, to change what it holds in place.
    pub(crate) fn footer_mut(&mut self) -> &mut Option<Box<dyn Widget>> {
        &mut self.footer
    }

    pub(crate) const fn has_header(&self) -> bool {
        self.header.is_some()
    }

    pub(crate) const fn has_footer(&self) -> bool {
        self.footer.is_some()
    }

    /// Make a field about to go in the header slot embedded.
    pub(crate) fn embed_header(header: &mut dyn Widget) {
        embed(header);
    }

    /// How much taller than laid out its body's page is drawn this frame, while a band opens or
    /// closes in it ([`crate::widget::Disclosing`]); the body's scroll view keeps its own box, so
    /// the page is asked through it.
    fn body_growth(&self) -> Option<f64> {
        let page = match self
            .body
            .as_any()
            .and_then(|any| any.downcast_ref::<crate::widget::Scrollable>())
        {
            Some(scroll) => scroll.content_disclosing(),
            None => self.body.disclosing(),
        };
        page.map(|moving| moving.follower_offset(0.0))
    }

    fn body_is_empty(&self) -> bool {
        self.body_rect.height() <= 0.5
    }

    /// Whether the panel arrives, or its window moves to the height its parts need.
    fn moving(&self) -> bool {
        let now = crate::motion::frame_now();
        self.placed.is_moving(now)
            || self
                .fit
                .as_ref()
                .is_some_and(|fit| fit.extent.is_moving(now))
    }

    fn slots_mut(&mut self) -> impl Iterator<Item = &mut Box<dyn Widget>> {
        self.header
            .iter_mut()
            .chain(std::iter::once(&mut self.body))
            .chain(self.footer.iter_mut())
    }

    /// Whether a slot of the panel asks for a frame of its own.
    fn slots_frame(&self) -> bool {
        self.header
            .iter()
            .chain(std::iter::once(&self.body))
            .chain(self.footer.iter())
            .any(|slot| slot.needs_animation_frame())
    }

    /// Whether a slot of the panel asks to be drawn again.
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
        let (_, inner) = face.paint(scene, transform, self.rect, 1.0, theme);
        for slot in self.slots_mut() {
            crate::widget::draw_child(
                slot.as_mut(),
                scene,
                transform,
                text_renderer,
                &inner,
                presence,
            );
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
        // A panel that takes the height it is offered gives the room its page
        // does not need to the body, so its footer stands on the panel's
        // bottom edge.
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
        // What the body has beyond the room it was given (or short of it) is what the panel's
        // window is short of (or has to spare).
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
        // A panel arriving is as tall as its page from its first frame: the window it shows in is
        // fitted, and no spring from the room it was given runs behind the person's eyes.
        let arriving = self.placed.is_moving(now);
        // A section opening or closing in the page is drawn taller or shorter than it is laid
        // out, on its own motion: the window goes with its edge, so what follows it moves down
        // inside a panel growing under it, rather than after it on a spring of its own.
        let growth = self.body_growth();
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
        let pose = self.placed.pose(self.rect, now);
        let bounds = self.rect.union(pose.drawn_rect(self.rect));
        pose.paint(scene, transform, bounds, |scene, transform| {
            self.paint_panel(scene, transform, text_renderer, theme, presence);
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

    /// The height its parts need, while it fits its window to them: the window follows it.
    fn preferred_window_size(&self) -> Option<(f64, f64)> {
        let fit = self.fit.as_ref()?;
        let now = crate::motion::frame_now();
        // The first measurement is the height it wants, and a panel arriving is that tall before
        // the frame following its arrival: the window it first shows in is fitted, not the room
        // it was given. A page that changes under a panel already up moves the window on its
        // spring.
        let height = if fit.extent.is_known() && !self.placed.is_moving(now) {
            fit.extent.get(now)
        } else {
            fit.wanted
        };
        (self.rect.width() > 0.0 && height > 0.0)
            .then(|| (self.rect.width(), height.max(1.0).round()))
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        let wrappers = ctx.taffy.children(ctx.node_id).unwrap_or_default();
        for (slot, wrapper) in self.slots_mut().zip(wrappers) {
            if *ctx.handled {
                return;
            }
            if let Some(&child) = ctx.taffy.children(wrapper).unwrap_or_default().first() {
                slot.handle_event(event, &mut ctx.reborrow(ctx.path, child));
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
        // The header content is inset by the slot inset; rows by the menu
        // inset, so they run concentric with the panel's corners.
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
            .and_then(|h| {
                // `as_any` gives a shared view of the same widget.
                h.as_any()
            })
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
        // Its first placement, at rest.
        panel.placed.follow(
            rect,
            true,
            Theme::dark().motion(),
            crate::motion::input_source(),
            now,
        );
        assert!(!panel.placed.is_moving(now));

        // Shown from closed, it comes in: at the frame it is asked for, it is on its way.
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
        // The page it showed before was the room; the one it is built for is shorter.
        fit.extent.follow(
            room.height(),
            Theme::dark().motion(),
            crate::tokens::MotionSource::Pointer,
            now,
        );
        fit.wanted = 180.0;
        // Shown from closed, it is on its way in.
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
