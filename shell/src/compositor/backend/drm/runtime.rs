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
use smithay::backend::input::{
    AbsolutePositionEvent, Axis, ButtonState, InputEvent, KeyState, KeyboardKeyEvent,
    PointerAxisEvent, PointerButtonEvent, PointerMotionEvent,
};
use smithay::backend::libinput::{LibinputInputBackend, LibinputSessionInterface};
// xkb keymap/state for keyboard handling — smithay re-exports `xkbcommon`
// here, so we don't add a new dependency. `XkbConfig::default()` reads
// the `XKB_DEFAULT_*` env vars; we mirror that by compiling a keymap with
// empty rule names (see `KeyboardXkb::new`).
use smithay::input::keyboard::{keysyms, xkb, Keysym};
use winit::dpi::PhysicalPosition;
use winit::event::{
    ElementState, KeyEvent, Modifiers, MouseButton, MouseScrollDelta, TouchPhase, WindowEvent,
};
use winit::keyboard::{
    Key, KeyLocation, ModifiersState, NamedKey, NativeKeyCode, PhysicalKey, SmolStr,
};
use smithay::backend::session::libseat::LibSeatSession;
use smithay::backend::session::Session;
use smithay::backend::udev::{all_gpus, UdevBackend};
use smithay::reexports::calloop::{EventLoop, LoopHandle};
use smithay::reexports::drm as drm_re;
use smithay::reexports::input::Libinput;

use super::devices::{filter_kms_nodes, ModeInfo};
use super::outputs::{ConnectorInfo, OutputRegistry};
use super::scanout::{DrmScanout, HwCursor};
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
    /// Current pointer position in output coords. Updated from libinput
    /// motion; the compositor receives matching `CursorMoved` events and
    /// (when the hardware cursor is unavailable) the scanout composites a
    /// software cursor here.
    pub pointer: (f64, f64),
    /// Primary output size (px) — needed to transform libinput's
    /// absolute (0..1) coordinates and to clamp relative motion.
    pub output_size: (i32, i32),
    /// Hardware-cursor mover for the primary scanout. `Some` only when
    /// the KMS cursor plane is active; the libinput motion handlers call
    /// `move_to` on every pointer event so the cursor tracks at input
    /// rate without a framebuffer redraw. `None` → the scanout draws a
    /// software cursor from `pointer` instead.
    pub hw_cursor: Option<HwCursor>,
    /// xkb keymap + state for translating libinput keycodes into winit
    /// `KeyboardInput` events (the same wire the winit backend feeds to
    /// `FrameBuilder::dispatch_event`). `None` when the keymap couldn't
    /// be compiled — the session then runs pointer-only instead of
    /// aborting. Built from the `XKB_DEFAULT_*` environment so the
    /// installer-seeded layout reaches the session (see [`KeyboardXkb`]).
    pub keyboard: Option<KeyboardXkb>,
}

impl DrmDispatchState {
    pub fn push(&mut self, ev: BackendEvent) {
        self.events.push(ev);
    }

