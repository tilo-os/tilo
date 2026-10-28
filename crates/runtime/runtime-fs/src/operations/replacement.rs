//! Atomic external namespace replacement under genuine original-user rights.
#[cfg(not(target_os = "linux"))]
use super::ItemOutcome;
#[cfg(target_os = "linux")]
use super::{
    Arc, Dir, FsRight, ItemOutcome, MetadataExt, MutationSource, NativeIdentity, NativeObject,
    OsStrExt, RecoveryKind, Step, check_named, ensure_recovery, io_error, metadata_facts,
    no_replace, replacement_intent, sync_dir,
};
use super::{Context, DirectoryGrant, EntryName, FsError, GrantedEntry, OperationId, OsString};
#[cfg(target_os = "linux")]
use crate::native::{removal::RemovalCustody, xattrs::NativeAttributes};

pub(super) fn stage_component(id: &OperationId, item: usize) -> Result<EntryName, FsError> {
    EntryName::new(OsString::from(format!("replace-{}-{}", id.native(), item)))
        .map_err(FsError::from)
}
#[cfg(target_os = "linux")]
fn stage_name(ctx: &Context<'_>) -> Result<EntryName, FsError> {
    stage_component(&ctx.work.id, ctx.item)
}

#[cfg(target_os = "linux")]
fn captured_outcome(ctx: &Context<'_>, step: Step, error: FsError, exchanged: bool) -> ItemOutcome {
    ctx.recovery(
        step,
        if exchanged {
            RecoveryKind::UnknownNativeCommit
        } else {
            RecoveryKind::QuarantinedNative
        },
        error,
        None,
    )
}
#[cfg(target_os = "linux")]
pub(super) fn replace_entry(
    ctx: &Context<'_>,
    entry: &GrantedEntry,
    into: &DirectoryGrant,
    name: &EntryName,
    victim: &GrantedEntry,
) -> Result<ItemOutcome, FsError> {
    let ReplacementPreparation {
        source,
        old,
        destination,
        custody,
        permissions,
        attributes,
    } = validate_replacement(ctx, entry, into, name, victim)?;
    let stage = stage_name(ctx)?;
    let mut step = ctx.work.owner.step(&ctx.work.id, ctx.item)?;
    step.source = Some(source.locator.clone());
    step.destination = Some(ctx.session().mutation_locator(
        into,
        name,
        Some(source.object.identity()),
    )?);
    step.stage = Some(crate::native::NativeLocator {
        root: custody.identity,
        parents: Vec::new(),
        name: stage.as_os_str().as_bytes().to_vec(),
        identity: None,
    });
    ctx.checkpoint(&mut step, "ReplaceTempCaptureStarted")?;
    let mut captured = false;
    let result = ctx.effect(Some((entry, FsRight::Rename)), Some(into), || {
        custody.check()?;
        check_named(&source.parent, &source.name, source.object.identity())?;
        no_replace(&source.parent, &source.name, &custody.directory, &stage)
            .map_err(|e| io_error(e.into()))?;
        captured = true;
        sync_dir(&source.parent)?;
        sync_dir(&custody.directory)
    });
    if let Err(error) = result {
        return if captured {
            Ok(captured_outcome(ctx, step, error, false))
        } else {
            Err(error)
        };
    }
    let actual = match custody.directory.symlink_metadata(stage.as_os_str()) {
        Ok(metadata) => NativeIdentity::of(&metadata),
        Err(error) => return Ok(captured_outcome(ctx, step, io_error(error), false)),
    };
    step.stage
        .as_mut()
        .ok_or(FsError::StoreUnavailable)?
        .identity = Some(actual);
    if let Err(error) = ctx.checkpoint(&mut step, "ReplaceTempCaptured") {
        return Ok(captured_outcome(ctx, step, error, false));
    }
    if actual != source.object.identity() {
        return Ok(captured_outcome(ctx, step, FsError::StaleEntry, false));
    }
    let request = ExchangeRequest {
        entry,
        into,
        victim,
        expected_victim: old.object.identity(),
        permissions,
        attributes: &attributes,
        source: &source,
        custody: &custody,
        destination: &destination,
        stage: &stage,
        name,
    };
    let metadata_result = prepare_metadata(ctx, &request);
    if let Err(error) = metadata_result {
        return Ok(captured_outcome(ctx, step, error, false));
    }
    if let Err(error) = ctx.checkpoint(&mut step, "ReplaceExchangeStarted") {
        return Ok(captured_outcome(ctx, step, error, false));
    }
    exchange(ctx, &request, step)
}

