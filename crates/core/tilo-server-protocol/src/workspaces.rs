//! The workspaces a host governs, as a page reaches them over HTTPS.
//!
//! One route below [`API_PREFIX`](crate::web::API_PREFIX):
//!
//! | Route | Session | Body | Answer |
//! |---|---|---|---|
//! | `/invitation` ([`crate::web::INVITATION_PATH`]) | no | the signed invitation (`Vec<u8>`) | [`JoinReply`] |
//!
//! [`WorkspacesRequest`] is not a route of its own: a page asks the host's
//! `workspace.administer` action over the one actions transport, and the host
//! answers each request in it as the workspace it names decides it. It
//! administers the host's workspaces for a device of the host's own user:
//! create a governed workspace, enter it as its owner, invite and cancel
//! invitations, change members' roles and remove them, and delete a workspace
//! of one's own with nobody else in it.
//! `/invitation` says what an invitation offers ([`JoinReply::Invitation`]) to
//! anyone holding it, before they have a device of their own to enter with: the
//! page shows it to someone following a link before it makes them a person.
//! Redeeming an invitation is an entry on the device's session
//! ([`crate::entering::Through::Invitation`]).

use collab_types::WorkspaceId;
use serde::{Deserialize, Serialize};
use tilo_actions::ActionAddress;
use tilo_actions::remote::{InvokeArgument, InvokeRequest, RemoteActionTarget};

/// What a member may do in a workspace.
pub use collab_types::{Member, Role};

/// A governed workspace.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspace {
    /// The workspace's UUID.
    pub workspace: String,
    /// The server workspace carrying it.
    pub workspace_id: WorkspaceId,
    pub name: String,
    /// Its committed governance generation.
    pub generation: u64,
    /// Every person governance records, removed ones included: a person's
    /// name is their recorded one, else their device's label; the owner's
    /// role is [`Role::Owner`], whether they are the workspace's structural
    /// owner or the person a host keeps it for; and the row of the person the
    /// asking device acts for carries `Member::is_self`.
    pub members: Vec<Member>,
    /// The holders of it, each as it names itself in its own signed address.
    #[serde(default)]
    pub holders: Vec<WorkspaceHolder>,
}

/// The roles and assignments of a workspace, requested independently of the roster.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRoles {
    pub custom_roles: std::collections::BTreeMap<String, String>,
    pub assigned_roles: std::collections::BTreeMap<String, std::collections::BTreeSet<String>>,
}

/// A holder of a workspace, as its own signed address names it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceHolder {
    /// Its device key, hex: what removing it may name it by.
    pub device: String,
    /// The person it serves, by their user public key, hex.
    pub person: String,
    /// What it calls itself: its server's name from its person's admin
    /// workspace, or its site's name. Its device's label when it names none.
    pub label: String,
    /// Whether it holds the workspace's keys: it reads the workspace. A store
    /// holds its ciphertext and reads none of it.
    pub reads: bool,
    /// Whether the machine under it is rented from a cloud provider, whose
    /// operator can read what it holds.
    pub cloud: bool,
}

/// The person who sent an invitation, as the host signed it into the
/// invitation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Inviter {
    /// Their user root.
    pub user_public_key: [u8; 32],
    /// The name the workspace knows them by, empty when it knows none: a
    /// surface showing the invitation names them their own way then, from
    /// their profile or in plain words, never by a key.
    pub name: String,
}

/// Domain of the owner's signature over a [`PreAdmission`].
pub const PRE_ADMISSION_DOMAIN: &[u8] = b"tilo/workspace-pre-admission/v1\0";

/// Longest label a pre-admission gives the person it is for.
pub const MAX_PRE_ADMISSION_LABEL_CHARS: usize = 128;

/// The workspace owner's lasting decision to admit, without deciding again,
/// whoever redeems the invitation issued with it and presents the one-time
/// token whose SHA-256 is `token_sha256`. Whoever hands out the token (a
/// backend that verified the person's email, say) never holds the workspace's
/// keys; the owner's user root signs the grant.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreAdmission {
    /// The workspace's UUID.
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
    /// workspace's entry, the device key of the host that answers it under
    /// the owner's listing (`crate::directory`).
    pub owner_user_public_key: [u8; 32],
    /// The signature of that key over [`PreAdmission::digest`].
    #[serde(with = "serde_bytes")]
    pub signature: [u8; 64],
}

#[derive(Serialize)]
struct PreAdmissionView<'a> {
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
    /// Someone reported the public workspace through its entry
    /// ([`crate::directory::EntryReport`]).
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
    /// It also makes whoever redeems it an administrator.
    #[serde(default)]
    pub administrator: bool,
}

