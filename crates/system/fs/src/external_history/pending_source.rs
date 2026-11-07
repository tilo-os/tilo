//! The private daemon half of the same-provider pending-only handoff.
//! A generated client is wrapped only after genuine root enrollment, a sealed
//! exact source expectation and actual worker kernel proof have all matched.
use super::{
    AsRawFd, AtomicBool, Duration, ExternalSourceAdmission, File, Instant, LocalFuture,
    MetadataExt, Ordering, Read, Seek, UnixStream, Write, io, nonce, process_binding, refused,
};
use runtime_fs::{
    connection::{Connection, Object},
    directory_client::PendingReplacementClient,
    external_history::NativeReservationDescription,
};
use runtime_portal::retained_role::NativeHistoryPendingHandoff;
use std::{cell::RefCell, rc::Rc};

pub(super) struct AuthenticatedPendingSource {
    pub(super) pending: PendingReplacementClient,
    peer: process_binding::ExecutingPeer,
    program: File,
    expected: NativeHistoryPendingHandoff,
    pub(super) observation: runtime_fs::directory_backend::PendingReplacementObservation,
    pub(super) description: NativeReservationDescription,
    driver: SourceRpcDriver,
}
fn sealed_handoff(mut file: File) -> io::Result<NativeHistoryPendingHandoff> {
    let metadata = file.metadata()?;
    let seals = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GET_SEALS) };
    let required = libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;
    if metadata.uid() != 0
        || !metadata.is_file()
        || metadata.nlink() != 0
        || metadata.len() == 0
        || metadata.len() > 4096
        || seals < 0
        || seals & required != required
    {
        return Err(refused());
    }
    file.rewind()?;
    let mut bytes = Vec::new();
    file.take(4097).read_to_end(&mut bytes)?;
    NativeHistoryPendingHandoff::decode(&bytes)
}
impl AuthenticatedPendingSource {
    /// Caller is the actual same-lane enrolled admission; there is no public
    /// constructor from a source observation, caller UID, path or raw capability.
    pub(super) fn receive(
        admission: Rc<RefCell<ExternalSourceAdmission>>,
        sealed: File,
        stream: UnixStream,
        program: File,
        cancel: &AtomicBool,
        deadline: Instant,
    ) -> LocalFuture<'_, io::Result<Self>> {
        Box::pin(async move {
            ExternalSourceAdmission::refresh_shared(&admission, cancel, deadline).await?;
            let expected = sealed_handoff(sealed)?;
            {
                let admission = admission.try_borrow().map_err(|_| refused())?;
                if expected.enrollment_context_digest != admission.context.digest()?
                    || expected.source_owner != admission.context.verifier.source_owner
                {
                    return Err(refused());
                }
            }
            let operation =
                uuid::Uuid::parse_str(&expected.operation_id).map_err(io::Error::other)?;
            let held = program.metadata()?;
            if !held.is_file() || [held.dev(), held.ino()] != expected.worker_program {
                return Err(refused());
            }
            let expectation = expected.clone();
            let (stream, result) =
                super::owned_io::exchange(stream, cancel, deadline, move |stream| {
                    let expected = expectation;

                    stream.set_nonblocking(false)?;
                    stream.write_all(b"RPH1")?;
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

            if [program.metadata()?.dev(), program.metadata()?.ino()] != expected.worker_program
                || !peer.current()
            {
                return Err(refused());
            }
            ExternalSourceAdmission::refresh_shared(&admission, cancel, deadline).await?;
            let connection = Connection::connect("history-pending-source", stream)?;
            let pending = PendingReplacementClient::from_object(Object::root(
                std::sync::Arc::clone(&connection),
            ));
            let driver = SourceRpcDriver::new(connection);
            // Keep driver ownership immediately, including every parse/refusal path.
            let Ok(observation) = within(cancel, deadline, async {
                pending.check_scope().await.map_err(io::Error::other)?;
                let value = pending.observe().await.map_err(io::Error::other)?;
                pending.check_scope().await.map_err(io::Error::other)?;
                Ok(value)
            })
            .await
            else {
                return Err(refused());
            };
            let description = NativeReservationDescription::decode(
                &observation.reservation_description,
                observation.operation,
                observation.source_owner,
                observation.intent_digest,
            );
            if cancel.load(Ordering::Acquire)
                || Instant::now() >= deadline
                || observation.operation != *operation.as_bytes()
                || observation.source_owner != expected.source_owner
                || observation.intent_digest != expected.intent_digest
                || description.is_err()
            {
                return Err(refused());
            }
            let result = Self {
                pending,
                peer,
                program,
                expected,
                observation,
                description: description?,
                driver,
            };
            result.check_peer()?;
            ExternalSourceAdmission::refresh_shared(&admission, cancel, deadline).await?;
            Ok(result)
        })
    }
    pub(super) fn check_peer(&self) -> io::Result<()> {
        if !self.peer.current()
            || self.peer.pid() != self.expected.worker_pid
            || self.peer.uid() != self.expected.worker_uid
            || self.peer.gid() != self.expected.worker_gid
            || self.peer.start_time()? != self.expected.worker_start
            || self.driver.is_finished()
        {
            return Err(refused());
        }
        let held = self.program.metadata()?;
        if [held.dev(), held.ino()] != self.expected.worker_program || !self.peer.current() {
            return Err(refused());
        }
        Ok(())
    }
}

// This drop is safe for receive/parse refusals before an effect is submitted.
// The coordinator retains the whole source owner after an attempted start until
// actual completion and the same pin acknowledgement; it never relies on drop
// as cancellation or effect undo.
impl AuthenticatedPendingSource {
    // Only the canonical completed-owner retirement gate calls this. It stops
    // this exact RPC connection, never the shared native source worker.
    pub(super) fn retire_rpc(&self) -> LocalFuture<'_, io::Result<()>> {
        self.driver.retire()
    }
}

