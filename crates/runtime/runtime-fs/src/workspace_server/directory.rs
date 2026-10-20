//! Raw-name directory capabilities on the existing connected service.
use super::{Arc, File, FileSource, OpenMode, Promise, Rc, Server, fs_capnp, write_metadata};
use crate::EntryName;
use crate::directory_backend::{
    JobUpdate, WorkspaceDirectoryBackend, WorkspaceJobBackend, WorkspaceMetadataBackend,
};
use crate::fs_capnp::{
    NativeItemState, directory_cursor, directory_handle, metadata_pin, native_job,
};
use crate::operations::ItemState;
use std::os::unix::ffi::{OsStrExt, OsStringExt};

struct Directory {
    server: Rc<Server>,
    backend: Arc<dyn WorkspaceDirectoryBackend>,
}
fn wire_u32(value: usize) -> capnp::Result<u32> {
    u32::try_from(value)
        .map_err(|_| capnp::Error::failed("native response exceeds list bound".into()))
}
fn name(raw: capnp::Result<&[u8]>) -> Result<EntryName, capnp::Error> {
    let raw = raw?;
    if raw.len() > 4096 {
        return Err(capnp::Error::failed("component exceeds bound".into()));
    }
    EntryName::new(std::ffi::OsString::from_vec(raw.to_vec()))
        .map_err(|e| capnp::Error::failed(e.to_string()))
}
pub(super) fn client(
    server: Rc<Server>,
    backend: Arc<dyn WorkspaceDirectoryBackend>,
) -> directory_handle::Client {
    capnp_rpc::new_client(Directory { server, backend })
}
async fn run<T: Send + 'static>(
    server: &Server,
    permit: tokio::sync::OwnedSemaphorePermit,
    call: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, capnp::Error> {
    let drain = server
        .drain
        .as_ref()
        .map(super::pending_handoff::CallDrain::enter)
        .transpose()
        .map_err(capnp::Error::failed)?;
    server
        .runtime
        .spawn_blocking(move || {
            let _drain = drain;
            let _permit = permit;
            call()
        })
        .await
        .map_err(|e| capnp::Error::failed(format!("native directory backend failed: {e}")))?
        .map_err(capnp::Error::failed)
}
async fn run_directory<T: Send + 'static>(
    server: &Server,
    permit: tokio::sync::OwnedSemaphorePermit,
    backend: Arc<dyn WorkspaceDirectoryBackend>,
    call: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<Option<T>, capnp::Error> {
    run(server, permit, move || {
        backend.check()?;
        let value = call();
        backend.check()?;
        Ok(value.ok())
    })
    .await
}
fn checked_result<T>(
    scope: impl Fn() -> Option<Result<(), String>>,
    call: impl FnOnce() -> Result<T, String>,
) -> Result<Option<T>, String> {
    if let Some(live) = scope() {
        live?;
        let value = call();
        scope().ok_or("live scope disappeared")??;
        Ok(value.ok())
    } else {
        call().map(Some)
    }
}
fn job_client(server: Rc<Server>, job: Arc<dyn WorkspaceJobBackend>) -> native_job::Client {
    capnp_rpc::new_client(Job { server, job })
}
#[allow(
    refining_impl_trait,
    reason = "capnp server methods return the concrete Promise behind the generated impl Future"
)]
impl directory_handle::Server for Directory {
    fn history_query(
        self: Rc<Self>,
        params: directory_handle::HistoryQueryParams,
        mut results: directory_handle::HistoryQueryResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let response = {
                let permit = self.server.admit_call()?;
                let request = params.get()?;
                let selected = name(request.get_name())?;
                let expected = [request.get_expected_device(), request.get_expected_inode()];
                let query = request.get_query()?;
                crate::directory_backend::validate_history_query(query)
                    .map_err(capnp::Error::failed)?;
                if expected[1] == 0 {
                    return Err(capnp::Error::failed(
                        "missing history subject identity".into(),
                    ));
                }
                let query = query.to_vec();
                let backend = self.backend.clone();
                run_directory(&self.server, permit, backend.clone(), move || {
                    let response = backend.history_query(&selected, expected, &query)?;
                    crate::directory_backend::validate_history_response(&query, &response)?;
                    Ok(response)
                })
                .await?
            };
            if let Some(response) = response {
                results.get().set_response(&response);
            } else {
                results
                    .get()
                    .set_status(fs_capnp::NativeEntryStatus::Refused);
            }
            Ok(())
        })
    }

    fn history_read_subject(
        self: Rc<Self>,
        params: directory_handle::HistoryReadSubjectParams,
        mut results: directory_handle::HistoryReadSubjectResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let subject = {
                let permit = self.server.admit_call()?;
                let selected = name(params.get()?.get_name())?;
                let backend = self.backend.clone();
                run_directory(&self.server, permit, backend.clone(), move || {
                    // Busy is a per-operation refusal only after fresh provider
                    // scope brackets, rather than poisoning the entire Home.
                    let slot = HistoryReadSlot::acquire().map_err(|error| error.to_string())?;
                    let subject = backend.history_read_subject(&selected)?;
                    subject.check()?;
                    subject.observe()?.validate()?;
                    Ok((subject, slot))
                })
                .await?
            };
            let Some((subject, slot)) = subject else {
                results
                    .get()
                    .set_status(fs_capnp::NativeEntryStatus::Refused);
                return Ok(());
            };
            results
                .get()
                .set_handle(capnp_rpc::new_client(HistoryReadSubject {
                    server: self.server.clone(),
                    backend: subject,
                    _slot: slot,
                }));
            Ok(())
        })
    }

    fn sync_directory(
        self: Rc<Self>,
        _: directory_handle::SyncDirectoryParams,
        mut results: directory_handle::SyncDirectoryResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let synced = {
                let permit = self.server.admit_call()?;
                let backend = self.backend.clone();
                run_directory(&self.server, permit, backend.clone(), move || {
                    backend.sync_directory()
                })
                .await?
            };
            results.get().set_status(if synced.is_some() {
                fs_capnp::NativeEntryStatus::Ok
            } else {
                fs_capnp::NativeEntryStatus::Refused
            });
            Ok(())
        })
    }

    fn prepare_replace(
        self: Rc<Self>,
        params: directory_handle::PrepareReplaceParams,
        mut results: directory_handle::PrepareReplaceResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let pending = {
                let permit = self.server.admit_call()?;
                let request = params.get()?;
                let temporary = name(request.get_temporary())?;
                let victim = name(request.get_victim())?;
                let expected = [
                    request.get_temporary_device(),
                    request.get_temporary_inode(),
                ];
                let old = [request.get_victim_device(), request.get_victim_inode()];
                if expected[1] == 0 || old[1] == 0 || expected == old || expected[0] != old[0] {
                    return Err(capnp::Error::failed(
                        "Invalid replacing inode observations".into(),
                    ));
                }
                let backend = self.backend.clone();
                run_directory(&self.server, permit, backend.clone(), move || {
                    backend.prepare_replace(&temporary, expected, &victim, old)
                })
                .await?
            };
            let Some(pending) = pending else {
                results
                    .get()
                    .set_status(fs_capnp::NativeEntryStatus::Refused);
                return Ok(());
            };
            results
                .get()
                .set_handle(capnp_rpc::new_client(PendingReplacement {
                    server: self.server.clone(),
                    pending,
                }));
            Ok(())
        })
    }
    fn readlink(
        self: Rc<Self>,
        params: directory_handle::ReadlinkParams,
        mut results: directory_handle::ReadlinkResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let target = {
                let permit = self.server.admit_call()?;
                let input = params.get()?;
                let name = name(input.get_name())?;
                let expected = [input.get_expected_device(), input.get_expected_inode()];
                if expected[1] == 0 {
                    return Err(capnp::Error::failed(
                        "native expected identity missing".into(),
                    ));
                }
                let backend = self.backend.clone();
                run_directory(&self.server, permit, backend.clone(), move || {
                    backend.readlink(&name, expected)
                })
                .await?
            };
            let Some(target) = target else {
                results
                    .get()
                    .set_status(fs_capnp::NativeEntryStatus::Refused);
                return Ok(());
            };
            if target.is_empty() || target.len() > 4096 || target.contains(&0) {
                return Err(capnp::Error::failed("invalid native symlink target".into()));
            }
            results.get().set_target(&target);
            Ok(())
        })
    }
    fn statfs(
        self: Rc<Self>,
        _: directory_handle::StatfsParams,
        mut results: directory_handle::StatfsResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let stats = {
                let permit = self.server.admit_call()?;
                let backend = self.backend.clone();
                run_directory(&self.server, permit, backend.clone(), move || {
                    backend.statfs()
                })
                .await?
            };
            let Some(stats) = stats else {
                results
                    .get()
                    .set_status(fs_capnp::NativeEntryStatus::Refused);
                return Ok(());
            };
            stats.validate().map_err(capnp::Error::failed)?;
            let mut out = results.get().init_stats();
            out.set_blocks(stats.blocks);
            out.set_blocks_free(stats.blocks_free);
            out.set_blocks_available(stats.blocks_available);
            out.set_files(stats.files);
            out.set_files_free(stats.files_free);
            out.set_files_available(stats.files_available);
            out.set_block_size(u64::from(stats.block_size));
            out.set_fragment_size(u64::from(stats.fragment_size));
            out.set_name_max(u64::from(stats.name_max));
            out.set_read_only(stats.read_only);
            Ok(())
        })
    }
    fn open_cursor(
        self: Rc<Self>,
        _: directory_handle::OpenCursorParams,
        mut results: directory_handle::OpenCursorResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let cursor = {
                let permit = self.server.admit_call()?;
                let backend = self.backend.clone();
                run(&self.server, permit, move || {
                    backend.check()?;
                    let opened = backend.open_cursor();
                    backend.check()?;
                    Ok(opened)
                })
                .await?
            };
            match cursor {
                Ok(backend) => results.get().set_handle(capnp_rpc::new_client(Cursor {
                    server: self.server.clone(),
                    backend,
                })),
                Err(_) => results
                    .get()
                    .set_status(fs_capnp::NativeEntryStatus::Refused),
            }
            Ok(())
        })
    }
    fn metadata(
        self: Rc<Self>,
        _: directory_handle::MetadataParams,
        mut results: directory_handle::MetadataResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let metadata = {
                let permit = self.server.admit_call()?;
                let backend = Arc::clone(&self.backend);
                run_directory(&self.server, permit, backend.clone(), move || {
                    backend.metadata()
                })
                .await?
            };
            let Some(metadata) = metadata else {
                results
                    .get()
                    .set_status(fs_capnp::NativeEntryStatus::Refused);
                return Ok(());
            };
            let mut out = results.get();
            out.set_device(metadata.identity[0]);
            out.set_inode(metadata.identity[1]);
            out.set_file_type(metadata.file_type);
            out.set_execute_bits(metadata.execute_bits);
            write_metadata(out.init_metadata(), &metadata.metadata);
            Ok(())
        })
    }
    fn stat(
        self: Rc<Self>,
        params: directory_handle::StatParams,
        mut results: directory_handle::StatResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let metadata = {
                let permit = self.server.admit_call()?;
                let name = name(params.get()?.get_name())?;
                let backend = Arc::clone(&self.backend);
                run_directory(&self.server, permit, backend.clone(), move || {
                    backend.stat(&name)
                })
                .await?
            };
            let Some(metadata) = metadata else {
                results
                    .get()
                    .set_status(fs_capnp::NativeEntryStatus::Refused);
                return Ok(());
            };
            let mut out = results.get();
            out.set_present(metadata.is_some());
            if let Some(metadata) = metadata {
                out.set_device(metadata.identity[0]);
                out.set_inode(metadata.identity[1]);
                out.set_file_type(metadata.file_type);
                out.set_execute_bits(metadata.execute_bits);
                write_metadata(out.init_metadata(), &metadata.metadata);
            }
            Ok(())
        })
    }
    fn pin_metadata(
        self: Rc<Self>,
        params: directory_handle::PinMetadataParams,
        mut results: directory_handle::PinMetadataResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let pin = {
                let permit = self.server.admit_call()?;
                let request = params.get()?;
                let name = name(request.get_name())?;
                let expected = [request.get_expected_device(), request.get_expected_inode()];
                if expected[1] == 0 {
                    return Err(capnp::Error::failed(
                        "native expected identity missing".into(),
                    ));
                }
                let backend = self.backend.clone();
                run_directory(&self.server, permit, backend.clone(), move || {
                    let pin = backend.pin_metadata(&name, expected)?;
                    let stat = pin.metadata()?;
                    if stat.identity != expected || stat.metadata.kind != crate::EntryKind::File {
                        return Err("metadata pin identity/type differs".into());
                    }
                    Ok(pin)
                })
                .await?
            };
            let Some(pin) = pin else {
                results
                    .get()
                    .set_status(fs_capnp::NativeEntryStatus::Refused);
                return Ok(());
            };
            results.get().set_handle(capnp_rpc::new_client(MetadataPin {
                server: self.server.clone(),
                backend: pin,
            }));
            Ok(())
        })
    }
    fn list(
        self: Rc<Self>,
        params: directory_handle::ListParams,
        mut results: directory_handle::ListResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let page = {
                let permit = self.server.admit_call()?;
                let token = params.get()?.get_cursor();
                let backend = Arc::clone(&self.backend);
                run_directory(&self.server, permit, backend.clone(), move || {
                    backend.list(token)
                })
                .await?
            };
            // Invalid or replayed continuations must not look like successful
            // empty pages. Scope checks still bracket the backend operation.
            let page =
                page.ok_or_else(|| capnp::Error::failed("directory enumeration refused".into()))?;
            if page.entries.len() > 256
                || page
                    .entries
                    .iter()
                    .any(|entry| entry.name.as_os_str().as_bytes().len() > 4096)
            {
                return Err(capnp::Error::failed(
                    "directory backend exceeded page bound".into(),
                ));
            }
            let mut out = results.get();
            out.set_next(page.next);
            let mut entries = out.init_entries(
                u32::try_from(page.entries.len())
                    .map_err(|_| capnp::Error::failed("directory page exceeds bound".into()))?,
            );
            for (index, entry) in page.entries.iter().enumerate() {
                let mut row = entries.reborrow().get(wire_u32(index)?);
                row.set_name(entry.name.as_os_str().as_bytes());
                write_metadata(row.init_metadata(), &entry.metadata);
            }
            Ok(())
        })
    }
    fn open_directory(
        self: Rc<Self>,
        params: directory_handle::OpenDirectoryParams,
        mut results: directory_handle::OpenDirectoryResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let child = {
                let permit = self.server.admit_call()?;
                let name = name(params.get()?.get_name())?;
                let backend = Arc::clone(&self.backend);
                run_directory(&self.server, permit, backend.clone(), move || {
                    backend.directory(&name)
                })
                .await?
            };
            let Some(child) = child else {
                results
                    .get()
                    .set_status(fs_capnp::NativeEntryStatus::Refused);
                return Ok(());
            };
            results
                .get()
                .set_handle(client(Rc::clone(&self.server), child));
            Ok(())
        })
    }
    fn open_file(
        self: Rc<Self>,
        params: directory_handle::OpenFileParams,
        mut results: directory_handle::OpenFileResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let (opened, mode) = {
                let permit = self.server.admit_call()?;
                let request = params.get()?;
                let name = name(request.get_name())?;
                let expected = [request.get_expected_device(), request.get_expected_inode()];
                if expected[1] == 0 {
                    return Err(capnp::Error::failed(
                        "native expected identity missing".into(),
                    ));
                }
                let mode = match request.get_mode()? {
                    fs_capnp::OpenMode::Read => OpenMode::Read,
                    fs_capnp::OpenMode::Write => OpenMode::Write,
                    fs_capnp::OpenMode::Append => OpenMode::Append,
                    fs_capnp::OpenMode::ReadWriteExisting => OpenMode::ReadWriteExisting,
                    fs_capnp::OpenMode::WriteExisting => OpenMode::WriteExisting,
                };
                let backend = Arc::clone(&self.backend);
                run_directory(&self.server, permit, backend.clone(), move || {
                    let handle = backend.open_file(&name, mode, expected)?;
                    let identity = handle
                        .native_identity()?
                        .ok_or("native held identity unavailable")?;
                    if identity != expected {
                        return Err("opened native identity differs".into());
                    }
                    Ok((handle, identity))
                })
                .await
                .map(|opened| (opened, mode))?
            };
            let Some((handle, identity)) = opened else {
                results
                    .get()
                    .set_status(fs_capnp::NativeEntryStatus::Refused);
                return Ok(());
            };
            results.get().set_device(identity[0]);
            results.get().set_inode(identity[1]);
            results.get().set_handle(capnp_rpc::new_client(File {
                server: Rc::clone(&self.server),
                source: FileSource::Capability(handle),
                mode,
            }));
            Ok(())
        })
    }
    fn set_created_permissions(
        self: Rc<Self>,
        params: directory_handle::SetCreatedPermissionsParams,
        mut results: directory_handle::SetCreatedPermissionsResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let changed = {
                let permit = self.server.admit_call()?;
                let request = params.get()?;
                let name = name(request.get_name())?;
                let expected = [request.get_expected_device(), request.get_expected_inode()];
                let mode = request.get_mode();
                if expected[1] == 0 || mode & !0o7777 != 0 {
                    return Err(capnp::Error::failed(
                        "invalid own-created mode request".into(),
                    ));
                }
                let backend = self.backend.clone();
                run_directory(&self.server, permit, backend.clone(), move || {
                    backend.set_created_permissions(&name, expected, mode)
                })
                .await?
            };
            if changed.is_none() {
                results
                    .get()
                    .set_status(fs_capnp::NativeEntryStatus::Refused);
            }
            Ok(())
        })
    }
    fn retire_created(
        self: Rc<Self>,
        params: directory_handle::RetireCreatedParams,
        mut results: directory_handle::RetireCreatedResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let job = {
                let permit = self.server.admit_call()?;
                let request = params.get()?;
                let name = name(request.get_name())?;
                let expected = [request.get_expected_device(), request.get_expected_inode()];
                if expected[1] == 0 {
                    return Err(capnp::Error::failed(
                        "native expected identity missing".into(),
                    ));
                }
                let backend = self.backend.clone();
                run_directory(&self.server, permit, backend.clone(), move || {
                    backend.retire_created(&name, expected)
                })
                .await?
            };
            if let Some(job) = job {
                results.get().set_job(job_client(self.server.clone(), job));
            } else {
                results
                    .get()
                    .set_status(fs_capnp::NativeEntryStatus::Refused);
            }
            Ok(())
        })
    }
    fn create_file(
        self: Rc<Self>,
        params: directory_handle::CreateFileParams,
        mut results: directory_handle::CreateFileResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let job = {
                let permit = self.server.admit_call()?;
                let name = name(params.get()?.get_name())?;
                let backend = Arc::clone(&self.backend);
                run_directory(&self.server, permit, backend.clone(), move || {
                    backend.create_file(name)
                })
                .await?
            };
            let Some(job) = job else {
                results
                    .get()
                    .set_status(fs_capnp::NativeEntryStatus::Refused);
                return Ok(());
            };
            results
                .get()
                .set_job(job_client(Rc::clone(&self.server), job));
            Ok(())
        })
    }

    fn recoveries(
        self: Rc<Self>,
        params: directory_handle::RecoveriesParams,
        mut results: directory_handle::RecoveriesResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let (page, after) = {
                let permit = self.server.admit_call()?;
                let request = params.get()?;
                let raw = request.get_after()?;
                if !raw.is_empty() && raw.len() != 24 {
                    return Err(capnp::Error::failed("Invalid recovery continuation".into()));
                }
                let after = raw.to_vec();
                let requested_after = after.clone();
                let backend = self.backend.clone();
                run_directory(&self.server, permit, backend.clone(), move || {
                    backend.recoveries(&requested_after)
                })
                .await
                .map(|page| (page, after))?
            };
            let Some(page) = page else {
                results
                    .get()
                    .set_status(fs_capnp::NativeEntryStatus::Refused);
                return Ok(());
            };
            if page.entries.len() > 256
                || (!page.next.is_empty() && page.next.len() != 24)
                || (page.done != page.next.is_empty())
                || (!page.done && (page.entries.is_empty() || page.next == after))
            {
                return Err(capnp::Error::failed(
                    "Native recovery page exceeded bounds".into(),
                ));
            }
            let mut out = results.get();
            out.set_next(&page.next);
            out.set_done(page.done);
            let mut rows = out.init_entries(
                u32::try_from(page.entries.len())
                    .map_err(|_| capnp::Error::failed("Recovery page too large".into()))?,
            );
            for (index, entry) in page.entries.iter().enumerate() {
                let mut row = rows.reborrow().get(wire_u32(index)?);
                if entry.recovery == [0; 16] || entry.operation == [0; 16] {
                    return Err(capnp::Error::failed(
                        "Invalid native recovery identifiers".into(),
                    ));
                }
                row.set_recovery(&entry.recovery);
                row.set_operation(&entry.operation);
                let name = entry.original_name.as_os_str().as_bytes();
                if name.len() > 4096 {
                    return Err(capnp::Error::failed("Recovery name too large".into()));
                }
                row.set_original_name(name);
                row.set_kind(match entry.kind {
                    crate::recovery::RecoveryKind::NeedsRevalidation => {
                        fs_capnp::NativeRecoveryKind::NeedsRevalidation
                    }
                    crate::recovery::RecoveryKind::KnownStaged => {
                        fs_capnp::NativeRecoveryKind::KnownStaged
                    }
                    crate::recovery::RecoveryKind::UnknownNativeCommit => {
                        fs_capnp::NativeRecoveryKind::UnknownNativeCommit
                    }
                    crate::recovery::RecoveryKind::PublishedSourceRetained => {
                        fs_capnp::NativeRecoveryKind::PublishedSourceRetained
                    }
                    crate::recovery::RecoveryKind::QuarantinedNative => {
                        fs_capnp::NativeRecoveryKind::QuarantinedNative
                    }
                });
            }
            Ok(())
        })
    }
    fn restore_captured(
        self: Rc<Self>,
        params: directory_handle::RestoreCapturedParams,
        mut results: directory_handle::RestoreCapturedResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let job = {
                let permit = self.server.admit_call()?;
                let request = params.get()?;
                let recovery: [u8; 16] = request
                    .get_recovery()?
                    .try_into()
                    .map_err(|_| capnp::Error::failed("Invalid recovery ID".into()))?;
                if recovery == [0; 16] {
                    return Err(capnp::Error::failed("Invalid recovery ID".into()));
                }
                let name = name(request.get_name())?;
                let backend = self.backend.clone();
                run_directory(&self.server, permit, backend.clone(), move || {
                    backend.restore_captured(recovery, name)
                })
                .await?
            };
            let Some(job) = job else {
                results
                    .get()
                    .set_status(fs_capnp::NativeEntryStatus::Refused);
                return Ok(());
            };
            results.get().set_job(job_client(self.server.clone(), job));
            Ok(())
        })
    }
    fn create_directory(
        self: Rc<Self>,
        params: directory_handle::CreateDirectoryParams,
        mut results: directory_handle::CreateDirectoryResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let job = {
                let permit = self.server.admit_call()?;
                let name = name(params.get()?.get_name())?;
                let backend = Arc::clone(&self.backend);
                run_directory(&self.server, permit, backend.clone(), move || {
                    backend.create_directory(name)
                })
                .await?
            };
            let Some(job) = job else {
                results
                    .get()
                    .set_status(fs_capnp::NativeEntryStatus::Refused);
                return Ok(());
            };
            results
                .get()
                .set_job(job_client(Rc::clone(&self.server), job));
            Ok(())
        })
    }
    fn atomic_save(
        self: Rc<Self>,
        params: directory_handle::AtomicSaveParams,
        mut results: directory_handle::AtomicSaveResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            if {
                let permit = self.server.admit_call()?;
                let request = params.get()?;
                let temporary = name(request.get_temporary())?;
                let target = name(request.get_target())?;
                let expected = [request.get_expected_device(), request.get_expected_inode()];
                if expected[1] == 0 {
                    return Err(capnp::Error::failed(
                        "invalid AtomicSave temporary identity".into(),
                    ));
                }
                let backend = Arc::clone(&self.backend);
                run_directory(&self.server, permit, backend.clone(), move || {
                    backend.atomic_save(&temporary, expected, &target)
                })
                .await?
            }
            .is_none()
            {
                results
                    .get()
                    .set_status(fs_capnp::NativeEntryStatus::Refused);
            }
            Ok(())
        })
    }
    fn rename_replace(
        self: Rc<Self>,
        params: directory_handle::RenameReplaceParams,
        mut results: directory_handle::RenameReplaceResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let job = {
                let permit = self.server.admit_call()?;
                let request = params.get()?;
                let old = name(request.get_name())?;
                let new = name(request.get_new_name())?;
                let backend = Arc::clone(&self.backend);
                run_directory(&self.server, permit, backend.clone(), move || {
                    backend.rename_replace(&old, new)
                })
                .await?
            };
            let Some(job) = job else {
                results
                    .get()
                    .set_status(fs_capnp::NativeEntryStatus::Refused);
                return Ok(());
            };
            results
                .get()
                .set_job(job_client(Rc::clone(&self.server), job));
            Ok(())
        })
    }
    fn rename_no_replace(
        self: Rc<Self>,
        params: directory_handle::RenameNoReplaceParams,
        mut results: directory_handle::RenameNoReplaceResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let job = {
                let permit = self.server.admit_call()?;
                let request = params.get()?;
                let old = name(request.get_name())?;
                let new = name(request.get_new_name())?;
                let backend = Arc::clone(&self.backend);
                run_directory(&self.server, permit, backend.clone(), move || {
                    backend.rename_no_replace(&old, new)
                })
                .await?
            };
            let Some(job) = job else {
                results
                    .get()
                    .set_status(fs_capnp::NativeEntryStatus::Refused);
                return Ok(());
            };
            results
                .get()
                .set_job(job_client(Rc::clone(&self.server), job));
            Ok(())
        })
    }
}
struct Job {
    server: Rc<Server>,
    job: Arc<dyn WorkspaceJobBackend>,
}
#[allow(
    refining_impl_trait,
    reason = "capnp server methods return the concrete Promise behind the generated impl Future"
)]
impl native_job::Server for Job {
    fn created_temporary(
        self: Rc<Self>,
        _: native_job::CreatedTemporaryParams,
        mut results: native_job::CreatedTemporaryResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let temporary = {
                let permit = self.server.admit_call()?;
                let job = self.job.clone();
                run(&self.server, permit, move || {
                    checked_result(|| job.live_scope(), || job.created_temporary())
                })
                .await?
            };
            if let Some(backend) = temporary {
                results
                    .get()
                    .set_handle(capnp_rpc::new_client(CreatedTemporary {
                        server: self.server.clone(),
                        backend,
                    }));
            } else {
                results
                    .get()
                    .set_status(fs_capnp::NativeEntryStatus::Refused);
            }
            Ok(())
        })
    }

    fn pin_replacement(
        self: Rc<Self>,
        _: native_job::PinReplacementParams,
        mut results: native_job::PinReplacementResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let pin = {
                let permit = self.server.admit_call()?;
                let job = self.job.clone();
                run(&self.server, permit, move || {
                    checked_result(|| job.live_scope(), || job.pin_replacement())
                })
                .await?
            };
            if let Some(pin) = pin {
                results
                    .get()
                    .set_handle(capnp_rpc::new_client(ReplacementPin {
                        server: self.server.clone(),
                        pin,
                    }));
            } else {
                results
                    .get()
                    .set_status(fs_capnp::NativeEntryStatus::Refused);
            }
            Ok(())
        })
    }
    fn check_scope(
        self: Rc<Self>,
        _: native_job::CheckScopeParams,
        _: native_job::CheckScopeResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let permit = self.server.admit_call()?;
            let job = self.job.clone();
            run(&self.server, permit, move || {
                job.live_scope().ok_or("scope checking unavailable")?
            })
            .await
        })
    }
    fn poll(
        self: Rc<Self>,
        _: native_job::PollParams,
        mut results: native_job::PollResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let poll = {
                let permit = self.server.admit_call()?;
                let job = Arc::clone(&self.job);
                run(&self.server, permit, move || {
                    checked_result(|| job.live_scope(), || job.poll())
                })
                .await?
            };
            let Some(poll) = poll else {
                results
                    .get()
                    .set_status(fs_capnp::NativeEntryStatus::Refused);
                return Ok(());
            };
            let mut out = results.get();
            out.set_operation_id(&poll.operation_id);
            let updates = poll.updates;
            if updates.len() > 1025 {
                return Err(capnp::Error::failed(
                    "native job exceeded update bound".into(),
                ));
            }
            let mut rows = out.init_updates(wire_u32(updates.len())?);
            for (index, update) in updates.iter().enumerate() {
                let row = rows.reborrow().get(wire_u32(index)?);
                match update {
                    JobUpdate::Progress {
                        sequence,
                        item,
                        phase,
                        bytes,
                    } => {
                        if phase.len() > 4096 {
                            return Err(capnp::Error::failed(
                                "native job phase exceeds bound".into(),
                            ));
                        }
                        let mut out = row.init_progress();
                        out.set_sequence(*sequence);
                        out.set_item(*item as u64);
                        out.set_phase(phase.as_str());
                        out.set_bytes(*bytes);
                    }
                    JobUpdate::Item {
                        item,
                        state,
                        destination,
                        error,
                        recoveries,
                    } => {
                        let mut out = row.init_item();
                        out.set_item(*item as u64);
                        out.set_has_destination(destination.is_some());
                        if let Some(identity) = destination {
                            out.set_destination_device(identity[0]);
                            out.set_destination_inode(identity[1]);
                        }
                        out.set_state(match state {
                            ItemState::Completed => NativeItemState::Completed,
                            ItemState::Failed => NativeItemState::Failed,
                            ItemState::Cancelled => NativeItemState::Cancelled,
                            ItemState::CopiedSourceRetained => {
                                NativeItemState::CopiedSourceRetained
                            }
                            ItemState::RecoveryRequired => NativeItemState::RecoveryRequired,
                            ItemState::OutcomeUnknown => NativeItemState::OutcomeUnknown,
                        });
                        let message = error.as_deref().unwrap_or_default();
                        if message.len() > 4096 || recoveries.len() > 256 {
                            return Err(capnp::Error::failed(
                                "native job exceeded item bound".into(),
                            ));
                        }
                        out.set_error(message);
                        let mut ids = out.init_recoveries(wire_u32(recoveries.len())?);
                        for (index, recovery) in recoveries.iter().enumerate() {
                            ids.set(wire_u32(index)?, recovery);
                        }
                    }
                    JobUpdate::Finished {
                        completed,
                        failed,
                        cancelled,
                    } => {
                        let mut out = row.init_finished();
                        out.set_completed(*completed as u64);
                        out.set_failed(*failed as u64);
                        out.set_cancelled(*cancelled as u64);
                    }
                }
            }
            Ok(())
        })
    }
    fn cancel(
        self: Rc<Self>,
        _: native_job::CancelParams,
        mut results: native_job::CancelResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            if {
                let permit = self.server.admit_call()?;
                let job = Arc::clone(&self.job);
                run(&self.server, permit, move || {
                    checked_result(|| job.live_scope(), || job.cancel())
                })
                .await?
            }
            .is_none()
            {
                results
                    .get()
                    .set_status(fs_capnp::NativeEntryStatus::Refused);
            }
            Ok(())
        })
    }
}

