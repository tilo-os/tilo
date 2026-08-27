//! Hardware-touching glue for the DRM backend.
//!
//! Only the calls that need a real seat and hardware live here — opening
//! the seat, probing connectors, modesetting, ioctls on a libinput device.
//! Every decision those calls carry out is made in a pure sibling module
//! instead ([`super::hotplug`], [`super::session`], [`super::scale`],
//! [`super::input_config`], [`super::gestures`]), which is what keeps them
//! testable: none of this module runs under `cargo test` on a desktop
//! compositor session. Construction (`DrmRuntime::new`) fails cleanly with
//! `Err` rather than panicking when there is no seat.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};

use smithay::backend::drm::{DrmDevice, DrmDeviceFd};
use smithay::backend::input::{
    AbsolutePositionEvent, Axis, ButtonState, Event as InputEventTrait, GestureBeginEvent,
    GestureEndEvent, GesturePinchUpdateEvent, GestureSwipeUpdateEvent, InputEvent, KeyState,
    KeyboardKeyEvent, PointerAxisEvent, PointerButtonEvent, PointerMotionEvent, TouchEvent,
    TouchSlot,
};
use smithay::backend::libinput::{LibinputInputBackend, LibinputSessionInterface};
// xkb comes from smithay's re-export, so it needs no new dependency; we
// mirror `XkbConfig::default()`'s `XKB_DEFAULT_*` handling by compiling
// with empty rule names (see `KeyboardXkb::new`).
use smithay::backend::session::Session;
use smithay::backend::session::libseat::LibSeatSession;
use smithay::backend::udev::{UdevBackend, all_gpus};
use smithay::input::keyboard::{Keysym, keysyms, xkb};
use smithay::reexports::calloop::{EventLoop, LoopHandle};
use smithay::reexports::drm as drm_re;
use smithay::reexports::input::Libinput;
use winit::dpi::PhysicalPosition;
use winit::event::{
    ElementState, KeyEvent, Modifiers, MouseButton, MouseScrollDelta, TouchPhase, WindowEvent,
};
use winit::keyboard::{
    Key, KeyLocation, ModifiersState, NamedKey, NativeKeyCode, PhysicalKey, SmolStr,
};

use super::devices::{ModeInfo, filter_kms_nodes};
use super::gestures::{GestureAction, GestureConfig, GestureTracker};
use super::input_config;
use super::hotplug::{self, ConnectedOutput, LiveOutput};
use super::input_config::{DeviceCaps, InputSettings};
use super::outputs::{ConnectorInfo, DrmOutputKey, OutputRegistry};
use super::scale::resolve_scale;
use super::scanout::{CardHandle, CursorPlanes, DrmScanout, ScanoutBuild, open_all_scanouts};
use super::session::{SeatTick, SessionAction, SessionState};
use crate::compositor::backend::{BackendEvent, GestureEvent, Output, OutputId, TouchEvent as QosTouchEvent};

/// Errors a DRM backend can encounter during construction. Each
/// variant maps to a clean operator-visible message; the binary then
/// `eprintln!`s + exits 1 instead of panicking.
#[derive(Debug, thiserror::Error)]
pub enum DrmBackendError {
    /// Seat acquisition failed. The payload is the *diagnosis*, not the
    /// raw errno: libseat collapses "you are not in a session", "your
    /// session owns no seat" and "no seat backend is running" into one
    /// `ENOSYS`, which tells the operator nothing. See
    /// [`super::seat_diag`].
    #[error("failed to open libseat session\n{0}")]
    Seat(super::seat_diag::SeatFailure),

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
    /// The primary card, held open on the loop thread for the whole
    /// session. libseat refuses a second `open` of a node that is already
    /// open, so this one handle is the only way a display plugged in after
    /// startup can ever be enumerated and modeset. `None` when the session
    /// is running headless (no card came up).
    pub card: Option<CardHandle>,
    /// Every output currently being driven, in `OutputId` order. Mirrors
    /// what the presenter owns: the scanouts themselves move to the render
    /// thread, so this is the loop thread's record of what is live.
    pub live: Vec<LiveEntry>,
    /// Channel to whoever owns the scanouts — the presenter once it has
    /// been split off, the backend itself before that.
    pub commands: Option<Sender<ScanoutCommand>>,
    /// The persisted pointer / keyboard preferences, applied to every
    /// libinput device as it appears.
    pub input: InputSettings,
    /// Every libinput device currently attached.
    ///
    /// libinput has no "list your devices" call — a caller only learns
    /// about a device from its `DEVICE_ADDED` event — so keeping the
    /// handles is the only way to re-apply a changed setting to hardware
    /// that is already plugged in. `Device` is refcounted, so these are
    /// cheap clones of the same objects libinput holds.
    pub devices: Vec<smithay::reexports::input::Device>,
    /// `display.scale` from the settings store, when the user has set one.
    /// Overrides the per-output derivation.
    pub scale_override: Option<f64>,
    /// Accumulates touchpad swipes and pinches into shell navigation.
    pub gestures: GestureTracker,
    /// Set whenever the output set changed, so the backend refreshes its
    /// cached list and the compositor re-syncs its `wl_output` globals.
    pub outputs_changed: bool,
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
    /// Hardware-cursor multiplexer across all outputs. Carries one plane
    /// per output whose KMS cursor plane is active; the libinput motion
    /// handlers call `move_to(global_x, global_y)` on every pointer event,
    /// and [`CursorPlanes`] routes the move to the plane of the output the
    /// pointer is over (hiding the plane it just left), all at input rate
    /// without a framebuffer redraw. Empty → outputs draw a software
    /// cursor from `pointer` instead. Planes are added and dropped by
    /// output id as displays are plugged and unplugged.
    pub hw_cursor: CursorPlanes,
    /// xkb keymap + state for translating libinput keycodes into winit
    /// `KeyboardInput` events (the same wire the winit backend feeds to
    /// `FrameBuilder::dispatch_event`). `None` when the keymap couldn't
    /// be compiled — the session then runs pointer-only instead of
    /// aborting. Built from the `XKB_DEFAULT_*` environment so the
    /// installer-seeded layout reaches the session (see [`KeyboardXkb`]).
    pub keyboard: Option<KeyboardXkb>,
}

/// One output the backend is currently driving, from the loop thread's
/// point of view. The scanout itself lives on the render thread; this is
/// the identity, geometry and crtc the loop needs to reconcile a hotplug.
pub struct LiveEntry {
    pub key: DrmOutputKey,
    pub id: OutputId,
    pub size: (u32, u32),
    pub physical_mm: Option<(u32, u32)>,
    pub crtc: smithay::reexports::drm::control::crtc::Handle,
    pub position: (i32, i32),
    pub scale: f64,
    /// This output's presentation interval, captured when it was brought
    /// up. Kept here rather than read from the scanout because the
    /// scanouts move to the render thread — and a `refresh_interval` that
    /// read them would start returning `None` the moment they did.
    pub refresh: Option<std::time::Duration>,
}

/// What the loop thread asks the owner of the scanouts to do.
///
/// The scanouts move to the render thread at startup
/// ([`super::DrmBackend::take_presenter`]), which is the right place for
/// them — presentation is the only thing that touches them per frame. But
/// hotplug, VT switches and resume all originate on the loop thread, and
/// all of them have to reach across. A channel drained at the top of each
/// frame is that reach: it keeps the scanouts single-owner, and it orders
/// every reconfiguration against the frames around it, so a flip can never
/// be issued into a scanout that is being torn down.
pub enum ScanoutCommand {
    /// Drop these outputs, adopt these new ones, and take this as the
    /// complete output table (positions included).
    Reconfigure {
        remove: Vec<OutputId>,
        add: Vec<ScanoutEntry>,
        outputs: Vec<Output>,
    },
    /// The session lost the hardware: stop presenting entirely.
    Pause,
    /// The session has it back: re-modeset every output, restore the
    /// panels' power state, and resume presenting.
    Resume { dpms_on: bool },
}

