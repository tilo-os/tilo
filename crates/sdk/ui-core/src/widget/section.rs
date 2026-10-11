//! `Section`: a titled group of content on the ground, separated from the
//! next group by space, never boxed.
//!
//! A section is an intrinsic-height stack whose items sit `rhythm.sibling`
//! apart. Consecutive [`ListRow`]s are one list: they sit edge to edge with
//! an inset separator between them, hidden beside the selected row; their
//! badge columns share one width, and a run of selected rows is one lift.
//! The optional header carries the group's title (sentence case, in
//! `Heading` for a section of a page or `Subheading` for a group inside
//! one), a count of what it holds, and one trailing text action ("Clear
//! all", "Stage all"), `rhythm.related` above the first item. There is no
//! subtitle: a description goes in the content only when it tells the
//! person something they need to act on.
//!
//! **One content edge.** The header's title starts on the column's content
//! edge, like the rows' text under it; the action's label ends on the
//! trailing edge, its hover lift bleeding past it as a row's does. The
//! header is one fixed height whether or not it has an action.
//!
//! A [`Section::collapsible`] section is a disclosure: its header toggles
//! the items with a pointer, Enter or Space, and the arrow keys, and says
//! whether it is expanded.
//!
//! [`StatusLine`] and [`ObjectHeader`] are the two non-interactive parts a
//! section most often carries: a status with its dot, and the head of a
//! detail view.
//!
//! [`ListRow`]: crate::widget::ListRow

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

#[cfg(test)]
use automerge::AutoCommit;
use automerge::ObjId;
use taffy::style_helpers::{auto, length, percent};
use taffy::{NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Circle, Point, Rect};
use vello::peniko::Fill;
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use super::composition_parts::Parts;
use crate::a11y::{AccessNode, AccessState, Role};
use crate::chrome::{RowJoin, SelectionState, paint_row_selection, row_bleed};
use crate::icons::Icon;
use crate::paint::PaintCtx;
use crate::pointer_replay::{PressAt, Release};
use crate::presence::PresenceCtx;
use crate::text_render::{TextRenderer, TextStyle};
use crate::theme::Theme;
use crate::tokens::TextRole;
use crate::widget::list_row::RowStatus;
use crate::widget::{
    AvatarFace, Button, ButtonFace, ButtonRole, ControlSize, Element, Interaction, TaffyTreeExt,
    Text, Widget,
};

/// How prominent a section's title is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Level {
    /// A section of a page: `Heading`.
    #[default]
    Section,
    /// A group inside a section or a list section header: `Subheading`.
    Group,
}

impl Level {
    fn role(self) -> TextRole {
        match self {
            Self::Section => TextRole::Heading,
            Self::Group => TextRole::Subheading,
        }
    }

    fn heading_level(self) -> u8 {
        match self {
            Self::Section => 2,
            Self::Group => 3,
        }
    }
}

/// The geometry a section header has in one theme, shared by the retained
/// header and [`SectionHeaderFace`].
#[derive(Clone, Debug)]
struct HeaderMetrics {
    /// A small control's height: the header is at least this tall, with or
    /// without an action.
    control: f64,
    gap: f64,
    count: TextStyle,
    count_pad: f64,
    /// The action's label inset: the bleed its lift takes past the edge.
    action_pad: f64,
}

impl HeaderMetrics {
    fn new(theme: &Theme) -> Self {
        Self {
            control: ControlSize::Small.height(theme),
            gap: f64::from(theme.tokens.spacing.rhythm.related),
            count: theme.text(TextRole::Caption).numeric(),
            count_pad: f64::from(theme.tokens.spacing.s2),
            action_pad: row_bleed(theme),
        }
    }

    /// The header's height around a title line `line` tall: a small
    /// control's height, grown so the title stands a whole number of pixels
    /// below the top (it centres on the header's line), or the line itself
    /// once it is taller.
    fn height(&self, line: f64) -> f64 {
        if line >= self.control {
            line
        } else {
            line + 2.0 * ((self.control - line) / 2.0).ceil()
        }
    }

    fn count_size(&self, text: &mut TextRenderer, label: &str) -> (f64, f64) {
        let (w, h) = text.measure_style(label, None, &self.count);
        (
            f64::from(w) + 2.0 * self.count_pad,
            f64::from(h) + self.count_pad / 2.0,
        )
    }
}

/// Paint a count pill in `rect`: `Caption` in tabular figures on the raised
/// tier.
fn paint_count(ctx: &mut PaintCtx<'_>, rect: Rect, label: &str) {
    let theme = ctx.theme;
    crate::surface::card(
        ctx.scene,
        ctx.transform,
        rect,
        rect.height() / 2.0,
        theme.tokens.colors.surfaces.raised,
    );
    let style = theme.text(TextRole::Caption).numeric();
    let (w, h) = ctx.text.measure_style(label, None, &style);
    ctx.text(
        label,
        &style,
        theme.text_dim,
        Point::new(
            rect.center().x - f64::from(w) / 2.0,
            rect.center().y - f64::from(h) / 2.0,
        ),
        None,
    );
}

/// A section's header row.
pub struct SectionHeader {
    title: String,
    level: Level,
    count: Option<usize>,
    action: Option<Button>,
}

