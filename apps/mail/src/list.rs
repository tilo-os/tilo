//! The message list: one [`ListRowFace`] per conversation, newest first —
//! or, while an account's outbox is open, one per message still to leave,
//! with why it has not.
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
    AvatarFace, Banner, BannerContent, EmptyState, EmptyStateContent, Interaction, ListRowFace,
    RowAction, RowLeading, ScrollRegion, SwipeAction, TaffyTreeExt, Widget,
};

use crate::i18n::CATALOG;
use crate::model::{Folder, MessageId};
use crate::state::{CtxAction, Intent, MailState, Shared};
use crate::util::{format_relative, local_minutes};

pub(crate) struct ZeroState {
    icon: Icon,
    title: String,
    action: Option<String>,
}

pub(crate) fn zero_state(has_messages: bool, folder: Folder) -> ZeroState {
    if !has_messages {
        return ZeroState {
            icon: Icon::Inbox,
            title: crate::i18n::t("mail-zero-nothing-yet"),
            action: Some(crate::i18n::t("mail-account-settings")),
        };
    }
    ZeroState {
        icon: folder.icon(),
        title: CATALOG.t_args(
            "mail-zero-folder",
            &[(
                "folder",
                ui::i18n::ArgValue::Text(crate::i18n::t(folder.message())),
            )],
        ),
        action: None,
    }
}

/// What a row's own verbs are called, in the window's language: the hover
/// buttons and the swipe say what the context menu and the bar say.
#[derive(Clone, Debug)]
struct RowLabels {
    star: String,
    unstar: String,
    archive: String,
    delete: String,
}

impl RowLabels {
    fn of() -> Self {
        Self {
            star: CtxAction::ToggleStar.label(false, false),
            unstar: CtxAction::ToggleStar.label(false, true),
            archive: CtxAction::Archive.label(false, false),
            delete: CtxAction::Delete.label(false, false),
        }
    }
}

/// The actions a row shows while the pointer is over it.
fn row_actions(labels: &RowLabels, starred: bool) -> [(CtxAction, RowAction<'_>); 3] {
    [
        (
            CtxAction::ToggleStar,
            RowAction {
                icon: Icon::Star,
                label: if starred {
                    &labels.unstar
                } else {
                    &labels.star
                },
            },
        ),
        (
            CtxAction::Archive,
            RowAction {
                icon: Icon::Archive,
                label: &labels.archive,
            },
        ),
        (
            CtxAction::Delete,
            RowAction {
                icon: Icon::Trash2,
                label: &labels.delete,
            },
        ),
    ]
}

const CANCEL_SEND: RowAction<'static> = RowAction {
    icon: Icon::RotateCcw,
    label: "Cancel sending",
};

#[derive(Clone, Debug)]
struct QueuedSlot {
    id: String,
    band: Rect,
    rect: Rect,
    cancel: Rect,
}

const OLDER_AHEAD_ROWS: u32 = 10;

/// The swipe across a row: it archives the message when it completes.
fn swipe_archive(labels: &RowLabels) -> SwipeAction<'_> {
    SwipeAction {
        icon: Icon::Archive,
        label: &labels.archive,
        destructive: false,
    }
}

#[derive(Clone, Debug)]
struct Slot {
    id: MessageId,
    band: Rect,
    rect: Rect,
    starred: bool,
    messages: usize,
    actions: Vec<Rect>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct RowFrame {
    x0: f64,
    x1: f64,
    top: f64,
    pitch: f64,
    gap: f64,
}

impl RowFrame {
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

    pub(crate) fn content_height(&self, rows: usize, theme: &Theme) -> f64 {
        self.pitch * rows as f64 + 2.0 * f64::from(theme.tokens.spacing.s2)
    }

    pub(crate) fn band(&self, index: usize, scroll: f64) -> Rect {
        let y0 = self.top + index as f64 * self.pitch - scroll;
        Rect::new(self.x0, y0, self.x1, y0 + self.pitch)
    }

