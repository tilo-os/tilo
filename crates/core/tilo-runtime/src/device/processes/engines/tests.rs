//! The device's engine jobs as an app's process reaches them: a job handed to the device runs in
//! its engine host, a process this test's launcher starts, and each move it says is kept as the
//! job's record of the app that started it.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use runtime_portal::process_link::{
    Descriptor, EngineJob, FromProcess, JobNeeds, Packet as _, ToProcess,
};
use serde_json::{Value, json};
use tilo_actions::remote::{ResourceCopy, device_job_path};

use super::super::super::jobs::{DeviceJobs, JobEffects};
use super::super::{Alone, Host, HostKey, Processes};

const DEVICE: [u8; 32] = [0x0a; 32];
const WORKSPACE: &str = "7a110d00-0000-4000-8000-0000000000b1";
const BACKUPS: &str = "so.tilo.backups";
const NOTES: &str = "so.tilo.notes";

/// The variable that makes this test binary the engine host the test launcher starts.
const HOST_ENV: &str = "TILO_TEST_ENGINE_HOST";
const HOST_TEST: &str =
    "device::processes::engines::tests::the_engine_host_the_tests_launcher_starts";

/// Run as the device's engine host: each job runs as its op says (`echo` ends at once, `slow` a
/// moment later, `wait` once it is stopped), telling every move on the link.
#[test]
fn the_engine_host_the_tests_launcher_starts() {
    use std::os::fd::AsFd as _;
    if std::env::var_os(HOST_ENV).is_none() {
        return;
    }
    let link = std::io::stdin().as_fd().try_clone_to_owned().unwrap();
    let told = |job: &EngineJob, state: &str, detail: Value| {
        FromProcess::Job {
            job: job.job.clone(),
            // What the host says of whose job it is counts for nothing: the device names it.
            record: json!({
                "id": job.job, "app": "so.tilo.someone-else", "workspace": "elsewhere",
                "op": job.op, "state": state, "detail": detail,
            }),
        }
        .send(&link)
        .unwrap();
    };
    let mut under = None;
    let mut waiting: HashMap<String, EngineJob> = HashMap::new();
    loop {
        match ToProcess::receive(&link).unwrap() {
            Some(ToProcess::JobsUnder(said)) => under = Some(said),
            Some(ToProcess::Engine(job)) => {
                assert_eq!(
                    under.as_deref(),
                    Some(super::jobs_under(&DEVICE).as_str()),
                    "told where jobs are kept first"
                );
                told(&job, "running", json!({}));
                match job.op.as_str() {
                    "echo" => told(&job, "done", json!({ "answer": job.input })),
                    "slow" => {
                        std::thread::sleep(Duration::from_millis(300));
                        told(&job, "done", json!({ "answer": job.input }));
                    }
                    _ => {
                        waiting.insert(job.job.clone(), *job);
                    }
                }
            }
            Some(ToProcess::CancelJob { job }) => {
                let job = waiting.remove(&job).expect("a stop of a job that runs");
                told(&job, "cancelled", json!({}));
            }
            Some(ToProcess::Finish) | None => break,
            Some(other) => panic!("an engine host is asked nothing else: {other:?}"),
        }
    }
}

/// What the daemon does for each job, as the test sees it: every move after its first, and its
/// end.
#[derive(Default)]
struct Effects {
    moves: Mutex<Vec<Value>>,
    moved: Condvar,
}

impl JobEffects for Effects {
    fn progress(&self, record: &Value) {
        self.moves.lock().unwrap().push(record.clone());
        self.moved.notify_all();
    }

    fn ended(&self, record: &Value) {
        self.moves.lock().unwrap().push(record.clone());
        self.moved.notify_all();
    }

    fn hold(&self, _invocation: &str) {}

    fn release(&self, _invocation: &str) {}
}

/// A process of `app` running, as if the device had started it, with an instance of the app: its
/// key, which start it is, and the app's end of its link.
fn run_app(processes: &Processes, app: &str) -> (HostKey, u64, Descriptor) {
    let key = HostKey {
        workspace: WORKSPACE.to_owned(),
        app: app.to_owned(),
        alone: None,
        runtime: "tilolang@1".to_owned(),
        app_runtime: "tilo@1".to_owned(),
    };
    let (ours, theirs) = runtime_portal::process_link::pair().unwrap();
    let generation = processes
        .generations
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    processes.running().insert(
        key.clone(),
        Host {
            socket: Arc::new(ours),
            instances: HashMap::from([(1, app.to_owned())]),
            generation,
            builds: HashMap::new(),
            idle_since: None,
        },
    );
    (key, generation, theirs)
}

/// A device whose launcher starts this test binary as its engine host.
struct Device {
    _dir: tempfile::TempDir,
    processes: Arc<Processes>,
    jobs: Arc<DeviceJobs>,
    effects: Arc<Effects>,
    _launcher: tilo_test_env::StandIn,
}

