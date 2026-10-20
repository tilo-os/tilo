//! Full Tilo installed alongside the current system, without a USB drive.
//!
//! Contract: `docs/architecture/usb-free-install.md`
//! §5 (capability gate) and §8 (Imager UI). This module holds the reviewed
//! plan, host readiness and elevated helper runs. Nothing here writes to a
//! disk or firmware: only the elevated staging helper does, after it re-probes
//! the host and refuses a plan whose basis changed (`plan-stale`).
//!
//! Every host-facing function is injected through [`StagingRuntime`] so tests
//! drive the whole flow with fakes and never reach the platform stubs.

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use super::internal_cache::HeldReadSeek;
use super::internal_cache::coordinator::Cancellation;
use super::internal_download::SelectedManifest;
use super::model::HostOs;
use super::staging::helper::{self, Action, ComponentPaths, Outcome, Poll, Progress, Request};
use super::staging::plan::{self, Basis, Plan, PlanError, Requirements, TargetBounds};
use super::staging::probe::{self, Blocker, HostFacts};
use super::staging::record::{Record, ReleaseRef, Step};
use super::staging::{ALIGN_BYTES, DEFAULT_TARGET_BYTES};
use tilo_lock::MutexRecover as _;

/// Release jobs set this at build time only after the round trip passed.
pub(super) const BUILD_GATE: Option<&str> = option_env!("TILO_IMAGER_INTERNAL_INSTALL");
/// Development builds may opt in at run time.
pub(super) const RUNTIME_GATE_VAR: &str = "TILO_IMAGER_ALLOW_INTERNAL_INSTALL";

const GIB: u64 = 1 << 30;

/// Device-local inputs already reviewed in the installer journey. This value
/// retains encrypted custody; it is never shared UI state or helper authority.
#[derive(Clone)]
pub(crate) struct ReviewedFullOs {
    pub facts: HostFacts,
    pub plan: Plan,
    pub profile: crate::model::InstallProfile,
    pub migration: Option<super::migration_intent::MigrationIntent>,
    /// Present when a protected handoff was prepared. Interactive setups
    /// defer identity and credential capture to the destination's first boot,
    /// so this is absent rather than carrying plaintext or another machine's
    /// sealed secret.
    pub prepared: Option<Arc<super::journey::prepared_media::PreparedMedia>>,
    /// The reviewed selection deliberately carries metadata only; the
    /// destination's first login finishes the copy or application restore.
    pub deferred: bool,
}

fn remember_tpm_handoff(
    profile: &mut serde_json::Map<String, serde_json::Value>,
    prepared: &super::journey::prepared_media::PreparedMedia,
) -> Result<(), String> {
    let Some(handoff) = prepared.tpm_handoff() else {
        return Ok(());
    };
    let bytes = handoff.encode().map_err(|error| error.to_string())?;
    profile.insert(
        "tpm_handoff".into(),
        serde_json::Value::String(
            String::from_utf8(bytes)
                .map_err(|_| super::strings::t("imager-setup-internal-error-prepared-setup"))?,
        ),
    );
    Ok(())
}

/// Staging sizes assumed before the verified manifest is available (its
/// `minimum_partition_bytes`). The staging consumer refuses with `plan-stale`
/// when the manifest needs more than the reviewed plan holds; review then
/// recomputes with the manifest's sizes.
pub(super) use super::staging::{PROVISIONAL_STAGING_ESP_BYTES, PROVISIONAL_STAGING_ROOT_BYTES};

/// The product gate of spec §9: build gate or explicit development opt-in.
#[must_use]
pub(super) fn gate_open(build: Option<&str>, runtime: Option<&str>) -> bool {
    build == Some("1") || runtime == Some("1")
}

/// The gate as this process sees it.
#[must_use]
pub(super) fn live_gate_open() -> bool {
    gate_open(BUILD_GATE, std::env::var(RUNTIME_GATE_VAR).ok().as_deref())
}

/// A running elevated helper, as the UI observes it.
pub(super) trait HelperRun: Send {
    fn poll(&mut self) -> Poll;
}

impl HelperRun for helper::Running {
    fn poll(&mut self) -> Poll {
        helper::Running::poll(self)
    }
}

/// Host commands a person can ask for from Review. Each needs a restart or an
/// administrator prompt, so none of them runs on the UI thread.
pub(super) use imager_backend::setup::native_host::HostCommand;

type ProbeFn = dyn Fn() -> HostFacts + Send + Sync;
type PlanFn = dyn Fn(&Basis, Requirements) -> Result<Plan, PlanError> + Send + Sync;
type BoundsFn = dyn Fn(&Basis, Requirements) -> Result<TargetBounds, PlanError> + Send + Sync;
type LaunchFn = dyn Fn(&Request) -> Result<Box<dyn HelperRun>, String> + Send + Sync;
type RecordsFn = dyn Fn() -> Vec<Record> + Send + Sync;
type PathsFn = dyn Fn(&SelectedManifest) -> Result<ComponentPaths, String> + Send + Sync;
type HostCommandFn = dyn Fn(HostCommand) -> Result<(), String> + Send + Sync;

/// Every host-facing call of the install flow.
#[derive(Clone)]
pub(super) struct StagingRuntime {
    pub(super) probe: Arc<ProbeFn>,
    pub(super) compute: Arc<PlanFn>,
    pub(super) target_bounds: Arc<BoundsFn>,
    pub(super) launch: Arc<LaunchFn>,
    pub(super) pending_records: Arc<RecordsFn>,
    pub(super) component_paths: Arc<PathsFn>,
    pub(super) host_command: Arc<HostCommandFn>,
    pub(super) poll_interval: Duration,
}

impl StagingRuntime {
    /// The staging core and platform commands of this build.
    pub(super) fn live() -> Self {
        Self {
            probe: Arc::new(probe::probe),
            compute: Arc::new(plan::compute),
            target_bounds: Arc::new(plan::target_bounds),
            launch: Arc::new(|request| {
                helper::launch(request).map(|running| Box::new(running) as Box<dyn HelperRun>)
            }),
            pending_records: Arc::new(helper::pending_records),
            component_paths: Arc::new(live_component_paths),
            host_command: Arc::new(live_host_command),
            poll_interval: Duration::from_millis(200),
        }
    }
}

#[cfg(not(test))]
fn live_component_paths(selected: &SelectedManifest) -> Result<ComponentPaths, String> {
    let mut esp = None;
    let mut root = None;
    for index in 0..2 {
        let component = selected
            .bundle()
            .component(index)
            .ok_or("The verified download is missing a component.")?;
        let path = super::internal_cache::final_component_path(selected, index)?
            .to_str()
            .ok_or("The verified download path is not valid Unicode.")?
            .to_string();
        match component.role {
            "staging-esp" => esp = Some(path),
            "staging-root" => root = Some(path),
            _ => return Err("The verified download has an unknown component.".into()),
        }
    }
    Ok(ComponentPaths {
        staging_esp: esp.ok_or("The verified download has no installer boot image.")?,
        staging_root: root.ok_or("The verified download has no installer system image.")?,
    })
}

#[cfg(test)]
fn live_component_paths(_: &SelectedManifest) -> Result<ComponentPaths, String> {
    Err("Verified download paths are unavailable in this build.".into())
}

/// The UI-owned description of the next `stage` the consumer runs.
#[derive(Clone)]
struct Intent {
    migration: Option<super::migration_intent::MigrationIntent>,
    profile: serde_json::Map<String, serde_json::Value>,
    prepared_setup: Option<super::staging::prepared_setup::PreparedArtifact>,
    prepared_custody: Option<Arc<super::journey::prepared_media::PreparedMedia>>,
    /// Metadata-only consent: no prepared ciphertext exists because the
    /// destination has not established its login password yet.
    deferred: bool,
    operation_id: String,
    plan: Plan,
    plan_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Failure {
    /// Stable helper refusal ID, or an Imager-side one (`launch-failed`,
    /// `interrupted`, `download`).
    pub(super) code: Option<String>,
    pub(super) detail: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) enum RunState {
    #[default]
    Idle,
    Running {
        action: Action,
        progress: Option<Progress>,
    },
    Succeeded {
        action: Action,
    },
    Failed {
        action: Action,
        failure: Failure,
    },
}

#[derive(Default)]
struct RunShared {
    intent: Option<Intent>,
    state: RunState,
    revision: u64,
    /// `(staging ESP, staging root)` minimum sizes learned from the verified
    /// manifest, used for every later plan.
    requirements: Option<(u64, u64)>,
}

/// What the staging consumer and the UI share.
#[derive(Clone)]
pub(super) struct StagingLink {
    runtime: StagingRuntime,
    shared: Arc<Mutex<RunShared>>,
}

impl StagingLink {
    fn lock(&self) -> MutexGuard<'_, RunShared> {
        self.shared.lock_recover()
    }

    fn set_state(&self, state: RunState) {
        let mut shared = self.lock();
        if shared.state != state {
            shared.state = state;
            shared.revision = shared.revision.wrapping_add(1);
        }
    }

    fn finish(&self, action: Action, outcome: &Outcome) {
        self.set_state(if outcome.ok {
            RunState::Succeeded { action }
        } else {
            RunState::Failed {
                action,
                failure: Failure {
                    code: outcome.code.clone(),
                    detail: outcome.message.clone().unwrap_or_default(),
                },
            }
        });
    }

    fn progress(&self, action: Action, progress: Progress) {
        self.set_state(RunState::Running {
            action,
            progress: Some(progress),
        });
    }
}

fn refusal(code: &str, message: impl Into<String>) -> Outcome {
    Outcome {
        ok: false,
        code: Some(code.into()),
        message: Some(message.into()),
    }
}

