//! The checks a device's private configuration approved: what it reads, what it refuses, and
//! the job it keeps while one runs.

use std::os::unix::fs::PermissionsExt as _;
use std::sync::{Arc, Mutex};

use runtime_collab::approved_checks::ReportObservation;
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
        assert_eq!(
            check_id, self.check_id,
            "the approved id is the one that runs"
        );
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

fn registry(projects: Vec<Registered>) -> Arc<Registry> {
    Arc::new(Registry(Arc::new(Definitions(projects))))
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
        assert!(
            !invocation.is_empty(),
            "a placed check names its invocation"
        );
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
        registry(vec![registered(&workspace, artifacts.path())]),
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
        runtime_collab::approved_checks::Outcome::Exited
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
            workspace.clone(),
            RECORD.to_owned(),
            "unit".to_owned()
        )][..]
    );

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
        registry(Vec::new()),
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
fn a_check_approved_for_another_record_is_refused() {
    let workspace = uuid::Uuid::new_v4().to_string();
    let artifacts = tempfile::tempdir().expect("a private artifacts directory");
    let mut other = registered(&workspace, artifacts.path());
    other.binding.record = "another-project".into();
    let checks = NativeChecks::over(
        registry(vec![other]),
        Arc::new(Placed {
            taken: Mutex::default(),
            refusal: None,
        }) as Arc<dyn Placement>,
        jobs(),
    );
    let identity = identity();
    let error = checks
        .run_ask(&ask(&workspace, &identity), &|| Ok(()), &|_| {})
        .expect_err("approval binds one record");
    assert_eq!(error, "no approved checks for this exact project resource");
}

#[test]
fn a_check_this_device_does_not_take_runs_nowhere() {
    let workspace = uuid::Uuid::new_v4().to_string();
    let artifacts = tempfile::tempdir().expect("a private artifacts directory");
    let checks = NativeChecks::over(
        registry(vec![registered(&workspace, artifacts.path())]),
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

#[test]
fn the_startup_configuration_is_one_private_absolute_file() {
    let directory = tempfile::tempdir().expect("a fixture directory");
    let path = directory.path().join("checks.json");
    std::fs::write(&path, br#"{"version":1,"projects":[]}"#).expect("a fixture");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("a mode");
    assert!(
        load(&path).is_err(),
        "a publicly readable grading configuration is refused"
    );
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("a mode");
    assert!(load(&path).expect("a private configuration").0.0.is_empty());
    for bytes in [
        br#"{"version":1,"version":2,"projects":[]}"#.as_slice(),
        br#"{"version":1,"projects":[],"command":"id"}"#.as_slice(),
    ] {
        std::fs::write(&path, bytes).expect("a fixture");
        assert!(
            load(&path).is_err(),
            "an ambiguous configuration is refused"
        );
    }

    let flag = std::ffi::OsString::from("--approved-checks");
    assert!(argument_path([flag.clone()]).is_err());
    assert!(argument_path([flag.clone(), "relative.json".into()]).is_err());
    assert!(
        argument_path([
            flag.clone(),
            path.as_os_str().into(),
            flag,
            path.as_os_str().into()
        ])
        .is_err()
    );
    assert_eq!(
        argument_path(["--profile".into(), "native".into()]).expect("unrelated flags"),
        None
    );
}
