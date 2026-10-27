//! The menus Chat opens: a right-click on a channel, a category, a message or
//! someone in the call, and the rows each offers — named by what they do to
//! that target, and asking first where they remove something.

use ui::icons::Icon;
use ui::widget::{ContextMenu as SharedContextMenu, MenuItem};
use vello::kurbo::{Point, Rect};

use super::{ChatApp, Id};

/// A right-click context menu over a channel or category row, or the
/// inline rename editor it can open.
pub(crate) struct ContextMenu {
    pub target: MenuTarget,
    pub items: Vec<MenuAction>,
    /// Toolkit-owned paint, clamping, pointer/keyboard routing, focus trap,
    /// and accessibility. Chat only maps the selected row to its domain
    /// action.
    pub widget: SharedContextMenu,
}

impl ContextMenu {
    /// A menu whose rows are named by `label`, so a row can say what it will
    /// do to this target ("Collapse category", "Unpin message"), and whose
    /// rows `checked` says name a state, checked while it holds.
    pub(crate) fn labelled(
        target: MenuTarget,
        anchor: Point,
        items: Vec<MenuAction>,
        viewport: Rect,
        label: impl Fn(MenuAction) -> String,
        checked: impl Fn(MenuAction) -> Option<bool>,
    ) -> Self {
        let mut widget = SharedContextMenu::new();
        for action in &items {
            let name = label(*action);
            let item = if action.asks_first() {
                MenuItem::destructive_selection(&name).asking_first()
            } else if action.is_destructive() {
                MenuItem::destructive_selection(&name)
            } else {
                MenuItem::selection(&name)
            };
            let icon = match action {
                MenuAction::Rename | MenuAction::EditMessage => Icon::Pencil,
                MenuAction::EditChannel => Icon::Settings,
                MenuAction::Delete | MenuAction::DeleteMessage => Icon::Trash,
                MenuAction::BanAuthor => Icon::Ban,
                MenuAction::StopSharing => Icon::EyeOff,
                MenuAction::MoveUp => Icon::ChevronUp,
                MenuAction::MoveDown | MenuAction::ToggleCollapse => Icon::ChevronDown,
                MenuAction::NewChannelHere
                | MenuAction::NewVoiceChannelHere
                | MenuAction::NewCategory => Icon::Plus,
                MenuAction::Archive => Icon::Archive,
                MenuAction::PinMessage => Icon::Pin,
                MenuAction::CopyText => Icon::Copy,
                MenuAction::AddReaction => Icon::SmilePlus,
                MenuAction::ReplyInThread => Icon::Reply,
                MenuAction::ReportMessage | MenuAction::ReportAuthor => Icon::Flag,
                MenuAction::Silence => Icon::VolumeX,
                MenuAction::Volume(_) => Icon::Volume2,
            };
            let item = match checked(*action) {
                Some(on) => item.with_checked(on),
                None => item,
            };
            widget.push(item.with_icon(icon));
        }
        if viewport.width() > 0.0 && viewport.height() > 0.0 {
            widget.open_at_clamped([anchor.x as f32, anchor.y as f32], viewport);
        } else {
            widget.open_at([anchor.x as f32, anchor.y as f32]);
        }
        Self {
            target,
            items,
            widget,
        }
    }
}

/// What a context menu / create flow acts on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum MenuTarget {
    Channel(Id),
    Category(Id),
    /// One of your own messages — the action menu offers Edit.
    Message(Id),
    /// The room itself: what can be made in it.
    Room,
    /// Someone else in the call, by person or guest id: how loud they play
    /// on this device.
    CallParticipant(String),
}

