//! Raw-name directory capabilities on the connected service.
use super::{Arc, File, FileSource, Server, checked};
use crate::EntryName;
use crate::directory_backend::{
    JobUpdate, WorkspaceDirectoryBackend, WorkspaceJobBackend, WorkspaceMetadataBackend,
};
use std::future::Future;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::pin::Pin;
use tilo_fs_client::connection::{Capabilities, ServeObject};
use tilo_fs_client::wire::{CommittedPin, FsReply, FsRequest, OpenedFile};

fn name(raw: Vec<u8>) -> Result<EntryName, String> {
    if raw.len() > 4096 {
        return Err("component exceeds bound".into());
    }
    EntryName::new(std::ffi::OsString::from_vec(raw)).map_err(|error| error.to_string())
}

fn bound(value: usize, limit: usize, message: &str) -> Result<(), String> {
    if value > limit {
        return Err(message.into());
    }
    Ok(())
}

pub(super) struct Directory {
    pub(super) server: Arc<Server>,
    pub(super) backend: Arc<dyn WorkspaceDirectoryBackend>,
}

pub(super) fn directory_root(
    server: &Arc<Server>,
    backend: Arc<dyn WorkspaceDirectoryBackend>,
) -> Arc<dyn ServeObject> {
    Arc::new(Directory {
        server: Arc::clone(server),
        backend,
    })
}

pub(super) struct Job {
    server: Arc<Server>,
    job: Arc<dyn WorkspaceJobBackend>,
}

fn job_root(server: &Arc<Server>, job: Arc<dyn WorkspaceJobBackend>) -> Arc<dyn ServeObject> {
    Arc::new(Job {
        server: Arc::clone(server),
        job,
    })
}

