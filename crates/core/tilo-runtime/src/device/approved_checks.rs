//! The checks a project's host approved, run as this device's work.
//!
//! An app asks for a check by its id only. The commands, sources, images and bounds come from a
//! private startup configuration this device reads before it admits anything, so an app chooses
//! which approved check runs and never what it runs. A request naming a check, a project, a
//! record or a resource identity the configuration does not register is refused with the reason.
//!
//! The check is work of this device, like a turn an offloaded action takes: [`takes_turn`]
//! decides whether it runs here, the device keeps the job at `devices/<device>/jobs/<job>` while
//! it runs, and each phase it reports and its end are written there. The record is the one a view
//! follows through the record API; the caller that asked follows the same work as its invocation's
//! progress and answer.

use std::collections::BTreeMap;
use std::io::Read;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use runtime_collab::approved_checks::{
    Binding, Diagnostic, MAX_PREVIEW_BYTES, Outcome, Receipt, ReportObservation,
};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use tilo_actions::ResourceSemanticIdentity;
use tilo_actions::remote::{
    Caller, ResourceAddress, ResourceCopy, ResourceReadRequest, ResourceReads,
};
use tilo_lock::MutexRecover as _;
use tilo_workspace_service::apps::collab_app_broker::{
    ApprovedCheckContext, ApprovedCheckServices,
};
use tilo_workspace_service::work::{Actor, Claim, Turn, takes_turn};
use ush::approved_checks::{
    ApprovedCheck, ApprovedChecks, CheckOutcome, CheckReceipt, ProjectBinding,
};

/// The app a device's jobs are read under: the record `devices/<device>/jobs/<job>`.
pub(crate) const JOBS: &str = "jobs";

/// The longest private configuration this device reads, and the most projects it names.
const MAX_CONFIG_BYTES: u64 = 8 * 1024 * 1024;
const MAX_PROJECTS: usize = 32;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    version: u8,
    projects: Vec<ProjectConfig>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectConfig {
    app_id: String,
    resource_identity: ResourceIdentity,
    artifacts: PathBuf,
    checks: Vec<ApprovedCheck>,
}

#[derive(Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ResourceIdentity {
    kind: String,
    key: String,
}

/// One approved project: what it is, and the checks its host approved for exactly it.
struct Registered {
    app_id: String,
    identity: ResourceSemanticIdentity,
    binding: ProjectBinding,
    artifacts: PathBuf,
    checks: Box<dyn ApprovedProject>,
}

impl Registered {
    fn matches(
        &self,
        app: &str,
        workspace: &str,
        record: &str,
        identity: &ResourceSemanticIdentity,
    ) -> bool {
        self.app_id == app
            && self.binding.workspace.to_string() == workspace
            && self.binding.record == record
            && self.identity == *identity
    }
}

/// The checks one approved project hosts, run by id.
trait ApprovedProject: Send + Sync {
    /// Run the approved check `check_id`, rechecking `authority` before any external work and
    /// while it runs, and reporting each phase `report` hears.
    fn run(
        &self,
        check_id: &str,
        authority: &(dyn Fn() -> Result<(), String> + Sync),
        report: &dyn Fn(ReportObservation),
    ) -> Result<CheckReceipt, String>;
}

/// The checks one project hosts, with the binding they were approved under.
struct ProjectChecks {
    binding: ProjectBinding,
    checks: ApprovedChecks,
}

impl ApprovedProject for ProjectChecks {
    fn run(
        &self,
        check_id: &str,
        authority: &(dyn Fn() -> Result<(), String> + Sync),
        report: &dyn Fn(ReportObservation),
    ) -> Result<CheckReceipt, String> {
        self.checks
            .run_with_reports(&self.binding, check_id, authority, |event| {
                report(observation(event))
            })
    }
}

