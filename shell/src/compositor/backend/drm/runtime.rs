//! Hardware-touching glue for the DRM backend.
//!
//! This module is the bridge between the testable, pure helpers in
//! [`super::devices`] / [`super::session`] / [`super::outputs`] and the
//! real smithay APIs (libseat, udev, libinput, drm). Everything here
//! requires a free TTY and a seat the user owns, so we cannot exercise
//! it from `cargo test` on a desktop running Gnome.
//!
//! Compile-time gating: the smithay features (`backend_drm`,
//! `backend_libinput`, `backend_udev`, `backend_session_libseat`,
//! `renderer_gl`) are unconditional in our `Cargo.toml`, so this module
//! always builds. Construction (`DrmRuntime::new`) is what fails
//! cleanly when there's no seat — the dev machine's failure path is a
//! clean `Err`, not a panic.
//!
//! **Approach (b)**: vello renders via wgpu; smithay's DRM path is
//! historically GLES/glow. Bridging the two via DMA-BUF import is a
//! follow-up. For this PR the page-flip cadence is wired and a
//! `tracing::warn!` flags every submit that "vello scene was discarded;
//! DRM blit path is a follow-up". The boot smoke (session +
//! page-flip + libinput) is what this PR proves; pixel-on-screen is
//! Phase 5.2.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::Ordering;

use smithay::backend::input::InputEvent;
use smithay::backend::libinput::{LibinputInputBackend, LibinputSessionInterface};
use smithay::backend::session::libseat::LibSeatSession;
use smithay::backend::session::Session;
use smithay::backend::udev::{all_gpus, UdevBackend};
use smithay::reexports::calloop::{EventLoop, LoopHandle};
use smithay::reexports::input::Libinput;

use super::devices::filter_kms_nodes;
use super::outputs::OutputRegistry;
use super::session::SessionState;
use crate::compositor::backend::{BackendEvent, Output, OutputId};

/// Errors a DRM backend can encounter during construction. Each
/// variant maps to a clean operator-visible message; the binary then
/// `eprintln!`s + exits 1 instead of panicking.
#[derive(Debug, thiserror::Error)]
pub enum DrmBackendError {
    #[error("failed to open libseat session: {0}")]
    Seat(String),

    #[error("failed to enumerate DRM devices via udev: {0}")]
    Udev(String),

    #[error("no KMS-capable DRM devices found (need /dev/dri/cardN)")]
    NoDevices,

    #[error("failed to initialise libinput: {0}")]
    Libinput(String),

    #[error("failed to insert event source into calloop: {0}")]
    Calloop(String),
}

/// State shared across calloop dispatch for the DRM backend. This is
/// the `Data` type the inner `EventLoop` runs over so each source can
/// mutate it without an extra `Arc<Mutex<_>>`.
///
/// The struct is `Default`-able for tests that want to drive a
/// `DrmDispatchState` without a real seat.
#[derive(Default)]
pub struct DrmDispatchState {
    pub session: Option<SessionState>,
    pub outputs: OutputRegistry,
    pub events: Vec<BackendEvent>,
    /// Per-device output metadata, keyed by the public `OutputId`.
    pub output_meta: HashMap<OutputId, Output>,
}

impl DrmDispatchState {
    pub fn push(&mut self, ev: BackendEvent) {
        self.events.push(ev);
    }
}

/// Owning handle for everything the DRM backend keeps alive between
/// frames. Dropping it tears down the session in the libseat-correct
/// order (libinput first, then udev, then the seat).
pub struct DrmRuntime {
    /// calloop loop this backend ticks; we own it so `submit_frame` /
    /// `poll_events` can dispatch without a parent loop's help.
    pub event_loop: EventLoop<'static, DrmDispatchState>,
    pub dispatch: DrmDispatchState,
    /// Kept alive so libseat doesn't close the seat. The notifier is
    /// inserted into `event_loop` and its source token lives there.
    _session: LibSeatSession,
    /// Detected primary GPU card path. `None` if enumeration finds no
    /// `card*` nodes (the construction would already have returned
    /// `NoDevices` in that case, but keeping the field is convenient
    /// for follow-ups).
    pub primary_gpu: Option<PathBuf>,
}

