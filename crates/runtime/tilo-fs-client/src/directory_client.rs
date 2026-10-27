//! Raw-name clients on the existing capability connection. No native authority is
//! serialized. Append/mutation replies must not be blindly retried after loss.
#![allow(
    clippy::future_not_send,
    reason = "capnp-rpc's Rc-based client is inherently !Send; callers run inside a single-threaded LocalSet, per this crate's module docs"
)]
use crate::directory_types::ItemState;
use crate::directory_types::{DirectoryRow, DirectoryRows, DirectoryStat, JobPoll, JobUpdate};
use crate::{EntryName, FileHandle, Metadata, OpenMode, fs_capnp};
use runtime_portal::NotAvailable;
use std::{
    os::unix::ffi::{OsStrExt, OsStringExt},
    rc::Rc,
};
fn invalid(message: &str) -> capnp::Error {
    capnp::Error::failed(message.into())
}
fn component(name: &EntryName) -> Result<&[u8], NotAvailable> {
    let bytes = name.as_os_str().as_bytes();
    if bytes.len() > 4096 {
        return Err(invalid("component exceeds bound").into());
    }
    Ok(bytes)
}
/// Aliases of the same live directory capability; names carry no authority.
#[derive(Clone)]
pub struct DirectoryClient {
    inner: Rc<fs_capnp::directory_handle::Client>,
}
impl DirectoryClient {
    /// # Errors
    /// Refuses absent Read enrollment, malformed response or source loss.
    pub async fn history_query(
        &self,
        name: &EntryName,
        expected: [u64; 2],
        query: &[u8],
    ) -> Result<Vec<u8>, NotAvailable> {
        crate::directory_types::validate_history_query(query).map_err(|error| invalid(&error))?;
        if expected[1] == 0 {
            return Err(invalid("missing history subject identity").into());
        }
        let mut request = self.inner.history_query_request();
        request.get().set_name(component(name)?);
        request.get().set_expected_device(expected[0]);
        request.get().set_expected_inode(expected[1]);
        request.get().set_query(query);
        let reply = request.send().promise.await?;
        let out = reply.get()?;
        crate::native_reply::check_payload(
            out.get_status()
                .map_err(|_| invalid("unknown native result status"))?,
            !out.has_response(),
        )?;
        let bytes = out.get_response()?;
        crate::directory_types::validate_history_response(query, bytes)
            .map_err(|error| invalid(&error))?;
        Ok(bytes.to_vec())
    }

    /// # Errors
    /// Refuses absent original Read grant, malformed replies or source loss.
    pub async fn history_read_subject(
        &self,
        name: &EntryName,
    ) -> Result<HistoryReadSubjectClient, NotAvailable> {
        let mut request = self.inner.history_read_subject_request();
        request.get().set_name(component(name)?);
        let reply = request.send().promise.await?;
        let out = reply.get()?;
        crate::native_reply::check_payload(
            out.get_status()
                .map_err(|_| invalid("unknown native result status"))?,
            !out.has_handle(),
        )?;
        let subject = HistoryReadSubjectClient {
            inner: out.get_handle()?,
        };
        subject.check().await?;
        Ok(subject)
    }

    /// # Errors
    /// Refuses native metadata scope, durability failure or invalid transport.
    pub async fn sync_directory(&self) -> Result<(), NotAvailable> {
        let reply = self.inner.sync_directory_request().send().promise.await?;
        crate::native_reply::check_payload(
            reply
                .get()?
                .get_status()
                .map_err(|_| invalid("unknown native result status"))?,
            true,
        )?;
        Ok(())
    }

