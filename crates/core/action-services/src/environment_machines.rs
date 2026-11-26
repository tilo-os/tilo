//! Branching and deleting a workspace's Linux machine on the member device
//! that holds it (`docs/app-model/environments.md`, "Members that take work").
//!
//! A workspace's VM lives on one VM host, whose disk holds it. When the
//! workspace's host branches the workspace, the branch's machine is made on
//! that same host as a copy of the source's disk; when the host deletes a
//! workspace, its machine goes first. The host asks the member device of that
//! VM host over the device action service, as the device it is, the way it asks for a
//! shell command ([`environment_commands`](crate::environment_commands)):
//!
//! - [`branch_target`] names a branch, an action of the branch's environment
//!   ([`RESOURCE`]): the workspace asked in is the branch, and the argument is
//!   a [`BranchMachine`] naming the source. It succeeds with a
//!   [`MachineBranched`] once the branch's disk is there, or saying the source
//!   had no machine, so nothing was copied.
//! - [`delete_target`] names a deletion of the workspace's own machine: its
//!   disk and snapshot go. It succeeds once they are gone, a workspace that
//!   had none included.
//!
//! Only the workspace's host asks, as a device of the operator the member's
//! own identity belongs to, as for a run: a branch is paid for by the account
//! the request names, which only the host knows, and the workspace a machine
//! is branched into or deleted for is the caller's, never one the request
//! names.

use serde::{Deserialize, Serialize};

use tilo_actions::api::ActionAddress;
use tilo_actions::remote::RemoteActionTarget;

/// The provider the member answers a workspace's machine under: the one its
/// runs are.
pub const PROVIDER: &str = crate::environment_commands::PROVIDER;
/// The resource branching or deleting one is an action of: the workspace's
/// environment.
pub const RESOURCE: &str = crate::environment_commands::RESOURCE;
/// Make the workspace's machine a copy of another workspace's.
pub const BRANCH: &str = "branch-machine";
/// Delete the workspace's machine, its disk and its snapshot.
pub const DELETE: &str = "delete-machine";

/// What [`BRANCH`] is invoked on.
#[must_use]
pub fn branch_target() -> RemoteActionTarget {
    RemoteActionTarget {
        provider: PROVIDER.to_owned(),
        address: ActionAddress::new(RESOURCE, BRANCH),
    }
}

/// What [`DELETE`] is invoked on.
#[must_use]
pub fn delete_target() -> RemoteActionTarget {
    RemoteActionTarget {
        provider: PROVIDER.to_owned(),
        address: ActionAddress::new(RESOURCE, DELETE),
    }
}

/// The machine of the workspace asked in, to make from another's.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BranchMachine {
    /// The workspace whose machine is copied, by its id.
    pub from: String,
    /// Who pays for the branch's machine, as the host knows it: the
    /// source's account.
    pub account: String,
    /// The branch's size (`small`, `default`, `large`, `xlarge`), no smaller
    /// than the source's.
    pub size: String,
}

/// How a branch of a machine went.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineBranched {
    /// The branch's machine was made; `false` when the source had none, so
    /// there was nothing to copy and the branch's starts as any new one does.
    pub branched: bool,
}

/// What [`DELETE`] takes: nothing but the workspace it is asked in.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteMachine {}
