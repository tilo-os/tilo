//! Single-use routes to actual source-resident prepared Work. Tickets are not grants.
use super::{
    Arc, ConnectedWorkspaceServer, DirEntry, Duration, MAX_BACKEND_CALLS, Metadata, Rc, Server,
    WorkspaceBackend, directory, io, oneshot,
};
use crate::directory_backend::WorkspacePendingReplacementBackend;
use capnp_rpc::{RpcSystem, rpc_twoparty_capnp, twoparty};
use std::collections::HashMap;
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::sync::{Condvar, Mutex};
use std::time::Instant;
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
const LIMIT: usize = 64;
const EXPIRY: Duration = Duration::from_secs(10);
/// Counts actual blocking callback lifetimes, including futures dropped at EOF.
#[derive(Default)]
pub(super) struct CallDrain {
    count: Mutex<usize>,
    changed: Condvar,
}
pub(super) struct CallGuard(Arc<CallDrain>);
impl CallDrain {
    pub(super) fn enter(self: &Arc<Self>) -> Result<CallGuard, String> {
        let mut count = self.count.lock().map_err(|_| "handoff drain poisoned")?;
        *count = count.checked_add(1).ok_or("handoff callback overflow")?;
        drop(count);
        Ok(CallGuard(self.clone()))
    }
    fn wait(&self) -> io::Result<()> {
        let mut count = self
            .count
            .lock()
            .map_err(|_| io::Error::other("handoff drain poisoned"))?;
        while *count != 0 {
            count = self
                .changed
                .wait(count)
                .map_err(|_| io::Error::other("handoff drain poisoned"))?;
        }
        drop(count);
        Ok(())
    }
}
impl Drop for CallGuard {
    fn drop(&mut self) {
        // Poison is terminal: never manufacture an observed drain.
        if let Ok(mut count) = self.0.count.lock() {
            assert!(*count > 0, "held callback guard is counted");
            *count = count.saturating_sub(1);
            self.0.changed.notify_all();
        }
    }
}
enum ActiveCapability {
    Pending(Arc<dyn WorkspacePendingReplacementBackend>),
    Read(Arc<dyn crate::directory_backend::WorkspaceHistoryReadSubjectBackend>),
}
struct Active {
    capability: ActiveCapability,
    socket: Option<UnixStream>,
    joined: Option<Result<(), String>>,
}
struct Reservation {
    challenge: [u8; 32],
    expires: Instant,
    pending: Arc<dyn WorkspacePendingReplacementBackend>,
}
struct ReadReservation {
    challenge: [u8; 32],
    expires: Instant,
    subject: Arc<dyn crate::directory_backend::WorkspaceHistoryReadSubjectBackend>,
}
#[derive(Default)]
struct State {
    closed: bool,
    entries: HashMap<[u8; 16], Reservation>,
    reads: HashMap<[u8; 16], ReadReservation>,
    active: HashMap<[u8; 16], Active>,
}
/// Source-local routing registry. Empty construction grants no native rights;
/// only an actual Pending server on the owned controller connection can arm it.
pub struct PendingHandoffRegistry {
    state: Mutex<State>,
    calls: Arc<tokio::sync::Semaphore>,
    changed: Condvar,
}
impl Default for PendingHandoffRegistry {
    fn default() -> Self {
        Self {
            state: Mutex::new(State::default()),
            changed: Condvar::new(),
            calls: Arc::new(tokio::sync::Semaphore::new(MAX_BACKEND_CALLS)),
        }
    }
}
impl PendingHandoffRegistry {
    pub(super) fn arm(
        &self,
        pending: Arc<dyn WorkspacePendingReplacementBackend>,
        challenge: [u8; 32],
    ) -> Result<[u8; 16], String> {
        if challenge == [0; 32] {
            return Err("empty handoff challenge".into());
        }
        pending.check()?;
        let now = Instant::now();
        let mut state = self.state.lock().map_err(|_| "handoff registry poisoned")?;
        state.entries.retain(|_, entry| entry.expires > now);
        state.reads.retain(|_, entry| entry.expires > now);
        if state.closed
            || state.entries.len().saturating_add(state.reads.len()).saturating_add(state.active.len()) >= LIMIT
            || state
                .entries
                .values()
                .any(|entry| Arc::ptr_eq(&entry.pending, &pending))
            || state
                .active
                .values()
                .any(|entry| matches!(&entry.capability, ActiveCapability::Pending(held) if Arc::ptr_eq(held, &pending)))
        {
            return Err("handoff registry unavailable or already armed".into());
        }
        let ticket = *uuid::Uuid::new_v4().as_bytes();
        if state.entries.contains_key(&ticket)
            || state.reads.contains_key(&ticket)
            || state.active.contains_key(&ticket)
        {
            return Err("handoff ticket collision".into());
        }
        let expires = now.checked_add(EXPIRY).ok_or("handoff deadline overflow")?;
        state.entries.insert(
            ticket,
            Reservation {
                challenge,
                expires,
                pending: pending.clone(),
            },
        );
        drop(state);
        if let Err(error) = pending.check() {
            self.state
                .lock()
                .map_err(|_| "handoff registry poisoned")?
                .entries
                .remove(&ticket);
            return Err(error);
        }
        drop(pending);
        Ok(ticket)
    }
    pub(super) fn arm_read(
        &self,
        subject: Arc<dyn crate::directory_backend::WorkspaceHistoryReadSubjectBackend>,
        challenge: [u8; 32],
    ) -> Result<[u8; 16], String> {
        if challenge == [0; 32] {
            return Err("empty history Read handoff challenge".into());
        }
        subject.check()?;
        let now = Instant::now();
        let mut state = self.state.lock().map_err(|_| "handoff registry poisoned")?;
        state.entries.retain(|_, entry| entry.expires > now);
        state.reads.retain(|_, entry| entry.expires > now);
        if state.closed
            || state.entries.len().saturating_add(state.reads.len()).saturating_add(state.active.len()) >= LIMIT
            || state
                .reads
                .values()
                .any(|entry| Arc::ptr_eq(&entry.subject, &subject))
            || state.active.values().any(|entry| matches!(&entry.capability, ActiveCapability::Read(held) if Arc::ptr_eq(held, &subject)))
        {
            return Err("history Read handoff registry busy or already armed".into());
        }
        let ticket = *uuid::Uuid::new_v4().as_bytes();
        if state.entries.contains_key(&ticket)
            || state.reads.contains_key(&ticket)
            || state.active.contains_key(&ticket)
        {
            return Err("handoff ticket collision".into());
        }
        state.reads.insert(
            ticket,
            ReadReservation {
                challenge,
                expires: now.checked_add(EXPIRY).ok_or("handoff deadline overflow")?,
                subject: subject.clone(),
            },
        );
        drop(state);
        if let Err(error) = subject.check() {
            self.state
                .lock()
                .map_err(|_| "handoff registry poisoned")?
                .reads
                .remove(&ticket);
            return Err(error);
        }
        drop(subject);
        Ok(ticket)
    }
    /// Register the exact accepted pending transport before its QPE/RPC work.
    /// Only the owned source supervisor calls this with its actual held cap.
    /// # Errors
    /// Refuses an absent, replayed, mismatched or closed actual registry route.
    pub fn begin_pending(
        &self,
        ticket: [u8; 16],
        pending: Arc<dyn WorkspacePendingReplacementBackend>,
        socket: UnixStream,
    ) -> io::Result<()> {
        let result = self.begin(
            ticket,
            socket,
            |cap| matches!(cap, ActiveCapability::Pending(held) if Arc::ptr_eq(held, &pending)),
        );
        drop(pending);
        result
    }
    /// Register only the SAME actual consumed Read subject's accepted socket.
    /// # Errors
    /// Refuses absent/replayed/mismatched or closed actual source routes.
    pub fn begin_read(
        &self,
        ticket: [u8; 16],
        subject: Arc<dyn crate::directory_backend::WorkspaceHistoryReadSubjectBackend>,
        socket: UnixStream,
    ) -> io::Result<()> {
        let result = self.begin(
            ticket,
            socket,
            |cap| matches!(cap, ActiveCapability::Read(held) if Arc::ptr_eq(held, &subject)),
        );
        drop(subject);
        result
    }
    fn begin(
        &self,
        ticket: [u8; 16],
        socket: UnixStream,
        same: impl FnOnce(&ActiveCapability) -> bool,
    ) -> io::Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| io::Error::other("handoff registry poisoned"))?;
        if state.closed {
            return Err(io::Error::other("handoff lifecycle closed"));
        }
        let active = state
            .active
            .get_mut(&ticket)
            .ok_or_else(|| io::Error::other("handoff not consumed from registry"))?;
        if !same(&active.capability) || active.socket.is_some() || active.joined.is_some() {
            return Err(io::Error::other("handoff lifecycle mismatch"));
        }
        active.socket = Some(socket);
        drop(state);
        Ok(())
    }
    /// Called only AFTER the owned supervisor has joined the exact child.
    /// Its RPC thread has already drained every submitted blocking callback.
    /// # Errors
    /// Refuses an unobservable registry, absent route or duplicate join.
    pub fn observed_join(&self, ticket: [u8; 16], result: Result<(), String>) -> io::Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| io::Error::other("handoff registry poisoned"))?;
        let active = state
            .active
            .get_mut(&ticket)
            .ok_or_else(|| io::Error::other("handoff lifecycle absent"))?;
        if active.joined.is_some() {
            return Err(io::Error::other("handoff joined twice"));
        }
        active.joined = Some(result);
        self.changed.notify_all();
        drop(state);
        Ok(())
    }
    pub(super) fn retire(
        &self,
        pending: &Arc<dyn WorkspacePendingReplacementBackend>,
        ticket: [u8; 16],
    ) -> Result<(), String> {
        pending.check()?;
        self.retire_route(
            ticket,
            |cap| matches!(cap, ActiveCapability::Pending(held) if Arc::ptr_eq(held, pending)),
        )
    }
    pub(super) fn retire_read(
        &self,
        subject: &Arc<dyn crate::directory_backend::WorkspaceHistoryReadSubjectBackend>,
        ticket: [u8; 16],
    ) -> Result<(), String> {
        // A successor can change the named leaf while this actual held Read
        // subject remains current. Retirement requires scope, not old-name lookup.
        subject.check_scope()?;
        self.retire_route(
            ticket,
            |cap| matches!(cap, ActiveCapability::Read(held) if Arc::ptr_eq(held, subject)),
        )
    }
    fn retire_route(
        &self,
        ticket: [u8; 16],
        same: impl FnOnce(&ActiveCapability) -> bool,
    ) -> Result<(), String> {
        let mut state = self.state.lock().map_err(|_| "handoff registry poisoned")?;
        let active = state
            .active
            .get(&ticket)
            .ok_or("handoff lifecycle absent")?;
        if !same(&active.capability) {
            return Err("handoff capability mismatch".into());
        }
        active
            .socket
            .as_ref()
            .ok_or("handoff transport not yet observed")?
            .shutdown(Shutdown::Both)
            .map_err(|e| e.to_string())?;
        let deadline = Instant::now()
            .checked_add(EXPIRY)
            .ok_or("handoff retirement deadline overflow")?;
        loop {
            if state.closed {
                return Err("original source retired during handoff release".into());
            }
            let active = state
                .active
                .get(&ticket)
                .ok_or("handoff lifecycle absent")?;
            if let Some(result) = &active.joined {
                result.clone()?;
                state.active.remove(&ticket);
                return Ok(());
            }
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or("handoff retirement unobserved before deadline")?;
            state = self
                .changed
                .wait_timeout(state, remaining)
                .map_err(|_| "handoff registry poisoned")?
                .0;
        }
    }
    /// Consume an actual Read capability once; it cannot select Pending or `WorkspaceFs`.
    /// # Errors
    /// Refuses closed/replayed/expired tickets or current original Read loss.
    pub fn take_read(
        &self,
        ticket: [u8; 16],
        challenge: [u8; 32],
    ) -> Result<Arc<dyn crate::directory_backend::WorkspaceHistoryReadSubjectBackend>, String> {
        let mut state = self.state.lock().map_err(|_| "handoff registry poisoned")?;
        if state.closed {
            return Err("handoff registry closed".into());
        }
        let entry = state
            .reads
            .remove(&ticket)
            .ok_or("history Read reservation absent")?;
        if entry.challenge != challenge || entry.expires <= Instant::now() {
            return Err("history Read reservation mismatch or expired".into());
        }
        entry.subject.check()?;
        state.active.insert(
            ticket,
            Active {
                capability: ActiveCapability::Read(entry.subject.clone()),
                socket: None,
                joined: None,
            },
        );
        drop(state);
        Ok(entry.subject)
    }
    /// Consume once only after authenticating the owned source connection.
    /// This returns the existing native object, never reconstructs Work from data.
    /// # Errors
    /// Refuses closed, expired, replayed, mismatched or revoked reservations.
    pub fn take(
        &self,
        ticket: [u8; 16],
        challenge: [u8; 32],
    ) -> Result<Arc<dyn WorkspacePendingReplacementBackend>, String> {
        let mut state = self.state.lock().map_err(|_| "handoff registry poisoned")?;
        if state.closed {
            return Err("handoff registry closed".into());
        }
        let entry = state
            .entries
            .remove(&ticket)
            .ok_or("handoff reservation absent")?;
        if entry.challenge != challenge || entry.expires <= Instant::now() {
            return Err("handoff reservation mismatch or expired".into());
        }
        entry.pending.check()?;
        state.active.insert(
            ticket,
            Active {
                capability: ActiveCapability::Pending(entry.pending.clone()),
                socket: None,
                joined: None,
            },
        );
        drop(state);
        Ok(entry.pending)
    }
    /// Close routing before joining every owned handoff and native callback.
    /// # Errors
    /// Refuses an unobservable poisoned registry.
    pub fn close(&self) -> io::Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| io::Error::other("handoff registry poisoned"))?;
        state.closed = true;
        self.changed.notify_all();
        state.entries.clear();
        state.reads.clear();
        drop(state);
        Ok(())
    }
}
struct ClosedBackend;
impl WorkspaceBackend for ClosedBackend {
    fn list(&self, _: &str) -> Result<Vec<DirEntry>, String> {
        Err("pending-only connection".into())
    }
    fn stat(&self, _: &str) -> Result<Metadata, String> {
        Err("pending-only connection".into())
    }
    fn read(&self, _: &str, _: u64, _: u32) -> Result<Vec<u8>, String> {
        Err("pending-only connection".into())
    }
}
/// Serve ONLY this actual held pending capability on an independently kernel
/// authenticated connection. Opens no path, selects no arbitrary object, and
/// supplies no `WorkspaceFs` bootstrap or native authority constructor.
/// # Errors
/// Refuses revoked native Work or failure to own the exact RPC server.
pub fn serve_pending_connected(
    stream: std::os::unix::net::UnixStream,
    pending: Arc<dyn WorkspacePendingReplacementBackend>,
    runtime: tokio::runtime::Handle,
    registry: Arc<PendingHandoffRegistry>,
) -> io::Result<ConnectedWorkspaceServer> {
    pending.check().map_err(io::Error::other)?;
    stream.set_nonblocking(true)?;
    let local_runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let stream = {
        let _entered = local_runtime.enter();
        tokio::net::UnixStream::from_std(stream)?
    };
    let (stop, stopped) = oneshot::channel();
    let thread = std::thread::Builder::new()
        .name("source-pending-capability".into())
        .spawn(move || {
            let local = tokio::task::LocalSet::new();
            local.block_on(&local_runtime, async move {
                pending.check().map_err(io::Error::other)?;
                let drain = Arc::new(CallDrain::default());
                let server = Rc::new(Server {
                    backend: Arc::new(ClosedBackend),
                    runtime,
                    calls: registry.calls.clone(),
                    handoffs: None,
                    drain: Some(drain.clone()),
                });
                let client = directory::pending_client(server, pending);
                let (read, write) = stream.into_split();
                let network = twoparty::VatNetwork::new(
                    read.compat(),
                    write.compat_write(),
                    rpc_twoparty_capnp::Side::Server,
                    capnp::message::ReaderOptions::default(),
                );
                let rpc = RpcSystem::new(Box::new(network), Some(client.client));
                let result = tokio::select! {
                    result = rpc => match result {
                        Ok(()) => Ok(()),
                        Err(error) if error.kind == capnp::ErrorKind::Disconnected => Ok(()),
                        Err(error) => Err(io::Error::other(error.to_string())),
                    },
                    _ = stopped => Ok(()),
                };
                drain.wait()?;
                result
            })
        })?;
    Ok(ConnectedWorkspaceServer {
        stop: Some(stop),
        thread: Some(thread),
    })
}