    /// Reserve only actual held native work; observations are never source grants.
    /// # Errors
    /// Refuses absent sealed verifier, substitutions, rights or admission failure.
    pub async fn prepare_replace(
        &self,
        temporary: &EntryName,
        expected: [u64; 2],
        victim: &EntryName,
        old: [u64; 2],
    ) -> Result<PendingReplacementClient, NotAvailable> {
        if expected[1] == 0 || old[1] == 0 || expected == old || expected[0] != old[0] {
            return Err(invalid("Invalid replacing inode observations").into());
        }
        let mut request = self.inner.prepare_replace_request();
        request.get().set_temporary(component(temporary)?);
        request.get().set_temporary_device(expected[0]);
        request.get().set_temporary_inode(expected[1]);
        request.get().set_victim(component(victim)?);
        request.get().set_victim_device(old[0]);
        request.get().set_victim_inode(old[1]);
        let reply = request.send().promise.await?;
        let out = reply.get()?;
        crate::native_reply::check_payload(
            out.get_status()
                .map_err(|_| invalid("unknown native result status"))?,
            !out.has_handle(),
        )?;
        let pending = PendingReplacementClient {
            inner: out.get_handle()?,
        };
        pending.check_scope().await?;
        Ok(pending)
    }
    /// # Errors
    /// Refuses source scope/transport loss and malformed native capacity metadata.
    pub async fn statfs(
        &self,
    ) -> Result<crate::directory_types::DirectoryFilesystemStats, NotAvailable> {
        let reply = self.inner.statfs_request().send().promise.await?;
        let out = reply.get()?;
        crate::native_reply::check_payload(
            out.get_status()
                .map_err(|_| invalid("unknown native result status"))?,
            !out.has_stats(),
        )?;
        decode_filesystem_stats(out.get_stats()?)
    }
    /// Wraps a received capability; the host still owns admission and revocation.
    pub fn from_raw(inner: fs_capnp::directory_handle::Client) -> Self {
        Self {
            inner: Rc::new(inner),
        }
    }
    /// # Errors
    /// Refuses enumeration admission or transport/authority failure.
    pub async fn open_cursor(&self) -> Result<DirectoryCursorClient, NotAvailable> {
        let reply = self.inner.open_cursor_request().send().promise.await?;
        let out = reply.get()?;
        crate::native_reply::check_payload(
            out.get_status()
                .map_err(|_| invalid("unknown native result status"))?,
            !out.has_handle(),
        )?;
        Ok(DirectoryCursorClient {
            inner: Rc::new(out.get_handle()?),
        })
    }
    pub async fn readlink(
        &self,
        name: &EntryName,
        expected: [u64; 2],
    ) -> Result<Vec<u8>, NotAvailable> {
        let mut request = self.inner.readlink_request();
        request.get().set_name(component(name)?);
        request.get().set_expected_device(expected[0]);
        request.get().set_expected_inode(expected[1]);
        let reply = request.send().promise.await?;
        let out = reply.get()?;
        crate::native_reply::check_payload(
            out.get_status()
                .map_err(|_| invalid("unknown native result status"))?,
            !out.has_target(),
        )?;
        let target = out.get_target()?;
        if target.is_empty() || target.len() > 4096 || target.contains(&0) {
            return Err(invalid("invalid native symlink target").into());
        }
        Ok(target.to_vec())
    }
    pub async fn metadata(&self) -> Result<DirectoryStat, NotAvailable> {
        let reply = self.inner.metadata_request().send().promise.await?;
        let out = reply.get()?;
        crate::native_reply::check_payload(
            out.get_status()
                .map_err(|_| invalid("unknown native result status"))?,
            out.get_device() == 0
                && out.get_inode() == 0
                && out.get_file_type() == 0
                && out.get_execute_bits() == 0
                && !out.has_metadata(),
        )?;
        let identity = identity(out.get_device(), out.get_inode())?;
        Ok(DirectoryStat {
            execute_bits: execute_bits(out.get_execute_bits())?,
            metadata: Metadata::from_reader(out.get_metadata()?),
            identity,
            file_type: native_type(
                out.get_file_type(),
                Metadata::from_reader(out.get_metadata()?).kind,
            )?,
        })
    }
    pub async fn stat(&self, name: &EntryName) -> Result<Option<DirectoryStat>, NotAvailable> {
        let mut req = self.inner.stat_request();
        req.get().set_name(component(name)?);
        let reply = req.send().promise.await?;
        let out = reply.get()?;
        let empty = !out.get_present()
            && out.get_device() == 0
            && out.get_inode() == 0
            && out.get_file_type() == 0
            && out.get_execute_bits() == 0
            && !out.has_metadata();
        crate::native_reply::check_payload(
            out.get_status()
                .map_err(|_| invalid("unknown native result status"))?,
            empty,
        )?;
        if !out.get_present() && !empty {
            return Err(invalid("absent native stat carries metadata or inode claims").into());
        }
        Ok(if out.get_present() {
            Some(DirectoryStat {
                execute_bits: execute_bits(out.get_execute_bits())?,
                metadata: Metadata::from_reader(out.get_metadata()?),
                identity: identity(out.get_device(), out.get_inode())?,
                file_type: native_type(
                    out.get_file_type(),
                    Metadata::from_reader(out.get_metadata()?).kind,
                )?,
            })
        } else {
            None
        })
    }
    /// Retain metadata custody without any byte IO capability.
    /// # Errors
    /// Refuses changed identity, wrong type, revoked scope or transport failure.
    pub async fn pin_metadata(
        &self,
        name: &EntryName,
        expected: [u64; 2],
    ) -> Result<MetadataPinClient, NotAvailable> {
        let mut request = self.inner.pin_metadata_request();
        request.get().set_name(component(name)?);
        request.get().set_expected_device(expected[0]);
        request.get().set_expected_inode(expected[1]);
        let reply = request.send().promise.await?;
        crate::native_reply::check_payload(
            reply
                .get()?
                .get_status()
                .map_err(|_| invalid("unknown native result status"))?,
            !reply.get()?.has_handle(),
        )?;
        let pin = MetadataPinClient {
            inner: Rc::new(reply.get()?.get_handle()?),
        };
        let actual = pin.metadata().await?;
        if actual.identity != expected || actual.metadata.kind != crate::EntryKind::File {
            return Err(invalid("metadata pin identity/type differs").into());
        }
        Ok(pin)
    }
    /// # Errors
    /// Refuses stale/foreign continuations, oversized pages, revocation or transport loss.
    pub async fn list(&self, cursor: u64) -> Result<DirectoryRows, NotAvailable> {
        let mut req = self.inner.list_request();
        req.get().set_cursor(cursor);
        let reply = req.send().promise.await?;
        let out = reply.get()?;
        crate::native_reply::check_payload(
            out.get_status()
                .map_err(|_| invalid("unknown native result status"))?,
            out.get_next() == 0 && out.get_entries()?.is_empty(),
        )?;
        let entries = out.get_entries()?;
        if entries.len() > 256 {
            return Err(invalid("directory page exceeds bound").into());
        }
        let mut rows = Vec::with_capacity(entries.len() as usize);
        for entry in entries {
            let bytes = entry.get_name()?;
            if bytes.len() > 4096 {
                return Err(invalid("component exceeds bound").into());
            }
            let name = EntryName::new(std::ffi::OsString::from_vec(bytes.to_vec()))
                .map_err(|e| invalid(&e.to_string()))?;
            rows.push(DirectoryRow {
                name,
                metadata: Metadata::from_reader(entry.get_metadata()?),
            });
        }
        Ok(DirectoryRows {
            entries: rows,
            next: out.get_next(),
        })
    }
    /// # Errors
    /// Refuses non-directory, invalid-name, missing-right and revoked scopes.
    pub async fn directory(&self, name: &EntryName) -> Result<Self, NotAvailable> {
        let mut req = self.inner.open_directory_request();
        req.get().set_name(component(name)?);
        let reply = req.send().promise.await?;
        crate::native_reply::check_payload(
            reply
                .get()?
                .get_status()
                .map_err(|_| invalid("unknown native result status"))?,
            !reply.get()?.has_handle(),
        )?;
        Ok(Self::from_raw(reply.get()?.get_handle()?))
    }
    /// Open an existing no-follow child. Missing files require an actual create job.
    /// # Errors
    /// Refuses unsupported modes, missing rights, wrong file type and revoked scopes.
    pub async fn open_file(
        &self,
        name: &EntryName,
        mode: OpenMode,
    ) -> Result<FileHandle, NotAvailable> {
        let observed = self
            .stat(name)
            .await?
            .ok_or_else(|| invalid("native file missing"))?;
        self.open_file_expected(name, mode, observed.identity).await
    }
    /// # Errors
    /// Refuses a changed named inode before an actual open/truncate effect.
    pub async fn open_file_expected(
        &self,
        name: &EntryName,
        mode: OpenMode,
        expected: [u64; 2],
    ) -> Result<FileHandle, NotAvailable> {
        identity(expected[0], expected[1])?;
        let mut req = self.inner.open_file_request();
        req.get().set_name(component(name)?);
        req.get().set_mode(mode.into());
        req.get().set_expected_device(expected[0]);
        req.get().set_expected_inode(expected[1]);
        let reply = req.send().promise.await?;
        let out = reply.get()?;
        crate::native_reply::check_payload(
            out.get_status()
                .map_err(|_| invalid("unknown native result status"))?,
            out.get_device() == 0 && out.get_inode() == 0 && !out.has_handle(),
        )?;
        if identity(out.get_device(), out.get_inode())? != expected {
            return Err(invalid("opened native identity differs").into());
        }
        Ok(FileHandle {
            inner: Rc::new(out.get_handle()?),
        })
    }
    /// Admit empty-file creation through a real durable native job. No overwrite.
    /// # Errors
    /// Refuses missing owner store, queue admission, revoked scope or transport loss.
    pub async fn set_created_permissions(
        &self,
        name: &EntryName,
        expected: [u64; 2],
        mode: u32,
    ) -> Result<(), NotAvailable> {
        if expected[1] == 0 || mode & !0o7777 != 0 {
            return Err(invalid("invalid own-created mode request").into());
        }
        let mut request = self.inner.set_created_permissions_request();
        request.get().set_name(component(name)?);
        request.get().set_expected_device(expected[0]);
        request.get().set_expected_inode(expected[1]);
        request.get().set_mode(mode);
        let reply = request.send().promise.await?;
        crate::native_reply::check_payload(
            reply
                .get()?
                .get_status()
                .map_err(|_| invalid("unknown native result status"))?,
            true,
        )
    }
    pub async fn retire_created(
        &self,
        name: &EntryName,
        expected: [u64; 2],
    ) -> Result<NativeJobClient, NotAvailable> {
        if expected[1] == 0 {
            return Err(invalid("native expected identity missing").into());
        }
        let mut request = self.inner.retire_created_request();
        request.get().set_name(name.as_os_str().as_bytes());
        request.get().set_expected_device(expected[0]);
        request.get().set_expected_inode(expected[1]);
        let response = request.send().promise.await?;
        let out = response.get()?;
        crate::native_reply::check_payload(
            out.get_status()
                .map_err(|_| invalid("unknown native result status"))?,
            !out.has_job(),
        )?;
        Ok(NativeJobClient::from_raw(out.get_job()?))
    }
    pub async fn create_file(&self, name: &EntryName) -> Result<NativeJobClient, NotAvailable> {
        let mut req = self.inner.create_file_request();
        req.get().set_name(component(name)?);
        let reply = req.send().promise.await?;
        crate::native_reply::check_payload(
            reply
                .get()?
                .get_status()
                .map_err(|_| invalid("unknown native result status"))?,
            !reply.get()?.has_job(),
        )?;
        Ok(NativeJobClient::from_raw(reply.get()?.get_job()?))
    }
    /// # Errors
    /// Refuses missing owner store, queue admission, revoked scope or transport loss.
    pub async fn create_directory(
        &self,
        name: &EntryName,
    ) -> Result<NativeJobClient, NotAvailable> {
        let mut req = self.inner.create_directory_request();
        req.get().set_name(component(name)?);
        let reply = req.send().promise.await?;
        crate::native_reply::check_payload(
            reply
                .get()?
                .get_status()
                .map_err(|_| invalid("unknown native result status"))?,
            !reply.get()?.has_job(),
        )?;
        Ok(NativeJobClient::from_raw(reply.get()?.get_job()?))
    }

