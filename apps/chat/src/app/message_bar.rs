//! The bar over a message the pointer or the keyboard is on: the few things
//! done most to a message (react, reply, edit) as icon buttons at its top
//! trailing corner, and More for its whole menu. While Shift is held it
//! grows the quick actions (pin, copy text, delete), which go ahead at once:
//! Shift skips the question a removal otherwise asks, for what Undo takes
//! back ([`ui::framework::Dispatcher::submit_confirming`]).

use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::widget::{Button, ButtonFace, ButtonRole, ControlSize};
use vello::kurbo::{Point, Rect};
use vello::peniko::{BlendMode, Fill};

use super::{ChatApp, Hit, Id, MenuAction, MenuTarget};

/// The bar's arrival: quick, and at once under reduced motion.
const ARRIVAL: ui::motion::MotionSpec = ui::motion::MotionSpec {
    enter: ui::motion::Pace::Fast,
    exit: ui::motion::Pace::Instant,
    reduced: ui::motion::Reduced::Lands,
    shortcut: ui::motion::Shortcut::Lands,
};

/// Which message the bar is over and how far it has faded in.
#[derive(Debug)]
pub(crate) struct BarMotion {
    over: Option<Id>,
    /// The message whose row is lifted this frame: the bar's.
    pub(crate) lifted: Option<Id>,
    presence: ui::motion::Presence,
    pub(crate) animating: bool,
    /// Deterministic visual review: the bar at rest.
    settled: bool,
}

impl Default for BarMotion {
    fn default() -> Self {
        Self {
            over: None,
            lifted: None,
            presence: ui::motion::Presence::closed(),
            animating: false,
            settled: false,
        }
    }
}

impl BarMotion {
    /// Follow the bar onto `over`, and say how opaque it is now: a bar that
    /// moves to another message arrives there again.
    fn follow(&mut self, over: Option<&Id>, theme: &ui::theme::Theme) -> f32 {
        if self.over.as_ref() != over {
            self.over = over.cloned();
            let now = ui::time::now();
            self.presence = ui::motion::Presence::closed();
            if over.is_some() {
                self.presence.show(
                    true,
                    ARRIVAL,
                    theme.motion(),
                    ui::tokens::MotionSource::Pointer,
                    now,
                );
            }
        }
        if self.over.is_none() {
            self.animating = false;
            return 0.0;
        }
        if self.settled {
            self.animating = false;
            return 1.0;
        }
        let now = ui::time::now();
        self.animating = self.presence.is_moving(now);
        ui::motion::unit_opacity(self.presence.progress(now))
    }

    /// Show the bar at rest, for a deterministic shot.
    pub(crate) fn settle_for_visual_review(&mut self) {
        self.settled = true;
    }
}

impl ChatApp {
    /// Whether message `id`'s row lifts, as a list's hovered row does: its
    /// bar shows over it, or its menu is open.
    pub(crate) fn row_lifted(&self, id: &str) -> bool {
        self.message_bar.lifted.as_deref() == Some(id)
            || self.menu_targets(&super::MenuTarget::Message(id.to_owned()))
    }

    /// The message whose bar shows: the one whose menu is open, else the
    /// one under the pointer, else the one the keyboard is on. None while
    /// something else is up over the conversation, or for a message being
    /// edited.
    pub(crate) fn bar_message(&self) -> Option<Id> {
        if let Some(menu) = &self.menu {
            return match &menu.target {
                MenuTarget::Message(id) => Some(id.clone()),
                _ => None,
            };
        }
        if self.emoji_open || self.channel_sheet.is_some() || !self.can_participate() {
            return None;
        }
        let over = self.hovered_message.clone().or_else(|| {
            let targets = self.semantic_hits();
            let (_, hit, ..) = targets.get(self.keyboard_target?)?;
            message_of(hit).cloned()
        })?;
        (self.editing.as_ref() != Some(&over) && self.message(&over).is_some()).then_some(over)
    }

