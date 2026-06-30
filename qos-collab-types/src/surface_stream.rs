//! Live remote-surface streaming for hosting legacy Wayland apps in a
//! collaborative workspace.
//!
//! ## Why this exists
//!
//! Most artifacts in a channel are CRDT documents: every peer holds the
//! same Automerge tree and converges by exchanging incremental ops. That
//! model breaks for "I want to share my running Inkscape window with the
//! channel" — Inkscape doesn't speak Automerge. The workspace host runs
//! the legacy app locally (often inside a Flatpak sandbox), captures the
//! Wayland surfaces, encodes them to a video stream, and pushes the
//! pixels to every other workspace member. Remote members see a live
//! feed and can post pointer + keyboard events back; the host injects
//! them into the running app.
//!
//! Unlike CRDT ops, surface frames are **lossy + ordered**: a dropped
//! intermediate frame is fine, but reordering is not. They flow through
//! the same [`crate::op::SignedOp`] envelope so attestation, role
//! enforcement, and replay defence (the per-author `seq`) cover them
//! uniformly, but the payload is opaque video bytes, not Automerge.
//!
//! ## Payload mapping
//!
//! The four ops introduced here pair 1:1 with [`crate::role::OpClass`]
//! variants:
//!
//! | OpClass                      | Payload type                                    |
//! |------------------------------|--------------------------------------------------|
//! | [`SurfaceStreamStart`][a]    | [`SurfaceStreamStart`]                           |
//! | [`SurfaceStreamEnd`][b]      | [`SurfaceStreamEnd`]                             |
//! | [`SurfaceFrame`][c]          | [`SurfaceFrame`]                                 |
//! | [`RemoteInput`][d]           | [`RemoteInput`]                                  |
//!
//! [a]: crate::role::OpClass::SurfaceStreamStart
//! [b]: crate::role::OpClass::SurfaceStreamEnd
//! [c]: crate::role::OpClass::SurfaceFrame
//! [d]: crate::role::OpClass::RemoteInput
//!
//! Each is encoded with [`crate::canonical::canonical_bytes`] before
//! being stamped into the `SignedOp::payload` field.
//!
//! ## SurfaceId scope
//!
//! [`SurfaceId`] is a per-channel stable `u64` minted by the host.
//! Stability is the host's responsibility: the same window keeps the
//! same id across frames until a `SurfaceStreamEnd` retires it. The id
//! is opaque from the validator's perspective — nothing in this crate
//! resolves it to a window.
//!
//! ## Why mirror wayland enums in-crate
//!
//! The pointer + keyboard variants on [`RemoteInputEvent`] mirror the
//! wayland-protocols `wl_pointer.button_state`, `wl_keyboard.key_state`,
//! and the standard pointer-axis pair, but we do **not** depend on
//! `wayland-server`. This crate is the foundation library — pulling in
//! the compositor's transitive deps would re-introduce the X11/Wayland
//! coupling Phase 2 was specifically designed to avoid. The mirrored
//! enums fit in 1-2 bytes apiece and the translation to the live wire
//! enum lives in the compositor.

use serde::{Deserialize, Serialize};

/// Stable per-channel identifier for one streamed surface.
///
/// Minted by the workspace host when the user picks a window to share;
/// retired when [`SurfaceStreamEnd`] is emitted for it. Two surfaces
/// streamed concurrently on the same channel must hold distinct ids.
/// Reusing an id after a `SurfaceStreamEnd` is allowed but discouraged
/// (it confuses scrubback through the channel's op log).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SurfaceId(pub u64);

impl SurfaceId {
    /// Construct directly from a raw u64. The host's id minting policy
    /// is opaque to this crate; tests and the compositor both build
    /// `SurfaceId`s from concrete values they own.
    pub const fn from_u64(raw: u64) -> Self {
        Self(raw)
    }

    /// Raw underlying value, for cross-boundary handoff to systems
    /// that don't speak qOS types (`wayland-server`, logging, etc.).
    pub const fn as_u64(self) -> u64 {
        self.0
    }
}

/// Codec used by the host to encode video frames for one stream.
///
/// The host declares the codec once in [`SurfaceStreamStart`]; every
/// subsequent [`SurfaceFrame`] for that surface is encoded with the
/// same codec until a `SurfaceStreamEnd` arrives. Switching codecs
/// mid-stream requires emitting a new `SurfaceStreamStart` (which
/// implicitly retires the old surface state on every peer).
///
/// Variants intentionally cover the three codecs the compositor's
/// pipeline already negotiates with `pipewire` / `gstreamer`. Adding a
/// fourth (e.g. AV2) is a wire bump.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum VideoCodec {
    /// H.264 / AVC. Universal hardware support, the safe baseline. Use
    /// when the host can't characterise the decoder side (e.g. mixed
    /// remote-member fleet).
    H264,
    /// VP9. Royalty-free, better compression than H.264 at the same
    /// bitrate. Default for browser-on-host scenarios.
    Vp9,
    /// AV1. Best compression, narrower hardware-decode support. Use
    /// only when the host has confirmed every member can decode it.
    Av1,
}