    pub(crate) fn row(&self, index: usize, scroll: f64) -> Rect {
        self.band(index, scroll).inset((0.0, -self.gap))
    }
}

#[derive(Clone, Copy, Debug)]
struct Press {
    id: MessageId,
    at: Point,
    offset: f64,
}

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
    fn of(id: MessageId, state: &'a MailState) -> Option<Self> {
        let message = state.mailbox.get(id)?;
        let when = format_relative(
            local_minutes(message.timestamp_min),
            local_minutes(state.now_min),
        );
        let subject = message.thread_subject();
        let subject = if subject.trim().is_empty() {
            crate::i18n::no_subject()
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
    region: ScrollRegion,
    theme: Theme,
    row_h: f64,
    slots: Vec<Slot>,
    queued: Vec<QueuedSlot>,
    pointer: Option<Point>,
    press: Option<Press>,
    revealed: Option<MessageId>,
    dirty: bool,
    scrolled: bool,
    hovered_drawing: Option<Rect>,
    hovered_presented: Option<Rect>,
    arriving_step: Option<ui::arriving::Step>,
    zero_action: Option<(Rect, Intent)>,
    /// The failed check's control as last painted, in the list's banner or in
    /// the empty list's words: the way out of a check that cannot run.
    sync_action: Option<(Rect, Intent)>,
    /// What a row's own verbs are called, in the window's language.
    labels: RowLabels,
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
            queued: Vec::new(),
            pointer: None,
            press: None,
            revealed: None,
            dirty: true,
            scrolled: false,
            hovered_drawing: None,
            hovered_presented: None,
            arriving_step: None,
            zero_action: None,
            sync_action: None,
            labels: RowLabels::of(),
        }
    }

