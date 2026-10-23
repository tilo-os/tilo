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

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, MouseButton, WindowEvent};

use ui::a11y::{AccessNode, AccessState, Action, Role};
use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::time::Instant;
use ui::widget::{
    AvatarFace, EmptyState, EmptyStateContent, Interaction, ListRowFace, RowLeading, ScrollRegion,
    TaffyTreeExt, Widget,
};

use crate::form::{RowKind, Slot, TypeTarget};
use crate::model::ContactId;

/// One row of the contact list: what the list shows of a person.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ListEntry {
    pub id: ContactId,
    pub name: String,
    /// The one secondary fact: the organization, or the job title.
    pub secondary: String,
    /// A favourite carries a filled star after the name.
    pub favorite: bool,
    /// The person's picture, as [`Bridge::pictures`] holds it; empty for
    /// none, which shows their initials.
    pub photo: String,
}

/// What the person asked for, queued by a pane or a header button and
/// applied by the app after the event that produced it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Request {
    /// A row was chosen.
    Select(ContactId),
    /// A search hit or an agent opened a contact: show it, even when the
    /// chosen group hides it.
    Reveal(ContactId),
    /// A row was opened (double-click): edit it.
    Edit(ContactId),
    /// Edit the chosen fact through the same retained form.
    EditField(ContactId, Slot),
    /// A row's actions were asked for: at the pointer's window point, or
    /// (from the keyboard or assistive technology) under the row.
    ContextMenu(ContactId, Option<Point>),
    /// The list's order was chosen.
    Sort(crate::model::SortKey),
    NewContact,
    ToggleFavorite,
    EditSelected,
    DeleteSelected,
    Save,
    /// Enter submitted a shared input: focus the next field, or save at the end.
    AdvanceField(Slot),
    /// A local request to focus an existing draft field.
    FocusField(Slot),
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
    /// The empty state's sentence under its title, when it has one.
    pub empty_message: Option<String>,
    /// The empty state's icon (a star for favorites when `None`).
    pub empty_icon: Option<Icon>,
    /// A row to scroll into view on the next paint.
    pub reveal: Option<ContactId>,
    /// The rows the list painted last, in window coordinates.
    pub painted: Vec<(ContactId, Rect)>,
    /// The book as it was presented last: what a search hit or an agent
    /// may open.
    pub book: Rc<crate::resource::Contacts>,
    /// The pictures of the people shown, decoded off the UI thread by
    /// [`Self::images`].
    pub pictures: ui::held_images::HeldImages,
    pub images: Option<ui::ImageStore>,
    requests: Vec<Request>,
}

impl Bridge {
    /// The decoded picture at `address`, once it has loaded.
    pub(crate) fn picture(&self, address: &str) -> Option<vello::peniko::ImageData> {
        self.images.as_ref()?.get(address)
    }

    /// What the list shows in place of rows, when it says anything.
    pub(crate) fn empty_content(&self) -> Option<EmptyStateContent<'_>> {
        let title = self.empty_title.as_deref()?;
        Some(EmptyStateContent {
            icon: Some(self.empty_icon.unwrap_or(Icon::Star)),
            message: self.empty_message.as_deref(),
            ..EmptyStateContent::new(title)
        })
    }
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

/// The rest of the overview column, under the controls above the list.
fn fill_style() -> Style {
    Style {
        size: Size {
            width: Dimension::percent(1.0),
            height: Dimension::auto(),
        },
        min_size: Size::from_lengths(0.0, 0.0),
        flex_basis: Dimension::length(0.0),
        flex_grow: 1.0,
        ..Default::default()
    }
}

fn select_target(id: ContactId) -> String {
    format!("contacts.select:{id}")
}

/// A contact's row: the name, and under it the organization or title when
/// there is one. A row with neither centres the name instead of keeping an
/// empty second line.
fn row_face(row: &ListEntry) -> ListRowFace<'_> {
    let face = ListRowFace::new(&row.name);
    if row.secondary.trim().is_empty() {
        face
    } else {
        face.secondary(&row.secondary)
    }
}

/// The list of people: one [`ListRowFace`] per contact, avatar leading and
/// the organization as the secondary line. Only the rows in view are
/// measured and painted, so a book of many thousands stays responsive.
pub(crate) struct ContactList {
    shared: Shared,
    rect: Rect,
    scroller: ScrollRegion,
    /// The theme of the last paint, which the scroll region's lanes, steps
    /// and fade read between paints.
    theme: Theme,
    /// The measured row height, from the last paint.
    row_h: f64,
    seen: u64,
    cursor: Point,
    hovered: Option<ContactId>,
    pressed: Option<ContactId>,
    last_click: Option<(Instant, ContactId)>,
    /// Something the rows paint from changed besides the offset.
    dirty: bool,
    /// Only the offset changed since the frame on screen: a host that
    /// copies what stays on screen may move the rows instead.
    scrolled: bool,
}