/// Launch the helper and follow its progress until it reports an outcome.
/// When the owner stops watching, the helper keeps running on its own; its
/// host record, not this process, is the source of truth afterwards.
fn drive(link: &StagingLink, request: &Request, cancelled: &dyn Fn() -> bool) -> Outcome {
    let mut running = match (link.runtime.launch)(request) {
        Ok(running) => running,
        Err(error) => return refusal("launch-failed", error),
    };
    loop {
        let Poll { latest, outcome } = running.poll();
        if let Some(progress) = latest {
            link.progress(request.action, progress);
        }
        if let Some(outcome) = outcome {
            return outcome;
        }
        if cancelled() {
            return refusal(
                "interrupted",
                "Setup stopped watching the installer helper before it finished.",
            );
        }
        std::thread::sleep(link.runtime.poll_interval);
    }
}

/// `(staging-esp, staging-root)` minimum partition sizes from verified bytes.
fn manifest_requirements(selected: &SelectedManifest) -> Result<(u64, u64), String> {
    let manifest: serde_json::Value = serde_json::from_slice(selected.bytes())
        .map_err(|error| format!("The verified manifest could not be read: {error}"))?;
    let components = manifest["components"]
        .as_array()
        .ok_or("The verified manifest lists no components.")?;
    let minimum = |role: &str| {
        components
            .iter()
            .find(|component| component["role"] == role)
            .and_then(|component| component["minimum_partition_bytes"].as_u64())
            .ok_or_else(|| format!("The verified manifest has no {role} size."))
    };
    Ok((minimum("staging-esp")?, minimum("staging-root")?))
}

fn partition_bytes(plan: &Plan, first_lba: u64, last_lba: u64) -> u64 {
    last_lba
        .saturating_sub(first_lba)
        .saturating_add(1)
        .saturating_mul(plan.basis.disk.sector_bytes)
}

fn stage_request(
    link: &StagingLink,
    selected: &SelectedManifest,
    mut intent: Intent,
    manifest_dir: &Path,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<Request, Outcome> {
    selected
        .bundle()
        .require_service_session_protocol(
            intent
                .prepared_custody
                .as_ref()
                .and_then(|media| media.service_session_protocol()),
        )
        .map_err(|_| {
            refusal(
                "account-consumer-unavailable",
                super::strings::t("imager-setup-account-consumer-unavailable"),
            )
        })?;
    selected
        .bundle()
        .require_account_protocol(
            intent
                .prepared_custody
                .as_ref()
                .and_then(|media| media.account_custody_protocol()),
        )
        .map_err(|_| {
            refusal(
                "account-consumer-unavailable",
                super::strings::t("imager-setup-account-consumer-unavailable"),
            )
        })?;
    selected
        .bundle()
        .require_home_protocol(
            intent
                .prepared_custody
                .as_ref()
                .and_then(|media| media.home_archive_protocol()),
        )
        .map_err(|_| {
            refusal(
                "home-consumer-unavailable",
                super::strings::t("imager-setup-home-consumer-unavailable"),
            )
        })?;
    selected
        .bundle()
        .require_applications_protocol(
            intent
                .prepared_custody
                .as_ref()
                .and_then(|media| media.applications_protocol()),
        )
        .map_err(|_| {
            refusal(
                "applications-consumer-unavailable",
                super::strings::t("imager-setup-applications-consumer-unavailable"),
            )
        })?;
    let (esp_min, root_min) =
        manifest_requirements(selected).map_err(|error| refusal("manifest", error))?;
    link.lock().requirements = Some((esp_min, root_min));
    let reviewed = intent.plan;
    let aligned = |bytes: u64| bytes.div_ceil(ALIGN_BYTES) * ALIGN_BYTES;
    let esp_bytes = partition_bytes(
        &reviewed,
        reviewed.staging_esp.first_lba,
        reviewed.staging_esp.last_lba,
    );
    let root_bytes = partition_bytes(
        &reviewed,
        reviewed.staging_root.first_lba,
        reviewed.staging_root.last_lba,
    );
    if esp_bytes < aligned(esp_min) || root_bytes < aligned(root_min) {
        return Err(refusal(
            "plan-stale",
            "The verified download needs more installer space than the reviewed plan holds.",
        ));
    }
    // Review ran before the manifest was known, on provisional sizes. The
    // helper accepts only the planner's exact result for the bundle's sizes,
    // so replan now; the replanned layout is used only when it keeps the
    // same Tilo size and shrinks the system volume no further than reviewed.
    let (plan, plan_sha256) = if esp_bytes == aligned(esp_min) && root_bytes == aligned(root_min) {
        (reviewed, intent.plan_sha256)
    } else {
        let sector = reviewed.basis.disk.sector_bytes;
        let stale = || {
            refusal(
                "plan-stale",
                "The verified download changes the reviewed plan.",
            )
        };
        let replanned = (link.runtime.compute)(
            &reviewed.basis,
            Requirements {
                staging_esp_bytes: esp_min,
                staging_root_bytes: root_min,
                target_bytes: reviewed.target_extent.lbas() * sector,
            },
        )
        .map(|p| plan::with_loader_path(p, &reviewed.boot_entry.loader_path.replace('\\', "/")))
        .map_err(|_| stale())?;
        let kept = |p: &Plan| p.shrink.as_ref().map_or(u64::MAX, |s| s.to_bytes);
        if kept(&replanned) < kept(&reviewed)
            || replanned.target_extent.lbas() != reviewed.target_extent.lbas()
        {
            return Err(stale());
        }
        let digest = replanned
            .digest()
            .map_err(|error| refusal("plan-digest", error.to_string()))?;
        (replanned, digest)
    };
    let migration = intent
        .migration
        .as_ref()
        .map(|grant| grant.bind(&intent.operation_id, &plan))
        .transpose()
        .map_err(|error| refusal("migration-changed", error.to_string()))?;
    if let (Some(raw), Some(grant)) = (&migration, &intent.migration) {
        let source = super::migration_handoff::Plan::parse(
            &serde_json::to_vec(raw)
                .map_err(|error| refusal("migration-changed", error.to_string()))?,
        )
        .map_err(|error| refusal("migration-changed", error.to_string()))?;
        if !intent.deferred
            && grant
                .has_computer_applications()
                .map_err(|error| refusal("migration-changed", error.to_string()))?
        {
            let reservation = plan
                .target_extent
                .lbas()
                .checked_mul(plan.basis.disk.sector_bytes)
                .ok_or_else(|| {
                    refusal(
                        "migration-changed",
                        "Selected application target capacity overflows.",
                    )
                })?;
            let prepared = intent
                .prepared_custody
                .as_ref()
                .ok_or_else(|| {
                    refusal(
                        "migration-changed",
                        "Computer application preparation custody is missing.",
                    )
                })?
                .prepare_computer_applications(
                    grant,
                    &intent.operation_id,
                    &plan,
                    reservation,
                    cancel,
                )
                .map_err(|error| refusal("migration-changed", error.to_string()))?;
            intent.prepared_setup = Some(
                prepared
                    .staging_artifact()
                    .map_err(|error| refusal("migration-changed", error.to_string()))?,
            );
            intent.profile.insert(
                "prepared_setup".into(),
                serde_json::to_value(prepared.expectation())
                    .map_err(|error| refusal("migration-changed", error.to_string()))?,
            );
            remember_tpm_handoff(&mut intent.profile, &prepared)
                .map_err(|error| refusal("migration-changed", error))?;
            intent.prepared_custody = Some(prepared);
        }
        if source.home_policy == super::migration_handoff::HomePolicy::Copy
            && (source.categories.home || source.categories.flatpak)
        {
            if plan.shrink.is_some() {
                return Err(refusal(
                    "migration-changed",
                    "Original-copy source records require explicit renewal after a source resize.",
                ));
            }
            let reservation = plan
                .target_extent
                .lbas()
                .checked_mul(plan.basis.disk.sector_bytes)
                .ok_or_else(|| {
                    refusal(
                        "migration-changed",
                        "Original-copy target capacity overflows.",
                    )
                })?;
            if !intent.deferred {
                // A deferred selection keeps the retained source untouched
                // until first login; the installed onboarding then performs
                // the copy with a renewed source grant and its own receipt.
                let prepared = intent
                    .prepared_custody
                    .as_ref()
                    .ok_or_else(|| {
                        refusal(
                            "migration-changed",
                            "Original-copy preparation custody is missing.",
                        )
                    })?
                    .prepare_staging_copy(grant, &source, reservation, cancel)
                    .map_err(|error| refusal("migration-changed", error.to_string()))?;
                intent.prepared_setup = Some(
                    prepared
                        .staging_artifact()
                        .map_err(|error| refusal("migration-changed", error.to_string()))?,
                );
                intent.profile.insert(
                    "prepared_setup".into(),
                    serde_json::to_value(prepared.expectation())
                        .map_err(|error| refusal("migration-changed", error.to_string()))?,
                );
                remember_tpm_handoff(&mut intent.profile, &prepared)
                    .map_err(|error| refusal("migration-changed", error))?;
                // prepare_staging_copy caches the final generation in the actual
                // previous Arc retained by the download consumer through drive().
                intent.prepared_custody = Some(prepared);
            }
        }
    }
    if let Some(prepared) = &intent.prepared_custody {
        prepared
            .validate_recovery_for_stage(&plan, cancel)
            .map_err(|error| refusal("recovery-unverified", error.to_string()))?;
    }
    let component_paths =
        (link.runtime.component_paths)(selected).map_err(|error| refusal("paths", error))?;
    let manifest_path = manifest_dir.join("manifest.json");
    std::fs::write(&manifest_path, selected.bytes()).map_err(|error| {
        refusal(
            "manifest",
            format!("The manifest could not be saved: {error}"),
        )
    })?;
    let bundle = selected.bundle();
    Ok(Request {
        schema: 1,
        operation_id: intent.operation_id,
        action: Action::Stage,
        release: ReleaseRef {
            release_id: bundle.release_id().into(),
            manifest_sha256: selected.manifest_sha256().into(),
            architecture: bundle.architecture().into(),
        },
        manifest_path: manifest_path
            .to_str()
            .ok_or_else(|| refusal("manifest", "The manifest path is not valid Unicode."))?
            .into(),
        component_paths,
        plan,
        plan_sha256,
        profile: intent.profile,
        prepared_setup: intent.prepared_setup,
        migration,
    })
}

/// The verified-cache consumer: runs on the download worker while both
/// verified readers are held, so the component files the helper re-hashes are
/// exactly the verified ones. It grants the helper nothing; the helper
/// re-derives every authority itself.
pub(super) fn staging_consumer(
    link: StagingLink,
) -> impl FnMut(
    &SelectedManifest,
    &Cancellation<'_>,
    &mut dyn HeldReadSeek,
    &mut dyn HeldReadSeek,
) -> Result<(), String>
+ Send
+ 'static {
    move |selected, cancel, _, _| {
        let Some(intent) = link.lock().intent.take() else {
            let outcome = refusal("no-plan", "No reviewed installation plan was waiting.");
            link.finish(Action::Stage, &outcome);
            return Err(outcome.message.unwrap_or_default());
        };
        let mut intent = intent;
        if let Some(prepared) = &intent.prepared_custody {
            if let Ok((fresh, artifact, expected)) = prepared
                .refreshed_for_transfer(cancel.signal())
                .and_then(|fresh| {
                    let artifact = fresh.staging_artifact()?;
                    let expected =
                        serde_json::to_value(fresh.expectation()).map_err(std::io::Error::other)?;
                    Ok((fresh, artifact, expected))
                })
            {
                intent.profile.insert("prepared_setup".into(), expected);
                if let Err(error) = remember_tpm_handoff(&mut intent.profile, &fresh) {
                    let outcome = refusal("prepared-source", error);
                    link.finish(Action::Stage, &outcome);
                    return Err(outcome.message.unwrap_or_default());
                }
                intent.prepared_setup = Some(artifact);
                intent.prepared_custody = Some(fresh);
            } else {
                let outcome = refusal(
                    "prepared-source",
                    "Selected browser contents could not be prepared",
                );
                link.finish(Action::Stage, &outcome);
                return Err(outcome.message.unwrap_or_default());
            }
        }
        let _prepared_custody = intent.prepared_custody.clone();
        let manifest_dir = tempfile::Builder::new()
            .prefix("tilo-staging-")
            .tempdir()
            .map_err(|error| error.to_string());
        let outcome = match manifest_dir {
            Err(error) => refusal("manifest", error),
            Ok(dir) => match stage_request(&link, selected, intent, dir.path(), cancel.signal()) {
                Err(outcome) => outcome,
                Ok(request) => drive(&link, &request, &|| cancel.is_cancelled()),
            },
        };
        link.finish(Action::Stage, &outcome);
        if outcome.ok {
            Ok(())
        } else {
            Err(outcome
                .message
                .or(outcome.code)
                .unwrap_or_else(|| "The installer helper refused.".into()))
        }
    }
}