impl SectionHeader {
    /// A section of a page, titled in `Heading`.
    pub fn heading(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            level: Level::Section,
            count: None,
            action: None,
        }
    }

    /// A group inside a section (or a list's section header), titled in
    /// `Subheading`.
    pub fn group(title: impl Into<String>) -> Self {
        Self {
            level: Level::Group,
            ..Self::heading(title)
        }
    }

    /// How many items the section holds, in a quiet pill after the title.
    #[must_use]
    pub fn count(mut self, count: usize) -> Self {
        self.count = Some(count);
        self
    }

    /// One trailing text action that acts on the whole section. Its label
    /// ends on the trailing edge.
    #[must_use]
    pub fn action(mut self, button: Button) -> Self {
        self.action = Some(button);
        self
    }

    fn into_widget(self, disclosure: Option<Disclosure>) -> HeaderRow {
        let mut children: Vec<Element> = vec![Box::new(
            Text::new(&self.title)
                .with_role(self.level.role())
                .with_heading_level(self.level.heading_level()),
        )];
        if let Some(count) = self.count {
            children.push(Box::new(CountPill {
                label: count.to_string(),
                rect: Rect::ZERO,
                metrics: None,
            }));
        }
        let has_action = self.action.is_some();
        if let Some(action) = self.action {
            children.push(Box::new(
                action
                    .role(ButtonRole::Plain)
                    .control_size(ControlSize::Small),
            ));
        }
        let has_chevron = disclosure.is_some();
        if let Some(disclosure) = &disclosure {
            children.push(Box::new(Chevron {
                expanded: Rc::clone(&disclosure.expanded),
                rect: Rect::ZERO,
            }));
        }
        HeaderRow {
            parts: Parts::new(children),
            title: self.title,
            level: self.level,
            has_action,
            has_chevron,
            disclosure,
        }
    }
}

/// An immediate-mode section header for a painted host, with the retained
/// [`SectionHeader`]'s geometry: the title on the content edge, the count
/// after it, the action's label on the trailing edge, one fixed height.
#[derive(Clone, Debug)]
pub struct SectionHeaderFace<'a> {
    title: &'a str,
    level: Level,
    count: Option<usize>,
    action: Option<&'a str>,
}

impl<'a> SectionHeaderFace<'a> {
    /// A section of a page, titled in `Heading`.
    pub fn heading(title: &'a str) -> Self {
        Self {
            title,
            level: Level::Section,
            count: None,
            action: None,
        }
    }

    /// A group inside a section, titled in `Subheading`.
    pub fn group(title: &'a str) -> Self {
        Self {
            level: Level::Group,
            ..Self::heading(title)
        }
    }

    #[must_use]
    pub fn count(mut self, count: usize) -> Self {
        self.count = Some(count);
        self
    }

    /// The label of the one action on the section; hit-test it with
    /// [`Self::action_rect`].
    #[must_use]
    pub fn action(mut self, label: &'a str) -> Self {
        self.action = Some(label);
        self
    }

    /// The header's height: one small control's, or the title's wrapped
    /// lines when they are taller.
    pub fn height(&self, text: &mut TextRenderer, theme: &Theme, width: f64) -> f64 {
        let m = HeaderMetrics::new(theme);
        let style = theme.text(self.level.role());
        let room = (width - self.trailing_width(text, theme, &m)).max(1.0);
        let title_h = f64::from(
            text.measure_style(self.title, Some(crate::num::f32_of_f64(room)), &style)
                .1,
        );
        let line = f64::from(style.line_height_px());
        title_h.max(m.height(line))
    }

    fn action_width(&self, text: &mut TextRenderer, theme: &Theme) -> f64 {
        self.action.map_or(0.0, |label| {
            ButtonFace::new(Rect::ZERO, label)
                .role(ButtonRole::Plain)
                .control_size(ControlSize::Small)
                .intrinsic_width(text, theme)
        })
    }

    fn trailing_width(&self, text: &mut TextRenderer, theme: &Theme, m: &HeaderMetrics) -> f64 {
        let count = self.count.map_or(0.0, |count| {
            m.count_size(text, &count.to_string()).0 + m.gap
        });
        let action = self.action_width(text, theme);
        count
            + if action > 0.0 {
                action - m.action_pad + m.gap
            } else {
                0.0
            }
    }

    /// Where the action's button sits in `rect`: its label ends on the
    /// trailing edge, the button's inset past it.
    pub fn action_rect(&self, rect: Rect, text: &mut TextRenderer, theme: &Theme) -> Option<Rect> {
        let m = HeaderMetrics::new(theme);
        self.action?;
        let width = self.action_width(text, theme);
        let line = f64::from(theme.text(self.level.role()).line_height_px());
        let height = m.height(line);
        let cy = rect.y0 + height / 2.0;
        Some(Rect::new(
            rect.x1 + m.action_pad - width,
            cy - m.control / 2.0,
            rect.x1 + m.action_pad,
            cy + m.control / 2.0,
        ))
    }

    /// Paint the header into `rect`, the column's content box at the
    /// header's height. `action` is the action's pointer state.
    pub fn paint(&self, ctx: &mut PaintCtx<'_>, rect: Rect, action: Interaction) {
        let theme = ctx.theme;
        let m = HeaderMetrics::new(theme);
        let style = theme.text(self.level.role());
        let room = (rect.width() - self.trailing_width(ctx.text, theme, &m)).max(1.0);
        let wrap = crate::num::f32_of_f64(room);
        let (title_w, title_h) = ctx.text.measure_style(self.title, Some(wrap), &style);
        let first_line = f64::from(style.line_height_px());
        let header_h = m.height(first_line);
        let cy = rect.y0 + header_h / 2.0;
        let top = if f64::from(title_h) > header_h {
            rect.y0
        } else {
            cy - f64::from(title_h) / 2.0
        };
        ctx.text(
            self.title,
            &style,
            theme.text_primary,
            Point::new(rect.x0, top),
            Some(wrap),
        );
        if let Some(count) = self.count {
            let label = count.to_string();
            let (w, h) = m.count_size(ctx.text, &label);
            let x = rect.x0 + f64::from(title_w).min(room) + m.gap;
            paint_count(ctx, Rect::new(x, cy - h / 2.0, x + w, cy + h / 2.0), &label);
        }
        if let (Some(label), Some(target)) = (self.action, self.action_rect(rect, ctx.text, theme))
        {
            let face = ButtonFace::new(target, label)
                .role(ButtonRole::Plain)
                .control_size(ControlSize::Small)
                .hovered(action.hovered)
                .pressed(action.pressed)
                .focused(action.focused)
                .disabled(action.disabled);
            Button::paint(ctx.scene, ctx.transform, ctx.text, theme, &face);
        }
    }