/// An approved check's informational phase, as the app hears it.
fn observation(event: ush::approved_checks::ReportedCheckEvent) -> ReportObservation {
    use runtime_collab::approved_checks::{EndpointRefusal, ReportObservation};
    use ush::approved_checks::{CheckReportRefusal, ReportedCheckEvent};
    match event {
        ReportedCheckEvent::Reported(report) => ReportObservation::Reported {
            phase: report.phase,
            completed: report.completed,
            total: report.total,
        },
        ReportedCheckEvent::Disabled(reason) => ReportObservation::Disabled {
            reason: match reason {
                CheckReportRefusal::InvalidReport => EndpointRefusal::InvalidReport,
                CheckReportRefusal::OversizedReport => EndpointRefusal::OversizedReport,
                CheckReportRefusal::UnknownPhase => EndpointRefusal::UnknownPhase,
                CheckReportRefusal::CorrelationChanged => EndpointRefusal::CorrelationChanged,
                CheckReportRefusal::SequenceChanged => EndpointRefusal::SequenceChanged,
                CheckReportRefusal::OutputUnavailable => EndpointRefusal::OutputUnavailable,
                CheckReportRefusal::Backpressure => EndpointRefusal::Backpressure,
            },
        },
    }
}

#[derive(Default)]
struct Definitions(Vec<Registered>);

impl Definitions {
    /// The approved project exactly `app`, `workspace` and `record` name, whose resource
    /// `identity` names too, or `None` when no project was approved for it.
    fn find(
        &self,
        app: &str,
        workspace: &str,
        record: &str,
        identity: &ResourceSemanticIdentity,
    ) -> Option<&Registered> {
        self.0
            .iter()
            .find(|registered| registered.matches(app, workspace, record, identity))
    }
}

/// The approved projects a device reads at startup: what it runs, and what it refuses.
struct Registry(Arc<Definitions>);

impl Registry {
    fn find(
        &self,
        app: &str,
        workspace: &str,
        record: &str,
        identity: &ResourceSemanticIdentity,
    ) -> Option<&Registered> {
        self.0.find(app, workspace, record, identity)
    }
}

static DEFINITIONS: OnceLock<Arc<Registry>> = OnceLock::new();

fn effective_uid() -> u32 {
    rustix::process::geteuid().as_raw()
}

