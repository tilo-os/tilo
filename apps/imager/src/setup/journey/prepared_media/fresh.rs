//! One immutable transfer generation: selected browser bytes and parent seal agree.
use super::*;
use std::fs::OpenOptions;
use std::os::unix::fs::OpenOptionsExt;
#[cfg(test)]
use std::os::unix::fs::PermissionsExt;

impl PreparedMedia {
    /// Refresh browser contents under the same grants before any transfer begins.
    /// Only a complete authenticated new generation is returned; failures leave
    /// the original browser/parent package untouched. Retries use this generation.
    pub(crate) fn refreshed_for_transfer(
        self: &Arc<Self>,
        cancel: &AtomicBool,
    ) -> io::Result<Arc<Self>> {
        super::super::super::migration::check_cancel(cancel)?;
        if self.browser.is_none() || self.browser_frozen {
            self.validate_for_write(cancel)?;
            return Ok(Arc::clone(self));
        }
        let mut generation = self
            .transfer_generation
            .lock()
            .map_err(|_| io::Error::other("Transfer preparation custody unavailable"))?;
        if let Some(prepared) = &*generation {
            prepared.validate_for_write(cancel)?;
            return Ok(Arc::clone(prepared));
        }
        if self.delivery_started.load(Ordering::Acquire) {
            return Err(io::Error::other("Prepared transfer has already begun"));
        }
        self.validate_for_write(cancel)?;
        let password = self
            .browser_password
            .as_ref()
            .ok_or_else(|| io::Error::other("Browser preparation custody missing"))?;
        let prepared = self.refresh_browser_uncached(password, cancel)?;
        *generation = Some(Arc::clone(&prepared));
        Ok(prepared)
    }

    /// Private capture for an original-source preparation transaction. This does
    /// not publish a generation or perform ordinary WholeHome refresh. The
    /// caller must validate its exact offline scope before publishing anything.
    pub(super) fn refresh_browser_uncached(
        &self,
        password: &[u8],
        cancel: &AtomicBool,
    ) -> io::Result<Arc<Self>> {
        super::super::super::migration::check_cancel(cancel)?;
        let browser = self
            .browser
            .as_ref()
            .ok_or_else(|| io::Error::other("Browser custody missing"))?;
        let custody = imager_backend::session::BackendSession::create(
            imager_backend::session::SessionIntent::PrepareMedia,
        )?;
        let root = custody.path().to_path_buf();
        let original = root.join("source-parent.sealed");
        let mut output = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&original)?;
        self.delivery
            .lock()
            .map_err(|_| io::Error::other("Prepared setup custody unavailable"))?
            .copy_preparation_ciphertext(&mut output, cancel)?;
        drop(output);
        let quarantine = root.join("authenticated-parent");
        let parent =
            prepared_setup::PreparedSetupReview::open(&original, password, &quarantine, cancel)?;
        if parent.manifest() != &self.manifest {
            return Err(io::Error::other("Prepared parent manifest changed"));
        }
        drop(parent);
        std::fs::remove_file(&original)?;
        let refreshed = prepare_browser(
            browser.source.clone(),
            &root,
            password,
            &self.manifest.setup_binding,
            self.public_key,
            cancel,
        )?;
        let mut manifest = self.manifest.clone();
        manifest.browser = Some(prepared_setup::BrowserHandoff {
            plan: refreshed.plan.clone(),
            payload: refreshed.expectation.clone(),
        });
        let tree = quarantine.join("home");
        std::fs::remove_file(tree.join(prepared_setup::MANIFEST_FILE))?;
        manifest.write(&tree)?;
        let package = root.join("prepared-setup.sealed");
        ExportReview::review(
            &tree,
            &package,
            Purpose::PreparedSetup,
            prepared_setup::LIMITS,
        )?
        .export(Zeroizing::new(password.to_vec()), cancel)?;
        let delivery = PreparedSetupDelivery::review(&package, password, cancel)?;
        let expectation = delivery.expectation();
        let sealed_bytes = delivery.info().sealed_bytes;
        let home_bytes = self
            .home
            .as_ref()
            .map(|home| {
                home.lock()
                    .map(|home| home.reservation)
                    .map_err(|_| io::Error::other("Home custody unavailable"))
            })
            .transpose()?
            .unwrap_or(0);
        let reserved = [
            sealed_bytes,
            home_bytes,
            refreshed.expectation.sealed_bytes,
            self.applications
                .as_ref()
                .map_or(0, |apps| apps.expectation.sealed_bytes),
            128 * 1024,
        ]
        .into_iter()
        .try_fold(0u64, |total, value| {
            total
                .checked_add(value)
                .ok_or_else(|| io::Error::other("Prepared media capacity overflows"))
        })?;
        std::fs::remove_dir_all(&quarantine)?;
        super::super::super::migration::check_cancel(cancel)?;
        for (source, _) in refreshed.source.profiles() {
            source.validate()?;
        }
        let prepared = Arc::new(Self::from_native(NativePreparedMedia {
            _custody: custody,
            delivery: Mutex::new(delivery),
            public_key: self.public_key,
            account_ids: self.account_ids.clone(),
            sealed_bytes,
            reserved_data_bytes: AtomicU64::new(reserved),
            expectation,
            install_profile: self.install_profile.clone(),
            retained: self.retained.clone(),
            pending_applications_intent: self.pending_applications_intent.clone(),
            home: self.home.clone(),
            home_preview: self.home_preview.clone(),
            applications: self.applications.clone(),
            browser: Some(Arc::new(refreshed)),
            other_device: self.other_device.clone(),
            tpm_handoff: self.tpm_handoff.clone(),
            manifest,
            current_identity: self.current_identity.clone(),
            browser_password: None,
            original_password: Zeroizing::new(password.to_vec()),
            browser_frozen: true,
            delivery_started: std::sync::atomic::AtomicBool::new(false),
            recovery: Mutex::new(
                self.recovery
                    .lock()
                    .map_err(|_| io::Error::other("Recovery custody unavailable"))?
                    .clone(),
            ),
            transfer_generation: Mutex::new(None),
        }));
        Ok(prepared)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::{
        account_transfer,
        journey::{browser::SelectedSources, prepared_media::MediaSelections},
    };
    use imager_backend::setup::browser_profiles;
    use std::{fs, path::PathBuf};
    const PASSWORD: &str = "a sufficiently long transport password";