struct MetadataPin {
    server: Rc<Server>,
    backend: Arc<dyn WorkspaceMetadataBackend>,
}
#[allow(
    refining_impl_trait,
    reason = "capnp server methods return the concrete Promise behind the generated impl Future"
)]
impl metadata_pin::Server for MetadataPin {
    fn check_scope(
        self: Rc<Self>,
        _: metadata_pin::CheckScopeParams,
        _: metadata_pin::CheckScopeResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let permit = self.server.admit_call()?;
            let backend = self.backend.clone();
            run(&self.server, permit, move || {
                backend.live_scope().ok_or("scope checking unavailable")?
            })
            .await
        })
    }
    fn metadata(
        self: Rc<Self>,
        _: metadata_pin::MetadataParams,
        mut results: metadata_pin::MetadataResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let stat = {
                let permit = self.server.admit_call()?;
                let backend = self.backend.clone();
                run(&self.server, permit, move || {
                    checked_result(|| backend.live_scope(), || backend.metadata())
                })
                .await?
            };
            let Some(stat) = stat else {
                results
                    .get()
                    .set_status(fs_capnp::NativeEntryStatus::Refused);
                return Ok(());
            };
            let mut out = results.get();
            out.set_device(stat.identity[0]);
            out.set_inode(stat.identity[1]);
            out.set_file_type(stat.file_type);
            out.set_execute_bits(stat.execute_bits);
            write_metadata(out.init_metadata(), &stat.metadata);
            Ok(())
        })
    }
}

