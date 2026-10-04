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
    Action, AppInstallResult, ApplyResult, BaseInfo, BootConfirmResult, ComponentInfo, DEVICE_SLOT,
    GcResult, HelloResult, InputAction, InputResult, Issue, KeptLayer, LayerImportResult,
    LayerSource, LayerUploadBeginResult, LayerUploadChunkResult, Lease, LogsResult, Manifest,
    OutputSpec, Owner, PROTOCOL_VERSION, Policy, Provenance, Request, Scope, SeatResult,
    ShotResult, SlotCreateResult, SlotFile, SlotInfo, SlotKind, SlotRmResult, SlotShowResult,
    StatusResult, Storage, TestIdentity, UPLOAD_CHUNK_BYTES, VM_SLOT, VmTools,
};

use crate::apply::{self, Apply, GREETER_UNIT, Session, SessionChange};
use crate::error::DaemonError;
use crate::layers::LayerStore;
use crate::locks::{Guard, Locks};
use crate::slots::{self, Generation, SlotRecord, SlotStore};
use crate::targets::TargetStore;

/// The domain of a greetd session unless the image says otherwise.
pub const DEFAULT_DOMAIN: &str = "unconfined_t";

/// Layers and uploads younger than this survive gc: they may belong to a
/// swap that is still in flight on another connection.
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

