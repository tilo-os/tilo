//! The checks a device's setting approved: what it reads, what it refuses, and the job it
//! keeps while one runs.

use std::os::unix::fs::PermissionsExt as _;
use std::sync::{Arc, Mutex};

use tilo_action_services::approved_checks::ReportObservation;
use tilo_actions::ResourceSemanticIdentity;
use ush::approved_checks::{CheckLimits, CheckOutcome, CheckReceipt, ProjectBinding};

use super::*;

const APP: &str = "so.tilo.git-client";
const RECORD: &str = "project-one";
const RESOURCE: &str = "project";

fn identity() -> ResourceSemanticIdentity {
    ResourceSemanticIdentity::new("git_client.repo", RESOURCE).expect("a valid identity")
}

fn binding(workspace: &str) -> ProjectBinding {
    ProjectBinding {
        workspace: uuid::Uuid::parse_str(workspace).expect("a workspace id"),
        host: [7; 32],
        record: RECORD.to_owned(),
        resource: RESOURCE.to_owned(),
    }
}

fn limits() -> CheckLimits {
    CheckLimits {
        cpu_percent: 100,
        memory_bytes: 64 * 1024 * 1024,
        processes: 8,
        wall_seconds: 60,
        output_bytes: 4096,
    }
}

/// An approved project whose check reports one phase and observes a passing exit.
struct Passing {
    binding: ProjectBinding,
    check_id: String,
}

impl ApprovedProject for Passing {
    fn run(
        &self,
        check_id: &str,
        authority: &(dyn Fn() -> Result<(), String> + Sync),
        report: &dyn Fn(ReportObservation),
    ) -> Result<CheckReceipt, String> {
        if check_id != self.check_id {
            return Err("the approved id is the one that runs".to_owned());
        }
        authority()?;
        let observation = ReportObservation::Reported {
            phase: "tests".into(),
            completed: Some(1),
            total: Some(2),
        };
        report(observation);
        authority()?;
        Ok(CheckReceipt {
            binding: self.binding.clone(),
            check_id: check_id.to_owned(),
            unit: "unit".into(),
            config_sha256: "a".repeat(64),
            runtime_binary_sha256: "b".repeat(64),
            supervisor_source_sha256: "c".repeat(64),
            limits: limits(),
            cgroup: None,
            outcome: CheckOutcome::Exited,
            exit_code: Some(0),
            check_signal: None,
            wrapper_exit_code: None,
            service_result: None,
            service_state_before_cleanup: None,
            stdout: Some("ok\n".into()),
            stderr: None,
            stdout_sha256: Some("d".repeat(64)),
            stderr_sha256: None,
            elapsed_ms: 12,
            failure: None,
            cleanup_observed: true,
        })
    }
}

fn registered(workspace: &str, artifacts: &std::path::Path) -> Registered {
    Registered {
        app_id: APP.to_owned(),
        identity: identity(),
        binding: binding(workspace),
        artifacts: artifacts.to_path_buf(),
        checks: Box::new(Passing {
            binding: binding(workspace),
            check_id: "unit".into(),
        }),
    }
}

/// The approvals a test gives: one project, or none once it is revoked.
struct Given {
    found: Mutex<Option<Arc<Registered>>>,
}

impl Approvals for Given {
    fn find(
        &self,
        _app: &str,
        _workspace: &str,
        _record: &str,
        _identity: &ResourceSemanticIdentity,
    ) -> Result<Option<Arc<Registered>>, String> {
        Ok(self.found.lock().unwrap().clone())
    }
}

fn given(project: Option<Arc<Registered>>) -> Arc<Given> {
    Arc::new(Given {
        found: Mutex::new(project),
    })
}

/// Never losing the turn: what a placement that took it answers.
struct Kept;
impl Renewal for Kept {
    fn renew(&self) -> Result<(), String> {
        Ok(())
    }
}

/// A placement that takes every turn and says where it was taken.
struct Placed {
    taken: Mutex<Vec<(String, String, String, String)>>,
    refusal: Option<String>,
}

