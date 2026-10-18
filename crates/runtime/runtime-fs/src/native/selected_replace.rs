//! Root-sealed selected edit derivative. Parsed observations alone never admit
//! Replace, and ordinary Home rights are never widened.
use super::{
    Arc, DirectoryGrant, EntryName, FsError, FsRight, FsSession, GrantLease, GrantedEntry,
    MetadataExt, Mutex, NativeFileAccess, NativeObject, Weak, io_error, lock,
};
use runtime_portal::retained_replacement::SelectedReplacementPermitRecord;
#[cfg(target_os = "linux")]
use {
    super::OsString,
    runtime_portal::retained_replacement::MAX_PERMIT_BYTES,
    std::io::{Read, Seek},
    std::os::fd::AsRawFd,
    std::os::unix::{ffi::OsStringExt, fs::MetadataExt as StdMetadataExt},
};

pub(super) struct SelectedPermit {
    pub(super) root: GrantLease,
    pub(super) parent: DirectoryGrant,
    pub(super) leaf: EntryName,
    pub(super) record: SelectedReplacementPermitRecord,
    pub(super) selected: Mutex<GrantedEntry>,
}
/// Only a genuine sealed original-source bootstrap can construct this scope.
/// It still supplies no signed daemon prepare or successful history receipt.
pub struct SelectedReplacementAuthority {
    session: FsSession,
    permit: Arc<SelectedPermit>,
}
impl SelectedReplacementAuthority {
    /// Prepare only the selected current victim using genuine created custody.
    /// # Errors
    /// Refuses stale original lineage, arbitrary temporaries and absent authority.
    pub fn prepare_atomic_save(
        &self,
        temporary: &crate::operations::NativeCreatedTemporary,
    ) -> Result<crate::operations::PreparedNativeReplacement, FsError> {
        if self.permit.record.purpose
            != runtime_portal::retained_replacement::SelectedReplacementPurpose::AtomicSave
        {
            return Err(FsError::Denied);
        }
        let verifier = self.session.retained_history_verifier()?;
        if self.permit.record.source_owner != verifier.source_owner
            || self.permit.record.source_operation != verifier.source_operation
            || self.permit.record.person != verifier.person
            || self.permit.record.personal_room != verifier.personal_room
        {
            return Err(FsError::Denied);
        }
        let victim = lock(&self.permit.selected)?.clone();
        self.session.prepare_atomic_save(
            temporary,
            self.permit.parent.clone(),
            self.permit.leaf.clone(),
            victim,
        )
    }
}
impl FsSession {
    /// Bind only a sealed, anonymous root-produced permit delivered through the
    /// existing signed source admission. No parsed record or path is authority.
    /// # Errors
    /// Refuses malformed/nonroot/unsealed files, mismatched owner/root/current
    /// native locus, revoked original scope or a second binding.
    #[cfg(target_os = "linux")]
    pub fn bind_selected_replace_authority(
        &self,
        root: &DirectoryGrant,
        mut sealed: std::fs::File,
    ) -> Result<SelectedReplacementAuthority, FsError> {
        let metadata = sealed.metadata().map_err(io_error)?;
        let seals = unsafe { libc::fcntl(sealed.as_raw_fd(), libc::F_GET_SEALS) };
        let required =
            libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;
        if metadata.uid() != 0
            || !metadata.is_file()
            || metadata.nlink() != 0
            || metadata.len() == 0
            || metadata.len() > MAX_PERMIT_BYTES as u64
            || seals < 0
            || seals & required != required
        {
            return Err(FsError::Denied);
        }
        sealed.rewind().map_err(io_error)?;
        let capacity = usize::try_from(metadata.len()).map_err(|_| FsError::LimitExceeded)?;
        let read_limit = (MAX_PERMIT_BYTES as u64)
            .checked_add(1)
            .ok_or(FsError::LimitExceeded)?;
        let mut bytes = Vec::with_capacity(capacity);
        sealed
            .take(read_limit)
            .read_to_end(&mut bytes)
            .map_err(io_error)?;
        let record = SelectedReplacementPermitRecord::decode(&bytes).map_err(io_error)?;
        if record.source_owner != self.retained_source_owner_key()? {
            return Err(FsError::Denied);
        }
        let verifier = self.retained_history_verifier()?;
        if record.source_owner != verifier.source_owner
            || record.source_operation != verifier.source_operation
            || record.person != verifier.person
            || record.personal_room != verifier.personal_room
        {
            return Err(FsError::Denied);
        }
        {
            let table = self.call()?;
            self.validate(&table, &root.lease, Some(FsRight::Enumerate))?;
            let custody = lock(&self.inner.removal)?.clone().ok_or(FsError::Denied)?;
            if !Arc::ptr_eq(&root.lease.record, &custody.root.record) {
                return Err(FsError::Denied);
            }
            custody.check()?;
            let identity = root.lease.object()?.identity();
            if [identity.device, identity.inode] != record.source_root {
                return Err(FsError::StaleEntry);
            }
        }
        let mut parent = root.clone();
        let (leaf, components) = record.components.split_last().ok_or(FsError::Denied)?;
        for raw in components {
            let name = EntryName::new(OsString::from_vec(raw.clone()))?;
            let GrantedEntry::Directory(directory) = self.lookup(&parent, &name)? else {
                return Err(FsError::Denied);
            };
            parent = directory;
        }
        let leaf = EntryName::new(OsString::from_vec(leaf.clone()))?;
        let selected =
            GrantedEntry::File(self.lookup_file_for(&parent, &leaf, NativeFileAccess::Write)?);
        {
            let table = self.call()?;
            self.validate(&table, &parent.lease, Some(FsRight::Create))?;
            self.validate(&table, selected.lease(), Some(FsRight::Write))?;
            let parent_id = parent.lease.object()?.identity();
            let selected_id = selected.lease().object()?.identity();
            if [parent_id.device, parent_id.inode] != record.parent
                || [selected_id.device, selected_id.inode] != record.selected
                || !matches!(selected.lease().object()?, NativeObject::File { .. })
            {
                return Err(FsError::StaleEntry);
            }
        }
        let permit = Arc::new(SelectedPermit {
            root: root.lease.clone(),
            parent,
            leaf,
            record,
            selected: Mutex::new(selected),
        });
        let table = self.call()?;
        self.validate(&table, &root.lease, Some(FsRight::Enumerate))?;
        let mut slot = lock(&self.inner.selected_replace)?;
        if slot.is_some() {
            return Err(FsError::Busy);
        }
        *slot = Some(permit.clone());
        let authority = SelectedReplacementAuthority {
            session: self.clone(),
            permit,
        };
        drop(slot);
        drop(table);
        Ok(authority)
    }
    /// Bind selected replacement authority only on supported hosts.
    /// # Errors
    /// Always returns an unsupported-platform error outside Linux.
    #[cfg(not(target_os = "linux"))]
    pub fn bind_selected_replace_authority(
        &self,
        _: &DirectoryGrant,
        _: std::fs::File,
    ) -> Result<SelectedReplacementAuthority, FsError> {
        Err(FsError::Unsupported(
            "selected replacement bootstrap requires Linux",
        ))
    }
    pub(crate) fn selected_replacement_authority(
        &self,
    ) -> Result<SelectedReplacementAuthority, FsError> {
        let table = self.call()?;
        let permit = lock(&self.inner.selected_replace)?
            .clone()
            .ok_or(FsError::Denied)?;
        self.validate(&table, &permit.root, Some(FsRight::Enumerate))?;
        Ok(SelectedReplacementAuthority {
            session: self.clone(),
            permit,
        })
    }
    pub(crate) fn has_selected_replace_authority(&self) -> Result<bool, FsError> {
        self.check_alive()?;
        Ok(lock(&self.inner.selected_replace)?.is_some())
    }
    pub(super) fn selected_replace_right(&self, lease: &GrantLease) -> Result<bool, FsError> {
        let Some(permit) = lock(&self.inner.selected_replace)?.clone() else {
            return Ok(false);
        };
        let custody = lock(&self.inner.removal)?.clone().ok_or(FsError::Denied)?;
        custody.check()?;
        if !Arc::ptr_eq(&permit.root.record, &custody.root.record)
            || *lock(&custody.owner_key)? != Some(permit.record.source_owner)
        {
            return Err(FsError::Denied);
        }
        if Arc::ptr_eq(&lease.record, &permit.root.record)
            || Arc::ptr_eq(&lease.record, &permit.parent.lease.record)
        {
            return Ok(true);
        }
        let object = lease.object()?;
        if matches!(object, NativeObject::Directory { .. }) {
            // A separately looked-up SAME held selected parent is allowed; no
            // other directory inherits Replace from the Home grant.
            let Some(parent) = lease.record.parent.as_ref() else {
                return Ok(false);
            };
            let Some(name) = lease.record.name.as_ref() else {
                return Ok(false);
            };
            let Some(expected_parent) = permit.parent.lease.record.parent.as_ref() else {
                return Ok(false);
            };
            let Some(expected_name) = permit.parent.lease.record.name.as_ref() else {
                return Ok(false);
            };
            return Ok(
                object.identity() == permit.parent.lease.object()?.identity()
                    && self.locator_locked(parent, name, Some(object.identity()))?
                        == self.locator_locked(
                            expected_parent,
                            expected_name,
                            Some(object.identity()),
                        )?,
            );
        }
        let selected = lock(&permit.selected)?;
        let parent = lease.record.parent.as_ref().ok_or(FsError::Denied)?;
        let selected_parent = selected
            .lease()
            .record
            .parent
            .as_ref()
            .ok_or(FsError::Denied)?;
        Ok(object.identity() == selected.lease().object()?.identity()
            && lease.record.name.as_ref() == Some(&permit.leaf)
            && self.locator_locked(parent, &permit.leaf, Some(object.identity()))?
                == self.locator_locked(selected_parent, &permit.leaf, Some(object.identity()))?)
    }
    // Only the verified committed-history path may call this internal hook.
    // Its input is the actual live native pin, never a caller inode DTO. The
    // canonical signed commit sidecar must already own this capture.
    pub(crate) fn advance_selected_lineage_from_history(
        &self,
        parent: &DirectoryGrant,
        pin: &crate::recovery::NativeReplacementPin,
    ) -> Result<(), FsError> {
        let Some(permit) = lock(&self.inner.selected_replace)?.clone() else {
            return Ok(());
        };
        pin.check(self, parent)?;
        pin.verify_owned_history(self)?;
        if pin.source_owner() != permit.record.source_owner {
            return Err(FsError::Denied);
        }
        let (_, step) = self.operation_store()?.find_recovery(&pin.capture_id())?;
        if step.phase != "ReplaceHistoryOwned" {
            return Err(FsError::Denied);
        }
        let new = GrantedEntry::File(self.lookup_file_for(
            &permit.parent,
            &permit.leaf,
            NativeFileAccess::Write,
        )?);
        if self.entry_identity(&new)? != pin.destination_identity() {
            return Err(FsError::StaleEntry);
        }
        let table = self.call()?;
        self.validate(&table, &permit.root, Some(FsRight::Enumerate))?;
        let mut selected = lock(&permit.selected)?;
        let old = selected.lease().object()?.identity();
        if [old.device, old.inode] != pin.victim_identity()
            && [old.device, old.inode] != pin.destination_identity()
        {
            return Err(FsError::StaleEntry);
        }
        *selected = new;
        drop(selected);
        drop(table);
        Ok(())
    }
    pub(crate) fn same_session(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
    pub(crate) fn same_created_entry(
        &self,
        original: &GrantedEntry,
        entry: &GrantedEntry,
    ) -> Result<bool, FsError> {
        Ok(self.mutation_source(original, FsRight::Rename)?.locator
            == self.mutation_source(entry, FsRight::Rename)?.locator)
    }
    pub(crate) fn set_created_temporary_permissions(
        &self,
        parent: &DirectoryGrant,
        original: &GrantedEntry,
        mode: u32,
    ) -> Result<(), FsError> {
        if mode & !0o7777 != 0 {
            return Err(FsError::Denied);
        }
        let expected = self.entry_identity(original)?;
        let name = original
            .lease()
            .record
            .name
            .as_ref()
            .ok_or(FsError::Denied)?;
        let opened = self.lookup_file_for(parent, name, NativeFileAccess::Write)?;
        let table = self.call()?;
        self.validate(&table, &parent.lease, Some(FsRight::Create))?;
        self.validate(&table, original.lease(), Some(FsRight::Write))?;
        self.validate(&table, &opened.lease, Some(FsRight::Write))?;
        let NativeObject::File {
            descriptor,
            identity,
        } = opened.lease.object()?
        else {
            return Err(FsError::Denied);
        };
        if [identity.device, identity.inode] != expected {
            return Err(FsError::StaleEntry);
        }
        rustix::fs::fchmod(&*descriptor, rustix::fs::Mode::from_raw_mode(mode as _))
            .map_err(|error| io_error(error.into()))?;
        let actual = descriptor.metadata().map_err(io_error)?;
        if actual.mode() & 0o7777 != mode {
            return Err(FsError::Denied);
        }
        self.validate(&table, original.lease(), Some(FsRight::Write))
    }
    pub(crate) fn check_created_temporary_capacity(&self) -> Result<(), FsError> {
        self.check_alive()?;
        let mut entries = lock(&self.inner.created_temporaries)?;
        entries.retain(|entry| entry.strong_count() > 0);
        if entries.len() >= 256 {
            return Err(FsError::LimitExceeded);
        }
        drop(entries);
        Ok(())
    }
    pub(crate) fn register_created_temporary(
        &self,
        created: &Arc<crate::operations::NativeCreatedTemporary>,
    ) -> Result<(), FsError> {
        self.check_alive()?;
        let mut entries = lock(&self.inner.created_temporaries)?;
        entries.retain(|entry| entry.strong_count() > 0);
        if entries.len() >= 256 {
            return Err(FsError::LimitExceeded);
        }
        entries.push(Arc::downgrade(created));
        drop(entries);
        Ok(())
    }
    pub(crate) fn validate_selected_victim(&self, victim: &GrantedEntry) -> Result<(), FsError> {
        if !self.has_selected_replace_authority()? {
            return Ok(());
        }
        let table = self.call()?;
        self.validate(&table, victim.lease(), Some(FsRight::Replace))?;
        let NativeObject::File { descriptor, .. } = victim.lease().object()? else {
            return Err(FsError::Denied);
        };
        if descriptor.metadata().map_err(io_error)?.nlink() != 1 {
            return Err(FsError::Unsupported(
                "selected atomic save cannot preserve multiple hard links",
            ));
        }
        self.validate(&table, victim.lease(), Some(FsRight::Replace))
    }
    pub(crate) fn validate_selected_temporary(&self, entry: &GrantedEntry) -> Result<(), FsError> {
        let Some(permit) = lock(&self.inner.selected_replace)?.clone() else {
            return Ok(());
        };
        let source = self.mutation_source(entry, FsRight::Rename)?;
        if source.locator
            != self.mutation_locator(
                &permit.parent,
                &source.name,
                Some(source.object.identity()),
            )?
        {
            return Err(FsError::Denied);
        }
        let entries: Vec<_> = lock(&self.inner.created_temporaries)?
            .iter()
            .filter_map(Weak::upgrade)
            .collect();
        for temporary in entries {
            if temporary.matches_entry(self, entry) {
                return temporary.check();
            }
        }
        Err(FsError::Denied)
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use crate::{FsOwner, NativeGrantAuthority};
    #[test]
    fn named_file_cannot_admit_selected_replace_or_widen_original_home_grant() {
        let directory = tempfile::tempdir().unwrap();
        let authority = NativeGrantAuthority::new();
        let owner = FsOwner {
            workspace: "selected".into(),
            app_id: "so.tilo.file-explorer".into(),
        };
        let root = authority
            .admit_directory(
                std::fs::File::open(directory.path()).unwrap(),
                owner.clone(),
                vec![
                    FsRight::Enumerate,
                    FsRight::Write,
                    FsRight::Create,
                    FsRight::Rename,
                ],
            )
            .unwrap();
        let session = authority
            .bind(
                owner,
                uuid::Uuid::new_v4(),
                std::slice::from_ref(&root),
                &[],
            )
            .unwrap();
        let named = directory.path().join("caller-permit");
        std::fs::write(&named, b"caller metadata").unwrap();
        assert!(
            session
                .bind_selected_replace_authority(&root, std::fs::File::open(&named).unwrap())
                .is_err()
        );
        assert!(!session.has_selected_replace_authority().unwrap());
        let table = session.call().unwrap();
        assert!(matches!(
            session.validate(&table, &root.lease, Some(FsRight::Replace)),
            Err(FsError::Denied)
        ));
        assert_eq!(std::fs::read(&named).unwrap(), b"caller metadata");
    }
}
