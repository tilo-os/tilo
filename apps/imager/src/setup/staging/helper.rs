//! The elevated staging helper mode (`--tilo-staging-helper <request> <progress>`).
//!
//! The request is a claim, never authority: the release is resolved against
//! the catalog compiled into this binary, the manifest and both component
//! files are hashed here, the host is re-probed and compared with the
//! reviewed plan's basis, and every mutation is recorded `intended` before
//! it is attempted and `done` only after it is read back (spec §4).

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use super::bundle::BundleFacts;
use super::efi;
use super::fat::{self, Range};
use super::handoff;
use super::plan::{Basis, Plan};
use super::platform::{Disk, NewPartition, Partition, Platform};
use super::record::{
    OriginalFirmware, OwnedBootEntry, Record, RecordStatus, RecordStore, RecordedBundle,
    RecordedComponent, ReleaseRef, Step, StepState,
};

pub const STAGING_HELPER_ARG: &str = "--tilo-staging-helper";
pub const REQUEST_SCHEMA: u32 = 1;
const MAX_REQUEST_BYTES: u64 = 1 << 20;
const MAX_MANIFEST_BYTES: u64 = crate::setup::internal_bundle::MAX_MANIFEST_BYTES as u64;
/// Streaming chunk for component writes and readback; a multiple of every
/// supported sector size.
const CHUNK: usize = 4 << 20;
const MIB: u64 = 1 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Action {
    Stage,
    Arm,
    Remove,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentPaths {
    #[serde(rename = "staging-esp")]
    pub staging_esp: String,
    #[serde(rename = "staging-root")]
    pub staging_root: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub schema: u32,
    pub operation_id: String,
    pub action: Action,
    pub release: ReleaseRef,
    pub manifest_path: String,
    pub component_paths: ComponentPaths,
    pub plan: Plan,
    pub plan_sha256: String,
    /// Secret-free installer profile fields copied into the handoff.
    #[serde(default)]
    pub profile: serde_json::Map<String, serde_json::Value>,
}

/// One line of `progress.jsonl`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Progress {
    pub step: super::record::Step,
    pub done_bytes: u64,
    pub total_bytes: u64,
}

/// Final line of `progress.jsonl`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outcome {
    pub ok: bool,
    /// Stable refusal ID (`plan-stale`, `boot-next-busy`, `digest-mismatch`, ...).
    pub code: Option<String>,
    pub message: Option<String>,
}

/// Stable refusal IDs written to [`Outcome::code`].
pub mod codes {
    /// The request is malformed or inconsistent with itself.
    pub const REQUEST_INVALID: &str = "request-invalid";
    /// The release is not in this binary's catalog or its manifest differs.
    pub const RELEASE_UNKNOWN: &str = "release-unknown";
    pub const MANIFEST_MISMATCH: &str = "manifest-mismatch";
    /// The host no longer matches the reviewed plan's basis; re-review.
    pub const PLAN_STALE: &str = "plan-stale";
    /// A component's bytes do not match the manifest.
    pub const DIGEST_MISMATCH: &str = "digest-mismatch";
    /// The staging ESP image lacks the handoff/record placeholders.
    pub const ESP_PLACEHOLDER: &str = "esp-placeholder-missing";
    /// The staging ESP image's FAT sectors are smaller than the disk's.
    pub const SECTOR_SIZE: &str = "sector-size-unsupported";
    /// Another operation on this disk is unfinished.
    pub const PENDING_OPERATION: &str = "pending-operation";
    pub const SECURE_BOOT_ON: &str = "secure-boot-on";
    /// `BootNext` names an entry that is not ours.
    pub const BOOT_NEXT_BUSY: &str = "boot-next-busy";
    /// Our `Boot####` changed (or its number was taken) since we wrote it.
    pub const BOOT_ENTRY_MODIFIED: &str = "boot-entry-modified";
    /// A staged partition, shrink or firmware entry no longer holds.
    pub const STAGING_CHANGED: &str = "staging-changed";
    /// `arm` without a completed `stage`, or `remove` without a record.
    pub const NOT_STAGED: &str = "not-staged";
    pub const RECORD_FAILED: &str = "record-failed";
    pub const IO_FAILED: &str = "io-failed";
    /// The helper exited without writing an outcome.
    pub const HELPER_EXITED: &str = "helper-exited";
}

/// Why the helper stopped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    pub code: &'static str,
    pub message: String,
}

impl Refusal {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

fn io_refusal(e: impl std::fmt::Display) -> Refusal {
    Refusal::new(codes::IO_FAILED, e.to_string())
}

/// Binds a request's release claim to something this binary trusts.
pub trait ReleaseAuthority {
    /// Resolve `release` and validate `manifest` (whose SHA-256 already
    /// equals `release.manifest_sha256`) against it.
    fn verify(&self, release: &ReleaseRef, manifest: &[u8]) -> Result<BundleFacts, String>;
}

/// The internal releases compiled into this executable
/// (`release::bundled_internal_releases`).
pub struct CompiledCatalog;

impl ReleaseAuthority for CompiledCatalog {
    fn verify(&self, release: &ReleaseRef, manifest: &[u8]) -> Result<BundleFacts, String> {
        let selected = crate::release::bundled_internal_releases()?
            .into_iter()
            .find(|r| {
                r.release_id() == release.release_id
                    && r.architecture() == release.architecture
                    && r.manifest_sha256() == release.manifest_sha256
            })
            .ok_or("the release is not in this Imager's catalog")?;
        let bundle = selected.verify_manifest(manifest)?;
        let facts = super::bundle::facts(manifest)?;
        if facts.release_id != bundle.release_id() || facts.architecture != bundle.architecture() {
            return Err("manifest does not match the selected release".into());
        }
        Ok(facts)
    }
}

type Detail = serde_json::Map<String, serde_json::Value>;

fn detail(pairs: &[(&str, serde_json::Value)]) -> Detail {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), v.clone()))
        .collect()
}

/// What a step's check found on the host.
enum Actual {
    /// The step's effect holds (with detail to record).
    Done(Detail),
    /// The effect is absent; the step may be (re)applied.
    Absent,
    /// The host holds something else; refuse.
    Conflict(Refusal),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Which {
    Esp,
    Root,
}

/// The two placeholder files inside the staging ESP image.
#[derive(Clone, Debug, Default)]
struct Placeholders {
    handoff: Vec<Range>,
    record: Vec<Range>,
}

impl Placeholders {
    fn all(&self) -> Vec<Range> {
        self.handoff.iter().chain(&self.record).copied().collect()
    }
}

/// Validate what the request can prove about itself.
pub fn validate_request(request: &Request) -> Result<(), Refusal> {
    let bad = |m: &str| Err(Refusal::new(codes::REQUEST_INVALID, m));
    if request.schema != REQUEST_SCHEMA {
        return bad("unsupported request schema");
    }
    if !super::record::valid_operation_id(&request.operation_id) {
        return bad("invalid operation ID");
    }
    if request.plan.digest() != request.plan_sha256 {
        return bad("the plan does not match its reviewed digest");
    }
    let hex64 = |s: &str| {
        s.len() == 64
            && s.bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    };
    if !hex64(&request.release.manifest_sha256)
        || !crate::setup::internal_bundle::identifier(&request.release.release_id)
        || !matches!(request.release.architecture.as_str(), "x86_64" | "aarch64")
    {
        return bad("invalid release reference");
    }
    let plan = &request.plan;
    if plan.boot_entry.description != super::BOOT_ENTRY_DESCRIPTION {
        return bad("unexpected boot entry description");
    }
    let (t, e, r) = (
        plan.target_extent,
        plan.staging_esp.extent(),
        plan.staging_root.extent(),
    );
    if t.first_lba > t.last_lba
        || e.first_lba > e.last_lba
        || r.first_lba > r.last_lba
        || t.last_lba + 1 != e.first_lba
        || e.last_lba + 1 != r.first_lba
    {
        return bad("the plan layout is not contiguous");
    }
    let guids = [
        &plan.staging_esp.partition_guid,
        &plan.staging_root.partition_guid,
    ];
    let expected_some = plan.basis.sets_partition_guids();
    if guids.iter().any(|g| g.is_some() != expected_some)
        || guids
            .iter()
            .any(|g| g.as_deref().is_some_and(|g| !super::is_guid(g)))
        || (expected_some && guids[0] == guids[1])
    {
        return bad("partition GUIDs do not match what this host can set");
    }
    if plan.basis.disk.partitions.iter().any(|p| {
        guids
            .iter()
            .any(|g| g.as_deref().is_some_and(|g| super::same_guid(g, &p.guid)))
    }) {
        return bad("a staging GUID names an existing partition");
    }
    handoff::validate_profile(&request.profile)
        .map_err(|m| Refusal::new(codes::REQUEST_INVALID, m))?;
    Ok(())
}

/// Firmware facts of the running host, as the plan's basis records them.
pub fn read_firmware(
    platform: &dyn Platform,
) -> super::platform::IoResult<super::plan::FirmwareFacts> {
    let number = |bytes: Option<Vec<u8>>| -> Result<Option<String>, String> {
        match bytes {
            None => Ok(None),
            Some(b) => {
                let list = efi::parse_u16_list(&b)?;
                match list.as_slice() {
                    [n] => Ok(Some(efi::format_boot_number(*n))),
                    _ => Err("boot number variable is not one u16".into()),
                }
            }
        }
    };
    Ok(super::plan::FirmwareFacts {
        uefi: true,
        secure_boot: platform
            .get_efi_var("SecureBoot")?
            .is_some_and(|b| b.first() == Some(&1)),
        boot_current: number(platform.get_efi_var("BootCurrent")?)?,
        boot_order: efi::parse_u16_list(&platform.get_efi_var("BootOrder")?.unwrap_or_default())?
            .into_iter()
            .map(efi::format_boot_number)
            .collect(),
        boot_next: number(platform.get_efi_var("BootNext")?)?,
    })
}

/// A fresh basis from the platform.
pub fn read_basis(platform: &dyn Platform) -> super::platform::IoResult<Basis> {
    Ok(Basis {
        disk: platform.system_disk()?,
        system_volume: platform.system_volume()?,
        firmware: read_firmware(platform)?,
    })
}

fn sorted(mut parts: Vec<Partition>) -> Vec<Partition> {
    for p in &mut parts {
        p.guid = p.guid.to_ascii_lowercase();
        p.type_guid = p.type_guid.to_ascii_lowercase();
    }
    parts.sort_by_key(|p| p.first_lba);
    parts
}

fn same_extent(p: &Partition, first: u64, last: u64) -> bool {
    p.first_lba == first && p.last_lba == last
}

fn overlaps(p: &Partition, first: u64, last: u64) -> bool {
    p.first_lba <= last && first <= p.last_lba
}

fn open_input(path: &str) -> Result<std::fs::File, String> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Never follow a final symlink or block on a FIFO with root authority.
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|e| format!("{path}: {e}"))?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err(format!("{path} is not a regular file"));
    }
    Ok(file)
}

