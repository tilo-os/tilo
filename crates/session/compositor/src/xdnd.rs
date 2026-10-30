//! The XDND protocol (version 5) as plain data: its client messages, the
//! version both sides speak, the actions and the names formats go by.
//!
//! X11 clients drag and drop with XDND among themselves; the X11 window
//! manager does not take part. To let a drag cross between X11 and Wayland
//! windows (and hosted surface apps) the compositor speaks it too, through
//! [`crate::xdnd_bridge`]: as the target, with a window it raises over
//! everything that is not an X11 window while an X11 drag runs, and as the
//! source when a Wayland or hosted drag enters an X11 window. What travels
//! is encoded and decoded here, free of any connection.

use smithay::reexports::wayland_server::protocol::wl_data_device_manager::DndAction;

use crate::selection::{TEXT_MIME, is_text_mime, normalized_mimes};

/// The newest XDND version spoken.
pub(crate) const VERSION: u32 = 5;
/// The oldest XDND version spoken: older ones lack the action field.
pub(crate) const MIN_VERSION: u32 = 3;

/// The version a drag with a peer that speaks `theirs` uses, if any.
pub(crate) fn negotiate(theirs: u32) -> Option<u32> {
    (theirs >= MIN_VERSION).then(|| theirs.min(VERSION))
}

/// The atoms XDND's messages and actions are named by, interned on the
/// connection the bridge holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct XdndAtoms {
    pub(crate) enter: u32,
    pub(crate) position: u32,
    pub(crate) status: u32,
    pub(crate) leave: u32,
    pub(crate) drop: u32,
    pub(crate) finished: u32,
    pub(crate) action_copy: u32,
    pub(crate) action_move: u32,
    pub(crate) action_ask: u32,
    pub(crate) action_private: u32,
}

/// A drop's action, as both protocols know it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action {
    /// Nothing happens (a refused drop).
    None,
    /// The target gets a copy.
    Copy,
    /// The target takes it; the source deletes its own.
    Move,
    /// The target asks the person which.
    Ask,
}

impl Action {
    /// The atom naming this action (`None` for no action).
    pub(crate) const fn atom(self, atoms: &XdndAtoms) -> u32 {
        match self {
            Self::None => 0,
            Self::Copy => atoms.action_copy,
            Self::Move => atoms.action_move,
            Self::Ask => atoms.action_ask,
        }
    }

    /// The action an atom names; a private action is a copy to us, an
    /// unknown one no action.
    pub(crate) fn of_atom(atom: u32, atoms: &XdndAtoms) -> Self {
        if atom == 0 {
            Self::None
        } else if atom == atoms.action_copy || atom == atoms.action_private {
            Self::Copy
        } else if atom == atoms.action_move {
            Self::Move
        } else if atom == atoms.action_ask {
            Self::Ask
        } else {
            Self::None
        }
    }

    /// The Wayland action.
    pub(crate) const fn dnd(self) -> DndAction {
        match self {
            Self::None => DndAction::None,
            Self::Copy => DndAction::Copy,
            Self::Move => DndAction::Move,
            Self::Ask => DndAction::Ask,
        }
    }

    /// One action out of a set Wayland offers: copy when allowed, as the
    /// safe default, then move, then ask.
    pub(crate) fn preferred(actions: DndAction) -> Self {
        if actions.contains(DndAction::Copy) {
            Self::Copy
        } else if actions.contains(DndAction::Move) {
            Self::Move
        } else if actions.contains(DndAction::Ask) {
            Self::Ask
        } else {
            Self::None
        }
    }
}

/// One XDND client message (format 32).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Message {
    /// The source's drag entered the target; up to three types inline,
    /// `more_types` when the rest are on the source's `XdndTypeList`.
    Enter {
        source: u32,
        version: u32,
        more_types: bool,
        types: Vec<u32>,
    },
    /// The pointer is at this root position, asking for `action`.
    Position {
        source: u32,
        x: i16,
        y: i16,
        time: u32,
        action: u32,
    },
    /// The target's answer to a position: whether it would take a drop
    /// there, and with which action.
    Status {
        target: u32,
        accept: bool,
        action: u32,
    },
    /// The drag left the target.
    Leave { source: u32 },
    /// The button was released over the target.
    Drop { source: u32, time: u32 },
    /// The target is done with the drop.
    Finished {
        target: u32,
        success: bool,
        action: u32,
    },
}