struct Cursor {
    server: Rc<Server>,
    backend: Arc<dyn crate::directory_backend::WorkspaceDirectoryCursorBackend>,
}
#[allow(
    refining_impl_trait,
    reason = "capnp server methods return the concrete Promise behind the generated impl Future"
)]
impl directory_cursor::Server for Cursor {
    fn page(
        self: Rc<Self>,
        params: directory_cursor::PageParams,
        mut results: directory_cursor::PageResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let page = {
                let permit = self.server.admit_call()?;
                let expected = params.get()?.get_expected_sequence();
                let backend = self.backend.clone();
                run(&self.server, permit, move || {
                    backend.check()?;
                    let page = backend.page(expected);
                    backend.check()?;
                    Ok(page)
                })
                .await?
            };
            let Ok(page) = page else {
                results
                    .get()
                    .set_status(fs_capnp::NativeEntryStatus::Refused);
                return Ok(());
            };
            if page.entries.len() > 128
                || page
                    .entries
                    .iter()
                    .any(|row| row.name.as_os_str().as_bytes().len() > 4096)
            {
                return Err(capnp::Error::failed("cursor page exceeds bound".into()));
            }
            let mut out = results.get();
            out.set_done(page.done);
            out.set_next_sequence(page.next_sequence);
            let mut rows = out.init_entries(wire_u32(page.entries.len())?);
            for (index, entry) in page.entries.iter().enumerate() {
                let mut row = rows.reborrow().get(wire_u32(index)?);
                row.set_name(entry.name.as_os_str().as_bytes());
                if let Some(stat) = &entry.stat {
                    row.set_device(stat.identity[0]);
                    row.set_inode(stat.identity[1]);
                    row.set_file_type(stat.file_type);
                    row.set_execute_bits(stat.execute_bits);
                    write_metadata(row.init_metadata(), &stat.metadata);
                } else {
                    row.set_status(fs_capnp::NativeEntryStatus::Refused);
                }
            }
            Ok(())
        })
    }
}

struct ReplacementPin {
    server: Rc<Server>,
    pin: Arc<dyn crate::directory_backend::WorkspaceReplacementPinBackend>,
}
#[allow(
    refining_impl_trait,
    reason = "capnp methods return the concrete Promise behind generated impl Future"
)]
impl fs_capnp::replacement_pin::Server for ReplacementPin {
    fn commit_with_history(
        self: Rc<Self>,
        params: fs_capnp::replacement_pin::CommitWithHistoryParams,
        _: fs_capnp::replacement_pin::CommitWithHistoryResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let permit = self.server.admit_call()?;
            let request = params.get()?;
            let raw = request.get_receipt()?;
            if raw.is_empty() || raw.len() > crate::external_history::LIMIT {
                return Err(capnp::Error::failed(
                    "Invalid certified commit receipt length".into(),
                ));
            }
            let bytes = raw.to_vec();
            let pin = self.pin.clone();
            run(&self.server, permit, move || {
                pin.commit_with_history(&bytes)
            })
            .await
        })
    }

    fn check(
        self: Rc<Self>,
        _: fs_capnp::replacement_pin::CheckParams,
        _: fs_capnp::replacement_pin::CheckResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let permit = self.server.admit_call()?;
            let pin = self.pin.clone();
            run(&self.server, permit, move || pin.check()).await
        })
    }
    fn observe(
        self: Rc<Self>,
        _: fs_capnp::replacement_pin::ObserveParams,
        mut results: fs_capnp::replacement_pin::ObserveResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let value = {
                let permit = self.server.admit_call()?;
                let pin = self.pin.clone();
                run(&self.server, permit, move || {
                    pin.check()?;
                    let value = pin.observe()?;
                    pin.check()?;
                    Ok(value)
                })
                .await?
            };
            if value.operation == [0; 16]
                || value.capture == [0; 16]
                || value.source_owner == [0; 32]
                || value.source[1] == 0
                || value.victim[1] == 0
                || value.destination != value.source
                || value.source == value.victim
                || value.source[0] != value.victim[0]
            {
                return Err(capnp::Error::failed(
                    "invalid native replacement observation".into(),
                ));
            }
            let mut out = results.get().init_observation();
            out.set_operation(&value.operation);
            out.set_source_owner(&value.source_owner);
            out.set_capture(&value.capture);
            out.set_source_device(value.source[0]);
            out.set_source_inode(value.source[1]);
            out.set_victim_device(value.victim[0]);
            out.set_victim_inode(value.victim[1]);
            out.set_destination_device(value.destination[0]);
            out.set_destination_inode(value.destination[1]);
            Ok(())
        })
    }
}

