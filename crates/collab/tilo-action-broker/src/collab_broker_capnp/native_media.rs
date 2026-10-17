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
pub type AudioOutputFuture = std::pin::Pin<
    Box<
        dyn std::future::Future<Output = Result<runtime_collab::audio_output::Snapshot, String>>
            + Send,
    >,
>;

pub trait AudioOutputServices: Send + Sync {
    fn output(
        &self,
        context: NativeMediaRequestContext,
        command: runtime_collab::audio_output::Command,
    ) -> AudioOutputFuture;
    /// Fence output belonging to this exact connected process.
    fn disconnect(&self, peer_id: u64);
}

pub trait NativeMediaServices: Send + Sync {
    fn command(&self, context: NativeMediaRequestContext, request: Request) -> NativeMediaFuture;
    /// Must fence pending work and stop local capture before returning.
    fn disconnect(&self, peer_id: u64);
}

/// Exact connected peer, retained independently from the launch ticket's TTL.
pub(super) struct LocalAppConnection {
    pub(super) app_id: String,
    pub(super) workspace_id: String,
    pub(super) creation: Option<crate::services::ApplicationCreationEvidence>,
    pub(super) continuation_instance: Option<u64>,
    process: PeerProcess,
    /// What Files hands over on its connection's descriptor lane: the only
    /// app whose selected objects this broker takes. Other apps' descriptors
    /// are closed on receipt.
    pub(super) descriptors: Option<runtime_portal::descriptor_lane::ReceivedDescriptors>,
    connected: AtomicBool,
}

/// The only app that hands this broker selected objects.
const FILES: &str = "so.tilo.file-explorer";

/// Selected objects one Files connection may have handed over and not yet
/// named: two whole captures, each its sources and one destination.
const MAX_HANDED_DESCRIPTORS: usize = 2 * (runtime_collab::file_clipboard::MAX_ENTRIES + 1);

