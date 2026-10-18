//! The invite/join sheet as assistive technology sees and operates it.
//!
//! The painted targets are published exactly as the last paint registered
//! them, so a reader's cursor and a click agree on what is under them. An
//! action names its target by what it does (`join`, `role/editor`), never by
//! its position, so a target from an earlier state of the sheet resolves to
//! nothing rather than to whatever now sits in its place.

use ui::a11y::{AccessNode, AccessState, Action, Politeness, Role};

use super::{Button, EventOutcome, InviteRecipientPath, PeerSyncOverlay};
use crate::shell::i18n::t;

const TARGET_PREFIX: &str = "peer-sync:";
const FIELD: &str = "field";

fn slug(button: Button) -> String {
    use crate::network::invite::InviteRole;
    match button {
        Button::CreateInvite => "create".into(),
        Button::ReviewInvite => "review".into(),
        Button::BackToComposer => "back".into(),
        Button::SelectRole(role) => format!(
            "role/{}",
            match role {
                InviteRole::Viewer => "viewer",
                InviteRole::Commenter => "commenter",
                InviteRole::Editor => "editor",
            }
        ),
        Button::SelectKnown(index) => format!("known/{index}"),
        Button::SelectNewRecipient => "new-recipient".into(),
        Button::Join => "join".into(),
        Button::Cancel => "cancel".into(),
        Button::CopyToken => "copy".into(),
        Button::PasteToken => "paste".into(),
        Button::Resend(id) => format!("resend/{}", hex::encode(id)),
        Button::CancelPending(id) => format!("cancel-pending/{}", hex::encode(id)),
        Button::PasteIdentity => "paste-identity".into(),
        Button::CopyIdentity => "copy-identity".into(),
        Button::CopyPairLink => "copy-pair-link".into(),
    }
}

/// Whether `target` was published by this sheet.
pub fn is_sheet_target(target: &str) -> bool {
    target.starts_with(TARGET_PREFIX)
}

impl PeerSyncOverlay {
    fn title_key(&self) -> &'static str {
        if self.state.invite_review_open {
            "peer-sync-title-review"
        } else if self.state.invite_name.is_some() {
            "peer-sync-title-invite"
        } else if self.state.connect_input.is_some() {
            "peer-sync-title-join"
        } else {
            "peer-sync-title-workspace-invite"
        }
    }

    /// The text being typed, when the sheet takes typing.
    fn editable_text(&self) -> Option<(String, &str)> {
        if let Some(input) = self.state.connect_input.as_deref() {
            return Some((t("peer-sync-paste-token"), input));
        }
        if self.state.invite_review_open
            || self.state.invite_recipient_path != InviteRecipientPath::New
        {
            return None;
        }
        self.state
            .invite_name
            .as_deref()
            .map(|name| (t("peer-sync-name-new-person"), name))
    }

    /// The sheet as one modal dialog, or `None` while it is closed.
    pub fn accessibility(&self) -> Option<AccessNode> {
        if !self.visible() {
            return None;
        }
        let mut children = Vec::new();
        let field = self.editable_text();
        if let Some((name, value)) = &field {
            let end = value.len();
            children.push(
                AccessNode::new(Role::TextInput)
                    .with_name(name.clone())
                    .with_value((*value).to_owned())
                    .with_text_selection(end, end)
                    .with_action_target(format!("{TARGET_PREFIX}{FIELD}"))
                    .with_state(AccessState::new().focusable(true).focused(true)),
            );
        } else if let Some(token) = self.state.invite_token.as_deref()
            && self.state.invite_name.is_none()
        {
            children.push(AccessNode::new(Role::StaticText).with_name(token.to_owned()));
        }
        for (index, ((button, rect), drawn)) in self
            .button_rects
            .iter()
            .zip(&self.button_labels)
            .enumerate()
        {
            let mut state = AccessState::new()
                .focusable(true)
                .focused(field.is_none() && index == 0);
            let role = if let Some(selected) = drawn.selected {
                state = state.checked(selected).selected(selected);
                Role::RadioButton
            } else {
                Role::Button
            };
            let mut node = AccessNode::new(role)
                .with_name(drawn.label.clone())
                .with_action_target(format!("{TARGET_PREFIX}{}", slug(*button)))
                .with_bounds(*rect)
                .with_state(state);
            if let Some(description) = &drawn.description {
                node = node.with_description(description.clone());
            }
            children.push(node);
        }
        if let Some(status) = self.state.status.as_deref() {
            children.push(
                AccessNode::new(Role::StaticText)
                    .with_name(status.to_owned())
                    .with_live(Politeness::Polite),
            );
        }
        let mut dialog = AccessNode::new(Role::Dialog)
            .with_name(t(self.title_key()))
            .with_state(AccessState::new().modal(true))
            .with_children(children);
        if self.state.invite_review_open {
            dialog = dialog.with_description(t("peer-sync-review-note"));
        }
        if self.rect.area() > 0.0 {
            dialog = dialog.with_bounds(self.rect);
        }
        Some(dialog)
    }

    /// Operate the sheet from an assistive technology: the same outcome a
    /// click on the target, or typing into the field, has. `None` when
    /// `target` names nothing on the sheet as it is now.
    pub fn perform_accessibility_action(
        &mut self,
        target: &str,
        action: &Action,
    ) -> Option<EventOutcome> {
        let part = target.strip_prefix(TARGET_PREFIX)?;
        if !self.visible() {
            return None;
        }
        if part == FIELD {
            self.editable_text()?;
            return match action {
                Action::Focus | Action::Activate | Action::ScrollIntoView => {
                    Some(EventOutcome::consumed())
                }
                Action::SetValue(value) => {
                    let value: String = value.chars().filter(|ch| !ch.is_control()).collect();
                    if let Some(input) = self.state.connect_input.as_mut() {
                        *input = value.split_whitespace().collect();
                    } else if let Some(name) = self.state.invite_name.as_mut() {
                        *name = value;
                    }
                    Some(EventOutcome::consumed())
                }
                _ => None,
            };
        }
        let button = self
            .button_rects
            .iter()
            .map(|(button, _)| *button)
            .find(|button| slug(*button) == part)?;
        match action {
            Action::Activate | Action::ToggleCheck => Some(self.activate(button)),
            Action::Focus | Action::ScrollIntoView => Some(EventOutcome::consumed()),
            _ => None,
        }
    }
}
