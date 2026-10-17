//! Read committed setup only through genuine selected raw/GPT boot custody.
use super::selection_facts::{self, Committed};
use super::*;
use crate::setup::media_setup::{boot_medium::BootMedium, boot_review::PendingBootReview};
use crate::setup::migration::{changed, check_cancel};
use crate::setup::prepared_setup::OriginalSelectionPlan;
use cap_primitives::fs::MetadataExt as _;
use std::ffi::OsStr;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct Selection {
    pub(super) schema: u32,
    pub(super) operation: String,
    pub(super) medium: crate::media_layout::MediaPartitionBinding,
    pub(super) declaration_sha256: [u8; 32],
    pub(super) attempt: String,
    pub(super) pending_setup: crate::imager_model::PreparedSetupExpectation,
    pub(super) completed_setup: crate::imager_model::PreparedSetupExpectation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) home: Option<crate::setup::portable_home_media::Expectation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) applications: Option<crate::setup::flatpak_transfer::portable::Expectation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) home_inode: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) applications_inode: Option<u64>,
    pub(super) completed_inode: u64,
    pub(super) marker_sha256: [u8; 32],
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Marker {
    schema: u32,
    attempt: String,
    pub(super) pending: crate::imager_model::PreparedSetupExpectation,
    setup: crate::imager_model::PreparedSetupExpectation,
    #[serde(default)]
    home: Option<crate::setup::portable_home_media::Expectation>,
    #[serde(default)]
    applications: Option<crate::setup::flatpak_transfer::portable::Expectation>,
}
/// Independent capsule may outlive the readonly bootstrap mount. Contains no
/// medium pin, source descriptor or permission to read a Home directory.
pub struct AuthenticatedCompletedSetup {
    quarantine: tempfile::TempDir,
    setup: Arc<PreparedSetupReview>,
    expected: crate::imager_model::PreparedSetupExpectation,
}
impl AuthenticatedCompletedSetup {
    pub fn authenticated_setup(&self) -> Arc<PreparedSetupReview> {
        self.setup.clone()
    }
    pub fn expectation(&self) -> &crate::imager_model::PreparedSetupExpectation {
        &self.expected
    }
    pub fn validate(&self) -> io::Result<()> {
        self.setup.validate()
    }
    pub fn package_path(&self) -> PathBuf {
        self.quarantine.path().join(crate::setup::media_setup::FILE)
    }
}
/// Borrowed selected-medium scope. Constructors cannot accept a JSON receipt
/// or a caller path as custody; all source lookups occur within BootMedium.
pub struct CompletedBootReview<'a> {
    pub(super) medium: &'a BootMedium,
    pub(super) pending: PendingBootReview<'a>,
    pub(super) selected: Selection,
    pub(super) capsule: Arc<AuthenticatedCompletedSetup>,
}
impl<'a> CompletedBootReview<'a> {
    /// A missing derived pointer first checks canonical committed custody.
    /// Absence requires authenticated pending custody and readonly Store proof
    /// that export has not started; corrupt or started state never becomes absence.
    pub fn review_if_committed(
        medium: &'a BootMedium,
        password: &[u8],
        cancel: &AtomicBool,
    ) -> io::Result<Option<Self>> {
        let pending = PendingBootReview::review(medium, password, cancel)?;
        let exists = medium.with_root(|root, _binding| {
            pending.validate()?;
            let private = child(root, crate::setup::media_setup::DIRECTORY)?;
            match crate::setup::nofollow::metadata_nofollow(
                &private.file,
                Path::new("completed-selection.json"),
            ) {
                Ok(_) => Ok(true),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    let declaration = pending
                        .authenticated_setup()
                        .pending_original_declaration()?;
                    let committed = selection_facts::lookup(&private, &declaration)?;
                    if let Some(facts) = committed {
                        // A durable commit may precede its derived pointer.
                        // Release readonly Store/source pins inside this scope;
                        // Self::review below authenticates reconstructed source
                        // selection, both seals and exact password lineage.
                        drop(facts);
                        pending.validate()?;
                        Ok(true)
                    } else {
                        selection_facts::uncommitted(&private, &declaration)?;
                        pending.validate()?;
                        Ok(false)
                    }
                }
                Err(error) => Err(error),
            }
        })?;
        pending.validate()?;
        drop(pending);
        if exists {
            Self::review(medium, password, cancel).map(Some)
        } else {
            check_cancel(cancel)?;
            Ok(None)
        }
    }
    pub fn review(
        medium: &'a BootMedium,
        password: &[u8],
        cancel: &AtomicBool,
    ) -> io::Result<Self> {
        let pending = PendingBootReview::review(medium, password, cancel)?;
        let quarantine = tempfile::Builder::new()
            .prefix("tilo-completed-boot-")
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()?;
        let destination = quarantine.path().join(crate::setup::media_setup::FILE);
        let selected = medium.with_root(|root, binding| {
            let (selection, _, completed, _lease) = review_source(root, binding, &pending, cancel)?;
            copy_ciphertext(&completed, &destination, &selection.completed_setup, cancel)?;
            Ok(selection)
        })?;
        let setup = Arc::new(PreparedSetupReview::open(
            &destination,
            password,
            &quarantine.path().join("setup"),
            cancel,
        )?);
        setup.require_completed_export()?;
        let original = pending.authenticated_setup();
        let mut manifest = original.manifest().clone();
        manifest.offline_home = None;
        manifest.offline_applications = None;
        manifest.completed_applications_only = selected.schema == 2;
        manifest.completed_home.clone_from(&selected.home);
        manifest.portable_flatpak.clone_from(&selected.applications);
        if setup.manifest() != &manifest {
            return Err(changed("Completed setup changed prepared choices"));
        }
        for name in [IDENTITY_FILE, ACCOUNTS_FILE, SERVICE_SESSIONS_FILE] {
            let included = name == IDENTITY_FILE
                || name == ACCOUNTS_FILE && !manifest.account_ids.is_empty()
                || name == SERVICE_SESSIONS_FILE && manifest.service_sessions.is_some();
            if included {
                let old =
                    Zeroizing::new(bounded_file(&original.pin, name, LIMITS.plaintext_bytes)?);
                let new = Zeroizing::new(bounded_file(&setup.pin, name, LIMITS.plaintext_bytes)?);
                if old.as_slice() != new.as_slice() {
                    return Err(changed("Completed setup changed sealed belongings"));
                }
            }
        }
        let review = Self {
            medium,
            pending,
            capsule: Arc::new(AuthenticatedCompletedSetup {
                quarantine,
                setup,
                expected: selected.completed_setup.clone(),
            }),
            selected,
        };
        review.validate(cancel)?;
        Ok(review)
    }
    pub fn retained_setup(
        &self,
        cancel: &AtomicBool,
    ) -> io::Result<Arc<AuthenticatedCompletedSetup>> {
        self.validate(cancel)?;
        let capsule = self.capsule.clone();
        self.validate(cancel)?;
        Ok(capsule)
    }
    pub fn validate(&self, cancel: &AtomicBool) -> io::Result<()> {
        self.pending.validate()?;
        self.capsule.validate()?;
        self.medium.with_root(|root, binding| {
            let (selection, _, _, _lease) = review_source(root, binding, &self.pending, cancel)?;
            if selection != self.selected {
                return Err(changed("Completed boot selection changed"));
            }
            Ok(())
        })
    }
    pub fn adopt_install_generation(&self, cancel: &AtomicBool) -> io::Result<()> {
        super::handoff::adopt(self, cancel)
    }
    pub fn with_home<T>(
        &self,
        cancel: &AtomicBool,
        callback: impl FnOnce(
            &Arc<PinnedDirectory>,
            &crate::setup::portable_home_media::Expectation,
        ) -> io::Result<T>,
    ) -> io::Result<T> {
        self.validate(cancel)?;
        let result = self.medium.with_root(|root, binding| {
            let (selection, home, _, _lease) = review_source(root, binding, &self.pending, cancel)?;
            if selection != self.selected {
                return Err(changed("Completed Home selection changed"));
            }
            callback(&home, selection.home.as_ref().ok_or_else(invalid)?)
        });
        self.validate(cancel)?;
        result
    }
    /// Copies only the exact completed Apps ciphertext authenticated by this
    /// real selected-medium review. Destination callers receive no source FD.
    pub fn stream_qualification_applications(
        &self,
        expected: &crate::setup::flatpak_transfer::portable::Expectation,
        output: &mut impl std::io::Write,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        if !self
            .capsule
            .authenticated_setup()
            .manifest()
            .completed_applications_only
        {
            return Err(invalid());
        }
        self.with_payloads(cancel, |directory, selection| {
            if selection.home.is_some() || selection.applications.as_ref() != Some(expected) {
                return Err(invalid());
            }
            let payload = super::handoff::Payload::Applications(expected.clone());
            let file = super::handoff::open_payload(&directory.file, &payload)?;
            let member = super::handoff::Member {
                payload,
                snapshot: super::handoff::snapshot(&file)?,
            };
            super::stream::stream(&directory.file, &member, output, cancel)
        })
    }
    pub(super) fn with_payloads<T>(
        &self,
        cancel: &AtomicBool,
        callback: impl FnOnce(&Arc<PinnedDirectory>, &Selection) -> io::Result<T>,
    ) -> io::Result<T> {
        if self.selected.home.is_some() {
            return self.with_home(cancel, |directory, _| callback(directory, &self.selected));
        }
        self.validate(cancel)?;
        let result = self.medium.with_root(|root, binding| {
            let (selection, payload, _, _lease) =
                review_source(root, binding, &self.pending, cancel)?;
            if selection != self.selected {
                return Err(invalid());
            }
            callback(&payload, &selection)
        });
        self.validate(cancel)?;
        result
    }
}
pub(super) fn child(parent: &Arc<PinnedDirectory>, name: &str) -> io::Result<Arc<PinnedDirectory>> {
    let metadata = crate::setup::nofollow::metadata_nofollow(&parent.file, Path::new(name))?;
    let child = parent.open_child(OsStr::new(name), &metadata)?;
    child.validate_destination()?;
    let metadata = child.file.metadata()?;
    if metadata.uid() != 0
        || metadata.gid() != 0
        || metadata.mode() & 0o7777 != 0o700
        || metadata.dev() != parent.file.metadata()?.dev()
    {
        return Err(invalid());
    }
    Ok(child)
}
fn read(parent: &Arc<PinnedDirectory>, name: &str, bound: u64) -> io::Result<Vec<u8>> {
    bounded_file(parent, name, bound)
}
fn review_source(
    root: &Arc<PinnedDirectory>,
    binding: &crate::media_layout::MediaPartitionBinding,
    pending: &PendingBootReview<'_>,
    cancel: &AtomicBool,
) -> io::Result<(
    Selection,
    Arc<PinnedDirectory>,
    Arc<PinnedDirectory>,
    Committed,
)> {
    pending.validate()?;
    let declaration = pending
        .authenticated_setup()
        .pending_original_declaration()?;
    review_declared_source(
        root,
        binding,
        &declaration,
        pending.pending_expectation(),
        cancel,
    )
}
pub(super) fn review_declared_source(
    root: &Arc<PinnedDirectory>,
    binding: &crate::media_layout::MediaPartitionBinding,
    declaration: &OriginalSelectionPlan,
    pending: &crate::imager_model::PreparedSetupExpectation,
    cancel: &AtomicBool,
) -> io::Result<(
    Selection,
    Arc<PinnedDirectory>,
    Arc<PinnedDirectory>,
    Committed,
)> {
    check_cancel(cancel)?;
    let private = child(root, crate::setup::media_setup::DIRECTORY)?;
    let mut recovered = None;
    let selected: Selection = match read(&private, "completed-selection.json", 16 * 1024) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|_| invalid())?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let facts = selection_facts::lookup(&private, declaration)?.ok_or_else(invalid)?;
            let selection = facts.selected()?;
            recovered = Some(facts);
            selection
        }
        Err(error) => return Err(error),
    };
    let digest = declaration.canonical_digest()?;
    selected.validate_shape()?;
    if selected.schema
        != if declaration.is_applications_only() {
            2
        } else {
            1
        }
        || selected.medium != *binding
        || selected.operation != declaration.source().operation_id
        || selected.declaration_sha256 != digest
        || selected.pending_setup != *pending
        || !selected.completed_setup.valid()
        || selected.payload_inode()? == 0
        || selected.completed_inode == 0
        || !uuid::Uuid::parse_str(&selected.attempt)
            .is_ok_and(|id| !id.is_nil() && id.to_string() == selected.attempt)
        || selected
            .home
            .as_ref()
            .is_some_and(|home| home.setup_binding != declaration.setup_binding())
        || selected
            .applications
            .as_ref()
            .is_some_and(|apps| apps.setup_binding != declaration.setup_binding())
    {
        return Err(invalid());
    }

    let output = child(&private, "original-output")?;
    let home = child(&output, &format!("stage-{}", selected.attempt))?;
    let completed = child(&output, &format!("completed-setup-{}", selected.attempt))?;
    let facts = match recovered {
        Some(facts) => facts,
        None => {
            selection_facts::review(&private, declaration, &selected.attempt, &home, &completed)?
        }
    };
    if home.file.metadata()?.ino() != selected.payload_inode()?
        || completed.file.metadata()?.ino() != selected.completed_inode
        || facts.selected()? != selected
    {
        return Err(invalid());
    }
    let bytes = read(&completed, "completion.json", 16 * 1024)?;
    let digest: [u8; 32] = Sha256::digest(&bytes).into();
    let marker: Marker = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
    if digest != selected.marker_sha256
        || marker.schema != selected.schema
        || marker.attempt != selected.attempt
        || marker.pending != selected.pending_setup
        || marker.setup != selected.completed_setup
        || marker.home != selected.home
        || marker.applications != selected.applications
    {
        return Err(invalid());
    }
    let private_setup = child(&completed, crate::setup::media_setup::DIRECTORY)?;
    let receipt: crate::imager_model::PreparedSetupExpectation =
        serde_json::from_slice(&read(&private_setup, "receipt.json", 1024)?)
            .map_err(|_| invalid())?;
    if receipt != selected.completed_setup {
        return Err(invalid());
    }
    private.validate_destination()?;
    output.validate_destination()?;
    home.validate_destination()?;
    completed.validate_destination()?;
    check_cancel(cancel)?;
    Ok((selected, home, completed, facts))
}
pub(super) fn copy_ciphertext(
    parent: &Arc<PinnedDirectory>,
    destination: &Path,
    expected: &crate::imager_model::PreparedSetupExpectation,
    cancel: &AtomicBool,
) -> io::Result<()> {
    let source = child(parent, crate::setup::media_setup::DIRECTORY)?;
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No).nonblock(true);
    let mut file = cap_primitives::fs::open(
        &source.file,
        Path::new(crate::setup::media_setup::FILE),
        &options,
    )?;
    let before = cap_primitives::fs::Metadata::from_file(&file)?;
    if !before.is_file()
        || before.uid() != 0
        || before.gid() != 0
        || before.mode() & 0o077 != 0
        || before.nlink() != 1
        || before.len() != expected.sealed_bytes
        || before.len() > LIMITS.sealed_bound().ok_or_else(invalid)?
    {
        return Err(invalid());
    }
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(destination)?;
    let mut hash = Sha256::new();
    let mut total = 0u64;
    let mut bytes = [0u8; 65536];
    loop {
        check_cancel(cancel)?;
        let count = file.read(&mut bytes)?;
        if count == 0 {
            break;
        }
        total = total
            .checked_add(count as u64)
            .filter(|n| *n <= expected.sealed_bytes)
            .ok_or_else(invalid)?;
        hash.update(&bytes[..count]);
        output.write_all(&bytes[..count])?;
    }
    if total != expected.sealed_bytes
        || hex::encode(hash.finalize()) != expected.ciphertext_sha256
        || !crate::setup::migration::same_snapshot(
            &before,
            &cap_primitives::fs::Metadata::from_file(&file)?,
        )
        || !crate::setup::migration::same_snapshot(
            &before,
            &crate::setup::nofollow::metadata_nofollow(
                &source.file,
                Path::new(crate::setup::media_setup::FILE),
            )?,
        )
    {
        return Err(invalid());
    }
    output.sync_all()?;
    source.validate_destination()?;
    check_cancel(cancel)
}
