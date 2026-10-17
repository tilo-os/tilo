use super::{ApplicationsManifest, Deployment, Installation, OfflineRunner};
use crate::setup::migration::{PinnedDirectory, changed};
use crate::setup::{
    migration::check_cancel,
    portable_transfer::{ImportReview, Purpose},
    prepared_setup::PreparedSetupReview,
};
use cap_fs_ext::{OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_primitives::fs::{FollowSymlinks, OpenOptions};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    io,
    path::{Path, PathBuf},
    sync::Arc,
};
use std::{io::Read, sync::atomic::AtomicBool};
use zeroize::Zeroizing;
/// Authenticated private app subset. Call only after genuine Home adoption;
/// restoration changes the writable target repo, never the archived source home.
pub struct ApplicationImport {
    pub(super) review: ImportReview,
    pub(super) root: PathBuf,
    pub(super) manifest: ApplicationsManifest,
    pub(super) expectation: super::Expectation,
}
impl ApplicationImport {
    /// # Errors
    /// Refuses undeclared packages, changed ciphertext/binding/manifests and
    /// nonprivate or existing extraction destinations.
    pub fn review(
        setup: &PreparedSetupReview,
        package: &Path,
        password: &[u8],
        quarantine: &Path,
        cancel: &AtomicBool,
    ) -> io::Result<Self> {
        let expected = setup
            .manifest()
            .portable_flatpak
            .as_ref()
            .ok_or_else(|| changed("Prepared setup does not declare portable applications"))?;
        expected.validate()?;
        if expected.setup_binding != setup.manifest().setup_binding
            || package.file_name().and_then(|name| name.to_str())
                != Some(expected.filename.as_str())
        {
            return Err(changed(
                "Application package differs from the authenticated prepared setup",
            ));
        }
        Self::review_expected(expected, package, password, quarantine, cancel)
    }
    pub(in crate::setup::flatpak_transfer) fn review_expected(
        expected: &super::Expectation,
        package: &Path,
        password: &[u8],
        quarantine: &Path,
        cancel: &AtomicBool,
    ) -> io::Result<Self> {
        expected.validate()?;
        if package.file_name().and_then(|name| name.to_str()) != Some(expected.filename.as_str()) {
            return Err(changed("Applications filename differs from expectation"));
        }
        let review = ImportReview::review(
            package,
            password,
            Purpose::FlatpakApplications,
            expected.limits,
            cancel,
        )?;
        let info = review.info();
        if review.ciphertext_digest() != expected.ciphertext_sha256
            || info.sealed_bytes != expected.sealed_bytes
            || info.plaintext_bytes != expected.plaintext_bytes
            || info.entries != expected.entries
            || info.runtime_sockets != 0
            || info.setup_binding.as_deref() != Some(expected.setup_binding.as_str())
        {
            return Err(changed(
                "Application ciphertext or authenticated footer differs",
            ));
        }
        Self::review_authenticated(expected, review, password, quarantine, cancel)
    }
    /// Review received bytes without reopening a sender-controlled path. The
    /// caller must bind `expected` to its authenticated operation grant.
    pub fn review_received(
        expected: &super::Expectation,
        file: std::fs::File,
        password: &[u8],
        quarantine: &Path,
        cancel: &AtomicBool,
    ) -> io::Result<Self> {
        expected.validate()?;
        let review = ImportReview::review_file_in(
            file,
            password,
            Purpose::FlatpakApplications,
            expected.limits,
            cancel,
            crate::setup::portable_transfer::archive::Scratch::default(),
        )?;
        let info = review.info();
        if review.ciphertext_digest() != expected.ciphertext_sha256
            || info.sealed_bytes != expected.sealed_bytes
            || info.plaintext_bytes != expected.plaintext_bytes
            || info.entries != expected.entries
            || info.runtime_sockets != 0
            || info.setup_binding.as_deref() != Some(expected.setup_binding.as_str())
        {
            return Err(changed(
                "Received application archive differs from its authenticated operation",
            ));
        }
        Self::review_authenticated(expected, review, password, quarantine, cancel)
    }
    fn review_authenticated(
        expected: &super::Expectation,
        mut review: ImportReview,
        password: &[u8],
        quarantine: &Path,
        cancel: &AtomicBool,
    ) -> io::Result<Self> {
        let root = quarantine.join("home");
        match std::fs::symlink_metadata(quarantine) {
            Ok(_) => review.verify_existing_tree(&root, cancel)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                review.extract_held(Zeroizing::new(password.to_vec()), quarantine, cancel)?;
            }
            Err(error) => return Err(error),
        }
        let pin = PinnedDirectory::open_absolute(&root)?;
        let bytes = read(&pin, "applications.json", 8 * 1024 * 1024)?;
        if <[u8; 32]>::from(Sha256::digest(&bytes)) != expected.manifest_sha256 {
            return Err(changed(
                "Portable application selection or signing metadata changed",
            ));
        }
        let manifest: ApplicationsManifest = serde_json::from_slice(&bytes)?;
        manifest.validate()?;
        if manifest.schema != expected.consumer_protocol {
            return Err(changed(
                "Application consumer requirement differs from authenticated payload",
            ));
        }
        if manifest.schema == 2 {
            super::selected_state::verify_tree(&root, &manifest.app_state)?;
        }
        for item in &manifest.deployments {
            let trust = manifest
                .trust
                .iter()
                .find(|trust| trust.remote == item.remote)
                .ok_or_else(|| changed("Missing signing metadata"))?;
            let bytes = read(&pin, &trust.definition, 2 * 1024 * 1024)?;
            let canonical = super::super::extract_remote_definition(
                &trust.source,
                &root.join(&item.repository).join(".ostree"),
            )
            .map_err(io::Error::other)?;
            if canonical != bytes || <[u8; 32]>::from(Sha256::digest(&bytes)) != trust.sha256 {
                return Err(changed(
                    "Offline signing definition differs from original approved authority",
                ));
            }
        }
        Ok(Self {
            review,
            root,
            manifest,
            expectation: expected.clone(),
        })
    }
    fn extra_data_requirement(&self, runner: &impl OfflineRunner) -> io::Result<u64> {
        self.manifest
            .deployments
            .iter()
            .try_fold(0u64, |total, item| {
                let bytes = runner
                    .verify_extra_data(
                        &self.root.join(&item.repository).join(".ostree/repo"),
                        &item.commit,
                    )
                    .map_err(io::Error::other)?;
                total
                    .checked_add(bytes)
                    .ok_or_else(|| changed("Extra-data target allowance overflows"))
            })
    }
    pub fn manifest(&self) -> &ApplicationsManifest {
        &self.manifest
    }
    /// Independently re-attest package, staged bytes, exact deployed commits,
    /// original approved signature keys and target repository bindings.
    /// Existing user refs are preserved; conflicting refs require a new review.
    /// # Errors
    /// Refuses changed inputs, user-ref conflicts, missing signatures/dependencies
    /// or partial native operations. No completed status is synthesized on failure.
    pub fn restore(
        &mut self,
        runner: &impl OfflineRunner,
        home: &Path,
        cancel: &AtomicBool,
    ) -> io::Result<usize> {
        self.review.verify_existing_tree(&self.root, cancel)?;
        let extra_bytes = self.extra_data_requirement(runner)?;
        let target = PinnedDirectory::open_absolute(home)?;
        target.validate_destination()?;
        crate::setup::destination_security::check_private_file(&target.file)?;
        extra_capacity(&target, extra_bytes)?;
        let existing = super::super::discover(runner, home).map_err(io::Error::other)?;
        let mut skip = BTreeSet::new();
        for item in &self.manifest.deployments {
            if let Some(found) = existing.refs.iter().find(|found| {
                found.installation == Installation::User
                    && found.reference == item.installed.reference
            }) {
                let actual = runner
                    .run(
                        home,
                        &[
                            "info".into(),
                            "--user".into(),
                            "--show-commit".into(),
                            found.reference.clone(),
                        ],
                    )
                    .map_err(io::Error::other)?;
                if actual.trim() != item.commit {
                    return Err(changed(
                        "Existing user application/runtime differs; no data was replaced",
                    ));
                }
                skip.insert(item.installed.reference.clone());
            }
        }
        if skip.len() == self.manifest.deployments.len() {
            if !skip.is_empty() {
                runner.verify_repository(home).map_err(io::Error::other)?;
            }
            self.review.verify_existing_tree(&self.root, cancel)?;
            target.validate_destination()?;
            return Ok(skip.len());
        }
        self.add_remotes(runner, home, cancel)?;
        let mut deployed: Vec<_> = self.manifest.deployments.iter().collect();
        deployed.sort_by_key(|item| item.installed.app_id().is_some());
        for item in deployed {
            check_cancel(cancel)?;
            self.review.verify_existing_tree(&self.root, cancel)?;
            if skip.contains(&item.installed.reference) {
                continue;
            }
            deploy(runner, &self.root, item, home, &item.remote, cancel)?;
        }
        runner.verify_repository(home).map_err(io::Error::other)?;
        self.review.verify_existing_tree(&self.root, cancel)?;
        target.validate_destination()?;
        Ok(self.manifest.deployments.len())
    }
    /// Independently verify exact target deployments and already adopted user
    /// state. This cannot install a missing ref or repair a partial copy.
    pub fn verify_received_completion(
        &mut self,
        runner: &impl OfflineRunner,
        home: &Path,
        workspace: &Path,
        full_home: bool,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        check_cancel(cancel)?;
        self.review.verify_existing_tree(&self.root, cancel)?;
        let target = PinnedDirectory::open_absolute(home)?;
        target.validate_destination()?;
        crate::setup::destination_security::check_private_file(&target.file)?;
        let actual = crate::setup::user_transfer::applications::states(runner, home)
            .map_err(io::Error::other)?;
        for item in &self.manifest.deployments {
            if !actual.iter().any(|state| {
                state.reference == item.installed.reference && state.commit == item.commit
            }) {
                return Err(changed(
                    "A selected target application deployment is incomplete or changed",
                ));
            }
        }
        if !self.manifest.deployments.is_empty() {
            runner.verify_repository(home).map_err(io::Error::other)?;
        }
        if !full_home && self.manifest.schema == 2 {
            self.verify_adopted_selected_state(home, workspace, cancel)?;
        } else if !full_home && self.manifest.user_state {
            let source_grant = self.state_grant(home, workspace, cancel)?;
            let source_home = self.root.join("user-state");
            let scoped = super::scoped::PackageRunner::new(
                runner,
                &self.root,
                &source_home,
                &self.manifest,
                cancel,
            );
            let request = crate::setup::user_transfer::TransferRequest {
                source_home: source_home.clone(),
                target_home: workspace.join("user-state-copy"),
                private_journals: workspace.join("copy-journals"),
                copy_home: false,
                copy_flatpak: true,
                system_refs: scoped.selected_refs().map_err(io::Error::other)?,
            };
            let (_, promotion) =
                crate::setup::user_transfer::recover_portable_promotion_with_runner(
                    &scoped,
                    request,
                    home,
                    &workspace.join("adoption.journal"),
                    &source_grant,
                )
                .map_err(io::Error::other)?;
            promotion.verify_completed().map_err(io::Error::other)?;
        }
        self.review.verify_existing_tree(&self.root, cancel)?;
        target.validate_destination()
    }
    fn preflight_refs(&self, runner: &impl OfflineRunner, home: &Path) -> io::Result<()> {
        let existing = super::super::discover(runner, home).map_err(io::Error::other)?;
        for item in &self.manifest.deployments {
            if existing.refs.iter().any(|found| {
                found.installation == Installation::User
                    && found.reference == item.installed.reference
            }) {
                let actual = runner
                    .run(
                        home,
                        &[
                            "info".into(),
                            "--user".into(),
                            "--show-commit".into(),
                            item.installed.reference.clone(),
                        ],
                    )
                    .map_err(io::Error::other)?;
                if actual.trim() != item.commit {
                    return Err(changed(
                        "Existing user application/runtime differs; no data was replaced",
                    ));
                }
            }
        }
        Ok(())
    }
    /// Explicitly adopt only authenticated user Flatpak state when the person
    /// deselected their documents. The canonical namespace/copy/promotion engine
    /// preserves collisions and private Tilo custody.
    /// # Errors
    /// Refuses incomplete copies, changed receipts, collisions or cancellation.
    pub fn adopt_user_state(
        &mut self,
        runner: &impl OfflineRunner,
        home: &Path,
        workspace: &Path,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        if self.manifest.schema == 2 {
            self.preflight_refs(runner, home)?;
            return self.adopt_selected_state(home, workspace, cancel);
        }
        if !self.manifest.user_state {
            return Ok(());
        }
        self.review.verify_existing_tree(&self.root, cancel)?;
        let extra_bytes = self.extra_data_requirement(runner)?;
        let target = PinnedDirectory::open_absolute(home)?;
        target.validate_destination()?;
        crate::setup::destination_security::check_private_file(&target.file)?;
        extra_capacity(&target, extra_bytes)?;
        let held = PinnedDirectory::open_absolute(workspace)?;
        held.validate_destination()?;
        crate::setup::destination_security::check_private_file(&held.file)?;
        if !workspace.starts_with(home.join(".local/state/tilo/portable-applications")) {
            return Err(changed(
                "Application adoption requires its private target workspace",
            ));
        }
        let journals = workspace.join("copy-journals");
        match std::fs::symlink_metadata(&journals) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                held.create_child(std::ffi::OsStr::new("copy-journals"))?;
            }
            Ok(_) => {
                PinnedDirectory::open_absolute(&journals)?.validate_destination()?;
            }
            Err(error) => return Err(error),
        }
        let source_grant = self.state_grant(home, workspace, cancel)?;
        let source_home = self.root.join("user-state");
        let scoped = super::scoped::PackageRunner::new(
            runner,
            &self.root,
            &source_home,
            &self.manifest,
            cancel,
        );
        let request = crate::setup::user_transfer::TransferRequest {
            source_home: source_home.clone(),
            target_home: workspace.join("user-state-copy"),
            private_journals: journals,
            copy_home: false,
            copy_flatpak: true,
            system_refs: scoped.selected_refs().map_err(io::Error::other)?,
        };
        let journal = workspace.join("adoption.journal");
        let (promotion, retry) = match std::fs::symlink_metadata(&journal) {
            Ok(_) => {
                let (_, promotion) =
                    crate::setup::user_transfer::recover_portable_promotion_with_runner(
                        &scoped,
                        request,
                        home,
                        &journal,
                        &source_grant,
                    )
                    .map_err(io::Error::other)?;
                (promotion, true)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let transfer = crate::setup::user_transfer::review(&scoped, request)
                    .map_err(io::Error::other)?;
                let report = transfer
                    .execute(&scoped, cancel, transfer.recovered(), |_| {})
                    .map_err(io::Error::other)?;
                if !report.complete() {
                    return Err(changed("Independent app state copy is incomplete"));
                }
                (
                    transfer
                        .review_portable_promotion(home, &journal, &source_grant)
                        .map_err(io::Error::other)?,
                    false,
                )
            }
            Err(error) => return Err(error),
        };
        let mut worker = promotion.start(retry).map_err(io::Error::other)?;
        loop {
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                worker.cancel();
            }
            match worker.try_next() {
                Some(crate::setup::home_transfer::promotion::PromotionEvent::Finished(report))
                    if report.complete() =>
                {
                    break;
                }
                Some(_) => {
                    return Err(changed(
                        "Independent app state adoption is incomplete; existing files retained",
                    ));
                }
                None => std::thread::sleep(std::time::Duration::from_millis(10)),
            }
        }
        self.review.verify_existing_tree(&self.root, cancel)
    }
    fn add_remotes(
        &self,
        runner: &impl OfflineRunner,
        home: &Path,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        let pin = PinnedDirectory::open_absolute(&self.root)?;
        for trust in &self.manifest.trust {
            check_cancel(cancel)?;
            let definition = read(&pin, &trust.definition, 2 * 1024 * 1024)?;
            if <[u8; 32]>::from(Sha256::digest(&definition)) != trust.sha256 {
                return Err(changed(
                    "Original signing authority differs from authenticated manifest",
                ));
            }
            let file = self.root.join(&trust.definition);
            runner
                .run(
                    home,
                    &[
                        "remote-add".into(),
                        "--user".into(),
                        "--if-not-exists".into(),
                        "--from".into(),
                        trust.remote.clone(),
                        file.to_str()
                            .ok_or_else(|| changed("Non-UTF8 approved definition"))?
                            .into(),
                    ],
                )
                .map_err(io::Error::other)?;
        }
        Ok(())
    }
}
fn read(pin: &Arc<PinnedDirectory>, name: &str, limit: u64) -> io::Result<Vec<u8>> {
    pin.validate_destination()?;
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No).nonblock(true);
    let file = cap_primitives::fs::open(&pin.file, Path::new(name), &options)?;
    if !file.metadata()?.is_file() || file.metadata()?.len() > limit {
        return Err(changed("Invalid bounded application metadata"));
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(changed("Application metadata exceeds bounds"));
    }
    pin.validate_destination()?;
    Ok(bytes)
}

