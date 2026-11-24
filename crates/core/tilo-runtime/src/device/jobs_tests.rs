//! The device's engine jobs: read and followed through the one record API, in its own record
//! space, and ended with the process that ran them.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tilo_actions::remote::{
    ActionCallError, Caller, ReadAnswer, RemoteActionMessage, ResourceCopy, ResourceReadRequest,
    ResourceReads, device_job_address, device_job_path, device_record_space, parse_job_path,
};

use super::{DeviceJobs, JobEffects, ProcessKey};

const DEVICE: [u8; 32] = [7; 32];
const PERSON: [u8; 32] = [9; 32];
const STRANGER: [u8; 32] = [1; 32];

fn process() -> ProcessKey {
    ProcessKey {
        workspace: "0a0a0a0a-0a0a-4a0a-8a0a-0a0a0a0a0a0a".to_owned(),
        app: "so.tilo.video".to_owned(),
        generation: 1,
    }
}

/// The device's own caller.
fn device() -> Caller {
    Caller::authenticated(PERSON, DEVICE)
}

/// The value a reading of `job` holds, as the record API answers it.
async fn reading(
    jobs: &Arc<DeviceJobs>,
    caller: &Caller,
    job: &str,
    after: Option<String>,
    wait_ms: Option<u32>,
) -> Value {
    let request = ResourceReadRequest {
        workspace: device_record_space(&DEVICE),
        app: tilo_actions::remote::DEVICE_JOBS.to_owned(),
        record: job.to_owned(),
        joined: false,
        after,
        wait_ms,
        answer: ReadAnswer::Reading,
    };
    let answer = Arc::clone(jobs)
        .answer_read(caller, 0, request)
        .await
        .expect("the device answers the read");
    let RemoteActionMessage::ResourceReading { result, .. } = answer else {
        panic!("a read is answered by a reading");
    };
    let readings: Value = serde_json::from_str(&result.expect("the device reads its job"))
        .expect("a reading reads as JSON");
    readings
}

/// The revision a reading of `job` stands at, and the job it holds.
async fn revision(jobs: &Arc<DeviceJobs>, caller: &Caller, job: &str) -> (String, Value) {
    let readings = reading(jobs, caller, job, None, None).await;
    (
        readings["revision"]
            .as_str()
            .expect("a reading names its revision")
            .to_owned(),
        readings["records"][0]["value"].clone(),
    )
}

/// What the daemon did for the jobs' invocations.
#[derive(Default)]
struct Recorded {
    held: Vec<String>,
    released: Vec<String>,
    progress: Vec<Value>,
    ended: Vec<Value>,
}

struct Effects(Arc<Mutex<Recorded>>);

impl JobEffects for Effects {
    fn progress(&self, record: &Value) {
        self.0.lock().unwrap().progress.push(record.clone());
    }

    fn ended(&self, record: &Value) {
        self.0.lock().unwrap().ended.push(record.clone());
    }

    fn hold(&self, invocation: &str) {
        self.0.lock().unwrap().held.push(invocation.to_owned());
    }

    fn release(&self, invocation: &str) {
        self.0.lock().unwrap().released.push(invocation.to_owned());
    }
}

