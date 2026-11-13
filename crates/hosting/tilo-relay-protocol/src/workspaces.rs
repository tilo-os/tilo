//! The workspaces a host governs, as a page reaches them over HTTPS.
//!
//! Two routes below [`API_PREFIX`](crate::web::API_PREFIX):
//!
//! | Route | Session | Body | Answer |
//! |---|---|---|---|
//! | `/rooms` | yes | [`WorkspacesRequest`] | [`WorkspacesReply`] |
//! | `/invitation` ([`crate::web::INVITATION_PATH`]) | no | the signed invitation (`Vec<u8>`) | [`JoinReply`] |
//!
//! `/rooms` administers the host's workspaces for a device of the host's own
//! user: create a governed workspace, enter it as its owner, invite and
//! cancel invitations, change members' roles and remove them. `/invitation`
//! says what an invitation offers ([`JoinReply::Invitation`]) to anyone
//! holding it, before they have a device of their own to enter with: the
//! page shows it to someone following a link before it makes them a person.
//! Redeeming an invitation is an entry on the device's session
//! ([`crate::entering::Through::Invitation`]).

use serde::{Deserialize, Serialize};

/// What a member may do in a workspace.
pub use collab_types::Role;

/// A person governance records in a workspace.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceMember {
    /// The person's user root.
    pub user_public_key: [u8; 32],
    /// How the person is named: their recorded name, else their device's
    /// label.
    pub name: Option<String>,
    /// Their role.
    pub role: Role,
    pub owner: bool,
    /// They were removed.
    pub removed: bool,
}

/// A governed workspace.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspace {
    /// The workspace's UUID.
    #[serde(rename = "room")]
    pub workspace: String,
    /// The relay workspace carrying it.
    pub workspace_id: u64,
    pub name: String,
    /// Its committed governance generation.
    pub generation: u64,
    /// Every person governance records, removed ones included.
    pub members: Vec<WorkspaceMember>,
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

/// The workspace owner's standing decision to admit, without deciding again,
/// whoever redeems the invitation issued with it and presents the one-time
/// token whose SHA-256 is `token_sha256`. Whoever hands out the token (a
/// backend that verified the person's email, say) never holds the workspace's
/// keys; the owner's user root signs the grant.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreAdmission {
    /// The workspace's UUID.
    #[serde(rename = "room")]
    pub workspace: String,
    pub role: Role,
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
    #[serde(rename = "room")]
    workspace: &'a str,
    role: Role,
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
            workspace: &self.workspace,
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

/// Something that happened in a workspace without its owner deciding it then,
/// for the owner to see.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkspaceNotice {
    /// A person was admitted under the owner's [`PreAdmission`].
    PreAdmitted {
        /// The person.
        user_public_key: [u8; 32],
        /// The name they gave.
        name: String,
        /// The pre-admission's label.
        label: String,
        /// Their role.
        role: Role,
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
    pub role: Role,
    /// It stops being redeemable here.
    pub expires_at_ns: u64,
    /// The one person it names, when it names one: only a device of theirs
    /// redeems it.
    #[serde(default)]
    pub recipient: Option<[u8; 32]>,
    /// How it names that person.
    #[serde(default)]
    pub invitee_name: Option<String>,
}

