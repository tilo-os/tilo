//! Custody of an entry genuinely created by one completed native job. Cleanup
//! removes its namespace binding into protected recovery; it never purges bytes.
use super::{
    Arc, Context, DirectoryGrant, EntryName, FsError, FsJob, FsRight, FsSession, GrantedEntry,
    ItemOutcome, ItemState, MetadataExt, Mutex, NativeIdentity, OperationId, OperationOwner,
    OsStrExt, OsString, PreparedNativeReplacement, RecoveryKind, Request, Work, admit, check_named,
    ensure_recovery, io_error, lock, no_replace, sync_dir,
};

pub struct NativeCreatedTemporary {
    session: FsSession,
    owner: Arc<OperationOwner>,
    creation: OperationId,
    parent: DirectoryGrant,
    entry: GrantedEntry,
    submitted: Mutex<bool>,
}
impl NativeCreatedTemporary {
    pub(super) fn mint(
        work: &Work,
        parent: &DirectoryGrant,
        entry: &GrantedEntry,
    ) -> Result<Self, FsError> {
        let source = work.session.0.mutation_source(entry, FsRight::Rename)?;
        let step = work.owner.step(&work.id, 0)?;
        if step.phase != "Published"
            || step.destination.as_ref() != Some(&source.locator)
            || step
                .fingerprint
                .as_ref()
                .is_none_or(|actual| actual.identity != source.object.identity())
        {
            return Err(FsError::StaleEntry);
        }
        Ok(Self {
            session: work.session.0.clone(),
            owner: work.owner.clone(),
            creation: work.id.clone(),
            parent: parent.clone(),
            entry: entry.clone(),
            submitted: Mutex::new(false),
        })
    }
    pub(crate) fn register(self: &Arc<Self>) -> Result<(), FsError> {
        self.session.register_created_temporary(self)
    }
    pub(crate) fn matches_entry(&self, session: &FsSession, entry: &GrantedEntry) -> bool {
        self.session.same_session(session)
            && self
                .session
                .same_created_entry(&self.entry, entry)
                .unwrap_or(false)
    }
    pub(crate) fn selected_entry(&self, session: &FsSession) -> Result<GrantedEntry, FsError> {
        if !self.session.same_session(session) {
            return Err(FsError::Denied);
        }
        self.check()?;
        Ok(self.entry.clone())
    }
    /// Revalidate the actual creation's durable completion, live original parent
    /// and exact held created inode. No caller name can change this custody.
    /// # Errors
    /// Refuses another owner, unfinished creation, namespace substitution or revoke.
    pub fn check_scope(&self) -> Result<(), FsError> {
        if !Arc::ptr_eq(&self.session.operation_store()?, &self.owner) {
            return Err(FsError::Denied);
        }
        let report = self.owner.report(&self.creation)?;
        if report.state != crate::recovery::OperationState::Finished
            || report.items.len() != 1
            || report
                .items
                .first()
                .is_none_or(|item| item.state != ItemState::Completed)
        {
            return Err(FsError::Denied);
        }
        self.session.check_directory(&self.parent)?;
        self.session.metadata(&self.entry)?;
        Ok(())
    }
    /// Revalidate the exact named created inode before submitting retirement.
    /// # Errors
    /// Refuses namespace substitution, aliases and expired creation authority.
    pub fn check(&self) -> Result<(), FsError> {
        self.check_scope()?;
        let source = self.session.mutation_source(&self.entry, FsRight::Rename)?;
        let metadata = source
            .parent
            .symlink_metadata(source.name.as_os_str())
            .map_err(io_error)?;
        if !metadata.is_file() || metadata.nlink() != 1 {
            return Err(FsError::Denied);
        }
        Ok(())
    }
    /// Set permissions only on this genuine own-created save temporary.
    /// # Errors
    /// Refuses retired custody, substituted names, unsupported bits and original
    /// kernel permission denial. It changes no selected victim or sibling.
    pub fn set_permissions(&self, mode: u32) -> Result<(), FsError> {
        let submitted = lock(&self.submitted)?;
        if *submitted || mode & !0o7777 != 0 {
            return Err(FsError::Denied);
        }
        self.check()?;
        let result =
            self.session
                .set_created_temporary_permissions(&self.parent, &self.entry, mode);
        drop(submitted);
        result
    }
    /// A creator capability alone does not admit generic replacement. The
    /// selected sealed scope is specifically `AtomicSave`; trusted general
    /// replacement remains the separate `FsSession::prepare_replace` operation.
    /// # Errors
    /// Refuses generic replacement without widening the selected Save permit.
    pub fn prepare_replacement(&self) -> Result<PreparedNativeReplacement, FsError> {
        self.check()?;
        // A selected AtomicSave permit cannot become generic rename authority.
        Err(FsError::Denied)
    }
    /// Reserve a document-preserving Save only through the actual sealed
    /// selected Author scope and this exact completed creator capability.
    /// # Errors
    /// Refuses missing selected Save admission, stale creation or revoked scope.
    pub fn prepare_atomic_save(&self) -> Result<PreparedNativeReplacement, FsError> {
        self.session
            .selected_replacement_authority()?
            .prepare_atomic_save(self)
    }
    /// One-use retirement through the same genuine native operation owner. The
    /// old bytes remain recoverable under protected custody, independently of Purge.
    pub fn retire(self: &Arc<Self>) -> Result<FsJob, FsError> {
        let mut submitted = lock(&self.submitted)?;
        if *submitted {
            return Err(FsError::Denied);
        }
        self.check()?;
        let job = admit(
            &self.session,
            self.owner.clone(),
            Request::RetireCreatedTemporary(self.clone()),
            1,
        )?;
        *submitted = true;
        drop(submitted);
        Ok(job)
    }
}