/// Serve ONLY a genuine original Read subject after independent source QPE.
/// No record mapping, byte grant, Pending or `WorkspaceFs` fallback.
/// # Errors
/// Refuses source retirement or inability to own the exact connected server.
pub fn serve_history_read_connected(
    stream: std::os::unix::net::UnixStream,
    subject: Arc<dyn crate::directory_backend::WorkspaceHistoryReadSubjectBackend>,
    runtime: tokio::runtime::Handle,
    registry: Arc<PendingHandoffRegistry>,
) -> io::Result<ConnectedWorkspaceServer> {
    subject.check().map_err(io::Error::other)?;
    stream.set_nonblocking(true)?;
    let local_runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let stream = {
        let _entered = local_runtime.enter();
        tokio::net::UnixStream::from_std(stream)?
    };
    let (stop, stopped) = oneshot::channel();
    let thread = std::thread::Builder::new()
        .name("source-history-read-capability".into())
        .spawn(move || {
            let local = tokio::task::LocalSet::new();
            local.block_on(&local_runtime, async move {
                subject.check().map_err(io::Error::other)?;
                let drain = Arc::new(CallDrain::default());
                let server = Rc::new(Server {
                    backend: Arc::new(ClosedBackend),
                    runtime,
                    calls: registry.calls.clone(),
                    handoffs: None,
                    drain: Some(drain.clone()),
                });
                let client = directory::history_read_client(server, subject)
                    .map_err(|error| io::Error::other(error.to_string()))?;
                let (read, write) = stream.into_split();
                let network = twoparty::VatNetwork::new(
                    read.compat(),
                    write.compat_write(),
                    rpc_twoparty_capnp::Side::Server,
                    capnp::message::ReaderOptions::default(),
                );
                let rpc = RpcSystem::new(Box::new(network), Some(client.client));
                let result = tokio::select! {
                    result = rpc => match result {
                        Ok(()) => Ok(()),
                        Err(error) if error.kind == capnp::ErrorKind::Disconnected => Ok(()),
                        Err(error) => Err(io::Error::other(error.to_string())),
                    },
                    _ = stopped => Ok(()),
                };
                drain.wait()?;
                result
            })
        })?;
    Ok(ConnectedWorkspaceServer {
        stop: Some(stop),
        thread: Some(thread),
    })
}