#[cfg(target_os = "linux")]
struct ReplacementPreparation {
    source: MutationSource,
    old: MutationSource,
    destination: Arc<Dir>,
    custody: Arc<RemovalCustody>,
    permissions: (u32, u32, u32),
    attributes: NativeAttributes,
}

#[cfg(target_os = "linux")]
fn validate_replacement(
    ctx: &Context<'_>,
    entry: &GrantedEntry,
    into: &DirectoryGrant,
    name: &EntryName,
    victim: &GrantedEntry,
) -> Result<ReplacementPreparation, FsError> {
    if let Some(expected) = ctx.work.prepared_intent.as_ref()
        && replacement_intent(ctx.work, expected.owner)? != *expected
    {
        return Err(FsError::StaleEntry);
    }
    let source = ctx.session().mutation_source(entry, FsRight::Rename)?;
    ctx.session().mutation_source(entry, FsRight::Write)?;
    let old = ctx.session().mutation_source(victim, FsRight::Replace)?;
    ctx.session().validate_selected_victim(victim)?;
    let victim_metadata = old
        .parent
        .symlink_metadata(old.name.as_os_str())
        .map_err(io_error)?;
    let permissions = (
        victim_metadata.uid(),
        victim_metadata.gid(),
        victim_metadata.mode() & 0o7777,
    );
    if let Some(expected) = ctx.work.prepared_intent.as_ref()
        && permissions != expected.permissions
    {
        return Err(FsError::StaleEntry);
    }
    #[cfg(test)]
    ctx.barrier("ReplaceMetadataObservationBoundary");
    let attributes = ctx.session().replacement_attributes(victim)?;
    if ctx
        .work
        .prepared_intent
        .as_ref()
        .is_some_and(|expected| *expected.attributes != attributes)
    {
        return Err(FsError::StaleEntry);
    }
    let destination = ctx.session().mutation_directory(into)?;
    let custody = ctx.session().replacement_directory(entry)?;
    ctx.session().retained_source_owner_key()?;
    if !matches!(&source.object, NativeObject::File { .. })
        || !matches!(&old.object, NativeObject::File { .. })
    {
        return Err(FsError::Unsupported(
            "replacing rename requires regular files",
        ));
    }
    if NativeIdentity::of(&destination.dir_metadata().map_err(io_error)?)
        != NativeIdentity::of(&old.parent.dir_metadata().map_err(io_error)?)
        || name != &old.name
        || source.object.identity() == old.object.identity()
    {
        return Err(FsError::Denied);
    }
    let temporary_metadata = source
        .parent
        .symlink_metadata(source.name.as_os_str())
        .map_err(io_error)?;
    if ctx.work.prepared_intent.as_ref().is_some_and(|expected| {
        expected.facts
            != (
                metadata_facts(&temporary_metadata),
                metadata_facts(&victim_metadata),
            )
    }) {
        return Err(FsError::StaleEntry);
    }
    if temporary_metadata.nlink() != 1 {
        return Err(FsError::Unsupported(
            "replacement temporary must have one link",
        ));
    }
    Ok(ReplacementPreparation {
        source,
        old,
        destination,
        custody,
        permissions,
        attributes,
    })
}

