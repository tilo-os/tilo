//! Original Home's metadata-only source-native provenance. It is distinct from
//! target FUSE identities and from independently admitted editor write grants.
use super::{HomeDirectory, InstalledRetainedLaunchInput, refused};
use runtime_fs::{
    EntryKind, EntryName,
    directory_backend::{WorkspaceDirectoryBackend, WorkspaceMetadataBackend},
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
    pub(in crate::setup) fn components(&self) -> &[EntryName] {
        &self.components
    }
    pub(in crate::setup) fn uses_input(&self, input: &Arc<InstalledRetainedLaunchInput>) -> bool {
        Arc::ptr_eq(&self.root.custody.input, input)
    }
}