/// A fresh RFC 4122 version 4 identifier. Not a secret: it names one
/// operation's record, and the helper refuses to reuse a finished one.
fn new_operation_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Host readiness for Full Tilo, as shown on the experience page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Readiness {
    /// Build gate closed: release builds until the round trip passes.
    Gated,
    /// The probe has not reported yet.
    Checking,
    /// No internal release for this architecture in the catalog.
    NoRelease,
    /// Something blocks that Imager cannot help clear.
    Unsupported(Vec<Blocker>),
    Ready,
}

/// A task for a background thread; the view's worker launcher runs it.
pub(super) type Task = Box<dyn FnOnce() + Send + 'static>;

/// UI-side state of the Full Tilo installation.
pub(super) struct InternalInstall {
    migration_intent: Option<super::migration_intent::MigrationIntent>,
    prepared_custody: Option<Arc<super::journey::prepared_media::PreparedMedia>>,
    deferred: bool,
    profile: serde_json::Map<String, serde_json::Value>,
    link: StagingLink,
    gate: bool,
    host: HostOs,
    readiness: Readiness,
    facts: Option<HostFacts>,
    pending: Vec<Record>,
    target_gib: ui::LocalState<f64>,
    target_bytes: u64,
    minimum_target_bytes: u64,
    bounds: Option<Result<TargetBounds, PlanError>>,
    plan: Option<Result<Plan, PlanError>>,
    seen_revision: u64,
    state: RunState,
    probing: bool,
    reprobe_due: bool,
    restart_deferred: bool,
    revision: u64,
}

impl InternalInstall {
    pub(super) fn new(runtime: StagingRuntime, gate: bool, host: HostOs) -> Self {
        Self {
            migration_intent: None,
            prepared_custody: None,
            deferred: false,
            profile: serde_json::Map::new(),
            link: StagingLink {
                runtime,
                shared: Arc::new(Mutex::new(RunShared::default())),
            },
            gate,
            host,
            readiness: if gate {
                Readiness::Checking
            } else {
                Readiness::Gated
            },
            facts: None,
            pending: Vec::new(),
            target_gib: ui::LocalState::new((DEFAULT_TARGET_BYTES / GIB) as f64),
            target_bytes: DEFAULT_TARGET_BYTES,
            minimum_target_bytes: super::staging::MIN_TARGET_BYTES,
            bounds: None,
            plan: None,
            seen_revision: 0,
            state: RunState::Idle,
            probing: false,
            reprobe_due: false,
            restart_deferred: false,
            revision: 0,
        }
    }

    /// The closed-gate state for deterministic fixtures.
    pub(super) fn gated(host: HostOs) -> Self {
        Self::new(StagingRuntime::live(), false, host)
    }

    pub(super) fn link(&self) -> StagingLink {
        self.link.clone()
    }

    pub(super) fn runtime(&self) -> &StagingRuntime {
        &self.link.runtime
    }

    pub(super) fn gate(&self) -> bool {
        self.gate
    }

    pub(super) fn host(&self) -> HostOs {
        self.host
    }

    pub(super) fn revision(&self) -> u64 {
        self.revision
    }

    fn bump(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    /// A probe task, or `None` when the gate is closed. It returns the facts
    /// and unfinished records for [`Self::accept_probe`].
    pub(super) fn probe_task(
        &mut self,
    ) -> Option<impl FnOnce() -> (HostFacts, Vec<Record>) + Send + 'static> {
        if !self.gate || self.probing {
            return None;
        }
        self.probing = true;
        let runtime = self.link.runtime.clone();
        Some(move || ((runtime.probe)(), (runtime.pending_records)()))
    }

    pub(super) fn probe_failed(&mut self) {
        self.probing = false;
        self.readiness = Readiness::Unsupported(vec![Blocker::DiskUnsupported]);
        self.bump();
    }

    pub(super) fn is_probing(&self) -> bool {
        self.probing
    }

    /// Apply a finished probe. `has_release` is whether the catalog offers an
    /// internal release for this architecture.
    pub(super) fn accept_probe(
        &mut self,
        facts: HostFacts,
        records: Vec<Record>,
        has_release: bool,
    ) {
        self.probing = false;
        self.readiness = if !self.gate {
            Readiness::Gated
        } else if !has_release {
            Readiness::NoRelease
        } else if facts.offers_full_os() {
            Readiness::Ready
        } else {
            Readiness::Unsupported(
                facts
                    .blockers
                    .iter()
                    .copied()
                    .filter(|blocker| !blocker.has_remedy())
                    .collect(),
            )
        };
        self.facts = Some(facts);
        self.pending = records;
        self.recompute();
        self.bump();
    }

    pub(super) fn readiness(&self) -> &Readiness {
        &self.readiness
    }

    /// Full Tilo may be chosen: the probe passed, the catalog has a release
    /// and the build gate is open.
    pub(super) fn offers_full_os(&self) -> bool {
        self.readiness == Readiness::Ready
    }

    pub(super) fn blockers(&self) -> &[Blocker] {
        self.facts.as_ref().map_or(&[], |facts| &facts.blockers)
    }

    pub(super) fn pending(&self) -> Option<&Record> {
        self.pending.first()
    }

    fn requirements(&self, target_bytes: u64) -> Requirements {
        let (esp, root) = self.link.lock().requirements.unwrap_or((
            PROVISIONAL_STAGING_ESP_BYTES,
            PROVISIONAL_STAGING_ROOT_BYTES,
        ));
        Requirements {
            staging_esp_bytes: esp,
            staging_root_bytes: root,
            target_bytes,
        }
    }

    /// Whole GiB the stepper may offer, when the disk has room at all.
    pub(super) fn bounds_gib(&self) -> Option<(u64, u64)> {
        let Some(Ok(bounds)) = &self.bounds else {
            return None;
        };
        let min = bounds.min_bytes.div_ceil(GIB);
        let max = bounds.max_bytes / GIB;
        (min <= max).then_some((min, max))
    }

    fn recompute(&mut self) {
        let Some(basis) = self.facts.as_ref().and_then(|facts| facts.basis.clone()) else {
            self.bounds = None;
            self.plan = None;
            return;
        };
        self.bounds = Some(
            (self.link.runtime.target_bounds)(&basis, self.requirements(0)).and_then(
                |mut bounds| {
                    bounds.min_bytes = bounds.min_bytes.max(self.minimum_target_bytes);
                    if bounds.min_bytes > bounds.max_bytes {
                        return Err(PlanError::InsufficientSpace {
                            needed_bytes: bounds.min_bytes,
                            available_bytes: bounds.max_bytes,
                        });
                    }
                    Ok(bounds)
                },
            ),
        );
        if let Some((min, max)) = self.bounds_gib() {
            let gib = (self.target_bytes / GIB).clamp(min, max);
            self.target_bytes = gib * GIB;
            self.target_gib.set(gib as f64);
        }
        self.plan = Some(if self.target_bytes < self.minimum_target_bytes {
            Err(PlanError::InsufficientSpace {
                needed_bytes: self.minimum_target_bytes,
                available_bytes: self.target_bytes,
            })
        } else {
            (self.link.runtime.compute)(&basis, self.requirements(self.target_bytes))
        });
    }

