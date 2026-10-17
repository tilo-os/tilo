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

pub use imager_backend::staging::protocol::{REQUEST_SCHEMA, STAGING_HELPER_ARG};
const MAX_MANIFEST_BYTES: u64 = crate::setup::internal_bundle::MAX_MANIFEST_BYTES as u64;
/// Streaming chunk for component writes and readback; a multiple of every
/// supported sector size.
const CHUNK: usize = 4 << 20;
const MIB: u64 = 1 << 20;

pub use imager_backend::staging::protocol::{Action, ComponentPaths};

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
    /// Metadata-only consent tied to the retained Linux source volume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub migration: Option<serde_json::Value>,
    /// Exact caller-private encrypted setup; never plaintext credentials.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prepared_setup: Option<super::prepared_setup::PreparedArtifact>,
}

pub use imager_backend::staging::protocol::{Outcome, Progress, Refusal, codes};

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
    prepared: Vec<Range>,
    receipt: Vec<Range>,
    browser: Vec<Range>,
    browser_receipt: Vec<Range>,
    browser_selection: Vec<Range>,
}

impl Placeholders {
    fn all(&self) -> Vec<Range> {
        self.handoff
            .iter()
            .chain(&self.record)
            .chain(&self.prepared)
            .chain(&self.receipt)
            .chain(&self.browser)
            .chain(&self.browser_receipt)
            .chain(&self.browser_selection)
            .copied()
            .collect()
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
    if request
        .plan
        .digest()
        .map_err(|error| Refusal::new(codes::REQUEST_INVALID, error.to_string()))?
        != request.plan_sha256
    {
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
    if let Some(raw) = &request.migration {
        #[cfg(target_os = "linux")]
        {
            let bytes = serde_json::to_vec(raw)
                .map_err(|e| Refusal::new(codes::REQUEST_INVALID, e.to_string()))?;
            let migration = crate::setup::migration_handoff::Plan::parse(&bytes)
                .map_err(|e| Refusal::new(codes::REQUEST_INVALID, e.to_string()))?;
            if migration.operation_id != request.operation_id {
                return bad("migration belongs to a different installation operation");
            }
            migration
                .validate_for_install(&request.plan)
                .map_err(|e| Refusal::new(codes::REQUEST_INVALID, e.to_string()))?;
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = raw;
            return bad("Linux migration cannot be authorized on this host");
        }
    }
    if let Some(prepared) = &request.prepared_setup {
        if !prepared.valid() || request.plan.basis.host_os() != super::plan::HostOs::Linux {
            return bad("Prepared setup requires bounded private Linux staging custody");
        }
        if let Some(expected) = request.profile.get("prepared_setup") {
            let claimed = serde_json::to_value(&prepared.expectation).map_err(io_refusal)?;
            if expected != &claimed {
                return bad("Prepared profile expectation differs from encrypted setup");
            }
        }
    }
    handoff::validate_profile(&request.profile)
        .map_err(|m| Refusal::new(codes::REQUEST_INVALID, m))?;
    Ok(())
}

pub use imager_backend::firmware::read_firmware;

/// A fresh basis from the platform.
pub fn read_basis(platform: &dyn Platform) -> super::platform::IoResult<Basis> {
    Ok(Basis {
        disk: platform.system_disk()?,
        system_volume: platform.system_volume()?,
        linux_btrfs_volume: platform.linux_btrfs_volume()?,
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

use imager_backend::staging::request_io::{open_input, read_input};

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

fn linux_binding(volume: &super::platform::LinuxBtrfsVolume) -> super::btrfs_resize::Binding {
    super::btrfs_resize::Binding {
        partition_guid: volume.partition_guid.clone(),
        filesystem_uuid: volume.filesystem_uuid.clone(),
        device_id: volume.device_id,
        mount_id: volume.mount_id,
        first_lba: volume.first_lba,
        sector_bytes: volume.sector_bytes,
    }
}

/// Mount IDs are invocation-local, not durable reboot identity. The canonical
/// adapter pins the newly admitted mount/namespace for this invocation; only
/// stable physical/filesystem facts compare against the protected old plan.
fn same_linux_volume(
    a: &super::platform::LinuxBtrfsVolume,
    b: &super::platform::LinuxBtrfsVolume,
) -> bool {
    a.partition_guid == b.partition_guid
        && a.filesystem_uuid == b.filesystem_uuid
        && a.device_id == b.device_id
        && a.first_lba == b.first_lba
        && a.sector_bytes == b.sector_bytes
}

#[derive(Clone, Copy)]
enum MigrationHandoffPurpose {
    /// Reservation sizing only; prospective geometry is never delivered here.
    Preflight,
    Delivery,
}

struct JournaledBtrfsVolume<'a, 'b> {
    engine: &'a mut Engine<'b>,
}
impl super::btrfs_resize::Volume for JournaledBtrfsVolume<'_, '_> {
    fn observe(&mut self) -> super::platform::IoResult<super::btrfs_resize::Facts> {
        let facts = self
            .engine
            .linux_volume()
            .map_err(|error| error.to_string())?;
        let minimum_bytes = super::plan::minimum_linux_btrfs_bytes(&facts)
            .map_err(|error| format!("Btrfs shrink bound changed: {error:?}"))?;
        Ok(super::btrfs_resize::Facts {
            binding: linux_binding(&facts),
            partition_bytes: facts.size_bytes,
            kernel_partition_bytes: facts.kernel_partition_bytes,
            filesystem_bytes: facts.filesystem_bytes,
            minimum_bytes,
            // The canonical native accessor admits only this exact supported layout.
            single_direct_device: true,
            mounted_writable: true,
            exclusive_operation_clear: true,
        })
    }
    fn record_intent(
        &mut self,
        phase: super::btrfs_resize::Phase,
        bytes: u64,
    ) -> super::platform::IoResult<()> {
        use super::btrfs_resize::Phase;
        let predecessor = match phase {
            Phase::ShrinkPartition => Some(Phase::ShrinkFilesystem),
            Phase::GrowFilesystem => Some(Phase::GrowPartition),
            _ => None,
        };
        if let Some(predecessor) = predecessor {
            let observed = self
                .engine
                .linux_volume()
                .map_err(|error| error.to_string())?;
            let exact = match predecessor {
                Phase::ShrinkFilesystem => observed.filesystem_bytes == bytes,
                Phase::GrowPartition => {
                    observed.size_bytes == bytes && observed.kernel_partition_bytes == bytes
                }
                _ => false,
            };
            if !exact {
                return Err("Btrfs preceding phase readback changed".into());
            }
            if predecessor == Phase::GrowPartition
                && self
                    .engine
                    .record()
                    .resize_checkpoint(predecessor)?
                    .is_none()
            {
                // Filesystem-only shrink needs no GPT growth, but its exact
                // original geometry still gets a durable intent/readback pair.
                if [Step::RemoveRoot, Step::RemoveEsp]
                    .iter()
                    .any(|step| !self.engine.record().is_done(*step))
                {
                    return Err("Btrfs restore precedes owned partition retirement".into());
                }
                self.engine.record_mut().resize_intent(predecessor, bytes)?;
                self.engine.save().map_err(|error| error.to_string())?;
            }
            self.engine
                .finish_resize_phase(predecessor, bytes)
                .map_err(|error| error.to_string())?;
            if self
                .engine
                .record()
                .resize_checkpoint(predecessor)?
                .is_none_or(|checkpoint| checkpoint.state != StepState::Done)
            {
                return Err("Btrfs phase lacks durable preceding readback".into());
            }
        }
        self.engine.record_mut().resize_intent(phase, bytes)?;
        self.engine.save().map_err(|error| error.to_string())
    }
    fn record_done(
        &mut self,
        phase: super::btrfs_resize::Phase,
        bytes: u64,
    ) -> super::platform::IoResult<()> {
        self.engine.record_mut().resize_done(phase, bytes)?;
        self.engine.save().map_err(|error| error.to_string())
    }
    fn resize_filesystem(&mut self, bytes: u64) -> super::platform::IoResult<()> {
        let guid = self
            .engine
            .plan()
            .shrink
            .as_ref()
            .expect("reviewed shrink")
            .partition_guid
            .clone();
        self.engine
            .platform
            .resize_linux_btrfs_filesystem(&guid, bytes)
    }
    fn resize_partition(&mut self, bytes: u64) -> super::platform::IoResult<()> {
        let guid = self
            .engine
            .plan()
            .shrink
            .as_ref()
            .expect("reviewed shrink")
            .partition_guid
            .clone();
        self.engine
            .platform
            .resize_linux_btrfs_partition(&guid, bytes)
    }
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
            if r.plan_sha256 != self.request.plan_sha256
                || r.release != self.request.release
                || self
                    .request
                    .migration
                    .as_ref()
                    .is_some_and(|requested| Some(requested) != r.migration.as_ref())
                || self
                    .request
                    .prepared_setup
                    .as_ref()
                    .is_some_and(|requested| {
                        Some(&requested.expectation) != r.prepared_setup.as_ref()
                            || requested.browser_expectation() != r.browser_expectation()
                    })
                || (!self.request.profile.is_empty() && self.request.profile != r.profile)
            {
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
        if !firmware.uefi {
            return Err(Refusal::new(
                codes::REQUEST_INVALID,
                "UEFI firmware is unavailable; preparation geometry does not admit installation",
            ));
        }
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
        self.normalize_linux_btrfs_basis(&mut current)?;
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
        match (&current.linux_btrfs_volume, &expected.linux_btrfs_volume) {
            (None, None) => {}
            (Some(c), Some(e))
                if same_linux_volume(c, e)
                    && c.size_bytes == e.size_bytes
                    && c.filesystem_bytes == e.filesystem_bytes
                    && c.kernel_partition_bytes == e.kernel_partition_bytes
                    && c.reserve_bytes == e.reserve_bytes => {}
            _ => return stale("the retained Btrfs volume changed since review".into()),
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
                if self.plan().basis.linux_btrfs_volume.is_some() {
                    return self.apply_linux_btrfs_resize(true);
                }
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
        if self.plan().basis.linux_btrfs_volume.is_some() {
            return self.check_linux_btrfs_resize(false);
        }
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
        if self.plan().basis.linux_btrfs_volume.is_some() {
            return self.apply_linux_btrfs_resize(false);
        }
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

    fn linux_volume(&self) -> Result<super::platform::LinuxBtrfsVolume, Refusal> {
        let expected = self
            .plan()
            .basis
            .linux_btrfs_volume
            .as_ref()
            .ok_or_else(|| Refusal::new(codes::PLAN_STALE, "no reviewed Btrfs binding"))?;
        let actual = self
            .platform
            .linux_btrfs_volume()
            .map_err(io_refusal)?
            .ok_or_else(|| {
                Refusal::new(codes::STAGING_CHANGED, "retained Btrfs volume is absent")
            })?;
        if !same_linux_volume(&actual, expected) || actual.reserve_bytes != expected.reserve_bytes {
            return Err(Refusal::new(
                codes::STAGING_CHANGED,
                "retained Btrfs binding changed",
            ));
        }
        let disk = self.disk()?;
        let partition = disk
            .partitions
            .iter()
            .find(|partition| super::same_guid(&partition.guid, &actual.partition_guid))
            .ok_or_else(|| {
                Refusal::new(codes::STAGING_CHANGED, "retained Btrfs partition is absent")
            })?;
        let bytes = partition
            .last_lba
            .checked_sub(partition.first_lba)
            .and_then(|span| span.checked_add(1))
            .and_then(|span| span.checked_mul(disk.sector_bytes));
        if partition.first_lba != actual.first_lba
            || disk.sector_bytes != actual.sector_bytes
            || bytes != Some(actual.size_bytes)
            || actual.filesystem_bytes > actual.size_bytes
            || actual.filesystem_bytes > actual.kernel_partition_bytes
        {
            return Err(Refusal::new(
                codes::STAGING_CHANGED,
                "retained Btrfs geometry disagrees",
            ));
        }
        Ok(actual)
    }

    fn resize_authorized(&self, phase: super::btrfs_resize::Phase) -> Result<bool, Refusal> {
        self.record
            .as_ref()
            .map(|record| {
                record
                    .resize_checkpoint(phase)
                    .map(|checkpoint| checkpoint.is_some())
            })
            .transpose()
            .map(|authorized| authorized.unwrap_or(false))
            .map_err(io_refusal)
    }

    /// Normalize only exact journaled resize effects, never an arbitrary map.
    fn normalize_linux_btrfs_basis(&self, current: &mut Basis) -> Result<(), Refusal> {
        use super::btrfs_resize::Phase;
        let Some(expected) = self.plan().basis.linux_btrfs_volume.as_ref() else {
            return Ok(());
        };
        let Some(actual) = current.linux_btrfs_volume.as_mut() else {
            return Err(Refusal::new(
                codes::PLAN_STALE,
                "retained Btrfs volume disappeared",
            ));
        };
        if !same_linux_volume(actual, expected) || !matches!(actual.sector_bytes, 512 | 4096) {
            return Err(Refusal::new(
                codes::PLAN_STALE,
                "retained Btrfs binding changed",
            ));
        }
        let Some(shrink) = self.plan().shrink.as_ref() else {
            return Ok(());
        };
        let (from, to) = (shrink.from_bytes, shrink.to_bytes);
        if actual.filesystem_bytes == from
            && actual.size_bytes == from
            && actual.kernel_partition_bytes == from
        {
            return Ok(());
        }
        let filesystem = self.resize_authorized(Phase::ShrinkFilesystem)?;
        let partition = self.resize_authorized(Phase::ShrinkPartition)?;
        let growing = self.resize_authorized(Phase::GrowPartition)?;
        let shrinking = actual.filesystem_bytes == to
            && filesystem
            && ((actual.size_bytes == from && actual.kernel_partition_bytes == from)
                || (actual.size_bytes == to
                    && [from, to].contains(&actual.kernel_partition_bytes)
                    && partition)
                || (actual.size_bytes == from && actual.kernel_partition_bytes == to && growing));
        if !shrinking {
            return Err(Refusal::new(
                codes::PLAN_STALE,
                "retained Btrfs size lacks exact resize intent",
            ));
        }
        let entry = current
            .disk
            .partitions
            .iter_mut()
            .find(|partition| super::same_guid(&partition.guid, &shrink.partition_guid))
            .ok_or_else(|| {
                Refusal::new(codes::PLAN_STALE, "retained Btrfs GPT entry disappeared")
            })?;
        let actual_last = actual
            .first_lba
            .checked_add(actual.size_bytes / actual.sector_bytes)
            .and_then(|end| end.checked_sub(1));
        if entry.first_lba != actual.first_lba || Some(entry.last_lba) != actual_last {
            return Err(Refusal::new(
                codes::PLAN_STALE,
                "retained Btrfs GPT readback differs",
            ));
        }
        entry.last_lba = actual
            .first_lba
            .checked_add(from / actual.sector_bytes)
            .and_then(|end| end.checked_sub(1))
            .ok_or_else(|| Refusal::new(codes::PLAN_STALE, "retained Btrfs extent overflow"))?;
        actual.size_bytes = from;
        actual.filesystem_bytes = from;
        actual.kernel_partition_bytes = from;
        Ok(())
    }

    fn check_linux_btrfs_resize(&mut self, restoring: bool) -> Result<Actual, Refusal> {
        use super::btrfs_resize::Phase;
        let shrink = self.plan().shrink.clone().expect("reviewed shrink");
        let volume = self.linux_volume()?;
        let (from, to) = (shrink.from_bytes, shrink.to_bytes);
        let fs_intent = self.resize_authorized(Phase::ShrinkFilesystem)?;
        let part_intent = self.resize_authorized(Phase::ShrinkPartition)?;
        if !restoring {
            if volume.size_bytes == from
                && volume.kernel_partition_bytes == from
                && volume.filesystem_bytes == from
            {
                return Ok(Actual::Absent);
            }
            if volume.filesystem_bytes == to && fs_intent {
                self.finish_resize_phase(Phase::ShrinkFilesystem, to)?;
                if volume.size_bytes == to && volume.kernel_partition_bytes == to && part_intent {
                    self.finish_resize_phase(Phase::ShrinkPartition, to)?;
                    return Ok(Actual::Done(Detail::new()));
                }
                if (volume.size_bytes == from && volume.kernel_partition_bytes == from)
                    || (volume.size_bytes == to
                        && volume.kernel_partition_bytes == from
                        && part_intent)
                {
                    return Ok(Actual::Absent);
                }
            }
            return Err(Refusal::new(
                codes::STAGING_CHANGED,
                "Btrfs shrink requires explicit recovery",
            ));
        }
        if volume.size_bytes == from
            && volume.kernel_partition_bytes == from
            && volume.filesystem_bytes == from
        {
            if self.resize_authorized(Phase::GrowPartition)? {
                self.finish_resize_phase(Phase::GrowPartition, from)?;
            }
            if self.resize_authorized(Phase::GrowFilesystem)? {
                self.finish_resize_phase(Phase::GrowFilesystem, from)?;
            }
            return Ok(Actual::Done(Detail::new()));
        }
        let growth = self.resize_authorized(Phase::GrowPartition)?;
        let fs_growth = self.resize_authorized(Phase::GrowFilesystem)?;
        if !fs_intent
            || volume.filesystem_bytes < to
            || volume.filesystem_bytes > from
            || ![from, to].contains(&volume.size_bytes)
            || ![from, to].contains(&volume.kernel_partition_bytes)
            || (volume.size_bytes == to && !part_intent)
            || (volume.size_bytes == from && volume.kernel_partition_bytes == to && !growth)
            || (volume.filesystem_bytes > to && !fs_growth && volume.size_bytes != from)
        {
            return Err(Refusal::new(
                codes::STAGING_CHANGED,
                "Btrfs restore lacks journaled geometry",
            ));
        }
        // Existing Remove ordering has already retired both owned staging entries.
        let disk = self.disk()?;
        let partition = disk
            .partitions
            .iter()
            .find(|partition| super::same_guid(&partition.guid, &shrink.partition_guid))
            .ok_or_else(|| {
                Refusal::new(codes::STAGING_CHANGED, "retained partition disappeared")
            })?;
        let last = partition
            .first_lba
            .checked_add(from / disk.sector_bytes)
            .and_then(|end| end.checked_sub(1))
            .ok_or_else(|| Refusal::new(codes::STAGING_CHANGED, "restore extent overflow"))?;
        if partition.last_lba < last
            && disk
                .partitions
                .iter()
                .any(|other| overlaps(other, partition.last_lba + 1, last))
        {
            return Err(Refusal::new(
                codes::STAGING_CHANGED,
                "retained restore extent is occupied",
            ));
        }
        Ok(Actual::Absent)
    }

    fn finish_resize_phase(
        &mut self,
        phase: super::btrfs_resize::Phase,
        bytes: u64,
    ) -> Result<(), Refusal> {
        if self
            .record()
            .resize_checkpoint(phase)
            .map_err(io_refusal)?
            .is_some_and(|checkpoint| checkpoint.state == StepState::Intended)
        {
            self.record_mut()
                .resize_done(phase, bytes)
                .map_err(io_refusal)?;
            self.save()?;
        }
        Ok(())
    }

    fn apply_linux_btrfs_resize(&mut self, restoring: bool) -> Result<(), Refusal> {
        // Re-admit stable reviewed identity, then pin the current invocation's
        // actual mount ID throughout all mutation/readback steps.
        let current = self.linux_volume()?;
        let binding = linux_binding(&current);
        let shrink = self.plan().shrink.clone().expect("reviewed shrink");
        let mut volume = JournaledBtrfsVolume { engine: self };
        if restoring {
            super::btrfs_resize::restore(&mut volume, &binding, shrink.from_bytes, shrink.to_bytes)
        } else {
            super::btrfs_resize::shrink(
                &mut volume,
                &binding,
                shrink.from_bytes,
                shrink.to_bytes,
                &|| false,
            )
        }
        .map_err(io_refusal)
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

    fn prepared_migration_handoff(
        &mut self,
        purpose: MigrationHandoffPurpose,
    ) -> Result<Option<serde_json::Value>, Refusal> {
        let Some(raw) = self.record().migration.clone() else {
            return Ok(None);
        };
        #[cfg(target_os = "linux")]
        {
            if self.plan().shrink.is_some() {
                if self.plan().basis.linux_btrfs_volume.is_none() {
                    return Err(Refusal::new(
                        codes::STAGING_CHANGED,
                        "Migration resize lacks reviewed Btrfs binding",
                    ));
                }
                if matches!(purpose, MigrationHandoffPurpose::Delivery)
                    && (!self.record().is_done(Step::Shrink)
                        || !matches!(self.check_linux_btrfs_resize(false)?, Actual::Done(_)))
                {
                    return Err(Refusal::new(
                        codes::STAGING_CHANGED,
                        "Migration geometry cannot be delivered before verified Btrfs shrink",
                    ));
                }
                let original = crate::setup::migration_handoff::Plan::parse(
                    &serde_json::to_vec(&raw).map_err(io_refusal)?,
                )
                .map_err(io_refusal)?;
                let completed = original
                    .for_completed_staging(self.plan())
                    .map_err(io_refusal)?;
                return serde_json::to_value(completed)
                    .map(Some)
                    .map_err(io_refusal);
            }
        }
        #[cfg(not(target_os = "linux"))]
        let _ = purpose;
        Ok(Some(raw))
    }

    fn handoff_bytes(&mut self, number: u16, len: u64) -> Result<Vec<u8>, Refusal> {
        let (_, esp) = self.require_staged(Which::Esp)?;
        let (_, root) = self.require_staged(Which::Root)?;
        let mut handoff = handoff::for_plan(
            &self.request.operation_id,
            self.plan(),
            &esp.guid,
            &root.guid,
            &efi::format_boot_number(number),
            self.record().original_firmware.boot_order.clone(),
            self.record().profile.clone(),
        );
        handoff.migration = self
            .prepared_migration_handoff(MigrationHandoffPurpose::Delivery)?
            .map(serde_json::from_value)
            .transpose()
            .map_err(io_refusal)?;
        if let Some(expected) = &self.record().prepared_setup {
            handoff.profile.insert(
                "prepared_setup".into(),
                serde_json::to_value(expected).map_err(io_refusal)?,
            );
        }
        handoff.validate().map_err(io_refusal)?;
        handoff
            .to_padded(len)
            .map_err(|m| Refusal::new(codes::ESP_PLACEHOLDER, m))
    }

    fn retain_prepared_setup(&self) -> Result<(), Refusal> {
        let Some(expected) = &self.record().prepared_setup else {
            return Ok(());
        };
        let slots = self.placeholders.as_ref().ok_or_else(|| {
            Refusal::new(codes::ESP_PLACEHOLDER, "Prepared setup slots unavailable")
        })?;
        let package_len = fat::ranges_len(&slots.prepared);
        let receipt_len = fat::ranges_len(&slots.receipt);
        if package_len < expected.sealed_bytes
            || package_len > super::prepared_setup::SLOT_BYTES
            || receipt_len != super::prepared_setup::RECEIPT_BYTES
        {
            return Err(Refusal::new(
                codes::ESP_PLACEHOLDER,
                "This installer bundle lacks bounded encrypted setup reservations; download a compatible installer",
            ));
        }
        if let Some(artifact) = &self.request.prepared_setup {
            let bytes = artifact
                .read(super::prepared_setup::caller_uid().map_err(io_refusal)?)
                .map_err(io_refusal)?;
            self.store
                .retain_prepared(&self.request.operation_id, &bytes, expected)
                .map_err(io_refusal)?;
        } else {
            self.store
                .read_prepared(&self.request.operation_id, expected)
                .map_err(io_refusal)?;
        }
        Ok(())
    }

    fn prepared_payloads(
        &self,
        slots: &Placeholders,
    ) -> Result<Option<(Vec<u8>, Vec<u8>)>, Refusal> {
        let Some(expected) = &self.record().prepared_setup else {
            return Ok(None);
        };
        let mut bytes = self
            .store
            .read_prepared(&self.request.operation_id, expected)
            .map_err(io_refusal)?;
        let package_len = fat::ranges_len(&slots.prepared);
        if package_len < expected.sealed_bytes
            || package_len > super::prepared_setup::SLOT_BYTES
            || fat::ranges_len(&slots.receipt) != super::prepared_setup::RECEIPT_BYTES
        {
            return Err(Refusal::new(
                codes::ESP_PLACEHOLDER,
                "Prepared setup slot changed",
            ));
        }
        bytes.resize(usize::try_from(package_len).map_err(io_refusal)?, b' ');
        let receipt = handoff::pad_json(
            &serde_json::to_vec(expected).map_err(io_refusal)?,
            super::prepared_setup::RECEIPT_BYTES,
        )
        .map_err(io_refusal)?;
        Ok(Some((bytes, receipt)))
    }

    fn check_prepared_setup(&mut self, disk: &Disk, guid: &str) -> Result<bool, Refusal> {
        let slots = self.placeholders(disk, guid)?;
        #[cfg(target_os = "linux")]
        if !self.check_browser(disk, guid, &slots)? {
            return Ok(false);
        }
        let Some((bytes, receipt)) = self.prepared_payloads(&slots)? else {
            return Ok(true);
        };
        Ok(self.read_ranges(disk, guid, &slots.prepared)? == bytes
            && self.read_ranges(disk, guid, &slots.receipt)? == receipt)
    }

    fn deliver_prepared_setup(&mut self, disk: &Disk, guid: &str) -> Result<(), Refusal> {
        let slots = self.placeholders(disk, guid)?;
        let Some((bytes, receipt)) = self.prepared_payloads(&slots)? else {
            return Ok(());
        };
        self.write_ranges(disk, guid, &slots.prepared, &bytes)?;
        self.write_ranges(disk, guid, &slots.receipt, &receipt)?;
        self.platform.sync_disk(disk).map_err(io_refusal)?;
        if !self.check_prepared_setup(disk, guid)? {
            return Err(Refusal::new(
                codes::IO_FAILED,
                "Prepared ciphertext failed target readback",
            ));
        }
        Ok(())
    }

    fn preflight_migration_transport(&mut self) -> Result<(), Refusal> {
        let placeholders = self.placeholders.as_ref().ok_or_else(|| {
            Refusal::new(
                codes::ESP_PLACEHOLDER,
                "Verified ESP reservations are unavailable",
            )
        })?;
        let len = placeholders
            .handoff
            .iter()
            .try_fold(0u64, |total, range| total.checked_add(range.len))
            .ok_or_else(|| {
                Refusal::new(codes::ESP_PLACEHOLDER, "ESP reservation length overflow")
            })?;
        let mut handoff = handoff::for_plan(
            &self.request.operation_id,
            self.plan(),
            "00000000-0000-4000-8000-000000000001",
            "00000000-0000-4000-8000-000000000002",
            "FFFF",
            self.record().original_firmware.boot_order.clone(),
            self.record().profile.clone(),
        );
        handoff.migration = self
            .prepared_migration_handoff(MigrationHandoffPurpose::Preflight)?
            .map(serde_json::from_value)
            .transpose()
            .map_err(io_refusal)?;
        if let Some(expected) = &self.record().prepared_setup {
            handoff.profile.insert(
                "prepared_setup".into(),
                serde_json::to_value(expected).map_err(io_refusal)?,
            );
        }
        handoff.validate().map_err(io_refusal)?;
        handoff
            .to_padded(len)
            .map_err(|message| Refusal::new(codes::ESP_PLACEHOLDER, message))?;
        Ok(())
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
        if !self.check_prepared_setup(&disk, &esp.guid)? {
            return Ok(Actual::Conflict(Refusal::new(
                codes::STAGING_CHANGED,
                "Prepared ciphertext delivery changed",
            )));
        }
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
        #[cfg(target_os = "linux")]
        self.deliver_browser(&disk, &esp.guid)?;
        self.deliver_prepared_setup(&disk, &esp.guid)?;
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
        if self.plan().basis.linux_btrfs_volume.is_some() {
            return self.check_linux_btrfs_resize(true);
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
        if self.record.is_none()
            && self.request.prepared_setup.is_none()
            && self.request.profile.contains_key("prepared_setup")
        {
            return Err(Refusal::new(
                codes::REQUEST_INVALID,
                "Prepared profile requires actual encrypted setup custody",
            ));
        }
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
            let mut record = Record::new(
                self.request.operation_id.clone(),
                self.plan().clone(),
                self.request.release.clone(),
                OriginalFirmware {
                    boot_order: firmware.boot_order,
                    boot_next: firmware.boot_next,
                    entries,
                },
            )
            .map_err(|error| Refusal::new(codes::REQUEST_INVALID, error))?;
            record.migration.clone_from(&self.request.migration);
            record.profile.clone_from(&self.request.profile);
            record.prepared_setup = self
                .request
                .prepared_setup
                .as_ref()
                .map(|artifact| artifact.expectation.clone());
            #[cfg(target_os = "linux")]
            {
                record.prepared_browser = self
                    .request
                    .prepared_setup
                    .as_ref()
                    .and_then(|artifact| artifact.browser.as_ref())
                    .map(|browser| browser.expectation.clone());
            }
            self.record = Some(record);
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
        self.retain_prepared_setup()?;
        #[cfg(target_os = "linux")]
        self.retain_browser()?;
        self.preflight_migration_transport()?;
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
            // ESP component hashing masks declared setup reservations. Recheck
            // their exact receipt/ciphertext and private custody before arming.
            Step::Handoff,
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
    let prepared =
        fat::file_ranges(read, size, super::prepared_setup::PACKAGE_PATH).unwrap_or_default();
    let receipt =
        fat::file_ranges(read, size, super::prepared_setup::RECEIPT_PATH).unwrap_or_default();
    Ok(Placeholders {
        handoff,
        record,
        prepared,
        receipt,
        browser: fat::file_ranges(read, size, "/.tilo-installer/browser.sealed")
            .unwrap_or_default(),
        browser_receipt: fat::file_ranges(read, size, "/.tilo-installer/browser-receipt.json")
            .unwrap_or_default(),
        browser_selection: fat::file_ranges(read, size, "/.tilo-installer/browser-selection.json")
            .unwrap_or_default(),
    })
}

// ----- process boundary ------------------------------------------------------

pub use imager_backend::staging::process::{Poll, Running, helper_executable};

/// Validate the reviewed request before launching the native helper transport.
pub fn launch(request: &Request) -> Result<Running, String> {
    validate_request(request).map_err(|r| r.to_string())?;
    let bytes = serde_json::to_vec(request).map_err(|e| e.to_string())?;
    imager_backend::staging::process::launch(&bytes)
}

use imager_backend::staging::request_io::{create_progress, read_request};

pub use imager_backend::firmware::test_overrides_allowed;

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
    #[test]
    fn durable_btrfs_recovery_readmits_mount_but_keeps_stable_identity() {
        let original = super::super::platform::LinuxBtrfsVolume {
            partition_guid: "11111111-2222-3333-4444-555555555555".into(),
            filesystem_uuid: "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee".into(),
            device_id: 1,
            mount_id: 42,
            first_lba: 2048,
            sector_bytes: 512,
            size_bytes: 128 << 30,
            filesystem_bytes: 128 << 30,
            kernel_partition_bytes: 128 << 30,
            supported_min_bytes: 32 << 30,
            reserve_bytes: 4 << 30,
        };
        let mut remounted = original.clone();
        remounted.mount_id = 117;
        assert!(super::same_linux_volume(&original, &remounted));
        // Mutation still pins the newly held invocation, never the old mount.
        assert_ne!(
            super::linux_binding(&original),
            super::linux_binding(&remounted)
        );
        assert_eq!(super::linux_binding(&remounted).mount_id, 117);
        remounted.filesystem_uuid = "ffffffff-bbbb-cccc-dddd-eeeeeeeeeeee".into();
        assert!(!super::same_linux_volume(&original, &remounted));
        remounted = original.clone();
        remounted.first_lba += 1;
        assert!(!super::same_linux_volume(&original, &remounted));
    }

    use super::*;

    /// Helper elevation, mode entry and progress polling.
    #[test]
    fn staging_helper_mode_is_exact_and_polls_partial_writes() {
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

#[cfg(target_os = "linux")]
mod browser;
