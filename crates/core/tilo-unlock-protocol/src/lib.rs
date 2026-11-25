//! Wire protocol for unlocking an encrypted disk with a sibling's consent.
//!
//! A LUKS slot of an enrolled device opens with a key derived from two
//! halves ([`derive_slot_key`]): the device half stays on the machine, and the
//! sibling half is held by the person's other devices, sealed to the person's
//! root-derived secret-sync key ([`SealedSiblingHalf`]).
//!
//! At enrolment the person's root certifies the boot-only Ed25519 key as an
//! ordinary [`tilo_user_identity::DeviceCert`] with no capabilities: it
//! grants nothing, so a leaked boot partition holds a key that can ask, never
//! one that can act as the device. Early boot proves possession of that key
//! against a fresh challenge from a sibling, and the sibling releases its half
//! encrypted to an ephemeral key of that one request ([`Release`]), only after
//! the owner approved the typed `device.unlock` action (or the owner turned on
//! auto-approval for the device and its TPM attested the expected boot chain,
//! see [`attest`]). Which volume and key slot the request asks for is in the
//! signed request, and the answering device checks it against the enrolment it
//! recorded.
//!
//! # Exchange
//!
//! One bidirectional stream on [`UNLOCK_ALPN`], frames encoded by
//! [`encode_frame`]:
//!
//! ```text
//! boot  → sibling:  ClientFrame::Hello(UnlockHello { boot_cert, .. })
//! sibling → boot:   SiblingFrame::Challenge(Challenge)      | Refused(reason)
//! boot  → sibling:  ClientFrame::Request(UnlockRequest)     signed by the boot key
//! sibling → boot:   SiblingFrame::Pending { approval_code } (zero or more times)
//! sibling → boot:   SiblingFrame::Released(Release) | Denied | Refused(reason)
//! ```
//!
//! # Enrolment
//!
//! A device enrols its boot key with the `certifyKey` device action and an
//! [`EnrolRequest`]: run under the person's root where the device keeps it,
//! and otherwise asked of a device of theirs that does, where the person
//! answers it. The enrolling device seals its sibling half to the person's
//! secret-sync key itself and publishes the enrolment
//! ([`published::UnlockEnrolment`]) in its own entry of their devices.
//!
//! # Wire ABI
//!
//! Signed and bound values are `blake3::derive_key(context, postcard(view))`
//! digests with one frozen context string per purpose. Neither the contexts
//! nor the field order may change without bumping [`PROTOCOL_VERSION`] and the
//! ALPN.

#![cfg_attr(
    test,
    allow(
        clippy::arithmetic_side_effects,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod attest;
mod enrol;
mod half;
pub mod opened;
pub mod published;
pub mod startup_secret;
mod wire;

pub use enrol::{EnrolRequest, EnrolResponse, RequestError, SiblingContact};
pub use half::{
    HALF_LEN, Half, SealedSiblingHalf, derive_slot_key, open_sibling_half, seal_sibling_half,
};
pub use opened::{OPENED_PATH, Opening, UnlockMethod};
pub use wire::{
    Challenge, ClientFrame, FrameError, RefusalReason, Release, SiblingFrame, UnlockHello,
    UnlockRequest, approval_code, decode_frame, encode_frame, frame_len, open_release,
    seal_release,
};

/// ALPN of the boot-time unlock exchange. Frozen wire ABI.
pub const UNLOCK_ALPN: &[u8] = b"tilo-device-unlock/1";
/// Version carried in every versioned message of this protocol.
pub const PROTOCOL_VERSION: u16 = 1;
/// Largest frame either side accepts, length prefix excluded.
pub const MAX_FRAME_BYTES: usize = 16 * 1024;
/// How long a sibling's challenge stays answerable.
pub const CHALLENGE_LIFETIME_NS: u64 = 60 * 1_000_000_000;
/// How long a sibling waits for the owner's decision before denying.
pub const APPROVAL_TIMEOUT_NS: u64 = 10 * 60 * 1_000_000_000;

/// The LUKS slot a sibling half belongs to.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct SlotId {
    /// The LUKS2 header UUID of the volume, as its 16 raw bytes.
    pub volume_uuid: [u8; 16],
    /// The LUKS2 keyslot the derived key was enrolled into.
    pub keyslot: u16,
}

/// A random id drawn once per boot by the early-boot client. A sibling
/// releases at most once per boot id.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct BootId(pub [u8; 16]);

/// Domain-separated digest of a postcard-encoded view.
pub(crate) fn digest<T: serde::Serialize>(
    context: &str,
    view: &T,
) -> Result<[u8; 32], postcard::Error> {
    let bytes = postcard::to_allocvec(view)?;
    Ok(blake3::derive_key(context, &bytes))
}

#[cfg(test)]
mod tests;
