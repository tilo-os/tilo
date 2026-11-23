//! The layouts' reads of this device's workspace service, made off the session's loop.
//!
//! A read of a workspace's layout or of a person's view of it opens the resource in the service
//! and merges its history there; on a busy machine that takes seconds, and a loop waiting for it
//! reads no app's request to the window system meanwhile. Here every read runs on a thread of its
//! own. The loop asks for a resource whenever a look wants it, and the service's notice of each
//! write it took ([`tilo_workspace_service::workspaces::Workspaces::document_writes`]) asks for
//! the resource it reached; an answer that differs from the last one wakes the loop, which takes
//! it on its next pass. Nothing here is the resource: each answer replaces the last, and a look
//! reads the latest one.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError, mpsc};
use std::time::{Duration, Instant};

use tilo_peer_state::workspace_documents::{DeviceDocuments, DocumentRead};
use tilo_workspace_service::workspaces::ResourceWrite;
use tokio::sync::broadcast;

use super::{Key, NotRead};

/// The service's notice of the writes it takes, once it is open: `None` before then.
pub(super) type Notices = Box<dyn Fn() -> Option<broadcast::Receiver<ResourceWrite>> + Send>;

/// What the last read of a resource answered.
enum Answer {
    /// The resource whole, at its revision.
    Whole(Box<automerge::AutoCommit>, [u8; 32]),
    /// The service has nothing of it yet.
    Nothing,
    /// It is not readable yet: its keys or its checkpoint are on their way.
    Waiting,
    /// It could not be read.
    Failed(String),
}

impl Answer {
    /// Whether `self` says what `other` says: an answer that does not is news for the loop.
    fn same(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Whole(_, mine), Self::Whole(_, theirs)) => mine == theirs,
            (Self::Nothing, Self::Nothing) | (Self::Waiting, Self::Waiting) => true,
            (Self::Failed(mine), Self::Failed(theirs)) => mine == theirs,
            _ => false,
        }
    }
}

/// What the loop knows of one resource it follows.
#[derive(Default)]
struct Asked {
    /// The last read's answer; `None` until one finished.
    answer: Option<Answer>,
    /// Whether a read of it is waiting for the reading thread.
    queued: bool,
}

#[derive(Default)]
struct State {
    asked: HashMap<Key, Asked>,
    /// The reads asked for that have not answered yet.
    outstanding: usize,
    /// Whether an answer changed since the loop last looked.
    news: bool,
}

struct Shared {
    state: Mutex<State>,
    /// Told whenever a read answered.
    answered: Condvar,
    /// The session's loop, woken by an answer that changed.
    wake: tilo_service::Wake,
    /// Whether every write the service takes asks for the resources it reaches: a resource is
    /// then read again exactly when a write reached it, and no other device's write elsewhere
    /// is a reason to read it.
    following: AtomicBool,
}

impl Shared {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Ask the reading thread for `key`, once however often it is asked before the read starts.
    fn ask(&self, asks: &mpsc::Sender<Key>, key: &Key) {
        let mut state = self.lock();
        let asked = state.asked.entry(key.clone()).or_default();
        if asked.queued {
            return;
        }
        if asks.send(key.clone()).is_err() {
            return;
        }
        asked.queued = true;
        state.outstanding = state.outstanding.saturating_add(1);
    }

    /// Every resource the loop follows.
    fn followed(&self) -> Vec<Key> {
        self.lock().asked.keys().cloned().collect()
    }
}

/// The reads of the layouts and views a session follows, made on a thread of their own.
#[derive(Clone)]
pub(super) struct Reads {
    shared: Arc<Shared>,
    asks: mpsc::Sender<Key>,
    /// Where the service's notice of its writes is subscribed from, until it is.
    notices: Arc<Mutex<Option<Notices>>>,
}

impl Reads {
    /// Reads through `reads` on a thread of their own, waking the loop with `wake` when an answer
    /// changed, and asked again by each write `notices` tells of.
    pub(super) fn start(
        reads: Arc<dyn DeviceDocuments>,
        wake: tilo_service::Wake,
        notices: Option<Notices>,
    ) -> Self {
        let shared = Arc::new(Shared {
            state: Mutex::default(),
            answered: Condvar::new(),
            wake,
            following: AtomicBool::new(false),
        });
        let (asks, asked) = mpsc::channel::<Key>();
        let reading = Arc::clone(&shared);
        let started = std::thread::Builder::new()
            .name("layout reads".to_owned())
            .spawn(move || {
                while let Ok(key) = asked.recv() {
                    if let Some(asked) = reading.lock().asked.get_mut(&key) {
                        // A write that lands while this read is under way asks for another.
                        asked.queued = false;
                    }
                    let answer = read(reads.as_ref(), &key);
                    let mut state = reading.lock();
                    let asked = state.asked.entry(key).or_default();
                    let news = asked.answer.as_ref().is_none_or(|was| !was.same(&answer));
                    asked.answer = Some(answer);
                    state.outstanding = state.outstanding.saturating_sub(1);
                    state.news |= news;
                    drop(state);
                    reading.answered.notify_all();
                    if news {
                        (reading.wake)();
                    }
                }
            });
        if let Err(error) = started {
            tracing::error!(%error, "no thread reads the workspaces' layouts");
        }
        Self {
            shared,
            asks,
            notices: Arc::new(Mutex::new(notices)),
        }
    }

