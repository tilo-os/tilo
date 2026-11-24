//! The tilo-share/1 object profile: what a viewer of a shared window and the
//! device sharing it say to each other on the workspace's live session.
//!
//! A shared window's pictures and its page travel tracks of their own
//! ([`crate::video`], [`crate::page`]), and the live session is their demand: a
//! window is encoded only while a member's live session asks for its track.
//! What a track cannot say travels here: which windows a viewer shows and what
//! it decodes, the viewer's input and its ask for a keyframe, a window's page
//! announced by its host, and the end of a share.
//!
//! # Track
//!
//! One track per device, [`crate::live::Track::Share`], published by every
//! live session and read of every device it hears, like presence. A burst of
//! objects is one group.
//!
//! # Objects
//!
//! Each object is addressed to one device, with this header before the sealed
//! frame:
//!
//! | Bytes | Field |
//! | --- | --- |
//! | 1 | version, `1` |
//! | 1 | [`Keyed`]: whose share key the frame is sealed under |
//! | 32 | the device it is addressed to |
//!
//! The sealed frame's body is one postcard [`Said`], which names the device
//! again: a reader opens only what is addressed to it, and drops an object
//! whose body names someone else.
//!
//! An object is sealed under the share key of the device whose window it is
//! about: what a viewer says to the host under the host's key, what a host
//! says under its own. Every member derives any device's key, and a device
//! confined to its own keys (a workspace's VM, lent the keys of its own
//! streams) still opens what its viewers say to it.

use serde::{Deserialize, Serialize};

use crate::frame::{LiveKeys, Refusal, Sealer, Stream, TrackName};
use crate::roster::Roster;
use crate::scope::Scope;

/// Bytes of header before the sealed frame.
pub const HEADER_BYTES: usize = 34;
/// The largest object: a burst's object is one control, a pasted text at most.
pub const MAX_OBJECT_BYTES: usize = 64 * 1024;
/// Windows one viewer says it shows of one host at most.
pub const MAX_VIEWS: usize = 64;
/// Hardware decoders one viewer names at most.
pub const MAX_DECODERS: usize = 8;
/// The longest side a viewer may ask a window at.
pub const MAX_VIEW_SIDE: u32 = 8192;
/// The name the share track is sealed under.
const TRACK: &str = "share";

/// One window a viewer shows: the workspace tab holding it (the same id on
/// every device of the workspace) and the tile's size in logical pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct View {
    pub tab: u64,
    pub width: u32,
    pub height: u32,
}

/// The largest picture a viewer's hardware decoder for `codec` (a codec
/// label, `"h264"`, `"vp9"` …) takes, where it has a limit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecodeLimit {
    pub codec: String,
    pub max: Option<(u32, u32)>,
}

/// A viewer's input into another device's window: the events a person's
/// pointer and keys make on the window's picture, which the device hosting the
/// window replays on a seat of its own.
///
/// `PointerEnter`/`PointerLeave` scope the viewer's seat; keyboard and text
/// input focus stay on the window's tab, so its client sees one stable seat.
/// Two viewers typing at once interleave in event order, and held keys are
/// counted per viewer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum InputEvent {
    /// Pointer moved to logical `(x, y)`, surface-local; the compositor's input firewall translates it into the owner's output space.
    PointerMotion { x: f64, y: f64 },
    /// Mouse button transitioned to `pressed`; `button` is a libinput-style code (272 = `BTN_LEFT`).
    PointerButton { button: u32, pressed: bool },
    /// The viewer's pointer came into the window of `surface`.
    PointerEnter { surface: u64 },
    /// The viewer's pointer left the window of `surface`. Keyboard focus
    /// stays: moving a mouse out of a text field keeps its caret.
    PointerLeave { surface: u64 },
    /// Key press or release. `keysym` is a Linux evdev keycode (30 = A),
    /// translated to XKB space by the host. `modifiers` is an untrusted
    /// xkb-style bitfield; held modifiers are rebuilt from each viewer's key
    /// transitions.
    Key {
        keysym: u32,
        pressed: bool,
        modifiers: u32,
    },
    /// Keyboard focus moved, by a click rather than by hover.
    KeyboardFocus { surface: u64, focused: bool },
    /// The wheel or a trackpad scrolled by `(dx, dy)` surface pixels, at the
    /// viewer's last pointer position.
    PointerAxis { dx: f64, dy: f64 },
    /// The viewer pasted `text`: the host offers it as the clipboard, so the
    /// window's own paste (a key chord that follows) reads it.
    ClipboardText { text: String },
    /// The viewer closed its view of the window: the window is asked to
    /// close, as a person's close of its tab does.
    Close,
    /// Text the viewer's input method committed (a composed character, a
    /// dictated phrase, an emoji), delivered as an input method delivers it.
    Text { text: String },
}

