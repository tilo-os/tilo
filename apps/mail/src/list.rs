//! The message list: one [`ListRowFace`] per conversation, newest first.
//!
//! Rows are the toolkit's; this widget lays them out under the header row,
//! scrolls them, and turns a click, a hover action, a swipe or a right click
//! into the matching mail action.

use std::collections::HashMap;

use taffy::{NodeId, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{BlendMode, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};

use ui::chrome::RowJoin;
use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{
    AvatarFace, EmptyState, EmptyStateContent, Interaction, ListRowFace, RowAction, RowLeading,
    ScrollRegion, SwipeAction, TaffyTreeExt, Widget,
};

use crate::model::{Folder, MessageId};
use crate::state::{CtxAction, Intent, MailState, Shared};
use crate::util::{format_relative, gravatar_url, local_minutes};

/// What the list says when it has no rows.
pub(crate) fn zero_state(
    has_messages: bool,
    has_accounts: bool,
    folder: Folder,
) -> (Icon, &'static str, Option<&'static str>) {
    if !has_accounts {
        (
            Icon::Mail,
            "No account yet",
            Some("Add an account to send and receive mail."),
        )
    } else if !has_messages {
        (Icon::Inbox, "Nothing has arrived yet", None)
    } else {
        let title = match folder {
            Folder::Inbox => "Your inbox is empty",
            Folder::Starred => "No starred messages",
            Folder::Drafts => "No drafts",
            Folder::Sent => "No sent messages",
            Folder::Archive => "Nothing archived",
            Folder::Junk => "No junk",
            Folder::Trash => "Trash is empty",
        };
        (folder.icon(), title, None)
    }
}

/// The actions a row shows while the pointer is over it.
fn row_actions(starred: bool) -> [(CtxAction, RowAction<'static>); 3] {
    [
        (
            CtxAction::ToggleStar,
            RowAction {
                icon: Icon::Star,
                label: if starred { "Remove star" } else { "Star" },
            },
        ),
        (
            CtxAction::Archive,
            RowAction {
                icon: Icon::Archive,
                label: "Archive",
            },
        ),
        (
            CtxAction::Delete,
            RowAction {
                icon: Icon::Trash2,
                label: "Delete",
            },
        ),
    ]
}

const SWIPE_ARCHIVE: SwipeAction<'static> = SwipeAction {
    icon: Icon::Archive,
    label: "Archive",
    destructive: false,
};

/// One row as last painted, with where its hover actions sit.
#[derive(Clone, Debug)]
struct Slot {
    id: MessageId,
    /// The row's whole band, gaps included: the height a press lands on.
    band: Rect,
    /// The painted row inside it.
    rect: Rect,
    starred: bool,
    /// How many messages the conversation holds.
    messages: usize,
    actions: Vec<Rect>,
}

/// How the rows sit in the list: their text on the column's content edge,
/// where the column's title starts, and their lifts bleeding past it (clear
/// of the scroll bar while the list scrolls); the first a step below the
/// header, edge to edge so a run of selected rows is one lift.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RowFrame {
    x0: f64,
    x1: f64,
    /// The top of the first row's band, before scrolling.
    top: f64,
    /// The distance from one row's band to the next.
    pitch: f64,
    /// The space between a band's edge and its painted row.
    gap: f64,
}

impl RowFrame {
    /// The frame of rows `row_h` tall in `list`.
    pub(crate) fn new(list: Rect, row_h: f64, scrolls: bool, theme: &Theme) -> Self {
        let spacing = &theme.tokens.spacing;
        let edge = ui::chrome::column_gutter(theme) - ui::chrome::row_bleed(theme);
        let x1 = if scrolls {
            list.x1 - f64::from(theme.tokens.metrics.scrollbar_gutter) - f64::from(spacing.s1)
        } else {
            list.x1 - edge
        };
        Self {
            x0: list.x0 + edge,
            x1,
            top: list.y0 + f64::from(spacing.s2),
            pitch: row_h,
            gap: 0.0,
        }
    }

    /// The height every row and the space around them take.
    pub(crate) fn content_height(&self, rows: usize, theme: &Theme) -> f64 {
        self.pitch * rows as f64 + 2.0 * f64::from(theme.tokens.spacing.s2)
    }

    /// Row `index`'s band, scrolled by `scroll`.
    pub(crate) fn band(&self, index: usize, scroll: f64) -> Rect {
        let y0 = self.top + index as f64 * self.pitch - scroll;
        Rect::new(self.x0, y0, self.x1, y0 + self.pitch)
    }

