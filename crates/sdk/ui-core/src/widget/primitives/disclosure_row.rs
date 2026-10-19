//! `DisclosureRow`: a list row whose trailing chevron turns and whose body
//! opens in place (`docs/design-language.md`, "Primitives").
//!
//! The row's height follows the body on the standard disclose motion
//! ([`crate::motion::Disclose`]): the container grows and the content fades
//! in as it is revealed; reduced motion lands at once. Rows that share a
//! [`DisclosureGroup`] keep one open at a time: opening one closes the
//! others.
//!
//! **Open across rebuilds.** A view that rebuilds its rows from fresh data
//! (a list of devices read every few seconds) builds a new row each time; a
//! row that kept its own openness would close under the person's pointer.
//! Such a view keys its rows instead: [`DisclosureRow::keyed`] takes the
//! row's [`DisclosureState`] from the group by a stable key (a device's
//! address), so the rebuilt row is open where the last one was, mid-motion
//! where it was moving, at the height its body last had. A lone row binds a
//! [`DisclosureState`] the view keeps with [`DisclosureRow::with_state`].

use std::cell::{Cell, RefCell};
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
use crate::tokens::EffectiveMotion;
use crate::widget::{Element, TaffyTreeExt, Widget};

use super::TextStackFace;

/// Whether a disclosure row is open, its reveal and its body's open height:
/// what outlives the row when the view that owns it rebuilds it. Clones
/// share one state.
#[derive(Clone, Debug, Default)]
pub struct DisclosureState(Rc<RefCell<Openness>>);

#[derive(Clone, Copy, Debug, Default)]
struct Openness {
    open: bool,
    reveal: Presence,
    /// The body's laid-out height when fully open.
    body_height: f64,
    /// Opened or closed since it was made: a first-build default
    /// ([`DisclosureRow::opened`]) no longer applies.
    touched: bool,
}

impl DisclosureState {
    /// Closed, at rest.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Open, at rest.
    #[must_use]
    pub fn opened() -> Self {
        Self(Rc::new(RefCell::new(Openness {
            open: true,
            reveal: Presence::opened(),
            body_height: 0.0,
            touched: false,
        })))
    }

    /// Whether the row is open (or opening).
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.0.borrow().open
    }

    /// Open or close a lone row's state from the view, on the disclose
    /// motion; a keyed row opens through its group
    /// ([`DisclosureGroup::open`]).
    pub fn set_open(&self, open: bool) {
        self.show(open, crate::tokens::EffectiveMotion::default());
    }

    /// Open or close on the disclose motion. Returns whether it changed.
    fn show(&self, open: bool, motion: EffectiveMotion) -> bool {
        let mut state = self.0.borrow_mut();
        if state.open == open {
            return false;
        }
        state.open = open;
        state.touched = true;
        state.reveal.show(
            open,
            Disclose::SPEC,
            motion,
            crate::motion::input_source(),
            crate::motion::frame_now(),
        );
        if let Some(until) = state.reveal.settles_at() {
            crate::motion::want_frames_until(until);
        }
        true
    }

    fn progress(&self) -> f64 {
        self.0.borrow().reveal.progress(crate::motion::frame_now())
    }

    fn is_moving(&self) -> bool {
        self.0.borrow().reveal.is_moving(crate::motion::frame_now())
    }

    fn body_height(&self) -> f64 {
        self.0.borrow().body_height
    }

    /// Record the body's open height; returns whether it changed.
    fn set_body_height(&self, height: f64) -> bool {
        let mut state = self.0.borrow_mut();
        if (height - state.body_height).abs() > 0.5 {
            state.body_height = height;
            true
        } else {
            false
        }
    }

    fn same(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }
}

/// Who a member of a group is: a row numbered as it joined, or a row
/// keyed by the view so that it survives rebuilds.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Member {
    Joined(u64),
    Key(String),
}

/// Rows that keep one open at a time. Clone it into every row of the group;
/// a view that rebuilds its rows keeps the group and keys the rows
/// ([`DisclosureRow::keyed`]).
#[derive(Clone, Debug, Default)]
pub struct DisclosureGroup {
    open: Rc<RefCell<Option<Member>>>,
    next: Rc<Cell<u64>>,
    keyed: Rc<RefCell<HashMap<String, DisclosureState>>>,
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

