//! Canonical owner custody for an actual launcher-owned agent session.
use launcher::managed::{
    ManagedBinding, ManagedLaunchAuthority, ManagedLaunchScope, ManagedRuntimeFacts,
    VerifiedManagedProcess, VerifiedManagedRuntime, WORKSPACE_AGENT_PACKAGE_ID,
    channel::ManagedProcessAuthority,
};
use std::{sync::OnceLock, time::SystemTime};
use tilo_joined_room::{AgentSessionBinding, AgentSessionReservation, NativeWorkspaceOwner};
use tilo_user_identity::DeviceCert;

/// Independently configured host inputs, not installed manifest claims.
pub struct NativeAgentInstall {
    pub package: launcher::managed::InstalledManagedPackage,
    pub trust: launcher::managed::PackageTrust,
    pub runtime: launcher::managed::ManagedRuntime,
}

impl NativeAgentInstall {
    /// Resolve the fixed agent through the ordinary installed-app catalog.
    /// Release trust is loaded independently; neither the registry publisher
    /// string nor the installed package's embedded key can grant trust.
    /// Run this on the host launch worker: it verifies actual installed bytes.
    ///
    /// # Errors
    /// Refuses an unavailable, unsigned, untrusted or damaged installation.
    pub fn from_host_catalog(
        catalog: &package_store::Catalog,
        options: &launcher::run::RunOptions,
        sandbox_shim: std::path::PathBuf,
        bwrap: std::path::PathBuf,
    ) -> Result<Self, String> {
        let installed = catalog
            .find(WORKSPACE_AGENT_PACKAGE_ID)
            .map_err(|error| error.to_string())?
            .ok_or("workspace agent is not installed")?;
        let id = installed
            .package_id()
            .ok_or("workspace agent has no original signed package identity")?;
        let data_root = if installed.system {
            catalog
                .system()
                .ok_or("workspace agent system store is unavailable")?
                .data_dir
                .clone()
        } else {
            catalog.user().root().to_owned()
        };
        let trust = launcher::managed::PackageTrust::from_user_config()?;
        let package =
            launcher::managed::InstalledManagedPackage::verify(installed, data_root, id, &trust)?;
        let manifest = package.revalidate(&trust)?;
        let runtime = launcher::managed::ManagedRuntime::from_launch_options(
            &manifest,
            options,
            sandbox_shim,
            bwrap,
        )?;
        Ok(Self {
            package,
            trust,
            runtime,
        })
    }
}

/// Original authenticated person request retained by the native room owner.
pub struct NativeAgentStart {
    pub room: tilo_desktop_model::RoomId,
    pub caller: DeviceCert,
    pub agent: String,
}

/// Host-selected service locations. The managed child receives descriptors,
/// never these socket paths, provider credentials or an identity-file path.
#[cfg(target_os = "linux")]
pub struct NativeAgentServiceEndpoints {
    pub identity: std::path::PathBuf,
    pub model_gateway: std::path::PathBuf,
    pub data_home: std::path::PathBuf,
    pub provider: launcher::services::ManagedProviderSelection,
}

/// A Stop request for one owning session. It interrupts startup I/O but is
/// not a claim that the process has stopped; only the owning worker can
/// confirm kernel shutdown and canonical retirement.
#[cfg(target_os = "linux")]
#[derive(Clone, Default)]
pub struct NativeAgentCancellation(
    std::sync::Arc<std::sync::Mutex<CancellationState>>,
    std::sync::Arc<tokio::sync::Notify>,
);

#[cfg(target_os = "linux")]
#[derive(Default)]
struct CancellationState {
    requested: bool,
    channel: Option<launcher::managed::channel::ManagedConnectionCancellation>,
}

#[cfg(target_os = "linux")]
impl NativeAgentCancellation {
    /// Request Stop and wake this session's in-flight descriptor challenge.
    ///
    /// # Errors
    /// Reports poisoned custody or a kernel socket-shutdown failure.
    pub fn cancel(&self) -> Result<(), String> {
        let mut state = self.0.lock().map_err(|_| "agent cancellation poisoned")?;
        state.requested = true;
        self.1.notify_waiters();
        let result = state.channel.as_ref().map_or(Ok(()), |channel| {
            channel.cancel().map_err(|error| error.to_string())
        });
        drop(state);
        result
    }

