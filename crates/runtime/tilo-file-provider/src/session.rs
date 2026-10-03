//! One connection to the runtime's workspace, owned by a thread of its own.

use runtime_fs::{EntryKind, FsClient, Metadata, OpenMode};
use runtime_portal::NotAvailable;
use serde::Serialize;
use serde_json::Value;
use std::{
    io::{self, Write as _},
    path::{Path, PathBuf},
    sync::mpsc,
    thread::JoinHandle,
};

pub enum Request {
    List(String),
    Stat(String),
    Fetch { path: String, destination: PathBuf },
}

type Answer = Result<Value, String>;

/// Calls answered in order by the connection's thread.
pub struct Session {
    requests: Option<mpsc::Sender<(Request, mpsc::Sender<Answer>)>>,
    worker: Option<JoinHandle<()>>,
}

impl Session {
    pub fn open(socket: PathBuf) -> io::Result<Self> {
        let (requests, incoming) = mpsc::channel::<(Request, mpsc::Sender<Answer>)>();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let worker = std::thread::Builder::new()
            .name("tilo-file-provider".into())
            .spawn(move || {
                let local = tokio::task::LocalSet::new();
                let mut connection = None;
                for (request, reply) in incoming {
                    let answer =
                        local.block_on(&runtime, answer(&mut connection, &socket, &request));
                    let _ = reply.send(answer);
                }
            })?;
        Ok(Self {
            requests: Some(requests),
            worker: Some(worker),
        })
    }

    pub fn call(&self, request: Request) -> Answer {
        let (reply, answer) = mpsc::channel();
        self.requests
            .as_ref()
            .and_then(|requests| requests.send((request, reply)).ok())
            .ok_or("the session has stopped")?;
        answer
            .recv()
            .map_err(|_| "the session has stopped".to_owned())?
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        drop(self.requests.take());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[derive(Serialize)]
struct Item {
    name: String,
    kind: &'static str,
    size: u64,
    modified: i64,
}

impl Item {
    fn new(name: String, metadata: &Metadata) -> Self {
        Self {
            name,
            kind: match metadata.kind {
                EntryKind::File => "file",
                EntryKind::Directory => "directory",
                EntryKind::Symlink => "symlink",
                EntryKind::Other => "other",
            },
            size: metadata.size,
            modified: metadata.modified_unix_seconds.unwrap_or(0),
        }
    }
}

fn disconnected(error: &NotAvailable) -> bool {
    match error {
        NotAvailable::Capnp(error) => error.kind == capnp::ErrorKind::Disconnected,
        _ => false,
    }
}

/// Answers `request`, connecting first when there is no connection, and
/// once more on a fresh one when the runtime went away since the last call.
#[allow(
    clippy::future_not_send,
    reason = "the Cap'n Proto client is Rc-based and runs on this thread's LocalSet"
)]
async fn answer(connection: &mut Option<FsClient>, socket: &Path, request: &Request) -> Answer {
    let mut retried = false;
    loop {
        let fs = match connection {
            Some(fs) => fs.clone(),
            None => connection
                .insert(runtime_fs::connect(socket).map_err(|error| error.to_string())?)
                .clone(),
        };
        match run(&fs, request).await {
            Err(Failure::Rpc(error)) if disconnected(&error) && !retried => {
                *connection = None;
                retried = true;
            }
            Err(Failure::Rpc(error)) => {
                if disconnected(&error) {
                    *connection = None;
                }
                return Err(error.to_string());
            }
            Err(Failure::Local(error)) => return Err(error),
            Ok(value) => return Ok(value),
        }
    }
}

enum Failure {
    Rpc(NotAvailable),
    Local(String),
}

impl From<NotAvailable> for Failure {
    fn from(error: NotAvailable) -> Self {
        Self::Rpc(error)
    }
}

fn json(value: impl Serialize) -> Result<Value, Failure> {
    serde_json::to_value(value).map_err(|error| Failure::Local(error.to_string()))
}

#[allow(
    clippy::future_not_send,
    reason = "the Cap'n Proto client is Rc-based and runs on this thread's LocalSet"
)]
async fn run(fs: &FsClient, request: &Request) -> Result<Value, Failure> {
    match request {
        Request::List(path) => {
            let entries = fs.list(path).await?;
            json(
                entries
                    .into_iter()
                    .map(|entry| Item::new(entry.name, &entry.metadata))
                    .collect::<Vec<_>>(),
            )
        }
        Request::Stat(path) => {
            let metadata = fs.stat(path).await?;
            let name = path.rsplit('/').next().unwrap_or_default().to_owned();
            json(Item::new(name, &metadata))
        }
        Request::Fetch { path, destination } => {
            let file = fs.open(path, OpenMode::Read).await?;
            let mut out = std::fs::File::create_new(destination)
                .map_err(|error| Failure::Local(error.to_string()))?;
            let mut size = 0_u64;
            loop {
                let chunk = file.read_at(size, runtime_fs::MAX_READ_CHUNK).await?;
                if chunk.is_empty() {
                    break;
                }
                out.write_all(&chunk)
                    .map_err(|error| Failure::Local(error.to_string()))?;
                size = size.saturating_add(chunk.len() as u64);
            }
            out.sync_all()
                .map_err(|error| Failure::Local(error.to_string()))?;
            json(serde_json::json!({ "size": size }))
        }
    }
}