impl Placement for Placed {
    fn take(
        &self,
        app: &str,
        workspace: &str,
        record: &str,
        check_id: &str,
        invocation: &str,
    ) -> Result<Box<dyn Renewal>, String> {
        if let Some(refusal) = &self.refusal {
            return Err(refusal.clone());
        }
        self.taken.lock().unwrap().push((
            app.to_owned(),
            workspace.to_owned(),
            record.to_owned(),
            check_id.to_owned(),
        ));
        if invocation.is_empty() {
            return Err("a placed check names its invocation".to_owned());
        }
        Ok(Box::new(Kept))
    }
}

fn jobs() -> Arc<CheckJobs> {
    Arc::new(CheckJobs::at(
        &tempfile::tempdir().expect("a profile").keep(),
        [7; 32],
    ))
}

fn ask<'a>(workspace: &'a str, identity: &'a ResourceSemanticIdentity) -> Ask<'a> {
    Ask {
        app: APP,
        workspace,
        record: RECORD,
        resource: RESOURCE,
        identity,
        check_id: "unit",
        invocation: "0f8fad5b-d9cb-469f-a165-70867728950e",
    }
}

#[test]
fn an_approved_check_is_placed_kept_and_answered() {
    let workspace = uuid::Uuid::new_v4().to_string();
    let jobs = jobs();
    let artifacts = tempfile::tempdir().expect("a private artifacts directory");
    let placement = Arc::new(Placed {
        taken: Mutex::default(),
        refusal: None,
    });
    let checks = NativeChecks::over(
        given(Some(Arc::new(registered(&workspace, artifacts.path())))),
        Arc::clone(&placement) as Arc<dyn Placement>,
        Arc::clone(&jobs),
    );
    let identity = identity();
    let heard = Mutex::new(Vec::new());
    let receipt = checks
        .run_ask(&ask(&workspace, &identity), &|| Ok(()), &|observation| {
            heard.lock().unwrap().push(observation);
        })
        .expect("the approved check ran");

    assert_eq!(receipt.invocation, ask(&workspace, &identity).invocation);
    assert_eq!(receipt.check_id, "unit");
    assert_eq!(receipt.binding.app_id, APP);
    assert_eq!(receipt.binding.record, RECORD);
    assert_eq!(
        receipt.outcome,
        tilo_action_services::approved_checks::Outcome::Exited
    );
    assert_eq!(
        heard.lock().unwrap().len(),
        1,
        "the phase reached the caller that asked"
    );
    let placed = placement.taken.lock().unwrap();
    assert_eq!(
        placed.as_slice(),
        &[(
            APP.to_owned(),
            workspace,
            RECORD.to_owned(),
            "unit".to_owned()
        )][..]
    );
    drop(placed);

    let job = jobs
        .get("0f8fad5b-d9cb-469f-a165-70867728950e")
        .expect("the device keeps the job it ran");
    assert_eq!(job["state"], "done");
    assert_eq!(job["check_id"], "unit");
    assert_eq!(job["progress"]["phase"], "tests");
    assert_eq!(job["progress"]["completed"], 1);
    assert_eq!(job["done"]["outcome"], "exited");
}

#[test]
fn a_check_no_project_approved_is_refused_and_names_why() {
    let workspace = uuid::Uuid::new_v4().to_string();
    let jobs = jobs();
    let checks = NativeChecks::over(
        given(None),
        Arc::new(Placed {
            taken: Mutex::default(),
            refusal: None,
        }) as Arc<dyn Placement>,
        Arc::clone(&jobs),
    );
    let identity = identity();
    let error = checks
        .run_ask(&ask(&workspace, &identity), &|| Ok(()), &|_| {})
        .expect_err("an unapproved check never runs");
    assert_eq!(error, "no approved checks for this exact project resource");
    assert!(
        jobs.get("0f8fad5b-d9cb-469f-a165-70867728950e").is_none(),
        "no job is kept for a check that never started"
    );
}