    /// Ask for `key` to be read, and answer what its latest read found, against `known`, the
    /// revision the loop holds of it: `Ok(None)` where nothing moved since or the service has
    /// nothing of it, [`NotRead::Waiting`] while no read of it answered yet.
    pub(super) fn read(
        &self,
        key: &Key,
        known: Option<[u8; 32]>,
    ) -> Result<Option<(automerge::AutoCommit, [u8; 32])>, NotRead> {
        self.follow_notices();
        self.shared.ask(&self.asks, key);
        let state = self.shared.lock();
        let answer = match state.asked.get(key).and_then(|asked| asked.answer.as_ref()) {
            None | Some(Answer::Waiting) => Err(NotRead::Waiting),
            Some(Answer::Failed(error)) => Err(NotRead::Failed(error.clone())),
            Some(Answer::Nothing) => Ok(None),
            Some(Answer::Whole(_, revision)) if known == Some(*revision) => Ok(None),
            Some(Answer::Whole(doc, revision)) => Ok(Some((doc.as_ref().clone(), *revision))),
        };
        drop(state);
        answer
    }

    /// Whether each write the service takes asks for the resources it reaches: from the first read
    /// after the service opened until the service closes.
    pub(super) fn follows_writes(&self) -> bool {
        self.shared.following.load(Ordering::Acquire)
    }

    /// Whether an answer changed since this was last asked: the loop then looks again, whatever
    /// else moved.
    pub(super) fn take_news(&self) -> bool {
        std::mem::take(&mut self.shared.lock().news)
    }

    /// Read every resource followed again and wait up to `patience` for the answers: what a
    /// session that is ending writes on.
    pub(super) fn read_through(&self, patience: Duration) {
        for key in self.shared.followed() {
            self.shared.ask(&self.asks, &key);
        }
        let deadline = Instant::now().checked_add(patience);
        let mut state = self.shared.lock();
        while state.outstanding > 0 {
            let left = deadline.map_or(patience, |deadline| {
                deadline.saturating_duration_since(Instant::now())
            });
            if left.is_zero() {
                break;
            }
            state = self
                .shared
                .answered
                .wait_timeout(state, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        drop(state);
    }

    /// Once the service is open, read again each resource followed that a write it took reaches.
    fn follow_notices(&self) {
        let mut notices = self.notices.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(mut writes) = notices.as_ref().and_then(|subscribe| subscribe()) else {
            return;
        };
        *notices = None;
        drop(notices);
        let runtime = match tilo_rt::try_app() {
            Ok(runtime) => runtime,
            Err(error) => {
                tracing::warn!(%error, "the layouts are read again only as the session looks");
                return;
            }
        };
        let shared = Arc::clone(&self.shared);
        let asks = self.asks.clone();
        shared.following.store(true, Ordering::Release);
        runtime.spawn(async move {
            use tokio::sync::broadcast::error::RecvError;
            loop {
                match writes.recv().await {
                    Ok(write) => {
                        for key in shared.followed() {
                            if reaches(&write, &key) {
                                shared.ask(&asks, &key);
                            }
                        }
                    }
                    // A write missed is in the resource: everything followed is read again.
                    Err(RecvError::Lagged(_)) => {
                        for key in shared.followed() {
                            shared.ask(&asks, &key);
                        }
                    }
                    Err(RecvError::Closed) => {
                        shared.following.store(false, Ordering::Release);
                        return;
                    }
                }
            }
        });
    }
}

/// Whether `write` is a write to `key`'s resource: its id on the wire is the route's.
fn reaches(write: &ResourceWrite, (workspace, route): &Key) -> bool {
    let ResourceWrite {
        workspace: written,
        app,
        record: resource,
        ..
    } = write;
    route.app == app && &route.document == resource && &workspace.to_string() == written
}

/// Read `key` through `reads`, on the reading thread.
fn read(reads: &dyn DeviceDocuments, (workspace, route): &Key) -> Answer {
    match reads.read(&workspace.to_string(), route.app, &route.document, None) {
        Ok(DocumentRead::Whole {
            revision, bytes, ..
        }) => match automerge::AutoCommit::load(&bytes) {
            Ok(doc) => Answer::Whole(Box::new(doc), revision),
            Err(error) => Answer::Failed(error.to_string()),
        },
        Ok(DocumentRead::Changes { .. } | DocumentRead::Unchanged(_) | DocumentRead::Absent) => {
            Answer::Nothing
        }
        Ok(DocumentRead::Pending) => Answer::Waiting,
        Err(error) => Answer::Failed(error),
    }
}
