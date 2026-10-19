//! Request handling: one [`Daemon`] per RPC connection.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::Serialize;
use tilo_vm_protocol::{
    Action, AppInstallResult, ApplyResult, AwakeLease, BaseInfo, BootConfirmResult, ComponentInfo,
    DEVICE_SLOT, GcResult, HelloResult, ImageChange, InputAction, InputResult, Issue, KeptLayer,
    LayerImportResult, LayerSource, LayerUploadBeginResult, LayerUploadChunkResult, Lease,
    LogsResult, Manifest, OutputSpec, Owner, PROTOCOL_VERSION, Policy, Provenance, Request, Scope,
    SeatResult, ShotResult, SlotCreateResult, SlotFile, SlotInfo, SlotKind, SlotRmResult,
    SlotShowResult, StatusResult, Storage, TestIdentity, UPLOAD_CHUNK_BYTES, VM_SLOT, VmTools,
};

use crate::apply::{self, Apply, GREETER_UNIT, Restarts, Session, SessionChange};
use crate::awake;
use crate::error::DaemonError;
use crate::image;
use crate::layers::LayerStore;
use crate::locks::{Guard, Locks};
use crate::slots::{self, Generation, SlotRecord, SlotStore};
use crate::targets::TargetStore;

/// The domain of a greetd session unless the image says otherwise.
pub const DEFAULT_DOMAIN: &str = "unconfined_t";

mod runtime_retention;
/// Layers and uploads younger than this survive gc: they may belong to a
/// swap that is still in flight on another connection.
mod userspace;

const GC_MIN_AGE: Duration = Duration::from_secs(3600);

/// How long a started session may take to bind its control socket.
const CONTROL_SOCKET_TIMEOUT: Duration = Duration::from_secs(30);

/// In-VM build dirs `gc` keeps by default, most recently used first.
pub const DEFAULT_KEEP_TARGETS: u32 = 2;

pub struct Daemon {
    pub(crate) layers: LayerStore,
    slots: SlotStore,
    pub(crate) locks: Locks,
    pub(crate) targets: TargetStore,
    uploads_dir: PathBuf,
    uploads: BTreeMap<String, PathBuf>,
    pub(crate) apply: Box<dyn Apply>,
    manifest: Option<Manifest>,
    pub(crate) base: BaseInfo,
    client_protocol: Option<u32>,
    pub(crate) root: PathBuf,
    expected_domain: String,
    /// Problems found re-creating vkms seats when this process started.
    seat_issues: Option<Vec<Issue>>,
}

/// Is `tool` on `PATH` (or in `/usr/local/bin`, which a non-login SSH
/// shell may lack)?
pub fn on_path(tool: &str) -> bool {
    let paths = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&paths)
        .chain([PathBuf::from("/usr/local/bin")])
        .any(|dir| dir.join(tool).is_file())
}

/// What this VM can build and pull with.
pub fn detect_tools() -> VmTools {
    VmTools {
        cargo: on_path("cargo"),
        rsync: on_path("rsync"),
        python3: on_path("python3"),
        sccache: on_path("sccache"),
        podman: on_path("podman"),
    }
}

/// Re-create the vkms card of every headful slot. configfs is empty after
/// a reboot, and a slot whose card is missing cannot start its session.
/// Slots another command is changing are left to that command.
pub fn reconcile_seats(slots: &SlotStore, locks: &Locks, apply: &mut dyn Apply) -> Vec<Issue> {
    let mut issues = Vec::new();
    for record in slots.list().unwrap_or_default() {
        if record.kind != SlotKind::Headful || record.seat.is_none() {
            continue;
        }
        let Ok(_guard) = locks.slot(&record.name) else {
            continue;
        };
        if let Err(error) = apply.allocate_seat(&record.name, record.connectors()) {
            issues.push(Issue {
                slot: record.name.clone(),
                problem: format!("its vkms seat could not be re-created: {error}"),
                hint: format!(
                    "tilo-vm slot restart {}   (tilo-vm doctor checks vkms and configfs)",
                    record.name
                ),
            });
        }
    }
    issues
}

fn layer_paths(layers: &LayerStore, generation: &Generation) -> Result<Vec<PathBuf>, DaemonError> {
    layers.merged_paths(&generation.layers)
}

fn system_layers(
    slots: &SlotStore,
    layers: &LayerStore,
    record: &SlotRecord,
    own: Vec<PathBuf>,
) -> Result<Vec<PathBuf>, DaemonError> {
    let login = |r: &SlotRecord| r.device.as_ref().is_some_and(|d| d.login.is_some());
    let other = match record.kind {
        SlotKind::Vm => DEVICE_SLOT,
        SlotKind::Device if login(record) => VM_SLOT,
        _ => {
            let vm = slots.load(VM_SLOT)?;
            let vm_layers = layer_paths(layers, &slots.generation(VM_SLOT, vm.current)?)?;
            let mut merged = system_layers(slots, layers, &vm, vm_layers)?;
            for layer in own {
                if !merged.contains(&layer) {
                    merged.push(layer);
                }
            }
            return Ok(merged);
        }
    };
    let Some(stored) = slots.stored(other)? else {
        return Ok(own);
    };
    if other == DEVICE_SLOT && !login(&stored) {
        return Ok(own);
    }
    let theirs = layer_paths(layers, &slots.generation(other, stored.current)?)?;
    let (first, second) = if record.kind == SlotKind::Vm {
        (own, theirs)
    } else {
        (theirs, own)
    };
    let mut layers: Vec<PathBuf> = Vec::new();
    for layer in first.into_iter().chain(second) {
        if !layers.contains(&layer) {
            layers.push(layer);
        }
    }
    Ok(layers)
}

fn prepare_slot_session(
    slots: &SlotStore,
    layers: &LayerStore,
    apply: &mut dyn Apply,
    record: &SlotRecord,
    system: Option<&[PathBuf]>,
) -> Result<(), DaemonError> {
    let session = record
        .session()
        .ok_or_else(|| DaemonError::InvalidParams("slot has no session".into()))?;
    let generation = slots.generation(&record.name, record.current)?;
    let own = layer_paths(layers, &generation)?;
    let paths = if let Some(system) = system {
        let mut paths = system.to_vec();
        for layer in own {
            if !paths.contains(&layer) {
                paths.push(layer);
            }
        }
        paths
    } else {
        system_layers(slots, layers, record, own)?
    };
    let mut dropin = apply.session_layer_dropin(&record.name, generation.number, &paths)?;
    if session.stand_in {
        dropin.push_str(&apply.slot_identity_environment(&session.user)?);
    }
    let uid = apply::session_uid(apply, &session)?;
    for unit in apply::layered_units(&session, uid) {
        apply.write_dropin(&unit, apply::LAYERS_DROPIN, &dropin)?;
    }
    apply.reload()
}

/// Link and merge the VM-wide sysext set, the `vm` slot's layers and the
/// owner's login's (the `device` slot), as their current generations name
/// them. `systemd-sysext.service` merges at boot whatever links
/// `/var/lib/extensions` holds; when those are not exactly this set (a
/// transition cut short, a link lost or left behind) they are put right and
/// `/usr` is refreshed, so the device slot always comes up on its current
/// generation. A set that is already linked is left merged as it is.
///
/// # Errors
/// A layer the generations name that is missing, and I/O failures; the
/// links stay as they were.
pub fn merge_system_layers(
    slots: &SlotStore,
    layers: &LayerStore,
    apply: &mut dyn Apply,
) -> Result<(), DaemonError> {
    let vm = slots.load(VM_SLOT)?;
    let own = layer_paths(layers, &slots.generation(VM_SLOT, vm.current)?)?;
    let paths = system_layers(slots, layers, &vm, own)?;
    if apply.link_sysext(&paths)? {
        apply.sysext_refresh()?;
    }
    Ok(())
}

/// Bring every slot back after the VM boots: re-create headful slots' vkms
/// seats, then start each slot's session on its current generation. Run
/// once at boot by `tilo-vm-restore.service`; a slot that cannot come back
/// is reported, and the others still start.
pub fn restore_slots(
    slots: &SlotStore,
    layers: &LayerStore,
    locks: &Locks,
    apply: &mut dyn Apply,
) -> Vec<Issue> {
    let issue = |slot: &str, error: DaemonError| Issue {
        slot: slot.into(),
        problem: format!("its session view could not be restored: {error}"),
        hint: "tilo-vm doctor; then retry the normal slot restart".into(),
    };
    let _global = match locks.vm_shared() {
        Ok(guard) => guard,
        Err(error) => return vec![issue(VM_SLOT, error)],
    };
    let records = match slots.list() {
        Ok(records) => records,
        Err(error) => return vec![issue(VM_SLOT, error)],
    };
    let mut issues = reconcile_seats(slots, locks, apply);
    let failed: BTreeSet<String> = issues.iter().map(|i| i.slot.clone()).collect();
    for record in records {
        let Some(session) = record.session() else {
            continue;
        };
        // The owner starts a login at the greeter; its layers merged at boot
        // with systemd-sysext.
        if failed.contains(&record.name) || session.login.is_some() {
            continue;
        }
        let _slot = match locks.slot(&record.name) {
            Ok(guard) => guard,
            Err(error) => {
                issues.push(issue(&record.name, error));
                continue;
            }
        };
        let record = match slots.load(&record.name) {
            Ok(record) => record,
            Err(error) => {
                issues.push(issue(&record.name, error));
                continue;
            }
        };
        if let Err(error) = prepare_slot_session(slots, layers, apply, &record, None)
            .and_then(|()| apply.start_unit(&session.unit))
        {
            issues.push(Issue {
                slot: record.name.clone(),
                problem: format!("its session did not start at boot: {error}"),
                hint: format!(
                    "tilo-vm logs {}   then   tilo-vm slot restart {}",
                    record.name, record.name
                ),
            });
        }
    }
    issues
}

/// `installed: <id> <version>` from tilo-install's output.
pub fn parse_installed(output: &str) -> (Option<String>, Option<String>) {
    output
        .lines()
        .find_map(|line| line.trim().strip_prefix("installed:"))
        .map_or((None, None), |rest| {
            let mut words = rest.split_whitespace().map(str::to_owned);
            (words.next(), words.next())
        })
}

fn to_value<T: Serialize>(value: &T) -> Result<serde_json::Value, DaemonError> {
    serde_json::to_value(value).map_err(|e| DaemonError::InvalidParams(e.to_string()))
}

fn unchanged(record: SlotRecord) -> ApplyResult {
    ApplyResult {
        slot: record.name,
        generation: record.current,
        changed: false,
        policy: Policy::NoRestart,
        actions: Vec::new(),
        cohort_selection: Vec::new(),
        warnings: Vec::new(),
        boot: None,
    }
}

/// Refuse layers the VM cannot run; warn about differences it can.
///
/// Refused: another CPU architecture or target triple, another builder
/// image, or another `Cargo.lock` than the VM's image was built from (the
/// swapped component and the rest of the image would disagree on shared
/// crates). Each is compared only when both sides record it. A dev image
/// records no builder image (it is its own builder), so there the target,
/// toolchain and `Cargo.lock` identify the builder instead.
///
/// # Errors
/// [`DaemonError::AbiMismatch`] naming the first field that differs.
pub fn abi_check(
    provenance: &Provenance,
    base: &BaseInfo,
    arch: &str,
) -> Result<Vec<String>, DaemonError> {
    abi_check_verified(provenance, base, arch, None)
}

pub(crate) fn abi_check_verified(
    provenance: &Provenance,
    base: &BaseInfo,
    arch: &str,
    cohort: Option<&crate::cohort::VerifiedCohort>,
) -> Result<Vec<String>, DaemonError> {
    let mismatch = |field: &str, layer: &str, vm: &str| DaemonError::AbiMismatch {
        field: field.into(),
        layer: layer.into(),
        vm: vm.into(),
    };
    if let Some(target) = &provenance.target {
        // The image's full triple when base.json has it, else the architecture.
        let foreign = match base.target.as_deref() {
            Some(vm) => vm != target,
            None => target.split('-').next() != Some(arch),
        };
        if foreign {
            return Err(mismatch(
                "target",
                target,
                base.target.as_deref().unwrap_or(arch),
            ));
        }
    }
    // Compared only when both name one. A dev image's base.json names none:
    // the image built itself.
    if let (Some(layer), Some(vm)) = (&provenance.builder_image_digest, &base.builder_image_digest)
        && layer != vm
    {
        return Err(mismatch("builder image", layer, vm));
    }
    let mut warnings = Vec::new();
    // Thin apps link the image's libruntime_dylib.so, whose symbols carry
    // crate hashes: another Cargo.lock breaks them. Statically linked
    // components only drift from the image's dependency set.
    if let (Some(layer), Some(vm)) = (&provenance.cargo_lock_hash, &base.cargo_lock_hash)
        && layer != vm
    {
        if provenance.cargo_profile.as_deref() == Some("thin-app") && cohort.is_none() {
            return Err(mismatch("Cargo.lock", short(layer), short(vm)));
        }
        warnings.push(format!(
            "layer Cargo.lock {} differs from the VM's {}: dependencies drift from the image",
            short(layer),
            short(vm)
        ));
        if let Some(cohort) = cohort {
            warnings.push(format!(
                "verified complete apps+runtime cohort carries its own native toolkit {}",
                cohort.version
            ));
        }
    }
    if let (Some(layer), Some(vm)) = (&provenance.base_image_digest, &base.image_digest)
        && layer != vm
    {
        warnings.push(format!(
            "layer was built against base {layer}, the VM runs {vm}"
        ));
    }
    if let (Some(layer), Some(vm)) = (&provenance.toolchain, &base.toolchain)
        && layer != vm
    {
        warnings.push(format!(
            "layer toolchain {layer} differs from the VM's {vm}"
        ));
    }
    if provenance.builder.as_deref() == Some("host") && base.builder_image_digest.is_some() {
        warnings.push(
            "host build: not built in the VM's builder image, so ABI parity is not guaranteed"
                .into(),
        );
    }
    Ok(warnings)
}

