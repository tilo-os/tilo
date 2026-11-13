//! The bounded message and thread rows a mail window materialises on demand:
//! a stable id resolves even when the open-resource tree carries only a
//! prefix of the folder.

#[allow(
    clippy::wildcard_imports,
    reason = "the rows are the mailroom's own, reading the mailroom module's types"
)]
use super::*;

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