    /// The state of the row keyed `key`, kept for as long as the group is.
    #[must_use]
    pub fn state(&self, key: &str) -> DisclosureState {
        self.keyed
            .borrow_mut()
            .entry(key.to_owned())
            .or_default()
            .clone()
    }

    /// Open the row keyed `key` from the view (an agent asked for its
    /// details), closing the group's other rows, on the disclose motion.
    pub fn open(&self, key: &str) {
        let state = self.state(key);
        let motion = crate::tokens::EffectiveMotion::default();
        state.show(true, motion);
        self.opened(Member::Key(key.to_owned()), &state, motion);
    }

    /// Close the row keyed `key`, if it is open.
    pub fn close(&self, key: &str) {
        let member = Member::Key(key.to_owned());
        self.state(key)
            .show(false, crate::tokens::EffectiveMotion::default());
        self.closed(&member);
    }

    /// The key of the open keyed row, if one is.
    #[must_use]
    pub fn open_key(&self) -> Option<String> {
        match &*self.open.borrow() {
            Some(Member::Key(key)) => Some(key.clone()),
            _ => None,
        }
    }

    /// Forget the kept states of keys not in `keys` (things that are gone).
    pub fn retain(&self, keys: &[&str]) {
        self.keyed
            .borrow_mut()
            .retain(|key, _| keys.contains(&key.as_str()));
        let stale =
            matches!(&*self.open.borrow(), Some(Member::Key(key)) if !keys.contains(&key.as_str()));
        if stale {
            *self.open.borrow_mut() = None;
        }
    }

    /// `member` opened: close every other keyed row on the motion.
    fn opened(&self, member: Member, own: &DisclosureState, motion: EffectiveMotion) {
        *self.open.borrow_mut() = Some(member);
        for state in self.keyed.borrow().values() {
            if !state.same(own) {
                state.show(false, motion);
            }
        }
    }

    /// `member` closed.
    fn closed(&self, member: &Member) {
        let mut open = self.open.borrow_mut();
        if open.as_ref() == Some(member) {
            *open = None;
        }
    }

    fn is_open(&self, member: &Member) -> bool {
        self.open.borrow().as_ref() == Some(member)
    }
}

/// A row that opens its body in place.
pub struct DisclosureRow {
    title: String,
    secondary: String,
    leading: Option<Icon>,
    /// One control on the header's trailing edge, before the chevron (a
    /// device's Connect): it acts without opening the row.
    trailing: Option<Element>,
    trailing_rect: Rect,
    body: Box<dyn Widget>,
    state: DisclosureState,
    group: Option<(DisclosureGroup, Member)>,
    hovered: bool,
    pressed: bool,
    focused: bool,
    /// An inset rule under the row: a row of its list follows
    /// ([`crate::widget::Section`] sets it).
    separator: bool,
    rect: Rect,
    header: Rect,
    /// The header's height, from the last layout.
    header_px: f64,
    dirty: bool,
    /// The motion the person allows, from the last layout.
    motion: EffectiveMotion,
}

impl DisclosureRow {
    /// A closed row titled `title` over `body`.
    #[must_use]
    pub fn new(title: impl Into<String>, body: Box<dyn Widget>) -> Self {
        Self {
            title: title.into(),
            secondary: String::new(),
            leading: None,
            trailing: None,
            trailing_rect: Rect::ZERO,
            body,
            state: DisclosureState::new(),
            group: None,
            hovered: false,
            pressed: false,
            focused: false,
            separator: false,
            rect: Rect::ZERO,
            header: Rect::ZERO,
            header_px: 0.0,
            dirty: true,
            motion: EffectiveMotion::default(),
        }
    }