/// The component that carries the native toolkit and the thin apps built on it.
const COHORT_COMPONENT: &str = "apps+runtime";

/// The `<version>` of a `tilo-runtime/<version>` runtime, the directory
/// it occupies under `/usr/lib/tilo-runtime`; `None` for any other runtime.
fn native_toolkit(runtime: &str) -> Option<&str> {
    let version = runtime.strip_prefix("tilo-runtime/")?;
    let mut parts = Path::new(version).components();
    match (parts.next(), parts.next()) {
        (Some(std::path::Component::Normal(_)), None) if !version.contains('/') => Some(version),
        _ => None,
    }
}

/// The first 12 hex digits, enough to tell two hashes apart in a message.
fn short(hash: &str) -> &str {
    hash.get(..12).unwrap_or(hash)
}

impl Daemon {
    pub fn new(
        root: &Path,
        apply: Box<dyn Apply>,
        manifest: Option<Manifest>,
        base: BaseInfo,
    ) -> Self {
        Self {
            layers: LayerStore::new(root),
            slots: SlotStore::new(root),
            locks: Locks::new(root),
            targets: TargetStore::new(root),
            seat_issues: None,
            uploads_dir: root.join("uploads"),
            uploads: BTreeMap::new(),
            apply,
            manifest,
            base,
            client_protocol: None,
            root: root.to_owned(),
            expected_domain: DEFAULT_DOMAIN.into(),
        }
    }

    /// The `SELinux` domain a greetd session runs in on this image.
    #[must_use]
    pub fn with_expected_domain(mut self, domain: impl Into<String>) -> Self {
        self.expected_domain = domain.into();
        self
    }

    /// How a slot's session differs from a greetd login; the device's own
    /// session is the production one and is not compared.
    fn parity(&mut self, session: &Session) -> Option<String> {
        if !session.stand_in {
            return None;
        }
        let process = self.apply.session_process(session)?;
        apply::parity_problem(&process, &self.expected_domain)
    }

    /// After starting sessions: refuse to report success for one that did
    /// not come up, or is not equivalent to a production greetd session.
    fn verify_parity(
        &mut self,
        sessions: impl IntoIterator<Item = Session>,
    ) -> Result<(), DaemonError> {
        for session in sessions {
            if let Err(state) = self.apply.wait_session_ready(&session) {
                if session.login.is_some() {
                    let log = self
                        .apply
                        .journal(
                            &[
                                format!("_SYSTEMD_UNIT={GREETER_UNIT}"),
                                format!("_SYSTEMD_UNIT={}", session.unit),
                            ],
                            12,
                        )
                        .unwrap_or_default();
                    return Err(DaemonError::DesktopFailed {
                        slot: session.slot,
                        detail: state,
                        log: log.trim_end().to_owned(),
                    });
                }
                let log = self
                    .apply
                    .journal(
                        &[
                            format!("_SYSTEMD_UNIT={}", session.unit),
                            format!("SYSLOG_IDENTIFIER=tilo-session-{}", session.slot),
                        ],
                        12,
                    )
                    .unwrap_or_default();
                return Err(DaemonError::SessionFailed {
                    slot: session.slot,
                    state,
                    log: log.trim_end().to_owned(),
                });
            }
            if let Some(detail) = self.parity(&session) {
                return Err(DaemonError::SessionParity {
                    slot: session.slot,
                    detail,
                });
            }
        }
        Ok(())
    }

    /// The sessions of every slot that has one.
    fn sessions(&mut self) -> Result<Vec<Session>, DaemonError> {
        Ok(self
            .stored_slots()?
            .iter()
            .filter_map(SlotRecord::session)
            .collect())
    }

    /// Every slot on disk, with the machine's own session found again for
    /// the `device` slot: a login's session and processes change with every
    /// login.
    fn stored_slots(&mut self) -> Result<Vec<SlotRecord>, DaemonError> {
        let mut records = self.slots.list()?;
        if let Some(record) = records.iter_mut().find(|r| r.name == DEVICE_SLOT) {
            *record = match self.load_slot(DEVICE_SLOT) {
                Ok(fresh) => fresh,
                Err(DaemonError::NoDeviceSession) => SlotRecord {
                    device: None,
                    ..record.clone()
                },
                Err(error) => return Err(error),
            };
        }
        Ok(records)
    }

    /// Every slot on disk, and the `device` slot wherever the machine runs
    /// its own session, swapped into or not.
    fn listed_slots(&mut self) -> Result<Vec<SlotRecord>, DaemonError> {
        let mut records = self.stored_slots()?;
        if !records.iter().any(|r| r.name == DEVICE_SLOT)
            && let Some(device) = self.apply.device_session()
        {
            records.push(self.slots.load_with(DEVICE_SLOT, Some(device))?);
            records.sort_by(|a, b| a.name.cmp(&b.name));
        }
        Ok(records)
    }

    /// The layers to merge system-wide for `record`: the `vm` slot and a
    /// laptop's `device` slot (the owner's login runs in the host's mount
    /// namespace) share one `systemd-sysext` set, the `vm` slot's layers
    /// first. Other slots merge that system-wide set with their own.
    fn system_layers(
        &self,
        record: &SlotRecord,
        own: Vec<PathBuf>,
    ) -> Result<Vec<PathBuf>, DaemonError> {
        system_layers(&self.slots, &self.layers, record, own)
    }

    /// Load a slot, finding the machine's own session for the `device` slot.
    fn load_slot(&mut self, name: &str) -> Result<SlotRecord, DaemonError> {
        let device = if name == DEVICE_SLOT {
            self.apply.device_session()
        } else {
            None
        };
        self.slots.load_with(name, device)
    }

    /// Record `booted` as the image the slots run on; see [`crate::image`].
    /// Never changes a slot or deletes a layer.
    ///
    /// # Errors
    /// A held VM lock, and I/O failures.
    pub fn reconcile_image(&mut self, booted: &str) -> Result<Option<ImageChange>, DaemonError> {
        image::ensure_root(&self.root)?;
        let _vm = self.locks.vm_exclusive()?;
        let booted = image::BootedImage::read(
            booted,
            self.base.image_digest.clone(),
            Path::new(tilo_vm_protocol::SYSTEM_INTERFACES),
        );
        image::reconcile(&self.root, &self.slots, &self.layers, &booted)
    }

    /// Bring every slot back after the VM boots. First the booted image is
    /// recorded (when the early-boot unit has not done so already), then
    /// the VM-wide layers are merged as the slots' current generations name
    /// them ([`merge_system_layers`]), then every slot session starts
    /// ([`restore_slots`]).
    pub fn restore(&mut self, booted: Option<&str>) -> Vec<Issue> {
        let mut issues = Vec::new();
        if let Some(booted) = booted
            && let Err(error) = self.reconcile_image(booted)
        {
            issues.push(Issue {
                slot: VM_SLOT.into(),
                problem: format!("the booted image {booted} could not be recorded: {error}"),
                hint: "tilo-vm doctor; the next boot tries again".into(),
            });
        }
        // The person is told which layers are held, and why.
        let notice = image::held_notice(&self.root);
        if let Err(error) = self
            .apply
            .publish_notice(image::HELD_NOTICE, notice.as_deref())
        {
            issues.push(Issue {
                slot: VM_SLOT.into(),
                problem: format!("the notice about held layers could not be published: {error}"),
                hint: "tilo-vm status names them".into(),
            });
        }
        let merged = self
            .locks
            .vm_exclusive()
            .and_then(|_vm| merge_system_layers(&self.slots, &self.layers, self.apply.as_mut()));
        if let Err(error) = merged {
            issues.push(Issue {
                slot: DEVICE_SLOT.into(),
                problem: format!(
                    "the vm and device slots' layers could not be merged at boot: {error}"
                ),
                hint: "tilo-vm doctor; then tilo-vm rollback --slot device to a generation whose layers are all present".into(),
            });
        }
        issues.extend(restore_slots(
            &self.slots,
            &self.layers,
            &self.locks,
            self.apply.as_mut(),
        ));
        self.seat_issues = Some(issues.clone());
        issues
    }

