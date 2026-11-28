//! Native session restart and cleanup ownership, independent of the rendering host.
use crate::sessions::SessionCookies;
use std::path::Path;

/// Event-loop notification supplied by the host.
pub type Wake = std::sync::Arc<dyn Fn() + Send + Sync>;

/// Opaque engine contract; the host supplies its SDK adapter.
pub trait Engine: Clone + Send + Sync + 'static {
    /// # Errors
    /// Returns an error when the native operation cannot be confirmed.
    fn shutdown_service_session(&self) -> Result<(), String>;
    /// # Errors
    /// Returns an error when the native operation cannot be confirmed.
    fn reopen(path: &Path, session_id: &str) -> Result<Self, String>;
    fn attach_waker(&self, wake: Wake);
    /// # Errors
    /// Returns an error when the native operation cannot be confirmed.
    fn verify(&self, session: SessionCookies) -> Result<(), String>;
}

/// Lease-holding profile authority supplied by the host.
pub trait Profile: Send + 'static {
    /// # Errors
    /// Returns an error when the native operation cannot be confirmed.
    fn path(&self) -> std::io::Result<&Path>;
    /// # Errors
    /// Returns an error when the native operation cannot be confirmed.
    fn cleanup_after_engine_exit(&mut self) -> std::io::Result<()>;
}

type CleanupOwnership<E, P> = (E, P);
type CleanupSlot<E, P> = std::sync::Arc<std::sync::Mutex<Option<CleanupOwnership<E, P>>>>;

/// Origin storage is qualified by a cold reopen, not by a write callback in
/// the first process. This worker retains the same profile lease throughout.
pub struct NativeRestart<E: Engine, P: Profile> {
    slot: CleanupSlot<E, P>,
    handle: Option<std::thread::JoinHandle<()>>,
    cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ready: std::sync::Arc<std::sync::atomic::AtomicBool>,
    failed: std::sync::Arc<std::sync::atomic::AtomicBool>,
    done: std::sync::Arc<std::sync::atomic::AtomicBool>,
    cleanup: Option<NativeCleanup<E, P>>,
    waker: Option<Wake>,
}
impl<E: Engine, P: Profile> NativeRestart<E, P> {
    pub fn start(engine: E, profile: P, session: SessionCookies, waker: Option<Wake>) -> Self {
        let slot = std::sync::Arc::new(std::sync::Mutex::new(Some((engine, profile))));
        let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let ready = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let failed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_slot = slot.clone();
        let worker_cancelled = cancelled.clone();
        let worker_ready = ready.clone();
        let worker_failed = failed.clone();
        let worker_done = done.clone();
        let wake = waker.clone();
        let spawned = std::thread::Builder::new()
            .name("browser-service-verify".into())
            .spawn(move || {
                let result = restart_for_session_verification(
                    worker_slot,
                    session,
                    &worker_cancelled,
                    &worker_done,
                    wake.clone(),
                );
                worker_failed.store(result.is_err(), std::sync::atomic::Ordering::Release);
                worker_ready.store(true, std::sync::atomic::Ordering::Release);
                if let Some(waker) = wake {
                    waker();
                }
            });
        let handle = if let Ok(handle) = spawned {
            Some(handle)
        } else {
            failed.store(true, std::sync::atomic::Ordering::Release);
            ready.store(true, std::sync::atomic::Ordering::Release);
            None
        };
        Self {
            slot,
            handle,
            cancelled,
            ready,
            failed,
            done,
            cleanup: None,
            waker,
        }
    }

    fn settled(&self) -> bool {
        self.ready.load(std::sync::atomic::Ordering::Acquire)
            && self
                .handle
                .as_ref()
                .is_none_or(std::thread::JoinHandle::is_finished)
    }

    pub fn poll(&mut self) -> Option<Result<CleanupOwnership<E, P>, ()>> {
        if !self.settled() || self.cancelled.load(std::sync::atomic::Ordering::Acquire) {
            return None;
        }
        if self.failed.load(std::sync::atomic::Ordering::Acquire) {
            return Some(Err(()));
        }
        self.slot.try_lock().ok()?.take().map(Ok)
    }

