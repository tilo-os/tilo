//! A project's host runs the checks it approved, end to end on the device's own daemon: the
//! approval is written with the device's `settings#set` action, an app whose project the
//! approval names runs each check through its own published action, and the device keeps what
//! it observed in the job's record at `devices/<device>/jobs/<job>`. Removing the approval
//! refuses the next run with the reason the app shows.
//!
//! The fixture raises a session bus and the account's user manager, as a login session would,
//! because the micro-VM it runs in boots no init and the supervisor starts each check as a
//! service of that manager. Only the run named by [`FIXTURE`] runs it; the rest of the suite
//! passes it without starting anything.

#![cfg(unix)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::too_many_lines,
    reason = "in a test these are the assertion, not a hazard; one scenario reads in order"
)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, InvokeArgument, InvokeRequest, ReadAnswer,
    ReceiptStatus, RemoteActionTarget, ResourceReadRequest, answered, device_record_space,
};
use tilo_actions::{ActionAddress, InvocationId, InvocationUpdate};
use tilo_server_protocol::page::window::WindowLink;

use crate::language::host;
use crate::language::host::Host;

/// The fixture app: a project whose one action runs a check its host approved.
const APP: &str = "so.tilo.lang.check-fixture";

/// The environment the check sets to run this fixture: without it, the suite passes it without
/// a device of its own, since a check is the only place root and a user manager are at hand.
const FIXTURE: &str = "TILO_APPROVED_CHECKS_FIXTURE";

/// The fixture app's source: a project and the action that runs a check the project's host
/// approved, keeping what the device answers.
const SOURCE: &str = r#"app so.tilo.lang.check-fixture "Check fixture" (icon: .hash, category: .utility)

/// A project checked by its host.
type Project {
    said: Text = ""
}

root Project

default view project for Project

/// Runs a check this project's host approved, by its id.
action run_approved_check(p: Project, check_id: Text) uses check (label: "Run check") {
    match engine.check(check_id) {
        .err(why) => {
            p.said = why
        }
        .ok(job) => {
            p.said = job
        }
    }
}

/// The project, and the action that runs a check on its record.
view project(p: Project) {
    ui.Text(p.said)
    row(gap: .related) {
        button(run_approved_check(p, "passing")) as check
    }
}
"#;

/// The line each check's success prints, and the one its failure prints before exiting non-zero.
const PASSED: &str = "approved check passed";
const FAILED: &str = "approved check failed on purpose";

/// A check's command: it reports one phase on stderr, as the supervisor reads it, then prints
/// its line and ends with `code`.
fn command(line: &str, code: u8) -> Vec<String> {
    vec![
        "/usr/bin/sh".into(),
        "-c".into(),
        format!(
            "printf 'TILO_CHECK_REPORT_V1 {{\"token\":\"%s\",\"check_id\":\"%s\",\"sequence\":1,\
             \"phase_id\":\"tests\",\"completed\":1,\"total\":2}}\\n' \
             \"$TILO_CHECK_REPORT_TOKEN\" \"$TILO_CHECK_REPORT_ID\" >&2\necho {line}\nexit {code}"
        ),
    ]
}

fn limits() -> Value {
    json!({
        "cpu_percent": 100,
        "memory_bytes": 512 * 1024 * 1024,
        "processes": 64,
        "wall_seconds": 120,
        "output_bytes": 65536,
    })
}

fn phases() -> Value {
    json!([{ "id": "tests", "label": "Tests" }])
}