    /// Translate one libinput keyboard event through xkb and enqueue the
    /// resulting events: a `ModifiersChanged` when the Ctrl/Shift/Alt/
    /// Super mask flips (so chord handling in the chrome sees the right
    /// state) followed by the `KeyboardInput` itself. No-op when the xkb
    /// keymap failed to load.
    ///
    /// `keycode` is the value reported by smithay's
    /// [`KeyboardKeyEvent::key_code`], which already includes the +8
    /// evdev→xkb offset, so it feeds straight into the xkb state.
    fn handle_keyboard_key(&mut self, keycode: xkb::Keycode, pressed: bool) {
        // Scope the `&mut self.keyboard` borrow so we can push onto
        // `self.events` afterwards without aliasing.
        let outcome = {
            let Some(kbd) = self.keyboard.as_mut() else {
                return;
            };
            kbd.process(keycode, pressed)
        };
        if let Some(mods) = outcome.modifiers_changed {
            self.events.push(BackendEvent::ModifiersChanged(mods));
        }
        if let Some(input) = outcome.input {
            self.events.push(BackendEvent::Input(input));
        }
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
                // Absolute motion (usb-tablet / touch): map normalized
                // coords through the output size to a pixel position.
                InputEvent::PointerMotionAbsolute { event } => {
                    let (w, h) = state.output_size;
                    let x = event.x_transformed(w.max(1));
                    let y = event.y_transformed(h.max(1));
                    state.pointer = (x, y);
                    // Reposition the hardware cursor plane immediately —
                    // no framebuffer redraw needed for a cursor move.
                    if let Some(c) = &state.hw_cursor {
                        c.move_to(x as i32, y as i32);
                    }
                    state.push(BackendEvent::Input(cursor_moved_event(x, y)));
                }
                // Relative motion (mouse): accumulate + clamp to output.
                InputEvent::PointerMotion { event } => {
                    let (w, h) = state.output_size;
                    let max_x = (w.max(1) - 1) as f64;
                    let max_y = (h.max(1) - 1) as f64;
                    let x = (state.pointer.0 + event.delta_x()).clamp(0.0, max_x);
                    let y = (state.pointer.1 + event.delta_y()).clamp(0.0, max_y);
                    state.pointer = (x, y);
                    if let Some(c) = &state.hw_cursor {
                        c.move_to(x as i32, y as i32);
                    }
                    state.push(BackendEvent::Input(cursor_moved_event(x, y)));
                }
                InputEvent::PointerButton { event } => {
                    if let Some(button) = map_mouse_button(event.button()) {
                        let st = match event.state() {
                            ButtonState::Pressed => ElementState::Pressed,
                            ButtonState::Released => ElementState::Released,
                        };
                        state.push(BackendEvent::Input(mouse_input_event(st, button)));
                    }
                }
                InputEvent::PointerAxis { event } => {
                    let dx = event.amount(Axis::Horizontal).unwrap_or(0.0);
                    let dy = event.amount(Axis::Vertical).unwrap_or(0.0);
                    if dx != 0.0 || dy != 0.0 {
                        state.push(BackendEvent::Input(mouse_wheel_event(dx, dy)));
                    }
                }
                // Keyboard: run the libinput keycode through xkb to get a
                // logical key + text, then enqueue a winit-shaped
                // `KeyboardInput` (plus a `ModifiersChanged` when the
                // modifier mask flips). This is the same wire the winit
                // backend feeds, so the chrome's `dispatch_event` handles
                // DRM keystrokes identically to hosted-mode ones.
                InputEvent::Keyboard { event } => {
                    let pressed = matches!(event.state(), KeyState::Pressed);
                    state.handle_keyboard_key(event.key_code(), pressed);
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
                    position: (0, 0),
                    size: (w, h),
                    scale: 1.0,
                },
            );
        }

        // Seed pointer state from the scanout's mode so absolute-motion
        // transforms and cursor clamping use the right output size; start
        // the cursor centered.
        let (ow, oh) = scanout.as_ref().map(|s| s.size()).unwrap_or((0, 0));
        // Grab a hardware-cursor mover so the libinput motion handlers can
        // reposition the plane at input rate. `None` if the cursor plane
        // isn't active (software-cursor fallback in the scanout).
        let hw_cursor = scanout.as_ref().and_then(|s| s.cursor_mover());

        // Compile the xkb keymap once at startup from the session's
        // `XKB_DEFAULT_*` environment (installer-seeded, PAM-exported).
        // A failure here is non-fatal: the session keeps running with
        // pointer-only input rather than refusing to boot.
        let keyboard = KeyboardXkb::new();
        match &keyboard {
            Some(_) => tracing::info!("DRM backend: xkb keyboard initialised"),
            None => tracing::warn!(
                "DRM backend: xkb keymap unavailable; keyboard input disabled"
            ),
        }

        let dispatch = DrmDispatchState {
            session: Some(session_state),
            outputs,
            events: Vec::new(),
            output_meta,
            pointer: (ow as f64 / 2.0, oh as f64 / 2.0),
            output_size: (ow as i32, oh as i32),
            hw_cursor,
            keyboard,
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
            pointer: (0.0, 0.0),
            output_size: (0, 0),
            hw_cursor: None,
            keyboard: None,
        }
    }
}