    /// The header's heading node and, with an action, its button.
    pub fn accessibility(&self, rect: Rect) -> AccessNode {
        let mut node = AccessNode::new(Role::Label)
            .with_name(self.title)
            .with_level(usize::from(self.level.heading_level()))
            .with_bounds(rect);
        if let Some(count) = self.count {
            node = node.with_value(count.to_string());
        }
        node
    }
}

/// A count in a quiet pill: `Caption` in tabular figures.
struct CountPill {
    label: String,
    rect: Rect,
    metrics: Option<HeaderMetrics>,
}

impl Widget for CountPill {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.metrics = Some(HeaderMetrics::new(theme));
        let style = Style {
            flex_shrink: 0.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn measure(
        &mut self,
        _known: taffy::Size<Option<f32>>,
        _available: taffy::Size<taffy::AvailableSpace>,
        text: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        let Some(metrics) = &self.metrics else {
            return taffy::Size::ZERO;
        };
        let (w, h) = metrics.count_size(text, &self.label);
        taffy::Size {
            width: crate::num::f32_of_f64(w),
            height: crate::num::f32_of_f64(h),
        }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        if self.rect.height() <= 0.0 {
            return;
        }
        paint_count(
            &mut PaintCtx::new(scene, transform, text, theme),
            self.rect,
            &self.label,
        );
    }

    /// A pill is laid out once it has a rect.
    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }

    fn name(&self) -> &'static str {
        "SectionCount"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<AccessNode> {
        Some(AccessNode::new(Role::StaticText).with_value(self.label.clone()))
    }
}

/// The disclosure chevron of a collapsible section's header.
struct Chevron {
    expanded: Rc<Cell<bool>>,
    rect: Rect,
}

impl Widget for Chevron {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let side = theme.tokens.metrics.icon_sm;
        let style = Style {
            flex_shrink: 0.0,
            size: Size {
                width: length(side),
                height: length(side),
            },
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        _text: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let icon = if self.expanded.get() {
            Icon::ChevronDown
        } else {
            Icon::ChevronRight
        };
        icon.draw(scene, transform, self.rect, theme.text_dim);
    }

    fn name(&self) -> &'static str {
        "SectionDisclosure"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

/// The shared open state of a collapsible section.
struct Disclosure {
    expanded: Rc<Cell<bool>>,
    on_toggle: Option<Box<dyn FnMut(bool)>>,
    interaction: Interaction,
    /// The press on the header; `interaction.pressed` mirrors it.
    press: crate::pointer_replay::HeldPress,
    cursor: Point,
    toggled: bool,
}

impl Disclosure {
    fn set(&mut self, expanded: bool) -> bool {
        if self.expanded.get() == expanded {
            return false;
        }
        self.expanded.set(expanded);
        self.toggled = true;
        if let Some(on_toggle) = &mut self.on_toggle {
            on_toggle(expanded);
        }
        true
    }
}

struct HeaderRow {
    parts: Parts,
    title: String,
    level: Level,
    has_action: bool,
    has_chevron: bool,
    disclosure: Option<Disclosure>,
}

impl HeaderRow {
    fn action_rect(&self) -> Option<Rect> {
        if !self.has_action {
            return None;
        }
        let index = self.parts.children.len() - 1 - usize::from(self.has_chevron);
        self.parts.children.get(index).map(|child| child.get_rect())
    }
}

impl Widget for HeaderRow {
    fn start_operation(
        &mut self,
        op: &crate::widget::ResourceOperation<'_>,
        doc: &mut crate::widget::Edits<'_>,
        path: &ObjId,
    ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
        crate::widget::start_in_children(self, op, doc, path)
    }

    fn update(&mut self, state: &crate::widget::AppModel, path: &crate::widget::ModelPath) -> bool {
        let mut changed = false;
        for child in &mut self.parts.children {
            changed |= child.update(state, path);
        }
        changed
    }

    fn sync(&mut self, state: &crate::widget::AppModel, path: &crate::widget::ModelPath) {
        for child in &mut self.parts.children {
            child.sync(state, path);
        }
    }

    fn mark_drawn(&mut self) {
        for child in &mut self.parts.children {
            child.mark_drawn();
        }
    }

    fn wants_pointer_capture(&self) -> bool {
        self.parts
            .children
            .iter()
            .any(|child| child.wants_pointer_capture())
    }

    fn get_rect(&self) -> Rect {
        self.parts.rect
    }

