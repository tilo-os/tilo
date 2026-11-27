//! The app process a connection was admitted for, pinned by the kernel, and the media context
//! that process reaches its device's services through.
//!
//! A launcher hands an app's connection over with the pidfds that pin it, so the daemon knows
//! the exact process behind the socket and fences its services when it dies, without trusting a
//! namespace-local pid. The media context re-checks that the same process is still the app's
//! before every command it forwards.

use super::media::MediaRequestContext;
use super::{AppBrokerState, Arc, AtomicBool, JoinedChannel, Ordering};
use crate::BrokerError;

/// Exact connected peer, retained independently from the launch ticket's TTL.
pub(super) struct LocalAppConnection {
    pub(super) app_id: String,
    pub(super) workspace_id: String,
    pub(super) continuation_instance: Option<u64>,
    /// An instance of the app the host runs with no window
    /// ([`crate::services::AuthenticatedApplicationLaunch::windowless`]).
    pub(super) windowless: bool,
    /// The caller the host opened this instance's view for
    /// ([`crate::services::AuthenticatedApplicationLaunch::opened_for`]).
    pub(super) opened_for: Option<tilo_actions::remote::Caller>,
    /// The certificate the link that opened that view verified
    /// ([`crate::services::AuthenticatedApplicationLaunch::opened_with`]).
    pub(super) opened_with: Option<tilo_user_identity::DeviceCert>,
    /// The workspace agent the launch named, which the app acts as; `None` for the person.
    pub(super) member: Option<String>,
    process: PeerProcess,
    /// What Files hands over on its connection's descriptor stream: the only
    /// app whose selected objects this broker takes. Other apps' descriptors
    /// are closed on receipt.
    pub(super) descriptors: Option<runtime_portal::descriptors::ReceivedDescriptors>,
    connected: AtomicBool,
}

/// The only app that hands this broker selected objects.
use tilo_app_id::apps::FILE_EXPLORER as FILES;

/// Selected objects one Files connection may have handed over and not yet
/// named: two whole captures, each its sources and one destination.
const MAX_HANDED_DESCRIPTORS: usize = 2 * (runtime_collab::file_clipboard::MAX_ENTRIES + 1);

/// The app's process, pinned by the system (`tilo_socket::peer`), beneath the launcher that
/// handed it over when one did.
#[derive(Clone)]
pub(super) struct PeerProcess {
    task: tilo_socket::peer::Pinned,
    launcher: Option<tilo_socket::peer::Pinned>,
}

impl PeerProcess {
    /// The process at the other end of `stream`, while it runs.
    pub(super) fn from_socket(stream: &tilo_socket::tokio::Stream) -> Option<Self> {
        let task = tilo_failure::ignored!(
            "a peer the system pins no process for is admitted as no app, which refuses its \
             records",
            tilo_socket::peer::Pinned::of_tokio(stream).ok()
        )?;
        let process = Self {
            task,
            launcher: None,
        };
        process.alive().then_some(process)
    }
    /// The process a shell pinned and handed over with an app's connection (`task`), beneath the
    /// launcher that handed the app over (`launcher`), when one did: both pidfds.
    #[cfg(target_os = "linux")]
    pub(super) fn handed(
        task: std::os::fd::OwnedFd,
        launcher: Option<std::os::fd::OwnedFd>,
    ) -> Option<Self> {
        let pin = |pidfd| {
            tilo_failure::ignored!(
                "a handed process that has ended is refused as ended",
                tilo_socket::peer::Pinned::from_pidfd(pidfd).ok()
            )
        };
        let process = Self {
            task: pin(task)?,
            launcher: match launcher {
                Some(launcher) => Some(pin(launcher)?),
                None => None,
            },
        };
        process.alive().then_some(process)
    }
    /// Only Linux's shells hand the daemon an app's pinned process.
    #[cfg(not(target_os = "linux"))]
    pub(super) fn handed(
        _: runtime_portal::descriptors::Owned,
        _: Option<runtime_portal::descriptors::Owned>,
    ) -> Option<Self> {
        None
    }
    /// Copies of the pidfds pinning this process and its launcher: what a shell hands the
    /// daemon with the app's connection.
    #[cfg(target_os = "linux")]
    pub(super) fn pidfds(
        &self,
    ) -> std::io::Result<(std::os::fd::OwnedFd, Option<std::os::fd::OwnedFd>)> {
        Ok((
            self.task.pidfd().try_clone_to_owned()?,
            self.launcher
                .as_ref()
                .map(|launcher| launcher.pidfd().try_clone_to_owned())
                .transpose()?,
        ))
    }
    #[cfg(target_os = "linux")]
    pub(super) fn with_executing(
        self,
        peer: &runtime_portal::process_binding::ExecutingPeer,
    ) -> Option<Self> {
        let task = peer.pidfd().try_clone_to_owned().ok()?;
        let process = Self {
            task: tilo_failure::ignored!(
                "an executing task that has ended is refused as ended",
                tilo_socket::peer::Pinned::from_pidfd(task).ok()
            )?,
            launcher: Some(self.task),
        };
        process.alive().then_some(process)
    }
    /// Resolves when this task, or the launch it runs beneath, has exited, or when either
    /// cannot be watched.
    pub(super) async fn exited(&self) {
        match &self.launcher {
            Some(launcher) => tokio::select! {
                () = self.task.exited() => {},
                () = launcher.exited() => {},
            },
            None => self.task.exited().await,
        }
    }