    fn fixture(root: &std::path::Path) -> (Arc<PreparedMedia>, PathBuf) {
        let home = root.join("source");
        let profile = home.join(".config/chromium/Default");
        fs::create_dir_all(&profile).unwrap();
        fs::write(profile.join("Bookmarks"), b"initial bookmarks").unwrap();
        fs::write(profile.join("History"), b"excluded history").unwrap();
        fs::write(profile.join("Cookies"), b"excluded credentials").unwrap();
        let inventory = account_transfer::discover(&account_transfer::SourceRoots {
            home: home.clone(),
            config: home.join(".config"),
        });
        let selected = SelectedSources::from_reviewed(
            browser_profiles::discover(&home, &inventory.candidates)
                .unwrap()
                .into_iter()
                .map(|source| {
                    let mut profile = source.profile.clone();
                    profile.history = false;
                    (Arc::new(source), profile)
                })
                .collect(),
        )
        .unwrap();
        let identity = PreparedIdentity::create(
            &root.join("identity"),
            "Ada",
            [20, 80, 140],
            tilo_identity_host::provisioning::IdentityStorage::Files,
        )
        .unwrap();
        let prepared = PreparedMedia::prepare_with_selections(
            &identity,
            None,
            MediaSelections {
                home_applications: false,
                home: None,
                retained: None,
                planned_direct: None,
                applications: None,
                browser: Some(selected),
                sessions: None,
                mail: None,

                other_device: None,
                tpm_target: None,
            },
            Zeroizing::new(PASSWORD.into()),
            "browser-transfer-boundary".into(),
            None,
        )
        .unwrap();
        (prepared, profile)
    }