/// The structural actions reachable from a context menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MenuAction {
    Rename,
    /// Change a channel's name, category and topic in the channel sheet.
    EditChannel,
    Delete,
    MoveUp,
    MoveDown,
    NewChannelHere,
    NewVoiceChannelHere,
    NewCategory,
    ToggleCollapse,
    /// Archive / unarchive a channel — hides it from the sidebar (reversible
    /// from Ctrl+K): an edit of the channel's `archived` field.
    Archive,
    /// React to a message with an emoji: opens the picker for it. The
    /// reaction row's "+" exists only once a message has reactions, so this
    /// is how anyone adds the first one.
    AddReaction,
    /// Open a message's thread to reply in it, also when it has no replies
    /// yet (the thread bar exists only once one does).
    ReplyInThread,
    /// Load one of your own messages into the inline editor.
    EditMessage,
    /// Pin / unpin one of your own messages (shared truth every peer sees).
    PinMessage,
    /// Delete one of your own messages, or as a moderator someone else's.
    DeleteMessage,
    /// Put a message's text on the clipboard.
    CopyText,
    /// Report someone else's message to the room's moderators.
    ReportMessage,
    /// Report a message's author to the room's moderators.
    ReportAuthor,
    /// As a moderator, ban a message's author from the room.
    BanAuthor,
    /// Stop sharing the window a message of yours shares.
    StopSharing,
    /// Stop hearing someone in the call, on this device only, or hear them
    /// again.
    Silence,
    /// Play someone in the call at this percent of as they arrive, on this
    /// device only.
    Volume(u16),
}

impl MenuAction {
    pub fn label(self) -> String {
        crate::i18n::t(match self {
            MenuAction::Rename => "chat-menu-rename",
            MenuAction::EditChannel => "chat-menu-edit-channel",
            MenuAction::Delete => "chat-menu-delete",
            MenuAction::MoveUp => "chat-menu-move-up",
            MenuAction::MoveDown => "chat-menu-move-down",
            MenuAction::NewChannelHere => "chat-menu-new-channel",
            MenuAction::NewVoiceChannelHere => "chat-menu-new-voice-channel",
            MenuAction::NewCategory => "chat-menu-new-category",
            MenuAction::ToggleCollapse => "chat-menu-collapse-category",
            MenuAction::Archive => "chat-menu-archive-channel",
            MenuAction::AddReaction => "chat-emoji-add-reaction",
            MenuAction::ReplyInThread => "chat-menu-reply-in-thread",
            MenuAction::EditMessage => "chat-menu-edit-message",
            MenuAction::PinMessage => "chat-menu-pin-message",
            MenuAction::DeleteMessage => "chat-menu-delete-message",
            MenuAction::CopyText => "chat-menu-copy-text",
            MenuAction::ReportMessage => "chat-menu-report-message",
            MenuAction::ReportAuthor => "chat-menu-report-sender",
            MenuAction::BanAuthor => "chat-menu-ban",
            MenuAction::StopSharing => "chat-share-stop",
            MenuAction::Silence => "chat-call-silence",
            MenuAction::Volume(_) => "chat-menu-volume",
        })
    }
    pub fn is_destructive(self) -> bool {
        matches!(
            self,
            MenuAction::Delete | MenuAction::DeleteMessage | MenuAction::BanAuthor
        )
    }
    /// Whether choosing it from a menu asks first: a removal, which Shift
    /// skips for what Undo takes back
    /// ([`ui::framework::Dispatcher::submit_confirming`]).
    pub fn asks_first(self) -> bool {
        matches!(self, MenuAction::Delete | MenuAction::DeleteMessage)
    }
    /// The stable key a message bar's button is published under.
    pub fn bar_key(self) -> &'static str {
        match self {
            MenuAction::AddReaction => "react",
            MenuAction::ReplyInThread => "reply",
            MenuAction::EditMessage => "edit",
            MenuAction::PinMessage => "pin",
            MenuAction::CopyText => "copy",
            MenuAction::DeleteMessage => "delete",
            _ => "other",
        }
    }
    /// Menu items for a channel row, in order: the destructive one last.
    pub fn for_channel() -> [MenuAction; 6] {
        [
            MenuAction::EditChannel,
            MenuAction::Rename,
            MenuAction::Archive,
            MenuAction::MoveUp,
            MenuAction::MoveDown,
            MenuAction::Delete,
        ]
    }
    /// Menu items for the room: the window bar's Create, and a right click
    /// in the conversation.
    pub fn for_room() -> [MenuAction; 2] {
        [MenuAction::NewChannelHere, MenuAction::NewCategory]
    }
    /// Menu items for a category row.
    pub fn for_category() -> [MenuAction; 5] {
        [
            MenuAction::NewChannelHere,
            MenuAction::NewVoiceChannelHere,
            MenuAction::Rename,
            MenuAction::ToggleCollapse,
            MenuAction::Delete,
        ]
    }
    /// Menu items for one of your own messages.
    pub fn for_message() -> [MenuAction; 6] {
        [
            MenuAction::AddReaction,
            MenuAction::ReplyInThread,
            MenuAction::EditMessage,
            MenuAction::PinMessage,
            MenuAction::CopyText,
            MenuAction::DeleteMessage,
        ]
    }
    /// Menu items for someone else in the call: how loud they play here,
    /// silenced or at one of a few levels, on this device only.
    pub fn for_call_participant() -> [MenuAction; 5] {
        [
            MenuAction::Silence,
            MenuAction::Volume(50),
            MenuAction::Volume(100),
            MenuAction::Volume(150),
            MenuAction::Volume(200),
        ]
    }
    /// Menu items for someone else's message: reporting it or its sender,
    /// and removing it, which only the room's moderators may.
    pub fn for_others_message() -> [MenuAction; 6] {
        [
            MenuAction::AddReaction,
            MenuAction::ReplyInThread,
            MenuAction::CopyText,
            MenuAction::ReportMessage,
            MenuAction::ReportAuthor,
            MenuAction::DeleteMessage,
        ]
    }
}

