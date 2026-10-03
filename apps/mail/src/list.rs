//! The message list: one [`ListRowFace`] per conversation, newest first.
//!
//! Rows are the toolkit's; this widget lays them out under the header row,
//! scrolls them, and turns a click, a hover action, a swipe or a right click
//! into the matching mail action.

use std::collections::HashMap;

use automerge::AutoCommit;
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{BlendMode, Fill};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};

use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{
    AvatarFace, EmptyState, EmptyStateContent, Interaction, ListRowFace, RowAction, RowLeading,
    ScrollMetrics, Scrollbar, SwipeAction, TaffyTreeExt, Widget,
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
    rect: Rect,
    starred: bool,
    actions: Vec<Rect>,
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
    preview: String,
    when: String,
    unread: bool,
}

impl<'a> RowText<'a> {
    /// The row of message `id`; its subject is kept to the first of the
    /// two secondary lines (`subject_w` wide) and the start of the message
    /// fills the second.
    fn of(
        id: MessageId,
        state: &'a MailState,
        text: &mut TextRenderer,
        theme: &Theme,
        (subject_w, hovered): (f64, bool),
    ) -> Option<Self> {
        let message = state.mailbox.get(id)?;
        let when = format_relative(
            local_minutes(message.timestamp_min),
            local_minutes(state.now_min),
        );
        // The row narrows its text for the time, or under the pointer for
        // its actions: keep the subject on its own line beside either.
        let spacing = &theme.tokens.spacing;
        let metrics = &theme.tokens.metrics;
        let trailing = if hovered {
            3.0 * f64::from(metrics.bar_control_height) + 2.0 * f64::from(spacing.s1)
        } else {
            f64::from(
                text.measure_style(&when, None, &theme.text(ui::TextRole::Caption).numeric())
                    .0,
            )
        };
        let subject_w = subject_w - trailing - f64::from(spacing.s3);
        let subject = message.thread_subject();
        let subject = if subject.trim().is_empty() {
            "No subject".to_owned()
        } else {
            subject
        };
        let subject = text.elide_to_width_style(
            &subject,
            ui::num::f32_of_f64(subject_w.max(0.0)),
            &theme.text(ui::TextRole::Caption),
        );
        Some(Self {
            sender: message.from.display(),
            email: &message.from.email,
            preview: format!("{subject}\n{}", message.snippet()),
            when,
            unread: !message.read,
        })
    }
}

pub struct MessageList {
    shared: Shared,
    rect: Rect,
    scroll: f64,
    content_h: f64,
    row_h: f64,
    slots: Vec<Slot>,
    pointer: Option<Point>,
    press: Option<Press>,
    scrollbar: Scrollbar,
    /// The selection the list last scrolled into view.
    revealed: Option<MessageId>,
    dirty: bool,
}

impl MessageList {
    pub fn new(shared: Shared) -> Self {
        Self {
            shared,
            rect: Rect::ZERO,
            scroll: 0.0,
            content_h: 0.0,
            row_h: 0.0,
            slots: Vec::new(),
            pointer: None,
            press: None,
            scrollbar: Scrollbar::vertical(),
            revealed: None,
            dirty: true,
        }
    }