    pub(super) fn alive(&self) -> bool {
        self.task.alive()
            && self
                .launcher
                .as_ref()
                .is_none_or(tilo_socket::peer::Pinned::alive)
    }
}

impl LocalAppConnection {
    #[cfg(test)]
    pub(super) fn attested(
        app_id: String,
        workspace_id: String,
        process: PeerProcess,
        launch: crate::services::AuthenticatedApplicationLaunch,
    ) -> Option<Arc<Self>> {
        Self::attested_keeping((app_id, workspace_id, None), process, launch)
    }
    /// The attested launch of `app_id` in `workspace_id`, acting as `member` (an agent of the
    /// workspace) when the launch named one.
    pub(super) fn attested_keeping(
        (app_id, workspace_id, member): (String, String, Option<String>),
        process: PeerProcess,
        launch: crate::services::AuthenticatedApplicationLaunch,
    ) -> Option<Arc<Self>> {
        process.alive().then(|| {
            Arc::new(Self {
                descriptors: (app_id == FILES).then(|| {
                    runtime_portal::descriptors::ReceivedDescriptors::new(MAX_HANDED_DESCRIPTORS)
                }),
                app_id,
                workspace_id,
                continuation_instance: launch.continuation_instance,
                windowless: launch.windowless,
                opened_for: launch.opened_for,
                opened_with: launch.opened_with,
                member,
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
            Self::attested_keeping(
                (handoff.app_id, handoff.workspace_id, handoff.member),
                process,
                launch,
            )
        })
    }
    /// Copies of the pidfds pinning the app's process, and its launcher's when the launcher
    /// handed it over: what the seat hands the daemon with the app's connection. `None` where the
    /// kernel pins no process.
    #[cfg(unix)]
    #[cfg_attr(
        not(target_os = "linux"),
        expect(
            clippy::unused_self,
            reason = "only Linux's kernel pins a process the app has"
        )
    )]
    pub(super) fn pidfds(
        &self,
    ) -> Option<(
        runtime_portal::descriptors::Owned,
        Option<runtime_portal::descriptors::Owned>,
    )> {
        #[cfg(target_os = "linux")]
        {
            self.process.pidfds().ok()
        }
        #[cfg(not(target_os = "linux"))]
        {
            None
        }
    }
    pub(super) fn current(&self) -> bool {
        self.connected.load(Ordering::Acquire) && self.process.alive()
    }
    /// Whether the process another part of the system names by `pid` and `start` is the one
    /// this connection pinned, still running: never a pid alone, which another process may take.
    pub(super) fn matches_process(&self, pid: u32, start: u64) -> bool {
        self.current() && self.process.task.is(pid, start)
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
        state: &Arc<AppBrokerState>,
        peer: u64,
    ) -> Self {
        let mut task = None;
        if let Some(connection) = local.as_ref() {
            let checked = Arc::clone(connection);
            let state = Arc::downgrade(state);
            let stop: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
                checked.stop();
                // Launcher ownership ended: held committed-read waits re-check
                // it now rather than on a clock.
                tilo_identity::policy_change::announce();
                if let Some(state) = state.upgrade() {
                    if let Some(service) = state.services.call_media.as_ref() {
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
            let process = connection.process.clone();
            // Death, and a process whose end cannot be watched, both revoke before the callback
            // returns.
            task = Some(tokio::spawn(async move {
                process.exited().await;
                stop();
            }));
        }
        Self { local, task }
    }
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

impl JoinedChannel {
    #[expect(
        clippy::suspicious_operation_groupings,
        reason = "a route's cluster is the connection's workspace; the names differ by layer"
    )]
    pub(super) fn media_request_context(
        &self,
        stopping: bool,
    ) -> Result<MediaRequestContext, BrokerError> {
        let local = self
            .lease
            .as_ref()
            .and_then(|lease| lease.local_media.clone())
            .ok_or_else(|| {
                BrokerError::failed("local media requires a live launcher-attested process".into())
            })?;
        let route = self.route();
        if self.verified_identity.agent_id.is_some()
            || !local.current()
            || route.app_id != local.app_id
            || route.cluster_id != local.workspace_id
        {
            return Err(BrokerError::failed(
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
        let opened_for = local.opened_for.clone();
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
        Ok(MediaRequestContext {
            peer_id: self.connection_id,
            app_id: route.app_id,
            workspace_id: route.cluster_id,
            document_id: route.document_id,
            opened_for,
            revalidate,
        })
    }

    /// Selected objects the launcher-attested Files process handed over on
    /// its own connection's descriptor stream, taken by descriptor number. The broker
    /// never reaches into the app for a descriptor: that needs ptrace-attach
    /// rights, which a hardened host withholds from everyone.
    pub(super) fn clipboard_descriptor_source(
        &self,
    ) -> Result<crate::services::SelectedDescriptorSource, String> {
        let context = self
            .media_request_context(false)
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

    /// The descriptor stream of this connection's launcher-attested Files process.
    pub(super) fn handed_descriptors(
        &self,
    ) -> Option<runtime_portal::descriptors::ReceivedDescriptors> {
        self.lease
            .as_ref()
            .and_then(|lease| lease.local_media.as_ref())
            .and_then(|local| local.descriptors.clone())
    }
}

#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests;

#[cfg(test)]
mod descriptor_tests {
    /// Selected objects reach this broker only on the app's descriptor stream.
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