/// The directories one approved check runs on, below `base`: a source tree whose `target`
/// mount point is empty, a build directory, a private scratch, and the image's `usr` and `etc`.
fn directories(base: &Path, name: &str) -> Value {
    let root = base.join(name);
    let checkout = root.join("source");
    let build = root.join("build");
    let scratch = root.join("scratch");
    let image = root.join("image");
    for dir in [
        checkout.clone(),
        checkout.join("target"),
        build.clone(),
        image.clone(),
    ] {
        std::fs::create_dir_all(&dir).expect("a check directory");
    }
    std::fs::create_dir_all(&scratch).expect("a private scratch");
    std::fs::set_permissions(&scratch, std::fs::Permissions::from_mode(0o700))
        .expect("a private mode");
    // The image stands for the one a project's check is run on: the host's own `/usr` and
    // `/etc`, which carry the shell the commands name. Mounted, so the supervisor binds the
    // files themselves rather than a symlink to them.
    for subtree in ["usr", "etc"] {
        let at = image.join(subtree);
        std::fs::create_dir_all(&at).expect("the image's subtree");
        let mounted = std::process::Command::new("/usr/bin/mount")
            .args(["--bind"])
            .arg(Path::new("/").join(subtree))
            .arg(&at)
            .status()
            .expect("mount the image's subtree");
        assert!(mounted.success(), "the image's {} mounts", at.display());
    }
    json!({
        "checkout": checkout,
        "build": build,
        "scratch": scratch,
        "runtime_binary": std::env::var("TILO_APPROVED_CHECKS_USH")
            .expect("the check names the ush supervisor"),
        "images": [{ "identity": format!("sha256:{}", "a".repeat(64)), "root": image }],
        "environment": [],
        "limits": limits(),
        "reported_phases": phases(),
    })
}

/// The `checks` device setting approving `app`'s project in `workspace`, whose record and
/// resource identity the app published, and its two checks `passing` and `failing`.
fn approval(
    base: &Path,
    workspace: &str,
    record: &str,
    identity: &tilo_actions::ResourceSemanticIdentity,
) -> Value {
    let mut passing = directories(base, "passing");
    passing["id"] = json!("passing");
    passing["command"] = json!(command(PASSED, 0));
    let mut failing = directories(base, "failing");
    failing["id"] = json!("failing");
    failing["command"] = json!(command(FAILED, 1));
    json!({
        "version": 1,
        "projects": [{
            "app_id": APP,
            "workspace": workspace,
            "record": record,
            "resource_identity": { "kind": identity.kind, "key": identity.key },
            "checks": [passing, failing],
        }],
    })
}

/// The `checks` setting written, or removed, through the device's own action.
async fn settings(host: &Host, action: &str, arguments: Value) -> Result<Value, ActionCallError> {
    let target = RemoteActionTarget {
        provider: tilo_device_identity::login::PROVIDER.to_owned(),
        address: ActionAddress::new("settings", action),
    };
    host::output(host, target, arguments)
        .await
        .map(|output| output.value.unwrap_or(Value::Null))
}

