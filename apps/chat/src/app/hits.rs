//! What a frame painted and where: the vocabulary of clickable areas it
//! records, the parts of each message row it kept, and the inventory those
//! derive — one row per control, with its role, spoken name, value and state —
//! that pointer, keyboard and assistive technology all read.

use vello::kurbo::{Point, Rect};

use crate::model::Id;

use super::{ChatApp, MenuAction};

/// Everything the user can click. Recorded during `draw`, resolved on the
/// next mouse-press.
#[derive(Clone, Debug)]
pub(crate) enum Hit {
    HostModel(String),
    Channel(Id),
    DirectPerson(String),
    AgentWindow,
    AgentDragHandle,
    /// Make a channel in a category.
    AddChannel {
        category: Id,
    },
    /// Make a channel or a category: what can be made in the room.
    AddCategory,
    Reaction {
        msg: Id,
        emoji: String,
    },
    /// The add button under a message; `anchor` is the button, which the
    /// picker opens beside.
    AddReaction {
        msg: Id,
        anchor: Rect,
    },
    OpenThread(Id),
    EditHistory {
        msg: Id,
        anchor: Point,
    },
    /// Right-click target over one of your own messages — opens the message
    /// action menu (Edit). Left-click on it is a no-op.
    MessageMenu {
        msg: Id,
    },
    /// A button of the bar over the message the pointer or the keyboard
    /// is on: what the same row of its menu does.
    MessageBar {
        msg: Id,
        action: MenuAction,
    },
    /// The bar's "More" button: the message's whole menu.
    MessageMore {
        msg: Id,
    },
    /// Commit the inline message editor to the CRDT.
    EditSave,
    /// Discard the inline message editor without writing.
    EditCancel,
    /// Focus/place the caret in the inline message editor.
    FocusEdit,
    EmojiChoice(String),
    /// Show one Unicode emoji group (index into the catalogue) in the picker.
    EmojiGroup(usize),
    /// The picker's "Add emoji" tile: choose a picture for the workspace.
    EmojiAddCustom,
    /// Focus/place the caret in the emoji picker's search field.
    FocusEmojiSearch,
    FocusMain,
    FocusThread,
    Send,
    /// Bring back the message of your own you just deleted.
    UndoDelete,
    /// Return a scrolled conversation to its newest message without changing
    /// any shared document state.
    JumpToLatest,
    ThreadScroll(bool),
    AgentReasoning(String),
    ComposerEmoji(Rect),
    /// Open the workspace file picker to share an attachment.
    Attach,
    /// One rung of an attachment's ladder: open it at full size, save a copy,
    /// or show the copy the person saved.
    Attachment {
        msg: Id,
        action: ui::widget::conversation::AttachmentAction,
    },
    /// Close the open attachment.
    CloseAttachment,
    /// Pick a specific file (by path) from the open attachment picker.
    AttachPick(String),
    /// Choose files on this device with the platform's file chooser.
    AttachDevice,
    StartCall,
    StopSharingLiveContext,
    CopyResourceLink(Id),
    OpenResource(Id),
    /// Open the window a message shares, as a window of this person's own.
    OpenShare(Id),
    /// Stop sharing the window a message of yours shares.
    StopShare(Id),
    /// Let the others do this with the window a message of yours shares.
    SwitchShare(Id, crate::record::ShareAccess),
    /// Open the "Share a window" panel from the attach menu.
    ShareWindowPanel,
    /// What the people a window is shared with may do there.
    ShareAccess(crate::record::ShareAccess),
    /// Choose the window of this tab in the "Share a window" panel.
    SharePick(u64),
    /// Share the window the panel has chosen.
    ShareChosen,
    CloseThread,
    /// Grab the floating call overlay to drag it (records the press point).
    CallDragHandle,
    /// Toggle the participant list expand/collapse in the overlay.
    CallParticipantsToggle,
    CallMute,
    /// Hear nothing of the call (and be muted), or hear it again.
    CallDeafen,
    /// Turn this person's camera on or off in the call.
    CallCamera,
    /// Share this person's screen in the call, or stop.
    CallScreen,
    CallExpand,
    CallLeave,
    /// Someone else in the call, by person or guest id: opens how loud
    /// they play here.
    CallParticipant(String),
    /// The mark of the service a bridge carries a call guest in from,
    /// beside their name: hovering it says where they are.
    CallMark(String),
    /// Toggle the right flap (members / thread).
    ToggleRightFlap,
    /// Show or hide the open channel's pinned messages in the detail drawer.
    TogglePins,
    /// Show a message where it was written (a pinned message's row).
    RevealMessage(Id),
    /// Open the card of a message's author (their avatar and name), or a
    /// member's in the list: who they are, Message and Follow. `place`
    /// tells the targets of one person apart.
    ShowPerson {
        person: String,
        place: String,
    },
    Dismiss, // click on a scrim closes the top-most popover
}