/// Build a `WindowEvent::CursorMoved` for the compositor's input wire.
/// `device_id` is zeroed — the host path never inspects it (mirrors
/// `native::execute_planned_action`).
fn cursor_moved_event(x: f64, y: f64) -> WindowEvent {
    WindowEvent::CursorMoved {
        device_id: unsafe { std::mem::zeroed() },
        position: PhysicalPosition::new(x, y),
    }
}

/// Build a `WindowEvent::MouseInput`.
fn mouse_input_event(state: ElementState, button: MouseButton) -> WindowEvent {
    WindowEvent::MouseInput {
        device_id: unsafe { std::mem::zeroed() },
        state,
        button,
    }
}

/// Build a `WindowEvent::MouseWheel` from libinput axis amounts. libinput's
/// `amount` is ~15 per wheel notch with `+y` = scroll *down*; winit's
/// `LineDelta` uses `+y` = scroll *up*, so we negate and scale to lines.
fn mouse_wheel_event(dx: f64, dy: f64) -> WindowEvent {
    WindowEvent::MouseWheel {
        device_id: unsafe { std::mem::zeroed() },
        delta: MouseScrollDelta::LineDelta(-(dx / 15.0) as f32, -(dy / 15.0) as f32),
        phase: TouchPhase::Moved,
    }
}

/// Map a raw Linux input button code (`input-event-codes.h`) to winit's
/// `MouseButton`. Side/extra buttons are dropped.
fn map_mouse_button(code: u32) -> Option<MouseButton> {
    match code {
        0x110 => Some(MouseButton::Left),
        0x111 => Some(MouseButton::Right),
        0x112 => Some(MouseButton::Middle),
        _ => None,
    }
}

/// xkb keymap + state owned by the DRM backend. Turns libinput keycodes
/// into the winit `KeyboardInput` events the chrome understands. The
/// winit backend gets these from the OS; on a bare TTY libinput only
/// reports raw keycodes, so we run the keymap ourselves — the missing
/// half of the input wire on DRM.
pub struct KeyboardXkb {
    // Held for the lifetime of `state`. xkbcommon refcounts internally,
    // but keeping ownership explicit makes the lifetime obvious. Never
    // read after construction (hence the underscore).
    _context: xkb::Context,
    _keymap: xkb::Keymap,
    state: xkb::State,
    /// Last modifier mask we emitted. We only push `ModifiersChanged`
    /// when it actually flips, matching winit's behaviour.
    mods: ModifiersState,
}

/// What translating one key event yields: an optional `KeyboardInput`
/// (absent for keys with no logical mapping, e.g. bare modifiers) and an
/// optional `ModifiersChanged` (present only when the mask flips).
struct KeyOutcome {
    input: Option<WindowEvent>,
    modifiers_changed: Option<Modifiers>,
}

impl KeyboardXkb {
    /// Compile a keymap from the `XKB_DEFAULT_*` environment. Passing
    /// empty rule names makes xkbcommon consult `XKB_DEFAULT_RULES`,
    /// `_MODEL`, `_LAYOUT`, `_VARIANT` and `_OPTIONS` — byte-for-byte
    /// what `smithay::input::keyboard::XkbConfig::default()` does. The
    /// installer writes those into `/etc/environment` and PAM exports
    /// them into the greetd session, so the user's chosen layout is what
    /// gets compiled here.
    ///
    /// Returns `None` (the caller logs) if the keymap can't be built so
    /// the backend degrades to pointer-only rather than aborting.
    pub fn new() -> Option<Self> {
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let keymap = xkb::Keymap::new_from_names(
            &context,
            "", // rules  -> XKB_DEFAULT_RULES
            "", // model  -> XKB_DEFAULT_MODEL
            "", // layout -> XKB_DEFAULT_LAYOUT
            "", // variant-> XKB_DEFAULT_VARIANT
            None, // options -> XKB_DEFAULT_OPTIONS
            xkb::KEYMAP_COMPILE_NO_FLAGS,
        )?;
        let state = xkb::State::new(&keymap);
        Some(Self {
            _context: context,
            _keymap: keymap,
            state,
            mods: ModifiersState::empty(),
        })
    }