    /// The message whose row is under `point`, or whose bar is.
    pub(crate) fn message_under(&self, point: Point) -> Option<Id> {
        let on_bar = self.hits.iter().rev().find_map(|(rect, hit)| match hit {
            Hit::MessageBar { msg, .. } | Hit::MessageMore { msg } if rect.contains(point) => {
                Some(msg.clone())
            }
            _ => None,
        });
        on_bar.or_else(|| {
            self.message_rows
                .iter()
                .find(|(rect, _)| rect.contains(point))
                .map(|(_, id)| id.clone())
        })
    }

    /// What the bar over `msg` offers, in order: with Shift held, the
    /// quick actions too.
    pub(crate) fn bar_actions(&self, msg: &Id) -> Vec<MenuAction> {
        let Some(message) = self.message(msg) else {
            return Vec::new();
        };
        let mine = self.is_me(&message.author);
        let mut actions = vec![MenuAction::AddReaction, MenuAction::ReplyInThread];
        if mine {
            actions.push(MenuAction::EditMessage);
        }
        if ui::widget::shift_held() {
            if mine {
                actions.push(MenuAction::PinMessage);
            }
            if !message.text.is_empty() {
                actions.push(MenuAction::CopyText);
            }
            if mine || self.perms.can_moderate() {
                actions.push(MenuAction::DeleteMessage);
            }
        }
        actions
    }

    /// The name of the bar's `action` button over `msg`: what it does now.
    pub(crate) fn bar_label(&self, msg: &Id, action: MenuAction) -> String {
        let mine = self.message(msg).is_some_and(|m| self.is_me(&m.author));
        match action {
            MenuAction::PinMessage if self.message(msg).is_some_and(|m| m.pinned) => {
                crate::i18n::t("chat-menu-unpin-message")
            }
            // Shown only with Shift held: it goes ahead at once.
            MenuAction::DeleteMessage if mine => crate::i18n::t("chat-message-bar-delete-now"),
            MenuAction::DeleteMessage => crate::i18n::t("chat-message-bar-remove-now"),
            other => other.label(),
        }
    }

    /// Open `msg`'s whole menu at `anchor`, as a right click on it does.
    pub(crate) fn open_message_menu(&mut self, msg: Id, anchor: Point) {
        let Some(message) = self.message(&msg) else {
            return;
        };
        let mine = self.is_me(&message.author);
        let author = message.author.clone();
        let items = if mine {
            let mut items = MenuAction::for_message().to_vec();
            if self.is_my_live_share(message) {
                items.insert(0, MenuAction::StopSharing);
            }
            items
        } else {
            let mut items = MenuAction::for_others_message().to_vec();
            if !self.perms.can_moderate() {
                items.retain(|action| *action != MenuAction::DeleteMessage);
            } else if crate::moderation::bannable(&author) {
                items.push(MenuAction::BanAuthor);
            }
            items
        };
        self.menu = Some(self.context_menu(MenuTarget::Message(msg), anchor, items));
    }

    /// Open `msg`'s whole menu from its bar's More `button`: under it, its
    /// trailing edge on the button's, and above it where there is no room
    /// below, as a menu from a button at the end of a row opens.
    pub(crate) fn open_message_menu_from(&mut self, msg: Id, button: Rect) {
        self.open_message_menu(msg, Point::new(button.x1, button.y1));
        let bounds = self.menu_bounds();
        if let Some(menu) = self.menu.as_mut() {
            menu.widget.set_align(ui::widget::PanelAlign::Trailing);
            menu.widget.reanchor(button, bounds);
        }
    }