pub(super) fn deploy(
    runner: &impl OfflineRunner,
    root: &Path,
    item: &Deployment,
    home: &Path,
    remote: &str,
    cancel: &AtomicBool,
) -> io::Result<()> {
    check_cancel(cancel)?;
    let repository = home.join(".local/share/flatpak/repo");
    import_commit(runner, root, item, &repository, remote)?;
    runner.verify_repository(home).map_err(io::Error::other)?;
    runner
        .run(
            home,
            &[
                "install".into(),
                "--user".into(),
                "--noninteractive".into(),
                "--assumeyes".into(),
                "--no-pull".into(),
                "--no-deps".into(),
                "--no-related".into(),
                remote.to_owned(),
                item.installed.reference.clone(),
            ],
        )
        .map_err(io::Error::other)?;
    let actual = runner
        .run(
            home,
            &[
                "info".into(),
                "--user".into(),
                "--show-commit".into(),
                item.installed.reference.clone(),
            ],
        )
        .map_err(io::Error::other)?;
    let origin = runner
        .run(
            home,
            &[
                "info".into(),
                "--user".into(),
                "--show-origin".into(),
                item.installed.reference.clone(),
            ],
        )
        .map_err(io::Error::other)?;
    if actual.trim() != item.commit || origin.trim() != remote {
        return Err(changed(
            "Restored application differs from the reviewed signed deployment",
        ));
    }
    Ok(())
}