    /// The stepper's binding; committed changes are picked up by [`Self::poll`].
    pub(super) fn target_state(&self) -> &ui::LocalState<f64> {
        &self.target_gib
    }

    pub(super) fn target_bytes(&self) -> u64 {
        self.target_bytes
    }

    /// Set the Tilo size in whole GiB within the offered bounds.
    pub(super) fn set_target_gib(&mut self, gib: u64) -> Result<(), String> {
        let (min, max) = self
            .bounds_gib()
            .ok_or_else(|| super::strings::t("imager-setup-internal-error-no-plan"))?;
        if !(min..=max).contains(&gib) {
            return Err(super::strings::t_args(
                "imager-setup-internal-error-size-range",
                &[("min", min.into()), ("max", max.into())],
            ));
        }
        self.target_gib.set(gib as f64);
        if self.target_bytes != gib * GIB {
            self.target_bytes = gib * GIB;
            self.recompute();
            self.bump();
        }
        Ok(())
    }

    pub(super) fn plan(&self) -> Option<&Result<Plan, PlanError>> {
        self.plan.as_ref()
    }

    /// The digest an install must name: the current plan, or the plan of the
    /// unfinished operation Continue resumes.
    pub(super) fn reviewed_digest(&self) -> Result<Option<String>, String> {
        if let Some(record) = self.pending() {
            return Ok(Some(record.plan_sha256.clone()));
        }
        match &self.plan {
            Some(Ok(plan)) => plan.digest().map(Some).map_err(|error| error.to_string()),
            _ => Ok(None),
        }
    }

    pub(super) fn run_state(&self) -> &RunState {
        &self.state
    }

    pub(super) fn is_running(&self) -> bool {
        matches!(self.state, RunState::Running { .. })
    }

    /// Why Install is not available, or `Ok` when it is.
    pub(super) fn install_blocker(&self, download_ready: bool) -> Result<(), String> {
        let t = super::strings::t;
        if !self.offers_full_os() {
            return Err(t("imager-setup-internal-error-unavailable"));
        }
        if self.is_running() {
            return Err(t("imager-setup-internal-error-running"));
        }
        if matches!(
            self.state,
            RunState::Succeeded {
                action: Action::Stage | Action::Arm
            }
        ) {
            return Err(t("imager-setup-internal-error-staged"));
        }
        let continuing = self.pending().is_some();
        if self
            .blockers()
            .iter()
            .any(|blocker| !(continuing && *blocker == Blocker::PendingOperation))
        {
            return Err(t("imager-setup-internal-error-blocked"));
        }
        if !continuing && !matches!(self.plan, Some(Ok(_))) {
            return Err(t("imager-setup-internal-error-no-plan"));
        }
        if !download_ready {
            return Err(t("imager-setup-internal-error-download"));
        }
        Ok(())
    }

    /// Queue the reviewed `stage` for the staging consumer. The caller then
    /// asks the download worker to consume; on failure it calls
    /// [`Self::abort_stage`].
    pub(super) fn set_migration_intent(
        &mut self,
        intent: Option<super::migration_intent::MigrationIntent>,
    ) -> Result<(), String> {
        if self.is_running() || self.pending().is_some() {
            return Err(
                "Migration consent cannot change while installation is running or already staged."
                    .into(),
            );
        }
        self.minimum_target_bytes = intent.as_ref().map_or(
            super::staging::MIN_TARGET_BYTES,
            super::migration_intent::MigrationIntent::application_minimum,
        );
        self.migration_intent = intent;
        self.recompute();
        self.bump();
        Ok(())
    }

    pub(super) fn adopt_reviewed(
        &mut self,
        reviewed: ReviewedFullOs,
        has_release: bool,
    ) -> Result<(), String> {
        let refused = || super::strings::t("imager-setup-internal-error-plan-changed");
        if self.is_running() || self.pending().is_some() || self.is_probing() {
            return Err(refused());
        }
        let profile = super::staging::handoff::profile_from_install(&reviewed.profile)?;
        let authenticated_profile =
            super::prepared_setup::PublicInstallProfile::from_draft(&reviewed.profile)
                .map_err(|_| refused())?;
        if !reviewed.profile.user.password.is_empty()
            || !reviewed.profile.encryption_passphrase.is_empty()
            || reviewed.facts.basis.as_ref() != Some(&reviewed.plan.basis)
            || (!reviewed.deferred && reviewed.prepared.is_none())
            || match &reviewed.prepared {
                Some(prepared) => {
                    reviewed.profile.prepared_setup.as_ref() != Some(prepared.expectation())
                        || prepared.install_profile() != Some(&authenticated_profile)
                }
                None => reviewed.profile.prepared_setup.is_some(),
            }
        {
            return Err(refused());
        }
        let bytes = reviewed
            .plan
            .target_extent
            .lbas()
            .checked_mul(reviewed.plan.basis.disk.sector_bytes)
            .ok_or_else(refused)?;
        let minimum = reviewed.migration.as_ref().map_or(
            super::staging::MIN_TARGET_BYTES,
            super::migration_intent::MigrationIntent::application_minimum,
        );
        if bytes < minimum || !bytes.is_multiple_of(GIB) {
            return Err(refused());
        }
        let mut canonical =
            (self.link.runtime.compute)(&reviewed.plan.basis, self.requirements(bytes))
                .map_err(|_| refused())?;
        // Partition UUIDs are minted once while reviewing, not placement inputs.
        canonical
            .staging_esp
            .partition_guid
            .clone_from(&reviewed.plan.staging_esp.partition_guid);
        canonical
            .staging_root
            .partition_guid
            .clone_from(&reviewed.plan.staging_root.partition_guid);
        if canonical != reviewed.plan {
            return Err(refused());
        }
        if !self.gate || !has_release || !reviewed.facts.offers_full_os() {
            return Err(super::strings::t("imager-setup-internal-error-unavailable"));
        }
        if let Some(prepared) = &reviewed.prepared {
            prepared
                .staging_artifact()
                .map_err(|_| super::strings::t("imager-setup-internal-error-prepared-setup"))?;
        }
        self.target_bytes = bytes;
        self.minimum_target_bytes = minimum;
        self.migration_intent = reviewed.migration;
        self.profile = profile;
        self.prepared_custody = reviewed.prepared;
        self.deferred = reviewed.deferred;
        self.accept_probe(reviewed.facts, Vec::new(), has_release);
        self.plan = Some(Ok(reviewed.plan));
        Ok(())
    }

    pub(super) fn begin_stage(
        &mut self,
        reviewed: &str,
        download_ready: bool,
    ) -> Result<(), String> {
        self.install_blocker(download_ready)?;
        if self.reviewed_digest()?.as_deref() != Some(reviewed) {
            return Err(super::strings::t(
                "imager-setup-internal-error-plan-changed",
            ));
        }
        let intent = if let Some(record) = self.pending() {
            Intent {
                migration: None,
                profile: record.profile.clone(),
                prepared_setup: None,
                prepared_custody: None,
                deferred: record.profile.get("prepared_setup").is_none(),
                operation_id: record.operation_id.clone(),
                plan: record.plan.clone(),
                plan_sha256: record.plan_sha256.clone(),
            }
        } else {
            let Some(Ok(plan)) = &self.plan else {
                return Err(super::strings::t("imager-setup-internal-error-no-plan"));
            };
            Intent {
                migration: self.migration_intent.clone(),
                profile: self.profile.clone(),
                deferred: self.deferred,
                prepared_setup: {
                    {
                        self.prepared_custody
                            .as_ref()
                            .map(|prepared| prepared.staging_artifact())
                            .transpose()
                            .map_err(|_| {
                                super::strings::t("imager-setup-internal-error-prepared-setup")
                            })?
                    }
                },
                prepared_custody: self.prepared_custody.clone(),
                operation_id: new_operation_id(),
                plan: plan.clone(),
                plan_sha256: plan.digest().map_err(|error| error.to_string())?,
            }
        };
        self.link.lock().intent = Some(intent);
        self.link.set_state(RunState::Running {
            action: Action::Stage,
            progress: None,
        });
        self.restart_deferred = false;
        self.poll();
        Ok(())
    }

    /// The download worker refused or lost the consumption before or while
    /// the consumer ran.
    pub(super) fn abort_stage(&mut self, code: &str, detail: String) {
        self.link.lock().intent = None;
        if matches!(
            self.link.lock().state,
            RunState::Running {
                action: Action::Stage,
                ..
            }
        ) {
            self.link.set_state(RunState::Failed {
                action: Action::Stage,
                failure: Failure {
                    code: Some(code.into()),
                    detail,
                },
            });
        }
        self.poll();
    }