/// The check the fixture app publishes on its project: the action's address, and the identity
/// of the record it acts on, which the approval names exactly.
async fn awaiting_check(host: &Host) -> (ActionAddress, tilo_actions::ResourceSemanticIdentity) {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let page = host::listed(host, APP)
            .await
            .expect("the app lists what it publishes");
        let found = page.rows.iter().find_map(|row| {
            let descriptor = row.action.as_ref()?;
            if descriptor.address.action != "run-approved-check" {
                return None;
            }
            let identity = descriptor.semantic_identity.clone()?;
            Some((descriptor.address.clone(), identity))
        });
        if let Some(found) = found {
            return found;
        }
        let shown: Vec<String> = page
            .rows
            .iter()
            .map(|row| {
                format!(
                    "{} {} {} identity={}",
                    row.provider,
                    row.resource.id,
                    row.action
                        .as_ref()
                        .map_or("-", |action| action.address.action.as_str()),
                    row.action
                        .as_ref()
                        .is_some_and(|action| action.semantic_identity.is_some()),
                )
            })
            .collect();
        assert!(
            Instant::now() < deadline,
            "the fixture app never published its check action on a record: {shown:?}"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// The job `job` at `devices/<device>/jobs/<job>`, read as the person from this device.
async fn job_reading(host: &Host, job: &str) -> Option<Value> {
    let reads = host.dispatch().resource_reads()?;
    let cert = host.daemon().workspaces().device_cert().clone();
    let caller = Caller::authenticated(cert.user_public_key, cert.device_public_key);
    let request = ResourceReadRequest {
        workspace: device_record_space(&host.daemon().workspaces().host_device()),
        app: "jobs".into(),
        record: job.into(),
        joined: false,
        after: None,
        wait_ms: None,
        answer: ReadAnswer::Reading,
    };
    let answer = answered(reads, &caller, request).await.ok()?;
    answer["records"]
        .get(0)
        .and_then(|entry| entry.get("value"))
        .cloned()
}

/// Run the approved check `check_id` through the app's own published action, following the
/// invocation until the device answers it: its receipt and the job's name.
async fn run_check(
    host: &Host,
    resource: &str,
    check_id: &str,
) -> Result<(String, Value), ActionCallError> {
    let invocation = InvocationId::new();
    let name = invocation.0.to_string();
    let provider = host.dispatch();
    let caller = host.caller();
    let request = InvokeRequest {
        invocation,
        target: host::app_action(host, APP, resource, "run-approved-check"),
        argument: InvokeArgument::Json(
            json!({ "check_id": check_id })
                .as_object()
                .cloned()
                .expect("the check's arguments"),
        ),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    let receipt = provider
        .invoke(&caller, request)
        .await
        .expect("the dispatch answers")?;
    let deadline = tilo_test_env::hang_deadline();
    let mut status = Some(receipt.status);
    loop {
        if let Some(state) = provider.observe(&caller, &invocation) {
            match state.update {
                InvocationUpdate::Succeeded { output, .. } => {
                    return Ok((name, output.value.unwrap_or(Value::Null)));
                }
                InvocationUpdate::Failed { error, .. } => {
                    return Err(ActionCallError::Refused {
                        reason: error.message,
                    });
                }
                InvocationUpdate::Cancelled { .. } => {
                    return Err(ActionCallError::Refused {
                        reason: "the check was cancelled".into(),
                    });
                }
                _ => {}
            }
        }
        match status.take() {
            Some(ReceiptStatus::Accepted) => {}
            Some(ReceiptStatus::Completed { output }) => {
                return Ok((name, output.value.unwrap_or(Value::Null)));
            }
            Some(ReceiptStatus::Failed { error }) => {
                return Err(ActionCallError::Refused {
                    reason: error.message,
                });
            }
            Some(ReceiptStatus::Rejected { error }) => return Err(error),
            Some(ReceiptStatus::Cancelled { .. }) => {
                return Err(ActionCallError::Refused {
                    reason: "the check was cancelled".into(),
                });
            }
            None => {}
        }
        assert!(Instant::now() < deadline, "the check never ended");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The job's record, once it holds one.
async fn job_record(host: &Host, job: &str) -> Value {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        if let Some(record) = job_reading(host, job).await {
            return record;
        }
        assert!(Instant::now() < deadline, "the job's record never appeared");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The account's session bus and the user manager a login session starts: the micro-VM boots
/// no init, so the check raises both itself. The supervisor starts each check as a service of
/// the manager, in a cgroup the manager bounds, so the manager must be the one the runner's
/// `systemd-run --user` reaches over the account's own bus.
struct UserManager {
    bus: Child,
}

impl Drop for UserManager {
    fn drop(&mut self) {
        let _ = std::process::Command::new("/usr/bin/systemctl")
            .args(["--user", "exit"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = self.bus.kill();
        let _ = self.bus.wait();
    }
}

fn start_user_manager() -> UserManager {
    let runtime = std::env::var("XDG_RUNTIME_DIR").expect("the test names its runtime directory");
    std::fs::create_dir_all("/run/systemd/system").expect("a session's system directory");
    // A session bus answers a request for the account's manager by starting it: the stock
    // service starts nothing (`/bin/false`), since a login starts the manager itself.
    std::fs::write(
        "/usr/share/dbus-1/services/org.freedesktop.systemd1.service",
        "[D-BUS Service]\nName=org.freedesktop.systemd1\nExec=/usr/lib/systemd/systemd --user\n",
    )
    .expect("the bus's service for the account's manager");
    // The manager must not start a bus of its own over the one above: the stock socket would
    // take the path this bus listens on, and the manager's own broker is not what a check is
    // started over.
    let home = std::env::var("HOME").expect("the test names its home");
    let units = Path::new(&home).join(".config/systemd/user");
    std::fs::create_dir_all(&units).expect("the account's user units");
    for unit in ["dbus.socket", "dbus.service"] {
        let at = units.join(unit);
        let _ = std::fs::remove_file(&at);
        std::os::unix::fs::symlink("/dev/null", &at).expect("the bus's own units are masked");
    }
    // The manager bounds each service in the cgroup a login gives it: root's own slice, with
    // the controllers delegated to the manager's unit.
    let unit = Path::new("/sys/fs/cgroup/user.slice/user-0.slice/user@0.service");
    for dir in [
        PathBuf::from("/sys/fs/cgroup"),
        PathBuf::from("/sys/fs/cgroup/user.slice"),
        PathBuf::from("/sys/fs/cgroup/user.slice/user-0.slice"),
        unit.to_path_buf(),
    ] {
        std::fs::create_dir_all(&dir).unwrap_or_else(|error| panic!("{}: {error}", dir.display()));
        std::fs::write(dir.join("cgroup.subtree_control"), "+cpu +memory +pids")
            .unwrap_or_else(|error| panic!("{}: {error}", dir.display()));
    }
    let scope = unit.join("init.scope");
    std::fs::create_dir_all(&scope).expect("the manager's own scope");
    std::fs::write(scope.join("cgroup.procs"), std::process::id().to_string())
        .expect("this process joins the manager's scope");
    // Started after this process joined its scope, so the manager the bus starts below is in
    // it too, and starts each check's service under it.
    let bus_socket = Path::new(&runtime).join("bus");
    let log = Path::new(&runtime).join("session-bus.log");
    eprintln!("approved checks: starting the account's bus at {}", bus_socket.display());
    let bus = std::process::Command::new("/usr/bin/dbus-daemon")
        .args([
            "--session",
            "--nofork",
            "--address",
            &format!("unix:path={}", bus_socket.display()),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(&log).expect("the bus's log"))
        .spawn()
        .expect("the account's session bus starts");
    let manager = UserManager { bus };
    let deadline = Instant::now() + Duration::from_secs(240);
    let mut said = String::new();
    loop {
        // Each ask is bounded: a manager that takes a message and never answers one would
        // otherwise leave this waiting with no deadline of its own.
        let state = std::process::Command::new("/usr/bin/timeout")
            .args(["--signal=KILL", "5", "/usr/bin/systemctl", "--user", "is-system-running"])
            .output();
        said = state.as_ref().map_or_else(
            |error| error.to_string(),
            |state| format!("{:?} {}", state.status, String::from_utf8_lossy(&state.stdout)),
        );
        if let Ok(state) = &state {
            if state.status.success()
                || String::from_utf8_lossy(&state.stdout).trim() == "degraded"
            {
                eprintln!("approved checks: the account's user manager answers: {said}");
                return manager;
            }
        }
        // A message to the manager's name is what the bus starts it for; systemctl reaches a
        // manager already running, so the first request asks it into being.
        let _ = std::process::Command::new("/usr/bin/timeout")
            .args([
                "--signal=KILL",
                "5",
                "/usr/bin/busctl",
                "--address",
                &format!("unix:path={}", bus_socket.display()),
                "call",
                "org.freedesktop.systemd1",
                "/org/freedesktop/systemd1",
                "org.freedesktop.DBus.Peer",
                "Ping",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        assert!(
            Instant::now() < deadline,
            "the account's user manager did not start (last answer {said}):\n{}",
            std::fs::read_to_string(&log).unwrap_or_default()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn a_projects_host_runs_its_approved_checks_and_removal_refuses_the_next() {
    if std::env::var_os(FIXTURE).is_none() {
        return;
    }
    host::run_with(&["counter"], |host| async move {
        let _manager = start_user_manager();
        eprintln!("approved checks: the account's user manager started");
        let workspace = host.workspace().to_owned();
        host::apps::install_source(APP, SOURCE);
        // The app runs as a window of its own, on the signed runtime, as a person opens it: its
        // view reaches the action that runs a check, so the app publishes that action on the
        // project's record.
        let (daemon, caller, viewing) = (host.daemon(), host.caller(), workspace.clone());
        let window = tokio::task::spawn_blocking(move || -> Result<WindowLink, String> {
            let link = daemon.open_view(&caller, APP, &viewing, None, None)?;
            let mut window = WindowLink::over(link);
            window.open(None).map_err(|error| error.to_string())?;
            Ok(window)
        })
        .await
        .expect("the view opens")
        .expect("the fixture app's window answers");
        eprintln!("approved checks: the fixture app's view is open");
        // The view stays open for the whole test: an app whose window closed stops.
        let _view = window;
        let (address, identity) = awaiting_check(&host).await;
        let page = host::listed(&host, APP)
            .await
            .expect("the app lists its record");
        let record = host::running_record(&page, &workspace, APP)
            .expect("the app opened the record its project is");
        let directory = tempfile::tempdir_in(std::env::temp_dir()).expect("a check directory");
        let approved = approval(directory.path(), &workspace, &record, &identity);

        let written = settings(&host, "set", json!({ "name": "checks", "value": approved }))
            .await
            .expect("the person approves the project's checks");
        assert_eq!(written["name"], "checks");
        eprintln!("approved checks: the approval is written");

        let (job, receipt) = run_check(&host, &address.resource, "passing")
            .await
            .expect("the approved check runs");
        let record = job_record(&host, &job).await;
        assert_eq!(record["state"], "done", "{record}");
        assert_eq!(record["progress"]["phase"], "Tests", "{record}");
        assert_eq!(record["progress"]["completed"], 1, "{record}");
        assert_eq!(record["progress"]["total"], 2, "{record}");
        assert_eq!(record["done"]["outcome"], "Exited", "{record}");
        assert_eq!(record["done"]["exit_code"], 0, "{record}");
        assert_eq!(record["done"]["cleanup_observed"], true, "{record}");
        assert!(
            record["done"]["stdout"]["preview"]
                .as_str()
                .is_some_and(|stdout| stdout.contains(PASSED)),
            "{record}"
        );
        assert_eq!(receipt["check_id"], "passing");
        assert_eq!(receipt["invocation"], job);
        eprintln!("approved checks: the passing check ran");

        let (job, receipt) = run_check(&host, &address.resource, "failing")
            .await
            .expect("the failing approved check runs to its observed end");
        let record = job_record(&host, &job).await;
        assert_eq!(record["state"], "done", "{record}");
        assert_eq!(record["progress"]["phase"], "Tests", "{record}");
        assert_eq!(record["progress"]["completed"], 1, "{record}");
        assert_eq!(record["progress"]["total"], 2, "{record}");
        assert_eq!(record["done"]["outcome"], "Exited", "{record}");
        assert_eq!(record["done"]["exit_code"], 1, "{record}");
        assert!(
            record["done"]["stdout"]["preview"]
                .as_str()
                .is_some_and(|stdout| stdout.contains(FAILED)),
            "{record}"
        );
        assert_eq!(receipt["exit_code"], 1);
        eprintln!("approved checks: the failing check ran to its observed end");

        let removed = settings(&host, "remove", json!({ "name": "checks" }))
            .await
            .expect("the person removes the approval");
        assert_eq!(removed["removed"], true);
        eprintln!("approved checks: the approval is removed");
        let refusal = run_check(&host, &address.resource, "passing")
            .await
            .expect_err("a removed approval refuses the next check");
        assert!(
            matches!(&refusal, ActionCallError::Refused { reason } if reason.contains("approved checks")),
            "the refusal names why: {refusal:?}"
        );
    });
}
