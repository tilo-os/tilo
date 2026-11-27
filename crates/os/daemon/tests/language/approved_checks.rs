//! A project's host runs the checks it approved, end to end on the device's own daemon: the
//! approval is written with the device's `settings#set` action, an app whose project the
//! approval names runs each check through its own published action, and the device keeps what
//! it observed in the job's record at `devices/<device>/jobs/<job>`. Removing the approval
//! refuses the next run with the reason the app shows.
//!
//! The app is installed in the workspace as a person installs one there, because an app keeps
//! its records in a workspace only once that workspace runs it.
//!
//! The fixture raises the account's user manager before the first check runs, as a login session
//! does, because the micro-VM it runs in boots no init and the supervisor starts each check as a
//! service of that manager, in a cgroup it bounds. Only the run named by [`FIXTURE`] runs it;
//! the rest of the suite passes it without starting anything.

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
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tilo_actions::remote::{
    ActionCallError, InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionTarget,
    WORKSPACE_GOVERNANCE_PROVIDER, workspace_governance_address,
};
use tilo_actions::{ActionAddress, ActionOutput, InvocationId, InvocationUpdate};
use tilo_server_protocol::page::window::WindowLink;
use tilo_test_env::SpawnUnderEnvLock as _;

use crate::language::host;
use crate::language::host::Host;

/// Say what the fixture is doing, in a file the check streams and on its own error output: the
/// harness that runs this test in its own process keeps the process's streams until it ends, so
/// a step that waits is visible only where it was written.
fn step(what: &str) {
    use std::io::Write as _;
    eprintln!("approved checks: {what}");
    let Ok(dir) = std::env::var("TMPDIR") else {
        return;
    };
    let path = Path::new(&dir).join("approved-checks-steps.log");
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = writeln!(file, "approved checks: {what}");
        let _ = file.flush();
    }
}

/// End this process where it is: the steps carry the failure already, and the unwinding a panic
/// would take stops the daemon, which waits on an app that never answered.
#[expect(
    clippy::exit,
    reason = "a fixture that has said why ends there, not through a teardown that waits on an app"
)]
fn end_here() -> ! {
    std::process::exit(101);
}

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
/// What a device answers a check its setting no longer approves, in either moment the removal
/// may reach it: before the invocation is admitted, or as its start reads the setting.
const NO_APPROVAL: &str = "no approved checks for this exact project resource";

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
            .status_locked()
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

/// What `target` answers `arguments` with, run as the person and followed to its end.
async fn answer(
    host: &Host,
    target: RemoteActionTarget,
    arguments: Value,
    what: &str,
) -> Result<ActionOutput, ActionCallError> {
    tokio::time::timeout(
        Duration::from_secs(60),
        host::output(host, target, arguments),
    )
    .await
    .unwrap_or_else(|_| panic!("the device did not answer {what} within 60s"))
}

/// The `checks` setting written, or removed, through the device's own action.
async fn settings(host: &Host, action: &str, arguments: Value) -> Result<Value, ActionCallError> {
    let target = RemoteActionTarget {
        provider: tilo_device_identity::login::PROVIDER.to_owned(),
        address: ActionAddress::new("settings", action),
    };
    answer(host, target, arguments, &format!("`settings#{action}`"))
        .await
        .map(|output| output.value.unwrap_or(Value::Null))
}