impl DrmDispatchState {
    pub fn push(&mut self, ev: BackendEvent) {
        self.events.push(ev);
    }

    /// Send a command to whoever owns the scanouts. Silently dropped when
    /// there is no channel (a headless DRM session that never brought an
    /// output up) or the far end is gone (the render thread died, which
    /// the loop reports separately and survives).
    fn command(&self, cmd: ScanoutCommand) {
        if let Some(tx) = self.commands.as_ref() {
            let _ = tx.send(cmd);
        }
    }

    /// Re-ask the hardware what is connected and reconcile.
    ///
    /// Every udev edge lands here, including the ones that turn out to
    /// change nothing — a `Changed` event fires on every DPMS transition
    /// and every EDID re-read, and re-modesetting on those would flash the
    /// panel for no reason. [`hotplug::plan`] is what tells the two apart.
    pub fn rescan_outputs(&mut self, reason: &str) {
        let Some(card) = self.card.as_ref() else {
            return;
        };
        let device_id = card.device_id();
        let snapshots = card.connected();
        let connected: Vec<ConnectedOutput> = snapshots
            .iter()
            .map(|s| {
                let (w, h) = s.mode.size();
                ConnectedOutput {
                    key: DrmOutputKey {
                        device: device_id,
                        connector: s.connector_id,
                    },
                    size: (w as u32, h as u32),
                    mm: s.physical_mm,
                }
            })
            .collect();
        let live_view: Vec<LiveOutput> = self
            .live
            .iter()
            .map(|l| LiveOutput {
                key: l.key,
                id: l.id,
                size: l.size,
            })
            .collect();
        let plan = hotplug::plan(&live_view, &connected);
        if plan.is_noop() {
            tracing::debug!(reason, "DRM hotplug: nothing changed");
            return;
        }
        tracing::info!(
            reason,
            added = plan.added.len(),
            removed = plan.removed.len(),
            remoded = plan.remoded.len(),
            "DRM hotplug: reconciling outputs"
        );

        // A re-moded connector is torn down and brought back up: the mode
        // is baked into the scanout's framebuffers, so there is nothing to
        // adjust in place. It keeps its `OutputId` because the registry
        // interns by `(device, connector)`, so clients see a mode change
        // rather than a display vanishing and a new one appearing.
        let mut remove: Vec<OutputId> = plan.removed.clone();
        remove.extend(plan.remoded.iter().map(|(id, _)| *id));
        self.live.retain(|l| !remove.contains(&l.id));
        let surviving: Vec<OutputId> = self.live.iter().map(|l| l.id).collect();
        self.hw_cursor.retain_outputs(&surviving);

        // Bring up everything new, avoiding the crtcs the survivors hold.
        let mut used: Vec<_> = self.live.iter().map(|l| l.crtc).collect();
        let to_add: Vec<&ConnectedOutput> = plan
            .added
            .iter()
            .chain(plan.remoded.iter().map(|(_, c)| c))
            .collect();
        let mut builds: Vec<ScanoutBuild> = Vec::new();
        for want in to_add {
            let Some(snapshot) = snapshots
                .iter()
                .find(|s| s.connector_id == want.key.connector)
            else {
                continue;
            };
            if let Some(build) = card.bring_up(snapshot, &used) {
                used.push(build.crtc);
                builds.push(build);
            }
        }

        let added_entries = self.adopt_builds(builds);
        self.relayout_outputs();
        let outputs = self.output_table();
        self.command(ScanoutCommand::Reconfigure {
            remove,
            add: added_entries,
            outputs,
        });
        self.outputs_changed = true;
        // A changed output set changes the size of the scene and the
        // extent the pointer lives in. Both have to reach the compositor
        // before it paints again.
        self.push(BackendEvent::OutputsChanged);
        self.push(BackendEvent::Redraw);
    }

    /// Fold freshly-modeset scanouts into the live set, minting (or
    /// re-using) their `OutputId`s and taking their cursor movers.
    ///
    /// Returns the entries to hand to the scanout owner. The `position`
    /// on each is provisional; [`Self::relayout_outputs`] fixes it for the
    /// whole set at once, and the `Reconfigure` command carries the final
    /// table.
    fn adopt_builds(&mut self, builds: Vec<ScanoutBuild>) -> Vec<ScanoutEntry> {
        let mut entries = Vec::with_capacity(builds.len());
        for build in builds {
            let key = DrmOutputKey {
                device: build.device_id,
                connector: build.connector_id,
            };
            let id = self.outputs.intern(key);
            if let Some(mover) = build.scanout.cursor_mover() {
                let (w, h) = build.scanout.size();
                self.hw_cursor
                    .insert(CursorPlanes::output_cursor(id, (0, 0, w, h), mover));
            }
            self.live.push(LiveEntry {
                key,
                id,
                size: build.scanout.size(),
                physical_mm: build.physical_mm,
                crtc: build.crtc,
                position: (0, 0),
                scale: 1.0,
                refresh: build.scanout.refresh_interval(),
            });
            entries.push(ScanoutEntry {
                id,
                position: (0, 0),
                scanout: build.scanout,
            });
        }
        entries
    }

    /// Re-pack the live outputs side by side, re-derive their scales, and
    /// bring the pointer and the cursor planes back into the new space.
    ///
    /// Called after any change to the live set. Re-packing (rather than
    /// leaving survivors where they were) is what closes the hole an
    /// unplugged middle monitor would otherwise leave in the combined
    /// coordinate space.
    fn relayout_outputs(&mut self) {
        self.live.sort_by_key(|l| l.id.0);
        let sizes: Vec<(u32, u32)> = self.live.iter().map(|l| l.size).collect();
        let positions = hotplug::layout(&sizes);
        for (entry, position) in self.live.iter_mut().zip(&positions) {
            entry.position = *position;
            entry.scale = resolve_scale(entry.size, entry.physical_mm, self.scale_override);
        }
        let extent = hotplug::combined_extent(&positions, &sizes);
        self.output_size = extent;
        self.pointer = hotplug::clamp_pointer(self.pointer, extent);

        self.output_meta = self
            .live
            .iter()
            .map(|l| {
                (
                    l.id,
                    Output {
                        id: l.id,
                        position: l.position,
                        size: l.size,
                        scale: l.scale as f32,
                    },
                )
            })
            .collect();

        self.rebuild_cursor_routing();
    }

    /// Point every cursor plane at its output's rectangle in the *new*
    /// layout. A plane left with a stale rect routes the pointer onto the
    /// wrong monitor for every position past where the layout shifted.
    fn rebuild_cursor_routing(&mut self) {
        for entry in &self.live {
            self.hw_cursor.set_rect(
                entry.id,
                (
                    entry.position.0,
                    entry.position.1,
                    entry.size.0,
                    entry.size.1,
                ),
            );
        }
        self.hw_cursor.move_to(self.pointer.0, self.pointer.1);
    }

    /// The public output table, in `OutputId` order.
    fn output_table(&self) -> Vec<Output> {
        let mut outs: Vec<Output> = self.output_meta.values().cloned().collect();
        outs.sort_by_key(|o| o.id.0);
        outs
    }