fn read_input(path: &str, limit: u64) -> Result<Vec<u8>, String> {
    let file = open_input(path)?;
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit {
        return Err(format!("{path} is unexpectedly large"));
    }
    Ok(bytes)
}

/// Primary Btrfs superblock position in the filesystem.
const BTRFS_SUPERBLOCK_OFFSET: u64 = 0x1_0000;

/// The canonical fsid of a Btrfs superblock (`sb` starts at the superblock),
/// or `None` when the magic is absent.
fn btrfs_fsid(sb: &[u8]) -> Option<String> {
    if sb.get(0x40..0x48)? != b"_BHRfS_M" {
        return None;
    }
    let id: [u8; 16] = sb.get(0x20..0x30)?.try_into().ok()?;
    Some(uuid::Uuid::from_bytes(id).to_string())
}

fn sha_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Run one request against `platform`. Sequencing, recording and every
/// refusal live here; the platform only performs I/O.
pub fn execute(
    platform: &mut dyn Platform,
    store: &RecordStore,
    authority: &dyn ReleaseAuthority,
    request: &Request,
    progress: &mut dyn FnMut(&Progress),
) -> Result<(), Refusal> {
    validate_request(request)?;
    let mut engine = Engine {
        platform,
        store,
        progress,
        request,
        record: None,
        facts: None,
        esp_file: None,
        root_file: None,
        placeholders: None,
        written: Vec::new(),
    };
    match request.action {
        Action::Stage => engine.stage(authority),
        Action::Arm => engine.arm(),
        Action::Remove => engine.remove(),
    }
}

struct Engine<'a> {
    platform: &'a mut dyn Platform,
    store: &'a RecordStore,
    progress: &'a mut dyn FnMut(&Progress),
    request: &'a Request,
    record: Option<Record>,
    facts: Option<BundleFacts>,
    esp_file: Option<std::fs::File>,
    root_file: Option<std::fs::File>,
    placeholders: Option<Placeholders>,
    /// Write steps applied in this run (verified by the `verify` readback).
    written: Vec<Step>,
}