    /// # Errors
    /// Requires a genuine current scoped directory capability; no IDs grant access.
    pub async fn recoveries(
        &self,
        after: &[u8],
    ) -> Result<crate::directory_types::RecoveryPage, NotAvailable> {
        if !after.is_empty() && after.len() != 24 {
            return Err(invalid("Invalid recovery continuation").into());
        }
        let mut request = self.inner.recoveries_request();
        request.get().set_after(after);
        let reply = request.send().promise.await?;
        let out = reply.get()?;
        crate::native_reply::check_payload(
            out.get_status()
                .map_err(|_| invalid("Unknown native result status"))?,
            out.get_entries()?.is_empty() && out.get_next()?.is_empty() && !out.get_done(),
        )?;
        let rows = out.get_entries()?;
        let next = out.get_next()?;
        if rows.len() > 256
            || (!next.is_empty() && next.len() != 24)
            || out.get_done() != next.is_empty()
            || (!out.get_done() && (rows.is_empty() || next == after))
        {
            return Err(invalid("Invalid recovery page bounds").into());
        }
        let mut entries = Vec::with_capacity(rows.len() as usize);
        for row in rows {
            let recovery = row
                .get_recovery()?
                .try_into()
                .map_err(|_| invalid("Invalid recovery ID"))?;
            let operation = row
                .get_operation()?
                .try_into()
                .map_err(|_| invalid("Invalid operation ID"))?;
            if recovery == [0; 16] || operation == [0; 16] {
                return Err(invalid("Invalid native recovery identifiers").into());
            }
            let raw = row.get_original_name()?;
            if raw.len() > 4096 {
                return Err(invalid("Recovery name too large").into());
            }
            let original_name = EntryName::new(std::ffi::OsString::from_vec(raw.to_vec()))
                .map_err(|error| invalid(&error.to_string()))?;
            let kind = match row
                .get_kind()
                .map_err(|_| invalid("Unknown recovery kind"))?
            {
                fs_capnp::NativeRecoveryKind::NeedsRevalidation => {
                    crate::directory_types::RecoveryKind::NeedsRevalidation
                }
                fs_capnp::NativeRecoveryKind::KnownStaged => {
                    crate::directory_types::RecoveryKind::KnownStaged
                }
                fs_capnp::NativeRecoveryKind::UnknownNativeCommit => {
                    crate::directory_types::RecoveryKind::UnknownNativeCommit
                }
                fs_capnp::NativeRecoveryKind::PublishedSourceRetained => {
                    crate::directory_types::RecoveryKind::PublishedSourceRetained
                }
                fs_capnp::NativeRecoveryKind::QuarantinedNative => {
                    crate::directory_types::RecoveryKind::QuarantinedNative
                }
            };
            entries.push(crate::directory_types::RecoveryRow {
                recovery,
                operation,
                original_name,
                kind,
            });
        }
        Ok(crate::directory_types::RecoveryPage {
            entries,
            next: next.to_vec(),
            done: out.get_done(),
        })
    }
    /// # Errors
    /// Never retries a lost mutation reply. Parent and source scope remain genuine.
    pub async fn restore_captured(
        &self,
        recovery: [u8; 16],
        name: &EntryName,
    ) -> Result<NativeJobClient, NotAvailable> {
        if recovery == [0; 16] {
            return Err(invalid("Invalid recovery ID").into());
        }
        let mut request = self.inner.restore_captured_request();
        request.get().set_recovery(&recovery);
        request.get().set_name(component(name)?);
        let reply = request.send().promise.await?;
        crate::native_reply::check_payload(
            reply
                .get()?
                .get_status()
                .map_err(|_| invalid("Unknown native result status"))?,
            !reply.get()?.has_job(),
        )?;
        Ok(NativeJobClient::from_raw(reply.get()?.get_job()?))
    }
    /// Same-parent NOREPLACE only. Never an atomic-save or replacing-rename claim.
    /// # Errors
    /// Refuses native lookup/admission, revoked scope or transport loss.
    pub async fn atomic_save(
        &self,
        temporary: &EntryName,
        expected: [u64; 2],
        target: &EntryName,
    ) -> Result<(), NotAvailable> {
        if expected[1] == 0 {
            return Err(invalid("invalid AtomicSave temporary identity").into());
        }
        let mut request = self.inner.atomic_save_request();
        request.get().set_temporary(component(temporary)?);
        request.get().set_expected_device(expected[0]);
        request.get().set_expected_inode(expected[1]);
        request.get().set_target(component(target)?);
        let reply = request.send().promise.await?;
        crate::native_reply::check_payload(
            reply
                .get()?
                .get_status()
                .map_err(|_| invalid("unknown native result status"))?,
            true,
        )?;
        Ok(())
    }
    pub async fn rename_replace(
        &self,
        name: &EntryName,
        new_name: &EntryName,
    ) -> Result<NativeJobClient, NotAvailable> {
        let mut req = self.inner.rename_replace_request();
        req.get().set_name(component(name)?);
        req.get().set_new_name(component(new_name)?);
        let reply = req.send().promise.await?;
        crate::native_reply::check_payload(
            reply
                .get()?
                .get_status()
                .map_err(|_| invalid("unknown native result status"))?,
            !reply.get()?.has_job(),
        )?;
        Ok(NativeJobClient::from_raw(reply.get()?.get_job()?))
    }
    pub async fn rename_no_replace(
        &self,
        name: &EntryName,
        new_name: &EntryName,
    ) -> Result<NativeJobClient, NotAvailable> {
        let mut req = self.inner.rename_no_replace_request();
        req.get().set_name(component(name)?);
        req.get().set_new_name(component(new_name)?);
        let reply = req.send().promise.await?;
        crate::native_reply::check_payload(
            reply
                .get()?
                .get_status()
                .map_err(|_| invalid("unknown native result status"))?,
            !reply.get()?.has_job(),
        )?;
        Ok(NativeJobClient::from_raw(reply.get()?.get_job()?))
    }
}
/// Single custody of draining native job updates. IDs identify actual receipts;
/// fetching a report still requires the actual scoped `OperationOwner`.
pub struct NativeJobClient {
    inner: fs_capnp::native_job::Client,
}
impl NativeJobClient {
    /// # Errors
    /// Acquires only this actual remote job's held completed-creation capability.
    pub async fn created_temporary(&self) -> Result<CreatedTemporaryClient, NotAvailable> {
        let reply = self
            .inner
            .created_temporary_request()
            .send()
            .promise
            .await?;
        let out = reply.get()?;
        crate::native_reply::check_payload(
            out.get_status()
                .map_err(|_| invalid("unknown native result status"))?,
            !out.has_handle(),
        )?;
        let value = CreatedTemporaryClient {
            inner: out.get_handle()?,
        };
        value.check().await?;
        Ok(value)
    }