    fn name(&self) -> &'static str {
        "SectionHeader"
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.parts.apply_layout(taffy, node, origin);
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.parts
            .children
            .iter()
            .map(|child| child.as_ref())
            .collect()
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        for child in &self.parts.children {
            visitor(child.as_ref());
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.parts
            .children
            .iter_mut()
            .map(|child| child.as_mut() as &mut (dyn Widget + '_))
            .collect()
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let m = HeaderMetrics::new(theme);
        let node = self.parts.build(
            Style {
                display: taffy::Display::Flex,
                flex_direction: taffy::FlexDirection::Row,
                align_items: Some(taffy::AlignItems::Center),
                gap: Size {
                    width: length(theme.tokens.spacing.rhythm.related),
                    height: length(0.0_f32),
                },
                size: Size {
                    width: percent(1.0_f32),
                    height: auto(),
                },
                // One height whether or not there is an action.
                min_size: Size {
                    width: auto(),
                    height: length(crate::num::f32_of_f64(
                        m.height(f64::from(theme.text(self.level.role()).line_height_px())),
                    )),
                },
                flex_shrink: 0.0,
                ..Default::default()
            },
            taffy,
            theme,
            map,
        );
        // The title takes what is left and wraps; the action trails.
        if let Some(&title) = self.parts.nodes.first()
            && let Ok(style) = taffy.style(title)
        {
            let mut style = style.clone();
            style.flex_shrink = 1.0;
            style.min_size.width = length(0.0_f32);
            let _ = taffy.set_style(title, style);
        }
        let trailing_index = if self.has_action {
            self.parts
                .nodes
                .len()
                .checked_sub(1 + usize::from(self.has_chevron))
        } else if self.has_chevron {
            self.parts.nodes.len().checked_sub(1)
        } else {
            None
        };
        if let Some(index) = trailing_index
            && let Some(&trailing) = self.parts.nodes.get(index)
            && let Ok(style) = taffy.style(trailing)
        {
            let mut style = style.clone();
            style.margin.left = auto();
            if self.has_action && !self.has_chevron {
                // The label, not the button, ends on the trailing edge: the
                // button's inset (its hover lift) bleeds past it.
                style.margin.right = length(-crate::num::f32_of_f64(m.action_pad));
            }
            let _ = taffy.set_style(trailing, style);
        }
        map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        for (child, &node) in self.parts.children.iter_mut().zip(&self.parts.nodes) {
            let path = ctx.path;
            child.handle_event(event, &mut ctx.reborrow(path, node));
            if *ctx.handled {
                return;
            }
        }
        let rect = self.parts.rect;
        let action = self.action_rect();
        // The title names the header across a rebuild.
        let title = self.title.as_str();
        let Some(disclosure) = &mut self.disclosure else {
            return;
        };
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                disclosure.cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                disclosure.interaction.hovered = rect.contains(disclosure.cursor)
                    && !action.is_some_and(|r| r.contains(disclosure.cursor));
            }
            WindowEvent::CursorLeft { .. } => disclosure.interaction.hovered = false,
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => match state {
                ElementState::Pressed if disclosure.interaction.hovered => {
                    disclosure.press.press(PressAt::one(title, rect));
                    disclosure.interaction.pressed = true;
                    *ctx.focus_target = Some(ctx.node_id);
                    *ctx.handled = true;
                }
                ElementState::Released => {
                    let over = disclosure.interaction.hovered;
                    let release = disclosure
                        .press
                        .release(over.then_some(PressAt::one(title, rect)));
                    disclosure.interaction.pressed = false;
                    *ctx.handled |= release.ended();
                    if release == Release::Click(()) {
                        let open = !disclosure.expanded.get();
                        disclosure.set(open);
                    }
                }
                ElementState::Pressed => {}
            },
            WindowEvent::KeyboardInput { event, .. }
                if disclosure.interaction.focused && event.state == ElementState::Pressed =>
            {
                let handled = match &event.logical_key {
                    Key::Named(NamedKey::ArrowRight) => {
                        disclosure.set(true);
                        true
                    }
                    Key::Named(NamedKey::ArrowLeft) => {
                        disclosure.set(false);
                        true
                    }
                    _ => false,
                };
                *ctx.handled |= handled;
            }
            _ => {}
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if let Some(disclosure) = &self.disclosure {
            let state = disclosure.interaction;
            paint_row_selection(
                scene,
                transform,
                self.parts.rect.inflate(row_bleed(theme), 0.0),
                SelectionState::new(state.pressed, state.hovered, false),
                theme.tokens.colors.surfaces.content,
                theme,
            );
            if state.focused {
                crate::widget::paint_focus_ring(
                    scene,
                    transform,
                    self.parts.rect.inflate(row_bleed(theme) - 2.0, -2.0),
                    f64::from(theme.tokens.radii.item) - 2.0,
                    theme,
                );
            }
        }
        self.parts.draw(scene, transform, text, theme, presence);
    }

    fn is_focusable(&self) -> bool {
        self.disclosure.is_some()
    }

    fn set_focus(&mut self, focused: bool) {
        if let Some(disclosure) = &mut self.disclosure {
            disclosure.interaction.focused = focused;
        }
    }

    fn is_focused(&self) -> bool {
        self.disclosure
            .as_ref()
            .is_some_and(|d| d.interaction.focused)
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        (self.disclosure.is_some() && self.parts.rect.contains(point))
            .then_some(winit::window::CursorIcon::Pointer)
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        _state: &mut crate::widget::Edits<'_>,
        _root: &crate::widget::ModelPath,
    ) -> bool {
        use crate::a11y::Action;
        let Some(disclosure) = &mut self.disclosure else {
            return false;
        };
        match action {
            Action::Activate => {
                let open = !disclosure.expanded.get();
                disclosure.set(open)
            }
            Action::Expand => disclosure.set(true),
            Action::Collapse => disclosure.set(false),
            _ => false,
        }
    }

    fn take_structure_dirty(&mut self) -> bool {
        let mut dirty = self
            .disclosure
            .as_mut()
            .is_some_and(|d| std::mem::take(&mut d.toggled));
        for child in &mut self.parts.children {
            dirty |= child.take_structure_dirty();
        }
        dirty
    }

    fn accessibility(&self) -> Option<AccessNode> {
        let disclosure = self.disclosure.as_ref()?;
        Some(
            AccessNode::new(Role::Button)
                .with_name(self.title.clone())
                .with_bounds(self.parts.rect)
                .with_state(
                    AccessState::new()
                        .focusable(true)
                        .focused(disclosure.interaction.focused)
                        .expanded(disclosure.expanded.get()),
                ),
        )
    }
}

/// An intrinsic-height group of content with an optional header. See the
/// module docs.
pub struct Section {
    parts: Parts,
    has_header: bool,
    /// The header's title and level, kept so a later `collapsible` can
    /// rebuild the header around them.
    header: Option<String>,
    expanded: Option<Rc<Cell<bool>>>,
    /// The label column the section's property lists share.
    labels: crate::widget::property_list::PropertyColumn,
    /// The width the section's dropdowns share.
    choosers: crate::widget::dropdown::ChooserColumn,
}

impl Section {
    /// Stack `items` `rhythm.sibling` apart.
    pub fn new(items: Vec<Element>) -> Self {
        Self {
            parts: Parts::new(items),
            has_header: false,
            header: None,
            expanded: None,
            labels: crate::widget::property_list::PropertyColumn::new(),
            choosers: crate::widget::dropdown::ChooserColumn::default(),
        }
    }

