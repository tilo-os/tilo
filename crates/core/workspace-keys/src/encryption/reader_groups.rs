//! The key groups of reader sets within a workspace.
//!
//! A document only some of a workspace's members read seals under a group of its own: the
//! group of its reader set ([`collab_types::reader_group`]), whose keys are wrapped only to the
//! devices of the people in the set. It is settled exactly as the workspace's own group is, by
//! the same governance over the workspace's transport: its genesis, its access requests and its
//! commits are signed by the device that makes them and judged by every key holder against who
//! may read it, which is the devices of its people the workspace admits now
//! ([`KeyDirectory::reader_devices`]). A person who leaves the workspace or a device that is
//! revoked leaves the group by a commit like any other, so what is sealed after it is sealed
//! under keys it never gets. The set of every member is the workspace's own group.
//!
//! A device learns which people read a group when one of its endpoints asks for the group's
//! keys ([`super::WorkspaceKeys::reader_group`]): the group's id is derived from the set and
//! names neither. It keeps what it learned with its group state
//! ([`super::KeyStorage::reader_groups`]), so after a restart it goes on settling the groups it
//! holds, a removal included, before anything asks for their keys again. Governance for a group
//! no endpoint of this device asked about yet is not taken in; the group's key holders answer
//! this device once it asks.

use std::collections::{BTreeSet, HashMap};

use collab_types::{Ed25519PublicKey, Readers};
use neighbour_wire::KeyScope;
use tilo_desktop_model::WorkspaceId;

use super::KeyDirectory;

/// The workspace a reader set's group belongs to, and who reads it: its people, and the role
/// whose holders read it beside them.
struct ReaderGroup {
    workspace: WorkspaceId,
    readers: String,
    named: collab_types::NamedReaders,
}

/// A host's [`KeyDirectory`] that also names who reads each reader set's group this device
/// asked about.
pub(super) struct Directory {
    workspaces: Box<dyn KeyDirectory>,
    groups: HashMap<WorkspaceId, ReaderGroup>,
    documents: HashMap<WorkspaceId, KeyScope>,
}

impl Directory {
    pub(super) fn new(workspaces: Box<dyn KeyDirectory>) -> Self {
        Self {
            workspaces,
            groups: HashMap::new(),
            documents: HashMap::new(),
        }
    }

    /// The group of `readers` in `workspace`, known here from now on. `None` when `readers`
    /// names no set.
    pub(super) fn group(&mut self, workspace: WorkspaceId, readers: &str) -> Option<WorkspaceId> {
        let group = collab_types::reader_group(workspace, readers)?;
        if let Some(named) = Readers::parse(readers).and_then(|set| set.named()) {
            self.groups.entry(group).or_insert_with(|| ReaderGroup {
                workspace,
                readers: readers.to_owned(),
                named,
            });
        }
        Some(group)
    }

