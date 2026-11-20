//! An attachment's bytes, wherever its message keeps them: in the message, on this device, or
//! kept apart from a held message in pieces of the mailbox's content, read a piece at a time.

use super::{MailApp, Mailroom};
use crate::model::MessageId;
use crate::notice::AttachmentUnavailable;

impl Mailroom {
    /// Whether attachment `index` of `message` can be read here, without
    /// reading it.
    pub(crate) fn attachment_available(
        &self,
        message: &str,
        index: usize,
    ) -> Result<(), AttachmentUnavailable> {
        let message_id = message
            .parse::<MessageId>()
            .map_err(|_| AttachmentUnavailable::UnknownMessage)?;
        let shared = self.shared.borrow();
        let attachment = shared
            .mailbox
            .get(message_id)
            .and_then(|message| message.attachments.get(index))
            .ok_or(AttachmentUnavailable::NotLoaded)?;
        if !attachment.is_downloadable() {
            return Err(AttachmentUnavailable::NotDownloaded);
        }
        Ok(())
    }

    /// Attachment `index` of `message`: the name to save it under, its
    /// filename, its size, and its bytes, read a piece at a time where it is
    /// kept apart from its message.
    fn attachment_bytes(
        &self,
        message: &str,
        index: usize,
    ) -> Result<(String, String, u64, Box<dyn std::io::Read>), AttachmentUnavailable> {
        self.attachment_available(message, index)?;
        let message_id = message
            .parse::<MessageId>()
            .map_err(|_| AttachmentUnavailable::UnknownMessage)?;
        let (mut attachment, account) = {
            let shared = self.shared.borrow();
            let found = shared
                .mailbox
                .get(message_id)
                .ok_or(AttachmentUnavailable::NotLoaded)?;
            (
                found
                    .attachments
                    .get(index)
                    .cloned()
                    .ok_or(AttachmentUnavailable::NotLoaded)?,
                found.account.clone(),
            )
        };
        let name = MailApp::attachment_save_name(&attachment.filename);
        let not_kept = || AttachmentUnavailable::NotKept(attachment.filename.clone());
        if attachment.content.is_empty() && attachment.kept {
            attachment.content = self
                .store
                .attachment(&account, message_id, index)
                .ok_or_else(not_kept)?;
        }
        if attachment.content.is_empty() && attachment.apart {
            // Kept apart from the message, in the mailbox's own content.
            let held = {
                let shared = self.shared.borrow();
                let store = ui::tilo_resource::content::current();
                shared.chain.as_ref().zip(store).and_then(|(chain, store)| {
                    chain
                        .parts()
                        .iter()
                        .rev()
                        .find_map(|(_, part)| part.held_attachment(message, index, &store))
                })
            };
            let (body, size) = held.ok_or_else(not_kept)?;
            if attachment.size != size {
                return Err(AttachmentUnavailable::Changed);
            }
            return Ok((name, attachment.filename, size, Box::new(body)));
        }
        if attachment.size != attachment.content.len() as u64 {
            return Err(AttachmentUnavailable::Changed);
        }
        Ok((
            name,
            attachment.filename,
            attachment.size,
            Box::new(std::io::Cursor::new(attachment.content)),
        ))
    }

    /// What saving attachment `index` of `message` streams from the
    /// workspace's server, where the page carries files out
    /// ([`ui::carry_out`]): an attachment kept apart from a held message, as
    /// the pieces it came in, which the server decodes as it serves them. So
    /// a file of any size saves without its bytes passing through here.
    /// `None` for one read here ([`Self::attachment_to_save`]). `store` is
    /// the mailbox's content, where the message itself is read.
    pub(crate) fn attachment_link(
        &self,
        message: &str,
        index: usize,
        store: &tilo_content::ContentStore,
    ) -> Option<ui::tilo_actions::remote::ContentLinkAsk> {
        self.attachment_available(message, index).ok()?;
        let message_id = message.parse::<MessageId>().ok()?;
        let filename = {
            let shared = self.shared.borrow();
            let attachment = shared.mailbox.get(message_id)?.attachments.get(index)?;
            if !attachment.content.is_empty() || !attachment.apart {
                return None;
            }
            attachment.filename.clone()
        };
        let encoded = {
            let shared = self.shared.borrow();
            shared
                .chain
                .as_ref()?
                .parts()
                .iter()
                .rev()
                .find_map(|(_, part)| part.held_attachment_pieces(message, index, store))?
        };
        let name = MailApp::attachment_save_name(&filename);
        Some(ui::tilo_actions::remote::ContentLinkAsk {
            files: vec![ui::tilo_actions::remote::ContentLinkFile {
                path: name.clone(),
                hash: tilo_content::content_address(encoded.pieces.first()?)?,
                encoded: Some(encoded),
            }],
            name,
            zip: false,
        })
    }

    /// Attachment `index` of `message` whole, and the name to save it under.
    pub(crate) fn attachment_to_save(
        &self,
        message: &str,
        index: usize,
    ) -> Result<(String, Vec<u8>), AttachmentUnavailable> {
        let (name, filename, size, mut body) = self.attachment_bytes(message, index)?;
        let mut bytes = Vec::with_capacity(usize::try_from(size).unwrap_or(0));
        std::io::Read::read_to_end(&mut body, &mut bytes)
            .map_err(|_| AttachmentUnavailable::NotKept(filename))?;
        if bytes.len() as u64 != size {
            return Err(AttachmentUnavailable::Changed);
        }
        Ok((name, bytes))
    }

    /// Attachment `index` of `message` kept in `store` a part at a time, as
    /// content of any size is: the name to save it under, and where it is.
    pub(crate) fn keep_attachment(
        &self,
        message: &str,
        index: usize,
        store: &tilo_content::ContentStore,
    ) -> Result<(String, tilo_content::Stored), AttachmentUnavailable> {
        let (name, filename, size, mut body) = self.attachment_bytes(message, index)?;
        let kept = store
            .put_parts(&mut body)
            .map_err(|_| AttachmentUnavailable::NotKept(filename))?;
        if kept.len != size {
            return Err(AttachmentUnavailable::Changed);
        }
        Ok((name, kept))
    }
}