/// One `/workspaces` request. `workspace` is a workspace UUID. Each change answers with
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
        workspace: String,
    },
    /// Issue an invitation anyone holding it may redeem; it admits each
    /// device that redeems it, by this invitation's own grant.
    Invite {
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
        /// its entry with it when they ask. Absent, the invitation is a link
        /// anyone holding it may redeem.
        #[serde(default)]
        recipient: Option<[u8; 32]>,
        /// The invitation also makes whoever redeems it an administrator: an
        /// Editor with every governance action. `role` is then Editor, and
        /// only someone who may make administrators issues one.
        #[serde(default)]
        administrator: bool,
    },
    /// Cancel an invitation not yet redeemed: answered with
    /// [`WorkspacesReply::Pending`].
    Cancel {
        workspace: String,
        /// Caller-chosen idempotency key.
        request_id: [u8; 32],
        /// The invitation.
        invite_id: [u8; 32],
    },
    /// The invitations to `workspace` still redeemable.
    Pending {
        workspace: String,
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
        workspace: String,
    },
    /// Whether the calling device's person may make an undoable change at
    /// `node` of `workspace`, as the workspace decides their writes there: answered
    /// with [`WorkspacesReply::Permits`]. A page asks before it offers what only
    /// some members may do, such as acting on what other people made.
    Permits {
        workspace: String,
        /// A node of the workspace's published tree, in its text form
        /// (`workspace:<id>/app:<app>/…`).
        node: String,
    },
    /// The addresses left at `workspace`'s entry, oldest first: answered with
    /// [`WorkspacesReply::WaitList`]. Whoever may invite.
    WaitList {
        workspace: String,
    },
    /// Take an address left at `workspace`'s entry off its wait list, when it
    /// is not to be invited: answered with [`WorkspacesReply::WaitList`]. An entry
    /// invited ([`WorkspacesRequest::Invite`]'s `wait_list_entry`) leaves the
    /// list by itself once someone joins with that invitation.
    Unlist {
        workspace: String,
        /// The entry.
        request_id: [u8; 32],
    },
    /// Mail the invitation `link` to the wait-list entry `wait_list_entry`
    /// it was issued for ([`WorkspacesRequest::Invite`]'s `wait_list_entry`):
    /// answered with [`WorkspacesReply::WaitList`], or refused where the workspace
    /// sends no mail, when the page shares the link itself.
    MailInvite {
        workspace: String,
        /// The confirmed entry.
        wait_list_entry: [u8; 32],
        /// The invitation's web link: the short link that names the
        /// invitation the workspace keeps at its host,
        /// `https://<site>/#/@<host>/w/<workspace>?invite=<secret>`.
        link: String,
    },
    /// Share one record, or one item of it, with `workspace`'s members, as the
    /// calling device's person: answered with [`WorkspacesReply::Shared`], how
    /// many grants the share holds. The members below the owner get what
    /// `access` gives, never more than the sharer holds there.
    Share {
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
        workspace: String,
        share: String,
    },
    /// How the calling device's person names themself in `workspace` from now
    /// on, a display name ([`tilo_user_identity::display_name`]): the name
    /// the workspace's member list and every person list show for them.
    /// Answered with [`WorkspacesReply::Named`]. A page sends it as it joins and
    /// whenever its person renames themself.
    Name {
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
        workspace: String,
        name: String,
    },
    /// The servers keeping `workspace`, which its owners alone choose:
    /// answered with [`WorkspacesReply::Servers`], the one asked first.
    Servers {
        workspace: String,
    },
    /// Have the server whose own line is `host` keep `workspace` as well:
    /// answered with [`WorkspacesReply::Servers`] after it took it, or
    /// refused in its own words. Only the workspace's owners choose its
    /// servers, and the other server takes it only for an owner it let in.
    AddServer {
        workspace: String,
        /// The other server's own `web-host.json` line, which its operator
        /// hands over: its certificate and where it is dialled.
        host: String,
    },
    /// Have a server keep `workspace` no more, named by the name the server
    /// list shows or by its device key: answered with
    /// [`WorkspacesReply::Servers`]. Only the workspace's owners choose its
    /// servers.
    RemoveServer {
        workspace: String,
        /// The server's name in the list, or its device key.
        server: String,
    },
    /// Delete `workspace`, with everything this server keeps of it: final,
    /// and answered with [`WorkspacesReply::Deleted`]. A workspace of the
    /// calling device's person, with nobody else in it, is theirs to delete.
    Delete {
        workspace: String,
    },
    /// Change a workspace-defined role or one member's assignment.
    EditRoles {
        workspace: String,
        request_id: [u8; 32],
        change: tilo_action_policy::Change,
    },
    /// Replace a role or member's rules on a workspace, app, or document.
    SetDocumentAccess {
        workspace: String,
        request_id: [u8; 32],
        /// The app with an exact document rule. Empty for a default rule.
        app: String,
        /// The exact document, an app id for its default, or empty for the workspace default.
        document: String,
        recipient: String,
        access: String,
    },
    /// Read this workspace's role definitions and member assignments.
    Roles {
        workspace: String,
    },
    /// Read the document access settings and their current values.
    DocumentAccess {
        workspace: String,
    },
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
    /// Refused, and why.
    Refused(String),
    /// What happened without the owner deciding it then, oldest first.
    Notices(Vec<WorkspaceNotice>),
    /// Whether the caller may, for [`WorkspacesRequest::Permits`].
    Permits(bool),
    /// The requests left at the entry, oldest first.
    WaitList(Vec<WaitListEntry>),
    /// How many grants a [`WorkspacesRequest::Share`] holds, or a
    /// [`WorkspacesRequest::Unshare`] revoked.
    Shared(u32),
    /// The workspace names the caller as asked, for [`WorkspacesRequest::Name`].
    Named,
    /// The servers keeping a workspace ([`WorkspacesRequest::Servers`],
    /// [`WorkspacesRequest::AddServer`] and [`WorkspacesRequest::RemoveServer`]):
    /// the one asked first.
    Servers(Vec<ServerEntry>),
    /// The workspace was deleted ([`WorkspacesRequest::Delete`]).
    Deleted,
    /// The current definition and assignments after an edit or query.
    Roles(WorkspaceRoles),
    DocumentAccess(DocumentAccess),
}

/// Documents and their access settings as an administrator sees them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentAccess {
    pub documents: Vec<(String, String)>,
    pub rules: Vec<collab_types::DocumentAccessRule>,
}

/// One server keeping a workspace, as a page lists it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerEntry {
    /// The server's device key, hex: what removing it may name it by.
    pub device: String,
    /// The person the server keeps the workspace for, by their user public key, hex.
    pub person: String,
    /// What the server calls itself: its site's name, or the name it gives its
    /// own admin workspace, never a key. Its device's label when it names none.
    #[serde(default)]
    pub label: String,
    /// Whether this is the server asked: the one the page is connected to.
    pub here: bool,
}

/// What a host says of an invitation, or of a device it admitted with one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum JoinReply {
    /// What the invitation offers.
    Invitation {
        /// The workspace's UUID.
        workspace: String,
        /// Its server workspace.
        workspace_id: WorkspaceId,
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
        /// It makes whoever accepts it an administrator besides its role.
        administrator: bool,
        /// It makes whoever accepts it an owner: an administrator the
        /// workspace is kept for.
        owner: bool,
        /// It was sent to someone who left their address at the workspace's
        /// entry to be let in ([`WaitListEntry`]): a person new to this server,
        /// whose page opens a workspace of their own first rather than the
        /// one the invitation admits them to.
        wait_list: bool,
    },
    /// The workspace admitted the device.
    Admitted {
        /// The workspace's UUID.
        workspace: String,
        /// Its server workspace.
        workspace_id: WorkspaceId,
        name: String,
        role: Role,
    },
    /// Refused, and why.
    Refused(String),
}

// Requests left at the entry. A workspace that takes people by invitation also
// keeps the requests of people who asked at its entry without one, each an
// address to be invited at ([`WaitListEntry`]).

/// The longest address a request left at the entry gives ([`WaitListEntry`]):
/// the longest an email address can be.
pub const MAX_CONTACT_CHARS: usize = 254;

/// `text` as the address a request left at a workspace's entry keeps
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

/// The longest invitation link an entry mails: an invitation carries its
/// signed certificate chain, a few KiB once encoded.
pub const MAX_INVITATION_LINK: usize = 16 * 1024;