#[test]
fn a_check_this_device_does_not_take_runs_nowhere() {
    let workspace = uuid::Uuid::new_v4().to_string();
    let artifacts = tempfile::tempdir().expect("a private artifacts directory");
    let checks = NativeChecks::over(
        given(Some(Arc::new(registered(&workspace, artifacts.path())))),
        Arc::new(Placed {
            taken: Mutex::default(),
            refusal: Some(
                "this device does not run the check: waiting for the device that started it".into(),
            ),
        }) as Arc<dyn Placement>,
        jobs(),
    );
    let identity = identity();
    let error = checks
        .run_ask(&ask(&workspace, &identity), &|| Ok(()), &|_| {})
        .expect_err("a turn another device takes does not run here");
    assert!(error.contains("does not run the check"), "{error}");
}

/// A temporary device directory with every path an approved check names.
fn device_directory() -> tempfile::TempDir {
    let root = tempfile::tempdir().expect("a device directory");
    for name in ["source", "source/target", "build", "image"] {
        std::fs::create_dir(root.path().join(name)).expect("a directory");
    }
    let scratch = root.path().join("scratch");
    std::fs::create_dir(&scratch).expect("a private directory");
    std::fs::set_permissions(&scratch, std::fs::Permissions::from_mode(0o700))
        .expect("a private mode");
    std::fs::write(root.path().join("runtime"), b"runtime").expect("a runtime file");
    root
}

/// The `checks` setting approving one project at `root`, for `workspace`.
fn approved_setting(root: &std::path::Path, workspace: &str, record: &str) -> serde_json::Value {
    serde_json::json!({
        "version": 1,
        "projects": [{
            "app_id": APP,
            "workspace": workspace,
            "record": record,
            "resource_identity": { "kind": "git_client.repo", "key": RESOURCE },
            "checks": [{
                "id": "unit",
                "checkout": root.join("source"),
                "build": root.join("build"),
                "scratch": root.join("scratch"),
                "runtime_binary": root.join("runtime"),
                "images": [{ "identity": format!("sha256:{}", "a".repeat(64)), "root": root.join("image") }],
                "command": ["/usr/bin/true"],
                "environment": [],
                "limits": serde_json::to_value(limits()).expect("limits read"),
            }],
        }],
    })
}

fn settings_approvals(
    root: &std::path::Path,
    device: [u8; 32],
) -> (Arc<super::super::records::DeviceRecords>, SettingsApprovals) {
    let records = Arc::new(super::super::records::DeviceRecords::under(root, device));
    let approvals = SettingsApprovals {
        records: Arc::clone(&records),
        device,
        root: root.to_path_buf(),
    };
    (records, approvals)
}

#[test]
fn the_settings_record_approves_a_project_and_its_removal_refuses_the_next_check() {
    let directory = device_directory();
    let workspace = uuid::Uuid::new_v4().to_string();
    let device = [7; 32];
    let (records, approvals) = settings_approvals(directory.path(), device);
    records
        .set(
            CHECKS,
            approved_setting(directory.path(), &workspace, RECORD),
        )
        .expect("the person approves a project");

    let identity = identity();
    let project = approvals
        .find(APP, &workspace, RECORD, &identity)
        .expect("the setting reads")
        .expect("the approved project is found");
    assert_eq!(project.app_id, APP);
    assert_eq!(project.binding.record, RECORD);
    assert!(
        approvals
            .find(APP, &workspace, "another-project", &identity)
            .expect("the setting reads")
            .is_none(),
        "approval binds one record"
    );

    assert!(
        records.remove(CHECKS).expect("the person revokes it"),
        "the approval was there"
    );
    assert!(
        approvals
            .find(APP, &workspace, RECORD, &identity)
            .expect("an absent setting reads as no approval")
            .is_none(),
        "a revoked approval refuses the next check"
    );
}