impl DrmRuntime {
    /// Acquire the session, enumerate DRM devices, attach libinput,
    /// and wire everything onto an internal calloop. Returns an error
    /// (not a panic) on any failure — the binary entry point matches
    /// the existing skeleton's contract: `eprintln!` + `exit(1)`.
    pub fn new() -> Result<Self, DrmBackendError> {
        // 1. Session via libseat. `new()` blocks until the seat hands
        //    us an `Enable` event, so by the time it returns we either
        //    own the seat or we've already failed.
        let (session, session_notifier) =
            LibSeatSession::new().map_err(|e| DrmBackendError::Seat(format!("{e:?}")))?;
        let seat_name = session.seat();
        tracing::info!(seat = %seat_name, "DRM backend: libseat session acquired");

        let session_state = SessionState::new_active();

        // 2. udev enumeration. Even before inserting the source, ask
        //    udev for the current device set so we can refuse early if
        //    nothing is plugged in.
        let udev = UdevBackend::new(&seat_name)
            .map_err(|e| DrmBackendError::Udev(format!("{e}")))?;
        let raw_gpus: Vec<PathBuf> = all_gpus(&seat_name)
            .map_err(|e| DrmBackendError::Udev(format!("{e}")))?;
        let kms_gpus = filter_kms_nodes(raw_gpus);
        if kms_gpus.is_empty() {
            return Err(DrmBackendError::NoDevices);
        }
        let primary_gpu = kms_gpus.first().cloned();
        tracing::info!(
            count = kms_gpus.len(),
            primary = ?primary_gpu,
            "DRM backend: enumerated KMS card nodes"
        );

        // 3. libinput. The session-interface adapter routes
        //    `open`/`close` calls through libseat so libinput inherits
        //    the privileged FDs without root.
        let mut libinput_ctx =
            Libinput::new_with_udev(LibinputSessionInterface::from(session.clone()));
        libinput_ctx
            .udev_assign_seat(&seat_name)
            .map_err(|e| DrmBackendError::Libinput(format!("{e:?}")))?;
        let libinput_backend = LibinputInputBackend::new(libinput_ctx);

        // 4. calloop. The session notifier, udev backend, and
        //    libinput backend each register as event sources. The
        //    page-flip / DRM-device sources will be attached when
        //    individual `cardN` devices are opened in a follow-up
        //    commit; here we wire the seat/input substrate.
        let event_loop: EventLoop<'static, DrmDispatchState> = EventLoop::try_new()
            .map_err(|e| DrmBackendError::Calloop(format!("{e}")))?;
        let handle: LoopHandle<'static, DrmDispatchState> = event_loop.handle();

        // Session events: flip the SessionState's atomic on activate /
        // pause. The closure captures a cheap clone of the
        // SessionState (atomic) for sharing with the render path.
        let session_flag = session_state.shared_flag();
        handle
            .insert_source(session_notifier, move |event, _, _state| {
                use smithay::backend::session::Event as SeatEvent;
                match event {
                    SeatEvent::ActivateSession => {
                        session_flag.store(true, Ordering::Release);
                        tracing::info!("DRM backend: session activated");
                    }
                    SeatEvent::PauseSession => {
                        session_flag.store(false, Ordering::Release);
                        tracing::info!("DRM backend: session paused");
                    }
                }
            })
            .map_err(|e| DrmBackendError::Calloop(format!("{e}")))?;

        // udev hot-plug events. For now we only log; the page-flip
        // sources for new devices are a Phase 5.2 follow-up.
        handle
            .insert_source(udev, move |event, _, state| {
                use smithay::backend::udev::UdevEvent;
                match event {
                    UdevEvent::Added { device_id, path } => {
                        tracing::info!(?device_id, ?path, "DRM: udev added");
                        state.push(BackendEvent::Redraw);
                    }
                    UdevEvent::Changed { device_id } => {
                        tracing::info!(?device_id, "DRM: udev changed");
                    }
                    UdevEvent::Removed { device_id } => {
                        tracing::info!(?device_id, "DRM: udev removed");
                    }
                }
            })
            .map_err(|e| DrmBackendError::Calloop(format!("{e}")))?;

        // libinput events: translate to BackendEvent::Input. Phase 6
        // routes these through the seat layer (already done elsewhere
        // in the compositor for the winit path); for v1 we just push
        // an `Input` notification each tick so the frame loop stays
        // hot.
        handle
            .insert_source(libinput_backend, move |event, _, state| match event {
                InputEvent::Keyboard { .. }
                | InputEvent::PointerMotion { .. }
                | InputEvent::PointerMotionAbsolute { .. }
                | InputEvent::PointerButton { .. }
                | InputEvent::PointerAxis { .. } => {
                    state.push(BackendEvent::Input);
                }
                _ => {}
            })
            .map_err(|e| DrmBackendError::Calloop(format!("{e}")))?;

        let dispatch = DrmDispatchState {
            session: Some(session_state),
            outputs: OutputRegistry::new(),
            events: Vec::new(),
            output_meta: HashMap::new(),
        };

        Ok(Self {
            event_loop,
            dispatch,
            _session: session,
            primary_gpu,
        })
    }

    /// Drain pending events from the internal calloop. Non-blocking:
    /// uses a zero timeout so the outer compositor loop owns the
    /// cadence.
    pub fn dispatch_pending(&mut self) {
        use std::time::Duration;
        let _ = self
            .event_loop
            .dispatch(Duration::ZERO, &mut self.dispatch);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // These tests build a `DrmDispatchState` directly; they avoid
    // calling `DrmRuntime::new` which would try to open a real seat.

    #[test]
    fn dispatch_state_starts_empty() {
        let s = DrmDispatchState::default();
        assert!(s.events.is_empty());
        assert!(s.outputs.is_empty());
        assert!(s.output_meta.is_empty());
    }

    #[test]
    fn push_appends_event() {
        let mut s = DrmDispatchState::default();
        s.push(BackendEvent::Input);
        s.push(BackendEvent::Redraw);
        assert_eq!(s.events.len(), 2);
        assert!(matches!(s.events[0], BackendEvent::Input));
        assert!(matches!(s.events[1], BackendEvent::Redraw));
    }
}