#[cfg(test)]
mod replacement_pin_tests {
    use super::*;
    use crate::workspace_server::MAX_BACKEND_CALLS;
    use capnp_rpc::{RpcSystem, rpc_twoparty_capnp, twoparty};
    use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
    fn assert_connected_pin_custody<'a>(
        job: &'a crate::directory_client::NativeJobClient,
        fixture: &'a crate::operations::tests::ActualReplacementFixture,
        directory: &'a crate::directory::DirectoryCapability,
        operation: crate::recovery::OperationId,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + 'a>> {
        Box::pin(async move {
            let pin = job.pin_replacement().await.unwrap();
            assert!(pin.commit_with_history(b"{}").await.is_err());
            pin.check().await.unwrap();
            let observed = pin.observe().await.unwrap();
            assert_eq!(&observed.operation, operation.as_bytes());
            assert_eq!(observed.source_owner, [37; 32]);
            assert_eq!(observed.source, observed.destination);
            assert_ne!(observed.source, observed.victim);
            assert!(matches!(
                directory
                    .restore_captured(observed.capture, EntryName::new("restored".into()).unwrap()),
                Err(crate::FsError::Busy)
            ));
            // Duplicate requests share the one genuine native exclusion claim.
            let duplicate = job.pin_replacement().await.unwrap();
            assert_eq!(duplicate.observe().await.unwrap(), observed);
            drop(pin);
            drop(duplicate);
            // Observe actual remote-capability release, never assume dropping the
            // client synchronously released the original-user capture claim.
            let deadline = std::time::Instant::now()
                .checked_add(tilo_test_env::hang_bound())
                .unwrap();
            let local = loop {
                match fixture.session.operation_store().unwrap().pin_replacement(
                    &fixture.session,
                    &operation,
                    &fixture.parent,
                ) {
                    Ok(pin) => break pin,
                    Err(crate::FsError::Busy) if std::time::Instant::now() < deadline => {
                        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                    }
                    Err(error) => panic!("actual remote capture release failed: {error}"),
                }
            };
            local.check(&fixture.session, &fixture.parent).unwrap();
            drop(local);
            let pin = job.pin_replacement().await.unwrap();
            fixture.revoke().unwrap();
            assert!(pin.check().await.is_err());
            assert!(pin.observe().await.is_err());
            drop(pin);
        })
    }

    #[test]
    fn actual_connected_replacement_pin_holds_capture_and_refuses_revoked_scope() {
        let mut fixture = crate::operations::tests::completed_replacement_for_transport();
        let directory = crate::directory::DirectoryCapability::new(
            fixture.session.clone(),
            fixture.parent.clone(),
        )
        .unwrap();
        let job = fixture.job.take().unwrap();
        let operation = job.operation_id();
        let native = directory.job_for_transport_test(job).unwrap();
        let backend = Arc::new(
            crate::directory::DirectoryWorkspaceBackend::new(
                fixture.session.clone(),
                fixture.parent.clone(),
            )
            .unwrap(),
        );
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        tokio::task::LocalSet::new().block_on(&runtime, async {
            let (a, b) = std::os::unix::net::UnixStream::pair().unwrap();
            a.set_nonblocking(true).unwrap();
            b.set_nonblocking(true).unwrap();
            let a = tokio::net::UnixStream::from_std(a).unwrap();
            let b = tokio::net::UnixStream::from_std(b).unwrap();
            let server = Rc::new(Server {
                backend,
                runtime: runtime.handle().clone(),
                calls: Arc::new(tokio::sync::Semaphore::new(MAX_BACKEND_CALLS)),
                handoffs: None,
                drain: None,
            });
            let ordinary = directory
                .create_file(EntryName::new("ordinary-file".into()).unwrap())
                .unwrap();
            let ordinary = directory.job_for_transport_test(ordinary).unwrap();
            let ordinary = crate::directory_client::NativeJobClient::for_transport_test(
                job_client(server.clone(), ordinary),
            );
            assert!(ordinary.pin_replacement().await.is_err());
            directory.check().unwrap();
            drop(ordinary);
            let job = job_client(server, native);
            let (read, write) = a.into_split();
            let network = twoparty::VatNetwork::new(
                read.compat(),
                write.compat_write(),
                rpc_twoparty_capnp::Side::Server,
                capnp::message::ReaderOptions::default(),
            );
            let server_rpc = RpcSystem::new(Box::new(network), Some(job.client));
            let stop_server = server_rpc.get_disconnector();
            let server_task = tokio::task::spawn_local(server_rpc);
            let (read, write) = b.into_split();
            let network = twoparty::VatNetwork::new(
                read.compat(),
                write.compat_write(),
                rpc_twoparty_capnp::Side::Client,
                capnp::message::ReaderOptions::default(),
            );
            let mut client_rpc = RpcSystem::new(Box::new(network), None);
            let raw = client_rpc.bootstrap(rpc_twoparty_capnp::Side::Server);
            let stop_client = client_rpc.get_disconnector();
            let client_task = tokio::task::spawn_local(client_rpc);
            let job = crate::directory_client::NativeJobClient::for_transport_test(raw);
            assert_connected_pin_custody(&job, &fixture, &directory, operation).await;
            drop(job);
            stop_client.await.unwrap();
            stop_server.await.unwrap();
            tokio::time::timeout(tilo_test_env::hang_bound(), client_task)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            tokio::time::timeout(tilo_test_env::hang_bound(), server_task)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        });
    }
}

struct PendingReplacement {
    server: Rc<Server>,
    pending: Arc<dyn crate::directory_backend::WorkspacePendingReplacementBackend>,
}
#[allow(
    refining_impl_trait,
    reason = "capnp methods return the concrete Promise behind generated impl Future"
)]
impl fs_capnp::pending_replacement::Server for PendingReplacement {
    fn retire_daemon_handoff(
        self: Rc<Self>,
        params: fs_capnp::pending_replacement::RetireDaemonHandoffParams,
        _: fs_capnp::pending_replacement::RetireDaemonHandoffResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let permit = self.server.admit_call()?;
            let ticket: [u8; 16] = params
                .get()?
                .get_ticket()?
                .try_into()
                .map_err(|_| capnp::Error::failed("invalid handoff ticket".into()))?;
            let registry = self.server.handoffs.clone();
            let pending = self.pending.clone();
            run(&self.server, permit, move || match registry {
                Some(registry) => registry.retire(&pending, ticket),
                None => pending.retire_daemon_handoff(ticket),
            })
            .await
        })
    }
    fn acknowledged_committed_pin(
        self: Rc<Self>,
        _: fs_capnp::pending_replacement::AcknowledgedCommittedPinParams,
        mut results: fs_capnp::pending_replacement::AcknowledgedCommittedPinResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let (pin, job) = {
                let permit = self.server.admit_call()?;
                let pending = self.pending.clone();
                run(&self.server, permit, move || {
                    pending.check()?;
                    let pin = pending.acknowledged_committed_pin()?;
                    let job = pending.acknowledged_committed_job()?;
                    pin.check()?;
                    pin.observe()?;
                    pending.check()?;
                    Ok((pin, job))
                })
                .await?
            };
            results
                .get()
                .set_handle(capnp_rpc::new_client(ReplacementPin {
                    server: self.server.clone(),
                    pin,
                }));
            results.get().set_job(job_client(self.server.clone(), job));
            Ok(())
        })
    }
    fn arm_daemon_handoff(
        self: Rc<Self>,
        params: fs_capnp::pending_replacement::ArmDaemonHandoffParams,
        mut results: fs_capnp::pending_replacement::ArmDaemonHandoffResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let ticket = {
                let permit = self.server.admit_call()?;
                let bytes = params.get()?.get_challenge()?;
                let challenge: [u8; 32] = bytes
                    .try_into()
                    .map_err(|_| capnp::Error::failed("invalid handoff challenge".into()))?;
                let registry = self.server.handoffs.clone();
                let pending = self.pending.clone();
                run(&self.server, permit, move || {
                    pending.check()?;
                    let ticket = match registry {
                        Some(registry) => registry.arm(pending.clone(), challenge)?,
                        None => pending.arm_daemon_handoff(challenge)?,
                    };
                    pending.check()?;
                    Ok(ticket)
                })
                .await?
            };
            results.get().set_ticket(&ticket);
            Ok(())
        })
    }

    fn observe(
        self: Rc<Self>,
        _: fs_capnp::pending_replacement::ObserveParams,
        mut results: fs_capnp::pending_replacement::ObserveResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let value = {
                let permit = self.server.admit_call()?;
                let pending = self.pending.clone();
                run(&self.server, permit, move || {
                    pending.check()?;
                    let value = pending.observe()?;
                    pending.check()?;
                    value.validate()?;
                    Ok(value)
                })
                .await?
            };
            let mut out = results.get().init_observation();
            out.set_purpose(match value.purpose {
                crate::directory_backend::ReplacementPurpose::ReplacementRename => {
                    fs_capnp::ReplacementPurpose::ReplacementRename
                }
                crate::directory_backend::ReplacementPurpose::AtomicSave => {
                    fs_capnp::ReplacementPurpose::AtomicSave
                }
            });
            out.set_operation(&value.operation);
            out.set_source_owner(&value.source_owner);
            out.set_source_device(value.source[0]);
            out.set_source_inode(value.source[1]);
            out.set_victim_device(value.expected_victim[0]);
            out.set_victim_inode(value.expected_victim[1]);
            out.set_intent_digest(&value.intent_digest);
            out.set_archive_plan_digest(&value.archive_plan_digest);
            out.set_reservation_description(&value.reservation_description);
            Ok(())
        })
    }
    fn check_scope(
        self: Rc<Self>,
        _: fs_capnp::pending_replacement::CheckScopeParams,
        _: fs_capnp::pending_replacement::CheckScopeResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let permit = self.server.admit_call()?;
            let pending = self.pending.clone();
            run(&self.server, permit, move || pending.check()).await
        })
    }
    fn cancel(
        self: Rc<Self>,
        _: fs_capnp::pending_replacement::CancelParams,
        _: fs_capnp::pending_replacement::CancelResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let permit = self.server.admit_call()?;
            let pending = self.pending.clone();
            run(&self.server, permit, move || pending.cancel()).await
        })
    }
    fn start_with_history(
        self: Rc<Self>,
        params: fs_capnp::pending_replacement::StartWithHistoryParams,
        mut results: fs_capnp::pending_replacement::StartWithHistoryResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            // Finite admission precedes even the bounded payload clone.
            let job = {
                let permit = self.server.admit_call()?;
                let request = params.get()?;
                let raw = request.get_receipt()?;
                if raw.is_empty() || raw.len() > crate::external_history::LIMIT {
                    return Err(capnp::Error::failed(
                        "Invalid certified prepare receipt length".into(),
                    ));
                }
                let receipt = raw.to_vec();
                let pending = self.pending.clone();
                run(&self.server, permit, move || {
                    pending.check()?;
                    let job = pending.start_with_history(&receipt)?;
                    if let Some(scope) = job.live_scope() {
                        scope?;
                    }
                    Ok(job)
                })
                .await?
            };
            // The pending capability is consumed: no post-check on its closed
            // reservation and no raw start or automatic mutation replay exists.
            results.get().set_job(job_client(self.server.clone(), job));
            Ok(())
        })
    }
}