/// What one side of a shared window says to the other.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Control {
    /// A viewer shows `views` of the host and decodes what `hardware` and
    /// `software` name; said again while it shows them, and a host lets a
    /// viewer it stops hearing from go.
    View {
        views: Vec<View>,
        /// The codecs its hardware decodes, each with the largest picture it takes.
        hardware: Vec<DecodeLimit>,
        /// The codec labels it decodes in software.
        software: Vec<String>,
    },
    /// The viewer's input into the window of `tab`.
    Input { tab: u64, event: InputEvent },
    /// The viewer lost a picture of the window of `tab` after `seq`: the host
    /// sends a keyframe.
    Keyframe { tab: u64, seq: u64 },
    /// The window of `tab` travels as its page ([`crate::page`]), not as
    /// pictures: what a viewer sizes its tile by before the page's first frame.
    Page {
        tab: u64,
        app_id: String,
        width: u32,
        height: u32,
    },
    /// The host stopped sharing the window of `tab`.
    Ended { tab: u64 },
}

impl Control {
    /// Whether a viewer says this to the host of a window, rather than the
    /// host to a viewer: what it is sealed under ([`Keyed`]).
    #[must_use]
    pub const fn to_host(&self) -> bool {
        matches!(
            self,
            Self::View { .. } | Self::Input { .. } | Self::Keyframe { .. }
        )
    }

    /// Whether this control is within what the profile carries: no more
    /// windows, decoders or pixels than a viewer may ask for.
    #[must_use]
    pub fn within_bounds(&self) -> bool {
        match self {
            Self::View {
                views,
                hardware,
                software,
            } => {
                views.len() <= MAX_VIEWS
                    && hardware.len() <= MAX_DECODERS
                    && software.len() <= MAX_DECODERS
                    && views
                        .iter()
                        .all(|view| view.width <= MAX_VIEW_SIDE && view.height <= MAX_VIEW_SIDE)
            }
            Self::Page { width, height, .. } => *width <= MAX_VIEW_SIDE && *height <= MAX_VIEW_SIDE,
            Self::Input { .. } | Self::Keyframe { .. } | Self::Ended { .. } => true,
        }
    }
}

/// One control, and the device it is for.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Said {
    pub to: [u8; 32],
    pub control: Control,
}

impl Said {
    /// The postcard bytes a sealed frame carries.
    ///
    /// # Errors
    ///
    /// [`Refusal::Primitive`] when it cannot be encoded.
    pub fn encode(&self) -> Result<Vec<u8>, Refusal> {
        postcard::to_stdvec(self).map_err(|_| Refusal::Primitive)
    }

    /// The control a heard share object's body carries ([`crate::live_session::Heard`]),
    /// or `None` for bytes that are not one.
    #[must_use]
    pub fn decode(body: &[u8]) -> Option<Self> {
        postcard::from_bytes(body).ok()
    }
}

/// Whose share key an object is sealed under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Keyed {
    /// The device it is addressed to: a viewer to the host of a window.
    Recipient = 0,
    /// The device that said it: a host to a viewer.
    Sender = 1,
}

/// `said` as object `seq` of `sender`'s share track in `scope`.
///
/// # Errors
///
/// [`Refusal::NoKey`] when `keys` do not hold the key it is sealed under: a
/// confined device says nothing to a host, it only hosts. [`Refusal::Primitive`]
/// for a control outside the profile's bounds.
pub fn seal(
    keys: &LiveKeys,
    scope: &Scope,
    sender: [u8; 32],
    seq: u32,
    said: &Said,
) -> Result<Vec<u8>, Refusal> {
    if !said.control.within_bounds() {
        return Err(Refusal::Primitive);
    }
    let keyed = if said.control.to_host() {
        Keyed::Recipient
    } else {
        Keyed::Sender
    };
    let under = match keyed {
        Keyed::Recipient => said.to,
        Keyed::Sender => sender,
    };
    let sealer = Sealer::new(keys, under, Stream::Share, TrackName::new(scope, TRACK))?;
    let frame = sealer.seal(seq, &said.encode()?)?;
    let mut object = Vec::with_capacity(HEADER_BYTES.saturating_add(frame.len()));
    object.push(1);
    object.push(keyed as u8);
    object.extend_from_slice(&said.to);
    object.extend_from_slice(&frame);
    if object.len() > MAX_OBJECT_BYTES {
        return Err(Refusal::Primitive);
    }
    Ok(object)
}