#[test]
fn a_setting_for_an_apps_workspace_document_names_it_by_its_one_spelling() {
    let directory = device_directory();
    let workspace = uuid::Uuid::new_v4().to_string();
    let device = [7; 32];
    let (records, approvals) = settings_approvals(directory.path(), device);
    // An app whose records live in the workspace's document of it joins the route with no
    // record id; the approval and the route meet in the alias every surface carries.
    records
        .set(CHECKS, approved_setting(directory.path(), &workspace, ""))
        .expect("the person approves the app's workspace document");
    let identity = identity();
    for record in ["", "@tilo:package-primary"] {
        let project = approvals
            .find(APP, &workspace, record, &identity)
            .expect("the setting reads")
            .expect("the approved project is found");
        assert_eq!(project.binding.record, "@tilo:package-primary");
    }
}

#[test]
fn a_setting_that_does_not_read_is_refused_legibly() {
    let directory = device_directory();
    let device = [7; 32];
    let (records, approvals) = settings_approvals(directory.path(), device);
    records
        .set(CHECKS, serde_json::json!({ "version": 1 }))
        .expect("a setting is written");
    let identity = identity();
    let error = approvals
        .find(APP, &uuid::Uuid::new_v4().to_string(), RECORD, &identity)
        .err()
        .expect("a setting with no projects does not read");
    assert!(
        error.contains("does not read"),
        "the refusal says what is wrong: {error}"
    );
}

#[test]
fn an_entry_naming_a_path_this_device_lacks_is_refused() {
    let directory = device_directory();
    let workspace = uuid::Uuid::new_v4().to_string();
    let device = [7; 32];
    let (records, approvals) = settings_approvals(directory.path(), device);
    let mut setting = approved_setting(directory.path(), &workspace, RECORD);
    setting["projects"][0]["checks"][0]["checkout"] =
        serde_json::json!(directory.path().join("missing"));
    records.set(CHECKS, setting).expect("a setting is written");
    let identity = identity();
    let error = approvals
        .find(APP, &workspace, RECORD, &identity)
        .err()
        .expect("a check whose source is not here cannot run");
    assert!(!error.is_empty(), "the refusal says why");
}

#[test]
fn only_the_devices_person_writes_the_checks_setting() {
    let directory = device_directory();
    let device = [7; 32];
    let person = [9; 32];
    let records = super::super::records::DeviceRecords::under(directory.path(), device);
    records.carry_into(&person);
    let args = |value: serde_json::Value| {
        serde_json::json!({ "name": CHECKS, "value": value })
            .as_object()
            .cloned()
            .expect("the setting action's arguments")
    };
    let setting = serde_json::json!({ "version": 1, "projects": [] });
    let mine = Caller::authenticated(person, device);
    records
        .act(&mine, super::super::records::SET, &args(setting.clone()))
        .expect("the device's person approves a setting");
    let member = Caller::authenticated([8; 32], [6; 32]);
    assert!(
        records
            .act(&member, super::super::records::SET, &args(setting))
            .is_err(),
        "a member the person granted nothing may not set the checks"
    );
}

/// The invocation an app was delivered, as the dispatch hands it to the device's checks.
struct Delivered {
    workspace: String,
    address: tilo_actions::ActionAddress,
    argument: Option<String>,
    contract: ActionContract,
    identity: ResourceSemanticIdentity,
    kind: OperationKind,
}

impl Serving for Delivered {
    fn invocation(&self) -> tilo_actions::InvocationId {
        tilo_actions::InvocationId(
            uuid::Uuid::parse_str("0f8fad5b-d9cb-469f-a165-70867728950e").unwrap(),
        )
    }
    fn app(&self) -> &str {
        APP
    }
    fn workspace(&self) -> &str {
        &self.workspace
    }
    fn record(&self) -> &str {
        RECORD
    }
    fn address(&self) -> &tilo_actions::ActionAddress {
        &self.address
    }
    fn kind(&self) -> OperationKind {
        self.kind
    }
    fn contract(&self) -> &ActionContract {
        &self.contract
    }
    fn argument(&self) -> Option<&str> {
        self.argument.as_deref()
    }
    fn resource_identity(&self) -> Result<&ResourceSemanticIdentity, String> {
        Ok(&self.identity)
    }
    fn recheck(&self) -> Result<(), String> {
        Ok(())
    }
}