    /// # Errors
    /// Refuses absent genuine completed replacement custody or failed source scope.
    pub async fn pin_replacement(&self) -> Result<ReplacementPinClient, NotAvailable> {
        let reply = self.inner.pin_replacement_request().send().promise.await?;
        let out = reply.get()?;
        crate::native_reply::check_payload(
            out.get_status()
                .map_err(|_| invalid("unknown native result status"))?,
            !out.has_handle(),
        )?;
        let pin = ReplacementPinClient {
            inner: out.get_handle()?,
        };
        pin.check().await?;
        Ok(pin)
    }
    /// # Errors
    /// Refuses unavailable, revoked or disconnected native job scope.
    pub async fn check_scope(&self) -> Result<(), NotAvailable> {
        self.inner.check_scope_request().send().promise.await?;
        Ok(())
    }
    /// Wraps a received job capability, not an operation identifier or grant.
    pub fn from_raw(inner: fs_capnp::native_job::Client) -> Self {
        Self { inner }
    }
    pub async fn poll(&mut self) -> Result<JobPoll, NotAvailable> {
        let reply = self.inner.poll_request().send().promise.await?;
        let out = reply.get()?;
        crate::native_reply::check_payload(
            out.get_status()
                .map_err(|_| invalid("unknown native result status"))?,
            out.get_operation_id()?.is_empty() && out.get_updates()?.is_empty(),
        )?;
        let operation_id = out
            .get_operation_id()?
            .try_into()
            .map_err(|_| invalid("invalid operation identifier"))?;
        let rows = out.get_updates()?;
        if rows.len() > 1025 {
            return Err(invalid("job updates exceed bound").into());
        }
        let mut updates = Vec::with_capacity(rows.len() as usize);
        for row in rows {
            let update = match row
                .which()
                .map_err(|_| invalid("unknown native job update"))?
            {
                fs_capnp::native_job_update::Progress(progress) => decode_job_progress(progress?)?,
                fs_capnp::native_job_update::Item(item) => {
                    let i = item?;
                    let state = match i.get_state().map_err(|_| invalid("unknown item state"))? {
                        fs_capnp::NativeItemState::Completed => ItemState::Completed,
                        fs_capnp::NativeItemState::Failed => ItemState::Failed,
                        fs_capnp::NativeItemState::Cancelled => ItemState::Cancelled,
                        fs_capnp::NativeItemState::CopiedSourceRetained => {
                            ItemState::CopiedSourceRetained
                        }
                        fs_capnp::NativeItemState::RecoveryRequired => ItemState::RecoveryRequired,
                        fs_capnp::NativeItemState::OutcomeUnknown => ItemState::OutcomeUnknown,
                    };
                    let error = i
                        .get_error()?
                        .to_str()
                        .map_err(|_| invalid("invalid item error"))?;
                    let ids = i.get_recoveries()?;
                    if error.len() > 4096 || ids.len() > 256 {
                        return Err(invalid("item exceeds bound").into());
                    }
                    let mut recoveries = Vec::with_capacity(ids.len() as usize);
                    for id in ids {
                        recoveries.push(
                            id?.try_into()
                                .map_err(|_| invalid("invalid recovery identifier"))?,
                        );
                    }
                    JobUpdate::Item {
                        item: usize::try_from(i.get_item())
                            .map_err(|_| invalid("item overflow"))?,
                        state,
                        destination: if i.get_has_destination() {
                            Some(identity(
                                i.get_destination_device(),
                                i.get_destination_inode(),
                            )?)
                        } else {
                            None
                        },
                        error: if error.is_empty() {
                            None
                        } else {
                            Some(error.into())
                        },
                        recoveries,
                    }
                }
                fs_capnp::native_job_update::Finished(finished) => {
                    let f = finished?;
                    JobUpdate::Finished {
                        completed: usize::try_from(f.get_completed())
                            .map_err(|_| invalid("completed overflow"))?,
                        failed: usize::try_from(f.get_failed())
                            .map_err(|_| invalid("failed overflow"))?,
                        cancelled: usize::try_from(f.get_cancelled())
                            .map_err(|_| invalid("cancelled overflow"))?,
                    }
                }
            };
            updates.push(update);
        }
        Ok(JobPoll {
            operation_id,
            updates,
        })
    }
    /// Request canonical job cancellation. An in-flight native commit may already
    /// have happened; callers must consume its actual terminal outcome.
    pub async fn cancel(&self) -> Result<(), NotAvailable> {
        let reply = self.inner.cancel_request().send().promise.await?;
        crate::native_reply::check(
            reply
                .get()?
                .get_status()
                .map_err(|_| invalid("unknown native result status"))?,
        )?;
        Ok(())
    }
}

fn identity(device: u64, inode: u64) -> Result<[u64; 2], NotAvailable> {
    if inode == 0 {
        Err(invalid("invalid native identity").into())
    } else {
        Ok([device, inode])
    }
}

/// Metadata-only held regular inode; this type exposes no read/write methods.
#[derive(Clone)]
pub struct MetadataPinClient {
    inner: Rc<fs_capnp::metadata_pin::Client>,
}
impl MetadataPinClient {
    /// # Errors
    /// Refuses unavailable, revoked or disconnected metadata custody.
    pub async fn check_scope(&self) -> Result<(), NotAvailable> {
        self.inner.check_scope_request().send().promise.await?;
        Ok(())
    }
    /// # Errors
    /// Refuses revoked scope, invalid observation or transport failure.
    pub async fn metadata(&self) -> Result<DirectoryStat, NotAvailable> {
        let reply = self.inner.metadata_request().send().promise.await?;
        let out = reply.get()?;
        crate::native_reply::check_payload(
            out.get_status()
                .map_err(|_| invalid("unknown native result status"))?,
            out.get_device() == 0
                && out.get_inode() == 0
                && out.get_file_type() == 0
                && out.get_execute_bits() == 0
                && !out.has_metadata(),
        )?;
        let metadata = Metadata::from_reader(out.get_metadata()?);
        let file_type = native_type(out.get_file_type(), metadata.kind)?;
        if metadata.kind != crate::EntryKind::File {
            return Err(invalid("metadata pin must hold regular inode").into());
        }
        Ok(DirectoryStat {
            execute_bits: execute_bits(out.get_execute_bits())?,
            metadata,
            file_type,
            identity: identity(out.get_device(), out.get_inode())?,
        })
    }
}
fn native_type(raw: u32, kind: crate::EntryKind) -> Result<u32, capnp::Error> {
    use crate::{EntryKind, directory_types::mode_bits};
    let kinds = [
        (mode_bits(libc::S_IFREG), EntryKind::File),
        (mode_bits(libc::S_IFDIR), EntryKind::Directory),
        (mode_bits(libc::S_IFLNK), EntryKind::Symlink),
        (mode_bits(libc::S_IFIFO), EntryKind::Other),
        (mode_bits(libc::S_IFSOCK), EntryKind::Other),
        (mode_bits(libc::S_IFBLK), EntryKind::Other),
        (mode_bits(libc::S_IFCHR), EntryKind::Other),
    ];
    let expected = kinds
        .into_iter()
        .find_map(|(bits, kind)| (raw == bits).then_some(kind))
        .ok_or_else(|| invalid("invalid native file type"))?;
    if kind != expected {
        return Err(invalid("native file type disagrees with metadata"));
    }
    Ok(raw)
}

#[cfg(test)]
mod native_mode_tests {
    use super::native_type;
    use crate::{EntryKind, directory_types::mode_bits};

    #[test]
    fn mode_width_and_metadata_kind_preserve_wire_validation() {
        assert_eq!(mode_bits(0o170_000_u16), mode_bits(0o170_000_u32));
        for (bits, kind) in [
            (libc::S_IFREG, EntryKind::File),
            (libc::S_IFDIR, EntryKind::Directory),
            (libc::S_IFLNK, EntryKind::Symlink),
            (libc::S_IFIFO, EntryKind::Other),
            (libc::S_IFSOCK, EntryKind::Other),
            (libc::S_IFBLK, EntryKind::Other),
            (libc::S_IFCHR, EntryKind::Other),
        ] {
            let raw = mode_bits(bits);
            assert_eq!(native_type(raw, kind).unwrap(), raw);
            let wrong_kind = if kind == EntryKind::File {
                EntryKind::Directory
            } else {
                EntryKind::File
            };
            assert!(native_type(raw, wrong_kind).is_err());
        }
        assert!(native_type(0, EntryKind::Other).is_err());
        assert!(native_type(u32::MAX, EntryKind::Other).is_err());
    }
}