impl ServeObject for Directory {
    #[allow(clippy::too_many_lines, reason = "one request table per object family")]
    fn dispatch(
        self: Arc<Self>,
        capabilities: &Capabilities,
        request: FsRequest,
    ) -> Pin<Box<dyn Future<Output = Result<FsReply, String>> + Send + 'static>> {
        let capabilities = capabilities.clone();
        Box::pin(async move {
            match request {
                FsRequest::HistoryQuery {
                    name: raw,
                    expected,
                    query,
                } => {
                    if expected[1] == 0 {
                        return Err("missing history subject identity".into());
                    }
                    crate::directory_backend::validate_history_query(&query)?;
                    let selected = name(raw)?;
                    let backend = Arc::clone(&self.backend);
                    let response = self
                        .server
                        .run_directory(backend.clone(), move || {
                            let response = backend.history_query(&selected, expected, &query)?;
                            crate::directory_backend::validate_history_response(&query, &response)?;
                            Ok(response)
                        })
                        .await?;
                    Ok(FsReply::Bytes(response))
                }
                FsRequest::HistoryReadSubject { name: raw } => {
                    let selected = name(raw)?;
                    let backend = Arc::clone(&self.backend);
                    let subject = self
                        .server
                        .run_directory(backend.clone(), move || {
                            // Busy is a per-operation refusal only after fresh
                            // provider scope brackets, not a poisoned Home.
                            let slot = HistoryReadSlot::acquire()?;
                            let subject = backend.history_read_subject(&selected)?;
                            subject.check()?;
                            subject.observe()?.validate()?;
                            Ok((subject, slot))
                        })
                        .await?;
                    let Some((backend, slot)) = subject else {
                        return Ok(FsReply::Handle(None));
                    };
                    let subject = Arc::new(HistoryReadSubject {
                        server: Arc::clone(&self.server),
                        backend,
                        _slot: slot,
                    });
                    Ok(FsReply::Handle(Some(capabilities.register(subject))))
                }
                FsRequest::SyncDirectory => {
                    let backend = Arc::clone(&self.backend);
                    let synced = self
                        .server
                        .run_directory(backend.clone(), move || backend.sync_directory())
                        .await?;
                    Ok(FsReply::Ack(super::ack(synced.is_some())))
                }
                FsRequest::PrepareReplace {
                    temporary,
                    temporary_expected,
                    victim,
                    victim_expected,
                } => {
                    if temporary_expected[1] == 0
                        || victim_expected[1] == 0
                        || temporary_expected == victim_expected
                        || temporary_expected[0] != victim_expected[0]
                    {
                        return Err("Invalid replacing inode observations".into());
                    }
                    let temporary = name(temporary)?;
                    let victim = name(victim)?;
                    let backend = Arc::clone(&self.backend);
                    let pending = self
                        .server
                        .run_directory(backend.clone(), move || {
                            backend.prepare_replace(
                                &temporary,
                                temporary_expected,
                                &victim,
                                victim_expected,
                            )
                        })
                        .await?;
                    Ok(FsReply::Handle(pending.map(|pending| {
                        capabilities.register(Arc::new(PendingReplacement {
                            server: Arc::clone(&self.server),
                            pending,
                        }))
                    })))
                }
                FsRequest::Readlink {
                    name: raw,
                    expected,
                } => {
                    if expected[1] == 0 {
                        return Err("native expected identity missing".into());
                    }
                    let selected = name(raw)?;
                    let backend = Arc::clone(&self.backend);
                    let target = self
                        .server
                        .run_directory(backend.clone(), move || {
                            backend.readlink(&selected, expected)
                        })
                        .await?;
                    let Some(target) = target else {
                        return Ok(FsReply::Bytes(None));
                    };
                    if target.is_empty() || target.len() > 4096 || target.contains(&0) {
                        return Err("invalid native symlink target".into());
                    }
                    Ok(FsReply::Bytes(Some(target)))
                }
                FsRequest::Statfs => {
                    let backend = Arc::clone(&self.backend);
                    let stats = self
                        .server
                        .run_directory(backend.clone(), move || backend.statfs())
                        .await?;
                    let Some(stats) = stats else {
                        return Ok(FsReply::FilesystemStats(None));
                    };
                    stats.validate()?;
                    Ok(FsReply::FilesystemStats(Some(stats)))
                }
                FsRequest::OpenCursor => {
                    let backend = Arc::clone(&self.backend);
                    let cursor = self
                        .server
                        .run(move || {
                            backend.check()?;
                            let opened = backend.open_cursor();
                            backend.check()?;
                            opened
                        })
                        .await;
                    let Ok(backend) = cursor else {
                        return Ok(FsReply::Handle(None));
                    };
                    let cursor = Arc::new(Cursor {
                        server: Arc::clone(&self.server),
                        backend,
                    });
                    Ok(FsReply::Handle(Some(capabilities.register(cursor))))
                }
                FsRequest::DirectoryMetadata => {
                    let backend = Arc::clone(&self.backend);
                    let metadata = self
                        .server
                        .run_directory(backend.clone(), move || backend.metadata())
                        .await?;
                    Ok(FsReply::Stat(metadata))
                }
                FsRequest::StatEntry { name: raw } => {
                    let selected = name(raw)?;
                    let backend = Arc::clone(&self.backend);
                    let metadata = self
                        .server
                        .run_directory(backend.clone(), move || backend.stat(&selected))
                        .await?;
                    Ok(FsReply::EntryStat(metadata))
                }
                FsRequest::PinEntry {
                    name: raw,
                    expected,
                } => {
                    if expected[1] == 0 {
                        return Err("native expected identity missing".into());
                    }
                    let selected = name(raw)?;
                    let backend = Arc::clone(&self.backend);
                    let pin = self
                        .server
                        .run_directory(backend.clone(), move || {
                            let pin = backend.pin_metadata(&selected, expected)?;
                            let stat = pin.metadata()?;
                            if stat.identity != expected
                                || stat.metadata.kind != crate::EntryKind::File
                            {
                                return Err("metadata pin identity/type differs".into());
                            }
                            Ok(pin)
                        })
                        .await?;
                    Ok(FsReply::Handle(pin.map(|backend| {
                        capabilities.register(Arc::new(MetadataPin {
                            server: Arc::clone(&self.server),
                            backend,
                        }))
                    })))
                }
                FsRequest::ListRows { cursor } => {
                    let backend = Arc::clone(&self.backend);
                    let page = self
                        .server
                        .run_directory(backend.clone(), move || backend.list(cursor))
                        .await?;
                    // Invalid or replayed continuations must not look like
                    // successful empty pages.
                    let page = page.ok_or("directory enumeration refused")?;
                    bound(page.entries.len(), 256, "directory page exceeds bound")?;
                    for entry in &page.entries {
                        bound(
                            entry.name.as_os_str().as_bytes().len(),
                            4096,
                            "directory backend exceeded page bound",
                        )?;
                    }
                    Ok(FsReply::Rows(Some(page)))
                }
                FsRequest::OpenDirectory { name: raw } => {
                    let selected = name(raw)?;
                    let backend = Arc::clone(&self.backend);
                    let child = self
                        .server
                        .run_directory(backend.clone(), move || backend.directory(&selected))
                        .await?;
                    Ok(FsReply::Handle(child.map(|child| {
                        capabilities.register(directory_root(&self.server, child))
                    })))
                }
                FsRequest::OpenFile {
                    name: raw,
                    mode,
                    expected,
                } => {
                    if expected[1] == 0 {
                        return Err("native expected identity missing".into());
                    }
                    let selected = name(raw)?;
                    let backend = Arc::clone(&self.backend);
                    let opened = self
                        .server
                        .run_directory(backend.clone(), move || {
                            let handle = backend.open_file(&selected, mode, expected)?;
                            let identity = handle
                                .native_identity()?
                                .ok_or("native held identity unavailable")?;
                            if identity != expected {
                                return Err("opened native identity differs".into());
                            }
                            Ok((handle, identity))
                        })
                        .await?;
                    let Some((handle, identity)) = opened else {
                        return Ok(FsReply::OpenedFile(None));
                    };
                    let file = File {
                        server: Arc::clone(&self.server),
                        source: FileSource::Capability(handle),
                        mode,
                    };
                    Ok(FsReply::OpenedFile(Some(OpenedFile {
                        handle: capabilities.register(Arc::new(file)),
                        device: identity[0],
                        inode: identity[1],
                    })))
                }
                FsRequest::SetCreatedPermissions {
                    name: raw,
                    expected,
                    mode,
                } => {
                    if expected[1] == 0 || mode & !0o7777 != 0 {
                        return Err("invalid own-created mode request".into());
                    }
                    let selected = name(raw)?;
                    let backend = Arc::clone(&self.backend);
                    let changed = self
                        .server
                        .run_directory(backend.clone(), move || {
                            backend.set_created_permissions(&selected, expected, mode)
                        })
                        .await?;
                    Ok(FsReply::Ack(super::ack(changed.is_some())))
                }
                FsRequest::RetireCreated {
                    name: raw,
                    expected,
                } => {
                    if expected[1] == 0 {
                        return Err("native expected identity missing".into());
                    }
                    let selected = name(raw)?;
                    let backend = Arc::clone(&self.backend);
                    let job = self
                        .server
                        .run_directory(backend.clone(), move || {
                            backend.retire_created(&selected, expected)
                        })
                        .await?;
                    Ok(FsReply::Handle(job.map(|job| {
                        capabilities.register(job_root(&self.server, job))
                    })))
                }
                FsRequest::CreateFile { name: raw } => {
                    let selected = name(raw)?;
                    let backend = Arc::clone(&self.backend);
                    let job = self
                        .server
                        .run_directory(backend.clone(), move || backend.create_file(selected))
                        .await?;
                    Ok(FsReply::Handle(job.map(|job| {
                        capabilities.register(job_root(&self.server, job))
                    })))
                }
                FsRequest::CreateDirectory { name: raw } => {
                    let selected = name(raw)?;
                    let backend = Arc::clone(&self.backend);
                    let job = self
                        .server
                        .run_directory(backend.clone(), move || backend.create_directory(selected))
                        .await?;
                    Ok(FsReply::Handle(job.map(|job| {
                        capabilities.register(job_root(&self.server, job))
                    })))
                }
                FsRequest::Recoveries { after } => {
                    if !after.is_empty() && after.len() != 24 {
                        return Err("Invalid recovery continuation".into());
                    }
                    let backend = Arc::clone(&self.backend);
                    let requested = after.clone();
                    let page = self
                        .server
                        .run_directory(backend.clone(), move || backend.recoveries(&requested))
                        .await?;
                    let Some(page) = page else {
                        return Ok(FsReply::Recoveries(None));
                    };
                    if page.entries.len() > 256
                        || (!page.next.is_empty() && page.next.len() != 24)
                        || page.done != page.next.is_empty()
                        || (!page.done && (page.entries.is_empty() || page.next == after))
                    {
                        return Err("Native recovery page exceeded bounds".into());
                    }
                    for entry in &page.entries {
                        if entry.recovery == [0; 16] || entry.operation == [0; 16] {
                            return Err("Invalid native recovery identifiers".into());
                        }
                        bound(
                            entry.original_name.as_os_str().len(),
                            4096,
                            "Recovery name too large",
                        )?;
                    }
                    Ok(FsReply::Recoveries(Some(page)))
                }
                FsRequest::RestoreCaptured {
                    recovery,
                    name: raw,
                } => {
                    if recovery == [0; 16] {
                        return Err("Invalid recovery ID".into());
                    }
                    let selected = name(raw)?;
                    let backend = Arc::clone(&self.backend);
                    let job = self
                        .server
                        .run_directory(backend.clone(), move || {
                            backend.restore_captured(recovery, selected)
                        })
                        .await?;
                    Ok(FsReply::Handle(job.map(|job| {
                        capabilities.register(job_root(&self.server, job))
                    })))
                }
                FsRequest::AtomicSave {
                    temporary,
                    expected,
                    target,
                } => {
                    if expected[1] == 0 {
                        return Err("invalid AtomicSave temporary identity".into());
                    }
                    let temporary = name(temporary)?;
                    let target = name(target)?;
                    let backend = Arc::clone(&self.backend);
                    let saved = self
                        .server
                        .run_directory(backend.clone(), move || {
                            backend.atomic_save(&temporary, expected, &target)
                        })
                        .await?;
                    Ok(FsReply::Ack(super::ack(saved.is_some())))
                }
                FsRequest::RenameReplace {
                    name: raw,
                    new_name,
                } => {
                    let old = name(raw)?;
                    let new = name(new_name)?;
                    let backend = Arc::clone(&self.backend);
                    let job = self
                        .server
                        .run_directory(backend.clone(), move || backend.rename_replace(&old, new))
                        .await?;
                    Ok(FsReply::Handle(job.map(|job| {
                        capabilities.register(job_root(&self.server, job))
                    })))
                }
                FsRequest::RenameNoReplace {
                    name: raw,
                    new_name,
                } => {
                    let old = name(raw)?;
                    let new = name(new_name)?;
                    let backend = Arc::clone(&self.backend);
                    let job = self
                        .server
                        .run_directory(backend.clone(), move || {
                            backend.rename_no_replace(&old, new)
                        })
                        .await?;
                    Ok(FsReply::Handle(job.map(|job| {
                        capabilities.register(job_root(&self.server, job))
                    })))
                }
                _ => Err("unexpected directory request".into()),
            }
        })
    }
}