    /// Perform one step of the pause / resume sequence the session state
    /// machine asked for.
    fn perform(&mut self, action: SessionAction) {
        match action {
            SessionAction::PauseScanout => self.command(ScanoutCommand::Pause),
            SessionAction::DropMaster => {
                if let Some(card) = self.card.as_ref() {
                    card.drop_master();
                }
            }
            SessionAction::AcquireMaster => {
                if let Some(card) = self.card.as_ref() {
                    card.acquire_master();
                }
            }
            // The remaining three all happen inside the scanouts, which
            // live on the render thread, so they travel together as one
            // ordered `Resume`: re-modeset, restore the power state,
            // resume presenting. Splitting them into three messages would
            // let a frame slip between the modeset and the unpause.
            SessionAction::ResetOutputs => {
                let dpms_on = self
                    .session
                    .as_ref()
                    .map(|s| s.dpms_on())
                    .unwrap_or(true);
                self.command(ScanoutCommand::Resume { dpms_on });
            }
            SessionAction::RestoreDpms | SessionAction::ResumeScanout => {}
            SessionAction::FullDamage => {
                // The scanout buffers hold whatever the other session left
                // in them. Nothing short of a full repaint is correct.
                self.push(BackendEvent::Redraw);
            }
        }
    }

    /// Run a whole sequence in order.
    pub fn perform_all(&mut self, actions: impl IntoIterator<Item = SessionAction>) {
        for a in actions {
            self.perform(a);
        }
    }

    /// Apply the current input settings to one libinput device.
    ///
    /// Every failure is logged at debug and ignored: libinput returns an
    /// error for a setting a device does not support, and the capability
    /// gates in [`input_config::plan_for`] already avoid asking, so
    /// anything left is a driver quirk that must not take the session down.
    fn configure_device(&self, device: &mut smithay::reexports::input::Device) {
        let caps = device_caps(device);
        let plan = input_config::plan_for(&self.input, &caps);
        if let Some(on) = plan.tap {
            let _ = device.config_tap_set_enabled(on);
        }
        if let Some(on) = plan.tap_drag {
            let _ = device.config_tap_set_drag_enabled(on);
        }
        if let Some(on) = plan.natural_scroll {
            let _ = device.config_scroll_set_natural_scroll_enabled(on);
        }
        if let Some(on) = plan.disable_while_typing {
            let _ = device.config_dwt_set_enabled(on);
        }
        if let Some(on) = plan.middle_emulation {
            let _ = device.config_middle_emulation_set_enabled(on);
        }
        if let Some(speed) = plan.accel_speed() {
            let _ = device.config_accel_set_speed(speed);
        }
        tracing::info!(
            device = device.name(),
            touchpad = caps.is_touchpad(),
            tap = ?plan.tap,
            natural_scroll = ?plan.natural_scroll,
            accel = ?plan.accel_speed(),
            "DRM input: configured device"
        );
    }

