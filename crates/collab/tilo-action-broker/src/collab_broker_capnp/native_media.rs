//! Local media authority originates at the real launcher handoff, never wire JSON.
#[cfg(target_os = "linux")]
use super::AsRawFd;
use super::{Arc, AtomicBool, CapnpBrokerState, CapnpChannelHandle, Ordering, Rc};
use runtime_collab::native_media::{Request, Snapshot};

/// Immutable route plus a live connection/process/admission check. There is no
/// public constructor: only this broker's admitted channel can produce one.
#[derive(Clone)]
pub struct NativeMediaRequestContext {
    peer_id: u64,
    app_id: String,
    workspace_id: String,
    record_id: String,
    revalidate: Arc<dyn Fn() -> Result<(), String> + Send + Sync>,
}

impl NativeMediaRequestContext {
    pub fn peer_id(&self) -> u64 {
        self.peer_id
    }
    pub fn app_id(&self) -> &str {
        &self.app_id
    }
    pub fn workspace_id(&self) -> &str {
        &self.workspace_id
    }
    pub fn record_id(&self) -> &str {
        &self.record_id
    }
    /// Re-checks that the admitted connection, process and route are still live.
    ///
    /// # Errors
    ///
    /// Returns a message once the connection, process or admission lapsed.
    pub fn revalidate(&self) -> Result<(), String> {
        (self.revalidate)()
    }
}

pub type NativeMediaFuture =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<Snapshot, String>> + Send>>;

pub trait NativeMediaServices: Send + Sync {
    fn command(&self, context: NativeMediaRequestContext, request: Request) -> NativeMediaFuture;
    /// Must fence pending work and stop local capture before returning.
    fn disconnect(&self, peer_id: u64);
}

/// Exact connected peer, retained independently from the launch ticket's TTL.
pub(super) struct LocalAppConnection {
    pub(super) app_id: String,
    pub(super) workspace_id: String,
    process: PeerProcess,
    connected: AtomicBool,
}

#[cfg(target_os = "linux")]
#[derive(Clone)]
pub(super) struct PeerProcess(Arc<std::os::fd::OwnedFd>);
#[cfg(not(target_os = "linux"))]
pub(super) struct PeerProcess;

impl PeerProcess {
    #[cfg(target_os = "linux")]
    pub(super) fn from_socket(stream: &tokio::net::UnixStream) -> Option<Self> {
        use std::os::fd::FromRawFd;
        let mut fd: libc::c_int = -1;
        let mut len = libc::socklen_t::try_from(std::mem::size_of_val(&fd)).ok()?;
        // SAFETY: live connected socket; writable correctly sized output. The
        // kernel returns a new owned pidfd for the socket's actual peer task.
        let result = unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERPIDFD,
                (&raw mut fd).cast(),
                &raw mut len,
            )
        };
        if result != 0 || fd < 0 {
            return None;
        }
        // SAFETY: successful SO_PEERPIDFD transfers this fresh descriptor.
        let owned = unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) };
        if len as usize != std::mem::size_of::<libc::c_int>() {
            return None;
        }
        let process = Self(Arc::new(owned));
        process.alive().then_some(process)
    }
    #[cfg(not(target_os = "linux"))]
    pub(super) fn from_socket(_: &tokio::net::UnixStream) -> Option<Self> {
        None
    }

    #[cfg(target_os = "linux")]
    fn alive(&self) -> bool {
        let mut poll = libc::pollfd {
            fd: self.0.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one initialized pollfd, owned pidfd, nonblocking query.
        unsafe { libc::poll(&raw mut poll, 1, 0) == 0 }
    }
    #[cfg(not(target_os = "linux"))]
    #[expect(
        clippy::unused_self,
        reason = "mirrors the Linux pidfd check, which reads this process handle"
    )]
    fn alive(&self) -> bool {
        false
    }
}

#[cfg(target_os = "linux")]
impl AsRawFd for PeerProcess {
    fn as_raw_fd(&self) -> std::os::fd::RawFd {
        self.0.as_raw_fd()
    }
}

impl LocalAppConnection {
    pub(super) fn attested(
        app_id: String,
        workspace_id: String,
        process: PeerProcess,
    ) -> Option<Arc<Self>> {
        process.alive().then(|| {
            Arc::new(Self {
                app_id,
                workspace_id,
                process,
                connected: AtomicBool::new(true),
            })
        })
    }
    pub(super) fn current(&self) -> bool {
        self.connected.load(Ordering::Acquire) && self.process.alive()
    }
    pub(super) fn stop(&self) {
        self.connected.store(false, Ordering::Release);
    }
}