impl ServeObject for Job {
    fn dispatch(
        self: Arc<Self>,
        capabilities: &Capabilities,
        request: FsRequest,
    ) -> Pin<Box<dyn Future<Output = Result<FsReply, String>> + Send + 'static>> {
        let capabilities = capabilities.clone();
        Box::pin(async move {
            let server = &self.server;
            match request {
                FsRequest::JobCreatedTemporary => {
                    let job = Arc::clone(&self.job);
                    let temporary = server
                        .run(move || checked(|| job.live_scope(), || job.created_temporary()))
                        .await?;
                    Ok(FsReply::Handle(temporary.map(|backend| {
                        capabilities.register(Arc::new(CreatedTemporary {
                            server: Arc::clone(server),
                            backend,
                        }))
                    })))
                }
                FsRequest::JobPinReplacement => {
                    let job = Arc::clone(&self.job);
                    let pin = server
                        .run(move || checked(|| job.live_scope(), || job.pin_replacement()))
                        .await?;
                    Ok(FsReply::Handle(pin.map(|pin| {
                        capabilities.register(Arc::new(ReplacementPin {
                            server: Arc::clone(server),
                            pin,
                        }))
                    })))
                }
                FsRequest::JobCheckScope => {
                    let job = Arc::clone(&self.job);
                    server
                        .run(move || job.live_scope().ok_or("scope checking unavailable")?)
                        .await?;
                    Ok(FsReply::Empty)
                }
                FsRequest::JobPoll => {
                    let job = Arc::clone(&self.job);
                    let poll = server
                        .run(move || checked(|| job.live_scope(), || job.poll()))
                        .await?;
                    let Some(poll) = poll else {
                        return Ok(FsReply::Poll(None));
                    };
                    bound(poll.updates.len(), 1025, "native job exceeded update bound")?;
                    for update in &poll.updates {
                        match update {
                            JobUpdate::Progress { phase, .. } => {
                                bound(phase.len(), 4096, "native job phase exceeds bound")?;
                            }
                            JobUpdate::Item {
                                error, recoveries, ..
                            } => {
                                bound(
                                    error.as_deref().unwrap_or_default().len(),
                                    4096,
                                    "native job exceeded item bound",
                                )?;
                                bound(recoveries.len(), 256, "native job exceeded item bound")?;
                            }
                            JobUpdate::Finished { .. } => {}
                        }
                    }
                    Ok(FsReply::Poll(Some(poll)))
                }
                FsRequest::JobCancel => {
                    let job = Arc::clone(&self.job);
                    let cancelled = server
                        .run(move || checked(|| job.live_scope(), || job.cancel()))
                        .await?;
                    Ok(FsReply::Ack(super::ack(cancelled.is_some())))
                }
                _ => Err("unexpected native job request".into()),
            }
        })
    }
}

