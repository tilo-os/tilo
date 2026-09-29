//! Delegated admission into a public room by an always-on workspace host.
//!
//! A public room is owned by a person (for the Tilo community room, the
//! team's user root), and its governance is written by that person's device.
//! People join it by redeeming its standing invitation, and they must be able
//! to while that device is asleep. The owner therefore delegates exactly one
//! power to a host: admitting a redeeming person into the room at a role no
//! higher than Commenter. The admission names the person (their user root),
//! not one device: every device that root certifies is theirs in the room. Hosts never hold the human root, and this module
//! keeps it that way.
//!
//! The delegation is an ordinary attenuable [`crate::invite::InviteToken`]
//! chain rooted at the room owner, whose (tightest) subject is the host's
//! device key, scoped to the room's workspace id. Every [`HostAdmissionV1`]
//! carries that chain and is signed by the host device. A member accepts an
//! admission only when:
//!
//! 1. the chain verifies against the room's recorded owner, unexpired;
//! 2. the chain's subject is the device that signed the admission;
//! 3. the chain names this room's workspace and pins no sub-resource;
//! 4. neither the chain nor the admission grants more than
//!    [`HOST_ADMISSION_ROLE_CEILING`], and the admission's role is within the
//!    chain's cap bundle;
//! 5. the admission's own signature verifies.
//!
//! So a compromised or buggy host can admit strangers as commenters — which
//! is what the standing invitation already offers anyone — and nothing more:
//! it cannot mint an Editor, change the room's governance, or speak for
//! another room.

use web_time::SystemTime;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::canonical::CanonicalError;
use crate::caps::{Verb, role_caps};
use crate::crypto::{Ed25519PublicKey, Signature, VerifyError, sign_event, verify_event};
use crate::invite::{InviteError, InviteToken, verify_chain};
use crate::role::Role;

/// Signing-domain tag mixed into every admission's signed bytes.
pub const HOST_ADMISSION_DOMAIN_TAG: &str = "tilo-host-admission-v1";

/// The highest role a delegated host may admit anyone at.
pub const HOST_ADMISSION_ROLE_CEILING: Role = Role::Commenter;

/// One person admitted into a public room by a delegated host.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostAdmissionV1 {
    /// The room, as its canonical uuid string.
    pub room: String,
    /// The room's workspace (topic) id — what the delegation is scoped to.
    pub workspace: u64,
    /// The admitted person's user root key.
    pub member_user: Ed25519PublicKey,
    /// The role admitted at. Never above [`HOST_ADMISSION_ROLE_CEILING`].
    pub role: Role,
    /// Id of the standing invitation that was redeemed.
    pub invite: [u8; 32],
    /// When the host admitted the device, Unix seconds by the host's clock.
    pub admitted_at_unix_seconds: u64,
    /// The host device that admitted, and signed this record.
    pub host: Ed25519PublicKey,
    /// Owner-rooted delegation chain whose last subject is `host`.
    pub delegation: Vec<InviteToken>,
    /// Signature by `host` over every other field.
    pub signature: Signature,
}

/// Everything an admission says, before the host signs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostAdmissionSpec {
    /// See [`HostAdmissionV1::room`].
    pub room: String,
    /// See [`HostAdmissionV1::workspace`].
    pub workspace: u64,
    /// See [`HostAdmissionV1::member_user`].
    pub member_user: Ed25519PublicKey,
    /// See [`HostAdmissionV1::role`].
    pub role: Role,
    /// See [`HostAdmissionV1::invite`].
    pub invite: [u8; 32],
    /// See [`HostAdmissionV1::admitted_at_unix_seconds`].
    pub admitted_at_unix_seconds: u64,
    /// See [`HostAdmissionV1::delegation`].
    pub delegation: Vec<InviteToken>,
}