impl Engine<'_> {
    fn plan(&self) -> &Plan {
        &self.request.plan
    }

    fn record(&self) -> &Record {
        self.record
            .as_ref()
            .expect("record exists before steps run")
    }

    fn record_mut(&mut self) -> &mut Record {
        self.record
            .as_mut()
            .expect("record exists before steps run")
    }

    fn save(&self) -> Result<(), Refusal> {
        self.store
            .save(self.record())
            .map_err(|e| Refusal::new(codes::RECORD_FAILED, e))
    }

    fn report(&mut self, step: Step, done_bytes: u64, total_bytes: u64) {
        (self.progress)(&Progress {
            step,
            done_bytes,
            total_bytes,
        });
    }

    fn facts(&self) -> &BundleFacts {
        self.facts.as_ref().expect("facts resolved")
    }

    fn disk(&self) -> Result<Disk, Refusal> {
        self.platform.system_disk().map_err(io_refusal)
    }

    // ----- authority -------------------------------------------------------

    fn resolve_release(&mut self, authority: &dyn ReleaseAuthority) -> Result<(), Refusal> {
        let release = &self.request.release;
        let manifest = read_input(&self.request.manifest_path, MAX_MANIFEST_BYTES)
            .map_err(|m| Refusal::new(codes::MANIFEST_MISMATCH, m))?;
        if sha_hex(&manifest) != release.manifest_sha256 {
            return Err(Refusal::new(
                codes::MANIFEST_MISMATCH,
                "the manifest file does not match the requested release",
            ));
        }
        let facts = authority
            .verify(release, &manifest)
            .map_err(|m| Refusal::new(codes::RELEASE_UNKNOWN, m))?;
        if facts.release_id != release.release_id || facts.architecture != release.architecture {
            return Err(Refusal::new(
                codes::RELEASE_UNKNOWN,
                "the manifest names a different release",
            ));
        }
        let plan = self.plan();
        let s = plan.basis.disk.sector_bytes;
        let fits = |x: super::plan::Extent, c: &super::bundle::ComponentFacts| {
            x.lbas() * s >= c.minimum_partition_bytes.max(c.byte_size)
        };
        if !fits(plan.staging_esp.extent(), &facts.esp)
            || !fits(plan.staging_root.extent(), &facts.root)
        {
            return Err(Refusal::new(
                codes::REQUEST_INVALID,
                "the plan's staging partitions are smaller than the bundle needs",
            ));
        }
        if plan.boot_entry.loader_path != facts.efi_path.replace('/', "\\") {
            return Err(Refusal::new(
                codes::REQUEST_INVALID,
                "the plan's loader path is not the bundle's boot binding",
            ));
        }
        self.facts = Some(facts);
        Ok(())
    }

    /// The request's plan must be exactly what the planner produces for its
    /// (already revalidated) basis, the bundle's sizes and the reviewed Tilo
    /// size. Nothing about placement or the shrink amount is taken from the
    /// unprivileged caller; pre-generated partition GUIDs are the only
    /// caller-chosen values.
    fn check_plan_geometry(&self) -> Result<(), Refusal> {
        let plan = self.plan();
        let target_bytes = plan.target_extent.lbas() * plan.basis.disk.sector_bytes;
        let facts = self.facts();
        let expected = super::plan::compute(&plan.basis, facts.requirements(target_bytes))
            .map(|p| super::plan::with_loader_path(p, &facts.efi_path))
            .map_err(|e| {
                Refusal::new(
                    codes::REQUEST_INVALID,
                    format!("the plan cannot be reproduced for this host: {e:?}"),
                )
            })?;
        let mut comparable = plan.clone();
        comparable
            .staging_esp
            .partition_guid
            .clone_from(&expected.staging_esp.partition_guid);
        comparable
            .staging_root
            .partition_guid
            .clone_from(&expected.staging_root.partition_guid);
        let guids_valid = [&plan.staging_esp, &plan.staging_root]
            .iter()
            .all(|p| p.partition_guid.as_deref().is_none_or(super::is_guid))
            && plan.staging_esp.partition_guid.is_some()
                == expected.staging_esp.partition_guid.is_some()
            && plan.staging_root.partition_guid.is_some()
                == expected.staging_root.partition_guid.is_some();
        if comparable != expected || !guids_valid {
            return Err(Refusal::new(
                codes::REQUEST_INVALID,
                "the plan's placement or shrink differs from what this host and bundle require",
            ));
        }
        Ok(())
    }

    fn load_record(&mut self) -> Result<Option<Record>, Refusal> {
        let record = self
            .store
            .load(&self.request.operation_id)
            .map_err(|e| Refusal::new(codes::RECORD_FAILED, e))?;
        if let Some(r) = &record {
            if r.plan_sha256 != self.request.plan_sha256 || r.release != self.request.release {
                return Err(Refusal::new(
                    codes::REQUEST_INVALID,
                    "this operation ID belongs to a different plan",
                ));
            }
        }
        Ok(record)
    }

    fn check_no_other_pending(&self) -> Result<(), Refusal> {
        let records = self
            .store
            .list()
            .map_err(|e| Refusal::new(codes::RECORD_FAILED, e))?;
        let disk = &self.plan().basis.disk.guid;
        if let Some(other) = records.iter().find(|r| {
            r.operation_id != self.request.operation_id
                && super::same_guid(&r.plan.basis.disk.guid, disk)
                // A fully staged operation still owns partitions and a boot
                // entry; a second one would orphan them. Only removal ends it.
                && r.status() != RecordStatus::Removed
        }) {
            return Err(Refusal::new(
                codes::PENDING_OPERATION,
                format!(
                    "operation {} on this disk is not removed",
                    other.operation_id
                ),
            ));
        }
        Ok(())
    }

    fn owned_number(&self) -> Option<u16> {
        let r = self.record.as_ref()?;
        r.owned_boot_entry
            .as_ref()
            .map(|o| o.number.as_str())
            .or_else(|| r.detail_str(Step::Handoff, "boot_number"))
            .and_then(|n| efi::parse_boot_number(n).ok())
    }

    fn firmware_preconditions(&self) -> Result<(), Refusal> {
        let firmware = read_firmware(&*self.platform).map_err(io_refusal)?;
        if firmware.secure_boot {
            return Err(Refusal::new(
                codes::SECURE_BOOT_ON,
                "Secure Boot is enabled; the staged installer cannot start",
            ));
        }
        if let Some(next) = &firmware.boot_next {
            let ours = self.owned_number().map(efi::format_boot_number);
            if ours.as_deref() != Some(next.as_str()) {
                return Err(Refusal::new(
                    codes::BOOT_NEXT_BUSY,
                    format!("BootNext is already set to Boot{next}"),
                ));
            }
        }
        Ok(())
    }

    /// The fresh probe, with this operation's own recorded effects undone,
    /// must equal the plan's basis (volatile Windows usage figures excepted:
    /// the shrink bounds are re-checked against fresh values instead).
    fn check_basis(&self) -> Result<(), Refusal> {
        let stale = |m: String| Err(Refusal::new(codes::PLAN_STALE, m));
        let mut current = read_basis(&*self.platform).map_err(io_refusal)?;
        let plan = self.plan();
        let expected = &plan.basis;
        if let Some(record) = &self.record {
            current.disk.partitions.retain(|p| {
                let ours = [Which::Esp, Which::Root].iter().any(|w| {
                    let staged = self.staged(*w);
                    match self.guid_for(*w, record) {
                        Some(g) => super::same_guid(&g, &p.guid),
                        None => same_extent(p, staged.first_lba, staged.last_lba),
                    }
                });
                !ours
            });
            if let (Some(shrink), Some(volume)) = (&plan.shrink, current.system_volume.as_mut()) {
                if volume.size_bytes == shrink.to_bytes && record.state(Step::Shrink).is_some() {
                    volume.size_bytes = shrink.from_bytes;
                    let s = current.disk.sector_bytes;
                    if let Some(c) = current
                        .disk
                        .partitions
                        .iter_mut()
                        .find(|p| super::same_guid(&p.guid, &shrink.partition_guid))
                    {
                        c.last_lba = c.first_lba + shrink.from_bytes / s - 1;
                    }
                }
            }
            let ours = self.owned_number().map(efi::format_boot_number);
            if current.firmware.boot_next.is_some() && current.firmware.boot_next == ours {
                current
                    .firmware
                    .boot_next
                    .clone_from(&expected.firmware.boot_next);
            }
        }
        let (d, e) = (&current.disk, &expected.disk);
        if !super::same_guid(&d.guid, &e.guid)
            || d.size_bytes != e.size_bytes
            || d.sector_bytes != e.sector_bytes
        {
            return stale("the system disk changed".into());
        }
        if sorted(d.partitions.clone()) != sorted(e.partitions.clone()) {
            return stale("the partition map changed since review".into());
        }
        for x in [
            plan.target_extent,
            plan.staging_esp.extent(),
            plan.staging_root.extent(),
        ] {
            if x.first_lba < d.first_usable_lba || x.last_lba > d.last_usable_lba {
                return stale("the plan lies outside the disk's usable area".into());
            }
        }
        match (&current.system_volume, &expected.system_volume) {
            (None, None) => {}
            (Some(c), Some(e))
                if super::same_guid(&c.partition_guid, &e.partition_guid)
                    && c.size_bytes == e.size_bytes => {}
            _ => return stale("the system volume changed since review".into()),
        }
        let (cf, ef) = (&current.firmware, &expected.firmware);
        // The unprivileged Windows probe cannot read EFI variables, so its
        // basis carries no boot fields; there the helper's own live reads
        // (`firmware_preconditions`, `original_firmware`) are the authority.
        let boot_fields_probed = plan.basis.host_os() == super::plan::HostOs::Linux;
        if cf.uefi != ef.uefi
            || cf.secure_boot != ef.secure_boot
            || (boot_fields_probed
                && (cf.boot_current != ef.boot_current
                    || cf.boot_order != ef.boot_order
                    || cf.boot_next != ef.boot_next))
        {
            return stale("firmware boot settings changed since review".into());
        }
        Ok(())
    }

    // ----- component files ---------------------------------------------------

    fn open_components(&mut self) -> Result<(), Refusal> {
        let paths = &self.request.component_paths;
        let open = |p: &str| open_input(p).map_err(|m| Refusal::new(codes::DIGEST_MISMATCH, m));
        self.esp_file = Some(open(&paths.staging_esp)?);
        self.root_file = Some(open(&paths.staging_root)?);
        Ok(())
    }

    /// Pure verification read of both components, and the placeholder
    /// layout of the ESP image, before anything is mutated.
    fn verify_sources(&mut self) -> Result<(), Refusal> {
        for which in [Which::Esp, Which::Root] {
            let (size, sha) = self.component(which);
            let mut hash = Sha256::new();
            let mut buf = vec![0_u8; CHUNK];
            let file = match which {
                Which::Esp => self.esp_file.as_mut(),
                Which::Root => self.root_file.as_mut(),
            }
            .expect("component opened");
            file.seek(SeekFrom::Start(0)).map_err(io_refusal)?;
            let mut done = 0;
            while done < size {
                let n = (size - done).min(CHUNK as u64) as usize;
                file.read_exact(&mut buf[..n]).map_err(|_| {
                    Refusal::new(codes::DIGEST_MISMATCH, "a component file is truncated")
                })?;
                hash.update(&buf[..n]);
                done += n as u64;
                (self.progress)(&Progress {
                    step: Step::Verify,
                    done_bytes: done,
                    total_bytes: size,
                });
            }
            let file = self.file(which);
            if file.read(&mut buf[..1]).map_err(io_refusal)? != 0 {
                return Err(Refusal::new(
                    codes::DIGEST_MISMATCH,
                    "a component file is too long",
                ));
            }
            if hex::encode(hash.finalize()) != sha {
                return Err(Refusal::new(
                    codes::DIGEST_MISMATCH,
                    "a component does not match the manifest digest",
                ));
            }
        }
        let size = self.facts().esp.byte_size;
        let disk_sector = self.plan().basis.disk.sector_bytes;
        let file = self.esp_file.as_mut().expect("opened");
        let mut read = |at: u64, buf: &mut [u8]| -> Result<(), String> {
            file.seek(SeekFrom::Start(at)).map_err(|e| e.to_string())?;
            file.read_exact(buf).map_err(|e| e.to_string())
        };
        // Linux vfat (and firmware FAT drivers) refuse a FAT whose logical
        // sector is smaller than the device's, so a 512-byte ESP image on a
        // 4Kn disk would stage fine and then never boot. Refuse before any
        // mutation instead of after shrinking C:.
        let mut bpb = [0_u8; 2];
        read(11, &mut bpb).map_err(io_refusal)?;
        let fat_sector = u64::from(u16::from_le_bytes(bpb));
        if fat_sector < disk_sector {
            return Err(Refusal::new(
                codes::SECTOR_SIZE,
                format!(
                    "the staging ESP uses {fat_sector}-byte sectors; this disk needs {disk_sector}"
                ),
            ));
        }
        let placeholders = find_placeholders(&mut read, size)?;
        for r in placeholders.all() {
            let mut buf = vec![0_u8; r.len as usize];
            read(r.offset, &mut buf).map_err(io_refusal)?;
            if buf.iter().any(|b| *b != b' ') {
                return Err(Refusal::new(
                    codes::ESP_PLACEHOLDER,
                    "the staging ESP placeholders are not blank",
                ));
            }
        }
        self.placeholders = Some(placeholders);
        Ok(())
    }

    fn component(&self, which: Which) -> (u64, String) {
        let f = self.facts();
        let c = match which {
            Which::Esp => &f.esp,
            Which::Root => &f.root,
        };
        (c.byte_size, c.sha256.clone())
    }

    fn file(&mut self, which: Which) -> &mut std::fs::File {
        match which {
            Which::Esp => self.esp_file.as_mut(),
            Which::Root => self.root_file.as_mut(),
        }
        .expect("component opened")
    }

    // ----- identities --------------------------------------------------------

    fn staged(&self, which: Which) -> &super::plan::StagedPartition {
        match which {
            Which::Esp => &self.plan().staging_esp,
            Which::Root => &self.plan().staging_root,
        }
    }

    fn create_step(which: Which) -> Step {
        match which {
            Which::Esp => Step::CreateEsp,
            Which::Root => Step::CreateRoot,
        }
    }

    fn type_guid(which: Which) -> &'static str {
        match which {
            Which::Esp => super::ESP_TYPE,
            Which::Root => super::STAGING_ROOT_TYPE,
        }
    }

    fn guid_for(&self, which: Which, record: &Record) -> Option<String> {
        self.staged(which).partition_guid.clone().or_else(|| {
            record
                .detail_str(Self::create_step(which), "partition_guid")
                .map(str::to_string)
        })
    }

    /// Our staged partition on `disk`: matched by GUID when known, otherwise
    /// (Windows, before creation was confirmed) by exact extent and type.
    fn find_staged(&self, disk: &Disk, which: Which) -> Result<Option<Partition>, Refusal> {
        let staged = self.staged(which);
        let (first, last, kind) = (staged.first_lba, staged.last_lba, Self::type_guid(which));
        let changed = |m: &str| Err(Refusal::new(codes::STAGING_CHANGED, m.to_string()));
        let known = self.record.as_ref().and_then(|r| self.guid_for(which, r));
        let known = known.or_else(|| staged.partition_guid.clone());
        if let Some(g) = &known {
            if let Some(p) = disk
                .partitions
                .iter()
                .find(|p| super::same_guid(&p.guid, g))
            {
                return if same_extent(p, first, last) && super::same_guid(&p.type_guid, kind) {
                    Ok(Some(p.clone()))
                } else {
                    changed("a staging partition no longer has its recorded extent and type")
                };
            }
        }
        let overlapping: Vec<_> = disk
            .partitions
            .iter()
            .filter(|p| overlaps(p, first, last))
            .collect();
        match overlapping.as_slice() {
            [] => Ok(None),
            [p] if known.is_none()
                && same_extent(p, first, last)
                && super::same_guid(&p.type_guid, kind)
                && !self
                    .plan()
                    .basis
                    .disk
                    .partitions
                    .iter()
                    .any(|b| super::same_guid(&b.guid, &p.guid)) =>
            {
                Ok(Some((*p).clone()))
            }
            _ => changed("another partition occupies the staging extent"),
        }
    }

    fn require_staged(&self, which: Which) -> Result<(Disk, Partition), Refusal> {
        let disk = self.disk()?;
        let p = self.find_staged(&disk, which)?.ok_or_else(|| {
            Refusal::new(codes::STAGING_CHANGED, "a staging partition is missing")
        })?;
        Ok((disk, p))
    }

    // ----- aligned partition I/O ---------------------------------------------

    fn read_at(&self, disk: &Disk, guid: &str, offset: u64, buf: &mut [u8]) -> Result<(), Refusal> {
        let s = disk.sector_bytes;
        let start = offset / s * s;
        let end = (offset + buf.len() as u64).div_ceil(s) * s;
        let mut tmp = vec![0_u8; (end - start) as usize];
        self.platform
            .read_partition(disk, guid, start, &mut tmp)
            .map_err(io_refusal)?;
        let at = (offset - start) as usize;
        buf.copy_from_slice(&tmp[at..at + buf.len()]);
        Ok(())
    }

    /// Write arbitrary bytes, read-modify-writing partial edge sectors.
    fn write_at(
        &mut self,
        disk: &Disk,
        guid: &str,
        offset: u64,
        data: &[u8],
    ) -> Result<(), Refusal> {
        let s = disk.sector_bytes;
        let start = offset / s * s;
        let end = (offset + data.len() as u64).div_ceil(s) * s;
        let mut tmp = vec![0_u8; (end - start) as usize];
        if start != offset || end != offset + data.len() as u64 {
            self.platform
                .read_partition(disk, guid, start, &mut tmp)
                .map_err(io_refusal)?;
        }
        let at = (offset - start) as usize;
        tmp[at..at + data.len()].copy_from_slice(data);
        self.platform
            .write_partition(disk, guid, start, &tmp)
            .map_err(io_refusal)
    }

    /// SHA-256 of partition bytes `[0, len)`, with `mask` ranges read as
    /// spaces.
    fn readback_digest(
        &mut self,
        disk: &Disk,
        guid: &str,
        len: u64,
        mask: &[Range],
        step: Step,
    ) -> Result<String, Refusal> {
        let s = disk.sector_bytes;
        let mut hash = Sha256::new();
        let mut buf = vec![0_u8; CHUNK];
        let mut done = 0;
        while done < len {
            let n = (len - done).min(CHUNK as u64);
            let aligned = n.div_ceil(s) * s;
            self.platform
                .read_partition(disk, guid, done, &mut buf[..aligned as usize])
                .map_err(io_refusal)?;
            let chunk = &mut buf[..n as usize];
            fat::mask(chunk, done, mask);
            hash.update(&*chunk);
            done += n;
            self.report(step, done, len);
        }
        Ok(hex::encode(hash.finalize()))
    }

    fn placeholders_from_disk(&self, disk: &Disk, guid: &str) -> Result<Placeholders, Refusal> {
        let size = self.facts().esp.byte_size;
        let mut read = |at: u64, buf: &mut [u8]| -> Result<(), String> {
            self.read_at(disk, guid, at, buf).map_err(|r| r.message)
        };
        find_placeholders(&mut read, size)
    }

    fn placeholders(&mut self, disk: &Disk, guid: &str) -> Result<Placeholders, Refusal> {
        if let Some(p) = &self.placeholders {
            return Ok(p.clone());
        }
        let p = self.placeholders_from_disk(disk, guid)?;
        self.placeholders = Some(p.clone());
        Ok(p)
    }

    fn component_matches(&mut self, which: Which) -> Result<bool, Refusal> {
        let (disk, p) = self.require_staged(which)?;
        let (size, sha) = self.component(which);
        let mask = match which {
            Which::Esp => self.placeholders(&disk, &p.guid)?.all(),
            Which::Root => Vec::new(),
        };
        let step = match which {
            Which::Esp => Step::WriteEsp,
            Which::Root => Step::WriteRoot,
        };
        Ok(self.readback_digest(&disk, &p.guid, size, &mask, step)? == sha)
    }

    // ----- step dispatch -----------------------------------------------------

    /// Run one step: re-verify a done step, adopt an effect that already
    /// holds, otherwise record intent, apply, re-read and record done.
    /// `reapply` lets an idempotent step run again when its effect vanished
    /// (BootNext consumed by a restart; remove re-runs).
    fn run(&mut self, step: Step, reapply: bool) -> Result<(), Refusal> {
        let recorded = self.record().state(step);
        self.report(step, 0, 0);
        match self.check(step)? {
            Actual::Done(d) => {
                if recorded != Some(StepState::Done) {
                    self.record_mut().done(step, d);
                    self.save()?;
                }
                return Ok(());
            }
            Actual::Conflict(r) => return Err(r),
            Actual::Absent if recorded == Some(StepState::Done) && !reapply => {
                return Err(Refusal::new(
                    codes::STAGING_CHANGED,
                    format!("{step:?} no longer holds on this host"),
                ));
            }
            Actual::Absent => {}
        }
        let intent = self.intent(step)?;
        self.record_mut().intend(step, intent);
        self.save()?;
        let applied = self.apply(step).and_then(|()| self.check(step));
        match applied {
            Ok(Actual::Done(d)) => {
                self.record_mut().done(step, d);
                self.save()
            }
            Ok(Actual::Absent) => {
                let r = Refusal::new(codes::IO_FAILED, format!("{step:?} did not take effect"));
                self.fail(step, r)
            }
            Ok(Actual::Conflict(r)) | Err(r) => self.fail(step, r),
        }
    }

    fn fail(&mut self, step: Step, r: Refusal) -> Result<(), Refusal> {
        self.record_mut().failed(step, &r.to_string());
        let _ = self.save();
        Err(r)
    }

    fn check(&mut self, step: Step) -> Result<Actual, Refusal> {
        match step {
            Step::Shrink => self.check_shrink(),
            Step::CreateEsp => self.check_create(Which::Esp),
            Step::CreateRoot => self.check_create(Which::Root),
            Step::WriteEsp => self.check_write(Which::Esp),
            Step::WriteRoot => self.check_write(Which::Root),
            Step::Verify => self.check_verify(),
            Step::Handoff => self.check_handoff(),
            Step::BootEntry => self.check_boot_entry(),
            Step::BootNext => self.check_boot_next(),
            Step::RemoveBootNext => self.check_remove_boot_next(),
            Step::RemoveBootEntry => self.check_remove_boot_entry(),
            Step::RemoveRoot => self.check_remove(Which::Root),
            Step::RemoveEsp => self.check_remove(Which::Esp),
            Step::RestoreShrink => self.check_restore_shrink(),
        }
    }

    fn intent(&mut self, step: Step) -> Result<Detail, Refusal> {
        Ok(match step {
            Step::Shrink => {
                let s = self.plan().shrink.clone().expect("shrink planned");
                detail(&[
                    ("from_bytes", s.from_bytes.into()),
                    ("to_bytes", s.to_bytes.into()),
                ])
            }
            Step::CreateEsp | Step::CreateRoot => {
                let which = if step == Step::CreateEsp {
                    Which::Esp
                } else {
                    Which::Root
                };
                let staged = self.staged(which).clone();
                let mut d = detail(&[
                    ("first_lba", staged.first_lba.into()),
                    ("last_lba", staged.last_lba.into()),
                    ("type_guid", Self::type_guid(which).into()),
                ]);
                if let Some(g) = staged.partition_guid {
                    d.insert("partition_guid".into(), g.into());
                }
                d
            }
            Step::Handoff => {
                let number = match self.owned_number() {
                    Some(n) => n,
                    None => self.allocate_boot_number()?,
                };
                detail(&[("boot_number", efi::format_boot_number(number).into())])
            }
            Step::BootEntry => {
                let number = self.owned_number().expect("handoff allocated the number");
                let bytes = self.load_option()?;
                let owned = OwnedBootEntry {
                    number: efi::format_boot_number(number),
                    load_option_sha256: sha_hex(&bytes),
                };
                let d = detail(&[
                    ("number", owned.number.clone().into()),
                    (
                        "load_option_sha256",
                        owned.load_option_sha256.clone().into(),
                    ),
                ]);
                // Recorded before the write so `remove` can find an entry a
                // crash left unconfirmed.
                self.record_mut().owned_boot_entry = Some(owned);
                d
            }
            _ => Detail::new(),
        })
    }

    fn apply(&mut self, step: Step) -> Result<(), Refusal> {
        match step {
            Step::Shrink => self.apply_shrink(),
            Step::CreateEsp => self.apply_create(Which::Esp),
            Step::CreateRoot => self.apply_create(Which::Root),
            Step::WriteEsp => self.apply_write(Which::Esp),
            Step::WriteRoot => self.apply_write(Which::Root),
            Step::Verify => Ok(()),
            Step::Handoff => self.apply_handoff(),
            Step::BootEntry => {
                let number = self.owned_number().expect("allocated");
                let bytes = self.load_option()?;
                self.platform
                    .set_efi_var(&efi::boot_var_name(number), &bytes)
                    .map_err(io_refusal)
            }
            Step::BootNext => {
                let number = self.owned_number().expect("allocated");
                self.platform
                    .set_efi_var("BootNext", &efi::encode_u16(number))
                    .map_err(io_refusal)
            }
            Step::RemoveBootNext => self.platform.delete_efi_var("BootNext").map_err(io_refusal),
            Step::RemoveBootEntry => {
                let number = self.owned_number().expect("owned entry");
                self.platform
                    .delete_efi_var(&efi::boot_var_name(number))
                    .map_err(io_refusal)
            }
            Step::RemoveRoot => self.apply_remove(Which::Root),
            Step::RemoveEsp => self.apply_remove(Which::Esp),
            Step::RestoreShrink => {
                let s = self.plan().shrink.clone().expect("shrink planned");
                self.platform
                    .grow_system_volume(&s.partition_guid, s.from_bytes)
                    .map_err(io_refusal)
            }
        }
    }

    // ----- shrink ------------------------------------------------------------

    fn check_shrink(&mut self) -> Result<Actual, Refusal> {
        let Some(shrink) = self.plan().shrink.clone() else {
            return Ok(Actual::Done(detail(&[(
                "skipped",
                "no shrink planned".into(),
            )])));
        };
        let volume = self.platform.system_volume().map_err(io_refusal)?;
        Ok(match volume {
            Some(v) if v.size_bytes == shrink.to_bytes => Actual::Done(Detail::new()),
            Some(v) if v.size_bytes == shrink.from_bytes => Actual::Absent,
            _ => Actual::Conflict(Refusal::new(
                codes::STAGING_CHANGED,
                "the system volume size is neither the reviewed nor the shrunk size",
            )),
        })
    }

    fn apply_shrink(&mut self) -> Result<(), Refusal> {
        let shrink = self.plan().shrink.clone().expect("planned");
        let volume = self
            .platform
            .system_volume()
            .map_err(io_refusal)?
            .ok_or_else(|| Refusal::new(codes::PLAN_STALE, "no system volume"))?;
        if shrink.to_bytes < super::plan::minimum_system_volume_bytes(&volume) {
            return Err(Refusal::new(
                codes::PLAN_STALE,
                "the system volume can no longer shrink that far safely",
            ));
        }
        self.platform
            .shrink_system_volume(&shrink.partition_guid, shrink.to_bytes)
            .map_err(io_refusal)?;
        let disk = self.disk()?;
        self.platform.sync_disk(&disk).map_err(io_refusal)
    }

    // ----- create --------------------------------------------------------------

    fn check_create(&mut self, which: Which) -> Result<Actual, Refusal> {
        let disk = self.disk()?;
        match self.find_staged(&disk, which) {
            Ok(Some(p)) => Ok(Actual::Done(detail(&[
                ("partition_guid", p.guid.to_ascii_lowercase().into()),
                ("partition_number", p.number.into()),
            ]))),
            Ok(None) => Ok(Actual::Absent),
            Err(r) => Ok(Actual::Conflict(r)),
        }
    }

    fn apply_create(&mut self, which: Which) -> Result<(), Refusal> {
        let disk = self.disk()?;
        let staged = self.staged(which).clone();
        let spec = NewPartition {
            first_lba: staged.first_lba,
            last_lba: staged.last_lba,
            type_guid: Self::type_guid(which).into(),
            name: match which {
                Which::Esp => super::STAGING_ESP_NAME,
                Which::Root => super::STAGING_ROOT_NAME,
            }
            .into(),
            guid: staged.partition_guid.clone(),
        };
        let guid = self
            .platform
            .create_partition(&disk, &spec)
            .map_err(io_refusal)?;
        if let Some(expected) = &staged.partition_guid {
            if !super::same_guid(expected, &guid) {
                return Err(Refusal::new(
                    codes::IO_FAILED,
                    "the created partition did not take the planned GUID",
                ));
            }
        }
        let step = Self::create_step(which);
        let d = detail(&[("partition_guid", guid.to_ascii_lowercase().into())]);
        // Recorded immediately so a crash before `done` still knows the GUID.
        self.record_mut().intend(step, d);
        self.save()?;
        let disk = self.disk()?;
        self.platform.sync_disk(&disk).map_err(io_refusal)
    }

    // ----- write ---------------------------------------------------------------

    fn check_write(&mut self, which: Which) -> Result<Actual, Refusal> {
        let step = match which {
            Which::Esp => Step::WriteEsp,
            Which::Root => Step::WriteRoot,
        };
        if self.written.contains(&step) {
            // Just streamed with a matching source digest; `verify` reads back.
            return Ok(Actual::Done(Detail::new()));
        }
        if !self.record().is_done(step) {
            return Ok(Actual::Absent);
        }
        if self.record().is_done(Step::Verify) {
            return Ok(Actual::Done(Detail::new()));
        }
        Ok(if self.component_matches(which)? {
            Actual::Done(Detail::new())
        } else {
            Actual::Absent
        })
    }

    fn apply_write(&mut self, which: Which) -> Result<(), Refusal> {
        let (disk, p) = self.require_staged(which)?;
        let step = match which {
            Which::Esp => Step::WriteEsp,
            Which::Root => Step::WriteRoot,
        };
        let s = disk.sector_bytes;
        let part_bytes = (p.last_lba - p.first_lba + 1) * s;
        // Zero the first and last MiB so no stale signature (e.g. NTFS from
        // the shrunk region) is probed next to the real one.
        let zeros = vec![0_u8; MIB.min(part_bytes) as usize];
        self.platform
            .write_partition(&disk, &p.guid, 0, &zeros)
            .map_err(io_refusal)?;
        self.platform
            .write_partition(&disk, &p.guid, part_bytes - zeros.len() as u64, &zeros)
            .map_err(io_refusal)?;
        let (size, sha) = self.component(which);
        let mut hash = Sha256::new();
        let mut buf = vec![0_u8; CHUNK];
        self.file(which)
            .seek(SeekFrom::Start(0))
            .map_err(io_refusal)?;
        let mut done = 0;
        while done < size {
            let n = (size - done).min(CHUNK as u64) as usize;
            self.file(which)
                .read_exact(&mut buf[..n])
                .map_err(|_| Refusal::new(codes::DIGEST_MISMATCH, "a component file changed"))?;
            hash.update(&buf[..n]);
            if (n as u64).is_multiple_of(s) {
                self.platform
                    .write_partition(&disk, &p.guid, done, &buf[..n])
                    .map_err(io_refusal)?;
            } else {
                let data = buf[..n].to_vec();
                self.write_at(&disk, &p.guid, done, &data)?;
            }
            done += n as u64;
            self.report(step, done, size);
        }
        self.platform.sync_disk(&disk).map_err(io_refusal)?;
        if hex::encode(hash.finalize()) != sha {
            return Err(Refusal::new(
                codes::DIGEST_MISMATCH,
                "a component file changed while it was written",
            ));
        }
        self.written.push(step);
        Ok(())
    }

    fn check_verify(&mut self) -> Result<Actual, Refusal> {
        for which in [Which::Esp, Which::Root] {
            if !self.component_matches(which)? && !self.booted_root_holds(which)? {
                return Ok(Actual::Conflict(Refusal::new(
                    codes::DIGEST_MISMATCH,
                    "a staging partition does not read back as the manifest digest",
                )));
            }
        }
        Ok(Actual::Done(Detail::new()))
    }

    /// Once BootNext has been armed the staged installer may have booted:
    /// its first boot mounts the root read-write and grows the filesystem to
    /// the partition, so the root no longer hashes to the download. Such a
    /// root is still ours when the partition identity checks hold (done by
    /// `require_staged`) and its Btrfs superblock carries the fsid the
    /// manifest bound. The ESP never gets this allowance.
    fn booted_root_holds(&mut self, which: Which) -> Result<bool, Refusal> {
        if which != Which::Root || !self.record().is_done(Step::BootNext) {
            return Ok(false);
        }
        let expected = self.facts().root.filesystem_uuid.clone();
        if !super::is_guid(&expected) {
            return Ok(false);
        }
        let (disk, p) = self.require_staged(Which::Root)?;
        let sector = disk.sector_bytes;
        let start = BTRFS_SUPERBLOCK_OFFSET / sector * sector;
        let mut buf = vec![0_u8; sector.max(4096) as usize];
        self.read_at(&disk, &p.guid, start, &mut buf)?;
        let at = (BTRFS_SUPERBLOCK_OFFSET - start) as usize;
        Ok(btrfs_fsid(&buf[at..]).is_some_and(|fsid| super::same_guid(&fsid, &expected)))
    }

    // ----- handoff -------------------------------------------------------------

    fn allocate_boot_number(&mut self) -> Result<u16, Refusal> {
        let order = efi::parse_u16_list(
            &self
                .platform
                .get_efi_var("BootOrder")
                .map_err(io_refusal)?
                .unwrap_or_default(),
        )
        .map_err(io_refusal)?;
        let platform = &*self.platform;
        efi::allocate_boot_number(&order, |n| {
            Ok(platform.get_efi_var(&efi::boot_var_name(n))?.is_some())
        })
        .map_err(io_refusal)
    }

    fn handoff_bytes(&mut self, number: u16, len: u64) -> Result<Vec<u8>, Refusal> {
        let (_, esp) = self.require_staged(Which::Esp)?;
        let (_, root) = self.require_staged(Which::Root)?;
        handoff::for_plan(
            &self.request.operation_id,
            self.plan(),
            &esp.guid,
            &root.guid,
            &efi::format_boot_number(number),
            self.record().original_firmware.boot_order.clone(),
            self.request.profile.clone(),
        )
        .to_padded(len)
        .map_err(|m| Refusal::new(codes::ESP_PLACEHOLDER, m))
    }

    fn write_ranges(
        &mut self,
        disk: &Disk,
        guid: &str,
        ranges: &[Range],
        bytes: &[u8],
    ) -> Result<(), Refusal> {
        let mut at = 0;
        for r in ranges {
            let piece = bytes[at..at + r.len as usize].to_vec();
            self.write_at(disk, guid, r.offset, &piece)?;
            at += r.len as usize;
        }
        Ok(())
    }

    fn read_ranges(&self, disk: &Disk, guid: &str, ranges: &[Range]) -> Result<Vec<u8>, Refusal> {
        let mut out = Vec::new();
        for r in ranges {
            let mut buf = vec![0_u8; r.len as usize];
            self.read_at(disk, guid, r.offset, &mut buf)?;
            out.extend(buf);
        }
        Ok(out)
    }

    fn check_handoff(&mut self) -> Result<Actual, Refusal> {
        let Some(number) = self
            .record()
            .detail_str(Step::Handoff, "boot_number")
            .and_then(|n| efi::parse_boot_number(n).ok())
        else {
            return Ok(Actual::Absent);
        };
        let (disk, esp) = self.require_staged(Which::Esp)?;
        let ranges = self.placeholders(&disk, &esp.guid)?.handoff;
        let expected = self.handoff_bytes(number, fat::ranges_len(&ranges))?;
        Ok(
            if self.read_ranges(&disk, &esp.guid, &ranges)? == expected {
                Actual::Done(detail(&[(
                    "boot_number",
                    efi::format_boot_number(number).into(),
                )]))
            } else {
                Actual::Absent
            },
        )
    }

    fn apply_handoff(&mut self) -> Result<(), Refusal> {
        let number = self.owned_number().expect("intent allocated the number");
        let (disk, esp) = self.require_staged(Which::Esp)?;
        let ranges = self.placeholders(&disk, &esp.guid)?.handoff;
        let bytes = self.handoff_bytes(number, fat::ranges_len(&ranges))?;
        self.write_ranges(&disk, &esp.guid, &ranges, &bytes)?;
        self.write_record_copy()?;
        self.platform.sync_disk(&disk).map_err(io_refusal)
    }

    /// Refresh `/EFI/tilo-staging/record.json` on the staging ESP.
    fn write_record_copy(&mut self) -> Result<(), Refusal> {
        let (disk, esp) = self.require_staged(Which::Esp)?;
        let ranges = self.placeholders(&disk, &esp.guid)?.record;
        let json = serde_json::to_vec(self.record()).map_err(io_refusal)?;
        let bytes = handoff::pad_json(&json, fat::ranges_len(&ranges))
            .map_err(|m| Refusal::new(codes::ESP_PLACEHOLDER, m))?;
        self.write_ranges(&disk, &esp.guid, &ranges, &bytes)?;
        if self.read_ranges(&disk, &esp.guid, &ranges)? != bytes {
            return Err(Refusal::new(
                codes::IO_FAILED,
                "the ESP record copy did not read back",
            ));
        }
        self.platform.sync_disk(&disk).map_err(io_refusal)
    }

    // ----- firmware ------------------------------------------------------------

    fn load_option(&self) -> Result<Vec<u8>, Refusal> {
        let disk = self.disk()?;
        let esp = self
            .find_staged(&disk, Which::Esp)?
            .ok_or_else(|| Refusal::new(codes::STAGING_CHANGED, "the staging ESP is missing"))?;
        efi::encode_load_option(&efi::HardDriveLoadOption {
            description: self.plan().boot_entry.description.clone(),
            partition_number: esp.number,
            start_lba: esp.first_lba,
            size_lba: esp.last_lba - esp.first_lba + 1,
            partition_guid: esp.guid.to_ascii_lowercase(),
            file_path: self.plan().boot_entry.loader_path.clone(),
        })
        .map_err(|m| Refusal::new(codes::REQUEST_INVALID, m))
    }

    fn check_boot_entry(&mut self) -> Result<Actual, Refusal> {
        let number = self
            .owned_number()
            .ok_or_else(|| Refusal::new(codes::STAGING_CHANGED, "no boot number was allocated"))?;
        let expected = self.load_option()?;
        let current = self
            .platform
            .get_efi_var(&efi::boot_var_name(number))
            .map_err(io_refusal)?;
        Ok(match current {
            None => Actual::Absent,
            Some(bytes) if bytes == expected => Actual::Done(detail(&[
                ("number", efi::format_boot_number(number).into()),
                ("load_option_sha256", sha_hex(&bytes).into()),
            ])),
            Some(_) => Actual::Conflict(Refusal::new(
                codes::BOOT_ENTRY_MODIFIED,
                format!(
                    "{} holds a different load option",
                    efi::boot_var_name(number)
                ),
            )),
        })
    }

    fn check_boot_next(&mut self) -> Result<Actual, Refusal> {
        let number = self
            .owned_number()
            .ok_or_else(|| Refusal::new(codes::STAGING_CHANGED, "no boot number was allocated"))?;
        let current = self.platform.get_efi_var("BootNext").map_err(io_refusal)?;
        Ok(match current {
            None => Actual::Absent,
            Some(b) if b == efi::encode_u16(number) => Actual::Done(Detail::new()),
            Some(_) => Actual::Conflict(Refusal::new(
                codes::BOOT_NEXT_BUSY,
                "BootNext names another entry",
            )),
        })
    }

    // ----- remove --------------------------------------------------------------

    fn check_remove_boot_next(&mut self) -> Result<Actual, Refusal> {
        let Some(number) = self.owned_number() else {
            return Ok(Actual::Done(Detail::new()));
        };
        let current = self.platform.get_efi_var("BootNext").map_err(io_refusal)?;
        Ok(if current == Some(efi::encode_u16(number)) {
            Actual::Absent
        } else {
            Actual::Done(Detail::new())
        })
    }

    fn check_remove_boot_entry(&mut self) -> Result<Actual, Refusal> {
        let Some(owned) = self.record().owned_boot_entry.clone() else {
            return Ok(Actual::Done(Detail::new()));
        };
        let number = efi::parse_boot_number(&owned.number).map_err(io_refusal)?;
        let current = self
            .platform
            .get_efi_var(&efi::boot_var_name(number))
            .map_err(io_refusal)?;
        Ok(match current {
            None => Actual::Done(Detail::new()),
            Some(bytes) if sha_hex(&bytes) == owned.load_option_sha256 => Actual::Absent,
            Some(_) => Actual::Conflict(Refusal::new(
                codes::BOOT_ENTRY_MODIFIED,
                format!(
                    "Boot{} was changed by someone else; leaving it",
                    owned.number
                ),
            )),
        })
    }

    fn check_remove(&mut self, which: Which) -> Result<Actual, Refusal> {
        let disk = self.disk()?;
        let step = match which {
            Which::Esp => Step::RemoveEsp,
            Which::Root => Step::RemoveRoot,
        };
        match self.guid_for(which, self.record()) {
            // A known identity that is gone is removed, whatever now
            // occupies the extent (e.g. C: grown back over it).
            Some(g)
                if !disk
                    .partitions
                    .iter()
                    .any(|p| super::same_guid(&p.guid, &g)) =>
            {
                return Ok(Actual::Done(Detail::new()));
            }
            None if self.record().is_done(step) => return Ok(Actual::Done(Detail::new())),
            _ => {}
        }
        Ok(match self.find_staged(&disk, which) {
            Ok(None) => Actual::Done(Detail::new()),
            Ok(Some(_)) => Actual::Absent,
            Err(r) => Actual::Conflict(r),
        })
    }

    fn apply_remove(&mut self, which: Which) -> Result<(), Refusal> {
        let (disk, p) = self.require_staged(which)?;
        if self
            .plan()
            .basis
            .disk
            .partitions
            .iter()
            .any(|b| super::same_guid(&b.guid, &p.guid))
        {
            return Err(Refusal::new(
                codes::STAGING_CHANGED,
                "refusing to delete a partition that existed before staging",
            ));
        }
        self.platform
            .delete_partition(&disk, &p.guid)
            .map_err(io_refusal)?;
        let disk = self.disk()?;
        self.platform.sync_disk(&disk).map_err(io_refusal)
    }

    fn check_restore_shrink(&mut self) -> Result<Actual, Refusal> {
        let skipped = |why: &str| Ok(Actual::Done(detail(&[("skipped", why.into())])));
        let Some(shrink) = self.plan().shrink.clone() else {
            return skipped("no shrink planned");
        };
        if self.record().state(Step::Shrink).is_none() {
            return skipped("never shrunk");
        }
        let Some(volume) = self.platform.system_volume().map_err(io_refusal)? else {
            return skipped("no system volume");
        };
        if volume.size_bytes == shrink.from_bytes {
            return Ok(Actual::Done(Detail::new()));
        }
        if volume.size_bytes != shrink.to_bytes {
            return skipped("the system volume was resized by someone else");
        }
        let disk = self.disk()?;
        let s = disk.sector_bytes;
        let Some(c) = disk
            .partitions
            .iter()
            .find(|p| super::same_guid(&p.guid, &shrink.partition_guid))
        else {
            return skipped("the system volume is not on this disk");
        };
        let (first, last) = (c.last_lba + 1, c.first_lba + shrink.from_bytes / s - 1);
        if disk.partitions.iter().any(|p| overlaps(p, first, last)) {
            return skipped("the freed space is not contiguous with the system volume");
        }
        Ok(Actual::Absent)
    }

    // ----- actions ---------------------------------------------------------------

    fn stage(&mut self, authority: &dyn ReleaseAuthority) -> Result<(), Refusal> {
        self.resolve_release(authority)?;
        self.record = self.load_record()?;
        if self.record.is_none() {
            self.check_no_other_pending()?;
        }
        self.firmware_preconditions()?;
        self.check_basis()?;
        self.check_plan_geometry()?;
        self.open_components()?;
        self.verify_sources()?;
        if self.record.is_none() {
            let firmware = read_firmware(&*self.platform).map_err(io_refusal)?;
            let mut entries = std::collections::BTreeMap::new();
            for n in &firmware.boot_order {
                let number = efi::parse_boot_number(n).map_err(io_refusal)?;
                if let Some(bytes) = self
                    .platform
                    .get_efi_var(&efi::boot_var_name(number))
                    .map_err(io_refusal)?
                {
                    entries.insert(n.clone(), hex::encode(&bytes));
                }
            }
            self.record = Some(Record::new(
                self.request.operation_id.clone(),
                self.plan().clone(),
                self.request.release.clone(),
                OriginalFirmware {
                    boot_order: firmware.boot_order,
                    boot_next: firmware.boot_next,
                    entries,
                },
            ));
            self.save()?;
        }
        if self.record().bundle.is_none() {
            let facts = self.facts();
            let bundle = RecordedBundle {
                esp: RecordedComponent {
                    byte_size: facts.esp.byte_size,
                    sha256: facts.esp.sha256.clone(),
                    filesystem_uuid: Some(facts.esp.filesystem_uuid.clone()),
                },
                root: RecordedComponent {
                    byte_size: facts.root.byte_size,
                    sha256: facts.root.sha256.clone(),
                    filesystem_uuid: Some(facts.root.filesystem_uuid.clone()),
                },
            };
            self.record_mut().bundle = Some(bundle);
            self.save()?;
        }
        if self.record().state(Step::RemoveEsp).is_some()
            || self.record().state(Step::RemoveRoot).is_some()
        {
            return Err(Refusal::new(
                codes::REQUEST_INVALID,
                "this operation was removed; start a new one",
            ));
        }
        for step in Step::STAGE {
            self.run(step, step == Step::BootNext)?;
        }
        self.write_record_copy()
    }

    /// Authorised by the root-owned record alone: the request's manifest
    /// and component paths are ignored (the UI sends them empty, and the
    /// download may be gone). Readback is checked against the digests the
    /// record captured when `stage` verified the bundle.
    fn arm(&mut self) -> Result<(), Refusal> {
        self.record = self.load_record()?;
        let staged = self.record.as_ref().is_some_and(|r| {
            Step::STAGE[..8].iter().all(|s| r.is_done(*s))
                && Step::REMOVE.iter().all(|s| r.state(*s).is_none())
        });
        let bundle = self.record.as_ref().and_then(|r| r.bundle.clone());
        let (true, Some(bundle)) = (staged, bundle) else {
            return Err(Refusal::new(
                codes::NOT_STAGED,
                "nothing complete is staged to arm",
            ));
        };
        let component = |c: &RecordedComponent| super::bundle::ComponentFacts {
            byte_size: c.byte_size,
            sha256: c.sha256.clone(),
            minimum_partition_bytes: c.byte_size,
            filesystem_uuid: c.filesystem_uuid.clone().unwrap_or_default(),
        };
        let release = &self.record().release;
        self.facts = Some(BundleFacts {
            release_id: release.release_id.clone(),
            architecture: release.architecture.clone(),
            esp: component(&bundle.esp),
            root: component(&bundle.root),
            efi_path: self.plan().boot_entry.loader_path.replace('\\', "/"),
        });
        self.firmware_preconditions()?;
        for step in [
            Step::CreateEsp,
            Step::CreateRoot,
            Step::Verify,
            Step::BootEntry,
        ] {
            self.run(step, false)?;
        }
        self.run(Step::BootNext, true)?;
        self.write_record_copy()
    }

    fn remove(&mut self) -> Result<(), Refusal> {
        self.record = self.load_record()?;
        if self.record.is_none() {
            return Err(Refusal::new(
                codes::NOT_STAGED,
                "no staging record for this operation",
            ));
        }
        for step in Step::REMOVE {
            self.run(step, true)?;
        }
        Ok(())
    }
}

