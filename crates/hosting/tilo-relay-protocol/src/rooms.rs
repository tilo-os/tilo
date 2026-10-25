//! The rooms a host governs, as a page reaches them over HTTPS.
//!
//! Two routes below [`API_PREFIX`](crate::web::API_PREFIX):
//!
//! | Route | Session | Body | Answer |
//! |---|---|---|---|
//! | `/rooms` | yes | [`RoomsRequest`] | [`RoomsReply`] |
//! | `/join` | no | [`JoinStep`] | [`JoinReply`] |
//!
//! `/rooms` administers the host's rooms for a device of the host's own
//! user: create a governed workspace, enter it as its owner, invite and
//! cancel invitations, admit or decline the people who asked to join, change
//! members' roles and remove them. `/join` is for
//! anyone holding an invitation, before the host admits them to a session:
//! the device names itself with its certificate, signs the host's join
//! challenge ([`join_proof_digest`]) and waits until the room's owner admits
//! or declines it.

use serde::{Deserialize, Serialize};
use tilo_user_identity::DeviceCert;

/// Domain of the device signature that answers a join challenge.
pub const JOIN_PROOF_DOMAIN: &[u8] = b"tilo/workspace-join-proof/v1\0";

/// Pending join requests a room keeps at once.
pub const MAX_JOIN_REQUESTS: usize = 64;

/// What the device signs to answer the join challenge whose canonical
/// encoding the host sent as [`JoinReply::Challenge`]:
/// `blake3(JOIN_PROOF_DOMAIN ‖ challenge)`.
#[must_use]
pub fn join_proof_digest(challenge: &[u8]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(JOIN_PROOF_DOMAIN);
    hasher.update(challenge);
    *hasher.finalize().as_bytes()
}

/// The fields of a join challenge a device checks before it signs: the
/// host's challenge decodes as this view, field for field.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinChallengeView {
    /// Challenge format version.
    pub version: u32,
    /// The invitation being redeemed.
    pub signed_invitation: Vec<u8>,
    /// The room's UUID.
    pub room: String,
    /// The room's relay workspace.
    pub workspace_id: u64,
    pub authority_device_public_key: [u8; 32],
    /// The joining person.
    pub user_public_key: [u8; 32],
    pub device_public_key: [u8; 32],
    /// The device's request.
    pub request_id: [u8; 32],
    /// Host nonce.
    pub nonce: [u8; 32],
    /// The challenge expires here.
    pub expires_at_ns: u64,
}

/// What a member may do in a room.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RoomRole {
    Viewer,
    /// Reads and comments.
    Commenter,
    /// Reads and writes.
    Editor,
    /// Writes, moderates other people's content and removes or bans members
    /// ranked below Moderator.
    Moderator,
}

/// A person governance records in a room.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomMember {
    /// The person's user root.
    pub user_public_key: [u8; 32],
    /// How the person is named: their recorded name, else their device's
    /// label.
    pub name: Option<String>,
    /// Their role.
    pub role: RoomRole,
    pub owner: bool,
    /// They were removed.
    pub removed: bool,
}

/// A governed room.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Room {
    /// The room's UUID.
    pub room: String,
    /// The relay workspace carrying it.
    pub workspace_id: u64,
    pub name: String,
    /// Its committed governance generation.
    pub generation: u64,
    /// Every person governance records, removed ones included.
    pub members: Vec<RoomMember>,
}

/// A person who redeemed an invitation and waits for the owner.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinRequest {
    /// The device's request.
    pub request_id: [u8; 32],
    /// The person.
    pub user_public_key: [u8; 32],
    /// Their device.
    pub device_public_key: [u8; 32],
    /// The name they gave.
    pub name: String,
    /// The role the invitation grants.
    pub role: RoomRole,
    /// When they asked.
    pub requested_at_ns: u64,
}

/// The person who sent an invitation, as the host signed it into the
/// invitation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Inviter {
    /// Their user root.
    pub user_public_key: [u8; 32],
    /// The name they gave.
    pub name: String,
}

/// Domain of the owner's signature over a [`PreAdmission`].
pub const PRE_ADMISSION_DOMAIN: &[u8] = b"tilo/workspace-pre-admission/v1\0";

/// Longest label a pre-admission gives the person it is for.
pub const MAX_PRE_ADMISSION_LABEL_CHARS: usize = 128;

