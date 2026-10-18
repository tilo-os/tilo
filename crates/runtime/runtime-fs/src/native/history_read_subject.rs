//! Original-source readable inode custody for canonical external history queries.
//! Observation bytes never create an access grant or a canonical record identity.
use super::{
    Arc, Dir, DirectoryGrant, EntryName, FileGrant, FsError, FsRight, FsSession, NativeFileAccess,
    NativeIdentity, NativeObject, Table, io_error, lock,
};

const MAX_HISTORY_PARENTS: usize = 64;
const MAX_HISTORY_LOCATOR_BYTES: usize = 4096;

/// Descriptive facts observed through a genuine live retained Read subject.
/// A receiver still needs its independently authenticated source capability and
/// admitted current person/Read context; this value is not an enrollment token.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeHistoryReadObservation {
    pub source_owner: [u8; 32],
    pub root: [u64; 2],
    pub parent: [u64; 2],
    pub parents: Vec<Vec<u8>>,
    pub name: Vec<u8>,
    pub identity: [u64; 2],
}

/// Source-resident original Read custody. Only `FsSession` can mint it from a
/// genuinely opened readable child of its already admitted directory grant.
pub struct NativeHistoryReadSubject {
    session: FsSession,
    parent: DirectoryGrant,
    file: FileGrant,
    name: EntryName,
    observation: NativeHistoryReadObservation,
}

impl FsSession {
    /// Retain a readable regular child for metadata-only canonical history.
    /// No bytes, names, modes or rights are changed; source credentials and
    /// actual parent Read+Enumerate govern the same no-follow open as normal IO.
    /// # Errors
    /// Refuses absent retained source custody/Read, nonregular/symlink children,
    /// oversized lineage, changed names, or stale/revoked original authority.
    pub fn history_read_subject(
        &self,
        parent: &DirectoryGrant,
        name: &EntryName,
    ) -> Result<Arc<NativeHistoryReadSubject>, FsError> {
        let source_owner = self.retained_source_owner_key()?;
        let file = self.lookup_file_for(parent, name, NativeFileAccess::Read)?;
        let table = self.call()?;
        self.validate(&table, &parent.lease, Some(FsRight::Enumerate))?;
        self.validate(&table, &file.lease, Some(FsRight::Read))?;
        let NativeObject::File { identity, .. } = file.lease.object()? else {
            return Err(FsError::Unsupported("regular history subject required"));
        };
        let locus = self.locator_locked(&parent.lease, name, Some(identity))?;
        let custody = lock(&self.inner.removal)?.clone().ok_or(FsError::Denied)?;
        custody.check()?;
        self.validate(&table, &custody.root, Some(FsRight::Enumerate))?;
        if locus.root != custody.root.object()?.identity()
            || *lock(&custody.owner_key)? != Some(source_owner)
            || locus.name.len() > 255
            || locus.parents.len() > MAX_HISTORY_PARENTS
            || locus.parents.iter().any(|part| part.len() > 255)
            || locus
                .parents
                .iter()
                .try_fold(locus.name.len(), |sum, part| sum.checked_add(part.len()))
                .is_none_or(|sum| sum > MAX_HISTORY_LOCATOR_BYTES)
        {
            return Err(FsError::Denied);
        }
        let parent_identity = parent.lease.object()?.identity();
        let observation = NativeHistoryReadObservation {
            source_owner,
            root: [locus.root.device, locus.root.inode],
            parent: [parent_identity.device, parent_identity.inode],
            parents: locus.parents,
            name: locus.name,
            identity: [identity.device, identity.inode],
        };
        drop(table);
        let subject = Arc::new(NativeHistoryReadSubject {
            session: self.clone(),
            parent: parent.clone(),
            file,
            name: name.clone(),
            observation,
        });
        subject.check()?;
        Ok(subject)
    }
}