    /// Start `arm` (Try again) or `remove` for the unfinished operation named
    /// `operation`; the returned task runs the helper to its outcome.
    pub(super) fn helper_task(&mut self, action: Action, operation: &str) -> Result<Task, String> {
        let record = self
            .pending()
            .cloned()
            .ok_or_else(|| super::strings::t("imager-setup-internal-error-no-pending"))?;
        if record.operation_id != operation {
            return Err(super::strings::t(
                "imager-setup-internal-error-pending-changed",
            ));
        }
        if self.is_running() {
            return Err(super::strings::t("imager-setup-internal-error-running"));
        }
        // `arm` and `remove` act on the recorded staging, never on download
        // bytes, so no cache paths are named; the helper authorises them
        // from its own host record for `operation_id`.
        let request = Request {
            schema: 1,
            operation_id: record.operation_id,
            action,
            release: record.release,
            manifest_path: String::new(),
            component_paths: ComponentPaths {
                staging_esp: String::new(),
                staging_root: String::new(),
            },
            plan: record.plan,
            plan_sha256: record.plan_sha256,
            profile: serde_json::Map::new(),
            prepared_setup: None,
            migration: None,
        };
        self.link.set_state(RunState::Running {
            action,
            progress: None,
        });
        self.restart_deferred = false;
        self.poll();
        let link = self.link();
        Ok(Box::new(move || {
            let outcome = drive(&link, &request, &|| false);
            link.finish(action, &outcome);
        }))
    }

    /// A helper task could not be started at all.
    pub(super) fn helper_task_failed(&mut self, action: Action, detail: String) {
        self.link.set_state(RunState::Failed {
            action,
            failure: Failure {
                code: Some("launch-failed".into()),
                detail,
            },
        });
        self.poll();
    }

    /// Restart is offered once the installer is set to start next.
    pub(super) fn can_restart(&self) -> bool {
        matches!(
            self.state,
            RunState::Succeeded {
                action: Action::Stage | Action::Arm
            }
        )
    }

    pub(super) fn restart_deferred(&self) -> bool {
        self.restart_deferred
    }

    pub(super) fn defer_restart(&mut self) -> Result<(), String> {
        if !self.can_restart() {
            return Err(super::strings::t("imager-setup-internal-error-no-restart"));
        }
        self.restart_deferred = true;
        self.bump();
        Ok(())
    }

    /// The remedy command for `blocker`, when Imager offers one here.
    pub(super) fn remedy(&self, blocker: Blocker) -> Option<HostCommand> {
        match blocker {
            Blocker::SecureBootOn => Some(HostCommand::FirmwareSettings {
                suspend_bitlocker: self.host == HostOs::Windows
                    && self
                        .facts
                        .as_ref()
                        .is_some_and(|facts| facts.bitlocker_protected),
            }),
            Blocker::Hibernation if self.host == HostOs::Windows => {
                Some(HostCommand::DisableFastStartup)
            }
            _ => None,
        }
    }

    /// Whether `command` may run now.
    pub(super) fn command_available(&self, command: HostCommand) -> bool {
        match command {
            HostCommand::Restart => self.can_restart(),
            HostCommand::FirmwareSettings { .. } => self.blockers().iter().any(|blocker| {
                self.remedy(*blocker)
                    .is_some_and(|remedy| remedy == command)
            }),
            HostCommand::DisableFastStartup => {
                self.remedy(Blocker::Hibernation) == Some(command)
                    && self.blockers().contains(&Blocker::Hibernation)
            }
        }
    }

    /// The firmware-settings command this host would run, if offered.
    pub(super) fn firmware_command(&self) -> Option<HostCommand> {
        self.remedy(Blocker::SecureBootOn)
            .filter(|command| self.command_available(*command))
    }

    /// Pick up helper progress and stepper commits. `true` when anything
    /// shown changed.
    pub(super) fn poll(&mut self) -> bool {
        let mut changed = false;
        let (revision, state) = {
            let shared = self.link.lock();
            (shared.revision, shared.state.clone())
        };
        if revision != self.seen_revision {
            self.seen_revision = revision;
            self.reprobe_due |= match &state {
                RunState::Failed { failure, .. } => failure.code.as_deref() == Some("plan-stale"),
                RunState::Succeeded {
                    action: Action::Remove,
                } => true,
                _ => false,
            } && state != self.state;
            self.state = state;
            changed = true;
        }
        let gib = self.target_gib.get();
        if gib.is_finite() && gib >= 0.0 {
            let rounded = gib.round() as u64;
            if rounded * GIB != self.target_bytes {
                if let Some((min, max)) = self.bounds_gib() {
                    self.target_bytes = rounded.clamp(min, max) * GIB;
                    self.recompute();
                    changed = true;
                }
            }
        }
        if changed {
            self.bump();
        }
        changed
    }

    /// Once per finished run that should re-probe: review again after
    /// `plan-stale`, and refresh records after `remove`.
    pub(super) fn take_reprobe(&mut self) -> bool {
        std::mem::take(&mut self.reprobe_due)
    }

    /// Fixture state for screenshots and tests.
    pub(super) fn set_fixture_state(&mut self, state: RunState) {
        self.link.set_state(state);
        self.poll();
    }
}

/// Fluent key naming a blocker.
#[must_use]
pub(super) fn blocker_key(blocker: Blocker) -> &'static str {
    match blocker {
        Blocker::NotUefi => "not-uefi",
        Blocker::SecureBootOn => "secure-boot-on",
        Blocker::BitlockerLocked => "bitlocker-locked",
        Blocker::NotGpt => "not-gpt",
        Blocker::DiskUnsupported => "disk-unsupported",
        Blocker::Hibernation => "hibernation",
        Blocker::InsufficientSpace => "insufficient-space",
        Blocker::OnBattery => "on-battery",
        Blocker::PendingOperation => "pending-operation",
        Blocker::ArchMismatch => "arch-mismatch",
    }
}

/// The person-facing text of a blocker.
#[must_use]
pub(super) fn blocker_text(blocker: Blocker, host: HostOs) -> String {
    super::strings::t_args(
        &format!("imager-setup-internal-blocker-{}", blocker_key(blocker)),
        &[("host", host_arg(host).into())],
    )
}

/// The `$host` selector value every host-naming message takes.
#[must_use]
pub(super) fn host_arg(host: HostOs) -> &'static str {
    match host {
        HostOs::Windows => "windows",
        HostOs::Linux => "linux",
        HostOs::Other => "other",
    }
}

/// Fluent key naming a helper step.
#[must_use]
pub(super) fn step_key(step: Step) -> &'static str {
    match step {
        Step::Shrink => "shrink",
        Step::CreateEsp => "create-esp",
        Step::CreateRoot => "create-root",
        Step::WriteEsp => "write-esp",
        Step::WriteRoot => "write-root",
        Step::Verify => "verify",
        Step::Handoff => "handoff",
        Step::BootEntry => "boot-entry",
        Step::BootNext => "boot-next",
        Step::RemoveBootNext => "remove-boot-next",
        Step::RemoveBootEntry => "remove-boot-entry",
        Step::RemoveRoot => "remove-root",
        Step::RemoveEsp => "remove-esp",
        Step::RestoreShrink => "restore-shrink",
    }
}

/// The person-facing explanation of a failed run.
#[must_use]
pub(super) fn failure_text(failure: &Failure, host: HostOs) -> String {
    let key = match failure.code.as_deref() {
        Some("plan-stale") => "imager-setup-internal-failure-plan-stale",
        Some("boot-next-busy") => "imager-setup-internal-failure-boot-next-busy",
        Some("digest-mismatch") => "imager-setup-internal-failure-digest-mismatch",
        Some("launch-failed") => "imager-setup-internal-failure-launch",
        Some("interrupted") => "imager-setup-internal-failure-interrupted",
        _ => "imager-setup-internal-failure-generic",
    };
    super::strings::t_args(
        key,
        &[
            ("detail", failure.detail.as_str().into()),
            ("host", host_arg(host).into()),
        ],
    )
}

/// Size of the staging partitions in `plan`, in bytes.
#[must_use]
pub(super) fn staging_bytes(plan: &Plan) -> u64 {
    partition_bytes(plan, plan.staging_esp.first_lba, plan.staging_esp.last_lba).saturating_add(
        partition_bytes(
            plan,
            plan.staging_root.first_lba,
            plan.staging_root.last_lba,
        ),
    )
}

/// Size of the target extent in `plan`, in bytes.
#[must_use]
pub(super) fn target_extent_bytes(plan: &Plan) -> u64 {
    partition_bytes(
        plan,
        plan.target_extent.first_lba,
        plan.target_extent.last_lba,
    )
}

/// The platform remedies behind [`StagingRuntime::host_command`]. Tests and
/// shots inject fakes; nothing here runs outside a live session.
fn live_host_command(command: HostCommand) -> Result<(), String> {
    imager_backend::setup::native_host::run(command)
}

/// Deterministic host facts and a spec-shaped planner for `--shots` and
/// tests. Never reached from the live path.
pub(super) mod fixture {
    use super::{GIB, HostFacts, Plan, PlanError, Requirements, TargetBounds};
    use crate::setup::staging::plan::{
        Basis, BootEntry, Extent, FirmwareFacts, Shrink, StagedPartition,
    };
    use crate::setup::staging::platform::{Disk, SystemVolume};
    use crate::setup::staging::probe::Blocker;
    use crate::setup::staging::{BOOT_ENTRY_DESCRIPTION, MIN_TARGET_BYTES};

    /// Screenshot runtime: fixed facts with `blockers`, the fixture planner,
    /// and no helper, records or host commands.
    pub(in crate::setup) fn shot_runtime(blockers: Vec<Blocker>) -> super::StagingRuntime {
        let facts = facts(blockers);
        super::StagingRuntime {
            probe: std::sync::Arc::new(move || facts.clone()),
            compute: std::sync::Arc::new(fake_plan),
            target_bounds: std::sync::Arc::new(fake_bounds),
            launch: std::sync::Arc::new(|_| Err("screenshot fixture".into())),
            pending_records: std::sync::Arc::new(Vec::new),
            component_paths: std::sync::Arc::new(|_| Err("screenshot fixture".into())),
            host_command: std::sync::Arc::new(|_| Err("screenshot fixture".into())),
            poll_interval: std::time::Duration::from_millis(200),
        }
    }