    /// Paint the bar over the message it shows for, at the top trailing
    /// corner of its row, and record its buttons.
    pub(crate) fn draw_message_bar(
        &mut self,
        paint: PaintCtx<'_>,
        area: Rect,
        hits: &mut Vec<(Rect, Hit)>,
    ) {
        let PaintCtx {
            scene,
            transform: tf,
            text: tr,
            theme,
        } = paint;
        let over = self.bar_message();
        let row = over.as_ref().and_then(|msg| {
            self.message_rows
                .iter()
                .find(|(_, id)| id == msg)
                .map(|(rect, _)| *rect)
        });
        let (Some(msg), Some(row)) = (over, row) else {
            self.message_bar.follow(None, theme);
            return;
        };
        let opacity = self.message_bar.follow(Some(&msg), theme);
        let actions = self.bar_actions(&msg);
        let side = ButtonFace::height(ControlSize::Small, theme);
        let pad = f64::from(theme.tokens.spacing.s1);
        let count = actions.len() + 1;
        let width = side * count as f64 + 2.0 * pad;
        let height = side + 2.0 * pad;
        let right = area.x1 - self.chrome.gutter;
        // Over the row's top edge, as far into the row above as it is into
        // its own, never above the timeline.
        let top = (row.y0 - height / 2.0).max(area.y0 + pad);
        let bar = Rect::new(right - width, top, right, top + height);
        let radius = f64::from(theme.tokens.radii.item) + pad;
        let faded = opacity < 1.0;
        if faded {
            scene.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                opacity,
                tf,
                &bar.inflate(side, side),
            );
        }
        ui::widget::floating_surface::paint_rounded(scene, tf, bar, radius, theme);
        let pinned = self.message(&msg).is_some_and(|m| m.pinned);
        let mut x = bar.x0 + pad;
        let y = bar.y0 + pad;
        for action in actions {
            let rect = Rect::new(x, y, x + side, y + side);
            let name = self.bar_label(&msg, action);
            let icon = match action {
                MenuAction::AddReaction => Icon::SmilePlus,
                MenuAction::ReplyInThread => Icon::Reply,
                MenuAction::EditMessage => Icon::Pencil,
                MenuAction::PinMessage => Icon::Pin,
                MenuAction::CopyText => Icon::Copy,
                MenuAction::DeleteMessage => Icon::Trash,
                _ => Icon::MoreHorizontal,
            };
            let hit = Hit::MessageBar {
                msg: msg.clone(),
                action,
            };
            let mut face = ButtonFace::icon_only(rect, icon, &name)
                .control_size(ControlSize::Small)
                .hovered(self.hovered(rect))
                .focused(self.keyboard_focuses(&hit))
                .toggled(action == MenuAction::PinMessage && pinned);
            if action == MenuAction::DeleteMessage {
                face = face.role(ButtonRole::Destructive);
            }
            Button::paint(scene, tf, tr, theme, &face);
            Self::push_hit(hits, rect, hit);
            x += side;
        }
        let more = Rect::new(x, y, x + side, y + side);
        let more_hit = Hit::MessageMore { msg: msg.clone() };
        let more_label = crate::i18n::t("chat-message-more");
        Button::paint(
            scene,
            tf,
            tr,
            theme,
            &ButtonFace::icon_only(more, Icon::MoreHorizontal, &more_label)
                .control_size(ControlSize::Small)
                .hovered(self.hovered(more))
                .focused(self.keyboard_focuses(&more_hit))
                .toggled(self.menu.is_some()),
        );
        Self::push_hit(hits, more, more_hit);
        if faded {
            scene.pop_layer();
        }
    }

    /// Whether the keyboard is on `hit`.
    fn keyboard_focuses(&self, hit: &Hit) -> bool {
        let Some(index) = self.keyboard_target else {
            return false;
        };
        self.semantic_hits()
            .get(index)
            .is_some_and(|(_, target, ..)| target.semantic_id() == hit.semantic_id())
    }
}

/// The message `hit` belongs to, for one in a message's row.
fn message_of(hit: &Hit) -> Option<&Id> {
    match hit {
        Hit::Reaction { msg, .. }
        | Hit::AddReaction { msg, .. }
        | Hit::MessageBar { msg, .. }
        | Hit::MessageMore { msg }
        | Hit::OpenThread(msg) => Some(msg),
        _ => None,
    }
}