    /// Read the current effective Ctrl/Shift/Alt/Super state as a winit
    /// `ModifiersState`.
    fn current_mods(&self) -> ModifiersState {
        let eff = xkb::STATE_MODS_EFFECTIVE;
        let mut m = ModifiersState::empty();
        if self.state.mod_name_is_active(xkb::MOD_NAME_CTRL, eff) {
            m |= ModifiersState::CONTROL;
        }
        if self.state.mod_name_is_active(xkb::MOD_NAME_SHIFT, eff) {
            m |= ModifiersState::SHIFT;
        }
        if self.state.mod_name_is_active(xkb::MOD_NAME_ALT, eff) {
            m |= ModifiersState::ALT;
        }
        if self.state.mod_name_is_active(xkb::MOD_NAME_LOGO, eff) {
            m |= ModifiersState::SUPER;
        }
        m
    }

    /// Translate one key event. `keycode` already carries the +8 offset
    /// (see [`KeyboardKeyEvent::key_code`]).
    fn process(&mut self, keycode: xkb::Keycode, pressed: bool) -> KeyOutcome {
        // Read the produced keysym with the *current* modifier state
        // (prior Shift/Caps already applied), then advance the state for
        // this key. For a held Shift the letter is already uppercased
        // because the Shift-down event updated the state first; for a
        // modifier key the produced sym is unaffected by the ordering.
        let sym = self.state.key_get_one_sym(keycode);
        let direction = if pressed {
            xkb::KeyDirection::Down
        } else {
            xkb::KeyDirection::Up
        };
        let _ = self.state.update_key(keycode, direction);

        let new_mods = self.current_mods();
        let modifiers_changed = if new_mods != self.mods {
            self.mods = new_mods;
            Some(Modifiers::from(new_mods))
        } else {
            None
        };

        // Derive the logical key from the *keysym* (not the UTF-8 output)
        // so chords like Ctrl+K resolve to `Character("k")` rather than
        // the control character the key would otherwise produce.
        let input = keysym_to_winit_key(sym.raw()).map(|logical| {
            let text = text_for(sym.key_char(), new_mods);
            keyboard_input_event(logical, text, pressed)
        });

        KeyOutcome {
            input,
            modifiers_changed,
        }
    }
}

/// Map an xkb keysym (raw value) to a winit logical [`Key`]. Special
/// keys the chrome matches on become `Key::Named`; anything with a
/// printable Unicode value becomes `Key::Character`. Bare modifiers and
/// unmapped keys return `None` (their effect is carried by the separate
/// `ModifiersChanged` event).
fn keysym_to_winit_key(raw: u32) -> Option<Key> {
    let named = match raw {
        keysyms::KEY_Return | keysyms::KEY_KP_Enter => Some(NamedKey::Enter),
        keysyms::KEY_Escape => Some(NamedKey::Escape),
        keysyms::KEY_Tab | keysyms::KEY_ISO_Left_Tab => Some(NamedKey::Tab),
        keysyms::KEY_BackSpace => Some(NamedKey::Backspace),
        keysyms::KEY_Delete | keysyms::KEY_KP_Delete => Some(NamedKey::Delete),
        keysyms::KEY_Left | keysyms::KEY_KP_Left => Some(NamedKey::ArrowLeft),
        keysyms::KEY_Right | keysyms::KEY_KP_Right => Some(NamedKey::ArrowRight),
        keysyms::KEY_Up | keysyms::KEY_KP_Up => Some(NamedKey::ArrowUp),
        keysyms::KEY_Down | keysyms::KEY_KP_Down => Some(NamedKey::ArrowDown),
        keysyms::KEY_Home | keysyms::KEY_KP_Home => Some(NamedKey::Home),
        keysyms::KEY_End | keysyms::KEY_KP_End => Some(NamedKey::End),
        keysyms::KEY_Page_Up | keysyms::KEY_KP_Page_Up => Some(NamedKey::PageUp),
        keysyms::KEY_Page_Down | keysyms::KEY_KP_Page_Down => Some(NamedKey::PageDown),
        keysyms::KEY_space => Some(NamedKey::Space),
        _ => None,
    };
    if let Some(n) = named {
        return Some(Key::Named(n));
    }
    // Printable character (handles layout + shift + caps via the keysym).
    match Keysym::new(raw).key_char() {
        Some(c) if !c.is_control() => Some(Key::Character(SmolStr::new(c.to_string()))),
        _ => None,
    }
}