    /// Row `index` as painted, scrolled by `scroll`.
    pub(crate) fn row(&self, index: usize, scroll: f64) -> Rect {
        self.band(index, scroll).inset((0.0, -self.gap))
    }
}

/// A press on a row that may still become a swipe.
#[derive(Clone, Copy, Debug)]
struct Press {
    id: MessageId,
    at: Point,
    /// How far the row has slid toward its leading edge.
    offset: f64,
}

/// The text of one row.
struct RowText<'a> {
    sender: String,
    email: &'a str,
    subject: String,
    preview: String,
    when: String,
    unread: bool,
    starred: bool,
    attachments: bool,
}

impl<'a> RowText<'a> {
    /// The row of message `id`: the sender, the subject under it and the
    /// start of the message.
    fn of(id: MessageId, state: &'a MailState) -> Option<Self> {
        let message = state.mailbox.get(id)?;
        let when = format_relative(
            local_minutes(message.timestamp_min),
            local_minutes(state.now_min),
        );
        let subject = message.thread_subject();
        let subject = if subject.trim().is_empty() {
            "No subject".to_owned()
        } else {
            subject
        };
        Some(Self {
            sender: message.from.display(),
            email: &message.from.email,
            subject,
            preview: message.snippet(),
            when,
            unread: !message.read,
            starred: message.starred,
            attachments: message.has_attachments(),
        })
    }
}

pub struct MessageList {
    shared: Shared,
    rect: Rect,
    /// The rows' viewport: wheel, keyboard, bar and accessible scrolling.
    region: ScrollRegion,
    /// The theme the list was last drawn with, for the region's lanes.
    theme: Theme,
    row_h: f64,
    slots: Vec<Slot>,
    pointer: Option<Point>,
    press: Option<Press>,
    /// The selection the list last scrolled into view.
    revealed: Option<MessageId>,
    dirty: bool,
    /// Only the region's offset changed since the frame on screen: a host
    /// that copies what stays on screen may move the rows instead.
    scrolled: bool,
    /// The row under the pointer as this frame paints it and as the frame
    /// on screen did: its lift and actions follow the pointer, not the rows.
    hovered_drawing: Option<Rect>,
    hovered_presented: Option<Rect>,
}

impl MessageList {
    pub fn new(shared: Shared) -> Self {
        Self {
            shared,
            rect: Rect::ZERO,
            region: ScrollRegion::vertical(),
            theme: Theme::dark(),
            row_h: 0.0,
            slots: Vec::new(),
            pointer: None,
            press: None,
            revealed: None,
            dirty: true,
            scrolled: false,
            hovered_drawing: None,
            hovered_presented: None,
        }
    }

    fn slot_at(&self, point: Point) -> Option<&Slot> {
        if !self.rect.contains(point) {
            return None;
        }
        // A row answers across the list's whole width, its side insets
        // included.
        self.slots
            .iter()
            .find(|slot| slot.band.y0 <= point.y && point.y < slot.band.y1)
    }

    /// The hover action under `point` in `slot`, as painted.
    fn action_at(slot: &Slot, point: Point) -> Option<CtxAction> {
        slot.actions
            .iter()
            .position(|rect| rect.contains(point))
            .and_then(|index| {
                row_actions(slot.starred)
                    .get(index)
                    .map(|(action, _)| *action)
            })
    }

    /// Keep the selected conversation in view when the selection moves
    /// (arrow keys, a search hit, an agent opening a message).
    fn reveal(
        &mut self,
        rows: &[(MessageId, usize)],
        selected: Option<MessageId>,
        frame: RowFrame,
    ) {
        if selected == self.revealed || self.row_h <= 0.0 {
            return;
        }
        self.revealed = selected;
        let Some(index) = selected.and_then(|id| rows.iter().position(|(row, _)| *row == id))
        else {
            return;
        };
        let band = frame.band(index, 0.0);
        self.region
            .scroll_into_view(band - vello::kurbo::Vec2::new(self.rect.x0, self.rect.y0));
    }

    fn press(&mut self, doc: &mut ui::widget::Edits<'_>, point: Point) -> bool {
        let Some(slot) = self.slot_at(point).cloned() else {
            return false;
        };
        let mut state = self.shared.borrow_mut();
        if state.bulk_selecting {
            state.toggle_bulk_selection(slot.id);
            return true;
        }
        if let Some(action) = Self::action_at(&slot, point) {
            state.apply_message_action(doc, slot.id, action);
            return true;
        }
        let is_draft = state
            .mailbox
            .get(slot.id)
            .is_some_and(|message| message.folder == Folder::Drafts);
        if is_draft {
            // A draft reopens the composer, not the reader.
            state.push_intent(Intent::EditDraft(slot.id));
        } else {
            state.open_message(doc, slot.id);
        }
        drop(state);
        self.revealed = Some(slot.id);
        self.press = Some(Press {
            id: slot.id,
            at: point,
            offset: 0.0,
        });
        true
    }