    /// A row of `group` keyed `key`: its openness is the group's state for
    /// `key`, so a row rebuilt with the same key is open where the last one
    /// was. One keyed row of the group is open at a time.
    #[must_use]
    pub fn keyed(
        title: impl Into<String>,
        body: Box<dyn Widget>,
        group: &DisclosureGroup,
        key: &str,
    ) -> Self {
        let mut row = Self::new(title, body);
        row.state = group.state(key);
        row.group = Some((group.clone(), Member::Key(key.to_owned())));
        row
    }

    /// Show and change `state`, which the view keeps across rebuilds.
    #[must_use]
    pub fn with_state(mut self, state: &DisclosureState) -> Self {
        self.state = state.clone();
        self
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

    /// One control on the header's trailing edge, before the chevron: a
    /// [`crate::widget::Button`] or a [`crate::widget::Toggle`] that acts on
    /// the row's thing without opening it.
    #[must_use]
    pub fn trailing(mut self, control: Element) -> Self {
        self.trailing = Some(control);
        self
    }

    /// Start open, at rest. A keyed row or one with a kept state opens only
    /// the first time, while its state has never been changed.
    #[must_use]
    pub fn opened(self) -> Self {
        let fresh = {
            let state = self.state.0.borrow();
            !state.open && !state.touched
        };
        if fresh {
            *self.state.0.borrow_mut() = Openness {
                open: true,
                reveal: Presence::opened(),
                body_height: 0.0,
                touched: false,
            };
            if let Some((group, member)) = &self.group {
                *group.open.borrow_mut() = Some(member.clone());
            }
        }
        self
    }

    /// Keep one row of `group` open at a time.
    #[must_use]
    pub fn in_group(mut self, group: &DisclosureGroup) -> Self {
        let member = Member::Joined(group.join());
        if self.state.is_open() {
            *group.open.borrow_mut() = Some(member.clone());
        }
        self.group = Some((group.clone(), member));
        self
    }

    /// Whether the row is open (or opening).
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.state.is_open()
    }

    /// Open or close the row, on the disclose motion as the last input
    /// asked for it.
    pub fn set_open(&mut self, open: bool) {
        if !self.state.show(open, self.motion) {
            return;
        }
        if let Some((group, member)) = &self.group {
            if open {
                group.opened(member.clone(), &self.state, self.motion);
            } else {
                group.closed(member);
            }
        }
        self.dirty = true;
    }

    /// Close if another row of the group opened.
    fn follow_group(&mut self) {
        let other_opened = self
            .group
            .as_ref()
            .is_some_and(|(group, member)| self.state.is_open() && !group.is_open(member));
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
        self.state.progress()
    }

