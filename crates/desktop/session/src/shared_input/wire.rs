//! What one of the person's devices sends the device it drives, as the live link carries it
//! ([`crate::live::OwnDevices`]): each input under the hand-over the driven device let in, and
//! what a drag carries across. A drag never carries a byte of content, only what names it in the
//! workspace.

use serde::{Deserialize, Serialize};

use crate::platform::fabric::DeviceId;

/// The longest a hand-over lasts before it is asked for again.
pub(crate) const MAX_INPUT_LEASE_MS: u64 = 15 * 60 * 1000;

/// The most formats a carried drag offers.
pub(crate) const MAX_CARRIED_OFFERS: usize = 8;

/// One message to the device driven, under the hand-over `lease_nonce` names, sent at `issued_ms`
/// (ms since the epoch).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Driven {
    pub(crate) lease_nonce: [u8; 16],
    pub(crate) issued_ms: u64,
    pub(crate) what: Drive,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum Drive {
    /// One input event.
    Input(RemoteInput),
    /// What the pointer carried across in the middle of a drag: the drag goes on there.
    Carried(CarriedDrag),
}

impl Driven {
    pub(crate) fn encode(&self) -> Vec<u8> {
        postcard::to_allocvec(self).expect("a message to a driven device encodes")
    }

    /// The message `body` says, when it is a well-formed one.
    pub(crate) fn decode(body: &[u8]) -> Option<Self> {
        let driven: Self = postcard::from_bytes(body).ok()?;
        let valid = driven.lease_nonce != [0; 16]
            && match &driven.what {
                Drive::Input(event) => event.validate(),
                Drive::Carried(drag) => drag.valid(),
            };
        valid.then_some(driven)
    }
}

/// A hand-over the driven device let in: `controller` drives `target` until `expires_unix_ms`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InputLease {
    pub(crate) controller: DeviceId,
    pub(crate) target: DeviceId,
    pub(crate) issued_unix_ms: u64,
    pub(crate) expires_unix_ms: u64,
    pub(crate) nonce: [u8; 16],
    pub(crate) allow_keyboard: bool,
    pub(crate) allow_pointer: bool,
}

impl InputLease {
    /// Whether this hand-over binds exactly this controller to this target device.
    pub(crate) fn binds(&self, controller: &DeviceId, target: &DeviceId) -> bool {
        &self.controller == controller && &self.target == target
    }

    /// Whether the hand-over holds at `now_unix_ms`: well formed, and not run out.
    pub(crate) fn holds_at(&self, now_unix_ms: u64) -> bool {
        self.controller != self.target
            && self.expires_unix_ms > self.issued_unix_ms
            && self.expires_unix_ms - self.issued_unix_ms <= MAX_INPUT_LEASE_MS
            && (self.allow_keyboard || self.allow_pointer)
            && now_unix_ms <= self.expires_unix_ms
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum RemoteInput {
    PointerMotion {
        x: i32,
        y: i32,
    },
    PointerButton {
        button: u32,
        pressed: bool,
    },
    /// Amounts in [`ScrollUnit::STEPS`] of `unit`, so a high-resolution wheel's part notches and a
    /// touchpad's sub-pixel motion add up as they do on the device they came from.
    Scroll {
        horizontal: i32,
        vertical: i32,
        unit: ScrollUnit,
    },
    Key {
        keycode: u32,
        pressed: bool,
        modifiers: u32,
    },
}

/// What a scroll moves by: wheel lines, or the pixels of finger and continuous scrolling.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum ScrollUnit {
    Lines,
    Pixels,
}

impl ScrollUnit {
    /// Steps a scroll amount counts per line or pixel.
    pub(crate) const STEPS: f64 = 120.0;
}

impl RemoteInput {
    pub(crate) fn validate(self) -> bool {
        match self {
            Self::PointerMotion { x, y } => {
                x.unsigned_abs() <= 1_000_000 && y.unsigned_abs() <= 1_000_000
            }
            Self::PointerButton { button, .. } => (1..=0x3ff).contains(&button),
            // A finger scroll ends with an empty frame (kinetic scrolling starts from it); an
            // empty wheel step means nothing.
            Self::Scroll {
                horizontal,
                vertical,
                unit,
            } => {
                horizontal.unsigned_abs() <= 1_000_000
                    && vertical.unsigned_abs() <= 1_000_000
                    && (unit == ScrollUnit::Pixels || horizontal != 0 || vertical != 0)
            }
            Self::Key {
                keycode, modifiers, ..
            } => keycode <= 0x2ff && modifiers <= 0xffff,
        }
    }
}

/// What a drag that crossed between the person's devices carries. No byte of content rides in
/// it: content is kept in the workspace and named, a resource is named by its address.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum CarriedDrag {
    /// A drag out of an app: each format's content, kept in `workspace` and named by its sealed
    /// reference.
    Content {
        workspace: String,
        offers: Vec<CarriedOffer>,
    },
    /// A resource the shell carries (a window's record, an item of a list): what its drag names it
    /// by, and what it shows under the pointer.
    Resource {
        kind: String,
        address: String,
        label: String,
    },
}