/// The typed text for a key event, or `None`. Suppressed under Ctrl/Super
/// (those are chords, not text) and for control characters — matching
/// winit, and keeping control bytes out of text inputs.
fn text_for(ch: Option<char>, mods: ModifiersState) -> Option<SmolStr> {
    if mods.control_key() || mods.super_key() {
        return None;
    }
    match ch {
        Some(c) if !c.is_control() => Some(SmolStr::new(c.to_string())),
        _ => None,
    }
}

/// Build a `WindowEvent::KeyboardInput`. Mirrors the zero-init workaround
/// in `compositor::native::keyboard_input_event` and
/// `overlay_host::key_press`: winit's `KeyEvent` has a `pub(crate)`
/// `platform_specific` field, so we zero the struct and overwrite the
/// fields the host reads.
fn keyboard_input_event(logical_key: Key, text: Option<SmolStr>, pressed: bool) -> WindowEvent {
    // SAFETY: every public field of `KeyEvent` is overwritten below, and
    // the only non-public field (`platform_specific: KeyEventExtra`)
    // tolerates the all-zero bit pattern — the same invariant the two
    // existing `KeyEvent`-building helpers in this crate rely on.
    let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
    ev.logical_key = logical_key;
    ev.physical_key = PhysicalKey::Unidentified(NativeKeyCode::Unidentified);
    ev.location = KeyLocation::Standard;
    ev.state = if pressed {
        ElementState::Pressed
    } else {
        ElementState::Released
    };
    ev.repeat = false;
    ev.text = text;
    WindowEvent::KeyboardInput {
        device_id: unsafe { std::mem::zeroed() },
        event: ev,
        is_synthetic: false,
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

    // ===================================================================
    // Keyboard: xkb keycode → winit key translation (Track B2)
    // ===================================================================

    /// Pure mapping: named special keys and printable characters resolve
    /// to the winit `Key` the chrome matches on. Mirrors the level of the
    /// `preferred_mode` / pointer-helper tests — no real keymap needed.
    #[test]
    fn keysym_mapping_named_and_character() {
        use smithay::input::keyboard::keysyms as ks;
        assert!(matches!(
            keysym_to_winit_key(ks::KEY_Return),
            Some(Key::Named(NamedKey::Enter))
        ));
        assert!(matches!(
            keysym_to_winit_key(ks::KEY_Escape),
            Some(Key::Named(NamedKey::Escape))
        ));
        assert!(matches!(
            keysym_to_winit_key(ks::KEY_Tab),
            Some(Key::Named(NamedKey::Tab))
        ));
        assert!(matches!(
            keysym_to_winit_key(ks::KEY_BackSpace),
            Some(Key::Named(NamedKey::Backspace))
        ));
        assert!(matches!(
            keysym_to_winit_key(ks::KEY_Left),
            Some(Key::Named(NamedKey::ArrowLeft))
        ));
        assert!(matches!(
            keysym_to_winit_key(ks::KEY_space),
            Some(Key::Named(NamedKey::Space))
        ));
        // Letter keysyms carry case from the keysym itself (US 'a' vs 'A').
        match keysym_to_winit_key(ks::KEY_a) {
            Some(Key::Character(s)) => assert_eq!(s.as_str(), "a"),
            other => panic!("KEY_a mapped to {other:?}"),
        }
        match keysym_to_winit_key(ks::KEY_A) {
            Some(Key::Character(s)) => assert_eq!(s.as_str(), "A"),
            other => panic!("KEY_A mapped to {other:?}"),
        }
        // Bare modifier keysyms have no logical key — their effect rides
        // on the separate `ModifiersChanged` event instead.
        assert!(keysym_to_winit_key(ks::KEY_Control_L).is_none());
        assert!(keysym_to_winit_key(ks::KEY_Shift_L).is_none());
    }

    /// Text is produced for plain printable keys, suppressed under Ctrl
    /// (chords) and for control characters.
    #[test]
    fn text_for_respects_modifiers_and_control_chars() {
        let none = ModifiersState::empty();
        let ctrl = ModifiersState::CONTROL;
        assert_eq!(text_for(Some('a'), none).as_deref(), Some("a"));
        assert_eq!(text_for(Some('A'), none).as_deref(), Some("A"));
        // Ctrl held → no typed text (Ctrl+K must not insert a glyph).
        assert_eq!(text_for(Some('a'), ctrl), None);
        // ESC (0x1b) is a control char and never becomes text.
        assert_eq!(text_for(Some('\u{1b}'), none), None);
        assert_eq!(text_for(None, none), None);
    }

    /// End-to-end through a real keymap: pressing/releasing Left Shift
    /// flips the modifier mask. Layout-independent (Shift is Shift on
    /// every layout). Soft-skips if no xkb data is installed.
    #[test]
    fn xkb_left_shift_toggles_modifier_mask() {
        let Some(mut kbd) = KeyboardXkb::new() else {
            eprintln!("xkb keymap unavailable (no xkeyboard-config?); skipping");
            return;
        };
        // evdev KEY_LEFTSHIFT (42) + 8 = xkb keycode 50.
        let shift = xkb::Keycode::new(50);
        let down = kbd.process(shift, true);
        let m = down
            .modifiers_changed
            .expect("shift press should flip the modifier mask");
        assert!(m.state().shift_key(), "left shift down -> shift active");
        // The bare modifier produces no logical key event.
        assert!(down.input.is_none(), "bare modifier yields no KeyboardInput");

        let up = kbd.process(shift, false);
        let m = up
            .modifiers_changed
            .expect("shift release should flip the modifier mask");
        assert!(!m.state().shift_key(), "left shift up -> shift cleared");
    }

    /// End-to-end: an alpha-row keycode yields a `Character` KeyboardInput
    /// carrying text. The QWERTY 'a' position is a letter on every Latin
    /// layout, so asserting `Character` is layout-independent. Soft-skips
    /// without xkb data.
    #[test]
    fn xkb_alpha_key_produces_character_input() {
        let Some(mut kbd) = KeyboardXkb::new() else {
            eprintln!("xkb keymap unavailable; skipping");
            return;
        };
        // evdev KEY_A (30) + 8 = 38.
        let key_a = xkb::Keycode::new(38);
        let out = kbd.process(key_a, true);
        let input = out
            .input
            .expect("alpha key down should yield a KeyboardInput");
        match input {
            WindowEvent::KeyboardInput { event, .. } => {
                assert!(
                    matches!(event.logical_key, Key::Character(_)),
                    "alpha key should map to a Character logical key, got {:?}",
                    event.logical_key
                );
                assert_eq!(event.state, ElementState::Pressed);
                assert!(event.text.is_some(), "typed alpha key carries text");
            }
            other => panic!("expected KeyboardInput, got {other:?}"),
        }
    }

    #[test]
    fn synthesize_for_tests_populates_output_meta() {
        let outs = vec![
            (OutputId(0), Output::new(OutputId(0), (1920, 1080), 1.0)),
            (OutputId(1), Output::new(OutputId(1), (1280, 720), 1.0)),
        ];
        let state = DrmRuntime::synthesize_for_tests(outs);
        assert_eq!(state.output_meta.len(), 2);
        assert!(state.output_meta.contains_key(&OutputId(0)));
        assert!(state.output_meta.contains_key(&OutputId(1)));
    }
}