    fn check(&self) -> Result<(), String> {
        if self
            .0
            .lock()
            .map_err(|_| "agent cancellation poisoned")?
            .requested
        {
            Err("native agent Stop was requested".into())
        } else {
            Ok(())
        }
    }

    fn retain(
        &self,
        channel: launcher::managed::channel::ManagedConnectionCancellation,
    ) -> Result<(), String> {
        let mut state = self.0.lock().map_err(|_| "agent cancellation poisoned")?;
        if state.requested {
            drop(state);
            channel.cancel().map_err(|error| error.to_string())?;
            return Err("native agent Stop was requested".into());
        }
        state.channel = Some(channel);
        drop(state);
        Ok(())
    }

    async fn stopped(&self) -> String {
        loop {
            // Register before checking the sticky state: Stop between that
            // check and await must not be lost.
            let notified = self.1.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Err(error) = self.check() {
                return error;
            }
            notified.await;
        }
    }

    async fn connect_service(
        &self,
        path: &std::path::Path,
    ) -> Result<std::os::unix::net::UnixStream, String> {
        self.check()?;
        let connection = tokio::select! {
            biased;
            error = self.stopped() => return Err(error),
            result = tokio::net::UnixStream::connect(path) => result.map_err(|error| error.to_string())?,
            () = tokio::time::sleep(std::time::Duration::from_secs(5)) => {
                return Err("agent service connection timed out".into());
            }
        };
        self.check()?;
        let connection = connection.into_std().map_err(|error| error.to_string())?;
        connection
            .set_nonblocking(false)
            .map_err(|error| error.to_string())?;
        Ok(connection)
    }
}

/// One idle agent's owning launcher and canonical session custody.
/// Dropping it stops only its own kernel-bound children, never a recovered PID.
#[cfg(target_os = "linux")]
pub struct NativeManagedSession {
    authority: std::sync::Arc<NativeAgentAuthority>,
    launcher: launcher::managed::ManagedLauncher,
    binding: ManagedBinding,
    pending: Option<launcher::managed::channel::PendingManagedConnection>,
    started: bool,
    cancellation: NativeAgentCancellation,
}

#[cfg(target_os = "linux")]
impl NativeManagedSession {
    /// Verify independent package/runtime trust, then durably reserve the session.
    ///
    /// # Errors
    /// Refuses untrusted installs, stale runtime, or unauthorized person requests.
    pub fn new(
        owner: NativeWorkspaceOwner,
        attestor: DeviceCert,
        request: NativeAgentStart,
        install: NativeAgentInstall,
    ) -> Result<Self, String> {
        let authority = std::sync::Arc::new(NativeAgentAuthority::new(owner, attestor));
        let launcher = launcher::managed::ManagedLauncher::new(
            install.package,
            install.trust,
            install.runtime,
            authority.clone(),
        )?;
        let binding = authority.reserve(
            launcher.runtime_lease(),
            request.room,
            request.caller,
            request.agent,
        )?;
        Ok(Self {
            authority,
            launcher,
            binding,
            pending: None,
            started: false,
            cancellation: NativeAgentCancellation::default(),
        })
    }

    pub fn cancellation_handle(&self) -> NativeAgentCancellation {
        self.cancellation.clone()
    }

    pub fn launch_id(&self) -> &str {
        &self.binding.launch
    }

    /// Consume the durable claim and construct services around the exact channel.
    /// Call on the host's launch worker, not the frame thread.
    ///
    /// # Errors
    /// Refuses a second start or any descriptor/runtime/launch mismatch.
    pub fn start(
        &mut self,
        connect: impl FnOnce(
            &ManagedBinding,
            &str,
            &str,
            std::os::unix::net::UnixStream,
        ) -> Result<launcher::services::EnrolledServiceContext, String>,
    ) -> Result<(), String> {
        if self.started {
            return Err("native agent session start already consumed".into());
        }
        self.started = true;
        let result = self.start_once(connect);
        if result.is_err() {
            self.stop()?;
        }
        result
    }