    fn arriving_step_moved(&self) -> bool {
        let Some(painted) = self.arriving_step else {
            return false;
        };
        self.shared
            .borrow()
            .first_fetch
            .as_ref()
            .is_some_and(|clock| clock.step(ui::motion::frame_now()) != painted)
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

    /// The one control a failed check, or a mailbox no mail has arrived in,
    /// offers when `point` is on it: its press asks the app for the same thing
    /// its accessible control does.
    fn route_at(&self, point: Point) -> Option<Intent> {
        self.sync_action
            .iter()
            .chain(self.zero_action.iter())
            .find(|(pill, _)| pill.contains(point))
            .map(|(_, intent)| intent.clone())
    }

    /// The hover action under `point` in `slot`, as painted.
    fn action_at(&self, slot: &Slot, point: Point) -> Option<CtxAction> {
        slot.actions
            .iter()
            .position(|rect| rect.contains(point))
            .and_then(|index| {
                row_actions(&self.labels, slot.starred)
                    .get(index)
                    .map(|(action, _)| *action)
            })
    }

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
        if let Some(action) = self.action_at(&slot, point) {
            state.apply_message_action(doc, slot.id, action);
            return true;
        }
        let is_draft = state
            .mailbox
            .get(slot.id)
            .is_some_and(|message| message.folder == Folder::Drafts);
        if is_draft {
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

    fn hover_at(&self, point: Option<Point>) -> Option<(MessageId, Option<usize>)> {
        let point = point?;
        let slot = self.slots.iter().find(|slot| slot.rect.contains(point))?;
        if !self.rect.contains(point) {
            return None;
        }
        Some((
            slot.id,
            slot.actions.iter().position(|rect| rect.contains(point)),
        ))
    }

    fn pick(&self, point: Point, range: bool) -> bool {
        let Some(slot) = self.slot_at(point).cloned() else {
            return false;
        };
        let mut state = self.shared.borrow_mut();
        if !state.bulk_selecting {
            state.set_bulk_selecting(true);
            if let Some(open) = state.selected
                && open != slot.id
            {
                state.toggle_bulk_selection(open);
            }
        }
        if range {
            let ids: Vec<MessageId> = self.slots.iter().map(|slot| slot.id).collect();
            let anchor = state
                .bulk_selection
                .last()
                .and_then(|last| ids.iter().position(|id| id == last));
            let target = ids.iter().position(|id| *id == slot.id);
            if let (Some(anchor), Some(target)) = (anchor, target) {
                let (from, to) = (anchor.min(target), anchor.max(target));
                for id in ids.get(from..=to).unwrap_or_default() {
                    if !state.bulk_selection.contains(id) {
                        state.toggle_bulk_selection(*id);
                    }
                }
                return true;
            }
        }
        state.toggle_bulk_selection(slot.id);
        true
    }

    #[cfg(test)]
    pub(crate) fn row_rects(&self) -> Vec<(MessageId, Rect)> {
        self.slots.iter().map(|slot| (slot.id, slot.rect)).collect()
    }
}

fn action_rects(labels: &RowLabels, starred: bool, rect: Rect, theme: &Theme) -> Vec<Rect> {
    let actions = row_actions(labels, starred);
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

#[derive(Clone, Copy, Debug)]
struct RowState {
    selected: bool,
    previous_selected: bool,
    next_selected: Option<bool>,
}

impl MessageList {
    fn paint_outbox(&mut self, ctx: &mut PaintCtx<'_>, state: &MailState) {
        let r = self.rect;
        let theme = ctx.theme;
        let queued = state.queued();
        self.queued.clear();
        if queued.is_empty() {
            self.region.set_content_size(r.width(), 0.0);
            EmptyState::paint(
                ctx,
                r,
                &EmptyStateContent {
                    icon: Some(Icon::Send),
                    message: Some("Messages wait here until they are sent."),
                    ..EmptyStateContent::new("Nothing is waiting to be sent")
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
            .set_content_size(r.width(), unscrolled.content_height(queued.len(), theme));
        let frame = RowFrame::new(r, self.row_h, self.region.overflows_vertically(), theme);
        let scroll = self.region.offset().y;
        ctx.scene
            .push_layer(Fill::NonZero, BlendMode::default(), 1.0, ctx.transform, &r);
        for (index, message) in queued.iter().enumerate() {
            let band = frame.band(index, scroll);
            let rect = frame.row(index, scroll);
            if band.y1 < r.y0 || band.y0 > r.y1 {
                self.queued.push(QueuedSlot {
                    id: message.id.clone(),
                    band,
                    rect,
                    cancel: Rect::ZERO,
                });
                continue;
            }
            let selected = state.outbox_selected.as_deref() == Some(message.id.as_str());
            let hovered = self
                .pointer
                .filter(|point| r.contains(*point) && rect.contains(*point));
            let actions = [CANCEL_SEND];
            let recipient = crate::i18n::t_args(
                "mail-to",
                &[("recipient", ui::i18n::ArgValue::Text(message.to.clone()))],
            );
            let state_line = message.state_line();
            let mut face = ListRowFace::new(&recipient)
                .leading(RowLeading::Icon(Icon::Send))
                .subtitle(&message.subject)
                .secondary(&state_line)
                .selected(selected)
                .actions(&actions)
                .interaction(Interaction {
                    hovered: hovered.is_some(),
                    ..Interaction::default()
                });
            // A refused message says so with the mark as well as the words.
            if message.reason.is_some() {
                face = face.mark(Icon::CircleAlert);
            }
            let actions_at = face.action_rects(rect, theme);
            let under = hovered
                .and_then(|point| actions_at.iter().position(|action| action.contains(point)));
            if let Some(position) = under {
                face = face.hovered_action(Some(position));
            }
            face.paint(ctx, rect);
            let cancel = actions_at
                .get(under.unwrap_or(0))
                .copied()
                .unwrap_or(Rect::ZERO);
            self.queued.push(QueuedSlot {
                id: message.id.clone(),
                band,
                rect,
                cancel,
            });
        }
        ctx.scene.pop_layer();
        self.region.note_shown(ctx.transform, theme);
        self.region.paint_bars(ctx.scene, ctx.transform, theme);
        self.hovered_drawing = self
            .pointer
            .filter(|point| r.contains(*point))
            .and_then(|point| self.queued.iter().find(|slot| slot.rect.contains(point)))
            .map(|slot| slot.rect.inflate(2.0, 2.0));
    }

    fn queued_at(&self, point: Point) -> Option<(&QueuedSlot, bool)> {
        if !self.rect.contains(point) {
            return None;
        }
        self.queued
            .iter()
            .find(|slot| slot.band.y0 <= point.y && point.y < slot.band.y1)
            .map(|slot| (slot, slot.cancel.contains(point)))
    }

    fn handle_outbox_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let point = ctx.logical(position);
                let before = self
                    .queued_at(self.pointer.unwrap_or(Point::ZERO))
                    .map(|(slot, cancel)| (slot.id.clone(), cancel));
                self.pointer = Some(point);
                if self
                    .queued_at(point)
                    .map(|(slot, cancel)| (slot.id.clone(), cancel))
                    != before
                {
                    self.dirty = true;
                }
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
                let Some(point) = self.pointer else {
                    return;
                };
                let Some((slot, cancel)) = self.queued_at(point) else {
                    return;
                };
                let id = slot.id.clone();
                if cancel {
                    self.shared.borrow_mut().cancel_send(&mut ctx.state, &id);
                } else {
                    self.shared.borrow_mut().select_queued(&id);
                }
                *ctx.handled = true;
                self.dirty = true;
            }
            _ => {}
        }
    }

    /// Paint the rows that stand in `list`, under whatever the header and a
    /// failed check left there, and remember where each one and its buttons
    /// are.
    fn paint_rows(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        state: &MailState,
        rows: &[(MessageId, usize)],
        list: Rect,
        t: Affine,
        theme: &Theme,
    ) {
        self.row_h = ListRowFace::new("Ag")
            .subtitle("Ag")
            .secondary("Ag")
            .height(ctx.text, theme);
        let unscrolled = RowFrame::new(list, self.row_h, false, theme);
        self.region
            .set_content_size(list.width(), unscrolled.content_height(rows.len(), theme));
        // Rows stop short of the scroll bar when the list overflows.
        let frame = RowFrame::new(list, self.row_h, self.region.overflows_vertically(), theme);
        self.reveal(rows, state.selected, frame);
        let scroll = self.region.offset().y;

        let selected_rows = selected_rows(state, rows);
        ctx.scene
            .push_layer(Fill::NonZero, BlendMode::default(), 1.0, t, &list);
        let mut slots = Vec::with_capacity(rows.len());
        for (index, (id, len)) in rows.iter().enumerate() {
            let band = frame.band(index, scroll);
            let rect = frame.row(index, scroll);
            let starred = state.mailbox.get(*id).is_some_and(|m| m.starred);
            let shown = band.y1 >= list.y0 && band.y0 <= list.y1;
            let slot = Slot {
                id: *id,
                band,
                rect,
                starred,
                messages: *len,
                // Only a row on screen can be pointed at; one scrolled away
                // costs no layout of its buttons.
                actions: if shown {
                    action_rects(&self.labels, starred, rect, theme)
                } else {
                    Vec::new()
                },
            };
            if shown {
                self.paint_row(
                    ctx,
                    state,
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
        // Near the last row, the older mail is fetched before it is reached.
        if let Some(last) = slots.last()
            && last.band.y0 - list.y1 < self.row_h * f64::from(OLDER_AHEAD_ROWS)
        {
            state.wants_older.set(true);
        }
        self.region.note_shown(t, theme);
        self.region.paint_bars(ctx.scene, t, theme);
        self.hovered_drawing = self
            .pointer
            .filter(|point| list.contains(*point))
            .and_then(|point| slots.iter().find(|slot| slot.rect.contains(point)))
            .map(|slot| slot.band.union(slot.rect).inflate(2.0, 2.0));
        self.slots = slots;
    }

    fn paint_row(&self, ctx: &mut PaintCtx<'_>, state: &MailState, slot: &Slot, row: RowState) {
        let hovered = self
            .pointer
            .filter(|p| self.rect.contains(*p) && slot.rect.contains(*p) && !state.bulk_selecting);
        let Some(text) = RowText::of(slot.id, state) else {
            return;
        };
        let actions = row_actions(&self.labels, slot.starred);
        let action_faces: Vec<RowAction<'_>> = actions.iter().map(|(_, action)| *action).collect();
        let image = state.avatars.image(text.email);
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
            face = face.swiped(swipe_archive(&self.labels), press.offset);
        }
        face.paint(ctx, slot.rect);
        if text.unread {
            paint_unread_dot(ctx, &face, slot.rect);
        }
    }
}

fn selected_rows(state: &MailState, rows: &[(MessageId, usize)]) -> Vec<bool> {
    rows.iter()
        .map(|(id, _)| {
            if state.bulk_selecting {
                state.bulk_selection.contains(id)
            } else {
                state.selected == Some(*id)
            }
        })
        .collect()
}

fn paint_arriving_rows(
    ctx: &mut PaintCtx<'_>,
    list: Rect,
    clock: &ui::arriving::ArrivalClock,
    now: ui::time::Instant,
) -> ui::arriving::Step {
    let theme = ctx.theme;
    let kind = ListRowFace::new("Ag").subtitle("Ag").secondary("Ag");
    let row_h = kind.height(ctx.text, theme);
    let frame = RowFrame::new(list, row_h, false, theme);
    let face =
        ui::widget::ArrivalFace::new(ui::widget::SkeletonShape::Row(kind.shape()), clock, now);
    let mut named = ui::widget::NamedOnce::default();
    let mut index = 0;
    loop {
        let row = frame.row(index, 0.0);
        if row.y0 >= list.y1 {
            break;
        }
        face.clone().named_once(&mut named).paint(ctx, row);
        index += 1;
    }
    face.step()
}

fn paint_unread_dot(ctx: &mut PaintCtx<'_>, face: &ListRowFace<'_>, row: Rect) {
    let leading = face.leading_rect(row, ctx.text, ctx.theme);
    let gutter = (leading.x0 - row.x0).max(0.0);
    if gutter <= 0.0 {
        return;
    }
    let mark = Rect::new(row.x0, leading.y0, leading.x0, leading.y1);
    ui::widget::BadgeFace::dot(mark).paint(ctx.scene, ctx.transform, ctx.text, ctx.theme);
}

/// The empty list's face: why there is nothing here, and what to do next.
fn paint_zero_state(ctx: &mut PaintCtx<'_>, r: Rect, state: &MailState) -> Option<(Rect, Intent)> {
    let zero = zero_state(!state.mailbox.messages.is_empty(), state.folder);
    EmptyState::paint(
        ctx,
        r,
        &EmptyStateContent {
            icon: Some(zero.icon),
            title: &zero.title,
            action_label: zero.action.as_deref(),
            ..EmptyStateContent::new("")
        },
        Point::new(-1.0, -1.0),
    )
    .map(|pill| (pill, Intent::AccountSettings))
}

/// The failed check, as painted above the rows: what cannot be reached, what
/// it means, and the way out. Returns where the way out is and how tall the
/// words are, so the rows start under them.
fn paint_sync_banner(
    ctx: &mut PaintCtx<'_>,
    r: Rect,
    text: &crate::notice::SyncFailureText,
    pointer: Option<Point>,
) -> (Option<(Rect, Intent)>, f64) {
    let content = BannerContent {
        message: text.message.as_deref().map(std::borrow::Cow::Borrowed),
        action: text.action.as_ref().map(|offer| offer.label.as_str()),
        ..BannerContent::new(text.kind, &text.title)
    };
    let height = Banner::height(&content, r.width(), ctx.text, ctx.theme);
    let rect = Rect::new(r.x0, r.y0, r.x1, r.y0 + height);
    let hit = Banner::paint(
        ctx,
        rect,
        &content,
        pointer.unwrap_or(Point::new(-1.0, -1.0)),
    );
    let offer = text.action.as_ref().map(|offer| offer.intent.clone());
    (hit.action.zip(offer), height)
}

/// The empty list of an account whose check failed: what cannot be reached,
/// and the one control that helps. Returns it, so a press reaches the same
/// place the window's own control does.
fn paint_failure_state(
    ctx: &mut PaintCtx<'_>,
    r: Rect,
    text: &crate::notice::SyncFailureText,
) -> Option<(Rect, Intent)> {
    EmptyState::paint(
        ctx,
        r,
        &EmptyStateContent {
            icon: Some(ui::icons::Icon::WifiOff),
            title: &text.title,
            message: text.message.as_deref(),
            action_label: text.action.as_ref().map(|offer| offer.label.as_str()),
            error: text.kind == ui::widget::BannerKind::Error,
            ..EmptyStateContent::new("")
        },
        Point::new(-1.0, -1.0),
    )
    .zip(text.action.as_ref().map(|offer| offer.intent.clone()))
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
        self.theme = theme.clone();
        self.zero_action = None;
        self.sync_action = None;
        self.labels = RowLabels::of();
        let mut ctx = PaintCtx::new(scene, t, tr, theme);
        if state.outbox_open {
            // The outbox is a place of its own: what waits to leave, and why
            // what has not left has not. It is not a folder of the mailbox,
            // so none of a folder's rows or gestures are here.
            self.region.set_viewport(r, theme);
            self.slots.clear();
            self.paint_outbox(&mut ctx, &state);
            return;
        }
        self.queued.clear();
        let rows = state.list_rows();
        // A check that cannot run is the list's state, not a notice that
        // fades: its words stand above the rows, or in the place the rows
        // would be while there are none.
        let failure = state.sync_error.as_ref().map(crate::notice::sync_failure);
        let mut list = r;
        if let (Some(text), false) = (&failure, rows.is_empty()) {
            let (action, height) = paint_sync_banner(&mut ctx, r, text, self.pointer);
            self.sync_action = action;
            list.y0 = (r.y0 + height).min(r.y1);
        }
        self.region.set_viewport(list, theme);
        if rows.is_empty()
            && let Some(clock) = &state.first_fetch
        {
            self.slots.clear();
            self.region.set_content_size(list.width(), 0.0);
            let now = ui::motion::frame_now();
            self.arriving_step = Some(paint_arriving_rows(&mut ctx, list, clock, now));
            return;
        }
        self.arriving_step = None;
        if rows.is_empty() {
            self.slots.clear();
            self.region.set_content_size(list.width(), 0.0);
            self.zero_action = match &failure {
                Some(text) => paint_failure_state(&mut ctx, list, text),
                None => paint_zero_state(&mut ctx, list, &state),
            };
            return;
        }

        self.paint_rows(&mut ctx, &state, &rows, list, t, theme);
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
        if self.shared.borrow().outbox_open {
            self.handle_outbox_event(event, ctx);
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let point = ctx.logical(position);
                let before = self.hover_at(self.pointer);
                self.pointer = Some(point);
                if let Some(press) = self.press.as_mut() {
                    press.offset = (press.at.x - point.x).max(0.0);
                    self.dirty = true;
                }
                // Moving within one row (or over no row) changes nothing on
                // screen: only a new row or row button under the pointer
                // repaints.
                if self.hover_at(self.pointer) != before {
                    self.dirty = true;
                }
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
                let held = ctx.modifiers.state();
                let picking = held.control_key() || held.super_key() || held.shift_key();
                if let Some(point) = self.pointer
                    && let Some(intent) = self.route_at(point)
                {
                    self.shared.borrow_mut().push_intent(intent);
                    *ctx.handled = true;
                    self.dirty = true;
                } else if let Some(point) = self.pointer
                    && picking
                    && self.pick(point, held.shift_key())
                {
                    *ctx.handled = true;
                    self.dirty = true;
                } else if let Some(point) = self.pointer
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

    fn tooltip(&self) -> Option<std::borrow::Cow<'_, str>> {
        let point = self.pointer?;
        let slot = self.slot_at(point)?;
        self.action_at(slot, point).map(|action| {
            // The action's own name and the key that does the same.
            let (label, key) = match action {
                CtxAction::ToggleStar if slot.starred => (&self.labels.unstar, "S"),
                CtxAction::ToggleStar => (&self.labels.star, "S"),
                CtxAction::Archive => (&self.labels.archive, "E"),
                _ => (&self.labels.delete, "Delete"),
            };
            format!("{label} ({key})").into()
        })
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
            || self.scrolled
            || self.shared.borrow().dirty
            || self.arriving_step_moved()
            || ui::widget::Widget::needs_animation_frame(self)
    }

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

    fn update(&mut self, _state: &ui::widget::AppModel, _path: &ui::widget::ModelPath) -> bool {
        self.scrolled |= self.region.advance();
        false
    }

    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        let arriving = self
            .arriving_step
            .and_then(|_| self.shared.borrow().first_fetch.clone())
            .and_then(|clock| clock.next_step_at(ui::time::now()));
        self.region
            .next_wake_deadline(&self.theme)
            .into_iter()
            .chain(arriving)
            .min()
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
    use ui::widget::DrawChild;

    fn shared() -> Shared {
        MailState::shared(MailState::new_sample(crate::avatars::Avatars::new()))
    }

    fn draw(list: &mut MessageList) {
        let mut scene = Scene::new();
        list.draw_child(
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

        let (second, rect) = rows[1];
        let mut doc = crate::resource::TestMail::doc();
        assert!(list.press(&mut ui::widget::Edits::lend(&mut doc), rect.center()));
        assert_eq!(shared.borrow().selected, Some(second));

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

    /// A check that cannot run stands above the rows and keeps its place
    /// until it works: the rows move down for it, and its pill asks the app to
    /// check again.
    #[test]
    fn a_failed_check_stands_above_the_rows_and_offers_to_check_again() {
        let shared = shared();
        let mut list = MessageList::new(shared.clone());
        list.rect = Rect::new(0.0, 0.0, 340.0, 760.0);
        draw(&mut list);
        let rows_before = list.row_rects();
        assert!(list.sync_action.is_none(), "a working account says nothing");

        shared
            .borrow_mut()
            .note_sync_failure("Anna".into(), crate::state::SyncCause::Unreachable);
        draw(&mut list);
        let rows_after = list.row_rects();
        assert_eq!(rows_before.len(), rows_after.len(), "no row is lost");
        assert!(
            rows_after[0].1.y0 > rows_before[0].1.y0,
            "the words stand above the rows: {:?} then {:?}",
            rows_before[0].1,
            rows_after[0].1
        );
        let (_, intent) = list.sync_action.expect("the failure offers a way out");
        assert!(matches!(intent, Intent::CheckMail));
    }

    /// An empty mailbox whose check failed says that, not that nothing has
    /// arrived, and its pill is the failure's own way out.
    #[test]
    fn an_empty_mailbox_whose_check_failed_says_so_instead_of_nothing_yet() {
        let shared = shared();
        let mut list = MessageList::new(shared.clone());
        list.rect = Rect::new(0.0, 0.0, 340.0, 760.0);
        {
            let mut state = shared.borrow_mut();
            let ids: Vec<MessageId> = state.mailbox.messages.keys().copied().collect();
            for id in ids {
                state.mailbox.purge(id);
            }
            state.note_sync_failure("Anna".into(), crate::state::SyncCause::SignedOut);
        }
        draw(&mut list);
        let (_, intent) = list
            .zero_action
            .expect("a failed check offers its own way out");
        assert!(
            matches!(intent, Intent::AccountSettings),
            "a sign-in failure leads to the account that has to change"
        );
        assert!(
            list.sync_action.is_none(),
            "the words are in the empty list, not twice"
        );
    }

    #[test]
    fn every_empty_list_says_one_thing() {
        let nothing_yet = zero_state(false, Folder::Inbox);
        assert_eq!(nothing_yet.title, crate::i18n::t("mail-zero-nothing-yet"));
        assert!(
            nothing_yet.action.is_some(),
            "a mailbox no mail has arrived in offers its account route"
        );
        for folder in Folder::ALL {
            let empty = zero_state(true, folder);
            assert!(
                empty.action.is_none(),
                "{folder:?} is a folder of a working mailbox, not an account to fix"
            );
            assert!(empty.title.chars().next().is_some_and(char::is_uppercase));
        }
    }
}