/// Whether `link` is an invitation on the web entry `entry` (a URL ending
/// in `/`): under it as an `invite=` parameter (the invitation's own text, or
/// the secret of the one its workspace keeps), no longer than
/// [`MAX_INVITATION_LINK`] and without spaces or control characters.
/// Only such a link is mailed to an address on a wait list for a member.
#[must_use]
pub fn invitation_on_entry(entry: &str, link: &str) -> bool {
    entry.ends_with('/')
        && link.len() <= MAX_INVITATION_LINK
        && link
            .strip_prefix(entry)
            .is_some_and(|rest| rest.contains("invite="))
        && !link.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// What tells two addresses ([`contact_address`]) apart: lowercased, so
/// `Ada@Example.org` and `ada@example.org` are one request.
#[must_use]
pub fn contact_key(address: &str) -> String {
    address.trim().to_lowercase()
}

/// The language mails to an address left at the entry are in, for the
/// language tag its page was read in: `de` for German, otherwise `en`.
#[must_use]
pub fn mail_language(tag: &str) -> &'static str {
    if tilo_paths::language_of(tag) == "de" {
        "de"
    } else {
        "en"
    }
}

/// The bytes of the secret that takes a request left at a workspace's entry off
/// its wait list again: random, handed only to whoever left it. The workspace
/// keeps only its SHA-256.
pub const WAIT_LIST_REMOVAL_BYTES: usize = 32;

/// A request left at the entry of a workspace that takes people by invitation:
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

    tilo_test_env::scenario! {
        fn a_mailed_invitation_has_a_link_on_the_entry_and_one_address;

        phase a_mailed_invitation_is_on_the_entry;
        {
            let entry = "https://tilo.example/";
            assert!(invitation_on_entry(
                entry,
                "https://tilo.example/#/workspace?invite=signed-invitation"
            ));
            let secret = "s".repeat(43);
            assert!(invitation_on_entry(
                entry,
                &format!("https://tilo.example/#/@eu1.tilo.example/w/workspace?invite={secret}")
            ));
            for elsewhere in [
                "https://tilo.example/",
                "https://tilo.example/#/@eu1.tilo.example/w/workspace",
                "https://tilo.example/#/w/workspace?invite=sec ret",
                "https://elsewhere.example/#/w/workspace?invite=secret",
            ] {
                assert!(!invitation_on_entry(entry, elsewhere), "{elsewhere:?}");
            }
        }

        phase a_contact_is_one_email_address_however_it_was_typed;
        {
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
}

// ---------------------------------------------------------------------------
// The host's own workspace operations, as the actions a server serves them by.
//
// A server serves what it governs as actions addressed to its own device:
// a workspace's own governance (provider `tilo.workspace`, named in
// `tilo-actions`), and the host's own business, which no workspace decides
// (creating, listing and entering workspaces, letting an app author a
// workspace), as provider [`HOST_ACTIONS_PROVIDER`], to the devices of the
// server's own person only, on a workspace they hold there.
//
// What a member does for themselves (their effect receipts, their shares,
// the empty record of an app they open first) their own device writes into
// the workspace; no server is asked for it.
//
// These are postcard wire formats between separately updated hosts and
// desktops: field and variant order are the wire, so append only.
use tilo_workspace_join::MemberId;

/// The provider a server answers its host operations under: creating,
/// listing and entering the workspaces it governs. Only the devices of the
/// server's own person are offered them, and invoking one is decided as
/// every action API decides a host operation.
pub const HOST_ACTIONS_PROVIDER: &str = tilo_actions::policy::agent_answers::ACCOUNT_APP;
/// The one resource the host operations are published on.
pub const HOST_WORKSPACES_RESOURCE: &str = "host.workspaces";
/// Serve this server's own web UI (`serving`), the pages its members and their guests
/// open in a browser, or stop serving it: `false` stops it at once, without a restart,
/// and `true` serves it again where this process can. On by default. The choice is the
/// server's own, kept in its entry among its person's servers, and any device of that
/// person may change it. Answers `{"serving": serving}`.
pub const WEB_SERVE: &str = "web.serve";
/// Create a governed workspace. Arguments: `name`, and the terms of its first
/// invitation when one is wanted (`invite_role`, `invite_recipient`,
/// `invitee_name`, `invite_expires_in_seconds`). Answers a [`CreatedWorkspace`].
pub const CREATE_WORKSPACE: &str = "workspace.create";
/// Make the one workspace of the caller's own person on a host that keeps
/// workspaces of people's own, or answer theirs again: any person's device
/// may ask. Arguments: `name` (the new workspace's; unused once they have
/// one), `language` (the language one they do not name is named in, a tag such
/// as `de-DE`; English when omitted), `workspace` (the identity of a workspace
/// that moves here as theirs; a different one than theirs is refused with a
/// `tilo_actions::remote::server_account::AdminRefusal`), and `expires_in_seconds`
/// (how long the invitation stays redeemable; 600 when omitted). Answers a
/// [`CreatedWorkspace`] with the invitation the person's devices redeem to
/// hold it, as its administrator.
pub const CREATE_OWN_WORKSPACE: &str = "workspace.create-own";
/// Branch `workspace`, one of the caller's person's own on a host that keeps
/// workspaces of people's own: a new workspace of theirs, named `name` and
/// labelled `labels` when given, that starts with every record of `workspace`
/// with its history and the content those records name, and goes on apart
/// from it. What is tied to other people stays behind
/// (`tilo_desktop_model::branch::stays`), and the person is its only member.
/// `branch` is its identity (a UUID; a new one when omitted): asked again
/// under the same one, the host answers the branch it made and copies
/// nothing twice. All or nothing: a branch that could not take everything is
/// deleted again and the refusal names what did not come
/// ([`AdminRefusal::BranchNotMade`]). Only a person's
/// device, or an agent's grant reaching all their workspaces, branches; a
/// workspace that is not theirs is refused with [`AdminRefusal::NotYourOwn`]. Answers a
/// [`CreatedWorkspace`] with the invitation the person's devices redeem to
/// hold the branch (redeemable `expires_in_seconds`, 600 when omitted).
pub const BRANCH_OWN_WORKSPACE: &str = "workspace.branch-own";
/// Every workspace the host governs, each as a [`WorkspaceListing`].
pub const LIST_WORKSPACES: &str = "workspace.list";
/// Delete the workspace `workspace`, one of the caller's person's own (any
/// workspace this host governs, for the host's own person), with everything
/// the host keeps of it: its records, history, files and the storage they
/// took. Everyone in it is told it was deleted
/// ([`tilo_server_protocol::refusal::WORKSPACE_DELETED`]) and let in no more.
/// Final: there is no undo. Only a person's device, or an agent's grant
/// reaching all their workspaces, deletes; a workspace that is not theirs is
/// refused with [`AdminRefusal::NotYourOwn`]. Answers `{"deleted": workspace}`.
pub const DELETE_WORKSPACE: &str = "workspace.delete";
/// The workspace's governance verb (on its [`tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER`]
/// resource) that turns running commands unattended on or off: `{"on": bool}`.
/// On, a key of the workspace's owner that reaches this workspace alone runs
/// commands in its machine without the person being asked each time; a key
/// reaching all their workspaces is still asked. Off by default. Turning it on
/// asks the person on their approving devices, whoever calls it, and waits for
/// their answer; turning it off asks nothing. It is the owner's alone: in a
/// workspace that is not their own it is refused with [`AdminRefusal::NotYourOwn`].
/// Answers `{"workspace": uuid, "unattended": bool}`.
pub const RUN_UNATTENDED: &str = "commands.unattended";
/// The workspace's governance verb recording a key revoked: the caller's own or one it minted.
pub const REVOKE_KEY: &str = "key.revoke";
/// The workspace's own verb listing the webhooks the calling key registered: each as it
/// was made, without the key that signs its deliveries. Answers `{"webhooks": [...]}`.
pub const LIST_WEBHOOKS: &str = "webhook.list";
/// The workspace's own verb registering a webhook of the calling key's:
/// `{url, events?, path?}`. It is a record of the workspace, whose trigger posts the kinds
/// named (`change`, `invocation`, `presence`) to `url`; the device that keeps the workspace
/// posts and signs. Answers `{id, workspace, url, events, path?, key}`.
pub const CREATE_WEBHOOK: &str = "webhook.create";
/// The workspace's own verb removing a webhook the calling key registered:
/// `{webhook}`. Answers `{"deleted": webhook}`.
pub const DELETE_WEBHOOK: &str = "webhook.delete";
/// Why the host refused one of its own workspace operations (asking for,
/// branching or deleting a workspace, or turning a workspace's commands
/// unattended on): what the refusal's words carry as JSON
/// ([`AdminRefusal::to_reason`]), so a caller decides on the case without reading
/// them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "snake_case")]
pub enum AdminRefusal {
    /// The workspace is not one of the caller's own.
    NotYourOwn,
    /// A branch ([`BRANCH_OWN_WORKSPACE`]) could not take everything of its
    /// source: none was made, and `part` names what did not come (`the files
    /// its records name`, `the record plan of so.tilo.notes`).
    BranchNotMade { part: String },
}

impl AdminRefusal {
    /// The refusal as an action's refusal reason
    /// ([`tilo_actions::remote::ActionCallError::Refused`]): its JSON, which
    /// [`Self::from_reason`] reads back typed and which a caller that does
    /// not read it still shows as plain data.
    #[must_use]
    pub fn to_reason(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| self.to_string())
    }

    /// The refusal an action's refusal reason carries, when it carries one
    /// ([`Self::to_reason`]).
    #[must_use]
    pub fn from_reason(reason: &str) -> Option<Self> {
        serde_json::from_str(reason).ok()
    }
}