pub(super) fn retire(ctx: &Context<'_>, temporary: &NativeCreatedTemporary) -> ItemOutcome {
    match perform_retire(ctx, temporary) {
        Ok(outcome) => outcome,
        Err(error) => ctx.failure(error),
    }
}
fn perform_retire(
    ctx: &Context<'_>,
    temporary: &NativeCreatedTemporary,
) -> Result<ItemOutcome, FsError> {
    temporary.check()?;
    let source = ctx
        .session()
        .mutation_source(&temporary.entry, FsRight::Rename)?;
    let custody = ctx
        .session()
        .created_temporary_custody(&temporary.parent, &temporary.entry)?;
    let stage = EntryName::new(OsString::from(format!(
        "created-{}-0",
        ctx.work.id.native()
    )))?;
    let mut step = ctx.work.owner.step(&ctx.work.id, ctx.item)?;
    step.source = Some(source.locator.clone());
    step.stage = Some(crate::native::NativeLocator {
        root: custody.identity,
        parents: Vec::new(),
        name: stage.as_os_str().as_bytes().to_vec(),
        identity: None,
    });
    ctx.checkpoint(&mut step, "CreatedTemporaryCaptureStarted")?;
    let mut captured = false;
    let effect = ctx.session().created_temporary_effect(
        &custody,
        &temporary.parent,
        &temporary.entry,
        || {
            ctx.cancelled()?;
            check_named(&source.parent, &source.name, source.object.identity())?;
            #[cfg(test)]
            ctx.barrier("CreatedTemporaryCaptureBoundary");
            no_replace(&source.parent, &source.name, &custody.directory, &stage)
                .map_err(|e| io_error(e.into()))?;
            captured = true;
            sync_dir(&source.parent)?;
            sync_dir(&custody.directory)
        },
    );
    if let Err(error) = effect {
        return if captured {
            Ok(ctx.recovery(step, RecoveryKind::UnknownNativeCommit, error, None))
        } else {
            Err(error)
        };
    }
    let observed = match custody.directory.symlink_metadata(stage.as_os_str()) {
        Ok(metadata) => metadata,
        Err(error) => {
            return Ok(ctx.recovery(
                step,
                RecoveryKind::UnknownNativeCommit,
                io_error(error),
                None,
            ));
        }
    };
    let actual = NativeIdentity::of(&observed);
    step.stage
        .as_mut()
        .ok_or(FsError::StoreUnavailable)?
        .identity = Some(actual);
    let recovery = ensure_recovery(&mut step, RecoveryKind::QuarantinedNative);
    if actual != source.object.identity() || !observed.is_file() || observed.nlink() != 1 {
        return Ok(ctx.recovery(
            step,
            RecoveryKind::QuarantinedNative,
            FsError::StaleEntry,
            None,
        ));
    }
    if let Err(error) = ctx.checkpoint(&mut step, "CreatedTemporaryCaptured") {
        return Ok(ctx.recovery(step, RecoveryKind::UnknownNativeCommit, error, None));
    }
    let mut completed = ctx.completed(None);
    completed.recoveries.push(recovery);
    Ok(completed)
}
