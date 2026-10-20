//! A setup prepared on another computer: loading what it promises at login,
//! receiving it from a paired peer or the TPM, and opening the parts it brings.
use super::{
    KEY_PREPARED_IMPORT, OnboardingContext, OnboardingView, PersistedStage, clear_secret,
    typed_passphrase,
};
use crate::onboarding::MintedIdentity;
use crate::shell::i18n::t;
use std::path::Path;
use zeroize::{Zeroize, Zeroizing};

impl OnboardingView {
    pub(super) fn load_prepared_setup(&mut self, identity_dir: &Path) {
        #[cfg(target_os = "linux")]
        if let Some(home) = crate::onboarding_prepared::session_home()
            && crate::onboarding_peer::Peer::pending_at(&home)
        {
            self.load_peer_setup(&home, identity_dir);
            return;
        }
        if let Some(home) = crate::onboarding_prepared::session_home()
            && crate::onboarding_prepared::pending_at(&home)
        {
            self.prepared_pending = true;
            match crate::onboarding_prepared::Pending::open(&home) {
                Ok(pending) => match pending.completed(identity_dir) {
                    Ok(true) => {
                        self.prepared_pending = false;
                        self.reopen_completed_parts(&home, identity_dir, &pending);
                    }
                    Ok(false) => self.prepared_review = Some(pending),
                    Err(_) => self.error = Some(t("onboarding-prepared-failed")),
                },
                Err(_) => self.error = Some(t("onboarding-prepared-failed")),
            }
        }
    }

    /// A descriptor is a promise that this login completes the transfer. A
    /// computer that already carries the same person opens the ordinary
    /// receiver. A fresh computer joins that person first: the preparing
    /// computer approves this computer's own new device key and exports the
    /// identity, after which the receiver proves the same certified person.
    /// An unrelated active identity is never overwritten; its person gets an
    /// explicit message instead of a generic failure.
    #[cfg(target_os = "linux")]
    fn load_peer_setup(&mut self, home: &Path, identity_dir: &Path) {
        let active = runtime_identity::resolve_active_identity(identity_dir);
        match crate::onboarding_peer::Peer::open(home, identity_dir) {
            Ok(Some(peer)) => {
                self.peer = Some(peer);
                self.prepared_pending = true;
            }
            Ok(None) if active.is_none() => {
                let label = std::env::var("HOSTNAME").unwrap_or_else(|_| "Tilo device".into());
                match crate::onboarding_peer::Peer::join(home, identity_dir, &label) {
                    Ok(Some(peer)) => {
                        self.peer = Some(peer);
                        self.prepared_pending = true;
                    }
                    Ok(None) => {}
                    Err(_) => {
                        self.prepared_pending = true;
                        self.error = Some(t("onboarding-prepared-failed"));
                    }
                }
            }
            // Keep the ordinary restore/connect choices in front of the
            // person. Nothing about the active identity is touched; the
            // descriptor stays pending for a login that actually belongs to
            // its person.
            Err(_) if active.is_some() => self.peer_failed(home, active.as_deref()),
            Ok(None) | Err(_) => {}
        }
    }

    /// Explain a receiver that could not open: a descriptor for another
    /// person than the active identity says so, anything else is a failure.
    #[cfg(target_os = "linux")]
    fn peer_failed(&mut self, home: &Path, active: Option<&Path>) {
        let unrelated = active
            .and_then(|path| runtime_identity::load_metadata_from_path(path).ok())
            .is_some_and(|metadata| {
                crate::onboarding_peer::Peer::descriptor_identity_at(home)
                    .ok()
                    .flatten()
                    .is_some_and(|person| metadata.group_public_key != person)
            });
        if unrelated {
            self.error = Some(t("onboarding-prepared-other-person"));
        } else {
            self.prepared_pending = true;
            self.error = Some(t("onboarding-prepared-failed"));
        }
    }