    /// The row rects as last painted, for tests.
    #[cfg(test)]
    pub(crate) fn row_rects(&self) -> Vec<(MessageId, Rect)> {
        self.slots.iter().map(|slot| (slot.id, slot.rect)).collect()
    }
}

/// Where a row's hover actions sit in `rect`.
fn action_rects(starred: bool, rect: Rect, theme: &Theme) -> Vec<Rect> {
    let actions = row_actions(starred);
    let faces: Vec<RowAction<'_>> = actions.iter().map(|(_, action)| *action).collect();
    ListRowFace::new("")
        .subtitle("")
        .secondary("")
        .actions(&faces)
        .interaction(Interaction {
            hovered: true,
            ..Interaction::default()
        })
        .action_rects(rect, theme)
}

/// How one row stands among its neighbours.
#[derive(Clone, Copy, Debug)]
struct RowState {
    selected: bool,
    /// Whether the row above is selected.
    previous_selected: bool,
    /// Whether the row below is selected; `None` for the last row.
    next_selected: Option<bool>,
}

impl MessageList {
    /// Paint one conversation's row.
    fn paint_row(&self, ctx: &mut PaintCtx<'_>, state: &MailState, slot: &Slot, row: RowState) {
        let hovered = self
            .pointer
            .filter(|p| self.rect.contains(*p) && slot.rect.contains(*p) && !state.bulk_selecting);
        let Some(text) = RowText::of(slot.id, state) else {
            return;
        };
        let actions = row_actions(slot.starred);
        let action_faces: Vec<RowAction<'_>> = actions.iter().map(|(_, action)| *action).collect();
        let image = state.images.get(&gravatar_url(text.email));
        let avatar = AvatarFace::new(&text.sender, text.email).image(image.as_ref());
        let mut face = ListRowFace::new(&text.sender)
            .leading(RowLeading::Avatar(avatar))
            .subtitle(&text.subject)
            .secondary(&text.preview)
            .meta(&text.when)
            .emphasized(text.unread)
            .selected(row.selected)
            .joined(RowJoin::new(
                row.previous_selected,
                row.next_selected.unwrap_or(false),
            ))
            .actions(&action_faces)
            .interaction(Interaction {
                hovered: hovered.is_some(),
                ..Interaction::default()
            });
        if text.starred {
            face = face.mark(Icon::Star);
        }
        if text.attachments {
            face = face.mark(Icon::Paperclip);
        }
        if slot.messages > 1 {
            face = face.count(slot.messages);
        }
        if let Some(p) = hovered {
            face = face.hovered_action(slot.actions.iter().position(|r| r.contains(p)));
        }
        if let Some(next_selected) = row.next_selected {
            face = face.separator(next_selected);
        }
        if let Some(press) = self.press.filter(|p| p.id == slot.id && p.offset > 0.0) {
            face = face.swiped(SWIPE_ARCHIVE, press.offset);
        }
        face.paint(ctx, slot.rect);
    }
}

