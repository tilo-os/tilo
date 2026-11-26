//! Engine jobs as a tilolang process runs them on its device's link: the app's process hands work
//! that needs nothing of its own to the device, and runs the rest itself; the device's engine host
//! runs what it is handed, refuses what it may not run, and stops a job when asked. Every move is
//! said on the link as the job's record.
//!
//! A process has one link to its device: the tests that use it take it in turn ([`device`]).

use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use runtime_portal::process_link::{
    Descriptor, EngineJob, FromProcess, JobNeeds, Packet as _, pair,
};
use serde_json::{Value, json};
use tilolang_engine::{self as engine, JobOp, JobSink, Needs, Provider, State};
use tilolang_process::engines;

/// The device the tests' process runs on, and where it keeps its jobs' records.
pub(crate) const DEVICE: [u8; 32] = [0x0a; 32];
pub(crate) const UNDER: &str =
    "devices/0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a/jobs";

/// The device's end of this process's one link, once the process is attached to it: held by one
/// test at a time, with nothing left on it from the one before.
pub(crate) fn device() -> MutexGuard<'static, Descriptor> {
    use std::os::fd::AsRawFd as _;
    static DEVICE: OnceLock<Mutex<Descriptor>> = OnceLock::new();
    let device = DEVICE
        .get_or_init(|| {
            let (process, device) = pair().unwrap();
            engines::attach(Arc::new(process), Arc::new(|| {}));
            engines::jobs_under(UNDER.to_owned());
            Mutex::new(device)
        })
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    loop {
        let mut poll = libc::pollfd {
            fd: device.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one pollfd this test owns.
        if unsafe { libc::poll(&raw mut poll, 1, 0) } != 1 {
            return device;
        }
        let _ = FromProcess::receive(&device);
    }
}
const APP: &str = "so.tilo.engine-host-test";
const WORKSPACE: &str = "7a110d00-0000-4000-8000-0000000000c1";

/// An engine with work of each kind: `echo` and `wait` need nothing of the app's, `fetch` needs
/// its network and a secret.
struct Echo {
    waiting: Mutex<Vec<(String, JobSink)>>,
}

impl Provider for Echo {
    fn jobs(&self) -> &'static [JobOp] {
        const JOBS: &[JobOp] = &[
            JobOp::new("echo", Needs::NOTHING),
            JobOp::new("wait", Needs::NOTHING),
            JobOp::new("fetch", Needs::SECRET_AND_NETWORK),
        ];
        JOBS
    }

    fn call(&self, op: &str, _input: &str) -> Result<String, String> {
        Err(format!("no call `{op}`"))
    }

    fn start(&self, job: &str, op: &str, input: &str, moved: JobSink) -> Result<(), String> {
        moved.moved(State::Running, json!({ "step": 1 }));
        if op == "wait" {
            self.waiting.lock().unwrap().push((job.to_owned(), moved));
        } else {
            let input = input.to_owned();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(50));
                moved.moved(State::Done, json!({ "answer": input }));
            });
        }
        Ok(())
    }

    fn cancel(&self, job: &str) -> bool {
        let stopped = {
            let mut waiting = self.waiting.lock().unwrap();
            let at = waiting.iter().position(|(named, _)| named == job);
            let stopped = at.map(|at| waiting.remove(at).1);
            drop(waiting);
            stopped
        };
        let Some(moved) = stopped else {
            return false;
        };
        moved.moved(State::Cancelled, json!({}));
        true
    }
}