#[derive(Clone)]
pub struct DirectoryCursorClient {
    inner: Rc<fs_capnp::directory_cursor::Client>,
}
impl DirectoryCursorClient {
    /// # Errors
    /// Refuses sequence mismatch, native enumeration or transport/authority loss.
    pub async fn page(
        &self,
        expected: u64,
    ) -> Result<crate::directory_types::DirectoryStreamPage, NotAvailable> {
        let mut request = self.inner.page_request();
        request.get().set_expected_sequence(expected);
        let reply = request.send().promise.await?;
        let out = reply.get()?;
        crate::native_reply::check_payload(
            out.get_status()
                .map_err(|_| invalid("unknown native result status"))?,
            out.get_entries()?.is_empty() && out.get_next_sequence() == 0 && !out.get_done(),
        )?;
        if out.get_next_sequence()
            != expected
                .checked_add(1)
                .ok_or_else(|| invalid("cursor sequence exhausted"))?
        {
            return Err(invalid("invalid cursor sequence").into());
        }
        let rows = out.get_entries()?;
        if rows.len() > 128 {
            return Err(invalid("cursor page exceeds bound").into());
        }
        // An entire bounded batch may have vanished during live enumeration.
        // Exact sequence advancement still applies; only done denotes EOF.
        let mut entries = Vec::with_capacity(rows.len() as usize);
        for row in rows {
            let bytes = row.get_name()?;
            if bytes.len() > 4096 {
                return Err(invalid("component exceeds bound").into());
            }
            let name = EntryName::new(std::ffi::OsString::from_vec(bytes.to_vec()))
                .map_err(|e| invalid(&e.to_string()))?;
            let stat = match row
                .get_status()
                .map_err(|_| invalid("unknown native result status"))?
            {
                fs_capnp::NativeEntryStatus::Ok => {
                    let metadata = Metadata::from_reader(row.get_metadata()?);
                    Some(DirectoryStat {
                        execute_bits: execute_bits(row.get_execute_bits())?,
                        file_type: native_type(row.get_file_type(), metadata.kind)?,
                        identity: identity(row.get_device(), row.get_inode())?,
                        metadata,
                    })
                }
                fs_capnp::NativeEntryStatus::Refused => {
                    if row.get_device() != 0
                        || row.get_inode() != 0
                        || row.get_file_type() != 0
                        || row.get_execute_bits() != 0
                        || row.has_metadata()
                    {
                        return Err(invalid("refused row contains inode claim").into());
                    }
                    None
                }
            };
            entries.push(crate::directory_types::DirectoryStreamRow { name, stat });
        }
        Ok(crate::directory_types::DirectoryStreamPage {
            entries,
            next_sequence: out.get_next_sequence(),
            done: out.get_done(),
        })
    }
}

fn decode_filesystem_stats(
    raw: fs_capnp::directory_filesystem_stats::Reader<'_>,
) -> Result<crate::directory_types::DirectoryFilesystemStats, NotAvailable> {
    let stats = crate::directory_types::DirectoryFilesystemStats {
        blocks: raw.get_blocks(),
        blocks_free: raw.get_blocks_free(),
        blocks_available: raw.get_blocks_available(),
        files: raw.get_files(),
        files_free: raw.get_files_free(),
        files_available: raw.get_files_available(),
        block_size: raw
            .get_block_size()
            .try_into()
            .map_err(|_| invalid("filesystem block size overflow"))?,
        fragment_size: raw
            .get_fragment_size()
            .try_into()
            .map_err(|_| invalid("filesystem fragment size overflow"))?,
        name_max: raw
            .get_name_max()
            .try_into()
            .map_err(|_| invalid("filesystem name limit overflow"))?,
        read_only: raw.get_read_only(),
    };
    stats.validate().map_err(|e| invalid(&e))?;
    Ok(stats)
}

#[cfg(test)]
mod filesystem_stats_tests {
    use super::{decode_filesystem_stats, fs_capnp};
    #[test]
    fn actual_wire_decode_refuses_invalid_capacity_and_width_overflow() {
        for invalid_case in 0..8 {
            let mut message = capnp::message::Builder::new_default();
            let mut raw = message.init_root::<fs_capnp::directory_filesystem_stats::Builder>();
            raw.set_blocks(100);
            raw.set_blocks_free(50);
            raw.set_blocks_available(40);
            raw.set_files(80);
            raw.set_files_free(30);
            raw.set_files_available(20);
            raw.set_block_size(4096);
            raw.set_fragment_size(4096);
            raw.set_name_max(255);
            assert!(decode_filesystem_stats(raw.reborrow_as_reader()).is_ok());
            match invalid_case {
                0 => raw.set_blocks_free(101),
                1 => raw.set_blocks_available(51),
                2 => raw.set_files_free(81),
                3 => raw.set_files_available(31),
                4 => raw.set_block_size(0),
                5 => raw.set_fragment_size(u64::from(u32::MAX) + 1),
                6 => raw.set_name_max(u64::MAX),
                _ => raw.set_blocks(u64::MAX),
            }
            assert!(decode_filesystem_stats(raw.into_reader()).is_err());
        }
    }
}

// Observations cannot add special modes or any read/write/authority bit.
fn execute_bits(raw: u16) -> Result<u16, capnp::Error> {
    if raw & !0o111 != 0 {
        return Err(invalid("invalid original execute mode bits"));
    }
    Ok(raw)
}
#[cfg(test)]
mod execute_mode_codec_tests {
    use super::{execute_bits, fs_capnp};
    #[test]
    fn actual_wire_execute_bits_reject_read_write_and_special_modes() {
        for value in [0, 1, 0o10, 0o100, 0o111] {
            let mut message = capnp::message::Builder::new_default();
            let mut out = message.init_root::<fs_capnp::native_stream_entry::Builder>();
            out.set_execute_bits(value);
            assert_eq!(
                execute_bits(out.into_reader().get_execute_bits()).unwrap(),
                value
            );
        }
        for value in [0o2, 0o200, 0o400, 0o777, 0o1000, 0o4000, u16::MAX] {
            let mut message = capnp::message::Builder::new_default();
            let mut out = message.init_root::<fs_capnp::native_stream_entry::Builder>();
            out.set_execute_bits(value);
            assert!(execute_bits(out.into_reader().get_execute_bits()).is_err());
        }
    }
}

#[cfg(test)]
mod absent_stat_rpc_tests {
    use super::{DirectoryClient, EntryName, Rc, fs_capnp};
    use capnp_rpc::{RpcSystem, rpc_twoparty_capnp::Side, twoparty::VatNetwork};
    use std::os::unix::net::UnixStream;
    use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

    // Deliberately malformed remote observation, never a native grant fixture.
    struct AbsentClaim(u8);
    impl fs_capnp::directory_handle::Server for AbsentClaim {
        async fn stat(
            self: Rc<Self>,
            _: fs_capnp::directory_handle::StatParams,
            mut results: fs_capnp::directory_handle::StatResults,
        ) -> Result<(), capnp::Error> {
            let mut out = results.get();
            out.set_status(fs_capnp::NativeEntryStatus::Ok);
            out.set_present(false);
            match self.0 {
                0 => {}
                1 => out.set_execute_bits(0o111),
                2 => out.set_device(3),
                3 => out.set_inode(4),
                4 => out.set_file_type(crate::directory_types::mode_bits(libc::S_IFREG)),
                5 => {
                    out.init_metadata();
                }
                _ => unreachable!(),
            }
            Ok(())
        }
    }

