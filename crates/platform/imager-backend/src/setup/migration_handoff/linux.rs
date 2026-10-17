use super::*;
use block_devices::{BlockDevice, Lsblk};
use cap_primitives::fs::MetadataExt;
use std::os::unix::fs::MetadataExt as _;
use std::{ffi::OsString, fs, os::unix::ffi::OsStringExt};

mod installed_scratch;
pub use installed_scratch::{InstalledScratchFacts, InstalledScratchIdentity};
mod mounted_entry;
mod participant_namespace;
mod root_readonly;
pub use mounted_entry::{capture_mounted_entry, mounted_record_namespace};
pub use participant_namespace::ParticipantNamespace;

mod mounts;
use mounts::{Mount, parse as parse_mounts};

/// Explicit source-layout rejection, distinct from malformed or stale evidence.
fn unsupported_source(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, message)
}

const COLUMNS: &str = "NAME,TYPE,SIZE,START,FSTYPE,UUID,PARTUUID,PTUUID,PTTYPE,LOG-SEC,MAJ:MIN";

struct Facts {
    owned_bindings: Vec<(u64, String)>,
    mounts: Vec<Mount>,
    devices: Vec<BlockDevice>,
}
impl Facts {
    fn read() -> io::Result<Self> {
        #[cfg(any(test, feature = "test-support"))]
        if let Some((mountinfo, devices)) = crate::setup::discovery_fixture::fixture_source_facts()
        {
            return Ok(Self {
                owned_bindings: Vec::new(),
                mounts: parse_mounts(&mountinfo)?,
                devices: block_devices::lsblk::parse(&devices)
                    .ok_or_else(|| refusal("Malformed isolated source disk fixture"))?,
            });
        }
        use std::io::Read;
        let mut mountinfo = Vec::new();
        fs::File::open("/proc/self/mountinfo")?
            .take(4 * 1024 * 1024 + 1)
            .read_to_end(&mut mountinfo)?;
        let devices = Lsblk::new(COLUMNS)
            .full_paths(true)
            .list()
            .ok_or_else(|| refusal("Cannot establish stable source disk identities."))?;
        Ok(Self {
            owned_bindings: Vec::new(),
            mounts: parse_mounts(&mountinfo)?,
            devices,
        })
    }
    fn covering(&self, path: &Path) -> io::Result<&Mount> {
        let mount = self
            .mounts
            .iter()
            .enumerate()
            .filter(|(_, mount)| path.starts_with(&mount.point))
            .max_by_key(|(index, mount)| (mount.point.components().count(), *index))
            .map(|(_, mount)| mount)
            .ok_or_else(|| refusal("Source home has no visible filesystem mount."))?;
        if !mount.source_eligible {
            return Err(unsupported_source(
                "Selected source is a nonstorage namespace mount.",
            ));
        }
        Ok(mount)
    }
    fn volume(&self, mount: &Mount) -> io::Result<(SourceVolume, String)> {
        if !mount.source_eligible {
            return Err(unsupported_source(
                "Selected source is a nonstorage namespace mount.",
            ));
        }
        let mut matches = Vec::new();
        for disk in &self.devices {
            if disk.kind != "disk" || disk.pttype.as_deref() != Some("gpt") {
                continue;
            }
            for partition in &disk.children {
                if partition.kind == "part"
                    && (matches_mount(partition, mount)
                        || self
                            .owned_bindings
                            .iter()
                            .any(|(id, device)| *id == mount.id && *device == partition.name))
                {
                    matches.push((volume(disk, partition)?, partition.name.clone()));
                }
            }
        }
        let [(volume, device)] = matches.as_slice() else {
            return Err(unsupported_source(
                "Source requires a unique unencrypted GPT filesystem partition; encrypted, LVM, RAID and ambiguous sources need an explicit unlock/mapping service.",
            ));
        };
        Ok((volume.clone(), device.clone()))
    }
    fn matching_volume(&self, expected: &SourceVolume) -> io::Result<String> {
        let mut matches = Vec::new();
        for disk in &self.devices {
            if disk.kind != "disk" {
                continue;
            }
            for partition in &disk.children {
                if partition
                    .partuuid
                    .as_ref()
                    .is_some_and(|guid| guid.eq_ignore_ascii_case(&expected.partition_guid))
                {
                    if volume(disk, partition)? != *expected {
                        return Err(refusal(
                            "Migration partition or filesystem identity changed.",
                        ));
                    }
                    matches.push(partition.name.clone());
                }
            }
        }
        let [one] = matches.as_slice() else {
            return Err(refusal(
                "Migration source is missing or has duplicated partition identities.",
            ));
        };
        Ok(one.clone())
    }
}

fn matches_mount(device: &BlockDevice, mount: &Mount) -> bool {
    device
        .maj_min
        .as_ref()
        .is_some_and(|number| *number == mount.number && !number.starts_with("0:"))
        || mount.source == Path::new(&device.name)
        || (mount.source.starts_with("/dev/")
            && fs::canonicalize(&mount.source).ok().as_deref() == Some(Path::new(&device.name)))
}
fn volume(disk: &BlockDevice, partition: &BlockDevice) -> io::Result<SourceVolume> {
    if disk.kind != "disk"
        || disk.pttype.as_deref() != Some("gpt")
        || partition.kind != "part"
        || !partition.children.is_empty()
    {
        return Err(unsupported_source(
            "Source is not a directly readable GPT filesystem partition.",
        ));
    }
    let sector_bytes = disk
        .log_sec
        .ok_or_else(|| refusal("Source sector size is missing."))?;
    let start = partition
        .start
        .ok_or_else(|| refusal("Source partition start is missing."))?
        .checked_mul(512)
        .ok_or_else(|| refusal("Source partition offset overflow."))?;
    if !matches!(sector_bytes, 512 | 4096)
        || !start.is_multiple_of(sector_bytes)
        || partition.size == 0
        || !partition.size.is_multiple_of(sector_bytes)
    {
        return Err(refusal("Source partition geometry is incomplete."));
    }
    let filesystem = match partition.fstype.as_deref() {
        Some("ext4") => Filesystem::Ext4,
        Some("xfs") => Filesystem::Xfs,
        Some("btrfs") => Filesystem::Btrfs,
        _ => {
            return Err(unsupported_source(
                "Source filesystem is not supported for read-only Linux migration.",
            ));
        }
    };
    let first_lba = start
        .checked_div(sector_bytes)
        .ok_or_else(|| refusal("Source sector size is zero."))?;
    let last_lba = first_lba
        .checked_add(
            partition
                .size
                .checked_div(sector_bytes)
                .ok_or_else(|| refusal("Source sector size is zero."))?,
        )
        .and_then(|end| end.checked_sub(1))
        .ok_or_else(|| refusal("Source partition extent overflow."))?;
    Ok(SourceVolume {
        disk_guid: disk
            .ptuuid
            .as_deref()
            .ok_or_else(|| refusal("Source GPT UUID missing."))?
            .to_ascii_lowercase(),
        disk_bytes: disk.size,
        sector_bytes,
        partition_guid: partition
            .partuuid
            .as_deref()
            .ok_or_else(|| refusal("Source partition UUID missing."))?
            .to_ascii_lowercase(),
        first_lba,
        last_lba,
        filesystem_uuid: partition
            .uuid
            .as_deref()
            .ok_or_else(|| refusal("Source filesystem UUID missing."))?
            .to_ascii_lowercase(),
        filesystem,
    })
}