    /// Handle one request.
    ///
    /// # Errors
    /// Whatever the method reports; see [`DaemonError::hint`].
    #[expect(
        clippy::too_many_lines,
        reason = "one arm per method, each a line or three"
    )]
    pub fn handle(&mut self, request: Request) -> Result<serde_json::Value, DaemonError> {
        if let Request::Hello(hello) = &request {
            self.client_protocol = Some(hello.protocol);
            if self.seat_issues.is_none() {
                self.seat_issues = Some(reconcile_seats(
                    &self.slots,
                    &self.locks,
                    self.apply.as_mut(),
                ));
            }
            return to_value(&HelloResult {
                protocol: PROTOCOL_VERSION,
                daemon_version: env!("CARGO_PKG_VERSION").into(),
                root: self.root.display().to_string(),
                arch: std::env::consts::ARCH.into(),
                base: self.base.clone(),
                issues: self.issues()?,
                tools: detect_tools(),
                awake: Some(self.renew_awake(awake::Change::Request)),
                capabilities: vec!["userspace.apply".into()],
            });
        }
        match self.client_protocol {
            None => return Err(DaemonError::HelloRequired),
            Some(client) if client != PROTOCOL_VERSION => {
                return Err(DaemonError::VersionMismatch {
                    client,
                    daemon: PROTOCOL_VERSION,
                });
            }
            Some(_) => {}
        }
        match request {
            Request::Hello(_) => Err(DaemonError::HelloRequired),
            Request::DeviceActions(p) => self.device_actions(&p.line),
            Request::DeviceUnlockEnrol(p) => {
                p.validate().map_err(DaemonError::InvalidParams)?;
                let _locks = self.slot_locks(DEVICE_SLOT)?;
                let (_, session) = self.user_slot(DEVICE_SLOT)?;
                self.require_device_action_owner(&session)?;
                let uid = self.apply.account_uid(&session.user)?;
                to_value(&self.apply.enroll_device_unlock(uid, &p)?)
            }
            Request::Status(p) => to_value(&self.status(p.slot)?),
            Request::SlotCreate(p) => {
                let _locks = self.slot_locks(&p.name)?;
                to_value(&self.create(&p.name, p.kind, p.owner, &p.outputs, p.identity)?)
            }
            Request::SlotList(_) => {
                let records = self.listed_slots()?;
                to_value(
                    &records
                        .iter()
                        .map(|r| self.info(r))
                        .collect::<Result<Vec<_>, _>>()?,
                )
            }
            Request::SlotRestart(p) => {
                let _locks = self.slot_locks(&p.name)?;
                to_value(&self.restart(&p.name)?)
            }
            Request::SlotRm(p) => {
                let _locks = self.slot_locks(&p.name)?;
                to_value(&self.remove(&p.name)?)
            }
            Request::SlotShow(p) => to_value(&self.show(&p.name)?),
            Request::LayerUploadBinary(_) | Request::LayerUploadBegin(_) => {
                to_value(&self.upload_begin()?)
            }
            Request::LayerUploadChunk(p) => to_value(&self.upload_chunk(&p.upload, &p.data)?),
            Request::LayerImport(p) => {
                let _vm = self.locks.vm_shared()?;
                to_value(&self.import(p.source, &p.provenance)?)
            }
            Request::LayerList(_) => to_value(&self.layers.list()?),
            Request::LayerVerify(p) => {
                let _vm = self.locks.vm_shared()?;
                to_value(&self.layers.verify(&p.hash)?)
            }
            Request::Swap(p) => {
                let _locks = self.slot_locks(&p.slot)?;
                to_value(&self.swap(&p.slot, &p.component, &p.layer, p.info)?)
            }
            Request::UserspaceApply(p) => {
                let _locks = self.slot_locks(&p.slot)?;
                to_value(&self.apply_userspace(p)?)
            }
            Request::Rollback(p) => {
                let _locks = self.slot_locks(&p.slot)?;
                to_value(&self.rollback(&p.slot, p.to)?)
            }
            Request::Logs(p) => {
                let record = self.load_slot(&p.slot)?;
                let matches = self.journal_matches(&record)?;
                to_value(&LogsResult {
                    text: self.apply.journal(&matches, p.lines)?,
                })
            }
            Request::SeatClaim(p) => {
                let _vm = self.locks.vm_shared()?;
                let _seat = self.locks.slot("seat0")?;
                to_value(&self.claim(&p.slot, p.owner, p.steal)?)
            }
            Request::SeatRelease(p) => {
                let _vm = self.locks.vm_shared()?;
                let _seat = self.locks.slot("seat0")?;
                to_value(&self.release(&p.slot)?)
            }
            Request::Shot(p) => {
                let (_, session) = self.user_slot(&p.slot)?;
                let png = self.apply.shot(&session)?;
                to_value(&ShotResult {
                    png: STANDARD.encode(png),
                })
            }
            Request::Control(p) => {
                let (_, session) = self.user_slot(&p.slot)?;
                if let Some(input) = &p.private_args {
                    if !matches!(p.args.as_slice(), [actions, invoke, _, _, stdin] if actions == "actions" && invoke == "invoke" && stdin == "--args-stdin")
                        || input.line.len() > 65536
                    {
                        return Err(DaemonError::InvalidParams(
                            "invalid private control request".into(),
                        ));
                    }
                    to_value(&self.apply.private_control(&session, &p.args, &input.line)?)
                } else {
                    to_value(&self.apply.control(&session, &p.args)?)
                }
            }
            Request::Gc(p) => {
                let _vm = self.locks.vm_exclusive()?;
                to_value(&self.gc(&p)?)
            }
            Request::Input(p) => {
                let _locks = self.slot_locks(&p.slot)?;
                to_value(&self.input(&p.slot, &p.actions)?)
            }
            Request::AppInstall(p) => {
                let _locks = self.slot_locks(&p.slot)?;
                to_value(&self.install_app(&p.slot, p.source)?)
            }
            Request::NetworkWake(p) => {
                let status = match p.enabled {
                    Some(enabled) => os_actions::network_wake::configure(&p.profile, enabled),
                    None => os_actions::network_wake::status(&p.profile),
                }
                .map_err(|detail| DaemonError::Apply {
                    step: "network wake".into(),
                    detail,
                })?;
                to_value(&status)
            }
            Request::Awake(p) => {
                let change = match (p.off, p.seconds) {
                    (true, _) => Some(awake::Change::Off),
                    (false, Some(seconds)) => Some(awake::Change::Hold(seconds)),
                    (false, None) => None,
                };
                to_value(&match change {
                    Some(change) => self.renew_awake(change),
                    None => self.awake_lease(),
                })
            }
            Request::BootConfirm(p) => {
                let _locks = self.slot_locks(VM_SLOT)?;
                to_value(&self.confirm_boot(p.expect_version.as_deref())?)
            }
        }
    }

    fn confirm_boot(
        &mut self,
        expect_version: Option<&str>,
    ) -> Result<BootConfirmResult, DaemonError> {
        self.apply.confirm_boot(expect_version)
    }

    pub(crate) fn user_device_action_session(&mut self) -> Result<(), DaemonError> {
        let (_, session) = self.user_slot(DEVICE_SLOT)?;
        self.require_device_action_owner(&session)
    }

    fn require_device_action_owner(&mut self, session: &Session) -> Result<(), DaemonError> {
        if self.apply.device_action_owner(session) {
            Ok(())
        } else {
            Err(DaemonError::InvalidParams(
                "the device owner's signed-in session is required".into(),
            ))
        }
    }

    fn input(&mut self, name: &str, actions: &[InputAction]) -> Result<InputResult, DaemonError> {
        tilo_vm_protocol::input::validate(actions).map_err(DaemonError::InvalidParams)?;
        let (record, _) = self.user_slot(name)?;
        let seat = match (&record.seat0, record.kind, &record.seat) {
            (Some(_), _, _) => "seat0".to_owned(),
            (None, SlotKind::Device, _)
                if record
                    .device
                    .as_ref()
                    .is_some_and(|device| device.login.is_some()) =>
            {
                // A greetd device login already owns the physical seat; it
                // does not need the lease used by a developer's headful slot.
                "seat0".to_owned()
            }
            (None, SlotKind::Headful, Some(seat)) => seat.clone(),
            (None, SlotKind::Headful, None) => return Err(DaemonError::NoSession(name.to_owned())),
            _ => return Err(DaemonError::HeadlessInput(name.to_owned())),
        };
        // On seat0 the slot sees the VM's real display, not its vkms outputs.
        let geometry = match (&record.seat0, record.outputs.first()) {
            (None, Some(first)) => Some(crate::input::Geometry {
                desktop: crate::display::desktop_extent(&record.outputs),
                touch: (first.width, first.height),
            }),
            _ => None,
        };
        let events = self.apply.input(name, &seat, geometry, actions)?;
        Ok(InputResult {
            slot: record.name,
            seat,
            events,
        })
    }

    /// Install a `.tapp` into the slot user's app store. Not a generation:
    /// the user store shadows the image's apps, live at their next launch.
    ///
    /// # Errors
    /// Unknown slots, the `vm` slot, missing uploads and tilo-install failures.
    pub fn install_app(
        &mut self,
        slot: &str,
        source: LayerSource,
    ) -> Result<AppInstallResult, DaemonError> {
        let (record, session) = self.user_slot(slot)?;
        let (path, uploaded) = match source {
            LayerSource::Upload(id) => (
                self.uploads
                    .remove(&id)
                    .ok_or(DaemonError::UploadNotFound(id))?,
                true,
            ),
            LayerSource::Path(path) => (PathBuf::from(path), false),
        };
        let output = self.apply.install_app(&session, &path);
        if uploaded {
            let _ = fs::remove_file(&path);
        }
        let output = output?;
        let (id, version) = parse_installed(&output);
        Ok(AppInstallResult {
            slot: record.name,
            id,
            version,
            output,
        })
    }

    fn lease_path(&self) -> PathBuf {
        self.root.join(awake::LEASE_FILE)
    }

    /// Renew (or release) the keep-awake lease and make the lock follow it.
    /// A failure is reported in the lease, never as the request's error:
    /// a machine that cannot be held awake can still be driven.
    fn renew_awake(&mut self, change: awake::Change) -> AwakeLease {
        let path = self.lease_path();
        let now = tilo_clock::unix_now();
        let (lease, write) = awake::renewal(awake::read(&path), now, change);
        let mut problem = if write
            && let Err(error) = serde_json::to_vec(&lease)
                .map_err(|e| DaemonError::InvalidParams(e.to_string()))
                .and_then(|bytes| crate::fsutil::write_atomic(&path, &bytes))
        {
            Some(error.to_string())
        } else {
            None
        };
        let applied = if lease.active(now) {
            self.apply.hold_awake(&path)
        } else {
            self.apply.release_awake()
        };
        if let Err(error) = applied {
            problem = Some(error.to_string());
        }
        AwakeLease {
            problem,
            ..self.awake_lease()
        }
    }

    /// The keep-awake lease as it stands.
    fn awake_lease(&mut self) -> AwakeLease {
        let now = tilo_clock::unix_now();
        let lease = awake::read(&self.lease_path()).filter(|lease| lease.active(now));
        let held = self.apply.unit_state(awake::AWAKE_UNIT) == "active";
        AwakeLease {
            until: lease.map(|lease| lease.deadline()),
            hold_until: lease.and_then(|lease| lease.hold_until.filter(|&hold| hold > now)),
            held,
            problem: (lease.is_some() && !held).then(|| {
                format!(
                    "{} is not running; journalctl -u {} says why",
                    awake::AWAKE_UNIT,
                    awake::AWAKE_UNIT
                )
            }),
        }
    }

    fn status(&mut self, slot: Option<String>) -> Result<StatusResult, DaemonError> {
        let records = match slot {
            Some(name) => vec![self.load_slot(&name)?],
            None => self.listed_slots()?,
        };
        let slots = records
            .iter()
            .map(|r| self.info(r))
            .collect::<Result<_, _>>()?;
        let seat0 = self.slots.list()?.into_iter().find_map(|r| r.seat0);
        Ok(StatusResult {
            protocol: PROTOCOL_VERSION,
            daemon_version: env!("CARGO_PKG_VERSION").into(),
            slots,
            seat0,
            storage: self.storage(),
            issues: self.issues()?,
            image_change: image::last_change(&self.root),
            slots_image: image::slots_image(&self.root),
        })
    }

    fn upload_begin(&mut self) -> Result<LayerUploadBeginResult, DaemonError> {
        fs::create_dir_all(&self.uploads_dir).map_err(DaemonError::io("create uploads"))?;
        let id = uuid::Uuid::new_v4().simple().to_string();
        let path = self.uploads_dir.join(format!("{id}.tar"));
        fs::File::create(&path).map_err(DaemonError::io("create upload"))?;
        self.uploads.insert(id.clone(), path);
        Ok(LayerUploadBeginResult { upload: id })
    }

    pub(crate) fn cancel_upload(&mut self, id: &str) {
        if let Some(path) = self.uploads.remove(id) {
            let _ = fs::remove_file(path);
        }
    }

    pub(crate) fn receive_binary(
        &mut self,
        id: &str,
        input: impl std::io::Read,
    ) -> (std::io::Result<u64>, bool) {
        let Some(path) = self.uploads.get(id) else {
            return (Err(std::io::Error::other("binary upload missing")), false);
        };
        let mut reader = tilo_vm_protocol::binary::Reader::new(input);
        let result = fs::OpenOptions::new()
            .append(true)
            .open(path)
            .and_then(|mut file| {
                let bytes = std::io::copy(&mut reader, &mut file)?;
                file.flush()?;
                Ok(bytes)
            });
        let boundary = reader.decoder.at_boundary();
        if result.is_err() {
            self.cancel_upload(id);
        }
        (result, boundary)
    }

    fn upload_chunk(
        &self,
        upload: &str,
        data: &str,
    ) -> Result<LayerUploadChunkResult, DaemonError> {
        let path = self
            .uploads
            .get(upload)
            .ok_or_else(|| DaemonError::UploadNotFound(upload.to_owned()))?;
        let bytes = STANDARD
            .decode(data)
            .map_err(|e| DaemonError::InvalidParams(e.to_string()))?;
        if bytes.len() > UPLOAD_CHUNK_BYTES {
            return Err(DaemonError::InvalidParams(format!(
                "chunk exceeds {UPLOAD_CHUNK_BYTES} bytes"
            )));
        }
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(path)
            .map_err(DaemonError::io("open upload"))?;
        file.write_all(&bytes)
            .map_err(DaemonError::io("write upload"))?;
        let received = file
            .metadata()
            .map_err(DaemonError::io("stat upload"))?
            .len();
        Ok(LayerUploadChunkResult { received })
    }

    fn import(
        &mut self,
        source: LayerSource,
        provenance: &Provenance,
    ) -> Result<LayerImportResult, DaemonError> {
        let apply = &mut self.apply;
        let (hash, existed) = match source {
            LayerSource::Upload(id) => {
                let path = self
                    .uploads
                    .remove(&id)
                    .ok_or(DaemonError::UploadNotFound(id))?;
                let file = fs::File::open(&path).map_err(DaemonError::io("open upload"));
                let result = file.and_then(|f| {
                    self.layers
                        .import_tar(std::io::BufReader::new(f), provenance, &mut |l| {
                            apply.label_layer(l)
                        })
                });
                let _ = fs::remove_file(&path);
                result?
            }
            LayerSource::Path(path) => {
                self.layers
                    .import_path(Path::new(&path), provenance, &mut |l| apply.label_layer(l))?
            }
        };
        Ok(LayerImportResult { hash, existed })
    }

    /// VM-wide work on the `vm` slot excludes everything; slot work shares
    /// the VM and excludes other work on the same slot.
    pub(crate) fn slot_locks(&self, name: &str) -> Result<(Guard, Option<Guard>), DaemonError> {
        if name == VM_SLOT {
            Ok((self.locks.vm_exclusive()?, None))
        } else {
            Ok((self.locks.vm_shared()?, Some(self.locks.slot(name)?)))
        }
    }

    pub(crate) fn user_slot(&mut self, name: &str) -> Result<(SlotRecord, Session), DaemonError> {
        let record = self.load_slot(name)?;
        let session = record
            .session()
            .ok_or_else(|| DaemonError::NoSession(name.to_owned()))?;
        if session.logged_out() {
            return Err(DaemonError::LoggedOut(record.name));
        }
        Ok((record, session))
    }

    fn storage(&mut self) -> Storage {
        let space = self.apply.disk_space(&self.root);
        Storage {
            layers_bytes: self.layers.allocated_bytes(),
            targets_bytes: self.targets.total_bytes(),
            free_bytes: space.map(|s| s.0),
            total_bytes: space.map(|s| s.1),
        }
    }

    /// Reconcile recorded state with what is running.
    fn issues(&mut self) -> Result<Vec<Issue>, DaemonError> {
        let mut issues = self.seat_issues.clone().unwrap_or_default();
        for record in self.stored_slots()? {
            if let Some(number) = self.slots.pending(&record.name) {
                issues.push(Issue {
                    slot: record.name.clone(),
                    problem: format!(
                        "a transition to generation {number} was interrupted; generation {} is recorded as current",
                        record.current
                    ),
                    hint: format!("re-run the swap, or re-apply the current generation: tilo-vm slot restart {}", record.name),
                });
            }
            if let Some(session) = record.session() {
                let state = self.apply.unit_state(&session.unit);
                if state != "active" && state != "activating" {
                    issues.push(Issue {
                        slot: record.name.clone(),
                        problem: format!("session is {state}"),
                        hint: format!(
                            "tilo-vm logs {}   then   tilo-vm slot restart {}",
                            record.name, record.name
                        ),
                    });
                } else if let Some(detail) = self.parity(&session) {
                    let error = DaemonError::SessionParity {
                        slot: record.name.clone(),
                        detail,
                    };
                    issues.push(Issue {
                        slot: record.name.clone(),
                        problem: error.to_string(),
                        hint: error.hint(),
                    });
                }
            }
        }
        Ok(issues)
    }

    fn info(&mut self, record: &SlotRecord) -> Result<SlotInfo, DaemonError> {
        let generation = self.slots.generation(&record.name, record.current)?;
        let unit = record.session();
        let session = match &unit {
            None => "n/a".into(),
            Some(s) if s.logged_out() => "logged out".into(),
            Some(s) => self.apply.unit_state(&s.unit),
        };
        let parity = match &unit {
            Some(unit) if session == "active" => self.parity(unit),
            _ => None,
        };
        Ok(SlotInfo {
            session_identity: unit.as_ref().map(Session::identity),
            name: record.name.clone(),
            kind: record.kind,
            owner: record.owner.clone(),
            user: record.user(),
            generation: record.current,
            generations: std::iter::once(0)
                .chain(self.slots.generations(&record.name))
                .collect::<BTreeSet<u64>>()
                .into_iter()
                .collect(),
            components: generation
                .components
                .into_iter()
                .map(|(k, v)| (k, v.layer))
                .collect(),
            session,
            seat: if record.seat0.is_some() {
                Some("seat0".into())
            } else {
                record.seat.clone()
            },
            outputs: record.outputs.clone(),
            parity,
        })
    }

    /// Run `args` as slot `name` sees the system (see [`crate::exec`]),
    /// streaming to this process's stdout and stderr.
    ///
    /// # Errors
    /// Unknown slots, the `vm` slot, a stopped session, or a command that
    /// cannot start.
    pub fn exec(
        &mut self,
        name: &str,
        args: &[String],
        timeout: Duration,
        max_bytes: u64,
    ) -> Result<crate::exec::Ended, DaemonError> {
        let (record, session) = self.user_slot(name)?;
        let pid = self
            .apply
            .session_pid(&session)
            .ok_or_else(|| DaemonError::SessionStopped(record.name.clone()))?;
        let env = self.apply.session_environment(&session);
        crate::exec::run_bounded(
            crate::exec::in_session(pid, &session.user, &env, args),
            timeout,
            max_bytes,
            std::io::stdout(),
            std::io::stderr(),
        )
        .map_err(DaemonError::io(format!("run {}", args.join(" "))))
    }

    /// The slot and every file its current generation's layers add.
    fn show(&mut self, name: &str) -> Result<SlotShowResult, DaemonError> {
        let record = self.load_slot(name)?;
        let generation = self.slots.generation(name, record.current)?;
        let mut files = Vec::new();
        let mut provenance = BTreeMap::new();
        for (component, entry) in &generation.components {
            if let Some(imported) = self.layers.provenance(&entry.layer) {
                provenance.insert(component.clone(), imported);
            }
            for file in self.layers.files(&entry.layer)? {
                files.push(SlotFile {
                    component: component.clone(),
                    layer: entry.layer.clone(),
                    path: file.path,
                    sha256: file.sha256,
                    link: file.link,
                    executable: file.executable,
                });
            }
        }
        Ok(SlotShowResult {
            slot: self.info(&record)?,
            files,
            provenance,
        })
    }

    fn layer_paths(&self, generation: &Generation) -> Result<Vec<PathBuf>, DaemonError> {
        layer_paths(&self.layers, generation)
    }

    fn create(
        &mut self,
        name: &str,
        kind: SlotKind,
        owner: Owner,
        outputs: &[OutputSpec],
        identity: bool,
    ) -> Result<SlotCreateResult, DaemonError> {
        let (mut record, created) = self.slots.create(name, kind, owner, outputs)?;
        let session = record
            .session()
            .ok_or_else(|| DaemonError::NoSession(name.to_owned()))?;
        let running = self.apply.unit_state(&session.unit) == "active";
        let mut actions = if created || !running {
            let generation = self.slots.generation(name, record.current)?;
            let layers = self.layer_paths(&generation)?;
            let actions = apply::activate(
                self.apply.as_mut(),
                &mut record,
                &generation,
                &layers,
                Policy::NoRestart,
                &Restarts::default(),
                SessionChange::Ensure,
            )?;
            self.slots.save(&record)?;
            self.verify_parity([session])?;
            actions
        } else {
            Vec::new()
        };
        let identity = if identity {
            let (identity, steps) = self.provision_test_identity(&record)?;
            actions.extend(steps);
            Some(identity)
        } else {
            None
        };
        // Every create applies the layout again, so a create that failed
        // here is repaired by running it once more.
        if !record.outputs.is_empty() && record.seat0.is_none() {
            let user = record.user().unwrap_or_default();
            self.apply
                .apply_display(name, &user, &record.outputs)
                .map_err(|error| DaemonError::DisplayLayout {
                    slot: name.to_owned(),
                    detail: match error {
                        DaemonError::Apply { detail, .. } => detail,
                        other => other.to_string(),
                    },
                })?;
            actions.push(Action::SetOutputs {
                outputs: tilo_vm_protocol::display::format_outputs(&record.outputs),
            });
        }
        Ok(SlotCreateResult {
            slot: self.info(&record)?,
            created,
            actions,
            identity,
        })
    }

    /// Have the slot's running session give its user a throwaway test
    /// identity. A newly minted one comes with fresh desktop state: a
    /// workspace made before the identity is owned by the Unix user, not
    /// the identity, and hides every app launch.
    fn provision_test_identity(
        &mut self,
        record: &SlotRecord,
    ) -> Result<(TestIdentity, Vec<Action>), DaemonError> {
        let name = record.name.as_str();
        let fail = |detail: String| DaemonError::TestIdentity {
            slot: name.to_owned(),
            detail,
        };
        let session = record
            .session()
            .ok_or_else(|| fail("the slot has no user".into()))?;
        let user = session.user.clone();
        self.apply.wait_session_ready(&session).map_err(fail)?;
        let args = [
            "raw".to_owned(),
            r#"{"op":"provision_test_identity"}"#.to_owned(),
        ];
        let asked = Instant::now();
        let reply = loop {
            let reply = self.apply.control(&session, &args)?;
            // tilo-control exits 2 when it cannot reach the socket, which a
            // session that has only just started has not bound yet.
            if reply.exit_code != 2 || asked.elapsed() >= CONTROL_SOCKET_TIMEOUT {
                break reply;
            }
            std::thread::sleep(Duration::from_millis(250));
        };
        if reply.exit_code != 0 {
            return Err(fail(reply.stderr.trim().to_owned()));
        }
        let identity = serde_json::from_str::<serde_json::Value>(reply.stdout.trim())
            .ok()
            .and_then(|reply| {
                serde_json::from_value::<TestIdentity>(reply.get("answer")?.clone()).ok()
            })
            .filter(|identity| identity.throwaway)
            .ok_or_else(|| fail(format!("unexpected reply: {}", reply.stdout.trim())))?;
        let mut actions = vec![Action::ProvisionTestIdentity {
            created: identity.created,
        }];
        if identity.created {
            self.apply.stop_unit(&session.unit)?;
            actions.push(Action::StopUnit {
                unit: session.unit.clone(),
            });
            self.apply.reset_desktop_state(&user)?;
            actions.push(Action::ResetDesktopState);
            let uid = apply::session_uid(self.apply.as_mut(), &session)?;
            actions.extend(apply::restart_session(self.apply.as_mut(), &session, uid)?);
            self.apply.wait_session_ready(&session).map_err(fail)?;
            self.verify_parity([session])?;
        }
        Ok((identity, actions))
    }

    fn restart(&mut self, name: &str) -> Result<ApplyResult, DaemonError> {
        let mut record = self.load_slot(name)?;
        let generation = self.slots.generation(name, record.current)?;
        let layers = self.system_layers(&record, self.layer_paths(&generation)?)?;
        let policy = if record.kind == SlotKind::Vm {
            Policy::VmRefresh
        } else {
            Policy::SessionRestart
        };
        let restarts = Restarts::of(&generation);
        let mut actions = apply::activate(
            self.apply.as_mut(),
            &mut record,
            &generation,
            &layers,
            policy,
            &restarts,
            SessionChange::Restart,
        )?;
        self.slots.save(&record)?;
        self.slots.clear_pending(name)?;
        if record.kind == SlotKind::Vm {
            actions.extend(self.restart_all_sessions(&restarts.user_units, &layers)?);
            let sessions = self.sessions()?;
            self.verify_parity(sessions)?;
        } else {
            self.verify_parity(record.session())?;
        }
        Ok(ApplyResult {
            slot: record.name,
            generation: record.current,
            changed: true,
            policy,
            actions,
            cohort_selection: Vec::new(),
            warnings: Vec::new(),
            boot: None,
        })
    }

    fn remove(&mut self, name: &str) -> Result<SlotRmResult, DaemonError> {
        if name == VM_SLOT {
            return Err(DaemonError::ReservedName(name.to_owned()));
        }
        if name == DEVICE_SLOT {
            return Err(DaemonError::DeviceSlot("removed".into()));
        }
        let record = match self.slots.load(name) {
            Ok(record) => record,
            Err(DaemonError::SlotNotFound(_)) => {
                return Ok(SlotRmResult {
                    removed: false,
                    actions: Vec::new(),
                });
            }
            Err(error) => return Err(error),
        };
        let mut actions = apply::deactivate(self.apply.as_mut(), &record)?;
        if record.seat0.is_some() {
            self.apply.start_unit(GREETER_UNIT)?;
            actions.push(Action::StartUnit {
                unit: GREETER_UNIT.into(),
            });
        }
        let removed = self.slots.remove(name)?;
        Ok(SlotRmResult { removed, actions })
    }

    fn component_info(
        &self,
        record: &SlotRecord,
        component: &str,
        info: Option<ComponentInfo>,
    ) -> ComponentInfo {
        info.or_else(|| {
            self.manifest
                .as_ref()?
                .get(component)
                .map(|c| c.info.clone())
        })
        .unwrap_or_else(|| {
            // Unknown components take the scope of the slot they are swapped into.
            let scope = if record.kind == SlotKind::Vm {
                Scope::Vm
            } else {
                Scope::Session
            };
            ComponentInfo::new(scope, Vec::new())
        })
    }

    fn swap(
        &mut self,
        name: &str,
        component: &str,
        layer: &str,
        info: Option<ComponentInfo>,
    ) -> Result<ApplyResult, DaemonError> {
        let mut record = self.load_slot(name)?;
        let layer_path = self.layers.require(layer)?;
        // Naming a held layer in a swap brings it back deliberately.
        image::release(&self.root, layer)?;
        let provenance = self
            .layers
            .provenance(layer)
            .unwrap_or_else(|| Provenance::local(component));
        let cohort = if component == COHORT_COMPONENT
            && self
                .manifest
                .as_ref()
                .and_then(|manifest| manifest.get(component))
                .is_some_and(|entry| entry.image_assembled && entry.info.scope == Scope::Session)
        {
            if provenance.component != component
                || provenance.source_commit.is_none()
                || provenance.cargo_profile.as_deref() != Some("thin-app")
            {
                return Err(DaemonError::InvalidParams(
                    "apps+runtime requires actual source-bound thin graph provenance".into(),
                ));
            }
            Some(crate::cohort::verify(
                &layer_path,
                Path::new("/usr/share/tilo-apps"),
                std::env::consts::ARCH,
            )?)
        } else {
            None
        };
        let warnings = abi_check_verified(
            &provenance,
            &self.base,
            std::env::consts::ARCH,
            cohort.as_ref(),
        )?;
        let info = self.component_info(&record, component, info);
        apply::check_scope(&record, component, &info)?;
        let mut result = match self.slots.prepare_swap(&record, component, layer, info)? {
            Some((from, to)) => {
                // The cohort reselects its own applications for the new toolkit.
                let reselected = cohort
                    .as_ref()
                    .map(|cohort| cohort.ids().collect())
                    .unwrap_or_default();
                self.check_cohort_users(&record, &from, &to, &reselected)?;
                self.transition(&mut record, &from, &to)?
            }
            None => unchanged(record),
        };
        if !result.changed {
            self.layers.verify(layer)?;
        }
        if let Some(cohort) = &cohort {
            let (record, session) = self.user_slot(name)?;
            if !result.changed {
                let current = self.slots.generation(name, record.current)?;
                let layers = self.system_layers(&record, self.layer_paths(&current)?)?;
                if self.cohort_is_active(&session, &layers, cohort)? {
                    result.warnings.splice(0..0, warnings);
                    return Ok(result);
                }
            }
            result.cohort_selection.extend(crate::cohort::select(
                self.apply.as_mut(),
                &session,
                cohort,
            )?);
            let uid = self.apply.account_uid(&session.user)?;
            result
                .actions
                .extend(apply::restart_session(self.apply.as_mut(), &session, uid)?);
            self.verify_parity(Some(session))?;
            result.changed = true;
            result.policy = Policy::SessionRestart;
        }
        result.warnings.splice(0..0, warnings);
        Ok(result)
    }

    fn rollback(&mut self, name: &str, target: Option<u64>) -> Result<ApplyResult, DaemonError> {
        let mut record = self.load_slot(name)?;
        let (from, to) = self.slots.rollback_target(&record, target)?;
        if from.number == to.number {
            return Ok(unchanged(record));
        }
        self.check_cohort_users(&record, &from, &to, &BTreeSet::new())?;
        self.transition(&mut record, &from, &to)
    }

    /// Refuse a transition that takes away a native toolkit the slot user's
    /// own packages run on: one the apps+runtime layer of `from` ships
    /// (`/usr/lib/tilo-runtime/<version>`) and neither the layers `to`
    /// merges nor the image itself ship. Packages in `reselected` are
    /// installed again for the new toolkit. A package that names no
    /// runtime never blocks; runtime-kind packages are among them, since
    /// a runtime may not declare one.
    fn check_cohort_users(
        &mut self,
        record: &SlotRecord,
        from: &Generation,
        to: &Generation,
        reselected: &BTreeSet<&str>,
    ) -> Result<(), DaemonError> {
        let Some(current) = from.components.get(COHORT_COMPONENT) else {
            return Ok(());
        };
        if to
            .components
            .get(COHORT_COMPONENT)
            .map(|entry| &entry.layer)
            == Some(&current.layer)
        {
            return Ok(());
        }
        let Some(session) = record.session() else {
            return Ok(());
        };
        let packages = self.apply.user_packages(&session)?;
        let cohort = self.layers.require(&current.layer)?;
        let target = self.system_layers(record, self.layer_paths(to)?)?;
        let mut stranded = Vec::new();
        for package in packages
            .iter()
            .filter(|package| !reselected.contains(package.id.as_str()))
        {
            let Some(version) = native_toolkit(&package.runtime) else {
                continue;
            };
            let relative = Path::new("usr/lib/tilo-runtime").join(version);
            if cohort.join(&relative).is_dir()
                && !target.iter().any(|layer| layer.join(&relative).is_dir())
                && !self.apply.image_has_runtime(version)
            {
                stranded.push(package);
            }
        }
        if stranded.is_empty() {
            return Ok(());
        }
        let needs = stranded
            .iter()
            .map(|package| format!("{} needs {}", package.id, package.runtime))
            .collect::<Vec<_>>()
            .join(", ");
        let remove = stranded
            .iter()
            .map(|package| format!("`tilo app remove {}`", package.id))
            .collect::<Vec<_>>()
            .join(", ");
        Err(DaemonError::InvalidParams(format!(
            "generation {} drops the native toolkit the {COHORT_COMPONENT} layer of generation {} \
             ships, and {}'s own packages run on it: {needs}; neither generation {} nor the image \
             provides it. Remove them ({remove}) or roll to a generation that provides it",
            to.number, from.number, session.user, to.number
        )))
    }

    fn transition(
        &mut self,
        record: &mut SlotRecord,
        from: &Generation,
        to: &Generation,
    ) -> Result<ApplyResult, DaemonError> {
        // Boot changes have a separate boot-confirmation lifecycle. Runtime
        // transitions recover once, without recursively retrying a broken build.
        let (policy, _) = apply::policy(record, from, to)?;
        match self.activate_generation(record, from, to) {
            Ok(result) => Ok(result),
            Err(error) if policy != Policy::Reboot => {
                let recovery = match self.activate_generation(record, to, from) {
                    Ok(_) => format!("restored generation {}", from.number),
                    Err(recovery) => format!(
                        "recovery to generation {} also failed: {recovery}; inspect logs --slot {} and retry rollback --slot {} --to {}",
                        from.number, record.name, record.name, from.number
                    ),
                };
                Err(DaemonError::Apply {
                    step: format!(
                        "activate generation {} in slot '{}'",
                        to.number, record.name
                    ),
                    detail: format!("{error}; {recovery}"),
                })
            }
            Err(error) => Err(error),
        }
    }

    fn activate_generation(
        &mut self,
        record: &mut SlotRecord,
        from: &Generation,
        to: &Generation,
    ) -> Result<ApplyResult, DaemonError> {
        let (policy, restarts) = apply::policy(record, from, to)?;
        let layers = self.system_layers(record, self.layer_paths(to)?)?;
        self.slots.mark_pending(&record.name, to.number)?;
        let mut actions = apply::activate(
            self.apply.as_mut(),
            record,
            to,
            &layers,
            policy,
            &restarts,
            SessionChange::Keep,
        )?;
        let mut boot = None;
        let mut warnings = Vec::new();
        match policy {
            Policy::VmRefresh => {
                actions.extend(self.restart_all_sessions(&restarts.user_units, &layers)?);
                let sessions = self.sessions()?;
                self.verify_parity(sessions)?;
                self.slots.set_current(record, to.number)?;
            }
            Policy::Reboot => {
                // The kernel's modules are merged now; its boot entry is next.
                // Sessions restart with the reboot the client runs.
                let kernel = to
                    .components
                    .values()
                    .find(|entry| entry.info.reboot)
                    .map(|entry| self.layers.require(&entry.layer))
                    .transpose()?;
                let (change, notes) = self.apply.boot_kernel(kernel.as_deref())?;
                if let Some(entry) = &change.entry {
                    if let Some(kver) = &change.kver {
                        actions.push(Action::InstallBootEntry {
                            entry: entry.clone(),
                            kver: kver.clone(),
                        });
                        actions.push(Action::SetNextBoot {
                            entry: entry.clone(),
                        });
                    } else {
                        actions.push(Action::SetDefaultBoot {
                            entry: entry.clone(),
                        });
                    }
                }
                warnings.extend(notes);
                boot = Some(change);
                self.slots.set_current(record, to.number)?;
            }
            Policy::SessionRestart => {
                self.verify_parity(record.session())?;
                self.slots.set_current(record, to.number)?;
            }
            Policy::NoRestart => self.slots.set_current(record, to.number)?,
        }
        Ok(ApplyResult {
            slot: record.name.clone(),
            generation: to.number,
            changed: true,
            policy,
            actions,
            cohort_selection: Vec::new(),
            warnings,
            boot,
        })
    }

    /// Slot sessions snapshot `/usr` when they start, so a refreshed VM
    /// `/usr` reaches them only through a restart.
    fn restart_all_sessions(
        &mut self,
        user_units: &[String],
        system: &[PathBuf],
    ) -> Result<Vec<Action>, DaemonError> {
        let mut actions = Vec::new();
        for session in self.sessions()? {
            if session.login.is_some() && !session.logged_out() {
                for unit in user_units {
                    self.apply.restart_user_unit(&session.user, unit)?;
                    actions.push(Action::RestartUserUnit {
                        user: session.user.clone(),
                        unit: unit.clone(),
                    });
                }
            }
            if session.stand_in {
                let record = self
                    .stored_slots()?
                    .into_iter()
                    .find(|record| record.session().as_ref() == Some(&session))
                    .ok_or_else(|| {
                        DaemonError::InvalidParams("stand-in session has no slot".into())
                    })?;
                prepare_slot_session(
                    &self.slots,
                    &self.layers,
                    self.apply.as_mut(),
                    &record,
                    Some(system),
                )?;
            }
            let uid = apply::session_uid(self.apply.as_mut(), &session)?;
            actions.extend(apply::restart_session(self.apply.as_mut(), &session, uid)?);
        }
        Ok(actions)
    }

    fn journal_matches(&mut self, record: &SlotRecord) -> Result<Vec<String>, DaemonError> {
        let mut matches = Vec::new();
        if let Some(session) = record.session() {
            // PAM moves the session into session-N.scope, so its output is
            // found by the unit's SyslogIdentifier, not by the unit name.
            // The device's session runs its compositor in units it starts:
            // everything its account runs is the session's.
            let uid = apply::session_uid(self.apply.as_mut(), &session)?;
            if session.stand_in {
                matches.push(format!("SYSLOG_IDENTIFIER=tilo-session-{}", record.name));
            } else {
                matches.push(format!("_SYSTEMD_UNIT={}", session.unit));
            }
            // A login's output reaches the journal through greetd's stream.
            if session.login.is_some() && session.unit != GREETER_UNIT {
                matches.push(format!("_SYSTEMD_UNIT={GREETER_UNIT}"));
            }
            matches.push(format!("UNIT={}", session.unit));
            matches.push(format!("_UID={uid}"));
        } else {
            let generation = self.slots.generation(&record.name, record.current)?;
            for unit in generation
                .components
                .values()
                .flat_map(|e| e.info.units.iter())
            {
                matches.push(format!("_SYSTEMD_UNIT={unit}"));
                matches.push(format!("UNIT={unit}"));
            }
            matches.push("SYSLOG_IDENTIFIER=systemd-sysext".into());
        }
        Ok(matches)
    }

    /// Re-activate a slot's current generation with a session restart,
    /// after its seat assignment changed.
    fn reseat(&mut self, record: &mut SlotRecord) -> Result<Vec<Action>, DaemonError> {
        let generation = self.slots.generation(&record.name, record.current)?;
        let layers = self.layer_paths(&generation)?;
        let actions = apply::activate(
            self.apply.as_mut(),
            record,
            &generation,
            &layers,
            Policy::SessionRestart,
            &Restarts::default(),
            SessionChange::Restart,
        )?;
        self.slots.save(record)?;
        self.verify_parity(record.session())?;
        Ok(actions)
    }

    fn claim(&mut self, name: &str, owner: Owner, steal: bool) -> Result<SeatResult, DaemonError> {
        let (mut record, _) = self.user_slot(name)?;
        if record.kind == SlotKind::Device {
            return Err(DaemonError::DeviceSlot("moved between seats".into()));
        }
        if record.seat0.is_some() {
            return Ok(SeatResult {
                slot: record.name,
                seat: "seat0".into(),
                changed: false,
                actions: Vec::new(),
            });
        }
        let mut actions = Vec::new();
        if let Some(mut holder) = self.slots.list()?.into_iter().find(|r| r.seat0.is_some()) {
            if !steal {
                let lease = holder.seat0.unwrap_or_else(|| Lease {
                    slot: holder.name.clone(),
                    owner: Owner::default(),
                    since: String::new(),
                });
                let owner = [lease.owner.host, lease.owner.worktree, lease.owner.branch]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" ");
                return Err(DaemonError::SeatBusy {
                    slot: lease.slot,
                    owner,
                    since: lease.since,
                });
            }
            holder.seat0 = None;
            actions.extend(self.reseat(&mut holder)?);
        } else {
            self.apply.stop_unit(GREETER_UNIT)?;
            actions.push(Action::StopUnit {
                unit: GREETER_UNIT.into(),
            });
        }
        record.seat0 = Some(Lease {
            slot: record.name.clone(),
            owner,
            since: slots::now(),
        });
        actions.extend(self.reseat(&mut record)?);
        Ok(SeatResult {
            slot: record.name,
            seat: "seat0".into(),
            changed: true,
            actions,
        })
    }

    fn release(&mut self, name: &str) -> Result<SeatResult, DaemonError> {
        let (mut record, _) = self.user_slot(name)?;
        if record.kind == SlotKind::Device {
            return Err(DaemonError::DeviceSlot("moved between seats".into()));
        }
        let own_seat = |r: &SlotRecord| r.seat.clone().unwrap_or_else(|| "none".into());
        if record.seat0.is_none() {
            return Ok(SeatResult {
                seat: own_seat(&record),
                slot: record.name,
                changed: false,
                actions: Vec::new(),
            });
        }
        record.seat0 = None;
        let mut actions = self.reseat(&mut record)?;
        self.apply.start_unit(GREETER_UNIT)?;
        actions.push(Action::StartUnit {
            unit: GREETER_UNIT.into(),
        });
        Ok(SeatResult {
            seat: own_seat(&record),
            slot: record.name,
            changed: true,
            actions,
        })
    }

    fn gc(&mut self, request: &tilo_vm_protocol::Gc) -> Result<GcResult, DaemonError> {
        let (keep, dry_run) = (request.keep, request.dry_run);
        let mut referenced = BTreeSet::new();
        let mut removed_generations = Vec::new();
        let mut records = self.slots.list()?;
        if !records.iter().any(|r| r.name == VM_SLOT) {
            records.push(self.slots.load(VM_SLOT)?);
        }
        // The device slot's generations that carry a held layer are the
        // person's system waiting on a deliberate choice: gc keeps them.
        let held = image::held_hashes(&self.root);
        for record in &records {
            let also_keep: BTreeSet<u64> = if record.name == DEVICE_SLOT {
                self.slots
                    .generations(&record.name)
                    .into_iter()
                    .filter(|number| {
                        self.slots
                            .generation(&record.name, *number)
                            .is_ok_and(|g| g.layers.iter().any(|layer| held.contains(layer)))
                    })
                    .collect()
            } else {
                BTreeSet::new()
            };
            let (layers, removed) = self.slots.prune(record, keep, &also_keep, dry_run)?;
            referenced.extend(layers);
            removed_generations.extend(removed.into_iter().map(|n| format!("{}/{n}", record.name)));
        }
        let min_age = request
            .min_age_seconds
            .map_or(GC_MIN_AGE, Duration::from_secs);
        let collected = self.layers.collect(&referenced, min_age, dry_run)?;
        let (removed_layers, freed_bytes) = (collected.removed, collected.freed);
        let kept_young = collected
            .young
            .into_iter()
            .map(|(hash, age)| KeptLayer {
                component: self
                    .layers
                    .provenance(&hash)
                    .map_or_else(|| "?".into(), |p| p.component),
                hash,
                age_seconds: age.as_secs(),
                eligible_in_seconds: min_age.saturating_sub(age).as_secs(),
            })
            .collect();
        if !dry_run {
            // Uploads and staging dirs left by dropped connections.
            for dir in [&self.uploads_dir, &self.root.join("staging")] {
                for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
                    let old = entry
                        .metadata()
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.elapsed().ok());
                    if old.is_some_and(|age| age > GC_MIN_AGE) {
                        let path = entry.path();
                        let _ = if path.is_dir() {
                            fs::remove_dir_all(&path)
                        } else {
                            fs::remove_file(&path)
                        };
                    }
                }
            }
        }
        // Build dirs, least recently used first; never one mid-build.
        let free = self
            .apply
            .disk_space(&self.root)
            .map(|(free, _)| free.saturating_add(if dry_run { freed_bytes } else { 0 }));
        let locks = &self.locks;
        let plan = self.targets.plan_eviction(
            request.keep_targets.unwrap_or(DEFAULT_KEEP_TARGETS),
            request.free_goal_bytes,
            free,
            &mut |worktree| locks.src(worktree).is_err(),
        );
        let mut removed_targets = Vec::new();
        let mut freed_target_bytes: u64 = 0;
        for record in plan {
            if !dry_run {
                let Ok(_guard) = self.locks.src(&record.worktree) else {
                    continue;
                };
                self.targets.evict(&record.worktree)?;
            }
            freed_target_bytes = freed_target_bytes.saturating_add(record.bytes);
            removed_targets.push(record.worktree);
        }
        Ok(GcResult {
            dry_run,
            removed_layers,
            removed_generations,
            freed_bytes,
            removed_targets,
            freed_target_bytes,
            storage: self.storage(),
            kept_young,
            min_age_seconds: min_age.as_secs(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apply::session_unit;

    #[test]
    fn network_wake_requires_the_normal_protocol_handshake() {
        let root = tempfile::tempdir().unwrap();
        let mut daemon = Daemon::new(
            root.path(),
            Box::new(RecordingApply::default()),
            None,
            BaseInfo::default(),
        );
        let request = Request::NetworkWake(tilo_vm_protocol::NetworkWake {
            profile: "--invalid".into(),
            enabled: Some(true),
        });
        assert!(matches!(
            daemon.handle(request.clone()),
            Err(DaemonError::HelloRequired)
        ));
        daemon
            .handle(Request::Hello(tilo_vm_protocol::Hello {
                protocol: PROTOCOL_VERSION,
                client_version: String::new(),
            }))
            .unwrap();
        let error = daemon.handle(request).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("invalid NetworkManager profile UUID")
        );
    }

    #[test]
    fn slot_show_reads_imported_provenance_and_keeps_it_after_failed_activation() {
        let root = tempfile::tempdir().unwrap();
        let payload = tempfile::tempdir().unwrap();
        fs::create_dir_all(payload.path().join("usr/bin")).unwrap();
        let binary = payload.path().join("usr/bin/shell");
        fs::write(&binary, "baseline").unwrap();
        let mut original = Provenance::local("shell");
        original.source_commit = Some("ab".repeat(20));
        original.target = Some("x86_64-unknown-linux-gnu".into());
        original.cargo_lock_hash = Some("cd".repeat(32));
        let store = LayerStore::new(root.path());
        let (baseline, _) = store
            .import_path(payload.path(), &original, &mut |_| Ok(()))
            .unwrap();
        fs::write(&binary, "candidate").unwrap();
        let mut candidate = original.clone();
        candidate.source_commit = Some("ef".repeat(20));
        let (next, _) = store
            .import_path(payload.path(), &candidate, &mut |_| Ok(()))
            .unwrap();
        let mut apply = RecordingApply::default();
        apply.readiness_replies.extend([
            Ok(()), // fresh slot
            Ok(()), // baseline
            Err("candidate failed readiness".into()),
            Ok(()), // recovered baseline
        ]);
        let mut daemon = Daemon::new(root.path(), Box::new(apply), None, BaseInfo::default());
        daemon
            .create("proof", SlotKind::Headless, Owner::default(), &[], false)
            .unwrap();
        assert!(daemon.show("proof").unwrap().provenance.is_empty());
        let info = Some(ComponentInfo::new(Scope::Session, vec![]));
        daemon
            .swap("proof", "shell", &baseline, info.clone())
            .unwrap();
        let before = daemon.show("proof").unwrap();
        assert_eq!(before.provenance["shell"], original);
        assert!(
            before
                .files
                .iter()
                .any(|file| file.path == "/usr/bin/shell")
        );
        let error = daemon.swap("proof", "shell", &next, info).unwrap_err();
        assert!(error.to_string().contains("restored generation 1"));
        let recovered = daemon.show("proof").unwrap();
        assert_eq!(recovered.slot.components["shell"], baseline);
        assert_eq!(recovered.provenance, before.provenance);
        // Old imports without metadata remain observable as unknown, rather
        // than inventing provenance from the component name or swap request.
        fs::remove_file(
            root.path()
                .join("layers")
                .join(&baseline)
                .join("provenance.json"),
        )
        .unwrap();
        let unknown = daemon.show("proof").unwrap();
        assert!(unknown.provenance.is_empty());
        assert_eq!(unknown.slot.components["shell"], baseline);
        assert!(!unknown.files.is_empty());
        let mut legacy = serde_json::to_value(&unknown).unwrap();
        legacy.as_object_mut().unwrap().remove("provenance");
        let legacy: SlotShowResult = serde_json::from_value(legacy).unwrap();
        assert!(legacy.provenance.is_empty());
    }

    #[test]
    fn failed_swap_restores_generation_and_reports_failed_recovery() {
        for (slot, recovery) in [
            (DEVICE_SLOT, Ok(())),
            (DEVICE_SLOT, Err("old desktop also failed".to_owned())),
            ("review", Ok(())),
            ("review", Err("old desktop also failed".to_owned())),
        ] {
            let root = tempfile::tempdir().unwrap();
            let mirror = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let mut apply = RecordingApply::with_login("owner");
            apply.mirror = Some(mirror.clone());
            if slot != DEVICE_SLOT {
                apply.readiness_replies.push_back(Ok(()));
            }
            apply.readiness_replies.extend([
                Ok(()),
                Err("new desktop failed".to_owned()),
                recovery.clone(),
            ]);
            let mut daemon = Daemon::new(root.path(), Box::new(apply), None, BaseInfo::default());
            if slot != DEVICE_SLOT {
                daemon
                    .create(slot, SlotKind::Headless, Owner::default(), &[], false)
                    .unwrap();
            }
            // Start with a real prior generation, then exercise the production
            // swap path: preparation, activation, readiness and recovery.
            let hash = "ab".repeat(32);
            fs::create_dir_all(root.path().join("layers").join(&hash).join("usr/bin")).unwrap();
            let info = Some(ComponentInfo::new(Scope::Session, vec![]));
            assert_eq!(
                daemon
                    .swap(slot, "shell", &hash, info.clone())
                    .unwrap()
                    .generation,
                1
            );
            mirror.lock().unwrap().clear();
            let next = "cd".repeat(32);
            fs::create_dir_all(root.path().join("layers").join(&next).join("usr/bin")).unwrap();
            let error = daemon.swap(slot, "shell", &next, info).unwrap_err();
            let detail = error.to_string();
            assert!(detail.contains("new desktop failed"), "{detail}");
            let record = daemon.load_slot(slot).unwrap();
            assert_eq!(
                record.current, 1,
                "a failed candidate never becomes current"
            );
            if slot == DEVICE_SLOT {
                assert_eq!(record.device.as_ref().unwrap().account, "owner");
            } else {
                assert_eq!(record.kind, SlotKind::Headless);
            }
            let calls = mirror.lock().unwrap().clone();
            assert_eq!(
                calls
                    .iter()
                    .filter(|c| c.starts_with("wait_session_ready "))
                    .count(),
                2
            );
            assert!(
                !calls
                    .iter()
                    .any(|c| c.contains("reset_desktop") || c.contains("remove_user"))
            );
            if recovery.is_ok() {
                assert!(detail.contains("restored generation 1"), "{detail}");
                assert_eq!(daemon.slots.pending(slot), None);
            } else {
                assert!(
                    detail.contains("recovery to generation 1 also failed"),
                    "{detail}"
                );
                assert!(detail.contains("old desktop also failed"), "{detail}");
                assert_eq!(daemon.slots.pending(slot), Some(1));
            }
        }
    }

    const TOOLKIT: &str = "2026.07+native.0123456789abcdef0123456789abcdef";

    fn user_package(id: &str, runtime: &str) -> package_store::registry::InstalledPackage {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "payload_root": format!("/home/quickuser/.local/share/tilo-apps/pkgs/{id}"),
            "manifest_path": format!("/home/quickuser/.local/share/tilo-apps/pkgs/{id}.pkg/manifest.toml"),
            "runtime": runtime,
        }))
        .unwrap()
    }

    /// Roll the owner's laptop back to the bare image after it took an
    /// apps+runtime cohort shipping `TOOLKIT` and then a shell. Returns the
    /// rollback's outcome, the calls the refusal check left behind, and
    /// the generation current afterwards.
    fn roll_cohort_back_to_image(
        packages: Vec<package_store::registry::InstalledPackage>,
        image_runtimes: &[&str],
    ) -> (Result<ApplyResult, DaemonError>, usize, usize, u64) {
        let root = tempfile::tempdir().unwrap();
        let mirror = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut apply = RecordingApply::with_login("quickuser");
        apply.mirror = Some(mirror.clone());
        apply.user_packages = packages;
        apply.image_runtimes = image_runtimes.iter().map(|v| (*v).to_owned()).collect();
        let mut daemon = Daemon::new(root.path(), Box::new(apply), None, BaseInfo::default());
        daemon
            .handle(Request::Hello(tilo_vm_protocol::Hello {
                protocol: PROTOCOL_VERSION,
                client_version: String::new(),
            }))
            .unwrap();
        let first = "01".repeat(32);
        let next = "02".repeat(32);
        let layers = root.path().join("layers");
        fs::create_dir_all(
            layers
                .join(&first)
                .join("usr/lib/tilo-runtime")
                .join(TOOLKIT),
        )
        .unwrap();
        fs::create_dir_all(layers.join(&next).join("usr/bin")).unwrap();
        let session = Some(ComponentInfo::new(Scope::Session, Vec::new()));
        let cohort = daemon
            .swap(DEVICE_SLOT, COHORT_COMPONENT, &first, session.clone())
            .unwrap();
        daemon.swap(DEVICE_SLOT, "shell", &next, session).unwrap();
        // Rolling back a component the cohort does not carry is never checked.
        daemon
            .rollback(DEVICE_SLOT, Some(cohort.generation))
            .unwrap();
        let before = mirror.lock().unwrap().len();
        let result = daemon.rollback(DEVICE_SLOT, Some(0));
        let after = mirror.lock().unwrap().len();
        let current = daemon.load_slot(DEVICE_SLOT).unwrap().current;
        (result, before, after, current)
    }

    #[test]
    fn cohort_rollback_without_user_packages_is_allowed() {
        let (result, _, _, current) = roll_cohort_back_to_image(Vec::new(), &[]);
        assert_eq!(result.unwrap().generation, 0);
        assert_eq!(current, 0);
    }

    #[test]
    fn cohort_rollback_ignores_packages_that_need_no_app_toolkit() {
        // A runtime package (it may not declare a runtime) and an app that
        // names none, as `tilo-install` records them.
        let (result, _, _, current) = roll_cohort_back_to_image(
            vec![
                user_package("so.tilo.runtime.chromium", ""),
                user_package("org.example.static", ""),
            ],
            &[],
        );
        assert_eq!(result.unwrap().generation, 0);
        assert_eq!(current, 0);
    }

    #[test]
    fn cohort_rollback_refuses_to_strand_a_package_on_the_removed_toolkit() {
        let runtime = format!("tilo-runtime/{TOOLKIT}");
        let (result, before, after, current) = roll_cohort_back_to_image(
            vec![
                user_package("so.tilo.runtime.chromium", ""),
                user_package("org.example.thin", &runtime),
                user_package("org.example.elsewhere", "tilo-runtime/2025.01+native.ffff"),
            ],
            &[],
        );
        let error = result.unwrap_err().to_string();
        assert!(
            error.contains(&format!("org.example.thin needs {runtime}")),
            "{error}"
        );
        assert!(
            error.contains("tilo app remove org.example.thin"),
            "{error}"
        );
        assert!(!error.contains("chromium"), "{error}");
        assert!(!error.contains("org.example.elsewhere"), "{error}");
        assert_eq!(after, before, "refusal must precede any layer side effect");
        assert_ne!(current, 0);
    }

    #[test]
    fn cohort_rollback_allows_a_package_whose_toolkit_the_image_ships() {
        let (result, _, _, current) = roll_cohort_back_to_image(
            vec![user_package(
                "org.example.thin",
                &format!("tilo-runtime/{TOOLKIT}"),
            )],
            &[TOOLKIT],
        );
        assert_eq!(result.unwrap().generation, 0);
        assert_eq!(current, 0);
    }

    #[test]
    fn only_a_single_tilo_runtime_directory_names_a_native_toolkit() {
        assert_eq!(
            native_toolkit(&format!("tilo-runtime/{TOOLKIT}")),
            Some(TOOLKIT)
        );
        for runtime in [
            "",
            "tilo-runtime/",
            "tilo-runtime/..",
            "tilo-runtime/a/b",
            "other/1.0",
        ] {
            assert_eq!(native_toolkit(runtime), None, "{runtime}");
        }
    }

    /// Every connection's hello takes the keep-awake lease; `awake` holds it
    /// longer, reports it, and releases it, and the next hello takes it again.
    #[test]
    fn every_connection_keeps_the_machine_awake_until_released() {
        let root = tempfile::tempdir().unwrap();
        let mut daemon = Daemon::new(
            root.path(),
            Box::new(RecordingApply::default()),
            None,
            BaseInfo::default(),
        );
        let hello = |daemon: &mut Daemon| -> HelloResult {
            serde_json::from_value(
                daemon
                    .handle(Request::Hello(tilo_vm_protocol::Hello {
                        protocol: PROTOCOL_VERSION,
                        client_version: String::new(),
                    }))
                    .unwrap(),
            )
            .unwrap()
        };
        let awake = |daemon: &mut Daemon, seconds, off| -> AwakeLease {
            serde_json::from_value(
                daemon
                    .handle(Request::Awake(tilo_vm_protocol::Awake { seconds, off }))
                    .unwrap(),
            )
            .unwrap()
        };
        let now = tilo_clock::unix_now();
        let lease = hello(&mut daemon).awake.unwrap();
        assert!(lease.held, "{lease:?}");
        assert_eq!(lease.problem, None);
        let until = lease.until.unwrap();
        assert!((now + 1800..now + 1810).contains(&until), "{until}");
        assert!(root.path().join(awake::LEASE_FILE).is_file());

        let held = awake(&mut daemon, Some(7200), false);
        assert!(held.held);
        assert!(held.until.unwrap() >= now + 7200);
        assert_eq!(held.hold_until, held.until);
        // Reporting changes nothing; a new connection keeps the hold.
        assert_eq!(awake(&mut daemon, None, false).until, held.until);
        assert_eq!(hello(&mut daemon).awake.unwrap().until, held.until);

        let off = awake(&mut daemon, None, true);
        assert_eq!(off, AwakeLease::default());
        assert!(hello(&mut daemon).awake.unwrap().held);
    }

    #[test]
    fn vm_component_refresh_restarts_declared_live_owner_user_services() {
        let root = tempfile::tempdir().unwrap();
        let mirror = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut apply = RecordingApply::with_login("quickuser");
        apply.mirror = Some(mirror.clone());
        let mut daemon = Daemon::new(root.path(), Box::new(apply), None, BaseInfo::default());
        daemon
            .handle(Request::Hello(tilo_vm_protocol::Hello {
                protocol: PROTOCOL_VERSION,
                client_version: String::new(),
            }))
            .unwrap();
        let device = daemon.load_slot(DEVICE_SLOT).unwrap();
        daemon.slots.save(&device).unwrap();
        let hash = "cd".repeat(32);
        fs::create_dir_all(root.path().join("layers").join(&hash).join("usr/bin")).unwrap();
        let mut info = ComponentInfo::new(Scope::Vm, vec!["fs.service".into()]);
        info.user_units = vec!["tilo-fs-cloud.service".into()];
        let result: ApplyResult = serde_json::from_value(
            daemon
                .handle(Request::Swap(tilo_vm_protocol::Swap {
                    slot: VM_SLOT.into(),
                    component: "fs".into(),
                    layer: hash,
                    info: Some(info),
                }))
                .unwrap(),
        )
        .unwrap();
        let user_service = Action::RestartUserUnit {
            user: "quickuser".into(),
            unit: "tilo-fs-cloud.service".into(),
        };
        let desktop = Action::RestartDesktop {
            user: "quickuser".into(),
        };
        let user_ix = result
            .actions
            .iter()
            .position(|action| action == &user_service)
            .unwrap();
        let desktop_ix = result
            .actions
            .iter()
            .position(|action| action == &desktop)
            .unwrap();
        assert!(
            user_ix < desktop_ix,
            "owner services must observe the new binary before the desktop restarts"
        );
        let calls = mirror.lock().unwrap().clone();
        assert!(calls.iter().any(|call| call == "try-restart fs.service"));
        assert!(
            calls
                .iter()
                .any(|call| call == "restart_user_unit quickuser tilo-fs-cloud.service")
        );
        assert!(
            !calls.iter().any(|call| call == "restart user@1000.service"),
            "the owner login remains intact"
        );
    }

    /// A laptop's `device` slot (the owner's greetd login) and the `vm` slot
    /// merge one system-wide layer set: neither drops the other's layers, and
    /// every change restarts the owner's desktop in place.
    #[test]
    fn a_laptops_device_slot_shares_the_system_wide_layers_with_the_vm_slot() {
        let root = tempfile::tempdir().unwrap();
        let mirror = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut apply = RecordingApply::with_login("quickuser");
        apply.mirror = Some(mirror.clone());
        let mut daemon = Daemon::new(root.path(), Box::new(apply), None, BaseInfo::default());
        daemon
            .handle(Request::Hello(tilo_vm_protocol::Hello {
                protocol: PROTOCOL_VERSION,
                client_version: String::new(),
            }))
            .unwrap();
        let layer = |n: u8| {
            let hash = format!("{n:02x}").repeat(32);
            fs::create_dir_all(root.path().join("layers").join(&hash).join("usr/bin")).unwrap();
            hash
        };
        let mut apply = |request: Request| -> ApplyResult {
            serde_json::from_value(daemon.handle(request).unwrap()).unwrap()
        };
        let swap = |slot: &str, component: &str, layer: &str, scope: Scope| {
            Request::Swap(tilo_vm_protocol::Swap {
                slot: slot.into(),
                component: component.into(),
                layer: layer.into(),
                info: Some(ComponentInfo::new(scope, Vec::new())),
            })
        };
        let restarted = Action::RestartDesktop {
            user: "quickuser".into(),
        };

        let shell = apply(swap(DEVICE_SLOT, "shell", &layer(1), Scope::Session));
        assert_eq!(shell.policy, Policy::SessionRestart);
        assert!(
            shell.actions.contains(&Action::SysextRefresh),
            "{:?}",
            shell.actions
        );
        assert_eq!(shell.actions.last(), Some(&restarted));
        let fs = apply(swap(VM_SLOT, "fs", &layer(2), Scope::Vm));
        assert!(fs.actions.contains(&restarted), "{:?}", fs.actions);
        let back = apply(Request::Rollback(tilo_vm_protocol::Rollback {
            slot: DEVICE_SLOT.into(),
            to: Some(0),
        }));
        assert_eq!(back.generation, 0);

        let calls = mirror.lock().unwrap().clone();
        let linked: Vec<&str> = calls
            .iter()
            .filter(|c| c.starts_with("link_sysext"))
            .map(String::as_str)
            .collect();
        assert_eq!(
            linked,
            ["link_sysext 1", "link_sysext 2", "link_sysext 1"],
            "{calls:?}"
        );
        assert!(
            !calls
                .iter()
                .any(|c| c.starts_with("write_dropin") || c.starts_with("stop ")),
            "the owner's login is never stopped: {calls:?}"
        );
        assert_eq!(
            calls
                .iter()
                .filter(|c| c.starts_with("restart_desktop"))
                .count(),
            3
        );
    }

    #[test]
    fn creating_with_outputs_applies_the_layout_every_time() {
        let root = tempfile::tempdir().unwrap();
        let mut daemon = Daemon::new(
            root.path(),
            Box::new(RecordingApply::default()),
            None,
            BaseInfo::default(),
        );
        daemon
            .handle(Request::Hello(tilo_vm_protocol::Hello {
                protocol: PROTOCOL_VERSION,
                client_version: String::new(),
            }))
            .unwrap();
        let create = || {
            Request::SlotCreate(tilo_vm_protocol::SlotCreate {
                name: "a".into(),
                kind: SlotKind::Headful,
                owner: Owner::default(),
                outputs: tilo_vm_protocol::display::parse_outputs("2560x1600@2,1920x1080").unwrap(),
                identity: false,
            })
        };
        for _ in 0..2 {
            let result: SlotCreateResult =
                serde_json::from_value(daemon.handle(create()).unwrap()).unwrap();
            assert_eq!(result.slot.outputs.len(), 2);
            assert!(
                result.actions.contains(&Action::SetOutputs {
                    outputs: "2560x1600@2,1920x1080@1".into()
                }),
                "{:?}",
                result.actions
            );
        }
    }

    use crate::apply::RecordingApply;

    #[test]
    fn slot_create_identity_mints_in_the_session_and_restarts_it_fresh() {
        use tilo_vm_protocol::ControlResult;
        let reply = |exit_code, stdout: &str, stderr: &str| ControlResult {
            exit_code,
            stdout: stdout.into(),
            stderr: stderr.into(),
        };
        let mut apply = RecordingApply::default();
        apply.control_replies.extend([
            reply(2, "", "tilo-control: connect: No such file or directory"),
            reply(
                0,
                r#"{"answer":{"created":true,"display_name":"Throwaway test identity","throwaway":true}}"#,
                "",
            ),
            reply(
                0,
                r#"{"answer":{"created":false,"display_name":"Throwaway test identity","throwaway":true}}"#,
                "",
            ),
            reply(1, "", "error [identity_exists]: this user already has an identity"),
        ]);
        let root = tempfile::tempdir().unwrap();
        let mut daemon = Daemon::new(root.path(), Box::new(apply), None, BaseInfo::default());
        daemon
            .handle(Request::Hello(tilo_vm_protocol::Hello {
                protocol: PROTOCOL_VERSION,
                client_version: String::new(),
            }))
            .unwrap();
        let create = |identity| {
            Request::SlotCreate(tilo_vm_protocol::SlotCreate {
                name: "a".into(),
                kind: SlotKind::Headless,
                owner: Owner::default(),
                outputs: Vec::new(),
                identity,
            })
        };
        let session = session_unit("a");
        // Phase `a_new_throwaway_restarts_the_session_with_fresh_desktop_state`.
        {
            let result: SlotCreateResult =
                serde_json::from_value(daemon.handle(create(true)).unwrap()).unwrap();
            let identity = result.identity.expect("the identity answer");
            assert!(identity.created && identity.throwaway);
            let minted = result
                .actions
                .iter()
                .position(|action| action == &Action::ProvisionTestIdentity { created: true })
                .expect("the identity is minted");
            // After minting: the session stops, loses its layout, starts.
            let after: Vec<&Action> = result.actions[minted..]
                .iter()
                .filter(|action| {
                    matches!(action, Action::StopUnit { unit } | Action::StartUnit { unit } if *unit == session)
                        || **action == Action::ResetDesktopState
                })
                .collect();
            assert_eq!(
                after.first(),
                Some(&&Action::StopUnit {
                    unit: session.clone()
                }),
                "{:?}",
                result.actions
            );
            assert_eq!(after.get(1), Some(&&Action::ResetDesktopState));
            assert_eq!(
                after.last(),
                Some(&&Action::StartUnit {
                    unit: session.clone()
                })
            );
        }
        // Phase `a_kept_throwaway_keeps_the_running_session`.
        {
            let result: SlotCreateResult =
                serde_json::from_value(daemon.handle(create(true)).unwrap()).unwrap();
            assert_eq!(
                result.identity.map(|identity| identity.created),
                Some(false)
            );
            assert!(!result.actions.contains(&Action::ResetDesktopState));
            assert!(!result.actions.contains(&Action::StopUnit { unit: session }));
        }
        // Phase `a_refused_identity_fails_the_create`.
        {
            let error = daemon.handle(create(true)).unwrap_err();
            assert!(matches!(error, DaemonError::TestIdentity { .. }), "{error}");
            assert!(error.to_string().contains("identity_exists"), "{error}");
        }
        // Phase `a_plain_create_leaves_identity_alone`.
        {
            let result: SlotCreateResult =
                serde_json::from_value(daemon.handle(create(false)).unwrap()).unwrap();
            assert_eq!(result.identity, None);
            assert!(result.actions.is_empty(), "{:?}", result.actions);
        }
    }

    #[test]
    fn candidate_slot_view_preserves_identity_before_generation_commit() {
        let root = tempfile::tempdir().unwrap();
        let slots = SlotStore::new(root.path());
        let layers = LayerStore::new(root.path());
        let mut apply = RecordingApply::default();
        let (mut proof, _) = slots
            .create("proof", SlotKind::Headless, Owner::default(), &[])
            .unwrap();
        let own_hash = "03".repeat(32);
        let own = layers.path(&own_hash);
        fs::create_dir_all(own.join("usr")).unwrap();
        let (_, generation) = slots
            .prepare_swap(
                &proof,
                "shell",
                &own_hash,
                ComponentInfo::new(Scope::Session, vec![]),
            )
            .unwrap()
            .unwrap();
        slots.set_current(&mut proof, generation.number).unwrap();
        let candidate = layers.path(&"04".repeat(32));
        fs::create_dir_all(candidate.join("usr")).unwrap();
        let before = slots.load(VM_SLOT).unwrap().current;
        prepare_slot_session(
            &slots,
            &layers,
            &mut apply,
            &proof,
            Some(std::slice::from_ref(&candidate)),
        )
        .unwrap();
        assert_eq!(
            apply.prepared_session_layers,
            [("proof".into(), vec![candidate, own])]
        );
        assert_eq!(
            slots.load(VM_SLOT).unwrap().current,
            before,
            "candidate view preparation must not commit a generation before readiness"
        );
        for unit in ["user@60001.service", "tilo-vm-session@proof.service"] {
            assert!(apply.dropins[&(unit.into(), apply::LAYERS_DROPIN.into())].contains("Environment=\"TILO_IDENTITY_ROOT=/home/tds-proof/.local/share/tilo/test-identity\"\n"));
        }
    }

    #[test]
    fn restored_slot_views_preserve_global_layers_and_refuse_missing_layers() {
        let root = tempfile::tempdir().unwrap();
        let slots = SlotStore::new(root.path());
        let layers = LayerStore::new(root.path());
        let locks = Locks::new(root.path());
        let mut apply = RecordingApply::with_login("owner");
        let mut expected = Vec::new();
        for (slot, n) in [(VM_SLOT, 1u8), (DEVICE_SLOT, 2), ("proof", 3)] {
            let mut record = if slot == VM_SLOT {
                slots.load(slot).unwrap()
            } else if slot == DEVICE_SLOT {
                slots.load_with(slot, apply.device.clone()).unwrap()
            } else {
                slots
                    .create(slot, SlotKind::Headless, Owner::default(), &[])
                    .unwrap()
                    .0
            };
            slots.save(&record).unwrap();
            let hash = format!("{n:02x}").repeat(32);
            let path = layers.path(&hash);
            fs::create_dir_all(path.join("usr")).unwrap();
            expected.push(path);
            let (_, generation) = slots
                .prepare_swap(
                    &record,
                    "component",
                    &hash,
                    ComponentInfo::new(Scope::Session, Vec::new()),
                )
                .unwrap()
                .unwrap();
            slots.set_current(&mut record, generation.number).unwrap();
        }
        assert!(restore_slots(&slots, &layers, &locks, &mut apply).is_empty());
        assert_eq!(
            apply.prepared_session_layers,
            [("proof".into(), expected.clone())]
        );
        for unit in ["user@60001.service", "tilo-vm-session@proof.service"] {
            assert!(apply.dropins[&(unit.into(), apply::LAYERS_DROPIN.into())].contains(
                "Environment=\"TILO_IDENTITY_ROOT=/home/tds-proof/.local/share/tilo/test-identity\"\n"
            ));
        }
        let reload = apply
            .calls
            .iter()
            .position(|call| call == "reload")
            .unwrap();
        let start = apply
            .calls
            .iter()
            .position(|call| call == &format!("start {}", session_unit("proof")))
            .unwrap();
        assert!(reload < start);
        let held = locks.slot("proof").unwrap();
        let mut busy = RecordingApply::with_login("owner");
        let issues = restore_slots(&slots, &layers, &locks, &mut busy);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].slot, "proof");
        assert!(busy.prepared_session_layers.is_empty());
        assert!(!busy.calls.iter().any(|call| call.starts_with("start ")));
        drop(held);
        let held = locks.vm_exclusive().unwrap();
        let issues = restore_slots(&slots, &layers, &locks, &mut busy);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].slot, VM_SLOT);
        assert!(busy.prepared_session_layers.is_empty());
        drop(held);
        fs::remove_dir_all(&expected[2]).unwrap();
        let mut missing = RecordingApply::with_login("owner");
        let issues = restore_slots(&slots, &layers, &locks, &mut missing);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].slot, "proof");
        assert!(!missing.calls.iter().any(|call| call.starts_with("start ")));
    }

    /// gc deletes only what nobody holds: it never touches the device
    /// slot's current generation or a generation with a held layer, nor the
    /// held layer itself, however few generations it is asked to keep.
    #[test]
    fn gc_never_touches_the_device_slots_current_or_held_generation() {
        let root = tempfile::tempdir().unwrap();
        let slots = SlotStore::new(root.path());
        let layers = LayerStore::new(root.path());
        let apply = RecordingApply::with_login("owner");
        let mut record = slots.load_with(DEVICE_SLOT, apply.device.clone()).unwrap();
        slots.save(&record).unwrap();
        let mut hashes = Vec::new();
        for n in 1..=4u8 {
            let hash = format!("{n:02x}").repeat(32);
            fs::create_dir_all(layers.path(&hash).join("usr")).unwrap();
            let (_, generation) = slots
                .prepare_swap(
                    &record,
                    &format!("component-{n}"),
                    &hash,
                    ComponentInfo::new(Scope::Session, Vec::new()),
                )
                .unwrap()
                .unwrap();
            slots.set_current(&mut record, generation.number).unwrap();
            hashes.push(hash);
        }
        // Back on generation 1 (component-1 only); generation 2 added the
        // held layer, generations 3 and 4 build on it.
        slots.set_current(&mut record, 1).unwrap();
        let held = serde_json::json!([{
            "layer": hashes[1], "component": "component-2", "reason": "speaks shell-ui 9"
        }]);
        fs::write(root.path().join("held-layers.json"), held.to_string()).unwrap();
        let mut daemon = Daemon::new(root.path(), Box::new(apply), None, BaseInfo::default());
        let result = daemon
            .gc(&tilo_vm_protocol::Gc {
                keep: 0,
                dry_run: false,
                keep_targets: None,
                free_goal_bytes: None,
                min_age_seconds: Some(0),
            })
            .unwrap();
        // Every generation from 2 on names the held layer: all are kept.
        assert_eq!(slots.generations(DEVICE_SLOT), vec![1, 2, 3, 4]);
        assert!(result.removed_generations.is_empty(), "{result:?}");
        for hash in &hashes {
            assert!(layers.require(hash).is_ok(), "{hash} kept");
        }
    }

    /// The device slot comes up on its current generation at every boot:
    /// the restore links the `vm` and `device` slots' layers and refreshes
    /// `/usr` when the links `systemd-sysext` merged at boot were not that
    /// set, and leaves a set that already was merged alone.
    #[test]
    fn the_device_slot_re_merges_its_layers_at_boot() {
        let root = tempfile::tempdir().unwrap();
        let slots = SlotStore::new(root.path());
        let layers = LayerStore::new(root.path());
        let mut apply = RecordingApply::with_login("owner");
        let mut expected = Vec::new();
        for (slot, n) in [(VM_SLOT, 1u8), (DEVICE_SLOT, 2)] {
            let mut record = if slot == VM_SLOT {
                slots.load(slot).unwrap()
            } else {
                slots.load_with(slot, apply.device.clone()).unwrap()
            };
            slots.save(&record).unwrap();
            let hash = format!("{n:02x}").repeat(32);
            let path = layers.path(&hash);
            fs::create_dir_all(path.join("usr")).unwrap();
            expected.push(path);
            let (_, generation) = slots
                .prepare_swap(
                    &record,
                    "component",
                    &hash,
                    ComponentInfo::new(Scope::Session, Vec::new()),
                )
                .unwrap()
                .unwrap();
            slots.set_current(&mut record, generation.number).unwrap();
        }

        // The boot merged no links: they are put back and /usr refreshed.
        merge_system_layers(&slots, &layers, &mut apply).unwrap();
        assert_eq!(apply.sysext, expected);
        assert_eq!(apply.calls, ["link_sysext 2", "sysext_refresh"]);

        // Already merged: nothing is refreshed.
        apply.calls.clear();
        merge_system_layers(&slots, &layers, &mut apply).unwrap();
        assert_eq!(apply.calls, ["link_sysext 2"]);

        // The daemon's boot restore does the same.
        let mirror = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut watched = RecordingApply::with_login("owner");
        watched.mirror = Some(mirror.clone());
        let mut daemon = Daemon::new(root.path(), Box::new(watched), None, BaseInfo::default());
        assert!(daemon.restore(Some("composefs:one")).is_empty());
        let calls = mirror.lock().unwrap().clone();
        assert_eq!(
            calls
                .iter()
                .filter(|c| c.starts_with("link_sysext") || c.as_str() == "sysext_refresh")
                .collect::<Vec<_>>(),
            ["link_sysext 2", "sysext_refresh"],
            "a fresh boot's links are relinked and merged"
        );

        // A layer the device's generation names is gone: reported, links
        // left as they were.
        fs::remove_dir_all(&expected[1]).unwrap();
        apply.calls.clear();
        assert!(merge_system_layers(&slots, &layers, &mut apply).is_err());
        assert!(apply.calls.is_empty(), "{:?}", apply.calls);
    }

    #[test]
    fn daemon_sessions_and_seats_are_restored_and_required_for_exec() {
        // Phase `exec_needs_a_user_slot_with_a_running_session`.
        {
            let root = tempfile::tempdir().unwrap();
            let mut daemon = Daemon::new(
                root.path(),
                Box::new(RecordingApply::default()),
                None,
                BaseInfo::default(),
            );
            SlotStore::new(root.path())
                .create("a", SlotKind::Headless, Owner::default(), &[])
                .unwrap();
            let run = |daemon: &mut Daemon, slot: &str| {
                daemon
                    .exec(slot, &["true".into()], Duration::from_secs(5), 1024)
                    .unwrap_err()
            };
            // The recording backend has no session process.
            let stopped = run(&mut daemon, "a");
            assert!(
                matches!(stopped, DaemonError::SessionStopped(_)),
                "{stopped}"
            );
            assert!(stopped.hint().contains("tilo-vm slot restart --slot a"));
            let vm = run(&mut daemon, VM_SLOT);
            assert!(vm.hint().contains("tilo-vm vm ssh"), "{}", vm.hint());
            assert!(matches!(
                run(&mut daemon, "nope"),
                DaemonError::SlotNotFound(_)
            ));
        }
        // Phase `headful_seats_are_recreated_at_start`.
        {
            let root = tempfile::tempdir().unwrap();
            let slots = SlotStore::new(root.path());
            let locks = Locks::new(root.path());
            for (name, kind) in [
                ("a", SlotKind::Headful),
                ("b", SlotKind::Headless),
                ("c", SlotKind::Headful),
            ] {
                let (mut record, _) = slots.create(name, kind, Owner::default(), &[]).unwrap();
                if kind == SlotKind::Headful {
                    record.seat = Some(format!("seat-{name}"));
                    slots.save(&record).unwrap();
                }
            }
            let mut apply = RecordingApply::default();
            // A slot another command is changing is left to that command.
            let busy = locks.slot("c").unwrap();
            assert!(reconcile_seats(&slots, &locks, &mut apply).is_empty());
            assert_eq!(apply.calls, ["allocate_seat a"]);
            drop(busy);
        }
        // Phase `restore_brings_back_seats_then_sessions`.
        {
            let tmp = tempfile::tempdir().unwrap();
            let slots = SlotStore::new(tmp.path());
            let locks = Locks::new(tmp.path());
            for (name, kind) in [("a", SlotKind::Headful), ("b", SlotKind::Headless)] {
                let (mut record, _) = slots.create(name, kind, Owner::default(), &[]).unwrap();
                if kind == SlotKind::Headful {
                    record.seat = Some(format!("seat-{name}"));
                    slots.save(&record).unwrap();
                }
            }
            let mut apply = RecordingApply::default();
            assert!(
                restore_slots(&slots, &LayerStore::new(tmp.path()), &locks, &mut apply).is_empty()
            );
            let calls = &apply.calls;
            let seat = calls.iter().position(|c| c == "allocate_seat a").unwrap();
            let session = calls
                .iter()
                .position(|c| c == &format!("start {}", session_unit("a")))
                .unwrap();
            assert!(seat < session, "{calls:?}");
            assert!(
                calls.contains(&format!("start {}", session_unit("b"))),
                "{calls:?}"
            );
        }
        // Phase `installed_line_names_the_app`.
        {
            assert_eq!(
                parse_installed("tilo-install: WARN self-signed\ninstalled: so.tilo.notes 0.1.0\n"),
                (Some("so.tilo.notes".into()), Some("0.1.0".into()))
            );
            assert_eq!(parse_installed("nothing"), (None, None));
        }
    }

    #[test]
    fn abi_checks_refuse_foreign_builds_and_compare_targets_and_locks() {
        // Phase `abi_refuses_foreign_arch_and_builder`.
        {
            let base = BaseInfo {
                image_digest: Some("sha256:base".into()),
                builder_image_digest: Some("sha256:builder".into()),
                toolchain: Some("1.97.1".into()),
                ..BaseInfo::default()
            };
            let mut layer = Provenance::local("shell");
            layer.target = Some("aarch64-unknown-linux-gnu".into());
            assert!(matches!(
                abi_check(&layer, &base, "x86_64"),
                Err(DaemonError::AbiMismatch { .. })
            ));
            layer.target = Some("x86_64-unknown-linux-gnu".into());
            assert!(abi_check(&layer, &base, "x86_64").unwrap().is_empty());
            layer.builder_image_digest = Some("sha256:other".into());
            assert!(matches!(
                abi_check(&layer, &base, "x86_64"),
                Err(DaemonError::AbiMismatch { .. })
            ));
            layer.builder_image_digest = None;
            layer.base_image_digest = Some("sha256:older".into());
            layer.toolchain = Some("1.96.0".into());
            assert_eq!(abi_check(&layer, &base, "x86_64").unwrap().len(), 2);
            // Nothing known about the VM: nothing to compare.
            assert!(
                abi_check(&layer, &BaseInfo::default(), "x86_64")
                    .unwrap()
                    .is_empty()
            );
        }
        // Phase `abi_compares_the_dev_images_target_and_cargo_lock`.
        {
            let base = dev_base();
            let mut layer = Provenance::local("shell");
            layer.target = Some("x86_64-unknown-linux-gnu".into());
            layer.cargo_lock_hash = base.cargo_lock_hash.clone();
            layer.toolchain = base.toolchain.clone();
            // A layer from some builder image, onto an image that is its own
            // builder: nothing to compare, so neither a refusal nor a warning.
            layer.builder_image_digest = Some("sha256:ci-builder".into());
            assert!(abi_check(&layer, &base, "x86_64").unwrap().is_empty());
            layer.builder = Some("host".into());
            assert!(abi_check(&layer, &base, "x86_64").unwrap().is_empty());

            // The same architecture, another triple.
            layer.target = Some("x86_64-unknown-linux-musl".into());
            let error = abi_check(&layer, &base, "x86_64").unwrap_err();
            assert_eq!(
                error.to_string(),
                "layer target is x86_64-unknown-linux-musl but the VM expects x86_64-unknown-linux-gnu"
            );
            layer.target = Some("x86_64-unknown-linux-gnu".into());

            layer.cargo_lock_hash = Some("fedcba9876543210fedc".into());
            // A statically linked component only warns.
            let warnings = abi_check(&layer, &base, "x86_64").unwrap();
            assert_eq!(warnings.len(), 1, "{warnings:?}");
            assert!(warnings[0].contains("Cargo.lock"), "{warnings:?}");
            // Thin apps share libruntime_dylib.so with the image: refused.
            layer.cargo_profile = Some("thin-app".into());
            let error = abi_check(&layer, &base, "x86_64").unwrap_err();
            assert_eq!(
                error.to_string(),
                "layer Cargo.lock is fedcba987654 but the VM expects 0123456789ab"
            );
            let rpc = error.to_rpc();
            assert!(rpc.hint.contains("Cargo.lock"), "{}", rpc.hint);
            assert!(rpc.hint.contains("tilo-vm vm upgrade"), "{}", rpc.hint);
            assert_eq!(rpc.see.as_deref(), Some("tilo-vm guide swapping"));

            // A CI layer records no Cargo.lock: nothing to compare.
            layer.cargo_lock_hash = None;
            assert!(abi_check(&layer, &base, "x86_64").unwrap().is_empty());
        }
    }

    /// base.json as a dev image writes it: no builder image, but a target
    /// triple and a Cargo.lock hash.
    fn dev_base() -> BaseInfo {
        BaseInfo {
            image_digest: Some("sha256:base".into()),
            builder_image_digest: None,
            toolchain: Some("rustc 1.97.1".into()),
            target: Some("x86_64-unknown-linux-gnu".into()),
            cargo_lock_hash: Some("0123456789abcdef0123".into()),
        }
    }
}
