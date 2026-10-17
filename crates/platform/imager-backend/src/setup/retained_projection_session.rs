//! Root-owned composition of the selected source service, guarded Capnp relay
//! and separately confined target FUSE service. No frontend capability is enabled.
use super::{
    home_capture::TrustedCaptureService,
    installed_retained_actor::InstalledRetainedLaunchInput,
    retained_source_relay::RetainedSourceRelay,
    retained_source_worker::{self, RetainedSourceWorker},
};
use launcher::{
    owned_service::{
        self, ServiceAuthority, ServiceCancellation, ServiceLaunch, ServiceLeaseKey, SupervisedRun,
        TargetProjectionAdmission, TargetProjectionAuthority, TargetProjectionCredentials,
        TargetProjectionEndpoint, TargetProjectionMode,
    },
    run::{IdentitySource, LaunchMode, RunOptions},
};
use runtime_fs::{
    directory_connection::DirectoryConnection, selected_client::SelectedFileConnection,
};
use sha2::{Digest, Sha256};
use std::{
    ffi::{OsStr, OsString},
    fs::File,
    io,
    os::unix::fs::PermissionsExt,
    sync::{Arc, atomic::AtomicBool, mpsc},
    thread::JoinHandle,
    time::Duration,
};

struct Authority {
    input: Arc<InstalledRetainedLaunchInput>,
    cancel: Arc<AtomicBool>,
    source_view: [u8; 32],
    context: [u8; 32],
    name: OsString,
    mode: TargetProjectionMode,
}
impl ServiceAuthority for Authority {
    fn check(&self) -> io::Result<()> {
        self.input.revalidate(&self.cancel)
    }
}
impl TargetProjectionAuthority for Authority {
    fn projection_mode(&self) -> TargetProjectionMode {
        self.mode
    }
    fn target_credentials(&self) -> io::Result<TargetProjectionCredentials> {
        let (uid, gid) = self.input.target_credentials(&self.cancel)?;
        TargetProjectionCredentials::admitted(uid, gid)
    }
    fn context_sha256(&self) -> [u8; 32] {
        self.context
    }
    fn source_view_sha256(&self) -> [u8; 32] {
        self.source_view
    }
    fn presentation_name(&self) -> io::Result<OsString> {
        self.check()?;
        Ok(self.name.clone())
    }
}

enum SourceConnection {
    Selected(SelectedFileConnection),
    Home(DirectoryConnection),
}
impl SourceConnection {
    fn serve(
        self,
        input: Arc<InstalledRetainedLaunchInput>,
        cancel: Arc<AtomicBool>,
        endpoint: Arc<TargetProjectionEndpoint>,
        history: Option<Arc<super::retained_history_read::RootHistoryReadSession>>,
    ) -> io::Result<RetainedSourceRelay> {
        match self {
            Self::Selected(source) => {
                RetainedSourceRelay::serve_projection(source, input, cancel, endpoint)
            }
            Self::Home(source) => match history {
                Some(history) => RetainedSourceRelay::serve_home_projection_with_history(
                    source, input, cancel, endpoint, history,
                ),
                None => RetainedSourceRelay::serve_home_projection(source, input, cancel, endpoint),
            },
        }
    }
}