impl Device {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let launcher = tilo_test_env::stand_in(
            dir.path().join("tilo-launcher"),
            &format!(
                "export {HOST_ENV}=1\nexec '{exe}' --exact {HOST_TEST} --nocapture --quiet\n",
                exe = std::env::current_exe().unwrap().display(),
            ),
        )
        .unwrap();
        let processes = Processes::with(
            dir.path().join("data"),
            launcher.path().to_owned(),
            Box::new(|_, app, _| Err(format!("{app} is not installed here"))),
        );
        let jobs = DeviceJobs::new(DEVICE);
        let effects = Arc::new(Effects::default());
        jobs.serve_effects(Arc::clone(&effects) as Arc<dyn JobEffects>);
        processes.serving_jobs(Arc::clone(&jobs));
        Self {
            _dir: dir,
            processes,
            jobs,
            effects,
            _launcher: launcher,
        }
    }

    /// The states job `id` was seen in after its first, once it reached `last`.
    fn until(&self, id: &str, last: &str) -> Vec<Value> {
        let (moves, waited) = self
            .effects
            .moved
            .wait_timeout_while(
                self.effects.moves.lock().unwrap(),
                Duration::from_secs(60),
                |moves| {
                    !moves
                        .iter()
                        .any(|record| record["id"] == id && record["state"] == last)
                },
            )
            .unwrap();
        let records: Vec<Value> = moves
            .iter()
            .filter(|record| record["id"] == id)
            .cloned()
            .collect();
        drop(moves);
        assert!(!waited.timed_out(), "{id} reached {last}: {records:?}");
        records
    }

    /// The job `id` as the device's records hold it now.
    fn record(&self, id: &str) -> Option<Value> {
        match self.jobs.copy(id) {
            ResourceCopy::Reading { value, .. } => Some(value),
            _ => None,
        }
    }

    fn engine_host(&self) -> Option<u64> {
        self.processes
            .running()
            .iter()
            .find(|(key, _)| key.alone == Some(Alone::Engines))
            .map(|(_, host)| host.generation)
    }
}

impl Drop for Device {
    /// The engine host this device started lets its link go and ends.
    fn drop(&mut self) {
        for host in self.processes.running().values() {
            let _ = ToProcess::Finish.send(&host.socket);
        }
    }
}

fn job(app: &str, id: &str, op: &str, needs: JobNeeds) -> EngineJob {
    EngineJob {
        job: device_job_path(&DEVICE, id),
        app: app.to_owned(),
        workspace: WORKSPACE.to_owned(),
        invocation: "inv-1".to_owned(),
        by: "person-1".to_owned(),
        op: op.to_owned(),
        input: "{\"folder\":\"Photos\"}".to_owned(),
        needs,
        here: false,
        handlers: json!({ "done": { "resource": "app", "action": "finished" } }),
    }
}

fn states(records: &[Value]) -> Vec<&str> {
    records
        .iter()
        .map(|record| record["state"].as_str().unwrap())
        .collect()
}

#[test]
fn a_shared_job_runs_in_the_engine_host_and_is_recorded_as_its_apps() {
    let device = Device::new();
    let (app, generation, _link) = run_app(&device.processes, BACKUPS);
    let asked = job(BACKUPS, "j1", "echo", JobNeeds::default());
    device.processes.engine_start(&app, generation, &asked);

    let records = device.until("j1", "done");
    assert_eq!(states(&records), ["running", "done"]);
    let last = device.record("j1").unwrap();
    assert_eq!(last["app"], BACKUPS, "the host's job is the app's");
    assert_eq!(last["workspace"], WORKSPACE);
    assert_eq!(last["invocation"], "inv-1");
    assert_eq!(last["by"], "person-1");
    assert_eq!(last["detail"]["answer"], asked.input);
    assert_eq!(
        last["done"], asked.handlers["done"],
        "what the app asked to run once it ends is in its record"
    );
    // The host no longer runs anything, so it idles out like every process here: it is marked
    // idle just after the job's last record is kept, so the test waits for the mark.
    let host = device.engine_host().expect("the host ran it");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !device
        .processes
        .running()
        .values()
        .any(|process| process.generation == host && process.idle_since.is_some())
    {
        assert!(
            std::time::Instant::now() < deadline,
            "the engine host idles once its job ended"
        );
        std::thread::yield_now();
    }
}

#[test]
fn a_job_finishes_after_the_process_that_started_it_ended() {
    let device = Device::new();
    let (app, generation, link) = run_app(&device.processes, BACKUPS);
    let asked = job(BACKUPS, "j2", "slow", JobNeeds::default());
    device.processes.engine_start(&app, generation, &asked);
    device.until("j2", "running");
    // The app's window closes and its process ends while its job runs.
    device.processes.running().remove(&app);
    drop(link);

    assert_eq!(states(&device.until("j2", "done")), ["running", "done"]);
}

