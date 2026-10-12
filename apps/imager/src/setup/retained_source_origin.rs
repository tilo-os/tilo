//! Original Home's metadata-only source-native provenance. It is distinct from
//! target FUSE identities and from independently admitted editor write grants.
use super::{HomeDirectory, InstalledRetainedLaunchInput, refused};
use runtime_fs::{
    EntryKind, EntryName,
    directory_backend::{WorkspaceDirectoryBackend, WorkspaceMetadataBackend},
    directory_connection::ROOT_DIRECTORY,
};
use std::{io, sync::Arc};

pub(in crate::setup) struct SourceSelectedOrigin {
    root: Arc<HomeDirectory>,
    parent: Arc<HomeDirectory>,
    pin: Arc<dyn WorkspaceMetadataBackend>,
    components: Vec<EntryName>,
    leaf: EntryName,
    root_identity: [u64; 2],
    parent_identity: [u64; 2],
    selected_identity: [u64; 2],
}
impl SourceSelectedOrigin {
    fn walk(root: &Arc<HomeDirectory>, names: &[EntryName]) -> io::Result<Arc<HomeDirectory>> {
        let mut current = root.clone();
        for name in names {
            let id = current
                .custody
                .call(|source| source.home()?.directory(current.id, name))?;
            current = Arc::new(HomeDirectory {
                custody: current.custody.clone(),
                id,
            });
        }
        Ok(current)
    }
    // Only the original relay can supply a concrete authenticated root.
    pub(super) fn pin(root: Arc<HomeDirectory>, components: &[EntryName]) -> io::Result<Arc<Self>> {
        if root.id != ROOT_DIRECTORY || components.is_empty() || components.len() > 64 {
            return Err(refused());
        }
        let (leaf, parents) = components.split_last().ok_or_else(refused)?;
        let root_stat = root.metadata().map_err(io::Error::other)?;
        if root_stat.metadata.kind != EntryKind::Directory {
            return Err(refused());
        }
        let parent = Self::walk(&root, parents)?;
        let parent_stat = parent.metadata().map_err(io::Error::other)?;
        if parent_stat.metadata.kind != EntryKind::Directory {
            return Err(refused());
        }
        let selected = parent
            .stat(leaf)
            .map_err(io::Error::other)?
            .ok_or_else(refused)?;
        if selected.metadata.kind != EntryKind::File {
            return Err(refused());
        }
        // Real source pin_metadata admits metadata only and refuses special
        // types. No bytes are opened and no Read/Write right is minted here.
        let pin = parent
            .pin_metadata(leaf, selected.identity)
            .map_err(io::Error::other)?;
        let value = Arc::new(Self {
            root,
            parent,
            pin,
            components: components.to_vec(),
            leaf: leaf.clone(),
            root_identity: root_stat.identity,
            parent_identity: parent_stat.identity,
            selected_identity: selected.identity,
        });
        value.check_named()?;
        Ok(value)
    }
    /// Pre-adoption only: validates the same live original Files source. Once
    /// an actual editor namespace is adopted, its fresh source grant owns live
    /// checks; these fixed identities remain original authenticated provenance.
    pub(in crate::setup) fn check_named(&self) -> io::Result<()> {
        self.observe_named()?;
        self.observe_named()
    }
    fn observe_named(&self) -> io::Result<()> {
        let root = self.root.metadata().map_err(io::Error::other)?;
        if root.identity != self.root_identity || root.metadata.kind != EntryKind::Directory {
            return Err(refused());
        }
        let named_parent = Self::walk(&self.root, &self.components[..self.components.len() - 1])?;
        let named_parent_stat = named_parent.metadata().map_err(io::Error::other)?;
        let held_parent = self.parent.metadata().map_err(io::Error::other)?;
        let named = self
            .parent
            .stat(&self.leaf)
            .map_err(io::Error::other)?
            .ok_or_else(refused)?;
        let held = self.pin.metadata().map_err(io::Error::other)?;
        if named_parent_stat.identity != self.parent_identity
            || held_parent.identity != self.parent_identity
            || named.identity != self.selected_identity
            || held.identity != self.selected_identity
            || named.metadata.kind != EntryKind::File
            || held.metadata.kind != EntryKind::File
        {
            return Err(refused());
        }
        self.root.check().map_err(io::Error::other)
    }
    /// Descriptive fixed native identities. Reading these never reopens a grant.
    pub(in crate::setup) fn parent_identity(&self) -> [u64; 2] {
        self.parent_identity
    }
    pub(in crate::setup) fn selected_identity(&self) -> [u64; 2] {
        self.selected_identity
    }
    pub(in crate::setup) fn components(&self) -> &[EntryName] {
        &self.components
    }
    pub(in crate::setup) fn uses_input(&self, input: &Arc<InstalledRetainedLaunchInput>) -> bool {
        Arc::ptr_eq(&self.root.custody.input, input)
    }
}