pub fn capture(home: &Path, operation: &str, categories: Categories) -> io::Result<Plan> {
    capture_with(home, operation, categories, &Facts::read()?)
}
fn capture_with(
    home: &Path,
    operation: &str,
    categories: Categories,
    facts: &Facts,
) -> io::Result<Plan> {
    let pin = PinnedDirectory::open_absolute(home)?;
    pin.validate()?;
    let mount = facts.covering(home)?;
    let (volume, _) = facts.volume(mount)?;
    let root = mount
        .root
        .strip_prefix("/")
        .map_err(|_| refusal("Source mount root is not filesystem-relative."))?;
    let relative = root.join(
        home.strip_prefix(&mount.point)
            .map_err(|_| refusal("Source home moved outside its mount."))?,
    );
    super::relative(&relative)?;
    let metadata = cap_primitives::fs::Metadata::from_file(&pin.file)?;
    let subvolume = if volume.filesystem == Filesystem::Btrfs {
        Some(super::super::migration_durability::identity(
            &super::super::migration_durability::readable_directory(&pin.file)?,
        )?)
    } else {
        None
    };
    let plan = Plan {
        schema: SCHEMA,
        flatpak_inventory: None,
        operation_id: operation.into(),
        categories,
        home_policy: HomePolicy::Copy,
        home: Home {
            volume,
            relative: NativePath::encode(&relative),
            inode: metadata.ino(),
            original_uid: metadata.uid(),
            subvolume,
        },
    };
    plan.validate()?;
    pin.validate()?;
    Ok(plan)
}

#[derive(Clone)]
pub struct ResolvedSource {
    home: PathBuf,
    plan: Plan,
    pin: Arc<PinnedDirectory>,
    mountpoint: PathBuf,
    mount: Option<Arc<MountedVolume>>,
}
impl ResolvedSource {
    fn facts(&self) -> io::Result<Facts> {
        let mut facts = Facts::read()?;
        if let Some(mount) = &self.mount {
            mount.service.bind_facts(&mut facts)?;
        }
        Ok(facts)
    }

    pub fn qualification_owned_root(&self) -> io::Result<()> {
        self.pin.validate()?;
        if self.mount.is_none() {
            return Err(refusal(
                "Qualification requires an actual owned source mount",
            ));
        }
        let facts = self.facts()?;
        let actual = resolve_mode_with(
            &self.plan,
            &self.mountpoint,
            &facts,
            true,
            AccessMode::ReadOnly,
        )?;
        if !super::super::nofollow::same_directory_identity(
            &actual.pin.identity,
            &self.pin.identity,
        ) {
            return Err(refusal("Qualification source root changed"));
        }
        let selected = facts
            .mounts
            .iter()
            .find(|mount| mount.point == self.mountpoint)
            .ok_or_else(|| refusal("Qualification source mount missing"))?;
        if selected.root != Path::new("/") || facts.volume(selected)?.0 != self.plan.home.volume {
            return Err(refusal(
                "Qualification requires actual full-root source volume",
            ));
        }
        let root = PinnedDirectory::open_absolute(&self.mountpoint)?;
        super::super::home_capture::consumer_qualification::check_source_root_marker(&root.file)?;
        root.validate()?;
        self.pin.validate()
    }

    /// Close all directory pins first, deduplicate exact retained mount Arcs,
    /// then observe every owned unmount. Related roots may share Home's volume;
    /// no one root can retire that mount while another still retains it.
    pub fn retire_many(sources: Vec<Arc<Self>>) -> io::Result<()> {
        let mut unique_sources = Vec::new();
        for source in sources {
            if unique_sources
                .iter()
                .any(|other| Arc::ptr_eq(other, &source))
            {
                drop(source);
            } else {
                unique_sources.push(source);
            }
        }
        let mut mounts = Vec::new();
        let mut failure = None;
        for source in unique_sources {
            match Arc::try_unwrap(source) {
                Ok(source) => {
                    let Self {
                        home,
                        plan,
                        pin,
                        mountpoint,
                        mount,
                    } = source;
                    drop((home, plan, pin, mountpoint));
                    if let Some(mount) = mount {
                        mounts.push(mount);
                    }
                }
                Err(shared) => {
                    if let Some(mount) = &shared.mount {
                        retain_failed_mount(mount.clone());
                    }
                    drop(shared);
                    failure.get_or_insert_with(|| {
                        refusal("Original source resolver still has custody leases")
                    });
                }
            }
        }
        let mut unique = Vec::new();
        for mount in mounts {
            if unique.iter().any(|other| Arc::ptr_eq(other, &mount)) {
                drop(mount);
            } else {
                unique.push(mount);
            }
        }
        for mount in unique {
            if let Err(error) = retire_owned_mount(mount) {
                failure.get_or_insert(error);
            }
        }
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
    /// Only a uniquely held mount created by this resolver can be released.
    /// Borrowed sources have no owned mount and gain no unmount authority.
    pub fn retire(self) -> io::Result<()> {
        let Self {
            home,
            plan,
            pin,
            mountpoint,
            mount,
        } = self;
        drop((home, plan, pin, mountpoint));
        let Some(mount) = mount else {
            return Ok(());
        };
        retire_owned_mount(mount)
    }
}
/// A verified original-files grant retaining its read-only mount lease.
/// This actor exposes access only; it creates no transfer destination or receipt.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AccessMode {
    ReadOnly,
    ReadWrite,
}
pub struct RetainedSource {
    source: ResolvedSource,
    mode: AccessMode,
    root_permit: Option<Arc<crate::setup::home_capture::RootRetainedPermit>>,
}
impl RetainedSource {
    /// Root-only protected record read for the separate retained credential
    /// purpose. Ordinary readonly original-source access stays readonly.
    pub fn open_retained_caller(&self, namespace: &Path) -> io::Result<std::fs::File> {
        self.validate()?;
        if self.mode != AccessMode::ReadWrite || rustix::process::geteuid().as_raw() != 0 {
            return Err(refusal(
                "Retained caller requires a root-held writable source lease",
            ));
        }
        self.source.open_control_file(
            namespace,
            OriginalFile::RetainedCaller,
            self.mode,
            self.root_permit.as_deref(),
        )
    }
    pub fn retained_control_directory(&self, namespace: &Path) -> io::Result<std::fs::File> {
        self.validate()?;
        if self.mode != AccessMode::ReadWrite || rustix::process::geteuid().as_raw() != 0 {
            return Err(refusal(
                "Retained namespace requires actual root writable lease",
            ));
        }
        let directory = self.source.open_control_directory(
            namespace,
            self.mode,
            self.root_permit.as_deref(),
        )?;
        super::super::migration_durability::readable_directory(&directory.file)
    }
    pub fn retained_directory(&self) -> io::Result<std::fs::File> {
        self.validate()?;
        if self.mode != AccessMode::ReadWrite || rustix::process::geteuid().as_raw() != 0 {
            return Err(refusal(
                "Retained projection requires root writable custody",
            ));
        }
        self.source.pin.file.try_clone()
    }
    pub fn retire(self) -> io::Result<()> {
        let Self {
            source,
            mode,
            root_permit,
        } = self;
        drop((mode, root_permit));
        source.retire()
    }
    pub fn mode(&self) -> AccessMode {
        self.mode
    }
    pub fn validate_plan(&self, plan: &Plan) -> io::Result<()> {
        if *plan != self.source.plan {
            return Err(refusal("Retained files consent or source plan changed"));
        }
        Ok(())
    }
    pub fn path(&self) -> &Path {
        &self.source.home
    }
    pub fn validate(&self) -> io::Result<()> {
        self.validate_with(&self.source.facts()?, true)
    }
    fn validate_with(&self, facts: &Facts, check_kernel: bool) -> io::Result<()> {
        self.source.pin.validate()?;
        let current = resolve_mode_with_retained(
            &self.source.plan,
            &self.source.mountpoint,
            facts,
            check_kernel,
            self.mode,
            self.root_permit.as_deref(),
        )?;
        if !super::super::nofollow::same_directory_identity(
            &current.pin.identity,
            &self.source.pin.identity,
        ) || current.home != self.source.home
        {
            return Err(refusal("Retained original-files grant changed"));
        }
        Ok(())
    }
    pub fn validate_target(&self, target: &Path) -> io::Result<()> {
        let target = PinnedDirectory::open_absolute(target)?;
        target.validate_destination()?;
        if target.contains_identity(&self.source.pin.identity)
            || self.source.pin.contains_identity(&target.identity)
        {
            return Err(refusal(
                "Tilo state and retained original files must remain separate",
            ));
        }
        Ok(())
    }
}
impl ResolvedSource {
    fn require_copy_policy(&self) -> Result<(), String> {
        if self.plan.home_policy != HomePolicy::Copy {
            return Err("This plan keeps original files accessible and does not authorize whole-home copying".into());
        }
        Ok(())
    }
    fn into_flatpak_source(self) -> Result<super::super::flatpak_transfer::ReadOnlySource, String> {
        self.pin.validate().map_err(|error| error.to_string())?;
        let root = self.home.clone();
        let pin = Arc::clone(&self.pin);
        super::super::flatpak_transfer::ReadOnlySource::from_owned_resolved(
            &root,
            pin,
            Arc::new(self),
        )
    }