/// The room owner's standing decision to admit, without deciding again,
/// whoever redeems the invitation issued with it and presents the one-time
/// token whose SHA-256 is `token_sha256`. Whoever hands out the token (a
/// backend that verified the person's email, say) never holds the room's
/// keys; the owner's user root signs the grant.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreAdmission {
    /// The room's UUID.
    pub room: String,
    pub role: RoomRole,
    /// SHA-256 of the token the person presents.
    pub token_sha256: [u8; 32],
    /// Whom it is for, as the owner knows them (an email address); the
    /// owner's notice names them with it.
    pub label: String,
    /// It stops admitting here, at most 30 days ahead.
    pub expires_at_ns: u64,
    /// The key that signs it: the owner's user root, or, for a public
    /// workspace's door, the device key of the host that answers it under
    /// the owner's listing (`crate::directory`).
    pub owner_user_public_key: [u8; 32],
    /// The signature of that key over [`PreAdmission::digest`].
    #[serde(with = "serde_bytes")]
    pub signature: [u8; 64],
}

#[derive(Serialize)]
struct PreAdmissionView<'a> {
    room: &'a str,
    role: RoomRole,
    token_sha256: &'a [u8; 32],
    label: &'a str,
    expires_at_ns: u64,
    owner_user_public_key: &'a [u8; 32],
}

impl PreAdmission {
    /// What the owner's root signs: `blake3(PRE_ADMISSION_DOMAIN ‖ postcard(fields
    /// but the signature))`.
    ///
    /// # Errors
    ///
    /// Returns an error when the fields cannot be encoded.
    pub fn digest(&self) -> Result<[u8; 32], postcard::Error> {
        let view = PreAdmissionView {
            room: &self.room,
            role: self.role,
            token_sha256: &self.token_sha256,
            label: &self.label,
            expires_at_ns: self.expires_at_ns,
            owner_user_public_key: &self.owner_user_public_key,
        };
        let mut hasher = blake3::Hasher::new();
        hasher.update(PRE_ADMISSION_DOMAIN);
        hasher.update(&postcard::to_allocvec(&view)?);
        Ok(*hasher.finalize().as_bytes())
    }

    /// Sign it with `key`, which it names as its signer.
    ///
    /// # Errors
    ///
    /// Returns an error when the fields cannot be encoded.
    pub fn sign(&mut self, key: &ed25519_dalek::SigningKey) -> Result<(), postcard::Error> {
        use ed25519_dalek::Signer as _;
        self.owner_user_public_key = key.verifying_key().to_bytes();
        self.signature = key.sign(&self.digest()?).to_bytes();
        Ok(())
    }

    /// Whether the key it names signed it and its label is printable.
    pub fn verify(&self) -> Result<(), String> {
        let label = self.label.trim();
        if label.is_empty()
            || label != self.label
            || label.chars().count() > MAX_PRE_ADMISSION_LABEL_CHARS
            || label.chars().any(char::is_control)
        {
            return Err("pre-admission label is invalid".into());
        }
        let digest = self.digest().map_err(|error| error.to_string())?;
        ed25519_dalek::VerifyingKey::from_bytes(&self.owner_user_public_key)
            .and_then(|key| {
                key.verify_strict(
                    &digest,
                    &ed25519_dalek::Signature::from_bytes(&self.signature),
                )
            })
            .map_err(|_| "pre-admission signature did not verify".to_string())
    }
}

/// Something that happened in a room without its owner deciding it then,
/// for the owner to see.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RoomNotice {
    /// A person was admitted under the owner's [`PreAdmission`].
    PreAdmitted {
        /// The person.
        user_public_key: [u8; 32],
        /// The name they gave.
        name: String,
        /// The pre-admission's label.
        label: String,
        /// Their role.
        role: RoomRole,
        at_ns: u64,
    },
    /// Someone reported the public workspace through its door
    /// ([`crate::directory::DoorReport`]).
    Reported {
        /// Who reported it.
        user_public_key: [u8; 32],
        /// What they said is wrong.
        reason: String,
        at_ns: u64,
    },
}

/// An invitation still redeemable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingInvite {
    /// Digest of the signed invitation.
    pub invite_id: [u8; 32],
    /// The role it grants.
    pub role: RoomRole,
    /// It stops being redeemable here.
    pub expires_at_ns: u64,
}

