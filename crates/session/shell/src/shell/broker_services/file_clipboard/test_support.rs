//! Synchronization only: assertions stay on real Files actions and file bytes.
use super::*;
use std::sync::{OnceLock, Weak};

static PAUSES: OnceLock<Mutex<Vec<Weak<PauseState>>>> = OnceLock::new();
static COMMAND_OBSERVERS: OnceLock<Mutex<Vec<Weak<CommandObserverState>>>> = OnceLock::new();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CommandObservation {
    pub(crate) command: &'static str,
    pub(crate) outcome: &'static str,
}

struct CommandObserverState {
    tree: crate::shell::state::WeakWorkspaceTreeHandle,
    observations: Mutex<Vec<CommandObservation>>,
}

pub(crate) struct TestCommandObserver {
    inner: Arc<CommandObserverState>,
}

impl TestCommandObserver {
    pub(crate) fn new(tree: &crate::shell::state::WorkspaceTreeHandle) -> Self {
        let inner = Arc::new(CommandObserverState {
            tree: tree.downgrade(),
            observations: Mutex::new(Vec::new()),
        });
        let mut observers = COMMAND_OBSERVERS
            .get_or_init(Mutex::default)
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        observers.retain(|observer| observer.strong_count() != 0);
        observers.push(Arc::downgrade(&inner));
        drop(observers);
        Self { inner }
    }

    pub(crate) fn observations(&self) -> Vec<CommandObservation> {
        self.inner
            .observations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

pub(super) fn observe_command(
    tree: &crate::shell::state::WorkspaceTreeHandle,
    command: &'static str,
    outcome: &'static str,
) {
    let observers = COMMAND_OBSERVERS
        .get()
        .and_then(|observers| observers.lock().ok())
        .map_or_else(Vec::new, |mut observers| {
            observers.retain(|observer| observer.strong_count() != 0);
            observers
                .iter()
                .filter_map(Weak::upgrade)
                .filter(|observer| {
                    observer
                        .tree
                        .upgrade()
                        .is_some_and(|observed| observed.ptr_eq(tree))
                })
                .collect()
        });
    for observer in observers {
        let mut observations = observer
            .observations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let observation = CommandObservation { command, outcome };
        if observations.last() != Some(&observation) && observations.len() < 16 {
            observations.push(observation);
        }
    }
}

struct PauseState {
    path: PathBuf,
    phase: &'static str,
    entered: AtomicBool,
    released: AtomicBool,
    finished: AtomicBool,
}
pub(crate) struct TestTransferPause {
    inner: Arc<PauseState>,
}
impl TestTransferPause {
    pub(crate) fn new(path: &Path, phase: &'static str) -> Self {
        let pause = Arc::new(PauseState {
            path: path.into(),
            phase,
            entered: AtomicBool::new(false),
            released: AtomicBool::new(false),
            finished: AtomicBool::new(false),
        });
        let mut registered = PAUSES
            .get_or_init(Mutex::default)
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        registered.retain(|pause| pause.strong_count() != 0);
        registered.push(Arc::downgrade(&pause));
        drop(registered);
        Self { inner: pause }
    }
    pub(crate) fn entered(&self) -> bool {
        self.inner.entered.load(Ordering::Acquire)
    }
    pub(crate) fn finished(&self) -> bool {
        self.inner.finished.load(Ordering::Acquire)
    }
    pub(crate) fn release(&self) {
        self.inner.released.store(true, Ordering::Release);
    }
}
impl Drop for TestTransferPause {
    fn drop(&mut self) {
        self.inner.released.store(true, Ordering::Release);
    }
}
fn matching(path: &Path) -> Vec<Arc<PauseState>> {
    PAUSES
        .get()
        .and_then(|pauses| pauses.lock().ok())
        .map_or_else(Vec::new, |pauses| {
            pauses
                .iter()
                .filter_map(Weak::upgrade)
                .filter(|pause| pause.path == path)
                .collect()
        })
}
pub(super) fn pause(path: &Path, phase: &str) -> io::Result<()> {
    for pause in matching(path)
        .into_iter()
        .filter(|pause| pause.phase == phase)
    {
        pause.entered.store(true, Ordering::Release);
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(10))
            .ok_or_else(|| io::Error::other("invalid native test deadline"))?;
        while !pause.released.load(Ordering::Acquire) {
            if Instant::now() >= deadline {
                return Err(io::Error::other("native test phase was not released"));
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    Ok(())
}
pub(super) fn finished(path: &Path) {
    for pause in matching(path) {
        pause.finished.store(true, Ordering::Release);
    }
}

#[test]
fn command_observer_only_records_its_workspace_tree() {
    let observed = crate::shell::state::WorkspaceTreeHandle::default();
    let unrelated = crate::shell::state::WorkspaceTreeHandle::default();
    let observer = TestCommandObserver::new(&observed);
    observe_command(&unrelated, "Capture", "accepted");
    assert!(observer.observations().is_empty());
    observe_command(&observed, "Publish", "accepted");
    assert_eq!(
        observer.observations(),
        vec![CommandObservation {
            command: "Publish",
            outcome: "accepted",
        }]
    );
}