    /// Resolve another reviewed directory in the original filesystem through
    /// this same mount. Its stable identity and operation are re-attested; no
    /// saved host path or global target Flatpak installation is substituted.
    pub fn resolve_related_flatpak_source(
        &self,
        bytes: &[u8],
    ) -> Result<super::super::flatpak_transfer::ReadOnlySource, String> {
        let plan = Plan::parse(bytes).map_err(|error| error.to_string())?;
        if plan.operation_id != self.plan.operation_id || plan.home.volume != self.plan.home.volume
        {
            return Err("Related Flatpak source does not belong to this reviewed operation and source volume".into());
        }
        self.pin.validate().map_err(|error| error.to_string())?;
        kernel_read_only(&self.pin).map_err(|error| error.to_string())?;
        let mut related = resolve_with(
            &plan,
            &self.mountpoint,
            &self.facts().map_err(|error| error.to_string())?,
            true,
        )
        .map_err(|error| error.to_string())?;
        related.mount.clone_from(&self.mount);
        related.into_flatpak_source()
    }

    /// Continue a saved Flatpak selection only with its retained original
    /// deployment/repository inventory, resolved against actual read-only roots.
    pub fn review_transfer_with_handoff(
        &self,
        destination: &Path,
        journals: &Path,
        handoff: &super::super::flatpak_transfer::HandoffInventory,
    ) -> Result<super::super::user_transfer::TransferReview, String> {
        self.require_copy_policy()?;
        if !self.plan.categories.flatpak {
            return Err(
                "System Flatpak restoration was not selected in this migration plan".into(),
            );
        }
        self.pin.validate().map_err(|error| error.to_string())?;
        kernel_read_only(&self.pin).map_err(|error| error.to_string())?;
        let mut secondary = Vec::new();
        let runner = handoff.resolve(
            &self.home,
            super::super::flatpak_transfer::NativeRunner,
            |bytes| self.resolve_inventory_root(bytes, &mut secondary),
        )?;
        let mut review = super::super::user_transfer::review(
            &runner,
            super::super::user_transfer::TransferRequest {
                source_home: self.home.clone(),
                target_home: destination.to_owned(),
                private_journals: journals.to_owned(),
                copy_home: self.plan.categories.home,
                copy_flatpak: true,
                system_refs: handoff.selected().to_vec(),
            },
        )?;
        // Both home and every original Flatpak root survive review cloning into
        // the worker. Target commands still run through the native runner.
        review.retain_source_guard(Arc::new((self.mount.clone(), runner)));
        Ok(review)
    }

    pub fn original_flatpak_runner(
        &self,
        grant: &super::super::flatpak_transfer::offline_original::OriginalInventoryGrant,
    ) -> Result<
        super::super::flatpak_transfer::SourceScopedRunner<
            super::super::flatpak_transfer::portable::NativeOfflineRunner,
        >,
        String,
    > {
        self.require_copy_policy()?;
        if self.plan.categories.home {
            grant.matches_plan(&self.plan)
        } else {
            grant.matches_applications_plan(&self.plan)
        }
        .map_err(|error| error.to_string())?;
        let inventory = grant.inventory();
        inventory.validate_operation(&self.plan.operation_id)?;
        self.pin.validate().map_err(|error| error.to_string())?;
        kernel_read_only(&self.pin).map_err(|error| error.to_string())?;
        let mut secondary = Vec::new();
        inventory.resolve(
            &self.home,
            super::super::flatpak_transfer::portable::NativeOfflineRunner,
            |bytes| self.resolve_inventory_root(bytes, &mut secondary),
        )
    }

    /// Original inventory authority remains bound to original bytes. Only this
    /// private installed companion maps its roots to completed physical geometry.
    pub fn original_flatpak_runner_completed(
        &self,
        grant: &super::super::flatpak_transfer::offline_original::OriginalInventoryGrant,
        geometry: &super::super::home_capture::CompletedOriginalGeometry,
    ) -> Result<
        super::super::flatpak_transfer::SourceScopedRunner<
            super::super::flatpak_transfer::portable::NativeOfflineRunner,
        >,
        String,
    > {
        self.require_copy_policy()?;
        geometry.check(self).map_err(|error| error.to_string())?;
        let requested = geometry.original();
        if requested.categories.home {
            grant.matches_plan(requested)
        } else {
            grant.matches_applications_plan(requested)
        }
        .map_err(|error| error.to_string())?;
        let inventory = grant.inventory();
        inventory.validate_operation(&requested.operation_id)?;
        let mut secondary = Vec::new();
        let runner = inventory.resolve(
            &self.home,
            super::super::flatpak_transfer::portable::NativeOfflineRunner,
            |bytes| {
                let original = Plan::parse(bytes).map_err(|error| error.to_string())?;
                let physical = geometry
                    .map_related(&original)
                    .map_err(|error| error.to_string())?;
                self.resolve_inventory_root(
                    &physical.bytes().map_err(|error| error.to_string())?,
                    &mut secondary,
                )
            },
        )?;
        geometry.check(self).map_err(|error| error.to_string())?;
        Ok(runner)
    }