impl Message {
    /// The message's type atom and its five 32-bit words.
    pub(crate) fn encode(&self, atoms: &XdndAtoms) -> (u32, [u32; 5]) {
        match self {
            Self::Enter {
                source,
                version,
                more_types,
                types,
            } => {
                let mut words = [*source, (version << 24) | u32::from(*more_types), 0, 0, 0];
                for (word, ty) in words.iter_mut().skip(2).zip(types.iter().take(3)) {
                    *word = *ty;
                }
                (atoms.enter, words)
            }
            Self::Position {
                source,
                x,
                y,
                time,
                action,
            } => (
                atoms.position,
                [*source, 0, pack_point(*x, *y), *time, *action],
            ),
            Self::Status {
                target,
                accept,
                action,
            } => {
                // Bit 1 asks for every position, not only ones that leave
                // an empty rectangle: the target behind the proxy changes
                // anywhere.
                let flags = u32::from(*accept) | 0b10;
                (atoms.status, [*target, flags, 0, 0, *action])
            }
            Self::Leave { source } => (atoms.leave, [*source, 0, 0, 0, 0]),
            Self::Drop { source, time } => (atoms.drop, [*source, 0, *time, 0, 0]),
            Self::Finished {
                target,
                success,
                action,
            } => (
                atoms.finished,
                [*target, u32::from(*success), *action, 0, 0],
            ),
        }
    }

    /// The message a client message of type `kind` carries, if it is XDND.
    pub(crate) fn decode(kind: u32, words: [u32; 5], atoms: &XdndAtoms) -> Option<Self> {
        let [first, second, third, fourth, fifth] = words;
        let message = if kind == atoms.enter {
            Self::Enter {
                source: first,
                version: second >> 24,
                more_types: second & 1 != 0,
                types: [third, fourth, fifth]
                    .into_iter()
                    .filter(|ty| *ty != 0)
                    .collect(),
            }
        } else if kind == atoms.position {
            let (x, y) = unpack_point(third);
            Self::Position {
                source: first,
                x,
                y,
                time: fourth,
                action: fifth,
            }
        } else if kind == atoms.status {
            Self::Status {
                target: first,
                accept: second & 1 != 0,
                action: fifth,
            }
        } else if kind == atoms.leave {
            Self::Leave { source: first }
        } else if kind == atoms.drop {
            Self::Drop {
                source: first,
                time: third,
            }
        } else if kind == atoms.finished {
            Self::Finished {
                target: first,
                success: second & 1 != 0,
                action: third,
            }
        } else {
            return None;
        };
        Some(message)
    }
}

/// A root position as XDND packs it: x in the high half, y in the low.
#[allow(
    clippy::cast_sign_loss,
    reason = "the halves carry the two's-complement bits of each coordinate"
)]
pub(crate) const fn pack_point(x: i16, y: i16) -> u32 {
    ((x as u16 as u32) << 16) | (y as u16 as u32)
}

/// The root position a packed word carries.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    reason = "each half is one coordinate's two's-complement bits"
)]
pub(crate) const fn unpack_point(word: u32) -> (i16, i16) {
    ((word >> 16) as u16 as i16, word as u16 as i16)
}

/// A logical coordinate as an X11 root coordinate.
#[allow(
    clippy::cast_possible_truncation,
    reason = "clamped to the i16 range first"
)]
pub(crate) fn root_coordinate(value: f64) -> i16 {
    value
        .round()
        .clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16
}

/// Targets that describe a selection rather than name a format.
const META_TARGETS: [&str; 7] = [
    "TARGETS",
    "MULTIPLE",
    "TIMESTAMP",
    "DELETE",
    "SAVE_TARGETS",
    "INCR",
    "XdndDirectSave0",
];

/// The formats an X11 drag's types are offered as to Wayland clients and
/// hosted apps: format names pass through as they are (`text/uri-list`
/// above all), text also under its Wayland names.
pub(crate) fn mimes_of_targets(targets: &[String]) -> Vec<String> {
    normalized_mimes(
        targets
            .iter()
            .filter(|target| !META_TARGETS.contains(&target.as_str()))
            .cloned(),
    )
}