    #[test]
    fn connected_absent_stat_refuses_every_nonempty_claim() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        tokio::task::LocalSet::new().block_on(&runtime, async {
            tokio::time::timeout(tilo_test_env::hang_bound(), async {
                for claim in 0..6 {
                    let (server, client) = UnixStream::pair().unwrap();
                    server.set_nonblocking(true).unwrap();
                    client.set_nonblocking(true).unwrap();
                    let server = tokio::net::UnixStream::from_std(server).unwrap();
                    let client = tokio::net::UnixStream::from_std(client).unwrap();
                    let (read, write) = server.into_split();
                    let network = VatNetwork::new(
                        read.compat(),
                        write.compat_write(),
                        Side::Server,
                        capnp::message::ReaderOptions::default(),
                    );
                    let bootstrap: fs_capnp::directory_handle::Client =
                        capnp_rpc::new_client(AbsentClaim(claim));
                    let server_task = tokio::task::spawn_local(RpcSystem::new(
                        Box::new(network),
                        Some(bootstrap.client),
                    ));
                    let (read, write) = client.into_split();
                    let network = VatNetwork::new(
                        read.compat(),
                        write.compat_write(),
                        Side::Client,
                        capnp::message::ReaderOptions::default(),
                    );
                    let mut rpc = RpcSystem::new(Box::new(network), None);
                    let raw = rpc.bootstrap(Side::Server);
                    let client_task = tokio::task::spawn_local(rpc);
                    let directory = DirectoryClient::from_raw(raw);
                    let result = directory
                        .stat(&EntryName::new("missing".into()).unwrap())
                        .await;
                    if claim == 0 {
                        assert!(result.unwrap().is_none());
                    } else {
                        assert!(result.is_err(), "accepted absent claim {claim}");
                    }
                    drop(directory);
                    // Intentional exact-driver cancellation after observing the reply;
                    // join both tasks so no RPC driver survives this fixture.
                    client_task.abort();
                    server_task.abort();
                    assert!(client_task.await.unwrap_err().is_cancelled());
                    assert!(server_task.await.unwrap_err().is_cancelled());
                }
            })
            .await
            .expect("connected absent-stat fixture exceeded total deadline");
        });
    }
}

/// Holds actual remote capture custody; observations are not authority constructors.
pub struct ReplacementPinClient {
    inner: fs_capnp::replacement_pin::Client,
}
impl ReplacementPinClient {
    pub async fn commit_with_history(&self, receipt: &[u8]) -> Result<(), NotAvailable> {
        if receipt.is_empty() || receipt.len() > crate::external_history::LIMIT {
            return Err(invalid("Invalid certified commit receipt length").into());
        }
        let mut request = self.inner.commit_with_history_request();
        request.get().set_receipt(receipt);
        request.send().promise.await?;
        Ok(())
    }

    /// # Errors
    /// Refuses dead source scope or changed native inode custody.
    pub async fn check(&self) -> Result<(), NotAvailable> {
        self.inner.check_request().send().promise.await?;
        Ok(())
    }
    /// # Errors
    /// Rejects malformed identifiers/identities or revoked native capture custody.
    pub async fn observe(
        &self,
    ) -> Result<crate::directory_types::ReplacementObservation, NotAvailable> {
        let reply = self.inner.observe_request().send().promise.await?;
        let out = reply.get()?.get_observation()?;
        decode_replacement_observation(out)
    }
}

fn decode_replacement_observation(
    out: fs_capnp::replacement_observation::Reader<'_>,
) -> Result<crate::directory_types::ReplacementObservation, NotAvailable> {
    let operation: [u8; 16] = out
        .get_operation()?
        .try_into()
        .map_err(|_| invalid("invalid native operation ID"))?;
    let capture: [u8; 16] = out
        .get_capture()?
        .try_into()
        .map_err(|_| invalid("invalid native capture ID"))?;
    let source_owner: [u8; 32] = out
        .get_source_owner()?
        .try_into()
        .map_err(|_| invalid("invalid native source owner"))?;
    let source = identity(out.get_source_device(), out.get_source_inode())?;
    let victim = identity(out.get_victim_device(), out.get_victim_inode())?;
    let destination = identity(out.get_destination_device(), out.get_destination_inode())?;
    if operation == [0; 16]
        || capture == [0; 16]
        || source_owner == [0; 32]
        || destination != source
        || source == victim
        || source[0] != victim[0]
    {
        return Err(invalid("invalid replacement custody observation").into());
    }
    Ok(crate::directory_types::ReplacementObservation {
        operation,
        source_owner,
        capture,
        source,
        victim,
        destination,
    })
}

#[cfg(test)]
mod replacement_observation_tests {
    use super::{decode_replacement_observation, fs_capnp};
    #[test]
    fn actual_wire_observation_rejects_malformed_ids_and_inode_bindings() {
        // Codec observations alone grant no rights; native positive authority is
        // exercised by the separate real completed-EXCHANGE connected fixture.
        for case in 0..9 {
            let mut message = capnp::message::Builder::new_default();
            let mut out = message.init_root::<fs_capnp::replacement_observation::Builder>();
            out.set_operation(&[1; 16]);
            out.set_capture(&[2; 16]);
            out.set_source_owner(&[3; 32]);
            out.set_source_device(4);
            out.set_source_inode(5);
            out.set_victim_device(4);
            out.set_victim_inode(6);
            out.set_destination_device(4);
            out.set_destination_inode(5);
            match case {
                0 => out.set_operation(&[1; 15]),
                1 => out.set_operation(&[0; 16]),
                2 => out.set_capture(&[0; 16]),
                3 => out.set_source_owner(&[3; 31]),
                4 => out.set_source_owner(&[0; 32]),
                5 => out.set_victim_inode(0),
                6 => out.set_destination_inode(7),
                7 => out.set_victim_inode(5),
                _ => out.set_victim_device(8),
            }
            assert!(decode_replacement_observation(out.into_reader()).is_err());
        }
    }
}

/// Actual remote zero-effect reservation; it cannot be constructed from IDs.
pub struct PendingReplacementClient {
    inner: fs_capnp::pending_replacement::Client,
}
impl PendingReplacementClient {
    pub async fn acknowledged_committed_pin(&self) -> Result<ReplacementPinClient, NotAvailable> {
        let reply = self
            .inner
            .acknowledged_committed_pin_request()
            .send()
            .promise
            .await?;
        let out = reply.get()?;
        crate::native_reply::check_payload(
            out.get_status()
                .map_err(|_| invalid("unknown native result status"))?,
            !out.has_handle() && !out.has_job(),
        )?;
        if !out.has_handle() || !out.has_job() {
            return Err(invalid("Incomplete committed source acknowledgment").into());
        }
        let pin = ReplacementPinClient {
            inner: out.get_handle()?,
        };
        pin.check().await?;
        pin.observe().await?;
        Ok(pin)
    }
    pub async fn acknowledged_committed_job(&self) -> Result<NativeJobClient, NotAvailable> {
        let reply = self
            .inner
            .acknowledged_committed_pin_request()
            .send()
            .promise
            .await?;
        let out = reply.get()?;
        crate::native_reply::check_payload(
            out.get_status()
                .map_err(|_| invalid("unknown native result status"))?,
            !out.has_handle() && !out.has_job(),
        )?;
        if !out.has_handle() || !out.has_job() {
            return Err(invalid("Incomplete committed source acknowledgment").into());
        }
        let pin = ReplacementPinClient {
            inner: out.get_handle()?,
        };
        pin.check().await?;
        pin.observe().await?;
        let job = NativeJobClient::from_raw(out.get_job()?);
        job.check_scope().await?;
        Ok(job)
    }
    pub async fn arm_daemon_handoff(&self, challenge: &[u8; 32]) -> Result<[u8; 16], NotAvailable> {
        if *challenge == [0; 32] {
            return Err(invalid("empty handoff challenge").into());
        }
        let mut request = self.inner.arm_daemon_handoff_request();
        request.get().set_challenge(challenge);
        let reply = request.send().promise.await?;
        let ticket: [u8; 16] = reply
            .get()?
            .get_ticket()?
            .try_into()
            .map_err(|_| invalid("invalid handoff ticket"))?;
        if ticket == [0; 16] {
            return Err(invalid("empty handoff ticket").into());
        }
        Ok(ticket)
    }

    /// Wrap an actual received capability after its owner independently checks
    /// the source kernel peer and sealed expectation on the fresh owned stream.
    /// This transport wrapper itself grants no execution or source authority.
    pub fn from_authenticated_source_capability(
        inner: fs_capnp::pending_replacement::Client,
    ) -> Self {
        Self { inner }
    }

