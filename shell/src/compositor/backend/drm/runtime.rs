//! Hardware-touching glue for the DRM backend.
//!
//! Only calls needing a real seat and hardware live here — opening the
//! seat, probing connectors, modesetting, libinput ioctls. Every decision
//! is made in a pure sibling module instead ([`super::hotplug`],
//! [`super::session`], [`super::scale`], [`super::input_config`],
//! [`super::gestures`]), which is what keeps them testable — none of this
//! runs under `cargo test`. `DrmRuntime::new` fails cleanly with `Err` rather than panicking when there is no seat.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, SyncSender, channel, sync_channel};

use smithay::backend::drm::{DrmDevice, DrmDeviceFd};
use smithay::backend::input::{
    AbsolutePositionEvent, Axis, AxisSource, ButtonState, GestureBeginEvent, GestureEndEvent,
    GesturePinchUpdateEvent, GestureSwipeUpdateEvent, InputEvent, KeyState, KeyboardKeyEvent,
    PointerAxisEvent, PointerButtonEvent, PointerMotionEvent, TouchEvent, TouchSlot,
};
use smithay::backend::libinput::{LibinputInputBackend, LibinputSessionInterface};
// xkb comes from smithay's re-export, no new dependency; mirrors `XkbConfig::default()`'s `XKB_DEFAULT_*` handling via empty rule names (see `KeyboardXkb::new`).
use smithay::backend::session::Session;
use smithay::backend::session::libseat::{LibSeatSession, LibSeatSessionNotifier};
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
    Key, KeyLocation, ModifiersState, NamedKey, NativeKey, PhysicalKey, SmolStr,
};
use winit::platform::scancode::PhysicalKeyExtScancode as _;

use super::devices::{ModeInfo, filter_kms_nodes, order_by_connected_connectors};
#[cfg(test)]
use super::gestures::GestureAction;
use super::gestures::{GestureConfig, GestureTracker};
use super::hotplug::{self, ConnectedOutput, LiveOutput};
use super::input_config;
use super::input_config::{
    AccessibilityInputSettings, DeviceCaps, InputSettings, KeyboardSettings,
};
use super::outputs::{ConnectorInfo, DrmOutputKey, OutputRegistry};
use super::scale::resolve_scale;
use super::scanout::{
    CardHandle, CursorPlanes, DrmScanout, ScanoutBuild, discover_icc_profiles, open_all_scanouts,
};
use super::session::{SeatTick, SessionAction, SessionState};
use crate::compositor::backend::{
    BackendEvent, GestureEvent, Output, OutputId, TouchEvent as QosTouchEvent,
};

/// One held repeatable key. Kept independent of xkb translation so timing is
/// deterministic and unit-testable without a seat or keyboard device.
#[derive(Debug)]
struct HeldRepeat {
    keycode: xkb::Keycode,
    next: std::time::Instant,
    interval: std::time::Duration,
}

#[derive(Debug, Default)]
struct KeyRepeat {
    held: Option<HeldRepeat>,
}

impl KeyRepeat {
    fn press(
        &mut self,
        keycode: xkb::Keycode,
        now: std::time::Instant,
        delay: std::time::Duration,
        rate: u32,
    ) {
        self.held = Some(HeldRepeat {
            keycode,
            next: now.checked_add(delay).unwrap_or(now),
            interval: std::time::Duration::from_secs_f64(1.0 / f64::from(rate.max(1))),
        });
    }

    fn release(&mut self, keycode: xkb::Keycode) {
        if self
            .held
            .as_ref()
            .is_some_and(|held| held.keycode == keycode)
        {
            self.held = None;
        }
    }

    fn cancel(&mut self) {
        self.held = None;
    }

    /// Return every repeat owed by `now`, capped so resuming from a debugger
    /// cannot pour seconds of stale Backspaces into the active widget.
    fn due(&mut self, now: std::time::Instant) -> Vec<xkb::Keycode> {
        let Some(held) = self.held.as_mut() else {
            return Vec::new();
        };
        let mut due = Vec::new();
        while now >= held.next && due.len() < 8 {
            due.push(held.keycode);
            held.next = held.next.checked_add(held.interval).unwrap_or(now);
        }
        if due.len() == 8 && now >= held.next {
            held.next = now.checked_add(held.interval).unwrap_or(now);
        }
        due
    }
}

/// Errors a DRM backend can encounter during construction — each variant maps to a clean operator-visible message; the binary `eprintln!`s + exits 1 instead of panicking.
#[derive(Debug, thiserror::Error)]
pub enum DrmBackendError {
    /// Seat acquisition failed — the payload is the *diagnosis*, not the
    /// raw errno: libseat collapses distinct failures into one `ENOSYS`. See [`super::seat_diag`].
    ///
    /// Boxed: [`super::seat_diag::SeatFailure`] carries a
    /// [`super::seat_diag::SeatEnvironment`] (several `Option<String>`
    /// fields) plus its own raw string, which otherwise makes every
    /// `Result<_, DrmBackendError>` several times larger than its `Ok` case
    /// needs, even on the hot paths that never see this variant.
    #[error("failed to open libseat session\n{0}")]
    Seat(Box<super::seat_diag::SeatFailure>),

    #[error("failed to enumerate DRM devices via udev: {0}")]
    Udev(String),

    #[error("no KMS-capable DRM devices found (need /dev/dri/cardN)")]
    NoDevices,

    #[error("none of the KMS devices have a usable live scanout: {0:?}")]
    NoScanout(Vec<PathBuf>),

    #[error("failed to initialise libinput: {0}")]
    Libinput(String),

    #[error("failed to insert event source into calloop: {0}")]
    Calloop(String),
}

/// State shared across calloop dispatch for the DRM backend — the `Data`
/// type the inner `EventLoop` runs over so each source can mutate it without an extra `Arc<Mutex<_>>`; `Default`-able for tests without a real seat.
#[derive(Default)]
pub struct DrmDispatchState {
    pub session: Option<SessionState>,
    pub outputs: OutputRegistry,
    pub events: Vec<BackendEvent>,
    /// The primary card, held open on the loop thread for the whole
    /// session — libseat refuses a second `open` of an already-open node, so this is the only way a later-plugged display can be enumerated; `None` when headless.
    pub card: Option<CardHandle>,
    /// Profiles already parsed by the ICC worker and proven capable of
    /// producing an RGBA8 output transform. Capability publication reads only
    /// this cache, so directory/profile I/O never stalls the compositor loop.
    pub color_profiles: BTreeMap<String, String>,
    /// Every output currently being driven, in `OutputId` order — mirrors what the presenter owns (scanouts live on the render thread).
    pub live: Vec<LiveEntry>,
    /// Channel to whoever owns the scanouts — the presenter once split off, the backend itself before that.
    pub commands: Option<Sender<ScanoutCommand>>,
    /// The persisted pointer/keyboard preferences, applied to every libinput device as it appears.
    pub input: InputSettings,
    pub accessibility: AccessibilityInputSettings,
    /// Slow-keys presses are accepted only after being held for the delay.
    pub slow_pending: HashMap<xkb::Keycode, std::time::Instant>,
    /// Sticky modifiers stay logically down through the next non-modifier.
    pub sticky_latched: ModifiersState,
    pub physical_modifiers: ModifiersState,
    /// Winit synthesises repeat presses for the hosted backend. Bare libinput
    /// only supplies key edges, so the DRM adapter mirrors that established
    /// event contract for qOS-native widgets. Wayland clients use the seat's
    /// `repeat_info` and never receive these synthetic edges.
    key_repeat: KeyRepeat,
    /// Every libinput device currently attached — libinput has no "list
    /// your devices" call, so keeping handles (cheap refcounted clones) is the only way to re-apply a changed setting to hardware already plugged in.
    pub devices: Vec<smithay::reexports::input::Device>,
    /// `display.scale` from the settings store, when the user has set one — overrides the per-output derivation.
    pub scale_override: Option<f64>,
    /// Accumulates touchpad swipes and pinches into shell navigation.
    pub gestures: GestureTracker,
    /// Set whenever the output set changed, so the backend refreshes its cached list and the compositor re-syncs its `wl_output` globals.
    pub outputs_changed: bool,
    /// Per-device output metadata, keyed by the public `OutputId`.
    pub output_meta: HashMap<OutputId, Output>,
    /// Current pointer position in output coords, updated from libinput motion; the scanout composites a software cursor here when no hardware cursor is available.
    pub pointer: (f64, f64),
    /// Primary output size (px) — needed to transform libinput's absolute (0..1) coordinates and to clamp relative motion.
    pub output_size: (i32, i32),
    /// Hardware-cursor multiplexer across all outputs — one plane per
    /// output whose KMS cursor plane is active. libinput motion handlers
    /// call `move_to(global_x, global_y)`, and [`CursorPlanes`] routes to
    /// the plane of the output the pointer is over (hiding the one it left),
    /// all at input rate with no framebuffer redraw. Empty → software cursor from `pointer`.
    pub hw_cursor: CursorPlanes,
    /// Per-user xkb keymap + state translating libinput keycodes into winit
    /// `KeyboardInput` events (`FrameBuilder::dispatch_event`'s wire); `None`
    /// when the keymap couldn't compile, running pointer-only. The persisted
    /// source list owns the session; installer-seeded XKB environment remains
    /// only the default before a preference exists.
    pub keyboard: Option<KeyboardXkb>,
}

/// One output the backend is currently driving, from the loop thread's view — the scanout itself lives on the render thread; this is the identity, geometry and crtc needed to reconcile a hotplug.
pub struct LiveEntry {
    pub key: DrmOutputKey,
    pub id: OutputId,
    pub connector_name: String,
    pub size: (u32, u32),
    pub physical_mm: Option<(u32, u32)>,
    pub crtc: smithay::reexports::drm::control::crtc::Handle,
    pub position: (i32, i32),
    pub scale: f64,
    pub transform: settings_store::hardware::OutputTransform,
    /// This output's presentation interval, captured when brought up — kept here rather than read from the scanout, since scanouts move to the render thread and would start returning `None`.
    pub refresh: Option<std::time::Duration>,
}

/// What the loop thread asks the owner of the scanouts to do.
///
/// The scanouts move to the render thread at startup
/// ([`super::DrmBackend::take_presenter`]) since presentation is the only
/// thing that touches them per frame; hotplug, VT switches and resume
/// originate on the loop thread and must reach across. A channel drained
/// at the top of each frame keeps the scanouts single-owner and orders every reconfiguration against surrounding frames.
pub enum ScanoutCommand {
    /// Drop these outputs, adopt these new ones, and take this as the complete output table (positions included).
    Reconfigure {
        remove: Vec<OutputId>,
        add: Vec<ScanoutEntry>,
        outputs: Vec<Output>,
    },
    /// The session lost the hardware: stop presenting entirely.
    Pause,
    /// The session has it back: re-modeset every output, restore the panels' power state, and resume presenting.
    Resume {
        dpms_on: bool,
    },
    SetCursorSize(u32),
}

impl DrmDispatchState {
    /// Build the initial dispatch state from everything [`DrmRuntime::new`]
    /// gathered before it — the seat's active session, the primary card (if
    /// scanout came up), and the persisted settings. Every other field
    /// starts at its zero value; [`Self::finalize_startup_scanouts`] is
    /// what actually populates `live`/`output_meta`/`pointer`. Extracted
    /// from that constructor to keep it a readable top-level sequence.
    fn new(
        session_state: SessionState,
        card: Option<CardHandle>,
        input: InputSettings,
        accessibility: AccessibilityInputSettings,
        scale_override: Option<f64>,
        commands_tx: Sender<ScanoutCommand>,
        keyboard: Option<KeyboardXkb>,
    ) -> Self {
        Self {
            session: Some(session_state),
            outputs: OutputRegistry::new(),
            events: Vec::new(),
            card,
            color_profiles: BTreeMap::new(),
            live: Vec::new(),
            commands: Some(commands_tx),
            input,
            accessibility,
            slow_pending: HashMap::new(),
            sticky_latched: ModifiersState::empty(),
            physical_modifiers: ModifiersState::empty(),
            key_repeat: KeyRepeat::default(),
            devices: Vec::new(),
            scale_override,
            gestures: GestureTracker::new(GestureConfig {
                natural_scroll: input.natural_scroll_for(true),
                ..GestureConfig::default()
            }),
            outputs_changed: false,
            output_meta: HashMap::new(),
            pointer: (0.0, 0.0),
            output_size: (0, 0),
            hw_cursor: CursorPlanes::default(),
            keyboard,
        }
    }