    /// A section of a page titled `title`.
    pub fn titled(title: impl Into<String>, items: Vec<Element>) -> Self {
        Self::new(items).with_header(SectionHeader::heading(title))
    }

    /// Replace the header.
    #[must_use]
    pub fn with_header(mut self, header: SectionHeader) -> Self {
        self.header = Some(header.title.clone());
        let disclosure = self.expanded.as_ref().map(|expanded| Disclosure {
            expanded: Rc::clone(expanded),
            on_toggle: None,
            interaction: Interaction::default(),
            press: crate::pointer_replay::HeldPress::default(),
            cursor: Point::ZERO,
            toggled: false,
        });
        self.set_header_row(header.into_widget(disclosure));
        self
    }

    fn set_header_row(&mut self, row: HeaderRow) {
        let row: Element = Box::new(row);
        if self.has_header
            && let Some(slot) = self.parts.children.first_mut()
        {
            *slot = row;
        } else {
            self.parts.children.insert(0, row);
            self.has_header = true;
        }
    }

    /// Make the section a disclosure: its header shows a chevron and
    /// toggles the items with a pointer, the keyboard (Enter, Space, the
    /// arrow keys) and assistive technology. Starts expanded; call after
    /// [`Self::with_header`].
    #[must_use]
    pub fn collapsible(self) -> Self {
        self.collapsible_with(true, None)
    }

    /// [`Self::collapsible`], starting collapsed.
    #[must_use]
    pub fn collapsed(self) -> Self {
        self.collapsible_with(false, None)
    }

    /// Run `f` with the new state each time the section opens or closes.
    #[must_use]
    pub fn on_toggle(self, f: impl FnMut(bool) + 'static) -> Self {
        let expanded = self.expanded.as_ref().is_none_or(|e| e.get());
        self.collapsible_with(expanded, Some(Box::new(f)))
    }

    fn collapsible_with(mut self, expanded: bool, on_toggle: Option<Box<dyn FnMut(bool)>>) -> Self {
        let state = Rc::new(Cell::new(expanded));
        self.expanded = Some(Rc::clone(&state));
        let Some(title) = self.header.clone() else {
            return self;
        };
        // Rebuild the header around the disclosure, keeping its count and
        // action.
        if self.has_header
            && let Some(first) = self.parts.children.first_mut()
            && let Some(row) = first
                .as_any_mut()
                .and_then(|any| any.downcast_mut::<HeaderRow>())
        {
            let had_chevron = row.has_chevron;
            row.disclosure = Some(Disclosure {
                expanded: Rc::clone(&state),
                on_toggle,
                interaction: Interaction::default(),
                press: crate::pointer_replay::HeldPress::default(),
                cursor: Point::ZERO,
                toggled: false,
            });
            if had_chevron {
                if let Some(chevron) = row
                    .parts
                    .children
                    .last_mut()
                    .and_then(|c| c.as_any_mut().and_then(|any| any.downcast_mut::<Chevron>()))
                {
                    chevron.expanded = Rc::clone(&state);
                }
            } else {
                row.parts.children.push(Box::new(Chevron {
                    expanded: state,
                    rect: Rect::ZERO,
                }));
                row.has_chevron = true;
            }
            row.title = title;
        }
        self
    }

    /// Whether a collapsible section shows its items.
    pub fn is_expanded(&self) -> bool {
        self.expanded.as_ref().is_none_or(|e| e.get())
    }

    /// Open or close a collapsible section from the host.
    pub fn set_expanded(&mut self, expanded: bool) {
        if let Some(state) = &self.expanded {
            state.set(expanded);
        }
    }

    /// The items shown: all of them, or only the header while collapsed.
    fn shown(&self) -> usize {
        if self.is_expanded() {
            self.parts.children.len()
        } else {
            usize::from(self.has_header)
        }
    }
}

impl Widget for Section {
    fn start_operation(
        &mut self,
        op: &crate::widget::ResourceOperation<'_>,
        doc: &mut crate::widget::Edits<'_>,
        path: &ObjId,
    ) -> Result<Option<crate::widget::ResourceOperationStart>, String> {
        crate::widget::start_in_children(self, op, doc, path)
    }

    fn update(&mut self, state: &crate::widget::AppModel, path: &crate::widget::ModelPath) -> bool {
        let mut changed = false;
        for child in &mut self.parts.children {
            changed |= child.update(state, path);
        }
        changed
    }