/// One `/rooms` request. `workspace` is a workspace UUID. Each change answers with
/// the workspace as it is after it, except as noted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkspacesRequest {
    List,
    /// Create a governed workspace named `name` (1–80 characters).
    Create {
        name: String,
    },
    /// Admit the calling device to `workspace` as its owner.
    Enter {
        #[serde(rename = "room")]
        workspace: String,
    },
    /// Issue an invitation anyone holding it may redeem; it admits each
    /// device that redeems it, by this invitation's own grant.
    Invite {
        #[serde(rename = "room")]
        workspace: String,
        /// Caller-chosen idempotency key; never zero.
        request_id: [u8; 32],
        /// The role the invitation grants.
        role: Role,
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
        /// The one person the invitation names, by their user root, when the
        /// inviter knows who they are inviting: only a device of theirs
        /// redeems it, and a workspace taking people by invitation answers
        /// its door with it when they ask. Absent, the invitation is a link
        /// anyone holding it may redeem.
        #[serde(default)]
        recipient: Option<[u8; 32]>,
    },
    /// Cancel an invitation not yet redeemed: answered with
    /// [`WorkspacesReply::Pending`].
    Cancel {
        #[serde(rename = "room")]
        workspace: String,
        /// Caller-chosen idempotency key.
        request_id: [u8; 32],
        /// The invitation.
        invite_id: [u8; 32],
    },
    /// The invitations to `workspace` still redeemable.
    Pending {
        #[serde(rename = "room")]
        workspace: String,
    },
    /// Retired: an invitation admits by itself, so nobody waits for the
    /// owner's answer. It keeps the requests after it at their encoded
    /// positions.
    RetiredJoinRequests {},
    /// Retired with [`WorkspacesRequest::RetiredJoinRequests`].
    RetiredAdmit {},
    /// Retired with [`WorkspacesRequest::RetiredJoinRequests`].
    RetiredDecline {},
    /// Change a member's role.
    SetRole {
        #[serde(rename = "room")]
        workspace: String,
        /// Caller-chosen idempotency key.
        request_id: [u8; 32],
        /// The member.
        user_public_key: [u8; 32],
        role: Role,
    },
    /// Remove a member and every device of theirs.
    Remove {
        #[serde(rename = "room")]
        workspace: String,
        /// Caller-chosen idempotency key.
        request_id: [u8; 32],
        /// The member.
        user_public_key: [u8; 32],
    },
    /// Remove a person, when a member, and keep them out until unbanned.
    /// The owner, and a Moderator for members ranked below Moderator.
    Ban {
        #[serde(rename = "room")]
        workspace: String,
        /// Caller-chosen idempotency key.
        request_id: [u8; 32],
        /// The person.
        user_public_key: [u8; 32],
    },
    /// Let a banned person be admitted again.
    Unban {
        #[serde(rename = "room")]
        workspace: String,
        /// Caller-chosen idempotency key.
        request_id: [u8; 32],
        /// The person.
        user_public_key: [u8; 32],
    },
    /// Issue a single-use invitation that admits whoever redeems it with
    /// the token `grant` names, without the owner deciding again: answered
    /// with [`WorkspacesReply::Invitation`].
    PreAdmit {
        /// Caller-chosen idempotency key; never zero.
        request_id: [u8; 32],
        /// The owner's signed grant; its workspace is the workspace.
        grant: PreAdmission,
        /// How the inviting person names themself.
        inviter_name: Option<String>,
    },
    /// What happened in `workspace` without the owner deciding it then.
    Notices {
        #[serde(rename = "room")]
        workspace: String,
    },
    /// Whether the calling device's person may make an undoable change at
    /// `node` of `workspace`, as the workspace decides their writes there: answered
    /// with [`WorkspacesReply::Permits`]. A page asks before it offers what only
    /// some members may do, such as acting on what other people made.
    Permits {
        #[serde(rename = "room")]
        workspace: String,
        /// A node of the workspace's published tree, in its text form
        /// (`workspace:<id>/app:<app>/…`).
        node: String,
    },
    /// The addresses left at `workspace`'s door, oldest first: answered with
    /// [`WorkspacesReply::WaitList`]. Whoever may invite.
    WaitList {
        #[serde(rename = "room")]
        workspace: String,
    },
    /// Take an address left at `workspace`'s door off its wait list, when it
    /// is not to be invited: answered with [`WorkspacesReply::WaitList`]. An entry
    /// invited ([`WorkspacesRequest::Invite`]'s `wait_list_entry`) leaves the
    /// list by itself once someone joins with that invitation.
    Unlist {
        #[serde(rename = "room")]
        workspace: String,
        /// The entry.
        request_id: [u8; 32],
    },
    /// Mail the invitation `link` to the wait-list entry `wait_list_entry`
    /// it was issued for ([`WorkspacesRequest::Invite`]'s `wait_list_entry`):
    /// answered with [`WorkspacesReply::WaitList`], or refused where the workspace
    /// sends no mail, when the page shares the link itself.
    MailInvite {
        #[serde(rename = "room")]
        workspace: String,
        /// The confirmed entry.
        wait_list_entry: [u8; 32],
        /// The invitation's web link: the site's short link,
        /// `https://<site>/i/<handle>#<key>`, which keeps the invitation as
        /// ciphertext the site cannot read.
        link: String,
    },
    /// Share one record, or one item of it, with `workspace`'s members, as the
    /// calling device's person: answered with [`WorkspacesReply::Shared`], how
    /// many grants the share holds. The members below the owner get what
    /// `access` gives, never more than the sharer holds there.
    Share {
        #[serde(rename = "room")]
        workspace: String,
        /// The share's id, such as the message that shares it: 1 to 128
        /// letters, digits or `.` `_` `:` `-`.
        share: String,
        /// The record or item, as a node of the workspace's tree in its text
        /// form (`workspace:<id>/app:<app>/kind:<kind>/resource:<id>[/item:<id>]`;
        /// `resource:*` names an item of the app's own record).
        node: String,
        /// `view` or `edit`.
        access: String,
    },
    /// End the share `share` in `workspace`: answered with
    /// [`WorkspacesReply::Shared`], how many of its grants were revoked (its
    /// sharer's, or any for a person who may govern revoking).
    Unshare {
        #[serde(rename = "room")]
        workspace: String,
        share: String,
    },
    /// How the calling device's person names themself in `workspace` from now
    /// on, a display name ([`tilo_user_identity::display_name`]): the name
    /// the workspace's member list and every person list show for them.
    /// Answered with [`WorkspacesReply::Named`]. A page sends it as it joins and
    /// whenever its person renames themself.
    Name {
        #[serde(rename = "room")]
        workspace: String,
        name: String,
    },
    /// Have the host govern `workspace`, a workspace of the calling device's
    /// person, creating it as `name` (1–80 characters) unless the host
    /// governs it already, and admit the calling device to it as its owner
    /// ([`WorkspacesRequest::Enter`]): answered with the workspace. Whoever may create
    /// workspaces. A page places its person's workspace before it subscribes, so
    /// the workspace has governance like every other.
    Place {
        #[serde(rename = "room")]
        workspace: String,
        name: String,
    },
    /// Retired: a conversation is an ordinary record of the workspace. It
    /// keeps the requests after it at their encoded positions.
    RetiredPair {},
}