impl ChatApp {
    /// A context menu over `target` whose rows say what they do to it.
    pub(crate) fn context_menu(
        &self,
        target: MenuTarget,
        anchor: Point,
        items: Vec<MenuAction>,
    ) -> ContextMenu {
        let collapsed = match &target {
            MenuTarget::Category(id) => self
                .categories
                .iter()
                .any(|category| category.id == *id && category.collapsed),
            _ => false,
        };
        let pinned = match &target {
            MenuTarget::Message(id) => self.message(id).is_some_and(|m| m.pinned),
            _ => false,
        };
        let channel = matches!(target, MenuTarget::Channel(_));
        let listening = match &target {
            MenuTarget::CallParticipant(id) => Some(self.call_listening(id)),
            _ => None,
        };
        let checked = move |action: MenuAction| {
            let listening = listening?;
            match action {
                MenuAction::Silence => Some(listening.silenced),
                MenuAction::Volume(percent) => {
                    Some(!listening.silenced && listening.percent == percent)
                }
                _ => None,
            }
        };
        let label = move |action: MenuAction| match action {
            MenuAction::Silence => crate::i18n::t("chat-call-silence"),
            MenuAction::Volume(percent) => {
                crate::i18n::t_args("chat-call-volume", &[("percent", percent.into())])
            }
            MenuAction::ToggleCollapse if collapsed => crate::i18n::t("chat-menu-expand-category"),
            MenuAction::PinMessage if pinned => crate::i18n::t("chat-menu-unpin-message"),
            MenuAction::Delete if channel => crate::i18n::t("chat-menu-delete-channel"),
            MenuAction::Delete => crate::i18n::t("chat-menu-delete-category"),
            MenuAction::Rename if channel => crate::i18n::t("chat-menu-rename-channel"),
            MenuAction::Rename => crate::i18n::t("chat-menu-rename-category"),
            other => other.label(),
        };
        ContextMenu::labelled(target, anchor, items, self.menu_bounds(), label, checked)
    }

    /// Where a menu may open: the conversation, and the sidebar beside it
    /// whose rows a menu also opens from.
    pub(super) fn menu_bounds(&self) -> Rect {
        self.places
            .list_rect()
            .filter(|list| list.width() > 0.0 && self.sidebar_open())
            .map_or(self.rect, |list| list.union(self.rect))
    }
}