pub(super) struct MetadataPin {
    server: Arc<Server>,
    backend: Arc<dyn WorkspaceMetadataBackend>,
}

impl ServeObject for MetadataPin {
    fn dispatch(
        self: Arc<Self>,
        _: &Capabilities,
        request: FsRequest,
    ) -> Pin<Box<dyn Future<Output = Result<FsReply, String>> + Send + 'static>> {
        Box::pin(async move {
            match request {
                FsRequest::PinCheckScope => {
                    let backend = Arc::clone(&self.backend);
                    self.server
                        .run(move || backend.live_scope().ok_or("scope checking unavailable")?)
                        .await?;
                    Ok(FsReply::Empty)
                }
                FsRequest::PinStat => {
                    let backend = Arc::clone(&self.backend);
                    let stat = self
                        .server
                        .run(move || checked(|| backend.live_scope(), || backend.metadata()))
                        .await?;
                    Ok(FsReply::Stat(stat))
                }
                _ => Err("unexpected metadata pin request".into()),
            }
        })
    }
}

pub(super) struct Cursor {
    server: Arc<Server>,
    backend: Arc<dyn crate::directory_backend::WorkspaceDirectoryCursorBackend>,
}

impl ServeObject for Cursor {
    fn dispatch(
        self: Arc<Self>,
        _: &Capabilities,
        request: FsRequest,
    ) -> Pin<Box<dyn Future<Output = Result<FsReply, String>> + Send + 'static>> {
        Box::pin(async move {
            let FsRequest::CursorPage { expected_sequence } = request else {
                return Err("unexpected cursor request".into());
            };
            let backend = Arc::clone(&self.backend);
            let page = self
                .server
                .run(move || {
                    backend.check()?;
                    let page = backend.page(expected_sequence);
                    backend.check()?;
                    page
                })
                .await;
            let Ok(page) = page else {
                return Ok(FsReply::StreamPage(None));
            };
            bound(page.entries.len(), 128, "cursor page exceeds bound")?;
            for row in &page.entries {
                bound(
                    row.name.as_os_str().len(),
                    4096,
                    "cursor page exceeds bound",
                )?;
            }
            Ok(FsReply::StreamPage(Some(page)))
        })
    }
}