    /// Connect the host's normal scoped identity and model services around
    /// this session's actual private collaboration descriptor. Missing services
    /// fail the durable start; there is no ambient identity-file fallback.
    ///
    /// # Errors
    /// Refuses missing services, scoped handoff failure or a repeated start.
    pub fn start_with_host_services(
        &mut self,
        endpoints: NativeAgentServiceEndpoints,
    ) -> Result<(), String> {
        let cancellation = self.cancellation.clone();
        self.start(move |binding, epoch, channel, collaboration| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_io()
                .enable_time()
                .build()
                .map_err(|error| error.to_string())?;
            let mut identity = runtime
                .block_on(cancellation.connect_service(&endpoints.identity))
                .map_err(|error| format!("agent identity service: {error}"))?;
            identity
                .set_write_timeout(Some(std::time::Duration::from_secs(5)))
                .map_err(|error| error.to_string())?;
            launcher::services::write_identity_handshake(
                &mut identity,
                &binding.app,
                &binding.room,
            )
            .map_err(|error| format!("agent scoped identity handoff: {error}"))?;
            identity
                .set_write_timeout(None)
                .map_err(|error| error.to_string())?;
            let model = runtime
                .block_on(cancellation.connect_service(&endpoints.model_gateway))
                .map_err(|error| format!("agent model service: {error}"))?;
            launcher::services::EnrolledServiceContext::new(
                binding.clone(),
                endpoints.data_home,
                epoch.to_owned(),
                channel.to_owned(),
                endpoints.provider,
                identity,
                collaboration,
                model,
            )
        })
    }

    fn start_once(
        &mut self,
        connect: impl FnOnce(
            &ManagedBinding,
            &str,
            &str,
            std::os::unix::net::UnixStream,
        ) -> Result<launcher::services::EnrolledServiceContext, String>,
    ) -> Result<(), String> {
        self.cancellation.check()?;
        let mut permit = self.launcher.authorize(self.binding.clone())?;
        let (pending, collab) =
            launcher::managed::channel::PendingManagedConnection::pair(&mut permit)?;
        self.cancellation
            .retain(pending.cancellation().map_err(|error| error.to_string())?)?;
        let epoch = self.launcher.runtime_lease().revalidate()?.runtime_epoch;
        let services = connect(&self.binding, &epoch, pending.channel_id(), collab)?;
        self.cancellation.check()?;
        self.launcher.launch(permit, services)?;
        self.pending = Some(pending);
        Ok(())
    }

    /// Take only a kernel-admitted connection for this session's actual child.
    /// The descriptor challenge can wait up to ten seconds; run on a launch worker.
    ///
    /// # Errors
    /// Refuses changed process custody or a failed durable process binding.
    pub fn poll_connection(
        &mut self,
    ) -> Result<Option<launcher::managed::AdmittedManagedConnection>, String> {
        if !self.started {
            return Err("native agent session has not started".into());
        }
        if let Err(error) = self.cancellation.check() {
            self.stop()?;
            return Err(error);
        }
        if let Err(error) = self.authority.revalidate_launch(&self.binding) {
            // Presence and an earlier handshake cannot keep a revoked agent
            // running. Retire only this worker's exact owned session.
            self.stop()?;
            return Err(error);
        }
        let mut admitted = None;
        for event in self.launcher.drain_events()? {
            match event {
                launcher::managed::ManagedLaunchEvent::Started(supervisor) => {
                    let pending = self
                        .pending
                        .take()
                        .ok_or("native agent channel already consumed")?;
                    match pending.admit_connection(&supervisor, self.authority.as_ref()) {
                        Ok(connection) => {
                            self.authority.revalidate_process_bound(&self.binding)?;
                            if let Err(error) = self.cancellation.check() {
                                self.stop()?;
                                return Err(error);
                            }
                            admitted = Some(connection);
                        }
                        Err(error) => {
                            self.stop()?;
                            return Err(error);
                        }
                    }
                }
                launcher::managed::ManagedLaunchEvent::Finished { .. } => {
                    self.pending = None;
                    self.authority.close()?;
                    return Err("native agent process exited".into());
                }
                launcher::managed::ManagedLaunchEvent::Failed { reason, .. } => {
                    self.stop()?;
                    return Err(reason);
                }
            }
        }
        Ok(admitted)
    }

    /// Deliver a newly verified connection into the same desktop action broker.
    /// `true` means only that this poll adopted a descriptor, not agent presence:
    /// the agent still must authenticate and subscribe through its channel.
    ///
    /// # Errors
    /// Refuses stale process, host or broker custody and stops the owned session.
    pub fn poll_broker_attachment(
        &mut self,
        broker: &tilo_action_broker::collab_broker::AppActionBroker,
    ) -> Result<bool, String> {
        let Some(connection) = self.poll_connection()? else {
            return Ok(false);
        };
        if let Err(error) = broker.adopt_managed_connection(connection, &self.authority.attestor) {
            self.stop()?;
            return Err(error);
        }
        Ok(true)
    }

    /// Stop exact owned processes before retiring durable custody.
    ///
    /// # Errors
    /// A failed kernel stop or publication remains an uncertain retained session.
    pub fn stop(&mut self) -> Result<(), String> {
        let cancelled = self.cancellation.cancel();
        self.launcher.shutdown()?;
        self.pending = None;
        self.authority.close()?;
        cancelled
    }
}