    fn sync(&mut self, state: &crate::widget::AppModel, path: &crate::widget::ModelPath) {
        for child in &mut self.parts.children {
            child.sync(state, path);
        }
    }

    fn mark_drawn(&mut self) {
        for child in &mut self.parts.children {
            child.mark_drawn();
        }
    }

    fn wants_pointer_capture(&self) -> bool {
        self.parts
            .children
            .iter()
            .any(|child| child.wants_pointer_capture())
    }

    fn get_rect(&self) -> Rect {
        self.parts.rect
    }

    fn name(&self) -> &'static str {
        "Section"
    }

    fn take_structure_dirty(&mut self) -> bool {
        let mut dirty = false;
        for child in &mut self.parts.children {
            dirty |= child.take_structure_dirty();
        }
        dirty
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let rhythm = &theme.tokens.spacing.rhythm;
        let node = self.parts.build(
            Style {
                // One-column implicit Grid, as an intrinsic Column: every row
                // is sized by its content, so the section's height is exact
                // inside a scrolling stack.
                display: taffy::Display::Grid,
                grid_template_columns: vec![taffy::style_helpers::minmax(
                    length(0.0_f32),
                    taffy::style_helpers::fr(1.0_f32),
                )],
                align_content: Some(taffy::AlignContent::Start),
                size: Size {
                    width: percent(1.0_f32),
                    height: auto(),
                },
                min_size: Size {
                    width: length(0.0_f32),
                    height: auto(),
                },
                flex_shrink: 0.0,
                ..Default::default()
            },
            taffy,
            theme,
            map,
        );
        // Each item's distance from the one above: `related` under the
        // header (it belongs to its items), nothing between consecutive list
        // rows (one list), `sibling` otherwise. A collapsed section lays out
        // only its header.
        // The section's property lists share one label column, and its
        // dropdowns one width.
        self.choosers.reset();
        for child in &mut self.parts.children {
            if let Some(list) = child
                .as_any_mut()
                .and_then(|any| any.downcast_mut::<crate::widget::PropertyList>())
            {
                list.join_column(&self.labels);
            } else {
                self.choosers.join(child.as_mut());
            }
        }
        let rows = self.row_flags();
        let shown = self.shown();
        for (index, &node) in self.parts.nodes.iter().enumerate() {
            let Ok(style) = taffy.style(node) else {
                continue;
            };
            let mut style = style.clone();
            if index >= shown {
                style.display = taffy::Display::None;
            } else if index > 0 {
                let gap = if self.has_header && index == 1 {
                    rhythm.related
                } else if rows.get(index - 1).copied().unwrap_or(false)
                    && rows.get(index).copied().unwrap_or(false)
                {
                    0.0
                } else {
                    rhythm.sibling
                };
                style.margin.top = length(gap);
            }
            let _ = taffy.set_style(node, style);
        }
        map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.parts.apply_layout(taffy, node, origin);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.sync_rows(text, theme);
        let shown = self.shown();
        for child in self.parts.children.iter_mut().take(shown) {
            child.draw(scene, transform, text, theme, presence);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        let shown = self.shown();
        for (child, &node) in self
            .parts
            .children
            .iter_mut()
            .zip(&self.parts.nodes)
            .take(shown)
        {
            let path = ctx.path;
            child.handle_event(event, &mut ctx.reborrow(path, node));
            if *ctx.handled {
                break;
            }
        }
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.parts
            .children
            .iter()
            .take(self.shown())
            .map(|child| child.as_ref())
            .collect()
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        for child in self.parts.children.iter().take(self.shown()) {
            visitor(child.as_ref());
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let shown = self.shown();
        self.parts
            .children
            .iter_mut()
            .take(shown)
            .map(|child| child.as_mut() as &mut (dyn Widget + '_))
            .collect()
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

impl Section {
    fn row_mut(child: &mut Element) -> Option<&mut crate::widget::ListRow> {
        child
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<crate::widget::ListRow>())
    }

    /// Which items are list rows.
    fn row_flags(&mut self) -> Vec<bool> {
        self.parts
            .children
            .iter_mut()
            .map(|child| Self::row_mut(child).is_some())
            .collect()
    }

    /// Bring the rows of each run of consecutive list rows into one list: a
    /// separator under each row a row follows (hidden beside a selected
    /// one), one lift across adjacent selected rows, and one badge column.
    fn sync_rows(&mut self, text: &mut TextRenderer, theme: &Theme) {
        self.sync_row_separators();
        // Badge columns, per run of consecutive rows.
        let badges: Vec<Option<Option<String>>> = self
            .parts
            .children
            .iter_mut()
            .map(|child| Self::row_mut(child).map(|row| row.badge_label().map(str::to_owned)))
            .collect();
        let mut start = 0;
        while start < badges.len() {
            if badges.get(start).is_some_and(Option::is_none) {
                start += 1;
                continue;
            }
            let end = (start..badges.len())
                .find(|&i| badges.get(i).is_some_and(Option::is_none))
                .unwrap_or(badges.len());
            let labels: Vec<&str> = badges
                .get(start..end)
                .unwrap_or_default()
                .iter()
                .filter_map(|b| b.as_ref().and_then(Option::as_deref))
                .collect();
            let shared =
                (labels.len() > 1).then(|| crate::widget::badge_column_width(text, theme, &labels));
            for child in self.parts.children.iter_mut().take(end).skip(start) {
                if let Some(row) = Self::row_mut(child) {
                    row.set_shared_leading(shared);
                }
            }
            start = end;
        }
    }

    /// A list row gets a separator when a list row follows it, hidden when
    /// either is selected; a selected row's lift joins a selected neighbour.
    fn sync_row_separators(&mut self) {
        let selected: Vec<Option<bool>> = self
            .parts
            .children
            .iter_mut()
            .map(|child| Self::row_mut(child).map(|row| row.is_selected()))
            .collect();
        for (index, child) in self.parts.children.iter_mut().enumerate() {
            let next = selected.get(index + 1).copied().flatten();
            let previous = index
                .checked_sub(1)
                .and_then(|i| selected.get(i).copied().flatten());
            if let Some(row) = Self::row_mut(child) {
                row.set_separator(next.is_some(), next.unwrap_or(false));
                row.set_join(RowJoin::new(
                    previous.unwrap_or(false),
                    next.unwrap_or(false),
                ));
            }
        }
    }
}

/// A non-interactive status: a [`RowStatus`] dot and a `Caption` sentence on
/// the content edge ("Engine running", "3 containers stopped"), for a list's
/// footer or a detail's state.
pub struct StatusLine {
    status: OwnedStatus,
    text: String,
    rect: Rect,
    style: Option<TextStyle>,
    dot: f64,
    gap: f64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum OwnedStatus {
    Active,
    Warning,
    Error,
    Idle,
    Presence(String),
}

impl StatusLine {
    fn with(status: OwnedStatus, text: impl Into<String>) -> Self {
        Self {
            status,
            text: text.into(),
            rect: Rect::ZERO,
            style: None,
            dot: 0.0,
            gap: 0.0,
        }
    }

    /// Running, connected, healthy.
    pub fn active(text: impl Into<String>) -> Self {
        Self::with(OwnedStatus::Active, text)
    }

    /// Needs attention soon.
    pub fn warning(text: impl Into<String>) -> Self {
        Self::with(OwnedStatus::Warning, text)
    }

    /// Failed or offline unexpectedly.
    pub fn error(text: impl Into<String>) -> Self {
        Self::with(OwnedStatus::Error, text)
    }

    /// Present but inactive.
    pub fn idle(text: impl Into<String>) -> Self {
        Self::with(OwnedStatus::Idle, text)
    }

    /// A person's presence, in their colour.
    pub fn presence(identity: impl Into<String>, text: impl Into<String>) -> Self {
        Self::with(OwnedStatus::Presence(identity.into()), text)
    }

    /// Replace the sentence (a live count).
    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
    }

    fn status(&self) -> RowStatus<'_> {
        match &self.status {
            OwnedStatus::Active => RowStatus::Active,
            OwnedStatus::Warning => RowStatus::Warning,
            OwnedStatus::Error => RowStatus::Error,
            OwnedStatus::Idle => RowStatus::Idle,
            OwnedStatus::Presence(id) => RowStatus::Presence(id),
        }
    }
}

impl Widget for StatusLine {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.style = Some(theme.text(TextRole::Caption));
        self.dot = f64::from(theme.tokens.spacing.s2);
        self.gap = f64::from(theme.tokens.spacing.rhythm.related);
        let style = Style {
            size: Size {
                width: percent(1.0_f32),
                height: auto(),
            },
            min_size: Size {
                width: length(0.0_f32),
                height: auto(),
            },
            flex_shrink: 0.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn measure(
        &mut self,
        known: taffy::Size<Option<f32>>,
        available: taffy::Size<taffy::AvailableSpace>,
        text: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        let Some(style) = &self.style else {
            return taffy::Size::ZERO;
        };
        let width = known.width.unwrap_or(match available.width {
            taffy::AvailableSpace::Definite(w) => w,
            _ => f32::INFINITY,
        });
        let room = (f64::from(width) - self.dot - self.gap).max(1.0);
        let (w, h) = text.measure_style(&self.text, Some(crate::num::f32_of_f64(room)), style);
        taffy::Size {
            width: if width.is_finite() {
                width
            } else {
                crate::num::f32_of_f64(f64::from(w) + self.dot + self.gap)
            },
            height: h,
        }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let style = theme.text(TextRole::Caption);
        let line = f64::from(style.line_height_px());
        let dot = f64::from(theme.tokens.spacing.s2);
        let gap = f64::from(theme.tokens.spacing.rhythm.related);
        scene.fill(
            Fill::NonZero,
            transform,
            self.status().colour(theme),
            None,
            &Circle::new(
                Point::new(self.rect.x0 + dot / 2.0, self.rect.y0 + line / 2.0),
                dot / 2.0,
            ),
        );
        let x = self.rect.x0 + dot + gap;
        PaintCtx::new(scene, transform, text, theme).text(
            &self.text,
            &style,
            theme.text_dim,
            Point::new(x, self.rect.y0),
            Some(crate::num::f32_of_f64((self.rect.x1 - x).max(1.0))),
        );
    }

    fn name(&self) -> &'static str {
        "StatusLine"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<AccessNode> {
        let value = match self.status().spoken() {
            Some(spoken) => format!("{spoken}: {}", self.text),
            None => self.text.clone(),
        };
        Some(
            AccessNode::new(Role::StaticText)
                .with_value(value)
                .with_bounds(self.rect),
        )
    }
}

/// The head of a detail view: an avatar or an icon, the object's name in
/// `Title` and one `Caption` line under it, `rhythm.tight` apart, on the
/// content edge ("Ada Lovelace / ada@example.org", a password's site).
pub struct ObjectHeader {
    title: String,
    caption: Option<String>,
    lead: Option<ObjectLead>,
    rect: Rect,
    metrics: Option<ObjectMetrics>,
}

enum ObjectLead {
    Avatar {
        name: String,
        identity: String,
        image: Option<vello::peniko::ImageData>,
        square: bool,
    },
    Icon(Icon),
}

#[derive(Clone, Debug)]
struct ObjectMetrics {
    title: TextStyle,
    caption: TextStyle,
    tight: f64,
    gap: f64,
    lead: f64,
}

impl ObjectMetrics {
    fn new(theme: &Theme) -> Self {
        Self {
            title: theme.text(TextRole::Title),
            caption: theme.text(TextRole::Caption),
            tight: f64::from(theme.tokens.spacing.rhythm.tight),
            gap: f64::from(theme.tokens.spacing.rhythm.sibling),
            lead: crate::widget::AvatarSize::Large.side(theme),
        }
    }
}

impl ObjectHeader {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            caption: None,
            lead: None,
            rect: Rect::ZERO,
            metrics: None,
        }
    }

    /// One line under the name: an address, a URL, a kind.
    #[must_use]
    pub fn caption(mut self, caption: impl Into<String>) -> Self {
        self.caption = Some(caption.into());
        self
    }

    /// A person's or account's avatar before the name.
    #[must_use]
    pub fn avatar(mut self, name: impl Into<String>, identity: impl Into<String>) -> Self {
        self.lead = Some(ObjectLead::Avatar {
            name: name.into(),
            identity: identity.into(),
            image: None,
            square: false,
        });
        self
    }

    /// The avatar's picture.
    #[must_use]
    pub fn avatar_image(mut self, picture: vello::peniko::ImageData) -> Self {
        if let Some(ObjectLead::Avatar { image, .. }) = &mut self.lead {
            *image = Some(picture);
        }
        self
    }

    /// A square avatar: an account or a site.
    #[must_use]
    pub fn square(mut self) -> Self {
        if let Some(ObjectLead::Avatar { square, .. }) = &mut self.lead {
            *square = true;
        }
        self
    }

    /// A kind of thing before the name.
    #[must_use]
    pub fn icon(mut self, icon: Icon) -> Self {
        self.lead = Some(ObjectLead::Icon(icon));
        self
    }

    fn text_x(&self, m: &ObjectMetrics) -> f64 {
        if self.lead.is_some() {
            m.lead + m.gap
        } else {
            0.0
        }
    }

    fn block(&self, text: &mut TextRenderer, m: &ObjectMetrics, width: f64) -> (f64, f64) {
        let room = crate::num::f32_of_f64((width - self.text_x(m)).max(1.0));
        let title = f64::from(text.measure_style(&self.title, Some(room), &m.title).1);
        let caption = self.caption.as_ref().map_or(0.0, |caption| {
            m.tight + f64::from(text.measure_style(caption, Some(room), &m.caption).1)
        });
        (title, caption)
    }
}

impl Widget for ObjectHeader {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.metrics = Some(ObjectMetrics::new(theme));
        let style = Style {
            size: Size {
                width: percent(1.0_f32),
                height: auto(),
            },
            min_size: Size {
                width: length(0.0_f32),
                height: auto(),
            },
            flex_shrink: 0.0,
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn measure(
        &mut self,
        known: taffy::Size<Option<f32>>,
        available: taffy::Size<taffy::AvailableSpace>,
        text: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        let Some(m) = self.metrics.clone() else {
            return taffy::Size::ZERO;
        };
        let width = known.width.unwrap_or(match available.width {
            taffy::AvailableSpace::Definite(w) => w,
            _ => 480.0,
        });
        let (title, caption) = self.block(text, &m, f64::from(width));
        let lead = if self.lead.is_some() { m.lead } else { 0.0 };
        taffy::Size {
            width,
            height: crate::num::f32_of_f64((title + caption).max(lead).ceil()),
        }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let m = ObjectMetrics::new(theme);
        let mut ctx = PaintCtx::new(scene, transform, text, theme);
        let rect = self.rect;
        let (title_h, caption_h) = self.block(ctx.text, &m, rect.width());
        let top = rect.center().y - (title_h + caption_h) / 2.0;
        match &self.lead {
            Some(ObjectLead::Avatar {
                name,
                identity,
                image,
                square,
            }) => {
                let face = AvatarFace::new(name, identity).image(image.as_ref());
                let face = if *square { face.square() } else { face };
                face.paint(
                    &mut ctx,
                    Rect::new(
                        rect.x0,
                        rect.center().y - m.lead / 2.0,
                        rect.x0 + m.lead,
                        rect.center().y + m.lead / 2.0,
                    ),
                );
            }
            Some(ObjectLead::Icon(icon)) => {
                let side = f64::from(theme.tokens.metrics.icon_lg);
                icon.draw(
                    ctx.scene,
                    ctx.transform,
                    Rect::from_center_size(
                        Point::new(rect.x0 + m.lead / 2.0, rect.center().y),
                        (side, side),
                    ),
                    theme.text_dim,
                );
            }
            None => {}
        }
        let x = rect.x0 + self.text_x(&m);
        let room = crate::num::f32_of_f64((rect.x1 - x).max(1.0));
        ctx.text(
            &self.title,
            &m.title,
            theme.text_primary,
            Point::new(x, top),
            Some(room),
        );
        if let Some(caption) = &self.caption {
            ctx.text(
                caption,
                &m.caption,
                theme.text_dim,
                Point::new(x, top + title_h + m.tight),
                Some(room),
            );
        }
    }

    fn name(&self) -> &'static str {
        "ObjectHeader"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<AccessNode> {
        let mut node = AccessNode::new(Role::Label)
            .with_name(self.title.clone())
            .with_level(1)
            .with_bounds(self.rect);
        if let Some(caption) = &self.caption {
            node = node.with_value(caption.clone());
        }
        Some(node)
    }
}

#[cfg(test)]
#[path = "section_tests.rs"]
mod tests;
