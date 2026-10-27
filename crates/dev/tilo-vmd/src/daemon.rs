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
        // A board's device slot is the machine's session, which only the
        // machine names; its record alone does not.
        let device = (record.name == DEVICE_SLOT)
            .then(|| apply.device_session())
            .flatten();
        let record = match slots.load_with(&record.name, device) {
            Ok(record) => record,
            Err(error) => {
                issues.push(issue(&record.name, error));
                continue;
            }
        };
        // A board's own session is enabled and may have started on the bare
        // image before this ran: it restarts onto the view it now binds.
        let started = |apply: &mut dyn Apply| {
            if !session.stand_in && apply.unit_state(&session.unit) == "active" {
                let uid = apply::session_uid(apply, &session)?;
                apply::restart_session(apply, &session, uid).map(drop)
            } else {
                apply.start_unit(&session.unit)
            }
        };
        if let Err(error) =
            prepare_slot_session(slots, layers, apply, &record, None).and_then(|()| started(apply))
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
    /// Where a verified apps+runtime cohort writes the archives it derives:
    /// beside the layer store's staging, which garbage collection clears of
    /// anything a dropped swap left.
    fn cohort_staging(&self) -> Result<PathBuf, DaemonError> {
        let staging = self.root.join("staging");
        fs::create_dir_all(&staging).map_err(DaemonError::io("create the cohort staging"))?;
        Ok(staging)
    }

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
        // A long session (an upload, a build) is activity as much as a new
        // connection is; `awake` sets the lease itself.
        if !matches!(request, Request::Awake(_))
            && awake::renewal(
                awake::read(&self.lease_path()),
                tilo_clock::unix_now(),
                awake::Change::Request,
            )
            .1
        {
            self.renew_awake(awake::Change::Request);
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
                &self.cohort_staging()?,
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
                self.check_interfaces(&record, &to)?;
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

    /// Refuse generation `to` of slot `record` when a component of the
    /// matched session set, its own layer's or the image's, was built for
    /// another system-interface declaration than the one `to` leaves the slot
    /// reading, or a VM-wide runtime service (fs) serves another runtime
    /// version. For the `vm` slot, refuse when its runtime services would
    /// leave any slot reading another runtime version
    /// ([`crate::interfaces`]). Checked before anything is activated.
    ///
    /// # Errors
    /// [`DaemonError::InvalidParams`] naming what to swap together; a
    /// missing layer.
    pub(crate) fn check_interfaces(
        &self,
        record: &SlotRecord,
        to: &Generation,
    ) -> Result<(), DaemonError> {
        let image = image::recorded_declaration(&self.root);
        let conflict = if record.name == VM_SLOT {
            // Only a change of a runtime service is judged against the
            // slots: a vmd or greeter swap never waits on fs.
            let current = (record.current != 0)
                .then(|| self.slots.generation(VM_SLOT, record.current))
                .transpose()?;
            let changed = crate::interfaces::VM_RUNTIME_SERVICES
                .iter()
                .any(|service| {
                    to.components.get(*service)
                        != current.as_ref().and_then(|c| c.components.get(*service))
                });
            if !changed {
                return Ok(());
            }
            let vm_services = self.interface_members(to)?;
            let mut slots = Vec::new();
            for slot in self.slots.list()? {
                if slot.name == VM_SLOT || slot.current == 0 {
                    continue;
                }
                let current = self.slots.generation(&slot.name, slot.current)?;
                slots.push((slot.name.clone(), self.interface_members(&current)?));
            }
            crate::interfaces::vm_conflict(image.as_ref(), &slots, &vm_services)
        } else {
            let vm_services = match self.slots.load(VM_SLOT) {
                Ok(vm) if vm.current != 0 => {
                    self.interface_members(&self.slots.generation(VM_SLOT, vm.current)?)?
                }
                _ => Vec::new(),
            };
            crate::interfaces::conflict(image.as_ref(), &self.interface_members(to)?, &vm_services)
        };
        match conflict {
            None => Ok(()),
            Some(conflict) => Err(DaemonError::InvalidParams(format!(
                "generation {} is not activated: {conflict}",
                to.number
            ))),
        }
    }

    /// Each component of `generation` as the interface check sees it.
    fn interface_members(
        &self,
        generation: &Generation,
    ) -> Result<Vec<crate::interfaces::Member>, DaemonError> {
        let relative = tilo_vm_protocol::SYSTEM_INTERFACES
            .strip_prefix('/')
            .unwrap_or(tilo_vm_protocol::SYSTEM_INTERFACES);
        let mut members = Vec::new();
        for (component, entry) in &generation.components {
            let declaration = self.layers.require(&entry.layer)?.join(relative);
            members.push(crate::interfaces::Member {
                component: component.clone(),
                layer: entry.layer.clone(),
                provenance: self.layers.provenance(&entry.layer),
                ships: declaration
                    .exists()
                    .then(|| tilo_vm_protocol::Declaration::read(&declaration)),
            });
        }
        Ok(members)
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
                if restarts.sessions {
                    actions.extend(self.restart_all_sessions(&restarts.user_units, &layers)?);
                }
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
mod tests;
