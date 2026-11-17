use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::Serialize;
use tilo_vm_protocol::{
    AccountCreateResult, AccountFile, AccountInfo, AccountKind, AccountRmResult, AccountShowResult,
    Action, AppInstallResult, ApplyResult, AwakeLease, BaseInfo, BootConfirmResult, ComponentInfo,
    DEVICE_ACCOUNT, GcResult, HelloResult, ImageChange, InputAction, InputResult, Issue, KeptLayer,
    LayerImportResult, LayerSource, LayerUploadBeginResult, LayerUploadChunkResult, Lease,
    LogsResult, Manifest, OutputSpec, Owner, PROTOCOL_VERSION, Policy, Provenance, Request, Scope,
    SeatResult, ShotResult, StatusResult, Storage, TestIdentity, UPLOAD_CHUNK_BYTES, VM_ACCOUNT,
    VmTools,
};

use crate::accounts::{self, AccountEntry, AccountStore, Generation};
use crate::apply::{self, Apply, GREETER_UNIT, Restarts, Session, SessionChange};
use crate::awake;
use crate::error::DaemonError;
use crate::image;
use crate::layers::LayerStore;
use crate::locks::{Guard, Locks};
use crate::targets::TargetStore;

pub const DEFAULT_DOMAIN: &str = "unconfined_t";

mod runtime_retention;
/// Layers and uploads younger than this survive gc: they may belong to a swap still in flight on
/// another connection.
mod userspace;

const GC_MIN_AGE: Duration = Duration::from_secs(3600);

const CONTROL_SOCKET_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a person's onboarding may take once its session is up, identity creation included. A
/// first-login session binds its control socket only after it has set up onboarding (47 s on a
/// loaded builder, against 10 s without), so waiting for the socket is part of this bound, not of
/// [`CONTROL_SOCKET_TIMEOUT`].
const ONBOARDING_TIMEOUT: Duration = Duration::from_secs(180);
const FS_START_TIMEOUT: Duration = Duration::from_secs(60);
const FS_UNIT: &str = "fs.service";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IdentityRequest {
    None,
    Throwaway,
    Device,
}

impl IdentityRequest {
    fn of(create: &tilo_vm_protocol::AccountCreate) -> Result<Self, DaemonError> {
        match (create.identity, create.device_identity) {
            (true, true) => Err(DaemonError::InvalidParams(
                "an account's user gets one identity: a throwaway or the VM's person".into(),
            )),
            (true, false) => Ok(Self::Throwaway),
            (false, true) => Ok(Self::Device),
            (false, false) => Ok(Self::None),
        }
    }
}

fn onboarding_target_names(reply: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(reply) else {
        return Vec::new();
    };
    let targets = value
        .get("answer")
        .unwrap_or(&value)
        .get("targets")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    targets
        .iter()
        .filter_map(|target| target.get("name")?.as_str())
        .filter(|name| name.starts_with(onboarding_walk::PREFIX))
        .map(str::to_owned)
        .collect()
}

/// Whether a `state_dump` answer says the desktop is between two layouts; an answer that does
/// not say counts as settled.
fn desktop_in_transition(reply: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(reply) else {
        return false;
    };
    let answer = value.get("answer").unwrap_or(&value);
    answer
        .get("state")
        .unwrap_or(answer)
        .pointer("/chrome/in_transition")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

/// An account's session as the onboarding walk sees it: its controls listed and pressed by name through
/// the control socket.
struct OnboardingScreen<'a> {
    daemon: &'a mut Daemon,
    session: &'a apply::Session,
    deadline: Instant,
}

impl onboarding_walk::Screen for OnboardingScreen<'_> {
    type Error = DaemonError;

    fn controls(&mut self) -> Result<Vec<String>, DaemonError> {
        self.daemon.onboarding_targets(self.session, self.deadline)
    }

    fn press(&mut self, name: &'static str) -> Result<(), DaemonError> {
        let request = serde_json::json!({ "op": "click_named", "name": name });
        let reply = self
            .daemon
            .apply
            .control(self.session, &["raw".to_owned(), request.to_string()])?;
        if reply.exit_code != 0 {
            return Err(DaemonError::DeviceIdentity {
                account: self.session.account.clone(),
                detail: format!("pressing {name}: {}", reply.stderr.trim()),
            });
        }
        Ok(())
    }

    fn settled(&mut self) -> Result<bool, DaemonError> {
        let request = serde_json::json!({ "op": "state_dump" });
        let reply = self
            .daemon
            .apply
            .control(self.session, &["raw".to_owned(), request.to_string()])?;
        if reply.exit_code != 0 {
            return Err(DaemonError::DeviceIdentity {
                account: self.session.account.clone(),
                detail: format!("reading the session's state: {}", reply.stderr.trim()),
            });
        }
        Ok(!desktop_in_transition(reply.stdout.trim()))
    }

    /// Onboarding is gone and its identity has certified the machine.
    fn finished(&mut self, controls: &[String]) -> bool {
        controls.is_empty() && self.daemon.apply.machine_identity().certified
    }
}

pub const DEFAULT_KEEP_TARGETS: u32 = 2;

pub struct Daemon {
    pub(crate) layers: LayerStore,
    accounts: AccountStore,
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
    /// The wall clock the keep-awake lease is read against: the process's, unless a test hands the
    /// daemon one of its own.
    clock: fn() -> u64,
    seat_issues: Option<Vec<Issue>>,
}

/// Is `tool` on `PATH`, or in `/usr/local/bin`, which a non-login SSH shell may lack?
pub fn on_path(tool: &str) -> bool {
    let paths = std::env::var_os("PATH").unwrap_or_default();
    std::env::split_paths(&paths)
        .chain([PathBuf::from("/usr/local/bin")])
        .any(|dir| dir.join(tool).is_file())
}

pub fn detect_tools() -> VmTools {
    VmTools {
        cargo: on_path("cargo"),
        rsync: on_path("rsync"),
        python3: on_path("python3"),
        sccache: on_path("sccache"),
        podman: on_path("podman"),
    }
}