/// One `/rooms` request. `room` is a room UUID. Each change answers with
/// the room as it is after it, except as noted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RoomsRequest {
    List,
    /// Create a governed workspace named `name` (1–80 characters).
    Create {
        name: String,
    },
    /// Admit the calling device to `room` as its owner.
    Enter {
        room: String,
    },
    /// Issue an invitation anyone holding it may redeem; the owner admits
    /// each redemption.
    Invite {
        room: String,
        /// Caller-chosen idempotency key; never zero.
        request_id: [u8; 32],
        /// The role the invitation grants.
        role: RoomRole,
        /// It stops being redeemable here, at most 30 days ahead.
        expires_at_ns: u64,
        /// How the inviting person names themself; the invitation records
        /// it with their user root.
        inviter_name: Option<String>,
        /// The wait-list entry ([`WaitListEntry::request_id`]) the invitation
        /// answers: the entry shows it was invited, and leaves the list
        /// once someone joins with the invitation.
        #[serde(default)]
        wait_list_entry: Option<[u8; 32]>,
    },
    /// Cancel an invitation not yet redeemed: answered with
    /// [`RoomsReply::Pending`].
    Cancel {
        room: String,
        /// Caller-chosen idempotency key.
        request_id: [u8; 32],
        /// The invitation.
        invite_id: [u8; 32],
    },
    /// The invitations to `room` still redeemable.
    Pending {
        room: String,
    },
    /// The people waiting to join `room`.
    Requests {
        room: String,
    },
    /// Admit a waiting person.
    Admit {
        room: String,
        /// Their request.
        request_id: [u8; 32],
    },
    /// Decline a waiting person: answered with [`RoomsReply::Requests`].
    Decline {
        room: String,
        /// Their request.
        request_id: [u8; 32],
    },
    /// Change a member's role.
    SetRole {
        room: String,
        /// Caller-chosen idempotency key.
        request_id: [u8; 32],
        /// The member.
        user_public_key: [u8; 32],
        role: RoomRole,
    },
    /// Remove a member and every device of theirs.
    Remove {
        room: String,
        /// Caller-chosen idempotency key.
        request_id: [u8; 32],
        /// The member.
        user_public_key: [u8; 32],
    },
    /// Remove a person, when a member, and keep them out until unbanned.
    /// The owner, and a Moderator for members ranked below Moderator.
    Ban {
        room: String,
        /// Caller-chosen idempotency key.
        request_id: [u8; 32],
        /// The person.
        user_public_key: [u8; 32],
    },
    /// Let a banned person be admitted again.
    Unban {
        room: String,
        /// Caller-chosen idempotency key.
        request_id: [u8; 32],
        /// The person.
        user_public_key: [u8; 32],
    },
    /// Issue a single-use invitation that admits whoever redeems it with
    /// the token `grant` names, without the owner deciding again: answered
    /// with [`RoomsReply::Invitation`].
    PreAdmit {
        /// Caller-chosen idempotency key; never zero.
        request_id: [u8; 32],
        /// The owner's signed grant; its room is the room.
        grant: PreAdmission,
        /// How the inviting person names themself.
        inviter_name: Option<String>,
    },
    /// What happened in `room` without the owner deciding it then.
    Notices {
        room: String,
    },
    /// Whether the calling device's person may make an undoable change at
    /// `node` of `room`, as the room decides their writes there: answered
    /// with [`RoomsReply::Permits`]. A page asks before it offers what only
    /// some members may do, such as acting on what other people made.
    Permits {
        room: String,
        /// A node of the room's published tree, in its text form
        /// (`room:<id>/app:<app>/…`).
        node: String,
    },
    /// The requests left at `room`'s door, oldest first: answered with
    /// [`RoomsReply::WaitList`]. Whoever may invite.
    WaitList {
        room: String,
    },
    /// Take a request left at `room`'s door off its wait list, when it is
    /// not to be invited: answered with [`RoomsReply::WaitList`]. An entry
    /// invited ([`RoomsRequest::Invite`]'s `wait_list_entry`) leaves the
    /// list by itself once someone joins with that invitation.
    Unlist {
        room: String,
        /// The entry.
        request_id: [u8; 32],
    },
    /// Mail the invitation `link` to the wait-list entry `wait_list_entry`
    /// it was issued for ([`RoomsRequest::Invite`]'s `wait_list_entry`):
    /// answered with [`RoomsReply::WaitList`], or refused where the room
    /// sends no mail, when the page shares the link itself.
    MailInvite {
        room: String,
        /// The confirmed entry.
        wait_list_entry: [u8; 32],
        /// The invitation's web link: the room's own door, with the signed
        /// invitation in its `invite` query parameter.
        link: String,
    },
}

