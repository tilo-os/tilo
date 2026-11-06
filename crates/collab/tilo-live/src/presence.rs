//! A workspace's presence track: what one participant said, in a frame a
//! reader on the roster can open.
//!
//! Presence is one sealed object per [`Presence`] frame on [`Track::Presence`]
//! ([`crate::plane`]): the plane opens it, this module names what is in it.
//! The relay copies bytes it cannot read; a reader that is not on the roster
//! never opens one.

use collab_types::presence::Presence;

use crate::live::Track;
use crate::plane::{Plane, PlaneError};

/// Say `presence` as object `sequence` of the plane's presence track.
///
/// # Errors
///
/// [`PlaneError::Invalid`] when the frame cannot be encoded, and whatever the
/// plane answers with: [`PlaneError::Ended`] when the session ended,
/// [`PlaneError::Unavailable`] when the broadcast is gone,
/// [`PlaneError::Refused`] when the scope's keys do not seal this device.
pub fn say(plane: &Plane, sequence: u32, presence: &Presence) -> Result<(), PlaneError> {
    let body = presence.encode().map_err(|_| PlaneError::Invalid)?;
    plane.publish(Track::Presence, sequence, &body)
}

/// The frame `body` carries, or `None` when it is not one.
#[must_use]
pub fn decode(body: &[u8]) -> Option<Presence> {
    Presence::decode(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use collab_types::presence::Payload;

    fn said(text: &str) -> Presence {
        Presence::desktop(
            "w1",
            Payload::Status {
                text: text.to_owned(),
                automatic: false,
            },
        )
    }

    #[test]
    fn a_frame_is_the_bytes_the_track_carries() {
        let presence = said("Reviewing");
        let body = presence.encode().unwrap();
        assert_eq!(decode(&body), Some(presence));
        assert_eq!(decode(&[0xff]), None);
    }
}