pub(super) struct ReplacementPin {
    server: Arc<Server>,
    pin: Arc<dyn crate::directory_backend::WorkspaceReplacementPinBackend>,
}

impl ServeObject for ReplacementPin {
    fn dispatch(
        self: Arc<Self>,
        _: &Capabilities,
        request: FsRequest,
    ) -> Pin<Box<dyn Future<Output = Result<FsReply, String>> + Send + 'static>> {
        Box::pin(async move {
            match request {
                FsRequest::ReplacementCommit { receipt } => {
                    if receipt.is_empty() || receipt.len() > crate::external_history::LIMIT {
                        return Err("Invalid certified commit receipt length".into());
                    }
                    let pin = Arc::clone(&self.pin);
                    self.server
                        .run(move || pin.commit_with_history(&receipt))
                        .await?;
                    Ok(FsReply::Empty)
                }
                FsRequest::ReplacementCheck => {
                    let pin = Arc::clone(&self.pin);
                    self.server.run(move || pin.check()).await?;
                    Ok(FsReply::Empty)
                }
                FsRequest::ReplacementObserve => {
                    let pin = Arc::clone(&self.pin);
                    let value = self
                        .server
                        .run(move || {
                            pin.check()?;
                            let value = pin.observe()?;
                            pin.check()?;
                            Ok(value)
                        })
                        .await?;
                    if value.operation == [0; 16]
                        || value.capture == [0; 16]
                        || value.source_owner == [0; 32]
                        || value.source[1] == 0
                        || value.victim[1] == 0
                        || value.destination != value.source
                        || value.source == value.victim
                        || value.source[0] != value.victim[0]
                    {
                        return Err("invalid native replacement observation".into());
                    }
                    Ok(FsReply::ReplacementObservation(value))
                }
                _ => Err("unexpected replacement pin request".into()),
            }
        })
    }
}