fn find_placeholders(
    read: &mut dyn FnMut(u64, &mut [u8]) -> Result<(), String>,
    size: u64,
) -> Result<Placeholders, Refusal> {
    let dir = super::ESP_STATE_DIR;
    let placeholder = |m: String| Refusal::new(codes::ESP_PLACEHOLDER, m);
    let handoff = fat::file_ranges(read, size, &format!("{dir}/{}", handoff::HANDOFF_FILE))
        .map_err(placeholder)?;
    let record = fat::file_ranges(read, size, &format!("{dir}/{}", super::ESP_RECORD_FILE))
        .map_err(placeholder)?;
    Ok(Placeholders { handoff, record })
}

// ----- process boundary ------------------------------------------------------

/// A running elevated helper launched by the UI.
pub struct Running {
    pub progress_path: std::path::PathBuf,
    child: Option<std::process::Child>,
    /// Private job directory; removed when the helper is dropped.
    _work: Option<tempfile::TempDir>,
    offset: u64,
    partial: String,
    latest: Option<Progress>,
    outcome: Option<Outcome>,
}

/// Snapshot of a running helper, read from `progress.jsonl`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Poll {
    pub latest: Option<Progress>,
    /// `Some` once the helper has written its final line or exited.
    pub outcome: Option<Outcome>,
}

