//! The worker process: the caller's own executable, started again in worker
//! mode.
//!
//! It reads requests from descriptor 3 and writes replies to descriptor 4.
//! It keeps documents it has already built, keyed by their state, so an
//! incoming change costs only the change and not a reload of the document.
//! What it keeps is bounded by heap bytes ([`crate::Limits::cache_bytes`]),
//! and freed memory is handed back to the kernel after an eviction or a
//! rebuild. It exits after [`crate::Limits::idle`] without a request, and it
//! never outlives its caller's end of the request pipe. It confines itself
//! ([`crate::sandbox`]) before it reads a request.

use std::collections::VecDeque;
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
/// Set in the worker's environment to its [`Config`], as
/// `documents,cache_bytes,idle_seconds`.
pub(crate) const ENV: &str = "TILO_AUTOMERGE_WORKER";
/// Request and reply descriptors in the worker.
pub(crate) const REQUEST_FD: i32 = 3;
pub(crate) const REPLY_FD: i32 = 4;
/// How many recently consumed and shared states a worker remembers.
const REMEMBERED_STATES: usize = 32;

/// What bounds a worker's cache and lifetime.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Config {
    pub(crate) documents: usize,
    pub(crate) cache_bytes: u64,
    pub(crate) idle_seconds: u64,
}

impl Config {
    pub(crate) fn encode(self) -> String {
        format!(
            "{},{},{}",
            self.documents, self.cache_bytes, self.idle_seconds
        )
    }

    fn decode(value: &str) -> Option<Self> {
        let mut parts = value.split(',');
        let config = Self {
            documents: parts.next()?.parse().ok()?,
            cache_bytes: parts.next()?.parse().ok()?,
            idle_seconds: parts.next()?.parse().ok()?,
        };
        parts.next().is_none().then_some(config)
    }
}

/// Whether this process was started as a worker: the argument and the
/// environment together, so a stray variable alone never diverts a program.
pub(crate) fn requested() -> Option<Config> {
    if !std::env::args_os().any(|arg| arg == ARG) {
        return None;
    }
    Config::decode(&std::env::var(ENV).ok()?)
}

/// Serve requests until the caller closes its end or none comes for
/// [`Config::idle_seconds`]. Returns the exit code.
pub(crate) fn serve(config: Config) -> i32 {
    // SAFETY: the caller opened these two descriptors for this process
    // (see `pool::spawn`) and nothing else here uses them.
    let (requests, replies) =
        unsafe { (File::from_raw_fd(REQUEST_FD), File::from_raw_fd(REPLY_FD)) };
    let mut requests = BufReader::new(requests);
    let mut replies = BufWriter::new(replies);
    if let Err(reason) = crate::sandbox::confine() {
        let _ = wire::write_frame(&mut replies, format!("unconfined: {reason}").as_bytes());
        return 1;
    }
    if wire::write_frame(&mut replies, wire::READY).is_err() {
        return 1;
    }
    let mut cache = Cache::new(config);
    loop {
        // An idle worker is ended by `SIGALRM`, whose default action is to
        // terminate; the caller recognises it and starts a fresh one.
        idle_alarm(config.idle_seconds);
        let frame = wire::read_frame(&mut requests, u64::MAX);
        idle_alarm(0);
        let frame = match frame {
            Ok(Some(frame)) => frame,
            Ok(None) => return 0,
            Err(_) => return 1,
        };
        let Some(request) = Request::decode(&frame) else {
            return 1;
        };
        let reply = cache.handle(request);
        drop(frame);
        cache.enforce_budget();
        if wire::write_frame(&mut replies, &reply.encode()).is_err() {
            return 1;
        }
    }
}

/// Arm (or with 0, disarm) the one-shot real-time timer.
fn idle_alarm(seconds: u64) {
    let timer = libc::itimerval {
        it_interval: libc::timeval {
            tv_sec: 0,
            tv_usec: 0,
        },
        it_value: libc::timeval {
            tv_sec: libc::time_t::try_from(seconds).unwrap_or(libc::time_t::MAX),
            tv_usec: 0,
        },
    };
    // SAFETY: setitimer with a valid timer value and no old-value pointer.
    unsafe { libc::setitimer(libc::ITIMER_REAL, &raw const timer, std::ptr::null_mut()) };
}

/// Bytes the allocator has handed out, where it can say.
fn heap_in_use() -> Option<u64> {
    #[cfg(target_env = "gnu")]
    {
        // SAFETY: mallinfo2 only reads the allocator's statistics.
        let info = unsafe { libc::mallinfo2() };
        u64::try_from(info.uordblks.saturating_add(info.hblkhd)).ok()
    }
    #[cfg(not(target_env = "gnu"))]
    None
}

/// Return freed heap to the kernel. glibc keeps it otherwise, so a worker
/// that once held a large document would keep its footprint.
fn release_freed_memory() {
    #[cfg(target_env = "gnu")]
    // SAFETY: malloc_trim only releases memory the allocator holds free.
    unsafe {
        libc::malloc_trim(0);
    }
}

struct Entry {
    key: Key,
    doc: AutoCommit,
    used: u64,
}

/// Documents by state.
///
/// A state is normally consumed by the request that moves it forward: the
/// caller's document is now at the new state and nothing else needs the old
/// one. Two cases keep a copy as a base instead, since a clone costs about a
/// millisecond where a rebuild costs a full load: a state the worker just
/// rebuilt from a full save, and a state a second document asked for after
/// it was consumed (copies of one document, say). A base that no document
/// starts from again ages out like any other state.
struct Cache {
    entries: Vec<Entry>,
    config: Config,
    tick: u64,
    consumed: VecDeque<Key>,
    shared: VecDeque<Key>,
    rebuilt: bool,
}