#[cfg(target_os = "linux")]
#[derive(Clone)]
pub(super) struct PeerProcess {
    task: Arc<std::os::fd::OwnedFd>,
    executing: Option<Arc<runtime_portal::process_binding::ExecutingPeer>>,
    launcher: Option<Arc<std::os::fd::OwnedFd>>,
}
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
        let process = Self {
            task: Arc::new(owned),
            executing: None,
            launcher: None,
        };
        process.alive().then_some(process)
    }
    #[cfg(target_os = "linux")]
    pub(super) fn with_executing(
        self,
        peer: runtime_portal::process_binding::ExecutingPeer,
    ) -> Option<Self> {
        let task = peer.pidfd().try_clone_to_owned().ok()?;
        let process = Self {
            task: Arc::new(task),
            executing: Some(Arc::new(peer)),
            launcher: Some(self.task),
        };
        process.alive().then_some(process)
    }
    /// Resolves when this task, or the launch it runs beneath, has exited.
    /// Pidfd readiness is the notification; it never resolves otherwise, and
    /// is pending forever if the descriptor cannot be watched.
    #[cfg(target_os = "linux")]
    pub(super) async fn exited(&self) {
        let watch = |task: &Arc<std::os::fd::OwnedFd>| {
            task.try_clone().ok().and_then(|task| {
                tokio::io::unix::AsyncFd::with_interest(task, tokio::io::Interest::READABLE).ok()
            })
        };
        let task = watch(&self.task);
        let launcher = self.launcher.as_ref().and_then(watch);
        tokio::select! {
            () = async {
                match &task {
                    Some(task) => { let _ = task.readable().await; }
                    None => std::future::pending::<()>().await,
                }
            } => {},
            () = async {
                match &launcher {
                    Some(launcher) => { let _ = launcher.readable().await; }
                    None => std::future::pending::<()>().await,
                }
            } => {},
        }
    }
    #[cfg(not(target_os = "linux"))]
    pub(super) fn from_socket(_: &tokio::net::UnixStream) -> Option<Self> {
        None
    }

    #[cfg(target_os = "linux")]
    pub(super) fn alive(&self) -> bool {
        let mut poll = libc::pollfd {
            fd: self.task.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one initialized pollfd, owned pidfd, nonblocking query.
        (unsafe { libc::poll(&raw mut poll, 1, 0) == 0 })
            && self.launcher.as_ref().is_none_or(|launcher| {
                let mut poll = libc::pollfd {
                    fd: launcher.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                // SAFETY: another owned launch pidfd and nonblocking query.
                unsafe { libc::poll(&raw mut poll, 1, 0) == 0 }
            })
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
        self.task.as_raw_fd()
    }
}

impl LocalAppConnection {
    pub(super) fn attested(
        app_id: String,
        workspace_id: String,
        process: PeerProcess,
        launch: crate::services::AuthenticatedApplicationLaunch,
    ) -> Option<Arc<Self>> {
        process.alive().then(|| {
            Arc::new(Self {
                descriptors: (app_id == FILES).then(|| {
                    runtime_portal::descriptor_lane::ReceivedDescriptors::new(
                        MAX_HANDED_DESCRIPTORS,
                    )
                }),
                app_id,
                workspace_id,
                creation: launch.creation,
                continuation_instance: launch.continuation_instance,
                process,
                connected: AtomicBool::new(true),
            })
        })
    }
    pub(super) fn from_launch(
        handoff: super::CollabLaunchHandoff,
        process: Option<PeerProcess>,
        launch: crate::services::AuthenticatedApplicationLaunch,
    ) -> Option<Arc<Self>> {
        process.and_then(|process| {
            Self::attested(handoff.app_id, handoff.workspace_id, process, launch)
        })
    }
    pub(super) fn current(&self) -> bool {
        self.connected.load(Ordering::Acquire) && self.process.alive()
    }
    /// Match the task pinned by the kernel pidfd, never a namespace-local app PID.
    #[cfg(target_os = "linux")]
    pub(super) fn matches_process(&self, pid: u32, start: u64) -> bool {
        if !self.current() {
            return false;
        }
        let info = std::fs::read_to_string(format!(
            "/proc/self/fdinfo/{}",
            self.process.task.as_raw_fd()
        ));
        let pinned = info.ok().and_then(|info| {
            info.lines().find_map(|line| {
                line.strip_prefix("Pid:")
                    .and_then(|value| value.trim().parse::<u32>().ok())
            })
        });
        let current_start = std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .ok()
            .and_then(|stat| stat.rsplit_once(") ").map(|(_, fields)| fields.to_owned()))
            .and_then(|fields| fields.split_whitespace().nth(19)?.parse::<u64>().ok());
        pinned == Some(pid) && current_start == Some(start) && self.current()
    }
    #[cfg(not(target_os = "linux"))]
    pub(super) fn matches_process(&self, _: u32, _: u64) -> bool {
        let _ = self;
        false
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
                // Launcher custody ended: held committed-read waits re-check
                // it now rather than on a clock.
                tilo_identity_host::policy_change::announce();
                if let Some(state) = state.upgrade() {
                    if let Some(service) = state.services.native_media.as_ref() {
                        service.disconnect(peer);
                    }
                    if let Some(service) = state.services.file_clipboard.as_ref() {
                        service.disconnect(peer);
                    }
                    if let Some(service) = state.services.audio_output.as_ref() {
                        service.disconnect(peer);
                    }
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
    let launcher = process
        .launcher
        .as_ref()
        .map(|task| {
            tokio::io::unix::AsyncFd::new(PeerProcess {
                task: Arc::clone(task),
                executing: None,
                launcher: None,
            })
        })
        .transpose()?;
    let process = tokio::io::unix::AsyncFd::new(process)?;
    Ok(tokio::spawn(async move {
        // Death and monitor failure both revoke before the callback returns.
        tokio::select! {
            _ = process.readable() => {},
            () = async {
                if let Some(launcher) = launcher { let _ = launcher.readable().await; }
                else { std::future::pending::<()>().await; }
            } => {},
        }
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
    pub(super) fn audio_output_dispatch(
        self: Rc<Self>,
        bytes: &[u8],
    ) -> Result<AudioOutputFuture, capnp::Error> {
        let command = runtime_collab::audio_output::Command::decode(bytes)
            .map_err(|error| capnp::Error::failed(error.into()))?;
        let context = self.native_media_context(matches!(
            command,
            runtime_collab::audio_output::Command::Close { .. }
        ));
        let context = match context {
            Ok(context) => context,
            Err(error) => {
                if let Some(service) = &self.state.services.audio_output {
                    service.disconnect(self.peer_id);
                }
                return Err(error);
            }
        };
        let service = self
            .state
            .services
            .audio_output
            .as_ref()
            .ok_or_else(|| capnp::Error::failed("governed audio output unavailable".into()))?;
        Ok(service.output(context, command))
    }

    #[expect(
        clippy::suspicious_operation_groupings,
        reason = "a route's cluster is the connection's workspace; the names differ by layer"
    )]
    pub(super) fn native_media_context(
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

    /// Selected objects the launcher-attested Files process handed over on
    /// its own connection's descriptor lane, taken by lane number. The broker
    /// never reaches into the app for a descriptor: that needs ptrace-attach
    /// rights, which a hardened host withholds from everyone.
    pub(super) fn clipboard_descriptor_source(
        &self,
    ) -> Result<crate::services::SelectedDescriptorSource, String> {
        let context = self
            .native_media_context(false)
            .map_err(|error| error.to_string())?;
        if context.app_id() != FILES {
            return Err("selected descriptors belong only to Files".into());
        }
        let descriptors = self
            .handed_descriptors()
            .ok_or("launcher-attested Files process unavailable")?;
        Ok(Arc::new(move |descriptor| {
            context.revalidate()?;
            let file = descriptors
                .take(descriptor)
                .map(std::fs::File::from)
                .ok_or("Files did not hand over this selected descriptor")?;
            context.revalidate()?;
            Ok(file)
        }))
    }

    #[cfg(target_os = "linux")]
    pub(super) fn selected_open_peer(
        &self,
    ) -> Result<Arc<runtime_portal::process_binding::ExecutingPeer>, String> {
        self.native_media_context(false)
            .map_err(|error| error.to_string())?
            .revalidate()?;
        let peer = self
            .lease
            .as_ref()
            .and_then(|lease| lease.local_media.as_ref())
            .and_then(|local| local.process.executing.clone())
            .ok_or("actual executing Files peer unavailable")?;
        if !peer.current() {
            return Err("actual executing Files peer ended".into());
        }
        Ok(peer)
    }

    /// The descriptor lane of this connection's launcher-attested Files process.
    pub(super) fn handed_descriptors(
        &self,
    ) -> Option<runtime_portal::descriptor_lane::ReceivedDescriptors> {
        self.lease
            .as_ref()
            .and_then(|lease| lease.local_media.as_ref())
            .and_then(|local| local.descriptors.clone())
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
        let context = self.native_media_context(stopping).inspect_err(|_| {
            tracing::debug!("native media broker refused local context");
        })?;
        let service = self
            .state
            .services
            .native_media
            .as_ref()
            .ok_or_else(|| capnp::Error::failed("native media unavailable".into()))?;
        Ok(service.command(context, request))
    }
}

#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests;

#[cfg(test)]
mod descriptor_tests {
    /// Selected objects reach this broker only on the app's descriptor lane.
    /// Pulling them out of the app's process (a pidfd descriptor grab, ptrace, process
    /// memory access) needs ptrace-attach rights that hardened hosts withhold
    /// from everyone (`kernel.yama.ptrace_scope = 3`), so no broker source may
    /// depend on one.
    #[test]
    fn no_broker_path_reaches_into_an_app_process() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut pending = vec![root];
        let mut offenders = Vec::new();
        let forbidden = [
            ["pidfd", "_getfd"].concat(),
            ["SYS_", "ptrace"].concat(),
            ["libc::", "ptrace"].concat(),
            ["process_vm", "_readv"].concat(),
            ["process_vm", "_writev"].concat(),
        ];
        while let Some(directory) = pending.pop() {
            for entry in std::fs::read_dir(&directory).expect("broker sources") {
                let path = entry.expect("broker source").path();
                if path.is_dir() {
                    pending.push(path);
                } else if path.extension().is_some_and(|extension| extension == "rs") {
                    let source = std::fs::read_to_string(&path).expect("broker source");
                    for call in &forbidden {
                        if source.contains(call.as_str()) {
                            offenders.push(format!("{}: {call}", path.display()));
                        }
                    }
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "ptrace-dependent calls: {offenders:?}"
        );
    }
}
