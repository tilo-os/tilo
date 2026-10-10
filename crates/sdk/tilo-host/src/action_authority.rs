//! Bounded read-only host policy transport. Consumers validate the typed
//! binding and policy; this service never interprets a caller's owner claim.
use std::path::PathBuf;
use std::sync::Arc;
#[cfg(feature = "native")]
use std::sync::atomic::{AtomicBool, Ordering};

pub type AuthoritySource = Arc<dyn Fn() -> Result<Option<Vec<u8>>, String> + Send + Sync>;

/// Read the namespace the trusted process host selected. Native I/O stays in
/// the host service; the portable toolkit consumes only bounded bytes.
pub fn source(path: PathBuf, max_bytes: usize) -> AuthoritySource {
    #[cfg(feature = "native")]
    {
        Arc::new(move || read_bounded(&path, max_bytes))
    }
    #[cfg(not(feature = "native"))]
    {
        let _ = (path, max_bytes);
        Arc::new(|| Ok(None))
    }
}

/// A live watch of the one host-selected action-authority projection.
/// Dropping it stops delivery and releases the native directory watch.
#[cfg(feature = "native")]
pub struct AuthorityWatch {
    task: tilo_rt::JoinHandle<()>,
    active: Arc<AtomicBool>,
}

#[cfg(feature = "native")]
impl Drop for AuthorityWatch {
    fn drop(&mut self) {
        self.active.store(false, Ordering::Release);
        self.task.abort();
    }
}

/// Watch the projection's parent directory so atomic replacement remains
/// visible. Events carry no authority: the consumer must re-read and validate
/// the bounded projection before changing any decision.
///
/// # Errors
///
/// Fails when `path` has no parent or the native watcher cannot be installed.
#[cfg(feature = "native")]
pub fn watch(
    path: &std::path::Path,
    on_change: impl Fn() + Send + Sync + 'static,
) -> Result<AuthorityWatch, String> {
    let directory = path
        .parent()
        .ok_or("host action policy path has no parent")?;
    let file_name = path
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .ok_or("host action policy path has no file name")?
        .to_owned();
    let watcher = tilo_files::native::watch_entries(directory)
        .map_err(|_| "host action policy watch is unavailable".to_owned())?;
    let callback = Arc::new(on_change);
    let active = Arc::new(AtomicBool::new(true));
    let task_active = Arc::clone(&active);
    let task = tilo_rt::app().spawn(async move {
        loop {
            let change = watcher.next().await;
            if task_active.load(Ordering::Acquire)
                && (change.kind == tilo_files::ChangeKind::Rescan
                    || change.path.as_str() == file_name)
            {
                callback();
            }
        }
    });
    Ok(AuthorityWatch { task, active })
}

#[cfg(feature = "native")]
fn read_bounded(path: &std::path::Path, max_bytes: usize) -> Result<Option<Vec<u8>>, String> {
    tilo_files::native::bounded::read(path, max_bytes).map_err(|error| match error.kind() {
        std::io::ErrorKind::InvalidData => "host action policy exceeds its byte bound".into(),
        std::io::ErrorKind::InvalidInput => "host action policy bound is invalid".into(),
        _ => "host action policy is unavailable".into(),
    })
}

#[cfg(all(test, feature = "native"))]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn atomic_projection_replacement_wakes_without_polling_and_drop_retires_watch() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("current.json");
        tilo_files::native::atomic::write_private(&path, b"first").unwrap();
        let (send, changes) = mpsc::channel();
        let watch = watch(&path, move || {
            let _ = send.send(());
        })
        .unwrap();

        tilo_files::native::atomic::write_private(
            &directory.path().join("unrelated.json"),
            b"ignored",
        )
        .unwrap();
        assert!(changes.recv_timeout(Duration::from_millis(250)).is_err());
        tilo_files::native::atomic::write_private(&path, b"second").unwrap();
        changes.recv_timeout(Duration::from_secs(5)).unwrap();
        while changes.try_recv().is_ok() {}
        std::fs::remove_file(&path).unwrap();
        changes.recv_timeout(Duration::from_secs(5)).unwrap();

        drop(watch);
        tilo_files::native::atomic::write_private(&path, b"third").unwrap();
        assert!(changes.recv_timeout(Duration::from_millis(250)).is_err());
    }

    #[test]
    fn projection_watch_delivers_event_only_to_its_live_wake_scope() {
        use tilo_rt::wake::{WakeHandle, WakeScope};

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("current.json");
        tilo_files::native::atomic::write_private(&path, b"first").unwrap();
        let (send, events) = mpsc::channel();
        let mut scope = WakeScope::new();
        scope
            .bind(move |event| {
                let _ = send.send(event);
            })
            .unwrap();
        let wake = {
            let _entry = scope.enter();
            WakeHandle::new()
        };
        let watch = watch(&path, move || wake.notify_events()).unwrap();

        tilo_files::native::atomic::write_private(&path, b"second").unwrap();
        let current = events.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(scope.accepts(&current));
        assert!(!current.requires_sync());
        let foreign = WakeScope::new();
        assert!(!foreign.accepts(&current));

        scope.close();
        tilo_files::native::atomic::write_private(&path, b"third").unwrap();
        assert!(events.recv_timeout(Duration::from_millis(250)).is_err());
        drop(watch);
    }
}