/// Keep the source's own connection in shared custody across cancellation of
/// the retirement future: retiring closes it at once, and the worker sees its
/// custody end without touching the shared native source.
struct SourceRpcDriver {
    connection: RefCell<Option<std::sync::Arc<Connection>>>,
}
impl SourceRpcDriver {
    fn new(connection: std::sync::Arc<Connection>) -> Self {
        Self {
            connection: RefCell::new(Some(connection)),
        }
    }
    fn is_finished(&self) -> bool {
        self.connection.try_borrow().map_or(true, |connection| {
            connection
                .as_ref()
                .is_none_or(|connection| !connection.live())
        })
    }
    fn retire(&self) -> LocalFuture<'_, io::Result<()>> {
        Box::pin(std::future::poll_fn(move |_| {
            let Ok(mut slot) = self.connection.try_borrow_mut() else {
                return std::task::Poll::Ready(Err(refused()));
            };
            if let Some(connection) = slot.take() {
                connection.close();
            }
            std::task::Poll::Ready(Ok(()))
        }))
    }
}
impl Drop for SourceRpcDriver {
    fn drop(&mut self) {
        if let Some(connection) = self.connection.get_mut().take() {
            connection.close();
        }
    }
}

pub(super) async fn within<T>(
    cancel: &AtomicBool,
    deadline: Instant,
    call: impl std::future::Future<Output = io::Result<T>>,
) -> io::Result<T> {
    tokio::pin!(call);
    loop {
        if cancel.load(Ordering::Acquire) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        if Instant::now() >= deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        let slice = deadline.min(
            Instant::now()
                .checked_add(Duration::from_millis(20))
                .ok_or_else(refused)?,
        );
        tokio::select! {
            result = &mut call => {
                if cancel.load(Ordering::Acquire) {return Err(io::ErrorKind::Interrupted.into());}
                if Instant::now()>=deadline {return Err(io::ErrorKind::TimedOut.into());}
                return result;
            }
            () = tokio::time::sleep_until(slice.into()) => {}
        }
    }
}

/// A destructive RPC remains owned across bounded turns. If a result wins,
/// return it without a post-completion timeout check that would discard it.
pub(super) async fn within_owned_task<T>(
    cancel: &AtomicBool,
    deadline: Instant,
    task: &mut tokio::task::JoinHandle<T>,
) -> io::Result<T> {
    loop {
        if cancel.load(Ordering::Acquire) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        if Instant::now() >= deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        let slice = deadline.min(
            Instant::now()
                .checked_add(Duration::from_millis(20))
                .ok_or_else(refused)?,
        );
        tokio::select! {
            result = &mut *task => return result.map_err(io::Error::other),
            () = tokio::time::sleep_until(slice.into()) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    #[test]
    fn consuming_result_is_retained_after_a_bounded_turn_expires() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let local = tokio::task::LocalSet::new();
        local.block_on(&runtime, async {
            let cancel = AtomicBool::new(false);
            let (sender, receiver) = tokio::sync::oneshot::channel();
            let mut owned = tokio::task::spawn_local(async move { receiver.await.unwrap() });
            let error = within_owned_task(
                &cancel,
                Instant::now() + Duration::from_millis(20),
                &mut owned,
            )
            .await
            .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::TimedOut);
            sender.send(42).unwrap();
            assert_eq!(
                within_owned_task(&cancel, tilo_test_env::hang_deadline(), &mut owned)
                    .await
                    .unwrap(),
                42
            );
        });
    }
    #[test]
    fn stalled_actual_future_observes_absolute_deadline_and_cancellation() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            // The future never ends: returning at all is the deadline's doing.
            let cancel = AtomicBool::new(false);
            let result = within::<()>(
                &cancel,
                Instant::now() + Duration::from_millis(50),
                std::future::pending(),
            )
            .await;
            assert!(result.is_err());
            let cancel = Arc::new(AtomicBool::new(false));
            let signal = Arc::clone(&cancel);
            let task = std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(30));
                signal.store(true, Ordering::Release);
            });
            // An hour's deadline: returning is the cancellation's doing.
            let result = within::<()>(
                &cancel,
                Instant::now() + Duration::from_secs(3600),
                std::future::pending(),
            )
            .await;
            task.join().unwrap();
            assert!(result.is_err());
        });
    }
}

#[cfg(test)]
mod retirement_tests {
    use super::*;
    use std::sync::Arc;
    use tokio::io::AsyncReadExt;

    #[test]
    fn the_source_connection_is_closed_once_before_retirement_returns() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let (owned, mut peer) = tokio::net::UnixStream::pair().unwrap();
            let owned = owned.into_std().unwrap();
            let connection = Connection::connect("test-pending-source", owned).unwrap();
            let driver = SourceRpcDriver::new(Arc::clone(&connection));
            assert!(!driver.is_finished());
            driver.retire().await.unwrap();
            assert!(driver.is_finished());
            assert!(driver.connection.borrow().is_none());
            // The worker sees this source end rather than the shared native
            // source staying open.
            let mut byte = [0_u8; 1];
            assert_eq!(peer.read(&mut byte).await.unwrap(), 0);
            assert!(!connection.live());
            // Retiring again observes the same closed slot; no new connection is made.
            driver.retire().await.unwrap();
        });
    }
}
