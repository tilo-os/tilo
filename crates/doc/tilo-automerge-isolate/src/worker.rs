//! The worker process: the caller's own executable, started again in worker
//! mode.
//!
//! It reads requests from descriptor 3 and writes replies to descriptor 4.
//! It keeps a few documents it has already built, keyed by their state, so an
//! incoming change costs only the change and not a reload of the document.
//! It never outlives its caller's end of the request pipe.

use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::os::fd::FromRawFd as _;

use automerge::AutoCommit;

use crate::engine::{self, Key};
use crate::wire::{self, Prepare, Reply, Request};
use crate::{Applied, Dependencies, Rejected};

/// The first argument of a worker. It is also the name of the test
/// `test_worker!` defines, so libtest runs exactly that test.
pub(crate) const ARG: &str = "tilo_automerge_isolate_worker";
/// Set in the worker's environment to how many documents it may keep.
pub(crate) const ENV: &str = "TILO_AUTOMERGE_WORKER";
/// Request and reply descriptors in the worker.
pub(crate) const REQUEST_FD: i32 = 3;
pub(crate) const REPLY_FD: i32 = 4;

/// Whether this process was started as a worker: the argument and the
/// environment together, so a stray variable alone never diverts a program.
pub(crate) fn requested() -> Option<usize> {
    if !std::env::args_os().any(|arg| arg == ARG) {
        return None;
    }
    std::env::var(ENV).ok()?.parse().ok()
}

/// Serve requests until the caller closes its end. Returns the exit code.
pub(crate) fn serve(capacity: usize) -> i32 {
    // SAFETY: the caller opened these two descriptors for this process
    // (see `pool::spawn`) and nothing else here uses them.
    let (requests, replies) =
        unsafe { (File::from_raw_fd(REQUEST_FD), File::from_raw_fd(REPLY_FD)) };
    let mut requests = BufReader::new(requests);
    let mut replies = BufWriter::new(replies);
    if wire::write_frame(&mut replies, wire::READY).is_err() {
        return 1;
    }
    let mut cache = Cache::new(capacity);
    loop {
        let frame = match wire::read_frame(&mut requests, u64::MAX) {
            Ok(Some(frame)) => frame,
            Ok(None) => return 0,
            Err(_) => return 1,
        };
        let Some(request) = Request::decode(&frame) else {
            return 1;
        };
        let reply = cache.handle(request);
        if wire::write_frame(&mut replies, &reply.encode()).is_err() {
            return 1;
        }
    }
}

/// Documents by state, most recently used first.
struct Cache {
    documents: Vec<(Key, AutoCommit)>,
    capacity: usize,
}

impl Cache {
    fn new(capacity: usize) -> Self {
        Self {
            documents: Vec::new(),
            capacity: capacity.max(1),
        }
    }

    fn take(&mut self, key: &Key) -> Option<AutoCommit> {
        let index = self.documents.iter().position(|(k, _)| k == key)?;
        Some(self.documents.remove(index).1)
    }

    fn keep(&mut self, mut doc: AutoCommit) {
        let key = Key::of(&mut doc);
        self.documents.retain(|(k, _)| *k != key);
        self.documents.insert(0, (key, doc));
        self.documents.truncate(self.capacity);
    }

    fn keys(&self) -> Vec<Key> {
        self.documents.iter().map(|(k, _)| k.clone()).collect()
    }

    fn handle(&mut self, request: Request<'_>) -> Reply {
        match request {
            Request::Load { incoming } => match engine::load(incoming) {
                Ok(doc) => {
                    self.keep(doc);
                    Reply::Applied(Applied::Changed(true))
                }
                Err(error) => invalid(error),
            },
            Request::Apply {
                key,
                dependencies,
                prepare,
                incoming,
            } => {
                let base = match prepare {
                    Prepare::Cached => self.take(&key),
                    Prepare::Full(saved) => match engine::load(saved) {
                        Ok(doc) => Some(doc),
                        Err(error) => return invalid(error),
                    },
                    Prepare::After { from, delta } => self.take(&from).and_then(|mut doc| {
                        engine::apply_trusted(&mut doc, delta, Dependencies::RetainOrphans)
                            .is_ok()
                            .then_some(doc)
                    }),
                };
                let Some(mut doc) =
                    base.and_then(|mut doc| (Key::of(&mut doc) == key).then_some(doc))
                else {
                    return Reply::Uncached(self.keys());
                };
                match engine::apply(&mut doc, incoming, dependencies) {
                    Ok(Applied::Changed(changed)) => {
                        self.keep(doc);
                        Reply::Applied(Applied::Changed(changed))
                    }
                    // The copy now holds changes the caller will not take.
                    Ok(Applied::MissingDependencies) => {
                        Reply::Applied(Applied::MissingDependencies)
                    }
                    Err(error) => invalid(error),
                }
            }
        }
    }
}

fn invalid(error: Rejected) -> Reply {
    Reply::Invalid(match error {
        Rejected::Invalid(message) => message,
        other => other.to_string(),
    })
}