    /// Reopen the parts a prepared setup that already completed still
    /// brings along, so a later login resumes them.
    fn reopen_completed_parts(
        &mut self,
        home: &Path,
        identity_dir: &Path,
        pending: &crate::onboarding_prepared::Pending,
    ) {
        match pending.portable_home(identity_dir) {
            Ok(Some(declaration)) => {
                if let Ok(portable) = crate::onboarding_portable_home::Pending::open(
                    home.to_path_buf(),
                    identity_dir.to_path_buf(),
                    declaration,
                ) {
                    self.portable_home = Some(portable);
                } else {
                    self.prepared_pending = true;
                    self.error = Some(t("onboarding-portable-home-unavailable"));
                }
            }
            Ok(None) => (),
            Err(_) => self.prepared_pending = true,
        }
        match pending.portable_applications(identity_dir) {
            Ok(Some(declaration)) => {
                if let Ok(applications) = crate::onboarding_portable_applications::Pending::open(
                    home.to_path_buf(),
                    identity_dir.to_path_buf(),
                    declaration,
                ) {
                    self.portable_applications = Some(applications);
                } else {
                    self.prepared_pending = true;
                    self.error = Some(t("onboarding-portable-applications-unavailable"));
                }
            }
            Ok(None) => (),
            Err(_) => self.prepared_pending = true,
        }
        match pending.browser(identity_dir) {
            Ok(Some(declaration)) => {
                crate::shell::launcher::set_browser_continuity(None);
                self.browser = crate::onboarding_browser::Pending::open(
                    home.to_path_buf(),
                    identity_dir.to_path_buf(),
                    declaration,
                )
                .ok();
                if self.browser.is_none() {
                    self.prepared_pending = true;
                    self.error = Some(t("onboarding-browser-unavailable"));
                }
            }
            Ok(None) => (),
            Err(_) => self.prepared_pending = true,
        }
    }

