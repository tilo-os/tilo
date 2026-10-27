//! What assistive technology is told: one node per control Chat paints, the
//! conversation's own structure, the composer field's focus, value and
//! selection, and the emoji picker's notice.

use super::{ChatApp, Hit};

impl ChatApp {
    /// One node per control Chat paints, with its state, value and target.
    pub(super) fn control_accessibility(&self) -> Vec<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};

        self.semantic_hits()
            .into_iter()
            .enumerate()
            .map(|(index, (rect, hit, name, role, selected))| {
                let input = self.text_field_access(&hit);
                let focused = self.host_focused
                    && self.keyboard_target.map_or_else(
                        || input.as_ref().is_some_and(|(focused, _, _)| *focused),
                        |target| target == index,
                    );
                let mut state = AccessState::new().focusable(true).focused(focused);
                // Of the buttons, the picker's emoji say which is the
                // highlighted one.
                if matches!(role, Role::Tab | Role::ListItem) || matches!(hit, Hit::EmojiChoice(_))
                {
                    state = state.selected(selected);
                }
                if matches!(role, Role::ToggleButton) {
                    state = state.checked(selected);
                }
                let mut node = AccessNode::new(role)
                    .with_name(name.text)
                    .with_interface_name(name.interface)
                    .with_state(state)
                    .with_bounds(rect)
                    .with_key(index as u64)
                    .with_action_target(hit.semantic_id());
                if let Some((_, value, (anchor, focus))) = input {
                    node = node.with_value(&value).with_text_selection(anchor, focus);
                } else if let Hit::Reaction { msg, emoji } = &hit
                    && let Some(reaction) = self.message(msg).and_then(|message| {
                        message
                            .reactions
                            .iter()
                            .find(|reaction| reaction.emoji == *emoji)
                    })
                {
                    node =
                        node.with_value(ui::widget::reaction_strip::pill_count(reaction.count()));
                }
                node
            })
            .collect()
    }

    pub(super) fn conversation_accessibility(&self) -> ui::a11y::AccessNode {
        use ui::a11y::{AccessNode, Role};

        if let Some(peer) = &self.direct_person {
            let rows = self
                .person_messages
                .iter()
                .map(|message| {
                    let author = if &message.author == peer {
                        self.person_display_name(peer)
                    } else {
                        self.me.name.clone()
                    };
                    AccessNode::new(Role::ListItem)
                        .with_name(format!("{author}: {}", message.text))
                        .with_role_description(crate::i18n::t("chat-message-kind"))
                        .with_hashed_key((&self.active_channel, &message.id, "private-message"))
                })
                .collect();
            return AccessNode::new(Role::List)
                .with_name(crate::i18n::t("chat-person-chat-private"))
                .with_children(rows)
                .with_hashed_key(("private-message-list", &self.active_channel));
        }
        let conversation = self
            .messages
            .iter()
            .filter(|message| {
                if self.thread_is_page() {
                    Some(&message.id) == self.thread_open.as_ref()
                        || message.parent.as_ref() == self.thread_open.as_ref()
                } else {
                    message.parent.is_none() || message.parent.as_ref() == self.thread_open.as_ref()
                }
            })
            .map(|message| {
                let mut node = AccessNode::new(Role::ListItem)
                    .with_name(self.model_message_label(message))
                    .with_role_description(crate::i18n::t("chat-message-kind"))
                    .with_hashed_key((&message.id, "message"));
                // A painted message is read and picked where it is drawn,
                // not over the whole conversation.
                if let Some(rect) = self.message_rect(&message.id) {
                    node = node.with_bounds(rect);
                }
                if let Some(parts) = self.painted_messages.get(&message.id) {
                    node = node.with_children(parts.access_children(&message.id));
                }
                if let Some(work) = &message.work {
                    node.with_description(crate::render_chat::agent_work_accessible_description(
                        work,
                    ))
                } else {
                    node
                }
            })
            .collect();
        AccessNode::new(Role::List)
            .with_name(crate::i18n::t("chat-messages"))
            .with_interface_name(true)
            .with_children(conversation)
            .with_hashed_key("message-list")
    }

    /// A text field's focus, value and selection (anchor, caret), for the
    /// hit that stands for it.
    pub(super) fn text_field_access(&self, hit: &Hit) -> Option<(bool, String, (usize, usize))> {
        match hit {
            Hit::FocusMain | Hit::FocusThread => {
                let composer = if matches!(hit, Hit::FocusMain) {
                    &self.composer_main
                } else {
                    &self.composer_thread
                };
                Some((
                    composer.is_focused(),
                    composer.value(),
                    composer.selection(),
                ))
            }
            Hit::FocusEdit => Some((
                self.edit_input.is_focused(),
                self.edit_input.value(),
                self.edit_input.selection(),
            )),
            _ => None,
        }
    }

    /// Why a picture chosen to add as an emoji was not, where the open
    /// picker says so.
    pub(super) fn emoji_notice_accessibility(&self) -> Option<ui::a11y::AccessNode> {
        let (notice, footer) = self.emoji_picker.notice().filter(|_| self.emoji_open)?;
        Some(
            ui::a11y::AccessNode::new(ui::a11y::Role::StaticText)
                .with_name(notice.to_owned())
                .with_bounds(footer)
                .with_live(ui::a11y::Politeness::Assertive)
                .with_hashed_key(("emoji-picker-notice", notice)),
        )
    }
}