impl std::fmt::Display for AdminRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotYourOwn => f.write_str("that workspace is not one of your own"),
            Self::BranchNotMade { part } => {
                write!(f, "the branch was not made: {part} did not come")
            }
        }
    }
}
/// Admit the calling device to `workspace` as one of its owner's. Answers
/// an [`EnteredWorkspace`]: the invitation the device redeems to hold the workspace,
/// restricted to its person and redeemable `expires_in_seconds` (600 when
/// omitted).
pub const ENTER_WORKSPACE: &str = "workspace.enter";
/// Administer this host's workspaces from a page: the request in argument
/// `request` (a [`WorkspacesRequest`]), answered with the [`WorkspacesReply`]
/// under `reply`. The asking device's certificate decides every request as the
/// workspace's governance decides it, exactly as the host answers a page's own
/// session: this is that same administration as one action of the host's own,
/// so a page asks it over the one actions transport rather than a request kind
/// of its own.
pub const ADMINISTER_WORKSPACES: &str = "workspace.administer";

/// Grant the asking device a token for the host's fetch proxy: the bearer
/// token a page sends with each request it makes through its host
/// ([`FETCH_PATH`](crate::web::FETCH_PATH), as `tilo_net::proxy` names it).
/// Asked in a workspace the page is in, as one of that workspace's actions:
/// its roster decides whether the device may ask, and the host answers
/// `{"token": "<hex>"}` ([`fetch_grant_answer`]) naming that device, or
/// `{"token": null}` when it serves no proxy.
pub const FETCH_GRANT: &str = "workspace.fetch-grant";

/// The scope a device with no certificate from its host yet asks the entry in: it names no
/// workspace, and the host answers nothing there but [`ENTER_ENTRY`].
pub const ENTRY_SCOPE: &str = "workspace.entry";

/// The record of the person's own workspace their list of workspaces is kept in: a
/// [`WorkspaceList`](tilo_user_identity::WorkspaceList), which their devices and every host of
/// theirs read and change through its actions, so a device reads the list from its own
/// workspace's link rather than being sent it.
pub const IDENTITY_LIST: (&str, &str) = (
    tilo_user_identity::workspace_list::APP,
    tilo_user_identity::workspace_list::RESOURCE,
);

/// The app a workspace keeps its members' memberships in, under [`member_record`].
pub const MEMBERSHIP_APP: &str = "so.tilo.membership";

/// The record of `person`'s membership of a workspace: the role they hold there and when the
/// workspace was made.
#[must_use]
pub fn member_record(person: &[u8; 32]) -> String {
    format!("member.{}", hex::encode(person))
}

/// The record of what a workspace's owner alone keeps of it: the labels they gave it and
/// whether keys reaching it alone run commands in its machine unasked.
pub const OWNERSHIP: (&str, &str) = (tilo_action_policy::roles::OWNER_ONLY_APP, "ownership");

/// The role a person holds in a workspace of their own.
pub const OWNER_ROLE: &str = "owner";

/// The person's list of their workspaces ([`IDENTITY_LIST`]), as read from that record through
/// the record API: each workspace by its id and the name the person last saw it by, in their
/// order. What else a list shows of a workspace is that workspace's own records: its membership
/// ([`MEMBERSHIP_APP`]) for the role and when it was made, and its owner's ownership record for
/// the labels and whether it runs unattended.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonWorkspaces {
    pub workspaces: Vec<ListedWorkspace>,
}

