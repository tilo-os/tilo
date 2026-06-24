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
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use smithay::backend::drm::{DrmDevice, DrmDeviceFd};
use smithay::backend::input::InputEvent;
use smithay::backend::libinput::{LibinputInputBackend, LibinputSessionInterface};
use smithay::backend::session::libseat::LibSeatSession;
use smithay::backend::session::Session;
use smithay::backend::udev::{all_gpus, UdevBackend};
use smithay::reexports::calloop::{EventLoop, LoopHandle};
use smithay::reexports::drm as drm_re;
use smithay::reexports::input::Libinput;

use super::devices::{filter_kms_nodes, ModeInfo};
use super::outputs::{ConnectorInfo, OutputRegistry};
use super::scanout::DrmScanout;
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
    /// Live scanout for the primary connector. `None` if modeset failed
    /// (e.g. not DRM master / no connected connector) — the compositor
    /// still runs headless in that case rather than crashing.
    pub scanout: Option<DrmScanout>,
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

        // 2.2. Bring up scanout on the primary GPU FIRST, keeping its fd
        //      open as DRM master. libseat refuses a second `open` of the
        //      same node, so the primary must NOT also be opened by the
        //      connector-enumeration pass below — we exclude it when
        //      scanout owns it. A modeset failure is non-fatal: log and
        //      run headless (the wayland socket is still served).
        let scanout = match primary_gpu.as_ref() {
            Some(path) => {
                let mut scanout_session = session.clone();
                match DrmScanout::new(&mut scanout_session, path) {
                    Ok(s) => {
                        tracing::info!("DRM backend: scanout live on primary GPU");
                        Some(s)
                    }
                    Err(e) => {
                        tracing::warn!("DRM backend: scanout setup failed ({e}); running headless");
                        None
                    }
                }
            }
            None => None,
        };

        // 2.5. Connector enumeration on the GPUs scanout does NOT own
        //      (the primary is held open above). Failures here are logged
        //      and skipped rather than aborting the boot.
        //
        //      `LibSeatSession` is cheap to clone (internal `Arc`), so
        //      we hand a borrow to the enumeration helper.
        let enum_gpus: Vec<PathBuf> = if scanout.is_some() {
            kms_gpus
                .iter()
                .filter(|p| Some((*p).clone()) != primary_gpu)
                .cloned()
                .collect()
        } else {
            kms_gpus.clone()
        };
        let mut session_for_enum = session.clone();
        let connectors = enumerate_connectors(&mut session_for_enum, &enum_gpus);
        tracing::info!(
            count = connectors.len(),
            "DRM backend: enumerated DRM connectors"
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
                    // TODO(drm-input): translate libinput's
                    // InputEvent::{Keyboard,Pointer*} into the
                    // `winit::WindowEvent` shape that
                    // `BackendEvent::Input(WindowEvent)` carries so
                    // `FrameBuilder::dispatch_event` can route real
                    // input on a bare TTY too. For now the DRM path
                    // silently drops input (same as before the
                    // hosted-mode fix); winit-backed dev binaries
                    // are the only path that delivers clicks.
                    let _ = state;
                }
                _ => {}
            })
            .map_err(|e| DrmBackendError::Calloop(format!("{e}")))?;

        let mut outputs = OutputRegistry::new();
        let populated = outputs.populate(&connectors);
        let mut output_meta: HashMap<OutputId, Output> = populated.into_iter().collect();

        // The scanout-driven primary is the canonical output the frame
        // loop paints into (`submit_frame` targets `outputs().first()`).
        // Expose it at the lowest id so it sorts first.
        if let Some(s) = &scanout {
            let (w, h) = s.size();
            output_meta.insert(
                OutputId(0),
                Output {
                    id: OutputId(0),
                    size: (w, h),
                    scale: 1.0,
                },
            );
        }

        let dispatch = DrmDispatchState {
            session: Some(session_state),
            outputs,
            events: Vec::new(),
            output_meta,
        };

        Ok(Self {
            event_loop,
            dispatch,
            _session: session,
            primary_gpu,
            scanout,
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

    /// Test-only seam: construct a `DrmRuntime`-shaped dispatch state
    /// with the given output set already populated. Used by tests in
    /// `super::mod` to verify `DrmBackend::outputs()` without touching
    /// real hardware. Not exposed outside `cfg(test)`.
    #[cfg(test)]
    pub fn synthesize_for_tests(outputs: Vec<(OutputId, Output)>) -> DrmDispatchState {
        let mut registry = OutputRegistry::new();
        // Re-intern so the registry's `next_id` is consistent with the
        // synthesised ids. The keys we feed are arbitrary; the test
        // only cares about `output_meta`.
        for (id, _) in &outputs {
            registry.intern(super::outputs::DrmOutputKey {
                device: 0,
                connector: id.0,
            });
        }
        DrmDispatchState {
            session: None,
            outputs: registry,
            events: Vec::new(),
            output_meta: outputs.into_iter().collect(),
        }
    }
}

/// Enumerate every connected connector on every `cardN` node in
/// `kms_gpus`. Each device is opened through the supplied session
/// (`session.open` returns an `OwnedFd` privileged via libseat); a
/// `DrmDevice` is built around the fd, its resource handles are read,
/// and each connector is queried (`force_probe = true` so EDID/modes
/// are fresh at boot). Failures on individual devices/connectors are
/// **logged and skipped** rather than aborted — a single misbehaving
/// GPU shouldn't down the whole compositor.
///
/// Returns the full flattened connector list ready to feed into
/// [`OutputRegistry::populate`].
///
/// **Side-effect**: temporarily holds DRM master on each opened
/// device. The fds are dropped at the end of this function, so master
/// is released before we hand control back to the caller for the
/// page-flip wiring. Phase 5.3 will keep the fds alive and reuse them.
pub fn enumerate_connectors<S>(session: &mut S, kms_gpus: &[PathBuf]) -> Vec<ConnectorInfo>
where
    S: smithay::backend::session::Session,
{
    let mut all = Vec::new();
    for path in kms_gpus {
        match enumerate_connectors_on_device(session, path) {
            Ok(mut conns) => all.append(&mut conns),
            Err(e) => {
                tracing::warn!(?path, "DRM connector enumeration failed on device: {e}");
            }
        }
    }
    all
}

/// Open a single `cardN` node through the session, build a
/// `DrmDevice`, and project each connector into [`ConnectorInfo`].
fn enumerate_connectors_on_device<S>(
    session: &mut S,
    path: &Path,
) -> Result<Vec<ConnectorInfo>, String>
where
    S: smithay::backend::session::Session,
{
    use drm_re::control::Device as ControlDevice;
    use smithay::reexports::rustix::fs::OFlags;
    use smithay::utils::DeviceFd;
    use smithay::backend::session::AsErrno;

    // O_RDWR|O_CLOEXEC|O_NONBLOCK matches what smithay's anvil example
    // uses for the DRM device fd.
    let fd = session
        .open(
            path,
            OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK,
        )
        .map_err(|e| format!("session.open({path:?}): errno={:?}", e.as_errno()))?;
    let device_fd = DrmDeviceFd::new(DeviceFd::from(fd));
    let dev_id_raw: u64 = device_fd
        .dev_id()
        .map_err(|e| format!("dev_id: {e}"))?
        as u64;

    let (drm_device, _notifier) =
        DrmDevice::new(device_fd, /* disable_connectors */ false)
            .map_err(|e| format!("DrmDevice::new: {e}"))?;

    let resources = drm_device
        .resource_handles()
        .map_err(|e| format!("resource_handles: {e}"))?;

    let mut connectors = Vec::with_capacity(resources.connectors().len());
    for &handle in resources.connectors() {
        match drm_device.get_connector(handle, /* force_probe */ true) {
            Ok(info) => {
                let connected = matches!(
                    info.state(),
                    drm_re::control::connector::State::Connected
                );
                let modes: Vec<ModeInfo> = info
                    .modes()
                    .iter()
                    .map(|m| {
                        let (w, h) = m.size();
                        let preferred = m
                            .mode_type()
                            .contains(drm_re::control::ModeTypeFlags::PREFERRED);
                        ModeInfo {
                            width: w,
                            height: h,
                            // `vrefresh()` returns plain Hz; multiply
                            // by 1000 to get milli-Hz (mode_info's
                            // canonical unit so 59.94 vs 60.00 stay
                            // distinguishable when fractional rates
                            // become available upstream).
                            refresh_mhz: m.vrefresh().saturating_mul(1000),
                            preferred,
                        }
                    })
                    .collect();
                connectors.push(ConnectorInfo {
                    device_id: dev_id_raw,
                    connector_id: u32::from(handle),
                    connected,
                    modes,
                });
            }
            Err(e) => {
                tracing::warn!(?handle, "DRM get_connector failed: {e}");
            }
        }
    }
    Ok(connectors)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compositor::backend::drm::devices::ModeInfo;

    // These tests build a `DrmDispatchState` directly; they avoid
    // calling `DrmRuntime::new` which would try to open a real seat.

    /// Pure-data stub `Session` for unit tests. Always fails to open
    /// any path (the real seat handling is owned by libseat at runtime
    /// and can't be exercised without a TTY), but the failure path is
    /// itself something we want to verify — `enumerate_connectors`
    /// must log + skip, not crash.
    struct StubSession {
        seat_name: String,
    }

    #[derive(Debug)]
    struct StubError;

    impl std::fmt::Display for StubError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "stub")
        }
    }

    impl smithay::backend::session::AsErrno for StubError {
        fn as_errno(&self) -> Option<i32> {
            Some(libc::ENODEV)
        }
    }

    impl smithay::backend::session::Session for StubSession {
        type Error = StubError;

        fn open(
            &mut self,
            _path: &std::path::Path,
            _flags: smithay::reexports::rustix::fs::OFlags,
        ) -> Result<std::os::fd::OwnedFd, Self::Error> {
            Err(StubError)
        }

        fn close(&mut self, _fd: std::os::fd::OwnedFd) -> Result<(), Self::Error> {
            Err(StubError)
        }

        fn change_vt(&mut self, _vt: i32) -> Result<(), Self::Error> {
            Err(StubError)
        }

        fn is_active(&self) -> bool {
            true
        }

        fn seat(&self) -> String {
            self.seat_name.clone()
        }
    }

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
        // `BackendEvent::Input` now carries the actual `WindowEvent`
        // (changed when hosted-mode click routing landed); synthesise
        // a minimal CursorMoved so the variant constructs.
        let fake = winit::event::WindowEvent::CursorMoved {
            device_id: unsafe { std::mem::zeroed() },
            position: winit::dpi::PhysicalPosition::new(0.0, 0.0),
        };
        s.push(BackendEvent::Input(fake));
        s.push(BackendEvent::Redraw);
        assert_eq!(s.events.len(), 2);
        assert!(matches!(s.events[0], BackendEvent::Input(_)));
        assert!(matches!(s.events[1], BackendEvent::Redraw));
    }

    #[test]
    fn enumerate_connectors_returns_empty_when_no_devices() {
        let mut session = StubSession {
            seat_name: "seat0".into(),
        };
        let conns = enumerate_connectors(&mut session, &[]);
        assert!(
            conns.is_empty(),
            "no card nodes → no connectors (got {} entries)",
            conns.len()
        );
    }

    #[test]
    fn enumerate_connectors_skips_devices_that_fail_to_open() {
        // The stub session refuses every open(), so even when paths
        // are supplied, we must end up with zero connectors and a
        // warn-log rather than a panic.
        let mut session = StubSession {
            seat_name: "seat0".into(),
        };
        let conns = enumerate_connectors(
            &mut session,
            &[PathBuf::from("/dev/dri/card0"), PathBuf::from("/dev/dri/card1")],
        );
        assert!(
            conns.is_empty(),
            "failed-to-open devices must be skipped, not crash"
        );
    }

    /// Pure-logic mirror of the policy `enumerate_connectors_on_device`
    /// implements when projecting DRM modes into `ModeInfo`s, the
    /// `preferred` flag is honored. The full flow needs a real DRM
    /// device, so we verify the *policy* end-to-end via
    /// `super::devices::preferred_mode` instead — that's what the
    /// runtime calls (via `OutputRegistry::populate`). This test
    /// pins the contract: given a list of modes containing a
    /// preferred one, the picked mode is the preferred one.
    #[test]
    fn preferred_mode_selection_picks_first_preferred() {
        use super::super::devices::preferred_mode;
        let modes = [
            ModeInfo {
                width: 3840,
                height: 2160,
                refresh_mhz: 60_000,
                preferred: false,
            },
            ModeInfo {
                width: 1920,
                height: 1080,
                refresh_mhz: 60_000,
                preferred: true,
            },
            ModeInfo {
                width: 2560,
                height: 1440,
                refresh_mhz: 144_000,
                preferred: false,
            },
        ];
        let picked = preferred_mode(&modes).expect("non-empty modes");
        assert!(picked.preferred, "preferred flag must win over higher resolution");
        assert_eq!(picked.width, 1920);
        assert_eq!(picked.height, 1080);
    }

    /// And when none of the modes are flagged preferred, the policy
    /// falls back to the highest resolution × refresh, matching what
    /// `enumerate_connectors_on_device` will deliver to the registry.
    #[test]
    fn preferred_mode_selection_falls_back_to_highest_when_no_preferred() {
        use super::super::devices::preferred_mode;
        let modes = [
            ModeInfo {
                width: 1920,
                height: 1080,
                refresh_mhz: 60_000,
                preferred: false,
            },
            ModeInfo {
                width: 2560,
                height: 1440,
                refresh_mhz: 60_000,
                preferred: false,
            },
        ];
        let picked = preferred_mode(&modes).expect("non-empty");
        assert_eq!(picked.width, 2560);
    }

    #[test]
    fn synthesize_for_tests_populates_output_meta() {
        let outs = vec![
            (OutputId(0), Output { id: OutputId(0), size: (1920, 1080), scale: 1.0 }),
            (OutputId(1), Output { id: OutputId(1), size: (1280, 720), scale: 1.0 }),
        ];
        let state = DrmRuntime::synthesize_for_tests(outs);
        assert_eq!(state.output_meta.len(), 2);
        assert!(state.output_meta.contains_key(&OutputId(0)));
        assert!(state.output_meta.contains_key(&OutputId(1)));
    }
}