/// A progress-file line.
#[derive(Deserialize)]
#[serde(untagged)]
enum Line {
    Outcome(Outcome),
    Progress(Progress),
}

impl Running {
    /// Observe a helper someone else started (tests, harnesses).
    pub fn watch(progress_path: PathBuf, child: Option<std::process::Child>) -> Self {
        Self {
            progress_path,
            child,
            _work: None,
            offset: 0,
            partial: String::new(),
            latest: None,
            outcome: None,
        }
    }

    pub fn poll(&mut self) -> Poll {
        self.read_new_lines();
        if self.outcome.is_none() {
            let exited = match self.child.as_mut() {
                Some(child) => child.try_wait().ok().flatten().map(|s| s.success()),
                None => None,
            };
            if let Some(success) = exited {
                // Consume bytes written between the last read and exit.
                self.read_new_lines();
                if self.outcome.is_none() {
                    self.outcome = Some(Outcome {
                        ok: false,
                        code: Some(codes::HELPER_EXITED.into()),
                        message: Some(if success {
                            "The installer helper stopped without reporting a result.".into()
                        } else {
                            "Authorization was cancelled or the installer helper stopped.".into()
                        }),
                    });
                }
            }
        }
        Poll {
            latest: self.latest.clone(),
            outcome: self.outcome.clone(),
        }
    }

