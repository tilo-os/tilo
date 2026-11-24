//! How each workspace the device holds stands, for the person's shell: published by the device's
//! daemon as resources the shell watches on its connection to the daemon's dispatch
//! (`runtime_collab::wire::shell`), never read from a copy of its own.
//!
//! The person's workspaces are listed at [`LIST`] (`account/workspaces`, an entry per workspace,
//! [`Listed`]); each one's standing — its people, its policy, its hosts and what this device's
//! person may do there — at [`resource`] (`account/workspaces/<id>`, a [`WorkspaceReading`]).
//! Watching either is its change feed: a reading is published again whenever what it says
//! changes, and is `None` once the device holds the workspace no more.

use std::collections::BTreeMap;

use crate::join::Role;
use crate::{Member, MemberList};
use serde::{Deserialize, Serialize};
use tilo_action_policy::WorkspacePolicy;

/// The person's workspaces on this device: a list of [`Listed`].
pub const LIST: &str = "account/workspaces";

/// [`LIST`]'s action that founds a workspace on this device, which keeps it: `{name, id}`.
pub const CREATE: &str = "create";

/// [`LIST`]'s action that joins the workspace an invitation is to: `{invite}`, the invitation's
/// text or link.
pub const JOIN: &str = "join";

/// A workspace's action that writes one change to its people and settings, signed as the device
/// that asks: `{command}`, a [`crate::workspace_membership::WorkspaceMembershipCommand`]. The
/// workspace's admission decides it, as it decides every member's change.
pub const MEMBERSHIP: &str = "membership";

/// The resource of `workspace`'s standing, a [`WorkspaceReading`].
#[must_use]
pub fn resource(workspace: &str) -> String {
    format!("{LIST}/{workspace}")
}

/// The workspace a [`resource`] names, when it names one.
#[must_use]
pub fn workspace_of(resource: &str) -> Option<&str> {
    resource
        .strip_prefix(LIST)?
        .strip_prefix('/')
        .filter(|id| !id.is_empty() && !id.contains('/'))
}

/// One of the person's workspaces, as [`LIST`] names it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Listed {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub labels: Vec<String>,
    /// The person owns it.
    pub own: bool,
    /// This device holds it.
    pub kept_here: bool,
    /// The device that runs its work when that is another than this one (a home server, a cloud
    /// host), by the address it signed into the workspace's membership (postcard, in hex): where
    /// its records' actions run, and where a device of the person's that holds none of it enters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
}

/// Why a workspace is not the person's to delete here, when it is not.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum NotDeletable {
    /// This device does not hold it.
    NotHeld,
    /// Someone else is still in it, so it is not this device's to delete.
    OthersInIt,
    /// Another of the person's devices holds it too: deleting it here would not delete it for them.
    OtherDevice,
    /// It belongs to someone else.
    NotOwner,
}

/// Who hosts a workspace this device holds, as its record says.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hosting {
    /// The host's name, as its person gave it; empty when the record names no host.
    pub host: String,
    /// The host is this device.
    pub here: bool,
    /// This device is the workspace's member: its host took it in.
    pub admitted: bool,
}

/// One device keeping a workspace, as a member's device reads it off its own signed address.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Holder {
    /// Its device key, hex.
    pub device: String,
    /// What it calls itself: its server's name from its person's admin workspace, or its site's
    /// name; its device's label when it names none.
    pub label: String,
    /// Whether it holds the workspace's keys: it reads the workspace. A store holds its
    /// ciphertext and reads none of it.
    pub reads: bool,
    /// Whether the machine under it is rented from a cloud provider, whose operator can read what
    /// it holds.
    pub cloud: bool,
}

