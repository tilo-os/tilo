//! The Contacts window's own widgets.
//!
//! The window is one [`ui::widget::NavShell`] (built in [`crate::app`]):
//! groups are the sidebar, [`ContactList`] is the overview, and the selected
//! person (or the edit form) is the content. [`crate::app::ContactsApp`] owns
//! the address book and every action; the panes own their rows, hit-testing
//! and paint. The two meet in a [`Bridge`]: the app publishes what the list
//! shows, the panes and header buttons queue what the person asked for, and
//! the app turns each request into a view change or a document action.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use taffy::style_helpers::{auto, length, percent};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::a11y::{AccessNode, AccessState, Action, Role};
use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::time::{Duration, Instant};
use ui::widget::{
    AppModel, AvatarFace, Column, EmptyState, EmptyStateContent, Interaction, ListRowFace,
    ModelPath, RowLeading, ScrollMetrics, Scrollbar, TaffyTreeExt, Widget,
};

use crate::form::{RowKind, TypeTarget};
use crate::model::ContactId;

/// Two presses on one row within this interval open it for editing.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

/// One row of the contact list: what the list shows of a person.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ListEntry {
    pub id: ContactId,
    pub name: String,
    /// The one secondary fact: the organization, or the job title.
    pub secondary: String,
}

/// What the person asked for, queued by a pane or a header button and
/// applied by the app after the event that produced it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Request {
    /// A row was chosen.
    Select(ContactId),
    /// A row was opened (double-click): edit it.
    Edit(ContactId),
    /// A row's actions were asked for at a window point.
    ContextMenu(ContactId, Point),
    NewContact,
    /// The overview header's More button.
    More,
    ToggleFavorite,
    EditSelected,
    DeleteSelected,
    Save,
    Cancel,
    Add(RowKind),
    Remove(TypeTarget),
    ToggleCalendar,
    /// A day chosen in the birthday calendar, as `YYYY-MM-DD`.
    PickDate(String),
    OpenWebsite(String),
    /// An answer to the open dialog.
    Dialog(DialogChoice),
}

/// A button of the open dialog.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DialogChoice {
    Primary,
    Secondary,
    /// Escape, a press on the scrim or the close control.
    Dismiss,
}

/// The state the app and its panes share.
#[derive(Default)]
pub(crate) struct Bridge {
    /// Bumped whenever anything the list shows changes.
    pub revision: u64,
    pub rows: Vec<ListEntry>,
    pub selected: Option<ContactId>,
    /// Shown in place of rows when the list is empty.
    pub empty_title: Option<String>,
    /// A row to scroll into view on the next paint.
    pub reveal: Option<ContactId>,
    /// The rows the list painted last, in window coordinates.
    pub painted: Vec<(ContactId, Rect)>,
    requests: Vec<Request>,
}

impl Bridge {
    pub(crate) fn push(&mut self, request: Request) {
        self.requests.push(request);
    }

    pub(crate) fn take_requests(&mut self) -> Vec<Request> {
        std::mem::take(&mut self.requests)
    }

    pub(crate) fn has_requests(&self) -> bool {
        !self.requests.is_empty()
    }
}

pub(crate) type Shared = Rc<RefCell<Bridge>>;

/// A closure that queues `request`, for a button's click handler.
pub(crate) fn queue(shared: &Shared, request: Request) -> impl FnMut() + 'static {
    let shared = Rc::clone(shared);
    move || shared.borrow_mut().push(request.clone())
}

fn fill_style() -> Style {
    Style {
        size: Size {
            width: Dimension::percent(1.0),
            height: Dimension::percent(1.0),
        },
        flex_grow: 1.0,
        ..Default::default()
    }
}

fn select_target(id: ContactId) -> String {
    format!("contacts.select:{id}")
}