    /// # Errors
    /// Refuses retired or consumed genuine prepared work.
    /// # Errors
    /// Unknown on lost reply; no capability release may be inferred from EOF.
    pub async fn retire_daemon_handoff(&self, ticket: [u8; 16]) -> Result<(), NotAvailable> {
        let mut request = self.inner.retire_daemon_handoff_request();
        request.get().set_ticket(&ticket);
        request.send().promise.await?;
        Ok(())
    }
    pub async fn check_scope(&self) -> Result<(), NotAvailable> {
        self.inner.check_scope_request().send().promise.await?;
        Ok(())
    }
    /// # Errors
    /// Refuses revoked work or malformed observational binding.
    pub async fn observe(
        &self,
    ) -> Result<crate::directory_types::PendingReplacementObservation, NotAvailable> {
        let reply = self.inner.observe_request().send().promise.await?;
        decode_pending_replacement(reply.get()?.get_observation()?)
    }
    /// # Errors
    /// A lost reply is unknown, never automatically retried. No raw start exists.
    pub async fn start_with_history(
        &self,
        receipt: &[u8],
    ) -> Result<NativeJobClient, NotAvailable> {
        if receipt.is_empty() || receipt.len() > crate::external_history::LIMIT {
            return Err(invalid("Invalid certified prepare receipt length").into());
        }
        let mut request = self.inner.start_with_history_request();
        request.get().set_receipt(receipt);
        let reply = request.send().promise.await?;
        let out = reply.get()?;
        crate::native_reply::check_payload(
            out.get_status()
                .map_err(|_| invalid("unknown native result status"))?,
            !out.has_job(),
        )?;
        Ok(NativeJobClient::from_raw(out.get_job()?))
    }
    /// # Errors
    /// Releases unsubmitted work only, never rolls back effects.
    pub async fn cancel(&self) -> Result<(), NotAvailable> {
        self.inner.cancel_request().send().promise.await?;
        Ok(())
    }
}
fn decode_pending_replacement(
    out: fs_capnp::pending_replacement_observation::Reader<'_>,
) -> Result<crate::directory_types::PendingReplacementObservation, NotAvailable> {
    let description = out.get_reservation_description()?;
    if description.is_empty() || description.len() > 2 * 1024 * 1024 {
        return Err(invalid("invalid native intent description length").into());
    }
    let value = crate::directory_types::PendingReplacementObservation {
        purpose: match out
            .get_purpose()
            .map_err(|_| invalid("unknown native replacement purpose"))?
        {
            fs_capnp::ReplacementPurpose::ReplacementRename => {
                crate::directory_types::ReplacementPurpose::ReplacementRename
            }
            fs_capnp::ReplacementPurpose::AtomicSave => {
                crate::directory_types::ReplacementPurpose::AtomicSave
            }
        },
        operation: out
            .get_operation()?
            .try_into()
            .map_err(|_| invalid("invalid native operation ID"))?,
        source_owner: out
            .get_source_owner()?
            .try_into()
            .map_err(|_| invalid("invalid native source owner"))?,
        source: identity(out.get_source_device(), out.get_source_inode())?,
        expected_victim: identity(out.get_victim_device(), out.get_victim_inode())?,
        intent_digest: out
            .get_intent_digest()?
            .try_into()
            .map_err(|_| invalid("invalid native intent digest"))?,
        reservation_description: description.to_vec(),
        archive_plan_digest: out
            .get_archive_plan_digest()?
            .try_into()
            .map_err(|_| invalid("invalid protected archive plan digest"))?,
    };
    value.validate().map_err(|error| invalid(&error))?;
    Ok(value)
}

#[cfg(test)]
mod pending_replacement_codec_tests {
    use super::{decode_pending_replacement, fs_capnp};
    #[test]
    fn actual_wire_pending_observation_rejects_malformed_intent_and_archive_plan() {
        for case in 0..13 {
            let mut message = capnp::message::Builder::new_default();
            let mut out = message.init_root::<fs_capnp::pending_replacement_observation::Builder>();
            out.set_operation(&[1; 16]);
            out.set_source_owner(&[2; 32]);
            out.set_source_device(3);
            out.set_source_inode(4);
            out.set_victim_device(3);
            out.set_victim_inode(5);
            let description = b"codec observation only";
            let mut digest = blake3::Hasher::new();
            digest.update(b"tilo-native-retained-replacement-intent-v2\0");
            digest.update(description);
            out.set_intent_digest(digest.finalize().as_bytes());
            out.set_reservation_description(description);
            out.set_archive_plan_digest(&[7; 32]);
            // Pure bounded codec observation, not a positive authority fixture.
            match case {
                0 => out.set_operation(&[1; 15]),
                1 => out.set_operation(&[0; 16]),
                2 => out.set_source_owner(&[0; 32]),
                3 => out.set_source_inode(0),
                4 => out.set_victim_inode(4),
                5 => out.set_victim_device(8),
                6 => out.set_intent_digest(&[6; 31]),
                7 => out.set_intent_digest(&[0; 32]),
                8 => out.set_archive_plan_digest(&[7; 33]),
                9 => out.set_archive_plan_digest(&[0; 32]),
                10 => out.set_reservation_description(&[]),
                11 => out.set_reservation_description(b"changed native intent description"),
                _ => out.set_reservation_description(&vec![0; 2 * 1024 * 1024 + 1]),
            }
            assert!(decode_pending_replacement(out.into_reader()).is_err());
        }
    }
}

/// Holds an actual remote native creation capability; metadata cannot mint it.
pub struct CreatedTemporaryClient {
    inner: fs_capnp::created_temporary::Client,
}
impl CreatedTemporaryClient {
    /// Wraps received creator custody; observations cannot construct this capability.
    pub fn from_authenticated_source_capability(
        inner: fs_capnp::created_temporary::Client,
    ) -> Self {
        Self { inner }
    }

    pub async fn set_permissions(&self, mode: u32) -> Result<(), NotAvailable> {
        if mode & !0o7777 != 0 {
            return Err(invalid("invalid own-created mode").into());
        }
        let mut request = self.inner.set_permissions_request();
        request.get().set_mode(mode);
        request.send().promise.await?;
        Ok(())
    }

    pub async fn prepare_atomic_save(
        &self,
    ) -> crate::directory_types::AtomicSavePreparation<PendingReplacementClient> {
        use crate::directory_types::AtomicSavePreparation as Outcome;
        let result: Result<Option<PendingReplacementClient>, NotAvailable> = async {
            let response = self
                .inner
                .prepare_atomic_save_request()
                .send()
                .promise
                .await?;
            let value = response.get()?;
            let status = value
                .get_status()
                .map_err(|_| invalid("unknown AtomicSave preparation status"))?;
            match status {
                fs_capnp::NativeEntryStatus::Refused if !value.has_pending() => Ok(None),
                fs_capnp::NativeEntryStatus::Ok if value.has_pending() => {
                    Ok(Some(PendingReplacementClient {
                        inner: value.get_pending()?,
                    }))
                }
                _ => Err(invalid("malformed AtomicSave preparation reply").into()),
            }
        }
        .await;
        // Do not postcheck and drop a returned source capability. Its recipient
        // must store it before performing any fallible observation or scope call.
        match result {
            Ok(Some(pending)) => Outcome::Prepared(pending),
            Ok(None) => Outcome::Refused("Source acknowledged no AtomicSave reservation".into()),
            Err(error) => Outcome::Unknown(error.to_string()),
        }
    }

    pub async fn prepare_replace(&self) -> Result<PendingReplacementClient, NotAvailable> {
        let response = self.inner.prepare_replace_request().send().promise.await?;
        let value = response.get()?;
        crate::native_reply::check_payload(
            value
                .get_status()
                .map_err(|_| invalid("unknown native result status"))?,
            !value.has_pending(),
        )?;
        let pending = PendingReplacementClient {
            inner: value.get_pending()?,
        };
        pending.check_scope().await?;
        Ok(pending)
    }
    /// # Errors
    /// Refuses changed original temp/source scope or failed native custody.
    pub async fn check(&self) -> Result<(), NotAvailable> {
        self.inner.check_scope_request().send().promise.await?;
        Ok(())
    }
    /// # Errors
    /// One-use submission only. Lost reply has unknown effects; never retry it.
    pub async fn retire(&self) -> Result<NativeJobClient, NotAvailable> {
        let reply = self.inner.retire_request().send().promise.await?;
        let out = reply.get()?;
        crate::native_reply::check_payload(
            out.get_status()
                .map_err(|_| invalid("unknown native result status"))?,
            !out.has_job(),
        )?;
        Ok(NativeJobClient::from_raw(out.get_job()?))
    }
}

