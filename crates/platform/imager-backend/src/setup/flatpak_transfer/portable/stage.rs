use super::{
    ApplicationsManifest, Deployment, OfflineRunner, OriginalInstallation, PortableFlatpakSource,
    Remote, Trust, commit,
};
use crate::setup::{
    destination_security,
    migration::{changed, check_cancel},
    migration_durability as durable,
    portable_transfer::{ExportReview, ImportReview, Limits, Purpose},
};
use crate::setup::{migration::PinnedDirectory, migration_review::SourceInventory};
use cap_fs_ext::{OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_primitives::fs::{FollowSymlinks, OpenOptions, OpenOptionsExt};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{ffi::OsStr, io::Write, sync::atomic::AtomicBool};
use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
};
use zeroize::Zeroizing;
pub struct StageGrant<'a> {
    pub root: &'a Path,
    pub journals: &'a Path,
    pub allocation_bytes: u64,
    pub include_user_state: bool,
}
const MANIFEST: &str = "applications.json";
/// A selected system subset in explicitly granted private staging. No source
/// user repo or app data is duplicated here.
pub struct StagedApplications {
    source: Arc<PortableFlatpakSource>,
    root: PathBuf,
    pin: Arc<PinnedDirectory>,
    inventory: SourceInventory,
    manifest_sha256: [u8; 32],
    consumer_protocol: u32,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expectation {
    pub schema: u32,
    #[serde(
        default = "legacy_protocol",
        skip_serializing_if = "is_legacy_protocol"
    )]
    pub consumer_protocol: u32,
    pub operation: String,
    pub purpose: Purpose,
    pub filename: String,
    pub setup_binding: String,
    pub sealed_bytes: u64,
    pub plaintext_bytes: u64,
    pub entries: usize,
    pub ciphertext_sha256: [u8; 32],
    pub manifest_sha256: [u8; 32],
    pub limits: Limits,
}
fn legacy_protocol() -> u32 {
    1
}
fn is_legacy_protocol<T: PartialEq<u32>>(value: &T) -> bool {
    *value == 1
}
impl Expectation {
    /// # Errors
    /// Refuses malformed public locators and overflowing storage bounds.
    pub fn validate(&self) -> io::Result<()> {
        if self.schema != 1
            || !matches!(self.consumer_protocol, 1 | 2)
            || self.purpose != Purpose::FlatpakApplications
            || uuid::Uuid::parse_str(&self.operation).is_err()
            || self.filename != format!("applications-{}.sealed", self.operation)
            || self.setup_binding.is_empty()
            || self.setup_binding.len() > 256
            || self.setup_binding.chars().any(char::is_control)
            || self.sealed_bytes == 0
            || self.plaintext_bytes == 0
            || self.plaintext_bytes > self.limits.plaintext_bytes
            || self.entries == 0
            || self.entries > self.limits.entries
            || self.limits.entries > 1_000_000
            || self.limits.metadata_bytes == 0
            || self.limits.metadata_bytes > 512 * 1024 * 1024
            || self
                .limits
                .sealed_bound()
                .is_none_or(|bound| self.sealed_bytes > bound)
        {
            return Err(changed("Invalid portable application expectation"));
        }
        Ok(())
    }
}
impl PortableFlatpakSource {
    /// Stage only actual selected apps and create-usb's exported runtime/extension
    /// closure. The caller owns this explicitly granted temporary directory and
    /// must remove it after delivery/cancellation. Partial output is never ready.
    /// # Errors
    /// Refuses nonprivate/overlapping stages, missing dependencies, unsigned
    /// origins, insufficient allocation and source changes.
    pub fn stage(
        self: &Arc<Self>,
        runner: &impl OfflineRunner,
        grant: StageGrant<'_>,
        cancel: &AtomicBool,
    ) -> io::Result<StagedApplications> {
        let root = grant.root;
        let allocation_bytes = grant.allocation_bytes;
        self.validate()?;
        check_cancel(cancel)?;
        let pin = PinnedDirectory::open_absolute(root)?;
        pin.validate_destination()?;
        destination_security::check_private_file(&pin.file)?;
        if root.starts_with(&self.home)
            || self.originals.iter().any(|source| {
                pin.contains_identity(&source.pin.identity)
                    || source.pin.contains_identity(&pin.identity)
            })
        {
            return Err(changed(
                "Application stage must be outside every source installation",
            ));
        }
        let directory = cap_std::fs::Dir::from_std_file(durable::readable_directory(&pin.file)?);
        if directory.entries()?.next().is_some() || allocation_bytes == 0 {
            return Err(changed(
                "Application stage must be empty and explicitly allocated",
            ));
        }
        if self.selected_state.is_some() && !grant.include_user_state {
            return Err(changed(
                "Application data consent differs from reviewed source",
            ));
        }
        check_allocation(self, &pin, &grant)?;
        let mut manifest = ApplicationsManifest {
            schema: if self.selected_state.is_some() { 2 } else { 1 },
            selected: self.selected.clone(),
            deployments: Vec::new(),
            trust: Vec::new(),
            user_state: grant.include_user_state && self.selected_state.is_none(),
            app_state: self
                .selected_state
                .as_ref()
                .map_or_else(Vec::new, |state| state.declarations()),
        };
        if let Some(state) = &self.selected_state {
            state.stage(root, grant.journals, cancel)?;
        } else if grant.include_user_state {
            stage_user_state(self, runner, &grant, cancel)?;
        }
        for (index, original) in self.originals.iter().enumerate() {
            check_cancel(cancel)?;
            let name = format!("installation-{index}");
            pin.create_child(OsStr::new(&name))?;
            let selected: Vec<_> = self
                .selected
                .iter()
                .filter(|item| item.installation == original.scope)
                .map(|item| item.reference.clone())
                .collect();
            let destination = root.join(&name);
            let destination_pin = PinnedDirectory::open_absolute(&destination)?;
            let grant = super::SubsetExportGrant {
                source: super::SourceExportGrant { original },
                destination: &destination_pin,
                path: &destination,
            };
            if !runner
                .export_offline_subset(&grant)
                .map_err(io::Error::other)?
            {
                super::super::export_sideload(
                    runner,
                    &self.home,
                    &original.scope,
                    &selected,
                    &root.join(&name),
                )
                .map_err(io::Error::other)?;
            }
            let repo = root.join(&name).join(".ostree/repo");
            let exported = runner
                .ostree(
                    &repo,
                    &["refs".into(), "--collections".into(), "--revision".into()],
                )
                .map_err(io::Error::other)?;
            add_exported(&mut manifest, original, &name, &exported, &pin)?;
            verify_dependency_export(&manifest, original)?;
            if SourceInventory::capture_home(&pin)?.summary()?.bytes > allocation_bytes {
                return Err(changed(
                    "Selected applications exceeded the granted staging allocation",
                ));
            }
        }
        manifest.validate()?;
        let bytes = serde_json::to_vec(&manifest)?;
        if bytes.len() > 8 * 1024 * 1024 {
            return Err(changed("Application manifest exceeds bounds"));
        }
        write(&pin, MANIFEST, &bytes)?;
        self.validate()?;
        let inventory = SourceInventory::capture_home(&pin)?;
        let summary = inventory.summary()?;
        if summary.bytes > allocation_bytes
            || summary.special_files != 0
            || summary.runtime_sockets != 0
        {
            return Err(changed(
                "Application stage exceeds allocation or contains special files",
            ));
        }
        Ok(StagedApplications {
            source: self.clone(),
            root: root.into(),
            pin,
            inventory,
            manifest_sha256: Sha256::digest(&bytes).into(),
            consumer_protocol: manifest.schema,
        })
    }
}
fn verify_dependency_export(
    manifest: &ApplicationsManifest,
    original: &OriginalInstallation,
) -> io::Result<()> {
    let actual = manifest
        .deployments
        .iter()
        .filter(|item| item.installed.installation == original.scope)
        .map(|item| item.installed.reference.clone())
        .collect();
    super::dependencies::verify_exported(&original.required, &actual).map_err(io::Error::other)
}
fn add_exported(
    manifest: &mut ApplicationsManifest,
    original: &OriginalInstallation,
    repository: &str,
    text: &str,
    pin: &Arc<PinnedDirectory>,
) -> io::Result<()> {
    for line in text.lines() {
        let (collection_ref, checksum) = line
            .split_once('\t')
            .ok_or_else(|| changed("Malformed exported dependency listing"))?;
        let tuple = collection_ref
            .strip_prefix('(')
            .and_then(|v| v.strip_suffix(')'))
            .ok_or_else(|| changed("Invalid exported collection ref"))?;
        let (_, reference) = tuple
            .split_once(", ")
            .ok_or_else(|| changed("Invalid exported collection"))?;
        // create-usb also exports repository/appstream metadata, not deployed
        // application/runtime refs (upstream create-usb.c638-665).
        if reference == "ostree-metadata" || reference.starts_with("appstream") {
            continue;
        }
        if !super::super::reference(reference) || !commit(checksum) {
            return Err(changed("Invalid exported deployment"));
        }
        let (item, original_commit) = original
            .deployments
            .iter()
            .find(|(item, value)| item.reference == reference && value == checksum)
            .ok_or_else(|| changed("Exported dependency differs from original deployed version"))?;
        if original_commit != checksum {
            return Err(changed("Exported commit changed"));
        }
        if item.app_id().is_some() && !manifest.selected.contains(item) {
            return Err(changed("Offline export included an unselected application"));
        }
        let (remote, definition) = original
            .remotes
            .iter()
            .find(|(remote, _)| remote.name == item.origin)
            .ok_or_else(|| changed("Exported dependency has no approved signing authority"))?;
        let id = format!("tilo-portable-{}", manifest.trust.len());
        let trust = if let Some(existing) = manifest.deployments.iter().find(|entry| {
            entry.installed.installation == item.installation
                && entry.installed.origin == remote.name
        }) {
            existing.remote.clone()
        } else {
            let filename = format!("{id}.flatpakrepo");
            write(pin, &filename, definition)?;
            manifest.trust.push(Trust {
                remote: id.clone(),
                definition: filename,
                sha256: Sha256::digest(definition).into(),
                source: remote.clone(),
            });
            id
        };
        publish_signing_key(pin, repository, remote, definition)?;
        manifest.deployments.push(Deployment {
            installed: item.clone(),
            commit: checksum.into(),
            repository: repository.into(),
            remote: trust,
        });
    }
    Ok(())
}
fn write(pin: &Arc<PinnedDirectory>, name: &str, bytes: &[u8]) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options
        .write(true)
        .create_new(true)
        .mode(0o600)
        .follow(FollowSymlinks::No)
        .nonblock(true);
    let mut file = cap_primitives::fs::open(&pin.file, Path::new(name), &options)?;
    file.write_all(bytes)?;
    durable::sync_file(&file)?;
    durable::sync_directory(&pin.file)
}
impl StagedApplications {
    /// # Errors
    /// Refuses changed originals or staged subset bytes/metadata.
    pub fn validate(&self) -> io::Result<()> {
        self.source.validate()?;
        self.pin.validate_destination()?;
        self.inventory.validate(&self.pin)
    }
    /// Seal the reviewed subset outside plaintext staging. The authenticated
    /// expectation must enter the small prepared setup before media publication.
    /// # Errors
    /// Refuses changed sources, allocation, password or incomplete encryption.
    pub fn seal(
        &self,
        package: &Path,
        operation: &str,
        binding: &str,
        password: Zeroizing<Vec<u8>>,
        limits: Limits,
        cancel: &AtomicBool,
    ) -> io::Result<Expectation> {
        self.seal_reporting(
            package,
            operation,
            binding,
            password,
            limits,
            cancel,
            &mut |_| Ok(()),
        )
    }
    pub fn seal_reporting(
        &self,
        package: &Path,
        operation: &str,
        binding: &str,
        password: Zeroizing<Vec<u8>>,
        limits: Limits,
        cancel: &AtomicBool,
        observer: &mut crate::setup::portable_transfer::Observer<'_>,
    ) -> io::Result<Expectation> {
        self.validate()?;
        if uuid::Uuid::parse_str(operation).is_err()
            || package.file_name().and_then(|n| n.to_str())
                != Some(format!("applications-{operation}.sealed").as_str())
        {
            return Err(changed(
                "Application package requires its exact operation filename",
            ));
        }
        let export =
            ExportReview::review(&self.root, package, Purpose::FlatpakApplications, limits)?;
        let info = export.export_bound_reporting(
            Zeroizing::new(password.to_vec()),
            binding,
            cancel,
            observer,
        )?;
        let imported = ImportReview::review(
            package,
            &password,
            Purpose::FlatpakApplications,
            limits,
            cancel,
        )?;
        let expectation = Expectation {
            schema: 1,
            consumer_protocol: self.consumer_protocol,
            operation: operation.into(),
            purpose: info.purpose,
            filename: format!("applications-{operation}.sealed"),
            setup_binding: binding.into(),
            sealed_bytes: info.sealed_bytes,
            plaintext_bytes: info.plaintext_bytes,
            entries: info.entries,
            ciphertext_sha256: imported.ciphertext_digest(),
            manifest_sha256: self.manifest_sha256,
            limits,
        };
        expectation.validate()?;
        self.validate()?;
        Ok(expectation)
    }
}