    /// Adopt the connectors already live at startup — the same path a
    /// hot-plug takes, so a display present at boot and one plugged in
    /// later end up configured identically. Each is interned by `(device,
    /// connector)` so its `OutputId` is stable, and the first gets
    /// `OutputId(0)`, preserving the "primary == `OutputId(0)`" contract.
    /// Also seeds the pointer at the center of the combined space and
    /// publishes the initial display snapshot. Extracted from
    /// [`DrmRuntime::new`] to keep that constructor a readable top-level
    /// sequence.
    fn finalize_startup_scanouts(
        &mut self,
        scanout_builds: Vec<ScanoutBuild>,
    ) -> Vec<ScanoutEntry> {
        let mut scanouts = self.adopt_builds(scanout_builds);
        self.relayout_outputs();
        // `adopt_builds` leaves positions provisional; the relayout above
        // computed the real ones, so copy them onto the entries the
        // presenter will own.
        for entry in &mut scanouts {
            if let Some(live) = self.live.iter().find(|l| l.id == entry.id) {
                entry.position = live.position;
                entry.transform = live.transform;
            }
            let profile = self
                .output_meta
                .get(&entry.id)
                .and_then(|output| output.color_profile.clone());
            entry.set_color_profile(profile);
        }
        scanouts.sort_by_key(|e| e.id.0);
        // Start the pointer in the middle of the combined space.
        let (ow, oh) = self.output_size;
        self.pointer = (f64::from(ow) / 2.0, f64::from(oh) / 2.0);
        self.hw_cursor.move_to(self.pointer.0, self.pointer.1);
        self.publish_display_snapshot();
        scanouts
    }

    pub fn push(&mut self, ev: BackendEvent) {
        self.events.push(ev);
    }

    /// Publish the live hardware contract for Settings. Failure is benign in
    /// test/headless sessions (usually no `XDG_RUNTIME_DIR`) and never changes
    /// the output plan itself.
    fn publish_display_snapshot(&self) {
        let Some(card) = self.card.as_ref() else {
            return;
        };
        let snapshot = settings_store::hardware::DisplayHardwareSnapshot {
            generated_unix_ms: settings_store::hardware::unix_ms(),
            backend: "drm-kms-xrgb8888".into(),
            publisher_pid: std::process::id(),
            publisher_start_ticks:
                settings_store::hardware::process_start_ticks(std::process::id())
                    .unwrap_or_default(),
            outputs: card.display_capabilities(&self.color_profiles),
        };
        if let Err(error) = snapshot.save() {
            tracing::debug!(%error, "DRM display: could not publish capability snapshot");
        }
    }

    /// Send a command to whoever owns the scanouts — silently dropped when there's no channel (headless) or the far end is gone (render thread died, reported separately).
    fn command(&self, cmd: ScanoutCommand) {
        if let Some(tx) = self.commands.as_ref() {
            let _ = tx.send(cmd);
        }
    }