/// The list of people: one [`ListRowFace`] per contact, avatar leading and
/// the organization as the secondary line. Only the rows in view are
/// measured and painted, so a book of many thousands stays responsive.
pub(crate) struct ContactList {
    shared: Shared,
    rect: Rect,
    scroll: f64,
    /// The measured row height, from the last paint.
    row_h: f64,
    /// The scroll bar's gutter, from the last paint.
    gutter: f64,
    seen: u64,
    cursor: Point,
    hovered: Option<ContactId>,
    pressed: Option<ContactId>,
    last_click: Option<(Instant, ContactId)>,
    bar: Scrollbar,
    dirty: bool,
}

impl ContactList {
    pub(crate) fn new(shared: Shared) -> Self {
        Self {
            shared,
            rect: Rect::ZERO,
            scroll: 0.0,
            row_h: 0.0,
            gutter: 0.0,
            seen: u64::MAX,
            cursor: Point::new(f64::NEG_INFINITY, f64::NEG_INFINITY),
            hovered: None,
            pressed: None,
            last_click: None,
            bar: Scrollbar::vertical(),
            dirty: true,
        }
    }

    fn metrics(&self) -> ScrollMetrics {
        let rows = self.shared.borrow().rows.len();
        ScrollMetrics::new(rows as f64 * self.row_h, self.rect.height(), self.scroll)
    }

    fn overflows(&self) -> bool {
        let m = self.metrics();
        m.content > m.viewport + 0.5
    }

    fn track(&self) -> Rect {
        Rect::new(
            self.rect.x1 - self.gutter,
            self.rect.y0,
            self.rect.x1,
            self.rect.y1,
        )
    }

    fn max_scroll(&self) -> f64 {
        let m = self.metrics();
        (m.content - m.viewport).max(0.0)
    }

    fn set_scroll(&mut self, scroll: f64) -> bool {
        let next = scroll.clamp(0.0, self.max_scroll());
        let changed = (next - self.scroll).abs() > f64::EPSILON;
        self.scroll = next;
        self.dirty |= changed;
        changed
    }

    /// The row under `point`, from the last paint.
    fn row_at(&self, point: Point) -> Option<ContactId> {
        if !self.rect.contains(point) || (self.overflows() && self.track().contains(point)) {
            return None;
        }
        self.shared
            .borrow()
            .painted
            .iter()
            .find(|(_, rect)| rect.contains(point))
            .map(|(id, _)| *id)
    }

    /// Scroll so the row at `index` is wholly in view.
    fn reveal_index(&mut self, index: usize) {
        if self.row_h <= 0.0 {
            return;
        }
        let top = index as f64 * self.row_h;
        let bottom = top + self.row_h;
        if top < self.scroll {
            self.set_scroll(top);
        } else if bottom > self.scroll + self.rect.height() {
            self.set_scroll(bottom - self.rect.height());
        }
    }