    fn resolve_inventory_root(
        &self,
        bytes: &[u8],
        secondary: &mut Vec<Self>,
    ) -> Result<super::super::flatpak_transfer::ReadOnlySource, String> {
        let plan = Plan::parse(bytes).map_err(|error| error.to_string())?;
        if plan.operation_id != self.plan.operation_id {
            return Err("Flatpak inventory belongs to another migration operation".into());
        }
        if plan.home.volume == self.plan.home.volume {
            return self.resolve_related_flatpak_source(bytes);
        }
        if let Some(grant) = secondary
            .iter()
            .find(|grant| grant.plan.home.volume == plan.home.volume)
        {
            return grant.resolve_related_flatpak_source(bytes);
        }
        let grant = if self
            .mount
            .as_ref()
            .is_some_and(|mount| mount.service.authenticated_readonly())
        {
            mount_authenticated_read_only(&plan)
        } else {
            mount_read_only(&plan)
        }
        .map_err(|error| error.to_string())?;
        secondary.push(grant.clone());
        grant.into_flatpak_source()
    }

    pub fn recover_promotion(
        &self,
        destination: &Path,
        journals: &Path,
        active_home: &Path,
        adoption_journal: &Path,
    ) -> Result<
        (
            super::super::user_transfer::TransferReview,
            super::super::home_transfer::promotion::PromotionReview,
        ),
        String,
    > {
        self.require_copy_policy()?;
        self.pin.validate().map_err(|error| error.to_string())?;
        kernel_read_only(&self.pin).map_err(|error| error.to_string())?;
        if self.plan.categories.flatpak {
            let raw = self
                .plan
                .flatpak_inventory
                .as_ref()
                .ok_or("Original reviewed application inventory is missing")?;
            let inventory = super::super::flatpak_transfer::HandoffInventory::parse(
                &serde_json::to_vec(raw).map_err(|error| error.to_string())?,
            )?;
            inventory.validate_operation(&self.plan.operation_id)?;
            let mut secondary = Vec::new();
            let runner = inventory.resolve(
                &self.home,
                super::super::flatpak_transfer::NativeRunner,
                |bytes| self.resolve_inventory_root(bytes, &mut secondary),
            )?;
            let (mut transfer, promotion) =
                super::super::user_transfer::recover_promotion_with_runner(
                    &runner,
                    super::super::user_transfer::TransferRequest {
                        source_home: self.home.clone(),
                        target_home: destination.to_owned(),
                        private_journals: journals.to_owned(),
                        copy_home: self.plan.categories.home,
                        copy_flatpak: true,
                        system_refs: inventory.selected().to_vec(),
                    },
                    active_home,
                    adoption_journal,
                )?;
            transfer.retain_source_guard(Arc::new((self.mount.clone(), runner)));
            return Ok((transfer, promotion));
        }
        let (mut transfer, promotion) = super::super::user_transfer::recover_promotion(
            super::super::user_transfer::TransferRequest {
                source_home: self.home.clone(),
                target_home: destination.to_owned(),
                private_journals: journals.to_owned(),
                copy_home: self.plan.categories.home,
                copy_flatpak: false,
                system_refs: Vec::new(),
            },
            active_home,
            adoption_journal,
        )?;
        if let Some(mount) = &self.mount {
            transfer.retain_source_guard(mount.clone());
        }
        Ok((transfer, promotion))
    }
    pub fn review_transfer(
        &self,
        destination: &Path,
        journals: &Path,
    ) -> Result<super::super::user_transfer::TransferReview, String> {
        self.require_copy_policy()?;
        self.pin.validate().map_err(|error| error.to_string())?;
        if self.plan.categories.flatpak {
            let raw = self.plan.flatpak_inventory.as_ref().ok_or(
                "Restoring system Flatpak applications requires the original reviewed app and repository inventory",
            )?;
            let bytes = serde_json::to_vec(raw).map_err(|error| error.to_string())?;
            let inventory = super::super::flatpak_transfer::HandoffInventory::parse(&bytes)?;
            inventory.validate_operation(&self.plan.operation_id)?;
            return self.review_transfer_with_handoff(destination, journals, &inventory);
        }
        if !self.plan.categories.home && !self.plan.categories.flatpak {
            return Err("This plan requests account connections only; use the reviewed account import actions.".into());
        }
        let mut review = super::super::user_transfer::review(
            &super::super::flatpak_transfer::NativeRunner,
            super::super::user_transfer::TransferRequest {
                source_home: self.home.clone(),
                target_home: destination.to_owned(),
                private_journals: journals.to_owned(),
                copy_home: self.plan.categories.home,
                copy_flatpak: self.plan.categories.flatpak,
                system_refs: Vec::new(),
            },
        )?;
        if let Some(mount) = &self.mount {
            review.retain_source_guard(mount.clone());
        }
        Ok(review)
    }
}

/// Resolve a separately reviewed Flatpak installation root through the same
/// stable GPT/filesystem/inode grant and retained read-only mount as home data.
pub fn resolve_flatpak_source(
    bytes: &[u8],
) -> Result<super::super::flatpak_transfer::ReadOnlySource, String> {
    let plan = Plan::parse(bytes).map_err(|error| error.to_string())?;
    mount_read_only(&plan)
        .map_err(|error| error.to_string())?
        .into_flatpak_source()
}