    fn slot_at(&self, point: Point) -> Option<&Slot> {
        if !self.rect.contains(point) {
            return None;
        }
        self.slots.iter().find(|slot| slot.rect.contains(point))
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

    fn clamp_scroll(&mut self) {
        let max = (self.content_h - self.rect.height()).max(0.0);
        self.scroll = self.scroll.clamp(0.0, max);
    }

    /// Keep the selected conversation in view when the selection moves
    /// (arrow keys, a search hit, an agent opening a message).
    fn reveal(&mut self, rows: &[(MessageId, usize)], selected: Option<MessageId>) {
        if selected == self.revealed || self.row_h <= 0.0 {
            return;
        }
        self.revealed = selected;
        let Some(index) = selected.and_then(|id| rows.iter().position(|(row, _)| *row == id))
        else {
            return;
        };
        let top = index as f64 * self.row_h;
        let bottom = top + self.row_h;
        if top < self.scroll {
            self.scroll = top;
        } else if bottom > self.scroll + self.rect.height() {
            self.scroll = bottom - self.rect.height();
        }
    }

    fn press(&mut self, doc: &mut AutoCommit, point: Point) -> bool {
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
            state.select_message(doc, slot.id);
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
        .secondary("")
        .two_line_secondary()
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
    /// Whether the row below is selected; `None` for the last row.
    next_selected: Option<bool>,
    /// The width the subject is kept to.
    subject_w: f64,
}

impl MessageList {
    /// Paint one conversation's row.
    fn paint_row(&self, ctx: &mut PaintCtx<'_>, state: &MailState, slot: &Slot, row: RowState) {
        let theme = ctx.theme;
        let hovered = self
            .pointer
            .filter(|p| self.rect.contains(*p) && slot.rect.contains(*p) && !state.bulk_selecting);
        let Some(text) = RowText::of(
            slot.id,
            state,
            ctx.text,
            theme,
            (row.subject_w, hovered.is_some()),
        ) else {
            return;
        };
        let actions = row_actions(slot.starred);
        let action_faces: Vec<RowAction<'_>> = actions.iter().map(|(_, action)| *action).collect();
        let image = state.images.get(&gravatar_url(text.email));
        let avatar = AvatarFace::new(&text.sender, text.email).image(image.as_ref());
        let mut face = ListRowFace::new(&text.sender)
            .leading(RowLeading::Avatar(avatar))
            .secondary(&text.preview)
            .two_line_secondary()
            .meta(&text.when)
            .emphasized(text.unread)
            .selected(row.selected)
            .actions(&action_faces)
            .interaction(Interaction {
                hovered: hovered.is_some(),
                ..Interaction::default()
            });
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
        let node = taffy.leaf_node(Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::percent(1.0),
            },
            ..Default::default()
        });
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let l = taffy.layout(node).copied().unwrap_or_default();
        self.rect = Rect::new(
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
            origin.x + f64::from(l.location.x) + f64::from(l.size.width),
            origin.y + f64::from(l.location.y) + f64::from(l.size.height),
        );
        self.clamp_scroll();
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
        let mut ctx = PaintCtx::new(scene, t, tr, theme);
        if rows.is_empty() {
            self.slots.clear();
            self.content_h = 0.0;
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
            .secondary("Ag")
            .two_line_secondary()
            .height(ctx.text, theme);
        self.content_h = self.row_h * rows.len() as f64;
        self.clamp_scroll();
        self.reveal(&rows, state.selected);
        // Rows stop short of the scroll bar when the list overflows.
        let metrics = ScrollMetrics::new(self.content_h, r.height(), self.scroll);
        let gutter = f64::from(theme.tokens.metrics.scrollbar_gutter);
        let rows_x1 = if metrics.overflows() {
            r.x1 - gutter
        } else {
            r.x1
        };
        // The row's text column: its insets, the avatar and the gap after it.
        let spacing = &theme.tokens.spacing;
        let subject_w = rows_x1
            - r.x0
            - 3.0 * f64::from(spacing.s3)
            - f64::from(theme.tokens.metrics.min_pointer_target);

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
        for (index, (id, _len)) in rows.iter().enumerate() {
            let y0 = r.y0 + index as f64 * self.row_h - self.scroll;
            let rect = Rect::new(r.x0, y0, rows_x1, y0 + self.row_h);
            let starred = state.mailbox.get(*id).is_some_and(|m| m.starred);
            let slot = Slot {
                id: *id,
                rect,
                starred,
                actions: action_rects(starred, rect, theme),
            };
            if rect.y1 >= r.y0 && rect.y0 <= r.y1 {
                self.paint_row(
                    &mut ctx,
                    &state,
                    &slot,
                    RowState {
                        selected: selected_rows.get(index).copied().unwrap_or(false),
                        next_selected: selected_rows.get(index + 1).copied(),
                        subject_w,
                    },
                );
            }
            slots.push(slot);
        }
        ctx.scene.pop_layer();
        if metrics.overflows() {
            let track = Rect::new(r.x1 - gutter, r.y0, r.x1, r.y1);
            self.scrollbar
                .draw(ctx.scene, t, track, metrics, theme, false);
        }
        self.slots = slots;
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        if *ctx.handled {
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let point =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
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
                    && self.press(ctx.state, point)
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
                            ctx.state,
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
            WindowEvent::MouseWheel { delta, .. }
                if self.pointer.is_some_and(|p| self.rect.contains(p)) =>
            {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => f64::from(*y) * self.row_h,
                    MouseScrollDelta::PixelDelta(p) => p.y / ctx.scale_factor,
                };
                self.scroll -= dy;
                self.clamp_scroll();
                self.dirty = true;
                *ctx.handled = true;
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
        self.dirty || self.shared.borrow().dirty
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
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
        let mut doc = automerge::AutoCommit::new();
        assert!(list.press(&mut doc, rect.center()));
        assert_eq!(shared.borrow().selected, Some(second));

        // The archive action under the pointer archives that row only.
        let (first, rect) = rows[0];
        list.pointer = Some(rect.center());
        draw(&mut list);
        let slot = list.slot_at(rect.center()).cloned().expect("first row");
        let archive = slot.actions[1];
        assert!(list.press(&mut doc, archive.center()));
        assert_eq!(
            shared.borrow().mailbox.get(first).map(|m| m.folder),
            Some(Folder::Archive)
        );
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