/// Re-create the vkms card of every headful account: configfs is empty after a reboot, and an account
/// whose card is missing cannot start its session. Accounts another command is changing are left to
/// it.
pub fn reconcile_seats(
    accounts: &AccountStore,
    locks: &Locks,
    apply: &mut dyn Apply,
) -> Vec<Issue> {
    let mut issues = Vec::new();
    for record in accounts.list().unwrap_or_default() {
        if record.kind != AccountKind::Headful || record.seat.is_none() {
            continue;
        }
        let Ok(_guard) = locks.account(&record.name) else {
            continue;
        };
        if let Err(error) = apply.allocate_seat(&record.name, record.connectors()) {
            issues.push(Issue {
                account: record.name.clone(),
                problem: format!("its vkms seat could not be re-created: {error}"),
                hint: format!(
                    "tilo-vm account restart {}   (tilo-vm doctor checks vkms and configfs)",
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

/// The merge that reaches the machine's `/usr`: the `vm` account's layers first, then the `device`
/// account's. A board's own session and a laptop's login both run in the host's mount namespace, so
/// their layers and the `vm` account's are one `systemd-sysext` set: whichever of the two accounts
/// applies, the other's layers stay merged. An account of any other kind stacks its own layers over
/// that set, in a view only its own session sees.
fn system_layers(
    accounts: &AccountStore,
    layers: &LayerStore,
    record: &AccountEntry,
    own: Vec<PathBuf>,
) -> Result<Vec<PathBuf>, DaemonError> {
    let mut machine: Vec<PathBuf> = Vec::new();
    let mut add = |paths: Vec<PathBuf>| {
        for layer in paths {
            if !machine.contains(&layer) {
                machine.push(layer);
            }
        }
    };
    for (account, mine) in [
        (VM_ACCOUNT, record.kind == AccountKind::Vm),
        (DEVICE_ACCOUNT, record.kind == AccountKind::Device),
    ] {
        if mine {
            add(own.clone());
        } else if let Some(stored) = accounts.stored(account)? {
            add(layer_paths(
                layers,
                &accounts.generation(account, stored.current)?,
            )?);
        }
    }
    if !matches!(record.kind, AccountKind::Vm | AccountKind::Device) {
        add(own);
    }
    Ok(machine)
}

fn prepare_account_session(
    accounts: &AccountStore,
    layers: &LayerStore,
    apply: &mut dyn Apply,
    record: &AccountEntry,
    system: Option<&[PathBuf]>,
) -> Result<(), DaemonError> {
    let session = record
        .session()
        .ok_or_else(|| DaemonError::InvalidParams("account has no session".into()))?;
    let generation = accounts.generation(&record.name, record.current)?;
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
        system_layers(accounts, layers, record, own)?
    };
    if record.kind == AccountKind::Device && session.login.is_none() {
        // A board's supervisor starts the compositor as a system transient unit in the host's mount
        // namespace, and no drop-in can bind a view under a transient unit, so the layers merge
        // into the host `/usr` instead. Those links survive a reboot, so `systemd-sysext` has
        // already merged them here and only a changed set is refreshed.
        if apply.link_sysext(&paths)? {
            apply.sysext_refresh()?;
        }
        return Ok(());
    }
    let mut dropin = apply.session_layer_dropin(&record.name, generation.number, &paths)?;
    // The drop-in `apply::activate` wrote: the VM's person keeps running on the machine's identity
    // tree after a reboot.
    dropin.push_str(&apply::identity_environment(apply, record, &session)?);
    let uid = apply::session_uid(apply, &session)?;
    for unit in apply::layered_units(&session, uid) {
        apply.write_dropin(&unit, apply::LAYERS_DROPIN, &dropin)?;
    }
    apply.reload()
}

/// Link and merge the VM-wide sysext set as the `vm` and `device` accounts' current generations name
/// it. `systemd-sysext.service` merges at boot whatever `/var/lib/extensions` holds, so a set a
/// transition cut short is put right and `/usr` refreshed; a set already linked is left merged as
/// it is.
pub fn merge_system_layers(
    accounts: &AccountStore,
    layers: &LayerStore,
    apply: &mut dyn Apply,
) -> Result<(), DaemonError> {
    let vm = accounts.load(VM_ACCOUNT)?;
    let own = layer_paths(layers, &accounts.generation(VM_ACCOUNT, vm.current)?)?;
    let paths = system_layers(accounts, layers, &vm, own)?;
    if apply.link_sysext(&paths)? {
        apply.sysext_refresh()?;
    }
    Ok(())
}

/// Bring every account back after the VM boots: re-create headful accounts' vkms seats, then start each
/// account's session on its current generation. An account that cannot come back is reported, and the
/// others still start.
pub fn restore_accounts(
    accounts: &AccountStore,
    layers: &LayerStore,
    locks: &Locks,
    apply: &mut dyn Apply,
) -> Vec<Issue> {
    let issue = |account: &str, error: DaemonError| Issue {
        account: account.into(),
        problem: format!("its session view could not be restored: {error}"),
        hint: "tilo-vm doctor; then retry the normal account restart".into(),
    };
    let _global = match locks.vm_shared() {
        Ok(guard) => guard,
        Err(error) => return vec![issue(VM_ACCOUNT, error)],
    };
    let records = match accounts.list() {
        Ok(records) => records,
        Err(error) => return vec![issue(VM_ACCOUNT, error)],
    };
    let mut issues = reconcile_seats(accounts, locks, apply);
    let failed: BTreeSet<String> = issues.iter().map(|i| i.account.clone()).collect();
    for record in records {
        let Some(session) = record.session() else {
            continue;
        };
        if failed.contains(&record.name) || session.login.is_some() {
            continue;
        }
        let _account = match locks.account(&record.name) {
            Ok(guard) => guard,
            Err(error) => {
                issues.push(issue(&record.name, error));
                continue;
            }
        };
        let device = (record.name == DEVICE_ACCOUNT)
            .then(|| apply.device_session())
            .flatten();
        let record = match accounts.load_with(&record.name, device) {
            Ok(record) => record,
            Err(error) => {
                issues.push(issue(&record.name, error));
                continue;
            }
        };
        // A board's own session is enabled and may have started on the bare image before this ran:
        // it restarts onto the merged `/usr`.
        let started = |apply: &mut dyn Apply| {
            if !session.stand_in && apply.unit_state(&session.unit) == "active" {
                let uid = apply::session_uid(apply, &session)?;
                apply::restart_session(apply, &session, uid).map(drop)
            } else {
                apply.start_unit(&session.unit)
            }
        };
        if let Err(error) = prepare_account_session(accounts, layers, apply, &record, None)
            .and_then(|()| started(apply))
        {
            issues.push(Issue {
                account: record.name.clone(),
                problem: format!("its session did not start at boot: {error}"),
                hint: format!(
                    "tilo-vm logs {}   then   tilo-vm account restart {}",
                    record.name, record.name
                ),
            });
        }
    }
    issues
}

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

fn unchanged(record: AccountEntry) -> ApplyResult {
    ApplyResult {
        account: record.name,
        generation: record.current,
        changed: false,
        policy: Policy::NoRestart,
        actions: Vec::new(),
        cohort_selection: Vec::new(),
        warnings: Vec::new(),
        boot: None,
    }
}

/// Refuse layers the VM cannot run and warn about differences it can. Refused: another CPU
/// architecture or target triple, another builder image, or another `Cargo.lock` than the image was
/// built from, each compared only when both sides record it; a dev image records no builder image,
/// so there the target, toolchain and lock identify the builder.
///
/// # Errors
///
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
    if let (Some(layer), Some(vm)) = (&provenance.builder_image_digest, &base.builder_image_digest)
        && layer != vm
    {
        return Err(mismatch("builder image", layer, vm));
    }
    let mut warnings = Vec::new();
    // Thin apps link the image's `libruntime_dylib.so`, whose symbols carry crate hashes, so
    // another `Cargo.lock` breaks them; statically linked components only drift from the image's
    // dependency set.
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

/// The version of a `tilo-runtime/<version>` runtime, the directory it occupies under
/// `/usr/lib/tilo-runtime`; `None` for any other runtime.
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
    /// Where a verified apps+runtime cohort writes the archives it derives: beside the layer
    /// store's staging, which gc clears of anything a dropped swap left.
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
            accounts: AccountStore::new(root),
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
            clock: tilo_clock::unix_now,
        }
    }

    /// Read the keep-awake lease against `clock` instead of the process's wall clock, so a test can
    /// hold it still.
    #[cfg(test)]
    pub(crate) fn with_clock(mut self, clock: fn() -> u64) -> Self {
        self.clock = clock;
        self
    }

    /// The `SELinux` domain a greetd session runs in on this image.
    #[must_use]
    pub fn with_expected_domain(mut self, domain: impl Into<String>) -> Self {
        self.expected_domain = domain.into();
        self
    }

    /// How an account's session differs from a greetd login; the device's own session is the production
    /// one and is not compared.
    fn parity(&mut self, session: &Session) -> Option<String> {
        if !session.stand_in {
            return None;
        }
        let process = self.apply.session_process(session)?;
        apply::parity_problem(&process, &self.expected_domain)
    }

    /// After starting sessions: refuse to report success for one that did not come up, or is not
    /// equivalent to a production greetd session.
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
                        account: session.account,
                        detail: state,
                        log: log.trim_end().to_owned(),
                    });
                }
                let log = self
                    .apply
                    .journal(
                        &[
                            format!("_SYSTEMD_UNIT={}", session.unit),
                            format!("SYSLOG_IDENTIFIER=tilo-session-{}", session.account),
                        ],
                        12,
                    )
                    .unwrap_or_default();
                return Err(DaemonError::SessionFailed {
                    account: session.account,
                    state,
                    log: log.trim_end().to_owned(),
                });
            }
            if let Some(detail) = self.parity(&session) {
                return Err(DaemonError::SessionParity {
                    account: session.account,
                    detail,
                });
            }
        }
        Ok(())
    }

    fn sessions(&mut self) -> Result<Vec<Session>, DaemonError> {
        Ok(self
            .stored_accounts()?
            .iter()
            .filter_map(AccountEntry::session)
            .collect())
    }

    /// Every account on disk, with the machine's own session found again for the `device` account: a
    /// login's session and processes change with every login.
    fn stored_accounts(&mut self) -> Result<Vec<AccountEntry>, DaemonError> {
        let mut records = self.accounts.list()?;
        if let Some(record) = records.iter_mut().find(|r| r.name == DEVICE_ACCOUNT) {
            *record = match self.load_account(DEVICE_ACCOUNT) {
                Ok(fresh) => fresh,
                Err(DaemonError::NoDeviceSession) => AccountEntry {
                    device: None,
                    ..record.clone()
                },
                Err(error) => return Err(error),
            };
        }
        Ok(records)
    }

    /// Every account on disk, and the `device` account wherever the machine runs its own session.
    fn listed_accounts(&mut self) -> Result<Vec<AccountEntry>, DaemonError> {
        let mut records = self.stored_accounts()?;
        if !records.iter().any(|r| r.name == DEVICE_ACCOUNT)
            && let Some(device) = self.apply.device_session()
        {
            records.push(self.accounts.load_with(DEVICE_ACCOUNT, Some(device))?);
            records.sort_by(|a, b| a.name.cmp(&b.name));
        }
        Ok(records)
    }

    /// The layers to merge system-wide for `record`: the `vm` account and a laptop's `device` account
    /// share one `systemd-sysext` set, the `vm` account's first. Other accounts merge that set with their
    /// own.
    fn system_layers(
        &self,
        record: &AccountEntry,
        own: Vec<PathBuf>,
    ) -> Result<Vec<PathBuf>, DaemonError> {
        system_layers(&self.accounts, &self.layers, record, own)
    }

    fn load_account(&mut self, name: &str) -> Result<AccountEntry, DaemonError> {
        let device = if name == DEVICE_ACCOUNT {
            self.apply.device_session()
        } else {
            None
        };
        self.accounts.load_with(name, device)
    }

    /// Record `booted` as the image the accounts run on (see [`crate::image`]); never changes an account
    /// or deletes a layer.
    pub fn reconcile_image(&mut self, booted: &str) -> Result<Option<ImageChange>, DaemonError> {
        image::ensure_root(&self.root)?;
        let _vm = self.locks.vm_exclusive()?;
        let booted = image::BootedImage::read(
            booted,
            self.base.image_digest.clone(),
            Path::new(tilo_vm_protocol::SYSTEM_INTERFACES),
        );
        image::reconcile(&self.root, &self.accounts, &self.layers, &booted)
    }

    /// Bring every account back after the VM boots: record the booted image when the early-boot unit
    /// has not, merge the VM-wide layers, then start every account session.
    pub fn restore(&mut self, booted: Option<&str>) -> Vec<Issue> {
        let mut issues = Vec::new();
        if let Some(booted) = booted
            && let Err(error) = self.reconcile_image(booted)
        {
            issues.push(Issue {
                account: VM_ACCOUNT.into(),
                problem: format!("the booted image {booted} could not be recorded: {error}"),
                hint: "tilo-vm doctor; the next boot tries again".into(),
            });
        }
        let notice = image::held_notice(&self.root);
        if let Err(error) = self
            .apply
            .publish_notice(image::HELD_NOTICE, notice.as_deref())
        {
            issues.push(Issue {
                account: VM_ACCOUNT.into(),
                problem: format!("the notice about held layers could not be published: {error}"),
                hint: "tilo-vm status names them".into(),
            });
        }
        let merged = self
            .locks
            .vm_exclusive()
            .and_then(|_vm| merge_system_layers(&self.accounts, &self.layers, self.apply.as_mut()));
        if let Err(error) = merged {
            issues.push(Issue {
                account: DEVICE_ACCOUNT.into(),
                problem: format!(
                    "the vm and device accounts' layers could not be merged at boot: {error}"
                ),
                hint: "tilo-vm doctor; then tilo-vm rollback --account device to a generation whose layers are all present".into(),
            });
        }
        issues.extend(restore_accounts(
            &self.accounts,
            &self.layers,
            &self.locks,
            self.apply.as_mut(),
        ));
        self.seat_issues = Some(issues.clone());
        issues
    }

    /// Handle one request.
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
                    &self.accounts,
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
        // A long session (an upload, a build) is activity as much as a new connection is; `awake`
        // sets the lease itself.
        if !matches!(request, Request::Awake(_))
            && awake::renewal(
                awake::read(&self.lease_path()),
                (self.clock)(),
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
                let _locks = self.account_locks(DEVICE_ACCOUNT)?;
                let (_, session) = self.user_account(DEVICE_ACCOUNT)?;
                self.require_device_action_owner(&session)?;
                let uid = self.apply.account_uid(&session.user)?;
                to_value(&self.apply.enroll_device_unlock(uid, &p)?)
            }
            Request::Status(p) => to_value(&self.status(p.account)?),
            Request::AccountCreate(p) => {
                let _locks = self.account_locks(&p.name)?;
                let identity = IdentityRequest::of(&p)?;
                to_value(&self.create(&p.name, p.kind, p.owner, &p.outputs, identity)?)
            }
            Request::AccountList(_) => {
                let records = self.listed_accounts()?;
                to_value(
                    &records
                        .iter()
                        .map(|r| self.info(r))
                        .collect::<Result<Vec<_>, _>>()?,
                )
            }
            Request::AccountRestart(p) => {
                let _locks = self.account_locks(&p.name)?;
                to_value(&self.restart(&p.name)?)
            }
            Request::AccountRm(p) => {
                let _locks = self.account_locks(&p.name)?;
                to_value(&self.remove(&p.name)?)
            }
            Request::AccountShow(p) => to_value(&self.show(&p.name)?),
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
                let _locks = self.account_locks(&p.account)?;
                to_value(&self.swap(
                    &p.account,
                    &p.component,
                    &p.layer,
                    p.info,
                    p.partner.as_ref(),
                )?)
            }
            Request::UserspaceApply(p) => {
                let _locks = self.account_locks(&p.account)?;
                to_value(&self.apply_userspace(p)?)
            }
            Request::Rollback(p) => {
                let _locks = self.account_locks(&p.account)?;
                to_value(&self.rollback(&p.account, p.to)?)
            }
            Request::Logs(p) => {
                let record = self.load_account(&p.account)?;
                let matches = self.journal_matches(&record)?;
                to_value(&LogsResult {
                    text: self.apply.journal(&matches, p.lines)?,
                })
            }
            Request::SeatClaim(p) => {
                let _vm = self.locks.vm_shared()?;
                let _seat = self.locks.account("seat0")?;
                to_value(&self.claim(&p.account, p.owner, p.steal)?)
            }
            Request::SeatRelease(p) => {
                let _vm = self.locks.vm_shared()?;
                let _seat = self.locks.account("seat0")?;
                to_value(&self.release(&p.account)?)
            }
            Request::Shot(p) => {
                let (_, session) = self.user_account(&p.account)?;
                let png = self.apply.shot(&session)?;
                to_value(&ShotResult {
                    png: STANDARD.encode(png),
                })
            }
            Request::Control(p) => {
                let (_, session) = self.user_account(&p.account)?;
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
                let _locks = self.account_locks(&p.account)?;
                to_value(&self.input(&p.account, &p.actions)?)
            }
            Request::AppInstall(p) => {
                let _locks = self.account_locks(&p.account)?;
                to_value(&self.install_app(&p.account, p.source)?)
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
                let _locks = self.account_locks(VM_ACCOUNT)?;
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
        let (_, session) = self.user_account(DEVICE_ACCOUNT)?;
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
        let (record, _) = self.user_account(name)?;
        let seat = match (&record.seat0, record.kind, &record.seat) {
            (Some(_), _, _) => "seat0".to_owned(),
            (None, AccountKind::Device, _)
                if record
                    .device
                    .as_ref()
                    .is_some_and(|device| device.login.is_some()) =>
            {
                // A greetd device login already owns the physical seat and does not need the lease
                // a developer's headful account uses.
                "seat0".to_owned()
            }
            (None, AccountKind::Headful, Some(seat)) => seat.clone(),
            (None, AccountKind::Headful, None) => {
                return Err(DaemonError::NoSession(name.to_owned()));
            }
            _ => return Err(DaemonError::HeadlessInput(name.to_owned())),
        };
        // On seat0 the account sees the VM's real display, not its vkms outputs.
        let geometry = match (&record.seat0, record.outputs.first()) {
            (None, Some(first)) => Some(crate::input::Geometry {
                desktop: crate::display::desktop_extent(&record.outputs),
                touch: (first.width, first.height),
            }),
            _ => None,
        };
        let events = self.apply.input(name, &seat, geometry, actions)?;
        Ok(InputResult {
            account: record.name,
            seat,
            events,
        })
    }

    /// Install a `.tapp` into the account user's app store. Not a generation: the user store shadows
    /// the image's apps, live at their next launch.
    pub fn install_app(
        &mut self,
        account: &str,
        source: LayerSource,
    ) -> Result<AppInstallResult, DaemonError> {
        let (record, session) = self.user_account(account)?;
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
            account: record.name,
            id,
            version,
            output,
        })
    }

    fn lease_path(&self) -> PathBuf {
        self.root.join(awake::LEASE_FILE)
    }

    /// Renew (or release) the keep-awake lease and make the lock follow it. A failure is reported
    /// in the lease, never as the request's error: a machine that cannot be held awake can still be
    /// driven.
    fn renew_awake(&mut self, change: awake::Change) -> AwakeLease {
        let path = self.lease_path();
        let now = (self.clock)();
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

    fn awake_lease(&mut self) -> AwakeLease {
        let now = (self.clock)();
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

    fn status(&mut self, account: Option<String>) -> Result<StatusResult, DaemonError> {
        let records = match account {
            Some(name) => vec![self.load_account(&name)?],
            None => self.listed_accounts()?,
        };
        let accounts = records
            .iter()
            .map(|r| self.info(r))
            .collect::<Result<_, _>>()?;
        let seat0 = self.accounts.list()?.into_iter().find_map(|r| r.seat0);
        Ok(StatusResult {
            protocol: PROTOCOL_VERSION,
            daemon_version: env!("CARGO_PKG_VERSION").into(),
            accounts,
            seat0,
            storage: self.storage(),
            issues: self.issues()?,
            image_change: image::last_change(&self.root),
            accounts_image: image::accounts_image(&self.root),
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

    /// VM-wide work on the `vm` account excludes everything; account work shares the VM and excludes
    /// other work on the same account.
    pub(crate) fn account_locks(&self, name: &str) -> Result<(Guard, Option<Guard>), DaemonError> {
        if name == VM_ACCOUNT {
            Ok((self.locks.vm_exclusive()?, None))
        } else {
            Ok((self.locks.vm_shared()?, Some(self.locks.account(name)?)))
        }
    }

    pub(crate) fn user_account(
        &mut self,
        name: &str,
    ) -> Result<(AccountEntry, Session), DaemonError> {
        let record = self.load_account(name)?;
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

    fn issues(&mut self) -> Result<Vec<Issue>, DaemonError> {
        let mut issues = self.seat_issues.clone().unwrap_or_default();
        for record in self.stored_accounts()? {
            if let Some(number) = self.accounts.pending(&record.name) {
                issues.push(Issue {
                    account: record.name.clone(),
                    problem: format!(
                        "a transition to generation {number} was interrupted; generation {} is recorded as current",
                        record.current
                    ),
                    hint: format!("re-run the swap, or re-apply the current generation: tilo-vm account restart {}", record.name),
                });
            }
            if let Some(session) = record.session() {
                let state = self.apply.unit_state(&session.unit);
                if state != "active" && state != "activating" {
                    issues.push(Issue {
                        account: record.name.clone(),
                        problem: format!("session is {state}"),
                        hint: format!(
                            "tilo-vm logs {}   then   tilo-vm account restart {}",
                            record.name, record.name
                        ),
                    });
                } else if let Some(detail) = self.parity(&session) {
                    let error = DaemonError::SessionParity {
                        account: record.name.clone(),
                        detail,
                    };
                    issues.push(Issue {
                        account: record.name.clone(),
                        problem: error.to_string(),
                        hint: error.hint(),
                    });
                }
            }
        }
        Ok(issues)
    }

    fn info(&mut self, record: &AccountEntry) -> Result<AccountInfo, DaemonError> {
        let generation = self.accounts.generation(&record.name, record.current)?;
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
        Ok(AccountInfo {
            session_identity: unit.as_ref().map(Session::identity),
            name: record.name.clone(),
            kind: record.kind,
            owner: record.owner.clone(),
            user: record.user(),
            generation: record.current,
            generations: std::iter::once(0)
                .chain(self.accounts.generations(&record.name))
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

    pub fn exec(
        &mut self,
        name: &str,
        args: &[String],
        timeout: Duration,
        max_bytes: u64,
    ) -> Result<crate::exec::Ended, DaemonError> {
        let (record, session) = self.user_account(name)?;
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

    fn show(&mut self, name: &str) -> Result<AccountShowResult, DaemonError> {
        let record = self.load_account(name)?;
        let generation = self.accounts.generation(name, record.current)?;
        let mut files = Vec::new();
        let mut provenance = BTreeMap::new();
        for (component, entry) in &generation.components {
            if let Some(imported) = self.layers.provenance(&entry.layer) {
                provenance.insert(component.clone(), imported);
            }
            for file in self.layers.files(&entry.layer)? {
                files.push(AccountFile {
                    component: component.clone(),
                    layer: entry.layer.clone(),
                    path: file.path,
                    sha256: file.sha256,
                    link: file.link,
                    executable: file.executable,
                });
            }
        }
        Ok(AccountShowResult {
            account: self.info(&record)?,
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
        kind: AccountKind,
        owner: Owner,
        outputs: &[OutputSpec],
        identity: IdentityRequest,
    ) -> Result<AccountCreateResult, DaemonError> {
        if identity == IdentityRequest::Device {
            self.admit_device_identity(name)?;
        }
        let (mut record, created) = self.accounts.create(name, kind, owner, outputs)?;
        if identity == IdentityRequest::Device && !record.device_identity {
            if !created {
                return Err(DaemonError::DeviceIdentity {
                    account: name.to_owned(),
                    detail: "the account already exists with an identity root of its own; remove it and create it again".into(),
                });
            }
            record.device_identity = true;
            self.accounts.save(&record)?;
        }
        let session = record
            .session()
            .ok_or_else(|| DaemonError::NoSession(name.to_owned()))?;
        let running = self.apply.unit_state(&session.unit) == "active";
        let mut actions = if created || !running {
            let generation = self.accounts.generation(name, record.current)?;
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
            self.accounts.save(&record)?;
            self.verify_parity([session])?;
            actions
        } else {
            Vec::new()
        };
        let identity = match identity {
            IdentityRequest::Throwaway => {
                let (identity, steps) = self.provision_test_identity(&record)?;
                actions.extend(steps);
                Some(identity)
            }
            IdentityRequest::Device => {
                let (identity, steps) = self.complete_onboarding(&record)?;
                actions.extend(steps);
                Some(identity)
            }
            IdentityRequest::None => None,
        };
        // Every create applies the layout again, so a create that failed here is repaired by
        // running it once more.
        if !record.outputs.is_empty() && record.seat0.is_none() {
            let user = record.user().unwrap_or_default();
            self.apply
                .apply_display(name, &user, &record.outputs)
                .map_err(|error| DaemonError::DisplayLayout {
                    account: name.to_owned(),
                    detail: match error {
                        DaemonError::Apply { detail, .. } => detail,
                        other => other.to_string(),
                    },
                })?;
            actions.push(Action::SetOutputs {
                outputs: tilo_vm_protocol::display::format_outputs(&record.outputs),
            });
        }
        Ok(AccountCreateResult {
            account: self.info(&record)?,
            created,
            actions,
            identity,
        })
    }

    /// Only one account can be this VM's person, and only while the machine's identity tree holds
    /// nobody else's identity: never replace or adopt one.
    fn admit_device_identity(&mut self, name: &str) -> Result<(), DaemonError> {
        let refuse = |detail: String| DaemonError::DeviceIdentity {
            account: name.to_owned(),
            detail,
        };
        let records = self.accounts.list()?;
        if let Some(other) = records
            .iter()
            .find(|record| record.device_identity && record.name != name)
        {
            return Err(refuse(format!(
                "account '{}' is this VM's person already",
                other.name
            )));
        }
        let ours = records
            .iter()
            .any(|record| record.device_identity && record.name == name);
        if !ours && self.apply.machine_identity().person {
            return Err(refuse(
                "this VM already holds an identity (/var/lib/tilo/user-identity), which an account never replaces or adopts".into(),
            ));
        }
        Ok(())
    }

    /// Make the account's user this VM's person the way a device's first login does: the session shows
    /// onboarding (it has no identity and no private root), and onboarding's own controls create
    /// the identity, which certifies the machine's device key; that certificate starts `fs`. The
    /// controls are pressed by name through the control socket, as a person's clicks reach them.
    fn complete_onboarding(
        &mut self,
        record: &AccountEntry,
    ) -> Result<(TestIdentity, Vec<Action>), DaemonError> {
        let name = record.name.as_str();
        let fail = |detail: String| DaemonError::DeviceIdentity {
            account: name.to_owned(),
            detail,
        };
        let session = record
            .session()
            .ok_or_else(|| fail("the account has no user".into()))?;
        let user = session.user.clone();
        self.apply.wait_session_ready(&session).map_err(fail)?;
        let created = !self.apply.machine_identity().certified;
        let mut actions = Vec::new();
        if created {
            let now = Instant::now();
            let mut screen = OnboardingScreen {
                daemon: self,
                session: &session,
                deadline: now.checked_add(ONBOARDING_TIMEOUT).unwrap_or(now),
            };
            let pace = onboarding_walk::Pace {
                timeout: ONBOARDING_TIMEOUT,
                ..onboarding_walk::Pace::default()
            };
            onboarding_walk::walk(&mut screen, pace).map_err(|error| match error {
                onboarding_walk::WalkError::Screen(error) => error,
                unfinished @ onboarding_walk::WalkError::Unfinished { .. } => {
                    fail(unfinished.to_string())
                }
            })?;
            actions.push(Action::CompleteOnboarding { created: true });
            // As after a throwaway: a layout made before the identity belongs to the Unix user and
            // hides every app launch.
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
        } else {
            actions.push(Action::CompleteOnboarding { created: false });
        }
        // `fs-identity.path` starts `fs` once the certificate exists.
        let started = Instant::now();
        loop {
            let state = self.apply.unit_state(FS_UNIT);
            if state == "active" {
                break;
            }
            if started.elapsed() >= FS_START_TIMEOUT {
                return Err(fail(format!(
                    "the machine's device key is certified, but {FS_UNIT} is {state} after {}s",
                    FS_START_TIMEOUT.as_secs()
                )));
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        Ok((
            TestIdentity {
                created,
                display_name: String::new(),
                throwaway: false,
            },
            actions,
        ))
    }

    /// The onboarding controls the session shows, by name; empty once onboarding is gone. Waits,
    /// until `deadline`, for a control socket a starting session has not bound yet.
    fn onboarding_targets(
        &mut self,
        session: &apply::Session,
        deadline: Instant,
    ) -> Result<Vec<String>, DaemonError> {
        let args = ["raw".to_owned(), r#"{"op":"list_targets"}"#.to_owned()];
        let reply = loop {
            let reply = self.apply.control(session, &args)?;
            if reply.exit_code != 2 || Instant::now() >= deadline {
                break reply;
            }
            std::thread::sleep(Duration::from_millis(250));
        };
        if reply.exit_code != 0 {
            return Err(DaemonError::DeviceIdentity {
                account: session.account.clone(),
                detail: format!("listing the session's controls: {}", reply.stderr.trim()),
            });
        }
        Ok(onboarding_target_names(reply.stdout.trim()))
    }

    /// Have the account's running session give its user a throwaway test identity. A newly minted one
    /// comes with fresh desktop state: a workspace made before the identity is owned by the Unix
    /// user and hides every app launch.
    fn provision_test_identity(
        &mut self,
        record: &AccountEntry,
    ) -> Result<(TestIdentity, Vec<Action>), DaemonError> {
        let name = record.name.as_str();
        let fail = |detail: String| DaemonError::TestIdentity {
            account: name.to_owned(),
            detail,
        };
        let session = record
            .session()
            .ok_or_else(|| fail("the account has no user".into()))?;
        let user = session.user.clone();
        self.apply.wait_session_ready(&session).map_err(fail)?;
        let args = [
            "raw".to_owned(),
            r#"{"op":"provision_test_identity"}"#.to_owned(),
        ];
        let asked = Instant::now();
        let reply = loop {
            let reply = self.apply.control(&session, &args)?;
            // `tilo-control` exits 2 when it cannot reach the socket, which a session that has only
            // just started has not bound yet.
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
        let mut record = self.load_account(name)?;
        let generation = self.accounts.generation(name, record.current)?;
        let layers = self.system_layers(&record, self.layer_paths(&generation)?)?;
        let policy = if record.kind == AccountKind::Vm {
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
        self.accounts.save(&record)?;
        self.accounts.clear_pending(name)?;
        if record.kind == AccountKind::Vm {
            actions.extend(self.restart_all_sessions(&restarts.user_units, &layers)?);
            let sessions = self.sessions()?;
            self.verify_parity(sessions)?;
        } else {
            self.verify_parity(record.session())?;
        }
        Ok(ApplyResult {
            account: record.name,
            generation: record.current,
            changed: true,
            policy,
            actions,
            cohort_selection: Vec::new(),
            warnings: Vec::new(),
            boot: None,
        })
    }

    fn remove(&mut self, name: &str) -> Result<AccountRmResult, DaemonError> {
        if name == VM_ACCOUNT {
            return Err(DaemonError::ReservedName(name.to_owned()));
        }
        if name == DEVICE_ACCOUNT {
            return Err(DaemonError::DeviceAccount("removed".into()));
        }
        let record = match self.accounts.load(name) {
            Ok(record) => record,
            Err(DaemonError::AccountNotFound(_)) => {
                return Ok(AccountRmResult {
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
        let removed = self.accounts.remove(name)?;
        Ok(AccountRmResult { removed, actions })
    }

    fn component_info(
        &self,
        record: &AccountEntry,
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
            let scope = if record.kind == AccountKind::Vm {
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
        partner: Option<&tilo_vm_protocol::Partner>,
    ) -> Result<ApplyResult, DaemonError> {
        let mut record = self.load_account(name)?;
        let layer_path = self.layers.require(layer)?;
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
        let mut result = match self
            .accounts
            .prepare_swap(&record, component, layer, info)?
        {
            Some((from, to)) => {
                let reselected = cohort
                    .as_ref()
                    .map(|cohort| cohort.ids().collect())
                    .unwrap_or_default();
                self.check_cohort_users(&record, &from, &to, &reselected)?;
                self.check_interfaces(&record, &to, partner)?;
                self.transition(&mut record, &from, &to)?
            }
            None => unchanged(record),
        };
        if !result.changed {
            self.layers.verify(layer)?;
        }
        if let Some(cohort) = &cohort {
            let (record, session) = self.user_account(name)?;
            if !result.changed {
                let current = self.accounts.generation(name, record.current)?;
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
        let mut record = self.load_account(name)?;
        let (from, to) = self.accounts.rollback_target(&record, target)?;
        if from.number == to.number {
            return Ok(unchanged(record));
        }
        self.check_cohort_users(&record, &from, &to, &BTreeSet::new())?;
        self.transition(&mut record, &from, &to)
    }

    /// Refuse generation `to` of account `record` when a component of the matched session set, its own
    /// layer's or the image's, was built for another system-interface declaration than the one `to`
    /// leaves the account reading, or when a VM-wide runtime service serves another runtime version.
    /// For the `vm` account, refuse when its runtime services would leave any account reading another
    /// version. `partner` names an account whose next generation moves with this swap, which is judged
    /// instead of the one it runs. Checked before anything is activated.
    ///
    /// # Errors
    ///
    /// [`DaemonError::InvalidParams`] naming what to swap together; a missing layer.
    pub(crate) fn check_interfaces(
        &self,
        record: &AccountEntry,
        to: &Generation,
        partner: Option<&tilo_vm_protocol::Partner>,
    ) -> Result<(), DaemonError> {
        let image = image::recorded_declaration(&self.root);
        let conflict = if record.name == VM_ACCOUNT {
            // Only a change of a runtime service is judged against the accounts: a vmd or greeter swap
            // never waits on fs.
            let current = (record.current != 0)
                .then(|| self.accounts.generation(VM_ACCOUNT, record.current))
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
            let mut accounts = Vec::new();
            for account in self.accounts.list()? {
                if account.name == VM_ACCOUNT || account.current == 0 {
                    continue;
                }
                let members = match partner.filter(|partner| partner.account == account.name) {
                    Some(partner) => self.interface_members_of(&partner.components)?,
                    None => self.interface_members(
                        &self.accounts.generation(&account.name, account.current)?,
                    )?,
                };
                accounts.push((account.name.clone(), members));
            }
            crate::interfaces::vm_conflict(image.as_ref(), &accounts, &vm_services)
        } else {
            let vm_services = match self.accounts.load(VM_ACCOUNT) {
                Ok(vm) if vm.current != 0 => {
                    self.interface_members(&self.accounts.generation(VM_ACCOUNT, vm.current)?)?
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

    fn interface_members(
        &self,
        generation: &Generation,
    ) -> Result<Vec<crate::interfaces::Member>, DaemonError> {
        let components: BTreeMap<String, String> = generation
            .components
            .iter()
            .map(|(component, entry)| (component.clone(), entry.layer.clone()))
            .collect();
        self.interface_members_of(&components)
    }

    fn interface_members_of(
        &self,
        components: &BTreeMap<String, String>,
    ) -> Result<Vec<crate::interfaces::Member>, DaemonError> {
        let relative = tilo_vm_protocol::SYSTEM_INTERFACES
            .strip_prefix('/')
            .unwrap_or(tilo_vm_protocol::SYSTEM_INTERFACES);
        let mut members = Vec::new();
        for (component, layer) in components {
            let declaration = self.layers.require(layer)?.join(relative);
            members.push(crate::interfaces::Member {
                component: component.clone(),
                layer: layer.clone(),
                provenance: self.layers.provenance(layer),
                ships: declaration
                    .exists()
                    .then(|| tilo_vm_protocol::Declaration::read(&declaration)),
            });
        }
        Ok(members)
    }

    /// Refuse a transition that takes away a native toolkit the account user's own packages run on:
    /// one the apps+runtime layer of `from` ships and neither the layers `to` merges nor the image
    /// ships. Packages in `reselected` are installed again for the new toolkit; a package that
    /// names no runtime never blocks, and runtime-kind packages are among them since a runtime may
    /// not declare one.
    fn check_cohort_users(
        &mut self,
        record: &AccountEntry,
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
        record: &mut AccountEntry,
        from: &Generation,
        to: &Generation,
    ) -> Result<ApplyResult, DaemonError> {
        // Boot changes have their own boot-confirmation lifecycle; runtime transitions recover
        // once, without recursively retrying a broken build.
        let (policy, _) = apply::policy(record, from, to)?;
        match self.activate_generation(record, from, to) {
            Ok(result) => Ok(result),
            Err(error) if policy != Policy::Reboot => {
                let recovery = match self.activate_generation(record, to, from) {
                    Ok(_) => format!("restored generation {}", from.number),
                    Err(recovery) => format!(
                        "recovery to generation {} also failed: {recovery}; inspect logs --account {} and retry rollback --account {} --to {}",
                        from.number, record.name, record.name, from.number
                    ),
                };
                Err(DaemonError::Apply {
                    step: format!(
                        "activate generation {} in account '{}'",
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
        record: &mut AccountEntry,
        from: &Generation,
        to: &Generation,
    ) -> Result<ApplyResult, DaemonError> {
        let (policy, restarts) = apply::policy(record, from, to)?;
        let layers = self.system_layers(record, self.layer_paths(to)?)?;
        self.accounts.mark_pending(&record.name, to.number)?;
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
                self.accounts.set_current(record, to.number)?;
            }
            Policy::Reboot => {
                // The kernel's modules are merged now; its boot entry is next, and sessions restart
                // with the reboot the client runs.
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
                self.accounts.set_current(record, to.number)?;
            }
            Policy::SessionRestart => {
                self.verify_parity(record.session())?;
                self.accounts.set_current(record, to.number)?;
            }
            Policy::NoRestart => self.accounts.set_current(record, to.number)?,
        }
        Ok(ApplyResult {
            account: record.name.clone(),
            generation: to.number,
            changed: true,
            policy,
            actions,
            cohort_selection: Vec::new(),
            warnings,
            boot,
        })
    }

    /// Account sessions snapshot `/usr` when they start, so a refreshed VM `/usr` reaches them only
    /// through a restart.
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
                    .stored_accounts()?
                    .into_iter()
                    .find(|record| record.session().as_ref() == Some(&session))
                    .ok_or_else(|| {
                        DaemonError::InvalidParams("stand-in session has no account".into())
                    })?;
                prepare_account_session(
                    &self.accounts,
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

    fn journal_matches(&mut self, record: &AccountEntry) -> Result<Vec<String>, DaemonError> {
        let mut matches = Vec::new();
        if let Some(session) = record.session() {
            // PAM moves the session into `session-N.scope`, so its output is found by the unit's
            // `SyslogIdentifier`, not by the unit name. The device's session runs its compositor in
            // units it starts, so everything its account runs is the session's.
            let uid = apply::session_uid(self.apply.as_mut(), &session)?;
            if session.stand_in {
                matches.push(format!("SYSLOG_IDENTIFIER=tilo-session-{}", record.name));
            } else {
                matches.push(format!("_SYSTEMD_UNIT={}", session.unit));
            }
            if session.login.is_some() && session.unit != GREETER_UNIT {
                matches.push(format!("_SYSTEMD_UNIT={GREETER_UNIT}"));
            }
            matches.push(format!("UNIT={}", session.unit));
            matches.push(format!("_UID={uid}"));
        } else {
            let generation = self.accounts.generation(&record.name, record.current)?;
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

    /// Re-activate an account's current generation with a session restart, after its seat assignment
    /// changed.
    fn reseat(&mut self, record: &mut AccountEntry) -> Result<Vec<Action>, DaemonError> {
        let generation = self.accounts.generation(&record.name, record.current)?;
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
        self.accounts.save(record)?;
        self.verify_parity(record.session())?;
        Ok(actions)
    }

    fn claim(&mut self, name: &str, owner: Owner, steal: bool) -> Result<SeatResult, DaemonError> {
        let (mut record, _) = self.user_account(name)?;
        if record.kind == AccountKind::Device {
            return Err(DaemonError::DeviceAccount("moved between seats".into()));
        }
        if record.seat0.is_some() {
            return Ok(SeatResult {
                account: record.name,
                seat: "seat0".into(),
                changed: false,
                actions: Vec::new(),
            });
        }
        let mut actions = Vec::new();
        if let Some(mut holder) = self
            .accounts
            .list()?
            .into_iter()
            .find(|r| r.seat0.is_some())
        {
            if !steal {
                let lease = holder.seat0.unwrap_or_else(|| Lease {
                    account: holder.name.clone(),
                    owner: Owner::default(),
                    since: String::new(),
                });
                let owner = [lease.owner.host, lease.owner.worktree, lease.owner.branch]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" ");
                return Err(DaemonError::SeatBusy {
                    account: lease.account,
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
            account: record.name.clone(),
            owner,
            since: accounts::now(),
        });
        actions.extend(self.reseat(&mut record)?);
        Ok(SeatResult {
            account: record.name,
            seat: "seat0".into(),
            changed: true,
            actions,
        })
    }

    fn release(&mut self, name: &str) -> Result<SeatResult, DaemonError> {
        let (mut record, _) = self.user_account(name)?;
        if record.kind == AccountKind::Device {
            return Err(DaemonError::DeviceAccount("moved between seats".into()));
        }
        let own_seat = |r: &AccountEntry| r.seat.clone().unwrap_or_else(|| "none".into());
        if record.seat0.is_none() {
            return Ok(SeatResult {
                seat: own_seat(&record),
                account: record.name,
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
            account: record.name,
            changed: true,
            actions,
        })
    }

    fn gc(&mut self, request: &tilo_vm_protocol::Gc) -> Result<GcResult, DaemonError> {
        let (keep, dry_run) = (request.keep, request.dry_run);
        let mut referenced = BTreeSet::new();
        let mut removed_generations = Vec::new();
        let mut records = self.accounts.list()?;
        if !records.iter().any(|r| r.name == VM_ACCOUNT) {
            records.push(self.accounts.load(VM_ACCOUNT)?);
        }
        // The device account's generations that carry a held layer are the person's system waiting on
        // a deliberate choice: gc keeps them.
        let held = image::held_hashes(&self.root);
        for record in &records {
            let also_keep: BTreeSet<u64> = if record.name == DEVICE_ACCOUNT {
                self.accounts
                    .generations(&record.name)
                    .into_iter()
                    .filter(|number| {
                        self.accounts
                            .generation(&record.name, *number)
                            .is_ok_and(|g| g.layers.iter().any(|layer| held.contains(layer)))
                    })
                    .collect()
            } else {
                BTreeSet::new()
            };
            let (layers, removed) = self.accounts.prune(record, keep, &also_keep, dry_run)?;
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
