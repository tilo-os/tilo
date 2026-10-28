//! Explicit private source grant carried from belongings review into staging.
//! Capturing an intent never mounts, copies or installs. The actual stage UUID
//! and retained partition geometry are bound again on the download worker.
use crate::setup::{
    destination_security,
    flatpak_transfer::{HandoffInventory, InstalledRef, NativeRunner},
    migration::{OwnedTemporary, PinnedDirectory},
    migration_durability,
    migration_handoff::{Categories, Plan},
    migration_store::NativePath,
    staging,
};
use cap_fs_ext::{OpenOptionsFollowExt, OpenOptionsSyncExt};
use cap_primitives::fs::{FollowSymlinks, OpenOptions};
use std::{
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};
#[path = "app_budget.rs"]
mod app_budget;
#[cfg(target_os = "linux")]
#[path = "computer_applications.rs"]
mod computer_applications;
#[cfg(target_os = "linux")]
#[path = "retained_caller.rs"]
mod retained_caller;

const FILE: &str = "migration-intent.json";
const MAX: u64 = 9 * 1024 * 1024;
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Grant {
    schema: u32,
    source: NativePath,
    plan: Plan,
    flatpak: Option<Vec<u8>>,
    #[serde(default)]
    app_budget: Option<app_budget::Budget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    computer_applications: Option<NativePath>,
}
#[derive(Clone)]
pub struct MigrationIntent {
    directory: PathBuf,
    directory_pin: Arc<PinnedDirectory>,
    source_pin: Arc<PinnedDirectory>,
    grant: Grant,
    #[cfg(target_os = "linux")]
    computer_applications: Option<Arc<MigrationIntent>>,
}
impl std::fmt::Debug for MigrationIntent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MigrationIntent")
            .field("operation", &self.grant.plan.operation_id)
            .finish_non_exhaustive()
    }
}
impl PartialEq for MigrationIntent {
    fn eq(&self, other: &Self) -> bool {
        self.directory == other.directory && self.grant == other.grant
    }
}
impl Eq for MigrationIntent {}
fn error(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn private(directory: &Path) -> io::Result<Arc<PinnedDirectory>> {
    let pin = PinnedDirectory::open_absolute(directory)?;
    pin.validate_destination()?;
    destination_security::check_private_file(&pin.file)?;
    Ok(pin)
}
pub type Capture = fn(&Path, &str, Categories) -> io::Result<Plan>;
impl MigrationIntent {
    /// Preserve separate original-home and application grants when both are
    /// selected. Platform admission belongs to the backend, not the journey.
    pub fn capture_reviewed_with_policy(
        home: &Path,
        directory: &Path,
        categories: [bool; 3],
        refs: Vec<InstalledRef>,
        policy: crate::setup::migration_handoff::HomePolicy,
    ) -> io::Result<Self> {
        Self::capture_reviewed_selection(home, directory, categories, refs, policy, &[])
    }

    /// The same reviewed capture with the person's nested source exclusions.
    /// Exclusions are consent metadata: the plan carries them and the receiving
    /// producer revalidates each path under its own pinned source.
    ///
    /// # Errors
    /// Refuses unsafe grants, unavailable private state and malformed exclusions.
    pub fn capture_reviewed_selection(
        home: &Path,
        directory: &Path,
        categories: [bool; 3],
        refs: Vec<InstalledRef>,
        policy: crate::setup::migration_handoff::HomePolicy,
        exclusions: &[NativePath],
    ) -> io::Result<Self> {
        if categories[0] && categories[1] {
            Self::capture_computer_applications_selected(
                home, directory, categories, refs, exclusions,
            )
        } else {
            Self::capture_source_selection(home, directory, categories, refs, policy, exclusions)
        }
    }
    /// Reattest the exact applications selected alongside a retained home.
    /// Inventory metadata alone is not a grant to additional application data.
    pub fn validate_computer_applications_selection(
        &self,
        selected: &[InstalledRef],
    ) -> io::Result<()> {
        let apps = self
            .computer_applications()?
            .ok_or_else(|| error("Missing selected computer applications"))?;
        let inventory = apps
            .for_media()?
            .flatpak_inventory
            .ok_or_else(|| error("Missing selected application inventory"))?;
        let inventory =
            HandoffInventory::parse(&serde_json::to_vec(&inventory)?).map_err(io::Error::other)?;
        if selected.len() != inventory.selected().len()
            || selected
                .iter()
                .any(|reference| !inventory.selected().contains(reference))
        {
            return Err(error(
                "Selected computer applications differ from reviewed inventory",
            ));
        }
        Ok(())
    }
    /// Export the encrypted artifact outside the actual selected original Home.
    /// Both ancestry directions are checked against held directory identities,
    /// rather than the caller's spelling of a path.
    pub fn validate_artifact_destination(&self, destination: &PinnedDirectory) -> io::Result<()> {
        self.source_pin.validate()?;
        destination.validate_destination()?;
        if destination.contains_identity(&self.source_pin.identity)
            || self.source_pin.contains_identity(&destination.identity)
        {
            return Err(error(
                "Prepared artifact destination overlaps original Home",
            ));
        }
        self.source_pin.validate()?;
        destination.validate_destination()
    }
    /// The actual rebound staging selection enters the canonical root helper;
    /// a serialized Plan cannot replace this retained original permission.
    #[cfg(target_os = "linux")]
    pub fn register_staging_copy(
        &self,
        bound: &Plan,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> io::Result<crate::setup::portable_home_media::RegisteredOriginalSource> {
        crate::setup::migration::check_cancel(cancel)?;
        let reviewed = self.for_media()?;
        if reviewed.home != bound.home
            || reviewed.categories != bound.categories
            || reviewed.home_policy != crate::setup::migration_handoff::HomePolicy::Copy
            || bound.home_policy != reviewed.home_policy
        {
            return Err(error(
                "Staging source differs from actual reviewed permission",
            ));
        }
        let source = self.grant.source.decode().map_err(io::Error::other)?;
        let registered = crate::setup::portable_home_media::register_staging_copy(
            &source,
            &self.source_pin,
            bound,
            cancel,
        )?;
        let (expected, _) = bound.original_inventory_selection()?;
        if registered.plan != expected || self.for_media()? != reviewed {
            return Err(error("Staging source changed during original registration"));
        }
        self.directory_pin.validate_destination()?;
        self.source_pin.validate()?;
        Ok(registered)
    }

    pub fn capture(
        source: &Path,
        directory: &Path,
        categories: [bool; 3],
        selected: Vec<InstalledRef>,
    ) -> io::Result<Self> {
        Self::capture_with_policy(
            source,
            directory,
            categories,
            selected,
            crate::setup::migration_handoff::HomePolicy::Copy,
        )
    }
    pub fn capture_with_policy(
        source: &Path,
        directory: &Path,
        categories: [bool; 3],
        selected: Vec<InstalledRef>,
        home_policy: crate::setup::migration_handoff::HomePolicy,
    ) -> io::Result<Self> {
        Self::capture_source_selection(source, directory, categories, selected, home_policy, &[])
    }
    /// Capture one source scope with explicit nested exclusions.
    ///
    /// # Errors
    /// Refuses unsafe grants, unavailable private state and malformed exclusions.
    pub fn capture_source_selection(
        source: &Path,
        directory: &Path,
        categories: [bool; 3],
        selected: Vec<InstalledRef>,
        home_policy: crate::setup::migration_handoff::HomePolicy,
        exclusions: &[NativePath],
    ) -> io::Result<Self> {
        Self::capture_inner(
            source,
            directory,
            categories,
            selected,
            home_policy,
            crate::setup::migration_handoff::capture,
            exclusions,
        )
    }
    #[cfg(any(test, feature = "test-support"))]
    pub fn capture_using(
        source: &Path,
        directory: &Path,
        categories: [bool; 3],
        selected: Vec<InstalledRef>,
        home_policy: crate::setup::migration_handoff::HomePolicy,
        capture: Capture,
    ) -> io::Result<Self> {
        Self::capture_inner(
            source,
            directory,
            categories,
            selected,
            home_policy,
            capture,
            &[],
        )
    }
    fn capture_inner(
        source: &Path,
        directory: &Path,
        categories: [bool; 3],
        selected: Vec<InstalledRef>,
        home_policy: crate::setup::migration_handoff::HomePolicy,
        capture: Capture,
        exclusions: &[NativePath],
    ) -> io::Result<Self> {
        let categories = Categories {
            home: categories[0],
            flatpak: categories[1],
            accounts: categories[2],
        };
        if !categories.flatpak && !selected.is_empty() {
            return Err(error(
                "Application selections require application migration consent",
            ));
        }
        let source_pin = PinnedDirectory::open_absolute(source)?;
        let directory_pin = private(directory)?;
        let operation = uuid::Uuid::new_v4().to_string();
        let mut plan = capture(source, &operation, categories)?;
        plan.home_policy = home_policy;
        plan.home_exclusions = exclusions.to_vec();
        plan.validate()?;
        let flatpak = if categories.flatpak {
            let bind = |root: &Path| {
                crate::setup::migration_handoff::capture(root, &operation, categories)
                    .and_then(|plan| plan.bytes())
                    .map_err(|e| e.to_string())
            };
            let inventory = if categories.home {
                HandoffInventory::capture(&NativeRunner, source, selected, bind)
            } else {
                HandoffInventory::capture_selected_state(&NativeRunner, source, selected, bind)
            }
            .map_err(io::Error::other)?;
            Some(inventory.bytes().map_err(io::Error::other)?)
        } else {
            None
        };
        source_pin.validate()?;
        let app_budget = flatpak
            .as_ref()
            .map(|bytes| {
                let inventory = HandoffInventory::parse(bytes).map_err(io::Error::other)?;
                if categories.home {
                    app_budget::Budget::capture(source, inventory.selected())
                } else {
                    app_budget::Budget::capture_selected(source, inventory.selected())
                }
            })
            .transpose()?;
        let grant = Grant {
            app_budget,
            computer_applications: None,
            schema: 1,
            source: NativePath::encode(source),
            plan,
            flatpak,
        };
        let bytes = serde_json::to_vec(&grant).map_err(io::Error::other)?;
        if bytes.len() as u64 > MAX {
            return Err(error("Migration intent exceeds its metadata bound"));
        }
        let mut temporary = OwnedTemporary::create(&directory_pin, std::ffi::OsStr::new(FILE))?;
        temporary.file.write_all(&bytes)?;
        migration_durability::sync_file(&temporary.file)?;
        temporary.validate()?;
        cap_primitives::fs::hard_link(
            &directory_pin.file,
            Path::new(&temporary.name),
            &directory_pin.file,
            Path::new(FILE),
        )?;
        migration_durability::sync_directory(&directory_pin.file)?;
        directory_pin.validate_destination()?;
        let intent = Self {
            directory: directory.to_path_buf(),
            directory_pin,
            source_pin,
            grant,
            #[cfg(target_os = "linux")]
            computer_applications: None,
        };
        intent.renew_application_budget()?;
        Ok(intent)
    }
    /// Whether the reviewed grant includes separately captured native applications.
    /// Unsupported hosts refuse native grants instead of treating them as empty.
    pub fn has_computer_applications(&self) -> io::Result<bool> {
        #[cfg(target_os = "linux")]
        {
            self.computer_applications().map(|intent| intent.is_some())
        }
        #[cfg(not(target_os = "linux"))]
        {
            if self.grant.computer_applications.is_some() {
                Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "Native application migration is unavailable on this host",
                ))
            } else {
                Ok(false)
            }
        }
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }
    pub fn application_minimum(&self) -> u64 {
        #[cfg(target_os = "linux")]
        if let Some(apps) = &self.computer_applications {
            return apps.application_minimum();
        }
        self.grant
            .app_budget
            .as_ref()
            .map_or(staging::MIN_TARGET_BYTES, app_budget::Budget::minimum)
    }
    fn renew_application_budget(&self) -> io::Result<()> {
        match (&self.grant.app_budget, &self.grant.flatpak) {
            (Some(budget), Some(bytes)) if self.grant.plan.categories.flatpak => {
                let inventory = HandoffInventory::parse(bytes).map_err(io::Error::other)?;
                budget.renew(
                    &self.grant.source.decode().map_err(io::Error::other)?,
                    inventory.selected(),
                )
            }
            (None, None) if !self.grant.plan.categories.flatpak => Ok(()),
            _ => Err(error(
                "Application transfer has no source-bound storage review",
            )),
        }
    }
    pub fn source_plan(&self) -> Plan {
        self.grant.plan.clone()
    }

    /// Bind an already written private source journal to this reviewed grant.
    /// This changes only the plan's opaque locator; source custody, identity
    /// and selection authority stay with the existing intent.
    ///
    /// # Errors
    /// Refuses a locator that does not match the reviewed UID 1000 plan.
    pub fn attach_deferred_handoff(
        &self,
        handoff: crate::setup::migration_handoff::DeferredHandoff,
    ) -> io::Result<Self> {
        let mut attached = self.clone();
        attached.grant.plan.attach_deferred_handoff(handoff)?;
        Ok(attached)
    }
    /// Re-attest a granted original home before sealing USB setup metadata.
    /// No destination geometry is invented; the USB installer must check the
    /// retained partition against its actual chosen target before mutation.
    pub fn for_media(&self) -> io::Result<Plan> {
        #[cfg(target_os = "linux")]
        self.validate_computer_applications()?;
        self.directory_pin.validate_destination()?;
        self.source_pin.validate()?;
        let source = self.grant.source.decode().map_err(io::Error::other)?;
        let mut current = crate::setup::migration_handoff::capture(
            &source,
            &self.grant.plan.operation_id,
            self.grant.plan.categories,
        )?;
        current.home_policy = self.grant.plan.home_policy;
        current
            .deferred_handoff
            .clone_from(&self.grant.plan.deferred_handoff);
        if current != self.grant.plan {
            return Err(error("The reviewed migration source changed"));
        }
        self.renew_application_budget()?;
        if let Some(bytes) = &self.grant.flatpak {
            let original = HandoffInventory::parse(bytes).map_err(io::Error::other)?;
            let bind = |root: &Path| {
                crate::setup::migration_handoff::capture(
                    root,
                    &current.operation_id,
                    current.categories,
                )
                .and_then(|plan| plan.bytes())
                .map_err(|error| error.to_string())
            };
            let fresh = if current.categories.home {
                HandoffInventory::capture(
                    &NativeRunner,
                    &source,
                    original.selected().to_vec(),
                    bind,
                )
            } else {
                HandoffInventory::capture_selected_state(
                    &NativeRunner,
                    &source,
                    original.selected().to_vec(),
                    bind,
                )
            }
            .map_err(io::Error::other)?;
            if fresh != original {
                return Err(error("The reviewed Flatpak source changed"));
            }
            original
                .validate_operation(&current.operation_id)
                .map_err(io::Error::other)?;
            current.flatpak_inventory = Some(serde_json::from_slice(bytes)?);
        } else if current.categories.flatpak {
            return Err(error("The reviewed Flatpak source is missing"));
        }
        self.source_pin.validate()?;
        self.directory_pin.validate_destination()?;
        current.validate()?;
        Ok(current)
    }
    /// Authority-only review for an authenticated pending offline declaration.
    /// This never authorizes live copying, deployment or completed export. The
    /// future offline epoch must independently capture content and capacity.
    pub fn for_pending_offline(&self) -> io::Result<Plan> {
        self.directory_pin.validate_destination()?;
        self.source_pin.validate()?;
        if self.grant.plan.home_policy != crate::setup::migration_handoff::HomePolicy::Copy
            || !self.grant.plan.categories.home
        {
            return Err(error(
                "Pending offline export requires original whole-home copy consent",
            ));
        }
        self.pending_original_selection()
    }
    pub fn for_pending_applications(&self) -> io::Result<Plan> {
        self.directory_pin.validate_destination()?;
        self.source_pin.validate()?;
        if self.grant.plan.categories.home
            || !self.grant.plan.categories.flatpak
            || self.grant.plan.home_policy != crate::setup::migration_handoff::HomePolicy::Copy
            || self.grant.flatpak.is_none()
        {
            return Err(error(
                "Pending applications export requires selected app-data consent",
            ));
        }
        self.pending_original_selection()
    }
    /// Revalidate the held producer and its exact private staging hint. The hint
    /// remains metadata; authenticated registration supplies reader authority.
    #[cfg(target_os = "linux")]
    pub fn validate_staging_copy(&self, declared: &Plan) -> io::Result<()> {
        let reviewed = self.for_media()?;
        let bound = Plan::read_private(&self.directory, &declared.operation_id)?;
        if reviewed.home != bound.home
            || reviewed.categories != bound.categories
            || reviewed.home_policy != crate::setup::migration_handoff::HomePolicy::Copy
            || bound.home_policy != crate::setup::migration_handoff::HomePolicy::Copy
            || bound.original_inventory_selection()?.0 != *declared
        {
            return Err(error(
                "Original staging declaration differs from held source",
            ));
        }
        self.source_pin.validate()?;
        self.directory_pin.validate_destination()
    }
    /// Root registration retains the exact selected inventory and original caller
    /// through the canonical helper/ACK; this is not an immutable export receipt.
    #[cfg(target_os = "linux")]
    pub fn register_pending_applications(
        &self,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> io::Result<crate::setup::portable_home_media::RegisteredOriginalSource> {
        crate::setup::migration::check_cancel(cancel)?;
        let plan = self.for_pending_applications()?;
        let source = self.grant.source.decode().map_err(io::Error::other)?;
        let result = crate::setup::portable_home_media::register_applications(
            &source,
            &self.source_pin,
            &plan,
            cancel,
        );
        self.source_pin.validate()?;
        self.directory_pin.validate_destination()?;
        if self.for_pending_applications()? != plan {
            return Err(error(
                "Selected original applications changed during registration",
            ));
        }
        crate::setup::migration::check_cancel(cancel)?;
        result
    }
    fn pending_original_selection(&self) -> io::Result<Plan> {
        let source = self.grant.source.decode().map_err(io::Error::other)?;
        let mut current = crate::setup::migration_handoff::capture(
            &source,
            &self.grant.plan.operation_id,
            self.grant.plan.categories,
        )?;
        current
            .deferred_handoff
            .clone_from(&self.grant.plan.deferred_handoff);
        if current != self.grant.plan {
            return Err(error("Original pending source authority changed"));
        }
        match (&self.grant.flatpak, current.categories.flatpak) {
            (Some(bytes), true) => {
                let inventory = HandoffInventory::parse(bytes).map_err(io::Error::other)?;
                inventory
                    .validate_operation(&current.operation_id)
                    .map_err(io::Error::other)?;
                // Exact original selection is retained, not asserted to be the
                // final coherent source deployment or current app-data epoch.
                current.flatpak_inventory = Some(serde_json::from_slice(bytes)?);
            }
            (None, false) => {}
            _ => return Err(error("Pending application selection custody is incomplete")),
        }
        current.validate()?;
        self.source_pin.validate()?;
        self.directory_pin.validate_destination()?;
        Ok(current)
    }
    /// Only the explicitly supplied private directory is inspected. No global
    /// discovery or source path fallback participates in stage authority.
    fn load_single(directory: &Path) -> io::Result<Self> {
        let directory_pin = private(directory)?;
        let mut options = OpenOptions::new();
        options.read(true).follow(FollowSymlinks::No).nonblock(true);
        let file = cap_primitives::fs::open(&directory_pin.file, Path::new(FILE), &options)?;
        destination_security::check_private_file(&file)?;
        if !file.metadata()?.is_file() || file.metadata()?.len() > MAX {
            return Err(error("Migration intent must be bounded private metadata"));
        }
        let mut bytes = Vec::new();
        file.take(MAX + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX {
            return Err(error("Migration intent exceeds its metadata bound"));
        }
        let grant: Grant = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
        if grant.schema != 1 {
            return Err(error("Migration intent version is unsupported"));
        }
        grant.plan.validate()?;
        match (&grant.flatpak, grant.plan.categories.flatpak) {
            (Some(bytes), true) => {
                HandoffInventory::parse(bytes).map_err(io::Error::other)?;
            }
            (None, false) => (),
            _ => {
                return Err(error(
                    "Migration application consent differs from its inventory",
                ));
            }
        }
        let source = grant.source.decode().map_err(io::Error::other)?;
        let source_pin = PinnedDirectory::open_absolute(&source)?;
        let mut current = crate::setup::migration_handoff::capture(
            &source,
            &grant.plan.operation_id,
            grant.plan.categories,
        )?;
        current.home_policy = grant.plan.home_policy;
        current
            .deferred_handoff
            .clone_from(&grant.plan.deferred_handoff);
        if current != grant.plan {
            return Err(error("The reviewed migration source changed"));
        }
        directory_pin.validate_destination()?;
        Ok(Self {
            directory: directory.to_path_buf(),
            directory_pin,
            source_pin,
            grant,
            #[cfg(target_os = "linux")]
            computer_applications: None,
        })
    }
    pub fn load(directory: &Path) -> io::Result<Self> {
        let value = Self::load_single(directory)?;
        #[cfg(target_os = "linux")]
        {
            value.load_computer_applications()
        }
        #[cfg(not(target_os = "linux"))]
        {
            if value.grant.computer_applications.is_some() {
                return Err(error("Computer applications require Linux"));
            }
            Ok(value)
        }
    }
    pub fn bind(
        &self,
        operation: &str,
        installation: &staging::plan::Plan,
    ) -> io::Result<serde_json::Value> {
        #[cfg(target_os = "linux")]
        self.validate_computer_applications()?;
        self.directory_pin.validate_destination()?;
        self.source_pin.validate()?;
        if self.grant.plan.categories.flatpak && self.grant.flatpak.is_none() {
            return Err(error("The original reviewed Flatpak inventory is missing"));
        }
        self.renew_application_budget()?;
        let source = self.grant.source.decode().map_err(io::Error::other)?;
        let mut current = crate::setup::migration_handoff::capture(
            &source,
            &self.grant.plan.operation_id,
            self.grant.plan.categories,
        )?;
        current.home_policy = self.grant.plan.home_policy;
        current
            .deferred_handoff
            .clone_from(&self.grant.plan.deferred_handoff);
        self.bind_attested(current, operation, installation)
    }
    fn bind_attested(
        &self,
        current: Plan,
        operation: &str,
        installation: &staging::plan::Plan,
    ) -> io::Result<serde_json::Value> {
        self.directory_pin.validate_destination()?;
        self.source_pin.validate()?;
        let capacity = installation
            .target_extent
            .lbas()
            .checked_mul(installation.basis.disk.sector_bytes)
            .ok_or_else(|| error("Planned application target capacity overflow"))?;
        if capacity < self.application_minimum() {
            return Err(error(
                "Selected Tilo space is below the source-bound application estimate",
            ));
        }
        if current != self.grant.plan {
            return Err(error("The reviewed migration source changed"));
        }
        let mut plan = current;
        plan.operation_id = operation.to_string();
        if let Some(bytes) = &self.grant.flatpak {
            let source = self.grant.source.decode().map_err(io::Error::other)?;
            let inventory = HandoffInventory::parse(bytes)
                .map_err(io::Error::other)?
                .bind_staging(
                    &source,
                    &self.grant.plan.operation_id,
                    operation,
                    installation,
                )
                .map_err(io::Error::other)?;
            plan.flatpak_inventory = Some(serde_json::from_slice(
                &inventory.bytes().map_err(io::Error::other)?,
            )?);
        }

        plan.validate_for_install(installation)?;
        match plan.write_private(&self.directory) {
            Ok(_) => (),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                if Plan::read_private(&self.directory, operation)? != plan {
                    return Err(error(
                        "A different migration is already bound to this installation",
                    ));
                }
            }
            Err(e) => return Err(e),
        }
        self.directory_pin.validate_destination()?;
        self.source_pin.validate()?;
        serde_json::to_value(plan).map_err(io::Error::other)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};
    pub(super) fn fixture() -> (tempfile::TempDir, MigrationIntent, staging::plan::Plan) {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let directory = root.path().join("intent");
        std::fs::create_dir(&source).unwrap();
        std::fs::create_dir(&directory).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(source.join("original"), b"preserve").unwrap();
        let mut basis = staging::plan::tests::linux_basis(
            512,
            256 << 30,
            vec![staging::platform::Partition {
                number: 1,
                guid: "22222222-2222-4222-8222-222222222222".into(),
                type_guid: "0fc63daf-8483-4772-8e79-3d69d8477de4".into(),
                first_lba: 2048,
                last_lba: 1048575,
            }],
        );
        basis.disk.guid = "11111111-1111-4111-8111-111111111111".into();
        let installation = staging::plan::compute(
            &basis,
            staging::plan::Requirements {
                staging_esp_bytes: 600 << 20,
                staging_root_bytes: 40 << 30,
                target_bytes: 64 << 30,
            },
        )
        .unwrap();
        let plan:Plan=serde_json::from_value(serde_json::json!({
            "schema":1,"operation_id":"review-1","categories":{"home":true,"flatpak":false,"accounts":false},
            "home":{"volume":{"disk_guid":basis.disk.guid,"disk_bytes":256u64<<30,"sector_bytes":512,"partition_guid":"22222222-2222-4222-8222-222222222222","first_lba":2048,"last_lba":1048575,"filesystem_uuid":"33333333-3333-4333-8333-333333333333","filesystem":"ext4"},"relative":NativePath::encode(Path::new("home/person")),"inode":1234,"original_uid":1000,"subvolume":null}
        })).unwrap();
        let intent = MigrationIntent {
            directory: directory.clone(),
            directory_pin: private(&directory).unwrap(),
            source_pin: PinnedDirectory::open_absolute(&source).unwrap(),
            #[cfg(target_os = "linux")]
            computer_applications: None,
            grant: Grant {
                schema: 1,
                source: NativePath::encode(&source),
                plan,
                flatpak: None,
                app_budget: None,
                computer_applications: None,
            },
        };
        (root, intent, installation)
    }
    #[test]
    fn actual_operation_binding_is_durable_idempotent_and_refuses_changed_source_or_layout() {
        let (root, intent, installation) = fixture();
        let operation = uuid::Uuid::new_v4().to_string();
        let value = intent
            .bind_attested(intent.grant.plan.clone(), &operation, &installation)
            .unwrap();
        assert_eq!(value["operation_id"], operation);
        assert_eq!(
            Plan::read_private(&intent.directory, &operation)
                .unwrap()
                .operation_id,
            operation
        );
        assert_eq!(
            intent
                .bind_attested(intent.grant.plan.clone(), &operation, &installation)
                .unwrap(),
            value
        );
        let mut changed = intent.grant.plan.clone();
        changed.categories.accounts = true;
        assert!(
            intent
                .bind_attested(changed, &operation, &installation)
                .is_err()
        );
        let mut different = installation;
        different.basis.disk.guid = "44444444-4444-4444-8444-444444444444".into();
        assert!(
            intent
                .bind_attested(
                    intent.grant.plan.clone(),
                    &uuid::Uuid::new_v4().to_string(),
                    &different
                )
                .is_err()
        );
        assert_eq!(
            std::fs::read(root.path().join("source/original")).unwrap(),
            b"preserve"
        );
    }
    #[test]
    fn retained_file_policy_survives_actual_installation_binding_without_copying() {
        let (root, mut intent, installation) = fixture();
        intent.grant.plan.home_policy =
            crate::setup::migration_handoff::HomePolicy::AccessReadWrite;
        let operation = uuid::Uuid::new_v4().to_string();
        let value = intent
            .bind_attested(intent.grant.plan.clone(), &operation, &installation)
            .unwrap();
        assert_eq!(value["home_policy"], "access-read-write");
        let saved = Plan::read_private(&intent.directory, &operation).unwrap();
        assert_eq!(
            saved.home_policy,
            crate::setup::migration_handoff::HomePolicy::AccessReadWrite
        );
        assert_eq!(
            std::fs::read(root.path().join("source/original")).unwrap(),
            b"preserve"
        );
        assert_eq!(std::fs::read_dir(&intent.directory).unwrap().count(), 1);
        assert!(!root.path().join("imported-home").exists());
    }
    #[test]
    fn selected_flatpak_cannot_be_silently_lost_in_stage_binding() {
        let (_root, mut intent, installation) = fixture();
        intent.grant.plan.categories.flatpak = true;
        let error = intent
            .bind(&uuid::Uuid::new_v4().to_string(), &installation)
            .unwrap_err();
        assert!(error.to_string().contains("Flatpak inventory"));
        assert_eq!(std::fs::read_dir(&intent.directory).unwrap().count(), 0);
    }
    #[test]
    fn explicit_intent_load_refuses_link_and_fifo_without_reading_other_content() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().canonicalize().unwrap();
        let outside = directory.join("outside");
        std::fs::write(&outside, b"unchanged").unwrap();
        symlink(&outside, directory.join(FILE)).unwrap();
        assert!(MigrationIntent::load(&directory).is_err());
        assert_eq!(std::fs::read(&outside).unwrap(), b"unchanged");
        std::fs::remove_file(directory.join(FILE)).unwrap();
        use std::os::unix::ffi::OsStrExt;
        let name = std::ffi::CString::new(directory.join(FILE).as_os_str().as_bytes()).unwrap();
        // SAFETY: terminated name points inside this isolated fixture.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(MigrationIntent::load(&directory).is_err());
    }
}