    pub(super) fn register_scope(&mut self, scope: KeyScope) -> Option<WorkspaceId> {
        let KeyScope::Document {
            workspace,
            app,
            document,
            readers,
        } = &scope
        else {
            return None;
        };
        KeyScope::document(*workspace, app, document, readers.as_deref())?;
        let group = scope.group();
        match self.documents.entry(group) {
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(scope);
                Some(group)
            }
            std::collections::hash_map::Entry::Occupied(entry) => {
                (entry.get() == &scope).then_some(group)
            }
        }
    }

    pub(super) fn scope_devices(
        &self,
        scope: &KeyScope,
        now: u64,
    ) -> Option<BTreeSet<Ed25519PublicKey>> {
        let KeyScope::Document {
            workspace,
            app,
            document,
            readers,
        } = scope
        else {
            return None;
        };
        let mut devices = self
            .workspaces
            .document_devices(*workspace, app, document, now)?;
        if let Some(readers) = readers {
            let named = Readers::parse(readers)?.named()?;
            let named = self.workspaces.reader_devices(*workspace, &named, now)?;
            devices.retain(|device| named.contains(device));
        }
        Some(devices)
    }

    pub(super) fn document_scope(&self, group: WorkspaceId) -> Option<&KeyScope> {
        self.documents.get(&group)
    }

    pub(super) fn named_readers(&self, workspace: WorkspaceId, document: &str) -> Option<String> {
        let name = collab_types::store_document_readers(document)?;
        let candidates: BTreeSet<_> = self
            .groups
            .values()
            .filter(|group| group.workspace == workspace)
            .map(|group| &group.readers)
            .chain(self.documents.values().filter_map(|scope| match scope {
                KeyScope::Document {
                    workspace: held,
                    document: held_document,
                    readers: Some(readers),
                    ..
                } if *held == workspace && held_document == document => Some(readers),
                _ => None,
            }))
            .filter(|readers| collab_types::reader_set_name(readers) == name)
            .collect();
        (candidates.len() == 1)
            .then(|| candidates.first().map(|readers| (*readers).clone()))
            .flatten()
    }

    /// Every reader set's group known here: its workspace and readers, as a store keeps them.
    pub(super) fn kept(&self) -> Vec<(WorkspaceId, String)> {
        let mut kept: Vec<(WorkspaceId, String)> = self
            .groups
            .values()
            .map(|group| (group.workspace, group.readers.clone()))
            .collect();
        kept.sort();
        kept
    }

    /// The reader sets' groups of `workspace` known here.
    pub(super) fn of(&self, workspace: WorkspaceId) -> Vec<WorkspaceId> {
        self.groups
            .iter()
            .filter(|(_, group)| group.workspace == workspace)
            .map(|(group, _)| *group)
            .chain(
                self.documents.iter().filter_map(|(group, scope)| {
                    (scope.workspace() == workspace).then_some(*group)
                }),
            )
            .collect()
    }

    /// The workspace whose transport carries `group`'s governance: the workspace of a reader
    /// set's group, else the group's own.
    pub(super) fn carrier(&self, group: WorkspaceId) -> WorkspaceId {
        self.document_scope(group).map_or_else(
            || {
                self.groups
                    .get(&group)
                    .map_or(group, |reader_group| reader_group.workspace)
            },
            KeyScope::workspace,
        )
    }

    /// The group governance naming `group` that arrived in `workspace` is for: the workspace's
    /// own, or a reader set's of that workspace known here. `None` for any other.
    pub(super) fn arrived(
        &self,
        workspace: WorkspaceId,
        group: WorkspaceId,
    ) -> Option<WorkspaceId> {
        (self.carrier(group) == workspace).then_some(group)
    }

    /// Forget the reader sets' groups of `workspace`, deleted, answering them.
    pub(super) fn forget(&mut self, workspace: WorkspaceId) -> Vec<WorkspaceId> {
        let groups = self.of(workspace);
        for group in &groups {
            self.groups.remove(group);
            self.documents.remove(group);
        }
        groups
    }
}

impl KeyDirectory for Directory {
    fn allowed_devices(
        &self,
        workspace: WorkspaceId,
        now: u64,
    ) -> Option<BTreeSet<Ed25519PublicKey>> {
        if let Some(scope) = self.document_scope(workspace) {
            return self.scope_devices(scope, now);
        }
        if KeyScope::requires_document(workspace) {
            return None;
        }
        match self.groups.get(&workspace) {
            Some(group) => self
                .workspaces
                .reader_devices(group.workspace, &group.named, now),
            None => self.workspaces.allowed_devices(workspace, now),
        }
    }

    fn document_devices(
        &self,
        workspace: WorkspaceId,
        app: &str,
        document: &str,
        now: u64,
    ) -> Option<BTreeSet<Ed25519PublicKey>> {
        self.workspaces
            .document_devices(workspace, app, document, now)
    }

    fn requires_document_keys(
        &self,
        workspace: WorkspaceId,
        app: &str,
        document: &str,
    ) -> Option<bool> {
        self.workspaces
            .requires_document_keys(workspace, app, document)
    }

    fn reader_devices(
        &self,
        workspace: WorkspaceId,
        named: &collab_types::NamedReaders,
        now: u64,
    ) -> Option<BTreeSet<Ed25519PublicKey>> {
        self.workspaces.reader_devices(workspace, named, now)
    }

    fn policy_epoch(&self) -> Option<u64> {
        self.workspaces.policy_epoch()
    }

    fn next_policy_change(&self, workspace: WorkspaceId, now: u64) -> Option<u64> {
        self.workspaces
            .next_policy_change(self.carrier(workspace), now)
    }

    fn announce_at(&self, at: u64) {
        self.workspaces.announce_at(at);
    }

    /// A reader set's group, and a document's group its roles decide, is founded by whichever of
    /// its devices first needs its keys: two founded at once settle on the genesis with the lower
    /// digest, as any group's do. Left to the workspace's founder, a document whose roles that
    /// founder does not read (a workspace a server made, where only members read the document)
    /// had no device that would found its group, and no member ever held its keys.
    fn founds(&self, workspace: WorkspaceId) -> bool {
        self.groups.contains_key(&workspace)
            || matches!(
                self.document_scope(workspace),
                Some(KeyScope::Document { .. })
            )
            || self.workspaces.founds(self.carrier(workspace))
    }
}