#[cfg(target_os = "linux")]
impl Drop for NativeManagedSession {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

struct RetainedSession {
    binding: AgentSessionBinding,
    runtime: VerifiedManagedRuntime,
    facts: ManagedRuntimeFacts,
}

/// Installed host callbacks, with no wire-deserializable authority constructor.
/// Create this before the launcher, then reserve using that launcher's live lease.
pub struct NativeAgentAuthority {
    owner: NativeWorkspaceOwner,
    attestor: DeviceCert,
    retained: OnceLock<RetainedSession>,
}

impl NativeAgentAuthority {
    pub fn new(owner: NativeWorkspaceOwner, attestor: DeviceCert) -> Self {
        Self {
            owner,
            attestor,
            retained: OnceLock::new(),
        }
    }

    /// Reserve the actual verified runtime in the canonical owner's durable store.
    ///
    /// # Errors
    /// Refuses repeated initialization, stale runtime, or inadmissible authority.
    pub fn reserve(
        &self,
        runtime: VerifiedManagedRuntime,
        room: tilo_desktop_model::RoomId,
        caller: DeviceCert,
        agent: String,
    ) -> Result<ManagedBinding, String> {
        if self.retained.get().is_some() {
            return Err("native agent authority already initialized".into());
        }
        let facts = runtime.revalidate()?;
        if facts.package_id != WORKSPACE_AGENT_PACKAGE_ID {
            return Err("native agent requires the fixed signed agent package".into());
        }
        let binding = self.owner.reserve_agent_session(
            AgentSessionReservation {
                room,
                caller,
                attestor: self.attestor.clone(),
                agent,
                launch: uuid::Uuid::new_v4().to_string(),
                runtime_epoch: facts.runtime_epoch.clone(),
                runtime_digest: runtime_digest(&facts),
            },
            SystemTime::now(),
        )?;
        let managed = managed_binding(&binding);
        self.retained
            .set(RetainedSession {
                binding: binding.clone(),
                runtime,
                facts,
            })
            .map_err(|_| {
                let _ = self
                    .owner
                    .close_agent_session(&binding, &self.attestor, SystemTime::now());
                "native agent authority initialization raced".to_string()
            })?;
        Ok(managed)
    }

    fn current(&self, binding: &ManagedBinding) -> Result<&RetainedSession, String> {
        binding.validate()?;
        let retained = self
            .retained
            .get()
            .ok_or("native agent runtime is not reserved")?;
        if managed_binding(&retained.binding) != *binding
            || retained.runtime.revalidate()? != retained.facts
        {
            return Err("native agent launch binding or runtime changed".into());
        }
        Ok(retained)
    }

    /// Retire retained custody after the owning process has been stopped.
    ///
    /// # Errors
    /// Refuses absent custody or a failed canonical publication.
    pub fn close(&self) -> Result<(), String> {
        let retained = self
            .retained
            .get()
            .ok_or("native agent runtime is not reserved")?;
        self.owner
            .close_agent_session(&retained.binding, &self.attestor, SystemTime::now())
    }
}

impl ManagedLaunchAuthority for NativeAgentAuthority {
    fn claim_launch(&self, binding: &ManagedBinding) -> Result<(), String> {
        let retained = self.current(binding)?;
        self.owner
            .claim_agent_session(&retained.binding, &self.attestor, SystemTime::now())
    }