pub(super) struct PendingReplacement {
    pub(super) server: Arc<Server>,
    pub(super) pending: Arc<dyn crate::directory_backend::WorkspacePendingReplacementBackend>,
}

impl ServeObject for PendingReplacement {
    fn dispatch(
        self: Arc<Self>,
        capabilities: &Capabilities,
        request: FsRequest,
    ) -> Pin<Box<dyn Future<Output = Result<FsReply, String>> + Send + 'static>> {
        let capabilities = capabilities.clone();
        Box::pin(async move {
            let server = &self.server;
            let pending = &self.pending;
            match request {
                FsRequest::PendingRetireHandoff { ticket } => {
                    let registry = server.handoffs.clone();
                    let pending = Arc::clone(pending);
                    server
                        .run(move || match registry {
                            Some(registry) => registry.retire(&pending, ticket),
                            None => pending.retire_daemon_handoff(ticket),
                        })
                        .await?;
                    Ok(FsReply::Empty)
                }
                FsRequest::PendingCommittedPin => {
                    let pending = Arc::clone(pending);
                    let (pin, job) = server
                        .run(move || {
                            pending.check()?;
                            let pin = pending.acknowledged_committed_pin()?;
                            let job = pending.acknowledged_committed_job()?;
                            pin.check()?;
                            pin.observe()?;
                            pending.check()?;
                            Ok((pin, job))
                        })
                        .await?;
                    let pin = capabilities.register(Arc::new(ReplacementPin {
                        server: Arc::clone(server),
                        pin,
                    }));
                    let job = capabilities.register(job_root(server, job));
                    Ok(FsReply::CommittedPin(Some(CommittedPin { pin, job })))
                }
                FsRequest::PendingArm { challenge } => {
                    let registry = server.handoffs.clone();
                    let pending = Arc::clone(pending);
                    let ticket = server
                        .run(move || {
                            pending.check()?;
                            let ticket = match registry {
                                Some(registry) => registry.arm(Arc::clone(&pending), challenge)?,
                                None => pending.arm_daemon_handoff(challenge)?,
                            };
                            pending.check()?;
                            Ok(ticket)
                        })
                        .await?;
                    Ok(FsReply::Ticket(ticket.to_vec()))
                }
                FsRequest::PendingObserve => {
                    let pending = Arc::clone(pending);
                    let value = server
                        .run(move || {
                            pending.check()?;
                            let value = pending.observe()?;
                            pending.check()?;
                            value.validate()?;
                            Ok(value)
                        })
                        .await?;
                    Ok(FsReply::PendingObservation(value))
                }
                FsRequest::PendingCheckScope => {
                    let pending = Arc::clone(pending);
                    server.run(move || pending.check()).await?;
                    Ok(FsReply::Empty)
                }
                FsRequest::PendingCancel => {
                    let pending = Arc::clone(pending);
                    server.run(move || pending.cancel()).await?;
                    Ok(FsReply::Empty)
                }
                FsRequest::PendingStart { receipt } => {
                    if receipt.is_empty() || receipt.len() > crate::external_history::LIMIT {
                        return Err("Invalid certified prepare receipt length".into());
                    }
                    let pending = Arc::clone(pending);
                    let job = server
                        .run(move || {
                            pending.check()?;
                            let job = pending.start_with_history(&receipt)?;
                            if let Some(scope) = job.live_scope() {
                                scope?;
                            }
                            Ok(job)
                        })
                        .await?;
                    // The pending capability is consumed: no post-check on its
                    // closed reservation and no automatic mutation replay.
                    Ok(FsReply::Handle(Some(
                        capabilities.register(job_root(server, job)),
                    )))
                }
                _ => Err("unexpected pending replacement request".into()),
            }
        })
    }
}