/// The types a Wayland or hosted drag's formats are offered as to an X11
/// target: the same names, text also under its X11 names.
pub(crate) fn targets_of_mimes(mimes: &[String]) -> Vec<String> {
    normalized_mimes(mimes.iter().cloned())
}

/// Text's names, best first: UTF-8 under either protocol's name, then
/// the ambiguous ones.
const TEXT_PREFERENCE: [&str; 5] = [TEXT_MIME, "UTF8_STRING", "text/plain", "STRING", "TEXT"];

/// Which of `available` serves a request for `wanted`: the same name, or
/// for text whichever text name is there, UTF-8 first.
pub(crate) fn pick_format(wanted: &str, available: &[String]) -> Option<String> {
    if available.iter().any(|name| name == wanted) {
        return Some(wanted.to_owned());
    }
    if !is_text_mime(wanted) {
        return None;
    }
    TEXT_PREFERENCE
        .iter()
        .find(|name| available.iter().any(|offered| offered == *name))
        .map(|name| (*name).to_owned())
}

/// Files dragged out of a sandbox through the document portal: a key the
/// receiving app redeems with the portal. It passes through as any format.
pub(crate) const PORTAL_FILES_MIME: &str = "application/vnd.portal.filetransfer";

/// Formats read ahead from a Wayland drag dropped on an X11 window, best
/// first, at most four: the source is told the drop is over as the button
/// comes up, so what the X11 target will ask for must be read before.
pub(crate) fn read_ahead(mimes: &[String]) -> Vec<String> {
    let mut chosen: Vec<String> = Vec::new();
    for wanted in ["text/uri-list", PORTAL_FILES_MIME, TEXT_MIME] {
        if let Some(found) = pick_format(wanted, mimes)
            && !chosen.contains(&found)
        {
            chosen.push(found);
        }
    }
    if let Some(other) = mimes
        .iter()
        .find(|mime| !is_text_mime(mime) && !chosen.contains(mime))
    {
        chosen.push(other.clone());
    }
    chosen.truncate(4);
    chosen
}

#[cfg(test)]
mod tests {
    use super::*;

    const ATOMS: XdndAtoms = XdndAtoms {
        enter: 101,
        position: 102,
        status: 103,
        leave: 104,
        drop: 105,
        finished: 106,
        action_copy: 201,
        action_move: 202,
        action_ask: 203,
        action_private: 204,
    };