/// The answer to a [`RoomsRequest`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RoomsReply {
    /// Every governed room.
    Rooms(Vec<Room>),
    Room(Room),
    /// The issued invitation.
    Invitation {
        /// Digest of `signed_invitation`.
        invite_id: [u8; 32],
        /// What a device redeems.
        signed_invitation: Vec<u8>,
    },
    /// The redeemable invitations.
    Pending(Vec<PendingInvite>),
    /// The people waiting to join.
    Requests(Vec<JoinRequest>),
    /// Refused, and why.
    Refused(String),
    /// What happened without the owner deciding it then, oldest first.
    Notices(Vec<RoomNotice>),
    /// Whether the caller may, for [`RoomsRequest::Permits`].
    Permits(bool),
    /// The requests left at the door, oldest first.
    WaitList(Vec<WaitListEntry>),
}

/// One `/join` step.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum JoinStep {
    /// What `invitation` offers.
    Inspect {
        /// The signed invitation.
        invitation: Vec<u8>,
    },
    /// Redeem `invitation` with the device `device_cert`: answered with the
    /// challenge to sign.
    Ask {
        /// The signed invitation.
        invitation: Vec<u8>,
        device_cert: DeviceCert,
        /// The device's request, random and kept until it is answered.
        request_id: [u8; 32],
    },
    /// The device's signature over [`join_proof_digest`] of `challenge`:
    /// the request then waits for the owner.
    Prove {
        /// The challenge exactly as the host sent it.
        challenge: Vec<u8>,
        /// Ed25519 signature by the joining device.
        #[serde(with = "serde_bytes")]
        signature: [u8; 64],
        /// How the person names themself to the owner: a display name
        /// ([`tilo_user_identity::display_name`]).
        name: String,
    },
    /// Where the request stands.
    Status {
        /// The device's request.
        request_id: [u8; 32],
        device_public_key: [u8; 32],
    },
    /// [`JoinStep::Prove`] with the one-time token of the owner's
    /// [`PreAdmission`] for the invitation: the device is admitted at once.
    ProvePreAdmitted {
        /// The challenge exactly as the host sent it.
        challenge: Vec<u8>,
        /// Ed25519 signature by the joining device.
        #[serde(with = "serde_bytes")]
        signature: [u8; 64],
        /// How the person names themself: a display name
        /// ([`tilo_user_identity::display_name`]).
        name: String,
        token: Vec<u8>,
    },
}

/// The answer to a [`JoinStep`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum JoinReply {
    /// What the invitation offers.
    Invitation {
        /// The room's UUID.
        room: String,
        /// Its relay workspace.
        workspace_id: u64,
        name: String,
        /// The role it grants.
        role: RoomRole,
        /// It stops being redeemable here.
        expires_at_ns: u64,
        /// Who sent it, when the invitation names them.
        inviter: Option<Inviter>,
    },
    /// The canonical encoding of the challenge to sign; it decodes as a
    /// [`JoinChallengeView`].
    Challenge {
        /// The challenge.
        challenge: Vec<u8>,
    },
    /// The owner has not decided yet.
    Waiting,
    /// The owner admitted the device: it may open a session and subscribe.
    Admitted {
        /// The room's UUID.
        room: String,
        /// Its relay workspace.
        workspace_id: u64,
        name: String,
        role: RoomRole,
    },
    /// The owner declined.
    Declined,
    /// Refused, and why.
    Refused(String),
}

// Requests left at the door. A room that takes people by invitation also
// keeps the requests of people who asked at its door without one, each an
// address to be invited at ([`WaitListEntry`]).

/// The longest address a request left at the door gives ([`WaitListEntry`]):
/// the longest an email address can be.
pub const MAX_CONTACT_CHARS: usize = 254;

/// `text` as the address a request left at a room's door keeps
/// ([`WaitListEntry`]): an email address, trimmed and otherwise as typed,
/// since that is how its owner writes it. One address is one request
/// however it was typed: requests are told apart by [`contact_key`].
///
/// # Errors
///
/// Anything that is not one plausible email address of at most
/// [`MAX_CONTACT_CHARS`] characters: one `@` with something before it and a
/// dotted domain after it, and no space or control character.
pub fn contact_address(text: &str) -> Result<String, &'static str> {
    let address = text.trim().to_owned();
    if address.chars().count() > MAX_CONTACT_CHARS {
        return Err("an email address is at most 254 characters");
    }
    let plausible = address.split_once('@').is_some_and(|(local, domain)| {
        !local.is_empty()
            && !domain.contains('@')
            && domain
                .split('.')
                .all(|label| !label.is_empty() && !label.starts_with('-'))
            && domain.contains('.')
    }) && !address
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '<' | '>' | ',' | ';'));
    if plausible {
        Ok(address)
    } else {
        Err("that is not an email address")
    }
}

