//! Native session restart and cleanup custody, independent of the rendering host.
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

type CleanupCustody<E, P> = (E, P);
type CleanupSlot<E, P> = std::sync::Arc<std::sync::Mutex<Option<CleanupCustody<E, P>>>>;

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

    pub fn poll(&mut self) -> Option<Result<CleanupCustody<E, P>, ()>> {
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
struct RestartCustody<E: Engine, P: Profile> {
    slot: CleanupSlot<E, P>,
    payload: Option<CleanupCustody<E, P>>,
}
impl<E: Engine, P: Profile> RestartCustody<E, P> {
    fn store(&mut self) {
        if let Some(payload) = self.payload.take() {
            *self
                .slot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(payload);
        }
    }
}
impl<E: Engine, P: Profile> Drop for RestartCustody<E, P> {
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
    let mut custody = RestartCustody { slot, payload };
    let Some((engine, profile)) = custody.payload.as_ref() else {
        return Err(());
    };
    engine.shutdown_service_session().map_err(|_| ())?;
    if cancelled.load(std::sync::atomic::Ordering::Acquire) {
        return finish_cancelled_restart(&mut custody, done);
    }
    let replacement =
        E::reopen(profile.path().map_err(|_| ())?, &session.session_id).map_err(|_| ())?;
    if let Some((engine, _)) = custody.payload.as_mut() {
        *engine = replacement.clone();
    }
    if let Some(waker) = waker {
        replacement.attach_waker(waker);
    }
    if cancelled.load(std::sync::atomic::Ordering::Acquire) {
        return finish_cancelled_restart(&mut custody, done);
    }
    replacement.verify(session).map_err(|_| ())?;
    if cancelled.load(std::sync::atomic::Ordering::Acquire) {
        return finish_cancelled_restart(&mut custody, done);
    }
    custody.store();
    Ok(())
}

fn finish_cancelled_restart<E: Engine, P: Profile>(
    custody: &mut RestartCustody<E, P>,
    done: &std::sync::atomic::AtomicBool,
) -> Result<(), ()> {
    let Some((engine, profile)) = custody.payload.as_mut() else {
        return Err(());
    };
    engine.shutdown_service_session().map_err(|_| ())?;
    profile.cleanup_after_engine_exit().map_err(|_| ())?;
    custody.payload = None;
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
            let mut custody = RestartCustody {
                slot: worker_slot,
                payload,
            };
            if let Some((engine, profile)) = custody.payload.as_mut()
                && engine.shutdown_service_session().is_ok()
                && profile.cleanup_after_engine_exit().is_ok()
            {
                custody.payload = None;
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
            let mut custody = RestartCustody {
                slot: self.slot.clone(),
                payload,
            };
            if let Some((engine, profile)) = custody.payload.as_mut()
                && engine.shutdown_service_session().is_ok()
                && profile.cleanup_after_engine_exit().is_ok()
            {
                custody.payload = None;
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

/// A private view prepared off the frame thread.
pub trait OpeningValue: Send + 'static {
    fn request_close(&mut self);
    fn close_complete(&self) -> bool;
    /// # Errors
    /// Returns an error when the native operation cannot be confirmed.
    fn finish_close(&mut self) -> Result<(), String>;
}
pub struct Opening<T: OpeningValue> {
    worker: Option<std::thread::JoinHandle<()>>,
    result: std::sync::Arc<std::sync::Mutex<Option<Result<T, String>>>>,
    cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    shutdown_failed: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
impl<T: OpeningValue> Default for Opening<T> {
    fn default() -> Self {
        Self {
            worker: None,
            result: std::sync::Arc::default(),
            cancelled: std::sync::Arc::default(),
            shutdown_failed: std::sync::Arc::default(),
        }
    }
}
impl<T: OpeningValue> Opening<T> {
    pub fn cancelled(&self) -> bool {
        self.cancelled.load(std::sync::atomic::Ordering::Acquire)
    }
    pub fn started(&self) -> bool {
        self.worker.is_some()
    }
    pub fn poll(&mut self) -> Option<Result<T, String>> {
        let answer = self.result.try_lock().ok()?.take();
        if let Some(answer) = answer {
            return Some(answer);
        }
        self.worker
            .as_ref()?
            .is_finished()
            .then(|| Err("This account could not be opened".into()))
    }
    /// # Errors
    /// Returns an error when the native operation cannot be confirmed.
    pub fn start(
        &mut self,
        prepare: impl FnOnce() -> Result<T, String> + Send + 'static,
        waker: Option<Wake>,
    ) -> Result<(), String> {
        let result = self.result.clone();
        let cancelled = self.cancelled.clone();
        let shutdown_failed = self.shutdown_failed.clone();
        self.worker = Some(
            std::thread::Builder::new()
                .name("browser-service-open".into())
                .spawn(move || {
                    let mut answer = prepare();
                    if let Ok(mut slot) = result.lock() {
                        if cancelled.load(std::sync::atomic::Ordering::Acquire) {
                            if let Ok(view) = &mut answer
                                && view.finish_close().is_err()
                            {
                                shutdown_failed.store(true, std::sync::atomic::Ordering::Release);
                            }
                        } else {
                            *slot = Some(answer);
                        }
                    }
                    if let Some(wake) = waker {
                        wake();
                    }
                })
                .map_err(|_| "This account could not be opened")?,
        );
        Ok(())
    }
    pub fn request_close(&mut self) -> bool {
        self.cancelled
            .store(true, std::sync::atomic::Ordering::Release);
        if let Ok(mut result) = self.result.try_lock()
            && let Some(Ok(view)) = result.as_mut()
        {
            view.request_close();
        }
        self.close_complete()
    }
    pub fn close_complete(&self) -> bool {
        self.cancelled.load(std::sync::atomic::Ordering::Acquire)
            && !self
                .shutdown_failed
                .load(std::sync::atomic::Ordering::Acquire)
            && self
                .worker
                .as_ref()
                .is_none_or(std::thread::JoinHandle::is_finished)
            && self
                .result
                .try_lock()
                .is_ok_and(|result| match result.as_ref() {
                    Some(Ok(view)) => view.close_complete(),
                    _ => true,
                })
    }
    /// # Errors
    /// Returns an error when the native operation cannot be confirmed.
    pub fn finish_close(&mut self) -> Result<(), String> {
        self.cancelled
            .store(true, std::sync::atomic::Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker
                .join()
                .map_err(|_| "This account could not be closed")?;
        }
        if self
            .shutdown_failed
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err("This account could not be closed".into());
        }
        let view = self
            .result
            .lock()
            .map_err(|_| "This account could not be closed")?
            .take();
        if let Some(Ok(mut view)) = view {
            view.finish_close()?;
        }
        Ok(())
    }
}
impl<T: OpeningValue> Drop for Opening<T> {
    fn drop(&mut self) {
        let _ = self.finish_close();
    }
}

/// The local fence/reap/delete barrier, before the host revokes canonical custody.
pub struct Forget<E: Engine, P: Profile> {
    cleanup: std::sync::Arc<std::sync::Mutex<Option<NativeCleanup<E, P>>>>,
    receive: std::sync::mpsc::Receiver<Result<(), String>>,
}
impl<E: Engine, P: Profile> Forget<E, P> {
    /// # Errors
    /// Returns retained cleanup custody if the worker cannot start.
    pub fn start(
        cleanup: Option<NativeCleanup<E, P>>,
        closed: impl FnOnce() -> Result<(), String> + Send + 'static,
        wake: Wake,
    ) -> Result<Self, Option<NativeCleanup<E, P>>> {
        let cleanup = std::sync::Arc::new(std::sync::Mutex::new(cleanup));
        let worker_cleanup = cleanup.clone();
        let (send, receive) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("browser-service-forget".into())
            .spawn(move || {
                let mut cleanup = worker_cleanup
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let result = if let Some(cleanup) = cleanup.as_mut() {
                    cleanup.finish().or_else(|_| cleanup.finish())
                } else {
                    closed()
                };
                drop(cleanup);
                let _ = send.send(result);
                wake();
            });
        if spawned.is_err() {
            return Err(cleanup
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take());
        }
        Ok(Self { cleanup, receive })
    }
    pub fn poll(&self) -> Option<Result<(), String>> {
        match self.receive.try_recv() {
            Ok(result) => Some(result),
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
            Err(_) => Some(Err("Account cleanup could not be confirmed".into())),
        }
    }
    pub fn take_cleanup(&self) -> Option<NativeCleanup<E, P>> {
        self.cleanup
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
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
    #[test]
    fn failed_shutdown_keeps_profile_and_retries_under_same_custody() {
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

    struct TestView(Arc<AtomicBool>);
    impl OpeningValue for TestView {
        fn request_close(&mut self) {
            self.0.store(true, Ordering::Release);
        }
        fn close_complete(&self) -> bool {
            self.0.load(Ordering::Acquire)
        }
        fn finish_close(&mut self) -> Result<(), String> {
            self.request_close();
            Ok(())
        }
    }
    #[test]
    fn cancelling_open_waits_for_worker_and_closes_unpublished_view() {
        let mut opening = Opening::default();
        let closed = Arc::new(AtomicBool::new(false));
        let worker_closed = closed.clone();
        let (release, gate) = mpsc::channel();
        let (wake, receive) = wake();
        opening
            .start(
                move || {
                    gate.recv().unwrap();
                    Ok(TestView(worker_closed))
                },
                Some(wake),
            )
            .unwrap();
        assert!(!opening.request_close());
        release.send(()).unwrap();
        tilo_test_env::recv("the restart to wake its owner", &receive);
        opening.finish_close().unwrap();
        assert!(closed.load(Ordering::Acquire));
        assert!(opening.close_complete());
    }
    #[test]
    fn forget_publishes_only_after_local_barrier() {
        let state = Arc::new(State::default());
        let done = Arc::new(AtomicBool::new(false));
        let cleanup = cleanup_after_native_exit(
            TestEngine(state.clone()),
            TestProfile(state.clone()),
            done.clone(),
            None,
        );
        let (wake, receive) = wake();
        let forget = Forget::start(
            Some(cleanup),
            || panic!("live cleanup must keep its lease"),
            wake,
        )
        .unwrap_or_else(|_| panic!("worker start"));
        tilo_test_env::recv("the restart to wake its owner", &receive);
        assert!(forget.poll().unwrap().is_ok());
        assert!(done.load(Ordering::Acquire));
        assert_eq!(*state.events.lock().unwrap(), ["shutdown", "cleanup"]);
        assert!(forget.take_cleanup().is_some());
    }
}