    fn strings(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn every_message_survives_encoding() {
        let messages = [
            Message::Enter {
                source: 7,
                version: 5,
                more_types: false,
                types: vec![11, 12],
            },
            Message::Enter {
                source: 7,
                version: 4,
                more_types: true,
                types: vec![11, 12, 13],
            },
            Message::Position {
                source: 7,
                x: -20,
                y: 300,
                time: 99,
                action: ATOMS.action_move,
            },
            Message::Status {
                target: 8,
                accept: true,
                action: ATOMS.action_copy,
            },
            Message::Status {
                target: 8,
                accept: false,
                action: 0,
            },
            Message::Leave { source: 7 },
            Message::Drop {
                source: 7,
                time: 1234,
            },
            Message::Finished {
                target: 8,
                success: true,
                action: ATOMS.action_copy,
            },
        ];
        for message in messages {
            let (kind, words) = message.encode(&ATOMS);
            assert_eq!(Message::decode(kind, words, &ATOMS), Some(message));
        }
        assert_eq!(Message::decode(999, [0; 5], &ATOMS), None);
    }

    #[test]
    fn messages_have_the_layout_the_specification_gives() {
        let (kind, words) = Message::Enter {
            source: 0x0040_0001,
            version: 5,
            more_types: true,
            types: vec![1, 2, 3, 4],
        }
        .encode(&ATOMS);
        assert_eq!(kind, ATOMS.enter);
        assert_eq!(words, [0x0040_0001, 0x0500_0001, 1, 2, 3]);

        let (_, words) = Message::Position {
            source: 1,
            x: 10,
            y: 20,
            time: 5,
            action: ATOMS.action_copy,
        }
        .encode(&ATOMS);
        assert_eq!(words, [1, 0, (0xA << 16) | 0x14, 5, ATOMS.action_copy]);

        let (_, words) = Message::Status {
            target: 2,
            accept: true,
            action: ATOMS.action_copy,
        }
        .encode(&ATOMS);
        assert_eq!(words[1] & 1, 1, "accept bit");
        assert_eq!(words[4], ATOMS.action_copy);

        let (_, words) = Message::Drop {
            source: 3,
            time: 77,
        }
        .encode(&ATOMS);
        assert_eq!(words, [3, 0, 77, 0, 0]);
    }

    #[test]
    fn negative_root_positions_pack_and_unpack() {
        assert_eq!(unpack_point(pack_point(-1, -32768)), (-1, -32768));
        assert_eq!(unpack_point(pack_point(1919, 1079)), (1919, 1079));
        assert_eq!(root_coordinate(1e9), i16::MAX);
        assert_eq!(root_coordinate(-3.6), -4);
    }

    #[test]
    fn versions_below_three_are_refused_and_newer_ones_capped() {
        assert_eq!(negotiate(2), None);
        assert_eq!(negotiate(3), Some(3));
        assert_eq!(negotiate(5), Some(5));
        assert_eq!(negotiate(7), Some(VERSION));
    }

    #[test]
    fn actions_map_both_ways() {
        for action in [Action::Copy, Action::Move, Action::Ask] {
            assert_eq!(Action::of_atom(action.atom(&ATOMS), &ATOMS), action);
        }
        assert_eq!(Action::of_atom(ATOMS.action_private, &ATOMS), Action::Copy);
        assert_eq!(Action::of_atom(0, &ATOMS), Action::None);
        assert_eq!(Action::of_atom(12345, &ATOMS), Action::None);
        assert_eq!(Action::Move.dnd(), DndAction::Move);
        assert_eq!(
            Action::preferred(DndAction::Move | DndAction::Copy),
            Action::Copy
        );
        assert_eq!(
            Action::preferred(DndAction::Move | DndAction::Ask),
            Action::Move
        );
        assert_eq!(Action::preferred(DndAction::Ask), Action::Ask);
        assert_eq!(Action::preferred(DndAction::empty()), Action::None);
    }

    #[test]
    fn file_lists_keep_their_name_and_text_gains_every_name() {
        let mimes = mimes_of_targets(&strings(&[
            "TARGETS",
            "text/uri-list",
            "UTF8_STRING",
            "application/vnd.portal.filetransfer",
            "TIMESTAMP",
        ]));
        assert!(mimes.contains(&"text/uri-list".to_owned()));
        assert!(mimes.contains(&PORTAL_FILES_MIME.to_owned()));
        assert!(mimes.contains(&TEXT_MIME.to_owned()));
        assert!(
            !mimes
                .iter()
                .any(|mime| mime == "TARGETS" || mime == "TIMESTAMP")
        );

        let targets = targets_of_mimes(&strings(&["text/uri-list", "text/plain"]));
        assert!(targets.contains(&"text/uri-list".to_owned()));
        assert!(targets.contains(&"UTF8_STRING".to_owned()));
        assert!(targets.contains(&"STRING".to_owned()));
    }

    #[test]
    fn requests_are_served_by_the_same_name_or_any_text_name() {
        let offered = strings(&["text/uri-list", "UTF8_STRING", "STRING"]);
        assert_eq!(
            pick_format("text/uri-list", &offered).as_deref(),
            Some("text/uri-list")
        );
        assert_eq!(
            pick_format(TEXT_MIME, &offered).as_deref(),
            Some("UTF8_STRING")
        );
        assert_eq!(pick_format("image/png", &offered), None);
        let wayland = strings(&["text/plain", TEXT_MIME]);
        assert_eq!(
            pick_format("UTF8_STRING", &wayland).as_deref(),
            Some(TEXT_MIME)
        );
    }

    #[test]
    fn file_lists_are_read_ahead_first() {
        let mimes = strings(&[
            TEXT_MIME,
            "text/plain",
            "image/png",
            "text/uri-list",
            "application/x-other",
        ]);
        assert_eq!(
            read_ahead(&mimes),
            strings(&["text/uri-list", TEXT_MIME, "image/png"])
        );
        assert_eq!(read_ahead(&[]), Vec::<String>::new());
    }
}