#[cfg(target_os = "linux")]
fn prepare_metadata(ctx: &Context<'_>, request: &ExchangeRequest<'_>) -> Result<(), FsError> {
    // Original-user kernel permissions perform metadata preparation. Root never
    // changes ownership on the user's behalf. Refusal keeps destination intact
    // and the actual staged temporary available through canonical recovery.
    ctx.session().replacement_effect(
        request.custody,
        request.entry,
        request.victim,
        request.into,
        || {
            ctx.cancelled()?;
            check_named(
                &request.custody.directory,
                request.stage,
                request.source.object.identity(),
            )?;
            let held = rustix::fs::openat(
                &*request.custody.directory,
                request.stage.as_os_str(),
                rustix::fs::OFlags::WRONLY
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::NONBLOCK
                    | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::empty(),
            )
            .map_err(|e| io_error(e.into()))?;
            let actual = rustix::fs::fstat(&held).map_err(|e| io_error(e.into()))?;
            if [actual.st_dev, actual.st_ino]
                != [
                    request.source.object.identity().device,
                    request.source.object.identity().inode,
                ]
            {
                return Err(FsError::StaleEntry);
            }
            if actual.st_nlink != 1 {
                return Err(FsError::Unsupported(
                    "replacement temporary must have one link",
                ));
            }
            if actual.st_uid != request.permissions.0 || actual.st_gid != request.permissions.1 {
                rustix::fs::fchown(
                    &held,
                    Some(rustix::fs::Uid::from_raw(request.permissions.0)),
                    Some(rustix::fs::Gid::from_raw(request.permissions.1)),
                )
                .map_err(|e| io_error(e.into()))?;
            }
            rustix::fs::fchmod(
                &held,
                rustix::fs::Mode::from_raw_mode(request.permissions.2),
            )
            .map_err(|e| io_error(e.into()))?;
            request.attributes.apply(std::os::fd::AsFd::as_fd(&held))?;
            let metadata = rustix::fs::fstat(&held).map_err(|e| io_error(e.into()))?;
            if metadata.st_nlink != 1 {
                return Err(FsError::Unsupported(
                    "replacement temporary gained an alias",
                ));
            }
            if (metadata.st_uid, metadata.st_gid, metadata.st_mode & 0o7777) != request.permissions
            {
                return Err(FsError::Denied);
            }
            rustix::fs::fsync(&held).map_err(|e| io_error(e.into()))
        },
    )
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy)]
struct ExchangeRequest<'a> {
    entry: &'a GrantedEntry,
    into: &'a DirectoryGrant,
    victim: &'a GrantedEntry,
    expected_victim: NativeIdentity,
    permissions: (u32, u32, u32),
    attributes: &'a crate::native::xattrs::NativeAttributes,
    source: &'a MutationSource,
    custody: &'a crate::native::removal::RemovalCustody,
    destination: &'a Dir,
    stage: &'a EntryName,
    name: &'a EntryName,
}
#[cfg(target_os = "linux")]
fn exchange(
    ctx: &Context<'_>,
    request: &ExchangeRequest<'_>,
    mut step: Step,
) -> Result<ItemOutcome, FsError> {
    let ExchangeRequest {
        entry,
        into,
        victim,
        expected_victim,
        permissions,
        source,
        custody,
        destination,
        stage,
        name,
        ..
    } = *request;
    let selected_mode = ctx.session().has_selected_replace_authority()?;
    let mut exchanged = false;
    let result = ctx
        .session()
        .replacement_effect(custody, entry, victim, into, || {
            ctx.cancelled()?;
            check_named(&custody.directory, stage, source.object.identity())?;
            check_named(destination, name, expected_victim)?;
            if selected_mode
                && destination
                    .symlink_metadata(name.as_os_str())
                    .map_err(io_error)?
                    .nlink()
                    != 1
            {
                return Err(FsError::StaleEntry);
            }
            #[cfg(test)]
            ctx.barrier("ReplaceExchangeBoundary");
            rustix::fs::renameat_with(
                &*custody.directory,
                stage.as_os_str(),
                destination,
                name.as_os_str(),
                rustix::fs::RenameFlags::EXCHANGE,
            )
            .map_err(|e| io_error(e.into()))?;
            exchanged = true;
            sync_dir(destination)?;
            sync_dir(&custody.directory)
        });
    if let Err(error) = result {
        return Ok(captured_outcome(ctx, step, error, exchanged));
    }
    let captured_metadata = match custody.directory.symlink_metadata(stage.as_os_str()) {
        Ok(metadata) => metadata,
        Err(error) => return Ok(captured_outcome(ctx, step, io_error(error), true)),
    };
    let captured = NativeIdentity::of(&captured_metadata);
    step.stage
        .as_mut()
        .ok_or(FsError::StoreUnavailable)?
        .identity = Some(captured);
    let recovery = ensure_recovery(&mut step, RecoveryKind::QuarantinedNative);
    if let Some(record) = step.recovery.as_mut() {
        record.original_name = name.as_os_str().as_bytes().to_vec();
    }
    if let Err(error) = ctx.checkpoint(&mut step, "ReplaceVictimCaptured") {
        return Ok(captured_outcome(ctx, step, error, true));
    }
    // The original OS can add aliases after preparation. A selected save
    // must never report success for a victim that acquired another hard link.
    if selected_mode && captured_metadata.nlink() != 1 {
        return Ok(captured_outcome(ctx, step, FsError::StaleEntry, true));
    }
    if captured != expected_victim
        || (
            captured_metadata.uid(),
            captured_metadata.gid(),
            captured_metadata.mode() & 0o7777,
        ) != permissions
    {
        return Ok(ctx.recovery(
            step,
            RecoveryKind::QuarantinedNative,
            FsError::StaleEntry,
            None,
        ));
    }
    if ctx.work.prepared_intent.as_ref().is_some_and(|expected| {
        let observed = metadata_facts(&captured_metadata);
        observed.0 != expected.facts.1.0 || observed.1 != expected.facts.1.1
    }) {
        return Ok(captured_outcome(ctx, step, FsError::StaleEntry, true));
    }
    if let Err(error) = validate_captured_attributes(ctx, request) {
        return Ok(captured_outcome(ctx, step, error, true));
    }
    if let Err(error) = validate_published(ctx, request) {
        return Ok(captured_outcome(ctx, step, error, true));
    }
    let grant = ctx.session().mutation_result(into, name).ok();
    let mut outcome = ctx.completed(grant);
    outcome.recoveries.push(recovery);
    Ok(outcome)
}

