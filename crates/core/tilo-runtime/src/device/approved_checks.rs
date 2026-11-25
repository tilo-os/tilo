//! The checks a project's host approved, run as this device's work.
//!
//! An app asks for a check by its id only, as the work of its `run-approved-check` invocation the
//! dispatch delivered to it: the device's [`checks::RUN`] runs only as such work, and reads the
//! app, record and resource the check is for from that invocation, never from the call. What
//! runs, and the device's own paths it runs on, come from the device's setting
//! `devices/<device>/settings/checks`: one entry per approved project, written and removed through
//! the settings actions by the device's person or whoever they granted there. The daemon reads it
//! again for every check, so a check, a project, a record or a resource identity the setting does
//! not approve is refused, and so is the next run after the entry is removed.
//!
//! The check is work of this device, like a turn an offloaded action takes: [`takes_turn`]
//! decides whether it runs here, the device keeps the job at `devices/<device>/jobs/<job>` while
//! it runs, and each phase it reports and its end are written there. The record is the one a view
//! follows through the record API; the caller that asked follows the same work as its invocation's
//! progress and answer.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use runtime_collab::approved_checks::{
    self as checks, Binding, Diagnostic, MAX_PREVIEW_BYTES, Outcome, Receipt, ReportObservation,
};
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use tilo_actions::api::{ActionAddress, ActionDescriptor, InputSpec, PlacedAction};
use tilo_actions::contract::{ActionContract, ActionKind};
use tilo_actions::params::{ActionParam, ParamType};
use tilo_actions::remote::{
    Admits, Admitted, Caller, InvokeArgument, InvokeRequest, Pending, ReadsAnswer, ResourceAddress,
    ResourceCopy, ResourceReads, Serving, reads_answer,
};
use tilo_actions::{
    ActionEffect, ActionFailure, ActionOutput, ExecutionLocus, ExecutionMode, OperationKind,
    ResourceSemanticIdentity,
};
use tilo_lock::MutexRecover as _;
use tilo_workspace_service::work::{Actor, Claim, Turn, takes_turn};
use ush::approved_checks::{
    ApprovedCheck, ApprovedChecks, CheckOutcome, CheckReceipt, ProjectBinding,
};

/// The app a device's jobs are read under: the record `devices/<device>/jobs/<job>`.
pub(crate) const JOBS: &str = "jobs";

/// The device setting that approves checks: `devices/<device>/settings/checks`, one entry per
/// project. The person sets and removes it through the settings actions; the daemon reads it
/// again for every check, so a removal refuses the next run.
pub(crate) const CHECKS: &str = "checks";

/// The most projects one device may approve.
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
    workspace: String,
    record: String,
    resource_identity: ResourceIdentity,
    checks: Vec<CheckConfig>,
}

#[derive(Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ResourceIdentity {
    kind: String,
    key: String,
}

/// One approved check, as the setting names it: what runs, and the device's own paths it runs
/// on. Its binding is the project's, so the setting names it once.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckConfig {
    id: String,
    checkout: PathBuf,
    build: PathBuf,
    scratch: PathBuf,
    runtime_binary: PathBuf,
    images: Vec<ush::approved_checks::CheckImage>,
    command: Vec<String>,
    environment: Vec<(String, String)>,
    limits: ush::approved_checks::CheckLimits,
    #[serde(default)]
    reported_phases: Vec<ush::approved_checks::ReportedCheckPhase>,
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
                report(observation(event));
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

/// What approves a check: the device's settings, read again for each request.
trait Approvals: Send + Sync {
    /// The approved project exactly `app`, `workspace` and `record` name, whose resource
    /// `identity` names too, or `None` when the setting approves none for it.
    ///
    /// # Errors
    /// The setting does not read, or an entry does not describe a check this device can run.
    fn find(
        &self,
        app: &str,
        workspace: &str,
        record: &str,
        identity: &ResourceSemanticIdentity,
    ) -> Result<Option<Arc<Registered>>, String>;
}

/// The approvals kept in `devices/<device>/settings/checks`, as the setting actions write them.
struct SettingsApprovals {
    records: Arc<super::records::DeviceRecords>,
    device: [u8; 32],
    /// The device's profile, where each project's private receipts are kept.
    root: PathBuf,
}

