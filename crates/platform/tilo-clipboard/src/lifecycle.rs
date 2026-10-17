//! Admission and off-thread teardown for a runner's native clipboard.
//!
//! Closing is deliberately not a reader cancellation guarantee. A smithay
//! loader may still be waiting on a peer; its owned display must remain alive
//! until that loader and its protocol worker have actually finished.

use std::sync::{Arc, Condvar, Mutex, mpsc};

use crate::{ClipboardBackend, ClipboardImage, ClipboardTarget, RichTextOffer};

struct AdmissionState {
    backend: Option<Arc<dyn ClipboardBackend>>,
    active: usize,
}

struct Admission {
    state: Mutex<AdmissionState>,
    idle: Condvar,
    #[cfg(test)]
    reaped: std::sync::atomic::AtomicBool,
}

type Teardown = (Arc<dyn ClipboardBackend>, Arc<Admission>);

pub(crate) struct Session {
    admission: Arc<Admission>,
    cleanup: mpsc::Sender<Teardown>,
}

// Field order is intentional: release the backend reference before the lease
// decrements active calls, allowing the reaper to perform the final drop.
struct Call {
    backend: Arc<dyn ClipboardBackend>,
    _lease: AdmissionLease,
}

struct AdmissionLease(Arc<Admission>);

impl std::ops::Deref for Call {
    type Target = dyn ClipboardBackend;
    fn deref(&self) -> &Self::Target {
        self.backend.as_ref()
    }
}

impl Drop for AdmissionLease {
    fn drop(&mut self) {
        let mut state = self
            .0
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.active = state.active.saturating_sub(1);
        let idle = state.active == 0;
        drop(state);
        if idle {
            self.0.idle.notify_one();
        }
    }
}

/// Runner-owned admission guard. Drop closes the attachment without joining
/// protocol workers on the event-loop thread. Retained system clipboard handles
/// then refuse operations on this connection. This is admission closure, not
/// complete shutdown: initialization and independently retained smithay
/// loaders may await the server or peer. They keep the owned display alive
/// until they finish; close neither abandons workers nor raises reader limits.
#[must_use = "keep the attachment until the runner closes"]
pub struct ClipboardAttachment {
    pub(crate) session: Arc<Session>,
}

impl Session {
    pub(crate) fn new(
        backend: impl FnOnce() -> Option<Arc<dyn ClipboardBackend>>,
    ) -> Option<Arc<Self>> {
        let (cleanup, pending) = mpsc::channel::<Teardown>();
        // Start the reaper before constructing any foreign-display workers: a
        // failed thread spawn must not force their potentially blocking Drop
        // onto the event-loop thread.
        std::thread::Builder::new()
            .name("tilo-clipboard-cleanup".into())
            .spawn(move || {
                if let Ok((backend, admission)) = pending.recv() {
                    let state = admission
                        .state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let idle = admission
                        .idle
                        .wait_while(state, |state| state.active != 0)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    drop(idle);
                    drop(backend);
                    #[cfg(test)]
                    admission
                        .reaped
                        .store(true, std::sync::atomic::Ordering::Release);
                }
            })
            .ok()?;
        let backend = backend()?;
        Some(Arc::new(Self {
            admission: Arc::new(Admission {
                state: Mutex::new(AdmissionState {
                    backend: Some(backend),
                    active: 0,
                }),
                idle: Condvar::new(),
                #[cfg(test)]
                reaped: std::sync::atomic::AtomicBool::new(false),
            }),
            cleanup,
        }))
    }

    fn admit(&self) -> Option<Call> {
        let mut state = self
            .admission
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let backend = state.backend.clone()?;
        state.active = state.active.checked_add(1)?;
        drop(state);
        Some(Call {
            backend,
            _lease: AdmissionLease(Arc::clone(&self.admission)),
        })
    }

    #[cfg(test)]
    pub(crate) fn reaped_for_test(&self) -> bool {
        self.admission
            .reaped
            .load(std::sync::atomic::Ordering::Acquire)
    }

    fn close(&self) {
        // This same short lock linearizes admission and close. No protocol
        // waits, callbacks, worker joins or clipboard destruction under it.
        let backend = self
            .admission
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .backend
            .take();
        if let Some(backend) = backend {
            // The cleanup receiver exists until this send; it runs no code
            // other than recv before receiving the sole teardown value.
            let _ = self.cleanup.send((backend, Arc::clone(&self.admission)));
        }
    }
}