fn running() -> Value {
    json!({
        "state": "running",
        "op": "cut",
        "invocation": "1a1a1a1a-1a1a-4a1a-8a1a-1a1a1a1a1a1a",
        "by": { "who": "person" },
        "detail": { "done": 1, "of": 10 },
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_held_read_wakes_when_the_job_moves() {
    let jobs = DeviceJobs::new(DEVICE);
    let caller = device();
    jobs.moved(&process(), "render-1", running());
    let (after, value) = revision(&jobs, &caller, "render-1").await;
    assert_eq!(value["state"], "running");

    let follow = {
        let jobs = Arc::clone(&jobs);
        let caller = caller.clone();
        tokio::spawn(
            async move { reading(&jobs, &caller, "render-1", Some(after), Some(5_000)).await },
        )
    };
    // Long enough for the follow to be watching, so the move has to wake it.
    tokio::time::sleep(Duration::from_millis(100)).await;
    jobs.moved(
        &process(),
        "render-1",
        json!({ "state": "done", "detail": { "url": "tilo-live://render-1" } }),
    );
    let readings = follow.await.expect("the follow ran");
    assert_eq!(
        readings["records"][0]["value"]["state"], "done",
        "the follow woke on the move and read the job's end"
    );
    // The record keeps the daemon's own naming of whose job it is.
    assert_eq!(readings["records"][0]["value"]["id"], "render-1");
    assert_eq!(readings["records"][0]["value"]["app"], "so.tilo.video");
    assert_eq!(
        readings["records"][0]["value"]["workspace"],
        process().workspace
    );

    // A job that ended stays readable: the answer a person waited for is never taken away.
    assert_eq!(
        jobs.read(&caller, &device_job_address(&DEVICE, "render-1"))
            .expect("the device reads its own job")
            .readable()
            .expect("a job reads as its JSON"),
        readings["records"][0]["value"]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_caller_who_may_not_read_the_jobs_app_reads_no_job() {
    let jobs = DeviceJobs::new(DEVICE);
    let caller = device();
    jobs.moved(&process(), "render-1", running());
    let address = device_job_address(&DEVICE, "render-1");
    // The device itself reads its own jobs.
    assert!(
        jobs.read(&caller, &address).is_ok(),
        "the device reads its own job"
    );
    // Anybody else is decided by the app the job runs in its workspace, which no stranger may
    // read: with no workspace admitted here, nobody else may.
    let refused = jobs
        .read(&Caller::authenticated(STRANGER, [2; 32]), &address)
        .expect_err("a stranger reads no job");
    assert!(
        matches!(refused, ActionCallError::Refused { .. }),
        "a stranger is refused: {refused:?}"
    );
    // A job the device runs none of is absent to the device, not refused.
    assert_eq!(
        jobs.read(&caller, &device_job_address(&DEVICE, "no-such-job"))
            .expect("the device reads its own space"),
        ResourceCopy::Absent
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_process_that_ended_leaves_its_unfinished_jobs_failed() {
    let jobs = DeviceJobs::new(DEVICE);
    let caller = device();
    jobs.moved(&process(), "render-1", running());
    jobs.moved(
        &process(),
        "render-2",
        json!({ "state": "done", "detail": { "url": "tilo-live://render-2" } }),
    );
    let other = ProcessKey {
        workspace: process().workspace,
        app: "so.tilo.video".to_owned(),
        generation: process().generation,
    };
    assert_eq!(jobs.live(&other), 1, "one job had not ended");
    // A later run of the same app in the same workspace is another process: its jobs are not
    // the ended run's.
    let again = ProcessKey {
        workspace: process().workspace,
        app: "so.tilo.video".to_owned(),
        generation: process().generation + 1,
    };
    jobs.moved(&again, "render-3", json!({ "state": "running" }));
    assert_eq!(jobs.live(&again), 1);

    jobs.ended_process(&other);
    assert_eq!(
        jobs.live(&again),
        1,
        "a later run's job does not end with this one"
    );
    assert_eq!(jobs.live(&other), 0);
    assert_eq!(
        jobs.read(&caller, &device_job_address(&DEVICE, "render-1"))
            .expect("the device reads its job")
            .readable()
            .expect("a job reads as its JSON")["state"],
        "failed",
        "a job does not outlive the process that ran it"
    );
    assert_eq!(
        jobs.read(&caller, &device_job_address(&DEVICE, "render-2"))
            .expect("the device reads its job")
            .readable()
            .expect("a job reads as its JSON")["state"],
        "done",
        "a job that ended keeps the answer it gave"
    );
}

#[test]
fn a_job_never_leaves_an_ended_state() {
    let jobs = DeviceJobs::new(DEVICE);
    let caller = device();
    jobs.moved(
        &process(),
        "render-1",
        json!({ "state": "cancelled", "detail": {} }),
    );
    jobs.moved(&process(), "render-1", running());
    assert_eq!(
        jobs.read(&caller, &device_job_address(&DEVICE, "render-1"))
            .expect("the device reads its job")
            .readable()
            .expect("a job reads as its JSON")["state"],
        "cancelled"
    );
}

#[test]
fn cancelling_the_invocation_cancels_the_jobs_it_started() {
    let jobs = DeviceJobs::new(DEVICE);
    jobs.moved(&process(), "render-1", running());
    jobs.moved(&process(), "render-2", running());
    jobs.moved(
        &process(),
        "render-3",
        json!({
            "state": "running",
            "invocation": "2b2b2b2b-2b2b-4b2b-8b2b-2b2b2b2b2b2b",
        }),
    );
    assert_eq!(
        jobs.cancel_invocation("1a1a1a1a-1a1a-4a1a-8a1a-1a1a1a1a1a1a"),
        2
    );
    let caller = device();
    assert_eq!(
        jobs.read(&caller, &device_job_address(&DEVICE, "render-1"))
            .expect("the device reads its job")
            .readable()
            .expect("a job reads as its JSON")["state"],
        "cancelled"
    );
    assert_eq!(
        jobs.read(&caller, &device_job_address(&DEVICE, "render-3"))
            .expect("the device reads its job")
            .readable()
            .expect("a job reads as its JSON")["state"],
        "running",
        "another invocation's job is not cancelled"
    );
    assert_eq!(
        jobs.cancel_invocation("1a1a1a1a-1a1a-4a1a-8a1a-1a1a1a1a1a1a"),
        0,
        "a cancelled job is not cancelled twice"
    );
}

#[test]
fn the_daemon_holds_the_invocation_and_runs_the_progress_and_done_handlers() {
    let jobs = DeviceJobs::new(DEVICE);
    let recorded = Arc::new(Mutex::new(Recorded::default()));
    jobs.serve_effects(Arc::new(Effects(Arc::clone(&recorded))));
    jobs.moved(&process(), "render-1", running());
    jobs.moved(
        &process(),
        "render-1",
        json!({ "state": "queued", "detail": { "done": 2, "of": 10 } }),
    );
    jobs.moved(
        &process(),
        "render-1",
        json!({ "state": "done", "detail": { "url": "tilo-live://render-1" } }),
    );
    jobs.moved(
        &process(),
        "render-1",
        json!({ "state": "done", "detail": { "url": "tilo-live://render-1" } }),
    );
    let recorded = recorded.lock().unwrap();
    assert_eq!(
        recorded.progress.len(),
        1,
        "a job's first word is its start; each move after it is progress"
    );
    assert_eq!(recorded.ended.len(), 1, "the done handler runs once");
    assert!(
        recorded
            .held
            .iter()
            .all(|held| held == "1a1a1a1a-1a1a-4a1a-8a1a-1a1a1a1a1a1a"),
        "the invocation the job started is held while it runs"
    );
    assert_eq!(
        recorded.released,
        vec!["1a1a1a1a-1a1a-4a1a-8a1a-1a1a1a1a1a1a".to_owned()],
        "its last job ended: the invocation is released"
    );
    drop(recorded);
}

#[test]
fn a_job_is_addressed_as_a_path_of_its_device() {
    let path = device_job_path(&DEVICE, "render-1");
    assert_eq!(
        path,
        format!("devices/{}/jobs/render-1", hex::encode(DEVICE))
    );
    assert_eq!(parse_job_path(&path), Some((DEVICE, "render-1".to_owned())));
    assert_eq!(parse_job_path("devices/not-hex/jobs/job"), None);
    assert_eq!(parse_job_path("devices/abc/jobs/job"), None);
    assert_eq!(
        parse_job_path(&format!("devices/{}/jobs/", hex::encode(DEVICE))),
        None
    );
    assert_eq!(
        parse_job_path(&format!("devices/{}/jobs/a/b", hex::encode(DEVICE))),
        None
    );
}