    #[test]
    fn original_archive_budget_preserves_two_setup_seals_and_actual_browser_ciphertext() {
        let root = tempfile::Builder::new()
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let (original, _) = fixture(root.path());
        let cancel = AtomicBool::new(false);
        let parent = prepared_setup::PreparedSetupReview::open(
            &original.package(),
            PASSWORD.as_bytes(),
            &root.path().join("verified-budget-parent"),
            &cancel,
        )
        .unwrap();
        assert_eq!(parent.manifest(), &original.manifest);
        let browser = original.browser.as_ref().unwrap();
        browser
            .archive
            .lock()
            .unwrap()
            .verify_held_ciphertext(&cancel)
            .unwrap();
        let browser_bytes =
            fs::metadata(original._custody.path().join(&browser.expectation.filename))
                .unwrap()
                .len();
        assert_eq!(browser_bytes, browser.expectation.sealed_bytes);
        let parent_bound = prepared_setup::LIMITS.sealed_bound().unwrap();
        let non_home = 2 * parent_bound + browser_bytes + 128 * 1024;
        assert!(super::super::offline::original_budget_for_capacity(&original, non_home).is_err());
        assert!(
            super::super::offline::original_budget_for_capacity(&original, non_home - 1).is_err()
        );
        assert_eq!(
            super::super::offline::original_budget_for_capacity(&original, non_home + 1).unwrap(),
            1
        );
        let capacity = non_home + 8 * 1024 * 1024;
        assert_eq!(
            super::super::offline::original_budget_for_capacity(&original, capacity).unwrap(),
            8 * 1024 * 1024
        );
        // Ordinary transfers reserve no amended setup and keep one Home copy.
        let ordinary_home = 8 * 1024 * 1024;
        assert_eq!(
            original.capacity_with_home(ordinary_home).unwrap(),
            original.sealed_bytes + ordinary_home + browser_bytes + 128 * 1024
        );
        assert!(original.transfer_generation.lock().unwrap().is_none());
        assert!(original.manifest.offline_home.is_none());
    }

    #[test]
    fn original_browser_capture_is_uncached_authenticated_and_cancel_recoverable() {
        let root = tempfile::Builder::new()
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let (original, profile) = fixture(root.path());
        let old_parent = fs::read(original.package()).unwrap();
        let old_browser = original.browser.as_ref().unwrap().expectation.clone();
        fs::write(
            profile.join("Bookmarks"),
            b"fresh original preparation bookmarks",
        )
        .unwrap();
        let cancel = AtomicBool::new(true);
        assert!(
            original
                .refresh_browser_uncached(PASSWORD.as_bytes(), &cancel)
                .is_err()
        );
        assert!(original.transfer_generation.lock().unwrap().is_none());
        assert_eq!(fs::read(original.package()).unwrap(), old_parent);
        cancel.store(false, Ordering::Release);
        let selected = original
            .refresh_browser_uncached(PASSWORD.as_bytes(), &cancel)
            .unwrap();
        assert!(original.transfer_generation.lock().unwrap().is_none());
        assert!(selected.transfer_generation.lock().unwrap().is_none());
        assert_eq!(original.browser.as_ref().unwrap().expectation, old_browser);
        let authenticated = prepared_setup::PreparedSetupReview::open(
            &selected.package(),
            PASSWORD.as_bytes(),
            &root.path().join("check-selected-parent"),
            &cancel,
        )
        .unwrap();
        assert_eq!(authenticated.manifest(), &selected.manifest);
        let browser = selected.browser.as_ref().unwrap();
        let projection = crate::setup::browser_transfer::extract(
            &browser.expectation,
            &browser.plan,
            &selected._custody.path().join(&browser.expectation.filename),
            Zeroizing::new(PASSWORD.as_bytes().to_vec()),
            selected._custody.path(),
            &cancel,
        )
        .unwrap();
        let profile = projection
            .tree
            .root
            .join(imager_backend::browser_selection::PROFILES_DIRECTORY)
            .join(&browser.plan.profiles[0].id);
        assert_eq!(
            fs::read(profile.join("Bookmarks")).unwrap(),
            b"fresh original preparation bookmarks"
        );
        assert_eq!(fs::read_dir(profile).unwrap().count(), 1);
        assert_eq!(fs::read(original.package()).unwrap(), old_parent);
    }