/// The answer to a [`WorkspacesRequest`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorkspacesReply {
    /// Every governed workspace.
    Workspaces(Vec<Workspace>),
    Workspace(Workspace),
    /// The issued invitation.
    Invitation {
        /// Digest of `signed_invitation`.
        invite_id: [u8; 32],
        /// What a device redeems.
        signed_invitation: Vec<u8>,
    },
    /// The redeemable invitations.
    Pending(Vec<PendingInvite>),
    /// Retired with [`WorkspacesRequest::RetiredJoinRequests`]; it keeps the
    /// replies after it at their encoded positions.
    RetiredAnsweredJoinRequests,
    /// Refused, and why.
    Refused(String),
    /// What happened without the owner deciding it then, oldest first.
    Notices(Vec<WorkspaceNotice>),
    /// Whether the caller may, for [`WorkspacesRequest::Permits`].
    Permits(bool),
    /// The requests left at the door, oldest first.
    WaitList(Vec<WaitListEntry>),
    /// How many grants a [`WorkspacesRequest::Share`] holds, or a
    /// [`WorkspacesRequest::Unshare`] revoked.
    Shared(u32),
    /// The workspace names the caller as asked, for [`WorkspacesRequest::Name`].
    Named,
    /// Retired with [`WorkspacesRequest::RetiredPair`]; it keeps the replies
    /// after it at their encoded positions.
    RetiredPaired,
}