    pub fn request_close(&mut self) {
        self.cancelled
            .store(true, std::sync::atomic::Ordering::Release);
        if !self.settled() || self.cleanup.is_some() {
            return;
        }
        let payload = self.slot.try_lock().ok().and_then(|mut slot| slot.take());
        if let Some((engine, profile)) = payload {
            self.cleanup = Some(cleanup_after_native_exit(
                engine,
                profile,
                self.done.clone(),
                self.waker.clone(),
            ));
        }
    }

    pub fn close_complete(&self) -> bool {
        self.cancelled.load(std::sync::atomic::Ordering::Acquire)
            && self.done.load(std::sync::atomic::Ordering::Acquire)
            && self
                .handle
                .as_ref()
                .is_none_or(std::thread::JoinHandle::is_finished)
    }

    /// Only component teardown calls this blocking path.
    /// # Errors
    /// Returns an error when the native operation cannot be confirmed.
    pub fn finish(&mut self) -> Result<(), String> {
        self.cancelled
            .store(true, std::sync::atomic::Ordering::Release);
        if let Some(handle) = self.handle.take()
            && handle.join().is_err()
        {
            retain_unconfirmed_cleanup(self.slot.clone());
            return Err("Account engine shutdown could not be confirmed".into());
        }
        self.request_close();
        if let Some(cleanup) = self.cleanup.as_mut() {
            cleanup.finish()?;
        }
        if self.close_complete() {
            Ok(())
        } else {
            Err("Account engine shutdown could not be confirmed".into())
        }
    }
}
impl<E: Engine, P: Profile> Drop for NativeRestart<E, P> {
    fn drop(&mut self) {
        self.request_close();
        let empty_settled = self.settled() && self.slot.try_lock().is_ok_and(|slot| slot.is_none());
        if !empty_settled && !self.done.load(std::sync::atomic::Ordering::Acquire) {
            retain_unconfirmed_cleanup(self.slot.clone());
        }
    }
}

/// Unexpected worker failure must preserve authority as well as the engine.
struct RestartOwnership<E: Engine, P: Profile> {
    slot: CleanupSlot<E, P>,
    payload: Option<CleanupOwnership<E, P>>,
}
impl<E: Engine, P: Profile> RestartOwnership<E, P> {
    fn store(&mut self) {
        if let Some(payload) = self.payload.take() {
            *self
                .slot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(payload);
        }
    }
}
impl<E: Engine, P: Profile> Drop for RestartOwnership<E, P> {
    fn drop(&mut self) {
        if self.payload.is_some() {
            self.store();
            retain_unconfirmed_cleanup(self.slot.clone());
        }
    }
}

fn restart_for_session_verification<E: Engine, P: Profile>(
    slot: CleanupSlot<E, P>,
    session: SessionCookies,
    cancelled: &std::sync::atomic::AtomicBool,
    done: &std::sync::atomic::AtomicBool,
    waker: Option<Wake>,
) -> Result<(), ()> {
    let payload = slot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    let mut ownership = RestartOwnership { slot, payload };
    let Some((engine, profile)) = ownership.payload.as_ref() else {
        return Err(());
    };
    engine.shutdown_service_session().map_err(|_| ())?;
    if cancelled.load(std::sync::atomic::Ordering::Acquire) {
        return finish_cancelled_restart(&mut ownership, done);
    }
    let replacement =
        E::reopen(profile.path().map_err(|_| ())?, &session.session_id).map_err(|_| ())?;
    if let Some((engine, _)) = ownership.payload.as_mut() {
        *engine = replacement.clone();
    }
    if let Some(waker) = waker {
        replacement.attach_waker(waker);
    }
    if cancelled.load(std::sync::atomic::Ordering::Acquire) {
        return finish_cancelled_restart(&mut ownership, done);
    }
    replacement.verify(session).map_err(|_| ())?;
    if cancelled.load(std::sync::atomic::Ordering::Acquire) {
        return finish_cancelled_restart(&mut ownership, done);
    }
    ownership.store();
    Ok(())
}