#[cfg(test)]
mod pending_replacement_tests {
    use super::*;
    use crate::workspace_server::MAX_BACKEND_CALLS;
    use capnp_rpc::{RpcSystem, rpc_twoparty_capnp, twoparty};
    use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
    fn assert_pending_refuses_uncertified_start<'a>(
        pending: &'a crate::directory_client::PendingReplacementClient,
        fixture: &'a crate::operations::tests::ActualPendingFixture,
        operation: [u8; 16],
        revoke_before_start: bool,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + 'a>> {
        use ed25519_dalek::{Signer, SigningKey};
        Box::pin(async move {
            assert!(
                pending.acknowledged_committed_pin().await.is_err(),
                "real zero-effect reservation has no committed job/pin"
            );
            assert!(pending.acknowledged_committed_job().await.is_err());
            let observed = pending.observe().await.unwrap();
            assert_eq!(observed.operation, operation);
            assert_eq!(observed.source, fixture.temporary_identity);
            assert_eq!(observed.expected_victim, fixture.victim_identity);
            fixture.assert_original_entries();
            if revoke_before_start {
                fixture.revoke().unwrap();
                assert!(pending.check_scope().await.is_err());
                assert!(pending.observe().await.is_err());
                assert!(pending.acknowledged_committed_pin().await.is_err());
                assert!(pending.acknowledged_committed_job().await.is_err());
                fixture.assert_original_entries();
            } else {
                // A genuinely signed well-formed receipt is still insufficient:
                // the actual source has no admitted root-sealed daemon verifier.
                let signer = SigningKey::from_bytes(&[43; 32]);
                let person = SigningKey::from_bytes(&[44; 32]);
                let mut receipt = crate::external_history::PreparedReceipt {
                    schema: 1,
                    operation_id: uuid::Uuid::from_bytes(observed.operation).to_string(),
                    source_owner: observed.source_owner,
                    intent_digest: observed.intent_digest,
                    archive_binding_digest: observed.archive_plan_digest,
                    personal_room: uuid::Uuid::from_u128(123).to_string(),
                    person: person.verifying_key().to_bytes(),
                    device: signer.verifying_key().to_bytes(),
                    transaction_digest: [45; 32],
                    signature: Vec::new(),
                };
                receipt.signature = signer
                    .sign(&receipt.signing_bytes().unwrap())
                    .to_bytes()
                    .to_vec();
                let bytes = serde_json::to_vec(&receipt).unwrap();
                assert!(pending.start_with_history(&bytes).await.is_err());
                fixture.assert_original_entries();
                assert!(
                    pending.check_scope().await.is_err(),
                    "one-shot refusal cannot replay native start"
                );
                assert!(pending.observe().await.is_err());
            }
        })
    }

    #[test]
    fn actual_connected_pending_work_cannot_start_without_certified_verifier() {
        for revoke_before_start in [false, true] {
            let mut fixture = crate::operations::tests::pending_replacement_for_transport();
            let directory = crate::directory::DirectoryCapability::new(
                fixture.session.clone(),
                fixture.parent.clone(),
            )
            .unwrap();
            // Production prepare is closed even though the actual local fixture has
            // genuine original grants: no private admitted daemon verifier exists.
            assert!(
                crate::directory_backend::WorkspaceDirectoryBackend::prepare_replace(
                    &directory,
                    &EntryName::new("temporary".into()).unwrap(),
                    fixture.temporary_identity,
                    &EntryName::new("document".into()).unwrap(),
                    fixture.victim_identity,
                )
                .is_err()
            );
            fixture.assert_original_entries();
            let prepared = fixture.prepared.take().unwrap();
            let operation = *prepared.operation_id().unwrap().as_bytes();
            let native = directory.pending_for_transport_test(prepared).unwrap();
            let backend = Arc::new(
                crate::directory::DirectoryWorkspaceBackend::new(
                    fixture.session.clone(),
                    fixture.parent.clone(),
                )
                .unwrap(),
            );
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap();
            tokio::task::LocalSet::new().block_on(&runtime, async {
                tokio::time::timeout(tilo_test_env::hang_bound(), async {
                    let (a, b) = std::os::unix::net::UnixStream::pair().unwrap();
                    a.set_nonblocking(true).unwrap();
                    b.set_nonblocking(true).unwrap();
                    let a = tokio::net::UnixStream::from_std(a).unwrap();
                    let b = tokio::net::UnixStream::from_std(b).unwrap();
                    let server = Rc::new(Server {
                        backend,
                        runtime: runtime.handle().clone(),
                        calls: Arc::new(tokio::sync::Semaphore::new(MAX_BACKEND_CALLS)),
                        handoffs: None,
                        drain: None,
                    });
                    let pending: fs_capnp::pending_replacement::Client =
                        capnp_rpc::new_client(PendingReplacement {
                            server,
                            pending: native,
                        });
                    let (read, write) = a.into_split();
                    let network = twoparty::VatNetwork::new(
                        read.compat(),
                        write.compat_write(),
                        rpc_twoparty_capnp::Side::Server,
                        capnp::message::ReaderOptions::default(),
                    );
                    let server_rpc = RpcSystem::new(Box::new(network), Some(pending.client));
                    let stop_server = server_rpc.get_disconnector();
                    let server_task = tokio::task::spawn_local(server_rpc);
                    let (read, write) = b.into_split();
                    let network = twoparty::VatNetwork::new(
                        read.compat(),
                        write.compat_write(),
                        rpc_twoparty_capnp::Side::Client,
                        capnp::message::ReaderOptions::default(),
                    );
                    let mut client_rpc = RpcSystem::new(Box::new(network), None);
                    let raw = client_rpc.bootstrap(rpc_twoparty_capnp::Side::Server);
                    let stop_client = client_rpc.get_disconnector();
                    let client_task = tokio::task::spawn_local(client_rpc);
                    let pending =
                        crate::directory_client::PendingReplacementClient::for_transport_test(raw);
                    assert_pending_refuses_uncertified_start(
                        &pending,
                        &fixture,
                        operation,
                        revoke_before_start,
                    )
                    .await;
                    pending.cancel().await.unwrap();
                    drop(pending);
                    stop_client.await.unwrap();
                    stop_server.await.unwrap();
                    client_task.await.unwrap().unwrap();
                    server_task.await.unwrap().unwrap();
                })
                .await
                .expect("owned pending RPC outcome deadline");
            });
            if !revoke_before_start {
                fixture.revoke().unwrap();
            }
            assert!(directory.check().is_err());
        }
    }
}

