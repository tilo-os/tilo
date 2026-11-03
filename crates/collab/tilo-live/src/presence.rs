//! A workspace's presence track: what one participant said, and the board a
//! reader keeps of everyone's latest words.
//!
//! Presence is one sealed object per [`Presence`] frame on [`Track::Presence`]
//! ([`crate::plane`]): the plane opens it, this module names what is in it.
//! The relay copies bytes it cannot read; a reader that is not on the roster
//! never opens one.

use std::collections::BTreeMap;

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

/// What every sender of one scope last said, newest word wins: a reader
/// draws from this and nothing else.
#[derive(Clone, Debug, Default)]
pub struct Board {
    said: BTreeMap<[u8; 32], Presence>,
    revision: u64,
}

impl Board {
    /// An empty board.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Take the frame `sender` said: `true` when it changed what the board
    /// says of them, so a reader that compares revisions draws one frame and
    /// a restated word draws none.
    pub fn hear(&mut self, sender: [u8; 32], presence: Presence) -> bool {
        match self.said.get(&sender) {
            Some(said) if *said == presence => false,
            _ => {
                self.said.insert(sender, presence);
                self.revision = self.revision.wrapping_add(1);
                true
            }
        }
    }

    /// The last frame `sender` said.
    #[must_use]
    pub fn said(&self, sender: [u8; 32]) -> Option<&Presence> {
        self.said.get(&sender)
    }

    /// Everyone the board holds, by sender.
    pub fn senders(&self) -> impl Iterator<Item = ([u8; 32], &Presence)> {
        self.said.iter().map(|(sender, said)| (*sender, said))
    }

    /// Forget `sender`: it left, or its frames stopped being opened.
    pub fn forget(&mut self, sender: [u8; 32]) -> bool {
        let forgotten = self.said.remove(&sender).is_some();
        if forgotten {
            self.revision = self.revision.wrapping_add(1);
        }
        forgotten
    }

    /// How many times what the board says changed.
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Whether the board holds nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.said.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use collab_types::presence::{Payload, PresenceKey};

    const SENDER: [u8; 32] = [0x21; 32];

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

    #[test]
    fn the_board_keeps_each_senders_last_word_and_counts_changes() {
        let mut board = Board::new();
        assert!(board.is_empty());
        assert!(board.hear(SENDER, said("Reviewing")));
        assert_eq!(board.revision(), 1);
        assert!(
            !board.hear(SENDER, said("Reviewing")),
            "a restated word draws no frame"
        );
        assert_eq!(board.revision(), 1);
        assert!(board.hear(SENDER, said("Done")));
        assert_eq!(board.revision(), 2);
        assert_eq!(
            board.said(SENDER).map(|said| said.payload.clone()),
            Some(Payload::Status {
                text: "Done".into(),
                automatic: false,
            })
        );
        assert!(board.forget(SENDER));
        assert!(!board.forget(SENDER));
        assert!(board.is_empty());
        assert_eq!(board.revision(), 3);
    }

    #[test]
    fn a_pointer_frame_names_its_tab() {
        let mut board = Board::new();
        board.hear(SENDER, Presence::pointing("w1", 3, 10.0, 20.0));
        let said = board.said(SENDER).unwrap();
        assert_eq!(
            said.key,
            PresenceKey::Pointer {
                space: collab_types::presence::PointerSpace::Tile {
                    workspace: "w1".into(),
                    tab: 3,
                },
            }
        );
    }
}
