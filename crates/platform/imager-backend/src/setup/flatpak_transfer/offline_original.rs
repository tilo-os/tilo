//! Protected original selection -> held source projections -> canonical exporter.
//! No serialized claim grants access outside the root-admitted worker capsule.
#[cfg(any(feature = "capture-worker", test))]
use super::ReadOnlySource;
use super::portable::NativeOfflineRunner;
#[cfg(any(feature = "capture-worker", test))]
use super::portable::PortableFlatpakSource;
use super::{HandoffInventory, SourceScopedRunner};
use crate::setup::home_capture::SourceReadContext;
#[cfg(any(feature = "capture-worker", test))]
use crate::setup::migration::PinnedDirectory;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
#[cfg(any(feature = "capture-worker", test))]
use std::path::{Path, PathBuf};
use std::{
    fs::File,
    io::{self, Read, Seek, Write},
    os::fd::{AsRawFd, FromRawFd},
    os::unix::fs::MetadataExt,
    sync::Arc,
};

#[cfg(any(feature = "capture-worker", test))]
pub const PROJECTION_ROOT: &str = "/run/tilo/migration/flatpak-source";
pub const INVENTORY_PATH: &str = "/run/tilo/migration/flatpak-inventory.json";
const MAX_ROOTS: usize = 64;
const INVENTORY_LIMIT: u64 = 8 * 1024 * 1024;
const SEALS: libc::c_int =
    libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;