#[cfg(test)]
mod retirement_tests {
    use super::*;
    use crate::fs_capnp;
    use capnp_rpc::{RpcSystem, rpc_twoparty_capnp, twoparty};
    use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

    #[test]
    fn actual_pending_rpc_release_joins_without_retiring_original_native_session() {
        let mut fixture = crate::operations::tests::pending_replacement_for_transport();
        let directory = crate::directory::DirectoryCapability::new(
            fixture.session.clone(),
            fixture.parent.clone(),
        )
        .unwrap();
        let pending = directory
            .pending_for_transport_test(fixture.prepared.take().unwrap())
            .unwrap();
        let registry = Arc::new(PendingHandoffRegistry::default());
        let challenge = [7; 32];
        let ticket = registry.arm(pending.clone(), challenge).unwrap();
        let held = registry.take(ticket, challenge).unwrap();
        let (source, client) = UnixStream::pair().unwrap();
        registry
            .begin_pending(ticket, held.clone(), source.try_clone().unwrap())
            .unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let server =
            serve_pending_connected(source, held, runtime.handle().clone(), registry.clone())
                .unwrap();
        let observer = registry.clone();
        let joined = std::thread::spawn(move || {
            let result = server.wait();
            observer
                .observed_join(
                    ticket,
                    result.as_ref().copied().map_err(ToString::to_string),
                )
                .unwrap();
            result
        });
        tokio::task::LocalSet::new().block_on(&runtime, async {
            tokio::time::timeout(Duration::from_secs(15), async {
                client.set_nonblocking(true).unwrap();
                let stream = tokio::net::UnixStream::from_std(client).unwrap();
                let (read, write) = stream.into_split();
                let network = twoparty::VatNetwork::new(read.compat(), write.compat_write(), rpc_twoparty_capnp::Side::Client, capnp::message::ReaderOptions::default());
                let mut rpc = RpcSystem::new(Box::new(network), None);
                let raw: fs_capnp::pending_replacement::Client = rpc.bootstrap(rpc_twoparty_capnp::Side::Server);
                let rpc = tokio::task::spawn_local(rpc);
                let remote = crate::directory_client::PendingReplacementClient::from_authenticated_source_capability(raw);
                remote.check_scope().await.unwrap();
                assert!(remote.retire_daemon_handoff(ticket).await.is_err(), "daemon bootstrap cannot retire its own provider through the controller lane");
                let registry = registry.clone();
                let pending = pending.clone();
                tokio::task::spawn_blocking(move || registry.retire(&pending, ticket)).await.unwrap().unwrap();
                if let Err(error) = rpc.await.expect("actual handoff RPC driver must not panic") {
                    assert_eq!(error.kind, capnp::ErrorKind::Disconnected,
                        "observed departure allows clean EOF/disconnect only");
                }
                assert!(remote.check_scope().await.is_err());
            }).await.expect("actual per-handoff retirement must be bounded");
        });
        joined.join().unwrap().unwrap();
        assert!(registry.state.lock().unwrap().active.is_empty());
        // Same actual source/session remains usable; no worker namespace stop.
        pending.check().unwrap();
        pending.observe().unwrap().validate().unwrap();
        fixture.assert_original_entries();
        let next = registry.arm(pending.clone(), [8; 32]).unwrap();
        assert_ne!(next, ticket);
        pending.cancel().unwrap();
        registry.close().unwrap();
    }