/// Owns the death watcher only for the live RPC connection; no orphan task can
/// retain the broker after a normal disconnect.
pub(super) struct ProcessMonitor {
    local: Option<Arc<LocalAppConnection>>,
    task: Option<tokio::task::JoinHandle<()>>,
}
impl ProcessMonitor {
    pub(super) fn start(
        local: Option<Arc<LocalAppConnection>>,
        state: &Arc<CapnpBrokerState>,
        peer: u64,
    ) -> Self {
        #[cfg_attr(
            not(target_os = "linux"),
            expect(unused_mut, reason = "only Linux monitors a local process")
        )]
        let mut task = None;
        #[cfg(target_os = "linux")]
        if let Some(connection) = local.as_ref() {
            let checked = Arc::clone(connection);
            let state = Arc::downgrade(state);
            let stop: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
                checked.stop();
                if let Some(state) = state.upgrade()
                    && let Some(service) = state.services.native_media.as_ref()
                {
                    service.disconnect(peer);
                }
            });
            match watch_death(connection.process.clone(), stop) {
                Ok(watcher) => task = Some(watcher),
                Err(_) => connection.stop(),
            }
        }
        #[cfg(not(target_os = "linux"))]
        let _ = (state, peer);
        Self { local, task }
    }
}
#[cfg(target_os = "linux")]
fn watch_death(
    process: PeerProcess,
    stop: Arc<dyn Fn() + Send + Sync>,
) -> std::io::Result<tokio::task::JoinHandle<()>> {
    let process = tokio::io::unix::AsyncFd::new(process)?;
    Ok(tokio::spawn(async move {
        // Death and monitor failure both revoke before the callback returns.
        let _ = process.readable().await;
        stop();
    }))
}

impl Drop for ProcessMonitor {
    fn drop(&mut self) {
        if let Some(local) = &self.local {
            local.stop();
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

impl CapnpChannelHandle {
    #[expect(
        clippy::suspicious_operation_groupings,
        reason = "a route's cluster is the connection's workspace; the names differ by layer"
    )]
    fn native_media_context(
        &self,
        stopping: bool,
    ) -> Result<NativeMediaRequestContext, capnp::Error> {
        let local = self
            .lease
            .as_ref()
            .and_then(|lease| lease.local_media.clone())
            .ok_or_else(|| {
                capnp::Error::failed("local media requires a live launcher-attested process".into())
            })?;
        let route = self.route();
        if self.verified_identity.agent_id.is_some()
            || self.verified_identity.host_scope.is_some()
            || !local.current()
            || route.app_id != local.app_id
            || route.cluster_id != local.workspace_id
        {
            return Err(capnp::Error::failed(
                "local media route or process unavailable".into(),
            ));
        }
        if !stopping {
            self.state
                .revalidate_admission(&route, &self.verified_identity)?;
        }
        let state = Arc::downgrade(&self.state);
        let admitted = self.verified_identity.clone();
        let checked_route = route.clone();
        let revalidate = Arc::new(move || {
            if !local.current() {
                return Err("local media process ended".into());
            }
            state
                .upgrade()
                .ok_or_else(|| "local media broker ended".to_string())?
                .revalidate_admission(&checked_route, &admitted)
                .map(|_| ())
                .map_err(|_| "local media admission ended".into())
        });
        Ok(NativeMediaRequestContext {
            peer_id: self.peer_id,
            app_id: route.app_id,
            workspace_id: route.cluster_id,
            record_id: route.record_id,
            revalidate,
        })
    }

    pub(super) fn native_media_dispatch(
        self: Rc<Self>,
        bytes: &[u8],
    ) -> Result<NativeMediaFuture, capnp::Error> {
        let request = Request::decode(bytes).map_err(|e| capnp::Error::failed(e.into()))?;
        if !request.dispatch_valid() {
            return Err(capnp::Error::failed("invalid native media intent".into()));
        }
        let stopping = matches!(
            request.command,
            runtime_collab::native_media::Command::Leave
                | runtime_collab::native_media::Command::SetMuted { muted: true }
        );
        let context = self.native_media_context(stopping)?;
        let service = self
            .state
            .services
            .native_media
            .as_ref()
            .ok_or_else(|| capnp::Error::failed("native media unavailable".into()))?;
        Ok(service.command(context, request))
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests;
