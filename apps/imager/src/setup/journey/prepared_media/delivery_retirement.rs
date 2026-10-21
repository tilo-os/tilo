//! Retain an abandoned delivery until its actual writer thread has exited.
//! Canonical app-runtime timers observe completion; no pool waits on a writer.
use super::{Delivered, PreparedMedia};
use std::{
    io,
    sync::{
        Arc, Condvar, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc::Receiver,
    },
    thread::JoinHandle,
    time::Duration,
};

struct Custody {
    joined: Option<JoinHandle<()>>,
    child: Option<std::process::Child>,
    native_read_key: Option<String>,
    _read_medium: Option<(std::fs::File, tempfile::TempDir)>,
    _receiver: Option<Receiver<io::Result<Delivered>>>,
    _artifact: Option<Arc<PreparedMedia>>,
    _cancel: Option<Arc<AtomicBool>>,
}
impl Custody {
    fn is_finished(&mut self) -> bool {
        self.native_read_key
            .as_deref()
            .is_some_and(imager_backend::setup::read_media::retirement_ready)
            || self.joined.as_ref().is_some_and(JoinHandle::is_finished)
            || self
                .child
                .as_mut()
                .is_some_and(|child| child.try_wait().is_ok_and(|exit| exit.is_some()))
    }
    fn wait(&mut self) -> io::Result<()> {
        if let Some(key) = self.native_read_key.as_deref() {
            imager_backend::setup::read_media::finish_retirement(key)?;
            self.native_read_key = None;
        }
        if let Some(child) = self.child.as_mut() {
            child.wait()?;
            self.child = None;
        }
        if let Some(joined) = self.joined.take() {
            joined.join().map_err(|_| {
                io::Error::other(crate::setup::strings::t("installer-media-worker-stopped"))
            })?;
        }
        Ok(())
    }
}
struct Retirement {
    key: Option<String>,
    custody: Mutex<Option<Custody>>,
    stopped: AtomicBool,
    failed: AtomicBool,
    wake: Condvar,
    // Retain the canonical runtime task handle; never abort it on UI teardown.
    task: Mutex<Option<ui::rt::JoinHandle<()>>>,
}
fn registry() -> &'static Mutex<Vec<Arc<Retirement>>> {
    static REGISTRY: OnceLock<Mutex<Vec<Arc<Retirement>>>> = OnceLock::new();
    REGISTRY.get_or_init(Default::default)
}
impl Retirement {
    fn finish(&self, failed: bool) {
        let _held = self
            .custody
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.failed.store(failed, Ordering::Release);
        if !failed {
            self.stopped.store(true, Ordering::Release);
        }
        self.wake.notify_all();
    }
    // Host teardown only: frames have stopped before this potentially blocking join.
    fn wait_stopped(&self) -> io::Result<()> {
        let mut held = self
            .custody
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(mut custody) = held.take() {
            drop(held);
            let result = custody.wait();
            if custody.child.is_some() || custody.native_read_key.is_some() {
                // A wait error is not evidence that the exact helper stopped.
                *self
                    .custody
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(custody);
                self.finish(true);
            } else {
                drop(custody);
                self.finish(false);
            }
            return result;
        }
        while !self.stopped.load(Ordering::Acquire) && !self.failed.load(Ordering::Acquire) {
            held = self
                .wake
                .wait(held)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        if self.failed.load(Ordering::Acquire) {
            Err(io::Error::other(crate::setup::strings::t(
                "installer-media-cleanup-failed",
            )))
        } else {
            Ok(())
        }
    }

    fn take_finished(&self) -> Option<Custody> {
        let mut slot = match self.custody.try_lock() {
            Ok(slot) => slot,
            Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return None,
        };
        if slot.as_mut().is_some_and(Custody::is_finished) {
            slot.take()
        } else {
            None
        }
    }
}

pub(super) fn retain(
    key: Option<String>,
    joined: JoinHandle<()>,
    receiver: Option<Receiver<io::Result<Delivered>>>,
    artifact: Option<Arc<PreparedMedia>>,
    cancel: Option<Arc<AtomicBool>>,
) {
    retain_custody(
        key,
        Custody {
            joined: Some(joined),
            child: None,
            native_read_key: None,
            _read_medium: None,
            _receiver: receiver,
            _artifact: artifact,
            _cancel: cancel,
        },
    );
}

#[cfg(test)]
pub(super) fn retain_read_medium(
    key: String,
    child: std::process::Child,
    file: std::fs::File,
    request: tempfile::TempDir,
) {
    retain_custody(
        Some(key),
        Custody {
            joined: None,
            child: Some(child),
            native_read_key: None,
            _read_medium: Some((file, request)),
            _receiver: None,
            _artifact: None,
            _cancel: None,
        },
    );
}

pub(in crate::setup) fn observe_read_medium(key: String) {
    retain_custody(
        Some(key.clone()),
        Custody {
            joined: None,
            child: None,
            native_read_key: Some(key),
            _read_medium: None,
            _receiver: None,
            _artifact: None,
            _cancel: None,
        },
    );
}

fn retain_custody(key: Option<String>, custody: Custody) {
    let retirement = Arc::new(Retirement {
        key,
        custody: Mutex::new(Some(custody)),
        stopped: AtomicBool::new(false),
        failed: AtomicBool::new(false),
        wake: Condvar::new(),
        task: Mutex::new(None),
    });
    // A failed runtime start must not drop the join handle or custody. The
    // registry owns both before scheduling; matching operations remain blocked.
    registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(retirement.clone());
    if let Ok(runtime) = ui::rt::try_app() {
        let observed = retirement.clone();
        let task = runtime.spawn(async move {
            loop {
                if observed.stopped.load(Ordering::Acquire)
                    || observed.failed.load(Ordering::Acquire)
                {
                    return;
                }
                if let Some(mut custody) = observed.take_finished() {
                    // Both a completed writer join and already observed Child
                    // wait are nonblocking. Cleanup remains off the UI thread.
                    if custody.native_read_key.is_some() {
                        let owner = observed.clone();
                        let failed = runtime
                            .spawn_blocking(move || {
                                let result = custody.wait();
                                if result.is_err() && custody.native_read_key.is_some() {
                                    *owner
                                        .custody
                                        .lock()
                                        .unwrap_or_else(std::sync::PoisonError::into_inner) =
                                        Some(custody);
                                }
                                result
                            })
                            .await;
                        observed.finish(!matches!(failed, Ok(Ok(()))));
                    } else {
                        let _ = custody.wait();
                        let failed = runtime.spawn_blocking(move || drop(custody)).await.is_err();
                        observed.finish(failed);
                    }
                    return;
                }
                ui::rt::sleep(Duration::from_millis(25)).await;
            }
        });
        *retirement
            .task
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(task);
    }
}

pub(super) fn pending(key: &str) -> bool {
    let selected = {
        let mut entries = match registry().try_lock() {
            Ok(entries) => entries,
            Err(std::sync::TryLockError::Poisoned(error)) => error.into_inner(),
            // A short concurrent registration is uncertainty, never permission
            // to start another writer. No registry mutex encloses a thread join.
            Err(std::sync::TryLockError::WouldBlock) => return true,
        };
        entries.retain(|entry| !entry.stopped.load(Ordering::Acquire));
        entries
            .iter()
            .filter(|entry| entry.key.as_deref() == Some(key))
            .cloned()
            .collect::<Vec<_>>()
    };
    imager_backend::setup::read_media::retirement_pending(key)
        || selected
            .into_iter()
            .any(|entry| !entry.stopped.load(Ordering::Acquire))
}

/// Synchronous final host teardown, never a busy check or a frame callback.
pub(super) fn finish_pending(key: &str) -> io::Result<()> {
    let entries = registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .filter(|entry| entry.key.as_deref() == Some(key))
        .cloned()
        .collect::<Vec<_>>();
    let mut failure = None;
    for entry in entries {
        if let Err(error) = entry.wait_stopped() {
            failure = Some(error);
        }
    }
    if let Err(error) = imager_backend::setup::read_media::finish_retirement(key) {
        failure = Some(error);
    }
    failure.map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests {
    use super::super::Delivery;
    use super::*;
    use std::{
        fs::OpenOptions, io::Write, os::unix::fs::PermissionsExt as _, sync::mpsc, time::Instant,
    };
    fn target(id: String) -> crate::model::TargetDrive {
        crate::model::TargetDrive {
            name: "fixture-medium".into(),
            model: "fixture".into(),
            stable_id: Some(id),
            size: 4096,
            kind: crate::model::TargetKind::Usb,
            tran: Some("usb".into()),
            removable: true,
            is_system: false,
            partitions: Vec::new(),
        }
    }
    #[test]
    fn owned_read_helper_retirement_retains_private_request_and_medium_until_actual_exit() {
        use std::os::unix::net::UnixStream;
        use std::process::{Command, Stdio};
        let request = tempfile::Builder::new()
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let root = request.path().to_path_buf();
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(root.join("held-medium-fixture"))
            .unwrap();
        let (mut release, input) = UnixStream::pair().unwrap();
        let child = Command::new("/bin/sh")
            .args(["-c", "read -r release"])
            .stdin(Stdio::from(std::fs::File::from(
                std::os::fd::OwnedFd::from(input),
            )))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let key = format!("target:{id}");
        // The helper reads until released below, so retaining it returns
        // only if UI teardown does not wait on it.
        let (retained, done) = mpsc::channel();
        let retaining = key.clone();
        std::thread::spawn(move || {
            retain_read_medium(retaining, child, file, request);
            let _ = retained.send(());
        });
        tilo_test_env::recv(
            "UI teardown to retain the read helper without waiting",
            &done,
        );
        assert!(pending(&key));
        assert!(super::super::pending_target(&id));
        assert!(!pending(&format!("target:{}", uuid::Uuid::new_v4())));
        assert!(
            root.join("held-medium-fixture").exists(),
            "private custody dropped before observed child exit"
        );
        release.write_all(b"release\n").unwrap();
        finish_pending(&key).unwrap();
        assert!(!pending(&key));
        assert!(
            !root.exists(),
            "private request outlived actual child teardown"
        );
    }

    #[test]
    fn host_teardown_waits_for_owned_retired_writer_without_unblocking_media_early() {
        let id = uuid::Uuid::new_v4().to_string();
        let key = format!("target:{id}");
        let (release, held) = mpsc::sync_channel(1);
        let (ready, arrived) = mpsc::sync_channel(1);
        let joined = std::thread::spawn(move || {
            ready.send(()).unwrap();
            held.recv_timeout(tilo_test_env::hang_bound()).unwrap();
        });
        arrived.recv_timeout(tilo_test_env::hang_bound()).unwrap();
        retain(Some(key.clone()), joined, None, None, None);
        let (complete, observed) = mpsc::sync_channel(1);
        let (started, teardown_tid) = mpsc::channel();
        let closing_key = key.clone();
        let teardown = std::thread::spawn(move || {
            started
                .send(tilo_test_env::idle::current_thread_id().ok())
                .unwrap();
            complete.send(finish_pending(&closing_key)).unwrap();
        });
        // Teardown blocks on the held worker, seen blocked where the platform
        // shows it, and has not finished.
        if let Some(tid) =
            tilo_test_env::recv("teardown to start", &teardown_tid).map(|tid| tid.to_string())
        {
            tilo_test_env::wait_until("teardown to block on the held worker", || {
                tilo_test_env::idle::blocked(&tid)
            });
        }
        assert!(matches!(
            observed.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        assert!(super::super::pending_target(&id));
        release.send(()).unwrap();
        tilo_test_env::recv("teardown to finish once the worker exits", &observed).unwrap();
        teardown.join().unwrap();
        assert!(!pending(&key));
        assert!(!super::super::pending_target(&id));
    }

    #[test]
    fn dropped_delivery_retains_actual_writer_and_scoped_busy_until_observed_exit() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("approved-output");
        let mut held = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .unwrap();
        held.write_all(b"preserved output").unwrap();
        let held = Arc::new(held);
        let weak = Arc::downgrade(&held);
        let (ready_send, ready_recv) = mpsc::sync_channel(1);
        let (release_send, release_recv) = mpsc::sync_channel(1);
        let (result_send, result_recv) = mpsc::channel::<io::Result<Delivered>>();
        let cancel = Arc::new(AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let joined = std::thread::spawn(move || {
            ready_send.send(()).unwrap();
            // Bounded rendezvous uses a real held writable descriptor. Cancellation
            // is deliberately cooperative; signalling it is not proof of exit.
            release_recv
                .recv_timeout(tilo_test_env::hang_bound())
                .unwrap();
            assert!(worker_cancel.load(Ordering::Acquire));
            assert!(held.metadata().is_ok());
            drop(held);
            drop(result_send);
        });
        ready_recv
            .recv_timeout(tilo_test_env::hang_bound())
            .unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let mut delivery = Delivery::default();
        delivery.target = Some(target(id.clone()));
        delivery.joined = Some(joined);
        delivery.worker = Some(result_recv);
        delivery.cancel = Some(cancel.clone());
        // The writer runs until released below, so the drop returns only if
        // UI teardown does not wait on it.
        let (dropped, done) = mpsc::channel();
        std::thread::spawn(move || {
            drop(delivery);
            let _ = dropped.send(());
        });
        tilo_test_env::recv("UI teardown to drop the delivery without waiting", &done);
        assert!(cancel.load(Ordering::Acquire));
        assert!(
            weak.upgrade().is_some(),
            "writer custody was released before exit"
        );
        let mut same = Delivery::default();
        same.target = Some(target(id));
        assert!(same.busy());
        assert!(
            crate::app::ImagerApp::require_stopped_target(same.target.as_ref().unwrap()).is_err()
        );
        assert!(super::super::pending_target(
            same.target.as_ref().unwrap().stable_id.as_deref().unwrap()
        ));
        let mut unrelated = Delivery::default();
        unrelated.target = Some(target(uuid::Uuid::new_v4().to_string()));
        assert!(
            !unrelated.busy(),
            "unrelated media inherited the retired writer"
        );
        assert!(
            crate::app::ImagerApp::require_stopped_target(unrelated.target.as_ref().unwrap())
                .is_ok()
        );
        release_send.send(()).unwrap();
        let deadline = tilo_test_env::hang_deadline();
        while same.busy() {
            assert!(
                Instant::now() < deadline,
                "retired writer did not become observably stopped"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            weak.upgrade().is_none(),
            "busy cleared while writer descriptor remained owned"
        );
        assert!(
            crate::app::ImagerApp::require_stopped_target(same.target.as_ref().unwrap()).is_ok()
        );
        assert_eq!(std::fs::read(path).unwrap(), b"preserved output");
    }
}