impl ContactList {
    pub(crate) fn new(shared: Shared) -> Self {
        Self {
            shared,
            rect: Rect::ZERO,
            scroller: ScrollRegion::vertical(),
            theme: Theme::dark(),
            row_h: 0.0,
            seen: u64::MAX,
            cursor: Point::new(f64::NEG_INFINITY, f64::NEG_INFINITY),
            hovered: None,
            pressed: None,
            last_click: None,
            dirty: true,
            scrolled: false,
        }
    }

    /// The row under `point`, from the last paint.
    fn row_at(&self, point: Point) -> Option<ContactId> {
        if !self.scroller.content_rect().contains(point) {
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
        let before = self.scroller.offset();
        self.scroller
            .scroll_into_view(Rect::new(0.0, top, 0.0, top + self.row_h));
        self.dirty |= self.scroller.offset() != before;
    }

    fn paint(&mut self, ctx: &mut PaintCtx<'_>) {
        let theme = ctx.theme;
        let probe = ListRowFace::new("Ag")
            .secondary("Ag")
            .leading(RowLeading::Avatar(AvatarFace::new("Ag", "Ag")));
        self.row_h = probe.height(ctx.text, theme);
        self.theme = theme.clone();
        let rows = self.shared.borrow().rows.len();
        self.scroller.set_viewport(self.rect, theme);
        self.scroller
            .set_content_size(0.0, rows as f64 * self.row_h);
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

        let scroll = self.scroller.offset().y;
        let bridge = self.shared.borrow();
        // The rows' text starts on the column's content edge, where the
        // column's title starts; a row's lift bleeds past it. The scroll bar
        // keeps its gutter.
        let edge = ui::chrome::column_gutter(theme) - ui::chrome::row_bleed(theme);
        let row_x0 = self.rect.x0 + edge;
        let width_end = if self.scroller.overflows_vertically() {
            self.scroller.content_rect().x1
        } else {
            self.rect.x1 - edge
        };
        let mut painted = Vec::new();
        if self.row_h > 0.0 && !bridge.rows.is_empty() {
            let first = (scroll / self.row_h).floor().max(0.0) as usize;
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
                let y = self.rect.y0 + index as f64 * self.row_h - scroll;
                let rect = Rect::new(row_x0, y, width_end, y + self.row_h);
                let selected = bridge.selected == Some(row.id);
                let identity = row.id.to_string();
                let picture = bridge.picture(&row.photo);
                let mut face = row_face(row)
                    .leading(RowLeading::Avatar(
                        AvatarFace::new(&row.name, &identity).image(picture.as_ref()),
                    ))
                    .selected(selected)
                    .interaction(Interaction {
                        hovered: self.hovered == Some(row.id),
                        pressed: self.pressed == Some(row.id),
                        focused: false,
                        disabled: false,
                    });
                if row.favorite {
                    face = face.mark(Icon::Star);
                }
                if let Some(next) = bridge.rows.get(index + 1) {
                    face = face.separator(bridge.selected == Some(next.id));
                }
                face.paint(ctx, rect);
                painted.push((row.id, rect));
            }
            ctx.scene.pop_layer();
        } else if let Some(content) = bridge.empty_content() {
            EmptyState::paint(ctx, self.rect, &content, self.cursor);
        }
        drop(bridge);
        self.scroller.note_shown(ctx.transform, theme);
        self.scroller.paint_bars(ctx.scene, ctx.transform, theme);
        self.shared.borrow_mut().painted = painted;
    }

