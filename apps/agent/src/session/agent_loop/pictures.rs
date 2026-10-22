//! Pictures a model is shown: those the invokes of a turn answered with (a
//! look at a canvas, a page, a photo), once the turn's results are in, and
//! only to a model that sees.

use super::{LoopState, Session};
use crate::model::Conversation;

/// The most pictures one turn shows the model.
pub(super) const MOST_PICTURES: usize = 4;
/// The largest picture shown to the model; providers refuse larger ones.
pub(super) const MOST_PICTURE_BYTES: usize = 4 * 1024 * 1024;

/// The media type of a picture a model can be shown, from its first bytes.
pub(super) fn picture_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP".as_slice()) {
        Some("image/webp")
    } else {
        None
    }
}

impl Session {
    /// Show the model the pictures this turn's invokes answered with: a
    /// look at a canvas, a page, a photo. Only a model that sees is shown
    /// them, only pictures the device's content holds, each at most
    /// [`MOST_PICTURE_BYTES`].
    pub(super) fn show_pictures(&self, conversation: &mut Conversation, state: &mut LoopState) {
        let mut shown = Vec::new();
        let mut pictures = Vec::new();
        for address in state.pictures.drain(..) {
            let Some(bytes) = tilo_actions::content_hash(&address)
                .and_then(|hash| self.cfg.content.read(hash))
                .filter(|bytes| bytes.len() <= MOST_PICTURE_BYTES)
            else {
                continue;
            };
            if let Some(media_type) = picture_type(&bytes) {
                shown.push(address);
                pictures.push((media_type.to_owned(), bytes));
            }
        }
        if !pictures.is_empty() {
            conversation.push_pictures(
                &format!(
                    "The pictures your invokes answered with, in order: {}.",
                    shown.join(", ")
                ),
                pictures,
            );
        }
    }
}
