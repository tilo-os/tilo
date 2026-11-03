//! A desktop shell's view of itself, as the other shells of its workspace
//! read it: who it shows there, where its windows are, which one it works
//! in, and the caret in it.
//!
//! The desktop's own pointer is not here: that travels as a
//! [`PointerSpace::Tile`](super::PointerSpace::Tile) pointer, the way every
//! device's pointer does, so a shell places it over its own tile for the
//! tab without knowing anything else about the sender's screen.

use serde::{Deserialize, Serialize};

/// Longest activity line an agent's presence carries.
pub const MAX_ACTIVITY_BYTES: usize = 96;

/// One window a shell shows: the app and record it draws, its edges, and
/// whether it is the one the person works in.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShellWindow {
    /// The app the window shows, as its stable id.
    pub app: String,
    /// The window's record, as its short digest
    /// ([`LiveRecord::short_digest`](tilo_relay_protocol::live::LiveRecord::short_digest)):
    /// a receiver knows which of its own windows shows the same record
    /// without ever seeing the sender's local id.
    pub record: [u8; 8],
    /// The window's edges across the sender's screen, in 1/10000 of
    /// [`ShellPresence::size`], as `[x0, y0, x1, y1]`.
    pub rect: [u16; 4],
    /// Whether the person works in this window: the record their caret and
    /// selection are in.
    pub focused: bool,
}

/// What the person says beside their pointer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShellChat {
    /// The text they say.
    pub text: String,
    /// Whether it is sent (shown and left to fade) or still being typed.
    pub sent: bool,
}

/// The latest reaction the person gave, as it floats up from their pointer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShellReaction {
    /// Its sequence: a reaction is new when this is.
    pub seq: u32,
    /// The emoji.
    pub emoji: String,
}

/// The caret and selection an app shows for the person, as the
/// record-scoped anchors the app reported.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShellSelection {
    /// The caret's anchor, hex.
    pub anchor: String,
    /// The other end of the selection, hex.
    pub head: String,
}

/// Who a shell shows itself as, and what its desktop looks like now.
///
/// Sent whole whenever any of it changes, not on pointer motion: a
/// receiver draws the sender's face and a mini view of its windows, and
/// places the sender's caret in the window it names.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ShellPresence {
    /// This view's sequence: receivers order views by it, and one at or
    /// below the last they took is a repeat of it.
    pub seq: u32,
    /// When it was sent, in the sender's wall-clock milliseconds: receivers
    /// map it onto their own clock, and read a repeat's arrival against it.
    pub sent_at_ms: u64,
    /// The name this device shows itself by.
    pub label: String,
    /// Whether it is an agent rather than a person.
    pub agent: bool,
    /// The colour its face wears, `#rrggbb`.
    pub colour: String,
    /// What an agent is doing now, never for a person.
    pub activity: Option<String>,
    /// Where within its desktop the person is: the strip it shows, or
    /// `None` while it shows no window and no strip.
    pub place: Option<String>,
    /// The sender's screen, in logical pixels.
    pub size: [u32; 2],
    /// Its windows, in the order it paints them.
    pub windows: Vec<ShellWindow>,
    /// The caret and selection its focused app shows, or `None` when it has
    /// none.
    pub selection: Option<ShellSelection>,
    /// What the person says beside their pointer, or `None`.
    pub chat: Option<ShellChat>,
    /// Their latest reaction, or `None`.
    pub reaction: Option<ShellReaction>,
}

impl ShellPresence {
    /// The app and record of the window the person works in, if any: what
    /// the people panel names as where they are.
    #[must_use]
    pub fn focus(&self) -> Option<(&str, [u8; 8])> {
        self.windows
            .iter()
            .find(|window| window.focused)
            .map(|window| (window.app.as_str(), window.record))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view() -> ShellPresence {
        ShellPresence {
            seq: 7,
            sent_at_ms: 1_700_000_000_000,
            label: "Ada".into(),
            agent: false,
            colour: "#44aaff".into(),
            activity: None,
            place: Some("2".into()),
            size: [1440, 852],
            windows: vec![
                ShellWindow {
                    app: "so.tilo.notes".into(),
                    record: [7; 8],
                    rect: [0, 0, 5000, 5000],
                    focused: false,
                },
                ShellWindow {
                    app: "so.tilo.word-processor".into(),
                    record: [9; 8],
                    rect: [5000, 0, 10000, 10000],
                    focused: true,
                },
            ],
            selection: Some(ShellSelection {
                anchor: "0102".into(),
                head: "0102".into(),
            }),
            chat: Some(ShellChat {
                text: "here".into(),
                sent: false,
            }),
            reaction: Some(ShellReaction {
                seq: 3,
                emoji: "🎉".into(),
            }),
        }
    }

    #[test]
    fn the_focused_window_is_where_the_person_is() {
        assert_eq!(focus(&view()), Some(("so.tilo.word-processor", [9; 8])));
        let mut none = view();
        for window in &mut none.windows {
            window.focused = false;
        }
        assert_eq!(focus(&none), None);
    }

    fn focus(view: &ShellPresence) -> Option<(&str, [u8; 8])> {
        view.focus()
    }

    #[test]
    fn a_shell_view_round_trips_through_its_wire_encoding() {
        let bytes = postcard::to_allocvec(&view()).unwrap();
        assert_eq!(
            postcard::from_bytes::<ShellPresence>(&bytes).unwrap(),
            view()
        );
    }
}