    fn press(&mut self, button: MouseButton) -> bool {
        if !self.rect.contains(self.cursor) {
            return false;
        }
        let Some(id) = self.row_at(self.cursor) else {
            return true;
        };
        match button {
            MouseButton::Left => {
                self.pressed = Some(id);
                let now = ui::time::now();
                // Two presses on one row within the shared multi-click window
                // open it for editing, so the list honours the same interval
                // as every other double-click in the session.
                let double = self.last_click.is_some_and(|(at, last)| {
                    last == id && now.duration_since(at) < ui::gesture::multi_click_interval()
                });
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
                    .push(Request::ContextMenu(id, Some(self.cursor)));
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
        taffy.widget_leaf(fill_style(), node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let rect = ui::widget::layout_rect(taffy, node, origin);
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
        // A new book revision is a change to the rows: the damage
        // collected after this draw must cover it.
        let revision = self.shared.borrow().revision;
        self.dirty |= self.seen != revision;
        self.seen = revision;
        self.paint(&mut PaintCtx::new(scene, transform, text, theme));
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        // The scroll region takes the wheel, touchpad gestures and its bar.
        // A move of the offset alone moves the rows (the hovered and pressed
        // rows are rows, and move with them); anything else repaints.
        let offset = self.scroller.offset();
        if self.scroller.handle_event(event, ctx, &self.theme) {
            if self.scroller.offset() == offset {
                self.dirty = true;
            } else {
                self.scrolled = true;
            }
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = ctx.logical(position);
                if self.scroller.is_dragging() {
                    return;
                }
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
            } if !*ctx.handled => {
                if self.press(*button) {
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } if self.pressed.take().is_some() => self.dirty = true,
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
        self.dirty || self.scrolled || self.seen != self.shared.borrow().revision
    }

    /// A scroll that changed nothing else offers the host a copy of the
    /// rows that stay on screen; a thumb fading while nothing else changes
    /// repaints its track. Anything else repaints the list.
    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        if !self.needs_redraw() {
            self.scroller.damage_changed_bars(regions);
            return;
        }
        if self.scrolled && !self.dirty && self.scroller.offer_copy(self, regions) {
            return;
        }
        regions.push(self.rect);
    }

    fn mark_drawn(&mut self) {
        self.scroller.mark_presented();
        self.dirty = false;
        self.scrolled = false;
    }

    fn needs_animation_frame(&self) -> bool {
        self.scroller.needs_animation_frame(&self.theme)
    }

    /// A glide after a flick scrolls as the wheel does: a copy.
    fn update(&mut self, _state: &ui::widget::AppModel, _path: &ui::widget::ModelPath) -> bool {
        self.scrolled |= self.scroller.advance();
        false
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        self.scroller.next_wake_deadline(&self.theme)
    }

    fn wants_pointer_capture(&self) -> bool {
        self.scroller.is_dragging()
    }

    fn tooltip(&self) -> Option<&str> {
        self.hovered
            .map(|_| "Double-click to edit; right-click for more actions")
    }

    fn accessibility(&self) -> Option<AccessNode> {
        let bridge = self.shared.borrow();
        let total = bridge.rows.len();
        let start = if self.row_h > 0.0 {
            (self.scroller.offset().y / self.row_h).floor().max(0.0) as usize
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
                let picture = bridge.picture(&row.photo);
                let mut face = row_face(row)
                    .leading(RowLeading::Avatar(
                        AvatarFace::new(&row.name, &identity).image(picture.as_ref()),
                    ))
                    .selected(bridge.selected == Some(row.id));
                if row.favorite {
                    face = face.mark(Icon::Star);
                }
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
            .collect::<Vec<_>>();
        // An empty list says why in words, for assistive technology too.
        let children = match bridge.empty_content() {
            Some(content) if bridge.rows.is_empty() => {
                vec![content.access_node(self.rect, None, "")]
            }
            _ => children,
        };
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
        _state: &mut ui::widget::Edits<'_>,
        _path: &ui::widget::ModelPath,
    ) -> bool {
        let Some(id) = target
            .strip_prefix("contacts.select:")
            .and_then(|raw| raw.parse::<ContactId>().ok())
        else {
            return false;
        };
        let request = match action {
            Action::Activate | Action::Focus => Request::Select(id),
            Action::ShowContextMenu => Request::ContextMenu(id, None),
            _ => return false,
        };
        self.shared.borrow_mut().push(request);
        self.dirty = true;
        true
    }
}

/// The content column when there is no one to show: the toolkit's empty
/// state, one sentence and, on a first run, the one action that starts the
/// book.
pub(crate) fn empty_pane(
    shared: &Shared,
    title: &str,
    message: Option<&str>,
    action: Option<(&str, Request)>,
) -> ui::widget::Element {
    let mut state = EmptyState::new(title).with_icon(Icon::Users);
    if let Some(message) = message {
        state = state.with_message(message);
    }
    Box::new(match action {
        Some((label, request)) => state
            .with_action(label)
            .with_action_id(EMPTY_ACTION)
            .on_view(queue(shared, request)),
        None => state,
    })
}

/// The empty state's action, for assistive technology and agents.
pub(crate) const EMPTY_ACTION: &str = "contacts.empty.action";

#[cfg(test)]
#[path = "view/copy_tests.rs"]
mod copy_tests;