    fn read_new_lines(&mut self) {
        let Ok(mut file) = std::fs::File::open(&self.progress_path) else {
            return;
        };
        if file.seek(SeekFrom::Start(self.offset)).is_err() {
            return;
        }
        let mut bytes = Vec::new();
        if file.take(1 << 20).read_to_end(&mut bytes).is_err() {
            return;
        }
        self.offset += bytes.len() as u64;
        self.partial.push_str(&String::from_utf8_lossy(&bytes));
        while let Some(end) = self.partial.find('\n') {
            let line: String = self.partial.drain(..=end).collect();
            match serde_json::from_str::<Line>(line.trim_end()) {
                Ok(Line::Outcome(o)) => self.outcome = Some(o),
                Ok(Line::Progress(p)) => self.latest = Some(p),
                Err(_) => {}
            }
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        // Never abandon an elevated writer: wait before the job directory goes.
        if let Some(child) = self.child.as_mut() {
            let _ = child.wait();
        }
    }
}

/// The executable the elevated helper runs (the AppImage when packaged).
fn helper_executable() -> std::io::Result<PathBuf> {
    #[cfg(target_os = "linux")]
    if let Some(appimage) = std::env::var_os("TILO_IMAGER_APPIMAGE") {
        let path = PathBuf::from(appimage);
        if path.is_absolute() && path.is_file() {
            return Ok(path);
        }
    }
    std::env::current_exe()
}

// `Start-Process` flattens `ArgumentList` into one command line, so each path
// is wrapped in literal double quotes (PowerShell `'"'`) to survive spaces.
#[cfg(any(test, windows))]
const WINDOWS_ELEVATION_SCRIPT: &str = r#"$p = Start-Process -FilePath $args[0] -Verb RunAs -Wait -PassThru -WindowStyle Hidden -ArgumentList @('--tilo-staging-helper', ('"' + $args[1] + '"'), ('"' + $args[2] + '"')); exit $p.ExitCode"#;

fn elevated_command(
    exe: &Path,
    request: &Path,
    progress: &Path,
) -> Result<std::process::Command, String> {
    #[cfg(target_os = "linux")]
    {
        let mut command = std::process::Command::new("pkexec");
        command
            .arg(exe)
            .arg(STAGING_HELPER_ARG)
            .arg(request)
            .arg(progress);
        Ok(command)
    }
    #[cfg(windows)]
    {
        let mut command = std::process::Command::new("powershell.exe");
        command
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                WINDOWS_ELEVATION_SCRIPT,
            ])
            .arg(exe)
            .arg(request)
            .arg(progress);
        Ok(command)
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = (exe, request, progress);
        Err("USB-free installation is available on Windows and Linux only".into())
    }
}