/// The configuration a startup flag named: an owned private regular file, never a link.
fn load(path: &Path) -> Result<Registry, String> {
    let mut file = tilo_files::native::private::read(path).map_err(|error| error.to_string())?;
    if file.metadata().map_err(|error| error.to_string())?.len() > MAX_CONFIG_BYTES {
        return Err("approved-check startup configuration exceeds bound".into());
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(MAX_CONFIG_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    drop(file);
    if u64::try_from(bytes.len()).map_err(|error| error.to_string())? > MAX_CONFIG_BYTES {
        return Err("approved-check startup configuration grew beyond bound".into());
    }
    let config: Config = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    if config.version != 1 || config.projects.len() > MAX_PROJECTS {
        return Err("unsupported or oversized approved-check startup configuration".into());
    }
    let mut definitions = Vec::<Registered>::new();
    for project in config.projects {
        let identity = ResourceSemanticIdentity::new(
            project.resource_identity.kind.clone(),
            project.resource_identity.key.clone(),
        )
        .ok_or("approved project semantic identity invalid")?;
        let binding = project
            .checks
            .first()
            .ok_or("approved project has no checks")?
            .binding
            .clone();
        if identity.key != binding.resource {
            return Err("approved fixed check resource must equal its semantic key".into());
        }
        if project.app_id.is_empty()
            || project.app_id.len() > 128
            || project.app_id.starts_with('-')
            || !project
                .app_id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        {
            return Err("approved project app identity invalid".into());
        }
        if binding.record.is_empty() || binding.record.len() > 1024 {
            return Err("approved project identity exceeds portable receipt vocabulary".into());
        }
        if definitions.iter().any(|other: &Registered| {
            other.app_id == project.app_id
                && other.identity == identity
                && other.binding.workspace == binding.workspace
                && other.binding.record == binding.record
        }) {
            return Err("duplicate approved project binding".into());
        }
        // The private artifacts directory is exact, owned and private, and never a tree the
        // checked source or its build output is written into.
        let metadata =
            std::fs::symlink_metadata(&project.artifacts).map_err(|error| error.to_string())?;
        if !project.artifacts.is_absolute()
            || project
                .artifacts
                .canonicalize()
                .map_err(|error| error.to_string())?
                != project.artifacts
            || !metadata.is_dir()
            || metadata.uid() != effective_uid()
            || metadata.mode() & 0o077 != 0
            || project.checks.iter().any(|check| {
                project.artifacts.starts_with(&check.checkout)
                    || project.artifacts.starts_with(&check.build)
                    || check.checkout.starts_with(&project.artifacts)
                    || check.build.starts_with(&project.artifacts)
            })
        {
            return Err("approved private artifacts directory is not owned/private".into());
        }
        let checks = ApprovedChecks::new(binding.clone(), project.checks)?;
        definitions.push(Registered {
            app_id: project.app_id,
            identity,
            binding: binding.clone(),
            artifacts: project.artifacts,
            checks: Box::new(ProjectChecks { binding, checks }),
        });
    }
    Ok(Registry(Arc::new(Definitions(definitions))))
}

/// The private configuration `arguments` names, if any: one flag, one absolute path.
fn argument_path(
    arguments: impl IntoIterator<Item = std::ffi::OsString>,
) -> Result<Option<PathBuf>, String> {
    let mut arguments = arguments.into_iter();
    let mut selected = None;
    while let Some(argument) = arguments.next() {
        let path = if argument == "--approved-checks" {
            Some(PathBuf::from(arguments.next().ok_or(
                "--approved-checks requires a private configuration path",
            )?))
        } else {
            argument
                .to_str()
                .and_then(|argument| argument.strip_prefix("--approved-checks="))
                .map(PathBuf::from)
        };
        if let Some(path) = path {
            if selected.is_some() || !path.is_absolute() {
                return Err("approved-check startup path must be unique and absolute".into());
            }
            selected = Some(path);
        }
    }
    Ok(selected)
}

/// Load the immutable private configuration `arguments` names before the device admits anything.
///
/// # Errors
/// Duplicate flags, a non-private or linked file, or a malformed or foreign registration.
pub fn install_from_args(
    arguments: impl IntoIterator<Item = std::ffi::OsString>,
) -> Result<(), String> {
    let definitions = argument_path(arguments)?
        .map_or_else(|| Ok(Registry(Arc::default())), |path| load(&path))?;
    DEFINITIONS
        .set(Arc::new(definitions))
        .map_err(|_| "approved-check definitions already installed".into())
}

/// The definitions this process installed, or none.
fn installed() -> Arc<Registry> {
    DEFINITIONS
        .get()
        .cloned()
        .unwrap_or_else(|| Arc::new(Registry(Arc::default())))
}

/// A claim on a check's turn of work, kept while the check runs.
trait Renewal: Send + Sync {
    fn renew(&self) -> Result<(), String>;
}

/// The claim a check took in its workspace, renewed while it runs.
struct Taken {
    claim: Claim,
}

impl Renewal for Taken {
    fn renew(&self) -> Result<(), String> {
        if on_runtime(self.claim.renew()) {
            Ok(())
        } else {
            Err("this device lost the check to another device".into())
        }
    }
}

/// Run `future` on the daemon's runtime from this thread: a check runs on its own thread, off the
/// runtime that answers the calls asking for it.
fn on_runtime<F: std::future::Future>(future: F) -> F::Output {
    tilo_rt::app().handle().block_on(future)
}

/// What places a check's turn: [`takes_turn`] in the project's workspace, on the device that
/// asks.
trait Placement: Send + Sync {
    fn take(
        &self,
        app: &str,
        workspace: &str,
        record: &str,
        check_id: &str,
        invocation: &str,
    ) -> Result<Box<dyn Renewal>, String>;
}

struct WorkspaceTurns {
    workspaces: tilo_workspace_service::workspaces::Workspaces,
}

impl Placement for WorkspaceTurns {
    fn take(
        &self,
        app: &str,
        workspace: &str,
        record: &str,
        check_id: &str,
        invocation: &str,
    ) -> Result<Box<dyn Renewal>, String> {
        let person = hex::encode(self.workspaces.device_cert().user_public_key);
        let turn = Turn::Once {
            address: format!("{app}/{record}#{check_id}"),
            inputs: serde_json::json!({ "invocation": invocation, "check": check_id }),
            actor: Some(Actor::Person(person)),
            about: Some((app.to_owned(), record.to_owned())),
            starter: Some(self.workspaces.host_device()),
            came_up_ms: None,
        };
        on_runtime(takes_turn(&self.workspaces, workspace, &turn))
            .map(|claim| Box::new(Taken { claim }) as Box<dyn Renewal>)
            .map_err(|not_taken| format!("this device does not run the check: {not_taken}"))
    }
}

/// One check a caller asked this device to run.
pub(crate) struct Ask<'a> {
    pub app: &'a str,
    pub workspace: &'a str,
    pub record: &'a str,
    pub resource: &'a str,
    pub identity: &'a ResourceSemanticIdentity,
    pub check_id: &'a str,
    /// The invocation the check answers, which names its record.
    pub invocation: &'a str,
}

/// The device's checks, as its dispatch asks for them: the approved definitions it read at
/// startup, the device's turn of work, and the jobs it keeps.
pub(crate) struct NativeChecks {
    registry: Arc<Registry>,
    placement: Arc<dyn Placement>,
    jobs: Arc<CheckJobs>,
}

impl NativeChecks {
    pub(crate) fn new(
        workspaces: &Arc<super::workspaces::ServiceWorkspaces>,
        jobs: Arc<CheckJobs>,
    ) -> Self {
        Self {
            registry: installed(),
            placement: Arc::new(WorkspaceTurns {
                workspaces: workspaces.copies().clone(),
            }),
            jobs,
        }
    }

    /// The service over the definitions `registry` names and `placement` places, for a test.
    #[cfg(test)]
    fn over(registry: Arc<Registry>, placement: Arc<dyn Placement>, jobs: Arc<CheckJobs>) -> Self {
        Self {
            registry,
            placement,
            jobs,
        }
    }

    /// Run the check `ask` names: refused when no approved project matches it exactly, then
    /// placed, run and kept as this device's job.
    ///
    /// # Errors
    /// The check is not approved for this project, this device does not take its turn, or the
    /// check did not run to an observed end.
    fn run_ask(
        &self,
        ask: &Ask<'_>,
        recheck: &(dyn Fn() -> Result<(), String> + Sync),
        told: &dyn Fn(ReportObservation),
    ) -> Result<Receipt, String> {
        recheck()?;
        let registered = self
            .registry
            .find(ask.app, ask.workspace, ask.record, ask.identity)
            .ok_or("no approved checks for this exact project resource")?;
        let taken = self.placement.take(
            ask.app,
            ask.workspace,
            ask.record,
            ask.check_id,
            ask.invocation,
        )?;
        self.jobs.running(ask)?;
        let authority = || {
            recheck()?;
            taken.renew()
        };
        let report = |observation: ReportObservation| {
            self.jobs.progress(ask.invocation, &observation);
            told(observation);
        };
        let observed = registered.checks.run(ask.check_id, &authority, &report);
        match observed {
            Ok(observed) => match receipt(ask, registered, observed) {
                Ok(receipt) => {
                    self.jobs.done(ask.invocation, &receipt);
                    Ok(receipt)
                }
                Err(error) => {
                    self.jobs.failed(ask.invocation, &error);
                    Err(error)
                }
            },
            Err(error) => {
                self.jobs.failed(ask.invocation, &error);
                Err(error)
            }
        }
    }
}

impl ApprovedCheckServices for NativeChecks {
    fn run(&self, context: &ApprovedCheckContext) -> Result<Receipt, String> {
        let identity = context.resource_identity()?.clone();
        let invocation = context.invocation();
        let ask = Ask {
            app: context.app_id(),
            workspace: context.workspace(),
            record: context.record(),
            resource: context.resource(),
            identity: &identity,
            check_id: context.check_id(),
            invocation: &invocation,
        };
        self.run_ask(&ask, &|| context.recheck(), &|observation| {
            let _ = context.report(observation);
        })
    }
}

/// What the host observed, as the portable receipt the caller reads.
fn receipt(
    ask: &Ask<'_>,
    registered: &Registered,
    observed: CheckReceipt,
) -> Result<Receipt, String> {
    let artifact_value = serde_json::json!({
        "resource_identity": { "kind": registered.identity.kind, "key": registered.identity.key },
        "accepted_opaque_resource": ask.resource,
        "observed": observed,
    });
    let bytes = serde_json::to_vec(&artifact_value).map_err(|error| error.to_string())?;
    let artifact_sha256 = hex::encode(Sha256::digest(&bytes));
    let path = registered
        .artifacts
        .join(format!("{}-{artifact_sha256}.json", ask.invocation));
    let mut artifact =
        tilo_files::native::private::create_new(&path).map_err(|error| error.to_string())?;
    std::io::Write::write_all(&mut artifact, &bytes)
        .and_then(|()| std::io::Write::flush(&mut artifact))
        .map_err(|error| error.to_string())?;
    drop(artifact);
    tilo_files::native::atomic::sync_directory(&registered.artifacts)
        .map_err(|error| error.to_string())?;
    let receipt = Receipt {
        version: 1,
        invocation: ask.invocation.to_owned(),
        check_id: ask.check_id.to_owned(),
        binding: Binding {
            app_id: registered.app_id.clone(),
            workspace: registered.binding.workspace.to_string(),
            host: registered.binding.host,
            record: registered.binding.record.clone(),
            resource: ask.resource.to_owned(),
        },
        artifact_sha256,
        config_sha256: observed.config_sha256,
        runtime_binary_sha256: observed.runtime_binary_sha256,
        supervisor_source_sha256: observed.supervisor_source_sha256,
        outcome: match observed.outcome {
            CheckOutcome::Exited => Outcome::Exited,
            CheckOutcome::Cancelled => Outcome::Cancelled,
            CheckOutcome::TimedOut => Outcome::TimedOut,
            CheckOutcome::OutputLimit => Outcome::OutputLimit,
            CheckOutcome::Failed => Outcome::Failed,
        },
        exit_code: observed.exit_code,
        signal: observed.check_signal,
        elapsed_ms: u64::try_from(observed.elapsed_ms).map_err(|error| error.to_string())?,
        cleanup_observed: observed.cleanup_observed,
        stdout: diagnostic(observed.stdout.as_ref(), observed.stdout_sha256),
        stderr: diagnostic(observed.stderr.as_ref(), observed.stderr_sha256),
        failure: observed.failure.as_ref().map(|reason| {
            diagnostic(
                Some(reason),
                Some(hex::encode(Sha256::digest(reason.as_bytes()))),
            )
        }),
    };
    receipt.encode()?;
    Ok(receipt)
}

fn diagnostic(text: Option<&String>, sha256: Option<String>) -> Diagnostic {
    let text = text.map_or("", String::as_str);
    let mut end = text.len().min(MAX_PREVIEW_BYTES);
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    Diagnostic {
        sha256,
        preview: text.get(..end).map_or_else(String::new, str::to_owned),
        truncated: end != text.len(),
    }
}

/// How long a job's record is kept after it ends before the oldest are forgotten.
const JOBS_KEPT: usize = 256;

/// The jobs this device runs, kept at `devices/<device>/jobs/<job>` and read through the one
/// record API. A job moves from `running` to `done`, `failed` or `cancelled`; each phase it
/// reports is kept under `progress` and its end under `done`.
pub(crate) struct CheckJobs {
    device: [u8; 32],
    space: String,
    dir: PathBuf,
    records: std::sync::Mutex<BTreeMap<String, serde_json::Value>>,
    revision: tokio::sync::watch::Sender<u64>,
}

impl CheckJobs {
    /// The jobs kept below the login's directory `place`, by the device whose key is `device`.
    pub(crate) fn at(place: &Path, device: [u8; 32]) -> Self {
        Self {
            device,
            space: tilo_actions::remote::device_record_space(&device),
            dir: place.join("records").join(JOBS),
            records: std::sync::Mutex::default(),
            revision: tokio::sync::watch::Sender::new(0),
        }
    }

    /// The job `job` as it stands now.
    #[must_use]
    pub(crate) fn get(&self, job: &str) -> Option<serde_json::Value> {
        self.records.lock_recover().get(job).cloned()
    }

    /// Keep the job `job` as `value`, and wake every reader following it.
    fn set(&self, job: &str, value: serde_json::Value) -> Result<(), String> {
        if !valid_job(job) {
            return Err(format!("`{job}` is no job name"));
        }
        let bytes = serde_json::to_vec(&value).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(&self.dir).map_err(|error| error.to_string())?;
        let path = self.dir.join(format!("{job}.json"));
        tilo_files::native::atomic::write_with_mode(&path, &bytes, 0o600)
            .map_err(|error| format!("{}: {error}", path.display()))?;
        let mut records = self.records.lock_recover();
        records.insert(job.to_owned(), value);
        while records.len() > JOBS_KEPT {
            let ended = records
                .iter()
                .find(|(_, record)| record["state"] != "running")
                .map(|(id, _)| id.clone());
            match ended {
                Some(ended) => {
                    records.remove(&ended);
                }
                None => break,
            }
        }
        drop(records);
        self.revision
            .send_modify(|revision| *revision = revision.wrapping_add(1));
        Ok(())
    }

    fn running(&self, ask: &Ask<'_>) -> Result<(), String> {
        self.set(
            ask.invocation,
            serde_json::json!({
                "id": ask.invocation,
                "app": ask.app,
                "workspace": ask.workspace,
                "record": ask.record,
                "resource": ask.resource,
                "check_id": ask.check_id,
                "state": "running",
                "progress": serde_json::Value::Null,
                "done": serde_json::Value::Null,
            }),
        )
    }

    fn progress(&self, job: &str, observation: &ReportObservation) {
        let progress = match observation {
            ReportObservation::Reported {
                phase,
                completed,
                total,
            } => serde_json::json!({
                "phase": phase,
                "completed": completed,
                "total": total,
            }),
            ReportObservation::Disabled { reason } => {
                serde_json::json!({ "disabled": format!("{reason:?}") })
            }
        };
        if let Some(mut record) = self.get(job) {
            record["progress"] = progress;
            if let Err(error) = self.set(job, record) {
                tracing::warn!(%error, job, "a check's progress was not kept");
            }
        }
    }

    fn done(&self, job: &str, receipt: &Receipt) {
        let state = if receipt.outcome == Outcome::Cancelled {
            "cancelled"
        } else {
            "done"
        };
        self.end(
            job,
            state,
            &serde_json::to_value(receipt).unwrap_or_default(),
        );
    }

    fn failed(&self, job: &str, why: &str) {
        self.end(job, "failed", &serde_json::json!({ "error": why }));
    }

    fn end(&self, job: &str, state: &str, done: &serde_json::Value) {
        if let Some(mut record) = self.get(job) {
            record["state"] = state.into();
            record["done"] = done.clone();
            if let Err(error) = self.set(job, record) {
                tracing::warn!(%error, job, "a check's end was not kept");
            }
        }
    }

    /// Whether the device's job `job` is `caller`'s to take `kind` at in `workspace`.
    fn allows(
        &self,
        caller: &Caller,
        workspace: &str,
        job: &str,
        kind: tilo_action_policy::ActionKind,
    ) -> Result<(), tilo_actions::remote::ActionCallError> {
        let refused = || tilo_actions::remote::ActionCallError::Refused {
            reason: JOBS_REFUSED.into(),
        };
        if caller.is_system() {
            return Ok(());
        }
        if workspace == self.space {
            if caller.device_public_key() == Some(&self.device) {
                return Ok(());
            }
            return if caller.acting().app().is_some() && caller.user_public_key().is_none() {
                Ok(())
            } else {
                Err(refused())
            };
        }
        let _ = (caller, job, kind);
        Err(refused())
    }

    fn names(&self, address: &ResourceAddress) -> bool {
        address.app == JOBS && address.workspace == self.space
    }
}

/// What a caller that may not read a device's job is told.
const JOBS_REFUSED: &str =
    "Only this device's person, from any of their devices, reads the jobs it runs";

/// Whether `job` is one job's name: letters, digits, `.`, `_` and `-`, never leaving its
/// directory.
fn valid_job(job: &str) -> bool {
    !job.is_empty()
        && job.len() <= 128
        && job
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        && job
            .bytes()
            .next()
            .is_some_and(|first| first.is_ascii_alphanumeric())
}

impl ResourceReads for CheckJobs {
    fn read(
        &self,
        caller: &Caller,
        address: &ResourceAddress,
    ) -> Result<ResourceCopy, tilo_actions::remote::ActionCallError> {
        self.allows(
            caller,
            &address.workspace,
            &address.record,
            tilo_action_policy::ActionKind::Read,
        )?;
        if !self.names(address) {
            return Ok(ResourceCopy::Absent);
        }
        let revision = self.revision.borrow().to_string();
        Ok(match self.get(&address.record) {
            Some(value) => ResourceCopy::reading(value, Some(revision)),
            None => ResourceCopy::Absent,
        })
    }

    fn answer_read(
        self: Arc<Self>,
        caller: &Caller,
        correlation: u64,
        read: ResourceReadRequest,
    ) -> tilo_actions::remote::Pending<tilo_actions::remote::RemoteActionMessage> {
        tilo_actions::remote::answer_read(self, caller, correlation, read)
    }

    fn list(
        &self,
        caller: &Caller,
        workspace: &str,
        app: &str,
    ) -> Result<Vec<ResourceAddress>, tilo_actions::remote::ActionCallError> {
        if app != JOBS || workspace != self.space {
            return Err(tilo_actions::remote::ActionCallError::Unavailable {
                reason: tilo_actions::remote::RESOURCE_READS_UNAVAILABLE.into(),
            });
        }
        let names: Vec<String> = self.records.lock_recover().keys().cloned().collect();
        Ok(names
            .into_iter()
            .filter(|job| {
                self.allows(caller, workspace, job, tilo_action_policy::ActionKind::Read)
                    .is_ok()
            })
            .map(|job| ResourceAddress {
                workspace: workspace.to_owned(),
                app: JOBS.to_owned(),
                record: job,
            })
            .collect())
    }

    fn watch(
        &self,
        caller: &Caller,
        address: &ResourceAddress,
        wake: &tilo_actions::remote::ResourceWake,
    ) -> Result<(), tilo_actions::remote::ActionCallError> {
        self.allows(
            caller,
            &address.workspace,
            &address.record,
            tilo_action_policy::ActionKind::Read,
        )?;
        if !self.names(address) {
            return Ok(());
        }
        let mut seen = self.revision.subscribe();
        let wake = wake.clone();
        tilo_rt::app().spawn(async move {
            if seen.changed().await.is_ok() {
                wake.wake();
            }
        });
        Ok(())
    }
}

#[cfg(test)]
#[path = "approved_checks_tests.rs"]
mod tests;