fn import_commit(
    runner: &impl OfflineRunner,
    root: &Path,
    item: &Deployment,
    repository: &Path,
    remote: &str,
) -> io::Result<()> {
    let source = root.join(&item.repository).join(".ostree/repo");
    let source_pin = PinnedDirectory::open_absolute(&source)?;
    runner
        .ostree(
            repository,
            &[
                "pull-local".into(),
                "--untrusted".into(),
                "--gpg-verify".into(),
                format!("--remote={remote}"),
                source
                    .to_str()
                    .ok_or_else(|| changed("Non-UTF8 offline repository"))?
                    .into(),
                item.commit.clone(),
            ],
        )
        .map_err(io::Error::other)?;
    source_pin.validate()?;
    let refs = runner
        .ostree(repository, &["refs".into(), "--revision".into()])
        .map_err(io::Error::other)?;
    let name = format!("{}:{}", remote, item.installed.reference);
    let existing = refs
        .lines()
        .filter_map(|line| line.split_once('\t'))
        .find(|(reference, _)| *reference == name);
    match existing {
        Some((_, checksum)) if checksum == item.commit => {}
        Some(_) => {
            return Err(changed(
                "Existing offline repository ref differs; it was not replaced",
            ));
        }
        None => {
            runner
                .ostree(
                    repository,
                    &[
                        "refs".into(),
                        format!("--create={name}"),
                        item.commit.clone(),
                    ],
                )
                .map_err(io::Error::other)?;
        }
    }
    Ok(())
}