    fn revalidate_launch(&self, binding: &ManagedBinding) -> Result<(), String> {
        let retained = self.current(binding)?;
        self.owner
            .revalidate_agent_session(&retained.binding, &self.attestor, SystemTime::now())
    }
}

impl NativeAgentAuthority {
    fn revalidate_process_bound(&self, binding: &ManagedBinding) -> Result<(), String> {
        let retained = self.current(binding)?;
        self.owner.revalidate_bound_agent_session(
            &retained.binding,
            &self.attestor,
            SystemTime::now(),
        )
    }
}

impl ManagedProcessAuthority for NativeAgentAuthority {
    fn bind_process(&self, process: &VerifiedManagedProcess) -> Result<(), String> {
        let facts = process.facts()?;
        let retained = self.current(&facts.binding)?;
        if facts.runtime != retained.facts {
            return Err("native agent process runtime differs from reservation".into());
        }
        let mut digest = blake3::Hasher::new();
        digest.update(b"tilo.native-agent-process.v1");
        digest.update(&runtime_digest(&facts.runtime));
        for field in [&facts.binding.launch, &facts.channel_id] {
            digest.update(&(field.len() as u64).to_le_bytes());
            digest.update(field.as_bytes());
        }
        for identity in [facts.process, facts.supervisor] {
            digest.update(&identity.pid.to_le_bytes());
            digest.update(&identity.start.to_le_bytes());
        }
        self.owner.bind_agent_session_process(
            &retained.binding,
            &self.attestor,
            *digest.finalize().as_bytes(),
            SystemTime::now(),
        )
    }
}

fn managed_binding(binding: &AgentSessionBinding) -> ManagedBinding {
    ManagedBinding {
        room: binding.room.to_string(),
        app: WORKSPACE_AGENT_PACKAGE_ID.into(),
        agent: binding.agent.clone(),
        scope: ManagedLaunchScope::AgentSession,
        launch: binding.launch.clone(),
        generation: binding.generation,
    }
}

fn runtime_digest(facts: &ManagedRuntimeFacts) -> [u8; 32] {
    let mut digest = blake3::Hasher::new();
    digest.update(b"tilo.native-agent-runtime.v1");
    for field in [
        &facts.runtime_epoch,
        &facts.package_id,
        &facts.package_hash,
        &facts.executable_hash,
        &facts.runtime_hash,
    ] {
        digest.update(&(field.len() as u64).to_le_bytes());
        digest.update(field.as_bytes());
    }
    *digest.finalize().as_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn stop_wakes_registered_waiters_only_for_its_session() {
        use std::{
            future::Future,
            task::{Context, Poll, Waker},
        };
        let first = NativeAgentCancellation::default();
        let other = NativeAgentCancellation::default();
        let mut first_wait = Box::pin(first.stopped());
        let mut other_wait = Box::pin(other.stopped());
        let mut context = Context::from_waker(Waker::noop());
        assert!(first_wait.as_mut().poll(&mut context).is_pending());
        assert!(other_wait.as_mut().poll(&mut context).is_pending());
        first.cancel().unwrap();
        assert_eq!(
            first_wait.as_mut().poll(&mut context),
            Poll::Ready("native agent Stop was requested".into())
        );
        assert!(other_wait.as_mut().poll(&mut context).is_pending());
        assert!(other.check().is_ok());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn host_service_connection_is_real_and_stop_refuses_another_connection() {
        use std::io::{Read, Write};
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("identity.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .unwrap();
        let control = NativeAgentCancellation::default();
        let mut client = runtime.block_on(control.connect_service(&path)).unwrap();
        let (mut peer, _) = listener.accept().unwrap();
        client.write_all(b"scoped").unwrap();
        let mut actual = [0; 6];
        peer.read_exact(&mut actual).unwrap();
        assert_eq!(&actual, b"scoped");
        control.cancel().unwrap();
        assert!(runtime.block_on(control.connect_service(&path)).is_err());
        listener.set_nonblocking(true).unwrap();
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn full_host_service_backlog_does_not_block_stop() {
        use std::os::fd::AsRawFd;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("busy-model.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        // SAFETY: this test owns a live listening Unix descriptor. Reducing
        // its backlog changes only this isolated temporary service.
        assert_eq!(unsafe { libc::listen(listener.as_raw_fd(), 0) }, 0);
        let _occupant = std::os::unix::net::UnixStream::connect(&path).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .unwrap();
        let control = NativeAgentCancellation::default();
        let stop = control.clone();
        let requester = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(50));
            stop.cancel().unwrap();
        });
        let began = std::time::Instant::now();
        assert!(runtime.block_on(control.connect_service(&path)).is_err());
        assert!(began.elapsed() < std::time::Duration::from_secs(2));
        requester.join().unwrap();
        listener.set_nonblocking(true).unwrap();
        let _original = listener.accept().unwrap();
        assert_eq!(
            listener.accept().unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cancellation_request_is_sticky_and_session_scoped() {
        let first = NativeAgentCancellation::default();
        let retained = first.clone();
        let second = NativeAgentCancellation::default();
        first.check().unwrap();
        first.cancel().unwrap();
        assert!(retained.check().is_err());
        retained.cancel().unwrap();
        assert!(first.check().is_err(), "Stop cannot be cleared by retry");
        second.check().unwrap();
    }

    #[test]
    fn runtime_custody_digest_binds_every_verified_component() {
        let facts = ManagedRuntimeFacts {
            runtime_epoch: "epoch".into(),
            package_id: WORKSPACE_AGENT_PACKAGE_ID.into(),
            package_hash: "package".into(),
            executable_hash: "executable".into(),
            runtime_hash: "runtime".into(),
        };
        let original = runtime_digest(&facts);
        for index in 0..5 {
            let mut changed = facts.clone();
            let fields = [
                &mut changed.runtime_epoch,
                &mut changed.package_id,
                &mut changed.package_hash,
                &mut changed.executable_hash,
                &mut changed.runtime_hash,
            ];
            fields.into_iter().nth(index).unwrap().push('x');
            assert_ne!(runtime_digest(&changed), original);
        }
        let mut left = facts.clone();
        left.package_hash = "ab".into();
        left.executable_hash = "c".into();
        let mut right = facts;
        right.package_hash = "a".into();
        right.executable_hash = "bc".into();
        assert_ne!(runtime_digest(&left), runtime_digest(&right));
    }

    #[test]
    fn idle_session_binding_never_fabricates_a_task_scope() {
        let retained = AgentSessionBinding {
            room: tilo_desktop_model::RoomId::parse("00000000-0000-4000-8000-000000000001")
                .unwrap(),
            agent: "scribe".into(),
            launch: "00000000-0000-4000-8000-000000000002".into(),
            runtime_epoch: "00000000-0000-4000-8000-000000000003".into(),
            runtime_digest: [1; 32],
            generation: 7,
        };
        let managed = managed_binding(&retained);
        managed.validate().unwrap();
        assert_eq!(managed.scope, ManagedLaunchScope::AgentSession);
        assert_eq!(managed.generation, retained.generation);
        assert_eq!(managed.launch, retained.launch);
        assert_eq!(managed.agent, retained.agent);
    }

    #[test]
    fn uninitialized_host_callback_cannot_claim_a_native_session() {
        let (_directory, owner, room, _, _) =
            crate::shell::native_workspace_owner::test_enrolled_owner();
        let (attestor, before) = owner
            .with_current(room, |view| Ok((view.device().clone(), view.generation())))
            .unwrap();
        let authority = NativeAgentAuthority::new(owner.clone(), attestor);
        let binding = ManagedBinding {
            room: room.to_string(),
            app: WORKSPACE_AGENT_PACKAGE_ID.into(),
            agent: "scribe".into(),
            scope: ManagedLaunchScope::AgentSession,
            launch: uuid::Uuid::new_v4().to_string(),
            generation: before,
        };
        assert!(authority.claim_launch(&binding).is_err());
        assert!(authority.revalidate_launch(&binding).is_err());
        assert!(authority.close().is_err());
        assert_eq!(
            owner
                .with_current(room, |view| Ok(view.generation()))
                .unwrap(),
            before,
            "a descriptive binding must not create or consume canonical custody"
        );
    }
}