    fn paint(&mut self, ctx: &mut PaintCtx<'_>) {
        let theme = ctx.theme;
        let probe = ListRowFace::new("Ag")
            .secondary("Ag")
            .leading(RowLeading::Avatar(AvatarFace::new("Ag", "Ag")));
        self.row_h = probe.height(ctx.text, theme);
        self.gutter = f64::from(theme.tokens.metrics.scrollbar_gutter);
        let reveal = self.shared.borrow_mut().reveal.take();
        if let Some(id) = reveal {
            let index = self
                .shared
                .borrow()
                .rows
                .iter()
                .position(|row| row.id == id);
            if let Some(index) = index {
                self.reveal_index(index);
            }
        }
        self.set_scroll(self.scroll);

        let overflows = self.overflows();
        let bridge = self.shared.borrow();
        // Rows sit `s2` in from the column's edges, so the selection lift
        // stands clear of them like the sidebar's.
        let inset = f64::from(theme.tokens.spacing.s2);
        let row_x0 = self.rect.x0 + inset;
        let width_end = if overflows {
            self.rect.x1 - self.gutter
        } else {
            self.rect.x1 - inset
        };
        let mut painted = Vec::new();
        if self.row_h > 0.0 && !bridge.rows.is_empty() {
            let first = (self.scroll / self.row_h).floor().max(0.0) as usize;
            let count = (self.rect.height() / self.row_h).ceil() as usize + 1;
            let end = first.saturating_add(count).min(bridge.rows.len());
            ctx.scene.push_layer(
                vello::peniko::Fill::NonZero,
                vello::peniko::BlendMode::default(),
                1.0,
                ctx.transform,
                &self.rect,
            );
            for index in first..end {
                let Some(row) = bridge.rows.get(index) else {
                    break;
                };
                let y = self.rect.y0 + index as f64 * self.row_h - self.scroll;
                let rect = Rect::new(row_x0, y, width_end, y + self.row_h);
                let selected = bridge.selected == Some(row.id);
                let identity = row.id.to_string();
                let mut face = ListRowFace::new(&row.name)
                    .secondary(&row.secondary)
                    .leading(RowLeading::Avatar(AvatarFace::new(&row.name, &identity)))
                    .selected(selected)
                    .interaction(Interaction {
                        hovered: self.hovered == Some(row.id),
                        pressed: self.pressed == Some(row.id),
                        focused: false,
                        disabled: false,
                    });
                if let Some(next) = bridge.rows.get(index + 1) {
                    face = face.separator(bridge.selected == Some(next.id));
                }
                face.paint(ctx, rect);
                painted.push((row.id, rect));
            }
            ctx.scene.pop_layer();
        } else if let Some(title) = bridge.empty_title.as_deref() {
            let content = EmptyStateContent {
                icon: Some(Icon::Star),
                ..EmptyStateContent::new(title)
            };
            EmptyState::paint(ctx, self.rect, &content, self.cursor);
        }
        drop(bridge);
        if overflows {
            self.bar.draw(
                ctx.scene,
                ctx.transform,
                self.track(),
                self.metrics(),
                theme,
                false,
            );
        }
        self.shared.borrow_mut().painted = painted;
    }

    fn press(&mut self, button: MouseButton) -> bool {
        if !self.rect.contains(self.cursor) {
            return false;
        }
        if button == MouseButton::Left && self.overflows() {
            let track = self.track();
            match self.bar.press(track, self.metrics(), self.cursor) {
                ui::widget::scrollbar::PressOutcome::Thumb => {
                    self.dirty = true;
                    return true;
                }
                ui::widget::scrollbar::PressOutcome::Page(target) => {
                    self.set_scroll(target);
                    return true;
                }
                ui::widget::scrollbar::PressOutcome::Miss => {}
            }
        }
        let Some(id) = self.row_at(self.cursor) else {
            return true;
        };
        match button {
            MouseButton::Left => {
                self.pressed = Some(id);
                let now = ui::time::now();
                let double = self
                    .last_click
                    .is_some_and(|(at, last)| last == id && now.duration_since(at) < DOUBLE_CLICK);
                self.last_click = (!double).then_some((now, id));
                let request = if double {
                    Request::Edit(id)
                } else {
                    Request::Select(id)
                };
                self.shared.borrow_mut().push(request);
            }
            MouseButton::Right => {
                self.shared
                    .borrow_mut()
                    .push(Request::ContextMenu(id, self.cursor));
            }
            _ => {}
        }
        self.dirty = true;
        true
    }
}

impl Widget for ContactList {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy.leaf_node(fill_style());
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        let rect = Rect::from_origin_size(
            (
                origin.x + f64::from(l.location.x),
                origin.y + f64::from(l.location.y),
            ),
            (f64::from(l.size.width), f64::from(l.size.height)),
        );
        if rect != self.rect {
            self.rect = rect;
            self.dirty = true;
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        self.seen = self.shared.borrow().revision;
        self.paint(&mut PaintCtx::new(scene, transform, text, theme));
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                if self.bar.is_dragging() {
                    let track = self.track();
                    if let Some(offset) = self.bar.drag_to(track, self.metrics(), self.cursor) {
                        self.set_scroll(offset);
                    }
                    *ctx.handled = true;
                    return;
                }
                let track = self.track();
                let metrics = self.metrics();
                self.dirty |= self.bar.update_hover(track, metrics, self.cursor);
                let hovered = self.row_at(self.cursor);
                if hovered != self.hovered {
                    self.hovered = hovered;
                    self.dirty = true;
                }
            }
            WindowEvent::CursorLeft { .. } => {
                if self.hovered.take().is_some() {
                    self.dirty = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button,
                ..
            } => {
                if self.press(*button) {
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                let released = self.bar.release();
                if self.pressed.take().is_some() || released {
                    self.dirty = true;
                }
            }
            WindowEvent::MouseWheel { delta, .. } if self.rect.contains(self.cursor) => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => f64::from(-*y) * self.row_h.max(1.0),
                    MouseScrollDelta::PixelDelta(p) => -p.y / ctx.scale_factor,
                };
                self.set_scroll(self.scroll + dy);
                *ctx.handled = true;
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "ContactList"
    }

