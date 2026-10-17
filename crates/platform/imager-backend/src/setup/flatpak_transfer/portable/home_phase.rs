//! Full-home application continuation: original archive fidelity remains proof,
//! while a repository checkpoint is compared against real bytes on every retry.
use super::{ApplicationImport, ApplicationsManifest, OfflineRunner};
use crate::setup::{
    migration::{PinnedDirectory, changed},
    migration_durability as durable,
    portable_home_import::PortableHomeReview,
    user_transfer::{
        TransferRequest,
        applications::{self, RefState},
    },
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    ffi::OsStr,
    io::{self, Read},
    path::Path,
    sync::atomic::AtomicBool,
};
use tilo_files::native::atomic::{Privacy, PublishMode, StorageDirectory};
const BASELINE: &str = "home-applications-original.json";
const OUTPUT: &str = "home-applications-output.json";
const LIMIT: u64 = 16 * 1024 * 1024;
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Baseline {
    schema: u32,
    home: durable::Identity,
    source: [u8; 32],
    selection: super::Expectation,
    original_refs: Vec<RefState>,
    original_config: Vec<((String, String), String)>,
    original_config_bytes: Vec<u8>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Output {
    schema: u32,
    digest: [u8; 32],
    refs: Vec<RefState>,
}
fn request(home: &Path, workspace: &Path) -> TransferRequest {
    TransferRequest {
        source_home: home.into(),
        target_home: home.into(),
        private_journals: workspace.into(),
        copy_home: false,
        copy_flatpak: true,
        system_refs: Vec::new(),
    }
}
fn storage(workspace: &Path) -> io::Result<StorageDirectory> {
    let pin = PinnedDirectory::open_absolute(workspace)?;
    pin.validate_destination()?;
    crate::setup::destination_security::check_private_file(&pin.file)?;
    StorageDirectory::from_file(durable::readable_directory(&pin.file)?)
}
fn read<T: serde::de::DeserializeOwned>(
    store: &StorageDirectory,
    name: &str,
) -> io::Result<Option<T>> {
    let file = match store.open_read(OsStr::new(name), Privacy::OwnerOnly) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    if !file.metadata()?.is_file() || file.metadata()?.len() > LIMIT {
        return Err(changed("Application proof exceeds its private bounds"));
    }
    let mut bytes = Vec::new();
    file.take(LIMIT + 1).read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).map_err(io::Error::other)? > LIMIT {
        return Err(changed("Application proof exceeds bounds"));
    }
    Ok(Some(serde_json::from_slice(&bytes)?))
}
fn publish(store: &StorageDirectory, name: &str, value: &impl Serialize) -> io::Result<()> {
    let bytes = serde_json::to_vec(value)?;
    if u64::try_from(bytes.len()).map_err(io::Error::other)? > LIMIT {
        return Err(changed("Application proof exceeds bounds"));
    }
    store
        .publish(
            OsStr::new(name),
            &bytes,
            PublishMode::IfAbsent,
            Privacy::OwnerOnly,
        )
        .map_err(|e| e.into_io_error())
}
fn lock(store: &StorageDirectory) -> io::Result<advisory_lock::FileLock> {
    match store.publish(
        OsStr::new("home-applications.lock"),
        &[],
        PublishMode::IfAbsent,
        Privacy::OwnerOnly,
    ) {
        Ok(()) => {}
        Err(e) if e.io_error().kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.into_io_error()),
    }
    advisory_lock::FileLock::try_exclusive(
        store.open_append_existing(OsStr::new("home-applications.lock"), Privacy::OwnerOnly)?,
    )
    .map_err(io::Error::other)
}
fn configuration(home: &Path) -> io::Result<BTreeMap<(String, String), String>> {
    let path = home.join(".local/share/flatpak/repo/config");
    match std::fs::symlink_metadata(&path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(e) => Err(e),
        Ok(_) => applications::config(&path).map_err(io::Error::other),
    }
}
fn configuration_bytes(home: &Path) -> io::Result<Vec<u8>> {
    let path = home.join(".local/share/flatpak/repo/config");
    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
        Ok(_) => {}
    }
    let pin = PinnedDirectory::open_absolute(
        path.parent()
            .ok_or_else(|| changed("Repository parent missing"))?,
    )?;
    let mut options = cap_primitives::fs::OpenOptions::new();
    cap_fs_ext::OpenOptionsFollowExt::follow(
        options.read(true),
        cap_primitives::fs::FollowSymlinks::No,
    );
    let file = cap_primitives::fs::open(&pin.file, Path::new("config"), &options)?;
    if !file.metadata()?.is_file() || file.metadata()?.len() > 2 * 1024 * 1024 {
        return Err(changed("Original configuration exceeds bounds"));
    }
    let mut bytes = Vec::new();
    file.take(2 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
    pin.validate()?;
    Ok(bytes)
}
fn validate_baseline(
    baseline: &Baseline,
    import: &ApplicationImport,
    home_review: &mut PortableHomeReview,
    home: &Path,
    cancel: &AtomicBool,
) -> io::Result<()> {
    let pin = PinnedDirectory::open_absolute(home)?;
    pin.validate_destination()?;
    if baseline.schema != 1
        || baseline.source != home_review.ciphertext_digest()
        || baseline.selection != import.expectation
        || baseline.home != durable::identity(&durable::readable_directory(&pin.file)?)?
    {
        return Err(changed(
            "Original application proof belongs to a different migration",
        ));
    }
    home_review.verify_completed_after_applications(cancel)?;
    home_review.verify_original_application_config(&baseline.original_config_bytes)?;
    let original =
        std::str::from_utf8(&baseline.original_config_bytes).map_err(io::Error::other)?;
    if applications::parse_config(original).map_err(io::Error::other)?
        != baseline.original_config.iter().cloned().collect()
    {
        return Err(changed("Original repository settings proof changed"));
    }
    let actual = configuration(home)?;
    if baseline
        .original_config
        .iter()
        .any(|(key, value)| actual.get(key) != Some(value))
    {
        return Err(changed(
            "An original repository setting or signing authority changed",
        ));
    }
    Ok(())
}
fn verify_refs(
    runner: &impl OfflineRunner,
    manifest: &ApplicationsManifest,
    baseline: &Baseline,
    home: &Path,
    complete: bool,
) -> io::Result<Vec<RefState>> {
    let actual = applications::states(runner, home).map_err(io::Error::other)?;
    let original: BTreeMap<_, _> = baseline
        .original_refs
        .iter()
        .map(|item| (item.reference.as_str(), item))
        .collect();
    let mut allowed = original.clone();
    let additions: Vec<_> = manifest
        .deployments
        .iter()
        .map(|item| RefState {
            reference: item.installed.reference.clone(),
            origin: item.remote.clone(),
            commit: item.commit.clone(),
        })
        .collect();
    for item in &additions {
        allowed.entry(&item.reference).or_insert(item);
    }
    if baseline
        .original_refs
        .iter()
        .any(|item| !actual.contains(item))
        || actual.iter().any(|item| {
            allowed
                .get(item.reference.as_str())
                .is_none_or(|expected| *expected != item)
        })
        || (complete && allowed.values().any(|item| !actual.contains(item)))
    {
        return Err(changed(
            "Original or selected deployed application refs differ",
        ));
    }
    Ok(actual)
}
impl ApplicationImport {
    /// Renew full-home fidelity plus actual original/signed application proof.
    /// This performs no migration or installation.
    /// # Errors
    /// Refuses altered original data, moved identities, settings, refs or checkpoint bytes.
    pub fn verify_home_for_applications(
        &mut self,
        home_review: &mut PortableHomeReview,
        runner: &impl OfflineRunner,
        home: &Path,
        workspace: &Path,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        let store = storage(workspace)?;
        let _guard = lock(&store)?;
        match read::<Baseline>(&store, BASELINE)? {
            None => home_review.verify_completed_adoption(cancel),
            Some(baseline) => self.validate_home_phase(
                home_review,
                runner,
                home,
                workspace,
                cancel,
                &store,
                &baseline,
            ),
        }
    }
    fn validate_home_phase(
        &mut self,
        home_review: &mut PortableHomeReview,
        runner: &impl OfflineRunner,
        home: &Path,
        workspace: &Path,
        cancel: &AtomicBool,
        store: &StorageDirectory,
        baseline: &Baseline,
    ) -> io::Result<()> {
        self.review.verify_existing_tree(&self.root, cancel)?;
        validate_baseline(baseline, self, home_review, home, cancel)?;
        let output = read::<Output>(store, OUTPUT)?;
        if let Some(output) = &output {
            if output.schema != 1
                || output.digest
                    != applications::tree_digest(&request(home, workspace))
                        .map_err(io::Error::other)?
            {
                return Err(changed("Completed application repository bytes changed"));
            }
        }
        let actual = verify_refs(runner, &self.manifest, baseline, home, output.is_some())?;
        if output.as_ref().is_some_and(|output| output.refs != actual) {
            return Err(changed("Completed application refs changed"));
        }
        runner.verify_repository(home).map_err(io::Error::other)?;
        if let Some(output) = &output {
            if output.digest
                != applications::tree_digest(&request(home, workspace)).map_err(io::Error::other)?
            {
                return Err(changed(
                    "Application repository changed during native verification",
                ));
            }
        }
        validate_baseline(baseline, self, home_review, home, cancel)
    }
    /// Restore exact signed refs after full-home adoption, retaining independently
    /// verifiable original and output state for interruption/restart.
    /// # Errors
    /// Refuses altered grants/data/proofs and incomplete signature/deployment checks.
    pub fn restore_after_home(
        &mut self,
        home_review: &mut PortableHomeReview,
        runner: &impl OfflineRunner,
        home: &Path,
        workspace: &Path,
        cancel: &AtomicBool,
    ) -> io::Result<usize> {
        let store = storage(workspace)?;
        let _guard = lock(&store)?;
        let baseline = if let Some(baseline) = read::<Baseline>(&store, BASELINE)? {
            self.validate_home_phase(
                home_review,
                runner,
                home,
                workspace,
                cancel,
                &store,
                &baseline,
            )?;
            baseline
        } else {
            home_review.verify_completed_adoption(cancel)?;
            let pin = PinnedDirectory::open_absolute(home)?;
            let baseline = Baseline {
                schema: 1,
                home: durable::identity(&durable::readable_directory(&pin.file)?)?,
                source: home_review.ciphertext_digest(),
                selection: self.expectation.clone(),
                original_refs: applications::states(runner, home).map_err(io::Error::other)?,
                original_config: configuration(home)?.into_iter().collect(),
                original_config_bytes: configuration_bytes(home)?,
            };
            home_review.verify_original_application_config(&baseline.original_config_bytes)?;
            publish(&store, BASELINE, &baseline)?;
            baseline
        };
        let restored = self.restore(runner, home, cancel)?;
        validate_baseline(&baseline, self, home_review, home, cancel)?;
        let refs = verify_refs(runner, &self.manifest, &baseline, home, true)?;
        let digest =
            applications::tree_digest(&request(home, workspace)).map_err(io::Error::other)?;
        if let Some(prior) = read::<Output>(&store, OUTPUT)? {
            if prior.schema != 1 || prior.digest != digest || prior.refs != refs {
                return Err(changed("Completed application output changed"));
            }
        } else {
            publish(
                &store,
                OUTPUT,
                &Output {
                    schema: 1,
                    digest,
                    refs,
                },
            )?;
        }
        self.validate_home_phase(
            home_review,
            runner,
            home,
            workspace,
            cancel,
            &store,
            &baseline,
        )?;
        Ok(restored)
    }
}