/// How one workspace stands for the device's person: what the shell reads of it, decided by the
/// daemon from the workspace it keeps.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each is a separate fact the daemon reads off the workspace, read by name"
)]
pub struct WorkspaceReading {
    pub id: String,
    /// The workspace's one name, as its membership records it.
    pub name: Option<String>,
    /// Who governs it, as the person id its membership names.
    pub owner: Option<String>,
    /// Its members' person ids.
    pub roster: Vec<String>,
    /// The devices its roster admits, by key.
    pub roster_devices: Vec<[u8; 32]>,
    pub people: Vec<Member>,
    pub members: MemberList,
    pub policy: Option<WorkspacePolicy>,
    /// The device's person administers it.
    pub administers: bool,
    /// The person may invite into it.
    pub may_invite: bool,
    /// The membership the device holds refuses the person inviting; `false` while the device has
    /// no committed view of it yet.
    pub invite_refused: bool,
    pub may_invite_administrator: bool,
    /// The parts the person may hand out in an invitation, least to most.
    pub invitable_roles: Vec<Role>,
    /// Why the person may not delete it here; `None` when they may.
    pub not_deletable: Option<NotDeletable>,
    /// The person reads everything it holds here, so it may be branched.
    pub reads_whole: bool,
    pub hosting: Option<Hosting>,
    /// The devices keeping it.
    pub holders: Vec<Holder>,
    /// The device keeping it, as this device asks it; this device's own key when it keeps it.
    pub holder: Option<[u8; 32]>,
    /// Device labels by device key.
    #[serde(with = "labels_by_key")]
    pub device_labels: BTreeMap<[u8; 32], String>,
    /// What its record refuses to take of this device, kept until the person sees why.
    pub write_refusals: Vec<neighbour_wire::WriteRefusal>,
    /// The records whose stored history the device refused when it opened the workspace, with
    /// why: not read until a device of the workspace sends their history again
    /// ([`crate::StatePeer::quarantined`]).
    #[serde(default)]
    pub quarantined: Vec<crate::Quarantined>,
    /// When the links out of it lapse, in nanoseconds since the Unix epoch: whoever redeems one
    /// dials this device while one is open.
    pub open_links: Vec<u64>,
    /// The workspace's own record, as the settings it keeps are read from it.
    pub document: Option<Vec<u8>>,
    /// The shortcuts its people saved, as each saver's device wrote them.
    #[serde(default)]
    pub shortcuts: Vec<tilo_automation::SavedShortcut>,
    /// What its governance lets its apps reach; `None` while that does not read.
    #[serde(default)]
    pub sandbox: Option<Vec<collab_types::caps::CapOverride>>,
}

impl WorkspaceReading {
    /// The name the workspace knows `person` by, when it knows one.
    #[must_use]
    pub fn name_of(&self, person: [u8; 32]) -> Option<String> {
        let person = hex::encode(person);
        self.people
            .iter()
            .find(|member| member.id.person_id() == Some(person.as_str()))
            .map(|member| member.name.clone())
            .filter(|name| !name.is_empty())
    }
}

/// A device key is no JSON object key, and the daemon publishes a reading as JSON: the labels
/// travel as pairs of key and label.
mod labels_by_key {
    use std::collections::BTreeMap;

    use serde::{Deserialize, Deserializer, Serializer};

    pub(super) fn serialize<S: Serializer>(
        labels: &BTreeMap<[u8; 32], String>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(labels)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<BTreeMap<[u8; 32], String>, D::Error> {
        Ok(Vec::<([u8; 32], String)>::deserialize(deserializer)?
            .into_iter()
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reading that names its devices is published as JSON, and reads back as it was.
    #[test]
    fn a_reading_with_device_labels_crosses_as_json() {
        let reading = WorkspaceReading {
            device_labels: BTreeMap::from([([7; 32], "Laptop".to_owned())]),
            ..WorkspaceReading::default()
        };
        let json = serde_json::to_string(&reading).expect("the reading encodes as JSON");
        let read: WorkspaceReading = serde_json::from_str(&json).expect("and reads back");
        assert_eq!(read.device_labels, reading.device_labels);
    }
}
