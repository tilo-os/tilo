//! The bounded message and thread rows a mail window materialises on demand:
//! a stable id resolves even when the open-resource tree carries only a
//! prefix of the folder.

#[allow(
    clippy::wildcard_imports,
    reason = "the rows are the mailroom's own, reading the mailroom module's types"
)]
use super::*;

use crate::actions::MailEffect;

impl Mailroom {
    pub(crate) fn resource_message_row(&self, id: &str) -> Option<crate::actions::MessageRow> {
        self.shared
            .borrow()
            .mailbox
            .messages
            .values()
            .find(|message| message.id.to_string() == id)
            .map(crate::actions::message_row)
    }

    /// What the row `id`'s effects say of themselves right now: what opening
    /// a row shows, the choice a contact draft is made from, and an
    /// attachment whose bytes are not here yet.
    pub(crate) fn row_status(&self, id: &str) -> Vec<ui::framework::EffectStatus> {
        if let Some(rest) = id.strip_prefix("mail.attachment:") {
            let Some((message_id, index)) = rest.rsplit_once(':') else {
                return Vec::new();
            };
            let Ok(index) = index.parse::<usize>() else {
                return Vec::new();
            };
            let Some(attachment) = self
                .resource_message_row(message_id)
                .and_then(|message| message.attachments.get(index).cloned())
            else {
                return Vec::new();
            };
            if attachment.downloadable {
                return Vec::new();
            }
            let reason = crate::i18n::CATALOG.t("mail-attachment-not-downloaded");
            return vec![
                ui::framework::EffectStatus::unavailable(
                    MailEffect::SAVE,
                    "not_downloaded",
                    reason.clone(),
                )
                .on(id),
                ui::framework::EffectStatus::unavailable(
                    MailEffect::CONTENT,
                    "not_downloaded",
                    reason.clone(),
                )
                .on(id),
                ui::framework::EffectStatus::unavailable(
                    MailEffect::OPEN_ATTACHMENT,
                    "not_downloaded",
                    reason,
                )
                .on(id),
            ];
        }
        if id.starts_with("mail.contact-draft:") {
            return self
                .contact_choices()
                .into_iter()
                .find(|choice| choice.id() == id)
                .map(|choice| crate::app::contact_recipients::contact_draft_status(&choice, id))
                .unwrap_or_default();
        }
        let label = if id.starts_with("mail.message:") {
            Some("Open message")
        } else if id.starts_with("mail.draft:") {
            Some("Open draft")
        } else if id.starts_with("mail.account:") {
            Some("Open account")
        } else {
            None
        };
        label
            .map(|label| {
                vec![ui::framework::EffectStatus::labelled(
                    MailEffect::OPEN,
                    label,
                )]
            })
            .unwrap_or_default()
    }

    pub(crate) fn resource_thread_rows(
        &self,
        stable: &str,
    ) -> Option<(String, Vec<crate::actions::MessageRow>)> {
        let (folder_raw, anchor_raw) = stable.split_once(':')?;
        let folder = Folder::from_key(folder_raw)?;
        if anchor_raw.is_empty() {
            return None;
        }
        let state = self.shared.borrow();
        let thread = state
            .mailbox
            .threads_in(folder)
            .into_iter()
            .find(|thread| thread.id == anchor_raw)?;
        let subject = thread.subject;
        Some((
            subject,
            thread
                .message_ids
                .into_iter()
                .filter_map(|id| state.mailbox.get(id))
                .take(crate::resource::OPEN_MESSAGE_LIMIT)
                .map(crate::actions::message_row)
                .collect(),
        ))
    }
}