impl SettingsApprovals {
    fn project(&self, project: ProjectConfig) -> Result<Registered, String> {
        let identity = ResourceSemanticIdentity::new(
            project.resource_identity.kind.clone(),
            project.resource_identity.key.clone(),
        )
        .ok_or("approved project semantic identity invalid")?;
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
        // An app's records are kept in its workspace's document of the app, whose route names
        // no record; the one spelling of it is the alias every surface uses.
        let record = tilo_broker_core::published::canonical_document(&project.record)
            .map_err(|why| format!("approved project record is no record id: {why}"))?;
        if record.len() > 1024 {
            return Err("approved project identity exceeds portable receipt vocabulary".into());
        }
        let workspace = uuid::Uuid::parse_str(&project.workspace)
            .map_err(|_| "approved project workspace is not a workspace id".to_owned())?;
        // The receipts are the device's own, kept in a private directory the setting never
        // names and the checked source or its build output never contains.
        let receipts = blake3::hash(
            format!(
                "{}/{}/{}/{}",
                project.workspace, record, identity.kind, identity.key
            )
            .as_bytes(),
        )
        .to_hex();
        let artifacts = self
            .root
            .join("state/approved-checks")
            .join(receipts.as_str());
        for directory in [
            self.root.join("state"),
            self.root.join("state/approved-checks"),
            artifacts.clone(),
        ] {
            tilo_files::native::private::directory(&directory).map_err(|error| {
                format!(
                    "the device keeps no private place for a check's receipts, {}: {error}",
                    directory.display()
                )
            })?;
        }
        if project.checks.iter().any(|check| {
            artifacts.starts_with(&check.checkout)
                || artifacts.starts_with(&check.build)
                || check.checkout.starts_with(&artifacts)
                || check.build.starts_with(&artifacts)
        }) {
            return Err("approved private receipts and the checked source are one tree".into());
        }
        let binding = ProjectBinding {
            workspace,
            host: self.device,
            record,
            resource: identity.key.clone(),
        };
        let checks = project
            .checks
            .into_iter()
            .map(|check| ApprovedCheck {
                id: check.id,
                binding: binding.clone(),
                checkout: check.checkout,
                build: check.build,
                scratch: check.scratch,
                runtime_binary: check.runtime_binary,
                images: check.images,
                command: check.command,
                environment: check.environment,
                limits: check.limits,
                reported_phases: check.reported_phases,
            })
            .collect();
        let checks = ApprovedChecks::new(binding.clone(), checks)?;
        Ok(Registered {
            app_id: project.app_id,
            identity,
            binding: binding.clone(),
            artifacts,
            checks: Box::new(ProjectChecks { binding, checks }),
        })
    }
}

impl Approvals for SettingsApprovals {
    fn find(
        &self,
        app: &str,
        workspace: &str,
        record: &str,
        identity: &ResourceSemanticIdentity,
    ) -> Result<Option<Arc<Registered>>, String> {
        // An app's records are kept in its workspace's document of it, whose route names no
        // record; the setting and the route meet in the one spelling of it.
        let record = tilo_broker_core::published::canonical_document(record)
            .unwrap_or_else(|_| record.to_owned());
        let Some(setting) = self.records.get(CHECKS) else {
            return Ok(None);
        };
        let config: Config = serde_json::from_value(setting)
            .map_err(|error| format!("the approved checks setting does not read: {error}"))?;
        if config.version != 1 || config.projects.len() > MAX_PROJECTS {
            return Err("unsupported or oversized approved checks setting".into());
        }
        for project in config.projects {
            let registered = self.project(project)?;
            if registered.matches(app, workspace, &record, identity) {
                return Ok(Some(Arc::new(registered)));
            }
        }
        Ok(None)
    }
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

/// One check an app asked this device to run.
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

/// The device's checks, as its dispatch asks for them: the approval setting it reads again for
/// every check, the device's turn of work, and the jobs it keeps.
#[derive(Clone)]
pub(crate) struct NativeChecks {
    approvals: Arc<dyn Approvals>,
    placement: Arc<dyn Placement>,
    jobs: Arc<CheckJobs>,
}

impl NativeChecks {
    pub(crate) fn new(
        workspaces: &Arc<super::workspaces::ServiceWorkspaces>,
        records: Arc<super::records::DeviceRecords>,
        root: PathBuf,
        jobs: Arc<CheckJobs>,
    ) -> Self {
        let device = workspaces.copies().host_device();
        Self {
            approvals: Arc::new(SettingsApprovals {
                records,
                device,
                root,
            }),
            placement: Arc::new(WorkspaceTurns {
                workspaces: workspaces.copies().clone(),
            }),
            jobs,
        }
    }