/// This owner remains on an existing root background job. A held view is not
/// independently transferable: it is borrowed with this complete source/target
/// custody. A future ordinary-app adjunct must retain this owner until all
/// consuming app NamespaceOwners have actually stopped.
pub(super) struct RetainedProjectionSession {
    source: Option<RetainedSourceWorker>,
    relay: Option<RetainedSourceRelay>,
    endpoint: Option<Arc<TargetProjectionEndpoint>>,
    cancellation: ServiceCancellation,
    job: Option<JoinHandle<SupervisedRun>>,
    key: ServiceLeaseKey,
    parent: Option<File>,
    working: Option<tempfile::TempDir>,
    target_stopped: bool,
}
impl RetainedProjectionSession {
    /// Only the installed Home grant and an independently reviewed ordinary
    /// selection can bind the endpoint to a concrete ConsumerLaunchBinding.
    /// Optional history enrollment must belong to that same retained input.
    pub(super) fn start_home_for_consumer_with_history(
        input: Arc<InstalledRetainedLaunchInput>,
        selection: &super::retained_consumer_selection::ReviewedConsumerSelection,
        cancel: Arc<AtomicBool>,
        reader: Option<Arc<super::retained_history_read::RootFilesHistoryReadEnrollment>>,
    ) -> io::Result<Self> {
        if reader
            .as_ref()
            .is_some_and(|reader| !reader.uses_input(&input))
        {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        if !selection.uses_input(&input) {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        selection.check(&cancel)?;
        let digest =
            hex::decode(selection.binding().context_sha256()?).map_err(io::Error::other)?;
        let context: [u8; 32] = digest
            .try_into()
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidData))?;
        let result = Self::start_selection(input, None, Arc::clone(&cancel), Some(context), reader);
        selection.check(&cancel)?;
        result
    }
    fn start_selection(
        input: Arc<InstalledRetainedLaunchInput>,
        name: Option<&OsStr>,
        cancel: Arc<AtomicBool>,
        consumer_context: Option<[u8; 32]>,
        reader: Option<Arc<super::retained_history_read::RootFilesHistoryReadEnrollment>>,
    ) -> io::Result<Self> {
        input.revalidate(&cancel)?;
        let mode = if name.is_some() {
            TargetProjectionMode::SelectedFile
        } else {
            TargetProjectionMode::HomeDirectory
        };
        let (mut source, source_stream) = match name {
            Some(name) => {
                RetainedSourceWorker::start(Arc::clone(&input), name, Arc::clone(&cancel))?
            }
            None => RetainedSourceWorker::start_home(Arc::clone(&input), Arc::clone(&cancel))?,
        };
        let source_connection = match name {
            Some(_) => {
                SelectedFileConnection::connected(source_stream).map(SourceConnection::Selected)
            }
            None => DirectoryConnection::connected(source_stream).map(SourceConnection::Home),
        };
        let source_connection = match source_connection {
            Ok(value) => value,
            Err(error) => {
                source.retire()?;
                return Err(error);
            }
        };
        let history = reader
            .map(|reader| {
                let publisher = source.history_read_publisher()?;
                super::retained_history_read::RootHistoryReadSession::admit(
                    super::retained_selected_editor_history_read::RootHistoryReadEnrollment::files(
                        reader,
                    )?,
                    publisher,
                )
            })
            .transpose()?;
        let source_view = source.source_view_sha256();
        let mut hash = Sha256::new();
        hash.update(b"tilo-retained-target-projection-v2\0");
        hash.update([match mode {
            TargetProjectionMode::SelectedFile => 1,
            TargetProjectionMode::HomeDirectory => 2,
            TargetProjectionMode::SelectedParent => return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Selected parent requires genuine root edit admission and filtered source composition")),
        }]);
        hash.update(input.binding().context_sha256.as_bytes());
        hash.update(source_view);
        let authority: Arc<dyn TargetProjectionAuthority> = Arc::new(Authority {
            input: Arc::clone(&input),
            cancel: Arc::clone(&cancel),
            source_view,
            context: consumer_context.unwrap_or_else(|| hash.finalize().into()),
            name: name.unwrap_or_else(|| OsStr::new("home")).to_owned(),
            mode,
        });
        let service = TrustedCaptureService::native()?;
        let admission = TargetProjectionAdmission::admit(authority)?
            .with_native_runtime(&service.root, &service.publisher)?
            .with_installation(service.installed.clone(), &service.service_publisher)?;
        let parent = retained_source_worker::working_parent()?;
        let working = tempfile::Builder::new()
            .prefix("tilo-retained-target-")
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir_in("/run/tilo-retained-workers")?;
        let key = ServiceLeaseKey::default();
        let cancellation = ServiceCancellation::default();
        let (send, receive) = mpsc::sync_channel(1);
        // Return before any provider/FUSE IO so canonical supervision can poll
        // cancellation while the root job establishes the guarded relay.
        let connected = Box::new(move |endpoint: TargetProjectionEndpoint| {
            send.send(Arc::new(endpoint))
                .map_err(|_| io::ErrorKind::BrokenPipe.into())
        });
        let launch = ServiceLaunch {
            manifest: service.manifest.clone(),
            payload: service.payload.clone(),
            options: RunOptions {
                no_services: true,
                expected_package_hash: Some(service.package_hash.clone()),
                app_id: Some(owned_service::RETAINED_IO_PACKAGE_ID.into()),
                data_home: Some(working.path().to_owned()),
                identity: IdentitySource::None,
                mode: LaunchMode::Service {
                    passthrough_argv: vec!["--tilo-retained-target-projection".into()],
                },
                ..RunOptions::default()
            },
        };
        let child_cancel = cancellation.clone();
        let child_key = key.clone();
        let job = std::thread::Builder::new()
            .name("retained-target-owner".into())
            .spawn(move || {
                owned_service::run_target_projection_connected(
                    launch,
                    admission,
                    child_cancel,
                    child_key,
                    connected,
                )
            })?;
        let mut owner = Self {
            source: Some(source),
            relay: None,
            endpoint: None,
            cancellation,
            job: Some(job),
            key,
            parent: Some(parent),
            working: Some(working),
            target_stopped: false,
        };
        if let Ok(endpoint) = receive.recv_timeout(Duration::from_secs(15)) {
            owner.endpoint = Some(Arc::clone(&endpoint));
            let relay = match source_connection.serve(input, cancel, Arc::clone(&endpoint), history)
            {
                Ok(relay) => relay,
                Err(error) => {
                    owner.retire()?;
                    return Err(error);
                }
            };
            owner.relay = Some(relay);
            if let Err(error) = endpoint.observe_ready() {
                owner.retire()?;
                return Err(error);
            }
            Ok(owner)
        } else {
            let cleanup = owner.retire();
            // Endpoint-only late handoffs carry no relay callbacks. Drain
            // after actual supervisor exit before allowing source release.
            drop(receive);
            Err(cleanup.err().unwrap_or_else(|| {
                io::Error::other("Target projection did not establish its held endpoint")
            }))
        }
    }
    /// Root actor only: the caller retains this complete owner through the
    /// consuming ordinary namespace's observed stop. A descriptor alone is
    /// neither source consent nor a lifetime grant.
    pub(super) fn prepare_consumer_exposure(
        &self,
        managed: Arc<std::sync::Mutex<launcher::ordinary_child::ManagedConsumingBootstrap>>,
        peer: Arc<runtime_portal::process_binding::ExecutingPeer>,
        binding: &launcher::retained_view::ConsumerLaunchBinding,
    ) -> io::Result<launcher::owned_service::target_projection::HeldViewExposure> {
        launcher::owned_service::target_projection::HeldViewExposure::prepare_remote(
            Arc::clone(self.endpoint.as_ref().ok_or(io::ErrorKind::NotConnected)?),
            peer,
            binding,
            managed,
        )
    }
    pub(super) fn with_consumer_view(
        &self,
        use_view: impl FnOnce(&File) -> io::Result<()>,
    ) -> io::Result<()> {
        let endpoint = self.endpoint.as_ref().ok_or(io::ErrorKind::NotConnected)?;
        endpoint.check()?;
        let view = endpoint.held_view()?;
        let result = use_view(&view);
        endpoint.check()?;
        result
    }
    /// Original source-native pin through the SAME admitted Home relay.
    /// Endpoint/FUSE identities do not identify native source objects.
    pub(super) fn pin_selected_origin(
        &self,
        components: &[runtime_fs::EntryName],
    ) -> io::Result<Arc<super::retained_source_relay::SourceSelectedOrigin>> {
        let endpoint = self.endpoint.as_ref().ok_or(io::ErrorKind::NotConnected)?;
        endpoint.check()?;
        let pin = self
            .relay
            .as_ref()
            .ok_or(io::ErrorKind::NotConnected)?
            .pin_home_selection(components)?;
        pin.check_named()?;
        endpoint.check()?;
        Ok(pin)
    }
    /// Private custody snapshot of THIS joint owner, not a wire-supplied endpoint.
    pub(super) fn retained_selected_endpoint(&self) -> io::Result<Arc<TargetProjectionEndpoint>> {
        let endpoint = self.endpoint.as_ref().ok_or(io::ErrorKind::NotConnected)?;
        endpoint.check()?;
        let held = Arc::clone(endpoint);
        endpoint.check()?;
        Ok(held)
    }
    /// Independent expected source digest comes from the retained source
    /// worker admission, never from echoing an observation's descriptive facts.
    pub(super) fn expected_source_view_sha256(&self) -> io::Result<[u8; 32]> {
        let endpoint = self.endpoint.as_ref().ok_or(io::ErrorKind::NotConnected)?;
        endpoint.check()?;
        let digest = self
            .source
            .as_ref()
            .ok_or(io::ErrorKind::NotConnected)?
            .source_view_sha256();
        if digest == [0; 32] {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        endpoint.check()?;
        Ok(digest)
    }
    pub(super) fn with_consumer_observation(
        &self,
        use_observation: impl FnOnce(
            &launcher::owned_service::target_projection::ObservedTargetObject<'_>,
        ) -> io::Result<()>,
    ) -> io::Result<()> {
        let endpoint = self.endpoint.as_ref().ok_or(io::ErrorKind::NotConnected)?;
        endpoint.check()?;
        let observation = endpoint.observe_held_view()?;
        let result = use_observation(&observation);
        observation.check()?;
        result
    }
    /// Kernel observations may only come from THIS retained target service.
    pub(super) fn uses_selected_endpoint(&self, endpoint: &Arc<TargetProjectionEndpoint>) -> bool {
        self.endpoint
            .as_ref()
            .is_some_and(|current| Arc::ptr_eq(current, endpoint))
    }
    /// Off UI: cancellation and real namespace exit precede source release.
    /// No timeout or pathname disappearance is treated as a positive stop.
    pub(super) fn retire(&mut self) -> io::Result<()> {
        self.cancellation.cancel();
        if !self.target_stopped {
            let run = self.job.take().map(|job| job.join());
            let path = self
                .working
                .as_ref()
                .ok_or_else(|| io::Error::other("Target workspace missing"))?
                .path();
            let receipt = run.as_ref().is_some_and(|result| {
                result.as_ref().is_ok_and(|run| {
                    run.shutdown.as_ref().is_some_and(|receipt| {
                        receipt.matches(&self.key, path, owned_service::RETAINED_IO_PACKAGE_ID)
                    }) || run.result.as_ref().is_ok_and(|receipt| {
                        receipt.matches(&self.key, path, owned_service::RETAINED_IO_PACKAGE_ID)
                    })
                })
            });
            let endpoint_stopped = self
                .endpoint
                .as_ref()
                .map(|endpoint| endpoint.observed_stopped())
                .transpose()?
                .unwrap_or(false);
            self.target_stopped = receipt || endpoint_stopped;
            if !self.target_stopped {
                return Err(io::Error::other(
                    "Target namespace shutdown was not observed; source custody retained",
                ));
            }
        }
        // Closed target service can no longer issue callbacks. Finish the actual
        // relay, release held mount exports, then retire the source namespace.
        if let Some(relay) = self.relay.as_ref() {
            relay.retire_history()?;
        }
        let relay = self.relay.take().map_or(Ok(()), |relay| relay.retire());
        self.endpoint.take();
        let source = match self.source.as_mut() {
            Some(source) => source.retire(),
            None => Ok(()),
        };
        if source.is_ok() {
            self.source.take();
        }
        relay.and(source)
    }
}
impl Drop for RetainedProjectionSession {
    fn drop(&mut self) {
        if !self.target_stopped || self.source.is_some() {
            let _ = self.retire();
        }
        if !self.target_stopped || self.source.is_some() {
            // Honest incomplete retirement: never let Drop erase the source
            // witness or control workspace without actual kernel stop evidence.
            // The owning root job normally retains Self on this error. If that
            // owner unwinds, deliberately retain custody until process teardown.
            std::mem::forget(self.relay.take());
            std::mem::forget(self.endpoint.take());
            std::mem::forget(self.source.take());
            std::mem::forget(self.parent.take());
            if let Some(working) = self.working.take() {
                let _ = working.keep();
            }
        }
    }
}