/// One format a carried drag offers, and the content reference its bytes are kept under.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CarriedOffer {
    pub(crate) mime: String,
    pub(crate) reference: String,
}

impl CarriedDrag {
    /// Whether every name it carries is well formed and bounded.
    pub(crate) fn valid(&self) -> bool {
        match self {
            Self::Content { workspace, offers } => {
                identifier(workspace, 64)
                    && !offers.is_empty()
                    && offers.len() <= MAX_CARRIED_OFFERS
                    && offers
                        .iter()
                        .all(|offer| mime(&offer.mime) && identifier(&offer.reference, 256))
            }
            Self::Resource {
                kind,
                address,
                label,
            } => {
                identifier(kind, 64)
                    && !address.is_empty()
                    && address.len() <= 1024
                    && !address.chars().any(char::is_control)
                    && label.len() <= 512
                    && !label.chars().any(char::is_control)
            }
        }
    }
}

fn identifier(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ':' | '/'))
}

fn mime(value: &str) -> bool {
    value.len() <= 128
        && value.contains('/')
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '-' | '+' | '_'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_crosses_to_a_driven_device_is_named_and_bounded_and_carries_no_bytes() {
        let content = |workspace: &str, offers: Vec<CarriedOffer>| {
            CarriedDrag::Content {
                workspace: workspace.into(),
                offers,
            }
            .valid()
        };
        let text = CarriedOffer {
            mime: "text/plain".into(),
            reference: format!("{}.{}", "a".repeat(64), "B".repeat(43)),
        };
        assert!(content("ws-1", vec![text.clone()]));
        assert!(!content("ws-1", Vec::new()), "a drag carries something");
        assert!(
            !content("ws-1", vec![text.clone(); MAX_CARRIED_OFFERS + 1]),
            "a drag carries a few formats"
        );
        let words = CarriedOffer {
            mime: "text/plain".into(),
            reference: "hello world".into(),
        };
        assert!(!content("ws-1", vec![words]), "a reference, never the text");
        assert!(!content("", vec![text]), "kept in a workspace");
        let resource = |address: &str| {
            CarriedDrag::Resource {
                kind: "resource".into(),
                address: address.into(),
                label: "Notes".into(),
            }
            .valid()
        };
        assert!(resource("notes.page:one"));
        assert!(!resource(""), "a resource is named");

        let message = |lease_nonce: [u8; 16], what: Drive| {
            Driven {
                lease_nonce,
                issued_ms: 7,
                what,
            }
            .encode()
        };
        let step = Drive::Input(RemoteInput::PointerMotion { x: 3, y: -4 });
        assert_eq!(
            Driven::decode(&message([1; 16], step.clone())).map(|driven| driven.what),
            Some(step.clone())
        );
        assert_eq!(
            Driven::decode(&message([0; 16], step)),
            None,
            "every message names the hand-over it is sent under"
        );
        let far = Drive::Input(RemoteInput::PointerMotion { x: 2_000_000, y: 0 });
        assert_eq!(Driven::decode(&message([1; 16], far)), None);
        let empty = Drive::Input(RemoteInput::Scroll {
            horizontal: 0,
            vertical: 0,
            unit: ScrollUnit::Lines,
        });
        assert_eq!(Driven::decode(&message([1; 16], empty)), None);
        assert_eq!(Driven::decode(b"\xff\xff"), None);
    }
}