#[cfg(any(test, feature = "test-support"))]
pub fn reviewed_app_routing_fixture() -> (tempfile::TempDir, MigrationIntent) {
    use std::os::unix::fs::PermissionsExt as _;
    let root = tempfile::tempdir().unwrap();
    let source = root.path().join("source");
    let directory = root.path().join("intent");
    std::fs::create_dir(&source).unwrap();
    std::fs::create_dir(&directory).unwrap();
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(source.join("original"), b"preserve").unwrap();
    // Metadata-only routing fixture; this cannot mint source or target authority.
    let mut plan = crate::setup::migration_handoff::fixture_plan();
    plan.operation_id = "review-1".into();
    plan.categories = Categories {
        home: true,
        flatpak: false,
        accounts: false,
    };
    plan.home.volume.disk_bytes = 256 << 30;
    let mut intent = MigrationIntent {
        directory: directory.clone(),
        directory_pin: private(&directory).unwrap(),
        source_pin: PinnedDirectory::open_absolute(&source).unwrap(),
        computer_applications: None,
        grant: Grant {
            schema: 1,
            source: NativePath::encode(&source),
            plan,
            flatpak: None,
            app_budget: None,
            computer_applications: None,
        },
    };
    let source = intent.grant.source.decode().unwrap();
    std::fs::create_dir_all(source.join(".var/app/org.example.Chat")).unwrap();
    std::fs::File::create(source.join(".var/app/org.example.Chat/data"))
        .unwrap()
        .set_len(80 << 30)
        .unwrap();
    intent.grant.app_budget = Some(app_budget::Budget::capture(&source, &[]).unwrap());
    intent.grant.plan.categories.flatpak = true;
    intent.grant.plan.home_policy = crate::setup::migration_handoff::HomePolicy::AccessReadWrite;
    let inventory = serde_json::json!({"schema":1,"inventory":{"refs":[],"remotes":[]},"selected":[],"installations":[]});
    let bytes = serde_json::to_vec(&inventory).unwrap();
    HandoffInventory::parse(&bytes).unwrap();
    intent.grant.flatpak = Some(bytes);
    intent.renew_application_budget().unwrap();
    (root, intent)
}

#[cfg(test)]
mod app_budget_gate_tests {
    use super::*;
    #[test]
    fn manually_selected_plan_below_actual_app_budget_cannot_bind_or_publish() {
        let (root, mut intent, installation) = super::tests::fixture();
        let source = intent.grant.source.decode().unwrap();
        std::fs::create_dir_all(source.join(".var/app/org.example.Chat")).unwrap();
        std::fs::File::create(source.join(".var/app/org.example.Chat/data"))
            .unwrap()
            .set_len(64 << 30)
            .unwrap();
        intent.grant.app_budget = Some(app_budget::Budget::capture(&source, &[]).unwrap());
        assert!(intent.application_minimum() > 64 << 30);
        let failure = intent
            .bind_attested(intent.grant.plan.clone(), "actual-stage", &installation)
            .unwrap_err();
        assert!(
            failure
                .to_string()
                .contains("source-bound application estimate")
        );
        assert_eq!(std::fs::read_dir(&intent.directory).unwrap().count(), 0);
        assert!(root.path().exists());
    }
}