    fn test_id(&self) -> Option<&str> {
        Some("contacts.list")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || self.seen != self.shared.borrow().revision
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }

    fn tooltip(&self) -> Option<&str> {
        self.hovered
            .map(|_| "Double-click to edit; right-click for more actions")
    }

    fn accessibility(&self) -> Option<AccessNode> {
        let bridge = self.shared.borrow();
        let total = bridge.rows.len();
        let start = if self.row_h > 0.0 {
            (self.scroll / self.row_h).floor().max(0.0) as usize
        } else {
            0
        };
        let children = bridge
            .painted
            .iter()
            .enumerate()
            .filter_map(|(offset, (id, rect))| {
                let row = bridge.rows.iter().find(|row| row.id == *id)?;
                let identity = row.id.to_string();
                let face = ListRowFace::new(&row.name)
                    .secondary(&row.secondary)
                    .leading(RowLeading::Avatar(AvatarFace::new(&row.name, &identity)))
                    .selected(bridge.selected == Some(row.id));
                Some(
                    face.accessibility(*rect, select_target(row.id))
                        .with_position(start + offset + 1, total)
                        .with_state(
                            AccessState::new()
                                .focusable(true)
                                .context_menu(true)
                                .selected(bridge.selected == Some(row.id)),
                        ),
                )
            })
            .collect();
        Some(
            AccessNode::new(Role::List)
                .with_name("Contacts")
                .with_bounds(self.rect)
                .with_children(children),
        )
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &Action,
        _state: &mut AppModel,
        _path: &ModelPath,
    ) -> bool {
        let Some(id) = target
            .strip_prefix("contacts.select:")
            .and_then(|raw| raw.parse::<ContactId>().ok())
        else {
            return false;
        };
        let anchor = self
            .shared
            .borrow()
            .painted
            .iter()
            .find(|(row, _)| *row == id)
            .map_or_else(
                || self.rect.center(),
                |(_, rect)| Point::new(rect.x0 + rect.height(), rect.center().y),
            );
        let request = match action {
            Action::Activate | Action::Focus => Request::Select(id),
            Action::ShowContextMenu => Request::ContextMenu(id, anchor),
            _ => return false,
        };
        self.shared.borrow_mut().push(request);
        self.dirty = true;
        true
    }
}

/// The content column when there is no one to show: one sentence and, on a
/// first run, the one action that starts the book.
pub(crate) struct EmptyPane {
    shared: Shared,
    title: String,
    action: Option<(String, Request)>,
    rect: Rect,
    action_rect: Rect,
    cursor: Point,
    focused: bool,
    dirty: bool,
}

impl EmptyPane {
    pub(crate) fn new(shared: Shared, title: &str, action: Option<(&str, Request)>) -> Self {
        Self {
            shared,
            title: title.to_owned(),
            action: action.map(|(label, request)| (label.to_owned(), request)),
            rect: Rect::ZERO,
            action_rect: Rect::ZERO,
            cursor: Point::new(f64::NEG_INFINITY, f64::NEG_INFINITY),
            focused: false,
            dirty: true,
        }
    }

    fn activate(&self) {
        if let Some((_, request)) = &self.action {
            self.shared.borrow_mut().push(request.clone());
        }
    }
}