    fn face(&self) -> TextStackFace<'_> {
        TextStackFace::new(&self.title).secondary(&self.secondary)
    }

    /// Where the title's text column starts from the row's leading edge:
    /// past the icon and its gap. The open body lines up with it too.
    fn text_inset(leading: Option<Icon>, theme: &Theme) -> f64 {
        leading.map_or(0.0, |_| {
            f64::from(theme.tokens.metrics.icon_control)
                + f64::from(theme.tokens.spacing.rhythm.sibling)
        })
    }

    fn chevron_rect(&self, theme: &Theme) -> Rect {
        let icon = f64::from(theme.tokens.metrics.icon_control);
        let cy = self.header.center().y;
        Rect::new(
            self.header.x1 - icon,
            cy - icon / 2.0,
            self.header.x1,
            cy + icon / 2.0,
        )
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
        let x = self.header.x0 + Self::text_inset(self.leading, theme);
        let chevron = self.chevron_rect(theme);
        let text_end = if self.trailing.is_some() && self.trailing_rect.width() > 0.0 {
            self.trailing_rect.x0
        } else {
            chevron.x0
        };
        let text = Rect::new(x, self.header.y0, text_end - gap, self.header.y1);
        if let Some(leading) = self.leading {
            // Beside two lines the icon belongs to the title, not to the
            // middle of the pair, where it reads as sitting low.
            let icon_cy = if self.secondary.is_empty() {
                cy
            } else {
                self.face().title_centre(text, ctx.text, theme)
            };
            let x0 = self.header.x0;
            leading.draw(
                ctx.scene,
                ctx.transform,
                Rect::new(x0, icon_cy - icon / 2.0, x0 + icon, icon_cy + icon / 2.0),
                colors.text_dim,
            );
        }
        self.face().paint(ctx, text);
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

    /// Set by the list the row sits in: whether a rule runs under it.
    pub(crate) fn set_separator(&mut self, below: bool) {
        if self.separator != below {
            self.separator = below;
            self.dirty = true;
        }
    }

    /// The rule under the row, from its text's edge to its end, like a list
    /// row's.
    fn paint_separator(&self, ctx: &mut PaintCtx<'_>) {
        if !self.separator {
            return;
        }
        let theme = ctx.theme;
        let x0 = self.header.x0 + Self::text_inset(self.leading, theme);
        crate::surface::separator(
            ctx.scene,
            ctx.transform,
            x0,
            self.rect.x1,
            self.rect.y1 - 0.5,
            0.0,
            theme.tokens.colors.surfaces.content,
        );
    }

    fn over_trailing(&self, point: Point) -> bool {
        self.trailing.is_some() && self.trailing_rect.contains(point)
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
        let icon = f64::from(theme.tokens.metrics.icon_control);
        let gap = f64::from(theme.tokens.spacing.rhythm.sibling);
        let header = self.header_height(theme);
        // The row's own node holds the body and the trailing control, in the
        // order `children` names them, so every walk that pairs widgets with
        // layout nodes meets each where it is. The header is the row's top
        // padding; the body follows it and is cut to what the reveal shows.
        let shut = !self.state.is_open() && !self.state.is_moving();
        let body = self.body.build_layout(taffy, theme, node_map);
        if shut && let Ok(style) = taffy.style(body) {
            // A closed body takes no room and no place on screen.
            let mut style = style.clone();
            style.display = taffy::Display::None;
            let _ = taffy.set_style(body, style);
        }
        let mut children = vec![body];
        if let Some(control) = &mut self.trailing {
            let node = control.build_layout(taffy, theme, node_map);
            if let Ok(style) = taffy.style(node) {
                // On the header's trailing edge, before the chevron; centred
                // on the header in `apply_layout`.
                let mut style = style.clone();
                style.position = taffy::Position::Absolute;
                style.inset = taffy::Rect {
                    left: taffy::LengthPercentageAuto::auto(),
                    right: taffy::LengthPercentageAuto::length(crate::num::f32_of_f64(icon + gap)),
                    top: taffy::LengthPercentageAuto::length(0.0),
                    bottom: taffy::LengthPercentageAuto::auto(),
                };
                style.flex_shrink = 0.0;
                let _ = taffy.set_style(node, style);
            }
            children.push(node);
        }
        let revealed = Disclose::reveal(self.progress(), self.state.body_height()).height;
        let style = Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            flex_shrink: 0.0,
            size: taffy::Size {
                width: taffy::Dimension::percent(1.0),
                height: taffy::Dimension::length(crate::num::f32_of_f64(header + revealed)),
            },
            // The body starts on the title's text column, not under the icon.
            padding: taffy::Rect {
                left: taffy::LengthPercentage::length(crate::num::f32_of_f64(Self::text_inset(
                    self.leading,
                    theme,
                ))),
                right: taffy::LengthPercentage::length(0.0),
                top: taffy::LengthPercentage::length(crate::num::f32_of_f64(header)),
                bottom: taffy::LengthPercentage::length(0.0),
            },
            overflow: taffy::Point {
                x: taffy::Overflow::Visible,
                y: taffy::Overflow::Hidden,
            },
            ..Default::default()
        };
        self.header_px = header;
        taffy.widget_node(style, &children, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        self.header = Rect::new(
            self.rect.x0,
            self.rect.y0,
            self.rect.x1,
            self.rect.y0 + self.header_px,
        );
        let children = taffy.children(node).unwrap_or_default();
        let row_origin = self.rect.origin();
        if let Some(&body) = children.first() {
            self.body.apply_layout(taffy, body, row_origin);
            let shown = taffy
                .style(body)
                .is_ok_and(|style| style.display != taffy::Display::None);
            if shown && self.state.set_body_height(self.body.get_rect().height()) {
                self.dirty = true;
            }
        }
        if let Some(control) = &mut self.trailing
            && let Some(&node) = children.get(1)
        {
            let height = f64::from(taffy.layout(node).map_or(0.0, |layout| layout.size.height));
            let centre = Point::new(
                row_origin.x,
                row_origin.y + ((self.header_px - height) / 2.0).max(0.0),
            );
            control.apply_layout(taffy, node, centre);
            self.trailing_rect = control.get_rect();
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
        let mut ctx = PaintCtx::new(scene, transform, text_renderer, theme);
        self.paint_header(&mut ctx);
        self.paint_separator(&mut ctx);
        if let Some(control) = &mut self.trailing {
            crate::widget::draw_child(
                control.as_mut(),
                scene,
                transform,
                text_renderer,
                theme,
                presence,
            );
        }
        let progress = self.progress();
        if progress <= 0.0 {
            return;
        }
        let reveal = Disclose::reveal(progress, self.state.body_height());
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
        let children = ctx.taffy.children(ctx.node_id).unwrap_or_default();
        if let Some(control) = &mut self.trailing
            && let Some(&node) = children.get(1)
        {
            control.handle_event(event, &mut ctx.reborrow(ctx.path, node));
            if *ctx.handled {
                self.dirty = true;
                return;
            }
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                let over = self.header.contains(p) && !self.over_trailing(p);
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
                        self.set_open(!self.is_open());
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
                    Key::Named(NamedKey::Enter | NamedKey::Space) => Some(!self.is_open()),
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
        if self.is_open()
            && let Some(&body) = children.first()
        {
            self.body
                .handle_event(event, &mut ctx.reborrow(ctx.path, body));
        }
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        _key_path: &[u64],
        _state: &mut crate::widget::Edits<'_>,
        _path: &crate::widget::ModelPath,
    ) -> bool {
        use crate::a11y::Action;
        let open = match action {
            Action::Activate => !self.is_open(),
            Action::Expand => true,
            Action::Collapse => false,
            _ => return false,
        };
        self.set_open(open);
        true
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

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        (self.header.contains(point) && !self.over_trailing(point))
            .then_some(winit::window::CursorIcon::Pointer)
    }

    fn needs_animation_frame(&self) -> bool {
        self.state.is_moving()
            || self.body.needs_animation_frame()
            || self
                .trailing
                .as_ref()
                .is_some_and(|control| control.needs_animation_frame())
    }

    fn is_layout_dirty(&self) -> bool {
        self.dirty
            || self.state.is_moving()
            || self.body.is_layout_dirty()
            || self
                .trailing
                .as_ref()
                .is_some_and(|control| control.is_layout_dirty())
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
            || self.state.is_moving()
            || self.body.needs_redraw()
            || self
                .trailing
                .as_ref()
                .is_some_and(|control| control.needs_redraw())
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        self.body.mark_drawn();
        if let Some(control) = &mut self.trailing {
            control.mark_drawn();
        }
    }

    // In the order of the row's layout nodes: the body, then the control.
    fn children(&self) -> Vec<&dyn Widget> {
        let mut children: Vec<&dyn Widget> = vec![self.body.as_ref()];
        if let Some(control) = &self.trailing {
            children.push(control.as_ref());
        }
        children
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        visitor(self.body.as_ref());
        if let Some(control) = &self.trailing {
            visitor(control.as_ref());
        }
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let mut children: Vec<&mut (dyn Widget + '_)> = vec![self.body.as_mut()];
        if let Some(control) = &mut self.trailing {
            children.push(control.as_mut());
        }
        children
    }

    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        if self.needs_redraw() {
            regions.push(self.rect);
        } else {
            self.body.collect_damage(regions);
            if let Some(control) = &self.trailing {
                control.collect_damage(regions);
            }
        }
    }

    fn take_structure_dirty(&mut self) -> bool {
        let control = self
            .trailing
            .as_mut()
            .is_some_and(|control| control.take_structure_dirty());
        self.body.take_structure_dirty() | control
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn name(&self) -> &'static str {
        "DisclosureRow"
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
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
                        .expanded(self.is_open()),
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
        assert!(group.open.borrow().is_none());
    }

    /// A view that reads its devices every two seconds rebuilds their rows
    /// each time; the row the person opened is open in every rebuild, at
    /// the height its body had, until they close it.
    #[test]
    fn a_keyed_row_rebuilt_from_fresh_data_stays_open_at_its_height() {
        let _settled = crate::motion::testing::settled();
        let group = DisclosureGroup::new();
        let body = || Box::new(crate::widget::Spacer::fixed(40.0)) as Box<dyn Widget>;
        let mut first = DisclosureRow::keyed("Headphones", body(), &group, "AA:01");
        first.set_open(true);
        assert!(first.state.set_body_height(40.0));
        drop(first);

        let rebuilt = DisclosureRow::keyed("Headphones", body(), &group, "AA:01");
        assert!(rebuilt.is_open(), "the rebuilt row is still open");
        assert!((rebuilt.state.body_height() - 40.0).abs() < 1e-6);
        assert!(
            (rebuilt.progress() - 1.0).abs() < 1e-6,
            "at rest, not reopening"
        );
        assert_eq!(group.open_key().as_deref(), Some("AA:01"));
        let other = DisclosureRow::keyed("Keyboard", body(), &group, "AA:02");
        assert!(!other.is_open());
    }

    #[test]
    fn opening_a_keyed_row_closes_the_open_one_across_rebuilds() {
        let _settled = crate::motion::testing::settled();
        let group = DisclosureGroup::new();
        let body = || Box::new(crate::widget::Spacer::fixed(40.0)) as Box<dyn Widget>;
        DisclosureRow::keyed("Headphones", body(), &group, "AA:01").set_open(true);
        DisclosureRow::keyed("Keyboard", body(), &group, "AA:02").set_open(true);
        assert!(!DisclosureRow::keyed("Headphones", body(), &group, "AA:01").is_open());
        assert!(DisclosureRow::keyed("Keyboard", body(), &group, "AA:02").is_open());
        // `opened` is a first-build default, never a reopening.
        DisclosureRow::keyed("Keyboard", body(), &group, "AA:02").set_open(false);
        let again = DisclosureRow::keyed("Keyboard", body(), &group, "AA:02").opened();
        assert!(!again.is_open(), "a row the person closed stays closed");
        group.retain(&["AA:01"]);
        assert!(group.open_key().is_none());
    }

    #[test]
    fn a_lone_row_shows_the_state_its_view_keeps() {
        let _settled = crate::motion::testing::settled();
        let state = DisclosureState::new();
        let body = || Box::new(crate::widget::Spacer::fixed(40.0)) as Box<dyn Widget>;
        DisclosureRow::new("Details", body())
            .with_state(&state)
            .set_open(true);
        assert!(state.is_open());
        assert!(
            DisclosureRow::new("Details", body())
                .with_state(&state)
                .is_open()
        );
    }

    #[test]
    fn the_open_body_starts_on_the_titles_text_column() {
        let _settled = crate::motion::testing::settled();
        let theme = Theme::dark();
        let body = Box::new(crate::widget::Spacer::fixed(40.0));
        let mut row = DisclosureRow::new("Saved devices", body)
            .secondary("3 devices")
            .icon(Icon::Bluetooth)
            .opened();
        let mut taffy = TaffyTree::<()>::new();
        let mut nodes: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = row.build_layout(&mut taffy, &theme, &mut nodes);
        taffy
            .compute_layout(
                node,
                taffy::Size {
                    width: taffy::AvailableSpace::Definite(400.0),
                    height: taffy::AvailableSpace::Definite(600.0),
                },
            )
            .expect("layout");
        row.apply_layout(&taffy, node, Point::ZERO);
        let inset = DisclosureRow::text_inset(Some(Icon::Bluetooth), &theme);
        assert!(inset > 0.0);
        assert!(
            (row.body.get_rect().x0 - (row.header.x0 + inset)).abs() < 0.5,
            "the body lines up with the title, not the icon"
        );
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