/// What the process said on its link next, waiting at most a minute.
pub(crate) fn said(device: &Descriptor) -> FromProcess {
    use std::os::fd::AsRawFd as _;
    let mut poll = libc::pollfd {
        fd: device.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one pollfd this test owns.
    let ready = unsafe { libc::poll(&raw mut poll, 1, 60_000) };
    assert_eq!(ready, 1, "the process said something");
    FromProcess::receive(device)
        .unwrap()
        .expect("the link is open")
}

/// The records of `job` the process said, until `last`.
fn moves(device: &Descriptor, job: &str, last: &str) -> Vec<Value> {
    let mut records = Vec::new();
    loop {
        let FromProcess::Job { job: named, record } = said(device) else {
            panic!("a job's moves come one after another");
        };
        assert_eq!(named, job);
        let done = record["state"] == last;
        records.push(record);
        if done {
            return records;
        }
    }
}

fn states(records: &[Value]) -> Vec<&str> {
    records
        .iter()
        .map(|record| record["state"].as_str().unwrap())
        .collect()
}

fn handed(id: &str, op: &str, needs: JobNeeds) -> EngineJob {
    EngineJob {
        job: format!("{UNDER}/{id}"),
        app: APP.to_owned(),
        workspace: WORKSPACE.to_owned(),
        invocation: "inv-1".to_owned(),
        by: "person-1".to_owned(),
        op: op.to_owned(),
        input: "{\"n\":1}".to_owned(),
        needs,
        here: false,
        handlers: Value::Null,
    }
}

#[test]
fn engine_jobs_run_where_their_ops_may_run_and_say_every_move() {
    engine::register(
        APP,
        Arc::new(Echo {
            waiting: Mutex::new(Vec::new()),
        }),
    );
    let device = device();
    let handlers = json!({ "done": { "action": "finished" } });
    let caller = engines::Caller {
        app: APP,
        workspace: WORKSPACE,
        invocation: "inv-1",
        by: "person-1",
        handlers: &handlers,
    };

    // As the device's engine host: a job it is handed runs, and says each move.
    let echo = handed("echo", "echo", JobNeeds::default());
    engines::run(&echo);
    let records = moves(&device, &echo.job, "done");
    assert_eq!(states(&records), ["running", "done"]);
    let last = records.last().unwrap();
    assert_eq!(last["id"], echo.job);
    assert_eq!(last["app"], APP);
    assert_eq!(last["invocation"], "inv-1");
    assert_eq!(last["by"], "person-1");
    assert_eq!(last["op"], "echo");
    assert_eq!(last["detail"]["answer"], echo.input);

    // Work that needs a secret and the network is refused by the shared host, even when the job
    // claims to need nothing: the host reads what the engine registered.
    let fetch = handed("fetch", "fetch", JobNeeds::default());
    engines::run(&fetch);
    let records = moves(&device, &fetch.job, "failed");
    assert_eq!(states(&records), ["failed"]);
    assert!(
        records[0]["detail"]["why"]
            .as_str()
            .unwrap()
            .contains("own process")
    );

    // A stop reaches the engine running the job.
    let wait = handed("wait", "wait", JobNeeds::default());
    engines::run(&wait);
    assert_eq!(states(&moves(&device, &wait.job, "running")), ["running"]);
    assert!(engines::busy(), "the host is held while its job runs");
    engines::stop(&wait.job);
    assert_eq!(
        states(&moves(&device, &wait.job, "cancelled")),
        ["cancelled"]
    );
    assert!(!engines::busy());

    // As the app's process: work that needs nothing of its own goes to the device, named beneath
    // its records, as the caller.
    let job = engines::start(&caller, "echo", "{}").unwrap();
    assert!(job.starts_with(&format!("{UNDER}/")), "{job}");
    let FromProcess::EngineStart(asked) = said(&device) else {
        panic!("the device is asked to run shared work");
    };
    assert_eq!(asked.job, job);
    assert_eq!(
        (asked.app.as_str(), asked.workspace.as_str()),
        (APP, WORKSPACE)
    );
    assert_eq!(
        (asked.invocation.as_str(), asked.by.as_str()),
        ("inv-1", "person-1")
    );
    assert!(asked.needs.shared());
    assert!(!asked.here);
    assert_eq!(asked.handlers, handlers);
    // A stop of a job the device runs goes to the device.
    let handed_wait = engines::start(&caller, "wait", "{}").unwrap();
    assert!(matches!(said(&device), FromProcess::EngineStart(_)));
    assert!(engines::cancel(&handed_wait));
    let FromProcess::EngineCancel { job } = said(&device) else {
        panic!("the device is asked to stop the job");
    };
    assert_eq!(job, handed_wait);

    // Work that needs a secret and the network runs in the app's own process, which says its
    // moves itself and is held until it ends.
    let own = engines::start(&caller, "fetch", "{}").unwrap();
    // The device is told of it first, so it records it and holds this process for it.
    let FromProcess::EngineStart(asked) = said(&device) else {
        panic!("the device is told of the job first");
    };
    assert_eq!(asked.job, own);
    assert!(asked.here);
    assert!(asked.needs.secret && asked.needs.network);
    assert_eq!(asked.handlers, handlers);
    let records = moves(&device, &own, "done");
    drop(device);
    assert_eq!(states(&records), ["running", "done"]);
    assert_eq!(records[1]["workspace"], WORKSPACE);
    assert!(!engines::busy(), "the process is let go once its job ended");

    // An op the engine never registered starts nowhere.
    assert!(engines::start(&caller, "paint", "{}").is_err());
}
