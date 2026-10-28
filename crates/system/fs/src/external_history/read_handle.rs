//! Actual source Read custody and independently certified Reader proof. The
//! adopted handle has no dependency on the completed editor's proof or source.
use super::{
    AsRawFd, AtomicBool, ExternalSourceAdmission, File, Instant, MetadataExt, Read, Seek,
    UnixStream, Write, io, nonce, process_binding, refused,
};
use super::{lane_controller::AcknowledgedRegistry, pending_source::within};
use capnp_rpc::{RpcSystem, rpc_twoparty_capnp::Side, twoparty::VatNetwork};
use fs_protocol::{
    FileBindingV1, FileVersionId,
    files::{
        FileHistoryReadErrorV1 as Error, FileHistoryReadQueryV1 as Query,
        FileHistoryReadResponseV1, FileHistoryReadScopeV1 as Scope,
    },
};
use futures_util::future::LocalBoxFuture;
use runtime_fs::{
    directory_backend::HistoryReadObservation, directory_client::HistoryReadSubjectClient,
};
use runtime_portal::retained_role::NativeHistoryReadHandoff;
use std::{cell::RefCell, rc::Rc};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

pub(super) struct ReadLineage {
    pub(super) observation: HistoryReadObservation,
    pub(super) binding: FileBindingV1,
    pub(super) version: FileVersionId,
    pub(super) person: [u8; 32],
    pub(super) room: String,
}
pub(super) struct ReadDescriptors {
    pub(super) sealed: File,
    pub(super) stream: File,
    pub(super) program: File,
}
struct ReadSource {
    admission: Rc<RefCell<ExternalSourceAdmission>>,
    subject: HistoryReadSubjectClient,
    peer: process_binding::ExecutingPeer,
    program: File,
    expected: NativeHistoryReadHandoff,
    observation: HistoryReadObservation,
    driver: Option<tokio::task::JoinHandle<Result<(), capnp::Error>>>,
}
impl Drop for ReadSource {
    fn drop(&mut self) {
        if let Some(driver) = &self.driver {
            driver.abort();
        }
    }
}
fn expectation(mut file: File) -> io::Result<NativeHistoryReadHandoff> {
    let m = file.metadata()?;
    let seals = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GET_SEALS) };
    let required = libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;
    if !m.is_file()
        || m.uid() != 0
        || m.nlink() != 0
        || m.len() == 0
        || m.len() > 4096
        || seals < 0
        || seals & required != required
    {
        return Err(refused());
    }
    file.rewind()?;
    let mut bytes = Vec::new();
    file.take(4097).read_to_end(&mut bytes)?;
    NativeHistoryReadHandoff::decode(&bytes)
}
impl ReadSource {
    fn receive<'a>(
        admission: Rc<RefCell<ExternalSourceAdmission>>,
        descriptors: ReadDescriptors,
        request_id: &'a str,
        cancel: &'a AtomicBool,
        deadline: Instant,
    ) -> LocalBoxFuture<'a, io::Result<Self>> {
        Box::pin(async move {
            ExternalSourceAdmission::refresh_shared(&admission, cancel, deadline).await?;
            let expected = expectation(descriptors.sealed)?;
            {
                let a = admission.try_borrow().map_err(|_| refused())?;
                if !a.purpose.history_reader()
                    || expected.read_request_id != request_id
                    || expected.enrollment_context_digest != a.context.digest()?
                    || expected.source_owner != a.context.verifier.source_owner
                {
                    return Err(refused());
                }
            }
            let program = descriptors.program;
            let meta = program.metadata()?;
            if !meta.is_file() || [meta.dev(), meta.ino()] != expected.worker_program {
                return Err(refused());
            }
            let stream = UnixStream::from(std::os::fd::OwnedFd::from(descriptors.stream));
            let expectation = expected.clone();
            let (stream, result) =
                super::owned_io::exchange(stream, cancel, deadline, move |stream| {
                    let expected = expectation;

                    stream.set_nonblocking(false)?;
                    stream.write_all(b"RHR1")?;
                    stream.write_all(&expected.ticket)?;
                    stream.write_all(&expected.challenge)?;
                    process_binding::arm(stream)?;
                    let challenge = nonce();
                    stream.write_all(&process_binding::MAGIC)?;
                    stream.write_all(&challenge)?;
                    let peer = process_binding::receive(stream, &challenge, expected.worker_uid)?;
                    if !peer.current()
                        || peer.pid() != expected.worker_pid
                        || peer.gid() != expected.worker_gid
                        || peer.start_time()? != expected.worker_start
                    {
                        return Err(refused());
                    }
                    stream.write_all(&[1])?;
                    Ok(peer)
                })
                .await?;
            let peer = result?;

            stream.set_nonblocking(true)?;
            let stream = tokio::net::UnixStream::from_std(stream)?;
            let (reader, writer) = stream.into_split();
            let network = VatNetwork::new(
                reader.compat(),
                writer.compat_write(),
                Side::Client,
                capnp::message::ReaderOptions::default(),
            );
            let mut rpc = RpcSystem::new(Box::new(network), None);
            let raw: runtime_fs::fs_capnp::history_read_subject::Client =
                rpc.bootstrap(Side::Server);
            let subject = HistoryReadSubjectClient::from_authenticated_source_capability(raw);
            let driver = tokio::task::spawn_local(rpc);
            let observation = match within(cancel, deadline, async {
                subject.check_scope().await.map_err(io::Error::other)?;
                subject.check().await.map_err(io::Error::other)?;
                let observation = subject.observe().await.map_err(io::Error::other)?;
                subject.check().await.map_err(io::Error::other)?;
                Ok(observation)
            })
            .await
            {
                Ok(value) => value,
                Err(error) => {
                    driver.abort();
                    let _ = driver.await;
                    return Err(error);
                }
            };
            if observation.source_owner != expected.source_owner {
                driver.abort();
                let _ = driver.await;
                return Err(refused());
            }
            let mut source = Self {
                admission,
                subject,
                peer,
                program,
                expected,
                observation,
                driver: Some(driver),
            };
            if let Err(error) = source.check(cancel, deadline).await {
                source.stop_and_wait().await?;
                return Err(error);
            }
            Ok(source)
        })
    }
    fn stop_and_wait(&mut self) -> LocalBoxFuture<'_, io::Result<()>> {
        Box::pin(async move {
            if let Some(driver) = self.driver.take() {
                driver.abort();
                match driver.await {
                    Ok(_) => {}
                    Err(error) if error.is_cancelled() => {}
                    Err(error) => return Err(io::Error::other(error)),
                }
            }
            Ok(())
        })
    }
    fn current(&self) -> io::Result<()> {
        let a = self.admission.try_borrow().map_err(|_| refused())?;
        a.factory.check_current()?;
        a.helper.check_current()?;
        if !a.purpose.history_reader()
            || a.revoked
            || !a.proof.shell.current()
            || !self.peer.current()
            || self
                .driver
                .as_ref()
                .is_none_or(tokio::task::JoinHandle::is_finished)
            || self.peer.pid() != self.expected.worker_pid
            || self.peer.uid() != self.expected.worker_uid
            || self.peer.gid() != self.expected.worker_gid
            || self.peer.start_time()? != self.expected.worker_start
        {
            return Err(refused());
        }
        let m = self.program.metadata()?;
        if [m.dev(), m.ino()] != self.expected.worker_program {
            return Err(refused());
        }
        Ok(())
    }
    fn check<'a>(
        &'a self,
        cancel: &'a AtomicBool,
        deadline: Instant,
    ) -> LocalBoxFuture<'a, io::Result<()>> {
        Box::pin(async move {
            ExternalSourceAdmission::refresh_shared(&self.admission, cancel, deadline).await?;
            self.current()?;
            within(cancel, deadline, async {
                self.subject.check_scope().await.map_err(io::Error::other)?;
                self.subject.check().await.map_err(io::Error::other)?;
                if self.subject.observe().await.map_err(io::Error::other)? != self.observation {
                    return Err(refused());
                }
                Ok(())
            })
            .await?;
            self.current()
        })
    }
}
pub(super) struct ExternalHistoryReadHandle {
    source: ReadSource,
    lineage: ReadLineage,
    lifetime: uuid::Uuid,
}
impl ExternalHistoryReadHandle {
    pub(super) fn retire(mut self) -> LocalBoxFuture<'static, io::Result<()>> {
        Box::pin(async move { self.source.stop_and_wait().await })
    }
    pub(super) fn adopt<'a>(
        admission: Rc<RefCell<ExternalSourceAdmission>>,
        descriptors: ReadDescriptors,
        request_id: &'a str,
        registry: &'a AcknowledgedRegistry,
        cancel: &'a AtomicBool,
        deadline: Instant,
    ) -> LocalBoxFuture<'a, io::Result<(Self, String)>> {
        Box::pin(async move {
            let mut source =
                ReadSource::receive(admission, descriptors, request_id, cancel, deadline).await?;
            let matched: io::Result<(String, ReadLineage)> = async {
                let candidates: Vec<_> = registry
                    .try_borrow()
                    .map_err(|_| refused())?
                    .completed
                    .iter()
                    .filter(|(id, _)| {
                        source
                            .expected
                            .completed_request_id
                            .as_ref()
                            .is_none_or(|expected| expected == *id)
                    })
                    .map(|(id, owner)| (id.clone(), Rc::clone(owner)))
                    .collect();
                let mut matched = None;
                for (id, owner) in candidates {
                    if let Ok(lineage) = owner.read_lineage(cancel, deadline).await {
                        let a = source.admission.try_borrow().map_err(|_| refused())?;
                        if lineage.observation == source.observation
                            && lineage.person == a.context.verifier.person
                            && lineage.room == a.context.verifier.personal_room
                        {
                            if matched.is_some() {
                                return Err(refused());
                            }
                            matched = Some((id, lineage));
                        }
                    }
                }
                matched.ok_or_else(refused)
            }
            .await;
            let (id, lineage) = match matched {
                Ok(value) => value,
                Err(error) => {
                    source.stop_and_wait().await?;
                    return Err(error);
                }
            };
            let mut handle = Self {
                source,
                lineage,
                lifetime: uuid::Uuid::new_v4(),
            };
            let result: io::Result<()> = async {
                handle.source.check(cancel, deadline).await?;
                handle.snapshot()?;
                Ok(())
            }
            .await;
            if let Err(error) = result {
                handle.source.stop_and_wait().await?;
                return Err(error);
            }
            Ok((handle, id))
        })
    }
    fn snapshot(
        &self,
    ) -> io::Result<(
        crate::history::RecordHistorySnapshot,
        Scope,
        Option<fs_protocol::ContentRevisionV1>,
    )> {
        self.source.current()?;
        let a = self.source.admission.try_borrow().map_err(|_| refused())?;
        let runtime = a.factory.runtime.as_ref().ok_or_else(refused)?;
        let namespace = runtime.namespace.lock().map_err(|_| refused())?;
        let history = runtime.history.lease().map_err(io::Error::other)?;
        let projection = namespace
            .external_record(self.lineage.binding.record_id)
            .map_err(io::Error::other)?
            .ok_or_else(refused)?;
        drop(namespace);
        if projection.binding != self.lineage.binding
            || projection.binding.tombstoned
            || projection.version != Some(self.lineage.version)
        {
            return Err(refused());
        }
        let key = crate::record_repository::RecordRepositoryKey {
            workspace_id: a.factory.authority.workspace(),
            record_id: self.lineage.binding.record_id,
        };
        let repository = a
            .factory
            .publisher
            .repository()
            .projection_lease(key)
            .map_err(io::Error::other)?;
        let signed = &repository.snapshot;
        if signed.binding.as_ref() != Some(&projection.binding)
            || signed
                .content_revision
                .as_ref()
                .is_none_or(|revision| revision.version_id != self.lineage.version)
        {
            return Err(refused());
        }
        let snapshot = history.snapshot(key.record_id).ok_or_else(refused)?;
        drop(history);
        if signed
            .versions
            .iter()
            .map(|v| (v.id, v))
            .collect::<std::collections::BTreeMap<_, _>>()
            != snapshot
                .versions
                .iter()
                .map(|v| (v.id, v))
                .collect::<std::collections::BTreeMap<_, _>>()
        {
            return Err(refused());
        }
        let current_revision = signed.content_revision;
        drop(repository);
        if current_revision.is_some_and(|revision| {
            revision.record_id != key.record_id
                || !snapshot.versions.iter().any(|version| {
                    (version.id, version.content_hash, version.metadata_hash)
                        == (
                            revision.version_id,
                            revision.content_hash,
                            revision.metadata_hash,
                        )
                })
        }) {
            return Err(refused());
        }
        let frontier = *blake3::hash(
            &serde_json::to_vec(&(
                "tilo-retained-history-read-v1",
                a.factory.authority.workspace(),
                &self.source.expected.read_request_id,
                &(
                    self.source.observation.source_owner,
                    self.source.observation.root,
                    self.source.observation.parent,
                    &self.source.observation.parents,
                    &self.source.observation.name,
                    self.source.observation.identity,
                ),
                &projection,
                &snapshot,
                current_revision,
            ))
            .map_err(io::Error::other)?,
        )
        .as_bytes();
        let scope = Scope {
            workspace_id: a.factory.authority.workspace(),
            mount_lifetime: self.lifetime,
            record_id: key.record_id,
            frontier,
        };
        self.source.current()?;
        Ok((snapshot, scope, current_revision))
    }
    pub(super) fn query<'a>(
        &'a self,
        query: &'a str,
        cancel: &'a AtomicBool,
        deadline: Instant,
    ) -> LocalBoxFuture<'a, io::Result<Vec<u8>>> {
        Box::pin(async move {
            use runtime_fs::external_history::read_control::{
                HISTORY_PROJECTION_QUERY, RECORD_ID_QUERY,
            };
            self.source.check(cancel, deadline).await?;
            let (snapshot, scope, current) = self.snapshot()?;
            let bytes = if query == RECORD_ID_QUERY {
                scope.record_id.0.as_bytes().to_vec()
            } else if query == HISTORY_PROJECTION_QUERY {
                serde_json::to_vec(&snapshot.projection()).map_err(io::Error::other)?
            } else {
                let query = Query::parse_xattr(query).map_err(|_| refused())?;
                let result = if let Query::Preview {
                    frontier,
                    version_id,
                    ..
                } = &query
                {
                    match snapshot.read_query(
                        scope,
                        current,
                        Query::Version {
                            frontier: *frontier,
                            version_id: *version_id,
                        },
                        &[],
                    ) {
                        Ok(_) => Err(Error::NoContent),
                        Err(error) => Err(error),
                    }
                } else {
                    snapshot.read_query(scope, current, query, &[])
                };
                serde_json::to_vec(&FileHistoryReadResponseV1 {
                    schema_version: 1,
                    result,
                })
                .map_err(io::Error::other)?
            };
            if (query == HISTORY_PROJECTION_QUERY && bytes.len() > 16 * 1024)
                || bytes.len() > fs_protocol::files::MAX_HISTORY_READ_BYTES
            {
                return Err(refused());
            }
            self.source.check(cancel, deadline).await?;
            Ok(bytes)
        })
    }
}