/// One workspace of the person's list, as a reader of the record shows it: the entry their own
/// workspace keeps, joined with the membership the workspace holds of them and its owner's
/// labels.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListedWorkspace {
    /// The workspace's UUID.
    pub workspace: String,
    /// The name the person last saw it by.
    pub name: String,
    /// Where it is hosted, as the list keeps it: `None` while no device of theirs said how to
    /// reach it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<KeepingDeviceAddress>,
    /// The part the person takes in it, as the workspace's membership says: `owner` for one of
    /// their own, else the role they hold.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// What its owner labelled it with, where the reader may read it.
    #[serde(default, skip_serializing_if = "WorkspaceLabels::is_empty")]
    pub labels: WorkspaceLabels,
    /// When the workspace was made, ms since the epoch, as its membership says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub made_at_ms: Option<i64>,
    /// Whether its owner let keys made for it alone run commands in its machine without asking
    /// them each time.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unattended: bool,
    /// The device the list says keeps it, by key, until the person's devices name it; only
    /// [`PersonWorkspaces::with_hosts`] reads it.
    #[serde(skip)]
    pub host_device: Option<String>,
    /// Where that device says it is reached, postcard in hex, until the reader is given its host.
    #[serde(skip)]
    pub host_hint: Option<String>,
}

/// What a membership record's read holds: the role and when the workspace was made.
#[derive(Default, serde::Deserialize)]
struct MembershipRead {
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    made_at_ms: Option<i64>,
}

/// What an ownership record's read holds: the owner's labels and whether it runs unattended.
#[derive(Default, serde::Deserialize)]
struct OwnershipRead {
    #[serde(default)]
    labels: WorkspaceLabels,
    #[serde(default)]
    unattended: bool,
}

impl PersonWorkspaces {
    /// The list a whole-document read answered (`{"revision", "records": [{..., "document"}]}`):
    /// `None` while the record is still arriving, an empty list where the person's workspace
    /// holds none yet.
    ///
    /// # Errors
    /// The answer names no reading of the list, or its document does not read.
    pub fn of_readings(readings: &serde_json::Value) -> Result<Option<Self>, String> {
        let record = readings
            .get("records")
            .and_then(serde_json::Value::as_array)
            .and_then(|records| records.first())
            .ok_or("the read answered no record")?;
        if record.get("arriving").and_then(serde_json::Value::as_bool) == Some(true) {
            return Ok(None);
        }
        if record.get("absent").and_then(serde_json::Value::as_bool) == Some(true) {
            return Ok(Some(Self::default()));
        }
        let bytes = record
            .get("document")
            .and_then(serde_json::Value::as_str)
            .and_then(|document| hex::decode(document).ok())
            .ok_or("the read answered no document")?;
        let list = tilo_user_identity::WorkspaceList::of_document(&bytes)?;
        Ok(Some(Self {
            workspaces: list
                .listed()
                .map(|(workspace, listed)| ListedWorkspace {
                    workspace: workspace.to_string(),
                    name: listed.name.clone(),
                    host: None,
                    role: None,
                    labels: WorkspaceLabels::default(),
                    made_at_ms: None,
                    unattended: false,
                    host_device: Some(listed.host.clone()).filter(|host| !host.is_empty()),
                    host_hint: listed.address().map(hex::encode),
                })
                .collect(),
        }))
    }

    /// Name each workspace's host from the person's devices record (`names`, by device key):
    /// the reader is answered where it is reached, and a host that is `answering`, the device
    /// the read was asked of, is reached there and names no address.
    pub fn with_hosts(
        &mut self,
        names: &std::collections::BTreeMap<String, String>,
        answering: Option<&[u8; 32]>,
    ) {
        let answering = answering.map(hex::encode);
        for entry in &mut self.workspaces {
            let Some(device) = entry.host_device.clone() else {
                continue;
            };
            let name = names
                .get(&device)
                .cloned()
                .unwrap_or_else(|| device.clone());
            let hint = if Some(&device) == answering.as_ref() {
                None
            } else {
                entry.host_hint.clone()
            };
            entry.host = Some(KeepingDeviceAddress { name, hint });
        }
    }

    /// The revision a record read answered, which the next read of it follows past.
    #[must_use]
    pub fn revision_of(readings: &serde_json::Value) -> Option<String> {
        readings
            .get("revision")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    }

    /// Fill `workspace`'s role and when it was made from a read of its membership record
    /// (`{"records": [{..., "value"}]}`): a record the workspace keeps none of leaves it as it
    /// was, as a workspace this device reaches nowhere does.
    pub fn with_membership(&mut self, workspace: &str, reading: &serde_json::Value) {
        let Some(entry) = self
            .workspaces
            .iter_mut()
            .find(|one| one.workspace == workspace)
        else {
            return;
        };
        let Ok(facts) =
            serde_json::from_value::<MembershipRead>(crate::devices::record_value(reading))
        else {
            return;
        };
        entry.role = facts.role;
        entry.made_at_ms = facts.made_at_ms;
    }

    /// Fill `workspace`'s labels and whether it runs unattended from a read of its ownership
    /// record: one the reader may not read leaves both as they were.
    pub fn with_ownership(&mut self, workspace: &str, reading: &serde_json::Value) {
        let Some(entry) = self
            .workspaces
            .iter_mut()
            .find(|one| one.workspace == workspace)
        else {
            return;
        };
        let Ok(facts) =
            serde_json::from_value::<OwnershipRead>(crate::devices::record_value(reading))
        else {
            return;
        };
        entry.labels = facts.labels;
        entry.unattended = facts.unattended;
    }
}

impl ListedWorkspace {
    /// Where a device of the person's that does not hold it enters it: the signed address the
    /// list keeps for its host, checked against the host's certificate.
    ///
    /// # Errors
    /// No device of theirs said where it is kept, or the address does not read.
    pub fn entered_at(&self) -> Result<tilo_workspace_join::TransportHint, String> {
        let host = self
            .host
            .as_ref()
            .ok_or("no device of yours says where it is kept yet")?;
        host.address()?;
        let hint = host
            .hint
            .as_deref()
            .ok_or("the host that answered keeps it: it is reached there")?;
        postcard::from_bytes(&hex::decode(hint).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())
    }
}

/// Enter a workspace from a device that is not a member of it yet: arguments `entering` (an
/// [`Entering`](crate::entering::Entering)) and `cert` (this device's certificate, whose key it
/// signs the request with), answered with the
/// [`Entered`](crate::entering::Entered) under `entered`. Asked in [`ENTRY_SCOPE`], before the
/// device holds a certificate from the host: the host verifies the certificate and admits the
/// device, and the device then joins the workspace's link.
pub const ENTER_ENTRY: &str = "workspace.entry";
/// The revocations of the host's person the host enforces, as the person's
/// devices announced them: a JSON array of root-signed
/// `tilo_user_identity::RevocationEntry`. What a service that signs the
/// person in checks a device against, from the same list the host's own
/// handshakes read.
pub const LIST_REVOCATIONS: &str = "identity.revocations";