    #[test]
    fn callback_guard_survives_cancelled_rpc_future_until_real_blocking_call_finishes() {
        let drain = Arc::new(CallDrain::default());
        let guard = drain.enter().unwrap();
        let (release, blocked) = std::sync::mpsc::channel();
        let (entered, observed) = std::sync::mpsc::channel();
        let callback = std::thread::spawn(move || {
            let _guard = guard;
            entered.send(()).unwrap();
            blocked.recv().unwrap();
        });
        observed.recv_timeout(Duration::from_secs(2)).unwrap();
        let waiting = drain;
        let (done, stopped) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            waiting.wait().unwrap();
            done.send(()).unwrap();
        });
        assert!(
            stopped.recv_timeout(Duration::from_millis(50)).is_err(),
            "callback still owns exact drain token"
        );
        release.send(()).unwrap();
        callback.join().unwrap();
        stopped.recv_timeout(Duration::from_secs(2)).unwrap();
        waiter.join().unwrap();
    }
}

#[cfg(test)]
mod read_release_tests {
    use super::*;
    use crate::workspace_server::serve_workspace_connected;
    use crate::{OpenMode, fs_capnp};
    use capnp_rpc::{RpcSystem, rpc_twoparty_capnp, twoparty};
    use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};
    /// Actual kernel peer refusal is not a reason to strand an independently
    /// owned source capability. This is the source/transport boundary, not an
    /// installed-editor authority-positive fixture.
    #[test]
    fn actual_peer_uid_refusal_preserves_owned_read_cache_cleanup_channel() {
        use crate::directory_connection::{DirectoryConnection, ROOT_DIRECTORY};
        use std::io::Write as _;
        let fixture = crate::operations::tests::history_read_for_transport(true);
        let directory = crate::directory::DirectoryWorkspaceBackend::new(
            fixture.session.clone(),
            fixture.parent.clone(),
        )
        .unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let (source, target) = UnixStream::pair().unwrap();
        let server =
            serve_workspace_connected(source, Arc::new(directory), runtime.handle().clone())
                .unwrap();
        let connection = DirectoryConnection::connected(target).unwrap();
        let subject = connection
            .history_read_subject(ROOT_DIRECTORY, &fixture.name)
            .unwrap();
        let observed = connection.observe_history_read(subject).unwrap();
        observed.validate().unwrap();

        // QPE compares actual SCM credentials/socket creator, not a fake role
        // boolean. Deliberately demand another UID and require native refusal.
        let (mut controller, mut consumer) = UnixStream::pair().unwrap();
        controller
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        controller
            .set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        runtime_portal::process_binding::arm(&controller).unwrap();
        let challenge = [91; 32];
        let task =
            std::thread::spawn(move || runtime_portal::process_binding::complete(&mut consumer));
        controller
            .write_all(&runtime_portal::process_binding::MAGIC)
            .unwrap();
        controller.write_all(&challenge).unwrap();
        let actual_uid = rustix::process::getuid().as_raw();
        let Err(error) = runtime_portal::process_binding::receive(
            &controller,
            &challenge,
            actual_uid.checked_add(1).unwrap(),
        ) else {
            panic!("a wrong actual peer UID must not authenticate");
        };
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        drop(controller);
        assert!(task.join().unwrap().is_err());

        // The original owned connection remains native-live. Exact alias release
        // plus the same ordered fixed root metadata barrier succeeds, with no
        // authority upgrade and no closure of another viewer's native File.
        connection.release_history_read(subject).unwrap();
        connection.metadata(ROOT_DIRECTORY).unwrap();
        assert!(connection.check_history_read(subject, false).is_err());
        let next = connection
            .history_read_subject(ROOT_DIRECTORY, &fixture.name)
            .unwrap();
        assert_eq!(connection.observe_history_read(next).unwrap(), observed);
        connection.release_history_read(next).unwrap();
        connection.metadata(ROOT_DIRECTORY).unwrap();
        connection.retire().unwrap();
        server.wait().unwrap();
    }
    #[test]
    fn actual_seventy_read_cache_releases_preserve_separate_public_file_handle() {
        use crate::directory_connection::{DirectoryConnection, ROOT_DIRECTORY};
        let fixture = crate::operations::tests::history_read_for_transport(true);
        let directory = crate::directory::DirectoryWorkspaceBackend::new(
            fixture.session.clone(),
            fixture.parent.clone(),
        )
        .unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let (source, target) = UnixStream::pair().unwrap();
        let server =
            serve_workspace_connected(source, Arc::new(directory), runtime.handle().clone())
                .unwrap();
        let connection = DirectoryConnection::connected(target).unwrap();
        let file = connection
            .open(ROOT_DIRECTORY, &fixture.name, OpenMode::Read)
            .unwrap();
        let expected = connection.read(file, 0, 256).unwrap();
        assert!(!expected.is_empty());
        for _ in 0..70 {
            let subject = connection
                .history_read_subject(ROOT_DIRECTORY, &fixture.name)
                .unwrap();
            let observed = connection.observe_history_read(subject).unwrap();
            observed.validate().unwrap();
            connection.release_history_read(subject).unwrap();
            // Same ordered original RPC connection, after actual local cap drop.
            connection.metadata(ROOT_DIRECTORY).unwrap();
            assert!(connection.check_history_read(subject, false).is_err());
            assert_eq!(
                connection.read(file, 0, 256).unwrap(),
                expected,
                "separate real File/OFD is never bulk-deleted"
            );
        }
        connection.release(file).unwrap();
        connection.retire().unwrap();
        server.wait().unwrap();
    }
    #[test]
    fn actual_history_read_scope_revocation_still_retires_the_source_connection() {
        use crate::directory_connection::{DirectoryConnection, ROOT_DIRECTORY};
        let fixture = crate::operations::tests::history_read_for_transport(true);
        let directory = crate::directory::DirectoryWorkspaceBackend::new(
            fixture.session.clone(),
            fixture.parent.clone(),
        )
        .unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let (source, target) = UnixStream::pair().unwrap();
        let server =
            serve_workspace_connected(source, Arc::new(directory), runtime.handle().clone())
                .unwrap();
        let connection = DirectoryConnection::connected(target).unwrap();
        let file = connection
            .open(ROOT_DIRECTORY, &fixture.name, OpenMode::Read)
            .unwrap();
        assert!(!connection.read(file, 0, 256).unwrap().is_empty());
        let subject = connection
            .history_read_subject(ROOT_DIRECTORY, &fixture.name)
            .unwrap();
        connection.check_history_read(subject, false).unwrap();
        fixture.revoke().unwrap();
        let error = connection.check_history_read(subject, false).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
        assert!(connection.read(file, 0, 256).is_err());
        assert!(
            connection
                .history_read_subject(ROOT_DIRECTORY, &fixture.name)
                .is_err()
        );
        // A real remote authority failure remains a terminal owned outcome,
        // unlike a missing already-released alias on an otherwise live lane.
        assert!(connection.retire().is_err());
        server.wait().unwrap();
    }
    #[test]
    fn actual_read_handoff_join_preserves_same_source_and_refuses_recursive_retirement() {
        use crate::directory_backend::WorkspaceDirectoryBackend;
        let fixture = crate::operations::tests::history_read_for_transport(true);
        let directory = crate::directory::DirectoryCapability::new(
            fixture.session.clone(),
            fixture.parent.clone(),
        )
        .unwrap();
        let subject = directory.history_read_subject(&fixture.name).unwrap();
        let registry = Arc::new(PendingHandoffRegistry::default());
        let ticket = registry.arm_read(subject.clone(), [9; 32]).unwrap();
        let held = registry.take_read(ticket, [9; 32]).unwrap();
        let (source, client) = UnixStream::pair().unwrap();
        registry
            .begin_read(ticket, held.clone(), source.try_clone().unwrap())
            .unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let server =
            serve_history_read_connected(source, held, runtime.handle().clone(), registry.clone())
                .unwrap();
        let observer = registry.clone();
        let joined = std::thread::spawn(move || {
            let result = server.wait();
            observer
                .observed_join(
                    ticket,
                    result.as_ref().copied().map_err(ToString::to_string),
                )
                .unwrap();
            result
        });
        tokio::task::LocalSet::new().block_on(&runtime, async {
            tokio::time::timeout(Duration::from_secs(15), async {
                client.set_nonblocking(true).unwrap();
                let stream = tokio::net::UnixStream::from_std(client).unwrap();
                let (read,write) = stream.into_split();
                let network = twoparty::VatNetwork::new(read.compat(), write.compat_write(), rpc_twoparty_capnp::Side::Client, capnp::message::ReaderOptions::default());
                let mut rpc = RpcSystem::new(Box::new(network),None);
                let raw: fs_capnp::history_read_subject::Client = rpc.bootstrap(rpc_twoparty_capnp::Side::Server);
                let rpc = tokio::task::spawn_local(rpc);
                let remote = crate::directory_client::HistoryReadSubjectClient::from_authenticated_source_capability(raw);
                remote.check_scope().await.unwrap();
                assert!(remote.retire_daemon_handoff(ticket).await.is_err());
                let registry = registry.clone();let subject = subject.clone();
                tokio::task::spawn_blocking(move || registry.retire_read(&subject,ticket)).await.unwrap().unwrap();
                if let Err(error) = rpc.await.expect("actual handoff RPC driver must not panic") {
                    assert_eq!(error.kind, capnp::ErrorKind::Disconnected,
                        "observed departure allows clean EOF/disconnect only");
                }
                assert!(remote.check_scope().await.is_err());
            }).await.expect("actual Read realm retirement must be bounded");
        });
        joined.join().unwrap().unwrap();
        assert!(registry.state.lock().unwrap().active.is_empty());
        subject.check_scope().unwrap();
        subject.check().unwrap();
        subject.observe().unwrap().validate().unwrap();
        registry.close().unwrap();
    }
}