    #[test]
    fn fresh_browser_parent_capacity_and_retry_share_one_authenticated_generation() {
        let root = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let (original, profile) = fixture(root.path());
        let original_bytes = fs::read(original.package()).unwrap();
        fs::write(
            profile.join("Bookmarks"),
            b"new bookmarks at actual transfer",
        )
        .unwrap();
        fs::write(profile.join("History"), b"new excluded history").unwrap();
        let cancel = AtomicBool::new(false);
        let fresh = original.refreshed_for_transfer(&cancel).unwrap();
        assert!(!Arc::ptr_eq(&fresh, &original));
        let parent = prepared_setup::PreparedSetupReview::open(
            &fresh.package(),
            PASSWORD.as_bytes(),
            &root.path().join("verify-parent"),
            &cancel,
        )
        .unwrap();
        assert_eq!(parent.manifest(), &fresh.manifest);
        assert_eq!(parent.manifest().identity_public_key, original.public_key);
        assert_eq!(
            parent.manifest().setup_binding,
            original.manifest.setup_binding
        );
        let browser = fresh.browser.as_ref().unwrap();
        assert_eq!(
            parent.manifest().browser.as_ref().unwrap().payload,
            browser.expectation
        );
        assert!(!browser.plan.profiles[0].history);
        let projection = crate::setup::browser_transfer::extract(
            &browser.expectation,
            &browser.plan,
            &fresh._custody.path().join(&browser.expectation.filename),
            Zeroizing::new(PASSWORD.as_bytes().to_vec()),
            fresh._custody.path(),
            &cancel,
        )
        .unwrap();
        let projected = projection
            .tree
            .root
            .join(imager_backend::browser_selection::PROFILES_DIRECTORY)
            .join(&browser.plan.profiles[0].id);
        assert_eq!(
            fs::read(projected.join("Bookmarks")).unwrap(),
            b"new bookmarks at actual transfer"
        );
        assert_eq!(fs::read_dir(projected).unwrap().count(), 1);
        assert_eq!(
            fresh.expectation().sealed_bytes,
            fs::metadata(fresh.package()).unwrap().len()
        );
        let app = crate::app::ImagerApp::new().with_prepared_media(Some(fresh.clone()));
        let request = app
            .write_request(
                crate::model::sample_source(),
                crate::model::sample_targets().remove(0),
                None,
            )
            .unwrap();
        assert_eq!(
            request.profile.prepared_setup.as_ref(),
            Some(fresh.expectation())
        );
        assert!(request.provision_migration_data);
        assert_eq!(
            fresh.reserved_data_bytes.load(Ordering::Acquire),
            fresh.sealed_bytes + browser.expectation.sealed_bytes + 128 * 1024
        );
        fs::write(profile.join("Bookmarks"), b"later browser activity").unwrap();
        let retry = original.refreshed_for_transfer(&cancel).unwrap();
        assert!(Arc::ptr_eq(&fresh, &retry));
        assert_eq!(retry.expectation(), fresh.expectation());
        assert_eq!(fs::read(original.package()).unwrap(), original_bytes);
        assert_eq!(
            fs::read(profile.join("Cookies")).unwrap(),
            b"excluded credentials"
        );
    }

    #[test]
    fn cancelled_or_redirected_browser_refresh_preserves_original_custody() {
        let root = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let (original, profile) = fixture(root.path());
        let before = fs::read(original.package()).unwrap();
        let stopped = AtomicBool::new(true);
        assert!(original.refreshed_for_transfer(&stopped).is_err());
        assert!(original.transfer_generation.lock().unwrap().is_none());
        fs::remove_file(profile.join("Bookmarks")).unwrap();
        std::os::unix::fs::symlink(profile.join("Cookies"), profile.join("Bookmarks")).unwrap();
        let cancel = AtomicBool::new(false);
        assert!(original.refreshed_for_transfer(&cancel).is_err());
        assert!(original.transfer_generation.lock().unwrap().is_none());
        assert_eq!(fs::read(original.package()).unwrap(), before);
        assert_eq!(
            fs::read(profile.join("Cookies")).unwrap(),
            b"excluded credentials"
        );
        assert!(original._custody.path().read_dir().unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("source-parent")
        }));
    }
}