/// Write `request` to a private temp file and launch the elevated helper
/// (`pkexec` on Linux, `RunAs` on Windows).
pub fn launch(request: &Request) -> Result<Running, String> {
    use std::io::Write;
    validate_request(request).map_err(|r| r.to_string())?;
    let work = tempfile::Builder::new()
        .prefix("tilo-imager-staging-")
        .tempdir()
        .map_err(|e| e.to_string())?;
    let request_path = work.path().join("request.json");
    let progress_path = work.path().join("progress.jsonl");
    let bytes = serde_json::to_vec(request).map_err(|e| e.to_string())?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&request_path)
        .map_err(|e| e.to_string())?;
    file.write_all(&bytes).map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())?;
    drop(file);
    let exe = helper_executable().map_err(|e| e.to_string())?;
    let child = elevated_command(&exe, &request_path, &progress_path)?
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let mut running = Running::watch(progress_path, Some(child));
    running._work = Some(work);
    Ok(running)
}

/// Read the request with the caller's authority checks: a regular file, not
/// a symlink, writable only by root or the authenticated caller, in a
/// directory nobody else can swap it out of.
fn read_request(path: &Path) -> Result<Vec<u8>, String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let caller = ["PKEXEC_UID", "SUDO_UID"]
            .iter()
            .find_map(|k| std::env::var(k).ok())
            .map(|v| v.parse::<u32>().map_err(|_| "invalid caller UID"))
            .transpose()?
            .unwrap_or(0);
        let trusted = |m: &std::fs::Metadata| m.uid() == 0 || m.uid() == caller;
        let parent = path.parent().ok_or("request has no directory")?;
        let dir = std::fs::symlink_metadata(parent).map_err(|e| e.to_string())?;
        let mode = dir.permissions().mode();
        if !dir.is_dir() || !trusted(&dir) || (mode & 0o022 != 0 && mode & 0o1000 == 0) {
            return Err("the request directory is writable by others".into());
        }
        let meta = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
        if !meta.is_file() || !trusted(&meta) || meta.permissions().mode() & 0o022 != 0 {
            return Err("the request file is not owned safely".into());
        }
    }
    read_input(&path.to_string_lossy(), MAX_REQUEST_BYTES)
}

