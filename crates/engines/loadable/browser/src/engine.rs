//! The Browser's engine, as its package reaches it: `engine.start("download", …)` answered for
//! `so.tilo.browser` in the app's own process, under its network grant.
//!
//! A download is a job of its own, not the page's: the page that offered it hands over what its
//! request would have carried and lets go, so closing the page's window ends nothing. The job is
//! the one writer of its progress, told through its sink, and ends saved (`{path, name}`),
//! failed (`{error}`) or cancelled.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use tilo_engine_loader::{Device, EngineApi, Frames, JobOp, JobSink, Needs};

use crate::download::{self, Ask, Move, Stop};

/// The one app this engine answers.
pub const APP: &str = "so.tilo.browser";

/// One download this process started: how to stop it, and where it stands.
struct Running {
    stop: Stop,
    standing: Mutex<Standing>,
}

/// Where a download stands, as its moves told it: every field, whichever move told it last.
#[derive(Clone, serde::Serialize)]
struct Standing {
    state: &'static str,
    done: u64,
    total: u64,
    path: String,
    name: String,
    error: String,
}

impl Standing {
    fn moved(&mut self, moved: &Move) {
        self.state = moved.state();
        match moved {
            Move::Running { done, total } => {
                self.done = *done;
                self.total = *total;
            }
            Move::Done { path, name } => {
                self.path = path.to_string_lossy().into_owned();
                self.name.clone_from(name);
            }
            Move::Failed { error } => self.error.clone_from(error),
            Move::Cancelled => {}
        }
    }
}

fn jobs() -> &'static Mutex<BTreeMap<String, Arc<Running>>> {
    static JOBS: OnceLock<Mutex<BTreeMap<String, Arc<Running>>>> = OnceLock::new();
    JOBS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// One `engine.call`: this engine answers none now, every one of its ops is a job.
fn call(_device: &Device, op: &str, _input: &str) -> Result<String, String> {
    Err(format!("`{op}` is no op this device answers for {APP}"))
}

/// One `engine.start`: a download, fetched into the person's Downloads as the job `job`.
///
/// # Errors
///
/// An op that is no download, an ask that names no web address, or a device with no Downloads.
fn start(device: &Device, job: &str, op: &str, input: &str, sink: &JobSink) -> Result<(), String> {
    if op != "download" {
        return Err(format!("`{op}` is no op this device runs for {APP}"));
    }
    let (ask, url) = Ask::read(input, |handle| device.redeem(handle))?;
    let folder = tilo_paths::download_dir()
        .ok_or_else(|| "This device has no Downloads folder".to_owned())?;
    let running = Arc::new(Running {
        stop: Stop::default(),
        standing: Mutex::new(Standing {
            state: "running",
            done: 0,
            total: 0,
            path: String::new(),
            name: String::new(),
            error: String::new(),
        }),
    });
    jobs()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(job.to_owned(), Arc::clone(&running));
    // The sink stays the host's to read until the job tells it an ending move.
    let sink = *sink;
    let spawned = std::thread::Builder::new()
        .name("browser-download".into())
        .spawn(move || {
            let mut tell = |moved: &Move| {
                running
                    .standing
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .moved(moved);
                sink.moved(moved.state(), &moved.detail());
            };
            // The download stays known once it ended, so where it stands can still be read.
            download::run(&ask, &url, &folder, &running.stop, &mut tell);
        });
    if let Err(error) = spawned {
        jobs()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(job);
        return Err(format!("The download could not start: {error}"));
    }
    Ok(())
}

/// Where a download stands: every field, as its moves told them.
///
/// # Errors
///
/// No such download was started in this process.
fn status(_device: &Device, job: &str) -> Result<String, String> {
    let running = jobs()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(job)
        .cloned()
        .ok_or_else(|| format!("no download `{job}` was started here"))?;
    let standing = running
        .standing
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone();
    serde_json::to_string(&standing).map_err(|error| error.to_string())
}

/// Stop a download: false when it is not this engine's or it had ended.
fn cancel(_device: &Device, job: &str) -> bool {
    let running = jobs()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(job)
        .cloned();
    running.is_some_and(|running| {
        let going = running
            .standing
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .state
            == "running";
        if going {
            running.stop.stop();
        }
        going
    })
}

/// The jobs this engine runs: a download reaches the network under the app's own grant, so it
/// runs in the app's process.
fn job_ops() -> &'static [JobOp] {
    const JOBS: &[JobOp] = &[JobOp::new("download", Needs::NETWORK)];
    JOBS
}

/// One stream: this engine runs none.
fn stream(_device: &Device, _op: &str, _input: &str, _frames: &Frames) -> Result<String, String> {
    Err("this engine runs no streams".to_owned())
}

/// The one engine this library answers with.
static API: EngineApi = EngineApi {
    app: APP,
    jobs: job_ops,
    call,
    start,
    status,
    stream,
    cancel,
};

/// The entry the process opens this library for.
#[unsafe(no_mangle)]
pub fn tilo_engine_entry() -> &'static EngineApi {
    &API
}

/// Proves this engine loads and runs against the runtime it is built with: a file a loopback
/// server sends is downloaded whole into a folder of the check's own, through the network client
/// this engine uses.
pub fn self_check() -> Result<String, String> {
    use std::io::{Read as _, Write as _};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").map_err(|error| error.to_string())?;
    let port = listener
        .local_addr()
        .map_err(|error| error.to_string())?
        .port();
    let server = std::thread::spawn(move || {
        if let Ok((mut stream, _)) = listener.accept() {
            let mut head = [0_u8; 4096];
            let _ = stream.read(&mut head);
            let _ = stream.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello",
            );
        }
    });
    let folder =
        std::env::temp_dir().join(format!("tilo-browser-engine-check-{}", std::process::id()));
    let input =
        serde_json::json!({ "url": format!("http://127.0.0.1:{port}/check.txt") }).to_string();
    let (ask, url) = Ask::read(&input, |_| None)?;
    let ended = download::run(&ask, &url, &folder, &Stop::default(), &mut |_| {});
    let _ = server.join();
    let read = match &ended {
        Move::Done { path, .. } => std::fs::read(path).map_err(|error| error.to_string()),
        other => Err(format!("the download ended {other:?}")),
    };
    let _ = std::fs::remove_dir_all(&folder);
    if read? == b"hello" {
        Ok("downloaded a file from a loopback server".to_owned())
    } else {
        Err("the downloaded file is not what the server sent".to_owned())
    }
}
