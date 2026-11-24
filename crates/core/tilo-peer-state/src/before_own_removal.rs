//! Messages as stores kept them while a document write named the member's own record it was: a
//! store's frames before version 5 ([`crate::store`]), and the publications a governed
//! workspace's host kept ([`kept_message`]). Each reads as the message of today. Nothing
//! reads which member's own record a write was any more, so that is dropped; the envelope is the
//! bytes its author signed.
//!
//! It brings up what earlier builds wrote, so a host moving onto this build keeps its history: a
//! host's store is what every other device fetches from, so nothing else holds it. It can go once
//! the hosts and devices kept by those builds have opened their state on one after them.

use neighbour_wire::NeighbourMsg;
use serde::Deserialize;

/// A document write's fields before its reader set, as those builds kept them.
#[derive(Deserialize)]
struct DocumentWrite {
    peer: neighbour_wire::PeerId,
    app_id: String,
    cluster_id: String,
    document_id: String,
    envelope: Box<collab_types::SignedOp>,
    /// The own record's kind and its person, which nothing reads.
    _own: Option<(String, [u8; 32])>,
}

impl DocumentWrite {
    fn with(self, readers: Option<String>) -> NeighbourMsg {
        NeighbourMsg::AppDocument {
            peer: self.peer,
            app_id: self.app_id,
            cluster_id: self.cluster_id,
            document_id: self.document_id,
            envelope: self.envelope,
            readers,
        }
    }
}

/// The message a whole frame `bytes` holds. A store's version 1 ended a document write before its
/// reader set, which reads as naming none.
///
/// # Errors
/// `bytes` are not one message of that layout.
pub(crate) fn frame(bytes: &[u8]) -> Result<NeighbourMsg, postcard::Error> {
    let Some(fields) = bytes.strip_prefix(&[NeighbourMsg::APP_DOCUMENT_TAG]) else {
        return NeighbourMsg::decode(bytes);
    };
    let (write, rest) = postcard::take_from_bytes::<DocumentWrite>(fields)?;
    if rest.is_empty() {
        return Ok(write.with(None));
    }
    match postcard::take_from_bytes::<Option<String>>(rest)? {
        (readers, []) => Ok(write.with(readers)),
        _ => Err(postcard::Error::DeserializeBadEncoding),
    }
}

/// The message a whole frame `bytes` holds, kept with no version that says which layout it is
/// in: this build's, or one of those builds' ([`frame`]). A governed workspace's host keeps its
/// publications so, and those builds' hosts published document writes in it.
///
/// Today's layout is read first. Bytes that read whole in it mean the same in an earlier one or
/// do not read there, and those builds' document writes do not read whole in it, so which layout
/// they are in is never in doubt.
///
/// # Errors
/// `bytes` are one message in neither layout; the error is the one today's layout gives.
pub fn kept_message(bytes: &[u8]) -> Result<NeighbourMsg, postcard::Error> {
    NeighbourMsg::decode(bytes).or_else(|today| frame(bytes).map_err(|_| today))
}