/// Why an admission is refused.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum HostAdmissionError {
    /// The delegation chain does not verify against the room owner.
    #[error("host delegation does not verify: {0}")]
    Delegation(#[from] InviteError),
    /// The delegation was issued to a different key than the admitting host.
    #[error("the delegation was issued to another host")]
    NotDelegate,
    /// The delegation is for another room, or pinned to one resource in it.
    #[error("the delegation does not cover this room")]
    WrongRoom,
    /// A role above the ceiling, or outside the delegation's caps.
    #[error(
        "a delegated host may admit at most {HOST_ADMISSION_ROLE_CEILING:?}, and only within its delegation"
    )]
    RoleTooHigh,
    /// The admission's own signature did not verify.
    #[error("admission signature did not verify: {0}")]
    BadSignature(#[source] VerifyError),
    /// Canonical encoding failed. Effectively unreachable.
    #[error("canonicalisation failed: {0}")]
    Canonical(#[from] CanonicalError),
}

#[derive(Serialize)]
struct AdmissionView<'a> {
    tag: &'a str,
    room: &'a str,
    workspace: u64,
    member_user: &'a Ed25519PublicKey,
    role: &'a Role,
    invite: &'a [u8; 32],
    admitted_at_unix_seconds: u64,
    host: &'a Ed25519PublicKey,
    delegation: &'a [InviteToken],
}

/// Whether `role` is within `ceiling`, by cap ceiling. `Denied` is within
/// everything; nothing but `Denied` is within `Denied`.
fn role_within(role: Role, ceiling: Role) -> bool {
    match (role_caps(role), role_caps(ceiling)) {
        (None, _) => true,
        (Some(_), None) => false,
        (Some(granted), Some(limit)) => limit.implies(granted),
    }
}

impl HostAdmissionV1 {
    fn view(&self) -> AdmissionView<'_> {
        AdmissionView {
            tag: HOST_ADMISSION_DOMAIN_TAG,
            room: &self.room,
            workspace: self.workspace,
            member_user: &self.member_user,
            role: &self.role,
            invite: &self.invite,
            admitted_at_unix_seconds: self.admitted_at_unix_seconds,
            host: &self.host,
            delegation: &self.delegation,
        }
    }

    /// Sign `spec` as the host holding `host_sk`.
    ///
    /// # Errors
    ///
    /// [`HostAdmissionError::RoleTooHigh`] for a role above the ceiling —
    /// refused at the source as well as by every verifier — or a canonical
    /// encoding failure.
    pub fn sign(
        spec: HostAdmissionSpec,
        host_sk: &ed25519_dalek::SigningKey,
    ) -> Result<Self, HostAdmissionError> {
        if !role_within(spec.role, HOST_ADMISSION_ROLE_CEILING) {
            return Err(HostAdmissionError::RoleTooHigh);
        }
        let mut admission = Self {
            room: spec.room,
            workspace: spec.workspace,
            member_user: spec.member_user,
            role: spec.role,
            invite: spec.invite,
            admitted_at_unix_seconds: spec.admitted_at_unix_seconds,
            host: Ed25519PublicKey::from_bytes(host_sk.verifying_key().to_bytes()),
            delegation: spec.delegation,
            signature: Signature::from_bytes([0; 64]),
        };
        admission.signature = sign_event(&admission.view(), host_sk)?;
        Ok(admission)
    }

    /// Verify this admission for a room whose recorded owner is `owner`, at
    /// `now`. Returns the role to grant `member_user`.
    ///
    /// # Errors
    ///
    /// The first of the rules in the module docs that fails.
    pub fn verify(
        &self,
        owner: Ed25519PublicKey,
        now: SystemTime,
    ) -> Result<Role, HostAdmissionError> {
        let delegate = verify_chain(&self.delegation, now, &[owner])?;
        if delegate.subject != self.host {
            return Err(HostAdmissionError::NotDelegate);
        }
        if delegate.workspace != self.workspace || delegate.sub_resource.is_some() {
            return Err(HostAdmissionError::WrongRoom);
        }
        let admitted_verb: Option<Verb> = role_caps(self.role);
        if !role_within(delegate.role, HOST_ADMISSION_ROLE_CEILING)
            || !role_within(self.role, HOST_ADMISSION_ROLE_CEILING)
            || !role_within(self.role, delegate.role)
            || admitted_verb.is_some_and(|verb| !delegate.caps.contains(&verb))
        {
            return Err(HostAdmissionError::RoleTooHigh);
        }
        verify_event(&self.view(), &self.signature, &self.host)
            .map_err(HostAdmissionError::BadSignature)?;
        Ok(self.role)
    }
}