impl Hit {
    pub(crate) fn semantic_id(&self) -> String {
        match self {
            Self::Channel(id) => format!("chat.channel:{id}"),
            Self::DirectPerson(key) => format!("chat.person:{key}"),
            Self::AgentWindow => "chat.agent-window".into(),
            Self::AgentDragHandle => "chat.agent-drag".into(),
            Self::AddChannel { category } => format!("chat.add-channel:{category}"),
            Self::AddCategory => "chat.add-category".into(),
            Self::TogglePins => "chat.pins".into(),
            Self::RevealMessage(id) => format!("chat.reveal-message:{id}"),
            Self::ShowPerson { place, .. } => format!("chat.person-card:{place}"),
            Self::FocusMain => "chat.composer".into(),
            Self::FocusThread => "chat.thread-composer".into(),
            Self::FocusEdit => "chat.message-edit".into(),
            Self::HostModel(command) => format!("chat.model.{command}"),
            Self::Send => "chat.send".into(),
            Self::UndoDelete => "chat.undo-delete".into(),
            Self::JumpToLatest => "chat.jump-to-latest".into(),
            Self::ThreadScroll(end) => format!("chat.thread-scroll-{end}"),
            Self::AgentReasoning(id) => format!("chat.reasoning.{id}"),
            Self::ComposerEmoji(_) => "chat.composer-emoji".into(),
            Self::Attach => "chat.attach".into(),
            Self::Attachment { msg, action } => {
                format!("chat.attachment:{msg}:{}", action.key())
            }
            Self::CloseAttachment => "chat.close-attachment".into(),
            Self::AttachPick(path) => format!("chat.attach-pick:{path}"),
            Self::AttachDevice => "chat.attach-device".into(),
            Self::StartCall => "chat.start-call".into(),
            Self::StopSharingLiveContext => "chat.stop-sharing-live-context".into(),
            Self::CopyResourceLink(id) => format!("chat.copy-resource-link:{id}"),
            Self::OpenResource(id) => format!("chat.open-resource:{id}"),
            Self::OpenShare(id) => format!("chat.open-share:{id}"),
            Self::StopShare(id) => format!("chat.stop-sharing:{id}"),
            Self::SwitchShare(id, access) => format!("chat.switch-share:{id}:{}", access.as_str()),
            Self::ShareWindowPanel => "chat.share-window".into(),
            Self::ShareAccess(access) => format!("chat.share-access:{}", access.as_str()),
            Self::SharePick(tab) => format!("chat.share-pick:{tab}"),
            Self::ShareChosen => "chat.share-chosen".into(),
            Self::ToggleRightFlap => "chat.toggle-details".into(),
            Self::OpenThread(id) => format!("chat.open-thread:{id}"),
            Self::CloseThread => "chat.close-thread".into(),
            Self::Reaction { msg, emoji } => format!("chat.reaction:{msg}:{emoji}"),
            Self::AddReaction { msg, .. } => format!("chat.add-reaction:{msg}"),
            Self::MessageBar { msg, action } => {
                format!("chat.message-bar:{msg}:{}", action.bar_key())
            }
            Self::MessageMore { msg } => format!("chat.message-more:{msg}"),
            Self::EmojiChoice(emoji) => format!("chat.emoji-choice:{emoji}"),
            Self::EmojiGroup(group) => format!("chat.emoji-group:{group}"),
            Self::EmojiAddCustom => "chat.emoji-add".into(),
            Self::FocusEmojiSearch => "chat.emoji-search".into(),
            Self::EditHistory { msg, .. } => format!("chat.edit-history:{msg}"),
            Self::EditSave => "chat.edit-save".into(),
            Self::EditCancel => "chat.edit-cancel".into(),
            Self::CallMute => "chat.call-mute".into(),
            Self::CallDeafen => "chat.call-deafen".into(),
            Self::CallCamera => "chat.call-camera".into(),
            Self::CallScreen => "chat.call-screen".into(),
            Self::CallParticipantsToggle => "chat.call-participants".into(),
            Self::CallExpand => "chat.call-expand".into(),
            Self::CallLeave => "chat.call-leave".into(),
            Self::CallParticipant(id) => format!("chat.call-participant:{id}"),
            other => format!("chat.control:{other:?}"),
        }
    }
}