fn extra_capacity(target: &PinnedDirectory, required: u64) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    if required == 0 {
        return Ok(());
    }
    let required = required
        .checked_add(16 * 1024 * 1024)
        .ok_or_else(|| changed("Extra-data target allowance overflows"))?;
    let mut status = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: held target descriptor and initialized bounded native output storage.
    if unsafe { libc::fstatvfs(target.file.as_raw_fd(), status.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful fstatvfs initialized every native field.
    let status = unsafe { status.assume_init() };
    let available = u128::from(status.f_bavail) * u128::from(status.f_frsize);
    target.validate_destination()?;
    if available < u128::from(required) {
        return Err(changed(
            "The target needs more free space for the reviewed extra-data downloads and installed application payloads",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod extra_capacity_tests {
    use super::*;
    use std::{
        fs,
        os::{fd::AsRawFd, unix::fs::PermissionsExt},
    };
    #[test]
    fn actual_target_extra_data_capacity_refuses_before_mutation() {
        let root = tempfile::tempdir().unwrap();
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(root.path().join("sentinel"), b"original target").unwrap();
        let pin = PinnedDirectory::open_absolute(root.path()).unwrap();
        let mut status = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        // SAFETY: held private fixture descriptor and complete native output buffer.
        assert_eq!(
            unsafe { libc::fstatvfs(pin.file.as_raw_fd(), status.as_mut_ptr()) },
            0
        );
        // SAFETY: successful fstatvfs initialized the native structure.
        let status = unsafe { status.assume_init() };
        let free =
            u64::try_from(u128::from(status.f_bavail) * u128::from(status.f_frsize)).unwrap();
        assert!(
            free > 32 * 1024 * 1024,
            "observable fixture volume needs minimal test capacity"
        );
        assert!(
            extra_capacity(&pin, free)
                .unwrap_err()
                .to_string()
                .contains("more free space")
        );
        assert_eq!(
            fs::read(root.path().join("sentinel")).unwrap(),
            b"original target"
        );
        assert!(!root.path().join(".local").exists());
        extra_capacity(&pin, 1).unwrap();
    }
}