fn create_progress(path: &Path) -> Result<std::fs::File, String> {
    let mut options = std::fs::OpenOptions::new();
    // create_new refuses a pre-planted file or symlink.
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o644);
    }
    options
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// Test-only host overrides (loop disk, fake efivarfs, record directory)
/// for the Linux loop-device and VM harnesses. Honoured only in debug builds
/// or builds compiled with `TILO_IMAGER_STAGING_TEST_OVERRIDES=1`.
pub fn test_overrides_allowed() -> bool {
    cfg!(debug_assertions) || option_env!("TILO_IMAGER_STAGING_TEST_OVERRIDES") == Some("1")
}

fn system_store() -> Result<RecordStore, String> {
    let mut store = RecordStore::system()?;
    if test_overrides_allowed() {
        if let Some(dir) = std::env::var_os("TILO_STAGING_TEST_RECORD_DIR") {
            let dir = PathBuf::from(dir);
            store.public_dir = Some(dir.join("staging-status"));
            store.dir = dir.join("staging");
        }
    }
    Ok(store)
}

fn open_platform() -> Result<Box<dyn Platform>, String> {
    #[cfg(target_os = "linux")]
    {
        Ok(Box::new(super::linux::LinuxPlatform::open()?))
    }
    #[cfg(windows)]
    {
        Ok(Box::new(super::windows::WindowsPlatform::elevated()?))
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        Err("USB-free installation is available on Windows and Linux only".into())
    }
}

/// Run helper mode when `args` requests it; `Ok(None)` otherwise.
pub fn maybe_run_staging_helper(args: &[String]) -> Result<Option<i32>, String> {
    use std::io::Write;
    if args.get(1).map(String::as_str) != Some(STAGING_HELPER_ARG) {
        return Ok(None);
    }
    let (Some(request_path), Some(progress_path), None) = (args.get(2), args.get(3), args.get(4))
    else {
        return Err(format!(
            "usage: {STAGING_HELPER_ARG} <request.json> <progress.jsonl>"
        ));
    };
    #[cfg(unix)]
    if tilo_process::effective_uid() != 0 {
        return Err("the staging helper must run as root".into());
    }
    let bytes = read_request(Path::new(request_path))?;
    let request: Request = serde_json::from_slice(&bytes).map_err(|e| format!("request: {e}"))?;
    let mut progress = create_progress(Path::new(progress_path))?;
    let emit = |line: &[u8], file: &mut std::fs::File| {
        let _ = file.write_all(line);
        let _ = file.write_all(b"\n");
        let _ = file.flush();
    };
    let result = (|| {
        let mut platform = open_platform().map_err(io_refusal)?;
        let store = system_store().map_err(|m| Refusal::new(codes::RECORD_FAILED, m))?;
        let mut on_progress = |p: &Progress| {
            if let Ok(line) = serde_json::to_vec(p) {
                emit(&line, &mut progress);
            }
        };
        execute(
            &mut *platform,
            &store,
            &CompiledCatalog,
            &request,
            &mut on_progress,
        )
    })();
    let outcome = match &result {
        Ok(()) => Outcome {
            ok: true,
            code: None,
            message: None,
        },
        Err(r) => Outcome {
            ok: false,
            code: Some(r.code.into()),
            message: Some(r.message.clone()),
        },
    };
    let line = serde_json::to_vec(&outcome).map_err(|e| e.to_string())?;
    emit(&line, &mut progress);
    if let Err(r) = &result {
        eprintln!("tilo staging helper: {r}");
    }
    Ok(Some(i32::from(result.is_err())))
}

/// Unfinished staging records visible to this user (for the resume card),
/// read from the helper's world-readable status mirror.
pub fn pending_records() -> Vec<super::record::Record> {
    let dir = if test_overrides_allowed() {
        std::env::var_os("TILO_STAGING_TEST_RECORD_DIR")
            .map(|d| PathBuf::from(d).join("staging-status"))
            .or_else(super::record::public_dir)
    } else {
        super::record::public_dir()
    };
    dir.map(|d| super::record::pending_in(&d))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper elevation, mode entry and progress polling.
    #[test]
    fn staging_helper_mode_is_exact_and_polls_partial_writes() {
        // Phase `windows_elevation_quotes_both_paths_and_runs_only_the_helper_mode`.
        {
            assert!(WINDOWS_ELEVATION_SCRIPT.contains("'--tilo-staging-helper'"));
            assert!(WINDOWS_ELEVATION_SCRIPT.contains("('\"' + $args[1] + '\"')"));
            assert!(WINDOWS_ELEVATION_SCRIPT.contains("('\"' + $args[2] + '\"')"));
            assert!(WINDOWS_ELEVATION_SCRIPT.contains("-Verb RunAs"));
        }

        // Phase `poll_reads_progress_then_outcome_across_partial_writes`.
        {
            use std::io::Write;
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("progress.jsonl");
            let mut file = std::fs::File::create(&path).unwrap();
            let mut running = Running::watch(path, None);
            assert_eq!(running.poll(), Poll::default());
            file.write_all(br#"{"step":"write-root","done_bytes":5,"total_bytes":10}"#)
                .unwrap();
            assert_eq!(running.poll().latest, None, "no newline yet");
            file.write_all(b"\n{\"ok\":false,\"code\":\"plan-stale\",\"message\":\"x\"}\n")
                .unwrap();
            let poll = running.poll();
            assert_eq!(
                poll.latest,
                Some(Progress {
                    step: Step::WriteRoot,
                    done_bytes: 5,
                    total_bytes: 10
                })
            );
            assert_eq!(poll.outcome.unwrap().code.as_deref(), Some("plan-stale"));
        }

        // Phase `helper_mode_is_only_entered_by_its_exact_argument`.
        {
            assert_eq!(maybe_run_staging_helper(&["imager".into()]).unwrap(), None);
            assert_eq!(
                maybe_run_staging_helper(&["imager".into(), "--other".into()]).unwrap(),
                None
            );
            assert!(
                maybe_run_staging_helper(&["imager".into(), STAGING_HELPER_ARG.into()]).is_err()
            );
        }
    }
}