#[test]
fn a_job_needing_a_secret_or_the_network_is_refused_by_the_shared_host() {
    let device = Device::new();
    let (app, generation, _link) = run_app(&device.processes, BACKUPS);
    for (id, needs) in [
        (
            "secret",
            JobNeeds {
                secret: true,
                ..JobNeeds::default()
            },
        ),
        (
            "network",
            JobNeeds {
                network: true,
                ..JobNeeds::default()
            },
        ),
    ] {
        let asked = job(BACKUPS, id, "echo", needs);
        device.processes.engine_start(&app, generation, &asked);
        let records = device.until(id, "failed");
        assert_eq!(states(&records), ["failed"]);
        let why = records[0]["detail"]["why"].as_str().unwrap();
        assert!(why.contains("own process"), "{why}");
    }
    assert_eq!(
        device.engine_host(),
        None,
        "no host was started for refused work"
    );
}

#[test]
fn a_process_starts_jobs_for_its_own_app_alone_and_beneath_the_devices_records() {
    let device = Device::new();
    let (app, generation, _link) = run_app(&device.processes, NOTES);
    let elsewhere = job(BACKUPS, "theirs", "echo", JobNeeds::default());
    device.processes.engine_start(&app, generation, &elsewhere);
    assert_eq!(states(&device.until("theirs", "failed")), ["failed"]);
    assert_eq!(
        device.record("theirs").unwrap()["app"],
        NOTES,
        "recorded as the app that asked, never as the one it named"
    );
    let mut misnamed = job(NOTES, "x", "echo", JobNeeds::default());
    misnamed.job = device_job_path(&[0xff; 32], "x");
    device.processes.engine_start(&app, generation, &misnamed);
    assert_eq!(
        device.record("x"),
        None,
        "another device's job is not this one's"
    );
    assert_eq!(device.engine_host(), None);
}

#[test]
fn a_stop_reaches_the_engine_running_the_job() {
    let device = Device::new();
    let (app, generation, _link) = run_app(&device.processes, BACKUPS);
    let asked = job(BACKUPS, "j3", "wait", JobNeeds::default());
    device.processes.engine_start(&app, generation, &asked);
    device.until("j3", "running");
    let host = device.engine_host().expect("the host runs it");
    let host_key = HostKey {
        workspace: String::new(),
        app: String::new(),
        alone: Some(Alone::Engines),
        runtime: app.runtime.clone(),
        app_runtime: app.app_runtime.clone(),
    };
    assert!(
        device.processes.holds_jobs(&host_key, host),
        "the host is held while the job runs"
    );
    // Another app's process stops nothing of this one's.
    let (other, _, _other_link) = run_app(&device.processes, NOTES);
    device.processes.engine_cancel(&other, &asked.job);
    device.processes.engine_cancel(&app, &asked.job);

    assert_eq!(
        states(&device.until("j3", "cancelled")),
        ["running", "cancelled"]
    );
    assert!(!device.processes.holds_jobs(&host_key, host));
}

#[test]
fn a_job_in_the_apps_own_process_holds_it_until_it_ends() {
    let device = Device::new();
    let (app, generation, _link) = run_app(&device.processes, BACKUPS);
    let mut asked = job(
        BACKUPS,
        "own",
        "verify",
        JobNeeds {
            secret: true,
            network: true,
            app_state: false,
        },
    );
    asked.here = true;
    let record = |state: &str| json!({ "op": "verify", "state": state, "detail": {} });
    device.processes.engine_start(&app, generation, &asked);
    assert!(
        device.processes.holds_jobs(&app, generation),
        "its process is held while it runs"
    );
    assert_eq!(
        device.engine_host(),
        None,
        "the shared host runs none of it"
    );
    device
        .processes
        .job_moved(&app, generation, &asked.job, record("running"));
    // Another app's process tells nothing of this job: what it says is its own app's.
    let (other, other_generation, _other_link) = run_app(&device.processes, NOTES);
    device
        .processes
        .job_moved(&other, other_generation, &asked.job, record("done"));
    assert!(device.processes.holds_jobs(&app, generation));

    device
        .processes
        .job_moved(&app, generation, &asked.job, record("done"));
    assert!(!device.processes.holds_jobs(&app, generation));
    assert_eq!(states(&device.until("own", "done")), ["running", "done"]);
    assert_eq!(
        device.record("own").unwrap()["done"],
        asked.handlers["done"]
    );
}

#[test]
fn a_process_reports_moves_only_of_jobs_the_device_admitted_for_it() {
    let device = Device::new();
    let (app, generation, _link) = run_app(&device.processes, BACKUPS);
    // A job the device never admitted: nothing of it is recorded.
    let path = device_job_path(&DEVICE, "made-up");
    device.processes.job_moved(
        &app,
        generation,
        &path,
        json!({ "op": "echo", "state": "done", "detail": {} }),
    );
    assert_eq!(device.record("made-up"), None);
    // A job the engine host runs for another app: this app's process tells nothing of it.
    let asked = job(NOTES, "theirs", "wait", JobNeeds::default());
    let (notes, notes_generation, _notes_link) = run_app(&device.processes, NOTES);
    device
        .processes
        .engine_start(&notes, notes_generation, &asked);
    device.until("theirs", "running");
    device.processes.job_moved(
        &app,
        generation,
        &asked.job,
        json!({ "op": "wait", "state": "done", "detail": {} }),
    );
    let record = device.record("theirs").unwrap();
    assert_eq!(record["state"], "running", "{record}");
    assert_eq!(record["app"], NOTES);
}
