//! The layouts' reads of the device's workspaces, as its daemon keeps them, made off the session's
//! loop.
//!
//! A read of a workspace's layout or of a person's view of it opens the resource in the daemon
//! and merges its history there; on a busy machine that takes seconds, and a loop waiting for it
//! reads no app's request to the window system meanwhile. Here every read runs on a thread of its
//! own. The daemon's notice of each write it took
//! ([`tilo_session_client::daemon_documents::DaemonWritesFeed`]) asks for the resource it reached,
//! and a look asks only for what no read answered yet, or what the notices cannot be trusted to
//! tell of: everything while they are not followed, and a resource that is not readable yet. An
//! answer that differs from the last one wakes the loop, which takes it on its next pass. Nothing here is the resource: each answer replaces the last, and a look
//! reads the latest one.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, PoisonError, mpsc};
use std::time::{Duration, Instant};

use runtime_collab::wire::shell::DocumentWritten;
use tilo_peer_state::workspace_documents::{DeviceDocuments, DocumentRead};
use tokio::sync::broadcast;

use super::{Key, NotRead};

/// The service's notice of the writes it takes, once it is open: `None` before then.
pub(super) type Notices = Box<dyn Fn() -> Option<broadcast::Receiver<DocumentWritten>> + Send>;

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
    /// Whether the service held the resource's workspace when the last read answered nothing of
    /// the resource: a workspace it does not hold is one it may have no documents of yet.
    workspace_held: bool,
    /// Whether a read of it is waiting for the reading thread.
    queued: bool,
    /// How many writes the service told of reaching it.
    told: u64,
    /// How many of those had been told when the read that gave [`Self::answer`] began: an answer
    /// read before the last write told of is no longer what the service holds.
    answered_after: u64,
}

/// What the service holds of a resource and its workspace, as the latest read of it found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Holding {
    /// The service holds the resource: whole, or on its way (its keys or its checkpoint have not
    /// arrived yet).
    pub(super) resource: bool,
    /// The service holds the resource's workspace.
    pub(super) workspace: bool,
}

#[derive(Default)]
struct State {
    asked: HashMap<Key, Asked>,
    /// The reads asked for that have not answered yet.
    outstanding: usize,
    /// Whether an answer changed since the loop last looked.
    news: bool,
    /// Whether the service's notice of its writes is followed: a read that answered is then read
    /// again when a write reaches it, and a look does not ask for it again.
    following: bool,
}

struct Shared {
    state: Mutex<State>,
    /// Told whenever a read answered.
    answered: Condvar,
    /// The session's loop, woken by an answer that changed.
    wake: tilo_service::Wake,
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

    /// A look at `key` asks for it to be read, unless a read already answered what the service
    /// holds of it and the service's notices say when that changes: a desktop that looks at its
    /// layouts on every pass reads each once, not on every pass.
    fn look(&self, asks: &mpsc::Sender<Key>, key: &Key) {
        let answered = {
            let state = self.lock();
            state.following
                && state.asked.get(key).is_some_and(|asked| {
                    matches!(asked.answer, Some(Answer::Whole(..) | Answer::Nothing))
                })
        };
        if !answered {
            self.ask(asks, key);
        }
    }

    /// Whether the service's notices are followed. Once they are, every resource followed is read
    /// again: a write it took before they were is in no notice.
    fn following(&self, asks: &mpsc::Sender<Key>, following: bool) {
        self.lock().following = following;
        if following {
            for key in self.followed() {
                self.told(asks, &key);
            }
        }
    }

