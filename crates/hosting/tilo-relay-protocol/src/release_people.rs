//! The people a released relay kept, as its state reads them, and what taking
//! them into a release's own workspace did.
//!
//! A release of tilo.sh starts its relay on fresh state: the workspace's people
//! are not read back from what the earlier release wrote. They are carried over
//! by hand instead: [`People`] is what `tilo-relay release-people export` reads
//! out of the earlier release's state directory (read-only), and what
//! `tilo-relay release-people import` hands the new relay over its operator
//! socket. [`Imported`] is what the new relay answers: the addresses now on its
//! wait list, the people declared or invited, and the link each invitation
//! carries, mailed where the person's address is known.

use serde::{Deserialize, Serialize};

/// Everyone an earlier release's workspace kept.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct People {
    /// The release that wrote the state this was read from, e.g. `v0.1.5`.
    pub release: String,
    /// The workspace's name in that release.
    pub workspace: String,
    /// The addresses left at its door: people who asked without an invitation.
    #[serde(default)]
    pub wait_list: Vec<Waiting>,
    /// The workspace's people, its owner included.
    #[serde(default)]
    pub members: Vec<Member>,
}

/// One address left at the door, as the release kept it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Waiting {
    /// The address, as first typed.
    pub contact: String,
    /// The language the door was read in (`en`, `de`), for the mails it gets.
    #[serde(default = "english")]
    pub language: String,
    /// When it was left, in nanoseconds since the Unix epoch; `0` when the
    /// release kept no time for it.
    #[serde(default)]
    pub added_at_ns: u64,
    /// Whether the address already stood on the list, its mail confirmed, so
    /// whoever may invite sees it. An unconfirmed address stays hidden until
    /// the confirmation link the release mailed is opened.
    #[serde(default)]
    pub confirmed: bool,
    /// SHA-256 (hex) of every removal secret the entry kept: the links the
    /// release mailed take the address off the list again, and keep doing so
    /// here.
    #[serde(default)]
    pub removal_sha256: Vec<String>,
    /// SHA-256 (hex) of every confirmation secret an unconfirmed entry kept:
    /// the confirmation link the release mailed still lists it.
    #[serde(default)]
    pub confirm_sha256: Vec<String>,
}

fn english() -> String {
    "en".to_owned()
}

/// One of the workspace's people.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Member {
    /// The user root (hex) the person's devices act for.
    pub root: String,
    /// The name the workspace knew them by.
    pub name: String,
    /// The address the workspace kept for them, where it kept one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// `owner`, `administrator`, `editor`, `commenter` or `viewer`.
    pub role: String,
}

/// What taking [`People`] into a workspace did.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Imported {
    /// The addresses on the workspace's wait list when it was done.
    pub waiting: Vec<Waiting>,
    /// One invitation per person besides the owner.
    pub invitations: Vec<Invitation>,
    /// The people declared owners by their key: whoever the workspace is kept
    /// for, and who needs no link to join.
    pub owners: Vec<Owner>,
    /// What could not be taken in, and why.
    pub refused: Vec<String>,
}

/// The person the workspace was declared kept for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Owner {
    pub name: String,
    /// The person's user root (hex).
    pub root: String,
}

/// The invitation one person got.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Invitation {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    pub role: String,
    /// The link that brings them in.
    pub link: String,
    /// Whether the door mailed it to `email`.
    pub mailed: bool,
}