/// Bring every slot back after the VM boots: re-create headful slots' vkms
/// seats, then start each slot's session on its current generation. Run
/// once at boot by `tilo-vm-restore.service`; a slot that cannot come back
/// is reported, and the others still start.
pub fn restore_slots(slots: &SlotStore, locks: &Locks, apply: &mut dyn Apply) -> Vec<Issue> {
    let mut issues = reconcile_seats(slots, locks, apply);
    let failed: BTreeSet<String> = issues.iter().map(|i| i.slot.clone()).collect();
    for record in slots.list().unwrap_or_default() {
        let Some(session) = record.session() else {
            continue;
        };
        if failed.contains(&record.name) {
            continue;
        }
        if let Err(error) = apply.start_unit(&session.unit) {
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
        if provenance.cargo_profile.as_deref() == Some("thin-app") {
            return Err(mismatch("Cargo.lock", short(layer), short(vm)));
        }
        warnings.push(format!(
            "layer Cargo.lock {} differs from the VM's {}: dependencies drift from the image",
            short(layer),
            short(vm)
        ));
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
    fn sessions(&self) -> Result<Vec<Session>, DaemonError> {
        Ok(self
            .slots
            .list()?
            .iter()
            .filter_map(SlotRecord::session)
            .collect())
    }

    /// Every slot on disk, and the `device` slot wherever the machine runs
    /// its own session, swapped into or not.
    fn listed_slots(&mut self) -> Result<Vec<SlotRecord>, DaemonError> {
        let mut records = self.slots.list()?;
        if let Some(device) = self.apply.device_session() {
            match records.iter_mut().find(|r| r.name == DEVICE_SLOT) {
                Some(record) => record.device = Some(device),
                None => records.push(self.slots.load_with(DEVICE_SLOT, Some(device))?),
            }
            records.sort_by(|a, b| a.name.cmp(&b.name));
        }
        Ok(records)
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

    /// Bring every slot back after the VM boots; see [`restore_slots`].
    pub fn restore(&mut self) -> Vec<Issue> {
        let issues = restore_slots(&self.slots, &self.locks, self.apply.as_mut());
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
            Request::LayerUploadBegin(_) => to_value(&self.upload_begin()?),
            Request::LayerUploadChunk(p) => to_value(&self.upload_chunk(&p.upload, &p.data)?),
            Request::LayerImport(p) => {
                let _vm = self.locks.vm_shared()?;
                to_value(&self.import(p.source, &p.provenance)?)
            }
            Request::LayerList(_) => to_value(&self.layers.list()?),
            Request::Swap(p) => {
                let _locks = self.slot_locks(&p.slot)?;
                to_value(&self.swap(&p.slot, &p.component, &p.layer, p.info)?)
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
                to_value(&self.apply.control(&session, &p.args)?)
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

    fn input(&mut self, name: &str, actions: &[InputAction]) -> Result<InputResult, DaemonError> {
        tilo_vm_protocol::input::validate(actions).map_err(DaemonError::InvalidParams)?;
        let (record, _) = self.user_slot(name)?;
        let seat = match (&record.seat0, record.kind, &record.seat) {
            (Some(_), _, _) => "seat0".to_owned(),
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
        let user = session.user;
        let (path, uploaded) = match source {
            LayerSource::Upload(id) => (
                self.uploads
                    .remove(&id)
                    .ok_or(DaemonError::UploadNotFound(id))?,
                true,
            ),
            LayerSource::Path(path) => (PathBuf::from(path), false),
        };
        let output = self.apply.install_app(&user, &path);
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
        for record in self.slots.list()? {
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
        let session = unit
            .as_ref()
            .map_or_else(|| "n/a".into(), |s| self.apply.unit_state(&s.unit));
        let parity = match &unit {
            Some(unit) if session == "active" => self.parity(unit),
            _ => None,
        };
        Ok(SlotInfo {
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
        for (component, entry) in &generation.components {
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
        })
    }

    fn layer_paths(&self, generation: &Generation) -> Result<Vec<PathBuf>, DaemonError> {
        generation
            .layers
            .iter()
            .map(|h| self.layers.require(h))
            .collect()
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
                &[],
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
        let layers = self.layer_paths(&generation)?;
        let (policy, units) = if record.kind == SlotKind::Vm {
            let units = generation
                .components
                .values()
                .flat_map(|e| e.info.units.clone())
                .collect();
            (Policy::VmRefresh, units)
        } else {
            (Policy::SessionRestart, Vec::new())
        };
        let mut actions = apply::activate(
            self.apply.as_mut(),
            &mut record,
            &generation,
            &layers,
            policy,
            &units,
            SessionChange::Restart,
        )?;
        self.slots.save(&record)?;
        self.slots.clear_pending(name)?;
        if record.kind == SlotKind::Vm {
            actions.extend(self.restart_all_sessions()?);
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
        self.layers.require(layer)?;
        let provenance = self
            .layers
            .provenance(layer)
            .unwrap_or_else(|| Provenance::local(component));
        let warnings = abi_check(&provenance, &self.base, std::env::consts::ARCH)?;
        let info = self.component_info(&record, component, info);
        apply::check_scope(&record, component, &info)?;
        let Some((from, to)) = self.slots.prepare_swap(&record, component, layer, info)? else {
            return Ok(ApplyResult {
                warnings,
                ..unchanged(record)
            });
        };
        let mut result = self.transition(&mut record, &from, &to)?;
        result.warnings.splice(0..0, warnings);
        Ok(result)
    }

    fn rollback(&mut self, name: &str, target: Option<u64>) -> Result<ApplyResult, DaemonError> {
        let mut record = self.load_slot(name)?;
        let (from, to) = self.slots.rollback_target(&record, target)?;
        if from.number == to.number {
            return Ok(unchanged(record));
        }
        self.transition(&mut record, &from, &to)
    }

    fn transition(
        &mut self,
        record: &mut SlotRecord,
        from: &Generation,
        to: &Generation,
    ) -> Result<ApplyResult, DaemonError> {
        let (policy, units) = apply::policy(record, from, to)?;
        let layers = self.layer_paths(to)?;
        self.slots.mark_pending(&record.name, to.number)?;
        let mut actions = apply::activate(
            self.apply.as_mut(),
            record,
            to,
            &layers,
            policy,
            &units,
            SessionChange::Keep,
        )?;
        let mut boot = None;
        let mut warnings = Vec::new();
        match policy {
            Policy::VmRefresh => {
                self.slots.set_current(record, to.number)?;
                actions.extend(self.restart_all_sessions()?);
                let sessions = self.sessions()?;
                self.verify_parity(sessions)?;
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
                self.slots.set_current(record, to.number)?;
                self.verify_parity(record.session())?;
            }
            Policy::NoRestart => self.slots.set_current(record, to.number)?,
        }
        Ok(ApplyResult {
            slot: record.name.clone(),
            generation: to.number,
            changed: true,
            policy,
            actions,
            warnings,
            boot,
        })
    }

    /// Slot sessions snapshot `/usr` when they start, so a refreshed VM
    /// `/usr` reaches them only through a restart.
    fn restart_all_sessions(&mut self) -> Result<Vec<Action>, DaemonError> {
        let mut actions = Vec::new();
        for session in self.sessions()? {
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
            &[],
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
        for record in &records {
            let (layers, removed) = self.slots.prune(record, keep, dry_run)?;
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
            assert!(restore_slots(&slots, &locks, &mut apply).is_empty());
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