fn stage_user_state(
    source: &PortableFlatpakSource,
    runner: &impl OfflineRunner,
    grant: &StageGrant<'_>,
    cancel: &AtomicBool,
) -> io::Result<()> {
    let transfer = crate::setup::user_transfer::review(
        runner,
        crate::setup::user_transfer::TransferRequest {
            source_home: source.home.clone(),
            target_home: grant.root.join("user-state"),
            private_journals: grant.journals.into(),
            copy_home: false,
            copy_flatpak: true,
            system_refs: Vec::new(),
        },
    )
    .map_err(io::Error::other)?;
    let report = transfer
        .execute(runner, cancel, false, |_| {})
        .map_err(io::Error::other)?;
    if !report.complete() {
        return Err(changed(
            "Independent app state copy is incomplete; no package is ready",
        ));
    }
    Ok(())
}

fn check_allocation(
    source: &PortableFlatpakSource,
    pin: &Arc<PinnedDirectory>,
    grant: &StageGrant<'_>,
) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    let mut required = source.staging_estimate;
    if let Some(state) = &source.selected_state {
        required = required
            .checked_add(state.bytes())
            .ok_or_else(|| changed("App stage size overflows"))?;
    }
    if grant.include_user_state && source.selected_state.is_none() {
        for name in [".local/share/flatpak", ".var/app"] {
            let path = source.home.join(name);
            match PinnedDirectory::open_absolute(&path) {
                Ok(data) => {
                    let inventory = SourceInventory::capture_home(&data)?;
                    let bytes = inventory
                        .summary()?
                        .bytes
                        .checked_add(
                            u64::try_from(inventory.entry_count())
                                .map_err(io::Error::other)?
                                .checked_mul(8192)
                                .ok_or_else(|| changed("App state size overflows"))?,
                        )
                        .ok_or_else(|| changed("App state size overflows"))?;
                    required = required
                        .checked_add(bytes)
                        .ok_or_else(|| changed("App stage size overflows"))?;
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
    }
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: held descriptor and complete writable native stat storage.
    if unsafe { libc::fstatvfs(pin.file.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful fstatvfs initialized the entire result.
    let stat = unsafe { stat.assume_init() };
    let free = stat
        .f_bavail
        .checked_mul(stat.f_frsize)
        .ok_or_else(|| changed("Stage free capacity overflows"))?;
    if required > grant.allocation_bytes || required > free {
        return Err(changed(
            "Selected apps need more private staging space; choose a larger staging filesystem",
        ));
    }
    Ok(())
}

fn publish_signing_key(
    pin: &Arc<PinnedDirectory>,
    repository: &str,
    remote: &Remote,
    definition: &[u8],
) -> io::Result<()> {
    let exported = pin.open_child(
        OsStr::new(repository),
        &crate::setup::nofollow::metadata_nofollow(&pin.file, Path::new(repository))?,
    )?;
    let dot = exported.open_child(
        OsStr::new(".ostree"),
        &crate::setup::nofollow::metadata_nofollow(&exported.file, Path::new(".ostree"))?,
    )?;
    let repo = dot.open_child(
        OsStr::new("repo"),
        &crate::setup::nofollow::metadata_nofollow(&dot.file, Path::new("repo"))?,
    )?;
    let keyname = format!("{}.trustedkeys.gpg", remote.name);
    match crate::setup::nofollow::metadata_nofollow(&repo.file, Path::new(&keyname)) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            use base64::Engine as _;
            let text = std::str::from_utf8(definition).map_err(io::Error::other)?;
            let encoded = text
                .lines()
                .find_map(|line| line.strip_prefix("GPGKey="))
                .ok_or_else(|| changed("Missing approved signing key"))?;
            let key = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(io::Error::other)?;
            write(&repo, &keyname, &key)?;
        }
        Ok(_) => {
            // Existing exported key files are verified through the held repo;
            // no source key or exported key is overwritten.
            use base64::Engine as _;
            use std::io::Read as _;
            let mut options = OpenOptions::new();
            options.read(true).follow(FollowSymlinks::No).nonblock(true);
            let file = cap_primitives::fs::open(&repo.file, Path::new(&keyname), &options)?;
            if !file.metadata()?.is_file() || file.metadata()?.len() > 1024 * 1024 {
                return Err(changed("Invalid exported signing key"));
            }
            let mut bytes = Vec::new();
            file.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
            let text = std::str::from_utf8(definition).map_err(io::Error::other)?;
            let encoded = text
                .lines()
                .find_map(|line| line.strip_prefix("GPGKey="))
                .ok_or_else(|| changed("Missing approved signing key"))?;
            let expected = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .map_err(io::Error::other)?;
            if bytes != expected {
                return Err(changed(
                    "Exported signing key differs from original approval",
                ));
            }
        }
        Err(error) => return Err(error),
    }
    Ok(())
}