struct CreatedTemporary {
    server: Rc<Server>,
    backend: Arc<dyn crate::directory_backend::WorkspaceCreatedTemporaryBackend>,
}
#[allow(
    refining_impl_trait,
    reason = "capnp methods return the concrete Promise behind generated impl Future"
)]
impl fs_capnp::created_temporary::Server for CreatedTemporary {
    fn set_permissions(
        self: Rc<Self>,
        params: fs_capnp::created_temporary::SetPermissionsParams,
        _: fs_capnp::created_temporary::SetPermissionsResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let permit = self.server.admit_call()?;
            let mode = params.get()?.get_mode();
            if mode & !0o7777 != 0 {
                return Err(capnp::Error::failed("invalid own-created mode".into()));
            }
            let backend = self.backend.clone();
            run(&self.server, permit, move || backend.set_permissions(mode)).await
        })
    }

    fn prepare_atomic_save(
        self: Rc<Self>,
        _: fs_capnp::created_temporary::PrepareAtomicSaveParams,
        mut results: fs_capnp::created_temporary::PrepareAtomicSaveResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            use crate::directory_backend::AtomicSavePreparation as Outcome;
            let outcome = {
                let permit = self.server.admit_call()?;
                let backend = self.backend.clone();
                run(&self.server, permit, move || {
                    backend.check()?;
                    let outcome = backend.prepare_atomic_save();
                    if matches!(outcome, Outcome::Refused(_)) {
                        backend.check()?;
                    }
                    Ok(outcome)
                })
                .await?
            };
            match outcome {
                Outcome::Prepared(pending) => {
                    results
                        .get()
                        .set_pending(capnp_rpc::new_client(PendingReplacement {
                            server: self.server.clone(),
                            pending,
                        }));
                }
                Outcome::Refused(_) => results
                    .get()
                    .set_status(fs_capnp::NativeEntryStatus::Refused),
                Outcome::Unknown(error) => {
                    return Err(capnp::Error::failed(error.chars().take(512).collect()));
                }
            }
            Ok(())
        })
    }

    fn prepare_replace(
        self: Rc<Self>,
        _: fs_capnp::created_temporary::PrepareReplaceParams,
        mut results: fs_capnp::created_temporary::PrepareReplaceResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let pending = {
                let permit = self.server.admit_call()?;
                let backend = self.backend.clone();
                run(&self.server, permit, move || {
                    checked_result(|| backend.live_scope(), || backend.prepare_replace())
                })
                .await?
            };
            if let Some(pending) = pending {
                results
                    .get()
                    .set_pending(capnp_rpc::new_client(PendingReplacement {
                        server: self.server.clone(),
                        pending,
                    }));
            } else {
                results
                    .get()
                    .set_status(fs_capnp::NativeEntryStatus::Refused);
            }
            Ok(())
        })
    }

    fn check_scope(
        self: Rc<Self>,
        _: fs_capnp::created_temporary::CheckScopeParams,
        _: fs_capnp::created_temporary::CheckScopeResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let permit = self.server.admit_call()?;
            let backend = self.backend.clone();
            run(&self.server, permit, move || backend.check()).await
        })
    }
    fn retire(
        self: Rc<Self>,
        _: fs_capnp::created_temporary::RetireParams,
        mut results: fs_capnp::created_temporary::RetireResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let job = {
                let permit = self.server.admit_call()?;
                let backend = self.backend.clone();
                run(&self.server, permit, move || {
                    checked_result(|| backend.live_scope(), || backend.retire())
                })
                .await?
            };
            if let Some(job) = job {
                results.get().set_job(job_client(self.server.clone(), job));
            } else {
                results
                    .get()
                    .set_status(fs_capnp::NativeEntryStatus::Refused);
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod created_temporary_tests {
    use super::*;
    use crate::workspace_server::MAX_BACKEND_CALLS;
    use capnp_rpc::{RpcSystem, rpc_twoparty_capnp, twoparty};
    use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
    fn assert_created_custody<'a>(
        cap: &'a crate::directory_client::CreatedTemporaryClient,
        fixture: &'a crate::operations::tests::ActualCreatedTemporaryFixture,
        revoke_before_retire: bool,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + 'a>> {
        Box::pin(async move {
            cap.check().await.unwrap();
            fixture.assert_original_entries();
            cap.set_permissions(0o640).await.unwrap();
            assert!(cap.set_permissions(0o10000).await.is_err());
            cap.check().await.unwrap();
            assert!(
                cap.prepare_replace().await.is_err(),
                "no sealed selected replacement permit was admitted"
            );
            assert!(
                matches!(
                    cap.prepare_atomic_save().await,
                    crate::directory_backend::AtomicSavePreparation::Refused(_)
                ),
                "source must acknowledge genuine no-reservation refusal for this creator without sealed AtomicSave consent"
            );
            fixture.assert_original_entries();
            if revoke_before_retire {
                fixture.revoke().unwrap();
                assert!(cap.check().await.is_err());
                assert!(cap.retire().await.is_err());
                fixture.assert_original_entries();
            } else {
                let mut cleanup = cap.retire().await.unwrap();
                assert!(
                    cap.retire().await.is_err(),
                    "actual native custody is one use"
                );
                let mut captured = false;
                loop {
                    let report = cleanup.poll().await.unwrap();
                    let mut finished = false;
                    for update in report.updates {
                        match update {
                            JobUpdate::Item {
                                item: 0,
                                state: crate::operations::ItemState::Completed,
                                destination: None,
                                error: None,
                                recoveries,
                            } => {
                                assert!(!recoveries.is_empty());
                                captured = true;
                            }
                            JobUpdate::Finished {
                                completed: 1,
                                failed: 0,
                                cancelled: 0,
                            } => {
                                assert!(captured);
                                finished = true;
                            }
                            JobUpdate::Progress { item: 0, .. } => {}
                            _ => panic!("unexpected actual cleanup outcome"),
                        }
                    }
                    if finished {
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
                cap.check().await.unwrap();
                fixture.assert_retired_recoverably();
            }
        })
    }

    #[test]
    fn actual_connected_created_custody_captures_once_and_refuses_revocation() {
        for revoke_before_retire in [false, true] {
            let mut fixture = crate::operations::tests::completed_created_temporary_for_transport();
            let directory = crate::directory::DirectoryCapability::new(
                fixture.session.clone(),
                fixture.parent.clone(),
            )
            .unwrap();
            let native = directory
                .job_for_transport_test(fixture.job.take().unwrap())
                .unwrap();
            let backend = Arc::new(
                crate::directory::DirectoryWorkspaceBackend::new(
                    fixture.session.clone(),
                    fixture.parent.clone(),
                )
                .unwrap(),
            );
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap();
            tokio::task::LocalSet::new().block_on(&runtime, async {
                tokio::time::timeout(tilo_test_env::hang_bound(), async {
                    let (a, b) = std::os::unix::net::UnixStream::pair().unwrap();
                    a.set_nonblocking(true).unwrap();
                    b.set_nonblocking(true).unwrap();
                    let a = tokio::net::UnixStream::from_std(a).unwrap();
                    let b = tokio::net::UnixStream::from_std(b).unwrap();
                    let server = Rc::new(Server {
                        backend,
                        runtime: runtime.handle().clone(),
                        calls: Arc::new(tokio::sync::Semaphore::new(MAX_BACKEND_CALLS)),
                        handoffs: None,
                        drain: None,
                    });
                    let job = job_client(server, native);
                    let (read, write) = a.into_split();
                    let network = twoparty::VatNetwork::new(
                        read.compat(),
                        write.compat_write(),
                        rpc_twoparty_capnp::Side::Server,
                        capnp::message::ReaderOptions::default(),
                    );
                    let server_rpc = RpcSystem::new(Box::new(network), Some(job.client));
                    let stop_server = server_rpc.get_disconnector();
                    let server_task = tokio::task::spawn_local(server_rpc);
                    let (read, write) = b.into_split();
                    let network = twoparty::VatNetwork::new(
                        read.compat(),
                        write.compat_write(),
                        rpc_twoparty_capnp::Side::Client,
                        capnp::message::ReaderOptions::default(),
                    );
                    let mut client_rpc = RpcSystem::new(Box::new(network), None);
                    let raw = client_rpc.bootstrap(rpc_twoparty_capnp::Side::Server);
                    let stop_client = client_rpc.get_disconnector();
                    let client_task = tokio::task::spawn_local(client_rpc);
                    let job = crate::directory_client::NativeJobClient::for_transport_test(raw);
                    let cap = job.created_temporary().await.unwrap();
                    assert_created_custody(&cap, &fixture, revoke_before_retire).await;
                    drop(cap);
                    drop(job);
                    stop_client.await.unwrap();
                    stop_server.await.unwrap();
                    client_task.await.unwrap().unwrap();
                    server_task.await.unwrap().unwrap();
                })
                .await
                .expect("actual connected creation custody deadline");
            });
        }
    }
}

pub(super) fn pending_client(
    server: Rc<Server>,
    pending: Arc<dyn crate::directory_backend::WorkspacePendingReplacementBackend>,
) -> fs_capnp::pending_replacement::Client {
    capnp_rpc::new_client(PendingReplacement { server, pending })
}

#[cfg(test)]
mod pending_handoff_tests {
    use crate::workspace_server::{PendingHandoffRegistry, serve_pending_connected};
    use capnp_rpc::{RpcSystem, rpc_twoparty_capnp, twoparty};
    use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
    #[test]
    fn actual_pending_only_bootstrap_refuses_workspace_and_rearm() {
        let mut fixture = crate::operations::tests::pending_replacement_for_transport();
        let directory = crate::directory::DirectoryCapability::new(
            fixture.session.clone(),
            fixture.parent.clone(),
        )
        .unwrap();
        let pending = directory
            .pending_for_transport_test(fixture.prepared.take().unwrap())
            .unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        tokio::task::LocalSet::new().block_on(&runtime, async {
            tokio::time::timeout(tilo_test_env::hang_bound(), async {
                let (source, client) = std::os::unix::net::UnixStream::pair().unwrap();
                let server = serve_pending_connected(source, pending, runtime.handle().clone(), Arc::new(PendingHandoffRegistry::default())).unwrap();
                client.set_nonblocking(true).unwrap();
                let client = tokio::net::UnixStream::from_std(client).unwrap();
                let (read, write) = client.into_split();
                let network = twoparty::VatNetwork::new(read.compat(), write.compat_write(), rpc_twoparty_capnp::Side::Client, capnp::message::ReaderOptions::default());
                let mut rpc = RpcSystem::new(Box::new(network), None);
                let raw: fs_capnp::pending_replacement::Client = rpc.bootstrap(rpc_twoparty_capnp::Side::Server);
                let workspace: fs_capnp::workspace_fs::Client = rpc.bootstrap(rpc_twoparty_capnp::Side::Server);
                let task = tokio::task::spawn_local(rpc);
                let pending = crate::directory_client::PendingReplacementClient::from_authenticated_source_capability(raw);
                let observed = pending.observe().await.unwrap();
                assert_eq!(observed.source, fixture.temporary_identity);
                observed.validate().unwrap();
                assert!(pending.arm_daemon_handoff(&[1; 32]).await.is_err(), "handoff bootstrap cannot arm recursive handoffs");
                assert!(workspace.list_request().send().promise.await.is_err(), "no generic WorkspaceFs bootstrap");
                fixture.assert_original_entries();
                pending.cancel().await.unwrap();
                assert!(pending.check_scope().await.is_err());
                fixture.assert_original_entries();
                tokio::task::spawn_blocking(move || server.retire()).await.unwrap().unwrap();
                // The RPC driver can finish with clean EOF. Observe shutdown
                // through the actual capability, which must now be unusable.
                super::history_read_handoff_tests::assert_read_driver_departure(task.await.unwrap());
                let task = tokio::task::spawn_local(async move { pending.observe().await });
                assert!(task.await.unwrap().is_err(), "client must observe actual connection shutdown");
            }).await.expect("actual connected pending fixture must complete");
        });
    }

    use super::*;
    #[test]
    fn actual_native_reservation_handoff_is_one_shot_and_revocation_checked() {
        let mut fixture = crate::operations::tests::pending_replacement_for_transport();
        let directory = crate::directory::DirectoryCapability::new(
            fixture.session.clone(),
            fixture.parent.clone(),
        )
        .unwrap();
        let native = directory
            .pending_for_transport_test(fixture.prepared.take().unwrap())
            .unwrap();
        let registry = PendingHandoffRegistry::default();
        let challenge = [3; 32];
        let ticket = registry.arm(native.clone(), challenge).unwrap();
        assert!(registry.arm(native.clone(), challenge).is_err());
        assert!(registry.take(ticket, [4; 32]).is_err());
        assert!(registry.take(ticket, challenge).is_err());
        fixture.assert_original_entries();
        let ticket = registry.arm(native.clone(), challenge).unwrap();
        let held = registry.take(ticket, challenge).unwrap();
        assert!(Arc::ptr_eq(&native, &held));
        assert!(registry.take(ticket, challenge).is_err());
        fixture.assert_original_entries();
        assert!(
            registry.arm(native.clone(), challenge).is_err(),
            "taken handoff retains its real lifecycle until observed join"
        );
        fixture.revoke().unwrap();
        assert!(registry.take(ticket, challenge).is_err());
        assert!(held.check().is_err());
        held.cancel().unwrap();
        registry.close().unwrap();
        assert!(registry.arm(native, challenge).is_err());
        fixture.assert_original_entries();
    }
}

// Finite held source capabilities across all owned connections in this worker.
static HISTORY_READ_SLOTS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
struct HistoryReadSlot;
impl HistoryReadSlot {
    fn acquire() -> capnp::Result<Self> {
        HISTORY_READ_SLOTS
            .fetch_update(
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
                |held| {
                    if held < 64 { held.checked_add(1) } else { None }
                },
            )
            .map_err(|_| capnp::Error::failed("history Read capability pool busy".into()))?;
        Ok(Self)
    }
}
impl Drop for HistoryReadSlot {
    fn drop(&mut self) {
        HISTORY_READ_SLOTS.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}
struct HistoryReadSubject {
    server: Rc<Server>,
    backend: Arc<dyn crate::directory_backend::WorkspaceHistoryReadSubjectBackend>,
    _slot: HistoryReadSlot,
}
#[allow(
    refining_impl_trait,
    reason = "capnp methods return concrete Promise behind generated impl Future"
)]
impl fs_capnp::history_read_subject::Server for HistoryReadSubject {
    fn retire_daemon_handoff(
        self: Rc<Self>,
        params: fs_capnp::history_read_subject::RetireDaemonHandoffParams,
        _: fs_capnp::history_read_subject::RetireDaemonHandoffResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let permit = self.server.admit_call()?;
            let ticket: [u8; 16] = params
                .get()?
                .get_ticket()?
                .try_into()
                .map_err(|_| capnp::Error::failed("invalid Read handoff ticket".into()))?;
            let registry = self.server.handoffs.clone();
            let subject = self.backend.clone();
            run(&self.server, permit, move || match registry {
                Some(registry) => registry.retire_read(&subject, ticket),
                None => subject.retire_daemon_handoff(ticket),
            })
            .await
        })
    }

    fn arm_daemon_handoff(
        self: Rc<Self>,
        params: fs_capnp::history_read_subject::ArmDaemonHandoffParams,
        mut results: fs_capnp::history_read_subject::ArmDaemonHandoffResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let ticket = {
                let permit = self.server.admit_call()?;
                let challenge: [u8; 32] =
                    params.get()?.get_challenge()?.try_into().map_err(|_| {
                        capnp::Error::failed("invalid history Read challenge".into())
                    })?;
                let backend = self.backend.clone();
                let registry = self.server.handoffs.clone();
                run(&self.server, permit, move || {
                    backend.check()?;
                    let ticket = match registry {
                        Some(registry) => registry.arm_read(backend.clone(), challenge),
                        None => backend.arm_daemon_handoff(challenge),
                    };
                    backend.check()?;
                    Ok(ticket)
                })
                .await?
            };
            match ticket {
                Ok(ticket) => results.get().set_ticket(&ticket),
                Err(_) => results
                    .get()
                    .set_status(fs_capnp::NativeEntryStatus::Refused),
            }
            Ok(())
        })
    }

    fn check_scope(
        self: Rc<Self>,
        _: fs_capnp::history_read_subject::CheckScopeParams,
        _: fs_capnp::history_read_subject::CheckScopeResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let permit = self.server.admit_call()?;
            let backend = self.backend.clone();
            run(&self.server, permit, move || backend.check_scope()).await
        })
    }
    fn check(
        self: Rc<Self>,
        _: fs_capnp::history_read_subject::CheckParams,
        _: fs_capnp::history_read_subject::CheckResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let permit = self.server.admit_call()?;
            let backend = self.backend.clone();
            run(&self.server, permit, move || backend.check()).await
        })
    }
    fn observe(
        self: Rc<Self>,
        _: fs_capnp::history_read_subject::ObserveParams,
        mut results: fs_capnp::history_read_subject::ObserveResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            let observation = {
                let permit = self.server.admit_call()?;
                let backend = self.backend.clone();
                run(&self.server, permit, move || {
                    backend.check()?;
                    let actual = backend.observe()?;
                    actual.validate()?;
                    backend.check()?;
                    Ok(actual)
                })
                .await?
            };
            let mut out = results.get().init_observation();
            out.set_source_owner(&observation.source_owner);
            out.set_root_device(observation.root[0]);
            out.set_root_inode(observation.root[1]);
            out.set_parent_device(observation.parent[0]);
            out.set_parent_inode(observation.parent[1]);
            out.set_device(observation.identity[0]);
            out.set_inode(observation.identity[1]);
            out.set_name(&observation.name);
            let mut parents = out.init_parents(wire_u32(observation.parents.len())?);
            for (i, part) in observation.parents.iter().enumerate() {
                parents.set(wire_u32(i)?, part);
            }
            Ok(())
        })
    }
}

