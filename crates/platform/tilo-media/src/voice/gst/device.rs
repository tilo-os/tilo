//! Bounded device state transitions. No driver state wait runs on a caller.
use super::{AudioError, RunningPipeline, check_bus, gst};
use gst::prelude::*;
use std::{
    os::fd::{AsRawFd, OwnedFd},
    sync::{
        Arc, Mutex, OnceLock, Weak,
        atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
    },
    thread::Thread,
    time::{Duration, Instant},
};
use tilo_lock::MutexRecover as _;

pub(super) const MAX_DEVICE_WORKERS: usize = 8;
static OUTSTANDING: AtomicUsize = AtomicUsize::new(0);
type Guard = Arc<dyn Fn() -> bool + Send + Sync>;

pub(super) struct Permit;
impl Permit {
    pub(super) fn acquire() -> Result<Self, AudioError> {
        OUTSTANDING
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < MAX_DEVICE_WORKERS).then(|| count.saturating_add(1))
            })
            .map(|_| Self)
            .map_err(|_| AudioError::Capacity)
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        OUTSTANDING.fetch_sub(1, Ordering::AcqRel);
    }
}

#[derive(Clone)]
pub struct CaptureStop(Arc<State>);
struct State {
    current: AtomicBool,
    ready: AtomicBool,
    finished: AtomicBool,
    failure: AtomicU8,
    /// When the worker began the Playing transition: the startup deadline
    /// measures the device from there, not the wait for the worker to run.
    started: OnceLock<Instant>,
    startup_deadline: Option<Duration>,
    remote_fd: Option<OwnedFd>,
    guard: Option<Guard>,
    thread: OnceLock<Thread>,
}
impl CaptureStop {
    /// Immediately revokes this generation and shuts down its exact daemon
    /// socket, including `GStreamer`'s duplicates. It never waits for a driver,
    /// takes an outer service mutex, joins a thread, or reconnects. The bounded
    /// worker performs Null cleanup and retains its permit until that completes.
    pub fn stop(&self) {
        self.0.current.store(false, Ordering::Release);
        self.0.ready.store(false, Ordering::Release);
        if let Some(fd) = &self.0.remote_fd {
            // SAFETY: fd is owned for this State's lifetime. shutdown does
            // not transfer ownership; duplicate descriptors share shutdown.
            unsafe {
                libc::shutdown(fd.as_raw_fd(), libc::SHUT_RDWR);
            }
        }
        if let Some(thread) = self.0.thread.get() {
            thread.unpark();
        }
    }
    pub(super) fn current(&self) -> bool {
        self.0.current.load(Ordering::Acquire)
    }
    pub(super) fn finished(&self) -> bool {
        self.0.finished.load(Ordering::Acquire)
    }
    fn fail(&self) {
        self.0.failure.store(1, Ordering::Release);
        self.stop();
    }
    fn startup_deadline_passed(&self, now: Instant) -> bool {
        self.0
            .startup_deadline
            .zip(self.0.started.get())
            .is_some_and(|(deadline, started)| now.saturating_duration_since(*started) >= deadline)
    }
    fn allowed(&self) -> bool {
        self.current() && self.0.guard.as_ref().is_none_or(|guard| guard())
    }
}

type Registry = Arc<Mutex<Vec<Weak<State>>>>;
fn registry() -> Result<&'static Registry, AudioError> {
    static REGISTRY: OnceLock<Result<Registry, ()>> = OnceLock::new();
    REGISTRY
        .get_or_init(|| {
            let registry: Registry = Arc::new(Mutex::new(Vec::with_capacity(MAX_DEVICE_WORKERS)));
            let observed = registry.clone();
            std::thread::Builder::new()
                .name("voice-device-deadlines".into())
                .spawn(move || {
                    loop {
                        {
                            let mut slots = observed.lock_recover();
                            slots.retain(|slot| {
                                let Some(state) = slot.upgrade() else {
                                    return false;
                                };
                                let stop = CaptureStop(state);
                                if stop.finished() {
                                    return false;
                                }
                                if !stop.allowed() {
                                    stop.stop();
                                } else if !stop.0.ready.load(Ordering::Acquire)
                                    && stop.startup_deadline_passed(Instant::now())
                                {
                                    stop.fail();
                                }
                                true
                            });
                        }
                        std::thread::sleep(Duration::from_millis(5));
                    }
                })
                .map_err(|_| ())?;
            Ok(registry)
        })
        .as_ref()
        .map_err(|()| AudioError::Unavailable)
}