/// Hold a workspace another host holds, as that holder asks: arguments
/// `workspace`, `workspace_id`, `holder_hint` (the handing holder's
/// signed address) and `bootstrap` (the workspace as a joining member gets it,
/// the host's device admitted), each signed value postcard in hex. The
/// hand-over carries the owner's change naming the host among the workspace's
/// holders, and a hand-over without it is refused. Asked with `workspace`,
/// `workspace_id` and `role` alone, it takes nothing in and answers the
/// address the host signs for the workspace. Only a certified host serves
/// workspaces this way. Answers a [`HostedWorkspace`].
pub const HOST_WORKSPACE: &str = "workspace.host";
/// Let `workspace` go, as its owners ask. Answers `{"stopped": bool}`.
pub const UNHOST_WORKSPACE: &str = "workspace.unhost";
/// Take in what the store of the asking device, one the workspace names among
/// those keeping it, keeps of `workspace` and this host's store lacks, over the
/// workspace's device link: what a device that hands a workspace over, or lets
/// it go, waits for before it does. Arguments `workspace`, `names_content`,
/// and the content the asking device keeps none of itself: listed in
/// `absent`, or, with `absent_parts`, as it handed the list before with
/// [`CATCH_UP_ABSENT`]. Answers a [`CaughtUpWorkspace`]: with `cursor_pages`,
/// its cursors in pages, the first here and the rest from
/// [`CATCH_UP_STANDING`].
pub const CATCH_UP_WORKSPACE: &str = "workspace.catch-up";
/// Hand part of the list of content the asking device, one keeping
/// `workspace`, keeps none of itself (`absent`), which its next
/// [`CATCH_UP_WORKSPACE`] with `absent_parts` counts as the whole list. With
/// `first` the part starts the list afresh, and an empty first part lets the
/// list go. A list too long for one ask's frame goes in parts, each well
/// within it. Answers `{"kept": count}`, how many addresses the list holds.
pub const CATCH_UP_ABSENT: &str = "workspace.catch-up.absent";
/// Where the answering host's store of `workspace` stands on the lineages
/// after `after` (a lineage, postcard in hex), as the device keeping it that
/// asked [`CATCH_UP_WORKSPACE`] reads the rest of its answer's cursors: a
/// store keeps a lineage per record and author, more than one frame carries.
/// Answers a [`StandingPage`].
pub const CATCH_UP_STANDING: &str = "workspace.catch-up.standing";
/// The most devices a person's record keeps.
pub const MAX_PERSONAL_DEVICES: usize = 1024;

/// Where a workspace is hosted, as a device dials it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeepingDeviceAddress {
    /// The host as people know it: a server's name (`eu1.tilo.sh`), or the
    /// label of the person's device that hosts it.
    pub name: String,
    /// The host's own signed address for the workspace
    /// (`tilo_peer_state::join::TransportHint`), postcard in hex: what a
    /// device dials, checked against the host's certificate it carries.
    /// `None` for a workspace the host that answered the list keeps itself:
    /// the reader reaches it where it asked, and a person with hundreds of
    /// them there reads their list in one answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

impl KeepingDeviceAddress {
    /// The address `hint` (a `TransportHint`, postcard) gives, named as its
    /// host's certificate names it; `None` for a hint that is no signed
    /// workspace address.
    #[must_use]
    pub fn of_hint(hint: &[u8]) -> Option<Self> {
        let decoded: tilo_workspace_join::TransportHint = postcard::from_bytes(hint).ok()?;
        tilo_workspace_join::verify_transport_hint(&decoded).ok()?;
        Some(Self {
            name: decoded.host.device_label,
            hint: Some(hex::encode(hint)),
        })
    }

    /// The signed address this names, checked against its host's certificate.
    ///
    /// # Errors
    /// It is no signed workspace address.
    pub fn address(&self) -> Result<HostedDevice, String> {
        let hint = self
            .hint
            .as_deref()
            .ok_or("the host that answered keeps it: it is reached there")?;
        let bytes = hex::decode(hint).map_err(|error| error.to_string())?;
        let decoded: tilo_workspace_join::TransportHint =
            postcard::from_bytes(&bytes).map_err(|error| error.to_string())?;
        tilo_workspace_join::verify_transport_hint(&decoded)?;
        let payload: HintPayload = serde_json::from_str(&decoded.opaque_json)
            .map_err(|error| format!("the signed address does not read: {error}"))?;
        Ok(HostedDevice {
            workspace_id: payload.workspace_id,
            endpoint_addr: payload.endpoint_addr,
            device_certificate: decoded.host,
        })
    }
}

/// What a signed workspace address names, as a device dials the holder it points at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostedDevice {
    /// The server workspace the holder serves.
    pub workspace_id: WorkspaceId,
    /// The endpoint the holder answers on for it.
    pub endpoint_addr: iroh_base::EndpointAddr,
    /// The root-certified identity of the device the address pins.
    pub device_certificate: tilo_user_identity::DeviceCert,
}

/// The fields of a signed address payload this module reads: the holder's own payload may name
/// more, and a member of it reads none of it.
#[derive(serde::Deserialize)]
struct HintPayload {
    workspace_id: WorkspaceId,
    endpoint_addr: iroh_base::EndpointAddr,
}

/// What [`HOST_WORKSPACE`] answers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostedWorkspace {
    /// The serving host's own signed address for the workspace, postcard in hex.
    pub host_hint: String,
}

/// What [`CATCH_UP_WORKSPACE`] answers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaughtUpWorkspace {
    /// Where the answering host's store of the workspace stands once it kept
    /// what it took in: a cursor per author's line, postcard in hex.
    pub cursors: String,
    /// How many blobs of the content the workspace's files name the answering
    /// host still lacks, kept apart from the writes that name them.
    #[serde(default)]
    pub content_missing: u32,
    /// Whether the answering host took in content while it answered. What it
    /// lacks is counted over the resources that open there, which grow while
    /// it takes in the workspace, so a count above the last answer's can still
    /// follow content it fetched.
    #[serde(default)]
    pub fetched: bool,
    /// Whether the store stands on more lineages than `cursors` names: the
    /// rest are read with [`CATCH_UP_STANDING`], after the last one named.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub more_cursors: bool,
}

/// What [`CATCH_UP_STANDING`] answers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StandingPage {
    /// Where the answering host's store stands on the lineages after the one
    /// asked for, in lineage order: a cursor per lineage, postcard in hex.
    pub cursors: String,
    /// Whether it stands on more lineages after the last one named.
    pub more: bool,
}

