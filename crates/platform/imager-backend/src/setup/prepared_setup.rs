//! Authenticated prepared setup custody. Import is explicit and runs as the
//! target user; this never activates an identity or writes another user's vault.
use super::{
    account_transfer::{Connector, PasswordSealedAccounts},
    destination_security,
    migration::PinnedDirectory,
    migration_durability,
    portable_transfer::{ImportReview, Limits, Purpose},
};
use accounts::AccountId;
use cap_fs_ext::{OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_primitives::fs::{FollowSymlinks, OpenOptions, OpenOptionsExt};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, atomic::AtomicBool},
};
use zeroize::Zeroizing;
#[cfg(target_os = "linux")]
mod capacity;
#[cfg(target_os = "linux")]
pub use capacity::{
    completed_setup_ciphertext_reservation, completed_setup_free_bytes,
    completed_setup_write_reservation,
};
#[cfg(target_os = "linux")]
pub mod completion;
#[cfg(target_os = "linux")]
mod original_selection;
pub use crate::prepared_setup::PublicInstallProfile;
#[cfg(target_os = "linux")]
pub use original_selection::OfflineApplicationsPlan;
#[cfg(target_os = "linux")]
pub use original_selection::OriginalSelectionPlan;

pub const MANIFEST_FILE: &str = "manifest.json";
#[cfg(target_os = "linux")]
mod computer_applications;
#[cfg(target_os = "linux")]
mod installed_original_copy;
#[cfg(target_os = "linux")]
mod installed_person;
#[cfg(target_os = "linux")]
mod installed_retained;
#[cfg(target_os = "linux")]
mod original_copy;
#[cfg(target_os = "linux")]
pub use computer_applications::{ComputerApplicationsScope, SharedHomeApplicationsDeclaration};
#[cfg(target_os = "linux")]
pub use installed_original_copy::AuthenticatedInstalledOriginalCopy;
#[cfg(target_os = "linux")]
pub use installed_person::{
    InstalledPersonAnchor, publish_original_copy_owner, publish_retained_primary_owner,
};
#[cfg(target_os = "linux")]
pub use installed_retained::AuthenticatedInstalledRetained;
#[cfg(target_os = "linux")]
pub use original_copy::OriginalCopyDeclaration;
mod planned_btrfs_resize;
pub use planned_btrfs_resize::PlannedBtrfsResize;

