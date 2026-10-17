//! Private root composition; no GUI/action route is enabled here.
//! Run and retire on an existing root background job, never on the UI thread.
use super::{
    home_capture::TrustedCaptureService,
    retained_selected_edit::{
        RootFilesEditOrigin, RootSelectedEditAdmission, RootSelectedEditor,
        SelectedEditProjectionAuthority,
    },
    retained_source_relay::{RetainedSelectedEditRelay, selected_parent::RootSelectedEditSource},
    retained_source_worker,
};
use launcher::{
    ordinary_child::ManagedConsumingBootstrap,
    owned_service::{
        self, ServiceCancellation, ServiceLaunch, ServiceLeaseKey, SupervisedRun,
        TargetProjectionAdmission, TargetProjectionEndpoint,
    },
    run::{IdentitySource, LaunchMode, RunOptions},
};
use runtime_fs::EntryName;
use std::{
    ffi::OsString,
    fs::File,
    io,
    os::unix::{ffi::OsStringExt, fs::PermissionsExt},
    sync::{Arc, Mutex, atomic::AtomicBool, mpsc},
    thread::JoinHandle,
    time::Duration,
};

fn component_names(raw: &[Vec<u8>]) -> io::Result<Vec<EntryName>> {
    if raw.is_empty() || raw.len() > 64 {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    raw.iter()
        .map(|bytes| {
            if bytes.is_empty() || bytes.len() > 255 {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            EntryName::new(OsString::from_vec(bytes.clone())).map_err(io::Error::other)
        })
        .collect()
}

/// The fresh source is exclusive to this edit. The admission separately retains
/// originating Files custody. Keep this complete owner until every consuming
/// ordinary namespace has actually stopped, then retire it off UI.
pub(super) struct RetainedSelectedEditSession {
    source: Option<Arc<Mutex<RootSelectedEditSource>>>,
    admission: Option<Arc<RootSelectedEditAdmission>>,
    authority: Option<Arc<SelectedEditProjectionAuthority>>,
    relay: Option<RetainedSelectedEditRelay>,
    endpoint: Option<Arc<TargetProjectionEndpoint>>,
    cancellation: ServiceCancellation,
    job: Option<JoinHandle<SupervisedRun>>,
    key: ServiceLeaseKey,
    parent: Option<File>,
    working: Option<tempfile::TempDir>,
    target_stopped: bool,
    exposure: Option<launcher::owned_service::target_projection::HeldViewExposure>,
    transport: Option<std::os::unix::net::UnixStream>,
    exported: bool,
    managed: Option<Arc<Mutex<ManagedConsumingBootstrap>>>,
}
impl RetainedSelectedEditSession {
    pub(super) fn start(
        origin: Arc<RootFilesEditOrigin>,
        editor: Arc<RootSelectedEditor>,
        cancel: Arc<AtomicBool>,
        managed: Arc<Mutex<ManagedConsumingBootstrap>>,
        daemon: Arc<super::retained_selected_edit::CertifiedNativeHistoryDaemon>,
        reader: Arc<super::retained_selected_editor_history_read::RootHistoryReadEnrollment>,
    ) -> io::Result<Self> {
        editor.check()?;
        reader.check()?;
        if !reader.uses_input(editor.input())
            || !reader
                .selected_editor()
                .is_some_and(|held| Arc::ptr_eq(held, &editor))
        {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let components = component_names(origin.components())?;
        let replacement = super::retained_selected_edit::RootSelectedReplacementSeed::admit(
            editor.clone(),
            &origin,
            daemon,
        )?;
        let source = Arc::new(Mutex::new(RootSelectedEditSource::start(
            editor.clone(),
            cancel,
            replacement,
        )?));
        // Establish root custody before any subsequent fallible composition.
        let mut owner = Self {
            source: Some(source.clone()),
            admission: None,
            authority: None,
            relay: None,
            endpoint: None,
            cancellation: ServiceCancellation::default(),
            job: None,
            key: ServiceLeaseKey::default(),
            parent: None,
            working: None,
            target_stopped: true,
            exposure: None,
            transport: None,
            exported: false,
            managed: Some(managed),
        };
        let selected = source
            .lock()
            .map_err(|_| io::Error::other("Edited source owner poisoned"))?
            .resolve(&components)
            .map_err(io::Error::other)?;
        // Install only the actual selected-leaf Reader before view exposure.
        // The source method independently matches this editor, native input,
        // current selected lineage and real worker Read publisher.
        source
            .lock()
            .map_err(|_| io::Error::other("Edited source owner poisoned"))?
            .install_selected_history_read(reader, &selected)?;
        let admission = RootSelectedEditAdmission::admit(origin, selected.clone(), editor)?;
        owner.admission = Some(admission.clone());
        let authority = SelectedEditProjectionAuthority::admit(admission.clone(), source.clone())?;
        owner.authority = Some(authority.clone());
        let service = TrustedCaptureService::native()?;
        let target_admission = TargetProjectionAdmission::admit(authority)?
            .with_native_runtime(&service.root, &service.publisher)?
            .with_installation(service.installed.clone(), &service.service_publisher)?;
        owner.parent = Some(retained_source_worker::working_parent()?);
        owner.working = Some(
            tempfile::Builder::new()
                .prefix("tilo-retained-edit-target-")
                .permissions(std::fs::Permissions::from_mode(0o700))
                .tempdir_in("/run/tilo-retained-workers")?,
        );
        let (send, receive) = mpsc::sync_channel(1);
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
                data_home: Some(
                    owner
                        .working
                        .as_ref()
                        .ok_or(io::ErrorKind::NotConnected)?
                        .path()
                        .to_owned(),
                ),
                identity: IdentitySource::None,
                mode: LaunchMode::Service {
                    passthrough_argv: vec!["--tilo-retained-target-projection".into()],
                },
                ..RunOptions::default()
            },
        };
        let child_cancel = owner.cancellation.clone();
        let child_key = owner.key.clone();
        owner.job = Some(
            std::thread::Builder::new()
                .name("retained-edit-target-owner".into())
                .spawn(move || {
                    owned_service::run_target_projection_connected(
                        launch,
                        target_admission,
                        child_cancel,
                        child_key,
                        connected,
                    )
                })?,
        );
        owner.target_stopped = false;
        let endpoint = if let Ok(endpoint) = receive.recv_timeout(Duration::from_secs(15)) {
            endpoint
        } else {
            // Retire before dropping a late endpoint handoff. An uncertain
            // stop leaves owner alive; Drop also refuses to release custody.
            drop(selected);
            drop(admission);
            let cleanup = owner.retire();
            drop(receive);
            return Err(cleanup
                .err()
                .unwrap_or_else(|| io::Error::other("Edited target endpoint did not arrive")));
        };
        owner.endpoint = Some(endpoint.clone());
        owner.relay = Some(RetainedSelectedEditRelay::serve(
            source,
            selected,
            admission,
            endpoint.clone(),
        )?);
        endpoint.observe_ready()?;
        owner.check()?;
        Ok(owner)
    }
    /// Root stage owns the actual editor admission before launcher QPE begins.
    /// This returns only after the postspawn namespace witness is retained; it
    /// supplies no LaunchReadiness or successful GUI receipt by itself.
    pub(super) fn issue_selected_edit(
        &mut self,
        channel: launcher::retained_view::RootConsumerChannel,
        cancellation: &ServiceCancellation,
    ) -> io::Result<()> {
        if cancellation.is_cancelled() {
            return Err(io::ErrorKind::Interrupted.into());
        }
        self.check()?;
        if self.transport.is_some() || self.exported {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let admission = self.admission.as_ref().ok_or(io::ErrorKind::NotConnected)?;
        let editor = admission.editor();
        editor.check()?;
        editor.binding().matches_peer(channel.peer())?;
        editor.binding().verify_peer_program(channel.peer())?;
        let peer = channel.retain_peer();
        let endpoint = self.endpoint.as_ref().ok_or(io::ErrorKind::NotConnected)?;
        self.exposure = Some(
            launcher::owned_service::target_projection::HeldViewExposure::prepare_remote(
                Arc::clone(endpoint),
                Arc::clone(&peer),
                editor.binding(),
                Arc::clone(self.managed.as_ref().ok_or(io::ErrorKind::NotConnected)?),
            )?,
        );
        let exposure = self.exposure.as_mut().ok_or(io::ErrorKind::NotConnected)?;
        exposure.place()?;
        let grant = endpoint.review_selected_parent_issue(editor.binding(), &peer, cancellation)?;
        admission.check()?;
        let (stream, issued_peer) =
            match channel.issue_selected_parent(grant, exposure, cancellation) {
                Ok(issued) => {
                    self.exported = true;
                    issued
                }
                Err(failure) => {
                    self.exported = failure.view_may_have_been_delivered();
                    exposure.observe_failed_issue(&failure);
                    let (error, stream) = failure.into_parts();
                    self.transport = Some(stream);
                    return Err(error);
                }
            };
        self.transport = Some(stream);
        if !Arc::ptr_eq(&peer, &issued_peer) {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        // Prepare the retained lease clone before receiving the unique witness.
        // Admission adopts both handles before any fallible validation afterward.
        let stream = self.transport.as_mut().ok_or(io::ErrorKind::NotConnected)?;
        let lease = stream.try_clone()?;
        let witness = launcher::ordinary_namespace::NamespaceWitness::receive(
            stream,
            &issued_peer,
            cancellation,
        )?;
        exposure.adopt_consumer(&witness)?;
        admission.adopt_consumer(lease, witness)?;
        self.check()
    }
    pub(super) fn check(&self) -> io::Result<()> {
        self.admission
            .as_ref()
            .ok_or(io::ErrorKind::NotConnected)?
            .check()?;
        self.endpoint
            .as_ref()
            .ok_or(io::ErrorKind::NotConnected)?
            .check()
    }
    /// Kernel stop observation remains available after authorization revocation.
    /// Missing witness is unknown, never proof that a possible export stopped.
    pub(super) fn consumer_observed_stopped(&self) -> io::Result<bool> {
        self.admission
            .as_ref()
            .map(|admission| admission.consumer_observed_stopped())
            .transpose()
            .map(|stopped| stopped.unwrap_or(false))
    }
    /// True only after explicit retirement has released all actual custody.
    /// Completed teardown errors do not erase kernel stop, and a returned job
    /// status or source error alone cannot satisfy this predicate.
    pub(super) fn cleanup_observed_stopped(&self) -> bool {
        self.target_stopped
            && !self.exported
            && self.source.is_none()
            && self.relay.is_none()
            && self.admission.is_none()
            && self.authority.is_none()
            && self.endpoint.is_none()
            && self.exposure.is_none()
            && self.transport.is_none()
            && self.job.is_none()
            && self.parent.is_none()
            && self.working.is_none()
            && self.managed.is_none()
    }
    pub(super) fn admission(&self) -> io::Result<&Arc<RootSelectedEditAdmission>> {
        self.check()?;
        self.admission
            .as_ref()
            .ok_or_else(|| io::ErrorKind::NotConnected.into())
    }
    /// Call only after the consuming ordinary NamespaceOwner is observed stopped.
    /// The target FUSE stop precedes relay stop, and all admission/filter/endpoint
    /// references are released before the fresh original source is retired.
    pub(super) fn retire(&mut self) -> io::Result<()> {
        // Do not tear down either transport/source worker while an admitted
        // native effect or its certified completion has an unknown outcome.
        // This owner is retried by the existing root supervisor off the UI.
        if let Some(source) = &self.source {
            if source
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .has_unresolved_replacement()
            {
                return Err(io::Error::other(
                    "Selected replacement outcome pending; complete source/target custody retained",
                ));
            }
        }
        if self.exported {
            let admission = self
                .admission
                .as_ref()
                .ok_or_else(|| io::Error::other("Issued editor admission missing"))?;
            admission.cancel_consumer()?;
            if !admission.consumer_observed_stopped()? {
                return Err(io::Error::other(
                    "Edited consumer stop unknown; complete projection custody retained",
                ));
            }
            self.exported = false;
        }
        if let Some(exposure) = &mut self.exposure {
            exposure.retire()?;
        }
        self.exposure.take();
        self.cancellation.cancel();
        if !self.target_stopped {
            let run = self.job.take().map(|job| job.join());
            let path = self
                .working
                .as_ref()
                .ok_or_else(|| io::Error::other("Edited target workspace missing"))?
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
            let stopped = self
                .endpoint
                .as_ref()
                .map(|endpoint| endpoint.observed_stopped())
                .transpose()?
                .unwrap_or(false);
            self.target_stopped = receipt || stopped;
            if !self.target_stopped {
                return Err(io::Error::other(
                    "Edited target stop unknown; exact source custody retained",
                ));
            }
        }
        let mut completed_error = None;
        if let Some(relay) = self.relay.as_mut() {
            let result = relay.retire();
            if !relay.observed_stop() {
                return Err(io::Error::other(
                    "Edited relay stop unknown; exact source custody retained",
                ));
            }
            completed_error = result.err();
        }
        self.relay.take();
        self.transport.take();
        self.endpoint.take();
        self.authority.take();
        self.admission.take();
        if let Some(source) = self.source.as_ref() {
            // Poison does not erase the actual root owner or kernel stop duty.
            let mut source = source.lock().unwrap_or_else(|error| error.into_inner());
            let result = source.retire();
            if !source.observed_stop() {
                return Err(result.err().unwrap_or_else(|| {
                    io::Error::other("Edited source stop unknown; exact custody retained")
                }));
            }
            if completed_error.is_none() {
                completed_error = result.err();
            }
        }
        self.source.take();
        self.parent.take();
        self.working.take();
        // Remote R has already stopped/reaped the outer launcher and detached
        // the exact mount; target, relay and source have now positively stopped.
        // Keep the consuming namespace owner until this final child exit/reap.
        if let Some(managed) = &self.managed {
            managed
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .retire_before_exit();
        }
        self.managed.take();
        completed_error.map_or(Ok(()), Err)
    }
}
impl Drop for RetainedSelectedEditSession {
    fn drop(&mut self) {
        // This private owner lives only on the root background supervisor. A
        // failing shutdown cannot let the root entry return, forget the owner,
        // or release mount/source custody. Normal callers keep Self and retry.
        while self.source.is_some() || !self.target_stopped || self.managed.is_some() {
            if self.retire().is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStrExt;
    #[test]
    fn approved_components_preserve_raw_leaf_and_refuse_traversal() {
        let raw = vec![b"nested".to_vec(), vec![b'x', 0xff, b'.', b't', b'x', b't']];
        let names = component_names(&raw).unwrap();
        assert_eq!(names[1].as_os_str().as_bytes(), raw[1]);
        for component in [b"..".to_vec(), b".".to_vec(), b"a/b".to_vec(), vec![0]] {
            assert!(component_names(&[component]).is_err());
        }
        assert!(component_names(&[]).is_err());
        assert!(component_names(&vec![b"a".to_vec(); 65]).is_err());
    }
}