impl Widget for EmptyPane {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy.leaf_node(fill_style());
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::from_origin_size(
            (
                origin.x + f64::from(l.location.x),
                origin.y + f64::from(l.location.y),
            ),
            (f64::from(l.size.width), f64::from(l.size.height)),
        );
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let content = EmptyStateContent {
            icon: Some(Icon::Users),
            action_label: self.action.as_ref().map(|(label, _)| label.as_str()),
            ..EmptyStateContent::new(&self.title)
        };
        let mut ctx = PaintCtx::new(scene, transform, text, theme);
        self.action_rect =
            EmptyState::paint(&mut ctx, self.rect, &content, self.cursor).unwrap_or(Rect::ZERO);
        if self.focused && self.action_rect.width() > 0.0 {
            let radius = f64::from(theme.tokens.radii.control);
            ui::widget::paint_focus_ring(scene, transform, self.action_rect, radius, theme);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                let was = self.action_rect.contains(self.cursor);
                self.cursor = cursor;
                self.dirty |= was != self.action_rect.contains(cursor);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if self.action_rect.contains(self.cursor) => {
                self.activate();
                *ctx.handled = true;
            }
            WindowEvent::KeyboardInput { event: key, .. }
                if self.focused
                    && key.state == ElementState::Pressed
                    && matches!(
                        key.logical_key,
                        Key::Named(NamedKey::Enter | NamedKey::Space)
                    ) =>
            {
                self.activate();
                *ctx.handled = true;
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "ContactsEmptyPane"
    }

    fn test_id(&self) -> Option<&str> {
        Some("contacts.empty")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn is_focusable(&self) -> bool {
        self.action.is_some()
    }

    fn set_focus(&mut self, focused: bool) {
        self.dirty |= self.focused != focused;
        self.focused = focused;
    }

    fn is_focused(&self) -> bool {
        self.focused
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }

    fn accessibility(&self) -> Option<AccessNode> {
        let mut children = vec![AccessNode::new(Role::StaticText).with_name(self.title.clone())];
        if let Some((label, _)) = &self.action {
            children.push(
                AccessNode::new(Role::Button)
                    .with_name(label.clone())
                    .with_action_target("contacts.empty.action")
                    .with_bounds(self.action_rect)
                    .with_state(AccessState::new().focusable(true).focused(self.focused)),
            );
        }
        Some(
            AccessNode::new(Role::Group)
                .with_bounds(self.rect)
                .with_children(children),
        )
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &Action,
        _state: &mut AppModel,
        _path: &ModelPath,
    ) -> bool {
        if target != "contacts.empty.action" {
            return false;
        }
        match action {
            Action::Activate => self.activate(),
            Action::Focus => self.focused = true,
            _ => return false,
        }
        self.dirty = true;
        true
    }
}

/// A column bounded to a readable measure and centred in the space it is
/// given, with the page gutter around it: detail pages and forms do not
/// stretch across a wide window.
pub(crate) struct Readable {
    column: Column,
    /// Its children are groups of sections (a page title, then a form),
    /// spaced as such.
    groups: bool,
}

impl Readable {
    pub(crate) fn new(column: Column) -> Self {
        Self {
            column,
            groups: false,
        }
    }

    /// A page whose children are groups of sections.
    pub(crate) fn of_groups(column: Column) -> Self {
        Self {
            column,
            groups: true,
        }
    }
}

impl ui::widget::Decorator for Readable {
    type Inner = Column;

    fn inner(&self) -> &Column {
        &self.column
    }

    fn inner_mut(&mut self) -> &mut Column {
        &mut self.column
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = self.column.build_layout(taffy, theme, node_map);
        if let Ok(style) = taffy.style(node) {
            let mut style = style.clone();
            let gutter = theme.tokens.spacing.s6;
            // The readable measure forms use (`FormPage`), plus its gutters.
            style.max_size.width = length(720.0 * theme.ui_scale + 2.0 * gutter);
            style.size.width = percent(1.0_f32);
            style.margin.left = auto();
            style.margin.right = auto();
            if self.groups {
                style.gap.height = length(theme.tokens.spacing.rhythm.group);
            }
            let _ = taffy.set_style(node, style);
        }
        node
    }
}