pub const IDENTITY_FILE: &str = "identity.sealed";
pub const ACCOUNTS_FILE: &str = "accounts.sealed";
pub const SERVICE_SESSIONS_FILE: &str = "service-sessions.sealed";
/// Authenticated retained-files metadata, never authority to mount an
/// arbitrary path. Installation must re-attest the original volume and home.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct RetainedHomePlan(super::migration_handoff::Plan);
impl RetainedHomePlan {
    pub fn as_plan(&self) -> &super::migration_handoff::Plan {
        &self.0
    }
    pub fn new(plan: super::migration_handoff::Plan) -> io::Result<Self> {
        let value = Self(plan);
        value.validate()?;
        Ok(value)
    }
    fn validate(&self) -> io::Result<()> {
        self.0.bytes()?;
        let shared_home = self.0.categories.home
            && self.0.home_policy == super::migration_handoff::HomePolicy::AccessReadWrite;
        let applications_only = !self.0.categories.home
            && self.0.categories.flatpak
            && self.0.home_policy == super::migration_handoff::HomePolicy::Copy;
        if !(shared_home || applications_only) {
            return Err(invalid());
        }
        if self.0.categories.flatpak {
            let inventory = super::flatpak_transfer::HandoffInventory::parse(&serde_json::to_vec(
                self.0.flatpak_inventory.as_ref().ok_or_else(invalid)?,
            )?)
            .map_err(io::Error::other)?;
            inventory
                .validate_operation(&self.0.operation_id)
                .map_err(io::Error::other)?;
        } else if self.0.flatpak_inventory.is_some() {
            return Err(invalid());
        }
        Ok(())
    }
}
pub const LIMITS: Limits = Limits {
    plaintext_bytes: 32 * 1024 * 1024,
    entries: 2048,
    metadata_bytes: 2 * 1024 * 1024,
};
// The retained Plan has its own 64 KiB bound; existing identity/account/profile
// metadata keeps its original 32 KiB allowance alongside it. A direct-only
// resize adds a bounded 16 KiB reviewed plan and its closed binding envelope.
// Optional retained caller declarations are separately bounded to 12 KiB; the
// existing total 128 KiB manifest cap still applies without truncation.
const MANIFEST_LIMIT: u64 = 128 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema: u32,
    pub setup_binding: String,
    pub identity_public_key: [u8; 32],
    #[serde(default, skip_serializing_if = "IdentityKind::is_single")]
    pub identity_kind: IdentityKind,
    pub account_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_custody_protocol: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub portable_home: Option<PortableHomePlan>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offline_home: Option<OfflineHomePlan>,
    #[cfg(target_os = "linux")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offline_applications: Option<OfflineApplicationsPlan>,
    /// Exact verified Home ciphertext bound inside the completed setup package.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_home: Option<super::portable_home_media::Expectation>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub completed_applications_only: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_profile: Option<PublicInstallProfile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retained_home: Option<RetainedHomePlan>,
    #[cfg(target_os = "linux")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_copy: Option<OriginalCopyDeclaration>,
    #[cfg(target_os = "linux")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub computer_applications: Option<SharedHomeApplicationsDeclaration>,
    /// Authenticated requested source credentials; only the protected source
    /// record and root-held RW lease can turn these bytes into scoped authority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retained_caller: Option<super::home_capture::RetainedCallerDeclaration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planned_btrfs_resize: Option<PlannedBtrfsResize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub portable_flatpak: Option<super::flatpak_transfer::portable::Expectation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser: Option<BrowserHandoff>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_sessions: Option<super::service_sessions::Declaration>,
}
/// Selected browser data authenticated with the same identity and setup as installation.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BrowserHandoff {
    pub plan: browser_continuity::Plan,
    pub payload: super::browser_transfer::Expectation,
}
pub use crate::prepared_media::{IdentityKind, PortableHomePlan, PortableHomeSelection};
/// Password-authenticated pending request, not source-group authority,
/// a mount grant or a completed Home receipt. A future producer must bind
/// actual source and target grants before sealing; delivery reattests media.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OfflineHomePlan {
    schema: u32,
    source: super::migration_handoff::Plan,
    export_operation: String,
    setup_binding: String,
    identity_public_key: [u8; 32],
    media: crate::media_layout::MediaPartitionBinding,
    caller: super::home_capture::OriginalCallerFacts,
    sealed_reservation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_witness: Option<super::home_capture::SourceWitness>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mounted_witness: Option<super::home_capture::MountedWitness>,
}
impl OfflineHomePlan {
    pub fn new(
        source: super::migration_handoff::Plan,
        setup_binding: String,
        identity_public_key: [u8; 32],
        media: crate::media_layout::MediaPartitionBinding,
        caller: super::home_capture::OriginalCallerFacts,
        sealed_reservation: u64,
    ) -> io::Result<Self> {
        let value = Self {
            schema: 1,
            export_operation: source.operation_id.clone(),
            source,
            setup_binding,
            identity_public_key,
            media,
            caller,
            sealed_reservation,
            source_witness: None,
            mounted_witness: None,
        };
        value.validate()?;
        Ok(value)
    }
    pub fn with_source_witness(
        mut self,
        witness: super::home_capture::SourceWitness,
    ) -> io::Result<Self> {
        witness.validate_plan(&self.source)?;
        self.source_witness = Some(witness);
        self.validate()?;
        Ok(self)
    }
    pub fn source_witness(&self) -> Option<&super::home_capture::SourceWitness> {
        self.source_witness.as_ref()
    }
    pub fn with_mounted_witness(
        mut self,
        witness: super::home_capture::MountedWitness,
    ) -> io::Result<Self> {
        witness.matches_source(self.source_witness.as_ref().ok_or_else(invalid)?)?;
        self.mounted_witness = Some(witness);
        self.validate()?;
        Ok(self)
    }
    pub fn mounted_witness(&self) -> Option<&super::home_capture::MountedWitness> {
        self.mounted_witness.as_ref()
    }
    fn validate(&self) -> io::Result<()> {
        self.source.validate()?;
        self.caller.validate(self.source.home.original_uid())?;
        if let Some(witness) = &self.source_witness {
            witness.validate_plan(&self.source)?;
        }
        if let Some(witness) = &self.mounted_witness {
            witness.matches_source(self.source_witness.as_ref().ok_or_else(invalid)?)?;
        }
        let uuid = |value: &str| {
            uuid::Uuid::parse_str(value).is_ok_and(|id| !id.is_nil() && id.to_string() == value)
        };
        if self.schema != 1
            || self.caller.uid() != self.source.home.original_uid()
            || self.source.home_policy != super::migration_handoff::HomePolicy::Copy
            || !self.source.categories.home
            || (self.source.categories.flatpak && self.source.flatpak_inventory.is_none())
            || self.source.operation_id != self.export_operation
            || !uuid(&self.export_operation)
            || self.setup_binding.is_empty()
            || self.setup_binding.len() > 256
            || self.setup_binding.chars().any(char::is_control)
            || self.media.schema != 1
            || self.media.target_stable_id.is_empty()
            || self.media.target_stable_id.len() > 256
            || self.media.target_stable_id.chars().any(char::is_control)
            || !uuid(&self.media.disk_guid)
            || !uuid(&self.media.partition_guid)
            || self.media.number == 0
            || self.sealed_reservation == 0
        {
            return Err(invalid());
        }
        Ok(())
    }
    pub fn requested_caller(&self) -> &super::home_capture::OriginalCallerFacts {
        &self.caller
    }
    pub fn source(&self) -> &super::migration_handoff::Plan {
        &self.source
    }
    pub fn media(&self) -> &crate::media_layout::MediaPartitionBinding {
        &self.media
    }
    /// Reconstruct the producer's complete geometry from the authenticated
    /// declaration and, for selected installations, its genuine source custody.
    /// Protected marker JSON alone never supplies an installation inventory.
    #[cfg(target_os = "linux")]
    pub fn authenticated_source_plans(&self) -> io::Result<Vec<Vec<u8>>> {
        let geometry = crate::setup::source_geometry::original_source_plans;
        if !self.source.categories.flatpak && self.mounted_witness.is_none() {
            return geometry(&self.source);
        }
        if rustix::process::geteuid().as_raw() != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Original installation geometry requires root custody",
            ));
        }
        let witness = self.source_witness().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Original installation geometry lacks source witness",
            )
        })?;
        let source = super::migration_handoff::mount_read_only(&self.source)?;
        // Retain the actual owned resolver while record authentication runs:
        // a construction error must not fall through an unobserved mount Drop.
        let original =
            match super::home_capture::SourceReadContext::review_witness(source.clone(), witness) {
                Ok(original) => {
                    drop(source);
                    original
                }
                Err(error) => {
                    source.retire()?;
                    return Err(error);
                }
            };
        let result = (|| {
            original.verify_requested(&self.source, witness, &self.caller)?;
            let mut plans = if self.source.categories.flatpak {
                let inventory =
                    super::flatpak_transfer::offline_original::OriginalInventoryGrant::review(
                        &original,
                    )?
                    .ok_or_else(|| {
                        io::Error::new(
                            io::ErrorKind::PermissionDenied,
                            "Selected original installation geometry lacks protected inventory",
                        )
                    })?;
                inventory.matches_plan(&self.source)?;
                let mut expanded = self.source.clone();
                expanded.flatpak_inventory = Some(serde_json::to_value(inventory.inventory())?);
                let plans = geometry(&expanded)?;
                inventory.matches_plan(&self.source)?;
                plans
            } else {
                geometry(&self.source)?
            };
            if let Some(mounted) = self.mounted_witness() {
                // This API is a bounded metadata-only review with no job token.
                // The export path uses its live token and retained epoch instead.
                let cancel = std::sync::atomic::AtomicBool::new(false);
                original.append_mounted_source_geometry(mounted, &mut plans, &cancel)?;
            }
            original.verify_requested(&self.source, witness, &self.caller)?;
            Ok(plans)
        })();
        // All borrowed inventory descriptors are gone before mount retirement.
        // Both successful and failed geometry reviews confirm owned cleanup.
        original.retire()?;
        result
    }
    pub fn sealed_reservation(&self) -> u64 {
        self.sealed_reservation
    }
}
impl Manifest {
    pub fn new(
        setup_binding: String,
        identity_public_key: [u8; 32],
        account_ids: Vec<String>,
    ) -> io::Result<Self> {
        let manifest = Self {
            schema: 1,
            setup_binding,
            identity_public_key,
            identity_kind: IdentityKind::SingleDeviceBackup,
            account_ids,
            account_custody_protocol: None,
            portable_home: None,
            offline_home: None,
            #[cfg(target_os = "linux")]
            offline_applications: None,
            completed_home: None,
            completed_applications_only: false,
            install_profile: None,
            retained_home: None,
            #[cfg(target_os = "linux")]
            original_copy: None,
            #[cfg(target_os = "linux")]
            computer_applications: None,
            retained_caller: None,
            planned_btrfs_resize: None,
            portable_flatpak: None,
            browser: None,
            service_sessions: None,
        };
        manifest.validate()?;
        Ok(manifest)
    }
    fn validate(&self) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        if let Some(composite) = &self.computer_applications {
            composite.validate(self)?;
        }
        #[cfg(target_os = "linux")]
        if let Some(copy) = &self.original_copy {
            copy.bytes()?;
            if copy.setup_binding() != self.setup_binding
                || copy.identity_public_key() != self.identity_public_key
                || matches!(self.identity_kind, IdentityKind::CurrentDevice { .. })
                || self.retained_home.is_some()
                || self.retained_caller.is_some()
                || self.planned_btrfs_resize.is_some()
                || self.portable_home.is_some()
                || self.offline_home.is_some()
                || self.offline_applications.is_some()
                || self.completed_home.is_some()
                || self.completed_applications_only
                || self.portable_flatpak.is_some()
            {
                return Err(invalid());
            }
        }
        if matches!(self.identity_kind, IdentityKind::CurrentDevice { .. })
            && (self.portable_home.is_some()
                || self.offline_home.is_some()
                || self.completed_home.is_some()
                || self.retained_home.is_some()
                || self.portable_flatpak.is_some()
                || self.planned_btrfs_resize.is_some())
        {
            return Err(invalid());
        }
        if let Some(caller) = &self.retained_caller {
            caller.validate_binding(
                self.retained_home.as_ref().ok_or_else(invalid)?.as_plan(),
                &self.setup_binding,
                &self.identity_public_key,
            )?;
        }
        if let Some(resize) = &self.planned_btrfs_resize {
            resize.validate(self.retained_home.as_ref().ok_or_else(invalid)?)?;
            if self.install_profile.is_none() {
                return Err(invalid());
            }
        }
        if self.completed_applications_only
            && (self.offline_home.is_some()
                || self.offline_applications.is_some()
                || self.portable_home.is_some()
                || self.completed_home.is_some()
                || self.retained_home.is_some()
                || self.portable_flatpak.is_none())
        {
            return Err(invalid());
        }
        if let Some(home) = &self.completed_home {
            home.validate()?;
            if self.offline_home.is_some()
                || self.portable_home.is_none()
                || self.retained_home.is_some()
                || home.setup_binding != self.setup_binding
                || (self.portable_flatpak.is_some()
                    && !self
                        .portable_home
                        .as_ref()
                        .is_some_and(|plan| plan.applications))
            {
                return Err(invalid());
            }
        }
        if let Some(offline) = &self.offline_home {
            offline.validate()?;
            let home = self.portable_home.as_ref().ok_or_else(invalid)?;
            if offline.setup_binding != self.setup_binding
                || offline.identity_public_key != self.identity_public_key
                || offline.source.categories.flatpak != home.applications
                || (!offline.source.categories.accounts
                    && (!self.account_ids.is_empty() || self.service_sessions.is_some()))
                || self.retained_home.is_some()
            {
                return Err(invalid());
            }
        }
        #[cfg(target_os = "linux")]
        if let Some(applications) = &self.offline_applications {
            applications.validate()?;
            if self.offline_home.is_some()
                || self.portable_home.is_some()
                || self.completed_home.is_some()
                || self.portable_flatpak.is_some()
                || self.retained_home.is_some()
                || self.retained_caller.is_some()
                || applications.setup_binding != self.setup_binding
                || applications.identity_public_key != self.identity_public_key
                || (!applications.source().categories.accounts
                    && (!self.account_ids.is_empty() || self.service_sessions.is_some()))
            {
                return Err(invalid());
            }
        }
        if let Some(sessions) = &self.service_sessions {
            sessions.validate()?;
            if sessions
                .sessions
                .iter()
                .any(|session| self.account_ids.contains(&session.id))
            {
                return Err(invalid());
            }
        }
        if let Some(browser) = &self.browser {
            browser.payload.validate_plan(&browser.plan)?;
            if browser.plan.setup_binding != self.setup_binding
                || browser.plan.identity_public_key != self.identity_public_key
            {
                return Err(invalid());
            }
        }
        if let Some(applications) = &self.portable_flatpak {
            applications.validate()?;
            if applications.setup_binding != self.setup_binding || self.retained_home.is_some() {
                return Err(invalid());
            }
        }
        if let Some(profile) = &self.install_profile {
            profile.validate()?;
        }
        if let Some(plan) = &self.retained_home {
            plan.validate()?;
            if self.portable_home.is_some() {
                return Err(invalid());
            }
        }
        crate::prepared_media::validate_manifest_binding(
            self.schema,
            &self.setup_binding,
            &self.account_ids,
            self.account_custody_protocol,
        )?;
        if self
            .portable_home
            .as_ref()
            .is_some_and(|plan| plan.schema != 1)
        {
            return Err(invalid());
        }
        Ok(())
    }
    /// Create a fresh private manifest in an explicitly granted staging root.
    pub fn write(&self, directory: &Path) -> io::Result<()> {
        self.validate()?;
        let pin = PinnedDirectory::open_absolute(directory)?;
        pin.validate_destination()?;
        destination_security::check_private_file(&pin.file)?;
        let bytes = serde_json::to_vec(self).map_err(|_| invalid())?;
        if bytes.len() > MANIFEST_LIMIT as usize {
            return Err(invalid());
        }
        let mut options = OpenOptions::new();
        options
            .write(true)
            .create_new(true)
            .mode(0o600)
            .follow(FollowSymlinks::No);
        let mut file = cap_primitives::fs::open(&pin.file, Path::new(MANIFEST_FILE), &options)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        pin.validate()?;
        migration_durability::sync_directory(&pin.file)
    }
    fn ids(&self) -> Vec<AccountId> {
        self.account_ids.iter().map(AccountId::new).collect()
    }
}
fn now_ns() -> io::Result<u64> {
    let elapsed = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map_err(|_| invalid())?;
    u64::try_from(elapsed.as_nanos()).map_err(|_| invalid())
}