    pub(in crate::setup) fn basis() -> Basis {
        Basis {
            linux_btrfs_volume: None,
            disk: Disk {
                id: "0".into(),
                guid: "11111111-2222-4333-8444-555555555555".into(),
                size_bytes: 512 * GIB,
                sector_bytes: 512,
                first_usable_lba: 34,
                last_usable_lba: 512 * GIB / 512 - 34,
                partitions: Vec::new(),
            },
            system_volume: Some(SystemVolume {
                partition_guid: "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee".into(),
                size_bytes: 400 * GIB,
                supported_min_bytes: 120 * GIB,
                used_bytes: 100 * GIB,
            }),
            firmware: FirmwareFacts {
                uefi: true,
                secure_boot: false,
                boot_current: Some("0001".into()),
                boot_order: vec!["0001".into(), "0000".into()],
                boot_next: None,
            },
        }
    }

    pub(in crate::setup) fn facts(blockers: Vec<Blocker>) -> HostFacts {
        HostFacts {
            basis: Some(basis()),
            bitlocker_protected: false,
            blockers,
        }
    }

    const SECTOR: u64 = 512;

    fn lbas(bytes: u64) -> u64 {
        bytes / SECTOR
    }

    /// A plan shaped like spec §2: shrink C:, target extent, then staging.
    pub(in crate::setup) fn fake_plan(
        basis: &Basis,
        requirements: Requirements,
    ) -> Result<Plan, PlanError> {
        let volume = basis.system_volume.as_ref().unwrap();
        let need = requirements.staging_esp_bytes
            + requirements.staging_root_bytes
            + requirements.target_bytes;
        if need > volume.size_bytes - volume.supported_min_bytes {
            return Err(PlanError::InsufficientSpace {
                needed_bytes: need,
                available_bytes: volume.size_bytes - volume.supported_min_bytes,
            });
        }
        let start = lbas(GIB) + lbas(volume.size_bytes - need);
        let target_last = start + lbas(requirements.target_bytes) - 1;
        let esp_last = target_last + lbas(requirements.staging_esp_bytes);
        let root_last = esp_last + lbas(requirements.staging_root_bytes);
        Ok(Plan {
            basis: basis.clone(),
            shrink: Some(Shrink {
                partition_guid: volume.partition_guid.clone(),
                from_bytes: volume.size_bytes,
                to_bytes: volume.size_bytes - need,
            }),
            target_extent: Extent {
                first_lba: start,
                last_lba: target_last,
            },
            staging_esp: StagedPartition {
                first_lba: target_last + 1,
                last_lba: esp_last,
                partition_guid: None,
            },
            staging_root: StagedPartition {
                first_lba: esp_last + 1,
                last_lba: root_last,
                partition_guid: None,
            },
            boot_entry: BootEntry {
                description: BOOT_ENTRY_DESCRIPTION.into(),
                loader_path: "\\EFI\\BOOT\\BOOTX64.EFI".into(),
            },
        })
    }

    pub(in crate::setup) fn fake_bounds(
        basis: &Basis,
        requirements: Requirements,
    ) -> Result<TargetBounds, PlanError> {
        let volume = basis.system_volume.as_ref().unwrap();
        Ok(TargetBounds {
            min_bytes: MIN_TARGET_BYTES,
            max_bytes: volume.size_bytes
                - volume.supported_min_bytes
                - requirements.staging_esp_bytes
                - requirements.staging_root_bytes,
        })
    }
}

#[cfg(test)]
#[cfg(unix)]
pub(super) mod tests {
    use super::fixture::{basis, facts, fake_bounds, fake_plan};
    use super::*;
    use crate::setup::staging::record::{OriginalFirmware, StepRecord, StepState};
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A scripted helper: each poll returns the next entry, the last repeats.
    pub(in crate::setup) struct ScriptedHelper(pub VecDeque<Poll>);

    impl HelperRun for ScriptedHelper {
        fn poll(&mut self) -> Poll {
            if self.0.len() > 1 {
                self.0.pop_front().unwrap()
            } else {
                self.0.front().cloned().unwrap_or_default()
            }
        }
    }

    pub(in crate::setup) fn finished(ok: bool, code: Option<&str>) -> Poll {
        Poll {
            latest: None,
            outcome: Some(Outcome {
                ok,
                code: code.map(Into::into),
                message: Some(format!("helper said {}", code.unwrap_or("ok"))),
            }),
        }
    }

    pub(in crate::setup) fn progress(step: Step, done: u64, total: u64) -> Poll {
        Poll {
            latest: Some(Progress {
                step,
                done_bytes: done,
                total_bytes: total,
            }),
            outcome: None,
        }
    }

    /// Requests the fake helper was launched with, in order.
    pub(in crate::setup) type Launches = Arc<Mutex<Vec<Request>>>;

    pub(in crate::setup) struct Fakes {
        pub facts: HostFacts,
        pub records: Vec<Record>,
        pub script: Vec<Poll>,
        pub commands: Arc<Mutex<Vec<HostCommand>>>,
        pub launches: Launches,
        /// Manifest bytes readable at each launch.
        pub manifests: Arc<Mutex<Vec<Vec<u8>>>>,
        pub probes: Arc<AtomicUsize>,
    }

    impl Fakes {
        pub(in crate::setup) fn new(blockers: Vec<Blocker>) -> Self {
            Self {
                facts: facts(blockers),
                records: Vec::new(),
                script: vec![finished(true, None)],
                commands: Arc::default(),
                launches: Arc::default(),
                manifests: Arc::default(),
                probes: Arc::default(),
            }
        }

        pub(in crate::setup) fn runtime(&self) -> StagingRuntime {
            let facts = self.facts.clone();
            let records = self.records.clone();
            let script = self.script.clone();
            let commands = Arc::clone(&self.commands);
            let launches = Arc::clone(&self.launches);
            let manifests = Arc::clone(&self.manifests);
            let probes = Arc::clone(&self.probes);
            StagingRuntime {
                probe: Arc::new(move || {
                    probes.fetch_add(1, Ordering::SeqCst);
                    facts.clone()
                }),
                compute: Arc::new(fake_plan),
                target_bounds: Arc::new(fake_bounds),
                launch: Arc::new(move |request| {
                    if let Ok(bytes) = std::fs::read(&request.manifest_path) {
                        manifests.lock().unwrap().push(bytes);
                    }
                    launches.lock().unwrap().push(request.clone());
                    Ok(Box::new(ScriptedHelper(script.clone().into())) as Box<dyn HelperRun>)
                }),
                pending_records: Arc::new(move || records.clone()),
                component_paths: Arc::new(|_| {
                    Ok(ComponentPaths {
                        staging_esp: "/verified/staging-esp.img".into(),
                        staging_root: "/verified/staging-root.img".into(),
                    })
                }),
                host_command: Arc::new(move |command| {
                    commands.lock().unwrap().push(command);
                    Ok(())
                }),
                poll_interval: Duration::from_millis(1),
            }
        }
    }

    pub(in crate::setup) fn record(operation: &str) -> Record {
        let plan = fake_plan(
            &basis(),
            Requirements {
                staging_esp_bytes: PROVISIONAL_STAGING_ESP_BYTES,
                staging_root_bytes: PROVISIONAL_STAGING_ROOT_BYTES,
                target_bytes: DEFAULT_TARGET_BYTES,
            },
        )
        .unwrap();
        Record {
            schema: 1,
            operation_id: operation.into(),
            plan_sha256: plan.digest().unwrap(),
            plan,
            release: ReleaseRef {
                release_id: "fixture-only".into(),
                manifest_sha256: "a".repeat(64),
                architecture: "x86_64".into(),
            },
            steps: vec![StepRecord {
                step: Step::WriteRoot,
                state: StepState::Intended,
                detail: serde_json::Map::new(),
                at: 0,
            }],
            original_firmware: OriginalFirmware {
                boot_order: vec!["0001".into()],
                boot_next: None,
                entries: std::collections::BTreeMap::default(),
            },
            owned_boot_entry: None,
            bundle: None,
            migration: None,
            profile: serde_json::Map::new(),
            prepared_setup: None,
            prepared_browser: None,
        }
    }