pub(super) struct CreatedTemporary {
    server: Arc<Server>,
    backend: Arc<dyn crate::directory_backend::WorkspaceCreatedTemporaryBackend>,
}

impl ServeObject for CreatedTemporary {
    fn dispatch(
        self: Arc<Self>,
        capabilities: &Capabilities,
        request: FsRequest,
    ) -> Pin<Box<dyn Future<Output = Result<FsReply, String>> + Send + 'static>> {
        let capabilities = capabilities.clone();
        Box::pin(async move {
            let server = &self.server;
            match request {
                FsRequest::TemporarySetPermissions { mode } => {
                    if mode & !0o7777 != 0 {
                        return Err("invalid own-created mode".into());
                    }
                    let backend = Arc::clone(&self.backend);
                    server.run(move || backend.set_permissions(mode)).await?;
                    Ok(FsReply::Empty)
                }
                FsRequest::TemporaryPrepareAtomicSave => {
                    use crate::directory_backend::AtomicSavePreparation as Outcome;
                    let backend = Arc::clone(&self.backend);
                    let outcome = server
                        .run(move || {
                            backend.check()?;
                            let outcome = backend.prepare_atomic_save();
                            if matches!(outcome, Outcome::Refused(_)) {
                                backend.check()?;
                            }
                            Ok(outcome)
                        })
                        .await?;
                    match outcome {
                        Outcome::Prepared(pending) => Ok(FsReply::Handle(Some(
                            capabilities.register(Arc::new(PendingReplacement {
                                server: Arc::clone(server),
                                pending,
                            })),
                        ))),
                        Outcome::Refused(_) => Ok(FsReply::Handle(None)),
                        Outcome::Unknown(error) => Err(error.chars().take(512).collect()),
                    }
                }
                FsRequest::TemporaryPrepareReplace => {
                    let backend = Arc::clone(&self.backend);
                    let pending = server
                        .run(move || checked(|| backend.live_scope(), || backend.prepare_replace()))
                        .await?;
                    Ok(FsReply::Handle(pending.map(|pending| {
                        capabilities.register(Arc::new(PendingReplacement {
                            server: Arc::clone(server),
                            pending,
                        }))
                    })))
                }
                FsRequest::TemporaryCheckScope => {
                    let backend = Arc::clone(&self.backend);
                    server.run(move || backend.check()).await?;
                    Ok(FsReply::Empty)
                }
                FsRequest::TemporaryRetire => {
                    let backend = Arc::clone(&self.backend);
                    let job = server
                        .run(move || checked(|| backend.live_scope(), || backend.retire()))
                        .await?;
                    Ok(FsReply::Handle(
                        job.map(|job| capabilities.register(job_root(server, job))),
                    ))
                }
                _ => Err("unexpected created temporary request".into()),
            }
        })
    }
}