/// One workspace a server governs, as its host actions answer it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceListing {
    /// The workspace's UUID.
    pub workspace: String,
    /// The server workspace carrying the workspace.
    pub workspace_id: WorkspaceId,
    /// The workspace's name.
    pub name: String,
    /// The workspace's committed governance generation.
    pub generation: u64,
    /// The part the asking device's person takes in it, when the answer is a
    /// person's listing: `owner` for a workspace of their own, else the role
    /// they hold (`viewer`, `commenter`, `editor`, `moderator`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// What its owner labelled it with, answered to the owner alone: a
    /// member invited in reads none.
    #[serde(default, skip_serializing_if = "WorkspaceLabels::is_empty")]
    pub labels: WorkspaceLabels,
    /// Whether its owner let keys made for it alone run commands in its
    /// machine without asking them each time ([`RUN_UNATTENDED`]), answered
    /// to the owner alone.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unattended: bool,
    /// When the workspace was made, ms since the epoch: the time of the first
    /// change its governance holds, which a later change does not move. Two
    /// workspaces of one name are told apart by it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub made_at_ms: Option<i64>,
}

/// What a person labels a workspace of their own with, for their own
/// programs to find it by: `customer_id` → `cus_8121`. A backend that keeps
/// a workspace per customer names each with its own id for that customer,
/// and lists, finds and deletes them by it. Only the workspace's owner sets
/// or reads them; they are kept by the host beside whose workspace it is,
/// not in the workspace, so nobody invited into it sees them.
pub type WorkspaceLabels = std::collections::BTreeMap<String, String>;

/// Whether a workspace labelled `labels` carries every one of `wanted`: any
/// workspace does for none.
#[must_use]
pub fn labelled(labels: &WorkspaceLabels, wanted: &WorkspaceLabels) -> bool {
    wanted
        .iter()
        .all(|(name, value)| labels.get(name) == Some(value))
}

/// The most labels one workspace keeps.
pub const MAX_WORKSPACE_LABELS: usize = 16;
/// The longest a label's name is, in bytes.
pub const MAX_LABEL_NAME_BYTES: usize = 63;
/// The longest a label's value is, in characters.
pub const MAX_LABEL_VALUE_CHARS: usize = 256;

/// `labels`, if every one is a label a workspace keeps: at most
/// [`MAX_WORKSPACE_LABELS`], each name 1 to [`MAX_LABEL_NAME_BYTES`] of
/// `a-z`, `0-9`, `_`, `-` and `.`, starting with a letter or digit, each value
/// 1 to [`MAX_LABEL_VALUE_CHARS`] characters with no control characters.
///
/// # Errors
///
/// Why one is not, said so a caller can fix it.
pub fn check_labels(labels: &WorkspaceLabels) -> Result<(), String> {
    if labels.len() > MAX_WORKSPACE_LABELS {
        return Err(format!(
            "a workspace keeps at most {MAX_WORKSPACE_LABELS} labels, not {}",
            labels.len()
        ));
    }
    for (name, value) in labels {
        let named = !name.is_empty()
            && name.len() <= MAX_LABEL_NAME_BYTES
            && name
                .bytes()
                .next()
                .is_some_and(|first| first.is_ascii_lowercase() || first.is_ascii_digit())
            && name.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_-.".contains(&byte)
            });
        if !named {
            return Err(format!(
                "the label name `{name}` is not 1 to {MAX_LABEL_NAME_BYTES} of a-z, 0-9, `_`, `-` and `.`, starting with a letter or digit"
            ));
        }
        let count = value.chars().count();
        if count == 0 || count > MAX_LABEL_VALUE_CHARS || value.chars().any(char::is_control) {
            return Err(format!(
                "the label `{name}` has no value of 1 to {MAX_LABEL_VALUE_CHARS} printable characters"
            ));
        }
    }
    Ok(())
}

/// An invitation a server's workspace action issued, as it answers it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionInvitation {
    /// Digest of the signed invitation, 64 hex characters.
    pub invite: String,
    /// The signed invitation a device redeems, in hex.
    pub invitation: String,
}

impl ActionInvitation {
    #[must_use]
    pub fn of(issued: &IssuedInvitation) -> Self {
        Self {
            invite: hex::encode(issued.invite_id),
            invitation: hex::encode(&issued.signed_invitation),
        }
    }

    /// The invitation's id and its signed bytes.
    pub fn decode(&self) -> Result<IssuedInvitation, String> {
        let mut invite_id = [0; 32];
        hex::decode_to_slice(&self.invite, &mut invite_id)
            .map_err(|_| "the server named an invalid invitation id".to_owned())?;
        let signed_invitation = hex::decode(&self.invitation)
            .map_err(|_| "the server sent an invalid invitation".to_owned())?;
        Ok(IssuedInvitation {
            invite_id,
            signed_invitation,
        })
    }
}

/// What [`CREATE_WORKSPACE`] answers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreatedWorkspace {
    #[serde(flatten)]
    pub workspace: WorkspaceListing,
    /// Its first invitation, when one was asked for.
    #[serde(default)]
    pub invitation: Option<ActionInvitation>,
}

/// What [`ENTER_WORKSPACE`] answers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnteredWorkspace {
    /// Whether governance changed to admit the caller.
    pub changed: bool,
    /// The invitation the caller redeems to hold the workspace.
    pub invitation: ActionInvitation,
}

pub use tilo_workspace_join::invitations::{
    CancelInvitationRequest, IssueInvitationRequest, IssuedInvitation, PendingInvitation,
};

/// Change a member's role.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetMemberRoleRequest {
    /// Caller-chosen idempotency key.
    pub request_id: [u8; 32],
    /// The member's person.
    pub user_public_key: [u8; 32],
    /// Their new role.
    pub role: Role,
}

/// Remove a member and every device of theirs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoveMemberRequest {
    /// Caller-chosen idempotency key.
    pub request_id: [u8; 32],
    /// The member's person.
    pub user_public_key: [u8; 32],
}

/// A governed workspace as its administrators see it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceSummary {
    /// The workspace's UUID.
    pub workspace: String,
    /// The server workspace carrying the workspace.
    pub workspace_id: WorkspaceId,
    /// The workspace's name.
    pub name: String,
    pub generation: u64,
    /// Every person the workspace has — its owner and everyone its governance
    /// admitted — removed ones included.
    pub members: Vec<Member>,
    /// The devices each member holds now.
    pub devices: std::collections::BTreeMap<MemberId, Vec<[u8; 32]>>,
}

impl WorkspaceSummary {
    /// How many people the workspace has now: everyone it has not removed.
    #[must_use]
    pub fn people(&self) -> usize {
        self.members.iter().filter(|member| !member.removed).count()
    }
}