/// What role does this surface play in the host's window?
///
/// Distinguishes the primary content from cursors and decorations so
/// the receiving compositor can composite them correctly without a
/// separate side-channel. A pointer surface needs to be drawn over the
/// primary at the cursor position; decoration surfaces (server-side
/// title bars on legacy apps) need to be drawn under content where
/// the host has them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum SurfaceRole {
    /// The main content surface. Most streams have exactly one.
    Primary,
    /// A cursor / pointer surface, drawn over `Primary` at the host's
    /// reported pointer position.
    Cursor,
    /// Server-side window decoration. Legacy GTK2 apps and a few X11
    /// holdouts paint their own title bars; we ship them as separate
    /// surfaces so client-side themes don't collide.
    Decoration,
}

/// Mirror of `wl_pointer.button_state`.
///
/// The wire is two enum variants rather than a `bool` so a future
/// `Repeat` / `LongPress` addition is forward-compatible. See the
/// module-level rationale for why we mirror rather than depend on
/// wayland-protocols directly.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum ButtonState {
    /// Button is now pressed.
    Pressed,
    /// Button is now released.
    Released,
}

/// Mirror of `wl_keyboard.key_state`.
///
/// Same two-variant rationale as [`ButtonState`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum KeyState {
    /// Key is now pressed.
    Pressed,
    /// Key is now released.
    Released,
}

/// Bitmask of modifier flags carried on each `KeyboardKey` event.
///
/// Layout mirrors `xkb_state_serialize_mods` with the qOS-relevant
/// subset. The host translates from its real modifier state on send;
/// the receiver translates back when injecting.
///
/// `0` is "no modifiers"; unknown bits are reserved for forward-compat.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct KeyboardModifiers(pub u32);

impl KeyboardModifiers {
    /// Shift key (any side).
    pub const SHIFT: u32 = 1 << 0;
    /// Control key (any side).
    pub const CTRL: u32 = 1 << 1;
    /// Alt / Meta key.
    pub const ALT: u32 = 1 << 2;
    /// Logo / Super / Windows key.
    pub const LOGO: u32 = 1 << 3;
    /// Caps lock latched.
    pub const CAPS_LOCK: u32 = 1 << 4;
    /// Num lock latched.
    pub const NUM_LOCK: u32 = 1 << 5;

    /// Construct from a raw mask. No bit-validation — unknown bits are
    /// reserved and survive a round-trip unchanged.
    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    /// Raw bitmask.
    pub const fn raw(self) -> u32 {
        self.0
    }
}

/// One pointer / keyboard event going from a remote member back to the
/// host so the host can inject it into the legacy app.
///
/// Variants are flat (not nested in a sub-enum) so postcard's
/// discriminant layout is one byte for the variant tag plus the
/// flattened fields — no double-tagging for `Pointer*` vs `Keyboard*`.
///
/// `Eq` is intentionally not derived because the pointer-motion and
/// pointer-scroll variants carry `f64` fields. `PartialEq` is enough
/// for test assertions; callers that need a stable identity hash
/// canonical-bytes-encode the value.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum RemoteInputEvent {
    /// Remote pointer has entered the streamed surface. Send before
    /// the first `PointerMotion` so the host can mark the surface as
    /// hovered (legacy apps care: tooltips, hover highlight, etc.).
    PointerEnter,
    /// Remote pointer has left the streamed surface. The host should
    /// dispatch the matching `wl_pointer.leave` to the legacy app.
    PointerLeave,
    /// Remote pointer moved. Coordinates are in surface-local pixels,
    /// at the resolution the host announced in [`SurfaceStreamStart`].
    /// The host scales to its real surface size on inject.
    PointerMotion {
        /// X coordinate inside the surface, 0 = left edge.
        surface_x: f64,
        /// Y coordinate inside the surface, 0 = top edge.
        surface_y: f64,
    },
    /// Pointer button changed state. `button` is a Linux input-event
    /// button code (BTN_LEFT = 0x110, BTN_RIGHT = 0x111, etc.) so the
    /// host can pass it through to `libinput` without translation.
    PointerButton {
        /// Linux input-event button code.
        button: u32,
        /// New state of the button.
        state: ButtonState,
    },
    /// Scroll wheel / touchpad scroll. `dx` and `dy` are in floating-
    /// point "discrete steps" matching `wl_pointer.axis_discrete`'s
    /// continuous variant. Sign convention: positive `dy` is downward
    /// scroll (matches `wl_pointer`).
    PointerScroll {
        /// Horizontal scroll delta, discrete steps.
        dx: f64,
        /// Vertical scroll delta, discrete steps. Positive = down.
        dy: f64,
    },
    /// Keyboard key changed state. `keycode` is a Linux evdev keycode
    /// (not an X11 keycode — they differ by 8). `modifiers` is the
    /// modifier state *at the time of the event*, which the host uses
    /// to keep its mirrored xkb state in sync with the remote
    /// member's.
    KeyboardKey {
        /// Linux evdev keycode (KEY_A = 30, etc.).
        keycode: u32,
        /// New state of the key.
        state: KeyState,
        /// Modifier bitmask in effect at the moment of the key event.
        modifiers: KeyboardModifiers,
    },
}