/// What a host says of an invitation, or of a device it admitted with one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum JoinReply {
    /// What the invitation offers.
    Invitation {
        /// The workspace's UUID.
        #[serde(rename = "room")]
        workspace: String,
        /// Its relay workspace.
        workspace_id: u64,
        name: String,
        /// The role it grants.
        role: Role,
        /// It stops being redeemable here.
        expires_at_ns: u64,
        /// Who sent it, when the invitation names them.
        inviter: Option<Inviter>,
        /// How many people the workspace has, so the invitation says what
        /// someone joins, not only where.
        members: usize,
    },
    /// The workspace admitted the device.
    Admitted {
        /// The workspace's UUID.
        #[serde(rename = "room")]
        workspace: String,
        /// Its relay workspace.
        workspace_id: u64,
        name: String,
        role: Role,
    },
    /// Refused, and why.
    Refused(String),
}

// Requests left at the door. A workspace that takes people by invitation also
// keeps the requests of people who asked at its door without one, each an
// address to be invited at ([`WaitListEntry`]).

/// The longest address a request left at the door gives ([`WaitListEntry`]):
/// the longest an email address can be.
pub const MAX_CONTACT_CHARS: usize = 254;

/// `text` as the address a request left at a workspace's door keeps
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
/// in `/`): under it, either as an `invite=` parameter or as the site's own
/// short invitation (`i/<handle>#<key>`, the key in the fragment), no longer
/// than [`MAX_INVITATION_LINK`] and without spaces or control characters.
/// Only such a link is mailed to an address on a wait list for a member.
#[must_use]
pub fn invitation_on_door(door: &str, link: &str) -> bool {
    door.ends_with('/')
        && link.len() <= MAX_INVITATION_LINK
        && link
            .strip_prefix(door)
            .is_some_and(|rest| rest.contains("invite=") || short_invitation(rest))
        && !link.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// Whether `rest`, a link minus its door, is the site's short invitation:
/// `i/<handle>#<key>`, both in the site's own alphabet and nothing else.
fn short_invitation(rest: &str) -> bool {
    let Some((handle, key)) = rest
        .strip_prefix("i/")
        .and_then(|rest| rest.split_once('#'))
    else {
        return false;
    };
    let named = |part: &str, longest: usize| {
        !part.is_empty()
            && part.len() <= longest
            && part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    };
    named(handle, 64) && named(key, 128)
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
    if tilo_paths::language_of(tag) == "de" {
        "de"
    } else {
        "en"
    }
}

/// The bytes of the secret that takes a request left at a workspace's door off
/// its wait list again: random, handed only to whoever left it. The workspace
/// keeps only its SHA-256.
pub const WAIT_LIST_REMOVAL_BYTES: usize = 32;

/// A request left at the door of a workspace that takes people by invitation:
/// someone who has no invitation, and so no identity the workspace knows yet,
/// left an address to be invited at. Someone who may invite sends an
/// invitation of their own there ([`WorkspacesRequest::Invite`] naming the
/// entry), and the entry leaves the list once someone joins with it; or
/// they take it off the list ([`WorkspacesRequest::Unlist`]). Whoever left it
/// takes it off themselves with the secret they were handed
/// ([`WAIT_LIST_REMOVAL_BYTES`]). It names no person
/// or device: it is not anyone to admit, only an address to invite.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitListEntry {
    /// The entry, for [`WorkspacesRequest::Unlist`].
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
    fn a_mailed_invitation_is_on_the_door() {
        let door = "https://tilo.example/";
        assert!(invitation_on_door(
            door,
            "https://tilo.example/#/room?invite=signed-invitation"
        ));
        assert!(invitation_on_door(
            door,
            "https://tilo.example/i/AbCdEfGhIjKlMnOp#s3cr3tKeyS3cr3tKey"
        ));
        for elsewhere in [
            "https://tilo.example/i/#",
            "https://tilo.example/i/handle",
            "https://tilo.example/i/handle#",
            "https://tilo.example//i/handle#key",
            "https://tilo.example/i/han dle#key",
            "https://elsewhere.example/i/handle#key",
        ] {
            assert!(!invitation_on_door(door, elsewhere), "{elsewhere:?}");
        }
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