#[cfg(all(test, target_os = "linux"))]
mod history_read_tests {
    use super::*;
    use crate::workspace_server::MAX_BACKEND_CALLS;
    use capnp_rpc::{RpcSystem, rpc_twoparty_capnp, twoparty};
    use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
    #[test]
    fn connected_history_subject_requires_real_read_and_refuses_revoke() {
        for with_read in [false, true] {
            let fixture = crate::operations::tests::history_read_for_transport(with_read);
            let directory = Arc::new(
                crate::directory::DirectoryCapability::new(
                    fixture.session.clone(),
                    fixture.parent.clone(),
                )
                .unwrap(),
            );
            let backend = Arc::new(
                crate::directory::DirectoryWorkspaceBackend::new(
                    fixture.session.clone(),
                    fixture.parent.clone(),
                )
                .unwrap(),
            );
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .unwrap();
            tokio::task::LocalSet::new().block_on(&runtime, async {
                tokio::time::timeout(tilo_test_env::hang_bound(), async {
                    let (a, b) = std::os::unix::net::UnixStream::pair().unwrap();
                    a.set_nonblocking(true).unwrap();
                    b.set_nonblocking(true).unwrap();
                    let a = tokio::net::UnixStream::from_std(a).unwrap();
                    let b = tokio::net::UnixStream::from_std(b).unwrap();
                    let server = Rc::new(Server {
                        backend,
                        runtime: runtime.handle().clone(),
                        calls: Arc::new(tokio::sync::Semaphore::new(MAX_BACKEND_CALLS)),
                        handoffs: None,
                        drain: None,
                    });
                    let raw = client(server, directory);
                    let (read, write) = a.into_split();
                    let network = twoparty::VatNetwork::new(
                        read.compat(),
                        write.compat_write(),
                        rpc_twoparty_capnp::Side::Server,
                        capnp::message::ReaderOptions::default(),
                    );
                    let server_rpc = RpcSystem::new(Box::new(network), Some(raw.client));
                    let stop_server = server_rpc.get_disconnector();
                    let server_task = tokio::task::spawn_local(server_rpc);
                    let (read, write) = b.into_split();
                    let network = twoparty::VatNetwork::new(
                        read.compat(),
                        write.compat_write(),
                        rpc_twoparty_capnp::Side::Client,
                        capnp::message::ReaderOptions::default(),
                    );
                    let mut rpc = RpcSystem::new(Box::new(network), None);
                    let raw = rpc.bootstrap(rpc_twoparty_capnp::Side::Server);
                    let stop_client = rpc.get_disconnector();
                    let client_task = tokio::task::spawn_local(rpc);
                    let directory = crate::directory_client::DirectoryClient::from_raw(raw);
                    let metadata = directory.stat(&fixture.name).await.unwrap().unwrap();
                    assert!(
                        directory
                            .history_query(&fixture.name, metadata.identity, b"user.tilo.history")
                            .await
                            .is_err(),
                        "genuine native Read alone cannot infer enrolled daemon history"
                    );
                    directory.metadata().await.unwrap();
                    let subject = directory.history_read_subject(&fixture.name).await;
                    if with_read {
                        let subject = subject.expect("genuine root-sealed Read subject must admit");
                        let observed = subject.observe().await.unwrap();
                        observed.validate().unwrap();
                        assert_eq!(observed.name, fixture.name.as_os_str().as_bytes());
                        subject.check_scope().await.unwrap();
                        fixture.revoke().unwrap();
                        assert!(subject.check_scope().await.is_err());
                        assert!(subject.observe().await.is_err());
                    } else {
                        assert!(
                            subject.is_err(),
                            "Enumerate alone cannot create Read subject"
                        );
                        directory.metadata().await.unwrap();
                    }
                    stop_client.await.unwrap();
                    client_task.await.unwrap().unwrap();
                    let _ = stop_server.await;
                    let _ = server_task.await.unwrap();
                })
                .await
                .expect("connected genuine Read fixture must finish within bound");
            });
        }
    }
}

pub(super) fn history_read_client(
    server: Rc<Server>,
    backend: Arc<dyn crate::directory_backend::WorkspaceHistoryReadSubjectBackend>,
) -> capnp::Result<fs_capnp::history_read_subject::Client> {
    let slot = HistoryReadSlot::acquire()?;
    Ok(capnp_rpc::new_client(HistoryReadSubject {
        server,
        backend,
        _slot: slot,
    }))
}

#[cfg(all(test, target_os = "linux"))]
mod history_read_handoff_tests {
    use super::*;
    use crate::workspace_server::{PendingHandoffRegistry, serve_history_read_connected};
    use capnp_rpc::{RpcSystem, rpc_twoparty_capnp, twoparty};
    use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
    #[test]
    fn actual_read_registry_is_one_shot_and_closed_source_revokes() {
        let fixture = crate::operations::tests::history_read_for_transport(true);
        let directory = crate::directory::DirectoryCapability::new(
            fixture.session.clone(),
            fixture.parent.clone(),
        )
        .unwrap();
        let subject = directory.history_read_subject(&fixture.name).unwrap();
        let registry = Arc::new(PendingHandoffRegistry::default());
        let challenge = [7; 32];
        let ticket = registry.arm_read(subject.clone(), challenge).unwrap();
        assert!(registry.arm_read(subject.clone(), challenge).is_err());
        assert!(
            registry.take(ticket, challenge).is_err(),
            "Read route cannot yield Pending work"
        );
        let same = registry.take_read(ticket, challenge).unwrap();
        assert!(Arc::ptr_eq(&same, &subject));
        assert!(registry.take_read(ticket, challenge).is_err());
        assert!(
            registry.arm_read(subject.clone(), [8; 32]).is_err(),
            "consumed Read handoff stays armed until its observed join and retirement"
        );
        // Taking a ticket does not retire its lifecycle. Register the actual
        // transport, observe its owned worker's departure, then retire the route
        // before attempting to rearm the same source.
        let (source, client) = std::os::unix::net::UnixStream::pair().unwrap();
        registry
            .begin_read(ticket, same.clone(), source.try_clone().unwrap())
            .unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let server =
            serve_history_read_connected(source, same, runtime.handle().clone(), registry.clone())
                .unwrap();
        let joined = server.retire();
        registry
            .observed_join(
                ticket,
                joined.as_ref().copied().map_err(ToString::to_string),
            )
            .unwrap();
        joined.unwrap();
        assert!(
            registry.arm_read(subject.clone(), [8; 32]).is_err(),
            "observed join alone must not release the original source route"
        );
        registry.retire_read(&subject, ticket).unwrap();
        drop(client);
        let next = registry.arm_read(subject.clone(), [8; 32]).unwrap();
        fixture.session.close();
        assert!(registry.take_read(next, [8; 32]).is_err());
        assert!(subject.check_scope().is_err());
        registry.close().unwrap();
        assert!(registry.arm_read(subject, [9; 32]).is_err());
    }
    #[test]
    fn actual_read_only_bootstrap_refuses_workspace_and_recursive_handoff() {
        let fixture = crate::operations::tests::history_read_for_transport(true);
        let directory = crate::directory::DirectoryCapability::new(
            fixture.session.clone(),
            fixture.parent.clone(),
        )
        .unwrap();
        let subject = directory.history_read_subject(&fixture.name).unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        tokio::task::LocalSet::new().block_on(&runtime, async {
            tokio::time::timeout(tilo_test_env::hang_bound(), async {
                let (source, socket) = std::os::unix::net::UnixStream::pair().unwrap();
                let server = serve_history_read_connected(source, subject, runtime.handle().clone(), Arc::new(PendingHandoffRegistry::default())).unwrap();
                socket.set_nonblocking(true).unwrap();
                let socket = tokio::net::UnixStream::from_std(socket).unwrap();
                let (read,write) = socket.into_split();
                let network = twoparty::VatNetwork::new(read.compat(),write.compat_write(),rpc_twoparty_capnp::Side::Client,capnp::message::ReaderOptions::default());
                let mut rpc = RpcSystem::new(Box::new(network),None);
                let raw = rpc.bootstrap(rpc_twoparty_capnp::Side::Server);
                let workspace: fs_capnp::workspace_fs::Client = rpc.bootstrap(rpc_twoparty_capnp::Side::Server);
                let task = tokio::task::spawn_local(rpc);
                let subject = crate::directory_client::HistoryReadSubjectClient::from_authenticated_source_capability(raw);
                let observed = subject.observe().await.unwrap();
                assert_eq!(observed.name, fixture.name.as_os_str().as_bytes());
                assert!(subject.arm_daemon_handoff([1; 32]).await.is_err());
                assert!(workspace.list_request().send().promise.await.is_err());
                fixture.revoke().unwrap();
                assert!(subject.check_scope().await.is_err());
                tokio::task::spawn_blocking(move || server.retire())
                    .await
                    .expect("owned Read server retirement worker must not panic")
                    .expect("owned Read server must observe thread departure");
                assert_read_driver_departure(task.await.expect("Read client driver must not panic"));
                assert!(subject.observe().await.is_err());
            }).await.expect("actual lower Read bootstrap must finish within bound");
        });
    }
    pub(super) fn assert_read_driver_departure(result: Result<(), capnp::Error>) {
        if let Err(error) = result {
            assert_eq!(
                error.kind,
                capnp::ErrorKind::Disconnected,
                "retirement may produce clean EOF or disconnection, never a malformed RPC failure"
            );
        }
    }

    #[test]
    fn actual_live_read_subject_refuses_rpc_after_observed_server_retirement() {
        let fixture = crate::operations::tests::history_read_for_transport(true);
        let directory = crate::directory::DirectoryCapability::new(
            fixture.session.clone(),
            fixture.parent.clone(),
        )
        .unwrap();
        let source_subject = directory.history_read_subject(&fixture.name).unwrap();
        let retained_subject = source_subject.clone();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        tokio::task::LocalSet::new().block_on(&runtime, async {
            tokio::time::timeout(tilo_test_env::hang_bound(), async {
                let (source, socket) = std::os::unix::net::UnixStream::pair().unwrap();
                let server = serve_history_read_connected(source, source_subject,
                    runtime.handle().clone(), Arc::new(PendingHandoffRegistry::default())).unwrap();
                socket.set_nonblocking(true).unwrap();
                let socket = tokio::net::UnixStream::from_std(socket).unwrap();
                let (read, write) = socket.into_split();
                let network = twoparty::VatNetwork::new(read.compat(), write.compat_write(),
                    rpc_twoparty_capnp::Side::Client, capnp::message::ReaderOptions::default());
                let mut rpc = RpcSystem::new(Box::new(network), None);
                let raw = rpc.bootstrap(rpc_twoparty_capnp::Side::Server);
                let task = tokio::task::spawn_local(rpc);
                let subject = crate::directory_client::HistoryReadSubjectClient::from_authenticated_source_capability(raw);
                subject.check().await.unwrap();
                assert_eq!(subject.observe().await.unwrap().name, fixture.name.as_os_str().as_bytes());
                tokio::task::spawn_blocking(move || server.retire()).await
                    .expect("owned Read server retirement worker must not panic")
                    .expect("owned Read server must observe thread departure");
                assert_read_driver_departure(task.await.expect("Read client driver must not panic"));
                // Native Read authority is still live: only the genuinely retired
                // connected service rejects this otherwise valid subject.
                retained_subject.check().unwrap();
                assert!(subject.check().await.is_err());
                assert!(subject.observe().await.is_err());
                retained_subject.check().unwrap();
            }).await.expect("live Read retirement and both drivers must finish within bound");
        });
    }
}