    /// The service over `approvals` and `placement`, for a test.
    #[cfg(test)]
    fn over(
        approvals: Arc<dyn Approvals>,
        placement: Arc<dyn Placement>,
        jobs: Arc<CheckJobs>,
    ) -> Self {
        Self {
            approvals,
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
            .approvals
            .find(ask.app, ask.workspace, ask.record, ask.identity)?
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
            Ok(observed) => match receipt(ask, &registered, observed) {
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

/// Why a call that is no delivered action's work is refused.
const NOT_THE_CHECK: &str = "An approved check runs only as the work of an action the workspace \
     dispatched to the app, for the check that action was asked to run";

/// The check `request` names, when it is the one `serving` was asked to run. The delivered action
/// must declare work its invoker can stop, an undoable background job, so a check the person
/// stops is one the action's contract lets them stop; what the action is called, and everything
/// else about it, is the effect it declares ([`Self::start`]).
fn asked_check<'a>(serving: &dyn Serving, request: &'a InvokeRequest) -> Result<&'a str, String> {
    let contract = serving.contract();
    if contract.kind != ActionKind::Undoable
        || contract.execution != (ExecutionMode::Background { cancellable: true })
    {
        return Err(NOT_THE_CHECK.to_owned());
    }
    let InvokeArgument::Json(arguments) = &request.argument else {
        return Err("an approved check is named by its id".to_owned());
    };
    let check_id = arguments
        .get(checks::ARGUMENT)
        .and_then(serde_json::Value::as_str)
        .filter(|check_id| checks::is_check_id(check_id))
        .ok_or("an approved check is named by its id, never by a command")?;
    let delivered = serving
        .argument()
        .and_then(|argument| argument.strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX))
        .and_then(|argument| {
            serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(argument).ok()
        });
    let asked = delivered
        .as_ref()
        .and_then(|arguments| arguments.get(checks::ARGUMENT))
        .and_then(serde_json::Value::as_str);
    if asked != Some(check_id) {
        return Err(NOT_THE_CHECK.to_owned());
    }
    Ok(check_id)
}

/// The action an approved check runs by.
fn check_rows() -> Vec<PlacedAction> {
    tilo_actions::remote::admitted::top_level(vec![ActionDescriptor {
        did: None,
        state: tilo_actions::ActionState::default(),
        semantic_identity: None,
        address: ActionAddress::new(checks::RESOURCE, checks::RUN),
        resource_kind: checks::PROVIDER.to_owned(),
        resource_label: "Approved checks".to_owned(),
        action_label: "Run an approved check".to_owned(),
        keywords: Vec::new(),
        icon: None,
        description: None,
        locus: ExecutionLocus::ClientLocal,
        kind: OperationKind::Effect,
        contract: ActionContract {
            kind: ActionKind::Undoable,
            execution: ExecutionMode::Background { cancellable: true },
            ..ActionContract::default()
        },
        input: InputSpec::None,
        params: vec![ActionParam::required(
            checks::ARGUMENT,
            ParamType::Text,
            "The check the project's host approved",
        )],
        view: None,
    }])
}

impl Admits for NativeChecks {
    const PROVIDER: &'static str = checks::PROVIDER;
    const APP: &'static str = tilo_app_id::apps::SETTINGS;

    fn rows(&self) -> Vec<PlacedAction> {
        check_rows()
    }