/// The check the fixture app publishes on its project: the action's address, and the identity
/// of the record it acts on, which the approval names exactly.
async fn awaiting_check(host: &Host) -> (ActionAddress, tilo_actions::ResourceSemanticIdentity) {
    tilo_test_env::within("the fixture app's check action", async {
        loop {
            let page = tokio::time::timeout(Duration::from_secs(30), host::listed(host, APP))
                .await
                .unwrap_or_else(|_| panic!("the app did not answer its catalogue within 30s"))
                .expect("the app lists what it publishes");
            let found = page.rows.iter().find_map(|row| {
                let descriptor = row.action.as_ref()?;
                if descriptor.address.action != "run-approved-check" {
                    return None;
                }
                Some((
                    descriptor.address.clone(),
                    descriptor.semantic_identity.clone()?,
                ))
            });
            if let Some(found) = found {
                return found;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
}

/// Install the package file `tapp` in `workspace`, as `tilo-app push` does: the workspace keeps
/// the file in the pieces it takes, and its governance names the version every member runs, with
/// the BLAKE3 `surface` of the compiled surface the package ships. An app keeps its records in a
/// workspace only once that workspace installed it.
async fn install_in_workspace(host: &Host, workspace: &str, tapp: &Path, surface: &str) {
    let bytes = std::fs::read(tapp).expect("the fixture's package file");
    let mut blobs = std::collections::BTreeMap::new();
    let read = tilo_app_package::tapp::read(bytes.as_slice(), &mut blobs)
        .expect("the fixture's package reads");
    let manifest = read.meta.manifest.package.clone();
    let file = blake3::hash(&bytes).to_hex().to_string();
    let total = u64::try_from(bytes.len()).expect("the package's size");
    let mut offset = 0_u64;
    step("installing the fixture app in the workspace");
    for piece in bytes.chunks(256 * 1024) {
        let arguments = json!({
            "file": &file,
            "offset": offset,
            "total": total,
            "data": hex::encode(piece),
        });
        output_to(host, workspace, "upload-app-file", arguments).await;
        offset += u64::try_from(piece.len()).expect("a piece's size");
    }
    let arguments = json!({
        "app": manifest.id,
        "version": manifest.version,
        "surface": surface,
        "package": hex::encode(read.meta.id.0),
        "publisher": hex::encode(read.meta.root.publisher_identity),
        "file": &file,
        "dev": true,
    });
    output_to(host, workspace, "install-app", arguments).await;
    step("the fixture app is installed in the workspace");
}

/// What `workspace`'s governance answers `action` with `arguments`, run as the person and
/// followed to its end.
async fn output_to(host: &Host, workspace: &str, action: &str, arguments: Value) {
    let target = RemoteActionTarget {
        provider: WORKSPACE_GOVERNANCE_PROVIDER.to_owned(),
        address: workspace_governance_address(workspace, action),
    };
    answer(host, target, arguments, &format!("`{action}`"))
        .await
        .unwrap_or_else(|error| panic!("the workspace answers `{action}`: {error:?}"));
}

/// The job `job` as the device keeps it: the record at `devices/<device>/jobs/<job>`, which the
/// device writes below its profile at `records/jobs/<job>.json` and answers to the person's own
/// dispatch. Read as it stands, or why it could not be read.
fn job_reading(job: &str) -> Result<Value, String> {
    let profile = std::env::var_os("TILO_HOST_TEST_PROFILE").ok_or("the test names no profile")?;
    let path = Path::new(&profile)
        .join("records/jobs")
        .join(format!("{job}.json"));
    let bytes = std::fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))?;
    serde_json::from_slice(&bytes).map_err(|error| format!("{}: {error}", path.display()))
}

/// The job's record, once it holds one.
fn job_record(job: &str) -> Value {
    let mut last = String::new();
    tilo_test_env::wait_for(format!("the job's record {job}"), || {
        match job_reading(job) {
            Ok(record) => Some(record),
            Err(said) => {
                if said != last {
                    step(&format!("the job's read: {said}"));
                    last = said;
                }
                None
            }
        }
    })
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
        asks: false,
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
    let receipt = tokio::time::timeout(Duration::from_secs(60), provider.invoke(&caller, request))
        .await
        .unwrap_or_else(|_| panic!("the dispatch did not take `{check_id}` within 60s"))
        .expect("the dispatch answers")?;
    let mut status = Some(receipt.status);
    tilo_test_env::within(format!("the check `{check_id}` to end"), async {
        loop {
            if let Some(state) = provider.observe(&caller, &invocation) {
                match state.update {
                    InvocationUpdate::Succeeded { output, .. } => {
                        let (receipt, job) = answered_receipt(&output, &name);
                        return Ok((job, receipt));
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
                Some(ReceiptStatus::Accepted) | None => {}
                Some(ReceiptStatus::Completed { output }) => {
                    let (receipt, job) = answered_receipt(&output, &name);
                    return Ok((job, receipt));
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
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
}

/// The reason a refusal names, for the refusals the fixture pins to their exact words: an
/// answer that is no refusal names none.
fn refusal_reason(error: &ActionCallError) -> Option<String> {
    match error {
        ActionCallError::Refused { reason } => Some(reason.clone()),
        _ => None,
    }
}

/// The receipt the action answered, and the job it names: the host's receipt of the check it
/// ran, whether the answer is that receipt or a view of it carrying the receipt under `receipt`.
/// An answer naming no invocation is the invocation asked with.
fn answered_receipt(output: &ActionOutput, asked: &str) -> (Value, String) {
    let value = output.value.clone().unwrap_or(Value::Null);
    let receipt = value
        .get("receipt")
        .filter(|receipt| receipt.is_object())
        .cloned()
        .unwrap_or(value);
    let job = receipt
        .get("invocation")
        .and_then(Value::as_str)
        .map_or_else(|| asked.to_owned(), str::to_owned);
    (receipt, job)
}

#[test]
fn a_projects_host_runs_its_approved_checks_and_removal_refuses_the_next() {
    if std::env::var_os(FIXTURE).is_none() {
        return;
    }
    host::run_with(&["counter"], |host| async move {
        // A panic's own words reach the check's stream too: this process's error output is kept
        // by the harness until the process ends, and a panic here can leave an app process
        // holding what it keeps.
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            step(&format!("panicked: {info}"));
            default_hook(info);
            // Unwinding stops the daemon, and a teardown that waits on an app that never
            // answered would keep the process here with nothing said. The steps carry the
            // failure already: end here, and the harness's process exits.
            end_here();
        }));
        step("fixture start");
        let workspace = host.workspace().to_owned();
        step("installing the fixture app");
        let tapp = host::apps::install_source(APP, SOURCE);
        step("the fixture app is installed on the device");
        // The compiled surface the package ships, which the workspace decides the app's record
        // changes against: naming it here is also what tells this device's daemon the surface of
        // the app it installed while the device ran, as its start does for the packages it holds.
        let surface = host
            .daemon()
            .apps()
            .installed_surface(APP)
            .expect("the fixture's package ships a compiled surface");
        let surface = blake3::hash(&surface).to_hex().to_string();
        // An app keeps its records in a workspace once that workspace installed it, as a person
        // who installs it there does.
        install_in_workspace(&host, &workspace, &tapp, &surface).await;
        // The app runs as a window of its own, on the signed runtime, as a person opens it: its
        // view reaches the action that runs a check, so the app publishes that action on the
        // project's record. The workspace's install reaches this device's store through the
        // device's own follow of its workspace's apps, which may still be unpacking the package
        // when the view opens: a view that ends for that is tried again, and the one it ends for
        // is said.
        let deadline = Instant::now() + Duration::from_secs(180);
        let mut last: String;
        let window = loop {
            let (daemon, caller, viewing) = (host.daemon(), host.caller(), workspace.clone());
            step("opening the fixture app's view");
            let attempt = tokio::time::timeout(
                Duration::from_secs(60),
                tokio::task::spawn_blocking(move || -> Result<WindowLink, String> {
                    step("the daemon is opening the app's view");
                    let link = daemon.open_view(&caller, APP, &viewing, None, None)?;
                    step("the app's view is opened: its window is handed the page");
                    // The ask and its answer are steps of their own: whether the window was told,
                    // and whether it said anything back, are different places to wait.
                    let mut link = link;
                    tilo_server_protocol::page::window::send(
                        &mut link,
                        &tilo_server_protocol::page::window::WindowAsk::Open { show: None },
                    )
                    .map_err(|error| error.to_string())?;
                    step("the window was asked for its page");
                    let answered = tilo_server_protocol::page::window::receive::<
                        tilo_server_protocol::page::window::WindowAnswer,
                    >(&mut link)
                    .map_err(|error| error.to_string())?;
                    step(&format!(
                        "the window answered: {}",
                        answered.as_ref().map_or("nothing", |answer| match answer {
                            tilo_server_protocol::page::window::WindowAnswer::Shown {
                                title,
                                ..
                            } => title.as_str(),
                            tilo_server_protocol::page::window::WindowAnswer::Changed(_)
                            | tilo_server_protocol::page::window::WindowAnswer::Desktop {
                                ..
                            } => "changes",
                        })
                    ));
                    if answered.is_none() {
                        return Err("the window let go of its page".to_owned());
                    }
                    Ok(WindowLink::over(link))
                }),
            )
            .await;
            match attempt {
                Ok(Ok(Ok(window))) => break window,
                Ok(Ok(Err(error))) => {
                    step(&format!("the app's view did not open: {error}"));
                    last = error;
                }
                Ok(Err(error)) => panic!("the view's task ends: {error}"),
                Err(_) => {
                    step("the app's view did not answer within 60s");
                    last = "it did not answer within 60s".to_owned();
                }
            }
            if Instant::now() >= deadline {
                // What the app does say is read before giving up: whether it published.
                let catalogue =
                    tokio::time::timeout(Duration::from_secs(20), host::listed(&host, APP)).await;
                let said = match catalogue {
                    Ok(Ok(page)) => {
                        let rows: Vec<String> = page
                            .rows
                            .iter()
                            .map(|row| {
                                let action = row
                                    .action
                                    .as_ref()
                                    .map_or("-", |action| action.address.action.as_str());
                                format!("{} {} {action}", row.provider, row.resource.id)
                            })
                            .collect();
                        format!("{} rows: {rows:?}", page.rows.len())
                    }
                    Ok(Err(error)) => format!("its catalogue failed: {error}"),
                    Err(_) => "it did not answer its catalogue within 20s".to_owned(),
                };
                panic!("the fixture app's view did not open within 180s ({last}); {said}");
            }
            std::thread::sleep(Duration::from_secs(1));
        };
        step("the fixture app's view is open");
        // The view stays open for the whole test: an app whose window closed stops.
        let _view = window;
        let (address, identity) = awaiting_check(&host).await;
        let page = tokio::time::timeout(Duration::from_secs(30), host::listed(&host, APP))
            .await
            .unwrap_or_else(|_| panic!("the app did not answer its record listing within 30s"))
            .expect("the app lists its record");
        let record = host::running_record(&page, &workspace, APP)
            .expect("the app opened the record its project is");
        let directory = tempfile::tempdir_in(std::env::temp_dir()).expect("a check directory");
        let approved = approval(directory.path(), &workspace, &record, &identity);

        // The account's user manager is what places a check in its service, so it starts before
        // the first check runs; the app is opened before it, as a person's app is opened whether
        // or not a check is running.
        let _manager = host::user_manager();
        step("the account's user manager started");
        let written = settings(&host, "set", json!({ "name": "checks", "value": approved }))
            .await
            .expect("the person approves the project's checks");
        assert_eq!(written["name"], "checks");
        step("the approval is written");

        let (job, receipt) = run_check(&host, &address.resource, "passing")
            .await
            .expect("the approved check runs");
        // What the action answered is the host's receipt of the check it ran, or the reason it
        // ran none: the job's record then says what it did.
        step(&format!("the check's own answer: {receipt}"));
        let record = job_record(&job);
        assert_eq!(record["state"], "done", "{record}");
        assert_eq!(record["progress"]["phase"], "Tests", "{record}");
        assert_eq!(record["progress"]["completed"], 1, "{record}");
        assert_eq!(record["progress"]["total"], 2, "{record}");
        assert_eq!(record["done"]["outcome"], "exited", "{record}");
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
        step("the passing check ran");

        let (job, receipt) = run_check(&host, &address.resource, "failing")
            .await
            .expect("the failing approved check runs to its observed end");
        let record = job_record(&job);
        assert_eq!(record["state"], "done", "{record}");
        assert_eq!(record["progress"]["phase"], "Tests", "{record}");
        assert_eq!(record["progress"]["completed"], 1, "{record}");
        assert_eq!(record["progress"]["total"], 2, "{record}");
        assert_eq!(record["done"]["outcome"], "exited", "{record}");
        assert_eq!(record["done"]["exit_code"], 1, "{record}");
        assert!(
            record["done"]["stdout"]["preview"]
                .as_str()
                .is_some_and(|stdout| stdout.contains(FAILED)),
            "{record}"
        );
        assert_eq!(receipt["exit_code"], 1);
        step("the failing check ran to its observed end");

        let removed = settings(&host, "remove", json!({ "name": "checks" }))
            .await
            .expect("the person removes the approval");
        assert_eq!(removed["removed"], true);
        step("the approval is removed");
        // The removal reaches the device as a record it reads when a check is asked for, so an
        // ask made the instant the setting answers may be admitted before the device has read
        // it and refused once its start reads the setting. Both say the same thing, and that is
        // what is asserted: the two asks below are the two sides of that moment, and neither
        // makes the answer the fixture demands different.
        let refusal = run_check(&host, &address.resource, "passing")
            .await
            .expect_err("a removed approval refuses the next check");
        assert_eq!(
            refusal_reason(&refusal).as_deref(),
            Some(NO_APPROVAL),
            "the refusal names the removed approval: {refusal:?}"
        );
        let again = run_check(&host, &address.resource, "passing")
            .await
            .expect_err("a removed approval refuses every later check");
        assert_eq!(
            refusal_reason(&again).as_deref(),
            Some(NO_APPROVAL),
            "the later refusal names the removed approval, as the first did: {again:?}"
        );
        step("a removed approval refuses, and names what it removed");
    });
}