impl Widget for MessageList {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.fill_leaf(node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        let theme = self.theme.clone();
        self.region.set_viewport(self.rect, &theme);
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        let r = self.rect;
        let shared = self.shared.clone();
        let state = shared.borrow();
        let rows = state.list_rows();
        self.theme = theme.clone();
        self.region.set_viewport(r, theme);
        let mut ctx = PaintCtx::new(scene, t, tr, theme);
        if rows.is_empty() {
            self.slots.clear();
            self.region.set_content_size(r.width(), 0.0);
            let (icon, title, message) = zero_state(
                !state.mailbox.messages.is_empty(),
                !state.accounts.is_empty(),
                state.folder,
            );
            EmptyState::paint(
                &mut ctx,
                r,
                &EmptyStateContent {
                    icon: Some(icon),
                    message,
                    ..EmptyStateContent::new(title)
                },
                Point::new(-1.0, -1.0),
            );
            return;
        }

        self.row_h = ListRowFace::new("Ag")
            .subtitle("Ag")
            .secondary("Ag")
            .height(ctx.text, theme);
        let unscrolled = RowFrame::new(r, self.row_h, false, theme);
        self.region
            .set_content_size(r.width(), unscrolled.content_height(rows.len(), theme));
        // Rows stop short of the scroll bar when the list overflows.
        let frame = RowFrame::new(r, self.row_h, self.region.overflows_vertically(), theme);
        self.reveal(&rows, state.selected, frame);
        let scroll = self.region.offset().y;

        let selected_rows: Vec<bool> = rows
            .iter()
            .map(|(id, _)| {
                if state.bulk_selecting {
                    state.bulk_selection.contains(id)
                } else {
                    state.selected == Some(*id)
                }
            })
            .collect();
        ctx.scene
            .push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &r);
        let mut slots = Vec::with_capacity(rows.len());
        for (index, (id, len)) in rows.iter().enumerate() {
            let band = frame.band(index, scroll);
            let rect = frame.row(index, scroll);
            let starred = state.mailbox.get(*id).is_some_and(|m| m.starred);
            let slot = Slot {
                id: *id,
                band,
                rect,
                starred,
                messages: *len,
                actions: action_rects(starred, rect, theme),
            };
            if band.y1 >= r.y0 && band.y0 <= r.y1 {
                self.paint_row(
                    &mut ctx,
                    &state,
                    &slot,
                    RowState {
                        selected: selected_rows.get(index).copied().unwrap_or(false),
                        previous_selected: index
                            .checked_sub(1)
                            .and_then(|above| selected_rows.get(above).copied())
                            .unwrap_or(false),
                        next_selected: selected_rows.get(index + 1).copied(),
                    },
                );
            }
            slots.push(slot);
        }
        ctx.scene.pop_layer();
        self.region.note_shown(t, theme);
        self.region.paint_bars(ctx.scene, t, theme);
        self.hovered_drawing = self
            .pointer
            .filter(|point| r.contains(*point))
            .and_then(|point| slots.iter().find(|slot| slot.rect.contains(point)))
            .map(|slot| slot.band.union(slot.rect).inflate(2.0, 2.0));
        self.slots = slots;
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if *ctx.handled {
            return;
        }
        let theme = self.theme.clone();
        let offset = self.region.offset();
        if self.region.handle_event(event, ctx, &theme) {
            if self.region.offset() == offset {
                self.dirty = true;
            } else {
                self.scrolled = true;
            }
        }
        if *ctx.handled {
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let point = ctx.logical(position);
                self.pointer = Some(point);
                if let Some(press) = self.press.as_mut() {
                    press.offset = (press.at.x - point.x).max(0.0);
                }
                self.dirty = true;
            }
            WindowEvent::CursorLeft { .. } => {
                self.pointer = None;
                self.dirty = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(point) = self.pointer
                    && self.press(&mut ctx.state, point)
                {
                    *ctx.handled = true;
                    self.dirty = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                if let Some(press) = self.press.take() {
                    // A swipe across a quarter of the row archives it.
                    let row_w = self.rect.width();
                    if row_w > 0.0 && press.offset >= row_w / 4.0 {
                        self.shared.borrow_mut().apply_message_action(
                            &mut ctx.state,
                            press.id,
                            CtxAction::Archive,
                        );
                        *ctx.handled = true;
                    }
                    self.dirty = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } => {
                if let Some(point) = self.pointer
                    && let Some(slot) = self.slot_at(point)
                {
                    let (id, row) = (slot.id, slot.rect);
                    self.shared
                        .borrow_mut()
                        .push_intent(Intent::ContextMenu(id, point, row));
                    *ctx.handled = true;
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "MailList"
    }

    fn test_id(&self) -> Option<&str> {
        Some("mail_list")
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn tooltip(&self) -> Option<&str> {
        let point = self.pointer?;
        let slot = self.slot_at(point)?;
        Self::action_at(slot, point).map(|action| match action {
            CtxAction::ToggleStar if slot.starred => "Remove star (S)",
            CtxAction::ToggleStar => "Star (S)",
            CtxAction::Archive => "Archive (E)",
            _ => "Delete (Delete)",
        })
    }

    fn needs_redraw(&self) -> bool {
        self.dirty || self.scrolled || self.shared.borrow().dirty
    }

    /// A scroll that changed nothing else offers the host a copy of the
    /// rows that stay on screen; the row under the resting pointer, whose
    /// lift and actions do not move with the rows, repaints where it is and
    /// was.
    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        if !self.needs_redraw() {
            return;
        }
        if self.scrolled
            && !self.dirty
            && self.press.is_none()
            && !self.shared.borrow().dirty
            && self.region.offer_copy(self, regions)
        {
            regions.extend(
                self.hovered_drawing
                    .into_iter()
                    .chain(self.hovered_presented),
            );
            return;
        }
        regions.push(self.rect);
    }

    fn needs_animation_frame(&self) -> bool {
        self.region.needs_animation_frame(&self.theme)
    }

    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        self.region.next_wake_deadline(&self.theme)
    }

    fn mark_drawn(&mut self) {
        self.region.mark_presented();
        self.hovered_presented = self.hovered_drawing.take();
        self.dirty = false;
        self.scrolled = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::MailState;

    fn shared() -> Shared {
        MailState::shared(MailState::new_sample(crate::app::avatar_store_offline()))
    }

    fn draw(list: &mut MessageList) {
        let mut scene = Scene::new();
        list.draw(
            &mut scene,
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &Theme::dark(),
            &PresenceCtx::empty(),
        );
    }

    #[test]
    fn rows_open_their_conversation_and_hover_actions_act_on_their_row() {
        let shared = shared();
        let mut list = MessageList::new(shared.clone());
        list.rect = Rect::new(0.0, 0.0, 340.0, 760.0);
        draw(&mut list);
        let rows = list.row_rects();
        assert_eq!(rows.len(), shared.borrow().list_rows().len());
        assert!(
            rows.windows(2)
                .all(|pair| pair[0].1.y1 <= pair[1].1.y0 + 0.5),
            "rows stack without overlap"
        );

        // A press on the second row opens it.
        let (second, rect) = rows[1];
        let mut doc = crate::resource::TestMail::doc();
        assert!(list.press(&mut ui::widget::Edits::lend(&mut doc), rect.center()));
        assert_eq!(shared.borrow().selected, Some(second));

        // The archive action under the pointer archives that row only.
        let (first, rect) = rows[0];
        list.pointer = Some(rect.center());
        draw(&mut list);
        let slot = list.slot_at(rect.center()).cloned().expect("first row");
        let archive = slot.actions[1];
        assert!(list.press(&mut ui::widget::Edits::lend(&mut doc), archive.center()));
        assert_eq!(
            shared.borrow().mailbox.get(first).map(|m| m.folder),
            Some(Folder::Archive)
        );
    }

    /// A scroll that changes nothing else offers the host a copy of the
    /// rows, damaging only the bar's track and the row under the resting
    /// pointer; a host that does not copy, or any other change, repaints the
    /// list.
    #[test]
    fn a_scroll_alone_offers_a_copy_of_the_rows() {
        let shared = shared();
        let mut list = MessageList::new(shared.clone());
        list.rect = Rect::new(0.0, 0.0, 340.0, 400.0);
        let frame = |list: &mut MessageList| {
            draw(list);
            list.mark_drawn();
            shared.borrow_mut().dirty = false;
        };
        frame(&mut list);
        list.pointer = Some(Point::new(120.0, 220.0));
        frame(&mut list);
        assert!(
            list.region.scroll_by(vello::kurbo::Vec2::new(0.0, 40.0)),
            "the sample mailbox overflows the list"
        );
        list.scrolled = true;
        draw(&mut list);
        let mut regions = Vec::new();
        let offers = ui::scroll_copy::accepting(|| list.collect_damage(&mut regions));
        assert_eq!(offers.len(), 1, "the scroll is offered");
        assert_eq!(offers[0].delta(), vello::kurbo::Vec2::new(0.0, -40.0));
        let hovered = list.hovered_drawing.expect("a row is under the pointer");
        assert!(
            regions
                .iter()
                .all(|rect| rect.area() <= hovered.area() * 1.5 || rect.width() < 40.0),
            "only the track and the hovered row repaint: {regions:?}"
        );
        let mut plain = Vec::new();
        list.collect_damage(&mut plain);
        assert_eq!(
            plain,
            vec![list.rect],
            "a host that does not copy repaints the list"
        );
        list.dirty = true;
        let mut changed = Vec::new();
        let offers = ui::scroll_copy::accepting(|| list.collect_damage(&mut changed));
        assert!(offers.is_empty(), "another change repaints the list");
        assert_eq!(changed, vec![list.rect]);
    }

    #[test]
    fn every_empty_list_says_one_thing() {
        let (_, title, message) = zero_state(false, false, Folder::Inbox);
        assert_eq!(title, "No account yet");
        assert!(message.is_some());
        for folder in Folder::ALL {
            let (_, title, message) = zero_state(true, true, folder);
            assert!(message.is_none(), "{folder:?} needs one sentence");
            assert!(title.chars().next().is_some_and(char::is_uppercase));
        }
    }
}
