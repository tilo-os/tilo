//! Non-blocking Task Center adapter to the existing authenticated native owner.
//! Every read/commit runs off the compositor thread and retains exact room scope.
#[cfg(test)]
mod tests;
mod worker;

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Instant;
use tilo_desktop::activity::{TaskReviewRequest, TaskReviewSnapshot};

const MAX_PENDING: usize = 16;
const MAX_RESULTS: usize = 128;

#[derive(Default)]
struct State {
    native_rooms: BTreeSet<String>,
    snapshots: BTreeMap<String, Result<std::sync::Arc<TaskReviewSnapshot>, String>>,
    queried: BTreeMap<String, Instant>,
    loading: BTreeSet<String>,
    requests: BTreeMap<uuid::Uuid, TaskReviewRequest>,
    results: BTreeMap<uuid::Uuid, Result<(), String>>,
    completed: VecDeque<uuid::Uuid>,
}

enum Job {
    Snapshot(String),
    Commit(TaskReviewRequest),
}

#[derive(Clone)]
pub(crate) struct NativeTaskReview {
    queue: mpsc::SyncSender<Job>,
    state: Arc<Mutex<State>>,
    // Fields drop in declaration order: every sender must disconnect before
    // the last worker handle joins the receiver's loop.
    _worker: Arc<WorkerThread>,
}

struct WorkerThread(Option<std::thread::JoinHandle<()>>);

impl Drop for WorkerThread {
    fn drop(&mut self) {
        if let Some(thread) = self.0.take()
            && thread.join().is_err()
        {
            tracing::warn!("the task review worker panicked during shutdown");
        }
    }
}

impl NativeTaskReview {
    pub(crate) fn new(
        tree: super::state::WorkspaceTreeHandle,
        network: Option<Arc<super::network::ShellNetwork>>,
    ) -> Result<Self, String> {
        let commit = Arc::new(super::broker_services::NativeOwnerCommitService::new(
            tree, network,
        ));
        let owner = commit.owner().ok_or("native task owner unavailable")?;
        Self::start(worker::Worker::new(owner, commit))
    }

    fn start(mut worker: worker::Worker) -> Result<Self, String> {
        let state = Arc::new(Mutex::new(State {
            native_rooms: worker.rooms(),
            ..State::default()
        }));
        let (queue, receiver) = mpsc::sync_channel(MAX_PENDING);
        worker.state = Arc::clone(&state);
        let worker_thread = std::thread::Builder::new()
            .name("tilo-task-review".into())
            .spawn(move || worker.run(receiver))
            .map_err(|error| error.to_string())?;
        Ok(Self {
            queue,
            state,
            _worker: Arc::new(WorkerThread(Some(worker_thread))),
        })
    }

    pub(crate) fn requires_room(&self, room: &str) -> bool {
        self.state.lock().map_or(true, |state| {
            state.native_rooms.contains(room) || !state.snapshots.contains_key(room)
        })
    }

    pub(crate) fn snapshot(
        &self,
        room: &str,
    ) -> Option<Result<std::sync::Arc<TaskReviewSnapshot>, String>> {
        let Ok(mut state) = self.state.lock() else {
            return Some(Err("native task review unavailable".into()));
        };
        if !state.loading.contains(room)
            && state
                .queried
                .get(room)
                .is_none_or(|when| when.elapsed().as_secs() >= 1)
            && self.queue.try_send(Job::Snapshot(room.to_owned())).is_ok()
        {
            state.loading.insert(room.to_owned());
            state.queried.insert(room.to_owned(), Instant::now());
        }
        state.snapshots.get(room).cloned()
    }

    pub(crate) fn submit(&self, request: TaskReviewRequest) -> Result<(), String> {
        if request.invocation.is_nil() {
            return Err("task review invocation is nil".into());
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| "native task review unavailable")?;
        if let Some(previous) = state.requests.get(&request.invocation) {
            return if previous == &request {
                Ok(())
            } else {
                Err("task review invocation already names another command".into())
            };
        }
        if state.requests.len().saturating_sub(state.results.len()) >= MAX_PENDING {
            return Err("native task review is busy".into());
        }
        self.queue
            .try_send(Job::Commit(request.clone()))
            .map_err(|_| "native task review is busy")?;
        state.requests.insert(request.invocation, request);
        drop(state);
        Ok(())
    }

    pub(crate) fn poll(&self, invocation: uuid::Uuid) -> Option<Result<(), String>> {
        let Ok(state) = self.state.lock() else {
            return Some(Err("native task review unavailable".into()));
        };
        state.results.get(&invocation).cloned().or_else(|| {
            (!state.requests.contains_key(&invocation)).then(|| {
                Err("task review result is no longer retained; inspect the task history".into())
            })
        })
    }
}