pub(super) struct DevicePipeline {
    pub(super) pipeline: gst::Pipeline,
    stop: CaptureStop,
}
impl DevicePipeline {
    /// `startup_deadline` revokes a device that is not ready in time. It is
    /// wall-clock, so it also fires on a pipeline that is only starved of
    /// CPU: give one only where a driver can hold the transition.
    pub(super) fn start(
        pipeline: gst::Pipeline,
        startup_deadline: Option<Duration>,
        remote_fd: Option<OwnedFd>,
        guard: Option<Guard>,
        needs_sample: bool,
        permit: Permit,
    ) -> Result<Self, AudioError> {
        let registry = registry()?;
        let stop = CaptureStop(Arc::new(State {
            current: AtomicBool::new(true),
            ready: AtomicBool::new(false),
            finished: AtomicBool::new(false),
            failure: AtomicU8::new(0),
            started: OnceLock::new(),
            startup_deadline,
            remote_fd,
            guard,
            thread: OnceLock::new(),
        }));
        {
            let mut slots = registry.lock_recover();
            slots.retain(|slot| {
                slot.upgrade()
                    .is_some_and(|state| !state.finished.load(Ordering::Acquire))
            });
            if slots.len() >= MAX_DEVICE_WORKERS {
                return Err(AudioError::Capacity);
            }
            slots.push(Arc::downgrade(&stop.0));
        }
        let worker_stop = stop.clone();
        let worker_pipeline = pipeline.clone();
        let worker = std::thread::Builder::new()
            .name("voice-device-state".into())
            .spawn(move || {
                let pipeline = RunningPipeline(worker_pipeline);
                if worker_stop.allowed() {
                    let _ = worker_stop.0.started.set(Instant::now());
                    tracing::debug!(
                        capture = needs_sample,
                        "native voice device transition started"
                    );
                    let transition = pipeline.0.set_state(gst::State::Playing);
                    tracing::debug!(
                        capture = needs_sample,
                        transition_ok = transition.is_ok(),
                        state = ?pipeline.0.current_state(),
                        "native voice device transition returned"
                    );
                    if transition.is_err() || check_bus(&pipeline.0).is_err() {
                        worker_stop.fail();
                    }
                } else {
                    worker_stop.stop();
                }
                while worker_stop.allowed() {
                    if !needs_sample && pipeline.0.current_state() == gst::State::Playing {
                        worker_stop.0.ready.store(true, Ordering::Release);
                    }
                    if check_bus(&pipeline.0).is_err() {
                        worker_stop.fail();
                        break;
                    }
                    std::thread::park_timeout(Duration::from_millis(5));
                }
                worker_stop.stop();
                // A driver may block here. Its permit and FD custody remain live;
                // cancellation and future admission never wait for this thread.
                loop {
                    let _ = pipeline.0.set_state(gst::State::Null);
                    if pipeline.0.current_state() == gst::State::Null {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                drop(pipeline);
                worker_stop.0.finished.store(true, Ordering::Release);
                drop(permit);
            })
            .map_err(|_| {
                stop.stop();
                AudioError::Unavailable
            })?;
        let _ = stop.0.thread.set(worker.thread().clone());
        drop(worker); // Detached: the worker, not this handle, retains the permit.
        Ok(Self { pipeline, stop })
    }
    pub(super) fn stop_handle(&self) -> CaptureStop {
        self.stop.clone()
    }
    pub(super) fn ready(&self) -> bool {
        self.stop.allowed() && self.stop.0.ready.load(Ordering::Acquire)
    }
    pub(super) fn mark_sample(&self) {
        if self.stop.allowed() {
            self.stop.0.ready.store(true, Ordering::Release);
        }
    }
    pub(super) fn failure(&self) -> Option<AudioError> {
        (self.stop.0.failure.load(Ordering::Acquire) != 0).then_some(AudioError::DeviceLost)
    }
    /// Whether the startup deadline had passed at `now` without the device
    /// becoming ready.
    pub(super) fn startup_expired(&self, now: Instant) -> bool {
        !self.ready() && self.stop.startup_deadline_passed(now)
    }
    pub(super) fn stop(&self) {
        self.stop.stop();
    }
}
impl Drop for DevicePipeline {
    fn drop(&mut self) {
        self.stop.stop();
    }
}

#[cfg(test)]
pub(super) fn outstanding_workers() -> usize {
    OUTSTANDING.load(Ordering::Acquire)
}