/// Aliases of an actual source capability; no observation-to-capability constructor.
#[derive(Clone)]
pub struct HistoryReadSubjectClient {
    inner: fs_capnp::history_read_subject::Client,
}
impl HistoryReadSubjectClient {
    /// # Errors
    /// Lost reply remains unknown; worker init lifetime is unchanged.
    pub async fn retire_daemon_handoff(&self, ticket: [u8; 16]) -> Result<(), NotAvailable> {
        let mut request = self.inner.retire_daemon_handoff_request();
        request.get().set_ticket(&ticket);
        request.send().promise.await?;
        Ok(())
    }

    /// Wrap only an actual source capability. Independent daemon admission and
    /// kernel source authentication remain required; wrapping grants neither.
    pub fn from_authenticated_source_capability(
        inner: fs_capnp::history_read_subject::Client,
    ) -> Self {
        Self { inner }
    }
    /// # Errors
    /// Refuses absent controller route or malformed challenge/ticket.
    pub async fn arm_daemon_handoff(&self, challenge: [u8; 32]) -> Result<[u8; 16], NotAvailable> {
        if challenge == [0; 32] {
            return Err(invalid("empty history Read challenge").into());
        }
        let mut request = self.inner.arm_daemon_handoff_request();
        request.get().set_challenge(&challenge);
        let reply = request.send().promise.await?;
        let out = reply.get()?;
        crate::native_reply::check_payload(
            out.get_status()
                .map_err(|_| invalid("unknown history Read handoff status"))?,
            !out.has_ticket(),
        )?;
        let ticket: [u8; 16] = reply
            .get()?
            .get_ticket()?
            .try_into()
            .map_err(|_| invalid("history Read ticket length"))?;
        if ticket == [0; 16] {
            return Err(invalid("empty history Read ticket").into());
        }
        Ok(ticket)
    }

    /// # Errors
    /// Refuses expired original Read/source scope or transport failure.
    pub async fn check_scope(&self) -> Result<(), NotAvailable> {
        self.inner.check_scope_request().send().promise.await?;
        Ok(())
    }
    /// # Errors
    /// Refuses named substitution, source revocation or transport failure.
    pub async fn check(&self) -> Result<(), NotAvailable> {
        self.inner.check_request().send().promise.await?;
        Ok(())
    }
    /// # Errors
    /// Refuses revoked source custody or malformed bounded observations.
    pub async fn observe(
        &self,
    ) -> Result<crate::directory_types::HistoryReadObservation, NotAvailable> {
        let reply = self.inner.observe_request().send().promise.await?;
        let out = reply.get()?;
        if !out.has_observation() {
            return Err(invalid("missing history Read observation").into());
        }
        let raw = out.get_observation()?;
        let owner = raw.get_source_owner()?;
        let source_owner: [u8; 32] = owner
            .try_into()
            .map_err(|_| invalid("history Read owner length"))?;
        let rows = raw.get_parents()?;
        if rows.len() > 64 {
            return Err(invalid("history Read lineage exceeds bound").into());
        }
        let mut parents = Vec::with_capacity(rows.len() as usize);
        let mut total = 0usize;
        for part in rows {
            let part = part?;
            if part.len() > 255 {
                return Err(invalid("history Read component exceeds bound").into());
            }
            total = total
                .checked_add(part.len())
                .ok_or_else(|| invalid("history Read overflow"))?;
            if total > 4096 {
                return Err(invalid("history Read lineage exceeds bound").into());
            }
            parents.push(part.to_vec());
        }
        let name = raw.get_name()?;
        if name.len() > 255 {
            return Err(invalid("history Read name exceeds bound").into());
        }
        let observation = crate::directory_types::HistoryReadObservation {
            source_owner,
            root: [raw.get_root_device(), raw.get_root_inode()],
            parent: [raw.get_parent_device(), raw.get_parent_inode()],
            parents,
            name: name.to_vec(),
            identity: [raw.get_device(), raw.get_inode()],
        };
        observation.validate().map_err(|error| invalid(&error))?;
        Ok(observation)
    }
}

#[cfg(test)]
mod history_read_codec_tests {
    use super::{HistoryReadSubjectClient, Rc, fs_capnp};
    use capnp::capability::Promise;
    use capnp_rpc::{RpcSystem, rpc_twoparty_capnp, twoparty};
    use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
    struct MalformedRead;
    #[allow(refining_impl_trait, reason = "capnp server returns concrete Promise")]
    impl fs_capnp::history_read_subject::Server for MalformedRead {
        fn arm_daemon_handoff(
            self: Rc<Self>,
            _: fs_capnp::history_read_subject::ArmDaemonHandoffParams,
            mut results: fs_capnp::history_read_subject::ArmDaemonHandoffResults,
        ) -> Promise<(), capnp::Error> {
            results
                .get()
                .set_status(fs_capnp::NativeEntryStatus::Refused);
            results.get().set_ticket(&[4; 16]);
            Promise::ok(())
        }

        fn observe(
            self: Rc<Self>,
            _: fs_capnp::history_read_subject::ObserveParams,
            mut results: fs_capnp::history_read_subject::ObserveResults,
        ) -> Promise<(), capnp::Error> {
            let mut out = results.get().init_observation();
            out.set_source_owner(&[1; 32]);
            out.set_root_inode(1);
            out.set_parent_inode(2);
            out.set_inode(3);
            out.set_name(b"document");
            let mut parents = out.init_parents(65);
            for i in 0..65 {
                parents.set(i, b"parent");
            }
            Promise::ok(())
        }
    }
    #[test]
    fn connected_history_read_rejects_oversized_source_observation() {
        let runtime = tokio::runtime::Builder::new_current_thread()
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
                let raw: fs_capnp::history_read_subject::Client =
                    capnp_rpc::new_client(MalformedRead);
                let (read, write) = a.into_split();
                let network = twoparty::VatNetwork::new(
                    read.compat(),
                    write.compat_write(),
                    rpc_twoparty_capnp::Side::Server,
                    capnp::message::ReaderOptions::default(),
                );
                let server =
                    tokio::task::spawn_local(RpcSystem::new(Box::new(network), Some(raw.client)));
                let (read, write) = b.into_split();
                let network = twoparty::VatNetwork::new(
                    read.compat(),
                    write.compat_write(),
                    rpc_twoparty_capnp::Side::Client,
                    capnp::message::ReaderOptions::default(),
                );
                let mut rpc = RpcSystem::new(Box::new(network), None);
                let raw = rpc.bootstrap(rpc_twoparty_capnp::Side::Server);
                let stop = rpc.get_disconnector();
                let client_task = tokio::task::spawn_local(rpc);
                let client = HistoryReadSubjectClient { inner: raw };
                assert!(client.observe().await.is_err());
                assert!(
                    client.arm_daemon_handoff([7; 32]).await.is_err(),
                    "refused reply cannot smuggle a ticket"
                );
                stop.await.unwrap();
                client_task.await.unwrap().unwrap();
                let _ = server.await.unwrap();
            })
            .await
            .expect("malformed connected reply must fail within bound");
        });
    }
}

fn decode_job_progress(
    p: fs_capnp::native_job_progress::Reader<'_>,
) -> Result<JobUpdate, NotAvailable> {
    let phase = p
        .get_phase()?
        .to_str()
        .map_err(|_| invalid("invalid phase"))?;
    if phase.len() > 4096 {
        return Err(invalid("phase exceeds bound").into());
    }
    Ok(JobUpdate::Progress {
        sequence: p.get_sequence(),
        item: usize::try_from(p.get_item()).map_err(|_| invalid("item overflow"))?,
        phase: phase.into(),
        bytes: p.get_bytes(),
    })
}