    /// Route one gesture: to the shell when the finger count says so,
    /// otherwise on to the focused client.
    #[allow(clippy::needless_pass_by_value)]
    fn handle_gesture(&mut self, ev: GestureEvent) {
        // Decide ownership *before* touching the tracker. Only the `begin`
        // carries a finger count, so every later event has to ask what the
        // gesture in flight belongs to — and an `end` clears that, which
        // would leave the closing event of a shell-owned gesture looking
        // unowned and send a client a `swipe_end` for a swipe it never saw
        // begin.
        let shell_owns = match ev {
            GestureEvent::SwipeBegin { fingers }
            | GestureEvent::PinchBegin { fingers }
            | GestureEvent::HoldBegin { fingers } => super::gestures::claims(fingers),
            _ => self.gestures.shell_owns_current(),
        };
        let action = match ev {
            GestureEvent::SwipeBegin { fingers } => {
                self.gestures.swipe_begin(fingers);
                None
            }
            GestureEvent::SwipeUpdate { dx, dy } => self.gestures.swipe_update(dx, dy),
            GestureEvent::SwipeEnd { .. } => {
                self.gestures.swipe_end();
                None
            }
            GestureEvent::PinchBegin { fingers } => {
                self.gestures.pinch_begin(fingers);
                None
            }
            GestureEvent::PinchUpdate { scale, .. } => self.gestures.pinch_update(scale),
            GestureEvent::PinchEnd { .. } => {
                self.gestures.pinch_end();
                None
            }
            // A hold has no shell meaning, but it still has to register as
            // a gesture in flight so the count that arrives with its begin
            // governs where its end goes.
            GestureEvent::HoldBegin { fingers } => {
                self.gestures.swipe_begin(fingers);
                None
            }
            GestureEvent::HoldEnd { .. } => {
                self.gestures.swipe_end();
                None
            }
        };
        if let Some(action) = action {
            self.push(BackendEvent::ShellGesture(action));
            // A navigation gesture changes what is on screen.
            self.push(BackendEvent::Redraw);
        }
        // Client delivery is gated on the shell not owning the gesture: a
        // three-finger swipe that switched the workspace *and* scrolled the
        // app underneath is wrong in both directions.
        if !shell_owns {
            self.push(BackendEvent::Gesture(ev));
        }
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
    session: LibSeatSession,
    /// Detected primary GPU card path. `None` if enumeration finds no
    /// `card*` nodes (the construction would already have returned
    /// `NoDevices` in that case, but keeping the field is convenient
    /// for follow-ups).
    pub primary_gpu: Option<PathBuf>,
    /// Live scanouts — one per connected output. Empty if
    /// modeset failed for every connector (e.g. not DRM master / nothing
    /// connected) — the compositor still runs headless in that case
    /// rather than crashing. Ordered by `OutputId`; `scanouts[0]` is the
    /// primary (the one the frame loop targets with `OutputId(0)`, and the
    /// fd the multi-output drain reads).
    pub scanouts: Vec<ScanoutEntry>,
    /// Receiving end of the reconfiguration channel. Held here while the
    /// backend still owns the scanouts inline; moved into the presenter by
    /// [`super::Backend::take_presenter`] along with them, so whoever owns
    /// the scanouts is also the one draining the commands for them.
    pub commands: Option<Receiver<ScanoutCommand>>,
}

/// One live output: its public [`OutputId`], its top-left position in the
/// compositor's combined coordinate space (outputs are laid out
/// side-by-side), and the [`DrmScanout`] driving its crtc.
pub struct ScanoutEntry {
    pub id: OutputId,
    pub position: (i32, i32),
    pub scanout: DrmScanout,
}

/// Lay output sizes out left-to-right at `y = 0`.
///
/// A thin alias for [`hotplug::layout`] so startup and hotplug pack
/// outputs through one implementation — two of them would drift, and the
/// drift shows up as a monitor in the wrong place after a re-plug.
#[cfg(test)]
use hotplug::layout as side_by_side_layout;

impl DrmRuntime {
    /// Acquire the session, enumerate DRM devices, attach libinput,
    /// and wire everything onto an internal calloop. Returns an error
    /// (not a panic) on any failure — the binary entry point matches
    /// the existing skeleton's contract: `eprintln!` + `exit(1)`.
    pub fn new() -> Result<Self, DrmBackendError> {
        // `LibSeatSession::new()` blocks until the seat hands us an
        // `Enable` event, so by the time it returns we either own the
        // seat or we've already failed.
        let (session, session_notifier) = LibSeatSession::new().map_err(|e| {
            DrmBackendError::Seat(super::seat_diag::diagnose_live(format!("{e:?}")))
        })?;
        let seat_name = session.seat();
        tracing::info!(seat = %seat_name, "DRM backend: libseat session acquired");

        let session_state = SessionState::new_active();

        // Ask udev for the current device set before inserting the source,
        // so we can refuse early if nothing is plugged in.
        let udev =
            UdevBackend::new(&seat_name).map_err(|e| DrmBackendError::Udev(format!("{e}")))?;
        let raw_gpus: Vec<PathBuf> =
            all_gpus(&seat_name).map_err(|e| DrmBackendError::Udev(format!("{e}")))?;
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

        // Bring up scanout(s) on the primary GPU first, keeping the card fd
        // open as DRM master. `open_all_scanouts` opens the card once and
        // modesets every connected connector, each on its own crtc, sharing
        // a dup of the one fd. libseat refuses a second `open` of the same
        // node, so the primary must not also be opened by the
        // connector-enumeration pass below — we exclude it when scanout
        // owns it. A total modeset failure is non-fatal: log and run
        // headless (the wayland socket is still served).
        let (card, scanout_builds) = match primary_gpu.as_ref() {
            Some(path) => {
                let mut scanout_session = session.clone();
                match open_all_scanouts(&mut scanout_session, path) {
                    Ok((card, b)) => {
                        tracing::info!(count = b.len(), "DRM backend: scanout live on primary GPU");
                        (Some(card), b)
                    }
                    Err(e) => {
                        tracing::warn!("DRM backend: scanout setup failed ({e}); running headless");
                        (None, Vec::new())
                    }
                }
            }
            None => (None, Vec::new()),
        };
        let have_scanouts = !scanout_builds.is_empty();

        // Connector enumeration on the GPUs scanout does not own (the
        // primary is held open above). Driving connectors on secondary
        // GPUs needs a second card fd plus cross-GPU buffer handling, so
        // this pass is diagnostic-only for now (it logs what's attached).
        // Failures here are logged and skipped rather than aborting the
        // boot. `LibSeatSession` is cheap to clone (internal `Arc`), so we
        // hand a borrow to the enumeration helper.
        let enum_gpus: Vec<PathBuf> = if have_scanouts {
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
            "DRM backend: enumerated DRM connectors on secondary GPUs (diagnostic; not yet driven)"
        );

        // The session-interface adapter routes libinput's `open`/`close`
        // calls through libseat so it inherits the privileged FDs without
        // root.
        let mut libinput_ctx =
            Libinput::new_with_udev(LibinputSessionInterface::from(session.clone()));
        libinput_ctx
            .udev_assign_seat(&seat_name)
            .map_err(|e| DrmBackendError::Libinput(format!("{e:?}")))?;
        let libinput_backend = LibinputInputBackend::new(libinput_ctx);

        // The session notifier, udev backend, and libinput backend each
        // register as event sources; page-flip / DRM-device sources attach
        // once individual `cardN` devices are opened.
        let event_loop: EventLoop<'static, DrmDispatchState> =
            EventLoop::try_new().map_err(|e| DrmBackendError::Calloop(format!("{e}")))?;
        let handle: LoopHandle<'static, DrmDispatchState> = event_loop.handle();

        // Session events: a VT switch. Pausing has to stop scanout and
        // give up DRM master before the incoming session can modeset;
        // activating has to take master back and rebuild every output,
        // because nothing about a CRTC survives the switch. The state
        // machine in `super::session` owns the ordering; this handler
        // performs what it returns.
        handle
            .insert_source(session_notifier, move |event, _, state: &mut DrmDispatchState| {
                use smithay::backend::session::Event as SeatEvent;
                let tick = match event {
                    SeatEvent::ActivateSession => {
                        tracing::info!("DRM backend: session activated");
                        SeatTick::Activate
                    }
                    SeatEvent::PauseSession => {
                        tracing::info!("DRM backend: session paused");
                        SeatTick::Pause
                    }
                };
                let actions = match state.session.as_ref() {
                    Some(s) => s.apply_with_actions(tick),
                    None => Vec::new(),
                };
                state.perform_all(actions);
            })
            .map_err(|e| DrmBackendError::Calloop(format!("{e}")))?;

        // Every one of the three udev events is the same question — what
        // is connected now? — so all three re-enumerate and reconcile; the
        // hotplug plan is what decides whether that implies any work.
        // `Changed` is the one a monitor plugged into a live card raises,
        // so treating it as a no-op means plugging in a display does
        // nothing.
        handle
            .insert_source(udev, move |event, _, state: &mut DrmDispatchState| {
                use smithay::backend::udev::UdevEvent;
                match event {
                    UdevEvent::Added { device_id, path } => {
                        tracing::info!(?device_id, ?path, "DRM: udev device added");
                        state.rescan_outputs("udev device added");
                    }
                    UdevEvent::Changed { device_id } => {
                        tracing::debug!(?device_id, "DRM: udev device changed");
                        state.rescan_outputs("udev device changed");
                    }
                    UdevEvent::Removed { device_id } => {
                        tracing::info!(?device_id, "DRM: udev device removed");
                        state.rescan_outputs("udev device removed");
                    }
                }
            })
            .map_err(|e| DrmBackendError::Calloop(format!("{e}")))?;

        // libinput events. Pointer, keyboard, gestures and touch all come
        // through here; the pointer and keyboard halves are translated
        // into the winit-shaped `WindowEvent`s the chrome already handles,
        // so a keystroke on a bare TTY takes the same path as one in a
        // nested dev window.
        handle
            .insert_source(libinput_backend, move |event, _, state: &mut DrmDispatchState| match event {
                // A device appearing is the only moment libinput will
                // accept its configuration, and it happens for every
                // device at startup as well as on a real hot-plug — so
                // this one arm is what makes the persisted input settings
                // reach the hardware at all.
                InputEvent::DeviceAdded { mut device } => {
                    state.configure_device(&mut device);
                    state.devices.push(device);
                }
                InputEvent::DeviceRemoved { device } => {
                    tracing::info!(device = device.name(), "DRM input: device removed");
                    state.devices.retain(|d| *d != device);
                }
                // Absolute motion (usb-tablet / touch): map normalized
                // coords through the output size to a pixel position.
                InputEvent::PointerMotionAbsolute { event } => {
                    let (w, h) = state.output_size;
                    let x = event.x_transformed(w.max(1));
                    let y = event.y_transformed(h.max(1));
                    state.pointer = (x, y);
                    // Reposition the hardware cursor plane immediately —
                    // no framebuffer redraw needed for a cursor move.
                    // `CursorPlanes` routes the global position to the
                    // plane of the output under the pointer.
                    state.hw_cursor.move_to(x, y);
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
                    state.hw_cursor.move_to(x, y);
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

                // ---- touchpad gestures ----
                InputEvent::GestureSwipeBegin { event } => {
                    state.handle_gesture(GestureEvent::SwipeBegin {
                        fingers: event.fingers(),
                    });
                }
                InputEvent::GestureSwipeUpdate { event } => {
                    state.handle_gesture(GestureEvent::SwipeUpdate {
                        dx: event.delta_x(),
                        dy: event.delta_y(),
                    });
                }
                InputEvent::GestureSwipeEnd { event } => {
                    state.handle_gesture(GestureEvent::SwipeEnd {
                        cancelled: event.cancelled(),
                    });
                }
                InputEvent::GesturePinchBegin { event } => {
                    state.handle_gesture(GestureEvent::PinchBegin {
                        fingers: event.fingers(),
                    });
                }
                InputEvent::GesturePinchUpdate { event } => {
                    state.handle_gesture(GestureEvent::PinchUpdate {
                        dx: event.delta_x(),
                        dy: event.delta_y(),
                        scale: event.scale(),
                        rotation: event.rotation(),
                    });
                }
                InputEvent::GesturePinchEnd { event } => {
                    state.handle_gesture(GestureEvent::PinchEnd {
                        cancelled: event.cancelled(),
                    });
                }
                InputEvent::GestureHoldBegin { event } => {
                    state.handle_gesture(GestureEvent::HoldBegin {
                        fingers: event.fingers(),
                    });
                }
                InputEvent::GestureHoldEnd { event } => {
                    state.handle_gesture(GestureEvent::HoldEnd {
                        cancelled: event.cancelled(),
                    });
                }

                // ---- touchscreen ----
                //
                // Each touch point is delivered twice: once as a
                // `wl_touch` event for a client that wants real
                // multi-touch, and once as pointer motion + a left click
                // so the shell chrome — which is driven by the winit event
                // stream and has no touch path — responds to a finger the
                // same way it responds to a click. Without the second
                // half, a touchscreen can operate apps but not the
                // desktop around them.
                InputEvent::TouchDown { event } => {
                    let (x, y) = touch_position(state, &event);
                    state.pointer = (x, y);
                    state.hw_cursor.move_to(x, y);
                    state.push(BackendEvent::Touch(QosTouchEvent::Down {
                        slot: touch_slot(event.slot()),
                        x,
                        y,
                    }));
                    state.push(BackendEvent::Input(cursor_moved_event(x, y)));
                    state.push(BackendEvent::Input(mouse_input_event(
                        ElementState::Pressed,
                        MouseButton::Left,
                    )));
                }
                InputEvent::TouchMotion { event } => {
                    let (x, y) = touch_position(state, &event);
                    state.pointer = (x, y);
                    state.hw_cursor.move_to(x, y);
                    state.push(BackendEvent::Touch(QosTouchEvent::Motion {
                        slot: touch_slot(event.slot()),
                        x,
                        y,
                    }));
                    state.push(BackendEvent::Input(cursor_moved_event(x, y)));
                }
                InputEvent::TouchUp { event } => {
                    state.push(BackendEvent::Touch(QosTouchEvent::Up {
                        slot: touch_slot(event.slot()),
                    }));
                    state.push(BackendEvent::Input(mouse_input_event(
                        ElementState::Released,
                        MouseButton::Left,
                    )));
                }
                InputEvent::TouchCancel { .. } => {
                    state.push(BackendEvent::Touch(QosTouchEvent::Cancel));
                    // A cancelled touch is not a click. Release the
                    // synthetic button so the chrome does not sit on a
                    // phantom held pointer.
                    state.push(BackendEvent::Input(mouse_input_event(
                        ElementState::Released,
                        MouseButton::Left,
                    )));
                }
                InputEvent::TouchFrame { .. } => {
                    state.push(BackendEvent::Touch(QosTouchEvent::Frame));
                }

                // Switches (lid, tablet mode) and tablet-tool events are
                // handled elsewhere or not at all; unlike the gestures
                // above they were never advertised to clients, so dropping
                // them promises nothing that isn't delivered.
                _ => {}
            })
            .map_err(|e| DrmBackendError::Calloop(format!("{e}")))?;

        // Compile the xkb keymap once at startup from the session's
        // `XKB_DEFAULT_*` environment (installer-seeded, PAM-exported).
        // A failure here is non-fatal: the session keeps running with
        // pointer-only input rather than refusing to boot.
        let keyboard = KeyboardXkb::new();
        match &keyboard {
            Some(_) => tracing::info!("DRM backend: xkb keyboard initialised"),
            None => tracing::warn!("DRM backend: xkb keymap unavailable; keyboard input disabled"),
        }

        // The persisted preferences. Read once here so the same values
        // drive libinput device configuration, gesture direction and the
        // per-output scale override.
        let input = InputSettings::load();
        let scale_override = load_display_scale_override();
        tracing::info!(
            tap_to_click = input.tap_to_click,
            natural_scroll = input.natural_scroll,
            scale_override = ?scale_override,
            "DRM backend: loaded input and display settings"
        );

        // The scanout side and the loop side talk over one channel, so a
        // reconfiguration is ordered against the frames around it.
        let (tx, rx) = channel::<ScanoutCommand>();

        let mut dispatch = DrmDispatchState {
            session: Some(session_state),
            outputs: OutputRegistry::new(),
            events: Vec::new(),
            card,
            live: Vec::new(),
            commands: Some(tx),
            input,
            devices: Vec::new(),
            scale_override,
            gestures: GestureTracker::new(GestureConfig {
                natural_scroll: input.natural_scroll,
                ..GestureConfig::default()
            }),
            outputs_changed: false,
            output_meta: HashMap::new(),
            pointer: (0.0, 0.0),
            output_size: (0, 0),
            hw_cursor: CursorPlanes::default(),
            keyboard,
        };

        // Adopt the connectors that were already live at startup. This is
        // the same path a hot-plug takes — one implementation, so a
        // display present at boot and one plugged in a minute later end up
        // configured identically. Each is interned by `(device,
        // connector)` so its `OutputId` is stable across re-enumeration,
        // and the first gets `OutputId(0)`, preserving the "primary ==
        // OutputId(0)" contract the frame loop relies on.
        let mut scanouts = dispatch.adopt_builds(scanout_builds);
        dispatch.relayout_outputs();
        // `adopt_builds` leaves positions provisional; the relayout above
        // computed the real ones, so copy them onto the entries the
        // presenter will own.
        for entry in scanouts.iter_mut() {
            if let Some(live) = dispatch.live.iter().find(|l| l.id == entry.id) {
                entry.position = live.position;
            }
        }
        scanouts.sort_by_key(|e| e.id.0);
        // Start the pointer in the middle of the combined space.
        let (ow, oh) = dispatch.output_size;
        dispatch.pointer = (ow as f64 / 2.0, oh as f64 / 2.0);
        dispatch.hw_cursor.move_to(dispatch.pointer.0, dispatch.pointer.1);

        Ok(Self {
            event_loop,
            dispatch,
            session,
            primary_gpu,
            scanouts,
            commands: Some(rx),
        })
    }

    /// Switch the session to another virtual terminal.
    ///
    /// Not optional on a bare TTY: the compositor puts the keyboard into a
    /// raw mode, so the kernel's own `Ctrl+Alt+Fn` handling never sees the
    /// chord. Without the compositor performing the switch itself the user
    /// is locked onto this VT for the life of the session — and the whole
    /// pause/resume path below can never be reached.
    pub fn change_vt(&mut self, vt: i32) {
        use smithay::backend::session::Session as _;
        match self.session.change_vt(vt) {
            Ok(()) => tracing::info!(vt, "DRM backend: switching virtual terminal"),
            Err(e) => tracing::warn!("DRM backend: change_vt({vt}) failed: {e:?}"),
        }
    }

    /// Run the come-back-from-suspend sequence.
    ///
    /// A system suspend does not pause the seat, so no `ActivateSession`
    /// arrives on the way out of it and the session was never marked
    /// inactive — the state machine has nothing to react to. The DRM
    /// device came back needing a full re-establish all the same, which is
    /// why this is a separate, unconditional entry point rather than a
    /// case of the VT path.
    pub fn resume_from_sleep(&mut self) {
        let actions = match self.dispatch.session.as_ref() {
            Some(s) => s.resume_actions(),
            None => Vec::new(),
        };
        tracing::info!("DRM backend: restoring the display after resume");
        self.dispatch.perform_all(actions);
    }

    /// Re-read the persisted input settings and push them at every
    /// libinput device again.
    ///
    /// Devices that appear later pick the new settings up from their
    /// `DeviceAdded`; the ones already attached are re-configured here
    /// from the handles the dispatch state kept, so a pointer-speed
    /// change is felt on the touchpad the user is holding rather than on
    /// the next one they plug in.
    pub fn reload_input_settings(&mut self) {
        let input = InputSettings::load();
        self.dispatch.input = input;
        self.dispatch.gestures.set_config(GestureConfig {
            natural_scroll: input.natural_scroll,
            ..GestureConfig::default()
        });
        let mut devices = std::mem::take(&mut self.dispatch.devices);
        for device in devices.iter_mut() {
            self.dispatch.configure_device(device);
        }
        self.dispatch.devices = devices;
        let scale_override = load_display_scale_override();
        if scale_override != self.dispatch.scale_override {
            self.dispatch.scale_override = scale_override;
            self.dispatch.relayout_outputs();
            self.dispatch.outputs_changed = true;
            self.dispatch.push(BackendEvent::OutputsChanged);
        }
    }

    /// Drain pending events from the internal calloop. Non-blocking:
    /// uses a zero timeout so the outer compositor loop owns the
    /// cadence.
    pub fn dispatch_pending(&mut self) {
        use std::time::Duration;
        let _ = self.event_loop.dispatch(Duration::ZERO, &mut self.dispatch);
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
            outputs: registry,
            output_meta: outputs.into_iter().collect(),
            ..Default::default()
        }
    }
}

/// Ask one libinput device what it can be configured to do.
///
/// Every question is a capability query libinput answers per device;
/// asking before setting is what keeps the configuration path free of
/// error returns for settings a device was never going to accept.
fn device_caps(device: &smithay::reexports::input::Device) -> DeviceCaps {
    DeviceCaps {
        supports_tap: device.config_tap_finger_count() > 0,
        supports_natural_scroll: device.config_scroll_has_natural_scroll(),
        supports_accel: device.config_accel_is_available(),
        supports_dwt: device.config_dwt_is_available(),
        supports_middle_emulation: device.config_middle_emulation_is_available(),
        is_pointer: device.has_capability(smithay::reexports::input::DeviceCapability::Pointer),
    }
}

/// The `display.scale` the user has set, or `None` when they have not.
///
/// Distinguishing "unset" from "set to 1.0" is the whole point: unset
/// means derive the scale from the panel, and 1.0 means the user has
/// looked at a HiDPI panel and decided they want it at 100 %.
fn load_display_scale_override() -> Option<f64> {
    settings_store::load_section("display")
        .get("scale")
        .and_then(|v| v.as_f64())
        .filter(|s| (0.5..=3.0).contains(s))
}

/// Touch slot as a plain integer for the backend event. libinput reports
/// `-1` for single-touch devices that have no slot concept; those are all
/// one finger, so slot 0 is the honest answer.
fn touch_slot(slot: TouchSlot) -> u32 {
    let id: i32 = slot.into();
    id.max(0) as u32
}

/// Map a normalised touch position onto the combined output space.
fn touch_position<E>(state: &DrmDispatchState, event: &E) -> (f64, f64)
where
    E: AbsolutePositionEvent<LibinputInputBackend>,
{
    let (w, h) = state.output_size;
    (
        event.x_transformed(w.max(1)),
        event.y_transformed(h.max(1)),
    )
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
            "",   // rules  -> XKB_DEFAULT_RULES
            "",   // model  -> XKB_DEFAULT_MODEL
            "",   // layout -> XKB_DEFAULT_LAYOUT
            "",   // variant-> XKB_DEFAULT_VARIANT
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
        // The function row. Without these the media and brightness keys
        // reach `key_char()`, which has no character for them, and the
        // press is dropped before any handler sees it — which is why
        // pressing volume-up on a real machine did nothing at all.
        keysyms::KEY_XF86AudioRaiseVolume => Some(NamedKey::AudioVolumeUp),
        keysyms::KEY_XF86AudioLowerVolume => Some(NamedKey::AudioVolumeDown),
        keysyms::KEY_XF86AudioMute => Some(NamedKey::AudioVolumeMute),
        keysyms::KEY_XF86AudioMicMute => Some(NamedKey::MicrophoneVolumeMute),
        keysyms::KEY_XF86MonBrightnessUp => Some(NamedKey::BrightnessUp),
        keysyms::KEY_XF86MonBrightnessDown => Some(NamedKey::BrightnessDown),
        keysyms::KEY_XF86AudioPlay => Some(NamedKey::MediaPlayPause),
        keysyms::KEY_XF86AudioPause => Some(NamedKey::MediaPause),
        keysyms::KEY_XF86AudioNext => Some(NamedKey::MediaTrackNext),
        keysyms::KEY_XF86AudioPrev => Some(NamedKey::MediaTrackPrevious),
        keysyms::KEY_XF86PowerOff => Some(NamedKey::Power),
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
/// page-flip wiring.
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
    use smithay::backend::session::AsErrno;
    use smithay::reexports::rustix::fs::OFlags;
    use smithay::utils::DeviceFd;

    // O_RDWR|O_CLOEXEC|O_NONBLOCK matches what smithay's anvil example
    // uses for the DRM device fd.
    let fd = session
        .open(
            path,
            OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK,
        )
        .map_err(|e| format!("session.open({path:?}): errno={:?}", e.as_errno()))?;
    let device_fd = DrmDeviceFd::new(DeviceFd::from(fd));
    let dev_id_raw: u64 = device_fd.dev_id().map_err(|e| format!("dev_id: {e}"))? as u64;

    let (drm_device, _notifier) = DrmDevice::new(device_fd, /* disable_connectors */ false)
        .map_err(|e| format!("DrmDevice::new: {e}"))?;

    let resources = drm_device
        .resource_handles()
        .map_err(|e| format!("resource_handles: {e}"))?;

    let mut connectors = Vec::with_capacity(resources.connectors().len());
    for &handle in resources.connectors() {
        match drm_device.get_connector(handle, /* force_probe */ true) {
            Ok(info) => {
                let connected =
                    matches!(info.state(), drm_re::control::connector::State::Connected);
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
        // `BackendEvent::Input` carries the actual `WindowEvent`; synthesise
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
            &[
                PathBuf::from("/dev/dri/card0"),
                PathBuf::from("/dev/dri/card1"),
            ],
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
        assert!(
            picked.preferred,
            "preferred flag must win over higher resolution"
        );
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
    // Keyboard: xkb keycode → winit key translation
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

    /// The function row has to survive the keysym translation. These
    /// keysyms have no character, so anything this table does not name
    /// falls through `key_char()` and is dropped before a handler runs.
    #[test]
    fn media_and_brightness_keysyms_reach_a_named_key() {
        use smithay::input::keyboard::keysyms as ks;
        for (raw, want) in [
            (ks::KEY_XF86AudioRaiseVolume, NamedKey::AudioVolumeUp),
            (ks::KEY_XF86AudioLowerVolume, NamedKey::AudioVolumeDown),
            (ks::KEY_XF86AudioMute, NamedKey::AudioVolumeMute),
            (ks::KEY_XF86AudioMicMute, NamedKey::MicrophoneVolumeMute),
            (ks::KEY_XF86MonBrightnessUp, NamedKey::BrightnessUp),
            (ks::KEY_XF86MonBrightnessDown, NamedKey::BrightnessDown),
            (ks::KEY_XF86AudioPlay, NamedKey::MediaPlayPause),
            (ks::KEY_XF86AudioNext, NamedKey::MediaTrackNext),
            (ks::KEY_XF86AudioPrev, NamedKey::MediaTrackPrevious),
        ] {
            assert_eq!(
                keysym_to_winit_key(raw),
                Some(Key::Named(want)),
                "keysym {raw:#x} must map to {want:?}"
            );
        }
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
        assert!(
            down.input.is_none(),
            "bare modifier yields no KeyboardInput"
        );

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
    fn side_by_side_layout_single_output_at_origin() {
        // The single-output case must place the one output at (0, 0).
        let positions = side_by_side_layout(&[(1920, 1080)]);
        assert_eq!(positions, vec![(0, 0)]);
    }

    #[test]
    fn side_by_side_layout_places_outputs_left_to_right() {
        // Each output's x is the sum of prior widths; y stays 0.
        let positions = side_by_side_layout(&[(1920, 1080), (1280, 1024), (2560, 1440)]);
        assert_eq!(positions, vec![(0, 0), (1920, 0), (3200, 0)]);
    }

    #[test]
    fn side_by_side_layout_empty_is_empty() {
        assert!(side_by_side_layout(&[]).is_empty());
    }

    // ===================================================================
    // Hotplug + session reconciliation, on the production methods
    //
    // `rescan_outputs` and `adopt_builds` need a real card (they modeset),
    // but everything they do *after* the ioctls — re-layout, scale
    // derivation, pointer clamping, the output table, and the commands
    // sent to whoever owns the scanouts — is reachable without one. These
    // drive `DrmDispatchState`'s own methods rather than re-deriving the
    // policy in the test.
    // ===================================================================

    fn crtc(n: u32) -> smithay::reexports::drm::control::crtc::Handle {
        smithay::reexports::drm::control::from_u32(n).expect("non-zero crtc handle")
    }

    fn entry(id: u32, connector: u32, size: (u32, u32), mm: Option<(u32, u32)>) -> LiveEntry {
        LiveEntry {
            key: DrmOutputKey {
                device: 1,
                connector,
            },
            id: OutputId(id),
            size,
            physical_mm: mm,
            crtc: crtc(id + 1),
            position: (0, 0),
            scale: 1.0,
            refresh: None,
        }
    }

    /// A dispatch state with a command channel attached, plus the
    /// receiving end so a test can assert on what was sent.
    fn dispatch_with_channel() -> (DrmDispatchState, Receiver<ScanoutCommand>) {
        let (tx, rx) = channel();
        let mut state = DrmDispatchState::default();
        state.commands = Some(tx);
        (state, rx)
    }

    #[test]
    fn relayout_packs_outputs_side_by_side_and_publishes_the_table() {
        let (mut state, _rx) = dispatch_with_channel();
        state.live = vec![
            entry(0, 10, (1920, 1080), None),
            entry(1, 11, (2560, 1440), None),
        ];
        state.relayout_outputs();

        assert_eq!(state.live[0].position, (0, 0));
        assert_eq!(state.live[1].position, (1920, 0));
        assert_eq!(state.output_size, (4480, 1440));
        let table = state.output_table();
        assert_eq!(table.len(), 2);
        assert_eq!(table[0].position, (0, 0));
        assert_eq!(table[1].position, (1920, 0));
        assert_eq!(table[1].size, (2560, 1440));
    }

    #[test]
    fn unplugging_the_right_monitor_brings_the_pointer_back_onto_the_left_one() {
        // The concrete "windows stranded off-screen" symptom that is
        // actually load-bearing: windows re-flow (they are not pinned to
        // an output), but the pointer lives in absolute combined
        // coordinates and would be left on a panel that is gone.
        let (mut state, _rx) = dispatch_with_channel();
        state.live = vec![
            entry(0, 10, (1920, 1080), None),
            entry(1, 11, (1920, 1080), None),
        ];
        state.relayout_outputs();
        state.pointer = (3000.0, 500.0);

        state.live.retain(|l| l.id == OutputId(0));
        state.relayout_outputs();

        assert_eq!(state.output_size, (1920, 1080));
        assert_eq!(
            state.pointer,
            (1919.0, 500.0),
            "the pointer must land on the surviving panel, not off it"
        );
    }

    #[test]
    fn unplugging_the_left_monitor_slides_the_survivor_to_the_origin() {
        // Leaving the survivor at x=1920 would open a 1920px hole in the
        // combined space that no panel displays and every click into it
        // would land on nothing.
        let (mut state, _rx) = dispatch_with_channel();
        state.live = vec![
            entry(0, 10, (1920, 1080), None),
            entry(1, 11, (2560, 1440), None),
        ];
        state.relayout_outputs();
        assert_eq!(state.live[1].position, (1920, 0));

        state.live.retain(|l| l.id == OutputId(1));
        state.relayout_outputs();
        assert_eq!(state.live[0].position, (0, 0));
        assert_eq!(state.output_size, (2560, 1440));
    }

    #[test]
    fn each_output_gets_the_scale_its_panel_calls_for() {
        // A laptop docked to a desktop monitor: one dense panel, one
        // sparse. A single session-wide scale is wrong for one of them.
        let (mut state, _rx) = dispatch_with_channel();
        state.live = vec![
            // 13.3" 1080p laptop panel.
            entry(0, 10, (1920, 1080), Some((294, 165))),
            // 24" 1080p desktop monitor.
            entry(1, 11, (1920, 1080), Some((527, 296))),
        ];
        state.relayout_outputs();
        assert_eq!(state.live[0].scale, 1.5);
        assert_eq!(state.live[1].scale, 1.0);
        let table = state.output_table();
        assert_eq!(table[0].scale, 1.5);
        assert_eq!(table[1].scale, 1.0);
    }

    #[test]
    fn a_display_scale_setting_overrides_every_output() {
        let (mut state, _rx) = dispatch_with_channel();
        state.scale_override = Some(1.25);
        state.live = vec![entry(0, 10, (1920, 1080), Some((294, 165)))];
        state.relayout_outputs();
        assert_eq!(
            state.live[0].scale, 1.25,
            "a user who set a scale is not asking for a second opinion"
        );
    }

    #[test]
    fn a_vt_switch_away_pauses_the_scanouts() {
        // The end-to-end of what the session state machine decided: the
        // scanout owner is told to stop before anything else happens.
        let (mut state, rx) = dispatch_with_channel();
        state.session = Some(SessionState::new_active());
        let actions = state
            .session
            .as_ref()
            .expect("session")
            .apply_with_actions(SeatTick::Pause);
        state.perform_all(actions);

        let sent: Vec<ScanoutCommand> = rx.try_iter().collect();
        assert!(
            matches!(sent.as_slice(), [ScanoutCommand::Pause]),
            "a pause must reach the scanout owner"
        );
    }

    #[test]
    fn a_vt_switch_back_tells_the_scanouts_to_re_modeset_and_forces_a_repaint() {
        let (mut state, rx) = dispatch_with_channel();
        state.session = Some(SessionState::new_active());
        let pause = state
            .session
            .as_ref()
            .expect("session")
            .apply_with_actions(SeatTick::Pause);
        state.perform_all(pause);
        let _ = rx.try_iter().count();
        state.events.clear();

        let activate = state
            .session
            .as_ref()
            .expect("session")
            .apply_with_actions(SeatTick::Activate);
        state.perform_all(activate);

        let sent: Vec<ScanoutCommand> = rx.try_iter().collect();
        assert!(
            matches!(sent.as_slice(), [ScanoutCommand::Resume { dpms_on: true }]),
            "activation must ask for a re-modeset, not just flip a flag"
        );
        assert!(
            state
                .events
                .iter()
                .any(|e| matches!(e, BackendEvent::Redraw)),
            "the scanout buffers hold the other session's pixels; the \
             compositor has to be told to repaint"
        );
    }

    #[test]
    fn a_session_blanked_for_idle_resumes_blanked() {
        // Coming back from a VT switch must restore the power state the
        // session had, not light a screen the user had turned off.
        let (mut state, rx) = dispatch_with_channel();
        let session = SessionState::new_active();
        session.set_dpms(false);
        state.session = Some(session.clone());
        state.perform_all(session.apply_with_actions(SeatTick::Pause));
        let _ = rx.try_iter().count();
        state.perform_all(session.apply_with_actions(SeatTick::Activate));

        let sent: Vec<ScanoutCommand> = rx.try_iter().collect();
        assert!(
            matches!(sent.as_slice(), [ScanoutCommand::Resume { dpms_on: false }]),
            "the restored power state must be the one the session had"
        );
    }

    #[test]
    fn resume_from_sleep_asks_for_the_same_rebuild_a_vt_switch_does() {
        // A suspend never pauses the seat, so nothing marks the session
        // inactive — and a handler that only acts on transitions does
        // nothing at all, which is the black-screen-after-lid-open bug.
        let (mut state, rx) = dispatch_with_channel();
        let session = SessionState::new_active();
        state.session = Some(session.clone());
        assert!(session.is_active(), "a suspend leaves the session active");

        state.perform_all(session.resume_actions());

        let sent: Vec<ScanoutCommand> = rx.try_iter().collect();
        assert!(
            matches!(sent.as_slice(), [ScanoutCommand::Resume { .. }]),
            "resume must re-modeset even though no state transition happened"
        );
        assert!(
            state
                .events
                .iter()
                .any(|e| matches!(e, BackendEvent::Redraw)),
            "and force a full repaint"
        );
    }

    #[test]
    fn a_headless_drm_session_survives_a_vt_switch_with_no_channel() {
        // No output came up, so there is nobody to send commands to.
        // Performing the sequence anyway must not panic.
        let mut state = DrmDispatchState::default();
        let session = SessionState::new_active();
        state.session = Some(session.clone());
        state.perform_all(session.apply_with_actions(SeatTick::Pause));
        state.perform_all(session.apply_with_actions(SeatTick::Activate));
    }

    // ===================================================================
    // Gesture routing, on the production `handle_gesture`
    // ===================================================================

    #[test]
    fn a_three_finger_swipe_becomes_a_shell_gesture_and_never_reaches_a_client() {
        let mut state = DrmDispatchState::default();
        state.handle_gesture(GestureEvent::SwipeBegin { fingers: 3 });
        state.handle_gesture(GestureEvent::SwipeUpdate { dx: 0.0, dy: -150.0 });
        state.handle_gesture(GestureEvent::SwipeEnd { cancelled: false });

        let shell: Vec<_> = state
            .events
            .iter()
            .filter_map(|e| match e {
                BackendEvent::ShellGesture(a) => Some(*a),
                _ => None,
            })
            .collect();
        assert_eq!(
            shell,
            vec![GestureAction::SwitchWorkspace(-1)]
        );
        assert!(
            !state
                .events
                .iter()
                .any(|e| matches!(e, BackendEvent::Gesture(_))),
            "a gesture the shell acted on must not also be delivered to a \
             client — the app would scroll under the workspace switch"
        );
    }

    #[test]
    fn the_closing_event_of_a_shell_gesture_does_not_leak_to_the_client() {
        // Ownership is read from the tracker, and the `end` clears it — so
        // deciding after the mutation sends a client a `swipe_end` for a
        // swipe it never saw begin, which is a protocol violation on a
        // wire the client is entitled to trust.
        let mut state = DrmDispatchState::default();
        for ev in [
            GestureEvent::SwipeBegin { fingers: 4 },
            GestureEvent::SwipeUpdate { dx: 0.0, dy: -150.0 },
            GestureEvent::SwipeEnd { cancelled: false },
        ] {
            state.handle_gesture(ev);
        }
        assert!(
            !state
                .events
                .iter()
                .any(|e| matches!(e, BackendEvent::Gesture(_))),
            "no part of a shell-owned gesture may reach a client: {:?}",
            state.events
        );
    }

    #[test]
    fn a_hold_gesture_ends_where_it_began() {
        // A hold carries its finger count only on `begin` too, and has no
        // shell meaning — so a three-finger hold must be swallowed whole
        // rather than half-delivered.
        let mut state = DrmDispatchState::default();
        state.handle_gesture(GestureEvent::HoldBegin { fingers: 3 });
        state.handle_gesture(GestureEvent::HoldEnd { cancelled: false });
        assert!(
            !state
                .events
                .iter()
                .any(|e| matches!(e, BackendEvent::Gesture(_)))
        );

        let mut state = DrmDispatchState::default();
        state.handle_gesture(GestureEvent::HoldBegin { fingers: 1 });
        state.handle_gesture(GestureEvent::HoldEnd { cancelled: false });
        assert_eq!(
            state
                .events
                .iter()
                .filter(|e| matches!(e, BackendEvent::Gesture(_)))
                .count(),
            2,
            "a one-finger hold is the client's, both halves of it"
        );
    }

    #[test]
    fn a_two_finger_pinch_is_forwarded_to_the_client_and_drives_nothing() {
        // Browser zoom. `zwp_pointer_gestures_v1` has been advertised all
        // along with nothing ever emitted into it.
        let mut state = DrmDispatchState::default();
        state.handle_gesture(GestureEvent::PinchBegin { fingers: 2 });
        state.handle_gesture(GestureEvent::PinchUpdate {
            dx: 0.0,
            dy: 0.0,
            scale: 2.0,
            rotation: 0.0,
        });
        state.handle_gesture(GestureEvent::PinchEnd { cancelled: false });

        let forwarded = state
            .events
            .iter()
            .filter(|e| matches!(e, BackendEvent::Gesture(_)))
            .count();
        assert_eq!(forwarded, 3, "every event of the gesture must be forwarded");
        assert!(
            !state
                .events
                .iter()
                .any(|e| matches!(e, BackendEvent::ShellGesture(_))),
            "two fingers is the client's gesture"
        );
    }

    #[test]
    fn natural_scroll_flips_the_gesture_direction_through_the_dispatch_state() {
        let mut state = DrmDispatchState {
            gestures: GestureTracker::new(GestureConfig {
                natural_scroll: true,
                ..GestureConfig::default()
            }),
            ..Default::default()
        };
        state.handle_gesture(GestureEvent::SwipeBegin { fingers: 3 });
        state.handle_gesture(GestureEvent::SwipeUpdate { dx: 0.0, dy: -150.0 });
        assert!(
            state.events.iter().any(|e| matches!(
                e,
                BackendEvent::ShellGesture(GestureAction::SwitchWorkspace(1))
            )),
            "the same preference that inverts scrolling inverts gestures"
        );
    }

    // ===================================================================
    // libinput device configuration, on the production plan
    // ===================================================================

    #[test]
    fn a_touchpad_plan_turns_tapping_on_by_default() {
        // The applier is one `config_*` call per `Some` in the plan, so
        // the plan is what decides what a laptop touchpad comes up as.
        let state = DrmDispatchState::default();
        let caps = DeviceCaps {
            supports_tap: true,
            supports_natural_scroll: true,
            supports_accel: true,
            supports_dwt: true,
            supports_middle_emulation: true,
            is_pointer: true,
        };
        let plan = input_config::plan_for(&state.input, &caps);
        assert_eq!(plan.tap, Some(true));
        assert_eq!(plan.disable_while_typing, Some(true));
        assert_eq!(plan.accel_speed(), Some(0.0));
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