/// The [`ADMINISTER_WORKSPACES`] ask for `request`, under `invocation`: what a
/// page sends on the host's one actions transport instead of asking the host a
/// request kind of its own.
///
/// # Errors
/// The request does not encode.
pub fn administer_invoke(
    request: &WorkspacesRequest,
    invocation: tilo_actions::InvocationId,
) -> Result<InvokeRequest, String> {
    Ok(InvokeRequest {
        invocation,
        target: RemoteActionTarget {
            provider: HOST_ACTIONS_PROVIDER.to_owned(),
            address: ActionAddress::new(HOST_WORKSPACES_RESOURCE.to_owned(), ADMINISTER_WORKSPACES),
        },
        argument: InvokeArgument::Json(administer_arguments(request)?),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    })
}

/// The arguments [`ADMINISTER_WORKSPACES`] carries `request` as.
///
/// # Errors
/// The request does not encode.
pub fn administer_arguments(
    request: &WorkspacesRequest,
) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    let request = serde_json::to_value(request).map_err(|error| error.to_string())?;
    Ok(serde_json::Map::from_iter([(
        "request".to_owned(),
        request,
    )]))
}

/// The request [`ADMINISTER_WORKSPACES`] was asked with, read from the action's
/// JSON arguments.
///
/// # Errors
/// The arguments name no request, or it does not decode.
pub fn administered_request(
    arguments: &serde_json::Map<String, serde_json::Value>,
) -> Result<WorkspacesRequest, String> {
    let request = arguments
        .get("request")
        .cloned()
        .ok_or("the action names no request")?;
    serde_json::from_value(request).map_err(|error| error.to_string())
}

/// The answer [`ADMINISTER_WORKSPACES`] gives `reply` as: the reply under
/// `reply`, which [`administered_reply`] reads back.
///
/// # Errors
/// The reply does not encode.
pub fn administered_answer(reply: &WorkspacesReply) -> Result<serde_json::Value, String> {
    let reply = serde_json::to_value(reply).map_err(|error| error.to_string())?;
    Ok(serde_json::json!({ "reply": reply }))
}

/// The reply [`ADMINISTER_WORKSPACES`] answered `value` with.
///
/// # Errors
/// The answer carries no reply, or it does not decode.
pub fn administered_reply(value: &serde_json::Value) -> Result<WorkspacesReply, String> {
    let reply = value
        .get("reply")
        .cloned()
        .ok_or("the host answered no reply")?;
    serde_json::from_value(reply).map_err(|error| error.to_string())
}

/// The [`FETCH_GRANT`] ask in `workspace`, under `invocation`: a page asks the
/// workspace's host for the token its proxied requests carry, as one of that
/// workspace's actions rather than a request kind of its own.
#[must_use]
pub fn fetch_grant_invoke(
    workspace: &str,
    invocation: tilo_actions::InvocationId,
) -> InvokeRequest {
    InvokeRequest {
        invocation,
        target: RemoteActionTarget {
            provider: tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER.to_owned(),
            address: tilo_actions::remote::workspace_governance_address(workspace, FETCH_GRANT),
        },
        argument: InvokeArgument::None,
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

/// The answer [`FETCH_GRANT`] gives `token` as: the token in hex under
/// `token`, or `null` when the host serves no proxy.
#[must_use]
pub fn fetch_grant_answer(token: Option<[u8; 32]>) -> serde_json::Value {
    serde_json::json!({ "token": token.map(hex::encode) })
}

/// The token [`FETCH_GRANT`] answered `value` with: `None` when the host
/// serves no proxy.
#[must_use]
pub fn fetch_grant_token(value: &serde_json::Value) -> Option<[u8; 32]> {
    let token = value.get("token")?.as_str()?;
    hex::decode(token).ok()?.try_into().ok()
}

/// The [`ENTER_ENTRY`] ask for `entering` and this device's `cert`, under `invocation`: what a
/// device without a certificate from its host sends on the actions transport to be let in.
///
/// # Errors
/// The entry or the certificate does not encode.
pub fn entry_invoke(
    entering: &crate::entering::Entering,
    cert: &tilo_user_identity::DeviceCert,
    invocation: tilo_actions::InvocationId,
) -> Result<InvokeRequest, String> {
    Ok(InvokeRequest {
        invocation,
        target: RemoteActionTarget {
            provider: HOST_ACTIONS_PROVIDER.to_owned(),
            address: ActionAddress::new(HOST_WORKSPACES_RESOURCE.to_owned(), ENTER_ENTRY),
        },
        argument: InvokeArgument::Json(entry_arguments(entering, cert)?),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    })
}

/// The arguments [`ENTER_ENTRY`] carries `entering` and `cert` as.
///
/// # Errors
/// They do not encode.
pub fn entry_arguments(
    entering: &crate::entering::Entering,
    cert: &tilo_user_identity::DeviceCert,
) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    let entering = serde_json::to_value(entering).map_err(|error| error.to_string())?;
    let cert = serde_json::to_value(cert).map_err(|error| error.to_string())?;
    Ok(serde_json::Map::from_iter([
        ("entering".to_owned(), entering),
        ("cert".to_owned(), cert),
    ]))
}

/// The entry and the certificate [`ENTER_ENTRY`] was asked with, read from the action's JSON
/// arguments.
///
/// # Errors
/// The arguments name no entry or certificate, or they do not decode.
pub fn entered_request(
    arguments: &serde_json::Map<String, serde_json::Value>,
) -> Result<(crate::entering::Entering, tilo_user_identity::DeviceCert), String> {
    let entering = arguments
        .get("entering")
        .cloned()
        .ok_or("the action names no entry")?;
    let cert = arguments
        .get("cert")
        .cloned()
        .ok_or("the action names no certificate")?;
    let entering = serde_json::from_value(entering).map_err(|error| error.to_string())?;
    let cert = serde_json::from_value(cert).map_err(|error| error.to_string())?;
    Ok((entering, cert))
}

/// The answer [`ENTER_ENTRY`] gives `entered` as: the entry's answer under `entered`, which
/// [`entered_reply`] reads back.
///
/// # Errors
/// The answer does not encode.
pub fn entered_answer(entered: &crate::entering::Entered) -> Result<serde_json::Value, String> {
    let entered = serde_json::to_value(entered).map_err(|error| error.to_string())?;
    Ok(serde_json::json!({ "entered": entered }))
}

/// The entry's answer [`ENTER_ENTRY`] answered `value` with.
///
/// # Errors
/// The answer carries no entry, or it does not decode.
pub fn entered_reply(value: &serde_json::Value) -> Result<crate::entering::Entered, String> {
    let entered = value
        .get("entered")
        .cloned()
        .ok_or("the host answered no entry")?;
    serde_json::from_value(entered).map_err(|error| error.to_string())
}

#[cfg(test)]
mod wire_tests;