#[cfg(target_os = "linux")]
fn validate_captured_attributes(
    ctx: &Context<'_>,
    request: &ExchangeRequest<'_>,
) -> Result<(), FsError> {
    let actual = ctx.session().replacement_captured_attributes(
        request.victim,
        request.custody,
        request.stage,
        request.expected_victim,
    )?;
    if &actual != request.attributes {
        return Err(FsError::StaleEntry);
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn validate_published(ctx: &Context<'_>, request: &ExchangeRequest<'_>) -> Result<(), FsError> {
    check_named(
        request.destination,
        request.name,
        request.source.object.identity(),
    )?;
    let published = request
        .destination
        .symlink_metadata(request.name.as_os_str())
        .map_err(io_error)?;
    if NativeIdentity::of(&published) != request.source.object.identity()
        || ctx.work.prepared_intent.as_ref().is_some_and(|expected| {
            let observed = metadata_facts(&published);
            observed.0 != expected.facts.0.0 || observed.1 != expected.facts.0.1
        })
    {
        return Err(FsError::StaleEntry);
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub(super) fn replace_entry(
    _: &Context<'_>,
    _: &GrantedEntry,
    _: &DirectoryGrant,
    _: &EntryName,
    _: &GrantedEntry,
) -> Result<ItemOutcome, FsError> {
    Err(FsError::Unsupported(
        "protected atomic replacement requires Linux",
    ))
}