fn invalid() -> io::Error {
    io::Error::other("Prepared setup custody is invalid or changed")
}
fn bounded_file(pin: &Arc<PinnedDirectory>, name: &str, limit: u64) -> io::Result<Vec<u8>> {
    pin.validate()?;
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No).nonblock(true);
    let file = cap_primitives::fs::open(&pin.file, Path::new(name), &options)?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.len() > limit {
        return Err(invalid());
    }
    destination_security::check_private_file(&file)?;
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(invalid());
    }
    pin.validate()?;
    Ok(bytes)
}
fn members(pin: &Arc<PinnedDirectory>, manifest: &Manifest) -> io::Result<()> {
    let directory = cap_std::fs::Dir::from_std_file(pin.file.try_clone()?);
    let mut expected: BTreeSet<_> = [MANIFEST_FILE.into(), IDENTITY_FILE.into()]
        .into_iter()
        .collect();
    if !manifest.account_ids.is_empty() {
        expected.insert(std::ffi::OsString::from(ACCOUNTS_FILE));
    }
    if manifest.service_sessions.is_some() {
        expected.insert(std::ffi::OsString::from(SERVICE_SESSIONS_FILE));
    }
    let mut found = BTreeSet::new();
    for entry in directory.entries()? {
        found.insert(entry?.file_name());
        if found.len() > 4 {
            return Err(invalid());
        }
    }
    if found != expected {
        return Err(invalid());
    }
    pin.validate()
}