    pub(crate) fn prepared_setup_status(&self) -> Option<&'static str> {
        self.prepared_pending.then_some(
            if self.prepared_worker.is_some() || self.prepared_secret_queued {
                "busy"
            } else if self.prepared_review.is_some() {
                "ready"
            } else {
                "unavailable"
            },
        )
    }
    pub(crate) fn submit_prepared_setup_secret(&mut self, password: &[u8]) -> Result<(), ()> {
        if self.prepared_setup_status() != Some("ready") {
            return Err(());
        }
        let text = std::str::from_utf8(password).map_err(|_| ())?;
        if text.is_empty() || text.len() > 4096 {
            return Err(());
        }
        let mut secret = self.connect_passphrase.lock().map_err(|_| ())?;
        secret.zeroize();
        secret.push_str(text);
        drop(secret);
        self.prepared_secret_queued = true;
        self.press(KEY_PREPARED_IMPORT);
        self.invalidation.both();
        Ok(())
    }
    fn activate_prepared(
        &self,
        mut imported: crate::onboarding_prepared::Imported,
    ) -> Result<ActivatedPrepared, ()> {
        self.prepared_review
            .as_ref()
            .ok_or(())?
            .validate()
            .map_err(|_| ())?;
        let directory = self.identity_dir.as_ref().ok_or(())?;
        if let Some(resolved) = imported.resolved_current.take() {
            resolved.validate().map_err(|_| ())?;
            let claim = resolved.declaration().map_err(|_| ())?;
            if imported.current_identity.as_ref() != Some(&claim)
                || claim.public_key() != imported.identity_public_key
            {
                return Err(());
            }
            let signer = resolved.load().map_err(|_| ())?;
            let registry = crate::identity_registry::IdentityRegistryStore::at(directory)
                .load()
                .map_err(|_| ())?;
            let profile = registry
                .identities
                .values()
                .find(|profile| {
                    profile.id == hex::encode(signer.group_public_key())
                        && profile.user_public_key == signer.group_public_key()
                        && profile.record_path == directory.join(claim.record_name())
                })
                .ok_or(())?;
            let identity = MintedIdentity {
                fingerprint: profile.id.clone(),
                path: profile.record_path.clone(),
            };
            resolved.validate().map_err(|_| ())?;
            return Ok((
                identity,
                imported.identity_public_key,
                imported.portable_home,
                imported.portable_flatpak,
                imported.browser,
                imported.service_sessions,
                Some(resolved),
            ));
        }
        if imported.current_identity.is_some() {
            return Err(());
        }
        let primary = directory.join("primary.json");
        let signer = runtime_identity::load_from_path(primary.clone()).map_err(|_| ())?;
        if signer.group_public_key() != imported.identity_public_key {
            return Err(());
        }
        let store = crate::identity_registry::IdentityRegistryStore::at(directory);
        let mut registry = store.load_reconciled().map_err(|_| ())?;
        let profile = registry.register_record(&primary).map_err(|_| ())?.clone();
        store.save(&registry).map_err(|_| ())?;
        let runtime_id = profile
            .record_path
            .file_stem()
            .and_then(|v| v.to_str())
            .ok_or(())?;
        runtime_identity::set_active_identity(directory, runtime_id).map_err(|_| ())?;
        let paths = crate::firstboot::collaboration_identity_paths();
        if imported.device_label.is_empty() {
            crate::firstboot::ensure_collaboration_identity(directory, &paths)
        } else {
            crate::firstboot::ensure_collaboration_identity_with_device_label(
                directory,
                &paths,
                &imported.device_label,
            )
        }
        .map_err(|_| ())?;
        crate::device_fleet::DeviceFleetStore::at(
            directory,
            crate::firstboot::collaboration_identity_paths(),
        )
        .register_current_device(&profile.id)
        .map_err(|_| ())?;
        Ok((
            MintedIdentity {
                fingerprint: profile.id,
                path: profile.record_path,
            },
            imported.identity_public_key,
            imported.portable_home,
            imported.portable_flatpak,
            imported.browser,
            imported.service_sessions,
            None,
        ))
    }

    fn load_browser_declaration(
        &mut self,
        declaration: crate::onboarding_prepared::BrowserDeclaration,
    ) {
        crate::shell::launcher::set_browser_continuity(None);
        self.browser = crate::onboarding_prepared::session_home().and_then(|home| {
            self.identity_dir.clone().and_then(|identity| {
                crate::onboarding_browser::Pending::open(home, identity, declaration).ok()
            })
        });
        if self.browser.is_none() {
            self.prepared_pending = true;
            self.error = Some(t("onboarding-browser-unavailable"));
        }
    }
    pub(crate) fn verified_browser_projection(&self) -> Option<&Path> {
        self.browser.as_ref()?.verified_projection()
    }
    /// Start importing the prepared setup with the passphrase typed: its
    /// targets made ready, then its worker started.
    fn start_prepared_import(&mut self) {
        self.prepared_secret_queued = false;
        let result = (|| {
            let home = crate::onboarding_prepared::session_home().ok_or(())?;
            let pending = crate::onboarding_prepared::Pending::open(&home).map_err(|_| ())?;
            let password = typed_passphrase(&self.connect_passphrase)?;
            let directory = self.identity_dir.clone().ok_or(())?;
            pending.prepare_targets(&directory).map_err(|_| ())?;
            if !self.persist_progress(PersistedStage::AcquiringIdentity) {
                return Err(());
            }
            let worker = pending
                .start(
                    &directory,
                    &password,
                    self.context == OnboardingContext::HostDesktop,
                )
                .map_err(|_| ())?;
            self.prepared_home_password = Some((pending.digest(), password));
            self.prepared_review = Some(pending);
            self.prepared_worker = Some(worker);
            Ok::<(), ()>(())
        })();
        clear_secret(&self.connect_passphrase);
        if result.is_err() {
            self.prepared_home_password = None;
        }
        self.error = result.err().map(|()| t("onboarding-prepared-failed"));
        self.invalidation.both();
    }

    /// Open what the prepared setup brings along, each part handed the
    /// passphrase the installer was given, since that was the person's
    /// consent.
    fn open_prepared_parts(
        &mut self,
        (portable_home, portable_applications, browser): PreparedParts,
        prepared_password: Option<([u8; 32], Zeroizing<Vec<u8>>)>,
    ) {
        if let Some(declaration) = portable_home {
            self.portable_home = crate::onboarding_prepared::session_home().and_then(|home| {
                self.identity_dir.clone().and_then(|directory| {
                    crate::onboarding_portable_home::Pending::open(home, directory, declaration)
                        .ok()
                })
            });
            if let (Some(portable), Some((digest, password))) =
                (self.portable_home.as_mut(), prepared_password.as_ref())
            {
                // The authenticated installer selection is prior consent.
                // The reducer continues it after activation; failures retain
                // the ordinary explicit retry/password controls.
                let _ =
                    portable.receive_prepared_password(*digest, Zeroizing::new(password.to_vec()));
            }
            if self.portable_home.is_none() {
                self.prepared_pending = true;
                self.error = Some(t("onboarding-portable-home-unavailable"));
            }
        }
        if let Some(declaration) = portable_applications {
            self.portable_applications =
                crate::onboarding_prepared::session_home().and_then(|home| {
                    self.identity_dir.clone().and_then(|directory| {
                        crate::onboarding_portable_applications::Pending::open(
                            home,
                            directory,
                            declaration,
                        )
                        .ok()
                    })
                });
            if let (Some(applications), Some((digest, password))) = (
                self.portable_applications.as_mut(),
                prepared_password.as_ref(),
            ) {
                let _ = applications
                    .receive_prepared_password(*digest, Zeroizing::new(password.to_vec()));
            }
            if self.portable_applications.is_none() {
                self.prepared_pending = true;
                self.error = Some(t("onboarding-portable-applications-unavailable"));
            }
        }
        if let Some(declaration) = browser {
            self.load_browser_declaration(declaration);
            if let (Some(browser), Some((digest, password))) =
                (self.browser.as_mut(), prepared_password)
            {
                let _ = browser.receive_prepared_password(digest, password);
            }
        }
    }

    pub(super) fn sync_prepared(&mut self) {
        #[cfg(target_os = "linux")]
        {
            self.open_peer_once_identity_active();
            self.receive_from_peer();
            self.start_with_tpm_secret();
        }
        if self.pressed.take(KEY_PREPARED_IMPORT) && self.prepared_worker.is_none() {
            self.start_prepared_import();
        }
        self.settle_prepared_worker();
    }

    /// Open the descriptor's receiver once this computer's identity is
    /// active, at most once per setup.
    #[cfg(target_os = "linux")]
    fn open_peer_once_identity_active(&mut self) {
        if self.peer_attempted || self.peer.is_some() {
            return;
        }
        let Some(directory) = self.identity_dir.clone() else {
            return;
        };
        let active = runtime_identity::resolve_active_identity(&directory);
        if active.is_none() {
            return;
        }
        let Some(home) = crate::onboarding_prepared::session_home() else {
            return;
        };
        match crate::onboarding_peer::Peer::open(&home, &directory) {
            Ok(Some(peer)) => {
                self.peer_attempted = true;
                self.peer = Some(peer);
                self.prepared_pending = true;
                self.invalidation.both();
            }
            // The identity is active but its device certificate is still
            // being provisioned. Retry on a later frame rather than blocking
            // the flow that is creating it.
            Ok(None) => {}
            Err(_) => {
                self.peer_attempted = true;
                self.peer_failed(&home, active.as_deref());
                self.invalidation.both();
            }
        }
    }

    /// Start importing once the peer has delivered the setup's passphrase.
    #[cfg(target_os = "linux")]
    fn receive_from_peer(&mut self) {
        if self.prepared_worker.is_some() || self.prepared_review.is_some() {
            return;
        }
        let Some(peer) = &self.peer else {
            return;
        };
        match peer.poll() {
            Some(Ok(received)) => {
                let result = (|| {
                    let home = crate::onboarding_prepared::session_home().ok_or(())?;
                    let pending =
                        crate::onboarding_prepared::Pending::open(&home).map_err(|_| ())?;
                    let directory = self.identity_dir.clone().ok_or(())?;
                    pending.prepare_targets(&directory).map_err(|_| ())?;
                    let worker = pending
                        .start(
                            &directory,
                            received.password.as_bytes(),
                            self.context == OnboardingContext::HostDesktop,
                        )
                        .map_err(|_| ())?;
                    self.prepared_home_password = Some((
                        pending.digest(),
                        Zeroizing::new(received.password.as_bytes().to_vec()),
                    ));
                    self.prepared_review = Some(pending);
                    self.prepared_worker = Some(worker);
                    self.peer_received = Some(received);
                    self.prepared_pending = true;
                    Ok::<(), ()>(())
                })();
                if result.is_err() {
                    self.error = Some(t("onboarding-prepared-failed"));
                    self.peer = None;
                }
                self.invalidation.both();
            }
            Some(Err(_)) => {
                self.error = Some(t("onboarding-prepared-failed"));
                self.peer = None;
                self.invalidation.both();
            }
            None => {}
        }
    }

    /// Start importing with the secret the TPM sealed for this setup, when
    /// no peer delivers one.
    #[cfg(target_os = "linux")]
    fn start_with_tpm_secret(&mut self) {
        if self.prepared_worker.is_some() || self.prepared_review.is_some() || self.peer.is_some() {
            return;
        }
        let Some(home) = crate::onboarding_prepared::session_home() else {
            return;
        };
        if !crate::onboarding_prepared::pending_at(&home) {
            return;
        }
        let Some(secret) = crate::onboarding_prepared::take_tpm_secret() else {
            return;
        };
        let result = (|| {
            let pending = crate::onboarding_prepared::Pending::open(&home).map_err(|_| ())?;
            let directory = self.identity_dir.clone().ok_or(())?;
            pending.prepare_targets(&directory).map_err(|_| ())?;
            if !self.persist_progress(PersistedStage::AcquiringIdentity) {
                return Err(());
            }
            let worker = pending
                .start(
                    &directory,
                    &secret,
                    self.context == OnboardingContext::HostDesktop,
                )
                .map_err(|_| ())?;
            self.prepared_home_password = Some((pending.digest(), secret));
            self.prepared_review = Some(pending);
            self.prepared_worker = Some(worker);
            Ok::<(), ()>(())
        })();
        if result.is_err() {
            self.prepared_home_password = None;
            self.error = Some(t("onboarding-prepared-failed"));
        }
        self.invalidation.both();
    }

    /// Activate what a finished import worker produced, acknowledge it to the
    /// prepared setup and its peer, and open the parts it brings along.
    fn settle_prepared_worker(&mut self) {
        let Some(outcome) = self
            .prepared_worker
            .as_mut()
            .and_then(crate::onboarding_prepared::Worker::poll)
        else {
            return;
        };
        self.prepared_worker = None;
        let prepared_password = self.prepared_home_password.take();
        let imported = outcome
            .map_err(|_| ())
            .and_then(|imported| self.activate_prepared(imported));
        match imported {
            Ok((
                identity,
                key,
                portable_home,
                portable_applications,
                browser,
                service_sessions,
                current_identity,
            )) => {
                self.flow.resume_created(identity);
                if self.refresh_backup_capability().is_err() {
                    self.error = Some(t("onboarding-prepared-failed"));
                    self.invalidation.both();
                    return;
                }
                let Ok(current_claim) = current_identity
                    .as_ref()
                    .map(tilo_identity_host::enrolled_transport::ResolvedCurrentIdentity::declaration)
                    .transpose()
                else {
                    self.error = Some(t("onboarding-prepared-failed"));
                    self.invalidation.both();
                    return;
                };
                if self.persist_progress(if self.identity_ready_page {
                    PersistedStage::ReadyToEnter
                } else {
                    PersistedStage::IdentityReady
                }) && self.prepared_review.as_ref().is_some_and(|review| {
                    review
                        .acknowledge(
                            key,
                            portable_home.clone(),
                            portable_applications.clone(),
                            browser.clone(),
                            service_sessions.clone(),
                            current_claim.clone(),
                        )
                        .is_ok()
                }) {
                    self.prepared_pending = false;
                    self.error = None;
                    #[cfg(target_os = "linux")]
                    if let (Some(peer), Some(received)) = (&self.peer, &self.peer_received) {
                        let _ = peer.acknowledge(received);
                        self.peer = None;
                        self.peer_received = None;
                    }
                    self.open_prepared_parts(
                        (portable_home, portable_applications, browser),
                        prepared_password,
                    );
                    self.prepared_review = None;
                } else {
                    self.error = Some(t("onboarding-prepared-failed"));
                }
            }
            Err(()) => self.error = Some(t("onboarding-prepared-failed")),
        }
        self.invalidation.both();
    }
}

/// The parts a prepared setup may bring along: a home, applications and a
/// browser.
type PreparedParts = (
    Option<crate::onboarding_prepared::PortableHome>,
    Option<crate::onboarding_prepared::PortableApplications>,
    Option<crate::onboarding_prepared::BrowserDeclaration>,
);

/// What activating a prepared identity yields: the identity, its device
/// key, and the parts of the old computer it brings along.
type ActivatedPrepared = (
    MintedIdentity,
    [u8; 32],
    Option<crate::onboarding_prepared::PortableHome>,
    Option<crate::onboarding_prepared::PortableApplications>,
    Option<crate::onboarding_prepared::BrowserDeclaration>,
    Option<crate::onboarding_prepared::ServiceSessions>,
    Option<tilo_identity_host::enrolled_transport::ResolvedCurrentIdentity>,
);