/// The body of `object`, which `sender` published on its share track, when it
/// is addressed to `own`: `Ok(None)` for an object addressed to another
/// device, which is not this device's to open.
///
/// # Errors
///
/// [`Refusal::OffRoster`] for a sender the roster does not name,
/// [`Refusal::Truncated`] for bytes that are not an object, and whatever
/// opening the frame answers.
pub fn open(
    (keys, roster): (&LiveKeys, &Roster),
    scope: &Scope,
    own: [u8; 32],
    sender: [u8; 32],
    object: &[u8],
) -> Result<Option<Vec<u8>>, Refusal> {
    if !roster.admits(sender) {
        return Err(Refusal::OffRoster);
    }
    if object.len() > MAX_OBJECT_BYTES {
        return Err(Refusal::Truncated);
    }
    let (&[version, keyed], rest) = object.split_first_chunk::<2>().ok_or(Refusal::Truncated)?;
    let (to, frame) = rest.split_first_chunk::<32>().ok_or(Refusal::Truncated)?;
    if version != 1 {
        return Err(Refusal::Unauthenticated);
    }
    if *to != own {
        return Ok(None);
    }
    let under = match keyed {
        0 => own,
        1 => sender,
        _ => return Err(Refusal::Unauthenticated),
    };
    let body = crate::frame::open_as(
        keys,
        under,
        Stream::Share,
        &TrackName::new(scope, TRACK),
        frame,
    )?;
    // The body names whom it is for under the seal; the header only lets the others pass it by.
    let said = Said::decode(&body).ok_or(Refusal::Unauthenticated)?;
    if said.to != own || said.control.to_host() != (under == own) || !said.control.within_bounds()
    {
        return Err(Refusal::Unauthenticated);
    }
    Ok(Some(body))
}

#[cfg(test)]
mod tests {
    use key_group::{KeyGroup, MemberKey};

    use super::*;

    const WORKSPACE: &str = "0f8fad5b-d9cb-469f-a165-70867728950e";
    const HOST: [u8; 32] = [1; 32];
    const VIEWER: [u8; 32] = [2; 32];
    const OTHER: [u8; 32] = [3; 32];

    fn keys() -> key_group::ContentKeys {
        let workspace = collab_types::WorkspaceId::parse(WORKSPACE).unwrap();
        KeyGroup::create(workspace, MemberKey::generate("owner").unwrap())
            .unwrap()
            .content_keys()
            .unwrap()
    }

    fn roster() -> Roster {
        Roster::new([HOST, VIEWER, OTHER])
    }

    /// The workspace's keys, its scope, a viewer's key press for the host and the object it is.
    fn pressed() -> (key_group::ContentKeys, Scope, LiveKeys, Said, Vec<u8>) {
        let content = keys();
        let scope = Scope::workspace(WORKSPACE);
        let channel = LiveKeys::from(content.clone());
        let input = Said {
            to: HOST,
            control: Control::Input {
                tab: 7,
                event: InputEvent::Key {
                    keysym: 30,
                    pressed: true,
                    modifiers: 0,
                },
            },
        };
        let object = seal(&channel, &scope, VIEWER, 1, &input).unwrap();
        (content, scope, channel, input, object)
    }

    tilo_test_env::scenario! {
        fn a_share_object_opens_only_for_the_device_it_is_addressed_to;

        phase a_viewers_input_opens_at_the_host_and_nowhere_else;
        {
            let (_, scope, channel, input, object) = pressed();
            let opened = open((&channel, &roster()), &scope, HOST, VIEWER, &object).unwrap();
            assert_eq!(opened.as_deref().and_then(Said::decode), Some(input));
            assert_eq!(
                open((&channel, &roster()), &scope, OTHER, VIEWER, &object).unwrap(),
                None,
                "another member passes it by"
            );
        }

        phase a_host_confined_to_its_own_keys_still_hears_its_viewers;
        {
            let (content, scope, channel, input, object) = pressed();
            let lent = content
                .sender_media_keys(content.epoch(), HOST, &[Stream::Share.tag()])
                .unwrap();
            let confined = LiveKeys::from(lent);
            let opened = open((&confined, &roster()), &scope, HOST, VIEWER, &object).unwrap();
            assert_eq!(opened.as_deref().and_then(Said::decode), Some(input));
            // What it says to a viewer is sealed under its own key, which the viewer derives.
            let ended = Said {
                to: VIEWER,
                control: Control::Ended { tab: 7 },
            };
            let object = seal(&confined, &scope, HOST, 2, &ended).unwrap();
            let opened = open((&channel, &roster()), &scope, VIEWER, HOST, &object).unwrap();
            assert_eq!(opened.as_deref().and_then(Said::decode), Some(ended));
        }

        phase a_sender_off_the_roster_or_a_changed_byte_is_refused;
        {
            let (_, scope, channel, _, object) = pressed();
            let outsider = Roster::new([HOST]);
            assert_eq!(
                open((&channel, &outsider), &scope, HOST, VIEWER, &object),
                Err(Refusal::OffRoster)
            );
            let mut changed = object.clone();
            let last = changed.len() - 1;
            changed[last] ^= 1;
            assert_eq!(
                open((&channel, &roster()), &scope, HOST, VIEWER, &changed),
                Err(Refusal::Unauthenticated)
            );
        }

        phase a_view_beyond_the_bounds_is_never_sealed;
        {
            let (_, scope, channel, _, _) = pressed();
            let view = Said {
                to: HOST,
                control: Control::View {
                    views: vec![View {
                        tab: 7,
                        width: MAX_VIEW_SIDE + 1,
                        height: 10,
                    }],
                    hardware: Vec::new(),
                    software: Vec::new(),
                },
            };
            assert_eq!(
                seal(&channel, &scope, VIEWER, 3, &view),
                Err(Refusal::Primitive)
            );
        }
    }
}