    fn start(
        &self,
        _caller: &Caller,
        request: &InvokeRequest,
        admitted: &Admitted,
    ) -> Result<Pending<tilo_actions::remote::Outcome>, String> {
        if request.target.address != ActionAddress::new(checks::RESOURCE, checks::RUN) {
            return Err(format!(
                "this device runs no `{}` on its approved checks",
                request.target.address.action
            ));
        }
        let serving = admitted.serving.clone().ok_or(NOT_THE_CHECK)?;
        // The action qualifies because its contract declares this device's check effect, never
        // because of its name or kind: what a caller reads of the action says it runs one here.
        let declared = ActionEffect::new(checks::PROVIDER, checks::RESOURCE, checks::RUN);
        if !serving.contract().effects.contains(&declared) {
            return Err(format!(
                "`{}` runs no approved check: its contract declares no {declared} effect",
                serving.address().action
            ));
        }
        let check_id = asked_check(&*serving, request)?.to_owned();
        serving.recheck()?;
        let identity = serving.resource_identity()?.clone();
        let checks = self.clone();
        let stop = Arc::clone(&admitted.stop);
        let progress = Arc::clone(&admitted.progress);
        let (end, outcome) = tilo_actions::remote::pending();
        std::thread::Builder::new()
            .name("approved-check".into())
            .spawn(move || {
                let invocation = serving.invocation().0.to_string();
                // The job names the record the approval does, in its one spelling.
                let record = tilo_broker_core::published::canonical_document(serving.record())
                    .unwrap_or_else(|_| serving.record().to_owned());
                let ask = Ask {
                    app: serving.app(),
                    workspace: serving.workspace(),
                    record: &record,
                    resource: &serving.address().resource,
                    identity: &identity,
                    check_id: &check_id,
                    invocation: &invocation,
                };
                let recheck = || {
                    if stop.load(std::sync::atomic::Ordering::Acquire) {
                        return Err("the check was stopped".to_owned());
                    }
                    serving.recheck()
                };
                let ran = checks.run_ask(&ask, &recheck, &|observation| {
                    if let ReportObservation::Reported {
                        phase,
                        completed,
                        total,
                    } = observation
                    {
                        progress(&phase, completed, total);
                    }
                });
                end.complete(
                    ran.and_then(|receipt| {
                        serde_json::to_value(&receipt)
                            .map(|value| ActionOutput {
                                value: Some(value),
                                ..ActionOutput::default()
                            })
                            .map_err(|error| format!("the check's receipt did not encode: {error}"))
                    })
                    .map_err(|message| {
                        Box::new(ActionFailure {
                            code: "action_failed".into(),
                            message,
                            details: None,
                            retryable: false,
                        })
                    }),
                );
            })
            .map_err(|error| format!("the check did not start: {error}"))?;
        Ok(outcome)
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
            let Some(fields) = record.as_object_mut() else {
                tracing::warn!(job, "a check's progress is not a record");
                return;
            };
            fields.insert("progress".to_owned(), progress);
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
            let Some(fields) = record.as_object_mut() else {
                tracing::warn!(job, "a check's end is not a record");
                return;
            };
            fields.insert("state".to_owned(), state.into());
            fields.insert("done".to_owned(), done.clone());
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
    fn read<'a>(
        &'a self,
        caller: &'a Caller,
        address: &'a ResourceAddress,
    ) -> ReadsAnswer<'a, ResourceCopy> {
        reads_answer(async move {
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
        })
    }

    fn list<'a>(
        &'a self,
        caller: &'a Caller,
        workspace: &'a str,
        app: &'a str,
    ) -> ReadsAnswer<'a, Vec<tilo_actions::remote::ListedResource>> {
        reads_answer(async move {
            if app != JOBS || workspace != self.space {
                return Err(tilo_actions::remote::ActionCallError::Unavailable {
                    reason: tilo_actions::remote::RESOURCE_READS_UNAVAILABLE.into(),
                });
            }
            let mut names: Vec<String> = self.records.lock_recover().keys().cloned().collect();
            names.retain(|job| {
                self.allows(caller, workspace, job, tilo_action_policy::ActionKind::Read)
                    .is_ok()
            });
            Ok(names
                .into_iter()
                .map(|job| {
                    tilo_actions::remote::ListedResource::document(ResourceAddress {
                        workspace: workspace.to_owned(),
                        app: JOBS.to_owned(),
                        record: job,
                    })
                })
                .collect())
        })
    }

    fn watch<'a>(
        &'a self,
        caller: &'a Caller,
        address: &'a ResourceAddress,
        wake: &'a tilo_actions::remote::ResourceWake,
    ) -> ReadsAnswer<'a, ()> {
        reads_answer(async move {
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
        })
    }
}

#[cfg(test)]
#[path = "approved_checks_tests.rs"]
mod tests;