    /// The service took a write to `key`'s resource: what was read of it before is no longer
    /// what it holds, and it is read again.
    fn told(&self, asks: &mpsc::Sender<Key>, key: &Key) {
        if let Some(asked) = self.lock().asked.get_mut(key) {
            asked.told = asked.told.wrapping_add(1);
        }
        self.ask(asks, key);
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
        });
        let (asks, asked) = mpsc::channel::<Key>();
        let reading = Arc::clone(&shared);
        let started = std::thread::Builder::new()
            .name("layout reads".to_owned())
            .spawn(move || {
                while let Ok(key) = asked.recv() {
                    let begun = reading.lock().asked.get_mut(&key).map(|asked| {
                        // A write that lands while this read is under way asks for another.
                        asked.queued = false;
                        asked.told
                    });
                    let answer = read(reads.as_ref(), &key);
                    // Where the service holds nothing of the resource, whether it holds its
                    // workspace at all: one it does not is no workspace a view is written onto.
                    let workspace_held = match &answer {
                        Answer::Whole(..) | Answer::Waiting => true,
                        Answer::Nothing | Answer::Failed(_) => {
                            let workspace = key.0.to_string();
                            reads.workspaces().contains(&workspace)
                        }
                    };
                    let mut state = reading.lock();
                    let asked = state.asked.entry(key).or_default();
                    let news = asked.answer.as_ref().is_none_or(|was| !was.same(&answer))
                        || asked.workspace_held != workspace_held;
                    asked.answer = Some(answer);
                    asked.workspace_held = workspace_held;
                    asked.answered_after = begun.unwrap_or(asked.told);
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
        self.shared.look(&self.asks, key);
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

    /// What the service holds of `key`'s resource and its workspace, as the latest read of it
    /// found, asking for it to be read again: `None` while that is not known yet, as no read of it
    /// answered, or the service told of a write to it since the read that answered began. A
    /// resource that could not be read is one the service holds nothing readable of.
    pub(super) fn holding(&self, key: &Key) -> Option<Holding> {
        self.follow_notices();
        self.shared.look(&self.asks, key);
        let state = self.shared.lock();
        let asked = state.asked.get(key)?;
        if asked.answered_after != asked.told {
            return None;
        }
        let resource = match asked.answer.as_ref()? {
            Answer::Whole(..) | Answer::Waiting => true,
            Answer::Nothing | Answer::Failed(_) => false,
        };
        let workspace = asked.workspace_held;
        drop(state);
        Some(Holding {
            resource,
            workspace,
        })
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
        shared.following(&asks, true);
        runtime.spawn(async move {
            use tokio::sync::broadcast::error::RecvError;
            loop {
                match writes.recv().await {
                    Ok(write) => {
                        for key in shared.followed() {
                            if reaches(&write, &key) {
                                shared.told(&asks, &key);
                            }
                        }
                    }
                    // A write missed is in the resource: everything followed is read again.
                    Err(RecvError::Lagged(_)) => {
                        for key in shared.followed() {
                            shared.told(&asks, &key);
                        }
                    }
                    // No write is told any more: each look asks again, as before they were followed.
                    Err(RecvError::Closed) => {
                        shared.following(&asks, false);
                        return;
                    }
                }
            }
        });
    }
}

/// Whether `write` is a write to `key`'s resource: its id on the wire is the route's.
fn reaches(write: &DocumentWritten, (workspace, route): &Key) -> bool {
    let DocumentWritten {
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

#[cfg(test)]
mod tests {
    use super::super::{Route, WorkspaceId};
    use super::*;

    /// A service that holds nothing of any resource, and tells `read` of each read it answers.
    struct Counted {
        read: Mutex<mpsc::Sender<()>>,
    }

    impl DeviceDocuments for Counted {
        fn read(
            &self,
            _workspace: &str,
            _app: &str,
            _resource: &str,
            _since: Option<&[[u8; 32]]>,
        ) -> Result<DocumentRead, String> {
            let _ = self
                .read
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .send(());
            Ok(DocumentRead::Absent)
        }

        fn listing(
            &self,
            _workspace: &str,
        ) -> Result<Vec<tilo_actions::remote::ListedResource>, String> {
            Ok(Vec::new())
        }

        fn holds(&self, _workspace: &str, _app: &str, _resource: &str) -> bool {
            false
        }

        fn workspaces(&self) -> Vec<String> {
            Vec::new()
        }
    }

    #[test]
    fn a_layout_looked_at_every_pass_is_read_again_only_when_a_write_reaches_it() {
        let (read, reads) = mpsc::channel();
        let (written, _) = broadcast::channel(16);
        let notices = written.clone();
        let reading = Reads::start(
            Arc::new(Counted {
                read: Mutex::new(read),
            }),
            Arc::new(|| {}),
            Some(Box::new(move || Some(notices.subscribe()))),
        );
        let key: Key = (WorkspaceId::new(), Route::layout());
        let patience = Duration::from_secs(30);

        assert!(matches!(reading.read(&key, None), Err(NotRead::Waiting)));
        reads.recv_timeout(patience).expect("the first look reads");
        reading.read_through(patience);
        while reads.try_recv().is_ok() {}

        // A desktop looks at its layout on every pass: nothing was written, so nothing is read.
        for _ in 0..100 {
            assert!(matches!(reading.read(&key, None), Ok(None)));
            assert!(reading.holding(&key).is_some());
        }
        assert_eq!(reading.shared.lock().outstanding, 0);
        assert!(reads.try_recv().is_err(), "a look read the layout again");

        // A write the service took to the layout is read.
        written
            .send(DocumentWritten {
                workspace: key.0.to_string(),
                app: key.1.app.to_owned(),
                record: key.1.document,
            })
            .expect("the session follows the writes");
        reads
            .recv_timeout(patience)
            .expect("a write to the layout reads it again");
    }
}
