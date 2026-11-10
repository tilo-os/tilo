//! Content read out of another device's workspace content store, through
//! this device's session: each piece is a
//! [`RemoteActionMessage::ContentGetRequest`] the device answers, a larger
//! read split into pieces a frame carries.

use tilo_actions::remote::{
    ActionHostError, Caller, ContentGets, ContentPiece, MAX_CONTENT_FRAME_BYTES,
};

use super::SessionActions;

/// The most raw bytes one request asks the device for.
const PIECE: u64 = MAX_CONTENT_FRAME_BYTES as u64;

/// The device's content gets, asked through the session.
pub(super) struct SessionGets(pub(super) SessionActions);

impl ContentGets for SessionGets {
    fn get(
        &self,
        _caller: &Caller,
        content: &str,
        offset: u64,
        length: u64,
    ) -> Result<ContentPiece, ActionHostError> {
        let mut bytes = Vec::new();
        let mut at = offset;
        // The whole content's size, once a piece says it.
        let mut size = None;
        // Bytes still wanted; zero asks for one piece.
        let mut left = length;
        loop {
            let cap = if left == 0 { PIECE } else { left.min(PIECE) };
            let wanted = size.map_or(cap, |size: u64| cap.min(size.saturating_sub(at)));
            let asked = self.0.get_piece(content, at, wanted)?;
            if asked.offset != at {
                return Err(ActionHostError::Unavailable {
                    reason: format!(
                        "the device answered the piece at {} asked at {at}",
                        asked.offset
                    ),
                });
            }
            let piece = asked.bytes().ok_or_else(|| ActionHostError::Unavailable {
                reason: format!("the device answered {content} with bytes that are not hex"),
            })?;
            size = Some(asked.size);
            if piece.is_empty() && at < asked.size {
                return Err(ActionHostError::Unavailable {
                    reason: format!(
                        "the device answered no bytes of {content} at {at} of {}",
                        asked.size
                    ),
                });
            }
            at = at.saturating_add(u64::try_from(piece.len()).unwrap_or(u64::MAX));
            bytes.extend_from_slice(&piece);
            if left == 0 || at >= asked.size {
                break;
            }
            left = left.saturating_sub(u64::try_from(piece.len()).unwrap_or(u64::MAX));
            if left == 0 {
                break;
            }
        }
        Ok(ContentPiece {
            size: size.unwrap_or(0),
            offset,
            bytes,
        })
    }
}