/// Where a shown message drew its own texts: the avatar, the author's name
/// and the time of a message that starts a group, and each laid-out line of
/// its body. All of it is the person's or the message's, none Chat's copy.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct PaintedMessage {
    pub(crate) avatar: Option<Rect>,
    pub(crate) author: Option<(Rect, String)>,
    /// The mark of the service a bridged author writes from, with what it
    /// says (`From Discord`).
    pub(crate) service: Option<(Rect, String)>,
    pub(crate) time: Option<(Rect, String)>,
    pub(crate) lines: Vec<(Rect, String)>,
}

impl PaintedMessage {
    /// What of it shows inside `area`, each part cut to it.
    pub(crate) fn clipped(self, area: Rect) -> Self {
        let cut = |rect: Rect| {
            let cut = rect.intersect(area);
            (cut.width() > 0.0 && cut.height() > 0.0).then_some(cut)
        };
        let cut_text = |(rect, text): (Rect, String)| cut(rect).map(|rect| (rect, text));
        Self {
            avatar: self.avatar.and_then(cut),
            author: self.author.and_then(cut_text),
            service: self.service.and_then(cut_text),
            time: self.time.and_then(cut_text),
            lines: self.lines.into_iter().filter_map(cut_text).collect(),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.avatar.is_none()
            && self.author.is_none()
            && self.service.is_none()
            && self.time.is_none()
            && self.lines.is_empty()
    }

    /// The row's children: the avatar as an image, then the name, the time
    /// and each body line as text, each with the rect it was drawn in.
    pub(crate) fn access_children(&self, id: &str) -> Vec<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, Role};
        let name = self.author.as_ref().map(|(_, name)| name.as_str());
        let mut children = Vec::new();
        if let Some(rect) = self.avatar {
            let mut avatar = AccessNode::new(Role::Image)
                .with_bounds(rect)
                .with_hashed_key((id, "avatar"));
            if let Some(name) = name {
                avatar = avatar.with_name(name);
            }
            children.push(avatar);
        }
        let texts = self.author.iter().map(|part| (part, "author"));
        for ((rect, text), part) in texts {
            children.push(
                AccessNode::new(Role::StaticText)
                    .with_name(text.as_str())
                    .with_bounds(*rect)
                    .with_hashed_key((id, part)),
            );
        }
        if let Some((rect, tip)) = &self.service {
            children.push(
                AccessNode::new(Role::Image)
                    .with_name(tip.as_str())
                    .with_bounds(*rect)
                    .with_hashed_key((id, "service")),
            );
        }
        let texts = self.time.iter().map(|part| (part, "time"));
        for ((rect, text), part) in texts {
            children.push(
                AccessNode::new(Role::StaticText)
                    .with_name(text.as_str())
                    .with_bounds(*rect)
                    .with_hashed_key((id, part)),
            );
        }
        for (index, (rect, text)) in self.lines.iter().enumerate() {
            children.push(
                AccessNode::new(Role::StaticText)
                    .with_name(text.as_str())
                    .with_bounds(*rect)
                    .with_hashed_key((id, "line", index)),
            );
        }
        children
    }
}