/// Accept only a freshly granted read-only mount. This does not mount anything
/// and never interprets the plan's locator as an ambient path.
#[cfg(all(test, feature = "native-capture-qualification"))]
pub fn resolve(plan: &Plan, mountpoint: &Path) -> io::Result<ResolvedSource> {
    resolve_with(plan, mountpoint, &Facts::read()?, true)
}
fn resolve_with(
    plan: &Plan,
    mountpoint: &Path,
    facts: &Facts,
    check_kernel: bool,
) -> io::Result<ResolvedSource> {
    resolve_mode_with(plan, mountpoint, facts, check_kernel, AccessMode::ReadOnly)
}
fn resolve_mode_with(
    plan: &Plan,
    mountpoint: &Path,
    facts: &Facts,
    check_kernel: bool,
    mode: AccessMode,
) -> io::Result<ResolvedSource> {
    resolve_mode_with_retained(plan, mountpoint, facts, check_kernel, mode, None)
}
fn resolve_mode_with_retained(
    plan: &Plan,
    mountpoint: &Path,
    facts: &Facts,
    check_kernel: bool,
    mode: AccessMode,
    permit: Option<&crate::setup::home_capture::RootRetainedPermit>,
) -> io::Result<ResolvedSource> {
    plan.validate()?;
    if let Some(permit) = permit {
        if mode != AccessMode::ReadWrite {
            return Err(refusal(
                "Root retained permit cannot enter readonly capture",
            ));
        }
        permit.validate_plan(plan)?;
    }
    let mount = facts
        .mounts
        .iter()
        .rev()
        .find(|mount| mount.point == mountpoint)
        .ok_or_else(|| {
            refusal("The granted source mount is not in the current kernel mount table.")
        })?;
    let device = facts.matching_volume(&plan.home.volume)?;
    let (observed, observed_device) = facts.volume(mount)?;
    if observed != plan.home.volume
        || observed_device != device
        || mount.read_only != (mode == AccessMode::ReadOnly)
    {
        return Err(refusal(
            "The granted source does not match the plan or is writable.",
        ));
    }
    for alias in &facts.mounts {
        if (alias.source == mount.source || alias.number == mount.number)
            && !alias.read_only
            && mode == AccessMode::ReadOnly
        {
            return Err(refusal(
                "Migration source also has a writable mount; unmount it before proceeding.",
            ));
        }
    }
    let mount_root = PinnedDirectory::open_absolute(mountpoint)?;
    if check_kernel {
        kernel_access(&mount_root, mode)?;
    }
    let relative = plan.home.relative.decode().map_err(refusal)?;
    let root = mount
        .root
        .strip_prefix("/")
        .map_err(|_| refusal("Source mount root is invalid."))?;
    let suffix = relative
        .strip_prefix(root)
        .map_err(|_| refusal("The granted source subvolume does not contain the reviewed home."))?;
    let home = mountpoint.join(suffix);
    let pin = PinnedDirectory::open_absolute(&home)?;
    let metadata = cap_primitives::fs::Metadata::from_file(&pin.file)?;
    if metadata.ino() != plan.home.inode || metadata.uid() != plan.home.original_uid {
        return Err(refusal(
            "Source home was replaced or needs an explicit user-ID mapping grant.",
        ));
    }
    if mode == AccessMode::ReadWrite {
        // SAFETY: reads current process credentials only; no ownership changes.
        if plan.home.original_uid != rustix::process::geteuid().as_raw() && permit.is_none() {
            return Err(refusal(
                "Shared writable files require matching original user ownership; an explicit UID mapping service is needed",
            ));
        }
    }
    if let Some(expected) = &plan.home.subvolume {
        let actual = super::super::migration_durability::identity(
            &super::super::migration_durability::readable_directory(&pin.file)?,
        )?;
        if actual != *expected {
            return Err(refusal("Source Btrfs subvolume identity changed."));
        }
    }
    if check_kernel {
        kernel_access(&pin, mode)?;
        let live = super::super::migration_durability::identity(
            &super::super::migration_durability::readable_directory(&pin.file)?,
        )?;
        if !live.binds_filesystem(&plan.home.volume.filesystem_uuid) {
            return Err(refusal(
                "Cannot attest the live source filesystem UUID; this kernel/filesystem needs a supported durable identity service.",
            ));
        }
    }
    pin.validate()?;
    Ok(ResolvedSource {
        home,
        plan: plan.clone(),
        pin,
        mountpoint: mountpoint.to_owned(),
        mount: None,
    })
}
fn kernel_access(directory: &PinnedDirectory, mode: AccessMode) -> io::Result<()> {
    if mode == AccessMode::ReadOnly {
        return kernel_read_only(directory);
    }
    use std::os::fd::AsRawFd;
    let file = super::super::migration_durability::readable_directory(&directory.file)?;
    let mut status = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: live descriptor and correctly sized output storage.
    if unsafe { libc::fstatvfs(file.as_raw_fd(), status.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful syscall initialized all fields.
    let flags = unsafe { status.assume_init() }.f_flag;
    let safe = libc::ST_NODEV | libc::ST_NOSUID | libc::ST_NOEXEC;
    if flags & libc::ST_RDONLY != 0 || flags & safe != safe {
        return Err(refusal(
            "Shared files need a verified writable nodev,nosuid,noexec source mount",
        ));
    }
    Ok(())
}
fn kernel_read_only(directory: &PinnedDirectory) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    let file = super::super::migration_durability::readable_directory(&directory.file)?;
    let mut status = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: descriptor stays live, output holds a complete native statvfs.
    if unsafe { libc::fstatvfs(file.as_raw_fd(), status.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful fstatvfs initialized the complete structure.
    if unsafe { status.assume_init() }.f_flag & libc::ST_RDONLY == 0 {
        return Err(refusal(
            "Kernel source filesystem is writable; no transfer was authorized.",
        ));
    }
    Ok(())
}

trait MountRelease: Send + Sync {
    fn authenticated_readonly(&self) -> bool {
        false
    }
    fn bind_facts(&self, _facts: &mut Facts) -> io::Result<()> {
        Ok(())
    }
    fn release(&self, device: &str) -> io::Result<()>;
}
struct OwnedMountWitness {
    namespace: (u64, u64),
    id: u64,
    number: String,
    root: PathBuf,
    point: PathBuf,
}
impl OwnedMountWitness {
    fn capture(facts: &Facts, point: &Path, namespace: (u64, u64)) -> io::Result<Self> {
        let mut matches = facts.mounts.iter().filter(|mount| mount.point == point);
        let mount = matches
            .next()
            .ok_or_else(|| refusal("Owned source mount is absent"))?;
        if matches.next().is_some() {
            return Err(refusal("Owned source mountpoint is ambiguous"));
        }
        let witness = Self {
            namespace,
            id: mount.id,
            number: mount.number.clone(),
            root: mount.root.clone(),
            point: mount.point.clone(),
        };
        witness.validate(facts, namespace)?;
        Ok(witness)
    }
    fn validate(&self, facts: &Facts, namespace: (u64, u64)) -> io::Result<()> {
        if self.namespace != namespace {
            return Err(refusal("Owned source mount namespace changed"));
        }
        let mut aliases = facts
            .mounts
            .iter()
            .filter(|mount| mount.number == self.number);
        let mount = aliases
            .next()
            .ok_or_else(|| refusal("Owned source mount disappeared"))?;
        if aliases.next().is_some()
            || mount.id != self.id
            || mount.root != self.root
            || mount.point != self.point
            || facts
                .mounts
                .iter()
                .filter(|other| other.point == self.point)
                .count()
                != 1
        {
            return Err(refusal("Owned source mount identity or aliases changed"));
        }
        Ok(())
    }
}
struct NativeMountRelease {
    service: block_devices::udisks::Udisks,
    volume: SourceVolume,
    point: PathBuf,
    namespace: fs::File,
    witness: Option<OwnedMountWitness>,
}
impl MountRelease for NativeMountRelease {
    fn release(&self, device: &str) -> io::Result<()> {
        let held = self.namespace.metadata()?;
        let current_namespace = fs::metadata("/proc/self/ns/mnt")?;
        if (held.dev(), held.ino()) != (current_namespace.dev(), current_namespace.ino()) {
            return Err(refusal("Owned source mount namespace changed"));
        }
        let witness = self
            .witness
            .as_ref()
            .ok_or_else(|| refusal("Owned source mount has no verified initial identity"))?;
        let facts = Facts::read()?;
        witness.validate(&facts, (held.dev(), held.ino()))?;
        if facts.matching_volume(&self.volume)? != device {
            return Err(refusal(
                "Owned retained source device changed before unmount",
            ));
        }
        let mount = facts
            .mounts
            .iter()
            .find(|mount| mount.id == witness.id)
            .ok_or_else(|| refusal("Owned retained source mount disappeared"))?;
        let (volume, current) = facts.volume(mount)?;
        if volume != self.volume || current != device || mount.point != self.point {
            return Err(refusal("Owned retained source mount was replaced"));
        }
        self.service.unmount(device).map_err(io::Error::other)
    }
}
struct MountedVolume {
    service: Box<dyn MountRelease>,
    device: String,
    automatic_release: bool,
}
fn retire_owned_mount(mount: Arc<MountedVolume>) -> io::Result<()> {
    let mut owned = match Arc::try_unwrap(mount) {
        Ok(owned) => owned,
        Err(shared) => {
            retain_failed_mount(shared);
            return Err(refusal("Original source mount still has custody leases"));
        }
    };
    // Explicit retirement never delegates its result to a Drop retry.
    owned.automatic_release = false;
    if let Err(error) = owned.service.release(&owned.device) {
        retain_failed_mount(Arc::new(owned));
        return Err(error);
    }
    Ok(())
}
/// A failed explicit retirement is quarantined, never reported as released.
/// Retain exact service/device custody; do not retry against replaced mounts.
fn retain_failed_mount(mount: Arc<MountedVolume>) {
    static FAILED: std::sync::Mutex<Vec<Arc<MountedVolume>>> = std::sync::Mutex::new(Vec::new());
    match FAILED.lock() {
        Ok(mut held) if held.len() < 64 => held.push(mount),
        _ => std::mem::forget(mount), // Fail closed if the bounded quarantine cannot accept it.
    }
}
impl Drop for MountedVolume {
    fn drop(&mut self) {
        if self.automatic_release && self.service.release(&self.device).is_err() {
            eprintln!("Cannot release migration source mount");
        }
    }
}

/// The existing UDisks service owns elevation and mount placement. Calls are
/// made only for an explicit action; every returned grant is re-attested.
/// Preserve existing authorized daemon placement in source namespaces which
/// have not yet entered the canonical private helper stage. Never change the
/// daemon's strict no-replay options or mount the kernel backend under shared
/// propagation; host target-user lease integration is a separate continuation.
fn mount_root_role_volume(
    volume: &SourceVolume,
    facts: &Facts,
) -> io::Result<(PathBuf, Arc<MountedVolume>)> {
    if rustix::process::geteuid().as_raw() != 0 {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    if facts.covering(Path::new("/run"))?.private {
        return root_readonly::RootReadOnlyMount::mount(volume, facts);
    }
    let device = facts.matching_volume(volume)?;
    if facts.mounts.iter().any(|mount| {
        facts.volume(mount).is_ok_and(|(actual, _)| {
            actual
                .partition_guid
                .eq_ignore_ascii_case(&volume.partition_guid)
        })
    }) {
        return Err(refusal(
            "Source is already mounted; explicitly grant its verified existing mount",
        ));
    }
    let filesystem = match volume.filesystem {
        Filesystem::Ext4 => block_devices::udisks::ReadOnlyFilesystem::Ext4,
        Filesystem::Xfs => block_devices::udisks::ReadOnlyFilesystem::Xfs,
        Filesystem::Btrfs => block_devices::udisks::ReadOnlyFilesystem::Btrfs,
    };
    let service = block_devices::udisks::Udisks::system().map_err(io::Error::other)?;
    let namespace = fs::File::open("/proc/self/ns/mnt")?;
    let metadata = namespace.metadata()?;
    let point = service
        .mount_read_only(&device, filesystem)
        .map_err(io::Error::other)?;
    let witness = Facts::read().ok().and_then(|facts| {
        OwnedMountWitness::capture(&facts, Path::new(&point), (metadata.dev(), metadata.ino())).ok()
    });
    let verified = witness.is_some();
    let owned = Arc::new(MountedVolume {
        service: Box::new(NativeMountRelease {
            service,
            volume: volume.clone(),
            point: PathBuf::from(&point),
            namespace,
            witness,
        }),
        device,
        automatic_release: false,
    });
    if !verified {
        retire_owned_mount(owned)?;
        return Err(refusal(
            "Authenticated readonly source has no initial mount witness",
        ));
    }
    Ok((PathBuf::from(point), owned))
}

/// Authenticated boot/installed-root source roles explicitly select this route.
/// Ordinary target-user continuations retain their authorized UDisks route.
pub fn mount_authenticated_read_only(plan: &Plan) -> io::Result<ResolvedSource> {
    plan.validate()?;
    let (point, owned) = mount_root_role_volume(&plan.home.volume, &Facts::read()?)?;
    let result = (|| {
        let mut facts = Facts::read()?;
        owned.service.bind_facts(&mut facts)?;
        resolve_with(plan, &point, &facts, true)
    })();
    match result {
        Ok(mut source) => {
            source.mount = Some(owned);
            Ok(source)
        }
        Err(error) => {
            retire_owned_mount(owned)?;
            Err(error)
        }
    }
}

pub fn mount_read_only(plan: &Plan) -> io::Result<ResolvedSource> {
    mount_source(plan, AccessMode::ReadOnly)
}
pub fn retain_access(plan: &Plan, mode: AccessMode) -> io::Result<RetainedSource> {
    if mode == AccessMode::ReadWrite && plan.home_policy != HomePolicy::AccessReadWrite {
        return Err(refusal(
            "Shared writable access requires explicit retained-files policy",
        ));
    }
    if mode == AccessMode::ReadOnly && plan.home_policy == HomePolicy::AccessReadWrite {
        return Err(refusal(
            "A shared-edit plan cannot silently become a read-only outcome",
        ));
    }
    let source = mount_source(plan, mode)?;
    Ok(RetainedSource {
        source,
        mode,
        root_permit: None,
    })
}
pub fn retain_scoped_source(
    plan: &Plan,
    permit: Arc<crate::setup::home_capture::RootRetainedPermit>,
) -> io::Result<RetainedSource> {
    permit.validate_plan(plan)?;
    let source = mount_source_with_retained(plan, AccessMode::ReadWrite, Some(&permit))?;
    Ok(RetainedSource {
        source,
        mode: AccessMode::ReadWrite,
        root_permit: Some(permit),
    })
}
fn mount_source(plan: &Plan, mode: AccessMode) -> io::Result<ResolvedSource> {
    mount_source_with_retained(plan, mode, None)
}
fn mount_source_with_retained(
    plan: &Plan,
    mode: AccessMode,
    permit: Option<&crate::setup::home_capture::RootRetainedPermit>,
) -> io::Result<ResolvedSource> {
    plan.validate()?;
    if let Some(permit) = permit {
        permit.validate_plan(plan)?;
    }
    let facts = Facts::read()?;
    let device = facts.matching_volume(&plan.home.volume)?;
    if facts.mounts.iter().any(|mount| {
        facts.volume(mount).is_ok_and(|(volume, _)| {
            volume
                .partition_guid
                .eq_ignore_ascii_case(&plan.home.volume.partition_guid)
        })
    }) {
        return Err(refusal(
            "The source is already mounted; explicitly grant access to its verified existing mount.",
        ));
    }
    let filesystem = match plan.home.volume.filesystem {
        Filesystem::Ext4 => block_devices::udisks::ReadOnlyFilesystem::Ext4,
        Filesystem::Xfs => block_devices::udisks::ReadOnlyFilesystem::Xfs,
        Filesystem::Btrfs => block_devices::udisks::ReadOnlyFilesystem::Btrfs,
    };
    let service = block_devices::udisks::Udisks::system()
        .map_err(|error| io::Error::other(error.to_string()))?;
    let namespace = fs::File::open("/proc/self/ns/mnt")?;
    let namespace_metadata = namespace.metadata()?;
    let point = match mode {
        AccessMode::ReadOnly => service.mount_read_only(&device, filesystem),
        AccessMode::ReadWrite => service.mount_shared_files(&device, filesystem),
    }
    .map_err(io::Error::other)?;
    let mounted_facts = Facts::read();
    let witness = mounted_facts.as_ref().ok().and_then(|facts| {
        OwnedMountWitness::capture(
            facts,
            Path::new(&point),
            (namespace_metadata.dev(), namespace_metadata.ino()),
        )
        .ok()
    });
    let initial_identity_verified = witness.is_some();
    let owned = MountedVolume {
        service: Box::new(NativeMountRelease {
            service,
            volume: plan.home.volume.clone(),
            point: PathBuf::from(&point),
            namespace,
            witness,
        }),
        device,
        automatic_release: true,
    };
    let resolved = mounted_facts.and_then(|facts| {
        if !initial_identity_verified {
            return Err(refusal("Owned source initial mount identity is unverified"));
        }
        resolve_mode_with_retained(plan, Path::new(&point), &facts, true, mode, permit)
    });
    match resolved {
        Ok(mut source) => {
            source.mount = Some(Arc::new(owned));
            Ok(source)
        }
        Err(error) => {
            retire_owned_mount(Arc::new(owned))?;
            Err(error)
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
mod fixture;
#[cfg(test)]
mod tests;

#[cfg(any(test, feature = "test-support"))]
pub fn capture_fixture(home: &Path, operation: &str, categories: Categories) -> io::Result<Plan> {
    let base = home
        .parent()
        .ok_or_else(|| refusal("Synthetic source requires a parent"))?;
    capture_with(home, operation, categories, &fixture::facts(base, false))
}

/// Resolve each exact source volume again and reject a shared physical target.
/// Source request bytes are never substituted for kernel block identity.
pub fn validate_offline_source_disks(plans: &[Vec<u8>], target_number: u64) -> io::Result<()> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    if plans.is_empty()
        || plans.len() > 128
        || plans
            .iter()
            .try_fold(0usize, |total, bytes| total.checked_add(bytes.len()))
            .is_none_or(|total| total > 9 * 1024 * 1024)
    {
        return Err(refusal(
            "Pending export source disk metadata exceeds its bound",
        ));
    }
    let facts = Facts::read()?;
    for bytes in plans {
        let geometry = super::SourceGeometry::parse(bytes)?;
        let source_volume = geometry.volume();
        facts.matching_volume(source_volume)?;
        let mut disks = Vec::new();
        for disk in &facts.devices {
            for partition in &disk.children {
                if partition
                    .partuuid
                    .as_ref()
                    .is_some_and(|guid| guid.eq_ignore_ascii_case(&source_volume.partition_guid))
                    && volume(disk, partition)? == *source_volume
                {
                    disks.push(disk);
                }
            }
        }
        let [disk] = disks.as_slice() else {
            return Err(refusal("Pending source parent disk is ambiguous"));
        };
        let metadata = fs::symlink_metadata(&disk.name)?;
        if !metadata.file_type().is_block_device() || metadata.rdev() == target_number {
            return Err(refusal(
                "Installer media overlaps an original migration source disk",
            ));
        }
        let actual = format!(
            "{}:{}",
            libc::major(metadata.rdev()),
            libc::minor(metadata.rdev())
        );
        if disk.maj_min.as_deref() != Some(actual.as_str()) {
            return Err(refusal(
                "Original source disk changed during media admission",
            ));
        }
    }
    Ok(())
}

/// Broker-only derivation of a fixed record namespace on the selected Home
/// volume. No second source-volume locator is returned or accepted.
pub fn original_record_namespace(
    plan: &Plan,
    home: &Path,
    records: &Path,
    held: &Arc<PinnedDirectory>,
) -> io::Result<PathBuf> {
    plan.validate()?;
    let actual = capture(home, &plan.operation_id, plan.categories)?;
    if actual.home != plan.home {
        return Err(refusal("Original Home selection changed"));
    }
    if !records.ends_with("var/lib/tilo-imager/home-capture/records") {
        return Err(refusal(
            "Original record namespace is not the fixed broker namespace",
        ));
    }
    let current = PinnedDirectory::open_absolute(records)?;
    if !super::super::nofollow::same_directory_identity(&current.identity, &held.identity) {
        return Err(refusal("Original record namespace was replaced"));
    }
    let facts = Facts::read()?;
    let mount = facts.covering(records)?;
    let (volume, _) = facts.volume(mount)?;
    if volume != plan.home.volume {
        return Err(unsupported_source(
            "Original Home and protected records are on different source volumes",
        ));
    }
    let relative = mount
        .root
        .strip_prefix("/")
        .map_err(|_| refusal("Invalid source mount root"))?
        .join(
            records
                .strip_prefix(&mount.point)
                .map_err(|_| refusal("Record path escaped source mount"))?,
        );
    super::relative(&relative)?;
    current.validate_destination()?;
    held.validate_destination()?;
    Ok(relative)
}

enum OriginalFile {
    Record,
    Inventory,
    MountedRoots,
    RetainedCaller,
}

impl ResolvedSource {
    /// Authenticate the completed single-device resize through the exact granted
    /// readonly source and held kernel block device, never a caller pathname.
    pub fn verify_completed_btrfs_resize(&self, bytes: u64) -> io::Result<()> {
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::{FileExt, FileTypeExt, OpenOptionsExt};
        let volume = &self.plan.home.volume;
        let expected = volume
            .last_lba
            .checked_sub(volume.first_lba)
            .and_then(|n| n.checked_add(1))
            .and_then(|n| n.checked_mul(volume.sector_bytes))
            .ok_or_else(|| refusal("Completed Btrfs extent overflow"))?;
        if volume.filesystem != Filesystem::Btrfs || bytes != expected || bytes == 0 {
            return Err(refusal("Completed Btrfs reviewed extent differs"));
        }
        self.pin.validate()?;
        kernel_read_only(&self.pin)?;
        let device = Facts::read()?.matching_volume(volume)?;
        let held = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&device)?;
        let identity = held.metadata()?;
        if !identity.file_type().is_block_device() {
            return Err(refusal("Completed Btrfs source is not a block device"));
        }
        let mut kernel_bytes = 0_u64;
        // SAFETY: BLKGETSIZE64 writes a u64 into a valid output on the held block FD.
        if unsafe { libc::ioctl(held.as_raw_fd(), 0x80081272, &mut kernel_bytes) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let mut superblock = [0_u8; 4096];
        held.read_exact_at(&mut superblock, 64 * 1024)?;
        verify_completed_superblock(&superblock, &volume.filesystem_uuid, bytes)?;
        let named = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&device)?
            .metadata()?;
        if kernel_bytes != bytes
            || named.rdev() != identity.rdev()
            || named.dev() != identity.dev()
            || named.ino() != identity.ino()
            || Facts::read()?.matching_volume(volume)? != device
        {
            return Err(refusal("Completed Btrfs device readback changed"));
        }
        self.pin.validate()?;
        kernel_read_only(&self.pin)
    }
}

fn verify_completed_superblock(block: &[u8; 4096], fsid: &str, bytes: u64) -> io::Result<()> {
    let number = |offset: usize| u64::from_le_bytes(block[offset..offset + 8].try_into().unwrap());
    let uuid = uuid::Uuid::from_bytes(block[32..48].try_into().unwrap());
    if &block[64..72] != b"_BHRfS_M"
        || !uuid.to_string().eq_ignore_ascii_case(fsid)
        || number(136) != 1
        || number(201) != 1
        || number(112) != bytes
        || number(209) != bytes
    {
        return Err(refusal(
            "Completed Btrfs filesystem size or identity differs",
        ));
    }
    Ok(())
}

impl ResolvedSource {
    /// Clone only this exact held Home after kernel readonly attestation.
    /// Its mount lease stays owned by the original SourceReadContext.
    pub fn clone_original_home(&self) -> io::Result<std::fs::File> {
        self.pin.validate()?;
        kernel_read_only(&self.pin)?;
        let file = self.pin.file.try_clone()?;
        self.pin.validate()?;
        Ok(file)
    }
    /// Read a broker record only inside this already reattested read-only
    /// source volume. Namespace is a bounded same-volume selection hint;
    /// the protected record must still prove its Home/operation binding.
    pub fn open_original_record(&self, namespace: &Path) -> io::Result<std::fs::File> {
        self.open_original_file(namespace, OriginalFile::Record)
    }
    pub fn open_retained_record_readonly(&self, namespace: &Path) -> io::Result<std::fs::File> {
        if rustix::process::geteuid().as_raw() != 0 {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        self.open_control_file(
            namespace,
            OriginalFile::RetainedCaller,
            AccessMode::ReadOnly,
            None,
        )
    }
    pub fn open_original_inventory(&self, namespace: &Path) -> io::Result<std::fs::File> {
        self.open_original_file(namespace, OriginalFile::Inventory)
    }
    pub fn open_original_mounted_record(&self, namespace: &Path) -> io::Result<std::fs::File> {
        self.open_original_file(namespace, OriginalFile::MountedRoots)
    }
    fn open_original_file(
        &self,
        namespace: &Path,
        kind: OriginalFile,
    ) -> io::Result<std::fs::File> {
        self.open_control_file(namespace, kind, AccessMode::ReadOnly, None)
    }
    fn open_control_directory(
        &self,
        namespace: &Path,
        mode: AccessMode,
        permit: Option<&crate::setup::home_capture::RootRetainedPermit>,
    ) -> io::Result<Arc<PinnedDirectory>> {
        super::relative(namespace)?;
        if !namespace.ends_with("tilo-imager/home-capture/records") {
            return Err(refusal("Expected fixed original source record namespace"));
        }
        self.pin.validate()?;
        let facts = self.facts()?;
        let renewed =
            resolve_mode_with_retained(&self.plan, &self.mountpoint, &facts, true, mode, permit)?;
        if !super::super::nofollow::same_directory_identity(
            &renewed.pin.identity,
            &self.pin.identity,
        ) {
            return Err(refusal("Original source grant changed"));
        }
        let mount = facts
            .mounts
            .iter()
            .rev()
            .find(|mount| mount.point == self.mountpoint)
            .ok_or_else(|| refusal("Original source mount disappeared"))?;
        let root_relative = mount
            .root
            .strip_prefix("/")
            .map_err(|_| refusal("Invalid original mount root"))?;
        let suffix = namespace.strip_prefix(root_relative).map_err(|_| {
            unsupported_source("Protected records are not visible in this source mount")
        })?;
        let mut directory = PinnedDirectory::open_absolute(&self.mountpoint)?;
        let mount_id = fs_native::snapshot_lease::mount_id(&directory.file)?;
        for component in suffix.components() {
            let std::path::Component::Normal(name) = component else {
                return Err(refusal("Invalid record namespace component"));
            };
            let metadata =
                super::super::nofollow::metadata_nofollow(&directory.file, Path::new(name))?;
            directory = directory.open_child(name, &metadata)?;
            if fs_native::snapshot_lease::mount_id(&directory.file)? != mount_id {
                return Err(refusal("Protected record namespace crosses a source mount"));
            }
            let storage = tilo_files::native::atomic::StorageDirectory::from_file(
                super::super::migration_durability::readable_directory(&directory.file)?,
            )?;
            storage.validate_namespace_control()?;
        }
        let storage = tilo_files::native::atomic::StorageDirectory::from_file(
            super::super::migration_durability::readable_directory(&directory.file)?,
        )?;
        storage.validate_privacy(tilo_files::native::atomic::Privacy::OwnerOnly)?;
        directory.validate()?;
        self.pin.validate()?;
        Ok(directory)
    }
    fn open_control_file(
        &self,
        namespace: &Path,
        kind: OriginalFile,
        mode: AccessMode,
        permit: Option<&crate::setup::home_capture::RootRetainedPermit>,
    ) -> io::Result<std::fs::File> {
        let directory = self.open_control_directory(namespace, mode, permit)?;
        let mount_id = fs_native::snapshot_lease::mount_id(&directory.file)?;
        let storage = tilo_files::native::atomic::StorageDirectory::from_file(
            super::super::migration_durability::readable_directory(&directory.file)?,
        )?;
        let file = storage.open_read(
            std::ffi::OsStr::new(&match kind {
                OriginalFile::Inventory => {
                    format!("{}.flatpak-inventory.json", self.plan.operation_id)
                }
                OriginalFile::Record => format!("{}.json", self.plan.operation_id),
                OriginalFile::RetainedCaller => {
                    format!("{}.retained-caller.json", self.plan.operation_id)
                }
                OriginalFile::MountedRoots => {
                    format!("{}.mounted-roots.json", self.plan.operation_id)
                }
            }),
            tilo_files::native::atomic::Privacy::OwnerOnly,
        )?;
        if std::os::unix::fs::MetadataExt::uid(&file.metadata()?) != 0
            || fs_native::snapshot_lease::mount_id(&file)? != mount_id
        {
            return Err(refusal(
                "Original record is not root-controlled on this source mount",
            ));
        }
        directory.validate()?;
        self.pin.validate()?;
        Ok(file)
    }
    pub fn original_plan(&self) -> &Plan {
        &self.plan
    }
}

#[cfg(test)]
mod completed_btrfs_resize_tests {
    use super::*;
    #[test]
    fn completed_resize_superblock_refuses_different_device_geometry() {
        let fsid = uuid::Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap();
        let bytes = 64_u64 << 30;
        let mut block = [0_u8; 4096];
        block[32..48].copy_from_slice(fsid.as_bytes());
        block[64..72].copy_from_slice(b"_BHRfS_M");
        for (offset, value) in [(112, bytes), (136, 1), (201, 1), (209, bytes)] {
            block[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
        }
        verify_completed_superblock(&block, &fsid.to_string(), bytes).unwrap();
        for offset in [112, 136, 201, 209] {
            let mut changed = block;
            changed[offset..offset + 8].copy_from_slice(&2_u64.to_le_bytes());
            assert!(verify_completed_superblock(&changed, &fsid.to_string(), bytes).is_err());
        }
        assert!(
            verify_completed_superblock(&block, "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee", bytes)
                .is_err()
        );
        assert!(verify_completed_superblock(&block, &fsid.to_string(), bytes / 2).is_err());
    }
}