/// Payload for [`crate::role::OpClass::SurfaceStreamStart`].
///
/// Announces a new streamed surface. After a peer accepts this op
/// (validation green-lights it, the broker fans it out, the receiving
/// compositor opens a decoder for it), subsequent [`SurfaceFrame`]
/// payloads for the same `surface_id` are interpreted against the
/// resolution + codec declared here.
///
/// The `hz_num` / `hz_den` pair is a frame-rate fraction so we don't
/// have to commit to integer Hz (24000/1001 for 23.976, etc.). The
/// host should pick the source rate; if it doesn't know, 60/1 is the
/// safe default.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurfaceStreamStart {
    /// New stable id for this surface, scoped to the host's channel.
    pub surface_id: SurfaceId,
    /// Width of the encoded video, in pixels.
    pub width: u32,
    /// Height of the encoded video, in pixels.
    pub height: u32,
    /// Frame-rate numerator. Use 60 for 60 Hz.
    pub hz_num: u32,
    /// Frame-rate denominator. Use 1 unless you have a fractional
    /// rate (e.g. 1001 for 23.976).
    pub hz_den: u32,
    /// Codec used for every subsequent `SurfaceFrame` on this stream.
    pub codec: VideoCodec,
    /// What role this surface plays in the host's window stack.
    pub role: SurfaceRole,
}

/// Payload for [`crate::role::OpClass::SurfaceStreamEnd`].
///
/// Retires `surface_id`. Subsequent `SurfaceFrame`s for that id should
/// be dropped on the floor by every receiving compositor; the broker's
/// per-artifact log keeps the End op for scrollback so a late-joining
/// peer learns the surface is gone.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurfaceStreamEnd {
    /// Id of the stream being retired.
    pub surface_id: SurfaceId,
}

/// Payload for [`crate::role::OpClass::SurfaceFrame`].
///
/// One encoded video frame. `bytes` is opaque codec output; this crate
/// does not parse it. `pts_us` is the presentation timestamp in
/// microseconds since the host opened the stream — receivers use it to
/// pace decode + present, not as a wall-clock value.
///
/// The payload deliberately uses [`bytes::Bytes`] rather than `Vec<u8>`
/// so the broker can fan out one underlying allocation to N peers
/// without copying. The serde representation is a length-prefixed byte
/// sequence (postcard treats it identically to `Vec<u8>`), so the wire
/// shape is identical to other binary fields in the crate.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurfaceFrame {
    /// Id of the stream this frame belongs to.
    pub surface_id: SurfaceId,
    /// Presentation timestamp in microseconds since stream open.
    pub pts_us: u64,
    /// `true` if this frame can be decoded standalone (a keyframe /
    /// I-frame). The receiver uses it to drop the decode buffer and
    /// resynchronise after a packet loss.
    pub key_frame: bool,
    /// Encoded frame bytes. Opaque from this crate's perspective.
    pub bytes: bytes::Bytes,
}

