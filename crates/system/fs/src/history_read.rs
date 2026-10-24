//! Read-only immutable history transport, independent of restore eligibility.
use super::{Access, EBUSY, EINVAL, EIO, ENOENT, Hash, QuickFS};
use fs_protocol::files::{
    FileHistoryReadErrorV1 as Error, FileHistoryReadQueryV1 as Query,
    FileHistoryReadScopeV1 as Scope, FileHistoryReadValueV1 as Value,
};

impl QuickFS {
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the frontier hashes one consistent snapshot of namespace, history and ACL"
    )]
    #[expect(
        clippy::suspicious_operation_groupings,
        reason = "a history version's `id` is the version id the content revision points at"
    )]
    fn history_read_snapshot(
        &self,
        ino: u64,
    ) -> Result<
        (
            Scope,
            crate::history::RecordHistorySnapshot,
            Option<fs_protocol::ContentRevisionV1>,
        ),
        Error,
    > {
        let namespace = self.namespace.lock().map_err(|_| Error::Unavailable)?;
        let history = self
            .history
            .as_ref()
            .ok_or(Error::Unavailable)?
            .0
            .lease()
            .map_err(|_| Error::Unavailable)?;
        let acl = self
            .enforcement
            .as_ref()
            .map(|(acl, _)| acl.admission_lease())
            .transpose()
            .map_err(|()| Error::NotVisible)?;
        let path = namespace.path_for(ino).ok_or(Error::NotVisible)?;
        if let (Some(acl), Some((_, identity))) = (acl.as_deref(), self.enforcement.as_ref())
            && !acl.permits(identity, &path, Access::Read)
        {
            return Err(Error::NotVisible);
        }
        let record_id = namespace.record_id_for(ino).ok_or(Error::NotVisible)?;
        let snapshot = history.snapshot(record_id).ok_or(Error::Unavailable)?;
        let current_revision = namespace.content_revision_for(ino);
        if let Some(revision) = current_revision
            && (revision.validate().is_err()
                || revision.record_id != record_id
                || !snapshot.versions.iter().any(|v| {
                    v.id == revision.version_id
                        && v.content_hash == revision.content_hash
                        && v.metadata_hash == revision.metadata_hash
                }))
        {
            return Err(Error::Unavailable);
        }
        let workspace_id = self.restore_workspace().map_err(|_| Error::Unavailable)?;
        // Entire immutable set (including late backfill), order, heads and
        // binding identity participate. A cursor is never a live-list offset.
        let binding = namespace
            .checked_record_frontier(ino)
            .ok_or(Error::Unavailable)?;
        let bytes = serde_json::to_vec(&(
            "tilo.history-read.v1",
            workspace_id,
            self.mount_lifetime,
            record_id,
            &path,
            binding,
            &snapshot,
            current_revision,
        ))
        .map_err(|_| Error::Unavailable)?;
        Ok((
            Scope {
                workspace_id,
                mount_lifetime: self.mount_lifetime,
                record_id,
                frontier: *blake3::hash(&bytes).as_bytes(),
            },
            snapshot,
            current_revision,
        ))
    }
    pub fn read_file_history(&self, ino: u64, query: Query) -> Result<Value, Error> {
        let (scope, snapshot, current_revision) = self.history_read_snapshot(ino)?;
        // Read after the history snapshot released the namespace and history
        // locks: the record repository is never taken under them.
        let attributions = self.version_attributions(scope.record_id);
        snapshot.read_query(scope, current_revision, query, &attributions)
    }
    /// Who caused each of `record_id`'s versions an agent caused, as the
    /// version's own signing device recorded it in the signed record
    /// (`AttributeVersion`). Empty without signed records.
    fn version_attributions(
        &self,
        record_id: fs_protocol::FileRecordId,
    ) -> Vec<fs_protocol::FileVersionAttributionV1> {
        self.record_publisher
            .as_ref()
            .and_then(|publisher| publisher.snapshot(record_id).ok().flatten())
            .map(|record| record.attributions)
            .unwrap_or_default()
    }

    /// Read a version preview, provided the history frontier still matches.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Stale`] when the frontier moved, otherwise the errors of
    /// [`Self::read_file_history_preview`].
    pub fn read_file_history_preview_at_frontier(
        &self,
        ino: u64,
        frontier: [u8; 32],
        version_id: fs_protocol::FileVersionId,
    ) -> Result<Vec<u8>, Error> {
        let (scope, _, _) = self.history_read_snapshot(ino)?;
        if scope.frontier != frontier {
            return Err(Error::Stale);
        }
        self.read_file_history_preview(ino, scope, version_id)
    }
    /// Read the verified content of one history version as a preview.
    pub fn read_file_history_preview(
        &self,
        ino: u64,
        scope: Scope,
        version_id: fs_protocol::FileVersionId,
    ) -> Result<Vec<u8>, Error> {
        self.read_file_history_preview_using(ino, scope, version_id, |hash| {
            // Bounded: a preview reads at most one byte past its limit.
            let limit = u64::try_from(fs_protocol::MAX_HISTORY_PREVIEW_BYTES)
                .unwrap_or(u64::MAX)
                .saturating_add(1);
            self.hydrator
                .block_on(self.hydrator.read(Hash::from(hash), 0, limit))
                .map_err(|_| Error::Unavailable)
        })
    }
    pub(super) fn read_file_history_preview_using(
        &self,
        ino: u64,
        scope: Scope,
        version_id: fs_protocol::FileVersionId,
        load: impl FnOnce([u8; 32]) -> Result<Vec<u8>, Error>,
    ) -> Result<Vec<u8>, Error> {
        let Value::Version {
            scope: actual,
            version,
            ..
        } = self.read_file_history(
            ino,
            Query::Version {
                frontier: scope.frontier,
                version_id,
            },
        )?
        else {
            return Err(Error::InvalidRequest);
        };
        if actual != scope {
            return Err(Error::Stale);
        }
        let hash = version.content_hash.ok_or(Error::NoContent)?;
        let bytes = load(hash.0)?;
        // Hydration releases all metadata locks. Recheck authorization and
        // actual record after the wait, even if immutable bytes were cached.
        let (current, snapshot, _) = self.history_read_snapshot(ino)?;
        if current.workspace_id != scope.workspace_id
            || current.mount_lifetime != scope.mount_lifetime
            || current.record_id != scope.record_id
        {
            return Err(Error::Stale);
        }
        if !snapshot.versions.iter().any(|v| v == &version) {
            return Err(Error::MissingVersion);
        }
        if bytes.len() > fs_protocol::MAX_HISTORY_PREVIEW_BYTES {
            return Err(Error::TooLarge);
        }
        // FileVersion content_hash identifies the chunk manifest, not raw
        // bytes. The bytes must be exactly the chunks the manifest names.
        let manifest = self
            .hydrator
            .block_on(self.hydrator.local_manifest_index(Hash::from(hash.0)));
        if !manifest.is_some_and(|manifest| manifest.verify(&bytes)) {
            return Err(Error::Unavailable);
        }
        Ok(bytes)
    }
}

pub(super) fn errno(error: Error) -> i32 {
    match error {
        Error::Stale => EBUSY,
        Error::NotVisible => ENOENT,
        Error::MissingVersion | Error::NoContent => libc::ENODATA,
        Error::InvalidRequest => EINVAL,
        Error::Unavailable => EIO,
        Error::TooLarge => libc::E2BIG,
    }
}