#[cfg(test)]
mod pending_purpose_transport_tests {
    use super::*;
    use capnp_rpc::{RpcSystem, rpc_twoparty_capnp, twoparty};
    use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
    struct RelabelledPending {
        actual: crate::directory_backend::PendingReplacementObservation,
    }
    impl fs_capnp::pending_replacement::Server for RelabelledPending {
        fn observe(
            self: Rc<Self>,
            _: fs_capnp::pending_replacement::ObserveParams,
            mut results: fs_capnp::pending_replacement::ObserveResults,
        ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
            let value = &self.actual;
            let mut out = results.get().init_observation();
            out.set_operation(&value.operation);
            out.set_source_owner(&value.source_owner);
            out.set_source_device(value.source[0]);
            out.set_source_inode(value.source[1]);
            out.set_victim_device(value.expected_victim[0]);
            out.set_victim_inode(value.expected_victim[1]);
            out.set_intent_digest(&value.intent_digest);
            out.set_archive_plan_digest(&value.archive_plan_digest);
            out.set_reservation_description(&value.reservation_description);
            // Malicious reply relabels only this field. The real native generic
            // reservation bytes, operation and digest are left unchanged.
            out.set_purpose(fs_capnp::ReplacementPurpose::AtomicSave);
            std::future::ready(Ok(()))
        }
    }
    #[test]
    fn actual_connected_client_refuses_atomic_label_on_genuine_generic_reservation() {
        let mut fixture = crate::operations::tests::pending_replacement_for_transport();
        let directory = crate::directory::DirectoryCapability::new(
            fixture.session.clone(),
            fixture.parent.clone(),
        )
        .unwrap();
        let native = directory
            .pending_for_transport_test(fixture.prepared.take().unwrap())
            .unwrap();
        let actual = native.observe().unwrap();
        assert_eq!(
            actual.purpose,
            crate::directory_backend::ReplacementPurpose::ReplacementRename
        );
        actual.validate().unwrap();
        fixture.assert_original_entries();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        tokio::task::LocalSet::new().block_on(&runtime, async {
            tokio::time::timeout(tilo_test_env::hang_bound(), async {
                let (a, b) = std::os::unix::net::UnixStream::pair().unwrap();
                a.set_nonblocking(true).unwrap();
                b.set_nonblocking(true).unwrap();
                let a = tokio::net::UnixStream::from_std(a).unwrap();
                let b = tokio::net::UnixStream::from_std(b).unwrap();
                let cap: fs_capnp::pending_replacement::Client =
                    capnp_rpc::new_client(RelabelledPending { actual });
                let (read, write) = a.into_split();
                let network = twoparty::VatNetwork::new(
                    read.compat(),
                    write.compat_write(),
                    rpc_twoparty_capnp::Side::Server,
                    capnp::message::ReaderOptions::default(),
                );
                let server = RpcSystem::new(Box::new(network), Some(cap.client));
                let stop_server = server.get_disconnector();
                let server_task = tokio::task::spawn_local(server);
                let (read, write) = b.into_split();
                let network = twoparty::VatNetwork::new(
                    read.compat(),
                    write.compat_write(),
                    rpc_twoparty_capnp::Side::Client,
                    capnp::message::ReaderOptions::default(),
                );
                let mut client = RpcSystem::new(Box::new(network), None);
                let raw = client.bootstrap(rpc_twoparty_capnp::Side::Server);
                let stop_client = client.get_disconnector();
                let client_task = tokio::task::spawn_local(client);
                let pending =
                    crate::directory_client::PendingReplacementClient::for_transport_test(raw);
                assert!(
                    pending.observe().await.is_err(),
                    "valid generic native bytes cannot be relabelled AtomicSave by RPC metadata"
                );
                fixture.assert_original_entries();
                drop(pending);
                let _ = stop_client.await;
                let _ = stop_server.await;
                let _ = client_task.await.unwrap();
                let _ = server_task.await.unwrap();
            })
            .await
            .expect("connected malicious response must finish within deadline");
        });
        native.cancel().unwrap();
        fixture.assert_original_entries();
    }
}

#[cfg(test)]
mod atomic_prepare_transport_tests {
    use super::*;
    use crate::workspace_server::MAX_BACKEND_CALLS;
    use capnp_rpc::{RpcSystem, rpc_twoparty_capnp, twoparty};
    use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
    fn completed(
        job: &mut crate::directory_client::NativeJobClient,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + '_>> {
        Box::pin(async move {
            let mut item = false;
            loop {
                let report = job.poll().await.unwrap();
                for update in report.updates {
                    match update {
                        JobUpdate::Item {
                            item: 0,
                            state: crate::operations::ItemState::Completed,
                            error: None,
                            ..
                        } => item = true,
                        JobUpdate::Finished {
                            completed: 1,
                            failed: 0,
                            cancelled: 0,
                        } => {
                            assert!(item);
                            return;
                        }
                        JobUpdate::Progress { item: 0, .. } => {}
                        _ => panic!("unexpected actual native effect outcome"),
                    }
                }
                tokio::task::yield_now().await;
            }
        })
    }
    #[test]
    fn actual_connected_prepare_refusal_allows_cleanup_and_fresh_retry_but_disconnect_is_unknown() {
        use crate::directory_backend::AtomicSavePreparation as Outcome;
        let fixture = crate::operations::tests::completed_created_temporary_for_transport();
        let directory = Arc::new(
            crate::directory::DirectoryCapability::new(
                fixture.session.clone(),
                fixture.parent.clone(),
            )
            .unwrap(),
        );
        let backend = Arc::new(
            crate::directory::DirectoryWorkspaceBackend::new(
                fixture.session.clone(),
                fixture.parent.clone(),
            )
            .unwrap(),
        );
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        tokio::task::LocalSet::new().block_on(&runtime,async {
            tokio::time::timeout(tilo_test_env::hang_bound(),async {
                let (a,b)=std::os::unix::net::UnixStream::pair().unwrap();
                let interrupt=b.try_clone().unwrap();
                a.set_nonblocking(true).unwrap();b.set_nonblocking(true).unwrap();
                let a=tokio::net::UnixStream::from_std(a).unwrap();let b=tokio::net::UnixStream::from_std(b).unwrap();
                let state=Rc::new(Server {backend,runtime:runtime.handle().clone(),calls:Arc::new(tokio::sync::Semaphore::new(MAX_BACKEND_CALLS)),handoffs:None,drain:None});
                let cap=client(state,directory.clone());
                let (read,write)=a.into_split();
                let network=twoparty::VatNetwork::new(read.compat(),write.compat_write(),rpc_twoparty_capnp::Side::Server,capnp::message::ReaderOptions::default());
                let server=RpcSystem::new(Box::new(network),Some(cap.client));let stop_server=server.get_disconnector();let server_task=tokio::task::spawn_local(server);
                let (read,write)=b.into_split();
                let network=twoparty::VatNetwork::new(read.compat(),write.compat_write(),rpc_twoparty_capnp::Side::Client,capnp::message::ReaderOptions::default());
                let mut rpc=RpcSystem::new(Box::new(network),None);let raw=rpc.bootstrap(rpc_twoparty_capnp::Side::Server);let stop_client=rpc.get_disconnector();let client_task=tokio::task::spawn_local(rpc);
                let remote=crate::directory_client::DirectoryClient::from_raw(raw);
                for raw in ["attempt-one","attempt-two"] {
                    let name=EntryName::new(raw.into()).unwrap();
                    let mut job=remote.create_file(&name).await.unwrap();completed(&mut job).await;
                    let creator=job.created_temporary().await.unwrap();
                    assert!(matches!(creator.prepare_atomic_save().await,Outcome::Refused(_)),"genuine missing consent is an acknowledged no-reservation result");
                    assert!(remote.stat(&name).await.unwrap().is_some());
                    let mut cleanup=creator.retire().await.unwrap();completed(&mut cleanup).await;
                    assert!(remote.stat(&name).await.unwrap().is_none(),"actual completed own-temp retirement removes only its own named transient into recovery");
                    fixture.assert_original_entries();
                }
                let name=EntryName::new("uncertain-attempt".into()).unwrap();
                let mut job=remote.create_file(&name).await.unwrap();completed(&mut job).await;
                let creator=job.created_temporary().await.unwrap();
                // Shut down only this actual held RPC socket, after minting the
                // real creator. No fake positive selected/AtomicSave grant exists.
                interrupt.shutdown(std::net::Shutdown::Both).unwrap();
                assert!(matches!(creator.prepare_atomic_save().await,Outcome::Unknown(_)),"disconnected transport cannot acknowledge no reservation");
                assert!(directory.stat(&name).unwrap().is_some(),"unknown preparation must not issue cleanup or claim transient disappearance");
                fixture.assert_original_entries();
                drop(creator);drop(job);drop(remote);
                let _=stop_client.await;let _=stop_server.await;
                let _=client_task.await.unwrap();let _=server_task.await.unwrap();
            }).await.expect("actual connected refusal/retry/unknown fixture must finish");
        });
    }
}

#[cfg(test)]
mod malformed_atomic_refusal_tests {
    use super::*;
    use crate::workspace_server::MAX_BACKEND_CALLS;
    use capnp_rpc::{RpcSystem, rpc_twoparty_capnp, twoparty};
    use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
    struct MalformedRefusal {
        server: Rc<Server>,
        pending: Arc<dyn crate::directory_backend::WorkspacePendingReplacementBackend>,
    }
    impl fs_capnp::created_temporary::Server for MalformedRefusal {
        fn prepare_atomic_save(
            self: Rc<Self>,
            _: fs_capnp::created_temporary::PrepareAtomicSaveParams,
            mut results: fs_capnp::created_temporary::PrepareAtomicSaveResults,
        ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
            results
                .get()
                .set_status(fs_capnp::NativeEntryStatus::Refused);
            results
                .get()
                .set_pending(capnp_rpc::new_client(PendingReplacement {
                    server: self.server.clone(),
                    pending: self.pending.clone(),
                }));
            std::future::ready(Ok(()))
        }
    }
    #[test]
    fn actual_connected_refused_reply_with_real_pending_is_unknown_not_no_reservation_ack() {
        use crate::directory_backend::AtomicSavePreparation as Outcome;
        let mut fixture = crate::operations::tests::pending_replacement_for_transport();
        let directory = crate::directory::DirectoryCapability::new(
            fixture.session.clone(),
            fixture.parent.clone(),
        )
        .unwrap();
        let pending = directory
            .pending_for_transport_test(fixture.prepared.take().unwrap())
            .unwrap();
        pending.observe().unwrap().validate().unwrap();
        let backend = Arc::new(
            crate::directory::DirectoryWorkspaceBackend::new(
                fixture.session.clone(),
                fixture.parent.clone(),
            )
            .unwrap(),
        );
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        tokio::task::LocalSet::new().block_on(&runtime,async {
            tokio::time::timeout(tilo_test_env::hang_bound(),async {
                let (a,b)=std::os::unix::net::UnixStream::pair().unwrap();a.set_nonblocking(true).unwrap();b.set_nonblocking(true).unwrap();
                let a=tokio::net::UnixStream::from_std(a).unwrap();let b=tokio::net::UnixStream::from_std(b).unwrap();
                let state=Rc::new(Server {backend,runtime:runtime.handle().clone(),calls:Arc::new(tokio::sync::Semaphore::new(MAX_BACKEND_CALLS)),handoffs:None,drain:None});
                let cap:fs_capnp::created_temporary::Client=capnp_rpc::new_client(MalformedRefusal {server:state,pending:pending.clone()});
                let (read,write)=a.into_split();let network=twoparty::VatNetwork::new(read.compat(),write.compat_write(),rpc_twoparty_capnp::Side::Server,capnp::message::ReaderOptions::default());
                let server=RpcSystem::new(Box::new(network),Some(cap.client));let stop_server=server.get_disconnector();let server_task=tokio::task::spawn_local(server);
                let (read,write)=b.into_split();let network=twoparty::VatNetwork::new(read.compat(),write.compat_write(),rpc_twoparty_capnp::Side::Client,capnp::message::ReaderOptions::default());
                let mut client=RpcSystem::new(Box::new(network),None);let raw=client.bootstrap(rpc_twoparty_capnp::Side::Server);let stop_client=client.get_disconnector();let client_task=tokio::task::spawn_local(client);
                let creator=crate::directory_client::CreatedTemporaryClient::for_transport_test(raw);
                assert!(matches!(creator.prepare_atomic_save().await,Outcome::Unknown(_)),"Refused plus an actual reservation capability must never release the cleanup slot");
                pending.check().unwrap();fixture.assert_original_entries();
                drop(creator);let _=stop_client.await;let _=stop_server.await;let _=client_task.await.unwrap();let _=server_task.await.unwrap();
            }).await.expect("actual malformed refusal fixture must finish");
        });
        pending.cancel().unwrap();
        fixture.assert_original_entries();
    }
}