pub struct PreparedSetupReview {
    root: PathBuf,
    pin: Arc<PinnedDirectory>,
    manifest: Manifest,
    identity_hash: [u8; 32],
    account_artifact: Option<PasswordSealedAccounts>,
    session_artifact: Option<PasswordSealedAccounts>,
}
impl PreparedSetupReview {
    /// Authenticate the whole outer package, extract exclusively into a fresh
    /// quarantine, then verify exact root members and both inner artifacts.
    pub fn open(
        package: &Path,
        password: &[u8],
        quarantine: &Path,
        cancel: &AtomicBool,
    ) -> io::Result<Self> {
        let tree = ImportReview::review(package, password, Purpose::PreparedSetup, LIMITS, cancel)?
            .extract(Zeroizing::new(password.to_vec()), quarantine, cancel)?;
        super::migration::check_cancel(cancel)?;
        let pin = PinnedDirectory::open_absolute(&tree.root)?;
        destination_security::check_private_file(&pin.file)?;
        let manifest: Manifest =
            serde_json::from_slice(&bounded_file(&pin, MANIFEST_FILE, MANIFEST_LIMIT)?)
                .map_err(|_| invalid())?;
        manifest.validate()?;
        members(&pin, &manifest)?;
        let identity = bounded_file(&pin, IDENTITY_FILE, LIMITS.plaintext_bytes)?;
        let identity_hash = Sha256::digest(&identity).into();
        match manifest.identity_kind {
            IdentityKind::SingleDeviceBackup => {
                if runtime_identity::verify_portable_backup(
                    tree.root.join(IDENTITY_FILE),
                    password,
                )?
                .group_public_key
                    != manifest.identity_public_key
                {
                    return Err(invalid());
                }
            }
            IdentityKind::CurrentDevice { device_public_key } => {
                let current = tilo_identity_host::enrolled_transport::AuthenticatedCurrentDeviceReference::open(&tree.root.join(IDENTITY_FILE), password, &manifest.setup_binding, manifest.identity_public_key, device_public_key, now_ns()?)?;
                if manifest
                    .install_profile
                    .as_ref()
                    .is_none_or(|profile| profile.device_label != current.device_label())
                {
                    return Err(invalid());
                }
            }
            IdentityKind::EnrolledDevice { device_public_key } => {
                let enrolled =
                    tilo_identity_host::enrolled_transport::AuthenticatedEnrolledDevice::open(
                        &tree.root.join(IDENTITY_FILE),
                        password,
                        &manifest.setup_binding,
                        manifest.identity_public_key,
                        device_public_key,
                        now_ns()?,
                    )?;
                if manifest
                    .install_profile
                    .as_ref()
                    .is_some_and(|profile| profile.device_label != enrolled.device_label())
                {
                    return Err(invalid());
                }
            }
        }
        let account_artifact = if manifest.account_ids.is_empty() {
            None
        } else {
            let artifact = PasswordSealedAccounts::from_bytes(&bounded_file(
                &pin,
                ACCOUNTS_FILE,
                LIMITS.plaintext_bytes,
            )?)
            .map_err(|_| invalid())?;
            let protected = artifact
                .requires_protected_consumer(
                    password,
                    manifest.setup_binding.as_bytes(),
                    &manifest.ids(),
                )
                .map_err(|_| invalid())?;
            if protected != manifest.account_custody_protocol.is_some() {
                return Err(invalid());
            }
            Some(artifact)
        };
        let session_artifact = if let Some(declaration) = &manifest.service_sessions {
            let artifact = PasswordSealedAccounts::from_bytes(&bounded_file(
                &pin,
                SERVICE_SESSIONS_FILE,
                LIMITS.plaintext_bytes,
            )?)
            .map_err(|_| invalid())?;
            super::service_sessions::validate_artifact(
                &artifact,
                declaration,
                password,
                manifest.identity_public_key,
                &manifest.setup_binding,
            )?;
            Some(artifact)
        } else {
            None
        };
        super::migration::check_cancel(cancel)?;
        pin.validate()?;
        Ok(Self {
            root: tree.root,
            pin,
            manifest,
            identity_hash,
            account_artifact,
            session_artifact,
        })
    }
    /// Authenticate retained pending metadata for source read admission only.
    /// This deliberately does not admit identity/account adoption or completion.
    pub fn pending_offline_declaration(&self) -> io::Result<OfflineHomePlan> {
        self.validate()?;
        let declaration = self.manifest.offline_home.as_ref().ok_or_else(invalid)?;
        if declaration.source_witness().is_none() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Pending export has no independently reopenable source witness",
            ));
        }
        Ok(declaration.clone())
    }
    #[cfg(target_os = "linux")]
    pub fn pending_original_declaration(&self) -> io::Result<OriginalSelectionPlan> {
        self.validate()?;
        match (
            &self.manifest.offline_home,
            &self.manifest.offline_applications,
        ) {
            (Some(_), None) => Ok(OriginalSelectionPlan::Home(
                self.pending_offline_declaration()?,
            )),
            (None, Some(plan)) if plan.source_witness().is_some() => {
                plan.validate()?;
                Ok(OriginalSelectionPlan::Applications(plan.clone()))
            }
            _ => Err(invalid()),
        }
    }
    /// Pending offline declarations are reviewable, but the ordinary native
    /// installer/first-use importer cannot treat them as finished. There is
    /// deliberately no public-JSON receipt bypass: a future offline consumer
    /// must authenticate the actual completed Home before admitting custody.
    ///
    /// # Errors
    /// Refuses changed setup custody or an unfinished offline export.
    pub fn require_completed_export(&self) -> io::Result<()> {
        self.validate()?;
        if self.manifest.offline_home.is_some() || self.pending_applications_export() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Prepared home export awaits explicit offline completion",
            ));
        }
        Ok(())
    }
    fn pending_applications_export(&self) -> bool {
        #[cfg(target_os = "linux")]
        {
            self.manifest.offline_applications.is_some()
        }
        #[cfg(not(target_os = "linux"))]
        {
            false
        }
    }
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }
    pub fn validate(&self) -> io::Result<()> {
        self.pin.validate()?;
        members(&self.pin, &self.manifest)?;
        let manifest: Manifest =
            serde_json::from_slice(&bounded_file(&self.pin, MANIFEST_FILE, MANIFEST_LIMIT)?)
                .map_err(|_| invalid())?;
        if manifest != self.manifest
            || <[u8; 32]>::from(Sha256::digest(bounded_file(
                &self.pin,
                IDENTITY_FILE,
                LIMITS.plaintext_bytes,
            )?)) != self.identity_hash
        {
            return Err(invalid());
        }
        if let Some(artifact) = &self.account_artifact {
            if bounded_file(&self.pin, ACCOUNTS_FILE, LIMITS.plaintext_bytes)?
                != artifact.as_bytes()
            {
                return Err(invalid());
            }
        }
        if let Some(artifact) = &self.session_artifact {
            if bounded_file(&self.pin, SERVICE_SESSIONS_FILE, LIMITS.plaintext_bytes)?
                != artifact.as_bytes()
            {
                return Err(invalid());
            }
        }
        Ok(())
    }
    fn target(&self, target: &Path) -> io::Result<()> {
        self.target_privacy(target, true)
    }
    fn target_privacy(&self, target: &Path, private: bool) -> io::Result<()> {
        let existing = match std::fs::symlink_metadata(target) {
            Ok(_) => true,
            Err(error) if error.kind() == io::ErrorKind::NotFound => false,
            Err(error) => return Err(error),
        };
        let (pin, candidate) = if existing {
            (
                PinnedDirectory::open_absolute(target)?,
                target.canonicalize()?,
            )
        } else {
            let parent = target.parent().ok_or_else(invalid)?;
            let name = target.file_name().ok_or_else(invalid)?;
            (
                PinnedDirectory::open_absolute(parent)?,
                parent.canonicalize()?.join(name),
            )
        };
        pin.validate_destination()?;
        if private {
            destination_security::check_private_file(&pin.file)?;
        }
        if candidate.starts_with(&self.root) || self.root.starts_with(&candidate) {
            return Err(invalid());
        }
        Ok(())
    }
    /// Check both explicit grants before accepting any identity/account custody.
    pub fn validate_destinations(&self, identity: &Path, accounts: &Path) -> io::Result<()> {
        self.require_completed_export()?;
        self.target(identity)?;
        self.target(accounts)
    }
    fn enrolled(
        &self,
        password: &[u8],
    ) -> io::Result<tilo_identity_host::enrolled_transport::AuthenticatedEnrolledDevice> {
        self.validate()?;
        let IdentityKind::EnrolledDevice { device_public_key } = self.manifest.identity_kind else {
            return Err(invalid());
        };
        tilo_identity_host::enrolled_transport::AuthenticatedEnrolledDevice::open(
            &self.root.join(IDENTITY_FILE),
            password,
            &self.manifest.setup_binding,
            self.manifest.identity_public_key,
            device_public_key,
            now_ns()?,
        )
    }
    /// Resolve exact existing current-owner custody without importing or replacing it.
    pub fn resolve_current_identity(
        &self,
        system: &Path,
        target: &Path,
        password: &[u8],
    ) -> io::Result<tilo_identity_host::enrolled_transport::ResolvedCurrentIdentity> {
        self.require_completed_export()?;
        self.validate()?;
        let IdentityKind::CurrentDevice { device_public_key } = self.manifest.identity_kind else {
            return Err(invalid());
        };
        let reference =
            tilo_identity_host::enrolled_transport::AuthenticatedCurrentDeviceReference::open(
                &self.root.join(IDENTITY_FILE),
                password,
                &self.manifest.setup_binding,
                self.manifest.identity_public_key,
                device_public_key,
                now_ns()?,
            )?;
        let identity = reference.resolve_current(system, target)?;
        identity.validate()?;
        self.validate()?;
        Ok(identity)
    }
    /// Root-owned installer phase; legacy backups authenticate without machine writes.
    pub fn publish_enrolled_system(&self, system_root: &Path, password: &[u8]) -> io::Result<()> {
        self.require_completed_export()?;
        if matches!(
            self.manifest.identity_kind,
            IdentityKind::CurrentDevice { .. }
        ) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Current device reuse is a same-user desktop preparation",
            ));
        }
        if matches!(
            self.manifest.identity_kind,
            IdentityKind::EnrolledDevice { .. }
        ) {
            self.target_privacy(system_root, false)?;
            let enrolled = self.enrolled(password)?;
            enrolled.import_system(system_root)?;
            #[cfg(target_os = "linux")]
            installed_person::publish_namespace(&enrolled, system_root)?;
        } else if runtime_identity::verify_portable_backup(self.root.join(IDENTITY_FILE), password)?
            .group_public_key
            != self.manifest.identity_public_key
        {
            return Err(invalid());
        }
        #[cfg(target_os = "linux")]
        installed_retained::publish_namespace(self, system_root)?;
        #[cfg(target_os = "linux")]
        installed_original_copy::publish_namespace(self, system_root)?;
        Ok(())
    }

    /// Import joined-device custody for a host Desktop login into its configured
    /// private device tree. This never publishes the OS-wide device namespace.
    /// # Errors
    /// Refuses non-enrolled or OS-only payloads, insecure or overlapping grants,
    /// conflicting existing device/persona files, and invalid encrypted custody.
    pub fn import_host_identity(
        &self,
        system: &Path,
        target: &Path,
        password: &[u8],
    ) -> io::Result<PathBuf> {
        self.require_completed_export()?;
        #[cfg(target_os = "linux")]
        if self
            .manifest
            .installed_original_copy_declaration()?
            .is_some()
        {
            return Err(invalid());
        }
        if !matches!(
            self.manifest.identity_kind,
            IdentityKind::EnrolledDevice { .. }
        ) || self.manifest.portable_home.is_some()
            || self.manifest.portable_flatpak.is_some()
            || self.manifest.retained_home.is_some()
        {
            return Err(invalid());
        }
        self.target(system)?;
        self.target(target)?;
        // The shell prepares this exact private tree before first-use import.
        // Existing canonical transport checks every file before publication.
        let pin = PinnedDirectory::open_absolute(system)?;
        destination_security::check_private_file(&pin.file)?;
        let imported = self.enrolled(password)?.import_into(system, target)?;
        pin.validate_destination()?;
        self.validate()?;
        Ok(imported.identity_record)
    }
    /// Import into explicit target-user custody; no active persona is switched.
    pub fn import_identity(&self, target: &Path, password: &[u8]) -> io::Result<PathBuf> {
        self.require_completed_export()?;
        self.target(target)?;
        if matches!(
            self.manifest.identity_kind,
            IdentityKind::CurrentDevice { .. }
        ) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "Current device identity requires retained activation custody",
            ));
        }
        if matches!(
            self.manifest.identity_kind,
            IdentityKind::EnrolledDevice { .. }
        ) {
            return self
                .enrolled(password)?
                .import_public_identity(Path::new("/var/lib/tilo"), target);
        }
        let imported = runtime_identity::import_portable_backup_matching(
            self.root.join(IDENTITY_FILE),
            password,
            target,
            "primary",
            self.manifest.identity_public_key,
        );
        match imported {
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                // The canonical importer already authenticated this password
                // before refusing the existing id. Resume only the same usable
                // signing custody; never replace another persona or remint it.
                let target_pin = PinnedDirectory::open_absolute(target)?;
                target_pin.validate_destination()?;
                destination_security::check_private_file(&target_pin.file)?;
                let path = target.join("primary.json");
                let signer =
                    runtime_identity::load_from_path(path.clone()).map_err(|_| invalid())?;
                if !signer.is_single_device()
                    || signer.group_public_key() != self.manifest.identity_public_key
                {
                    return Err(error);
                }
                target_pin.validate()?;
                self.validate()?;
                Ok(path)
            }
            other => other,
        }
    }
    /// Reseal explicitly selected website sessions. This does not authenticate a service.
    /// # Errors
    /// Refuses changed artifacts, bindings, private target grants or existing custody conflicts.
    pub fn import_service_sessions(&self, config_root: &Path, password: &[u8]) -> io::Result<()> {
        self.require_completed_export()?;
        self.target(config_root)?;
        match (&self.session_artifact, &self.manifest.service_sessions) {
            (Some(artifact), Some(declaration)) => super::service_sessions::import(
                artifact,
                declaration,
                password,
                self.manifest.identity_public_key,
                &self.manifest.setup_binding,
                config_root,
            ),
            (None, None) => Ok(()),
            _ => Err(invalid()),
        }
    }
    /// Reseal exact consented credentials under the target user's canonical
    /// vault. `config_root` is their private `.config/tilo`, never a live source.
    pub fn import_accounts(
        &self,
        config_root: &Path,
        password: &[u8],
    ) -> io::Result<Vec<AccountId>> {
        self.require_completed_export()?;
        self.target(config_root)?;
        match &self.account_artifact {
            Some(artifact) => {
                artifact
                    .verify_selection(
                        password,
                        self.manifest.setup_binding.as_bytes(),
                        &self.manifest.ids(),
                    )
                    .map_err(|_| invalid())?;
                // Canonical account storage may create a publicly searchable
                // config root. Preparation promises private target custody,
                // including verified retries, so create only this fresh grant
                // privately after authentication and retain its descriptor.
                let target = match std::fs::symlink_metadata(config_root) {
                    Ok(_) => PinnedDirectory::open_absolute(config_root)?,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        let parent = PinnedDirectory::open_absolute(
                            config_root.parent().ok_or_else(invalid)?,
                        )?;
                        parent.validate_destination()?;
                        destination_security::check_private_file(&parent.file)?;
                        parent.create_child(config_root.file_name().ok_or_else(invalid)?)?
                    }
                    Err(error) => return Err(error),
                };
                destination_security::check_private_file(&target.file)?;
                let imported = Connector::new(config_root.to_owned())
                    .import_password_sealed_accounts_matching(
                        artifact,
                        password,
                        self.manifest.setup_binding.as_bytes(),
                        &self.manifest.ids(),
                    )
                    .map_err(|_| invalid())?;
                target.validate()?;
                Ok(imported)
            }
            None => Ok(Vec::new()),
        }
    }
}
#[cfg(test)]
mod tests;