fn remember(list: &mut VecDeque<Key>, key: &Key) {
    if !list.contains(key) {
        if list.len() >= REMEMBERED_STATES {
            list.pop_front();
        }
        list.push_back(key.clone());
    }
}

impl Cache {
    fn new(config: Config) -> Self {
        Self {
            entries: Vec::new(),
            config,
            tick: 0,
            consumed: VecDeque::new(),
            shared: VecDeque::new(),
            rebuilt: false,
        }
    }

    /// The document at `key`, for a request that will move it forward.
    fn take(&mut self, key: &Key) -> Option<AutoCommit> {
        key.complete_heads()?;
        if key.is_blank() {
            return Some(AutoCommit::new());
        }
        let Some(index) = self.entries.iter().position(|e| e.key == *key) else {
            if self.consumed.contains(key) {
                remember(&mut self.shared, key);
            }
            return None;
        };
        if self.shared.contains(key) {
            let entry = self.entries.get_mut(index)?;
            entry.used = self.tick;
            return Some(entry.doc.clone());
        }
        remember(&mut self.consumed, key);
        Some(self.entries.remove(index).doc)
    }

    /// Keep a copy of a state the worker had to rebuild, or that other
    /// documents share, as a base the next document at that state can start
    /// from.
    fn keep_base(&mut self, key: &Key, doc: &AutoCommit, rebuilt: bool) {
        if key.complete_heads().is_none() {
            return;
        }
        if (rebuilt || self.shared.contains(key)) && !self.entries.iter().any(|e| e.key == *key) {
            self.entries.push(Entry {
                key: key.clone(),
                doc: doc.clone(),
                used: self.tick,
            });
        }
    }

    fn keep(&mut self, mut doc: AutoCommit) {
        let key = Key::of(&mut doc);
        if key.complete_heads().is_none() {
            return;
        }
        self.entries.retain(|e| e.key != key);
        self.entries.push(Entry {
            key,
            doc,
            used: self.tick,
        });
    }

    fn keys(&self) -> Vec<Key> {
        let mut entries: Vec<&Entry> = self.entries.iter().collect();
        entries.sort_by_key(|e| std::cmp::Reverse(e.used));
        entries.into_iter().map(|e| e.key.clone()).collect()
    }

    /// Evict until the heap fits [`Config::cache_bytes`] and at most
    /// [`Config::documents`] remain, least recently used first; what the
    /// last request used or produced stays. Then give freed memory
    /// back if anything was dropped or rebuilt.
    fn enforce_budget(&mut self) {
        let mut evicted = false;
        loop {
            let over_count = self.entries.len() > self.config.documents.max(1);
            let over_bytes = heap_in_use().is_some_and(|b| b > self.config.cache_bytes);
            if !(over_count || over_bytes) {
                break;
            }
            let victim = self
                .entries
                .iter()
                .enumerate()
                .filter(|(_, e)| e.used != self.tick)
                .min_by_key(|(_, e)| e.used)
                .map(|(index, _)| index);
            let Some(victim) = victim else {
                break;
            };
            self.entries.remove(victim);
            evicted = true;
        }
        if evicted || std::mem::take(&mut self.rebuilt) {
            release_freed_memory();
        }
    }

    fn handle(&mut self, request: Request<'_>) -> Reply {
        self.tick = self.tick.saturating_add(1);
        match request {
            #[cfg(test)]
            Request::Escape(escape) => try_escape(escape),
            Request::ChangeHashes { incoming } => match engine::change_hashes(incoming) {
                Ok(hashes) => Reply::ChangeHashes(hashes),
                Err(error) => invalid(error),
            },
            Request::Load { incoming } => {
                self.rebuilt = true;
                match engine::load(incoming) {
                    Ok(doc) => {
                        self.keep(doc);
                        Reply::Applied(Applied::Changed(true))
                    }
                    Err(error) => invalid(error),
                }
            }
            Request::Apply {
                key,
                dependencies,
                prepare,
                incoming,
            } => {
                let rebuilt = matches!(prepare, Prepare::Full(_));
                let base = match prepare {
                    Prepare::Cached => self.take(&key),
                    Prepare::Full(saved) => {
                        self.rebuilt = true;
                        match engine::load(saved) {
                            Ok(doc) => Some(doc),
                            Err(error) => return invalid(error),
                        }
                    }
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
                self.keep_base(&key, &doc, rebuilt);
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

/// Reply `Applied` if the escape worked, `Invalid` with the error if the
/// kernel refused it; a worker the sandbox kills does not reply at all.
#[cfg(test)]
fn try_escape(escape: wire::Escape) -> Reply {
    let fd = match escape {
        // SAFETY: openat(2) on a NUL-terminated path.
        wire::Escape::Open => unsafe {
            libc::openat(
                libc::AT_FDCWD,
                c"/proc/self/status".as_ptr(),
                libc::O_RDONLY,
            )
        },
        // SAFETY: socket(2) with constant arguments.
        wire::Escape::Socket => unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) },
    };
    if fd < 0 {
        return Reply::Invalid(std::io::Error::last_os_error().to_string());
    }
    Reply::Applied(Applied::Changed(true))
}

fn invalid(error: Rejected) -> Reply {
    Reply::Invalid(match error {
        Rejected::Invalid(message) => message,
        other => other.to_string(),
    })
}