fn finish_cancelled_restart<E: Engine, P: Profile>(
    ownership: &mut RestartOwnership<E, P>,
    done: &std::sync::atomic::AtomicBool,
) -> Result<(), ()> {
    let Some((engine, profile)) = ownership.payload.as_mut() else {
        return Err(());
    };
    engine.shutdown_service_session().map_err(|_| ())?;
    profile.cleanup_after_engine_exit().map_err(|_| ())?;
    ownership.payload = None;
    done.store(true, std::sync::atomic::Ordering::Release);
    Ok(())
}

// A failed thread start or unconfirmed process wait must retain the lease and
// private profile authority. It must never release them while a child may live.
fn retain_unconfirmed_cleanup<E: Engine, P: Profile>(slot: CleanupSlot<E, P>) {
    static RETAINED: std::sync::OnceLock<std::sync::Mutex<Vec<Box<dyn Send>>>> =
        std::sync::OnceLock::new();
    RETAINED
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(Box::new(slot));
}

pub fn cleanup_after_native_exit<E: Engine, P: Profile>(
    engine: E,
    profile: P,
    done: std::sync::Arc<std::sync::atomic::AtomicBool>,
    waker: Option<Wake>,
) -> NativeCleanup<E, P> {
    let slot = std::sync::Arc::new(std::sync::Mutex::new(Some((engine, profile))));
    let worker_slot = slot.clone();
    let worker_done = done.clone();
    let spawned = std::thread::Builder::new()
        .name("browser-service-close".into())
        .spawn(move || {
            let payload = worker_slot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            let mut ownership = RestartOwnership {
                slot: worker_slot,
                payload,
            };
            if let Some((engine, profile)) = ownership.payload.as_mut()
                && engine.shutdown_service_session().is_ok()
                && profile.cleanup_after_engine_exit().is_ok()
            {
                ownership.payload = None;
                worker_done.store(true, std::sync::atomic::Ordering::Release);
            }
            if let Some(waker) = waker {
                waker();
            }
        });
    NativeCleanup {
        slot,
        handle: spawned.ok(),
        done,
    }
}