impl Drop for ClipboardAttachment {
    fn drop(&mut self) {
        self.session.close();
    }
}

impl ClipboardBackend for Session {
    fn request_text(&self, target: ClipboardTarget) -> mpsc::Receiver<Option<String>> {
        if let Some(backend) = self.admit() {
            return backend.request_text(target);
        }
        let (reply, result) = mpsc::channel();
        let _ = reply.send(None);
        result
    }
    fn set_text(&self, target: ClipboardTarget, text: &str) -> bool {
        self.admit()
            .is_some_and(|backend| backend.set_text(target, text))
    }
    fn set_shared_text(&self, target: ClipboardTarget, text: &str) -> bool {
        self.admit()
            .is_some_and(|backend| backend.set_shared_text(target, text))
    }
    fn set_secret_text(&self, text: &str) -> bool {
        self.admit()
            .is_some_and(|backend| backend.set_secret_text(text))
    }
    fn text(&self, target: ClipboardTarget) -> Option<String> {
        self.admit()?.text(target)
    }
    fn set_image(&self, width: usize, height: usize, rgba: &[u8]) -> bool {
        self.admit()
            .is_some_and(|backend| backend.set_image(width, height, rgba))
    }
    fn image(&self) -> Option<ClipboardImage> {
        self.admit()?.image()
    }
    fn rich_text(&self) -> Option<RichTextOffer> {
        self.admit()?.rich_text()
    }
    fn name(&self) -> &'static str {
        "runner-clipboard"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct BlockedClipboard {
        entered: mpsc::Sender<()>,
        release: Mutex<mpsc::Receiver<()>>,
        dropped: mpsc::Sender<()>,
    }

    impl ClipboardBackend for BlockedClipboard {
        fn text(&self, _: ClipboardTarget) -> Option<String> {
            self.entered.send(()).unwrap();
            self.release
                .lock()
                .unwrap()
                .recv_timeout(tilo_test_env::hang_bound())
                .unwrap();
            Some("admitted before close".into())
        }
        fn set_text(&self, _: ClipboardTarget, _: &str) -> bool {
            panic!("a closed session admitted a write")
        }
        fn name(&self) -> &'static str {
            "blocked-test"
        }
    }

    impl Drop for BlockedClipboard {
        fn drop(&mut self) {
            assert_eq!(
                std::thread::current().name(),
                Some("tilo-clipboard-cleanup"),
                "protocol teardown ran on a caller instead of the reaper"
            );
            let _ = self.dropped.send(());
        }
    }

    #[test]
    fn retained_session_closes_without_waiting_for_an_admitted_call() {
        let (entered, waiting) = mpsc::channel();
        let (release, blocked) = mpsc::channel();
        let (dropped, destroyed) = mpsc::channel();
        let session = Session::new(|| {
            Some(Arc::new(BlockedClipboard {
                entered,
                release: Mutex::new(blocked),
                dropped,
            }))
        })
        .unwrap();
        let retained = Arc::clone(&session);
        let call = std::thread::spawn(move || retained.text(ClipboardTarget::Clipboard));
        waiting.recv_timeout(tilo_test_env::hang_bound()).unwrap();
        drop(ClipboardAttachment {
            session: Arc::clone(&session),
        });
        assert!(!session.set_text(ClipboardTarget::Clipboard, "late"));
        assert!(!session.set_shared_text(ClipboardTarget::Primary, "late"));
        assert!(session.text(ClipboardTarget::Clipboard).is_none());
        assert!(session.image().is_none());
        assert!(session.rich_text().is_none());
        assert!(!session.set_image(1, 1, &[0; 4]));
        assert_eq!(
            session
                .request_text(ClipboardTarget::Clipboard)
                .recv_timeout(tilo_test_env::hang_bound())
                .unwrap(),
            None
        );
        assert!(
            destroyed.try_recv().is_err(),
            "admitted call lost its backend"
        );
        release.send(()).unwrap();
        assert_eq!(
            call.join().unwrap().as_deref(),
            Some("admitted before close")
        );
        destroyed.recv_timeout(tilo_test_env::hang_bound()).unwrap();
        assert!(session.text(ClipboardTarget::Primary).is_none());
    }
}