impl NativeHistoryReadSubject {
    /// Validate original authority and held readable inode independently of its
    /// current name. This does not authorize a history query for a moved name.
    /// # Errors
    /// Refuses retired source/person scope, revoked grants or wrong source root.
    pub fn check_scope(&self) -> Result<(), FsError> {
        let table = self.session.call()?;
        self.scope_locked(&table)
    }
    fn scope_locked(&self, table: &Table) -> Result<(), FsError> {
        self.session
            .validate(table, &self.parent.lease, Some(FsRight::Enumerate))?;
        self.session
            .validate(table, &self.file.lease, Some(FsRight::Read))?;
        self.session.metadata_locked(table, &self.file.lease)?;
        let custody = lock(&self.session.inner.removal)?
            .clone()
            .ok_or(FsError::Denied)?;
        self.session
            .validate(table, &custody.root, Some(FsRight::Enumerate))?;
        custody.check()?;
        let root = custody.root.object()?.identity();
        if [root.device, root.inode] != self.observation.root
            || *lock(&custody.owner_key)? != Some(self.observation.source_owner)
        {
            return Err(FsError::Denied);
        }
        Ok(())
    }
    /// Match the actual named entry to the held original readable inode. This
    /// prevents a stale history subject from following a substituted file.
    /// # Errors
    /// Refuses substitution/deletion, stale scope and kernel metadata errors.
    pub fn check(&self) -> Result<(), FsError> {
        let table = self.session.call()?;
        self.scope_locked(&table)?;
        let parent = self.named_parent_locked(&table)?;
        let observed = parent
            .symlink_metadata(self.name.as_os_str())
            .map_err(io_error)?;
        let identity = NativeIdentity::of(&observed);
        if !observed.is_file() || [identity.device, identity.inode] != self.observation.identity {
            return Err(FsError::StaleEntry);
        }
        let result = self.scope_locked(&table);
        drop(table);
        result
    }
    // Rewalk the exact granted raw lineage from the held source root. A parent
    // descriptor alone must not silently authorize its old locus after a move.
    #[cfg(target_os = "linux")]
    fn named_parent_locked(&self, table: &Table) -> Result<Arc<Dir>, FsError> {
        use std::os::unix::ffi::OsStrExt;
        let custody = lock(&self.session.inner.removal)?
            .clone()
            .ok_or(FsError::Denied)?;
        self.session
            .validate(table, &custody.root, Some(FsRight::Enumerate))?;
        let mut current = FsSession::directory(&custody.root)?;
        for component in &self.observation.parents {
            let descriptor = rustix::fs::openat(
                &*current,
                std::ffi::OsStr::from_bytes(component),
                rustix::fs::OFlags::PATH
                    | rustix::fs::OFlags::DIRECTORY
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::empty(),
            )
            .map_err(|error| io_error(error.into()))?;
            current = Arc::new(Dir::from_std_file(std::fs::File::from(descriptor)));
        }
        let identity = NativeIdentity::of(&current.dir_metadata().map_err(io_error)?);
        if [identity.device, identity.inode] != self.observation.parent {
            return Err(FsError::StaleEntry);
        }
        Ok(current)
    }
    #[cfg(not(target_os = "linux"))]
    fn named_parent_locked(&self, table: &Table) -> Result<Arc<Dir>, FsError> {
        self.scope_locked(table)?;
        Err(FsError::Unsupported(
            "retained history requires Linux source lineage",
        ))
    }
    /// Bounded descriptive locus only, checked before and after observation.
    /// # Errors
    /// Refuses changed named identity or retired original Read custody.
    pub fn observe(&self) -> Result<NativeHistoryReadObservation, FsError> {
        self.check()?;
        let observed = self.observation.clone();
        self.check()?;
        Ok(observed)
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use crate::operations::tests::history_read_for_transport;

    #[test]
    fn actual_read_subject_preserves_raw_locus_and_refuses_revocation() {
        let fixture = history_read_for_transport(true);
        let subject = fixture
            .session
            .history_read_subject(&fixture.parent, &fixture.name)
            .unwrap();
        let observed = subject.observe().unwrap();
        assert_eq!(observed.source_owner, [37; 32]);
        assert_eq!(observed.parents, vec![b"nested".to_vec()]);
        assert_eq!(observed.name, b"document-\xff");
        assert_ne!(observed.identity[1], 0);
        assert_ne!(observed.parent[1], observed.identity[1]);
        fixture.revoke().unwrap();
        assert!(subject.check_scope().is_err());
        assert!(subject.check().is_err());
        assert!(subject.observe().is_err());
    }
    #[test]
    fn actual_read_subject_refuses_symlink_and_closed_session() {
        let fixture = history_read_for_transport(true);
        let alias = fixture.link_name();
        assert!(
            fixture
                .session
                .history_read_subject(&fixture.parent, &alias)
                .is_err()
        );
        let subject = fixture
            .session
            .history_read_subject(&fixture.parent, &fixture.name)
            .unwrap();
        fixture.session.close();
        assert!(matches!(subject.check_scope(), Err(FsError::Closed)));
        assert!(subject.observe().is_err());
    }
    #[test]
    fn actual_enumerate_without_read_does_not_mint_history_subject() {
        let fixture = history_read_for_transport(false);
        assert!(matches!(
            fixture
                .session
                .history_read_subject(&fixture.parent, &fixture.name),
            Err(FsError::Denied)
        ));
    }
    #[test]
    fn actual_held_read_subject_does_not_follow_replaced_leaf() {
        let fixture = history_read_for_transport(true);
        let subject = fixture
            .session
            .history_read_subject(&fixture.parent, &fixture.name)
            .unwrap();
        let before = subject.observe().unwrap();
        fixture.substitute_leaf();
        subject.check_scope().unwrap();
        assert!(matches!(subject.check(), Err(FsError::StaleEntry)));
        assert!(subject.observe().is_err());
        let current = fixture
            .session
            .history_read_subject(&fixture.parent, &fixture.name)
            .unwrap()
            .observe()
            .unwrap();
        assert_ne!(current.identity, before.identity);
    }
    #[test]
    fn actual_held_read_subject_refuses_changed_parent_locus() {
        let fixture = history_read_for_transport(true);
        let subject = fixture
            .session
            .history_read_subject(&fixture.parent, &fixture.name)
            .unwrap();
        subject.check().unwrap();
        fixture.move_parent();
        subject.check_scope().unwrap();
        assert!(matches!(subject.check(), Err(FsError::StaleEntry)));
        assert!(subject.observe().is_err());
    }
}