    fn probed(fakes: &Fakes, has_release: bool) -> InternalInstall {
        let mut install = InternalInstall::new(fakes.runtime(), true, HostOs::Windows);
        let (facts, records) = install.probe_task().unwrap()();
        install.accept_probe(facts, records, has_release);
        install
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn selected_application_budget_survives_controller_size_edits_and_reprobe() {
        let (_source, intent) = super::super::migration_intent::reviewed_app_routing_fixture();
        let minimum = intent.application_minimum().div_ceil(GIB);
        let fakes = Fakes::new(Vec::new());
        let mut install = probed(&fakes, true);
        install.set_migration_intent(Some(intent)).unwrap();
        assert_eq!(install.bounds_gib().unwrap().0, minimum);
        assert!(install.set_target_gib(minimum - 1).is_err());
        install.target_state().set(1.0);
        install.poll();
        assert_eq!(install.target_bytes(), minimum * GIB);
        let (facts, records) = install.probe_task().unwrap()();
        install.accept_probe(facts, records, true);
        assert_eq!(install.bounds_gib().unwrap().0, minimum);
        assert!(install.set_target_gib(minimum - 1).is_err());
        assert!(fakes.launches.lock().unwrap().is_empty());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn guided_review_retains_authenticated_ciphertext_and_refuses_changed_inputs() {
        use super::super::{
            journey::prepared_media::PreparedMedia, prepared_identity::PreparedIdentity,
        };
        use std::os::unix::fs::PermissionsExt;
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(runtime_identity::FILE_SECRETS_ENV, "1");
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let identity = PreparedIdentity::create(
            &root.join("identity"),
            "Ada",
            [20, 80, 140],
            tilo_identity_host::provisioning::IdentityStorage::for_session(),
        )
        .unwrap();
        let identity_bytes = std::fs::read(root.join("identity/primary.json")).unwrap();
        let mut profile = crate::model::InstallProfile::default();
        let public =
            super::super::prepared_setup::PublicInstallProfile::from_draft(&profile).unwrap();
        let prepared = PreparedMedia::prepare_for_install(
            &identity,
            None,
            None,
            zeroize::Zeroizing::new("a sufficiently long setup password".into()),
            "guided-internal-binding".into(),
            Some(public),
        )
        .unwrap();
        profile.prepared_setup = Some(prepared.expectation().clone());
        let artifact = prepared.staging_artifact().unwrap();
        let ciphertext = std::fs::read(&artifact.source_path).unwrap();
        let fakes = Fakes::new(Vec::new());
        let mut install = probed(&fakes, true);
        let reviewed = ReviewedFullOs {
            facts: fakes.facts.clone(),
            plan: install.plan().unwrap().as_ref().unwrap().clone(),
            profile,
            migration: None,
            prepared: Some(prepared),
            deferred: false,
        };
        let mut changed = reviewed.clone();
        changed.profile.hostname = "different-device".into();
        assert!(install.adopt_reviewed(changed, true).is_err());
        let mut changed = reviewed.clone();
        changed.plan.target_extent.first_lba += 1;
        assert!(install.adopt_reviewed(changed, true).is_err());
        let mut missing = reviewed.clone();
        missing.prepared = None;
        missing.profile.prepared_setup = None;
        assert!(
            install.adopt_reviewed(missing, true).is_err(),
            "a non-deferred review without prepared custody must be refused"
        );
        let mut deferred = reviewed.clone();
        deferred.prepared = None;
        deferred.deferred = true;
        deferred.profile.prepared_setup = None;
        let mut deferred_install = probed(&fakes, true);
        deferred_install.adopt_reviewed(deferred, true).unwrap();
        let mut gated = InternalInstall::new(fakes.runtime(), false, HostOs::Linux);
        assert!(gated.adopt_reviewed(reviewed.clone(), true).is_err());
        assert!(install.adopt_reviewed(reviewed.clone(), false).is_err());
        install.adopt_reviewed(reviewed.clone(), true).unwrap();
        assert_eq!(
            install.reviewed_digest().unwrap().unwrap(),
            reviewed.plan.digest().unwrap()
        );
        let digest = reviewed.plan.digest().unwrap();
        install.begin_stage(&digest, true).unwrap();
        let intent = install.link.lock().intent.take().unwrap();
        assert!(intent.prepared_custody.is_some());
        let manifest = selected_sizes(
            PROVISIONAL_STAGING_ESP_BYTES,
            PROVISIONAL_STAGING_ROOT_BYTES,
        );
        let directory = tempfile::tempdir().unwrap();
        let request = stage_request(
            &install.link,
            &manifest,
            intent,
            directory.path(),
            &std::sync::atomic::AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(request.prepared_setup, Some(artifact.clone()));
        assert_eq!(
            request.profile["prepared_setup"],
            serde_json::to_value(&artifact.expectation).unwrap()
        );
        assert!(
            !serde_json::to_string(&request)
                .unwrap()
                .contains("a sufficiently long setup password")
        );
        assert_eq!(std::fs::read(&artifact.source_path).unwrap(), ciphertext);
        assert_eq!(
            std::fs::read(root.join("identity/primary.json")).unwrap(),
            identity_bytes
        );
        assert!(fakes.launches.lock().unwrap().is_empty());
    }

    fn wait_state(install: &mut InternalInstall, done: impl Fn(&RunState) -> bool) {
        let deadline = tilo_test_env::hang_deadline();
        while !done(install.run_state()) {
            install.poll();
            assert!(ui::time::now() < deadline, "run did not finish");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Gate, readiness, remedies and copy.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn internal_install_gate_readiness_and_remedies_follow_blockers() {
        // Phase `gate_requires_the_release_flag_or_the_explicit_development_opt_in`.
        // Catches offering Full Tilo from platform identity alone or from a dev
        // build without the explicit opt-in.
        {
            assert!(!gate_open(None, None));
            assert!(!gate_open(Some("0"), Some("true")));
            assert!(gate_open(Some("1"), None));
            assert!(gate_open(None, Some("1")));

            let fakes = Fakes::new(Vec::new());
            let mut gated = InternalInstall::new(fakes.runtime(), false, HostOs::Windows);
            assert!(gated.probe_task().is_none());
            assert_eq!(gated.readiness(), &Readiness::Gated);
            gated.accept_probe(facts(Vec::new()), Vec::new(), true);
            assert!(!gated.offers_full_os());
            assert_eq!(fakes.probes.load(Ordering::SeqCst), 0);
        }

        // Phase `readiness_needs_probe_release_and_only_remediable_blockers`.
        // Catches enabling Full Tilo while checking, without a release, or when
        // a blocker without a remedy exists.
        {
            let fakes = Fakes::new(Vec::new());
            let mut install = InternalInstall::new(fakes.runtime(), true, HostOs::Windows);
            assert_eq!(install.readiness(), &Readiness::Checking);
            assert!(!install.offers_full_os());

            assert_eq!(probed(&fakes, false).readiness(), &Readiness::NoRelease);
            let ready = probed(&fakes, true);
            assert!(ready.offers_full_os());
            assert!(ready.install_blocker(true).is_ok());
            assert!(ready.install_blocker(false).is_err());

            let remediable = probed(&Fakes::new(vec![Blocker::SecureBootOn]), true);
            assert!(remediable.offers_full_os());
            assert!(remediable.install_blocker(true).is_err());
            assert!(remediable.firmware_command().is_some());

            let unsupported = probed(&Fakes::new(vec![Blocker::NotGpt, Blocker::OnBattery]), true);
            assert_eq!(
                unsupported.readiness(),
                &Readiness::Unsupported(vec![Blocker::NotGpt])
            );
            assert!(!unsupported.offers_full_os());
            let blind = HostFacts {
                basis: None,
                bitlocker_protected: false,
                blockers: Vec::new(),
            };
            install.accept_probe(blind, Vec::new(), true);
            assert!(!install.offers_full_os());
        }

        // Phase `remedies_follow_blockers_and_host`.
        // Catches remedies offered where they do not apply, e.g. Fast Startup on
        // Linux or BitLocker suspension without BitLocker.
        {
            let mut fakes = Fakes::new(vec![Blocker::SecureBootOn, Blocker::Hibernation]);
            fakes.facts.bitlocker_protected = true;
            let windows = probed(&fakes, true);
            assert_eq!(
                windows.firmware_command(),
                Some(HostCommand::FirmwareSettings {
                    suspend_bitlocker: true
                })
            );
            assert!(windows.command_available(HostCommand::DisableFastStartup));
            assert!(!windows.command_available(HostCommand::Restart));

            let mut linux = InternalInstall::new(fakes.runtime(), true, HostOs::Linux);
            let (facts, records) = linux.probe_task().unwrap()();
            linux.accept_probe(facts, records, true);
            assert_eq!(
                linux.firmware_command(),
                Some(HostCommand::FirmwareSettings {
                    suspend_bitlocker: false
                })
            );
            assert!(!linux.command_available(HostCommand::DisableFastStartup));
            assert!(
                probed(&Fakes::new(Vec::new()), true)
                    .firmware_command()
                    .is_none()
            );
        }

        // Phase `every_blocker_and_step_has_copy`.
        {
            for blocker in [
                Blocker::NotUefi,
                Blocker::SecureBootOn,
                Blocker::BitlockerLocked,
                Blocker::NotGpt,
                Blocker::DiskUnsupported,
                Blocker::Hibernation,
                Blocker::InsufficientSpace,
                Blocker::OnBattery,
                Blocker::PendingOperation,
                Blocker::ArchMismatch,
            ] {
                assert_eq!(
                    serde_json::to_value(blocker).unwrap(),
                    serde_json::json!(blocker_key(blocker))
                );
                let text = blocker_text(blocker, HostOs::Windows);
                assert!(!text.starts_with("imager-setup"), "{text}");
            }
            for step in [
                Step::Shrink,
                Step::CreateEsp,
                Step::CreateRoot,
                Step::WriteEsp,
                Step::WriteRoot,
                Step::Verify,
                Step::Handoff,
                Step::BootEntry,
                Step::BootNext,
                Step::RemoveBootNext,
                Step::RemoveBootEntry,
                Step::RemoveRoot,
                Step::RemoveEsp,
                Step::RestoreShrink,
            ] {
                assert_eq!(
                    serde_json::to_value(step).unwrap(),
                    serde_json::json!(step_key(step))
                );
                let text = super::super::strings::t(&format!(
                    "imager-setup-internal-step-{}",
                    step_key(step)
                ));
                assert!(!text.starts_with("imager-setup"), "{text}");
            }
        }
    }

    /// Target sizing and replanning.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn internal_install_target_size_replans_within_bounds() {
        // Phase `target_size_stays_within_bounds_and_replans`.
        // Catches a stepper value escaping the planner's bounds or a size change
        // that leaves the shown plan stale.
        {
            let fakes = Fakes::new(Vec::new());
            let mut install = probed(&fakes, true);
            let (min, max) = install.bounds_gib().unwrap();
            assert_eq!(min, 35);
            assert_eq!(max, 400 - 120 - 42);
            let first = install.reviewed_digest().unwrap().unwrap();
            assert!(install.set_target_gib(min - 1).is_err());
            assert!(install.set_target_gib(max + 1).is_err());
            install.set_target_gib(100).unwrap();
            assert_eq!(install.target_bytes(), 100 * GIB);
            let Some(Ok(plan)) = install.plan() else {
                panic!("plan")
            };
            assert_eq!(target_extent_bytes(plan), 100 * GIB);
            assert_ne!(install.reviewed_digest().unwrap().unwrap(), first);

            install.target_state().set(1000.0);
            assert!(install.poll());
            assert_eq!(install.target_bytes(), max * GIB);
        }

        // Phase `smaller_manifest_sizes_are_replanned_without_shrinking_further`.
        // Catches the helper refusing every real install (its plan must be the
        // planner's exact result for the bundle's sizes) and a replan that
        // silently shrinks the system volume further than the person reviewed.
        {
            let fakes = Fakes::new(Vec::new());
            let mut install = probed(&fakes, true);
            let digest = install.reviewed_digest().unwrap().unwrap();
            install.begin_stage(&digest, true).unwrap();
            let intent = install.link.lock().intent.clone().unwrap();
            let reviewed = intent.plan.clone();
            let dir = tempfile::tempdir().unwrap();
            let manifest = selected_sizes(64 << 20, 40 * GIB);
            let request = stage_request(
                &install.link,
                &manifest,
                intent,
                dir.path(),
                &std::sync::atomic::AtomicBool::new(false),
            )
            .unwrap();
            assert_ne!(request.plan_sha256, digest);
            assert_eq!(request.plan_sha256, request.plan.digest().unwrap());
            let s = request.plan.basis.disk.sector_bytes;
            assert_eq!(request.plan.staging_esp.extent().lbas() * s, 64 << 20);
            assert_eq!(
                request.plan.target_extent.lbas(),
                reviewed.target_extent.lbas()
            );
            let kept = |p: &Plan| p.shrink.as_ref().map_or(u64::MAX, |x| x.to_bytes);
            assert!(kept(&request.plan) >= kept(&reviewed));
        }

        // Phase `undersized_plan_is_refused_as_stale_and_replanned_with_manifest_sizes`.
        // Catches staging a plan whose installer partitions are smaller than the
        // verified manifest requires, and a re-review that ignores the real size.
        {
            let fakes = Fakes::new(Vec::new());
            let mut install = probed(&fakes, true);
            let digest = install.reviewed_digest().unwrap().unwrap();
            install.begin_stage(&digest, true).unwrap();
            let intent = install.link.lock().intent.take().unwrap();
            let dir = tempfile::tempdir().unwrap();
            let outcome = stage_request(
                &install.link,
                &selected(48 * GIB),
                intent,
                dir.path(),
                &std::sync::atomic::AtomicBool::new(false),
            )
            .unwrap_err();
            assert_eq!(outcome.code.as_deref(), Some("plan-stale"));
            install.link.finish(Action::Stage, &outcome);
            install.poll();
            assert!(install.take_reprobe());
            let (facts, records) = install.probe_task().unwrap()();
            install.accept_probe(facts, records, true);
            let Some(Ok(plan)) = install.plan() else {
                panic!("replanned")
            };
            assert!(
                partition_bytes(
                    plan,
                    plan.staging_root.first_lba,
                    plan.staging_root.last_lba
                ) >= 48 * GIB
            );
            assert_ne!(install.reviewed_digest().unwrap().unwrap(), digest);
        }
    }

    /// Stage request binding, helper refusals, operation ids, retry/remove and progress.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn internal_install_staging_binds_the_reviewed_request_and_reports_outcomes() {
        // Phase `retry_and_remove_act_on_the_current_record_only`.
        // Catches arm/remove running without the unfinished record or against a
        // record the person no longer sees.
        {
            let mut fakes = Fakes::new(vec![Blocker::PendingOperation]);
            assert!(probed(&fakes, true).helper_task(Action::Arm, "x").is_err());
            fakes.records = vec![record("op-1")];
            fakes.script = vec![progress(Step::BootNext, 0, 1), finished(true, None)];
            let mut install = probed(&fakes, true);
            assert!(install.helper_task(Action::Remove, "op-2").is_err());
            let task = install.helper_task(Action::Arm, "op-1").unwrap();
            assert!(install.is_running());
            assert!(install.helper_task(Action::Remove, "op-1").is_err());
            task();
            install.poll();
            assert_eq!(
                install.run_state(),
                &RunState::Succeeded {
                    action: Action::Arm
                }
            );
            assert!(install.can_restart());
            let launched = fakes.launches.lock().unwrap();
            assert_eq!(launched.len(), 1);
            assert_eq!(launched[0].action, Action::Arm);
            assert_eq!(launched[0].operation_id, "op-1");
            assert!(launched[0].manifest_path.is_empty());
        }

        // Phase `helper_refusals_keep_their_stable_codes`.
        // Catches a refused helper being reported as success, or a plan-stale
        // refusal that does not send the person back to review.
        {
            let mut fakes = Fakes::new(vec![Blocker::PendingOperation]);
            fakes.records = vec![record("op-1")];
            for code in ["plan-stale", "boot-next-busy", "digest-mismatch", "other"] {
                fakes.script = vec![finished(false, Some(code))];
                let mut install = probed(&fakes, true);
                install.helper_task(Action::Arm, "op-1").unwrap()();
                install.poll();
                let RunState::Failed { failure, .. } = install.run_state().clone() else {
                    panic!("{code} should fail");
                };
                assert_eq!(failure.code.as_deref(), Some(code));
                assert_eq!(install.take_reprobe(), code == "plan-stale");
                assert!(!install.take_reprobe());
                assert!(!install.can_restart());
                let text = failure_text(&failure, HostOs::Windows);
                assert!(!text.contains("imager-setup"), "{text}");
            }
        }

        // Phase `stage_requires_the_reviewed_digest_and_launch_failures_settle`.
        // Catches a launch failure hanging in Running or an install proceeding
        // against a plan that changed after review.
        {
            let fakes = Fakes::new(Vec::new());
            let mut install = probed(&fakes, true);
            assert!(install.begin_stage("stale", true).is_err());
            assert!(!install.is_running());
            let digest = install.reviewed_digest().unwrap().unwrap();
            assert!(install.begin_stage(&digest, false).is_err());
            install.begin_stage(&digest, true).unwrap();
            assert!(install.is_running());
            install.abort_stage("download", "worker refused".into());
            let RunState::Failed { failure, .. } = install.run_state() else {
                panic!("abort should fail");
            };
            assert_eq!(failure.code.as_deref(), Some("download"));
            assert!(install.link.lock().intent.is_none());
        }

        // Phase `stage_request_binds_release_plan_paths_and_manifest_bytes`.
        // Catches a request that names the wrong release, loses the reviewed
        // plan digest, or hands the helper a manifest it cannot re-hash.
        {
            let fakes = Fakes::new(Vec::new());
            let mut install = probed(&fakes, true);
            let digest = install.reviewed_digest().unwrap().unwrap();
            install.begin_stage(&digest, true).unwrap();
            let intent = install.link.lock().intent.clone().unwrap();
            let dir = tempfile::tempdir().unwrap();
            let manifest = selected(40 * GIB);
            let request = stage_request(
                &install.link,
                &manifest,
                intent,
                dir.path(),
                &std::sync::atomic::AtomicBool::new(false),
            )
            .unwrap();
            assert_eq!(request.action, Action::Stage);
            assert_eq!(request.plan_sha256, digest);
            assert_eq!(request.plan.digest().unwrap(), digest);
            assert_eq!(request.release.release_id, "fixture-only");
            assert_eq!(request.release.architecture, "x86_64");
            assert_eq!(request.release.manifest_sha256, manifest.manifest_sha256());
            assert_eq!(
                request.component_paths.staging_esp,
                "/verified/staging-esp.img"
            );
            assert_eq!(
                std::fs::read(&request.manifest_path).unwrap(),
                manifest.bytes()
            );
            assert_eq!(
                install.link.lock().requirements,
                Some((PROVISIONAL_STAGING_ESP_BYTES, 40 * GIB))
            );
        }

        // Phase `operation_ids_are_distinct_version_four_uuids`.
        {
            let a = new_operation_id();
            let b = new_operation_id();
            assert_ne!(a, b);
            assert_eq!(a.len(), 36);
            assert_eq!(&a[14..15], "4");
            assert!(matches!(&a[19..20], "8" | "9" | "a" | "b"));
        }

        // Phase `scripted_helper_reports_progress_before_its_outcome`.
        // `wait_state` is shared with the download-flow consumer tests.
        {
            let mut fakes = Fakes::new(vec![Blocker::PendingOperation]);
            fakes.records = vec![record("op-1")];
            fakes.script = vec![
                progress(Step::Verify, 5, 10),
                Poll::default(),
                finished(true, None),
            ];
            let mut install = probed(&fakes, true);
            let task = install.helper_task(Action::Remove, "op-1").unwrap();
            let thread = std::thread::spawn(task);
            wait_state(&mut install, |state| {
                !matches!(state, RunState::Running { .. })
            });
            thread.join().unwrap();
            assert_eq!(
                install.run_state(),
                &RunState::Succeeded {
                    action: Action::Remove
                }
            );
            assert!(install.take_reprobe());
        }
    }

    fn selected(root_minimum: u64) -> SelectedManifest {
        selected_sizes(PROVISIONAL_STAGING_ESP_BYTES, root_minimum)
    }

    fn selected_sizes(esp_minimum: u64, root_minimum: u64) -> SelectedManifest {
        let mut manifest = crate::setup::internal_bundle::tests::fixture();
        manifest["components"][0]["minimum_partition_bytes"] = serde_json::json!(esp_minimum);
        manifest["components"][1]["minimum_partition_bytes"] = serde_json::json!(root_minimum);
        let bytes = crate::setup::internal_bundle::tests::bytes(&manifest);
        let release = crate::release::InternalRelease::transport_fixture(
            &bytes,
            "https://code.tilo.so/releases/manifest.json",
        );
        SelectedManifest::from_bytes(&release, bytes).unwrap()
    }
}