/// The app's `run-approved-check` invocation of `asked`, as delivered.
fn delivered(workspace: &str, action: &str, asked: &str) -> Delivered {
    delivered_kind(workspace, action, asked, OperationKind::Effect)
}

/// The same delivery of an action of `kind`, declaring the device's check effect.
fn delivered_kind(workspace: &str, action: &str, asked: &str, kind: OperationKind) -> Delivered {
    Delivered {
        workspace: workspace.to_owned(),
        address: tilo_actions::ActionAddress::new(RESOURCE, action),
        argument: Some(format!(
            "{}{}",
            tilo_actions::ARGUMENTS_JSON_PREFIX,
            serde_json::json!({ "check_id": asked })
        )),
        contract: ActionContract {
            kind: ActionKind::Undoable,
            execution: ExecutionMode::Background { cancellable: false },
            effects: vec![ActionEffect::new(
                tilo_action_services::approved_checks::PROVIDER,
                tilo_action_services::approved_checks::RESOURCE,
                tilo_action_services::approved_checks::RUN,
            )],
            ..ActionContract::default()
        },
        identity: identity(),
        kind,
    }
}

/// An action of the same kind and contract that declares no effect of the device's.
fn undeclared(workspace: &str, action: &str, asked: &str) -> Delivered {
    undeclared_kind(workspace, action, asked, OperationKind::Effect)
}

/// [`undeclared`] for an action of `kind`.
fn undeclared_kind(workspace: &str, action: &str, asked: &str, kind: OperationKind) -> Delivered {
    let mut action = delivered_kind(workspace, action, asked, kind);
    action.contract.effects.clear();
    action
}