fn refused(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OriginalFlatpakProjection {
    pub index: u32,
    pub device: u64,
    pub inode: u64,
    pub owner: u32,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OriginalFlatpakClaim {
    schema: u32,
    operation: String,
    original_uid: u32,
    inventory_sha256: [u8; 32],
    inventory_len: u64,
    projections: Vec<OriginalFlatpakProjection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    selected_data_ids: Option<Vec<String>>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OriginalFlatpakClaimWire {
    schema: u32,
    operation: String,
    original_uid: u32,
    inventory_sha256: [u8; 32],
    inventory_len: u64,
    projections: Vec<OriginalFlatpakProjection>,
    #[serde(default)]
    selected_data_ids: Option<Vec<String>>,
}
impl<'de> Deserialize<'de> for OriginalFlatpakClaim {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        OriginalFlatpakClaimWire::deserialize(deserializer)?
            .try_into()
            .map_err(serde::de::Error::custom)
    }
}
impl TryFrom<OriginalFlatpakClaimWire> for OriginalFlatpakClaim {
    type Error = io::Error;

    fn try_from(wire: OriginalFlatpakClaimWire) -> io::Result<Self> {
        let claim = Self {
            schema: wire.schema,
            operation: wire.operation,
            original_uid: wire.original_uid,
            inventory_sha256: wire.inventory_sha256,
            inventory_len: wire.inventory_len,
            projections: wire.projections,
            selected_data_ids: wire.selected_data_ids,
        };
        claim.validate_shape(&claim.operation)?;
        Ok(claim)
    }
}
/// Native protected-record authority, not a serialized inventory or pathname.
/// The root source reader creates this only from its exact Store-owned child.
pub struct OriginalInventoryGrant {
    inventory: HandoffInventory,
    bytes: Vec<u8>,
    len: u64,
    digest: [u8; 32],
    plan_digest: [u8; 32],
    source: Option<(File, InventoryStamp)>,
}
#[derive(PartialEq, Eq)]
struct InventoryStamp {
    device: u64,
    inode: u64,
    owner: u32,
    mode: u32,
    links: u64,
    bytes: u64,
    mtime: i64,
    mtime_ns: i64,
    ctime: i64,
    ctime_ns: i64,
}
fn inventory_stamp(file: &File) -> io::Result<InventoryStamp> {
    let m = file.metadata()?;
    Ok(InventoryStamp {
        device: m.dev(),
        inode: m.ino(),
        owner: m.uid(),
        mode: m.mode(),
        links: m.nlink(),
        bytes: m.len(),
        mtime: m.mtime(),
        mtime_ns: m.mtime_nsec(),
        ctime: m.ctime(),
        ctime_ns: m.ctime_nsec(),
    })
}
fn read_protected_inventory(
    mut file: File,
    len: u64,
    digest: [u8; 32],
) -> io::Result<(Vec<u8>, (File, InventoryStamp))> {
    if len == 0 || len > INVENTORY_LIMIT || digest == [0; 32] {
        return Err(refused("Protected inventory bound is invalid"));
    }
    let before = inventory_stamp(&file)?;
    let facts = file.metadata()?;
    if !facts.is_file()
        || before.owner != 0
        || before.mode & 0o7777 != 0o600
        || before.links != 1
        || before.bytes != len
    {
        return Err(refused(
            "Protected inventory is not exact root-private custody",
        ));
    }
    check_inventory_readonly(&file)?;
    file.rewind()?;
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(INVENTORY_LIMIT + 1)
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).map_err(io::Error::other)? != len
        || <[u8; 32]>::from(Sha256::digest(&bytes)) != digest
        || inventory_stamp(&file)? != before
    {
        return Err(refused("Protected inventory changed during admission"));
    }
    Ok((bytes, (file, before)))
}
impl OriginalInventoryGrant {
    pub fn review(original: &SourceReadContext) -> io::Result<Option<Self>> {
        original.revalidate()?;
        let plan = original.plan();
        let protected = original.original_flatpak_inventory()?;
        let marker = crate::setup::migration_handoff::OriginalInventoryBinding::parse(plan)
            .map_err(io::Error::other)?;
        if !plan.categories.flatpak {
            if protected.is_some() || plan.flatpak_inventory.is_some() {
                return Err(refused("Original inventory lacks exact selection consent"));
            }
            return Ok(None);
        }
        let (bytes, source) = if let Some((file, len, digest)) = protected {
            let binding = marker.as_ref().ok_or_else(|| {
                refused("Protected inventory lacks exact normalized Plan binding")
            })?;
            if binding.len != len || binding.sha256 != digest {
                return Err(refused(
                    "Protected inventory differs from source Plan binding",
                ));
            }
            let (bytes, source) = read_protected_inventory(file, len, digest)?;
            (bytes, Some(source))
        } else if marker.is_some() {
            return Err(refused("Protected inventory named by Plan is unavailable"));
        } else if let Some(raw) = &plan.flatpak_inventory {
            (serde_json::to_vec(raw)?, None)
        } else {
            return Err(refused(
                "Selected original applications lack protected inventory custody",
            ));
        };
        if bytes.is_empty()
            || bytes.len() > usize::try_from(INVENTORY_LIMIT).map_err(io::Error::other)?
        {
            return Err(refused("Original inventory exceeds limit"));
        }
        if let Some(binding) = &marker {
            binding.validate_bytes(&bytes).map_err(io::Error::other)?;
        }
        let inventory = HandoffInventory::parse(&bytes).map_err(io::Error::other)?;
        inventory
            .validate_operation(&plan.operation_id)
            .map_err(io::Error::other)?;
        let grant = Self {
            inventory,
            digest: Sha256::digest(&bytes).into(),
            len: u64::try_from(bytes.len()).map_err(io::Error::other)?,
            bytes,
            plan_digest: Sha256::digest(plan.bytes()?).into(),
            source,
        };
        if plan.categories.home {
            grant.matches_plan(plan)?;
        } else {
            grant.matches_applications_plan(plan)?;
        }
        original.revalidate()?;
        Ok(Some(grant))
    }
    pub fn inventory(&self) -> &HandoffInventory {
        &self.inventory
    }
    pub fn matches_plan(&self, plan: &crate::setup::migration_handoff::Plan) -> io::Result<()> {
        if !plan.categories.home
            || !plan.categories.flatpak
            || plan.home_policy != crate::setup::migration_handoff::HomePolicy::Copy
            || <[u8; 32]>::from(Sha256::digest(plan.bytes()?)) != self.plan_digest
        {
            return Err(refused(
                "Protected inventory differs from original Home operation",
            ));
        }
        self.matches_authenticated_plan(plan)
    }
    pub fn matches_applications_plan(
        &self,
        plan: &crate::setup::migration_handoff::Plan,
    ) -> io::Result<()> {
        if plan.categories.home
            || !plan.categories.flatpak
            || plan.home_policy != crate::setup::migration_handoff::HomePolicy::Copy
            || self.selected_app_ids().is_empty()
            || <[u8; 32]>::from(Sha256::digest(plan.bytes()?)) != self.plan_digest
        {
            return Err(refused(
                "Protected inventory differs from applications-only operation",
            ));
        }
        self.matches_authenticated_plan(plan)
    }
    fn selected_app_ids(&self) -> Vec<String> {
        self.inventory
            .selected()
            .iter()
            .filter_map(|item| item.app_id())
            .map(str::to_owned)
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect()
    }
    fn matches_authenticated_plan(
        &self,
        plan: &crate::setup::migration_handoff::Plan,
    ) -> io::Result<()> {
        if let Some(binding) =
            crate::setup::migration_handoff::OriginalInventoryBinding::parse(plan)
                .map_err(io::Error::other)?
        {
            if self.source.is_none() || binding.len != self.len() || binding.sha256 != self.digest {
                return Err(refused(
                    "Protected inventory generation differs from normalized Plan",
                ));
            }
        } else if self.source.is_some() {
            return Err(refused("Protected inventory Plan binding was removed"));
        }
        self.inventory
            .validate_operation(&plan.operation_id)
            .map_err(io::Error::other)?;
        self.validate()
    }
    fn validate(&self) -> io::Result<()> {
        if let Some((file, stamp)) = &self.source {
            check_inventory_readonly(file)?;
            if inventory_stamp(file)? != *stamp {
                return Err(refused("Held protected inventory changed"));
            }
        }
        Ok(())
    }
    fn len(&self) -> u64 {
        self.len
    }
    pub fn digest(&self) -> [u8; 32] {
        self.digest
    }
}
fn check_inventory_readonly(file: &File) -> io::Result<()> {
    let mut status = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: retained file descriptor and writable statvfs output storage.
    if unsafe { libc::fstatvfs(file.as_raw_fd(), status.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful fstatvfs initialized the complete structure.
    if unsafe { status.assume_init() }.f_flag & libc::ST_RDONLY == 0 {
        return Err(refused("Protected inventory is not kernel read-only"));
    }
    Ok(())
}
pub struct OriginalFlatpakSources {
    claim: OriginalFlatpakClaim,
    inventory: HandoffInventory,
    original_inventory: OriginalInventoryGrant,
    inventory_file: File,
    runner: SourceScopedRunner<NativeOfflineRunner>,
}
impl OriginalFlatpakSources {
    /// Close immutable inventory and projection descriptors before native mount
    /// retirement; root callers combine these with Home's retained resolver.
    pub fn into_retirement_sources(
        self,
    ) -> io::Result<Vec<Arc<crate::setup::migration_handoff::ResolvedSource>>> {
        let Self {
            claim,
            inventory,
            original_inventory,
            inventory_file,
            runner,
        } = self;
        drop((claim, inventory, original_inventory, inventory_file));
        runner.into_original_sources()
    }
    /// Only the verified root-owned original record can select source roots.
    pub fn review(original: &SourceReadContext) -> io::Result<Option<Self>> {
        // SAFETY: read-only native effective credential query.
        if rustix::process::geteuid().as_raw() != 0 {
            return Err(refused(
                "Original application admission requires root controller",
            ));
        }
        original.revalidate()?;
        let plan = original.plan();
        let Some(original_inventory) = OriginalInventoryGrant::review(original)? else {
            return Ok(None);
        };
        let inventory = original_inventory.inventory().clone();
        if inventory.selected().is_empty() {
            return Ok(None);
        }
        let inventory_len = original_inventory.len();
        let inventory_sha256 = original_inventory.digest();
        let inventory_file = sealed_inventory(&original_inventory.bytes)?;
        let runner = original.original_flatpak_runner(&original_inventory)?;
        let files = runner
            .original_files(&inventory)
            .map_err(io::Error::other)?;
        let mut projections = Vec::new();
        for (index, (_installation, file)) in files.into_iter().enumerate() {
            let metadata = file.metadata()?;
            projections.push(OriginalFlatpakProjection {
                index: u32::try_from(index).map_err(io::Error::other)?,
                device: metadata.dev(),
                inode: metadata.ino(),
                owner: metadata.uid(),
            });
        }
        let claim = OriginalFlatpakClaim {
            schema: 1,
            operation: plan.operation_id.clone(),
            original_uid: original.original().uid(),
            inventory_sha256,
            inventory_len,
            projections,
            selected_data_ids: if plan.categories.home {
                None
            } else {
                original_inventory.matches_applications_plan(plan)?;
                Some(original_inventory.selected_app_ids())
            },
        };
        claim.validate_shape(&plan.operation_id)?;
        original.revalidate()?;
        Ok(Some(Self {
            claim,
            inventory,
            original_inventory,
            inventory_file,
            runner,
        }))
    }
    pub fn claim(&self) -> &OriginalFlatpakClaim {
        &self.claim
    }
    pub fn inventory_file(&self) -> io::Result<File> {
        self.inventory_file.try_clone()
    }
    /// Lease ownership remains with this source set and the root read admission
    /// until canonical child quiescence; returned descriptors are not credentials.
    pub fn source_plans(&self) -> io::Result<Vec<Vec<u8>>> {
        self.files()?;
        Ok(self
            .inventory
            .sources()
            .map(|(_, bytes)| bytes.to_vec())
            .collect())
    }
    pub fn files(&self) -> io::Result<Vec<File>> {
        self.original_inventory.validate()?;
        let files = self
            .runner
            .original_lease_files(&self.inventory)
            .map_err(io::Error::other)?;
        let mut retained = Vec::new();
        if files.len() != self.claim.projections.len() {
            return Err(refused("Original projection count changed"));
        }
        for ((_scope, file), expected) in files.into_iter().zip(&self.claim.projections) {
            let facts = file.metadata()?;
            if (facts.dev(), facts.ino(), facts.uid())
                != (expected.device, expected.inode, expected.owner)
            {
                return Err(refused("Original application projection changed"));
            }
            retained.push(file);
        }
        Ok(retained)
    }
}
impl OriginalFlatpakClaim {
    pub fn selected_data_ids(&self) -> Option<&[String]> {
        self.selected_data_ids.as_deref()
    }
    pub fn original_uid(&self) -> u32 {
        self.original_uid
    }
    pub fn inventory_sha256(&self) -> [u8; 32] {
        self.inventory_sha256
    }
    pub fn inventory_len(&self) -> u64 {
        self.inventory_len
    }
    pub fn projections(&self) -> &[OriginalFlatpakProjection] {
        &self.projections
    }

    pub fn validate_shape(&self, operation: &str) -> io::Result<()> {
        let canonical = uuid::Uuid::parse_str(operation).map_err(io::Error::other)?;
        if canonical.to_string() != operation {
            return Err(refused("Original application operation is not canonical"));
        }
        if self.schema != 1
            || self.operation != operation
            || self.original_uid == 0
            || self.original_uid == u32::MAX
            || self.projections.is_empty()
            || self.projections.len() > MAX_ROOTS
            || self.inventory_sha256 == [0; 32]
            || self.inventory_len == 0
            || self.inventory_len > INVENTORY_LIMIT
        {
            return Err(refused("Original application claim shape differs"));
        }
        if let Some(ids) = &self.selected_data_ids {
            if ids.is_empty()
                || ids.len() > 4096
                || ids.windows(2).any(|pair| pair[0] >= pair[1])
                || ids.iter().any(|id| {
                    id.is_empty()
                        || id.len() > 255
                        || id.starts_with('.')
                        || id.ends_with('.')
                        || !id
                            .bytes()
                            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
                })
            {
                return Err(refused("Original selected app-data IDs are not closed"));
            }
        }
        for (index, projection) in self.projections.iter().enumerate() {
            if usize::try_from(projection.index).ok() != Some(index)
                || projection.inode == 0
                || projection.owner == u32::MAX
            {
                return Err(refused("Original application projection shape differs"));
            }
        }
        Ok(())
    }
    /// Call only after the canonical OriginalWrite request authenticates this
    /// claim in its admitted digest. Fixed capsule mounts supply authority; a
    /// password-sealed declaration or claim bytes alone never supplies it.
    #[cfg(any(feature = "capture-worker", test))]
    pub fn review_reader(
        &self,
        operation: &str,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> io::Result<Arc<PortableFlatpakSource>> {
        if self.selected_data_ids.is_some() {
            return Err(refused(
                "Full-Home reader cannot accept selected app-data authority",
            ));
        }
        self.review_material_reader(operation, false, cancel)
    }
    #[cfg(feature = "capture-worker")]
    pub fn review_applications_reader(
        &self,
        operation: &str,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> io::Result<Arc<PortableFlatpakSource>> {
        if self.selected_data_ids.is_none() {
            return Err(refused(
                "Applications-only reader lacks selected app-data authority",
            ));
        }
        self.review_material_reader(operation, true, cancel)
    }
    #[cfg(any(feature = "capture-worker", test))]
    fn review_material_reader(
        &self,
        operation: &str,
        include_state: bool,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> io::Result<Arc<PortableFlatpakSource>> {
        self.validate_shape(operation)?;
        crate::setup::migration::check_cancel(cancel)?;
        // SAFETY: read-only current credentials. Root worker fallback forbidden.
        if rustix::process::geteuid().as_raw() != self.original_uid {
            return Err(refused("Original application reader credentials differ"));
        }
        let inventory = self.read_inventory()?;
        inventory
            .validate_operation(operation)
            .map_err(io::Error::other)?;
        if inventory.selected().is_empty() || inventory.sources().count() != self.projections.len()
        {
            return Err(refused(
                "Original inventory selection or source count differs",
            ));
        }
        if include_state {
            let ids: Vec<String> = inventory
                .selected()
                .iter()
                .filter_map(|item| item.app_id())
                .map(str::to_owned)
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect();
            if self.selected_data_ids.as_ref() != Some(&ids) {
                return Err(refused(
                    "Selected app-data IDs differ from sealed inventory",
                ));
            }
        }
        let home = Path::new(launcher::owned_service::CAPTURE_SOURCE_PATH);
        let mut index = 0usize;
        let runner = inventory
            .resolve(home, NativeOfflineRunner, |_plan| {
                let expected = self
                    .projections
                    .get(index)
                    .ok_or("Original projection missing")?;
                index = index.checked_add(1).ok_or("Original projection overflow")?;
                let path = projection_path(expected.index);
                let pin =
                    PinnedDirectory::open_absolute(&path).map_err(|error| error.to_string())?;
                let facts = pin.file.metadata().map_err(|error| error.to_string())?;
                if (facts.dev(), facts.ino(), facts.uid())
                    != (expected.device, expected.inode, expected.owner)
                {
                    return Err("Original capsule source inode differs".into());
                }
                // Kernel readonly and retained fixed capsule identity are rechecked
                // by the constructor and canonical resolve/closed review.
                ReadOnlySource::from_resolved(&path, pin, Arc::new(()))
            })
            .map_err(io::Error::other)?;
        if index != self.projections.len() {
            return Err(refused("Original projection set incomplete"));
        }
        if include_state {
            PortableFlatpakSource::review_closed_original_selected(
                &runner, &inventory, home, cancel,
            )
        } else {
            PortableFlatpakSource::review_closed_original(&runner, &inventory, home, cancel)
        }
        .map_err(io::Error::other)
    }
}
#[cfg(any(feature = "capture-worker", test))]
pub fn projection_path(index: u32) -> PathBuf {
    Path::new(PROJECTION_ROOT).join(index.to_string())
}

fn sealed_inventory(bytes: &[u8]) -> io::Result<File> {
    // SAFETY: fixed native name, no borrowed pointer retained by the kernel.
    let raw = unsafe {
        libc::memfd_create(
            c"tilo-original-flatpak-inventory".as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful memfd_create returned a new owned descriptor.
    let mut file = unsafe { File::from_raw_fd(raw) };
    file.write_all(bytes)?;
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(std::fs::Permissions::from_mode(0o444))?;
    file.rewind()?;
    // SAFETY: live held descriptor and a closed native seal mask.
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_ADD_SEALS, SEALS) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let readonly = File::open(format!("/proc/self/fd/{}", file.as_raw_fd()))?;
    let a = file.metadata()?;
    let b = readonly.metadata()?;
    if (a.dev(), a.ino()) != (b.dev(), b.ino()) {
        return Err(refused("Sealed inventory descriptor changed"));
    }
    Ok(readonly)
}
impl OriginalFlatpakClaim {
    #[cfg(any(feature = "capture-worker", test))]
    fn read_inventory(&self) -> io::Result<HandoffInventory> {
        use std::os::unix::fs::OpenOptionsExt;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(INVENTORY_PATH)?;
        let facts = file.metadata()?;
        // SAFETY: querying immutable seals of a held descriptor.
        let seals = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GET_SEALS) };
        if !facts.is_file()
            || facts.uid() != 0
            || facts.len() != self.inventory_len
            || seals < 0
            || seals & SEALS != SEALS
        {
            return Err(refused(
                "Original inventory is not the root-sealed exact projection",
            ));
        }
        let mut bytes = Vec::new();
        file.take(INVENTORY_LIMIT + 1).read_to_end(&mut bytes)?;
        if u64::try_from(bytes.len()).map_err(io::Error::other)? != self.inventory_len
            || <[u8; 32]>::from(Sha256::digest(&bytes)) != self.inventory_sha256
        {
            return Err(refused("Original projected inventory digest differs"));
        }
        HandoffInventory::parse(&bytes).map_err(io::Error::other)
    }
}
/// The canonical launcher pins the signed /usr/bin/ostree ELF and its closure.
/// Ambient environment never selects an executable or dependency search path.
pub(in crate::setup::flatpak_transfer) fn configure_ostree(
    command: &mut std::process::Command,
) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(INVENTORY_PATH)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let facts = file.metadata()?;
    // SAFETY: live file descriptor, read-only kernel seal query.
    let seals = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GET_SEALS) };
    if !facts.is_file()
        || facts.uid() != 0
        || facts.len() == 0
        || facts.len() > INVENTORY_LIMIT
        || seals < 0
        || seals & SEALS != SEALS
    {
        return Err(refused("Original OSTree helper admission is not sealed"));
    }
    command
        .env("LD_LIBRARY_PATH", "/run/tilo/migration/tools/lib")
        // Original-source bootstrap drops root to the source owner. Its /tmp
        // remains root-owned; OSTree's GPGME context needs owner-private scratch.
        // Use only the already granted private app directory after sealed proof.
        .env("TMPDIR", "/run/tilo/app");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn value() -> serde_json::Value {
        serde_json::json!({"schema":1,"operation":"11111111-2222-4333-8444-555555555555", "original_uid":1000,"inventory_len":10,"inventory_sha256":vec![1u8;32],"projections":[{"index":0,"device":1,"inode":2,"owner":1000}]})
    }
    #[test]
    fn original_flatpak_descriptor_is_bounded_and_rejects_unreviewed_authority() {
        let mut raw = value();
        raw["inventory"] = serde_json::json!({});
        assert!(serde_json::from_value::<OriginalFlatpakClaim>(raw).is_err());
        for uid in [0, u32::MAX] {
            let mut raw = value();
            raw["original_uid"] = serde_json::json!(uid);
            assert!(serde_json::from_value::<OriginalFlatpakClaim>(raw).is_err());
        }
        let mut raw = value();
        raw["inventory_len"] = serde_json::json!(INVENTORY_LIMIT + 1);
        assert!(serde_json::from_value::<OriginalFlatpakClaim>(raw).is_err());
        let mut raw = value();
        raw["projections"] = serde_json::json!([{"index":1,"device":1,"inode":2,"owner":1000}]);
        assert!(serde_json::from_value::<OriginalFlatpakClaim>(raw).is_err());
        let claim: OriginalFlatpakClaim = serde_json::from_value(value()).unwrap();
        assert!(
            claim
                .review_reader(&claim.operation, &std::sync::atomic::AtomicBool::new(false))
                .is_err()
        );
        assert!(serde_json::to_vec(&claim).unwrap().len() < 64 * 1024);
    }
    #[test]
    fn original_claim_deserialization_preserves_selected_data_scope() {
        let claim: OriginalFlatpakClaim = serde_json::from_value(value()).unwrap();
        assert_eq!(claim.selected_data_ids(), None);
        let mut raw = value();
        raw["selected_data_ids"] = serde_json::json!(["org.example.App"]);
        let claim: OriginalFlatpakClaim = serde_json::from_value(raw.clone()).unwrap();
        assert_eq!(
            claim.selected_data_ids(),
            Some(&["org.example.App".to_owned()][..])
        );
        raw["selected_data_ids"] = serde_json::json!([]);
        assert!(serde_json::from_value::<OriginalFlatpakClaim>(raw).is_err());
    }
    #[test]
    fn protected_inventory_admission_refuses_writable_and_unprotected_files() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("inventory.json");
        let bytes = b"exact inventory";
        std::fs::write(&path, bytes).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let original = std::fs::read(&path).unwrap();
        let digest = Sha256::digest(bytes).into();
        let result = read_protected_inventory(File::open(&path).unwrap(), 15, digest);
        assert!(result.is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        let result = read_protected_inventory(sealed_inventory(bytes).unwrap(), 15, digest);
        assert!(result.is_err());
    }
    #[test]
    fn original_inventory_memfd_is_readonly_and_irreversibly_sealed() {
        let mut file = sealed_inventory(b"exact inventory").unwrap();
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"exact inventory");
        assert!(file.write_all(b"changed").is_err());
        // SAFETY: live fixture descriptor and read-only kernel query.
        assert_eq!(
            unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GET_SEALS) } & SEALS,
            SEALS
        );
    }
}
