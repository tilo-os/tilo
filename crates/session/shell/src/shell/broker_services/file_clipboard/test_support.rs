//! Synchronization only: assertions stay on real Files actions and file bytes.
use super::*;
use std::sync::{OnceLock, Weak};

static PAUSES: OnceLock<Mutex<Vec<Weak<PauseState>>>> = OnceLock::new();

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
        let mut registered = PAUSES.get_or_init(Mutex::default).lock_recover();
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
        let deadline = tilo_test_env::hang_deadline();
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
