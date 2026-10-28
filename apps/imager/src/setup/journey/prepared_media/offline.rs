//! Pending source-offline preparation; no restart or export is executed here.
#[path = "offline/applications.rs"]
mod applications;
use super::*;
use crate::{
    media_layout::PreparedLayout,
    model::{Source, SourceIdentity, TargetDrive},
};
use std::fs::{File, OpenOptions};
#[cfg(all(test, feature = "native-capture-qualification"))]
use std::os::unix::fs::PermissionsExt;

/// Retains existing image and media descriptor grants through resealing.
/// The serialized binding is only an expectation; the privileged writer must
/// independently recompute and match it before its first media mutation.
pub(crate) struct OfflineMediaReview {
    admission: Option<super::super::super::read_media::ReadOnlyMediaGrant>,
    image: File,
    media: File,
    source: Source,
    target: TargetDrive,
    layout: PreparedLayout,
    target_number: u64,
    source_plans: Mutex<Option<Vec<Vec<u8>>>>,
    original_image: Option<super::super::super::original_admission::VerifiedOriginalImage>,
}
impl OfflineMediaReview {
    pub(in crate::setup) fn review(
        source: &Source,
        target: &TargetDrive,
        image: File,
        media: File,
    ) -> io::Result<Self> {
        if !source.installable || !target.is_media_target() {
            return Err(io::Error::other(
                "Offline preparation requires selected installer media",
            ));
        }
        validate_image(source, &image)?;
        crate::write::attest_target_descriptor(target, &media)?;
        use std::os::unix::fs::MetadataExt;
        let target_number = media.metadata()?.rdev();
        let layout = crate::media_layout::prepare(
            &image,
            &media,
            source.size,
            target.size,
            target.stable_id.as_deref().unwrap_or(""),
        )?;
        Ok(Self {
            admission: None,
            image,
            media,
            source: source.clone(),
            target: target.clone(),
            layout,
            target_number,
            source_plans: Mutex::new(None),
            original_image: None,
        })
    }
    pub(crate) fn review_admitted(
        source: &Source,
        target: &TargetDrive,
        image: File,
        grant: super::super::super::read_media::ReadOnlyMediaGrant,
    ) -> io::Result<Self> {
        let file = grant.clone_descriptor()?;
        let mut review = Self::review(source, target, image, file)?;
        review.admission = Some(grant);
        review.validate()?;
        Ok(review)
    }
    fn validate(&self) -> io::Result<()> {
        if let Some(image) = &self.original_image {
            image.validate()?;
        }
        if let Some(admission) = &self.admission {
            admission.validate()?;
        }
        validate_image(&self.source, &self.image)?;
        crate::write::attest_target_descriptor(&self.target, &self.media)?;
        use std::os::unix::fs::MetadataExt;
        if self.media.metadata()?.rdev() != self.target_number {
            return Err(io::Error::other("Retained raw media identity changed"));
        }
        if let Some(plans) = self
            .source_plans
            .lock()
            .map_err(|_| io::Error::other("Source disk custody unavailable"))?
            .as_ref()
        {
            super::super::super::migration_handoff::validate_offline_source_disks(
                plans,
                self.target_number,
            )?;
        }
        Ok(())
    }
    pub(in crate::setup) fn with_protected_delivery<T>(
        &self,
        target: &TargetDrive,
        artifact: &PreparedMedia,
        cancel: &AtomicBool,
        deliver: impl FnOnce(
            &super::super::super::media_setup::inbox::ProtectedInboxGrant<'_>,
        ) -> io::Result<T>,
    ) -> io::Result<T> {
        super::super::super::migration::check_cancel(cancel)?;
        self.validate()?;
        if target.name != self.target.name
            || target.stable_id != self.target.stable_id
            || target.size != self.target.size
            || target.model != self.target.model
            || target.removable != self.target.removable
            || target.tran != self.target.tran
            || target.kind != self.target.kind
            || target.is_system
        {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let declaration = artifact
            .pending_original_declaration()
            .ok_or(io::ErrorKind::InvalidData)?;
        let binding = self.layout.planned_binding()?;
        if declaration.media() != &binding {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let sources = self.retained_source_geometry(&declaration)?;
        self.bind_source_geometry(sources)?;
        let layout = crate::write::OfflineLayout {
            binding,
            reserved_bytes: artifact.reserved_data_bytes.load(Ordering::Acquire),
            setup: artifact.expectation.clone(),
            source_plans: self.retained_source_geometry(&declaration)?,
            target_number: self.target_number,
        };
        self.layout.require_capacity(layout.reserved_bytes)?;
        let result = self
            .admission
            .as_ref()
            .ok_or(io::ErrorKind::PermissionDenied)?
            .with_protected_inbox(&layout, cancel, deliver)?;
        self.validate()?;
        super::super::super::migration::check_cancel(cancel)?;
        Ok(result)
    }
    fn retained_source_geometry(
        &self,
        declaration: &prepared_setup::OriginalSelectionPlan,
    ) -> io::Result<Vec<Vec<u8>>> {
        let sources = self
            .source_plans
            .lock()
            .map_err(|_| io::Error::other("Source disk custody unavailable"))?;
        let sources = sources
            .as_ref()
            .ok_or_else(|| io::Error::other("Original source geometry was not retained"))?;
        let mut primary = declaration.source().clone();
        primary.flatpak_inventory = None;
        if sources.first() != Some(&primary.bytes()?) {
            return Err(io::Error::other(
                "Sealed primary source differs from retained geometry",
            ));
        }
        Ok(sources.clone())
    }
    fn bind_source_geometry(&self, plans: Vec<Vec<u8>>) -> io::Result<()> {
        super::super::super::migration_handoff::validate_offline_source_disks(
            &plans,
            self.target_number,
        )?;
        let mut held = self
            .source_plans
            .lock()
            .map_err(|_| io::Error::other("Source disk custody unavailable"))?;
        if held.as_ref().is_some_and(|original| original != &plans) {
            return Err(io::Error::other(
                "Original source disks differ from media review",
            ));
        }
        *held = Some(plans);
        Ok(())
    }
    pub(crate) fn attach_request(
        &self,
        artifact: &PreparedMedia,
        request: &mut crate::write::WriteRequest,
    ) -> io::Result<()> {
        self.validate()?;
        if request.source.identity != self.source.identity
            || request.source.size != self.source.size
            || request.target.stable_id != self.target.stable_id
            || request.target.size != self.target.size
        {
            return Err(io::Error::other(
                "Writer request differs from held preparation grants",
            ));
        }
        let declaration = artifact
            .pending_original_declaration()
            .ok_or_else(|| io::Error::other("No sealed pending export declaration"))?;
        let sources = self.retained_source_geometry(&declaration)?;
        self.bind_source_geometry(sources)?;
        let binding = self.layout.planned_binding()?;
        if declaration.media() != &binding {
            return Err(io::Error::other(
                "Sealed declaration differs from retained planner",
            ));
        }
        let reserved = artifact.reserved_data_bytes.load(Ordering::Acquire);
        self.layout.require_capacity(reserved)?;
        request.profile.prepared_setup = Some(artifact.expectation.clone());
        request.provision_migration_data = true;
        request.offline_layout = Some(crate::write::OfflineLayout {
            binding,
            reserved_bytes: reserved,
            setup: artifact.expectation.clone(),
            source_plans: self.retained_source_geometry(&declaration)?,
            target_number: self.target_number,
        });
        Ok(())
    }
}
fn validate_image(source: &Source, image: &File) -> io::Result<()> {
    let metadata = image.metadata()?;
    if source.identity.is_none()
        || !metadata.is_file()
        || metadata.len() != source.size
        || SourceIdentity::from_file(image)? != source.identity
    {
        return Err(io::Error::other("Reviewed installer image custody changed"));
    }
    Ok(())
}
impl PreparedMedia {
    /// Explicit backend preparation only. The password stays in private custody;
    /// source-owner facts and witness come from the admitted existing helper.
    /// Once sealed they remain requests, not ambient group authority; offline
    /// admission must reopen and verify the same original protected record.
    pub(in crate::setup) fn prepare_offline(
        self: &Arc<Self>,
        media: &OfflineMediaReview,
        intent: &super::super::super::migration_intent::MigrationIntent,
        password: Zeroizing<Vec<u8>>,
        cancel: &AtomicBool,
    ) -> io::Result<Arc<Self>> {
        super::super::super::migration::check_cancel(cancel)?;
        let mut generation = self
            .transfer_generation
            .lock()
            .map_err(|_| io::Error::other("Preparation generation unavailable"))?;
        if self.delivery_started.load(Ordering::Acquire) || generation.is_some() {
            return Err(io::Error::other("Preparation generation is already frozen"));
        }
        media.validate()?;
        let plan = intent.for_pending_offline()?;
        // The original-source transaction owns this generation lock. Capture
        // only the selected browser sidecar, without ordinary Home inventory or
        // publishing an intermediate cached generation. Immutable app carriers
        // are checked again below against this same authenticated source plan.
        let refreshed;
        let selected = if self.browser.is_some() && !self.browser_frozen {
            refreshed = self.refresh_browser_uncached(&password, cancel)?;
            &*refreshed
        } else {
            &**self
        };
        validate_payload_scope(selected, &plan, cancel)?;
        let home = selected
            .home
            .as_ref()
            .ok_or_else(|| io::Error::other("No selected original home grant"))?;
        let (reservation, registered) = {
            let home = home
                .lock()
                .map_err(|_| io::Error::other("Home custody unavailable"))?;
            if home.receipt.is_some() {
                return Err(io::Error::other("Completed export cannot become pending"));
            }
            home.source.validate_offline_scope(&plan)?;
            media.bind_source_geometry(home.source.original_source_geometry(&plan, cancel)?)?;
            let reservation = if home.source.original_only() {
                original_budget(selected, media)?
            } else {
                home.reservation
            };
            let registered = home.source.register_original_source(&plan, cancel)?;
            (reservation, registered)
        };
        let mut declaration = prepared_setup::OfflineHomePlan::new(
            registered.plan,
            self.manifest.setup_binding.clone(),
            self.public_key,
            media.layout.planned_binding()?,
            registered.original_caller,
            reservation,
        )?
        .with_source_witness(registered.witness)?;
        if let Some(mounted) = registered.mounted {
            declaration = declaration.with_mounted_witness(mounted)?;
        }
        let prepared = reseal(selected, declaration, &password, cancel)?;
        // A real target-bound budget is now assigned to this authenticated
        // generation; the original permission-only generation stays unassigned.
        prepared
            .reserved_data_bytes
            .store(prepared.capacity_with_home(reservation)?, Ordering::Release);
        media
            .layout
            .require_capacity(prepared.reserved_data_bytes.load(Ordering::Acquire))?;
        media.validate()?;
        super::super::super::migration::check_cancel(cancel)?;
        *generation = Some(Arc::clone(&prepared));
        Ok(prepared)
    }
}
fn original_budget(prepared: &PreparedMedia, media: &OfflineMediaReview) -> io::Result<u64> {
    // Capacity comes from this held target's canonical GPT/filesystem plan,
    // not a guessed live Home inventory. Parent growth is bounded by its actual
    // archive protocol; sidecar ciphertext sizes are already authenticated.
    original_budget_for_capacity(prepared, media.layout.payload_capacity()?)
}
pub(super) fn amended_setup_reservation() -> io::Result<u64> {
    prepared_setup::LIMITS
        .sealed_bound()
        .ok_or_else(|| io::Error::other("Amended setup capacity overflows"))
}
pub(super) fn original_budget_for_capacity(
    prepared: &PreparedMedia,
    capacity: u64,
) -> io::Result<u64> {
    let parent = amended_setup_reservation()?;
    let non_home = [
        parent, // pending source declaration, bounded before final reseal
        parent, // separately encrypted completed/amended setup at boot
        prepared
            .applications
            .as_ref()
            .map_or(0, |apps| apps.expectation.sealed_bytes),
        prepared
            .browser
            .as_ref()
            .map_or(0, |browser| browser.expectation.sealed_bytes),
        128 * 1024,
    ]
    .into_iter()
    .try_fold(0u64, |total, bytes| {
        total
            .checked_add(bytes)
            .ok_or_else(|| io::Error::other("Selected payload capacity overflows"))
    })?;
    capacity
        .checked_sub(non_home)
        .filter(|bytes| *bytes > 0)
        .ok_or_else(|| io::Error::other("Selected medium has no original archive budget"))
}
pub(super) fn validate_payload_scope(
    prepared: &PreparedMedia,
    plan: &super::super::super::migration_handoff::Plan,
    cancel: &AtomicBool,
) -> io::Result<()> {
    let home_applications = prepared
        .manifest
        .portable_home
        .as_ref()
        .is_some_and(|home| home.applications);
    if home_applications && (prepared.home.is_none() || !plan.categories.home) {
        return Err(io::Error::other(
            "Home application selection has no selected Home grant",
        ));
    }
    if prepared.retained.is_some()
        || plan.categories.flatpak != (home_applications || prepared.applications.is_some())
        || (!plan.categories.accounts
            && (!prepared.account_ids.is_empty() || prepared.manifest.service_sessions.is_some()))
    {
        return Err(io::Error::other(
            "Pending source categories differ from prepared selection",
        ));
    }
    if prepared.applications.is_none()
        && let Some(raw) = &plan.flatpak_inventory
    {
        let inventory =
            crate::setup::flatpak_transfer::HandoffInventory::parse(&serde_json::to_vec(raw)?)
                .map_err(io::Error::other)?;
        if inventory.selected().iter().any(|selected| {
            matches!(
                selected.installation,
                crate::setup::flatpak_transfer::Installation::System(_)
            )
        }) {
            return Err(io::Error::other(
                "Selected system applications have no independent carrier",
            ));
        }
    }
    if let Some(apps) = &prepared.applications {
        apps.source.validate_pending_offline_scope(plan)?;
        apps.archive
            .lock()
            .map_err(|_| io::Error::other("Application custody unavailable"))?
            .verify_held_ciphertext(cancel)?;
    }
    if let Some(browser) = &prepared.browser {
        for (source, _) in browser.source.profiles() {
            source.validate()?;
        }
        browser
            .archive
            .lock()
            .map_err(|_| io::Error::other("Browser custody unavailable"))?
            .verify_held_ciphertext(cancel)?;
    }
    Ok(())
}
fn reseal(
    original: &PreparedMedia,
    declaration: prepared_setup::OfflineHomePlan,
    password: &[u8],
    cancel: &AtomicBool,
) -> io::Result<Arc<PreparedMedia>> {
    reseal_selection(
        original,
        prepared_setup::OriginalSelectionPlan::Home(declaration),
        password,
        cancel,
        None,
    )
}
enum Amendment {
    Offline(prepared_setup::OriginalSelectionPlan),
    #[cfg(target_os = "linux")]
    StagingCopy(prepared_setup::OriginalCopyDeclaration),
    #[cfg(target_os = "linux")]
    ComputerApplications(prepared_setup::SharedHomeApplicationsDeclaration),
}
fn reseal_selection(
    original: &PreparedMedia,
    declaration: prepared_setup::OriginalSelectionPlan,
    password: &[u8],
    cancel: &AtomicBool,
    pending_applications_intent: Option<super::super::super::migration_intent::MigrationIntent>,
) -> io::Result<Arc<PreparedMedia>> {
    reseal_amendment(
        original,
        Amendment::Offline(declaration),
        password,
        cancel,
        pending_applications_intent,
    )
}
#[cfg(target_os = "linux")]
pub(super) fn reseal_staging_copy(
    original: &PreparedMedia,
    declaration: prepared_setup::OriginalCopyDeclaration,
    cancel: &AtomicBool,
) -> io::Result<Arc<PreparedMedia>> {
    reseal_amendment(
        original,
        Amendment::StagingCopy(declaration),
        &original.original_password,
        cancel,
        None,
    )
}
#[cfg(target_os = "linux")]
pub(super) fn reseal_computer_applications(
    original: &PreparedMedia,
    declaration: prepared_setup::SharedHomeApplicationsDeclaration,
    cancel: &AtomicBool,
) -> io::Result<Arc<PreparedMedia>> {
    reseal_amendment(
        original,
        Amendment::ComputerApplications(declaration),
        &original.original_password,
        cancel,
        None,
    )
}
fn reseal_amendment(
    original: &PreparedMedia,
    declaration: Amendment,
    password: &[u8],
    cancel: &AtomicBool,
    pending_applications_intent: Option<super::super::super::migration_intent::MigrationIntent>,
) -> io::Result<Arc<PreparedMedia>> {
    let export_reservation = match &declaration {
        Amendment::Offline(value) => value.sealed_reservation(),
        #[cfg(target_os = "linux")]
        Amendment::StagingCopy(_) | Amendment::ComputerApplications(_) => 0,
    };
    let custody = imager_backend::session::BackendSession::create(
        imager_backend::session::SessionIntent::AmendOfflineMedia,
    )?;
    let parent_path = custody.path().join("original.sealed");
    let mut output = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&parent_path)?;
    original
        .delivery
        .lock()
        .map_err(|_| io::Error::other("Prepared custody unavailable"))?
        .copy_preparation_ciphertext(&mut output, cancel)?;
    drop(output);
    let quarantine = custody.path().join("authenticated-parent");
    let parent =
        prepared_setup::PreparedSetupReview::open(&parent_path, password, &quarantine, cancel)?;
    if parent.manifest() != &original.manifest {
        return Err(io::Error::other("Prepared selection changed"));
    }
    let mut manifest = parent.manifest().clone();
    match declaration {
        Amendment::Offline(prepared_setup::OriginalSelectionPlan::Home(plan)) => {
            manifest.offline_home = Some(plan)
        }
        Amendment::Offline(prepared_setup::OriginalSelectionPlan::Applications(plan)) => {
            manifest.offline_applications = Some(plan)
        }
        #[cfg(target_os = "linux")]
        Amendment::StagingCopy(plan) => {
            manifest.retained_home = None;
            manifest.original_copy = Some(plan);
        }
        #[cfg(target_os = "linux")]
        Amendment::ComputerApplications(plan) => {
            manifest.planned_btrfs_resize = plan.planned_resize(&manifest)?;
            manifest.computer_applications = Some(plan);
        }
    }
    drop(parent);
    let tree = quarantine.join("home");
    std::fs::remove_file(tree.join(prepared_setup::MANIFEST_FILE))?;
    manifest.write(&tree)?;
    let package = custody.path().join("prepared-setup.sealed");
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
    if let Some(browser) = &original.browser {
        copy_sidecar(
            &browser.archive,
            &custody.path().join(&browser.expectation.filename),
            cancel,
        )?;
    }
    if let Some(apps) = &original.applications {
        copy_sidecar(
            &apps.archive,
            &custody.path().join(&apps.expectation.filename),
            cancel,
        )?;
    }
    let home = if manifest.offline_applications.is_some() || {
        #[cfg(target_os = "linux")]
        {
            manifest.original_copy.is_some() || manifest.computer_applications.is_some()
        }
        #[cfg(not(target_os = "linux"))]
        {
            false
        }
    } {
        export_reservation
    } else {
        original
            .home
            .as_ref()
            .ok_or_else(|| io::Error::other("Home grant missing"))?
            .lock()
            .map_err(|_| io::Error::other("Home custody unavailable"))?
            .reservation
    };
    let reserved = [
        sealed_bytes,
        amended_setup_reservation()?,
        home,
        original
            .applications
            .as_ref()
            .map_or(0, |apps| apps.expectation.sealed_bytes),
        original
            .browser
            .as_ref()
            .map_or(0, |browser| browser.expectation.sealed_bytes),
        128 * 1024,
    ]
    .into_iter()
    .try_fold(0u64, |total, next| {
        total
            .checked_add(next)
            .ok_or_else(|| io::Error::other("Pending payload capacity overflows"))
    })?;
    std::fs::remove_dir_all(&quarantine)?;
    std::fs::remove_file(&parent_path)?;
    Ok(Arc::new(PreparedMedia::from_native(NativePreparedMedia {
        _custody: custody,
        delivery: Mutex::new(delivery),
        public_key: original.public_key,
        account_ids: original.account_ids.clone(),
        sealed_bytes,
        reserved_data_bytes: AtomicU64::new(reserved),
        expectation,
        install_profile: original.install_profile.clone(),
        other_device: original.other_device.clone(),
        tpm_handoff: original.tpm_handoff.clone(),
        retained: {
            #[cfg(target_os = "linux")]
            {
                if manifest.original_copy.is_some() || manifest.computer_applications.is_some() {
                    original.retained.clone()
                } else {
                    None
                }
            }
            #[cfg(not(target_os = "linux"))]
            {
                None
            }
        },
        pending_applications_intent,
        home: original.home.clone(),
        home_preview: original.home_preview.clone(),
        applications: original.applications.clone(),
        browser: original.browser.clone(),
        manifest,
        current_identity: original.current_identity.clone(),
        browser_password: None,
        original_password: Zeroizing::new(password.to_vec()),
        browser_frozen: true,
        delivery_started: AtomicBool::new(false),
        recovery: Mutex::new(
            original
                .recovery
                .lock()
                .map_err(|_| io::Error::other("Recovery custody unavailable"))?
                .clone(),
        ),
        transfer_generation: Mutex::new(None),
    })))
}

fn copy_sidecar(
    archive: &Mutex<super::super::super::portable_transfer::ImportReview>,
    path: &std::path::Path,
    cancel: &AtomicBool,
) -> io::Result<()> {
    let mut output = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    archive
        .lock()
        .map_err(|_| io::Error::other("Sidecar custody unavailable"))?
        .copy_ciphertext(&mut output, cancel)?;
    output.sync_all()?;
    Ok(())
}

#[cfg(all(test, feature = "native-capture-qualification"))]
mod tests {
    use super::*;
    use crate::setup::{
        account_transfer,
        flatpak_transfer::{
            Installation, InstalledRef,
            portable::{self, PortableFlatpakSource},
        },
        journey::{accounts, browser::SelectedSources},
        portable_home_media::HomeSource,
    };
    use imager_backend::setup::browser_profiles;
    use std::{fs, path::Path};
    const PASSWORD: &[u8] = b"several unrelated installation words";