/// A routing derivative of the actual originally admitted Files selection.
/// Weak custody does not prolong the original Files projection or proof.
pub(in crate::setup) struct SourceHistoryReadOrigin {
    cleanup: Option<crate::setup::retained_history_read::RootHistoryReadCleanup>,
    custody: std::sync::Weak<super::Custody>,
    parents: Vec<EntryName>,
    leaf: EntryName,
    parent_identity: [u64; 2],
}
impl SourceSelectedOrigin {
    pub(in crate::setup) fn history_read_origin(&self) -> io::Result<Arc<SourceHistoryReadOrigin>> {
        self.check_named()?;
        Ok(Arc::new(SourceHistoryReadOrigin {
            cleanup: self
                .root
                .custody
                .history
                .lock()
                .map_err(|_| refused())?
                .as_ref()
                .map(|history| history.cleanup()),
            custody: Arc::downgrade(&self.root.custody),
            parents: self.components[..self.components.len() - 1].to_vec(),
            leaf: self.leaf.clone(),
            parent_identity: self.parent_identity,
        }))
    }
}
impl SourceHistoryReadOrigin {
    pub(in crate::setup) fn retire_history_preceding(
        &self,
        proof: &crate::setup::retained_selected_edit::AcknowledgedSelectedReplacement,
    ) -> io::Result<()> {
        match &self.cleanup {
            Some(owner) => {
                owner.retire_preceding(proof, &self.parents, &self.leaf, self.parent_identity)
            }
            None => Ok(()),
        }
    }
    /// Invoked only after this exact signed native ACK advanced live lineage.
    /// A fresh source Read capability is opened independently; the ACK does not
    /// create it. The fixed original metadata pin is never advanced/reused.
    pub(in crate::setup) fn adopt_acknowledged(
        &self,
        proof: &crate::setup::retained_selected_edit::AcknowledgedSelectedReplacement,
        edited: &crate::setup::retained_source_relay::selected_parent::SourceSelectedParent,
    ) -> io::Result<Option<Arc<crate::setup::retained_history_read::RootHistoryReadContinuity>>>
    {
        proof.check_custody(edited).map_err(io::Error::other)?;
        let Some(custody) = self.custody.upgrade() else {
            return Ok(None);
        };
        // Closed Files is an unavailable Reader, not a revoked editor.
        if custody.current().is_err() {
            return Ok(None);
        }
        if custody.history.lock().map_err(|_| refused())?.is_none() {
            return Ok(None);
        }
        let root = Arc::new(HomeDirectory {
            custody: custody.clone(),
            id: ROOT_DIRECTORY,
        });
        let parent = SourceSelectedOrigin::walk(&root, &self.parents)?;
        let stat = parent.metadata().map_err(io::Error::other)?;
        if stat.identity != self.parent_identity {
            return Err(refused());
        }
        let current = parent
            .stat(&self.leaf)
            .map_err(io::Error::other)?
            .ok_or_else(refused)?;
        if current.identity != proof.observation().destination
            || current.metadata.kind != EntryKind::File
        {
            return Err(refused());
        }
        let history = custody
            .history
            .lock()
            .map_err(|_| refused())?
            .as_ref()
            .ok_or_else(refused)?
            .clone();
        // Actual Read is opened independently of the ACK. The canonical daemon
        // matches this subject to its same held ACK before minting continuity.
        let subject = parent
            .history_read_subject(&self.leaf)
            .map_err(io::Error::other)?;
        let continuity = history.adopt_acknowledged(subject, proof, edited)?;
        proof.check_custody(edited).map_err(io::Error::other)?;
        custody.current()?;
        continuity.check()?;
        Ok(Some(continuity))
    }
}