// Finite held source capabilities across all owned connections in this worker.
static HISTORY_READ_SLOTS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
struct HistoryReadSlot;
impl HistoryReadSlot {
    fn acquire() -> Result<Self, String> {
        HISTORY_READ_SLOTS
            .fetch_update(
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
                |held| {
                    if held < 64 { held.checked_add(1) } else { None }
                },
            )
            .map_err(|_| "history Read capability pool busy".to_string())?;
        Ok(Self)
    }
}
impl Drop for HistoryReadSlot {
    fn drop(&mut self) {
        HISTORY_READ_SLOTS.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}
pub(super) struct HistoryReadSubject {
    server: Arc<Server>,
    backend: Arc<dyn crate::directory_backend::WorkspaceHistoryReadSubjectBackend>,
    _slot: HistoryReadSlot,
}

pub(super) fn history_read_root(
    server: &Arc<Server>,
    backend: Arc<dyn crate::directory_backend::WorkspaceHistoryReadSubjectBackend>,
) -> Result<Arc<dyn ServeObject>, String> {
    Ok(Arc::new(HistoryReadSubject {
        server: Arc::clone(server),
        backend,
        _slot: HistoryReadSlot::acquire()?,
    }))
}

impl ServeObject for HistoryReadSubject {
    fn dispatch(
        self: Arc<Self>,
        _: &Capabilities,
        request: FsRequest,
    ) -> Pin<Box<dyn Future<Output = Result<FsReply, String>> + Send + 'static>> {
        Box::pin(async move {
            let server = &self.server;
            let backend = &self.backend;
            match request {
                FsRequest::SubjectRetireHandoff { ticket } => {
                    let registry = server.handoffs.clone();
                    let subject = Arc::clone(backend);
                    server
                        .run(move || match registry {
                            Some(registry) => registry.retire_read(&subject, ticket),
                            None => subject.retire_daemon_handoff(ticket),
                        })
                        .await?;
                    Ok(FsReply::Empty)
                }
                FsRequest::SubjectArm { challenge } => {
                    let registry = server.handoffs.clone();
                    let subject = Arc::clone(backend);
                    let ticket = server
                        .run(move || {
                            subject.check()?;
                            let ticket = match registry {
                                Some(registry) => registry.arm_read(subject.clone(), challenge),
                                None => subject.arm_daemon_handoff(challenge),
                            };
                            subject.check()?;
                            Ok(ticket)
                        })
                        .await?;
                    Ok(FsReply::Ticket(ticket.unwrap_or_default().to_vec()))
                }
                FsRequest::SubjectCheckScope => {
                    let subject = Arc::clone(backend);
                    server.run(move || subject.check_scope()).await?;
                    Ok(FsReply::Empty)
                }
                FsRequest::SubjectCheck => {
                    let subject = Arc::clone(backend);
                    server.run(move || subject.check()).await?;
                    Ok(FsReply::Empty)
                }
                FsRequest::SubjectObserve => {
                    let subject = Arc::clone(backend);
                    let observation = server
                        .run(move || {
                            subject.check()?;
                            let actual = subject.observe()?;
                            actual.validate()?;
                            subject.check()?;
                            Ok(actual)
                        })
                        .await?;
                    Ok(FsReply::HistoryObservation(observation))
                }
                _ => Err("unexpected history Read request".into()),
            }
        })
    }
}
