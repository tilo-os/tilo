//! The person's shell on the device's broker.
//!
//! A desktop session never enters its person's workspaces itself: the device's daemon holds
//! them, and the session is its client. The daemon's broker admits the session's connection by
//! the account the kernel names at its other end, as that account's person's shell, beside the
//! apps it admits by their launch. A shell connection asks three things of the daemon:
//!
//! - the documents of the workspaces the device holds, read for the device itself and written as
//!   it: [`ShellCall`] carries what `tilo_peer_state::workspace_documents::DeviceDocuments` and
//!   `DeviceWrites` ask, and the daemon signs and seals each write with the device's key, so the
//!   shell holds no key of any workspace;
//! - an action of a workspace's governance or of the host itself, as a typed
//!   `tilo_actions::remote::InvokeRequest` ([`ShellCall::Invoke`]): the daemon answers it from a
//!   workspace it keeps, and forwards it to the workspace's keeping device over tilo-actions/1
//!   otherwise;
//! - how each workspace stands (its people, its policy, its hosts and what this device may do
//!   there): resources the daemon publishes and the shell watches on the same connection
//!   ([`super::Operation::Services`], [`super::Operation::Watch`]).

use serde::{Deserialize, Serialize};

/// What a shell's connection opens with, before its first call: it asks to be the shell of the
/// account the kernel names at its other end. As long as an app's launch marker, which it is
/// told apart from.
pub const SHELL_MAGIC: &[u8; 4] = b"QSH1";

/// What a shell asks of the device's documents and actions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShellCall {
    /// `app`'s `resource` of `workspace`, whole, or only what changed after the heads `since`.
    Read {
        workspace: String,
        app: String,
        resource: String,
        since: Option<Vec<[u8; 32]>>,
    },
    /// Whether `app`'s `resource` of `workspace` reads now: held here, nothing it waits for still
    /// on its way.
    Readable {
        workspace: String,
        app: String,
        resource: String,
    },
    /// Every document `workspace` holds, by its app and route.
    Listing { workspace: String },
    /// Whether the device holds anything of `app`'s `resource` in `workspace`.
    Holds {
        workspace: String,
        app: String,
        resource: String,
    },
    /// The workspaces the device holds, by their ids.
    Workspaces,
    /// Write `changes` to `app`'s `document` of `workspace`, signed and sealed as the device.
    Write {
        workspace: String,
        app: String,
        document: String,
        /// The members who read it, where only some do.
        readers: Option<String>,
        /// The document's founding write: the whole document, which the workspace must not hold
        /// yet.
        founding: bool,
        /// The whole document: a founding, or a checkpoint of one held already.
        snapshot: bool,
        changes: Vec<u8>,
    },
    /// Wait until the device serving `workspace` answered every write the device made to
    /// `documents` (each an app and a document of it) before this call.
    Served {
        workspace: String,
        documents: Vec<(String, String)>,
    },
    /// Invoke an action as the shell's person on this device: a workspace's governance
    /// (`RemoteActionTarget::workspace_governance`) or the host's own operations. The request is
    /// `tilo_actions::remote::InvokeRequest` as JSON.
    Invoke { request: String },
}

/// The daemon's answer to a [`ShellCall`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShellReply {
    /// A [`ShellCall::Read`].
    Read(ShellRead),
    /// A [`ShellCall::Readable`] or [`ShellCall::Holds`].
    Is(bool),
    /// A [`ShellCall::Listing`]: `(app, route)` of each document.
    Listing(Vec<(String, String)>),
    /// A [`ShellCall::Workspaces`].
    Workspaces(Vec<String>),
    /// A [`ShellCall::Write`]: the write as the workspace took it, or why it took none, as
    /// `tilo_peer_state::owner::WriteRefusal` JSON.
    Written(Result<ShellWrite, String>),
    /// A [`ShellCall::Served`]: `Ok` once the serving device holds each write, `Err` with why it
    /// refused one. `None` when the workspace has no serving device to ask: one the device keeps
    /// itself.
    Served(Option<Result<(), String>>),
    /// A [`ShellCall::Invoke`] once it settled: `tilo_actions::InvocationUpdate` as JSON.
    Invoked(String),
}

/// What the device holds of one document, as `tilo_peer_state::workspace_documents::DocumentRead`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShellRead {
    Whole {
        revision: [u8; 32],
        heads: Vec<[u8; 32]>,
        bytes: Vec<u8>,
    },
    Changes {
        revision: [u8; 32],
        heads: Vec<[u8; 32]>,
        bytes: Vec<u8>,
    },
    Unchanged([u8; 32]),
    Absent,
    Pending,
}

/// A write the workspace took, as `tilo_peer_state::owner::AppWrite`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShellWrite {
    pub peer: u64,
    pub app_id: String,
    pub cluster_id: String,
    pub document_id: String,
    /// The author's access, as `tilo_action_policy::MemberAccess` JSON.
    pub author: String,
    pub replayed: bool,
    /// The signed op, as the postcard bytes of its `collab_types::SignedOp`.
    pub envelope: Vec<u8>,
}