pub struct NativeCleanup<E: Engine, P: Profile> {
    slot: CleanupSlot<E, P>,
    handle: Option<std::thread::JoinHandle<()>>,
    done: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
impl<E: Engine, P: Profile> NativeCleanup<E, P> {
    pub fn settled(&self) -> bool {
        self.handle
            .as_ref()
            .is_none_or(std::thread::JoinHandle::is_finished)
    }
    /// # Errors
    /// Returns an error when the native operation cannot be confirmed.
    pub fn finish(&mut self) -> Result<(), String> {
        if let Some(handle) = self.handle.take() {
            handle
                .join()
                .map_err(|_| "Account engine shutdown could not be confirmed")?;
        } else if !self.done.load(std::sync::atomic::Ordering::Acquire) {
            // Thread creation can fail. Teardown owns the tree already and may
            // wait synchronously; render/sync never takes this fallback.
            let payload = self
                .slot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            let mut ownership = RestartOwnership {
                slot: self.slot.clone(),
                payload,
            };
            if let Some((engine, profile)) = ownership.payload.as_mut()
                && engine.shutdown_service_session().is_ok()
                && profile.cleanup_after_engine_exit().is_ok()
            {
                ownership.payload = None;
                self.done.store(true, std::sync::atomic::Ordering::Release);
            }
        }
        if self.done.load(std::sync::atomic::Ordering::Acquire) {
            Ok(())
        } else {
            Err("Account engine shutdown could not be confirmed".into())
        }
    }
}
impl<E: Engine, P: Profile> Drop for NativeCleanup<E, P> {
    fn drop(&mut self) {
        if !self.done.load(std::sync::atomic::Ordering::Acquire) {
            retain_unconfirmed_cleanup(self.slot.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    };

    #[derive(Default)]
    struct State {
        events: Mutex<Vec<&'static str>>,
        refuse_shutdown: AtomicBool,
        refuse_cleanup: AtomicBool,
    }
    #[derive(Clone)]
    struct TestEngine(Arc<State>);
    impl Engine for TestEngine {
        fn shutdown_service_session(&self) -> Result<(), String> {
            self.0.events.lock().unwrap().push("shutdown");
            if self.0.refuse_shutdown.load(Ordering::Acquire) {
                Err("unconfirmed".into())
            } else {
                Ok(())
            }
        }
        fn reopen(_: &Path, _: &str) -> Result<Self, String> {
            let state = restart_state();
            state.events.lock().unwrap().push("reopen");
            Ok(Self(state))
        }
        fn attach_waker(&self, _: Wake) {}
        fn verify(&self, session: SessionCookies) -> Result<(), String> {
            if session.session_id != "selected" {
                return Err("foreign session".into());
            }
            self.0.events.lock().unwrap().push("verify");
            Ok(())
        }
    }
    struct TestProfile(Arc<State>);
    impl Profile for TestProfile {
        fn path(&self) -> std::io::Result<&Path> {
            Ok(Path::new("/explicit-granted-profile"))
        }
        fn cleanup_after_engine_exit(&mut self) -> std::io::Result<()> {
            self.0.events.lock().unwrap().push("cleanup");
            if self.0.refuse_cleanup.load(Ordering::Acquire) {
                Err(std::io::Error::other("retained"))
            } else {
                Ok(())
            }
        }
    }
    fn restart_state() -> Arc<State> {
        static STATE: std::sync::OnceLock<Arc<State>> = std::sync::OnceLock::new();
        STATE.get_or_init(Arc::default).clone()
    }
    fn wake() -> (Wake, mpsc::Receiver<()>) {
        let (send, receive) = mpsc::channel();
        (
            Arc::new(move || {
                let _ = send.send(());
            }),
            receive,
        )
    }
    fn session() -> SessionCookies {
        SessionCookies {
            schema: 1,
            session_id: "selected".into(),
            service: crate::sessions::Service::Slack,
            identity_public_key: [1; 32],
            setup_binding: "binding".into(),
            source_snapshot_digest: [2; 32],
            cookies: Vec::new(),
            origin_storage: Vec::new(),
        }
    }
    #[test]
    fn restart_reaps_before_reopen_and_verifies_on_replacement() {
        let state = restart_state();
        let (wake, receive) = wake();
        let mut restart = NativeRestart::start(
            TestEngine(state.clone()),
            TestProfile(state.clone()),
            session(),
            Some(wake),
        );
        tilo_test_env::recv("the restart to wake its owner", &receive);
        // Joining is only test/teardown work; the production poll never waits.
        restart.handle.take().unwrap().join().unwrap();
        let (engine, profile) = restart.poll().unwrap().unwrap();
        assert_eq!(
            *state.events.lock().unwrap(),
            ["shutdown", "reopen", "verify"]
        );
        drop(restart);
        let done = Arc::new(AtomicBool::new(false));
        let mut cleanup = cleanup_after_native_exit(engine, profile, done.clone(), None);
        cleanup.finish().unwrap();
        assert!(done.load(Ordering::Acquire));
        assert_eq!(
            *state.events.lock().unwrap(),
            ["shutdown", "reopen", "verify", "shutdown", "cleanup"]
        );
    }
    tilo_test_env::scenario! {
        fn teardown_keeps_ownership_until_it_is_confirmed;

        phase failed_shutdown_keeps_profile_and_retries_under_same_ownership;
        {
            let state = Arc::new(State::default());
            state.refuse_shutdown.store(true, Ordering::Release);
            let done = Arc::new(AtomicBool::new(false));
            let mut cleanup = cleanup_after_native_exit(
                TestEngine(state.clone()),
                TestProfile(state.clone()),
                done.clone(),
                None,
            );
            assert!(cleanup.finish().is_err());
            assert!(!done.load(Ordering::Acquire));
            assert_eq!(*state.events.lock().unwrap(), ["shutdown"]);
            assert!(cleanup.slot.lock().unwrap().is_some());
            state.refuse_shutdown.store(false, Ordering::Release);
            state.refuse_cleanup.store(true, Ordering::Release);
            assert!(cleanup.finish().is_err());
            assert!(cleanup.slot.lock().unwrap().is_some());
            state.refuse_cleanup.store(false, Ordering::Release);
            cleanup.finish().unwrap();
            assert!(done.load(Ordering::Acquire));
            assert!(cleanup.slot.lock().unwrap().is_none());
        }
    }
}