    fn selected_preparation(
        root: &Path,
        independent_apps: bool,
    ) -> (Arc<PreparedMedia>, portable::tests::Fake) {
        let runner = portable::tests::fixture(root);
        let installation = root.join("original-installation");
        fs::write(installation.join("repo/config"),
            b"[core]\nmode=bare-user-only\n[remote \"flathub\"]\nurl=https://repo.example.test/repo/\ngpg-verify=true\ncollection-id=org.example.Repo\n").unwrap();
        for reference in [
            "app/org.example.Paint/x86_64/stable",
            "runtime/org.example.Platform/x86_64/stable",
        ] {
            let deployed = installation.join(reference).join("a".repeat(64));
            fs::create_dir_all(&deployed).unwrap();
            fs::write(deployed.join("deploy"), b"fixture deployment metadata").unwrap();
            std::os::unix::fs::symlink("a".repeat(64), installation.join(reference).join("active"))
                .unwrap();
        }
        let profile = runner.source_home.join(".config/chromium/Default");
        fs::create_dir_all(&profile).unwrap();
        fs::write(profile.join("Bookmarks"), b"selected bookmarks").unwrap();
        fs::write(profile.join("Cookies"), b"not a selected credential grant").unwrap();
        fs::write(runner.source_home.join("document"), b"selected full home").unwrap();
        let inventory = account_transfer::discover(&account_transfer::SourceRoots {
            home: runner.source_home.clone(),
            config: runner.source_home.join(".config"),
        });
        let browser = SelectedSources::from_reviewed(
            browser_profiles::discover(&runner.source_home, &inventory.candidates)
                .unwrap()
                .into_iter()
                .map(|source| {
                    let mut selection = source.profile.clone();
                    selection.history = false;
                    (Arc::new(source), selection)
                })
                .collect(),
        )
        .unwrap();
        // Browser discovery must run while this controlled source namespace
        // belongs to the effective test caller. Afterwards give only the Home
        // root its explicit nonroot original-owner fixture before Home/app
        // source grants are reviewed. Browser profile custody remains owned by
        // the actual caller and is validated again by the genuine producer.
        let _caller =
            crate::setup::home_capture::original_caller::fixture_source_owner(&runner.source_home);
        let app_source = PortableFlatpakSource::review(
            &runner,
            &runner.source_home,
            vec![InstalledRef {
                reference: "app/org.example.Paint/x86_64/stable".into(),
                origin: "flathub".into(),
                installation: Installation::System("default".into()),
                name: "Paint".into(),
            }],
        )
        .unwrap();
        let selected_accounts = accounts::checkpoint_fixture_for_journey(root);
        let identity = PreparedIdentity::create(
            &root.join("identity"),
            "Ada",
            [20, 80, 140],
            tilo_identity_host::provisioning::IdentityStorage::Files,
        )
        .unwrap();
        let prepared = PreparedMedia::prepare_with_runner(
            &identity,
            selected_accounts.prepared_handle(),
            MediaSelections {
                home_applications: true,
                home: Some(HomeSource::review(&runner.source_home).unwrap()),
                retained: None,
                planned_direct: None,
                applications: independent_apps.then_some(PortableAppsSelection {
                    source: app_source,
                    include_user_state: false,
                }),
                browser: Some(browser),
                sessions: None,
                mail: None,

                other_device: None,
                tpm_target: None,
            },
            Zeroizing::new(String::from_utf8(PASSWORD.to_vec()).unwrap()),
            "offline-full-selection".into(),
            None,
            &runner,
        )
        .unwrap();
        (prepared, runner)
    }
    fn declaration(
        root: &Path,
        original: &PreparedMedia,
        runner: &portable::tests::Fake,
    ) -> prepared_setup::OfflineHomePlan {
        let home = root.join("source-home");
        let caller = crate::setup::home_capture::original_caller::fixture_source_owner(&home);
        let mut source = crate::setup::migration_handoff::capture(
            &home,
            &uuid::Uuid::new_v4().to_string(),
            crate::setup::migration_handoff::Categories {
                home: true,
                flatpak: true,
                accounts: true,
            },
        )
        .unwrap();
        // Retain the exact source fixture's scoped paths; the producer crypto
        // fixture does not assert native Flatpak signature qualification.
        let selected = InstalledRef {
            reference: "app/org.example.Paint/x86_64/stable".into(),
            origin: "flathub".into(),
            installation: Installation::System("default".into()),
            name: "Paint".into(),
        };
        let inventory = crate::setup::flatpak_transfer::HandoffInventory::capture(
            runner,
            &runner.source_home,
            vec![selected],
            |installation| {
                crate::setup::migration_handoff::capture(
                    installation,
                    &source.operation_id,
                    source.categories,
                )
                .and_then(|plan| plan.bytes())
                .map_err(|error| error.to_string())
            },
        );
        // The raw source metadata pointer must never be invented from carrier
        // selection. Native source reattestation is the admission boundary.
        source.flatpak_inventory = Some(serde_json::to_value(inventory.unwrap()).unwrap());
        let (_disk_root, _path, file) = crate::media_layout::tests::disk();
        let plan = crate::media_layout::prepare(
            &file,
            &file,
            4 * 1024 * 1024,
            64 * 1024 * 1024,
            "usb:selected",
        )
        .unwrap();
        prepared_setup::OfflineHomePlan::new(
            source,
            original.manifest.setup_binding.clone(),
            original.public_key,
            plan.planned_binding().unwrap(),
            caller,
            original.home.as_ref().unwrap().lock().unwrap().reservation,
        )
        .unwrap()
    }
    #[test]
    fn applications_pending_parent_authenticates_exact_selection_without_home_or_completion() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let (home_prepared, runner) = selected_preparation(&root, false);
        let identity = PreparedIdentity::create(
            &root.join("apps-identity"),
            "Ada",
            [20, 80, 140],
            tilo_identity_host::provisioning::IdentityStorage::Files,
        )
        .unwrap();
        let ordinary = PreparedMedia::prepare(
            &identity,
            None,
            Zeroizing::new(String::from_utf8(PASSWORD.to_vec()).unwrap()),
            "apps-only-selection".into(),
        )
        .unwrap();
        let parent_bytes = fs::read(ordinary.package()).unwrap();
        let home_declaration = declaration(&root, &home_prepared, &runner);
        let mut source = home_declaration.source().clone();
        source.categories.home = false;
        source.categories.accounts = false;
        let selected = InstalledRef {
            reference: "app/org.example.Paint/x86_64/stable".into(),
            origin: "flathub".into(),
            installation: Installation::System("default".into()),
            name: "Paint".into(),
        };
        let inventory = crate::setup::flatpak_transfer::HandoffInventory::capture_selected_state(
            &runner,
            &runner.source_home,
            vec![selected.clone()],
            |installation| {
                crate::setup::migration_handoff::capture(
                    installation,
                    &source.operation_id,
                    source.categories,
                )
                .and_then(|plan| plan.bytes())
                .map_err(|error| error.to_string())
            },
        )
        .unwrap();
        source.flatpak_inventory = Some(serde_json::to_value(&inventory).unwrap());
        let (source, protected_inventory) = source.original_inventory_selection().unwrap();
        let protected_inventory = protected_inventory.unwrap();
        crate::setup::migration_handoff::OriginalInventoryBinding::parse(&source)
            .unwrap()
            .unwrap()
            .validate_bytes(&protected_inventory)
            .unwrap();
        assert_eq!(
            crate::setup::flatpak_transfer::HandoffInventory::parse(&protected_inventory).unwrap(),
            inventory
        );
        let declared = prepared_setup::OfflineApplicationsPlan::new(
            source,
            ordinary.manifest.setup_binding.clone(),
            ordinary.public_key,
            home_declaration.media().clone(),
            home_declaration.requested_caller().clone(),
            home_declaration.sealed_reservation(),
        )
        .unwrap();
        let expected_operation = declared.applications_operation().unwrap();
        assert!(
            reseal_selection(
                &ordinary,
                prepared_setup::OriginalSelectionPlan::Applications(declared.clone()),
                b"different wrong password",
                &AtomicBool::new(false),
                None,
            )
            .is_err()
        );
        assert_eq!(fs::read(ordinary.package()).unwrap(), parent_bytes);
        let pending = reseal_selection(
            &ordinary,
            prepared_setup::OriginalSelectionPlan::Applications(declared),
            PASSWORD,
            &AtomicBool::new(false),
            None,
        )
        .unwrap();
        assert!(pending.home.is_none());
        assert!(pending.applications.is_none());
        let controller = super::super::Delivery::with_artifact(pending.clone());
        assert!(controller.requires_data_partition());
        assert!(controller.requires_offline_export());
        assert!(!controller.awaiting_original_boot());
        assert_eq!(pending.applications_protocol(), Some(2));
        assert!(pending.validate_for_write(&AtomicBool::new(false)).is_err());
        let review = prepared_setup::PreparedSetupReview::open(
            &pending.package(),
            PASSWORD,
            &root.join("apps-review"),
            &AtomicBool::new(false),
        )
        .unwrap();
        let manifest = review.manifest();
        assert!(manifest.offline_home.is_none());
        assert!(manifest.portable_home.is_none());
        assert!(manifest.completed_home.is_none());
        assert!(manifest.portable_flatpak.is_none());
        let pending_apps = manifest.offline_applications.as_ref().unwrap();
        assert_eq!(
            pending_apps.applications_operation().unwrap(),
            expected_operation
        );
        let retained = crate::setup::flatpak_transfer::HandoffInventory::parse(
            &serde_json::to_vec(pending_apps.source().flatpak_inventory.as_ref().unwrap()).unwrap(),
        )
        .unwrap();
        assert!(retained.selected().contains(&selected));
        assert_eq!(retained, inventory);
        assert!(review.require_completed_export().is_err());
        assert!(
            review.pending_original_declaration().is_err(),
            "authenticated DTO without protected root registration witness must not gain read admission"
        );
        assert_eq!(fs::read(ordinary.package()).unwrap(), parent_bytes);
    }
    #[test]
    fn home_contained_application_consent_does_not_require_a_system_carrier() {
        // This regression proves selection/crypto on the production producer;
        // actual kernel/GPT admission is qualified by the separate native case.
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(runtime_identity::FILE_SECRETS_ENV, "1");
        let root = tempfile::tempdir().unwrap();
        let _facts = crate::setup::journey::main_story_tests::source_fixture(root.path());
        let (prepared, runner) = selected_preparation(root.path(), false);
        let authenticated = prepared_setup::PreparedSetupReview::open(
            &prepared.package(),
            PASSWORD,
            &root.path().join("authenticated"),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(authenticated.manifest(), &prepared.manifest);
        assert!(prepared.applications.is_none());
        assert!(
            prepared
                .manifest
                .portable_home
                .as_ref()
                .unwrap()
                .applications
        );
        let declared = declaration(root.path(), &prepared, &runner);
        let mut plan = declared.source().clone();
        // No system refs were selected: actual Home ciphertext carries user
        // repository/data, while the authenticated Home bit carries consent.
        plan.flatpak_inventory = None;
        let cancel = AtomicBool::new(false);
        validate_payload_scope(&prepared, &plan, &cancel).unwrap();
        plan.categories.flatpak = false;
        assert!(validate_payload_scope(&prepared, &plan, &cancel).is_err());
        plan.categories.flatpak = true;
        plan.categories.home = false;
        assert!(validate_payload_scope(&prepared, &plan, &cancel).is_err());
        // Actual independently reviewed System selection cannot be silently
        // covered by the Home activation bit alone.
        assert!(validate_payload_scope(&prepared, declared.source(), &cancel).is_err());
    }
    fn assert_pending_authority_accepts_activity_and_rejects_changes(
        root: &Path,
        original: &PreparedMedia,
        declared: &prepared_setup::OfflineHomePlan,
    ) {
        let home = root.join("source-home");
        let session = home.join(".var/app/org.example.Paint/config/session");
        fs::create_dir_all(session.parent().unwrap()).unwrap();
        fs::write(&session, b"ongoing app activity").unwrap();
        fs::write(home.join("document"), b"selected full home").unwrap();
        let cancel = AtomicBool::new(false);
        assert!(
            original
                .home
                .as_ref()
                .unwrap()
                .lock()
                .unwrap()
                .source
                .validate()
                .is_err()
        );
        validate_payload_scope(original, declared.source(), &cancel).unwrap();
        original
            .home
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .source
            .validate_offline_scope(declared.source())
            .unwrap();
        let key = root.join("original-installation/repo/flathub.trustedkeys.gpg");
        let original_key = fs::read(&key).unwrap();
        fs::write(&key, b"a different signing authority").unwrap();
        assert!(validate_payload_scope(original, declared.source(), &cancel).is_err());
        fs::write(&key, &original_key).unwrap();
        validate_payload_scope(original, declared.source(), &cancel).unwrap();
        let relocated = root.join("original-installation-kept");
        fs::rename(root.join("original-installation"), &relocated).unwrap();
        fs::create_dir(root.join("original-installation")).unwrap();
        assert!(validate_payload_scope(original, declared.source(), &cancel).is_err());
        fs::remove_dir(root.join("original-installation")).unwrap();
        fs::rename(&relocated, root.join("original-installation")).unwrap();
        let kept = root.join("source-home-kept");
        fs::rename(&home, &kept).unwrap();
        fs::create_dir(&home).unwrap();
        assert!(validate_payload_scope(original, declared.source(), &cancel).is_err());
        assert!(
            original
                .home
                .as_ref()
                .unwrap()
                .lock()
                .unwrap()
                .source
                .validate_offline_scope(declared.source())
                .is_err()
        );
        fs::remove_dir(&home).unwrap();
        fs::rename(&kept, &home).unwrap();
        validate_payload_scope(original, declared.source(), &cancel).unwrap();
        assert_eq!(fs::read(&session).unwrap(), b"ongoing app activity");
        assert_eq!(fs::read(&key).unwrap(), original_key);
    }
    fn assert_original_source_disks_are_excluded(declaration: &prepared_setup::OfflineHomePlan) {
        use std::os::unix::fs::MetadataExt;
        let plans = original_source_plans(declaration.source()).unwrap();
        let devices = block_devices::Lsblk::new(
            "NAME,TYPE,SIZE,START,FSTYPE,UUID,PARTUUID,PTUUID,PTTYPE,LOG-SEC,MAJ:MIN",
        )
        .full_paths(true)
        .list()
        .expect("actual source block namespace required");
        for bytes in &plans {
            let plan = crate::setup::migration_handoff::Plan::parse(bytes).unwrap();
            let matches: Vec<_> = devices
                .iter()
                .filter(|disk| {
                    disk.ptuuid
                        .as_deref()
                        .is_some_and(|id| id.eq_ignore_ascii_case(&plan.home.volume.disk_guid))
                })
                .collect();
            let [disk] = matches.as_slice() else {
                panic!("actual unique source parent required");
            };
            let number = fs::symlink_metadata(&disk.name).unwrap().rdev();
            assert!(
                crate::setup::migration_handoff::validate_offline_source_disks(&plans, number)
                    .is_err()
            );
        }
        assert!(crate::setup::migration_handoff::validate_offline_source_disks(&[], 0).is_err());
    }

    #[test]
    fn canonical_pending_reseal_preserves_home_apps_browser_accounts_and_original_ciphertext() {
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(runtime_identity::FILE_SECRETS_ENV, "1");
        let supplied = std::env::var_os("TILO_INSTALLER_NATIVE_SOURCE_STORE")
            .expect("explicit owned GPT-backed source fixture root is required");
        let supplied = Path::new(&supplied);
        let pin = crate::setup::migration::PinnedDirectory::open_absolute(supplied).unwrap();
        pin.validate_destination().unwrap();
        crate::setup::destination_security::check_private_file(&pin.file).unwrap();
        let root = tempfile::Builder::new()
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir_in(supplied)
            .unwrap();
        let (original, runner) = selected_preparation(root.path(), true);
        let original_bytes = fs::read(original.package()).unwrap();
        assert!(!original.account_ids.is_empty());
        let declared = declaration(root.path(), &original, &runner);
        assert_original_source_disks_are_excluded(&declared);
        assert_pending_authority_accepts_activity_and_rejects_changes(
            root.path(),
            &original,
            &declared,
        );

        assert!(
            reseal(
                &original,
                declared.clone(),
                b"a different incorrect password",
                &AtomicBool::new(false)
            )
            .is_err()
        );
        assert_eq!(fs::read(original.package()).unwrap(), original_bytes);
        let pending = reseal(&original, declared, PASSWORD, &AtomicBool::new(false)).unwrap();
        // The pending parent's staging paths must own actual authenticated
        // ciphertext, including after transient source generations are dropped.
        for filename in [
            &pending.browser.as_ref().unwrap().expectation.filename,
            &pending.applications.as_ref().unwrap().expectation.filename,
        ] {
            assert_eq!(
                fs::read(pending._custody.path().join(filename)).unwrap(),
                fs::read(original._custody.path().join(filename)).unwrap()
            );
        }
        assert!(pending.browser_staging_artifact().unwrap().unwrap().valid());
        let actual_parent_bytes = fs::metadata(pending.package()).unwrap().len();
        assert_eq!(actual_parent_bytes, pending.expectation.sealed_bytes);
        let selected_home_bytes = pending
            .manifest
            .offline_home
            .as_ref()
            .unwrap()
            .sealed_reservation();
        let selected_sidecars = pending.browser.as_ref().unwrap().expectation.sealed_bytes
            + pending
                .applications
                .as_ref()
                .unwrap()
                .expectation
                .sealed_bytes;
        let expected_capacity = actual_parent_bytes
            + selected_home_bytes
            + selected_sidecars
            + prepared_setup::LIMITS.sealed_bound().unwrap()
            + 128 * 1024;
        assert_eq!(
            pending.capacity_with_home(selected_home_bytes).unwrap(),
            expected_capacity
        );
        assert_eq!(
            pending.reserved_data_bytes.load(Ordering::Acquire),
            expected_capacity
        );
        assert_eq!(
            Delivery::with_artifact(pending.clone()).reserved_data_bytes(),
            expected_capacity
        );

        assert!(Arc::ptr_eq(
            original.applications.as_ref().unwrap(),
            pending.applications.as_ref().unwrap()
        ));
        assert!(Arc::ptr_eq(
            original.browser.as_ref().unwrap(),
            pending.browser.as_ref().unwrap()
        ));
        assert!(Arc::ptr_eq(
            original.home.as_ref().unwrap(),
            pending.home.as_ref().unwrap()
        ));
        assert_eq!(pending.account_ids, original.account_ids);
        let delivery = Delivery::with_artifact(Arc::clone(&pending));
        assert!(delivery.pending());
        assert_eq!(delivery.projection()["complete"], false);
        assert!(delivery.projection()["receipt"].is_null());
        assert!(delivery.projection()["home_receipt"].is_null());

        let review = prepared_setup::PreparedSetupReview::open(
            &pending.package(),
            PASSWORD,
            &root.path().join("reopened"),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(review.manifest(), &pending.manifest);
        assert_eq!(
            review.manifest().portable_flatpak,
            original.manifest.portable_flatpak
        );
        assert_eq!(review.manifest().browser, original.manifest.browser);
        assert_eq!(review.manifest().account_ids, original.manifest.account_ids);
        assert!(review.require_completed_export().is_err());
        assert_eq!(fs::read(original.package()).unwrap(), original_bytes);
        let mut corrupted = fs::read(pending.package()).unwrap();
        corrupted[20] ^= 1;
        fs::write(pending.package(), &corrupted).unwrap();
        assert!(
            prepared_setup::PreparedSetupReview::open(
                &pending.package(),
                PASSWORD,
                &root.path().join("refused"),
                &AtomicBool::new(false)
            )
            .is_err()
        );
        assert_eq!(fs::read(pending.package()).unwrap(), corrupted);
        assert_eq!(
            fs::read(root.path().join("source-home/document")).unwrap(),
            b"selected full home"
        );
    }
}

pub(in crate::setup) use imager_backend::setup::source_geometry::original_source_plans;

pub(crate) struct PreparedOriginalMedia {
    pub artifact: Arc<PreparedMedia>,
    pub review: Arc<OfflineMediaReview>,
}
impl PreparedMedia {
    /// Explicit original-source continuation from the same genuine portable
    /// directory grant. SourceChanged cannot enter this method implicitly.
    pub(crate) fn prepare_original_on_media(
        self: &Arc<Self>,
        image_review: &super::super::super::original_admission::ReviewedOriginalImage,
        target: &TargetDrive,
        original: &super::super::portable_source::OriginalSource,
        cancel: &AtomicBool,
    ) -> io::Result<PreparedOriginalMedia> {
        super::super::super::migration::check_cancel(cancel)?;
        if self.retained.is_some() {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        // Validate the exact authenticated image before source registration or
        // selected-media permission. Public Source JSON carries no proof grant.
        let applications_only = original.applications_only();
        if applications_only {
            image_review.require_applications()?;
            let (_, pins, _) =
                super::super::super::home_capture::installed_original_consumer_facts()?;
            if pins.worker_protocol != 6 {
                return Err(io::ErrorKind::Unsupported.into());
            }
        } else {
            image_review.require_protocol(5)?;
        }
        let verified_image = image_review.open_verified(cancel)?;
        let source = image_review.source();
        verified_image.validate()?;
        let intent = if applications_only {
            original.capture_applications_intent(cancel)?
        } else {
            original.capture_copy_intent(cancel)?
        };
        verified_image.validate()?;
        let grant = if applications_only {
            super::super::super::read_media::admit_applications(target, &intent, cancel)?
        } else {
            super::super::super::read_media::admit(target, &intent, cancel)?
        };
        verified_image.validate()?;
        // This is the already selected ordinary image file, never a raw device.
        // review_admitted independently matches its held identity and size.
        let mut review = OfflineMediaReview::review_admitted(
            source,
            target,
            verified_image.clone_descriptor()?,
            grant,
        )?;
        review.original_image = Some(verified_image);
        review.validate()?;
        let review = Arc::new(review);
        if applications_only {
            let artifact = self.prepare_applications_offline(
                &review,
                &intent,
                self.original_password.clone(),
                cancel,
            )?;
            return Ok(PreparedOriginalMedia { artifact, review });
        }
        let password = self
            .home
            .as_ref()
            .ok_or(io::ErrorKind::InvalidInput)?
            .lock()
            .map_err(|_| io::Error::other("Home custody unavailable"))?
            .password
            .clone();
        let artifact = self.prepare_offline(&review, &intent, password, cancel)?;
        Ok(PreparedOriginalMedia { artifact, review })
    }
}