/// The app's call to run `check_id` as that invocation's work.
fn run_call(check_id: &str) -> InvokeRequest {
    InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        target: tilo_actions::remote::RemoteActionTarget {
            provider: tilo_action_services::approved_checks::PROVIDER.to_owned(),
            address: tilo_actions::ActionAddress::new(
                tilo_action_services::approved_checks::RESOURCE,
                tilo_action_services::approved_checks::RUN,
            ),
        },
        argument: InvokeArgument::Json(
            serde_json::json!({ "check_id": check_id })
                .as_object()
                .cloned()
                .unwrap(),
        ),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

fn admitted(serving: Option<Delivered>, heard: &Arc<Mutex<Vec<String>>>) -> Admitted {
    let heard = Arc::clone(heard);
    Admitted {
        serving: serving.map(|serving| Arc::new(serving) as Arc<dyn Serving>),
        progress: Arc::new(move |phase, completed, total| {
            heard
                .lock()
                .unwrap()
                .push(format!("{phase} {completed:?}/{total:?}"));
        }),
        ..Admitted::default()
    }
}

#[test]
fn the_devices_check_runs_as_the_delivered_invocations_work_and_answers_its_receipt() {
    let workspace = uuid::Uuid::new_v4().to_string();
    let artifacts = tempfile::tempdir().expect("a private artifacts directory");
    let checks = NativeChecks::over(
        given(Some(Arc::new(registered(&workspace, artifacts.path())))),
        Arc::new(Placed {
            taken: Mutex::default(),
            refusal: None,
        }),
        jobs(),
    );
    let heard = Arc::new(Mutex::new(Vec::new()));
    let action = tilo_action_services::approved_checks::ACTION_ID;
    let ran = Admits::start(
        &checks,
        &Caller::local_system(),
        &run_call("unit"),
        &admitted(Some(delivered(&workspace, action, "unit")), &heard),
    )
    .expect("the check starts")
    .wait_timeout(tilo_test_env::hang_bound())
    .unwrap_or_else(|_| panic!("the check ends"))
    .expect("the check answers")
    .expect("the check ran");
    let receipt: Receipt = serde_json::from_value(ran.value.expect("a receipt")).unwrap();
    assert_eq!(receipt.check_id, "unit");
    assert_eq!(receipt.binding.app_id, APP);
    assert_eq!(receipt.invocation, "0f8fad5b-d9cb-469f-a165-70867728950e");
    assert_eq!(
        heard.lock().unwrap().as_slice(),
        ["tests Some(1)/Some(2)"],
        "how far it got reached whoever follows the call"
    );
}

#[test]
fn an_action_runs_a_check_only_when_it_declares_the_hosts_effect() {
    let workspace = uuid::Uuid::new_v4().to_string();
    let artifacts = tempfile::tempdir().expect("a private artifacts directory");
    let checks = NativeChecks::over(
        given(Some(Arc::new(registered(&workspace, artifacts.path())))),
        Arc::new(Placed {
            taken: Mutex::default(),
            refusal: None,
        }),
        jobs(),
    );
    let heard = Arc::new(Mutex::new(Vec::new()));
    // A second action of the app's, of the same kind, declares the device's check effect: the
    // host runs the check as its work, whatever the action is called.
    let ran = Admits::start(
        &checks,
        &Caller::local_system(),
        &run_call("unit"),
        &admitted(
            Some(delivered_kind(
                &workspace,
                "another-check",
                "unit",
                OperationKind::Document,
            )),
            &heard,
        ),
    )
    .expect("a declared effect runs")
    .wait_timeout(tilo_test_env::hang_bound())
    .unwrap_or_else(|_| panic!("the check ends"))
    .expect("the check answers")
    .expect("the check ran");
    let receipt: Receipt = serde_json::from_value(ran.value.expect("a receipt")).unwrap();
    assert_eq!(receipt.check_id, "unit");

    // The same kind without the declaration runs nothing, and the refusal names the action.
    let named = tilo_action_services::approved_checks::ACTION_ID;
    let Err(refused) = Admits::start(
        &checks,
        &Caller::local_system(),
        &run_call("unit"),
        &admitted(
            Some(undeclared_kind(
                &workspace,
                named,
                "unit",
                OperationKind::Document,
            )),
            &heard,
        ),
    ) else {
        panic!("an action that declares no effect is refused");
    };
    assert!(
        refused.contains(&format!("`{named}`")) && refused.contains("declares no"),
        "the refusal names the action and what it lacks: {refused}"
    );
}

#[test]
fn a_check_call_that_is_not_the_delivered_checks_work_is_refused() {
    let workspace = uuid::Uuid::new_v4().to_string();
    let artifacts = tempfile::tempdir().expect("a private artifacts directory");
    let placement = Arc::new(Placed {
        taken: Mutex::default(),
        refusal: None,
    });
    let checks = NativeChecks::over(
        given(Some(Arc::new(registered(&workspace, artifacts.path())))),
        Arc::clone(&placement) as Arc<dyn Placement>,
        jobs(),
    );
    let heard = Arc::new(Mutex::new(Vec::new()));
    let action = tilo_action_services::approved_checks::ACTION_ID;
    for (why, call, serving) in [
        ("no invocation it is the work of", run_call("unit"), None),
        (
            "another action's work",
            run_call("unit"),
            Some(undeclared(&workspace, "push", "unit")),
        ),
        (
            "a check other than the one asked",
            run_call("deploy"),
            Some(delivered(&workspace, action, "unit")),
        ),
        (
            "a command instead of a check",
            run_call("unit; rm -rf ~"),
            Some(delivered(&workspace, action, "unit; rm -rf ~")),
        ),
    ] {
        let Err(refused) = Admits::start(
            &checks,
            &Caller::local_system(),
            &call,
            &admitted(serving, &heard),
        ) else {
            panic!("{why} is refused");
        };
        assert!(
            refused.contains("approved check"),
            "{why} is refused legibly: {refused}"
        );
    }
    assert!(
        placement.taken.lock().unwrap().is_empty(),
        "nothing refused takes a turn"
    );
    assert!(heard.lock().unwrap().is_empty(), "nothing refused reports");
}