    /// Re-ask the hardware what is connected and reconcile.
    ///
    /// Every udev edge lands here, including no-op ones — a `Changed` event
    /// fires on every DPMS transition and EDID re-read, and re-modesetting on those would flash the panel for nothing. [`hotplug::plan`] tells the two apart.
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
                    size: (u32::from(w), u32::from(h)),
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
        // is baked into the framebuffers, nothing to adjust in place. It keeps its `OutputId` (interned by `(device, connector)`), so clients see a mode change, not a vanished + new display.
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
        // A changed output set changes the scene size and the pointer's extent — both must reach the compositor before it paints again.
        self.push(BackendEvent::OutputsChanged);
        self.push(BackendEvent::Redraw);
        self.publish_display_snapshot();
    }

    /// Fold freshly-modeset scanouts into the live set, minting (or
    /// re-using) their `OutputId`s and taking their cursor movers.
    ///
    /// Returns entries for the scanout owner; `position` is provisional — [`Self::relayout_outputs`] fixes it for the whole set, and `Reconfigure` carries the final table.
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
                connector_name: build.connector_name,
                size: build.scanout.size(),
                physical_mm: build.physical_mm,
                crtc: build.crtc,
                position: (0, 0),
                scale: 1.0,
                transform: settings_store::hardware::OutputTransform::Normal,
                refresh: build.scanout.refresh_interval(),
            });
            entries.push(ScanoutEntry {
                id,
                position: (0, 0),
                transform: settings_store::hardware::OutputTransform::Normal,
                color_profile: None,
                color_transform: None,
                color_buffer: Vec::new(),
                scanout: build.scanout,
            });
        }
        entries
    }

    /// Re-pack the live outputs side by side, re-derive their scales, and
    /// bring the pointer and cursor planes back into the new space.
    ///
    /// Called after any change to the live set — re-packing (not leaving survivors in place) closes the hole an unplugged middle monitor would leave.
    fn relayout_outputs(&mut self) {
        let config = settings_store::hardware::DisplayConfig::load_effective();
        self.live.sort_by_key(|l| {
            (
                !config
                    .outputs
                    .get(&l.connector_name)
                    .is_some_and(|c| c.primary),
                l.id.0,
            )
        });
        for entry in &mut self.live {
            entry.transform = config
                .outputs
                .get(&entry.connector_name)
                .map_or(settings_store::hardware::OutputTransform::Normal, |c| {
                    c.transform
                });
        }
        let sizes: Vec<(u32, u32)> = self
            .live
            .iter()
            .map(|entry| logical_output_size(entry.size, entry.transform))
            .collect();
        let fallback = hotplug::layout(&sizes);
        let mut positions: Vec<(i32, i32)> = self
            .live
            .iter()
            .zip(&fallback)
            .map(|(entry, fallback)| {
                config
                    .outputs
                    .get(&entry.connector_name)
                    .map_or(*fallback, |c| (c.x, c.y))
            })
            .collect();
        for index in 0..self.live.len() {
            let Some(target) = self
                .live
                .get(index)
                .and_then(|entry| config.outputs.get(&entry.connector_name))
                .and_then(|c| c.mirror_of.as_deref())
            else {
                continue;
            };
            let Some(target_index) = self.live.iter().position(|l| l.connector_name == target)
            else {
                continue;
            };
            let Some(&target_position) = positions.get(target_index) else {
                continue;
            };
            if let Some(position) = positions.get_mut(index) {
                *position = target_position;
            }
        }
        // KMS accepts negative logical coordinates, but the renderer's
        // combined texture starts at zero. Preserve relative arrangement and
        // translate the whole monitor island into that texture.
        let min_x = positions.iter().map(|p| p.0).min().unwrap_or(0);
        let min_y = positions.iter().map(|p| p.1).min().unwrap_or(0);
        for p in &mut positions {
            // `min_x`/`min_y` are the minimum over `positions`, so this can
            // never go negative; `saturating_sub` just avoids a raw `-`
            // clippy can't see is bounded (config-supplied coordinates, in
            // principle unbounded).
            p.0 = p.0.saturating_sub(min_x);
            p.1 = p.1.saturating_sub(min_y);
        }
        for (entry, position) in self.live.iter_mut().zip(&positions) {
            entry.position = *position;
            entry.scale = config.outputs.get(&entry.connector_name).map_or_else(
                || resolve_scale(entry.size, entry.physical_mm, self.scale_override),
                |c| c.scale,
            );
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
                        size: logical_output_size(l.size, l.transform),
                        // `l.scale` is a UI scale factor bounded to
                        // `[MIN_SCALE, MAX_SCALE]` (see `scale.rs`) or a
                        // validated settings override; f32 precision is
                        // ample for it.
                        #[allow(
                            clippy::cast_possible_truncation,
                            reason = "UI scale factor bounded to a small known range; f32 precision is ample"
                        )]
                        scale: l.scale as f32,
                        transform: l.transform,
                        color_profile: config
                            .outputs
                            .get(&l.connector_name)
                            .and_then(|output| output.color_profile.as_ref())
                            .filter(|profile| self.color_profiles.contains_key(*profile))
                            .cloned(),
                    },
                )
            })
            .collect();

        self.rebuild_cursor_routing();
    }

    /// Point every cursor plane at its output's rectangle in the *new* layout — a stale rect routes the pointer onto the wrong monitor.
    fn rebuild_cursor_routing(&mut self) {
        for entry in &self.live {
            let logical = logical_output_size(entry.size, entry.transform);
            self.hw_cursor.set_rect(
                entry.id,
                (entry.position.0, entry.position.1, logical.0, logical.1),
            );
            self.hw_cursor.set_transform(entry.id, entry.transform);
        }
        self.hw_cursor.move_to(self.pointer.0, self.pointer.1);
    }

    /// The public output table, in `OutputId` order.
    fn output_table(&self) -> Vec<Output> {
        let mut outs: Vec<Output> = self.output_meta.values().cloned().collect();
        outs.sort_by_key(|o| o.id.0);
        outs
    }

    /// Perform one step of the pause/resume sequence the session state machine asked for.
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
            // live on the render thread, so they travel together as one ordered `Resume` — splitting would let a frame slip between modeset and unpause.
            SessionAction::ResetOutputs => {
                let dpms_on = self
                    .session
                    .as_ref()
                    .is_none_or(super::session::SessionState::dpms_on);
                self.command(ScanoutCommand::Resume { dpms_on });
            }
            SessionAction::RestoreDpms | SessionAction::ResumeScanout => {}
            SessionAction::FullDamage => {
                // The scanout buffers hold whatever the other session left in them — nothing short of a full repaint is correct.
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
    /// Every failure is logged at debug and ignored: libinput errors on an
    /// unsupported setting, and [`input_config::plan_for`]'s capability gates already avoid asking — anything left is a driver quirk that must not take the session down.
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
        if let Some(on) = plan.left_handed {
            let _ = device.config_left_handed_set(on);
        }
        if let Some(flat) = plan.flat_acceleration {
            use smithay::reexports::input::AccelProfile;
            let _ = device.config_accel_set_profile(if flat {
                AccelProfile::Flat
            } else {
                AccelProfile::Adaptive
            });
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

    /// Route one gesture: to the shell when the finger count says so, otherwise on to the focused client.
    #[allow(clippy::needless_pass_by_value)]
    fn handle_gesture(&mut self, ev: GestureEvent) {
        // Decide ownership *before* touching the tracker. Only `begin`
        // carries a finger count, so every later event asks what the
        // gesture in flight belongs to — and `end` clears that, which
        // would leave a shell-owned gesture's closing event looking unowned, sending a client a `swipe_end` for a swipe it never saw begin.
        let shell_owns = self.input.touchpad_gestures
            && match ev {
                GestureEvent::SwipeBegin { fingers }
                | GestureEvent::PinchBegin { fingers }
                | GestureEvent::HoldBegin { fingers } => super::gestures::claims(fingers),
                _ => self.gestures.shell_owns_current(),
            };
        let action = match ev {
            // A hold has no shell meaning, but still registers as a gesture in flight so the finger count on its begin governs where its end goes — same tracking as a swipe.
            GestureEvent::SwipeBegin { fingers } | GestureEvent::HoldBegin { fingers } => {
                self.gestures.swipe_begin(fingers);
                None
            }
            GestureEvent::SwipeUpdate { dx, dy } => self.gestures.swipe_update(dx, dy),
            GestureEvent::SwipeEnd { .. } | GestureEvent::HoldEnd { .. } => {
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
        };
        if let Some(action) = action {
            self.push(BackendEvent::ShellGesture(action));
            // A navigation gesture changes what is on screen.
            self.push(BackendEvent::Redraw);
        }
        // Client delivery is gated on the shell not owning the gesture: a three-finger swipe that switched workspace *and* scrolled the app underneath is wrong in both directions.
        if !shell_owns {
            self.push(BackendEvent::Gesture(ev));
        }
    }

    /// Translate one libinput keyboard event through xkb and enqueue the
    /// resulting events: a `ModifiersChanged` when the Ctrl/Shift/Alt/
    /// Super mask flips, followed by the `KeyboardInput` itself. No-op when the xkb keymap failed to load.
    ///
    /// `keycode` is smithay's [`KeyboardKeyEvent::key_code`], already including the +8 evdev→xkb offset, so it feeds straight into xkb state.
    fn handle_keyboard_key(&mut self, keycode: xkb::Keycode, pressed: bool) {
        if self.accessibility.slow_keys {
            if pressed {
                self.slow_pending.insert(keycode, std::time::Instant::now());
                return;
            }
            let Some(started) = self.slow_pending.remove(&keycode) else {
                return;
            };
            if started.elapsed() < std::time::Duration::from_millis(300) {
                return;
            }
            self.process_keyboard_key(keycode, true);
        }
        self.process_keyboard_key(keycode, pressed);
        if pressed {
            if self
                .keyboard
                .as_ref()
                .is_some_and(|keyboard| keyboard.key_repeats(keycode))
            {
                let (delay, rate) = self.input.key_repeat_params();
                self.key_repeat
                    .press(keycode, std::time::Instant::now(), delay, rate);
            }
        } else {
            self.key_repeat.release(keycode);
        }
    }

    fn pump_keyboard_repeat(&mut self, now: std::time::Instant) {
        for keycode in self.key_repeat.due(now) {
            let input = self
                .keyboard
                .as_ref()
                .and_then(|keyboard| keyboard.repeat_event(keycode));
            if let Some(input) = input {
                self.events.push(BackendEvent::Input(input));
            }
        }
    }

    fn process_keyboard_key(&mut self, keycode: xkb::Keycode, pressed: bool) {
        // Scope the `&mut self.keyboard` borrow so we can push onto `self.events` afterwards without aliasing.
        let outcome = {
            let Some(kbd) = self.keyboard.as_mut() else {
                return;
            };
            kbd.process(keycode, pressed)
        };
        if let Some(mods) = outcome.modifiers_changed {
            let actual = mods.state();
            if self.accessibility.sticky_keys {
                if actual.is_empty() && !self.physical_modifiers.is_empty() {
                    self.sticky_latched |= self.physical_modifiers;
                    self.physical_modifiers = ModifiersState::empty();
                    self.events
                        .push(BackendEvent::ModifiersChanged(Modifiers::from(
                            self.sticky_latched,
                        )));
                } else {
                    self.physical_modifiers = actual;
                    self.events
                        .push(BackendEvent::ModifiersChanged(Modifiers::from(
                            actual | self.sticky_latched,
                        )));
                }
            } else {
                self.events.push(BackendEvent::ModifiersChanged(mods));
            }
        }
        if let Some(input) = outcome.input {
            let consumes_sticky =
                self.accessibility.sticky_keys && pressed && !self.sticky_latched.is_empty();
            self.events.push(BackendEvent::Input(input));
            if consumes_sticky {
                self.sticky_latched = ModifiersState::empty();
                self.events
                    .push(BackendEvent::ModifiersChanged(Modifiers::from(
                        self.physical_modifiers,
                    )));
            }
        }
    }

    /// Handle one libseat session event: a VT switch. Pausing has to stop
    /// scanout and give up DRM master before the incoming session can
    /// modeset; activating has to take master back and rebuild every
    /// output, since nothing about a CRTC survives the switch.
    /// `super::session`'s state machine owns the ordering; this only
    /// performs the actions it returns. Extracted from [`DrmRuntime::new`]'s
    /// calloop source registration so that function stays a readable list
    /// of "wire this event source up," not the handler bodies themselves.
    fn handle_session_event(&mut self, event: smithay::backend::session::Event) {
        use smithay::backend::session::Event as SeatEvent;
        let tick = match event {
            SeatEvent::ActivateSession => {
                tracing::info!("DRM backend: session activated");
                SeatTick::Activate
            }
            SeatEvent::PauseSession => {
                tracing::info!("DRM backend: session paused");
                self.key_repeat.cancel();
                SeatTick::Pause
            }
        };
        let actions = match self.session.as_ref() {
            Some(s) => s.apply_with_actions(tick),
            None => Vec::new(),
        };
        self.perform_all(actions);
    }

    /// Handle one udev event for a DRM device. Every one of the three is
    /// the same question — what is connected now? — so all three
    /// re-enumerate and reconcile; the hotplug plan decides whether that
    /// implies any work. `Changed` is what a monitor plugged into a live
    /// card raises, so treating it as a no-op means plugging in a display
    /// does nothing. Extracted from [`DrmRuntime::new`] for the same reason
    /// as [`Self::handle_session_event`].
    fn handle_udev_event(&mut self, event: smithay::backend::udev::UdevEvent) {
        use smithay::backend::udev::UdevEvent;
        match event {
            UdevEvent::Added { device_id, path } => {
                tracing::info!(?device_id, ?path, "DRM: udev device added");
                self.rescan_outputs("udev device added");
            }
            UdevEvent::Changed { device_id } => {
                tracing::debug!(?device_id, "DRM: udev device changed");
                self.rescan_outputs("udev device changed");
            }
            UdevEvent::Removed { device_id } => {
                tracing::info!(?device_id, "DRM: udev device removed");
                self.rescan_outputs("udev device removed");
            }
        }
    }

    /// Handle one libinput event: pointer, keyboard, gestures and touch all
    /// come through here, translated into winit-shaped `WindowEvent`s the
    /// chrome handles — a bare-TTY keystroke takes the same path as a
    /// nested dev window's. Extracted from [`DrmRuntime::new`] for the same
    /// reason as [`Self::handle_session_event`]; this one dwarfed the rest
    /// of the constructor at ~160 lines on its own.
    /// Handle one libinput event: pointer, keyboard, gestures and touch all
    /// come through here, translated into winit-shaped `WindowEvent`s the
    /// chrome handles — a bare-TTY keystroke takes the same path as a
    /// nested dev window's. Extracted from [`DrmRuntime::new`] for the same
    /// reason as [`Self::handle_session_event`]; this one dwarfed the rest
    /// of the constructor at ~160 lines on its own.
    fn handle_libinput_event(&mut self, event: InputEvent<LibinputInputBackend>) {
        match event {
            InputEvent::DeviceAdded { .. } | InputEvent::DeviceRemoved { .. } => {
                self.handle_libinput_device_event(event);
            }
            InputEvent::PointerMotionAbsolute { .. }
            | InputEvent::PointerMotion { .. }
            | InputEvent::PointerButton { .. }
            | InputEvent::PointerAxis { .. } => {
                self.handle_libinput_pointer_event(event);
            }
            // Keyboard: run the libinput keycode through xkb to get a
            // logical key + text, then enqueue a winit-shaped
            // `KeyboardInput` (plus `ModifiersChanged` on a modifier flip)
            // — the same wire the winit backend feeds, so `dispatch_event` handles DRM keystrokes identically to hosted-mode ones.
            InputEvent::Keyboard { event } => {
                let pressed = matches!(event.state(), KeyState::Pressed);
                self.handle_keyboard_key(event.key_code(), pressed);
            }
            InputEvent::GestureSwipeBegin { .. }
            | InputEvent::GestureSwipeUpdate { .. }
            | InputEvent::GestureSwipeEnd { .. }
            | InputEvent::GesturePinchBegin { .. }
            | InputEvent::GesturePinchUpdate { .. }
            | InputEvent::GesturePinchEnd { .. }
            | InputEvent::GestureHoldBegin { .. }
            | InputEvent::GestureHoldEnd { .. } => {
                self.handle_libinput_gesture_event(event);
            }
            InputEvent::TouchDown { .. }
            | InputEvent::TouchMotion { .. }
            | InputEvent::TouchUp { .. }
            | InputEvent::TouchCancel { .. }
            | InputEvent::TouchFrame { .. } => {
                self.handle_libinput_touch_event(event);
            }
            // Switches (lid, tablet mode) and tablet-tool events are handled elsewhere or not at all; unlike the gestures above they were never advertised to clients, so dropping them promises nothing undelivered.
            _ => {}
        }
    }

    /// `InputEvent::DeviceAdded`/`DeviceRemoved` half of
    /// [`Self::handle_libinput_event`] — split out to keep that dispatcher
    /// short. A device appearing is the only moment libinput will accept
    /// its configuration (at startup and on real hot-plug) — the arm that
    /// makes persisted input settings reach the hardware at all.
    fn handle_libinput_device_event(&mut self, event: InputEvent<LibinputInputBackend>) {
        match event {
            InputEvent::DeviceAdded { mut device } => {
                self.configure_device(&mut device);
                self.devices.push(device);
            }
            InputEvent::DeviceRemoved { device } => {
                tracing::info!(device = device.name(), "DRM input: device removed");
                self.devices.retain(|d| *d != device);
            }
            _ => {}
        }
    }

    /// `InputEvent::PointerMotionAbsolute`/`PointerMotion`/`PointerButton`/
    /// `PointerAxis` half of [`Self::handle_libinput_event`] — split out to
    /// keep that dispatcher short.
    fn handle_libinput_pointer_event(&mut self, event: InputEvent<LibinputInputBackend>) {
        match event {
            InputEvent::PointerMotionAbsolute { event } => {
                let (w, h) = self.output_size;
                let x = event.x_transformed(w.max(1));
                let y = event.y_transformed(h.max(1));
                self.pointer = (x, y);
                // Reposition the hardware cursor plane immediately — no framebuffer redraw needed; `CursorPlanes` routes the global position to the plane under the pointer.
                self.hw_cursor.move_to(x, y);
                self.push(BackendEvent::Input(cursor_moved_event(x, y)));
            }
            // Relative motion (mouse): accumulate + clamp to output.
            InputEvent::PointerMotion { event } => {
                let (w, h) = self.output_size;
                // `w`/`h` are clamped to at least 1 just above, so this
                // never underflows; `saturating_sub` just avoids a raw `-`
                // clippy can't see is bounded.
                let max_x = f64::from(w.max(1).saturating_sub(1));
                let max_y = f64::from(h.max(1).saturating_sub(1));
                let x = (self.pointer.0 + event.delta_x()).clamp(0.0, max_x);
                let y = (self.pointer.1 + event.delta_y()).clamp(0.0, max_y);
                self.pointer = (x, y);
                self.hw_cursor.move_to(x, y);
                self.push(BackendEvent::Input(cursor_moved_event(x, y)));
            }
            InputEvent::PointerButton { event } => {
                if let Some(button) = map_mouse_button(event.button()) {
                    let st = match event.state() {
                        ButtonState::Pressed => ElementState::Pressed,
                        ButtonState::Released => ElementState::Released,
                    };
                    self.push(BackendEvent::Input(mouse_input_event(st, button)));
                }
            }
            InputEvent::PointerAxis { event } => {
                let dx = event.amount(Axis::Horizontal).unwrap_or(0.0);
                let dy = event.amount(Axis::Vertical).unwrap_or(0.0);
                // Keep finger/continuous scrolling in pixel units. Converting
                // it to line deltas quantises short touchpad movements and
                // makes kinetic scrolling look laggy; only a wheel has a
                // discrete line interpretation. Zero-valued finger events
                // are retained as the terminating scroll frame.
                self.push(BackendEvent::Input(mouse_wheel_event(
                    event.source(),
                    dx,
                    dy,
                )));
            }
            _ => {}
        }
    }

    /// The touchpad-gesture arms of [`Self::handle_libinput_event`] — split
    /// out to keep that dispatcher short.
    fn handle_libinput_gesture_event(&mut self, event: InputEvent<LibinputInputBackend>) {
        match event {
            InputEvent::GestureSwipeBegin { event } => {
                self.handle_gesture(GestureEvent::SwipeBegin {
                    fingers: event.fingers(),
                });
            }
            InputEvent::GestureSwipeUpdate { event } => {
                self.handle_gesture(GestureEvent::SwipeUpdate {
                    dx: event.delta_x(),
                    dy: event.delta_y(),
                });
            }
            InputEvent::GestureSwipeEnd { event } => {
                self.handle_gesture(GestureEvent::SwipeEnd {
                    cancelled: event.cancelled(),
                });
            }
            InputEvent::GesturePinchBegin { event } => {
                self.handle_gesture(GestureEvent::PinchBegin {
                    fingers: event.fingers(),
                });
            }
            InputEvent::GesturePinchUpdate { event } => {
                self.handle_gesture(GestureEvent::PinchUpdate {
                    dx: event.delta_x(),
                    dy: event.delta_y(),
                    scale: event.scale(),
                    rotation: event.rotation(),
                });
            }
            InputEvent::GesturePinchEnd { event } => {
                self.handle_gesture(GestureEvent::PinchEnd {
                    cancelled: event.cancelled(),
                });
            }
            InputEvent::GestureHoldBegin { event } => {
                self.handle_gesture(GestureEvent::HoldBegin {
                    fingers: event.fingers(),
                });
            }
            InputEvent::GestureHoldEnd { event } => {
                self.handle_gesture(GestureEvent::HoldEnd {
                    cancelled: event.cancelled(),
                });
            }
            _ => {}
        }
    }

    /// The touchscreen arms of [`Self::handle_libinput_event`] — split out
    /// to keep that dispatcher short. Each touch point is delivered twice:
    /// once as a `wl_touch` event for real multi-touch clients, and once as
    /// pointer motion + a left click so the shell chrome (driven by the
    /// winit event stream, no touch path) responds like a click. Without the
    /// second half, a touchscreen can operate apps but not the desktop.
    fn handle_libinput_touch_event(&mut self, event: InputEvent<LibinputInputBackend>) {
        match event {
            InputEvent::TouchDown { event } => {
                let (x, y) = touch_position(self, &event);
                self.pointer = (x, y);
                self.hw_cursor.move_to(x, y);
                self.push(BackendEvent::Touch(QosTouchEvent::Down {
                    slot: touch_slot(event.slot()),
                    x,
                    y,
                }));
                self.push(BackendEvent::Input(cursor_moved_event(x, y)));
                self.push(BackendEvent::Input(mouse_input_event(
                    ElementState::Pressed,
                    MouseButton::Left,
                )));
            }
            InputEvent::TouchMotion { event } => {
                let (x, y) = touch_position(self, &event);
                self.pointer = (x, y);
                self.hw_cursor.move_to(x, y);
                self.push(BackendEvent::Touch(QosTouchEvent::Motion {
                    slot: touch_slot(event.slot()),
                    x,
                    y,
                }));
                self.push(BackendEvent::Input(cursor_moved_event(x, y)));
            }
            InputEvent::TouchUp { event } => {
                self.push(BackendEvent::Touch(QosTouchEvent::Up {
                    slot: touch_slot(event.slot()),
                }));
                self.push(BackendEvent::Input(mouse_input_event(
                    ElementState::Released,
                    MouseButton::Left,
                )));
            }
            InputEvent::TouchCancel { .. } => {
                self.push(BackendEvent::Touch(QosTouchEvent::Cancel));
                // A cancelled touch is not a click — release the synthetic button so the chrome doesn't sit on a phantom held pointer.
                self.push(BackendEvent::Input(mouse_input_event(
                    ElementState::Released,
                    MouseButton::Left,
                )));
            }
            InputEvent::TouchFrame { .. } => {
                self.push(BackendEvent::Touch(QosTouchEvent::Frame));
            }
            _ => {}
        }
    }
}

/// Owning handle for everything the DRM backend keeps alive between frames — dropping it tears down the session in libseat-correct order (libinput first, then udev, then the seat).
/// Keeps ICC filesystem walks and profile parsing away from both Settings and
/// the compositor loop. Requests are coalesced by a one-slot channel; results
/// are immutable maps swapped into [`DrmDispatchState`] at poll boundaries.
struct IccProfileWorker {
    requests: SyncSender<()>,
    results: Receiver<BTreeMap<String, String>>,
}

impl IccProfileWorker {
    fn new() -> Result<Self, DrmBackendError> {
        let (requests, request_rx) = sync_channel(1);
        let (result_tx, results) = channel();
        std::thread::Builder::new()
            .name("qos-icc-profile-probe".into())
            .spawn(move || {
                while request_rx.recv().is_ok() {
                    if result_tx.send(discover_icc_profiles()).is_err() {
                        break;
                    }
                }
            })
            .map_err(|error| {
                DrmBackendError::Calloop(format!("failed to start ICC profile worker: {error}"))
            })?;
        Ok(Self { requests, results })
    }

    fn request_refresh(&self) {
        let _ = self.requests.try_send(());
    }

    fn take_latest(&self) -> Option<BTreeMap<String, String>> {
        self.results.try_iter().last()
    }
}

pub struct DrmRuntime {
    /// calloop loop this backend ticks; we own it so `submit_frame`/`poll_events` can dispatch without a parent loop's help.
    pub event_loop: EventLoop<'static, DrmDispatchState>,
    pub dispatch: DrmDispatchState,
    /// Kept alive so libseat doesn't close the seat — the notifier is inserted into `event_loop` and its source token lives there.
    session: LibSeatSession,
    /// KMS card path that successfully acquired at least one scanout.
    pub primary_gpu: Option<PathBuf>,
    /// Live scanouts — one per connected output. Construction requires at
    /// least one; the vector can become empty later if every display is
    /// unplugged. Ordered by `OutputId`; `scanouts[0]` is the primary.
    pub scanouts: Vec<ScanoutEntry>,
    /// Receiving end of the reconfiguration channel — held here while the backend owns the scanouts inline; moved into the presenter along with them by [`super::Backend::take_presenter`].
    pub commands: Option<Receiver<ScanoutCommand>>,
    /// Bounded watchdog for unconfirmed display changes. It lives in the
    /// compositor rather than Settings so closing/crashing the app cannot
    /// strand the session on a blank mode.
    last_display_transaction_check: std::time::Instant,
    last_display_snapshot_publish: std::time::Instant,
    last_icc_profile_refresh: std::time::Instant,
    icc_profiles: IccProfileWorker,
}

/// One live output: its public [`OutputId`], top-left position in the combined coordinate space (laid out side-by-side), and the [`DrmScanout`] driving its crtc.
pub struct ScanoutEntry {
    pub id: OutputId,
    pub position: (i32, i32),
    pub transform: settings_store::hardware::OutputTransform,
    pub color_profile: Option<String>,
    pub color_transform: Option<qcms::Transform>,
    pub color_buffer: Vec<u8>,
    pub scanout: DrmScanout,
}

impl ScanoutEntry {
    pub fn set_color_profile(&mut self, profile: Option<String>) {
        if self.color_profile == profile {
            return;
        }
        self.color_profile = profile;
        self.color_transform = self.color_profile.as_deref().and_then(|path| {
            let input = qcms::Profile::new_sRGB();
            let output = qcms::Profile::new_from_path(path)?;
            qcms::Transform::new(
                &input,
                &output,
                qcms::DataType::RGBA8,
                qcms::Intent::Perceptual,
            )
        });
        if self.color_profile.is_some() && self.color_transform.is_none() {
            tracing::warn!(profile = ?self.color_profile, "DRM display: rejected invalid ICC profile");
        }
    }
}

/// Lay output sizes out left-to-right at `y = 0` — a thin alias for
/// [`hotplug::layout`] so startup and hotplug pack outputs through one implementation, or drift shows a monitor in the wrong place after a re-plug.
#[cfg(test)]
use hotplug::layout as side_by_side_layout;

pub(crate) fn logical_output_size(
    physical: (u32, u32),
    transform: settings_store::hardware::OutputTransform,
) -> (u32, u32) {
    use settings_store::hardware::OutputTransform::{Rotate90, Rotate270};
    if matches!(transform, Rotate90 | Rotate270) {
        (physical.1, physical.0)
    } else {
        physical
    }
}

/// Bring up scanout(s) on the first usable GPU, keeping its card fd open as
/// DRM master. Candidates are already ranked by connected connectors, but a
/// card can still fail at open or modeset time. Trying the remaining cards is
/// important on hybrid systems and when firmware exposes an unusable KMS node.
///
/// `open_all_scanouts` opens a card once and modesets every connected
/// connector, each on its own crtc, sharing a dup'd fd. libseat refuses a
/// second `open` of the selected node, so the connector-enumeration pass in
/// [`enumerate_secondary_connectors`] excludes it.
fn bring_up_first_scanouts(
    session: &LibSeatSession,
    kms_gpus: &[PathBuf],
) -> Option<(PathBuf, CardHandle, Vec<ScanoutBuild>)> {
    for path in kms_gpus {
        eprintln!(
            "nirios compositor: trying KMS scanout device {}",
            path.display()
        );
        let mut scanout_session = session.clone();
        match open_all_scanouts(&mut scanout_session, path) {
            Ok((card, builds)) if !builds.is_empty() => {
                eprintln!(
                    "nirios compositor: {} live DRM output(s) on {}",
                    builds.len(),
                    path.display()
                );
                tracing::info!(
                    count = builds.len(),
                    device = %path.display(),
                    "DRM backend: scanout live"
                );
                return Some((path.clone(), card, builds));
            }
            Ok((_card, _)) => {
                eprintln!(
                    "nirios compositor: no connected DRM output on {}",
                    path.display()
                );
                tracing::warn!(device = %path.display(), "DRM backend: no connected scanout");
            }
            Err(error) => {
                eprintln!(
                    "nirios compositor: cannot use DRM output on {}: {error}",
                    path.display()
                );
                tracing::warn!(
                    device = %path.display(),
                    %error,
                    "DRM backend: scanout setup failed"
                );
            }
        }
    }
    None
}

/// Connector enumeration on the GPUs scanout does not own (the primary is
/// held open by [`bring_up_first_scanouts`]). Driving connectors on
/// secondary GPUs needs a second card fd plus cross-GPU buffer handling, so
/// this pass is diagnostic-only (logs what's attached). Failures are logged
/// and skipped, not fatal; `LibSeatSession` is cheap to clone. Extracted
/// from [`DrmRuntime::new`] for the same reason as
/// [`bring_up_first_scanouts`].
fn enumerate_secondary_connectors(
    session: &LibSeatSession,
    kms_gpus: Vec<PathBuf>,
    primary_gpu: &PathBuf,
) -> Vec<ConnectorInfo> {
    let enum_gpus: Vec<PathBuf> = kms_gpus
        .into_iter()
        .filter(|path| path != primary_gpu)
        .collect();
    let mut session_for_enum = session.clone();
    let connectors = enumerate_connectors(&mut session_for_enum, &enum_gpus);
    tracing::info!(
        count = connectors.len(),
        "DRM backend: enumerated DRM connectors on secondary GPUs (diagnostic; not yet driven)"
    );
    connectors
}

/// Register the three loop-wide event sources: session (VT switch), udev
/// (hotplug), and libinput (all pointer/keyboard/gesture/touch input).
/// page-flip/DRM-device sources attach separately, once individual `cardN`
/// devices open. Extracted from [`DrmRuntime::new`] to keep that
/// constructor a readable top-level sequence — see
/// [`DrmDispatchState::handle_session_event`],
/// [`DrmDispatchState::handle_udev_event`], and
/// [`DrmDispatchState::handle_libinput_event`] for what each one actually does.
fn register_event_sources(
    handle: &LoopHandle<'static, DrmDispatchState>,
    session_notifier: LibSeatSessionNotifier,
    udev: UdevBackend,
    libinput_backend: LibinputInputBackend,
) -> Result<(), DrmBackendError> {
    // Session events: a VT switch. Pausing has to stop scanout and give up
    // DRM master before the incoming session can modeset; activating has
    // to take master back and rebuild every output, since nothing about a
    // CRTC survives the switch. `super::session`'s state machine owns the
    // ordering; the handler performs what it returns.
    handle
        .insert_source(
            session_notifier,
            |event, (), state: &mut DrmDispatchState| state.handle_session_event(event),
        )
        .map_err(|e| DrmBackendError::Calloop(format!("{e}")))?;

    // Every one of the three udev events is the same question — what is
    // connected now? — so all three re-enumerate and reconcile; the
    // hotplug plan decides whether that implies any work. `Changed` is
    // what a monitor plugged into a live card raises, so treating it as a
    // no-op means plugging in a display does nothing.
    handle
        .insert_source(udev, |event, (), state: &mut DrmDispatchState| {
            state.handle_udev_event(event);
        })
        .map_err(|e| DrmBackendError::Calloop(format!("{e}")))?;

    // libinput events. Pointer, keyboard, gestures and touch all come
    // through here, translated into winit-shaped `WindowEvent`s the chrome
    // handles — a bare-TTY keystroke takes the same path as a nested dev
    // window's.
    handle
        .insert_source(
            libinput_backend,
            |event, (), state: &mut DrmDispatchState| state.handle_libinput_event(event),
        )
        .map_err(|e| DrmBackendError::Calloop(format!("{e}")))?;

    Ok(())
}

impl DrmRuntime {
    /// Acquire the session, enumerate DRM devices, attach libinput, and wire everything onto an internal calloop — returns `Err`, not a panic, matching the binary entry point's `eprintln!` + `exit(1)` contract.
    ///
    /// # Errors
    ///
    /// Returns an error if the libseat session can't be acquired, udev
    /// enumeration fails, no KMS-capable `/dev/dri/cardN` device is found,
    /// libinput can't attach to the seat, or a source can't be inserted
    /// into the internal calloop event loop.
    pub fn new() -> Result<Self, DrmBackendError> {
        // `LibSeatSession::new()` blocks until the seat hands us an `Enable` event, so by return we either own the seat or already failed.
        let (session, session_notifier) = LibSeatSession::new().map_err(|e| {
            // `{e}` (Display): libseat's `Error` is a `thiserror` type whose
            // Display already folds in the wrapped `io::Error`'s detail
            // (e.g. "Function not implemented (os error 38)"), which reads
            // better here than the enum's raw Debug syntax — `classify()`
            // doesn't parse this string, so the format is cosmetic only.
            DrmBackendError::Seat(Box::new(super::seat_diag::diagnose_live(format!("{e}"))))
        })?;
        let seat_name = session.seat();
        tracing::info!(seat = %seat_name, "DRM backend: libseat session acquired");

        let session_state = SessionState::new_active();

        // Ask udev for the current device set before inserting the source, so we can refuse early if nothing is plugged in.
        let udev =
            UdevBackend::new(&seat_name).map_err(|e| DrmBackendError::Udev(format!("{e}")))?;
        let raw_gpus: Vec<PathBuf> =
            all_gpus(&seat_name).map_err(|e| DrmBackendError::Udev(format!("{e}")))?;
        let kms_gpus =
            order_by_connected_connectors(filter_kms_nodes(raw_gpus), Path::new("/sys/class/drm"));
        if kms_gpus.is_empty() {
            return Err(DrmBackendError::NoDevices);
        }
        tracing::info!(
            count = kms_gpus.len(),
            "DRM backend: enumerated KMS card nodes"
        );

        // Bring up scanout(s) on the highest-ranked usable GPU, then
        // diagnostically enumerate whatever is attached to the rest.
        let Some((primary_gpu, card, scanout_builds)) =
            bring_up_first_scanouts(&session, &kms_gpus)
        else {
            // `--backend headless` is the explicit display-free mode. A real
            // DRM login that cannot drive a connector must return control to
            // the display manager instead of staying alive behind the stale
            // grey framebuffer left by the greeter.
            return Err(DrmBackendError::NoScanout(kms_gpus));
        };
        // Diagnostic-only (logs internally); nothing later in construction
        // consults the result — see the helper's doc comment.
        enumerate_secondary_connectors(&session, kms_gpus, &primary_gpu);

        // The session-interface adapter routes libinput's `open`/`close` calls through libseat so it inherits privileged FDs without root.
        let mut libinput_ctx =
            Libinput::new_with_udev(LibinputSessionInterface::from(session.clone()));
        libinput_ctx
            .udev_assign_seat(&seat_name)
            .map_err(|e| DrmBackendError::Libinput(format!("{e:?}")))?;
        let libinput_backend = LibinputInputBackend::new(libinput_ctx);

        // The session notifier, udev backend, and libinput backend each register as event sources; page-flip/DRM-device sources attach once individual `cardN` devices open.
        let event_loop: EventLoop<'static, DrmDispatchState> =
            EventLoop::try_new().map_err(|e| DrmBackendError::Calloop(format!("{e}")))?;
        let handle: LoopHandle<'static, DrmDispatchState> = event_loop.handle();
        register_event_sources(&handle, session_notifier, udev, libinput_backend)?;

        // Compile the xkb keymap once at startup from the session's `XKB_DEFAULT_*` environment (installer-seeded, PAM-exported); a failure is non-fatal, running pointer-only rather than refusing to boot.
        let keyboard = KeyboardXkb::new();
        if keyboard.is_some() {
            tracing::info!("DRM backend: xkb keyboard initialised");
        } else {
            tracing::warn!("DRM backend: xkb keymap unavailable; keyboard input disabled");
        }

        // The persisted preferences, read once here so the same values drive libinput device config, gesture direction and the per-output scale override.
        let input = InputSettings::load();
        let accessibility = AccessibilityInputSettings::load();
        let scale_override = load_display_scale_override();
        tracing::info!(
            tap_to_click = input.tap_to_click,
            natural_scroll = input.natural_scroll_for(true),
            scale_override = ?scale_override,
            "DRM backend: loaded input and display settings"
        );

        // The scanout side and the loop side talk over one channel, so a reconfiguration is ordered against the frames around it.
        let (tx, rx) = channel::<ScanoutCommand>();
        let icc_profiles = IccProfileWorker::new()?;
        icc_profiles.request_refresh();

        let mut dispatch = DrmDispatchState::new(
            session_state,
            Some(card),
            input,
            accessibility,
            scale_override,
            tx,
            keyboard,
        );
        // Adopt the connectors already live at startup — the same path a
        // hot-plug takes — and seed the pointer/snapshot; see the helper's
        // doc comment.
        let scanouts = dispatch.finalize_startup_scanouts(scanout_builds);

        Ok(Self {
            event_loop,
            dispatch,
            session,
            primary_gpu: Some(primary_gpu),
            scanouts,
            commands: Some(rx),
            last_display_transaction_check: std::time::Instant::now(),
            last_display_snapshot_publish: std::time::Instant::now(),
            last_icc_profile_refresh: std::time::Instant::now(),
            icc_profiles,
        })
    }

    /// Switch the session to another virtual terminal.
    ///
    /// Not optional on a bare TTY: the compositor puts the keyboard into raw
    /// mode, so the kernel's `Ctrl+Alt+Fn` handling never sees the chord —
    /// without this the user is locked onto this VT for the session's life, and the pause/resume path below can never be reached.
    pub fn change_vt(&mut self, vt: i32) {
        use smithay::backend::session::Session as _;
        match self.session.change_vt(vt) {
            Ok(()) => tracing::info!(vt, "DRM backend: switching virtual terminal"),
            Err(e) => tracing::warn!("DRM backend: change_vt({vt}) failed: {e}"),
        }
    }

    /// Run the come-back-from-suspend sequence.
    ///
    /// A system suspend does not pause the seat, so no `ActivateSession`
    /// arrives on the way out — the state machine has nothing to react to.
    /// The DRM device still needs a full re-establish, which is why this is a separate, unconditional entry point rather than a case of the VT path.
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
    /// Devices appearing later pick up new settings from their
    /// `DeviceAdded`; already-attached ones are re-configured here from the dispatch state's handles, so a pointer-speed change is felt on the touchpad the user is holding, not the next one plugged in.
    pub fn reload_input_settings(&mut self) {
        let input = InputSettings::load();
        let keyboard_settings = KeyboardSettings::load();
        let previous_cursor_size = self.dispatch.accessibility.effective_cursor_size();
        let accessibility = AccessibilityInputSettings::load();
        self.dispatch.input = input;
        self.dispatch.key_repeat.cancel();
        if let Some(keyboard) = KeyboardXkb::from_settings(&keyboard_settings) {
            self.dispatch.keyboard = Some(keyboard);
            self.dispatch.slow_pending.clear();
            self.dispatch.physical_modifiers = ModifiersState::empty();
            self.dispatch.sticky_latched = ModifiersState::empty();
            self.dispatch
                .push(BackendEvent::ModifiersChanged(Modifiers::from(
                    ModifiersState::empty(),
                )));
            tracing::info!(
                sources = ?keyboard_settings.source_ids,
                active = keyboard_settings.active_index,
                "DRM input: applied per-user XKB sources"
            );
        } else {
            tracing::warn!(
                sources = ?keyboard_settings.source_ids,
                "DRM input: requested XKB keymap did not compile; preserving the working keymap"
            );
        }
        self.dispatch.accessibility = accessibility;
        if accessibility.effective_cursor_size() != previous_cursor_size {
            self.dispatch.command(ScanoutCommand::SetCursorSize(
                accessibility.effective_cursor_size(),
            ));
        }
        if !accessibility.sticky_keys {
            self.dispatch.sticky_latched = ModifiersState::empty();
        }
        if !accessibility.slow_keys {
            self.dispatch.slow_pending.clear();
        }
        self.dispatch.gestures.set_config(GestureConfig {
            natural_scroll: input.natural_scroll_for(true),
            ..GestureConfig::default()
        });
        let mut devices = std::mem::take(&mut self.dispatch.devices);
        for device in &mut devices {
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
        self.reconcile_display_transaction();
        // The same atomic settings write may carry a topology/mode change.
        // Re-probe first so a requested advertised mode is genuinely
        // modeset; then publish arrangement/scale even when the connector
        // set itself did not change.
        self.dispatch.rescan_outputs("display settings changed");
        self.dispatch.relayout_outputs();
        let outputs = self.dispatch.output_table();
        self.dispatch.command(ScanoutCommand::Reconfigure {
            remove: Vec::new(),
            add: Vec::new(),
            outputs,
        });
        self.dispatch.outputs_changed = true;
        self.dispatch.push(BackendEvent::OutputsChanged);
        self.dispatch.push(BackendEvent::Redraw);
        self.dispatch.publish_display_snapshot();
    }

    /// Check the timed display confirmation independently of settings-file
    /// changes. The compositor loop calls this continuously, but filesystem
    /// work is bounded to four times a second.
    pub fn poll_display_transaction(&mut self) {
        let now = std::time::Instant::now();
        if now.duration_since(self.last_display_transaction_check)
            < std::time::Duration::from_millis(250)
        {
            return;
        }
        self.last_display_transaction_check = now;
        if let Some(profiles) = self.icc_profiles.take_latest()
            && profiles != self.dispatch.color_profiles
        {
            self.dispatch.color_profiles = profiles;
            self.dispatch.relayout_outputs();
            self.dispatch.command(ScanoutCommand::Reconfigure {
                remove: Vec::new(),
                add: Vec::new(),
                outputs: self.dispatch.output_table(),
            });
            self.dispatch.outputs_changed = true;
            self.dispatch.push(BackendEvent::OutputsChanged);
            self.dispatch.push(BackendEvent::Redraw);
            self.dispatch.publish_display_snapshot();
        }
        if now.duration_since(self.last_icc_profile_refresh) >= std::time::Duration::from_secs(30) {
            self.last_icc_profile_refresh = now;
            self.icc_profiles.request_refresh();
        }
        if now.duration_since(self.last_display_snapshot_publish)
            >= std::time::Duration::from_secs(5)
        {
            self.last_display_snapshot_publish = now;
            self.dispatch.publish_display_snapshot();
        }
        let Some(transaction) = settings_store::hardware::DisplayTransaction::load() else {
            return;
        };
        if transaction.expired(settings_store::hardware::unix_ms()) {
            self.reload_input_settings();
        }
    }

    /// Advance one display request before the KMS re-probe consumes its
    /// effective configuration. Validation uses the compositor-published
    /// descriptor set, never values supplied by Settings.
    fn reconcile_display_transaction(&self) {
        use settings_store::hardware::{DisplayTransaction, DisplayTransactionState};

        let Some(mut transaction) = DisplayTransaction::load() else {
            return;
        };
        let now = settings_store::hardware::unix_ms();
        if transaction.expired(now) {
            transaction.state = DisplayTransactionState::RevertRequested;
            transaction.message = Some("Display change timed out and was restored".into());
        }
        match transaction.state {
            DisplayTransactionState::Requested => {
                let descriptors: Vec<_> = self
                    .dispatch
                    .card
                    .as_ref()
                    .map(|card| card.display_capabilities(&self.dispatch.color_profiles))
                    .unwrap_or_default()
                    .into_iter()
                    .map(|output| output.descriptor)
                    .collect();
                if descriptors.is_empty() {
                    transaction.state = DisplayTransactionState::Rejected;
                    transaction.message =
                        Some("No compositor-owned DRM capability snapshot is available".into());
                } else if let Err(error) = transaction.candidate.validate_for(&descriptors) {
                    transaction.state = DisplayTransactionState::Rejected;
                    transaction.message =
                        Some(format!("Rejected unsupported display plan: {error:?}"));
                } else {
                    transaction.state = DisplayTransactionState::AwaitingConfirmation;
                    transaction.deadline_unix_ms =
                        Some(now.saturating_add(DisplayTransaction::CONFIRM_TIMEOUT_MS));
                    transaction.message = Some(
                        "Applied temporarily; confirm within 15 seconds or it will be restored"
                            .into(),
                    );
                }
                if let Err(error) = transaction.save() {
                    tracing::warn!(%error, "DRM display: failed to update transaction");
                }
            }
            DisplayTransactionState::ConfirmRequested => {
                if let Err(error) = transaction.candidate.save() {
                    transaction.state = DisplayTransactionState::Rejected;
                    transaction.message = Some(format!("Could not commit display plan: {error}"));
                } else {
                    transaction.state = DisplayTransactionState::Confirmed;
                    transaction.deadline_unix_ms = None;
                    transaction.message = Some("Display configuration kept".into());
                }
                let _ = transaction.save();
            }
            DisplayTransactionState::RevertRequested => {
                if let Err(error) = transaction.previous.save() {
                    transaction.state = DisplayTransactionState::Rejected;
                    transaction.message = Some(format!("Could not restore display plan: {error}"));
                } else {
                    transaction.state = DisplayTransactionState::Reverted;
                    transaction.deadline_unix_ms = None;
                    transaction
                        .message
                        .get_or_insert_with(|| "Previous display configuration restored".into());
                }
                let _ = transaction.save();
            }
            DisplayTransactionState::AwaitingConfirmation
            | DisplayTransactionState::Confirmed
            | DisplayTransactionState::Reverted
            | DisplayTransactionState::Rejected => {}
        }
    }

    /// Drain pending events from the internal calloop — non-blocking, a zero timeout so the outer compositor loop owns the cadence.
    pub fn dispatch_pending(&mut self) {
        use std::time::Duration;
        let _ = self.event_loop.dispatch(Duration::ZERO, &mut self.dispatch);
        self.dispatch
            .pump_keyboard_repeat(std::time::Instant::now());
    }

    /// Test-only seam: construct a `DrmRuntime`-shaped dispatch state with the given output set already populated, used by `super::mod` tests to verify `DrmBackend::outputs()` without real hardware.
    #[cfg(test)]
    pub fn synthesize_for_tests(outputs: Vec<(OutputId, Output)>) -> DrmDispatchState {
        let mut registry = OutputRegistry::new();
        // Re-intern so the registry's `next_id` is consistent with the synthesised ids — arbitrary keys, since the test only cares about `output_meta`.
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

/// Ask one libinput device what it can be configured to do — every
/// question is a capability query, asked before setting to keep the path free of error returns for settings a device never accepted.
fn device_caps(device: &smithay::reexports::input::Device) -> DeviceCaps {
    DeviceCaps {
        supports_tap: device.config_tap_finger_count() > 0,
        supports_natural_scroll: device.config_scroll_has_natural_scroll(),
        supports_accel: device.config_accel_is_available(),
        supports_dwt: device.config_dwt_is_available(),
        supports_middle_emulation: device.config_middle_emulation_is_available(),
        is_pointer: device.has_capability(smithay::reexports::input::DeviceCapability::Pointer),
        supports_left_handed: device.config_left_handed_is_available(),
        supports_flat_acceleration: device
            .config_accel_profiles()
            .contains(&smithay::reexports::input::AccelProfile::Flat),
    }
}

/// The `display.scale` the user has set, or `None`. Distinguishing
/// "unset" from "set to 1.0" is the point: unset derives the scale from the panel, 1.0 means the user chose 100% on a `HiDPI` panel.
fn load_display_scale_override() -> Option<f64> {
    settings_store::load_section("display")
        .get("scale")
        .and_then(serde_json::Value::as_f64)
        .filter(|s| (0.5..=3.0).contains(s))
}

/// Touch slot as a plain integer for the backend event. libinput reports `-1` for single-touch devices with no slot concept — all one finger, so slot 0 is honest.
fn touch_slot(slot: TouchSlot) -> u32 {
    let id: i32 = slot.into();
    // `max(0)` already clamps to non-negative; `try_from` just avoids an
    // `as` cast clippy can't see is sign-safe.
    u32::try_from(id.max(0)).unwrap_or(0)
}

/// Map a normalised touch position onto the combined output space.
fn touch_position<E>(state: &DrmDispatchState, event: &E) -> (f64, f64)
where
    E: AbsolutePositionEvent<LibinputInputBackend>,
{
    let (w, h) = state.output_size;
    (event.x_transformed(w.max(1)), event.y_transformed(h.max(1)))
}

/// Build a `WindowEvent::CursorMoved` for the compositor's input wire. `device_id` is zeroed — the host path never inspects it (mirrors `native::execute_planned_action`).
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
/// positive axis direction is down/right while winit's positive direction is
/// up/left. Preserve the source: a wheel amount is in degrees (~15/click),
/// while finger and continuous amounts are pixels.
fn mouse_wheel_event(source: AxisSource, dx: f64, dy: f64) -> WindowEvent {
    let delta = match source {
        AxisSource::Wheel | AxisSource::WheelTilt => {
            // Scroll deltas in wheel "lines," not device pixels — f32
            // precision is ample and matches winit's LineDelta type.
            #[allow(
                clippy::cast_possible_truncation,
                reason = "scroll delta in wheel lines; f32 precision is ample and matches winit's LineDelta type"
            )]
            MouseScrollDelta::LineDelta(-(dx / 15.0) as f32, -(dy / 15.0) as f32)
        }
        AxisSource::Finger | AxisSource::Continuous => {
            MouseScrollDelta::PixelDelta(PhysicalPosition::new(-dx, -dy))
        }
    };
    WindowEvent::MouseWheel {
        device_id: unsafe { std::mem::zeroed() },
        delta,
        phase: TouchPhase::Moved,
    }
}

/// Map a raw Linux input button code (`input-event-codes.h`) to winit's
/// `MouseButton`. Preserve the browser navigation buttons: dropping them in
/// the DRM adapter made a real mouse behave differently from the hosted
/// backend and prevented Wayland clients from receiving back/forward clicks.
fn map_mouse_button(code: u32) -> Option<MouseButton> {
    match code {
        0x110 => Some(MouseButton::Left),
        0x111 => Some(MouseButton::Right),
        0x112 => Some(MouseButton::Middle),
        0x115 => Some(MouseButton::Forward),
        0x116 => Some(MouseButton::Back),
        _ => None,
    }
}

/// xkb keymap + state owned by the DRM backend, turning libinput keycodes
/// into the winit `KeyboardInput` the chrome understands — the winit backend gets these from the OS, but bare-TTY libinput only reports raw keycodes.
pub struct KeyboardXkb {
    // Held for the lifetime of `state`. xkbcommon refcounts internally, but keeping ownership explicit makes the lifetime obvious (never read after construction, hence the underscore).
    _context: xkb::Context,
    _keymap: xkb::Keymap,
    state: xkb::State,
    /// Last modifier mask we emitted — we only push `ModifiersChanged` when it actually flips, matching winit's behaviour.
    mods: ModifiersState,
}

/// What translating one key event yields: an optional `KeyboardInput` (absent for keys with no logical mapping, e.g. bare modifiers) and an optional `ModifiersChanged` (present only when the mask flips).
struct KeyOutcome {
    input: Option<WindowEvent>,
    modifiers_changed: Option<Modifiers>,
}

impl KeyboardXkb {
    /// Compile the validated source list from the per-user settings file.
    /// Returns `None` (caller logs) if the keymap can't build, degrading to
    /// pointer-only at boot and preserving the old working map on hot reload.
    pub fn new() -> Option<Self> {
        let requested = KeyboardSettings::load();
        Self::from_settings(&requested).or_else(|| {
            tracing::warn!(
                sources = ?requested.source_ids,
                "DRM input: saved XKB sources failed at session start; falling back to US"
            );
            Self::from_settings(&KeyboardSettings::default())
        })
    }

    pub fn from_settings(settings: &KeyboardSettings) -> Option<Self> {
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        let keymap = xkb::Keymap::new_from_names(
            &context,
            "", // rules -> XKB_DEFAULT_RULES (normally evdev)
            "", // model -> XKB_DEFAULT_MODEL
            &settings.layouts,
            &settings.variants,
            settings.options.clone(),
            xkb::KEYMAP_COMPILE_NO_FLAGS,
        )?;
        let mut state = xkb::State::new(&keymap);
        let active = u32::try_from(settings.active_index).ok()?;
        let _ = state.update_mask(0, 0, 0, 0, 0, active);
        Some(Self {
            _context: context,
            _keymap: keymap,
            state,
            mods: ModifiersState::empty(),
        })
    }

    fn key_repeats(&self, keycode: xkb::Keycode) -> bool {
        self._keymap.key_repeats(keycode)
    }

    /// Produce the same repeated `KeyboardInput` winit emits without
    /// mutating xkb's down/up state a second time.
    fn repeat_event(&self, keycode: xkb::Keycode) -> Option<WindowEvent> {
        let sym = self.state.key_get_one_sym(keycode);
        let logical = keysym_to_winit_key(sym.raw())?;
        let mut input = keyboard_input_event(
            logical,
            text_for(sym.key_char(), self.current_mods()),
            true,
            keycode.raw().saturating_sub(8),
        );
        if let WindowEvent::KeyboardInput { event, .. } = &mut input {
            event.repeat = true;
        }
        Some(input)
    }

    /// Read the current effective Ctrl/Shift/Alt/Super state as a winit `ModifiersState`.
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

    /// Translate one key event. `keycode` already carries the +8 offset (see [`KeyboardKeyEvent::key_code`]).
    fn process(&mut self, keycode: xkb::Keycode, pressed: bool) -> KeyOutcome {
        // Read the produced keysym with the *current* modifier state, then
        // advance for this key — a held Shift is already uppercased since Shift-down updated state first; a modifier key's sym is unaffected by ordering.
        let sym = self.state.key_get_one_sym(keycode);
        let direction = if pressed {
            xkb::KeyDirection::Down
        } else {
            xkb::KeyDirection::Up
        };
        let _ = self.state.update_key(keycode, direction);

        let new_mods = self.current_mods();
        let modifiers_changed = if new_mods == self.mods {
            None
        } else {
            self.mods = new_mods;
            Some(Modifiers::from(new_mods))
        };

        // Derive the logical key from the *keysym* (not UTF-8 output) so chords like Ctrl+K resolve to `Character("k")`, not the control character.
        let input = keysym_to_winit_key(sym.raw()).map(|logical| {
            let text = text_for(sym.key_char(), new_mods);
            keyboard_input_event(logical, text, pressed, keycode.raw().saturating_sub(8))
        });

        KeyOutcome {
            input,
            modifiers_changed,
        }
    }
}

/// Map an xkb keysym to a winit logical [`Key`]: special keys become
/// `Key::Named`, printable Unicode becomes `Key::Character`; bare modifiers and unmapped keys return `None` (carried by `ModifiersChanged` instead).
fn keysym_to_winit_key(raw: u32) -> Option<Key> {
    // Keep the function row intact. These are not printable keysyms, so they
    // must be named explicitly rather than falling through to `key_char()`.
    let function = match raw {
        keysyms::KEY_F1 => Some(NamedKey::F1),
        keysyms::KEY_F2 => Some(NamedKey::F2),
        keysyms::KEY_F3 => Some(NamedKey::F3),
        keysyms::KEY_F4 => Some(NamedKey::F4),
        keysyms::KEY_F5 => Some(NamedKey::F5),
        keysyms::KEY_F6 => Some(NamedKey::F6),
        keysyms::KEY_F7 => Some(NamedKey::F7),
        keysyms::KEY_F8 => Some(NamedKey::F8),
        keysyms::KEY_F9 => Some(NamedKey::F9),
        keysyms::KEY_F10 => Some(NamedKey::F10),
        keysyms::KEY_F11 => Some(NamedKey::F11),
        keysyms::KEY_F12 => Some(NamedKey::F12),
        keysyms::KEY_F13 => Some(NamedKey::F13),
        keysyms::KEY_F14 => Some(NamedKey::F14),
        keysyms::KEY_F15 => Some(NamedKey::F15),
        keysyms::KEY_F16 => Some(NamedKey::F16),
        keysyms::KEY_F17 => Some(NamedKey::F17),
        keysyms::KEY_F18 => Some(NamedKey::F18),
        keysyms::KEY_F19 => Some(NamedKey::F19),
        keysyms::KEY_F20 => Some(NamedKey::F20),
        keysyms::KEY_F21 => Some(NamedKey::F21),
        keysyms::KEY_F22 => Some(NamedKey::F22),
        keysyms::KEY_F23 => Some(NamedKey::F23),
        keysyms::KEY_F24 => Some(NamedKey::F24),
        _ => None,
    };
    if let Some(function) = function {
        return Some(Key::Named(function));
    }
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
        keysyms::KEY_Caps_Lock => Some(NamedKey::CapsLock),
        keysyms::KEY_Num_Lock => Some(NamedKey::NumLock),
        keysyms::KEY_Scroll_Lock => Some(NamedKey::ScrollLock),
        keysyms::KEY_Insert => Some(NamedKey::Insert),
        keysyms::KEY_Menu => Some(NamedKey::ContextMenu),
        keysyms::KEY_Multi_key => Some(NamedKey::Compose),
        keysyms::KEY_ISO_Level3_Shift => Some(NamedKey::AltGraph),
        keysyms::KEY_ISO_Next_Group => Some(NamedKey::GroupNext),
        keysyms::KEY_ISO_Prev_Group => Some(NamedKey::GroupPrevious),
        keysyms::KEY_Pause => Some(NamedKey::Pause),
        keysyms::KEY_Help => Some(NamedKey::Help),
        keysyms::KEY_Print => Some(NamedKey::PrintScreen),
        keysyms::KEY_Shift_L | keysyms::KEY_Shift_R => Some(NamedKey::Shift),
        keysyms::KEY_Control_L | keysyms::KEY_Control_R => Some(NamedKey::Control),
        keysyms::KEY_Alt_L | keysyms::KEY_Alt_R => Some(NamedKey::Alt),
        keysyms::KEY_Super_L | keysyms::KEY_Super_R => Some(NamedKey::Super),
        // The function row. Without these, media/brightness keys reach `key_char()` (no character for them) and the press is dropped before any handler sees it — why volume-up did nothing on real hardware.
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
    if let Some(c) = Keysym::new(raw).key_char()
        && !c.is_control()
    {
        return Some(Key::Character(SmolStr::new(c.to_string())));
    }
    // Never drop an otherwise valid physical key merely because this adapter
    // does not have a named-key spelling for its keysym. The seat still must
    // receive the raw keycode (IME/vendor keys included), and clients can
    // interpret the xkb keymap they were sent. Returning an unidentified
    // logical key preserves that protocol event while keeping it out of shell
    // chord matching.
    Some(Key::Unidentified(NativeKey::Xkb(raw)))
}

/// The typed text for a key event, or `None`. Suppressed under Ctrl/Super (chords, not text) and for control characters — matching winit, keeping control bytes out of text inputs.
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
/// in `compositor::native::keyboard_input_event`/`overlay_host::key_press`: winit's `pub(crate)` `platform_specific` field tolerates the zeroed struct.
fn keyboard_input_event(
    logical_key: Key,
    text: Option<SmolStr>,
    pressed: bool,
    evdev_keycode: u32,
) -> WindowEvent {
    // SAFETY: every public field of `KeyEvent` is overwritten below, and
    // the only non-public field (`platform_specific: KeyEventExtra`)
    // tolerates the all-zero bit pattern — the same invariant the two
    // existing `KeyEvent`-building helpers in this crate rely on.
    let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
    ev.logical_key = logical_key;
    ev.physical_key = PhysicalKey::from_scancode(evdev_keycode);
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
/// `DrmDevice` is built, its resource handles read, and each connector
/// queried (`force_probe = true` for fresh EDID/modes at boot). Failures
/// on individual devices/connectors are **logged and skipped** — a single misbehaving GPU shouldn't down the whole compositor.
///
/// Returns the flattened connector list for [`OutputRegistry::populate`].
///
/// **Side-effect**: temporarily holds DRM master on each opened device;
/// fds drop at function end, releasing master before page-flip wiring.
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

/// Open a single `cardN` node through the session, build a `DrmDevice`, and project each connector into [`ConnectorInfo`].
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

    // O_RDWR|O_CLOEXEC|O_NONBLOCK matches what smithay's anvil example uses for the DRM device fd.
    let fd = session
        .open(
            path,
            OFlags::RDWR | OFlags::CLOEXEC | OFlags::NOCTTY | OFlags::NONBLOCK,
        )
        .map_err(|e| format!("session.open({}): errno={:?}", path.display(), e.as_errno()))?;
    let device_fd = DrmDeviceFd::new(DeviceFd::from(fd));
    let dev_id_raw: u64 = device_fd.dev_id().map_err(|e| format!("dev_id: {e}"))?;

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
                            // `vrefresh()` returns plain Hz; multiply by 1000 to get milli-Hz (mode_info's canonical unit so 59.94 vs 60.00 stay distinguishable when fractional rates arrive upstream).
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
#[allow(
    clippy::float_cmp,
    reason = "deterministic output geometry fixtures use exactly representable values"
)]
mod tests {
    use super::*;
    use crate::compositor::backend::drm::devices::ModeInfo;

    fn test_keyboard_settings() -> KeyboardSettings {
        KeyboardSettings::default()
    }

    // These tests build a `DrmDispatchState` directly; they avoid calling `DrmRuntime::new` which would try to open a real seat.

    /// Pure-data stub `Session` for unit tests — always fails to open any
    /// path (real seat handling needs a TTY), but the failure path itself must be verified: `enumerate_connectors` must log + skip, not crash.
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
        // `BackendEvent::Input` carries the actual `WindowEvent`; synthesise a minimal CursorMoved so the variant constructs.
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
        // The stub session refuses every open(), so even with paths supplied, we must end up with zero connectors and a warn-log, not a panic.
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
    /// implements when projecting DRM modes into `ModeInfo`s: the
    /// `preferred` flag is honored. The full flow needs a real DRM device,
    /// so we verify the *policy* end-to-end via `super::devices::preferred_mode`
    /// instead — pinning the contract that a preferred mode in the list is the one picked.
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

    /// And when none of the modes are flagged preferred, the policy falls back to the highest resolution × refresh, matching `enumerate_connectors_on_device`.
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

    // Keyboard: xkb keycode → winit key translation

    /// Pure mapping: named special keys and printable characters resolve to the winit `Key` the chrome matches on — no real keymap needed, mirroring the `preferred_mode`/pointer-helper tests.
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
        assert_eq!(
            keysym_to_winit_key(ks::KEY_Print),
            Some(Key::Named(NamedKey::PrintScreen))
        );
        // Letter keysyms carry case from the keysym itself (US 'a' vs 'A').
        match keysym_to_winit_key(ks::KEY_a) {
            Some(Key::Character(s)) => assert_eq!(s.as_str(), "a"),
            other => panic!("KEY_a mapped to {other:?}"),
        }
        match keysym_to_winit_key(ks::KEY_A) {
            Some(Key::Character(s)) => assert_eq!(s.as_str(), "A"),
            other => panic!("KEY_A mapped to {other:?}"),
        }
        assert_eq!(
            keysym_to_winit_key(ks::KEY_Control_L),
            Some(Key::Named(NamedKey::Control))
        );
        assert_eq!(
            keysym_to_winit_key(ks::KEY_Shift_L),
            Some(Key::Named(NamedKey::Shift))
        );
        assert_eq!(
            keysym_to_winit_key(ks::KEY_F11),
            Some(Key::Named(NamedKey::F11))
        );
        assert_eq!(
            keysym_to_winit_key(ks::KEY_Caps_Lock),
            Some(Key::Named(NamedKey::CapsLock))
        );
        assert_eq!(
            keysym_to_winit_key(ks::KEY_Multi_key),
            Some(Key::Named(NamedKey::Compose))
        );
        assert_eq!(
            keysym_to_winit_key(ks::KEY_ISO_Level3_Shift),
            Some(Key::Named(NamedKey::AltGraph))
        );
        assert!(matches!(
            keysym_to_winit_key(0xffff_0bad),
            Some(Key::Unidentified(NativeKey::Xkb(0xffff_0bad)))
        ));
    }

    /// The function row has to survive the keysym translation — these keysyms have no character, so anything not in this table falls through `key_char()` and is dropped before a handler runs.
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

    /// Text is produced for plain printable keys, suppressed under Ctrl (chords) and for control characters.
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

    /// End-to-end through a real keymap: pressing/releasing Left Shift flips the modifier mask — layout-independent; soft-skips if no xkb data installed.
    #[test]
    fn xkb_left_shift_toggles_modifier_mask() {
        let Some(mut kbd) = KeyboardXkb::from_settings(&test_keyboard_settings()) else {
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
        let WindowEvent::KeyboardInput { event, .. } = down.input.expect("modifier key input")
        else {
            panic!("expected keyboard input")
        };
        assert_eq!(event.logical_key, Key::Named(NamedKey::Shift));
        assert_eq!(event.physical_key.to_scancode(), Some(42));

        let up = kbd.process(shift, false);
        let m = up
            .modifiers_changed
            .expect("shift release should flip the modifier mask");
        assert!(!m.state().shift_key(), "left shift up -> shift cleared");
    }

    /// End-to-end: an alpha-row keycode yields a `Character` `KeyboardInput` carrying text — QWERTY 'a' is a letter on every Latin layout, so layout-independent; soft-skips without xkb data.
    #[test]
    fn xkb_alpha_key_produces_character_input() {
        let Some(mut kbd) = KeyboardXkb::from_settings(&test_keyboard_settings()) else {
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
                assert_eq!(event.physical_key.to_scancode(), Some(30));
            }
            other => panic!("expected KeyboardInput, got {other:?}"),
        }
    }

    #[test]
    fn xkb_uses_the_persisted_active_group_without_reordering_sources() {
        let settings = KeyboardSettings {
            source_ids: vec!["us".into(), "de".into()],
            layouts: "us,de".into(),
            variants: ",".into(),
            options: Some("grp:alt_shift_toggle".into()),
            active_index: 1,
        };
        let Some(mut keyboard) = KeyboardXkb::from_settings(&settings) else {
            eprintln!("xkb keymap unavailable; skipping");
            return;
        };
        // AD06 is Y in US and Z in German.
        let output = keyboard.process(xkb::Keycode::new(29), true);
        let WindowEvent::KeyboardInput { event, .. } = output.input.expect("key input") else {
            panic!("expected keyboard input")
        };
        assert_eq!(event.logical_key, Key::Character("z".into()));
        assert_eq!(event.physical_key.to_scancode(), Some(21));
    }

    /// Exercise the exact German symbols and Shift handling that failed in
    /// Electron/Xwayland during the host-session trial. This runs through the
    /// production xkb translator using physical evdev positions, not a
    /// pre-translated character fixture.
    #[test]
    fn german_xkb_shift_and_punctuation_keep_physical_positions() {
        let settings = KeyboardSettings {
            source_ids: vec!["de".into()],
            layouts: "de".into(),
            variants: String::new(),
            options: None,
            active_index: 0,
        };
        let Some(mut keyboard) = KeyboardXkb::from_settings(&settings) else {
            eprintln!("German xkb keymap unavailable; skipping");
            return;
        };
        let character = |outcome: KeyOutcome| {
            let WindowEvent::KeyboardInput { event, .. } = outcome.input.expect("key input") else {
                panic!("expected keyboard input")
            };
            let Key::Character(value) = event.logical_key else {
                panic!("expected character key, got {:?}", event.logical_key)
            };
            (
                value.to_string(),
                event.text.map(|text| text.to_string()),
                event.physical_key,
            )
        };

        // German key left of Backspace: ß unshifted, ? shifted (evdev 12).
        let eszett = xkb::Keycode::new(20);
        let (plain, text, physical) = character(keyboard.process(eszett, true));
        assert_eq!((plain.as_str(), text.as_deref()), ("ß", Some("ß")));
        assert_eq!(physical.to_scancode(), Some(12));
        let _ = keyboard.process(eszett, false);

        let shift = xkb::Keycode::new(50); // evdev KEY_LEFTSHIFT + 8
        let down = keyboard.process(shift, true);
        assert!(
            down.modifiers_changed
                .expect("Shift must update modifiers")
                .state()
                .shift_key()
        );
        let (question, text, physical) = character(keyboard.process(eszett, true));
        assert_eq!((question.as_str(), text.as_deref()), ("?", Some("?")));
        assert_eq!(physical.to_scancode(), Some(12));
        let _ = keyboard.process(eszett, false);

        // German hash/apostrophe position (evdev 43) and uppercase letter.
        let apostrophe = xkb::Keycode::new(51);
        let (value, text, physical) = character(keyboard.process(apostrophe, true));
        assert_eq!((value.as_str(), text.as_deref()), ("'", Some("'")));
        assert_eq!(physical.to_scancode(), Some(43));
        let _ = keyboard.process(apostrophe, false);
        let a = xkb::Keycode::new(38);
        let (upper, text, physical) = character(keyboard.process(a, true));
        assert_eq!((upper.as_str(), text.as_deref()), ("A", Some("A")));
        assert_eq!(physical.to_scancode(), Some(30));
        let _ = keyboard.process(a, false);
        let up = keyboard.process(shift, false);
        assert!(
            !up.modifiers_changed
                .expect("Shift release must update modifiers")
                .state()
                .shift_key()
        );
    }

    #[test]
    fn xkb_alt_shift_cycles_configured_sources() {
        let settings = KeyboardSettings {
            source_ids: vec!["us".into(), "de".into()],
            layouts: "us,de".into(),
            variants: ",".into(),
            options: Some("grp:alt_shift_toggle".into()),
            active_index: 0,
        };
        let Some(mut keyboard) = KeyboardXkb::from_settings(&settings) else {
            eprintln!("xkb keymap unavailable; skipping");
            return;
        };
        // evdev Left Alt 56 / Left Shift 42, each plus the XKB offset 8.
        for (keycode, pressed) in [(64, true), (50, true), (50, false), (64, false)] {
            let _ = keyboard.process(xkb::Keycode::new(keycode), pressed);
        }
        let output = keyboard.process(xkb::Keycode::new(29), true);
        let WindowEvent::KeyboardInput { event, .. } = output.input.expect("key input") else {
            panic!("expected keyboard input")
        };
        assert_eq!(event.logical_key, Key::Character("z".into()));
    }

    #[test]
    fn xkb_german_nodeadkeys_preserves_shifted_symbols_and_physical_codes() {
        let settings = KeyboardSettings {
            source_ids: vec!["de:nodeadkeys".into()],
            layouts: "de".into(),
            variants: "nodeadkeys".into(),
            options: None,
            active_index: 0,
        };
        let Some(mut keyboard) = KeyboardXkb::from_settings(&settings) else {
            eprintln!("xkb keymap unavailable; skipping");
            return;
        };
        // Hold the physical left Shift (evdev 42, xkb 50), then use the
        // physical German `#` key (evdev 43) for apostrophe and the `ß` key
        // (evdev 12) for question mark. These are the symbols that exposed
        // the old logical-US fallback in Electron clients.
        let _ = keyboard.process(xkb::Keycode::new(50), true);
        let apostrophe = keyboard
            .process(xkb::Keycode::new(51), true)
            .input
            .expect("German apostrophe key should produce input");
        let question = keyboard
            .process(xkb::Keycode::new(20), true)
            .input
            .expect("German question-mark key should produce input");
        let logical_text = |event: WindowEvent| {
            let WindowEvent::KeyboardInput { event, .. } = event else {
                panic!("expected a keyboard input")
            };
            (
                event.logical_key,
                event.text,
                event.physical_key.to_scancode(),
            )
        };
        assert_eq!(
            logical_text(apostrophe),
            (Key::Character("'".into()), Some("'".into()), Some(43))
        );
        assert_eq!(
            logical_text(question),
            (Key::Character("?".into()), Some("?".into()), Some(12))
        );
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

    // Hotplug + session reconciliation, on the production methods
    //
    // `rescan_outputs` and `adopt_builds` need a real card (they modeset), but
    // everything they do *after* the ioctls — re-layout, scale derivation,
    // pointer clamping, the output table, and the commands sent to the
    // scanout owner — is reachable without one. These drive `DrmDispatchState`'s own methods rather than re-deriving the policy in the test.

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
            connector_name: format!("TEST-{connector}"),
            size,
            physical_mm: mm,
            // Test ids are always small literals; `saturating_add` just
            // avoids a raw `+` clippy can't see is bounded.
            crtc: crtc(id.saturating_add(1)),
            position: (0, 0),
            scale: 1.0,
            transform: settings_store::hardware::OutputTransform::Normal,
            refresh: None,
        }
    }

    /// A dispatch state with a command channel attached, plus the receiving end so a test can assert on what was sent.
    fn dispatch_with_channel() -> (DrmDispatchState, Receiver<ScanoutCommand>) {
        let (tx, rx) = channel();
        let state = DrmDispatchState {
            commands: Some(tx),
            ..Default::default()
        };
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
        // The concrete "windows stranded off-screen" symptom that is actually load-bearing: windows re-flow (not pinned to an output), but the pointer lives in absolute combined coordinates and would be left on a gone panel.
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
        // Leaving the survivor at x=1920 would open a 1920px hole in the combined space that no panel displays, and every click into it would land on nothing.
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
        // A laptop docked to a desktop monitor: one dense panel, one sparse — a single session-wide scale is wrong for one of them.
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
        // The end-to-end of what the session state machine decided: the scanout owner is told to stop before anything else happens.
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
        // Coming back from a VT switch must restore the power state the session had, not light a screen the user had turned off.
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
        // A suspend never pauses the seat, so nothing marks the session inactive — a handler that only acts on transitions does nothing at all, which is the black-screen-after-lid-open bug.
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
        // No output came up, so there is nobody to send commands to — performing the sequence anyway must not panic.
        let mut state = DrmDispatchState::default();
        let session = SessionState::new_active();
        state.session = Some(session.clone());
        state.perform_all(session.apply_with_actions(SeatTick::Pause));
        state.perform_all(session.apply_with_actions(SeatTick::Activate));
    }

    // Gesture routing, on the production `handle_gesture`

    #[test]
    fn a_three_finger_swipe_becomes_a_shell_gesture_and_never_reaches_a_client() {
        let mut state = DrmDispatchState::default();
        state.handle_gesture(GestureEvent::SwipeBegin { fingers: 3 });
        state.handle_gesture(GestureEvent::SwipeUpdate {
            dx: 0.0,
            dy: -150.0,
        });
        state.handle_gesture(GestureEvent::SwipeEnd { cancelled: false });

        let shell: Vec<_> = state
            .events
            .iter()
            .filter_map(|e| match e {
                BackendEvent::ShellGesture(a) => Some(*a),
                _ => None,
            })
            .collect();
        assert_eq!(shell, vec![GestureAction::SwitchWorkspace(-1)]);
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
        // Ownership is read from the tracker, and `end` clears it — so deciding after the mutation sends a client a `swipe_end` for a swipe it never saw begin, a protocol violation.
        let mut state = DrmDispatchState::default();
        for ev in [
            GestureEvent::SwipeBegin { fingers: 4 },
            GestureEvent::SwipeUpdate {
                dx: 0.0,
                dy: -150.0,
            },
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
        // A hold carries its finger count only on `begin` too, and has no shell meaning — a three-finger hold must be swallowed whole, not half-delivered.
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
        // Browser zoom — `zwp_pointer_gestures_v1` has been advertised all along with nothing ever emitted into it.
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
        state.handle_gesture(GestureEvent::SwipeUpdate {
            dx: 0.0,
            dy: -150.0,
        });
        assert!(
            state.events.iter().any(|e| matches!(
                e,
                BackendEvent::ShellGesture(GestureAction::SwitchWorkspace(1))
            )),
            "the same preference that inverts scrolling inverts gestures"
        );
    }

    #[test]
    fn drm_button_mapping_preserves_back_and_forward() {
        assert_eq!(map_mouse_button(0x110), Some(MouseButton::Left));
        assert_eq!(map_mouse_button(0x111), Some(MouseButton::Right));
        assert_eq!(map_mouse_button(0x112), Some(MouseButton::Middle));
        assert_eq!(map_mouse_button(0x115), Some(MouseButton::Forward));
        assert_eq!(map_mouse_button(0x116), Some(MouseButton::Back));
        assert_eq!(map_mouse_button(0x113), None);
    }

    #[test]
    fn touchpad_scroll_keeps_pixel_precision_and_sign() {
        let WindowEvent::MouseWheel { delta, .. } =
            mouse_wheel_event(AxisSource::Finger, 1.25, -2.5)
        else {
            panic!("expected a mouse-wheel event")
        };
        assert_eq!(
            delta,
            MouseScrollDelta::PixelDelta(PhysicalPosition::new(-1.25, 2.5))
        );
    }

    #[test]
    fn wheel_scroll_uses_line_units_without_losing_high_resolution_steps() {
        let WindowEvent::MouseWheel { delta, .. } =
            mouse_wheel_event(AxisSource::Wheel, 7.5, -30.0)
        else {
            panic!("expected a mouse-wheel event")
        };
        assert_eq!(delta, MouseScrollDelta::LineDelta(-0.5, 2.0));
    }

    #[test]
    fn key_repeat_waits_for_delay_ticks_at_rate_and_stops_on_release() {
        let start = std::time::Instant::now();
        let key = xkb::Keycode::new(38); // evdev KEY_A + 8
        let mut repeat = KeyRepeat::default();
        repeat.press(key, start, std::time::Duration::from_millis(300), 20);
        assert!(
            repeat
                .due(start + std::time::Duration::from_millis(299))
                .is_empty()
        );
        assert_eq!(
            repeat.due(start + std::time::Duration::from_millis(300)),
            [key]
        );
        assert_eq!(
            repeat.due(start + std::time::Duration::from_millis(401)),
            [key, key]
        );
        repeat.release(key);
        assert!(
            repeat
                .due(start + std::time::Duration::from_secs(1))
                .is_empty()
        );
    }

    #[test]
    fn drm_repeat_event_matches_winit_contract_without_repressing_xkb() {
        let Some(mut keyboard) = KeyboardXkb::from_settings(&KeyboardSettings::default()) else {
            return;
        };
        let key = xkb::Keycode::new(38); // evdev KEY_A + 8
        assert!(keyboard.key_repeats(key));
        let _ = keyboard.process(key, true);
        let WindowEvent::KeyboardInput { event, .. } =
            keyboard.repeat_event(key).expect("A has a repeat event")
        else {
            panic!("repeat must use the keyboard-input wire")
        };
        assert_eq!(event.logical_key, Key::Character("a".into()));
        assert_eq!(event.state, ElementState::Pressed);
        assert!(
            event.repeat,
            "native widgets distinguish the synthetic edge"
        );
        let _ = keyboard.process(key, false);
    }

    // libinput device configuration, on the production plan

    #[test]
    fn a_touchpad_plan_turns_tapping_on_by_default() {
        // The applier is one `config_*` call per `Some` in the plan, so the plan is what decides what a laptop touchpad comes up as.
        let state = DrmDispatchState::default();
        let caps = DeviceCaps {
            supports_tap: true,
            supports_natural_scroll: true,
            supports_accel: true,
            supports_dwt: true,
            supports_middle_emulation: true,
            is_pointer: true,
            supports_left_handed: true,
            supports_flat_acceleration: true,
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
