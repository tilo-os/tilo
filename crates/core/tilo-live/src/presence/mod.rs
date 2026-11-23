//! A workspace's presence: what one device said, in a frame a reader on
//! the roster can open, and the board a reader keeps of everyone's last word.
//!
//! Presence is one sealed object per [`Presence`] frame on [`Track::Presence`]
//! ([`crate::live_session`]): the live session opens it, this module names what is in it.
//! The relay copies bytes it cannot read; a reader that is not on the roster
//! never opens one.
//!
//! A frame carries one part of what its sender says ([`Payload`]): their
//! name, that they are here, how available they are, what they are doing,
//! whose view they follow, their wave, their pointer over a tile, or — as a
//! shell — their face, their screen and the caret in it. So a reader merges
//! the parts it hears into what each member says now ([`Board`]), smooths
//! their pointer ([`Smoother`]) and places it in the tiles its own surface
//! shows ([`place`]). One schedule sends this device's own word, at one rate
//! ([`publish`]), so a pointer moving at the display's rate costs the link
//! only what it carries.

use collab_types::presence::Presence;

use crate::live::Track;
use crate::live_session::{LiveError, LiveSession};

mod board;
pub mod place;
mod publish;
mod shown;
mod smooth;

#[cfg(test)]
mod tests;

pub use board::{
    Board, BridgedMember, Call, Device, LiveCall, LiveCaller, Own, Point, Present, SILENT_MS,
    Status, without_clock,
};
pub use place::{Tile, anchor, tile};
pub use publish::{KEEPALIVE_MS, MIN_INTERVAL_MS, REPEATS_MS};
pub use shown::{own_shown, shown};
pub use smooth::Smoother;

/// A wave as a member's word and a surface both carry it.
pub use collab_types::presence::ShownWave as Wave;

/// Say `frames`, one burst of this device's presence, each as the object its number names, as one
/// group of the live session's presence track ([`LiveSession::publish_burst`]).
///
/// # Errors
///
/// [`LiveError::Invalid`] when the frame cannot be encoded, and whatever the
/// live session answers with: [`LiveError::Ended`] when the session ended,
/// [`LiveError::Unavailable`] when the broadcast is gone,
/// [`LiveError::Refused`] when the scope's keys do not seal this device.
pub fn say(live_session: &LiveSession, frames: &[(u32, Presence)]) -> Result<(), LiveError> {
    let bodies = frames
        .iter()
        .map(|(sequence, presence)| {
            presence
                .encode()
                .map(|body| (*sequence, body))
                .map_err(|_| LiveError::Invalid)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let objects: Vec<(u32, &[u8])> = bodies
        .iter()
        .map(|(sequence, body)| (*sequence, body.as_slice()))
        .collect();
    live_session.publish_burst(Track::Presence, &objects)
}

/// The frame `body` carries, or `None` when it is not one.
#[must_use]
pub fn decode(body: &[u8]) -> Option<Presence> {
    Presence::decode(body)
}

#[cfg(test)]
mod frame_tests {
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
