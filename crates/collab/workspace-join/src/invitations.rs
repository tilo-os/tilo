//! What a device asks a room's governor about invitations, and what it
//! answers: issuing one, cancelling one, and listing those still
//! redeemable. The governor is whichever machine runs the room's
//! governance: a relay, a cloud host or the device that made the room.
//!
//! These are postcard wire formats between separately updated hosts and
//! desktops: field order is the wire, so append only. `tilo-relay-session`
//! re-exports them under `rooms`, their historic path.

use serde::{Deserialize, Serialize};

use crate::MemberRole;

/// Issue one invitation. A repeated `request_id` returns the invitation it
/// issued, and fails if it named a different request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueInvitationRequest {
    /// Caller-chosen idempotency key; never zero.
    pub request_id: [u8; 32],
    pub role: MemberRole,
    /// Restrict redemption to devices of this person.
    pub recipient_user_public_key: Option<[u8; 32]>,
    /// How the invitation names its recipient to them.
    pub invitee_name: Option<String>,
    /// When the invitation stops being redeemable, at most 30 days ahead.
    pub expires_at_ns: u64,
    /// The name the inviting person gives, recorded in the invitation with
    /// their user root.
    pub inviter_name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssuedInvitation {
    /// Digest of `signed_invitation`.
    pub invite_id: [u8; 32],
    /// The signed invitation a device redeems.
    pub signed_invitation: Vec<u8>,
}

/// Cancel an unredeemed invitation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelInvitationRequest {
    /// Caller-chosen idempotency key.
    pub request_id: [u8; 32],
    pub invite_id: [u8; 32],
}

/// An invitation still redeemable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingInvitation {
    /// Digest of the signed invitation.
    pub invite_id: [u8; 32],
    /// The role it grants.
    pub role: MemberRole,
    /// The only person who may redeem it, if restricted.
    pub recipient_user_public_key: Option<[u8; 32]>,
    /// How it names its recipient.
    pub invitee_name: Option<String>,
    pub expires_at_ns: u64,
}