/// Payload for [`crate::role::OpClass::RemoteInput`].
///
/// One input event from a remote member, targeted at one streamed
/// surface. The host's compositor receives it, validates that the
/// originating member's role permits input injection on this channel
/// (the role-table check), then dispatches the event into the live
/// app.
///
/// `Eq` is intentionally not derived: see [`RemoteInputEvent`].
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteInput {
    /// Id of the streamed surface the event targets.
    pub surface_id: SurfaceId,
    /// The input event itself.
    pub event: RemoteInputEvent,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical::{canonical_bytes, from_canonical_bytes};

    #[test]
    fn surface_id_round_trip_transparent() {
        let id = SurfaceId::from_u64(42);
        assert_eq!(id.as_u64(), 42);
        let bytes = canonical_bytes(&id).unwrap();
        let back: SurfaceId = from_canonical_bytes(&bytes).unwrap();
        assert_eq!(id, back);
    }

    #[test]
    fn video_codec_round_trip() {
        for codec in [VideoCodec::H264, VideoCodec::Vp9, VideoCodec::Av1] {
            let bytes = canonical_bytes(&codec).unwrap();
            let back: VideoCodec = from_canonical_bytes(&bytes).unwrap();
            assert_eq!(codec, back);
        }
    }

    #[test]
    fn surface_role_round_trip() {
        for role in [SurfaceRole::Primary, SurfaceRole::Cursor, SurfaceRole::Decoration] {
            let bytes = canonical_bytes(&role).unwrap();
            let back: SurfaceRole = from_canonical_bytes(&bytes).unwrap();
            assert_eq!(role, back);
        }
    }

    #[test]
    fn surface_stream_start_round_trip() {
        let s = SurfaceStreamStart {
            surface_id: SurfaceId::from_u64(7),
            width: 1920,
            height: 1080,
            hz_num: 60,
            hz_den: 1,
            codec: VideoCodec::Vp9,
            role: SurfaceRole::Primary,
        };
        let bytes = canonical_bytes(&s).unwrap();
        let back: SurfaceStreamStart = from_canonical_bytes(&bytes).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn surface_stream_end_round_trip() {
        let s = SurfaceStreamEnd {
            surface_id: SurfaceId::from_u64(9),
        };
        let bytes = canonical_bytes(&s).unwrap();
        let back: SurfaceStreamEnd = from_canonical_bytes(&bytes).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn surface_frame_round_trip() {
        let f = SurfaceFrame {
            surface_id: SurfaceId::from_u64(1),
            pts_us: 16_667,
            key_frame: true,
            bytes: bytes::Bytes::from_static(&[0, 1, 2, 3, 4]),
        };
        let bytes = canonical_bytes(&f).unwrap();
        let back: SurfaceFrame = from_canonical_bytes(&bytes).unwrap();
        assert_eq!(f, back);
        assert_eq!(back.bytes.as_ref(), &[0, 1, 2, 3, 4]);
    }

    #[test]
    fn remote_input_pointer_motion_round_trip() {
        let r = RemoteInput {
            surface_id: SurfaceId::from_u64(1),
            event: RemoteInputEvent::PointerMotion {
                surface_x: 12.5,
                surface_y: 34.75,
            },
        };
        let bytes = canonical_bytes(&r).unwrap();
        let back: RemoteInput = from_canonical_bytes(&bytes).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn remote_input_pointer_button_round_trip() {
        let r = RemoteInput {
            surface_id: SurfaceId::from_u64(1),
            event: RemoteInputEvent::PointerButton {
                button: 0x110, // BTN_LEFT
                state: ButtonState::Pressed,
            },
        };
        let bytes = canonical_bytes(&r).unwrap();
        let back: RemoteInput = from_canonical_bytes(&bytes).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn remote_input_pointer_scroll_round_trip() {
        let r = RemoteInput {
            surface_id: SurfaceId::from_u64(1),
            event: RemoteInputEvent::PointerScroll { dx: 0.0, dy: -1.5 },
        };
        let bytes = canonical_bytes(&r).unwrap();
        let back: RemoteInput = from_canonical_bytes(&bytes).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn remote_input_keyboard_key_round_trip() {
        let r = RemoteInput {
            surface_id: SurfaceId::from_u64(1),
            event: RemoteInputEvent::KeyboardKey {
                keycode: 30, // KEY_A
                state: KeyState::Pressed,
                modifiers: KeyboardModifiers::from_raw(
                    KeyboardModifiers::SHIFT | KeyboardModifiers::CTRL,
                ),
            },
        };
        let bytes = canonical_bytes(&r).unwrap();
        let back: RemoteInput = from_canonical_bytes(&bytes).unwrap();
        assert_eq!(r, back);
        if let RemoteInputEvent::KeyboardKey { modifiers, .. } = back.event {
            assert!(modifiers.raw() & KeyboardModifiers::SHIFT != 0);
            assert!(modifiers.raw() & KeyboardModifiers::CTRL != 0);
        } else {
            panic!("variant changed across round-trip");
        }
    }

    #[test]
    fn remote_input_enter_leave_round_trip() {
        for event in [RemoteInputEvent::PointerEnter, RemoteInputEvent::PointerLeave] {
            let r = RemoteInput {
                surface_id: SurfaceId::from_u64(1),
                event,
            };
            let bytes = canonical_bytes(&r).unwrap();
            let back: RemoteInput = from_canonical_bytes(&bytes).unwrap();
            assert_eq!(r, back);
        }
    }
}
