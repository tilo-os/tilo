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
use tilo_fs_client::wire::{FsReply, FsRequest, OpenedFile};

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
                FsRequest::SyncDirectory => {
                    let backend = Arc::clone(&self.backend);
                    let synced = self
                        .server
                        .run_directory(backend.clone(), move || backend.sync_directory())
                        .await?;
                    Ok(FsReply::Ack(super::ack(synced.is_some())))
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
        _capabilities: &Capabilities,
        request: FsRequest,
    ) -> Pin<Box<dyn Future<Output = Result<FsReply, String>> + Send + 'static>> {
        Box::pin(async move {
            let server = &self.server;
            match request {
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