/// A painted control's spoken name, and whether it is Chat's own fixed copy:
/// a catalogue message with nothing of a person or a resource in it. A
/// feedback report shows such names as they are and hides the rest until
/// the reporter shows them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ControlName {
    pub(crate) text: String,
    pub(crate) interface: bool,
}

impl ControlName {
    /// The catalogue message `key`, the same for everyone.
    pub(crate) fn copy(key: &str) -> Self {
        Self {
            text: crate::i18n::t(key),
            interface: true,
        }
    }

    /// A name that carries someone's words, a person, a file or a resource.
    pub(crate) fn content(text: String) -> Self {
        Self {
            text,
            interface: false,
        }
    }
}

/// One painted control in the inventory pointer, keyboard and assistive
/// technology share: where it is, what it does, its name, its role and
/// whether it is selected or checked.
pub(crate) type SemanticHit = (Rect, Hit, ControlName, ui::a11y::Role, bool);

impl ChatApp {
    pub(crate) fn push_hit(hits: &mut Vec<(Rect, Hit)>, rect: Rect, hit: Hit) {
        hits.push((rect, hit));
    }

    /// Pointer, keyboard and assistive technology all derive from this one
    /// immediate-mode control inventory. Purely decorative/scrim targets stay
    /// out; every included row has a useful spoken name and an activation path.
    #[allow(
        clippy::too_many_lines,
        reason = "one exhaustive semantic mapping keeps pointer, keyboard and assistive labels from drifting into separate inventories"
    )]
    pub(crate) fn semantic_hits_from(&self, hits: &[(Rect, Hit)]) -> Vec<SemanticHit> {
        use ui::a11y::Role;

        hits.iter()
            .filter_map(|(rect, hit)| {
                let copy = ControlName::copy;
                let content = ControlName::content;
                let (name, role, selected) = match hit {
                    Hit::AgentWindow => (
                        copy(if self.conversation_surface {
                            "chat-agent-return"
                        } else {
                            "chat-agent-float"
                        }),
                        Role::Button,
                        false,
                    ),
                    Hit::FocusEdit => (copy("chat-edit-field"), Role::MultilineTextInput, false),
                    Hit::FocusMain => (copy("chat-composer"), Role::MultilineTextInput, false),
                    Hit::FocusThread => (
                        copy("chat-thread-composer"),
                        Role::MultilineTextInput,
                        false,
                    ),
                    Hit::HostModel(command) => {
                        (copy(&format!("chat-model-{command}")), Role::Button, false)
                    }
                    Hit::Send => (copy("chat-send-message"), Role::Button, false),
                    Hit::UndoDelete => (copy("chat-undo"), Role::Button, false),
                    Hit::JumpToLatest => (copy("chat-jump-latest-message"), Role::Button, false),
                    Hit::ThreadScroll(end) => (
                        copy(if *end {
                            "chat-thread-end"
                        } else {
                            "chat-thread-start"
                        }),
                        Role::Button,
                        false,
                    ),
                    Hit::AgentReasoning(id) => (
                        copy(if self.expanded_reasoning.contains(id) {
                            "chat-agent-hide-reasoning"
                        } else {
                            "chat-agent-show-reasoning"
                        }),
                        Role::Button,
                        false,
                    ),
                    Hit::ComposerEmoji(_) => (
                        copy(&crate::i18n::t("chat-emoji-insert")),
                        Role::Button,
                        false,
                    ),
                    Hit::Attach => (
                        copy(&crate::i18n::t("chat-attach-share")),
                        Role::Button,
                        false,
                    ),
                    Hit::Attachment { action, .. } => (copy(&action.label()), Role::Button, false),
                    Hit::CloseAttachment => (
                        copy(&crate::i18n::t("chat-attachment-close")),
                        Role::Button,
                        false,
                    ),
                    Hit::AttachDevice => (
                        copy(&crate::i18n::t("chat-attach-device")),
                        Role::Button,
                        false,
                    ),
                    Hit::AttachPick(path) => (
                        content(crate::i18n::t_args(
                            "chat-attach-pick",
                            &[(
                                "name",
                                std::path::Path::new(path)
                                    .file_name()
                                    .and_then(std::ffi::OsStr::to_str)
                                    .unwrap_or(path)
                                    .to_owned()
                                    .into(),
                            )],
                        )),
                        Role::Button,
                        false,
                    ),
                    Hit::StartCall => (copy("chat-call-start-or-open"), Role::Button, false),
                    Hit::StopSharingLiveContext => (copy("chat-context-stop"), Role::Button, false),
                    Hit::CopyResourceLink(message) => (
                        ControlName {
                            text: self.resource_navigation.label(message, true),
                            interface: self.resource_navigation.label_is_copy(message),
                        },
                        Role::Button,
                        false,
                    ),
                    Hit::OpenResource(message) => (
                        content(
                            self.resource_navigation.open_label(
                                message,
                                &self
                                    .message(message)
                                    .and_then(|m| m.reference.as_ref().map(|r| r.label.clone()))
                                    .unwrap_or_default(),
                            ),
                        ),
                        Role::Button,
                        false,
                    ),
                    Hit::OpenShare(message) => (
                        content(self.message(message).map_or_else(String::new, |m| {
                            crate::i18n::t_args(
                                "chat-share-open",
                                &[
                                    (
                                        "title",
                                        m.reference
                                            .as_ref()
                                            .map(|r| r.label.clone())
                                            .unwrap_or_default()
                                            .into(),
                                    ),
                                    ("name", self.user_name(&m.author).into()),
                                ],
                            )
                        })),
                        Role::Button,
                        false,
                    ),
                    Hit::StopShare(_) => (copy("chat-share-stop"), Role::Button, false),
                    Hit::SwitchShare(id, access) => (
                        ControlName {
                            text: crate::live_share::access_label(*access),
                            interface: true,
                        },
                        Role::ToggleButton,
                        self.message(id)
                            .and_then(|m| m.share.as_ref())
                            .is_some_and(|share| share.access == *access),
                    ),
                    Hit::ShareWindowPanel => (copy("chat-share-window"), Role::Button, false),
                    Hit::ShareAccess(access) => (
                        ControlName {
                            text: crate::live_share::access_label(*access),
                            interface: true,
                        },
                        Role::ToggleButton,
                        self.live_shares
                            .panel
                            .as_ref()
                            .is_some_and(|panel| panel.access == *access),
                    ),
                    Hit::SharePick(tab) => (
                        content(
                            ui::window_share::windows()
                                .into_iter()
                                .find(|window| window.tab == *tab)
                                .map_or_else(String::new, |window| {
                                    crate::i18n::t_args(
                                        "chat-share-pick",
                                        &[("title", window.title.into())],
                                    )
                                }),
                        ),
                        Role::ToggleButton,
                        self.live_shares
                            .panel
                            .as_ref()
                            .is_some_and(|panel| panel.chosen == Some(*tab)),
                    ),
                    Hit::ShareChosen => (copy("chat-share-window-button"), Role::Button, false),
                    Hit::ToggleRightFlap => (copy("chat-right-flap"), Role::Button, false),
                    Hit::TogglePins => (copy("chat-pins"), Role::ToggleButton, self.pins_open),
                    Hit::RevealMessage(message) => (
                        self.message(message).map_or_else(
                            || copy("chat-pinned-message"),
                            |m| content(format!("{}: {}", self.user_name(&m.author), m.text)),
                        ),
                        Role::Button,
                        false,
                    ),
                    Hit::ShowPerson { person, .. } => (
                        content(crate::i18n::t_args(
                            "chat-person-card",
                            &[("name", self.user_name(person).into())],
                        )),
                        Role::Button,
                        false,
                    ),
                    Hit::OpenThread(_) => (copy("chat-thread-open"), Role::Button, false),
                    Hit::CloseThread => (copy("chat-thread-close"), Role::Button, false),
                    Hit::Reaction { msg, emoji } => {
                        let mine = self
                            .message(msg)
                            .and_then(|message| {
                                message
                                    .reactions
                                    .iter()
                                    .find(|reaction| reaction.emoji == *emoji)
                            })
                            .is_some_and(|reaction| reaction.includes(&self.me.id));
                        (
                            content(ui::widget::reaction_strip::pill_name(emoji, mine)),
                            Role::ToggleButton,
                            mine,
                        )
                    }
                    Hit::AddReaction { .. } => {
                        (copy("chat-emoji-add-reaction"), Role::Button, false)
                    }
                    Hit::MessageBar { msg, action } => (
                        content(self.bar_label(msg, *action)),
                        if *action == MenuAction::PinMessage {
                            Role::ToggleButton
                        } else {
                            Role::Button
                        },
                        *action == MenuAction::PinMessage
                            && self.message(msg).is_some_and(|m| m.pinned),
                    ),
                    Hit::MessageMore { .. } => (copy("chat-message-more"), Role::Button, false),
                    Hit::EmojiChoice(emoji) => (
                        content(crate::i18n::t_args(
                            "chat-emoji-choose",
                            &[(
                                "name",
                                crate::i18n::emoji_catalogue()
                                    .get(emoji)
                                    .map_or(emoji.as_str(), |found| found.name())
                                    .into(),
                            )],
                        )),
                        Role::Button,
                        // The highlighted emoji, as the footer names it: a
                        // workspace emoji just added is the one selected.
                        self.emoji_picker.selected() == Some(emoji.as_str()),
                    ),
                    Hit::EmojiGroup(group) => (
                        // A group's label is the emoji catalogue's own fixed
                        // copy in the desktop's language.
                        crate::i18n::emoji_catalogue()
                            .groups()
                            .get(*group)
                            .map_or_else(
                                || copy("chat-emoji-group"),
                                |group| ControlName {
                                    text: group.label().to_owned(),
                                    interface: true,
                                },
                            ),
                        Role::Tab,
                        *group == self.emoji_picker.group(),
                    ),
                    Hit::EmojiAddCustom => (copy("chat-emoji-add"), Role::Button, false),
                    Hit::FocusEmojiSearch => (copy("chat-emoji-search"), Role::TextInput, false),
                    Hit::EditHistory { .. } => {
                        (copy("chat-edit-history-show"), Role::Button, false)
                    }
                    Hit::EditSave => (copy("chat-edit-save"), Role::Button, false),
                    Hit::EditCancel => (copy("chat-edit-cancel"), Role::Button, false),
                    Hit::CallMute => (
                        copy(if self.microphone_missing() {
                            "chat-call-no-microphone"
                        } else {
                            "chat-call-mute-action"
                        }),
                        Role::ToggleButton,
                        self.call.muted,
                    ),
                    Hit::CallDeafen => (
                        copy("chat-call-deafen"),
                        Role::ToggleButton,
                        self.call.deafened,
                    ),
                    Hit::CallCamera => (
                        copy("chat-call-camera-on"),
                        Role::ToggleButton,
                        !self.call.audio_only,
                    ),
                    Hit::CallScreen => (
                        copy("chat-call-share-screen"),
                        Role::ToggleButton,
                        self.call.sharing,
                    ),
                    Hit::CallParticipantsToggle => (
                        copy("chat-call-participants-show"),
                        Role::ToggleButton,
                        self.call.participants_expanded,
                    ),
                    Hit::CallExpand => (
                        copy("chat-call-expand-restore"),
                        Role::Button,
                        self.call.expanded,
                    ),
                    Hit::CallLeave => (copy("chat-call-leave-action"), Role::Button, false),
                    Hit::CallParticipant(id) => (
                        content(crate::i18n::t_args(
                            "chat-call-participant",
                            &[("name", self.call_label(id).into())],
                        )),
                        Role::Button,
                        false,
                    ),
                    _ => return None,
                };
                // The composer's pointer target is the whole box (clicking
                // its padding focuses it), but what assistive technology
                // reads as the text field is the band the shared `TextInput`
                // was laid out into — the attach / emoji / send buttons sit
                // beside that band, not inside it. Before the first paint
                // the band is empty, so the box stands in for it.
                let text_band = |field: &crate::composer::DraftComposer| {
                    let inner = field.rect();
                    if inner.width() > 0.0 && inner.height() > 0.0 {
                        inner
                    } else {
                        *rect
                    }
                };
                let rect = match hit {
                    Hit::FocusMain => text_band(&self.composer_main),
                    Hit::FocusThread => text_band(&self.composer_thread),
                    _ => *rect,
                };
                Some((rect, hit.clone(), name, role, selected))
            })
            .collect()
    }

    pub(crate) fn semantic_hits(&self) -> Vec<SemanticHit> {
        Self::deduplicate_semantic_hits(self.semantic_hits_from(&self.hits))
    }

    pub(crate) fn hover_tooltip_target(&self, point: Point) -> Option<(Rect, String)> {
        // A bridged author's service mark says where they write from.
        if let Some(tip) = self.painted_messages.values().find_map(|parts| {
            parts
                .service
                .as_ref()
                .filter(|(rect, _)| rect.contains(point))
                .cloned()
        }) {
            return Some(tip);
        }
        // So does a call guest's, beside their name. It lies under their
        // tile's own target, which takes the click.
        if let Some(tip) = self.hits.iter().find_map(|(rect, hit)| match hit {
            Hit::CallMark(user) if rect.contains(point) => {
                self.call_mark(user).map(|(_, tip)| (*rect, tip))
            }
            _ => None,
        }) {
            return Some(tip);
        }
        // A reaction pill says who reacted, with the strip's shared wording.
        if let Some((_, Hit::Reaction { msg, .. })) = self
            .hits
            .iter()
            .rev()
            .find(|(rect, _)| rect.contains(point))
            && let Some(tip) = self
                .reaction_strips
                .borrow()
                .iter()
                .filter(|((_, id), _)| id == msg)
                .find_map(|(_, strip)| {
                    strip.tooltip_at(point, &|reactor| self.user_name(&reactor.by))
                })
        {
            return Some(tip);
        }
        if let Some((rect, hit)) = self.hits.iter().rev().find(|(rect, hit)| {
            rect.contains(point)
                && matches!(
                    hit,
                    Hit::ToggleRightFlap
                        | Hit::TogglePins
                        | Hit::CallMute
                        | Hit::CallDeafen
                        | Hit::Attach
                        | Hit::Send
                        | Hit::ComposerEmoji(_)
                        | Hit::AddReaction { .. }
                        | Hit::MessageBar { .. }
                        | Hit::MessageMore { .. }
                )
        }) && let Some((_, _, label, _, _)) = self
            .semantic_hits_from(&[(*rect, hit.clone())])
            .into_iter()
            .next()
        {
            return Some((*rect, label.text));
        }
        // Do not let the broad title region lend its text to a labelled
        // control layered above it (for example the Call button).
        if self
            .hits
            .iter()
            .rev()
            .any(|(rect, hit)| rect.contains(point) && !matches!(hit, Hit::Dismiss))
        {
            return None;
        }
        None
    }

    /// A semantic target names an operation, not a particular paint pass. A
    /// message can appear in both timeline and thread briefly; exporting one
    /// retained node keeps that operation's target unambiguous.
    pub(super) fn deduplicate_semantic_hits(hits: Vec<SemanticHit>) -> Vec<SemanticHit> {
        let mut targets = std::collections::HashSet::new();
        hits.into_iter()
            .filter(|(_, hit, ..)| targets.insert(hit.semantic_id()))
            .collect()
    }
}