/// The longest invitation link a door mails: an invitation carries its
/// signed certificate chain, a few KiB once encoded.
pub const MAX_INVITATION_LINK: usize = 16 * 1024;

/// Whether `link` is an invitation on the web entry `door` (a URL ending
/// in `/`): under it, with an `invite=` parameter, no longer than
/// [`MAX_INVITATION_LINK`] and without spaces or control characters. Only
/// such a link is mailed to an address on a wait list for a member.
#[must_use]
pub fn invitation_on_door(door: &str, link: &str) -> bool {
    door.ends_with('/')
        && link.len() <= MAX_INVITATION_LINK
        && link
            .strip_prefix(door)
            .is_some_and(|rest| rest.contains("invite="))
        && !link.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// What tells two addresses ([`contact_address`]) apart: lowercased, so
/// `Ada@Example.org` and `ada@example.org` are one request.
#[must_use]
pub fn contact_key(address: &str) -> String {
    address.trim().to_lowercase()
}

/// The language mails to an address left at the door are in, for the
/// language tag its page was read in: `de` for German, otherwise `en`.
#[must_use]
pub fn mail_language(tag: &str) -> &'static str {
    let primary = tag.split(['-', '_']).next().unwrap_or_default();
    if primary.eq_ignore_ascii_case("de") {
        "de"
    } else {
        "en"
    }
}

/// The bytes of the secret that takes a request left at a room's door off
/// its wait list again: random, handed only to whoever left it. The room
/// keeps only its SHA-256.
pub const WAIT_LIST_REMOVAL_BYTES: usize = 32;

/// A request left at the door of a room that takes people by invitation:
/// someone who has no invitation, and so no identity the room knows yet,
/// left an address to be invited at. Someone who may invite sends an
/// invitation of their own there ([`RoomsRequest::Invite`] naming the
/// entry), and the entry leaves the list once someone joins with it; or
/// they take it off the list ([`RoomsRequest::Unlist`]). Whoever left it
/// takes it off themselves with the secret they were handed
/// ([`WAIT_LIST_REMOVAL_BYTES`]). Unlike a [`JoinRequest`] it names no person
/// or device: it is not anyone to admit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitListEntry {
    /// The entry, for [`RoomsRequest::Unlist`].
    pub request_id: [u8; 32],
    /// The address ([`contact_address`]).
    pub contact: String,
    /// When it was left.
    pub added_at_ns: u64,
    /// When it was last invited, so nobody invites it twice.
    pub invited_at_ns: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_join_proof_digest_is_domain_separated() {
        let digest = join_proof_digest(b"challenge");
        assert_eq!(digest, join_proof_digest(b"challenge"));
        assert_ne!(digest, join_proof_digest(b"challengf"));
        assert_ne!(digest, *blake3::hash(b"challenge").as_bytes());
    }

    #[test]
    fn a_contact_is_one_email_address_however_it_was_typed() {
        assert_eq!(
            contact_address("  Ada@Example.ORG "),
            Ok("Ada@Example.ORG".to_owned())
        );
        assert_eq!(contact_key("Ada@Example.ORG"), "ada@example.org");
        assert_eq!(mail_language("de-AT"), "de");
        assert_eq!(mail_language("en-US"), "en");
        assert_eq!(mail_language("fr"), "en");
        assert_eq!(
            contact_key(" ada@example.org"),
            contact_key("ADA@example.org")
        );
        assert_eq!(
            contact_address("ada+tilo@mail.example.co.uk"),
            Ok("ada+tilo@mail.example.co.uk".to_owned())
        );
        for typed in [
            "",
            "ada",
            "ada@",
            "@example.org",
            "ada@example",
            "ada@example.",
            "ada@.org",
            "ada@@example.org",
            "a da@example.org",
            "ada@example.org, bob@example.org",
            "Ada <ada@example.org>",
        ] {
            assert!(contact_address(typed).is_err(), "{typed:?}");
        }
        let long = format!("{}@example.org", "a".repeat(MAX_CONTACT_CHARS));
        assert!(contact_address(&long).is_err());
    }
}
