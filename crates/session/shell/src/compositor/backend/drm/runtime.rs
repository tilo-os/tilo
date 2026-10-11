//! Hardware-touching glue for the DRM backend.
//!
//! Only calls needing a real seat and hardware live here — opening the
//! seat, probing connectors, modesetting, libinput ioctls. Every decision
//! is made in a pure sibling module instead ([`super::hotplug`],
//! [`super::session`], [`super::scale`], [`super::input_config`],
//! [`super::gestures`]), which is what keeps them testable — none of this
//! runs under `cargo test`. `DrmRuntime::new` fails cleanly with `Err` rather than panicking when there is no seat.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, SyncSender, channel, sync_channel};
use std::sync::{Arc, Mutex};

use smithay::backend::drm::{DrmDevice, DrmDeviceFd};
use smithay::backend::input::{
    AbsolutePositionEvent, Axis, AxisSource, ButtonState, Device as InputDevice,
    Event as InputEventTrait, GestureBeginEvent, GestureEndEvent, GesturePinchUpdateEvent,
    GestureSwipeUpdateEvent, InputEvent, KeyState, KeyboardKeyEvent, PointerAxisEvent,
    PointerButtonEvent, PointerMotionEvent, TouchEvent, TouchSlot,
};
use smithay::backend::libinput::{LibinputInputBackend, LibinputSessionInterface};
use tilo_clock::unix_now_ms;
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
    ElementState, Modifiers, MouseButton, MouseScrollDelta, Touch, TouchPhase, WindowEvent,
};
use winit::keyboard::{Key, ModifiersState, NamedKey, NativeKey, PhysicalKey, SmolStr};
use winit::platform::scancode::PhysicalKeyExtScancode as _;

use super::access_keys::{
    BounceKeys, BounceVerdict, ButtonEdge, MouseKeys, MouseKeysOutput, PointerButton,
};
use super::devices::{ModeInfo, filter_kms_nodes, is_kms_node, order_by_connected_connectors};
use super::direct_retry::DirectDegradation;
#[cfg(test)]
use super::gestures::GestureAction;
use super::gestures::{GestureConfig, GestureTracker};
use super::hotplug::{self, ConnectedOutput, LiveOutput};
use super::input_config;
use super::input_config::{
    AccessibilityInputSettings, DeviceCaps, InputSettings, KeyboardSettings,
};
use super::output_signal::{HdrRequest, OutputSignalStatus};
use super::outputs::{ConnectorInfo, DrmOutputKey, OutputRegistry};
use super::scale::resolve_scale;
use super::scanout::{
    CardHandle, ConnectorKeyCandidate, ConnectorSnapshot, CursorPlanes, DirectScanoutTargets,
    DrmScanout, ScanoutBuild, device_id_from_dev_t, discover_icc_profiles, is_builtin_connector,
    physical_card_identity, resolve_connector_config_keys,
};
use super::session::{SeatTick, SessionAction, SessionState};
use crate::compositor::backend::layout;
use crate::compositor::backend::{
    BackendEvent, GestureEvent, Output, OutputId, OutputIdentity, PointerConstraintRegion,
    TabletPhase, TouchEvent as TiloTouchEvent,
};
use crate::shell::render::icc_lut::IccLut;

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
    fn next_deadline(&self) -> Option<std::time::Instant> {
        self.held.as_ref().map(|held| held.next)
    }

    fn press(
        &mut self,
        keycode: xkb::Keycode,
        now: std::time::Instant,
        delay: std::time::Duration,
        rate: u32,
    ) {
        // `wl_keyboard.repeat_info` uses rate 0 to mean disabled. Keep the
        // in-process/native path on the same contract: coercing it to 1 Hz
        // would make a deliberately disabled Backspace repeat once a second.
        if rate == 0 {
            self.cancel();
            return;
        }
        self.held = Some(HeldRepeat {
            keycode,
            next: now.checked_add(delay).unwrap_or(now),
            interval: std::time::Duration::from_secs_f64(1.0 / f64::from(rate)),
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
    /// Every KMS card this seat can use, held open on the loop thread for
    /// the entire session.  A card owns the CRTCs for only its own
    /// connectors, so retaining just the startup winner strands connectors
    /// on a hybrid GPU or dock.  The map key is the card's `dev_t`, the same
    /// device component used in [`DrmOutputKey`].
    pub cards: BTreeMap<u64, CardHandle>,
    /// A clone of the active libseat session used only to acquire a card
    /// announced after startup.  The runtime keeps the original alive; this
    /// clone lets the udev callback add a KMS device without a side channel.
    seat: Option<LibSeatSession>,
    /// Cards no longer eligible for scanout, but whose original libseat fd
    /// cannot be closed until the presenter has dropped its duplicate scanout
    /// fds.  A udev `Removed` therefore moves a card here instead of simply
    /// dropping it from [`Self::cards`].
    retiring_cards: BTreeMap<u64, CardHandle>,
    /// A card can reappear before the presenter acknowledges removal of its
    /// former scanouts.  Retain its `Added` path and open it only after the
    /// original libseat device was closed, avoiding a forbidden second
    /// `Session::open` on one card node.
    deferred_card_adds: BTreeMap<u64, (u64, PathBuf)>,
    /// Presenter acknowledgements for card teardown.  The sender travels in
    /// the reconfigure command; the receiver is intentionally loop-thread
    /// owned because `LibSeatSession` is not sent to the render thread.
    card_removal_ack_sender: Option<Sender<u64>>,
    card_removal_acks: Option<Receiver<u64>>,
    /// A failed `Session::close` leaves libseat's device state uncertain.
    /// Do not guess by opening that path again in this session; only a fresh
    /// compositor/libseat session may safely recover it.
    failed_card_close_paths: BTreeSet<PathBuf>,
    /// Same fail-closed guard for a physical card which re-enumerates under
    /// another `cardN` path and `dev_t` before a session restart.
    failed_card_close_identities: BTreeSet<String>,
    /// Without a stable physical identity, no later card node can be proven
    /// distinct from the failed lease. Block all new opens until restart.
    unidentified_card_close_failure: bool,
    /// Pure ownership state mirroring `cards`/`retiring_cards`; it makes the
    /// remove → presenter ack → libseat close → deferred re-add ordering
    /// explicit and independently testable.
    card_leases: CardLeaseLifecycle,
    /// Profiles already parsed by the ICC worker and proven capable of
    /// producing an RGBA8 output transform. Capability publication reads only
    /// this cache, so directory/profile I/O never stalls the compositor loop.
    pub color_profiles: BTreeMap<String, String>,
    /// Every output currently being driven, in `OutputId` order — mirrors what the presenter owns (scanouts live on the render thread).
    pub live: Vec<LiveEntry>,
    /// Every physically connected output from the previous rescan, including
    /// outputs disabled in persistent intent. This is intentionally separate
    /// from `live`: a disabled dock connector still needs a fresh Settings
    /// capability snapshot when it is plugged or removed.
    capability_topology: BTreeSet<DrmOutputKey>,
    /// Every card's connectors as the last forced probe (startup or a udev
    /// hotplug) found them, before display intent is applied. A settings
    /// change and capability publication re-apply intent to these rather
    /// than force-probing every connector on the input thread.
    connector_probes: BTreeMap<u64, Vec<ConnectorSnapshot>>,
    /// How many times connectors were force-probed, so tests can assert
    /// which production paths reach the hardware.
    #[cfg(test)]
    forced_probes: usize,
    /// Channel to whoever owns the scanouts — the presenter once split off, the backend itself before that.
    pub commands: Option<Sender<ScanoutCommand>>,
    /// The persisted pointer/keyboard preferences, applied to every libinput device as it appears.
    pub input: InputSettings,
    pub accessibility: AccessibilityInputSettings,
    /// Slow-keys presses are accepted only after being held for the delay.
    pub slow_pending: HashMap<xkb::Keycode, std::time::Instant>,
    /// Sticky modifiers stay logically down through the next non-modifier.
    pub sticky_latched: ModifiersState,
    /// Swallows contact bounce ahead of every other key handling.
    bounce_keys: BounceKeys,
    /// The numeric keypad as a pointer, while mouse keys is on.
    mouse_keys: MouseKeys,
    pub physical_modifiers: ModifiersState,
    /// Winit synthesises repeat presses for the hosted backend. Bare libinput
    /// only supplies key edges, so the DRM adapter mirrors that established
    /// event contract for Tilo-native widgets. Wayland clients use the seat's
    /// `repeat_info` and never receive these synthetic edges.
    key_repeat: KeyRepeat,
    /// Every libinput device currently attached — libinput has no "list
    /// your devices" call, so keeping handles (cheap refcounted clones) is the only way to re-apply a changed setting to hardware already plugged in.
    pub devices: Vec<smithay::reexports::input::Device>,
    /// Libinput's optional per-touch-device output binding, keyed by its
    /// short-lived device id. Connector names, rather than [`OutputId`]s,
    /// survive an output being unplugged and later reconnected; each touch
    /// event resolves the name against the current layout.
    touch_output_bindings: HashMap<String, Option<String>>,
    /// `display.scale` from the settings store, when the user has set one — overrides the per-output derivation.
    pub scale_override: Option<f64>,
    /// Accumulates touchpad swipes and pinches into shell navigation.
    pub gestures: GestureTracker,
    /// Arbitration for direct touchscreen contacts. A stream begins as client
    /// touch, but the third simultaneous contact transfers it to shell
    /// gestures and cancels the client stream that began before that upgrade.
    direct_touch: DirectTouchOwnership,
    /// Class of the last physical input reported as [`BackendEvent::InputUsed`],
    /// so a stream of key presses or pointer motion reports it once.
    last_input_used: Option<ui::InputModality>,
    /// Set whenever the output set changed, so the backend refreshes its cached list and the compositor re-syncs its `wl_output` globals.
    pub outputs_changed: bool,
    /// Per-device output metadata, keyed by the public `OutputId`.
    pub output_meta: HashMap<OutputId, Output>,
    /// The capability snapshot last published for Settings, kept so
    /// output-management heads never force-probe connectors themselves.
    pub display_capabilities: Vec<settings_store::hardware::DisplayOutputSnapshot>,
    /// The built-in panel's orientation from the accelerometer, overriding
    /// its configured transform while set. `None` on machines without one.
    pub device_rotation: Option<settings_store::hardware::OutputTransform>,
    /// Current pointer position in scene coordinates (physical pixels of
    /// the combined texture): where the cursor is drawn and what the shell
    /// hit-tests. Always derived from [`Self::logical_pointer`].
    pub pointer: (f64, f64),
    /// Current pointer position in the logical layout, where motion
    /// happens: one unit of relative motion is one logical pixel on every
    /// output, and crossing between outputs of different scales keeps the
    /// logical row.
    pub logical_pointer: (f64, f64),
    /// Primary output size (px) — needed to transform libinput's absolute (0..1) coordinates and to clamp relative motion.
    pub output_size: (i32, i32),
    /// The monitors attached at the last probe — which saved layout applies.
    pub display_set: settings_store::hardware::DisplaySet,
    /// Hardware-cursor multiplexer across all outputs — one plane per
    /// output whose KMS cursor plane is active. libinput motion handlers
    /// call `move_to(global_x, global_y)`, and [`CursorPlanes`] routes to
    /// the plane of the output the pointer is over (hiding the one it left),
    /// all at input rate with no framebuffer redraw. Empty → software cursor from `pointer`.
    pub hw_cursor: CursorPlanes,
    /// What the pointer shows, applied to every scanout (including ones a
    /// hotplug adds later).
    pub pointer_image: crate::compositor::cursor::PointerImage,
    /// Where local motion may take the pointer while a client locks or
    /// confines it.
    pub pointer_constraint: crate::compositor::backend::PointerConstraintRegion,
    /// Drawing tablets: their saved settings, the shortcut table their
    /// buttons press, their pads and where each pad's rings and strips are.
    pub tablets: TabletRuntime,
    /// Per-user xkb keymap + state translating libinput keycodes into winit
    /// `KeyboardInput` events (`FrameBuilder::dispatch_event`'s wire); `None`
    /// when the keymap couldn't compile, running pointer-only. The persisted
    /// source list owns the session; installer-seeded XKB environment remains
    /// only the default before a preference exists.
    pub keyboard: Option<KeyboardXkb>,
    keyboard_device: Option<String>,
    keyboard_config: settings_store::hardware::RichInputConfig,
    keyboard_cache: BTreeMap<String, KeyboardXkb>,
    keyboard_identities: BTreeMap<String, String>,
    keyboard_key_owners: BTreeMap<u32, BTreeSet<String>>,
    /// Serialises KMS access between the render thread and session ownership
    /// changes. A pause command is queued first, but the render thread may
    /// already be inside a page flip when the libseat callback returns; the
    /// callback must not drop DRM master until that presentation has retired.
    pub present_gate: Option<Arc<Mutex<()>>>,
    /// The output showing a fullscreen client, which is where adaptive sync
    /// may engage.
    pub fullscreen_output: Option<OutputId>,
    /// Runtime signalling facts from the presenter, keyed by the output's
    /// configuration key so they survive a replug of the same connector.
    pub signal_status: BTreeMap<String, OutputSignalStatus>,
    /// Receiving end of the presenter's signalling reports.
    pub signal_reports: Option<Receiver<SignalReport>>,
    /// What each output's primary plane offers fullscreen clients, as the
    /// presenter last learned it ([`super::client_scanout::ClientPlanes`]).
    pub client_planes: super::client_scanout::ClientPlanes,
}

/// What the DRM backend keeps for drawing tablets.
#[derive(Default)]
pub struct TabletRuntime {
    /// Per-tablet settings, keyed by device identity.
    pub configs: BTreeMap<String, settings_store::hardware::TabletConfig>,
    /// The shortcut table, so a pen or pad button presses the chord the user
    /// bound to its shortcut.
    pub shortcuts: settings_store::hardware::ShortcutConfig,
    /// Pads opened a second time, for their buttons, rings and strips.
    pub pads: Option<PadInput>,
    /// Ring and strip travel not yet turned into a whole step.
    pub dials: crate::compositor::tablet_pad::DialSteps,
}

impl TabletRuntime {
    fn load() -> Self {
        Self {
            configs: settings_store::hardware::tablets_from_section(&settings_store::load_section(
                input_config::SECTION,
            )),
            shortcuts: settings_store::hardware::ShortcutConfig::load(),
            pads: None,
            dials: crate::compositor::tablet_pad::DialSteps::default(),
        }
    }

    /// Re-read the settings, keeping the open pads and touches in progress.
    fn reload(&mut self) {
        let pads = self.pads.take();
        let dials = std::mem::take(&mut self.dials);
        *self = Self {
            pads,
            dials,
            ..Self::load()
        };
    }

    fn config_for(
        &self,
        device: &smithay::reexports::input::Device,
    ) -> settings_store::hardware::TabletConfig {
        self.configs
            .get(&libinput_device_key(device))
            .cloned()
            .unwrap_or_default()
    }
}

/// Tablet pads' buttons, rings and strips. Smithay's libinput backend drops
/// pad events, so each pad is opened again in a path context of its own
/// that is read only for them.
pub struct PadInput {
    context: Libinput,
    /// Pad handles in `context`, by the udev context's device id.
    devices: HashMap<String, smithay::reexports::input::Device>,
}

impl PadInput {
    fn new(seat: LibSeatSession) -> Self {
        Self {
            context: Libinput::new_from_path(LibinputSessionInterface::from(seat)),
            devices: HashMap::new(),
        }
    }

    fn add(&mut self, pad: &smithay::reexports::input::Device) {
        let path = Path::new("/dev/input").join(pad.sysname());
        let Some(path) = path.to_str() else {
            return;
        };
        let Some(device) = self.context.path_add_device(path) else {
            tracing::warn!(pad = pad.name(), "DRM input: could not open tablet pad");
            return;
        };
        tracing::info!(pad = pad.name(), "DRM input: listening to tablet pad");
        self.devices.insert(pad.id(), device);
    }

    fn remove(&mut self, pad: &smithay::reexports::input::Device) {
        if let Some(device) = self.devices.remove(&pad.id()) {
            self.context.path_remove_device(device);
        }
    }

    /// Read pending pad events.
    fn take_events(&mut self) -> Vec<RawPadEvent> {
        use settings_store::hardware::PadDial;
        use smithay::reexports::input::event::tablet_pad::{
            ButtonState as PadButtonState, RingAxisSource, StripAxisSource, TabletPadEvent,
            TabletPadEventTrait as _,
        };
        use smithay::reexports::input::event::{Event as LibinputEvent, EventTrait as _};
        if let Err(error) = self.context.dispatch() {
            tracing::debug!(%error, "DRM input: tablet pad dispatch failed");
        }
        // libinput reports a lifted finger as a negative position.
        let touching = |position: f64| (position >= 0.0).then_some(position);
        let mut events = Vec::new();
        for event in &mut self.context {
            let LibinputEvent::TabletPad(event) = event else {
                continue;
            };
            match event {
                TabletPadEvent::Button(button) => {
                    let number = button.button_number();
                    let pressed = button.button_state() == PadButtonState::Pressed;
                    let group = button.mode_group();
                    events.push(RawPadEvent::Button {
                        pad: libinput_device_key(&button.device()),
                        button: number,
                        pressed,
                        time: button.time(),
                        mode_switch: (pressed && group.button_is_toggle(number))
                            .then(|| (group.index(), button.mode(), group.number_of_modes())),
                        mode_button: group.button_is_toggle(number),
                    });
                }
                TabletPadEvent::Ring(ring) => events.push(RawPadEvent::Dial {
                    pad: libinput_device_key(&ring.device()),
                    dial: PadDial::Ring(ring.number()),
                    position: touching(ring.position()),
                    finger: ring.source() == RingAxisSource::Finger,
                    mode: ring.mode(),
                    time: ring.time(),
                }),
                TabletPadEvent::Strip(strip) => events.push(RawPadEvent::Dial {
                    pad: libinput_device_key(&strip.device()),
                    dial: PadDial::Strip(strip.number()),
                    position: touching(strip.position()),
                    finger: strip.source() == StripAxisSource::Finger,
                    mode: strip.mode(),
                    time: strip.time(),
                }),
                _ => {}
            }
        }
        events
    }
}

/// One pad event as libinput reported it.
enum RawPadEvent {
    Button {
        pad: String,
        button: u32,
        pressed: bool,
        time: u32,
        /// `(group, new mode, modes)` when the button is its group's mode
        /// switch.
        mode_switch: Option<(u32, u32, u32)>,
        /// Whether the button switches its group's mode, pressed or not.
        mode_button: bool,
    },
    /// A ring (degrees) or strip (`0.0..=1.0`) moved, or `None`: the finger
    /// lifted.
    Dial {
        pad: String,
        dial: settings_store::hardware::PadDial,
        position: Option<f64>,
        finger: bool,
        /// The mode of the control's group.
        mode: u32,
        time: u32,
    },
}

/// The direct-touch equivalent of the pointer-gesture ownership tracker.
///
/// A touch client has to see the first one or two contacts promptly, before
/// we can know whether a third will join them. Once it does, `wl_touch.cancel`
/// retires that partial client stream and ownership remains with the shell
/// until every contact lifts. Turning touch into pointer input, where a
/// contact needs that, is the compositor's decision: only it knows whether
/// the contact reached a client that understands touch.
#[derive(Debug, Default)]
pub(crate) struct DirectTouchOwnership {
    contacts: BTreeMap<TouchContactKey, DirectTouchContact>,
    next_global_id: u32,
    client_stream_active: bool,
    client_frame_pending: bool,
    shell_owned: bool,
}

/// Libinput touch slots restart at zero for every physical device. Keep the
/// device identity in the compositor-side key, then allocate an independent
/// seat-global id for the `wl_touch` and winit wires.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct TouchContactKey {
    device: String,
    slot: u32,
}

#[derive(Clone, Copy, Debug)]
struct DirectTouchContact {
    id: u32,
    current: (f64, f64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DirectTouchDown {
    ForwardToClient(u32),
    CancelClientForShell(u32),
    ShellOwned(u32),
}

impl DirectTouchOwnership {
    /// Construct the stable compositor-side identity for a physical touch
    /// contact. Test-control's headless input adapter uses this same
    /// ownership state as DRM, rather than recreating the third-finger
    /// handoff policy in a second injector.
    pub(crate) fn key(device: impl Into<String>, slot: u32) -> TouchContactKey {
        TouchContactKey {
            device: device.into(),
            slot,
        }
    }

    pub(crate) fn down(&mut self, key: TouchContactKey, position: (f64, f64)) -> DirectTouchDown {
        let id = self.allocate_global_id();
        self.contacts.insert(
            key,
            DirectTouchContact {
                id,
                current: position,
            },
        );
        if self.shell_owned {
            return DirectTouchDown::ShellOwned(id);
        }
        if self.contacts.len() >= 3 {
            self.shell_owned = true;
            if self.client_stream_active {
                self.client_stream_active = false;
                // Smithay's TouchHandle::cancel is itself the complete
                // protocol boundary. A following frame is suppressed once
                // cancel advances each slot's sequence, so do not promise a
                // frame the seat cannot emit.
                self.client_frame_pending = false;
                return DirectTouchDown::CancelClientForShell(id);
            }
            return DirectTouchDown::ShellOwned(id);
        }
        self.client_stream_active = true;
        self.client_frame_pending = true;
        DirectTouchDown::ForwardToClient(id)
    }

    pub(crate) fn motion(
        &mut self,
        key: &TouchContactKey,
        position: (f64, f64),
    ) -> Option<(u32, bool)> {
        let contact = self.contacts.get_mut(key)?;
        contact.current = position;
        if self.client_stream_active {
            self.client_frame_pending = true;
        }
        Some((contact.id, self.client_stream_active))
    }

    /// Returns the last known position, the contact's id and whether this
    /// still belongs to the client touch stream.
    pub(crate) fn up(&mut self, key: &TouchContactKey) -> Option<((f64, f64), u32, bool)> {
        let contact = self.contacts.remove(key)?;
        let forward_to_client = self.client_stream_active;
        if self.contacts.is_empty() {
            self.reset();
        }
        if forward_to_client {
            // Set this after a possible ownership reset: Up still belongs to
            // the client protocol batch even when it removed the final
            // compositor-side contact.
            self.client_frame_pending = true;
        }
        Some((contact.current, contact.id, forward_to_client))
    }

    pub(crate) fn client_stream_active(&self) -> bool {
        self.client_stream_active
    }

    pub(crate) fn take_client_frame(&mut self) -> bool {
        std::mem::take(&mut self.client_frame_pending)
    }

    pub(crate) fn cancel(&mut self) -> Vec<(u32, (f64, f64))> {
        let forward_cancel = self.client_stream_active;
        let contacts = std::mem::take(&mut self.contacts);
        self.reset();
        if forward_cancel {
            // `wl_touch.cancel` completes the sequence; unlike Up it is not
            // followed by a frame in Smithay's touch state machine.
            self.client_frame_pending = false;
        }
        contacts
            .into_values()
            .map(|contact| (contact.id, contact.current))
            .collect()
    }

    fn allocate_global_id(&mut self) -> u32 {
        loop {
            let id = self.next_global_id;
            self.next_global_id = self.next_global_id.wrapping_add(1);
            if self.contacts.values().all(|contact| contact.id != id) {
                return id;
            }
        }
    }

    fn reset(&mut self) {
        self.client_stream_active = false;
        self.shell_owned = false;
        // `reset` retires ownership, not the protocol batch already queued by
        // a final Up. Keep `client_frame_pending` until the backend drains it
        // through `take_client_frame`, otherwise the last contact can
        // disappear without its closing wl_touch.frame. Cancel explicitly
        // clears the flag because wl_touch.cancel is its own boundary.
    }
}

/// One output the backend is currently driving, from the loop thread's view — the scanout itself lives on the render thread; this is the identity, geometry and crtc needed to reconcile a hotplug.
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent facts about one output (adaptive sync support, the user's allowance, \
              its engagement, HDR driver readiness), each read on its own"
)]
pub struct LiveEntry {
    pub key: DrmOutputKey,
    pub id: OutputId,
    /// Raw KMS label retained for libinput's `output_name` metadata.
    pub connector_name: String,
    /// Collision-resolved key used for display persistence and layout.
    pub config_key: String,
    pub size: (u32, u32),
    /// The mode this output is driven at.
    pub mode: settings_store::hardware::OutputMode,
    /// The configured mode it was brought up under, if any; a rescan keeps
    /// `mode` while this is unchanged (see [`hotplug::retained_mode`]).
    pub mode_intent: Option<settings_store::hardware::OutputMode>,
    pub physical_mm: Option<(u32, u32)>,
    /// EDID identity, advertised to clients through `wl_output`.
    pub monitor: Option<settings_store::hardware::MonitorIdentity>,
    pub crtc: smithay::reexports::drm::control::crtc::Handle,
    /// Top-left in the scene the presenter paints (physical pixels).
    pub position: (i32, i32),
    /// Top-left in the logical layout the pointer moves in.
    pub logical_position: (i32, i32),
    pub scale: f64,
    pub transform: settings_store::hardware::OutputTransform,
    /// This output's presentation interval, captured when brought up — kept here rather than read from the scanout, since scanouts move to the render thread and would start returning `None`.
    pub refresh: Option<std::time::Duration>,
    /// Sink and driver both support adaptive sync, captured at bring-up.
    pub adaptive_sync_supported: bool,
    /// The user allows adaptive sync on this output (on unless turned off).
    pub adaptive_sync_allowed: bool,
    /// Adaptive sync was last asked for on this output's CRTC.
    pub adaptive_sync_engaged: bool,
    /// The sink's HDR description, when its EDID advertises PQ.
    pub hdr_display: Option<crate::wayland::edid::HdrDisplayInfo>,
    /// The connector has every property HDR signalling needs.
    pub hdr_driver_ready: bool,
    /// HDR as the user configured it: SDR white in cd/m² when on.
    pub hdr_setting: Option<f32>,
    /// The HDR request last sent to the scanout owner.
    pub hdr_requested: Option<HdrRequest>,
}

/// HDR is requested only for a sink whose EDID advertises PQ, on a connector
/// with the HDR properties, when the user turned it on and no earlier attempt
/// failed on this output.
pub(crate) fn hdr_request_for(
    display: Option<&crate::wayland::edid::HdrDisplayInfo>,
    driver_ready: bool,
    setting: Option<f32>,
    failed: bool,
) -> Option<HdrRequest> {
    let display = display?;
    let sdr_white_nits = setting?;
    (driver_ready && !failed).then(|| HdrRequest {
        display: display.clone(),
        sdr_white_nits,
    })
}

/// Adaptive sync engages only for fullscreen content on an output that allows
/// and supports it. The desktop stays at the fixed rate: uneven UI frame times
/// make many panels flicker in brightness when the refresh rate follows them.
pub(crate) fn adaptive_sync_wanted(supported: bool, allowed: bool, fullscreen_here: bool) -> bool {
    supported && allowed && fullscreen_here
}

/// What the presenter reports about one output's signalling.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignalReport {
    pub output: OutputId,
    pub status: OutputSignalStatus,
}

/// What the loop thread asks the owner of the scanouts to do.
///
/// The scanouts move to the render thread at startup
/// (`super::DrmBackend::take_presenter`) since presentation is the only
/// thing that touches them per frame; hotplug, VT switches and resume
/// originate on the loop thread and must reach across. A channel drained
/// at the top of each frame keeps the scanouts single-owner and orders every reconfiguration against surrounding frames.
pub enum ScanoutCommand {
    /// Drop these outputs, adopt these new ones, and take this as the complete output table (positions included).
    Reconfigure {
        remove: Vec<OutputId>,
        add: Vec<ScanoutEntry>,
        outputs: Vec<Output>,
        /// Sent only after the presenter dropped every removed scanout.  The
        /// loop thread then closes the matching original libseat fd.
        removal_ack: Option<(u64, Sender<u64>)>,
    },
    /// The session lost the hardware: stop presenting entirely.
    Pause,
    /// The session has it back: re-modeset every output, restore the panels' power state, and resume presenting.
    Resume {
        dpms_on: bool,
    },
    SetCursorSize(u32),
    /// Draw the cursor on `output` for its scale.
    SetCursorScale {
        output: OutputId,
        scale: f64,
    },
    SetPointerImage(crate::compositor::cursor::PointerImage),
    /// Switch one panel off or back on for an output-power client.
    SetOutputPower {
        output: OutputId,
        on: bool,
    },
    /// Turn adaptive sync on or off on one output's CRTC.
    SetAdaptiveSync {
        output: OutputId,
        on: bool,
    },
    /// Drive one output in HDR with this request, or in SDR.
    SetHdr {
        output: OutputId,
        request: Option<HdrRequest>,
    },
}

/// The ownership state for one card across a udev remove/re-add race.  This
/// tiny state machine is deliberately fd-free so its ordering can be tested
/// without libseat or a presenter thread; the runtime keeps the actual
/// `CardHandle`s in the maps beside it.
#[derive(Debug, Default)]
struct CardLeaseLifecycle {
    active: BTreeSet<u64>,
    retiring: BTreeSet<u64>,
    deferred_readds: BTreeSet<u64>,
    failed_closes: BTreeSet<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CardLeaseAction {
    Open,
    AlreadyOpen,
    DeferReadd,
    DetachPresenter,
    CloseSessionFd,
    ReopenDeferred,
    BlockedByCloseFailure,
    Noop,
}

impl CardLeaseLifecycle {
    fn from_active(active: impl IntoIterator<Item = u64>) -> Self {
        Self {
            active: active.into_iter().collect(),
            ..Self::default()
        }
    }

    fn add_requested(&mut self, device_id: u64) -> CardLeaseAction {
        if self.failed_closes.contains(&device_id) {
            CardLeaseAction::BlockedByCloseFailure
        } else if self.retiring.contains(&device_id) {
            self.deferred_readds.insert(device_id);
            CardLeaseAction::DeferReadd
        } else if self.active.contains(&device_id) {
            CardLeaseAction::AlreadyOpen
        } else {
            CardLeaseAction::Open
        }
    }

    fn opened(&mut self, device_id: u64) {
        self.active.insert(device_id);
    }

    fn remove_requested(&mut self, device_id: u64) -> CardLeaseAction {
        if self.active.remove(&device_id) {
            self.retiring.insert(device_id);
            CardLeaseAction::DetachPresenter
        } else {
            CardLeaseAction::Noop
        }
    }

    fn presenter_detached(&self, device_id: u64) -> CardLeaseAction {
        if self.retiring.contains(&device_id) {
            CardLeaseAction::CloseSessionFd
        } else {
            CardLeaseAction::Noop
        }
    }

    fn session_close_finished(&mut self, device_id: u64, succeeded: bool) -> CardLeaseAction {
        if !self.retiring.remove(&device_id) {
            return CardLeaseAction::Noop;
        }
        let had_deferred_readd = self.deferred_readds.remove(&device_id);
        if succeeded && had_deferred_readd {
            CardLeaseAction::ReopenDeferred
        } else {
            if !succeeded {
                self.failed_closes.insert(device_id);
            }
            CardLeaseAction::Noop
        }
    }
}

/// Hardware part of a seat master handoff. Kept this small so the all-card
/// distribution policy can be proven with fakes while [`CardHandle`] remains
/// the only real DRM implementation.
trait KmsMasterControl {
    fn release_master(&self);
    fn acquire_master(&self);
}

impl KmsMasterControl for CardHandle {
    fn release_master(&self) {
        CardHandle::drop_master(self);
    }

    fn acquire_master(&self) {
        CardHandle::acquire_master(self);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MasterAction {
    Release,
    Acquire,
}

/// Apply a DRM-master transition to every retained seat GPU. The caller owns
/// sequencing with `PauseScanout`/`Resume`; this helper's narrow job is to
/// make "all cards" non-accidental and unit-testable.
fn apply_master_action<'a, T: KmsMasterControl + 'a>(
    cards: impl IntoIterator<Item = &'a T>,
    action: MasterAction,
) {
    for card in cards {
        match action {
            MasterAction::Release => card.release_master(),
            MasterAction::Acquire => card.acquire_master(),
        }
    }
}

/// A removed card stays libseat-owned until the presenter drops its duplicate
/// scanout fds. A VT pause in that interval must release its DRM master too;
/// a resume intentionally reacquires only active cards.
fn release_masters_for_pause<'a, T: KmsMasterControl + 'a>(
    active: impl IntoIterator<Item = &'a T>,
    retiring: impl IntoIterator<Item = &'a T>,
) {
    apply_master_action(active.into_iter().chain(retiring), MasterAction::Release);
}

/// Every probed connector, as display-set matching sees it.
fn display_set_of<'a>(
    snapshots: impl IntoIterator<Item = &'a ConnectorSnapshot>,
) -> settings_store::hardware::DisplaySet {
    settings_store::hardware::DisplaySet::of(snapshots.into_iter().map(|snapshot| {
        settings_store::hardware::AttachedOutput {
            connector: &snapshot.config_key,
            connector_label: &snapshot.connector_name,
            monitor: snapshot.monitor.as_ref(),
        }
    }))
}

/// Force-probe every retained card: what is connected and each sink's EDID
/// identity. Slow on real hardware (a DP-MST sink alone can take hundreds of
/// milliseconds) and on the input thread, so only startup and udev hotplug
/// call it; see [`ConnectorProbe`].
fn force_probe_connectors(
    cards: &BTreeMap<u64, CardHandle>,
) -> BTreeMap<u64, Vec<ConnectorSnapshot>> {
    cards
        .iter()
        .map(|(&device_id, card)| (device_id, card.connected()))
        .collect()
}

/// How [`DrmDispatchState::connector_snapshots`] learns what is connected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConnectorProbe {
    /// Ask the hardware ([`force_probe_connectors`]). A udev hotplug edge is
    /// precisely when the remembered connector state is stale, so only the
    /// udev paths pay for it.
    Force,
    /// What the last forced probe found. Connectors, their mode tables and
    /// their EDIDs change only through a hotplug, which re-probes, so a
    /// settings change and capability publication read this instead.
    Cached,
}

/// Take probed snapshots (one list per retained card), then resolve
/// connector keys over the *whole* seat before looking up display intent.
/// Per-card configuration lookup is
/// wrong for a hybrid laptop because both cards may legitimately have an
/// `HDMI-A-1`; this one path is shared by startup, hotplug, Settings
/// publication and transaction validation.
///
/// Intent is the layout saved for the attached set of monitors. The set's
/// key does not depend on connector keys, so the layout found under the raw
/// labels decides key qualification, and the final keys then place each
/// remembered monitor.
///
/// A connector already lit keeps its current mode unless its configured
/// mode changed (see [`hotplug::retained_mode`]), so `live` is the outputs
/// being driven now and `lit_for` the set of monitors they were configured
/// for (empty and `None` at startup).
fn configured_connectors(
    cards: &BTreeMap<u64, CardHandle>,
    mut snapshots: BTreeMap<u64, Vec<ConnectorSnapshot>>,
    live: &[LiveEntry],
    lit_for: Option<&settings_store::hardware::DisplaySet>,
) -> (
    BTreeMap<u64, Vec<ConnectorSnapshot>>,
    settings_store::hardware::DisplaySet,
) {
    let provisional = display_set_of(snapshots.values().flatten());
    let config = settings_store::hardware::DisplayConfig::load_effective(&provisional);
    let keys = resolve_connector_config_keys(
        snapshots.iter().flat_map(|(&device_id, outputs)| {
            let card_identity = cards
                .get(&device_id)
                .map_or("drm-unknown", CardHandle::config_identity);
            outputs.iter().map(move |output| ConnectorKeyCandidate {
                key: DrmOutputKey {
                    device: device_id,
                    connector: output.connector_id,
                },
                connector_name: &output.connector_name,
                card_identity,
            })
        }),
        &config,
    );
    for (&device_id, outputs) in &mut snapshots {
        for output in outputs {
            let raw = output.connector_name.clone();
            output.config_key = keys
                .get(&DrmOutputKey {
                    device: device_id,
                    connector: output.connector_id,
                })
                .cloned()
                // Every entry above creates a candidate. Retaining the raw
                // label is just defensive for a future partial probe.
                .unwrap_or(raw);
        }
    }
    let set = display_set_of(snapshots.values().flatten());
    let config = settings_store::hardware::DisplayConfig::load_effective(&set);
    let set_changed = lit_for.is_some_and(|lit_for| *lit_for != set);
    for (&device_id, outputs) in &mut snapshots {
        for output in outputs {
            let config_key = output.config_key.clone();
            CardHandle::apply_output_config(output, config_key, &config);
            let key = DrmOutputKey {
                device: device_id,
                connector: output.connector_id,
            };
            let lit = live
                .iter()
                .find(|entry| entry.key == key)
                .map(|entry| hotplug::LiveMode {
                    mode: entry.mode,
                    intent: entry.mode_intent,
                });
            if let Some(mode) = hotplug::retained_mode(
                lit,
                output.mode_intent,
                set_changed,
                &output.advertised_modes(),
            ) {
                output.retain_mode(mode);
            }
        }
    }
    (snapshots, set)
}

/// Build the Settings capability contract from the same topology that chose
/// scanout configuration keys (`snapshots`, already through
/// [`configured_connectors`]). Disabled sinks stay visible here so users can
/// independently re-enable a colliding connector.
fn display_capabilities(
    cards: &BTreeMap<u64, CardHandle>,
    snapshots: &BTreeMap<u64, Vec<ConnectorSnapshot>>,
    profiles: &BTreeMap<String, String>,
    signal_status: &BTreeMap<String, OutputSignalStatus>,
    scale_override: Option<f64>,
) -> Vec<settings_store::hardware::DisplayOutputSnapshot> {
    let mut outputs: Vec<_> = cards
        .iter()
        .flat_map(|(&device_id, card)| {
            card.display_capabilities(
                snapshots.get(&device_id).map_or(&[], Vec::as_slice),
                profiles,
                signal_status,
                scale_override,
            )
        })
        .collect();
    outputs.sort_by(|a, b| a.descriptor.connector.cmp(&b.descriptor.connector));
    outputs
}

/// Put each live output's actual scale over the one its capability entry
/// was given for running unconfigured, keyed by configuration key.
fn apply_live_scales<'a>(
    outputs: &mut [settings_store::hardware::DisplayOutputSnapshot],
    live: impl IntoIterator<Item = (&'a str, f64)>,
) {
    for (key, scale) in live {
        if let Some(output) = outputs
            .iter_mut()
            .find(|output| output.descriptor.connector == key)
        {
            output.scale = scale;
        }
    }
}

/// Rebind already-modeset entries to the collision-resolved key from the
/// latest full-card topology. A connector can stay on the same CRTC and mode
/// while a same-named peer appears; it still must switch its settings key
/// before layout/primary/scale intent is read.
fn reconcile_live_config_keys(
    live: &mut [LiveEntry],
    keys: &BTreeMap<DrmOutputKey, String>,
) -> bool {
    let mut changed = false;
    for entry in live {
        if let Some(key) = keys.get(&entry.key)
            && entry.config_key != *key
        {
            entry.config_key.clone_from(key);
            changed = true;
        }
    }
    changed
}

/// Record a physical capability topology independently from active scanout.
/// The return value is the publish trigger for a disabled connector/card
/// replug where `hotplug::plan` quite correctly has no modeset work.
fn reconcile_capability_topology(
    previous: &mut BTreeSet<DrmOutputKey>,
    current: BTreeSet<DrmOutputKey>,
) -> bool {
    if *previous == current {
        false
    } else {
        *previous = current;
        true
    }
}

impl DrmDispatchState {
    /// Keep the cursor planes' offset in step with the image they show.
    fn sync_cursor_hotspot(&mut self) {
        self.hw_cursor.set_image_hotspot(
            &self.pointer_image,
            self.accessibility.effective_cursor_size(),
        );
    }

    /// Build the initial dispatch state from everything [`DrmRuntime::new`]
    /// gathered before it — the seat's active session, every retained KMS
    /// card, and the persisted settings. Every other field
    /// starts at its zero value; [`Self::finalize_startup_scanouts`] is
    /// what actually populates `live`/`output_meta`/`pointer`. Extracted
    /// from that constructor to keep it a readable top-level sequence.
    fn new(
        session_state: SessionState,
        cards: BTreeMap<u64, CardHandle>,
        seat: LibSeatSession,
        input: InputSettings,
        accessibility: AccessibilityInputSettings,
        scale_override: Option<f64>,
        commands_tx: Sender<ScanoutCommand>,
        keyboard: Option<KeyboardXkb>,
    ) -> Self {
        let (card_removal_ack_sender, card_removal_acks) = channel();
        let card_leases = CardLeaseLifecycle::from_active(cards.keys().copied());
        Self {
            session: Some(session_state),
            outputs: OutputRegistry::new(),
            events: Vec::new(),
            cards,
            seat: Some(seat),
            retiring_cards: BTreeMap::new(),
            deferred_card_adds: BTreeMap::new(),
            card_removal_ack_sender: Some(card_removal_ack_sender),
            card_removal_acks: Some(card_removal_acks),
            failed_card_close_paths: BTreeSet::new(),
            failed_card_close_identities: BTreeSet::new(),
            unidentified_card_close_failure: false,
            card_leases,
            color_profiles: BTreeMap::new(),
            live: Vec::new(),
            capability_topology: BTreeSet::new(),
            connector_probes: BTreeMap::new(),
            #[cfg(test)]
            forced_probes: 0,
            commands: Some(commands_tx),
            input,
            accessibility,
            slow_pending: HashMap::new(),
            sticky_latched: ModifiersState::empty(),
            bounce_keys: BounceKeys::default(),
            mouse_keys: MouseKeys::default(),
            physical_modifiers: ModifiersState::empty(),
            key_repeat: KeyRepeat::default(),
            devices: Vec::new(),
            touch_output_bindings: HashMap::new(),
            scale_override,
            gestures: GestureTracker::new(GestureConfig {
                natural_scroll: input.natural_scroll_for(true),
                ..GestureConfig::default()
            }),
            direct_touch: DirectTouchOwnership::default(),
            last_input_used: None,
            outputs_changed: false,
            output_meta: HashMap::new(),
            display_capabilities: Vec::new(),
            device_rotation: None,
            pointer: (0.0, 0.0),
            logical_pointer: (0.0, 0.0),
            output_size: (0, 0),
            display_set: settings_store::hardware::DisplaySet::default(),
            hw_cursor: CursorPlanes::default(),
            pointer_image: crate::compositor::cursor::PointerImage::default(),
            pointer_constraint: crate::compositor::backend::PointerConstraintRegion::Free,
            tablets: TabletRuntime::load(),
            keyboard,
            keyboard_device: None,
            keyboard_config: settings_store::hardware::RichInputConfig::from_section(
                &settings_store::load_section("input"),
            ),
            keyboard_cache: BTreeMap::new(),
            keyboard_identities: BTreeMap::new(),
            keyboard_key_owners: BTreeMap::new(),
            present_gate: Some(Arc::new(Mutex::new(()))),
            fullscreen_output: None,
            signal_status: BTreeMap::new(),
            signal_reports: None,
            client_planes: super::client_scanout::ClientPlanes::default(),
        }
    }

    /// Adopt the connectors already live at startup — the same path a
    /// hot-plug takes, so a display present at boot and one plugged in
    /// later end up configured identically. Each is interned by `(device,
    /// connector)` so its `OutputId` is stable, and the first gets
    /// `OutputId(0)`: the last tie-break for the primary when Settings names
    /// none and no built-in panel is attached (see `relayout_with`).
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
        // Start the pointer in the middle of the logical desktop.
        let placements = self.placements();
        let (lw, lh) = hotplug::combined_extent(
            &placements
                .iter()
                .map(|p| p.logical_position)
                .collect::<Vec<_>>(),
            &placements
                .iter()
                .map(|p| p.logical_size)
                .collect::<Vec<_>>(),
        );
        self.set_pointer_logical(layout::clamp_to_outputs(
            &placements,
            (f64::from(lw) / 2.0, f64::from(lh) / 2.0),
        ));
        self.publish_display_snapshot();
        scanouts
    }

    pub fn push(&mut self, ev: BackendEvent) {
        self.events.push(ev);
    }

    /// Report a physical modality change, or deliberate pointer activation.
    /// Pointer activation also follows hover, which shares its modality.
    /// Emitted ahead of the event, so the compositor switches modality first.
    fn note_input_used(&mut self, modality: ui::InputModality) {
        if self.last_input_used != Some(modality) || modality == ui::InputModality::Pointer {
            self.last_input_used = Some(modality);
            self.push(BackendEvent::InputUsed(modality));
        }
    }

    /// Coalesce hover reports without losing a later touch or click transition.
    fn note_pointer_motion(&mut self) {
        if self.last_input_used != Some(ui::InputModality::Pointer) {
            self.last_input_used = Some(ui::InputModality::Pointer);
            self.push(BackendEvent::PointerMotionUsed);
        }
    }

    /// Publish the live hardware contract for Settings. Failure is benign in
    /// test/headless sessions (usually no `XDG_RUNTIME_DIR`) and never changes
    /// the output plan itself.
    fn publish_display_snapshot(&mut self) {
        let mut outputs = self.capability_outputs();
        apply_live_scales(
            &mut outputs,
            self.live
                .iter()
                .map(|entry| (entry.config_key.as_str(), entry.scale)),
        );
        let layout = self
            .live
            .iter()
            .map(|entry| {
                let (width, height) = logical_output_size(entry.size, entry.transform);
                settings_store::hardware::LiveOutputPlacement {
                    connector: entry.config_key.clone(),
                    x: entry.position.0,
                    y: entry.position.1,
                    width,
                    height,
                }
            })
            .collect();
        if outputs != self.display_capabilities {
            self.display_capabilities.clone_from(&outputs);
            // A disabled connector appearing changes the heads external
            // configuration tools see, even with no modeset to report.
            self.outputs_changed = true;
            self.push(BackendEvent::OutputsChanged);
        }
        let (publisher_pid, publisher_start_ticks) =
            match settings_store::hardware::publisher_identity() {
                Ok(identity) => identity,
                Err(error) => {
                    tracing::debug!(%error, "DRM display: no publisher lease for the snapshot");
                    return;
                }
            };
        let snapshot = settings_store::hardware::DisplayHardwareSnapshot {
            generated_unix_ms: unix_now_ms(),
            backend: "drm-kms-xrgb8888".into(),
            publisher_pid,
            publisher_start_ticks,
            outputs,
            layout,
        };
        if let Err(error) = snapshot.save() {
            tracing::debug!(%error, "DRM display: could not publish capability snapshot");
        }
    }

    /// Send a command to whoever owns the scanouts.  `false` means there is
    /// no owner: either a headless test/backend or a presenter that already
    /// exited and therefore dropped every scanout fd it held.
    fn command(&self, cmd: ScanoutCommand) -> bool {
        self.commands
            .as_ref()
            .is_some_and(|tx| tx.send(cmd).is_ok())
    }

    /// Every retained card's connectors, from the hardware or from the last
    /// forced probe (see [`ConnectorProbe`]). A forced probe replaces the
    /// remembered result for every card.
    fn connector_snapshots(
        &mut self,
        probe: ConnectorProbe,
    ) -> BTreeMap<u64, Vec<ConnectorSnapshot>> {
        match probe {
            ConnectorProbe::Force => {
                #[cfg(test)]
                {
                    self.forced_probes = self.forced_probes.saturating_add(1);
                }
                self.connector_probes = force_probe_connectors(&self.cards);
                self.connector_probes.clone()
            }
            ConnectorProbe::Cached => {
                let mut snapshots = BTreeMap::new();
                for (&device_id, card) in &self.cards {
                    let probed = self.connector_probes.entry(device_id).or_insert_with(|| {
                        // Every path that retains a card probes it (startup,
                        // then the udev rescan that follows an add). A card
                        // never probed has no state to reuse; reading it as
                        // connectorless would tear its outputs down.
                        tracing::warn!(
                            device_id,
                            "DRM display: card has no probed connectors; probing it"
                        );
                        #[cfg(test)]
                        {
                            self.forced_probes = self.forced_probes.saturating_add(1);
                        }
                        card.connected()
                    });
                    snapshots.insert(device_id, probed.clone());
                }
                snapshots
            }
        }
    }

    /// The Settings capability contract for the current topology, built from
    /// the last forced probe: publication follows hotplug, display settings
    /// and presenter reports, none of which changes what is connected.
    fn capability_outputs(&mut self) -> Vec<settings_store::hardware::DisplayOutputSnapshot> {
        let probed = self.connector_snapshots(ConnectorProbe::Cached);
        let (snapshots, _) =
            configured_connectors(&self.cards, probed, &self.live, Some(&self.display_set));
        display_capabilities(
            &self.cards,
            &snapshots,
            &self.color_profiles,
            &self.signal_status,
            self.scale_override,
        )
    }

    /// Re-ask the hardware what is connected and reconcile.
    ///
    /// Every udev edge lands here, including no-op ones — a `Changed` event
    /// fires on every DPMS transition and EDID re-read, and re-modesetting on those would flash the panel for nothing. [`hotplug::plan`] tells the two apart.
    pub fn rescan_outputs(&mut self, reason: &str) {
        self.rescan_outputs_with_removal_ack(reason, None, ConnectorProbe::Force);
    }

    /// Reconcile the outputs with the display settings, over the connectors
    /// the last forced probe found — the hotplug reconciliation without the
    /// probe. A settings write may request another mode or turn a connector
    /// on or off, and that modesets here; what is connected changes only
    /// through a udev edge, which takes [`Self::rescan_outputs`].
    pub fn reapply_outputs_from_cache(&mut self, reason: &str) {
        self.rescan_outputs_with_removal_ack(reason, None, ConnectorProbe::Cached);
    }

    /// As [`Self::rescan_outputs`], with an acknowledgement sent by the
    /// presenter after it has dropped the scanouts for a removed card.  The
    /// card's original libseat fd remains in `retiring_cards` until that
    /// acknowledgement reaches the loop thread.
    #[allow(
        clippy::too_many_lines,
        reason = "hotplug reconciliation must keep topology, scanout, cursor, acknowledgement, and redraw updates in one ordered transaction"
    )]
    fn rescan_outputs_with_removal_ack(
        &mut self,
        reason: &str,
        removal_ack: Option<(u64, Sender<u64>)>,
        probe: ConnectorProbe,
    ) {
        let probed = self.connector_snapshots(probe);
        let (snapshots, display_set) =
            configured_connectors(&self.cards, probed, &self.live, Some(&self.display_set));
        let display_set_changed = self.display_set != display_set;
        self.display_set = display_set;
        // A surviving output now answers to the configuration of this set
        // of monitors, whether it kept its mode or is re-modeset below.
        for entry in &mut self.live {
            if let Some(snapshot) = snapshots.get(&entry.key.device).and_then(|snapshots| {
                snapshots
                    .iter()
                    .find(|snapshot| snapshot.connector_id == entry.key.connector)
            }) {
                entry.mode_intent = snapshot.mode_intent;
            }
        }
        let config_keys: BTreeMap<_, _> = snapshots
            .iter()
            .flat_map(|(&device_id, snapshots)| {
                snapshots.iter().map(move |snapshot| {
                    (
                        DrmOutputKey {
                            device: device_id,
                            connector: snapshot.connector_id,
                        },
                        snapshot.config_key.clone(),
                    )
                })
            })
            .collect();
        let capability_topology_changed = reconcile_capability_topology(
            &mut self.capability_topology,
            config_keys.keys().copied().collect(),
        );
        // Another set of monitors reads another saved layout even when no
        // connector needs a modeset (the same port, a different monitor).
        let live_config_changed =
            reconcile_live_config_keys(&mut self.live, &config_keys) || display_set_changed;
        let connected: Vec<ConnectedOutput> = snapshots
            .iter()
            .flat_map(|(&device_id, snapshots)| {
                snapshots
                    .iter()
                    .filter(|snapshot| snapshot.enabled)
                    .map(move |s| {
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
            if let Some((device_id, ack)) = removal_ack {
                // The card had no presenter-owned scanouts, so there is
                // nothing to await.  Use the same loop-thread completion
                // path as the normal presenter acknowledgement.
                let _ = ack.send(device_id);
            }
            if live_config_changed {
                // No modeset is necessary, but the same physical output now
                // reads a different per-card key. Re-layout and tell the
                // presenter/client output table before the next frame.
                self.relayout_outputs();
                self.command(ScanoutCommand::Reconfigure {
                    remove: Vec::new(),
                    add: Vec::new(),
                    outputs: self.output_table(),
                    removal_ack: None,
                });
                self.outputs_changed = true;
                self.push(BackendEvent::OutputsChanged);
                self.push(BackendEvent::Redraw);
            }
            if capability_topology_changed || live_config_changed {
                // `connected` intentionally excludes disabled outputs, but
                // Settings must still see a disabled dock connector/card as
                // it appears or disappears so it can be re-enabled.
                self.publish_display_snapshot();
            }
            tracing::debug!(
                reason,
                live_config_changed,
                capability_topology_changed,
                "DRM hotplug: no scanout modeset required"
            );
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

        // Bring up everything new, avoiding the CRTCs survivors hold on
        // *that same card*.  CRTC handle values are card-local, so a flat
        // list would incorrectly make card1's CRTC 42 unavailable merely
        // because card0 also has a CRTC 42.
        let mut used: BTreeMap<u64, Vec<_>> = BTreeMap::new();
        for live in &self.live {
            used.entry(live.key.device).or_default().push(live.crtc);
        }
        let to_add: Vec<&ConnectedOutput> = plan
            .added
            .iter()
            .chain(plan.remoded.iter().map(|(_, c)| c))
            .collect();
        let mut builds: Vec<ScanoutBuild> = Vec::new();
        for want in to_add {
            let Some(card) = self.cards.get(&want.key.device) else {
                continue;
            };
            let Some(snapshot) = snapshots.get(&want.key.device).and_then(|snapshots| {
                snapshots
                    .iter()
                    .find(|s| s.connector_id == want.key.connector)
            }) else {
                continue;
            };
            let used_on_card = used.entry(want.key.device).or_default();
            if let Some(build) = card.bring_up(snapshot, used_on_card) {
                used_on_card.push(build.crtc);
                builds.push(build);
            }
        }

        let added_entries = self.adopt_builds(builds);
        self.relayout_outputs();
        let outputs = self.output_table();
        let send_failure_ack = removal_ack
            .as_ref()
            .map(|(device_id, ack)| (*device_id, ack.clone()));
        if !self.command(ScanoutCommand::Reconfigure {
            remove,
            add: added_entries,
            outputs,
            removal_ack,
        }) && let Some((device_id, ack)) = send_failure_ack
        {
            // If the receiver is gone, its presenter (and all duplicate card
            // fds) is gone too.  Complete teardown rather than holding the
            // original libseat device forever and blocking a later re-add.
            let _ = ack.send(device_id);
        }
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
        for mut build in builds {
            build.scanout.set_pointer_image(self.pointer_image.clone());
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
            // A replugged or rebuilt output gets a fresh chance at what an
            // earlier generation of it failed to drive.
            self.signal_status.remove(&build.config_key);
            // Its plane is probed again by the first fullscreen client on it.
            self.client_planes.publish(id, None);
            self.live.push(LiveEntry {
                key,
                id,
                connector_name: build.connector_name,
                config_key: build.config_key,
                size: build.scanout.size(),
                mode: build.scanout.output_mode(),
                mode_intent: build.mode_intent,
                physical_mm: build.physical_mm,
                monitor: build.monitor,
                crtc: build.crtc,
                position: (0, 0),
                logical_position: (0, 0),
                scale: 1.0,
                transform: settings_store::hardware::OutputTransform::Normal,
                refresh: build.scanout.refresh_interval(),
                adaptive_sync_supported: build.scanout.adaptive_sync_supported(),
                adaptive_sync_allowed: true,
                adaptive_sync_engaged: false,
                hdr_driver_ready: build.scanout.hdr_driver_missing().is_none(),
                hdr_display: build.hdr_display,
                hdr_setting: None,
                hdr_requested: None,
            });
            entries.push(ScanoutEntry {
                id,
                device_id: build.device_id,
                position: (0, 0),
                transform: settings_store::hardware::OutputTransform::Normal,
                color_profile: None,
                color_transform: None,
                color_buffer: Vec::new(),
                color_lut: None,
                direct_calibration: None,
                direct_targets: None,
                direct_ineligible: None,
                present_by_readback: false,
                readback_current: false,
                direct_degradation: DirectDegradation::default(),
                hdr_direct_degradation: DirectDegradation::default(),
                hdr_failure: None,
                powered_off: false,
                signal_reported: OutputSignalStatus::default(),
                scanout: build.scanout,
            });
        }
        entries
    }

    /// Apply the layout saved for the attached monitors, re-derive scales,
    /// and bring the pointer and cursor planes back onto a live output.
    /// Called after any change to the connector set or display plan.
    fn relayout_outputs(&mut self) {
        let config = settings_store::hardware::DisplayConfig::load_effective(&self.display_set);
        self.relayout_with(&config);
    }

    /// Lay out the live outputs from `config`. An output it does not
    /// mention — every output of a set never arranged before — is placed
    /// side by side, built-in panel first on the left and externals to its
    /// right, at the scale its pixel density calls for.
    fn relayout_with(&mut self, config: &settings_store::hardware::DisplayConfig) {
        self.live.sort_by_key(|l| {
            (
                !config.outputs.get(&l.config_key).is_some_and(|c| c.primary),
                !is_builtin_connector(&l.connector_name),
                l.id.0,
            )
        });
        for entry in &mut self.live {
            entry.transform = config
                .outputs
                .get(&entry.config_key)
                .map_or(settings_store::hardware::OutputTransform::Normal, |c| {
                    c.transform
                });
            // The accelerometer is in the built-in panel's lid: its reading
            // turns that panel and never an external monitor.
            if let Some(rotation) = self.device_rotation
                && is_builtin_connector(&entry.connector_name)
            {
                entry.transform = rotation;
            }
        }
        // Scale first: an output's logical extent — what the arrangement
        // is made of — is its mode over its scale.
        for entry in &mut self.live {
            entry.scale = config.outputs.get(&entry.config_key).map_or_else(
                || resolve_scale(entry.size, entry.physical_mm, self.scale_override),
                |c| c.scale,
            );
        }
        let logical_sizes: Vec<(u32, u32)> = self
            .live
            .iter()
            .map(|entry| layout::logical_extent(entry.size, entry.scale, entry.transform))
            .collect();
        let positions = self.logical_positions(config, &logical_sizes);
        let scene_sizes: Vec<(u32, u32)> = self
            .live
            .iter()
            .map(|entry| logical_output_size(entry.size, entry.transform))
            .collect();
        let scene_positions = layout::scene_positions(
            &positions
                .iter()
                .zip(&logical_sizes)
                .zip(&scene_sizes)
                .map(|((&p, &l), &s)| (p, l, s))
                .collect::<Vec<_>>(),
        );
        for ((entry, logical), scene) in self.live.iter_mut().zip(&positions).zip(&scene_positions)
        {
            entry.logical_position = *logical;
            entry.position = *scene;
        }
        self.output_size = hotplug::combined_extent(&scene_positions, &scene_sizes);
        let logical_pointer = layout::clamp_to_outputs(&self.placements(), self.logical_pointer);
        self.set_pointer_logical(logical_pointer);

        for entry in &mut self.live {
            let output = config.outputs.get(&entry.config_key);
            entry.adaptive_sync_allowed = output.is_none_or(|output| output.vrr);
            entry.hdr_setting = output
                .filter(|output| output.hdr)
                .map(|output| f32::from(output.sdr_brightness_nits));
        }
        self.output_meta = self.output_meta_for(config);
        self.rebuild_cursor_routing();
        self.reconcile_adaptive_sync();
        self.reconcile_hdr();
    }

    /// Tell the scanout owner about every output whose HDR request changed.
    /// An output whose HDR attempt failed is not asked again until it is
    /// rebuilt.
    fn reconcile_hdr(&mut self) {
        let mut changes = Vec::new();
        for entry in &mut self.live {
            let failed = self
                .signal_status
                .get(&entry.config_key)
                .is_some_and(|status| status.hdr_failure.is_some());
            let request = hdr_request_for(
                entry.hdr_display.as_ref(),
                entry.hdr_driver_ready,
                entry.hdr_setting,
                failed,
            );
            if request != entry.hdr_requested {
                entry.hdr_requested.clone_from(&request);
                changes.push((entry.id, request));
            }
        }
        for (output, request) in changes {
            self.command(ScanoutCommand::SetHdr { output, request });
        }
    }

    /// Record which output shows a fullscreen client and engage or release
    /// adaptive sync to match.
    pub fn set_fullscreen_output(&mut self, output: Option<OutputId>) {
        if self.fullscreen_output != output {
            self.fullscreen_output = output;
            self.reconcile_adaptive_sync();
        }
    }

    /// Tell the scanout owner about every output whose adaptive-sync wish
    /// changed. An output the driver refused is not asked again.
    fn reconcile_adaptive_sync(&mut self) {
        let mut changes = Vec::new();
        for entry in &mut self.live {
            let refused = self
                .signal_status
                .get(&entry.config_key)
                .is_some_and(|status| status.adaptive_sync_rejected);
            let wanted = adaptive_sync_wanted(
                entry.adaptive_sync_supported && !refused,
                entry.adaptive_sync_allowed,
                self.fullscreen_output == Some(entry.id),
            );
            if wanted != entry.adaptive_sync_engaged {
                entry.adaptive_sync_engaged = wanted;
                changes.push((entry.id, wanted));
            }
        }
        for (output, on) in changes {
            self.command(ScanoutCommand::SetAdaptiveSync { output, on });
        }
    }

    /// The HDR signal `output` is driven with: the request the presenter
    /// reports active.
    pub fn output_hdr(&self, output: OutputId) -> Option<crate::compositor::backend::OutputHdr> {
        let entry = self.live.iter().find(|entry| entry.id == output)?;
        let active = self
            .signal_status
            .get(&entry.config_key)
            .is_some_and(|status| status.hdr_active);
        let request = entry.hdr_requested.as_ref().filter(|_| active)?;
        Some(crate::compositor::backend::OutputHdr {
            display: request.display.clone(),
            sdr_white_nits: request.sdr_white_nits,
        })
    }

    /// Adaptive sync is engaged on at least one output, so frames should be
    /// paced by content rather than by a fixed refresh deadline.
    pub fn adaptive_sync_engaged(&self) -> bool {
        self.live.iter().any(|entry| entry.adaptive_sync_engaged)
    }

    /// Fold the presenter's signalling reports into the runtime status.
    /// Returns whether anything changed, so the caller republishes the
    /// Settings snapshot.
    fn drain_signal_reports(&mut self) -> bool {
        let Some(reports) = self.signal_reports.as_ref() else {
            return false;
        };
        let reports: Vec<SignalReport> = reports.try_iter().collect();
        let mut changed = false;
        for report in reports {
            let Some(key) = self
                .live
                .iter()
                .find(|entry| entry.id == report.output)
                .map(|entry| entry.config_key.clone())
            else {
                continue;
            };
            if self.signal_status.get(&key) != Some(&report.status) {
                self.signal_status.insert(key, report.status);
                changed = true;
            }
        }
        if changed {
            self.reconcile_adaptive_sync();
            self.reconcile_hdr();
        }
        changed
    }

    /// Where each live output sits in the logical layout: its configured
    /// position (a mirror takes its target's), else packed side by side at
    /// its logical size, translated so the island starts at the origin.
    fn logical_positions(
        &self,
        config: &settings_store::hardware::DisplayConfig,
        logical_sizes: &[(u32, u32)],
    ) -> Vec<(i32, i32)> {
        let fallback = hotplug::layout(logical_sizes);
        let mut positions: Vec<(i32, i32)> = self
            .live
            .iter()
            .zip(&fallback)
            .map(|(entry, fallback)| {
                config
                    .outputs
                    .get(&entry.config_key)
                    .map_or(*fallback, |c| (c.x, c.y))
            })
            .collect();
        for index in 0..self.live.len() {
            let Some(target) = self
                .live
                .get(index)
                .and_then(|entry| config.outputs.get(&entry.config_key))
                .and_then(|c| c.mirror_of.as_deref())
            else {
                continue;
            };
            let Some(target_index) = self.live.iter().position(|l| l.config_key == target) else {
                continue;
            };
            let Some(&target_position) = positions.get(target_index) else {
                continue;
            };
            if let Some(position) = positions.get_mut(index) {
                *position = target_position;
            }
        }
        // Logical coordinates may be negative, but the renderer's combined
        // texture starts at zero. Preserve relative arrangement and
        // translate the whole monitor island to the origin.
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
        positions
    }

    /// The public output table for the current live set and `config`.
    fn output_meta_for(
        &self,
        config: &settings_store::hardware::DisplayConfig,
    ) -> HashMap<OutputId, Output> {
        self.live
            .iter()
            .map(|l| {
                (
                    l.id,
                    Output {
                        id: l.id,
                        position: l.position,
                        logical_position: l.logical_position,
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
                            .get(&l.config_key)
                            .and_then(|output| output.color_profile.as_ref())
                            .filter(|profile| self.color_profiles.contains_key(*profile))
                            .cloned(),
                        refresh_mhz: l.refresh.and_then(crate::compositor::backend::interval_to_mhz),
                        // The collision-free key, so the `wl_output` name
                        // is the output-management head's name.
                        identity: OutputIdentity::from_monitor(
                            &l.config_key,
                            l.monitor.as_ref(),
                            l.physical_mm,
                        ),
                    },
                )
            })
            .collect()
    }

    /// Point every cursor plane at its output's rectangle in the *new* layout — a stale rect routes the pointer onto the wrong monitor.
    fn rebuild_cursor_routing(&mut self) {
        self.rebuild_cursor_planes();
        self.hw_cursor.move_to(self.pointer.0, self.pointer.1);
    }

    /// Every live output in both the logical layout and the scene.
    pub(crate) fn placements(&self) -> Vec<layout::Placement> {
        self.live
            .iter()
            .map(|l| layout::Placement {
                logical_position: l.logical_position,
                logical_size: layout::logical_extent(l.size, l.scale, l.transform),
                scene_position: l.position,
                scene_size: logical_output_size(l.size, l.transform),
            })
            .collect()
    }

    /// Put the pointer at a logical point (already on an output) and derive
    /// its scene position and the hardware cursor from it.
    fn set_pointer_logical(&mut self, logical: (f64, f64)) {
        self.logical_pointer = logical;
        self.pointer = layout::logical_to_scene(&self.placements(), logical);
        self.hw_cursor.move_to(self.pointer.0, self.pointer.1);
    }

    /// Relative pointer motion, in logical pixels. Returns the new scene
    /// position.
    /// Keep a confined pointer inside its region after it moved from
    /// `previous`, returning where it ends up (scene coordinates).
    fn confine_pointer(&mut self, previous: (f64, f64)) -> (f64, f64) {
        let moved = self.pointer;
        match self.pointer_constraint.constrain(previous, moved) {
            Some(confined) if confined != moved => {
                self.set_pointer_scene(confined);
                confined
            }
            _ => moved,
        }
    }

    fn move_pointer_by(&mut self, delta: (f64, f64)) -> (f64, f64) {
        let logical = layout::move_pointer(&self.placements(), self.logical_pointer, delta);
        self.set_pointer_logical(logical);
        self.pointer
    }

    /// Put the pointer at a scene point — for input calibrated to a panel's
    /// scene rectangle, such as a touchscreen — keeping the logical pointer
    /// in step so the next relative motion continues from there.
    fn set_pointer_scene(&mut self, scene: (f64, f64)) {
        self.pointer = scene;
        self.logical_pointer = layout::scene_to_logical(&self.placements(), scene);
        self.hw_cursor.move_to(scene.0, scene.1);
    }

    fn rebuild_cursor_planes(&mut self) {
        for entry in &self.live {
            let logical = logical_output_size(entry.size, entry.transform);
            self.hw_cursor.set_rect(
                entry.id,
                (entry.position.0, entry.position.1, logical.0, logical.1),
            );
            self.hw_cursor.set_transform(entry.id, entry.transform);
            self.hw_cursor.set_scale(entry.id, entry.scale);
            self.command(ScanoutCommand::SetCursorScale {
                output: entry.id,
                scale: entry.scale,
            });
        }
    }

    /// The public output table, in `OutputId` order.
    /// The published outputs in the order [`Self::relayout_with`] keeps the
    /// live set: the primary chosen in Settings first, else the built-in
    /// panel, then by id. Everything downstream takes the first as primary.
    pub(super) fn output_table(&self) -> Vec<Output> {
        self.live
            .iter()
            .filter_map(|l| self.output_meta.get(&l.id).cloned())
            .collect()
    }

    /// Perform one step of the pause/resume sequence the session state machine asked for.
    fn with_presentations_quiesced<T>(gate: Option<&Arc<Mutex<()>>>, f: impl FnOnce() -> T) -> T {
        let _quiesced = super::quiesce_presentations(gate);
        f()
    }

    fn perform(&mut self, action: SessionAction) {
        match action {
            SessionAction::PauseScanout => {
                self.command(ScanoutCommand::Pause);
            }
            SessionAction::DropMaster => {
                if !self.cards.is_empty() || !self.retiring_cards.is_empty() {
                    // The Pause command is asynchronous once the scanouts
                    // live on the presenter thread. Holding this gate until
                    // the release completes closes the window in which a
                    // page flip could race the loss of DRM master.
                    Self::with_presentations_quiesced(self.present_gate.as_ref(), || {
                        release_masters_for_pause(
                            self.cards.values(),
                            self.retiring_cards.values(),
                        );
                    });
                }
            }
            SessionAction::AcquireMaster => {
                apply_master_action(self.cards.values(), MasterAction::Acquire);
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

    /// Retire input state when its source cannot promise matching key-up
    /// events.  The compositor owns the authoritative Wayland-seat reset via
    /// `InputStateReset`; this clears only the DRM adapter state which would
    /// otherwise translate the next native key with stale modifiers or repeat
    /// it before that reset reaches the seat.
    fn reset_keyboard_input_source(&mut self) {
        // A session/source reset can lose touch-up just as readily as key-up.
        // Retire both the shell arena's raw contacts and the client's Wayland
        // touch focus before another device or resumed session reuses a slot.
        self.handle_direct_touch_cancel();
        self.key_repeat.cancel();
        self.slow_pending.clear();
        self.keyboard_key_owners.clear();
        self.bounce_keys.reset();
        self.release_mouse_keys();
        self.sticky_latched = ModifiersState::empty();
        self.physical_modifiers = ModifiersState::empty();
        if let Some(keyboard) = self.keyboard.as_mut() {
            keyboard.reset_state();
        }
        self.push(BackendEvent::InputStateReset);
    }

    /// Rebuild the display after system suspend.  Unlike a VT transition,
    /// suspend does not cause libseat to revoke the session, so it also has to
    /// explicitly retire keyboard state whose releases occurred while asleep.
    fn resume_from_sleep(&mut self) {
        self.reset_keyboard_input_source();
        let actions = match self.session.as_ref() {
            Some(session) => session.resume_actions(),
            None => Vec::new(),
        };
        self.perform_all(actions);
    }

    /// Run a whole sequence in order.
    pub fn perform_all(&mut self, actions: impl IntoIterator<Item = SessionAction>) {
        for a in actions {
            self.perform(a);
        }
    }

    /// Apply the current input settings to one libinput device.
    fn configure_device(&self, device: &mut smithay::reexports::input::Device) {
        let plan = input_config::configure(&self.input, device);
        tracing::info!(
            device = device.name(),
            kind = ?device_caps(device).kind(),
            tap = ?plan.tap,
            natural_scroll = ?plan.natural_scroll,
            accel = ?plan.accel_speed(),
            click_method = ?plan.click_method,
            scroll_method = ?plan.scroll_method,
            "DRM input: configured device"
        );
    }

    /// Tell Settings which kinds of input device are attached, so it shows
    /// only the sections that apply. Failure is benign: no runtime
    /// directory in a test session, and the devices themselves are unaffected.
    fn publish_input_snapshot(&self) {
        let (publisher_pid, publisher_start_ticks) =
            match settings_store::hardware::publisher_identity() {
                Ok(identity) => identity,
                Err(error) => {
                    tracing::debug!(%error, "DRM input: no publisher lease for the snapshot");
                    return;
                }
            };
        let snapshot = settings_store::hardware::InputHardwareSnapshot {
            publisher_pid,
            publisher_start_ticks,
            devices: self
                .devices
                .iter()
                .map(|device| {
                    let key = self
                        .keyboard_identities
                        .get(device.sysname())
                        .cloned()
                        .unwrap_or_else(|| libinput_device_key(device));
                    input_device_snapshot(device, key)
                })
                .collect(),
            active_keyboard: self.keyboard_device.clone(),
        };
        if let Err(error) = snapshot.save() {
            tracing::debug!(%error, "DRM input: could not publish device snapshot");
        }
    }

    /// Remember libinput's output association while the device is live.
    ///
    /// A touchscreen's coordinates are relative to its own panel, never to
    /// the desktop bounding box. `output_name` is libinput's authoritative
    /// association when udev has one; a missing association deliberately
    /// falls back to the first (primary) live output in [`touch_position`],
    /// rather than allowing an internal panel to span external monitors.
    fn remember_touch_output_binding(&mut self, device: &smithay::reexports::input::Device) {
        if !device.has_capability(smithay::reexports::input::DeviceCapability::Touch) {
            return;
        }
        let id = device.id();
        let output_name = device.output_name().map(str::to_owned);
        if let Some(output_name) = output_name.as_deref()
            && self
                .live
                .iter()
                .filter(|output| output.connector_name == output_name)
                .nth(1)
                .is_some()
        {
            // Libinput's metadata names a connector but carries no DRM card
            // identity. Do not let iteration order silently bind this touch
            // device to one of two same-named connectors; touch events use
            // the documented primary-output fallback below.
            tracing::warn!(
                device = %id,
                output = output_name,
                "DRM input: touchscreen output association is ambiguous; using primary fallback"
            );
        }
        tracing::debug!(
            device = %id,
            output = ?output_name,
            "DRM input: recorded touchscreen output association"
        );
        self.touch_output_bindings.insert(id, output_name);
    }

    /// Route one gesture: to the shell when the finger count says so, otherwise on to the focused client.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "gesture events are consumed as protocol state-machine inputs"
    )]
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
            GestureEvent::SwipeEnd { cancelled } => self.gestures.swipe_end_with_cancel(cancelled),
            GestureEvent::HoldEnd { .. } => {
                let _ = self.gestures.swipe_end();
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

    /// Switch a physical assignment before translating its first key. Replaced maps
    /// retire held state; cached maps are reset rather than carrying old modifiers.
    fn apply_keyboard_assignment(&mut self, scope: &str) -> bool {
        let settings = KeyboardSettings::from_config(&settings_store::keyboard_sources::effective(
            &self.keyboard_config,
            scope,
        ));
        if let Some(keyboard) = self
            .keyboard
            .as_mut()
            .filter(|keyboard| keyboard.compiles_to_same_keymap(&settings))
        {
            keyboard.set_active_layout(settings.active_index);
            self.push(BackendEvent::KeyboardConfiguration {
                scope: scope.into(),
                settings: Box::new(settings),
            });
            return true;
        }
        let candidate = self
            .keyboard_cache
            .remove(scope)
            .filter(|keyboard| keyboard.compiles_to_same_keymap(&settings))
            .or_else(|| KeyboardXkb::from_settings(&settings));
        let Some(mut candidate) = candidate else {
            tracing::warn!(
                scope,
                "keyboard assignment could not compile; retaining working input"
            );
            return self.keyboard.is_some();
        };
        candidate.reset_state();
        candidate.set_active_layout(settings.active_index);
        if let Some(mut previous) = self.keyboard.replace(candidate) {
            previous.reset_state();
            if let Some(previous_scope) = &self.keyboard_device {
                self.keyboard_cache.insert(previous_scope.clone(), previous);
            }
        }
        self.keyboard_key_owners.clear();
        self.key_repeat.cancel();
        self.slow_pending.clear();
        self.physical_modifiers = ModifiersState::empty();
        self.sticky_latched = ModifiersState::empty();
        self.push(BackendEvent::ModifiersChanged(Modifiers::default()));
        self.push(BackendEvent::InputStateReset);
        self.push(BackendEvent::KeyboardConfiguration {
            scope: scope.into(),
            settings: Box::new(settings),
        });
        true
    }

    fn handle_keyboard_device_key(
        &mut self,
        scope: &str,
        keycode: xkb::Keycode,
        pressed: bool,
        time: Option<u64>,
    ) {
        if pressed {
            if self.keyboard_device.as_deref() != Some(scope) {
                if !self.apply_keyboard_assignment(scope) {
                    return;
                }
                self.keyboard_device = Some(scope.to_owned());
                self.publish_input_snapshot();
            }
            let owners = self.keyboard_key_owners.entry(keycode.raw()).or_default();
            if !owners.insert(scope.to_owned()) || owners.len() > 1 {
                return;
            }
        } else {
            let Some(owners) = self.keyboard_key_owners.get_mut(&keycode.raw()) else {
                return;
            };
            if !owners.remove(scope) || !owners.is_empty() {
                return;
            }
            self.keyboard_key_owners.remove(&keycode.raw());
        }
        self.handle_keyboard_key(keycode, pressed, time);
    }

    fn retire_keyboard_device(&mut self, scope: &str) {
        let mut released = Vec::new();
        self.keyboard_key_owners.retain(|keycode, owners| {
            owners.remove(scope);
            if owners.is_empty() {
                released.push(*keycode);
                false
            } else {
                true
            }
        });
        for keycode in released {
            self.handle_keyboard_key(xkb::Keycode::new(keycode), false, None);
        }
    }

    /// Emit modifier changes followed by the translated keyboard input.
    /// `keycode` already includes the evdev-to-XKB offset.
    fn handle_keyboard_key(
        &mut self,
        keycode: xkb::Keycode,
        pressed: bool,
        input_time_ns: Option<u64>,
    ) {
        let evdev = keycode.raw().saturating_sub(8);
        let now = std::time::Instant::now();
        if self.accessibility.bounce_keys {
            let verdict = if pressed {
                self.bounce_keys
                    .press(evdev, now, self.accessibility.bounce_window())
            } else {
                self.bounce_keys.release(evdev, now)
            };
            if verdict == BounceVerdict::Reject {
                if pressed && self.accessibility.bounce_keys_alert {
                    self.push(BackendEvent::VisualAlert);
                }
                return;
            }
        }
        if self.accessibility.mouse_keys
            && let Some(output) = self.mouse_keys.key(evdev, pressed, now)
        {
            self.apply_mouse_keys(output);
            return;
        }
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
        self.process_keyboard_key_timed(keycode, pressed, input_time_ns);
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

    /// Deliver what mouse keys produced: a nudge moves the pointer exactly as
    /// relative device motion would, and button edges arrive as clicks.
    fn apply_mouse_keys(&mut self, output: MouseKeysOutput) {
        self.move_pointer_from_keys(output.motion);
        for edge in output.buttons {
            self.push_mouse_keys_button(edge);
        }
    }

    fn move_pointer_from_keys(&mut self, (dx, dy): (f64, f64)) {
        if dx == 0.0 && dy == 0.0 {
            return;
        }
        let (x, y) = self.move_pointer_by((dx, dy));
        // Synthetic motion has no acceleration curve, and libinput stamps
        // real motion on CLOCK_MONOTONIC, so the keys do the same.
        self.push(BackendEvent::RelativePointerMotion {
            dx,
            dy,
            dx_unaccel: dx,
            dy_unaccel: dy,
            time_usec: crate::shell::network::fabric_monotonic_us(),
        });
        self.push(BackendEvent::Input(cursor_moved(x, y)));
    }

    fn push_mouse_keys_button(&mut self, edge: ButtonEdge) {
        let button = match edge.button {
            PointerButton::Left => MouseButton::Left,
            PointerButton::Middle => MouseButton::Middle,
            PointerButton::Right => MouseButton::Right,
        };
        let state = if edge.pressed {
            ElementState::Pressed
        } else {
            ElementState::Released
        };
        self.push(BackendEvent::Input(mouse_input_event(state, button)));
    }

    /// Stop any glide and release every button mouse keys is holding, so
    /// switching it off mid-drag never leaves a button stuck down.
    fn release_mouse_keys(&mut self) {
        for edge in self.mouse_keys.reset() {
            self.push_mouse_keys_button(edge);
        }
    }

    fn pump_mouse_keys(&mut self, now: std::time::Instant) {
        if self.accessibility.mouse_keys {
            let motion = self.mouse_keys.motion(now);
            self.move_pointer_from_keys(motion);
        }
    }

    fn pump_keyboard_repeat(&mut self, now: std::time::Instant) {
        for keycode in self.key_repeat.due(now) {
            let input = self
                .keyboard
                .as_ref()
                .map(|keyboard| keyboard.repeat_event(keycode));
            if let Some(input) = input {
                self.events.push(BackendEvent::Input(input));
            }
        }
    }

    fn process_keyboard_key(&mut self, keycode: xkb::Keycode, pressed: bool) {
        self.process_keyboard_key_timed(keycode, pressed, None);
    }

    fn process_keyboard_key_timed(
        &mut self,
        keycode: xkb::Keycode,
        pressed: bool,
        input_time_ns: Option<u64>,
    ) {
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
            self.events.push(match input_time_ns {
                Some(input_time_ns) => BackendEvent::PhysicalKey {
                    event: input,
                    input_time_ns,
                },
                None => BackendEvent::Input(input),
            });
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
                // VT/session loss cuts raw libinput delivery. Pair the
                // repeat cancellation with a seat-level key reset instead
                // of assuming every held key will get an up event later.
                self.reset_keyboard_input_source();
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
                self.add_card(path.as_ref(), device_id_from_dev_t(device_id));
                self.rescan_outputs("udev device added");
            }
            UdevEvent::Changed { device_id } => {
                tracing::debug!(?device_id, "DRM: udev device changed");
                self.rescan_outputs("udev device changed");
            }
            UdevEvent::Removed { device_id } => {
                tracing::info!(?device_id, "DRM: udev device removed");
                // `device_id` is the node's `dev_t`, the same value used as
                // our card/output key.  Remove it from the active scanout
                // map before rescanning, but retain the original libseat fd
                // until the presenter has acknowledged dropping all of its
                // duplicate scanout fds.  Dropping `OwnedFd` alone would
                // leak libseat's tracked Device and make an eGPU re-add fail.
                self.retire_card(device_id_from_dev_t(device_id));
            }
        }
    }

    /// Begin ordered teardown for one removed KMS card.  The active card map
    /// must lose the card before the hotplug diff runs, while the separate
    /// retirement map keeps ownership of the original session fd until its
    /// presenter duplicates are gone.
    fn retire_card(&mut self, device_id: u64) {
        if self.card_leases.remove_requested(device_id) != CardLeaseAction::DetachPresenter {
            self.rescan_outputs("udev unknown device removed");
            return;
        }
        let Some(card) = self.cards.remove(&device_id) else {
            // Keep the fd-bearing map and the pure ownership map from ever
            // drifting silently if a future caller bypasses `add_card`.
            tracing::warn!(device_id, "DRM: card ownership state had no active handle");
            self.card_leases.retiring.remove(&device_id);
            self.rescan_outputs("udev unknown device removed");
            return;
        };
        self.retiring_cards.insert(device_id, card);
        let removal_ack = self
            .card_removal_ack_sender
            .clone()
            .map(|ack| (device_id, ack));
        self.rescan_outputs_with_removal_ack(
            "udev device removed",
            removal_ack,
            ConnectorProbe::Force,
        );
    }

    /// Remember that libseat could not close a device. Reopening it in this
    /// session is unsafe even if it returns as another card node, so guard by
    /// both the observed node and the stable physical sysfs identity.
    fn block_card_after_close_failure(
        &mut self,
        device_id: u64,
        path: &Path,
        physical_identity: Option<&str>,
    ) {
        self.failed_card_close_paths.insert(path.to_path_buf());
        if let Some(identity) = physical_identity {
            self.failed_card_close_identities
                .insert(identity.to_owned());
        } else {
            self.unidentified_card_close_failure = true;
        }
        self.card_leases.failed_closes.insert(device_id);
    }

    /// Close exactly one `Session::open` fd and install the same fail-closed
    /// guard for retirement and duplicate-open cleanup. Smithay removes its
    /// tracked `libseat::Device` before `close_device` can return an error,
    /// so retrying an open here could orphan a second lease.
    fn close_session_card_fd(
        &mut self,
        fd: OwnedFd,
        device_id: u64,
        path: &Path,
        physical_identity: Option<&str>,
        context: &str,
    ) -> bool {
        let result = if let Some(seat) = self.seat.as_mut() {
            seat.close(fd).map_err(|error| error.to_string())
        } else {
            drop(fd);
            Err("no libseat session clone".into())
        };
        match result {
            Ok(()) => {
                tracing::info!(device_id, device = %path.display(), context, "DRM: closed libseat card");
                true
            }
            Err(error) => {
                self.block_card_after_close_failure(device_id, path, physical_identity);
                tracing::error!(
                    device_id,
                    device = %path.display(),
                    ?physical_identity,
                    %error,
                    context,
                    "DRM: libseat close failed; refusing this physical card until a fresh session"
                );
                false
            }
        }
    }

    /// Complete card teardowns acknowledged by the scanout owner.  This is
    /// called only on the loop thread, where the libseat session lives.
    ///
    /// Returns whether a deferred udev add was opened and therefore requires
    /// a fresh connector rescan.
    fn finish_retired_cards(&mut self) -> bool {
        let completed: Vec<u64> = self
            .card_removal_acks
            .as_ref()
            .map(|acks| acks.try_iter().collect())
            .unwrap_or_default();
        let mut deferred_adds = Vec::new();
        for device_id in completed {
            if self.card_leases.presenter_detached(device_id) != CardLeaseAction::CloseSessionFd {
                continue;
            }
            let Some(card) = self.retiring_cards.remove(&device_id) else {
                continue;
            };
            let card_path = card.path().to_path_buf();
            let physical_identity = card.physical_identity().map(str::to_owned);
            let fd = card.into_session_fd();
            let close_succeeded = self.close_session_card_fd(
                fd,
                device_id,
                &card_path,
                physical_identity.as_deref(),
                "retired card",
            );
            if self
                .card_leases
                .session_close_finished(device_id, close_succeeded)
                == CardLeaseAction::ReopenDeferred
                && let Some((readd_device_id, path)) = self.deferred_card_adds.remove(&device_id)
            {
                deferred_adds.push((readd_device_id, path));
            } else if !close_succeeded {
                self.deferred_card_adds.remove(&device_id);
            }
        }
        for (device_id, path) in &deferred_adds {
            self.add_card(path, *device_id);
        }
        !deferred_adds.is_empty()
    }

    /// Retain a KMS card announced after startup.  Normal hybrid laptops
    /// already have both cards when the session begins, but docks and eGPUs
    /// can create a `cardN` later.  Keeping the fd is also what lets later
    /// connector edges use the same libseat-granted node rather than trying
    /// an illegal second `open`.
    fn add_card(&mut self, path: &Path, udev_device_id: u64) {
        let physical_identity = physical_card_identity(path);
        if self.unidentified_card_close_failure
            || self.failed_card_close_paths.contains(path)
            || physical_identity
                .as_ref()
                .is_some_and(|identity| self.failed_card_close_identities.contains(identity))
        {
            tracing::error!(
                device = %path.display(),
                udev_device_id,
                ?physical_identity,
                "DRM: refusing card re-add after libseat close failure; restart the session"
            );
            return;
        }
        if let Some((&retiring_device_id, _)) = self
            .retiring_cards
            .iter()
            .find(|(_, card)| card.same_physical_node(path))
        {
            // eGPUs and some USB display adapters may return as a new cardN
            // (and therefore a different dev_t) before the old fd is closed.
            // Physical identity matching keeps that GPU from receiving a
            // second libseat open even if it returned as another cardN.
            self.card_leases.deferred_readds.insert(retiring_device_id);
            self.deferred_card_adds
                .insert(retiring_device_id, (udev_device_id, path.to_path_buf()));
            tracing::debug!(
                device = %path.display(),
                udev_device_id,
                retiring_device_id,
                "DRM: deferred card re-add until old libseat fd closes"
            );
            return;
        }
        if self.card_leases.add_requested(udev_device_id) == CardLeaseAction::DeferReadd {
            // A physical re-add can race the render thread's teardown of the
            // old card.  Opening now would make libseat track two opens for
            // one node, so retain the newest path and reopen only after the
            // presenter acknowledgement reaches `finish_retired_cards`.
            self.deferred_card_adds
                .insert(udev_device_id, (udev_device_id, path.to_path_buf()));
            tracing::debug!(device = %path.display(), udev_device_id, "DRM: deferred card re-add until old libseat fd closes");
            return;
        }
        if self.card_leases.add_requested(udev_device_id) == CardLeaseAction::BlockedByCloseFailure
        {
            tracing::error!(
                device = %path.display(),
                udev_device_id,
                "DRM: refusing card re-add after libseat close failure; restart the session"
            );
            return;
        }
        if self.card_leases.active.contains(&udev_device_id) {
            tracing::debug!(device = %path.display(), udev_device_id, "DRM: card is already retained");
            return;
        }
        if !is_kms_node(path) || self.cards.values().any(|card| card.path() == path) {
            return;
        }
        let Some(mut seat) = self.seat.clone() else {
            tracing::debug!(device = %path.display(), "DRM: no seat clone for newly added card");
            return;
        };
        match CardHandle::open(&mut seat, path) {
            Ok(card) => {
                let device_id = card.device_id();
                if device_id != udev_device_id {
                    // Both identities originate from the same kernel dev_t
                    // (`fstat(card_fd).st_rdev` and UdevEvent::Added), so a
                    // mismatch means a racing node replacement. Keep the fd
                    // identity authoritative for scanout and log the race
                    // instead of attaching outputs to the wrong key.
                    tracing::warn!(
                        device = %path.display(),
                        udev_device_id,
                        card_device_id = device_id,
                        "DRM: added card dev_t changed while opening"
                    );
                }
                if self.cards.contains_key(&device_id) {
                    tracing::debug!(device = %path.display(), device_id, "DRM: card is already retained");
                    // `CardHandle::open` used `Session::open` before the
                    // duplicate identity was detected.  Balance it through
                    // libseat rather than merely dropping its OwnedFd.
                    let card_path = card.path().to_path_buf();
                    let card_physical_identity = card.physical_identity().map(str::to_owned);
                    let fd = card.into_session_fd();
                    self.close_session_card_fd(
                        fd,
                        device_id,
                        &card_path,
                        card_physical_identity.as_deref(),
                        "duplicate added card",
                    );
                    return;
                }
                self.cards.insert(device_id, card);
                self.card_leases.opened(device_id);
                tracing::info!(device = %path.display(), device_id, "DRM: retained newly added KMS card");
            }
            Err(error) => {
                tracing::warn!(device = %path.display(), %error, "DRM: cannot open added KMS card");
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
            InputEvent::PointerMotionAbsolute { .. } | InputEvent::PointerMotion { .. } => {
                self.note_pointer_motion();
                self.handle_libinput_pointer_event(event);
            }
            InputEvent::PointerButton { .. } | InputEvent::PointerAxis { .. } => {
                self.note_input_used(ui::InputModality::Pointer);
                self.handle_libinput_pointer_event(event);
            }
            // Keyboard: run the libinput keycode through xkb to get a
            // logical key + text, then enqueue a winit-shaped
            // `KeyboardInput` (plus `ModifiersChanged` on a modifier flip)
            // — the same wire the winit backend feeds, so `dispatch_event` handles DRM keystrokes identically to hosted-mode ones.
            InputEvent::Keyboard { event } => {
                let pressed = matches!(event.state(), KeyState::Pressed);
                if pressed {
                    self.note_input_used(ui::InputModality::Keyboard);
                }
                // libinput time_usec is the original kernel monotonic event time.
                let device = event.device();
                let scope = self
                    .keyboard_identities
                    .get(device.sysname())
                    .cloned()
                    .unwrap_or_else(|| libinput_device_key(&device));
                self.handle_keyboard_device_key(
                    &scope,
                    event.key_code(),
                    pressed,
                    event.time().checked_mul(1000),
                );
            }
            InputEvent::GestureSwipeBegin { .. }
            | InputEvent::GestureSwipeUpdate { .. }
            | InputEvent::GestureSwipeEnd { .. }
            | InputEvent::GesturePinchBegin { .. }
            | InputEvent::GesturePinchUpdate { .. }
            | InputEvent::GesturePinchEnd { .. }
            | InputEvent::GestureHoldBegin { .. }
            | InputEvent::GestureHoldEnd { .. } => {
                self.note_input_used(ui::InputModality::Pointer);
                self.handle_libinput_gesture_event(event);
            }
            InputEvent::TouchDown { .. } => {
                self.note_input_used(ui::InputModality::Touch);
                self.handle_libinput_touch_event(event);
            }
            InputEvent::TouchMotion { .. }
            | InputEvent::TouchUp { .. }
            | InputEvent::TouchCancel { .. }
            | InputEvent::TouchFrame { .. } => {
                self.handle_libinput_touch_event(event);
            }
            // Using a stylus means the person is pointing rather than typing
            // or touching.
            InputEvent::TabletToolAxis { .. }
            | InputEvent::TabletToolProximity { .. }
            | InputEvent::TabletToolTip { .. }
            | InputEvent::TabletToolButton { .. } => {
                self.note_input_used(ui::InputModality::Pointer);
                self.handle_libinput_tablet_event(&event);
            }
            // Only the tablet-mode switch matters here. Other switches (the
            // lid) are handled elsewhere or not at all; unlike the tools above
            // they were never advertised to clients, so dropping them promises
            // nothing undelivered.
            InputEvent::SwitchToggle { event } => {
                use smithay::backend::input::{Switch, SwitchState, SwitchToggleEvent};
                if SwitchToggleEvent::switch(&event) == Some(Switch::TabletMode) {
                    self.push(BackendEvent::TabletMode(matches!(
                        SwitchToggleEvent::state(&event),
                        SwitchState::On
                    )));
                }
            }
            // Tablet pads are read from their own context (`PadInput`), since
            // this backend drops their events.
            InputEvent::Special(_) => {}
        }
    }

    /// Tablet-tool half of [`Self::handle_libinput_event`]: every axis the
    /// tool reports, shaped by that tablet's saved settings — the display it
    /// maps onto, aspect ratio, orientation, pressure curve — and its barrel
    /// buttons remapped, or turned into a shortcut.
    fn handle_libinput_tablet_event(&mut self, event: &InputEvent<LibinputInputBackend>) {
        use smithay::backend::input::{
            ProximityState, TabletToolButtonEvent, TabletToolProximityEvent, TabletToolTipEvent,
            TabletToolTipState,
        };
        let device = match event {
            InputEvent::TabletToolAxis { event } => event.device(),
            InputEvent::TabletToolProximity { event } => event.device(),
            InputEvent::TabletToolTip { event } => event.device(),
            InputEvent::TabletToolButton { event } => event.device(),
            _ => return,
        };
        let config = self.tablets.config_for(&device);
        let Some(target) = super::tablet::pen_target(
            self.live.iter().map(|output| {
                (
                    output.connector_name.as_str(),
                    output.config_key.as_str(),
                    output.position,
                    logical_output_size(output.size, output.transform),
                )
            }),
            config.output.as_deref(),
        ) else {
            return;
        };
        let input = match event {
            InputEvent::TabletToolAxis { event } => {
                super::tablet::tablet_input(event, TabletPhase::Motion, target, &config)
            }
            InputEvent::TabletToolProximity { event } => super::tablet::tablet_input(
                event,
                match TabletToolProximityEvent::state(event) {
                    ProximityState::In => TabletPhase::ProximityIn,
                    ProximityState::Out => TabletPhase::ProximityOut,
                },
                target,
                &config,
            ),
            InputEvent::TabletToolTip { event } => super::tablet::tablet_input(
                event,
                match TabletToolTipEvent::tip_state(event) {
                    TabletToolTipState::Down => TabletPhase::TipDown,
                    TabletToolTipState::Up => TabletPhase::TipUp,
                },
                target,
                &config,
            ),
            InputEvent::TabletToolButton { event } => {
                let pressed = matches!(
                    TabletToolButtonEvent::button_state(event),
                    ButtonState::Pressed
                );
                let button = match super::tablet::stylus_button(
                    &config,
                    TabletToolButtonEvent::button(event),
                ) {
                    super::tablet::ButtonOutcome::Button(button) => button,
                    super::tablet::ButtonOutcome::Shortcut(action) => {
                        if pressed {
                            self.press_shortcut(action);
                        }
                        return;
                    }
                    super::tablet::ButtonOutcome::Ignore => return,
                };
                super::tablet::tablet_input(
                    event,
                    TabletPhase::Button { button, pressed },
                    target,
                    &config,
                )
            }
            _ => return,
        };
        if input.tool.kind == crate::compositor::backend::TabletToolKind::Eraser {
            let tip = match super::tablet::eraser_route(&config) {
                super::tablet::EraserRoute::Native => {
                    self.push(BackendEvent::Tablet(input));
                    return;
                }
                super::tablet::EraserRoute::Pointer(button) => Some(button),
                super::tablet::EraserRoute::Shortcut(action) => {
                    if input.phase == TabletPhase::TipDown {
                        self.press_shortcut(action);
                    }
                    None
                }
            };
            // A remapped eraser is a pointer in every app, never an eraser.
            for event in crate::compositor::tablet::tool_pointer_events(&input, tip) {
                self.push(BackendEvent::Input(event));
            }
            return;
        }
        self.push(BackendEvent::Tablet(input));
    }

    /// The pad half of a tablet. A mapped button presses its shortcut's
    /// chord; everything else goes to the compositor, which hands it to a
    /// focused tablet-aware app or applies the fallback built here from the
    /// pad's settings: a ring or strip scrolling, zooming, resizing the
    /// brush, or stepping the volume or brightness.
    fn handle_pad_events(&mut self) {
        use crate::compositor::backend::{PadAction, PadEvent};
        use settings_store::hardware::PadDial;
        let Some(pads) = self.tablets.pads.as_mut() else {
            return;
        };
        for event in pads.take_events() {
            match event {
                RawPadEvent::Button {
                    pad,
                    button,
                    pressed,
                    time,
                    mode_switch,
                    mode_button,
                } => {
                    if let Some((group, mode, modes)) = mode_switch {
                        self.push(BackendEvent::TabletPad(PadEvent {
                            pad: pad.clone(),
                            action: PadAction::ModeSwitch { group, mode, modes },
                            time_msec: time,
                            fallback: Vec::new(),
                        }));
                    }
                    // Holding a mode button, or the button picked for it,
                    // shows what every control does.
                    let config = self.tablets.configs.get(&pad);
                    let map_button = config.is_some_and(|c| c.pad_map_button == Some(button));
                    if mode_button || map_button {
                        let shown = pressed.then(|| Box::new(config.cloned().unwrap_or_default()));
                        self.push(BackendEvent::TabletPadMap {
                            pad: pad.clone(),
                            shown,
                            time_msec: time,
                        });
                    }
                    if map_button {
                        continue;
                    }
                    let shortcut = self
                        .tablets
                        .configs
                        .get(&pad)
                        .and_then(|config| super::tablet::pad_button(config, button));
                    if let Some(action) = shortcut {
                        if pressed {
                            self.press_shortcut(action);
                        }
                        continue;
                    }
                    self.push(BackendEvent::TabletPad(PadEvent {
                        pad,
                        action: PadAction::Button { button, pressed },
                        time_msec: time,
                        fallback: Vec::new(),
                    }));
                }
                RawPadEvent::Dial {
                    pad,
                    dial,
                    position,
                    finger,
                    mode,
                    time,
                } => {
                    let steps = self.tablets.dials.advance(&pad, dial, position);
                    let mapped = self
                        .tablets
                        .configs
                        .get(&pad)
                        .map(|config| config.dial_action(dial, mode))
                        .unwrap_or_default();
                    let fallback =
                        crate::compositor::tablet_pad::dial_events(mapped, steps, |step| {
                            self.dial_key_events(step)
                        });
                    let action = match dial {
                        PadDial::Ring(ring) => PadAction::Ring {
                            ring,
                            angle: position,
                            finger,
                        },
                        PadDial::Strip(strip) => PadAction::Strip {
                            strip,
                            position,
                            finger,
                        },
                    };
                    self.push(BackendEvent::TabletPad(PadEvent {
                        pad,
                        action,
                        time_msec: time,
                        fallback,
                    }));
                }
            }
        }
    }

    /// The keyboard events one ring or strip step presses and releases,
    /// kept aside rather than queued: they apply only if no tablet-aware app
    /// takes the step itself.
    fn dial_key_events(
        &mut self,
        step: &crate::compositor::tablet_pad::DialStep,
    ) -> Vec<BackendEvent> {
        let codes = match step {
            crate::compositor::tablet_pad::DialStep::Key(code) => vec![*code],
            crate::compositor::tablet_pad::DialStep::Chord(chord) => {
                let keyboard = self.keyboard.as_ref();
                let Some(codes) = super::tablet::chord_keycodes(chord, |ch| {
                    keyboard.and_then(|keyboard| keyboard.keycode_for_char(ch))
                }) else {
                    tracing::debug!(?chord, "DRM input: pad key is not on this layout");
                    return Vec::new();
                };
                codes
            }
        };
        let start = self.events.len();
        for code in &codes {
            self.process_keyboard_key(evdev_to_xkb(*code), true);
        }
        for code in codes.iter().rev() {
            self.process_keyboard_key(evdev_to_xkb(*code), false);
        }
        self.events.split_off(start)
    }

    /// Press and release the chord bound to `action`, through the ordinary
    /// keyboard path, so the one shortcut resolver decides what it does and
    /// a rebound shortcut moves its pen and pad buttons with it.
    fn press_shortcut(&mut self, action: settings_store::hardware::ShortcutAction) {
        let Some(chord) = self.tablets.shortcuts.bindings.get(&action) else {
            tracing::debug!(?action, "DRM input: pen shortcut has no chord bound");
            return;
        };
        let keyboard = self.keyboard.as_ref();
        let Some(codes) = super::tablet::chord_keycodes(chord, |ch| {
            keyboard.and_then(|keyboard| keyboard.keycode_for_char(ch))
        }) else {
            tracing::debug!(
                ?action,
                "DRM input: pen shortcut chord is not on this layout"
            );
            return;
        };
        for code in &codes {
            self.process_keyboard_key(evdev_to_xkb(*code), true);
        }
        for code in codes.iter().rev() {
            self.process_keyboard_key(evdev_to_xkb(*code), false);
        }
    }

    /// Adopt already queued startup devices without waiting for the first key
    /// or touch to make libinput's fd readable. Keep physical event translation
    /// in the registered Smithay backend.
    fn adopt_initial_input_devices(&mut self, context: &Libinput) -> Result<(), DrmBackendError> {
        use smithay::reexports::input::{AsRaw, event::EventTrait as _, ffi};
        let mut context = context.clone();
        // Do not dispatch or consume a key, contact or switch here: Smithay's
        // registered backend remains the sole translator of physical input.
        // This peek preserves any non-device event for that normal route.
        loop {
            // SAFETY: the live context is exclusively driven on this thread;
            // peeking changes neither its queue nor any event's ownership.
            let next = unsafe { ffi::libinput_next_event_type(context.as_raw_mut()) };
            if next != ffi::libinput_event_type_LIBINPUT_EVENT_DEVICE_ADDED {
                return Ok(());
            }
            let Some(smithay::reexports::input::Event::Device(
                smithay::reexports::input::event::DeviceEvent::Added(event),
            )) = context.next()
            else {
                return Err(DrmBackendError::Libinput(
                    "queued input device could not be adopted".into(),
                ));
            };
            self.handle_libinput_device_event(InputEvent::DeviceAdded {
                device: event.device(),
            });
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
                if device.keyboard_has_key(30).unwrap_or(false) {
                    self.keyboard_identities
                        .insert(device.sysname().to_owned(), libinput_device_key(&device));
                }
                self.remember_touch_output_binding(&device);
                self.push(BackendEvent::InputDeviceAdded {
                    id: device.id(),
                    capabilities: device_input_capabilities(&device),
                });
                if device.has_capability(smithay::reexports::input::DeviceCapability::TabletPad) {
                    if let Some(pads) = self.tablets.pads.as_mut() {
                        pads.add(&device);
                    }
                    self.push(BackendEvent::TabletPadAdded(pad_description(&device)));
                }
                self.devices.push(device);
                self.publish_input_snapshot();
            }
            InputEvent::DeviceRemoved { device } => {
                tracing::info!(device = device.name(), "DRM input: device removed");
                // libinput cannot deliver a release after the keyboard that
                // owned the key disappears. Never let a held editing key keep
                // firing into whichever field receives focus next.
                self.key_repeat.cancel();
                self.slow_pending.clear();
                if device.has_capability(smithay::reexports::input::DeviceCapability::Keyboard) {
                    self.bounce_keys.reset();
                    self.release_mouse_keys();
                    // A vanished keyboard cannot send the releases which
                    // clear Smithay's seat XKB state. Tell the compositor
                    // boundary to retire those keys before another device
                    // can type into a newly focused client.
                    self.push(BackendEvent::InputStateReset);
                }
                if device.has_capability(smithay::reexports::input::DeviceCapability::Touch) {
                    // Smithay touch cancellation is seat-wide, so a vanished
                    // touch source retires every current contact rather than
                    // risking a stale device-local slot being reused.
                    self.handle_direct_touch_source_removed();
                }
                if device.has_capability(smithay::reexports::input::DeviceCapability::TabletPad) {
                    self.push(BackendEvent::TabletPadRemoved(libinput_device_key(&device)));
                }
                if let Some(pads) = self.tablets.pads.as_mut() {
                    pads.remove(&device);
                }
                self.touch_output_bindings.remove(&device.id());
                self.push(BackendEvent::InputDeviceRemoved { id: device.id() });
                let removed_scope = self.keyboard_identities.remove(device.sysname());
                if let Some(scope) = &removed_scope {
                    self.keyboard_cache.remove(scope);
                    self.retire_keyboard_device(scope);
                }
                if self.keyboard_device.as_ref() == removed_scope.as_ref()
                    && removed_scope.is_some()
                {
                    self.keyboard_device = None;
                    self.keyboard_cache.clear();
                    self.apply_keyboard_assignment(settings_store::keyboard_sources::ALL);
                }
                self.devices.retain(|d| *d != device);
                self.publish_input_snapshot();
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
                // An absolute device (tablet, VM pointer) spans the logical
                // desktop, so equal travel covers equal logical distance on
                // every output.
                let placements = self.placements();
                let (w, h) = hotplug::combined_extent(
                    &placements
                        .iter()
                        .map(|p| p.logical_position)
                        .collect::<Vec<_>>(),
                    &placements
                        .iter()
                        .map(|p| p.logical_size)
                        .collect::<Vec<_>>(),
                );
                if self.pointer_constraint == PointerConstraintRegion::Locked {
                    return;
                }
                let previous = self.pointer;
                let requested = (event.x_transformed(w.max(1)), event.y_transformed(h.max(1)));
                self.set_pointer_logical(layout::clamp_to_outputs(&placements, requested));
                let (x, y) = self.confine_pointer(previous);
                self.push(BackendEvent::Input(cursor_moved(x, y)));
            }
            // Relative motion (mouse): accumulate in logical pixels, clamp to
            // the logical outputs, then present in the scene.
            InputEvent::PointerMotion { event } => {
                self.push(BackendEvent::RelativePointerMotion {
                    dx: event.delta_x(),
                    dy: event.delta_y(),
                    dx_unaccel: event.delta_x_unaccel(),
                    dy_unaccel: event.delta_y_unaccel(),
                    time_usec: event.time(),
                });
                // A locked pointer stays put; a confined one slides along
                // its region's edge. Relative motion above is reported
                // either way — that is what a game reads.
                if self.pointer_constraint == PointerConstraintRegion::Locked {
                    return;
                }
                let previous = self.pointer;
                self.move_pointer_by((event.delta_x(), event.delta_y()));
                let (x, y) = self.confine_pointer(previous);
                self.push(BackendEvent::Input(cursor_moved(x, y)));
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
                // The user's scroll speed scales wheel and finger scrolling
                // alike, before either becomes a line or pixel delta.
                let speed = self.input.scroll_speed;
                let dx = event.amount(Axis::Horizontal).unwrap_or(0.0) * speed;
                let dy = event.amount(Axis::Vertical).unwrap_or(0.0) * speed;
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
    /// to keep that dispatcher short. Every contact reaches the shell as raw
    /// touch. The first one or two contacts also reach `wl_touch`, ahead of
    /// their raw event so the compositor knows whether a client took them;
    /// a third transfers the complete stream to shell gestures and sends
    /// `wl_touch.cancel` for the partial client stream. Touch never moves the
    /// pointer: a finger is not the mouse.
    fn handle_libinput_touch_event(&mut self, event: InputEvent<LibinputInputBackend>) {
        match event {
            InputEvent::TouchDown { event } => {
                let (x, y) = touch_position(self, &event);
                self.handle_direct_touch_down(
                    touch_contact_key(event.device().id(), touch_slot(event.slot())),
                    x,
                    y,
                );
            }
            InputEvent::TouchMotion { event } => {
                let (x, y) = touch_position(self, &event);
                self.handle_direct_touch_motion(
                    &touch_contact_key(event.device().id(), touch_slot(event.slot())),
                    x,
                    y,
                );
            }
            InputEvent::TouchUp { event } => {
                self.handle_direct_touch_up(&touch_contact_key(
                    event.device().id(),
                    touch_slot(event.slot()),
                ));
            }
            InputEvent::TouchCancel { .. } => {
                self.handle_direct_touch_cancel();
            }
            InputEvent::TouchFrame { .. } => {
                self.flush_direct_touch_frame();
            }
            _ => {}
        }
    }

    /// Inject a direct-touch down after output calibration. Kept separate
    /// from libinput extraction so unit tests can cover the ownership wire
    /// without a real touch device or seat.
    fn handle_direct_touch_down(&mut self, key: TouchContactKey, x: f64, y: f64) {
        match self.direct_touch.down(key, (x, y)) {
            DirectTouchDown::ForwardToClient(id) => {
                self.push(BackendEvent::Touch(TiloTouchEvent::Down { slot: id, x, y }));
                self.push(BackendEvent::Input(touch_input_event(
                    id,
                    x,
                    y,
                    TouchPhase::Started,
                )));
            }
            DirectTouchDown::CancelClientForShell(id) => {
                self.push(BackendEvent::Input(touch_input_event(
                    id,
                    x,
                    y,
                    TouchPhase::Started,
                )));
                // The client received the earlier contacts, but no longer owns
                // their sequence. `cancel` is the Wayland protocol's explicit
                // handoff; importantly, the third down itself never leaks.
                self.push(BackendEvent::Touch(TiloTouchEvent::Cancel));
                // Do not rely on a later libinput frame here: an ownership
                // boundary must close the client sequence even when the
                // source disappears immediately afterwards.
                self.flush_direct_touch_frame();
            }
            DirectTouchDown::ShellOwned(id) => {
                self.push(BackendEvent::Input(touch_input_event(
                    id,
                    x,
                    y,
                    TouchPhase::Started,
                )));
            }
        }
    }

    fn handle_direct_touch_motion(&mut self, key: &TouchContactKey, x: f64, y: f64) {
        if let Some((id, forward_to_client)) = self.direct_touch.motion(key, (x, y)) {
            self.push(BackendEvent::Input(touch_input_event(
                id,
                x,
                y,
                TouchPhase::Moved,
            )));
            if forward_to_client {
                self.push(BackendEvent::Touch(TiloTouchEvent::Motion {
                    slot: id,
                    x,
                    y,
                }));
            }
        }
    }

    fn handle_direct_touch_up(&mut self, key: &TouchContactKey) {
        let Some(((x, y), id, forward_to_client)) = self.direct_touch.up(key) else {
            return;
        };
        self.push(BackendEvent::Input(touch_input_event(
            id,
            x,
            y,
            TouchPhase::Ended,
        )));
        // `up` must be queued before the stream is retired. The shell may
        // already own this stream, in which case `client_stream_active` was
        // cleared at the cancellation boundary and nothing reaches it here.
        if forward_to_client {
            self.push(BackendEvent::Touch(TiloTouchEvent::Up { slot: id }));
        }
    }

    fn handle_direct_touch_cancel(&mut self) {
        let forward_cancel = self.direct_touch.client_stream_active();
        for (slot, (x, y)) in self.direct_touch.cancel() {
            self.push(BackendEvent::Input(touch_input_event(
                slot,
                x,
                y,
                TouchPhase::Cancelled,
            )));
        }
        if forward_cancel {
            self.push(BackendEvent::Touch(TiloTouchEvent::Cancel));
        }
        // This also flushes a pending final `Up` when the source vanishes
        // between that up and libinput's normal frame event.
        self.flush_direct_touch_frame();
    }

    fn handle_direct_touch_source_removed(&mut self) {
        self.handle_direct_touch_cancel();
    }

    fn flush_direct_touch_frame(&mut self) {
        if self.direct_touch.take_client_frame() {
            self.push(BackendEvent::Touch(TiloTouchEvent::Frame));
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
            .name("tilo-icc-profile-probe".into())
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
    /// KMS card path that supplied the first adopted output.  This remains
    /// diagnostic/public primary metadata; scanout ownership is in
    /// [`DrmDispatchState::cards`] and includes every usable card.
    pub primary_gpu: Option<PathBuf>,
    /// Live scanouts — one per connected output. Construction requires at
    /// least one; the vector can become empty later if every display is
    /// unplugged. Ordered by `OutputId`; `scanouts[0]` is the primary.
    pub scanouts: Vec<ScanoutEntry>,
    /// Receiving end of the reconfiguration channel — held here while the backend owns the scanouts inline; moved into the presenter along with them by [`super::Backend::take_presenter`].
    pub commands: Option<Receiver<ScanoutCommand>>,
    /// Sending end of the signalling reports, travelling with the scanouts.
    pub signal_reports: Option<Sender<SignalReport>>,
    /// Bounded watchdog for unconfirmed display changes. It lives in the
    /// compositor rather than Settings so closing/crashing the app cannot
    /// strand the session on a blank mode.
    last_display_transaction_check: std::time::Instant,
    last_icc_profile_refresh: std::time::Instant,
    icc_profiles: IccProfileWorker,
    /// Second handle on the udev-seat libinput context registered in
    /// `event_loop`, used only to see whether it still has work queued.
    libinput: Libinput,
}

/// One live output: its public [`OutputId`], top-left position in the combined coordinate space (laid out side-by-side), and the [`DrmScanout`] driving its crtc.
pub struct ScanoutEntry {
    pub id: OutputId,
    /// `dev_t` of the KMS card that owns this CRTC.  Page-flip events and
    /// CRTC handles are scoped to this card, so multi-card presentation must
    /// drain and route them per device rather than per whole compositor.
    pub device_id: u64,
    pub position: (i32, i32),
    pub transform: settings_store::hardware::OutputTransform,
    pub color_profile: Option<String>,
    /// The profile's qcms transform, which the readback presenter runs on the CPU.
    pub color_transform: Option<qcms::Transform>,
    pub color_buffer: Vec<u8>,
    /// The same transform sampled for the direct path's final GPU pass.
    pub color_lut: Option<IccLut>,
    /// The [`IccLut::id`] the direct path last rendered this output through, `None` for no
    /// profile. When it is not `color_lut`'s, the profile changed and the next frame renders
    /// in full instead of keeping the image calibrated for the old one.
    pub direct_calibration: Option<u64>,
    /// Lazily created same-device GBM/Vulkan targets. `None` means the safe
    /// readback presenter remains authoritative.
    pub direct_targets: Option<DirectScanoutTargets>,
    /// Why this output cannot use direct scanout (no cursor plane, no page-flip
    /// events, another GPU, ...). Re-evaluated every frame.
    pub direct_ineligible: Option<&'static str>,
    /// The current frame presents this output through the combined readback:
    /// it is powered and its direct frame was not, or could not be, submitted.
    pub present_by_readback: bool,
    /// The dumb front shows the combined readback's latest image. False while
    /// the output is direct and until its first readback frame, so readback
    /// shortcuts that patch or skip pixels already shown never reach it stale.
    pub readback_current: bool,
    /// Direct scanout failed: the readback presenter stands in until the
    /// retry this records.
    pub direct_degradation: DirectDegradation,
    /// The same for HDR direct targets; HDR uses the readback path meanwhile.
    pub hdr_direct_degradation: DirectDegradation,
    /// HDR could not be driven at all; the output fell back to SDR.
    pub hdr_failure: Option<settings_store::hardware::HdrUnavailable>,
    /// An output-power client switched this panel off. It is neither
    /// painted nor lit by a session wake until switched back on.
    pub powered_off: bool,
    /// Signalling status last reported to the loop thread.
    pub signal_reported: OutputSignalStatus,
    pub scanout: DrmScanout,
}

impl ScanoutEntry {
    /// The signalling facts this output would report now.
    pub fn signal_status(&self) -> OutputSignalStatus {
        OutputSignalStatus {
            adaptive_sync_rejected: self.scanout.adaptive_sync_rejected(),
            hdr_active: self.scanout.hdr_active(),
            hdr_failure: self.hdr_failure,
        }
    }

    /// HDR is requested and has not failed on this output.
    pub fn hdr_wanted(&self) -> bool {
        self.scanout.hdr_request().is_some() && self.hdr_failure.is_none()
    }

    /// Adopt the loop thread's HDR request. Direct targets in the wrong
    /// format are released first, after the CRTC is back on the dumb front.
    pub fn set_hdr_request(&mut self, request: Option<HdrRequest>) {
        if self
            .direct_targets
            .as_ref()
            .is_some_and(|targets| targets.format().is_hdr() != request.is_some())
        {
            self.scanout.restore_legacy_front();
            self.direct_targets = None;
        }
        if request.is_some() {
            self.hdr_direct_degradation.reset();
        }
        self.scanout.set_hdr_request(request);
    }

    /// HDR could not be shown: return to 8-bit SDR and remember why, for
    /// Settings.
    pub fn fail_hdr(&mut self, reason: settings_store::hardware::HdrUnavailable, detail: &str) {
        tracing::warn!(?reason, detail, "DRM HDR failed; falling back to 8-bit SDR");
        eprintln!(
            "tilo compositor: DRM HDR failed output={} reason={reason:?} detail={detail:?}",
            self.id.0,
        );
        if self
            .direct_targets
            .as_ref()
            .is_some_and(|targets| targets.format().is_hdr())
        {
            self.scanout.restore_legacy_front();
            self.direct_targets = None;
        }
        self.hdr_failure = Some(reason);
        self.scanout.set_hdr_request(None);
    }

    /// Put the readback path's dumb buffers in the format HDR needs, or back
    /// in SDR. Returns whether the readback path presents HDR now.
    pub fn sync_hdr_readback(&mut self) -> bool {
        if self.hdr_wanted() {
            if !self.scanout.format().is_hdr() {
                match self.scanout.enter_hdr_readback() {
                    // The CRTC moved onto the 10-bit dumb pair, so direct
                    // targets are no longer what it shows.
                    Ok(()) => self.direct_targets = None,
                    Err((reason, detail)) => self.fail_hdr(reason, &detail),
                }
            }
        } else if self.scanout.format().is_hdr() {
            self.scanout.leave_hdr();
        }
        self.scanout.format().is_hdr()
    }

    pub fn set_color_profile(&mut self, profile: Option<String>) {
        if self.color_profile == profile {
            return;
        }
        self.color_profile = profile;
        let calibration = self.color_profile.as_deref().and_then(color_calibration);
        if self.color_profile.is_some() && calibration.is_none() {
            tracing::warn!(profile = ?self.color_profile, "DRM display: rejected invalid ICC profile");
        }
        (self.color_transform, self.color_lut) = calibration.unzip();
    }
}

/// The ICC profile at `path` as both presenters apply it: the qcms transform from sRGB to
/// the panel (perceptual) that the readback runs on the CPU, and the same transform sampled
/// for the direct path's final GPU pass. `None` when qcms cannot read the profile.
fn color_calibration(path: &str) -> Option<(qcms::Transform, IccLut)> {
    let input = qcms::Profile::new_sRGB();
    let output = qcms::Profile::new_from_path(path)?;
    let transform = qcms::Transform::new(
        &input,
        &output,
        qcms::DataType::RGBA8,
        qcms::Intent::Perceptual,
    )?;
    let lut = IccLut::from_transform(&transform);
    Some((transform, lut))
}

#[cfg(test)]
mod color_calibration_tests {
    use super::color_calibration;
    use crate::shell::render::icc_lut::tests::lut16;

    #[test]
    fn a_profile_file_calibrates_the_readback_and_the_direct_path_alike() {
        let dir = tempfile::tempdir().unwrap();
        let panel = lut16();
        let path = dir.path().join("panel.icc");
        std::fs::write(&path, panel.lut16_profile()).unwrap();
        let (transform, _) = color_calibration(path.to_str().unwrap())
            .expect("a readable profile yields the qcms transform and its LUT");
        // The readback's transform is the one the profile defines.
        let colours = [[12_u8, 200, 90, 255], [255, 255, 255, 255], [0, 0, 0, 255]];
        let mut readback = colours.concat();
        let mut reference = colours.concat();
        transform.apply(&mut readback);
        panel.transform().apply(&mut reference);
        assert_eq!(readback, reference);

        std::fs::write(&path, b"not an ICC profile").unwrap();
        assert!(color_calibration(path.to_str().unwrap()).is_none());
        assert!(color_calibration(dir.path().join("missing.icc").to_str().unwrap()).is_none());
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

/// Open every usable seat GPU once and bring up every connector each card can
/// drive.  `CardHandle` remains on the loop thread for the session lifetime;
/// per-connector scanouts receive dup'd fds, so libseat never has to grant a
/// second open for an existing `cardN` during hotplug.
///
/// Card ordering is still meaningful: it determines which output is adopted
/// first and therefore preserves `OutputId(0)` as the primary-output
/// fallback.  Unlike the previous "first usable GPU" implementation, a
/// failure or a connectorless card is local and never suppresses the other
/// GPUs.
fn bring_up_all_devices(session: &LibSeatSession, kms_gpus: &[PathBuf]) -> StartupDevices {
    let mut cards = BTreeMap::new();
    let mut card_order = Vec::new();
    let mut primary_gpu = None;
    for path in kms_gpus {
        eprintln!(
            "tilo compositor: trying KMS scanout device {}",
            path.display()
        );
        let mut scanout_session = session.clone();
        match CardHandle::open(&mut scanout_session, path) {
            Ok(card) => {
                let device_id = card.device_id();
                tracing::info!(
                    device = %path.display(),
                    "DRM backend: card retained for multi-card scanout"
                );
                cards.insert(device_id, card);
                card_order.push((device_id, path.clone()));
            }
            Err(error) => {
                eprintln!(
                    "tilo compositor: cannot use DRM output on {}: {error}",
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
    // Every card must be open before configuration is selected: a connector
    // name that looked unique on card0 may collide with one on card1. This is
    // the startup counterpart to `rescan_outputs`' card-wide reconciliation.
    let connector_probes = force_probe_connectors(&cards);
    let (snapshots, display_set) =
        configured_connectors(&cards, connector_probes.clone(), &[], None);
    let mut all_builds = Vec::new();
    for (device_id, path) in card_order {
        let Some(card) = cards.get(&device_id) else {
            continue;
        };
        let mut used = Vec::new();
        let mut builds = Vec::new();
        for snapshot in snapshots
            .get(&device_id)
            .into_iter()
            .flatten()
            .filter(|snapshot| snapshot.enabled)
        {
            if let Some(build) = card.bring_up(snapshot, &used) {
                used.push(build.crtc);
                builds.push(build);
            }
        }
        eprintln!(
            "tilo compositor: {} live DRM output(s) on {}",
            builds.len(),
            path.display()
        );
        if primary_gpu.is_none() && !builds.is_empty() {
            primary_gpu = Some(path);
        }
        all_builds.extend(builds);
    }
    StartupDevices {
        cards,
        primary_gpu,
        builds: all_builds,
        display_set,
        connector_probes,
    }
}

/// What [`bring_up_all_devices`] found and lit.
struct StartupDevices {
    cards: BTreeMap<u64, CardHandle>,
    /// The first card that lit an output.
    primary_gpu: Option<PathBuf>,
    builds: Vec<ScanoutBuild>,
    display_set: settings_store::hardware::DisplaySet,
    /// The startup probe, which later settings changes re-apply intent to.
    connector_probes: BTreeMap<u64, Vec<ConnectorSnapshot>>,
}

/// Register the three loop-wide event sources: session (VT switch), udev
/// (hotplug), and libinput (all pointer/keyboard/gesture/touch input).
/// page-flip/DRM-device sources attach separately, once individual `cardN`
/// devices open. Extracted from [`DrmRuntime::new`] to keep that
/// constructor a readable top-level sequence — see
/// [`DrmDispatchState::handle_session_event`],
/// [`DrmDispatchState::handle_udev_event`], and
/// [`DrmDispatchState::handle_libinput_event`] for what each one actually does.
/// Upper bound on the extra libinput passes one [`DrmRuntime::dispatch_pending`]
/// makes. One device plug raises about four uevents; a dock or a KVM switch a
/// few dozen at most.
const LIBINPUT_DRAIN_LIMIT: usize = 64;

/// Run `dispatch` again while `fd` still polls readable, at most `limit` times,
/// and return how many extra passes ran.
///
/// libinput's udev seat takes a single monitor message per `libinput_dispatch`,
/// and the compositor wakes on this backend's loop fd as an edge. When several
/// uevents land within one wake — every device plug and unplug raises a burst
/// of them — one pass leaves the rest queued, the fd stays readable, and no
/// further edge arrives. A keyboard or mouse plugged in after login then stayed
/// unopened until unrelated input woke the loop once per stuck message, and the
/// backlog carried over to the next plug.
fn redispatch_while_readable(
    fd: std::os::fd::BorrowedFd<'_>,
    limit: usize,
    mut dispatch: impl FnMut(),
) -> usize {
    (0..limit)
        .take_while(|_| fd_readable(fd))
        .map(|_| dispatch())
        .count()
}

fn fd_readable(fd: std::os::fd::BorrowedFd<'_>) -> bool {
    let mut descriptor = libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one valid pollfd for a descriptor the caller keeps borrowed, and
    // a zero timeout never blocks the compositor thread.
    let ready = unsafe { libc::poll(&raw mut descriptor, 1, 0) };
    ready > 0 && descriptor.revents & libc::POLLIN != 0
}

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
    /// Duplicate the internal calloop poll fd so a parent loop can wait on
    /// libinput/libseat/udev readiness directly. The duplicate owns only the
    /// descriptor reference; `event_loop` remains the sole event dispatcher.
    ///
    /// # Errors
    ///
    /// Returns the OS error when the descriptor cannot be duplicated.
    pub fn duplicate_event_fd(&self) -> std::io::Result<OwnedFd> {
        let raw = unsafe {
            libc::fcntl(
                self.event_loop.as_fd().as_raw_fd(),
                libc::F_DUPFD_CLOEXEC,
                3,
            )
        };
        if raw < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            // SAFETY: F_DUPFD_CLOEXEC returned a new descriptor owned by us.
            Ok(unsafe { OwnedFd::from_raw_fd(raw) })
        }
    }

    /// Deadline for work generated wholly inside this runtime rather than by
    /// one of its registered fds. Native keyboard repeat is the only such
    /// source today; exposing it lets the parent compositor block on the
    /// nested calloop fd when otherwise idle without delaying held keys.
    pub fn next_service_deadline(&self) -> Option<std::time::Instant> {
        let repeat = self.dispatch.key_repeat.next_deadline();
        let glide = self
            .dispatch
            .accessibility
            .mouse_keys
            .then(|| self.dispatch.mouse_keys.next_deadline())
            .flatten();
        match (repeat, glide) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

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

        // Retain and drive every usable KMS card.  The CPU bridge renders the
        // combined scene once, then each card scans its own XRGB dumb buffer;
        // no unsafe cross-GPU texture import is needed for this path.
        let startup = bring_up_all_devices(&session, &kms_gpus);
        if startup.builds.is_empty() {
            // `--backend headless` is the explicit display-free mode. A real
            // DRM login that cannot drive a connector must return control to
            // the display manager instead of staying alive behind the stale
            // grey framebuffer left by the greeter.
            return Err(DrmBackendError::NoScanout(kms_gpus));
        }

        // The session-interface adapter routes libinput's `open`/`close` calls through libseat so it inherits privileged FDs without root.
        let mut libinput_ctx =
            Libinput::new_with_udev(LibinputSessionInterface::from(session.clone()));
        libinput_ctx
            .udev_assign_seat(&seat_name)
            .map_err(|e| DrmBackendError::Libinput(format!("{e:?}")))?;
        let libinput = libinput_ctx.clone();
        let libinput_backend = LibinputInputBackend::new(libinput_ctx);

        // The session notifier, udev backend, and libinput backend each register as event sources; page-flip/DRM-device sources attach once individual `cardN` devices open.
        let event_loop: EventLoop<'static, DrmDispatchState> =
            EventLoop::try_new().map_err(|e| DrmBackendError::Calloop(format!("{e}")))?;
        let handle: LoopHandle<'static, DrmDispatchState> = event_loop.handle();
        register_event_sources(&handle, session_notifier, udev, libinput_backend)?;

        // Compile the xkb keymap once at startup from the person's input sources, or the machine's layout when they chose none (`XKB_DEFAULT_*`, localed; see `keyboard_sources::system_sources`); a failure is non-fatal, running pointer-only rather than refusing to boot.
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
        let (signal_tx, signal_rx) = channel::<SignalReport>();
        let icc_profiles = IccProfileWorker::new()?;
        icc_profiles.request_refresh();

        let mut dispatch = DrmDispatchState::new(
            session_state,
            startup.cards,
            session.clone(),
            input,
            accessibility,
            scale_override,
            tx,
            keyboard,
        );
        dispatch.display_set = startup.display_set;
        dispatch.connector_probes = startup.connector_probes;
        dispatch.signal_reports = Some(signal_rx);
        // Tablet pads are read from a context of their own; see `PadInput`.
        let pads = PadInput::new(session.clone());
        handle
            .insert_source(
                smithay::reexports::calloop::generic::Generic::new(
                    pads.context.clone(),
                    smithay::reexports::calloop::Interest::READ,
                    smithay::reexports::calloop::Mode::Level,
                ),
                |_, _, state: &mut DrmDispatchState| {
                    state.handle_pad_events();
                    Ok(smithay::reexports::calloop::PostAction::Continue)
                },
            )
            .map_err(|e| DrmBackendError::Calloop(format!("{e}")))?;
        dispatch.tablets.pads = Some(pads);
        // Adopt the connectors already live at startup — the same path a
        // hot-plug takes — and seed the pointer/snapshot; see the helper's
        // doc comment.
        let scanouts = dispatch.finalize_startup_scanouts(startup.builds);
        // Seat assignment queues the attached devices immediately, before the
        // libinput fd becomes readable. Publish/configure those devices through
        // the ordinary handler before waiting for physical input.
        dispatch.adopt_initial_input_devices(&libinput)?;

        Ok(Self {
            event_loop,
            dispatch,
            session,
            primary_gpu: startup.primary_gpu,
            scanouts,
            commands: Some(rx),
            signal_reports: Some(signal_tx),
            last_display_transaction_check: std::time::Instant::now(),
            last_icc_profile_refresh: std::time::Instant::now(),
            icc_profiles,
            libinput,
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
        tracing::info!("DRM backend: restoring the display after resume");
        self.dispatch.resume_from_sleep();
    }

    /// Queue cancellation for every active direct-touch contact. The session
    /// lock uses this before it hides client input, because a physical release
    /// after that boundary cannot be relied upon to arrive.
    /// Show `image` as the pointer on every output.
    pub fn set_pointer_image(&mut self, image: crate::compositor::cursor::PointerImage) {
        if self.dispatch.pointer_image == image {
            return;
        }
        self.dispatch.pointer_image = image.clone();
        self.dispatch.sync_cursor_hotspot();
        self.dispatch
            .command(ScanoutCommand::SetPointerImage(image));
    }

    pub fn set_pointer_constraint(
        &mut self,
        constraint: crate::compositor::backend::PointerConstraintRegion,
    ) {
        self.dispatch.pointer_constraint = constraint;
    }

    /// Place the pointer at global `(x, y)` (clamped to the outputs) and
    /// tell the compositor, as if it had moved there.
    pub fn warp_pointer(&mut self, x: f64, y: f64) {
        let dispatch = &mut self.dispatch;
        dispatch.set_pointer_scene((x, y));
        let (x, y) = dispatch.pointer;
        dispatch.push(BackendEvent::Input(cursor_moved(x, y)));
    }

    pub fn cancel_touch(&mut self) {
        self.dispatch.handle_direct_touch_cancel();
    }

    /// Switch the physical keyboard to input source `index` at once.
    pub fn set_keyboard_source(&mut self, index: usize) {
        self.dispatch.keyboard_config = settings_store::hardware::RichInputConfig::from_section(
            &settings_store::load_section("input"),
        );
        self.dispatch.keyboard_cache.clear();
        let scope = self
            .dispatch
            .keyboard_device
            .clone()
            .unwrap_or_else(|| settings_store::keyboard_sources::ALL.into());
        self.dispatch.apply_keyboard_assignment(&scope);
        if let Some(keyboard) = self.dispatch.keyboard.as_mut() {
            keyboard.set_active_layout(index);
        }
    }

    /// Re-read input settings and configure every currently attached device.
    /// New devices receive these settings when they appear.
    pub fn reload_input_settings(&mut self) {
        let input = InputSettings::load();
        self.dispatch.keyboard_config = settings_store::hardware::RichInputConfig::from_section(
            &settings_store::load_section("input"),
        );
        self.dispatch.keyboard_cache.clear();
        let scope = self
            .dispatch
            .keyboard_device
            .as_deref()
            .unwrap_or(settings_store::keyboard_sources::ALL);
        let keyboard_settings = KeyboardSettings::from_config(
            &settings_store::keyboard_sources::effective(&self.dispatch.keyboard_config, scope),
        );
        let previous_cursor_size = self.dispatch.accessibility.effective_cursor_size();
        let accessibility = AccessibilityInputSettings::load();
        self.dispatch.input = input;
        self.dispatch.key_repeat.cancel();
        if let Some(keyboard) = self
            .dispatch
            .keyboard
            .as_mut()
            .filter(|keyboard| keyboard.compiles_to_same_keymap(&keyboard_settings))
        {
            // Only the active source moved (the input-source chord persists
            // its choice): switch groups without a recompile, which would
            // forget the modifiers the person is still holding.
            keyboard.set_active_layout(keyboard_settings.active_index);
        } else if let Some(keyboard) = KeyboardXkb::from_settings(&keyboard_settings) {
            self.dispatch.keyboard = Some(keyboard);
            self.dispatch.keyboard_key_owners.clear();
            self.dispatch.slow_pending.clear();
            self.dispatch.physical_modifiers = ModifiersState::empty();
            self.dispatch.sticky_latched = ModifiersState::empty();
            self.dispatch
                .push(BackendEvent::ModifiersChanged(Modifiers::from(
                    ModifiersState::empty(),
                )));
            self.dispatch.push(BackendEvent::InputStateReset);
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
        self.dispatch.tablets.reload();
        if accessibility.effective_cursor_size() != previous_cursor_size {
            self.dispatch.command(ScanoutCommand::SetCursorSize(
                accessibility.effective_cursor_size(),
            ));
            self.dispatch.sync_cursor_hotspot();
        }
        if !accessibility.sticky_keys {
            self.dispatch.sticky_latched = ModifiersState::empty();
        }
        if !accessibility.slow_keys {
            self.dispatch.slow_pending.clear();
        }
        if !accessibility.bounce_keys {
            self.dispatch.bounce_keys.reset();
        }
        if !accessibility.mouse_keys {
            self.dispatch.release_mouse_keys();
        }
        self.dispatch.gestures.set_config(GestureConfig {
            natural_scroll: input.natural_scroll_for(true),
            ..GestureConfig::default()
        });
        input_config::reconfigure_all(&self.dispatch.input, &mut self.dispatch.devices);
    }

    /// Check the timed display confirmation independently of settings-file
    /// changes. The compositor loop calls this continuously, but filesystem
    /// work is bounded to four times a second. Capability snapshots are
    /// published only when their inputs change (startup, hotplug, display
    /// settings, or ICC results), and are built from the last hotplug probe:
    /// force-probing every KMS connector on this input-thread path can stall
    /// pointer and keyboard dispatch for seconds on real hardware.
    pub fn poll_display_transaction(&mut self) {
        if self.dispatch.drain_signal_reports() {
            self.dispatch.publish_display_snapshot();
        }
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
                removal_ack: None,
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
        let Some(transaction) = settings_store::hardware::DisplayTransaction::load() else {
            return;
        };
        if transaction.expired(unix_now_ms()) {
            self.dispatch.reload_display_settings();
        }
    }
}

impl DrmDispatchState {
    /// The `display`, `display_transaction` or `display_profiles` section
    /// changed (or a pending transaction timed out): advance the transaction,
    /// then re-apply scale, arrangement and modes.
    ///
    /// Never force-probes a connector. This runs on the input thread for a
    /// settings write — including the transaction write this very function
    /// makes — and a forced probe of every connector stalls pointer and
    /// keyboard dispatch for seconds on a DP-MST dock. What is connected
    /// changes only through a udev edge, which re-probes.
    pub fn reload_display_settings(&mut self) {
        let scale_override = load_display_scale_override();
        if scale_override != self.scale_override {
            self.scale_override = scale_override;
            self.relayout_outputs();
            self.outputs_changed = true;
            self.push(BackendEvent::OutputsChanged);
        }
        self.reconcile_display_transaction();
        // The same atomic settings write may carry a topology/mode change.
        // Reconcile first so a requested advertised mode is modeset; then
        // publish arrangement/scale even when the connector
        // set itself did not change.
        self.reapply_outputs_from_cache("display settings changed");
        self.relayout_outputs();
        let outputs = self.output_table();
        self.command(ScanoutCommand::Reconfigure {
            remove: Vec::new(),
            add: Vec::new(),
            outputs,
            removal_ack: None,
        });
        self.outputs_changed = true;
        self.push(BackendEvent::OutputsChanged);
        self.push(BackendEvent::Redraw);
        self.publish_display_snapshot();
    }

    /// Advance one display request before the output reconciliation consumes
    /// its effective configuration. Validation uses the compositor-published
    /// descriptor set, never values supplied by Settings.
    fn reconcile_display_transaction(&mut self) {
        use settings_store::hardware::{DisplaySet, DisplayTransaction, DisplayTransactionState};

        let Some(mut transaction) = DisplayTransaction::load() else {
            return;
        };
        let now = unix_now_ms();
        if transaction.expired(now) {
            transaction.state = DisplayTransactionState::RevertRequested;
            transaction.message = Some("Display change timed out and was restored".into());
        }
        match transaction.state {
            DisplayTransactionState::Requested => {
                let descriptors: Vec<_> = self
                    .capability_outputs()
                    .into_iter()
                    .map(|output| output.descriptor)
                    .collect();
                if descriptors.is_empty() {
                    transaction.state = DisplayTransactionState::Rejected;
                    transaction.message =
                        Some("No compositor-owned DRM capability snapshot is available".into());
                } else if DisplaySet::of_descriptors(&descriptors).key != transaction.set.key {
                    transaction.state = DisplayTransactionState::Rejected;
                    transaction.message = Some(
                        "The attached displays changed before the arrangement was applied".into(),
                    );
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
                if let Err(error) = transaction.candidate.save_for(&transaction.set) {
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
                if let Err(error) = transaction.previous.save_for(&transaction.set) {
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
}

impl DrmRuntime {
    /// Every attached connector as output-management heads: the cached
    /// capability snapshot joined with the outputs being driven and the
    /// effective intent for the ones that are off.
    pub fn output_heads(&self) -> Vec<crate::compositor::output_heads::OutputHead> {
        let live: Vec<crate::compositor::output_heads::LiveHead> = self
            .dispatch
            .live
            .iter()
            .filter_map(|entry| {
                let output = self.dispatch.output_meta.get(&entry.id)?;
                Some(crate::compositor::output_heads::LiveHead {
                    key: entry.config_key.clone(),
                    output: output.clone(),
                    physical_size_mm: entry.physical_mm,
                })
            })
            .collect();
        crate::compositor::output_heads::heads_from_capabilities(
            &self.dispatch.display_capabilities,
            &settings_store::hardware::DisplayConfig::load_effective(&self.dispatch.display_set),
            &live,
        )
    }

    /// Turn the built-in panel to follow the device. Touch input follows
    /// automatically: it is mapped through each live output's transform.
    pub fn set_device_rotation(
        &mut self,
        rotation: Option<settings_store::hardware::OutputTransform>,
    ) {
        if self.dispatch.device_rotation == rotation {
            return;
        }
        self.dispatch.device_rotation = rotation;
        self.dispatch.relayout_outputs();
        self.dispatch.command(ScanoutCommand::Reconfigure {
            remove: Vec::new(),
            add: Vec::new(),
            outputs: self.dispatch.output_table(),
            removal_ack: None,
        });
        self.dispatch.outputs_changed = true;
        self.dispatch.push(BackendEvent::OutputsChanged);
        self.dispatch.push(BackendEvent::Redraw);
    }

    /// Ask the scanout owner to switch one panel off or on.
    pub fn set_fullscreen_output(&mut self, output: Option<OutputId>) {
        self.dispatch.set_fullscreen_output(output);
    }

    pub fn set_output_power(&self, output: OutputId, on: bool) {
        self.dispatch
            .command(ScanoutCommand::SetOutputPower { output, on });
    }

    /// Drain pending events from the internal calloop — non-blocking, a zero timeout so the outer compositor loop owns the cadence.
    pub fn dispatch_pending(&mut self) {
        use std::time::Duration;
        let _ = self.event_loop.dispatch(Duration::ZERO, &mut self.dispatch);
        let event_loop = &mut self.event_loop;
        let dispatch = &mut self.dispatch;
        let extra = redispatch_while_readable(self.libinput.as_fd(), LIBINPUT_DRAIN_LIMIT, || {
            let _ = event_loop.dispatch(Duration::ZERO, &mut *dispatch);
        });
        if extra == LIBINPUT_DRAIN_LIMIT {
            tracing::warn!(
                passes = extra,
                "DRM input: libinput still has work queued after the drain limit"
            );
        }
        if self.dispatch.finish_retired_cards() {
            // A Udev Added that raced removal was deliberately held until
            // libseat closed the old fd.  It is a new physical card from the
            // active scanout map's perspective, so now reconcile it.
            self.dispatch
                .rescan_outputs("udev card re-added after teardown");
        }
        let now = std::time::Instant::now();
        self.dispatch.pump_keyboard_repeat(now);
        self.dispatch.pump_mouse_keys(now);
    }

    /// Test-only seam: a dispatch state driving `outputs` — `(connector name, mode size)`, in
    /// enumeration order — as if each had just been modeset, laid out with no saved
    /// arrangement. `super::mod` tests use it to drive `DrmBackend` without real hardware.
    #[cfg(test)]
    pub(super) fn synthesize_for_tests(outputs: &[(&str, (u32, u32))]) -> DrmDispatchState {
        let mut state = DrmDispatchState::default();
        for (connector, (name, size)) in (0_u32..).zip(outputs) {
            let id = state.outputs.intern(DrmOutputKey {
                device: 0,
                connector,
            });
            state.live.push(tests::named(
                tests::entry_on_device(0, id.0, connector, *size, None),
                name,
            ));
        }
        state.relayout_with(&settings_store::hardware::DisplayConfig::default());
        state
    }
}

/// Ask one libinput device what it can be configured to do — every
/// question is a capability query, asked before setting to keep the path free of error returns for settings a device never accepted.
/// libinput half of [`input_config::ConfigurableDevice`].
///
/// Every failure is ignored: libinput errors on an unsupported setting, and
/// [`input_config::plan_for`]'s capability gates already avoid asking —
/// anything left is a driver quirk that must not take the session down.
impl input_config::ConfigurableDevice for smithay::reexports::input::Device {
    fn caps(&self) -> DeviceCaps {
        device_caps(self)
    }

    fn apply(&mut self, plan: &input_config::DevicePlan) {
        if let Some(on) = plan.tap {
            let _ = self.config_tap_set_enabled(on);
        }
        if let Some(on) = plan.tap_drag {
            let _ = self.config_tap_set_drag_enabled(on);
        }
        if let Some(on) = plan.tap_drag_lock {
            let _ = self.config_tap_set_drag_lock_enabled(on);
        }
        if let Some(on) = plan.natural_scroll {
            let _ = self.config_scroll_set_natural_scroll_enabled(on);
        }
        if let Some(on) = plan.disable_while_typing {
            let _ = self.config_dwt_set_enabled(on);
        }
        if let Some(on) = plan.middle_emulation {
            let _ = self.config_middle_emulation_set_enabled(on);
        }
        if let Some(speed) = plan.accel_speed() {
            let _ = self.config_accel_set_speed(speed);
        }
        if let Some(on) = plan.left_handed {
            let _ = self.config_left_handed_set(on);
        }
        if let Some(mode) = plan.send_events {
            use smithay::reexports::input::SendEventsMode;
            let _ = self.config_send_events_set_mode(match mode {
                input_config::SendEvents::Enabled => SendEventsMode::ENABLED,
                input_config::SendEvents::Disabled => SendEventsMode::DISABLED,
                input_config::SendEvents::DisabledOnExternalMouse => {
                    SendEventsMode::DISABLED_ON_EXTERNAL_MOUSE
                }
            });
        }
        if let Some(flat) = plan.flat_acceleration {
            use smithay::reexports::input::AccelProfile;
            let _ = self.config_accel_set_profile(if flat {
                AccelProfile::Flat
            } else {
                AccelProfile::Adaptive
            });
        }
        if let Some(method) = plan.click_method {
            use smithay::reexports::input::ClickMethod;
            let _ = self.config_click_set_method(match method {
                input_config::ClickMethod::ButtonAreas => ClickMethod::ButtonAreas,
                input_config::ClickMethod::Clickfinger => ClickMethod::Clickfinger,
            });
        }
        if let Some(method) = plan.scroll_method {
            use smithay::reexports::input::ScrollMethod;
            let _ = self.config_scroll_set_method(match method {
                input_config::ScrollMethod::TwoFinger => ScrollMethod::TwoFinger,
                input_config::ScrollMethod::Edge => ScrollMethod::Edge,
            });
        }
    }
}

/// The identity input settings are keyed by, for a libinput device.
fn libinput_device_key(device: &smithay::reexports::input::Device) -> String {
    let base = settings_store::hardware::input_device_key(
        device.id_vendor(),
        device.id_product(),
        device.name(),
    );
    if !device.keyboard_has_key(30).unwrap_or(false) {
        return base;
    }
    let root = Path::new("/sys/class/input")
        .join(device.sysname())
        .join("device");
    let serial = std::fs::read_to_string(root.join("uniq")).unwrap_or_default();
    let physical = std::fs::read_to_string(root.join("phys")).unwrap_or_default();
    settings_store::keyboard_sources::device_key(&base, &serial, &physical)
}

/// libinput reports evdev keycodes; xkb numbers the same keys eight higher.
fn evdev_to_xkb(code: u32) -> xkb::Keycode {
    xkb::Keycode::new(code.saturating_add(8))
}

/// One attached device as Settings sees it.
fn input_device_snapshot(
    device: &smithay::reexports::input::Device,
    key: String,
) -> settings_store::hardware::InputDeviceSnapshot {
    let caps = device_caps(device);
    let mut snapshot = settings_store::hardware::InputDeviceSnapshot {
        key,
        name: device.name().to_string(),
        kind: caps.kind(),
        ..Default::default()
    };
    if caps.is_tablet_pad {
        let pad = pad_description(device);
        snapshot.pad_buttons = pad.buttons;
        let modes_of = |control: &dyn Fn(&crate::compositor::backend::PadGroup) -> bool| {
            pad.groups
                .iter()
                .find(|group| control(group))
                .map_or(1, |group| group.modes.max(1))
        };
        snapshot.pad_rings = (0..pad_count(device.tablet_pad_number_of_rings()))
            .map(|ring| modes_of(&|group| group.rings.contains(&ring)))
            .collect();
        snapshot.pad_strips = (0..pad_count(device.tablet_pad_number_of_strips()))
            .map(|strip| modes_of(&|group| group.strips.contains(&strip)))
            .collect();
        snapshot.pad_mode_buttons = pad_mode_buttons(device);
    }
    snapshot
}

/// A libinput count, which is negative for a device that is not a pad.
fn pad_count(count: i32) -> u32 {
    u32::try_from(count).unwrap_or(0)
}

/// A pad's buttons, rings and strips, grouped by the mode groups libinput
/// reports for it.
fn pad_description(
    device: &smithay::reexports::input::Device,
) -> crate::compositor::backend::PadDescription {
    let buttons = pad_count(device.tablet_pad_number_of_buttons());
    let rings = pad_count(device.tablet_pad_number_of_rings());
    let strips = pad_count(device.tablet_pad_number_of_strips());
    let groups = (0..pad_count(device.tablet_pad_number_of_mode_groups()))
        .filter_map(|index| device.tablet_pad_mode_group(index))
        .map(|group| crate::compositor::backend::PadGroup {
            buttons: (0..buttons).filter(|b| group.has_button(*b)).collect(),
            mode_buttons: (0..buttons)
                .filter(|b| group.button_is_toggle(*b))
                .collect(),
            rings: (0..rings).filter(|r| group.has_ring(*r)).collect(),
            strips: (0..strips).filter(|s| group.has_strip(*s)).collect(),
            modes: group.number_of_modes(),
        })
        .collect();
    let dirs: Vec<PathBuf> = crate::compositor::pad_layout::LIBWACOM_DIRS
        .iter()
        .map(PathBuf::from)
        .collect();
    let layout = crate::compositor::pad_layout::find_layout(
        &dirs,
        (device.id_vendor(), device.id_product()),
        buttons,
        rings,
        strips,
    );
    crate::compositor::backend::PadDescription {
        key: libinput_device_key(device),
        device: crate::compositor::backend::TabletDevice {
            name: device.name().to_string(),
            usb_id: Some((device.id_vendor(), device.id_product())),
            syspath: InputDevice::syspath(device),
        },
        buttons,
        groups,
        layout,
    }
}

/// The pad buttons that switch their group's mode.
fn pad_mode_buttons(device: &smithay::reexports::input::Device) -> Vec<u32> {
    let buttons = pad_count(device.tablet_pad_number_of_buttons());
    let groups: Vec<_> = (0..pad_count(device.tablet_pad_number_of_mode_groups()))
        .filter_map(|index| device.tablet_pad_mode_group(index))
        .collect();
    (0..buttons)
        .filter(|button| groups.iter().any(|group| group.button_is_toggle(*button)))
        .collect()
}

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
        supports_click_areas: device
            .config_click_methods()
            .contains(&smithay::reexports::input::ClickMethod::ButtonAreas),
        supports_clickfinger: device
            .config_click_methods()
            .contains(&smithay::reexports::input::ClickMethod::Clickfinger),
        default_click_method: match device.config_click_default_method() {
            Some(smithay::reexports::input::ClickMethod::ButtonAreas) => {
                Some(input_config::ClickMethod::ButtonAreas)
            }
            Some(smithay::reexports::input::ClickMethod::Clickfinger) => {
                Some(input_config::ClickMethod::Clickfinger)
            }
            _ => None,
        },
        supports_two_finger_scroll: device
            .config_scroll_methods()
            .contains(&smithay::reexports::input::ScrollMethod::TwoFinger),
        supports_disable_on_external_mouse: device
            .config_send_events_modes()
            .contains(smithay::reexports::input::SendEventsMode::DISABLED_ON_EXTERNAL_MOUSE),
        supports_edge_scroll: device
            .config_scroll_methods()
            .contains(&smithay::reexports::input::ScrollMethod::Edge),
        is_keyboard: device.has_capability(smithay::reexports::input::DeviceCapability::Keyboard)
            && device.keyboard_has_key(30).unwrap_or(false),
        is_touch: device.has_capability(smithay::reexports::input::DeviceCapability::Touch),
        is_tablet_tool: device
            .has_capability(smithay::reexports::input::DeviceCapability::TabletTool),
        is_tablet_pad: device
            .has_capability(smithay::reexports::input::DeviceCapability::TabletPad),
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

fn touch_contact_key(device: String, slot: u32) -> TouchContactKey {
    TouchContactKey { device, slot }
}

/// One output's geometry needed to calibrate a touchscreen. Kept separate
/// from [`LiveEntry`] so the selection and coordinate transform stay pure
/// and can be covered without opening DRM or libinput.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TouchOutputGeometry<'a> {
    connector_name: &'a str,
    config_key: &'a str,
    position: (i32, i32),
    physical_size: (u32, u32),
    transform: settings_store::hardware::OutputTransform,
}

impl<'a> From<&'a LiveEntry> for TouchOutputGeometry<'a> {
    fn from(output: &'a LiveEntry) -> Self {
        Self {
            connector_name: &output.connector_name,
            config_key: &output.config_key,
            position: output.position,
            physical_size: output.size,
            transform: output.transform,
        }
    }
}

/// Pick a touchscreen's output from libinput's connector name. The input is
/// in layout order, whose first member is the configured primary (then the
/// stable `OutputId` order), making missing and stale associations safe and
/// deterministic after a hotplug. Libinput provides only the raw connector
/// label, not a DRM card/bus id, so an ambiguous raw label deliberately falls
/// back to the primary instead of selecting the first matching GPU.
///
/// A touchscreen with no association at all is, in practice, the one built
/// into a laptop or tablet panel (udev tags the rare external touch monitor
/// with its output). It goes to the built-in panel, even when an external
/// monitor is the primary display.
fn touch_output_for_name<'a>(
    outputs: impl IntoIterator<Item = TouchOutputGeometry<'a>>,
    output_name: Option<&str>,
) -> Option<TouchOutputGeometry<'a>> {
    let mut fallback = None;
    let mut panel = None;
    let mut raw_match = None;
    let mut raw_match_count = 0usize;
    for output in outputs {
        if fallback.is_none() {
            fallback = Some(output);
        }
        if panel.is_none() && is_builtin_connector(output.connector_name) {
            panel = Some(output);
        }
        if output_name.is_some_and(|name| name == output.config_key) {
            return Some(output);
        }
        if output_name.is_some_and(|name| name == output.connector_name) {
            raw_match_count = raw_match_count.saturating_add(1);
            raw_match = Some(output);
        }
    }
    if raw_match_count == 1 {
        raw_match
    } else if output_name.is_none() {
        panel.or(fallback)
    } else {
        fallback
    }
}

/// Convert a clamped device-normalised touch coordinate to the selected
/// output's global logical coordinate. The transform is the inverse of the
/// logical-to-physical scanout transform, so touch, cursor and rendering all
/// agree when a panel is rotated.
fn calibrate_touch_position(
    output: TouchOutputGeometry<'_>,
    normalized_x: f64,
    normalized_y: f64,
) -> (f64, f64) {
    use settings_store::hardware::OutputTransform as T;

    let x = normalized_x.clamp(0.0, 1.0);
    let y = normalized_y.clamp(0.0, 1.0);
    let (logical_w, logical_h) = logical_output_size(output.physical_size, output.transform);
    // Output rectangles use an exclusive right/bottom edge. Stay inside the
    // final pixel even when libinput reports an exact normalised `1.0`, or a
    // touch at the panel's far edge could route to an adjacent monitor.
    let logical_w = f64::from(logical_w.saturating_sub(1));
    let logical_h = f64::from(logical_h.saturating_sub(1));
    let (local_x, local_y) = match output.transform {
        T::Normal => (x * logical_w, y * logical_h),
        // Inverse of `logical_to_physical_point` in `scanout.rs`, expressed
        // continuously, using the inclusive final pixel of the logical
        // rectangle rather than its exclusive edge.
        T::Rotate90 => (y * logical_w, (1.0 - x) * logical_h),
        T::Rotate180 => ((1.0 - x) * logical_w, (1.0 - y) * logical_h),
        T::Rotate270 => ((1.0 - y) * logical_w, x * logical_h),
    };
    (
        f64::from(output.position.0) + local_x,
        f64::from(output.position.1) + local_y,
    )
}

/// Map a touchscreen position onto its associated output, rather than the
/// desktop's combined extent. Both the `wl_touch` event and synthesized
/// pointer event consume this one result in the caller.
fn touch_position<E>(state: &DrmDispatchState, event: &E) -> (f64, f64)
where
    E: AbsolutePositionEvent<LibinputInputBackend>,
{
    let device = event.device();
    let output_name = state
        .touch_output_bindings
        .get(&device.id())
        .and_then(Option::as_deref);
    let Some(output) = touch_output_for_name(
        state.live.iter().map(TouchOutputGeometry::from),
        output_name,
    ) else {
        return (0.0, 0.0);
    };
    let width = i32::try_from(output.physical_size.0)
        .unwrap_or(i32::MAX)
        .max(1);
    let height = i32::try_from(output.physical_size.1)
        .unwrap_or(i32::MAX)
        .max(1);
    calibrate_touch_position(
        output,
        event.x_transformed(width) / f64::from(width),
        event.y_transformed(height) / f64::from(height),
    )
}

use ui::synthetic_input::cursor_moved;

/// Build a `WindowEvent::Touch` for the shell's direct-touch arena.
fn touch_input_event(slot: u32, x: f64, y: f64, phase: TouchPhase) -> WindowEvent {
    WindowEvent::Touch(Touch {
        device_id: winit::event::DeviceId::dummy(),
        phase,
        location: PhysicalPosition::new(x, y),
        force: None,
        id: u64::from(slot),
    })
}

/// What a libinput device contributes to the session's input availability.
///
/// Power buttons, lid switches and video-bus hotkeys all claim the keyboard
/// capability; only a device that can type letters counts as a keyboard, or
/// every tablet would appear to have one. A drawing tablet steers the pointer.
fn device_input_capabilities(device: &smithay::reexports::input::Device) -> ui::InputAvailability {
    use smithay::reexports::input::DeviceCapability;
    const KEY_A: u32 = 30;
    let mut capabilities = ui::InputAvailability::NONE;
    if device.has_capability(DeviceCapability::Keyboard)
        && device.keyboard_has_key(KEY_A).unwrap_or(false)
    {
        capabilities = capabilities.with(ui::InputDevice::Keyboard);
    }
    if device.has_capability(DeviceCapability::Pointer)
        || device.has_capability(DeviceCapability::TabletTool)
    {
        capabilities = capabilities.with(ui::InputDevice::Pointer);
    }
    if device.has_capability(DeviceCapability::Touch) {
        capabilities = capabilities.with(ui::InputDevice::Touch);
    }
    capabilities
}

/// Build a `WindowEvent::MouseInput`.
fn mouse_input_event(state: ElementState, button: MouseButton) -> WindowEvent {
    WindowEvent::MouseInput {
        device_id: winit::event::DeviceId::dummy(),
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
        device_id: winit::event::DeviceId::dummy(),
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
        // Both pairs are defined by input-event-codes.h and real devices use
        // both. winit intentionally normalises either pair to Back/Forward.
        0x113 | 0x116 => Some(MouseButton::Back),
        0x114 | 0x115 => Some(MouseButton::Forward),
        _ => None,
    }
}

/// xkb keymap + state owned by the DRM backend, turning libinput keycodes
/// into the winit `KeyboardInput` the chrome understands — the winit backend gets these from the OS, but bare-TTY libinput only reports raw keycodes.
pub struct KeyboardXkb {
    // Held for the lifetime of `state`. xkbcommon refcounts internally, but keeping ownership explicit makes the lifetime obvious (never read after construction, hence the underscore).
    _context: xkb::Context,
    keymap: xkb::Keymap,
    state: xkb::State,
    /// Last modifier mask we emitted — we only push `ModifiersChanged` when it actually flips, matching winit's behaviour.
    mods: ModifiersState,
    /// The layouts, variants and options this keymap was compiled from, so
    /// a settings reload that only moves the active source switches groups
    /// instead of recompiling and dropping held modifiers.
    compiled: (String, String, String, Option<String>),
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
            "",              // rules -> XKB_DEFAULT_RULES (normally evdev)
            &settings.model, // explicit physical model
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
            keymap,
            state,
            mods: ModifiersState::empty(),
            compiled: Self::keymap_names(settings),
        })
    }

    fn keymap_names(settings: &KeyboardSettings) -> (String, String, String, Option<String>) {
        (
            settings.model.clone(),
            settings.layouts.clone(),
            settings.variants.clone(),
            settings.options.clone(),
        )
    }

    /// Whether `settings` compile to exactly this keymap.
    fn compiles_to_same_keymap(&self, settings: &KeyboardSettings) -> bool {
        self.compiled == Self::keymap_names(settings)
    }

    /// Switch to input source `index`, keeping every held and locked
    /// modifier. An index past the configured sources is ignored.
    pub fn set_active_layout(&mut self, index: usize) {
        let Ok(group) = u32::try_from(index) else {
            return;
        };
        if group >= self.keymap.num_layouts() {
            return;
        }
        let depressed = self.state.serialize_mods(xkb::STATE_MODS_DEPRESSED);
        let latched = self.state.serialize_mods(xkb::STATE_MODS_LATCHED);
        let locked = self.state.serialize_mods(xkb::STATE_MODS_LOCKED);
        let _ = self
            .state
            .update_mask(depressed, latched, locked, 0, 0, group);
    }

    /// The active input source, counting from 0.
    pub fn active_layout(&self) -> usize {
        usize::try_from(self.state.serialize_layout(xkb::STATE_LAYOUT_EFFECTIVE)).unwrap_or(0)
    }

    fn key_repeats(&self, keycode: xkb::Keycode) -> bool {
        self.keymap.key_repeats(keycode)
    }

    /// The evdev keycode that types `ch` without modifiers on the active
    /// layout, if any key does.
    fn keycode_for_char(&self, ch: char) -> Option<u32> {
        let wanted = xkb::utf32_to_keysym(u32::from(ch.to_ascii_lowercase()));
        let layout = self.state.serialize_layout(xkb::STATE_LAYOUT_EFFECTIVE);
        (self.keymap.min_keycode().raw()..=self.keymap.max_keycode().raw())
            .find(|raw| {
                self.keymap
                    .key_get_syms_by_level(xkb::Keycode::new(*raw), layout, 0)
                    .contains(&wanted)
            })
            .map(|raw| raw.saturating_sub(8))
    }

    /// Forget keys and modifiers which may have been released while input
    /// delivery was suspended, while preserving the user's active layout.
    fn reset_state(&mut self) {
        let active_layout = self.state.serialize_layout(xkb::STATE_LAYOUT_EFFECTIVE);
        self.state = xkb::State::new(&self.keymap);
        let _ = self.state.update_mask(0, 0, 0, 0, 0, active_layout);
        self.mods = ModifiersState::empty();
    }

    /// Produce the same repeated `KeyboardInput` winit emits without
    /// mutating xkb's down/up state a second time.
    fn repeat_event(&self, keycode: xkb::Keycode) -> WindowEvent {
        let sym = self.state.key_get_one_sym(keycode);
        let logical = keysym_to_winit_key(sym.raw());
        let mut input = keyboard_input_event(
            logical,
            text_for(sym.key_char(), self.current_mods()),
            true,
            keycode.raw().saturating_sub(8),
        );
        if let WindowEvent::KeyboardInput { event, .. } = &mut input {
            event.repeat = true;
        }
        input
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
        let direction = if pressed {
            xkb::KeyDirection::Down
        } else {
            xkb::KeyDirection::Up
        };
        let _ = self.state.update_key(keycode, direction);

        // Resolve the symbol and UTF-8 text *after* xkb has incorporated the
        // event. That is required for the key which changes the state itself:
        // a Shift press must publish Shift, not the symbol from the previous
        // level, and a printable press must use every modifier/layout latch
        // effective for that key. Releases are not text-bearing in the
        // consumers below, so resolving their post-release level is harmless.
        let sym = self.state.key_get_one_sym(keycode);

        let new_mods = self.current_mods();
        let modifiers_changed = if new_mods == self.mods {
            None
        } else {
            self.mods = new_mods;
            Some(Modifiers::from(new_mods))
        };

        // Derive the logical key from the *keysym* (not UTF-8 output) so chords like Ctrl+K resolve to `Character("k")`, not the control character.
        let logical = keysym_to_winit_key(sym.raw());
        let text = text_for(sym.key_char(), new_mods);
        let input = Some(keyboard_input_event(
            logical,
            text,
            pressed,
            keycode.raw().saturating_sub(8),
        ));

        KeyOutcome {
            input,
            modifiers_changed,
        }
    }
}

/// Map an xkb keysym to a winit logical [`Key`]: special keys become
/// `Key::Named`, printable Unicode becomes `Key::Character`; bare modifiers and unmapped keys return `None` (carried by `ModifiersChanged` instead).
fn keysym_to_winit_key(raw: u32) -> Key {
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
        return Key::Named(function);
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
        // The stock `pc` keymap turns Alt+Print into Sys_Req; it is still
        // the Print Screen key, and Alt+Print is the focused-window capture.
        keysyms::KEY_Print | keysyms::KEY_Sys_Req => Some(NamedKey::PrintScreen),
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
        keysyms::KEY_XF86AudioStop => Some(NamedKey::MediaStop),
        // winit's own XKB table folds every sleep spelling into Standby and
        // names the calculator key LaunchApplication2; the nested backend
        // delivers those, so this one must too.
        keysyms::KEY_XF86Sleep | keysyms::KEY_XF86Suspend | keysyms::KEY_XF86Standby => {
            Some(NamedKey::Standby)
        }
        keysyms::KEY_XF86Calculator => Some(NamedKey::LaunchApplication2),
        _ => None,
    };
    if let Some(n) = named {
        return Key::Named(n);
    }
    // Printable character (handles layout + shift + caps via the keysym).
    if let Some(c) = Keysym::new(raw).key_char()
        && !c.is_control()
    {
        return Key::Character(SmolStr::new(c.to_string()));
    }
    // Never drop an otherwise valid physical key merely because this adapter
    // does not have a named-key spelling for its keysym. The seat still must
    // receive the raw keycode (IME/vendor keys included), and clients can
    // interpret the xkb keymap they were sent. Returning an unidentified
    // logical key preserves that protocol event while keeping it out of shell
    // chord matching.
    Key::Unidentified(NativeKey::Xkb(raw))
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

/// Build a `WindowEvent::KeyboardInput` from the evdev key `evdev_keycode`.
fn keyboard_input_event(
    logical_key: Key,
    text: Option<SmolStr>,
    pressed: bool,
    evdev_keycode: u32,
) -> WindowEvent {
    let mut ev = ui::synthetic_input::key_event(
        logical_key,
        text,
        if pressed {
            ElementState::Pressed
        } else {
            ElementState::Released
        },
    );
    ev.physical_key = PhysicalKey::from_scancode(evdev_keycode);
    WindowEvent::KeyboardInput {
        device_id: winit::event::DeviceId::dummy(),
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
mod libinput_drain_tests {
    use std::cell::Cell;
    use std::os::fd::AsFd;
    use std::os::unix::net::UnixDatagram;
    use std::rc::Rc;
    use std::time::Duration;

    use smithay::reexports::calloop::generic::Generic;
    use smithay::reexports::calloop::{EventLoop, Interest, Mode, PostAction};

    use super::{fd_readable, redispatch_while_readable};

    /// The DRM runtime's shape in miniature: an inner loop whose source, like
    /// libinput's udev seat, takes one queued message per dispatch, watched by
    /// an outer loop that registers the inner loop's fd as an edge.
    struct Nested {
        outer: EventLoop<'static, ()>,
        inner: EventLoop<'static, ()>,
        sender: UnixDatagram,
        source: UnixDatagram,
        received: Rc<Cell<usize>>,
        woken: Rc<Cell<usize>>,
    }

    fn nested() -> Nested {
        let (sender, source) = UnixDatagram::pair().expect("socket pair");
        source.set_nonblocking(true).expect("nonblocking");
        let reader = source.try_clone().expect("clone socket");
        let received = Rc::new(Cell::new(0_usize));
        let inner: EventLoop<'static, ()> = EventLoop::try_new().expect("inner loop");
        let count = received.clone();
        inner
            .handle()
            .insert_source(
                Generic::new(reader, Interest::READ, Mode::Level),
                move |_, socket, ()| {
                    let mut buf = [0u8; 16];
                    if socket.recv(&mut buf).is_ok() {
                        count.set(count.get().saturating_add(1));
                    }
                    Ok(PostAction::Continue)
                },
            )
            .expect("inner source");
        let outer: EventLoop<'static, ()> = EventLoop::try_new().expect("outer loop");
        let wake = inner.as_fd().try_clone_to_owned().expect("dup inner fd");
        let woken = Rc::new(Cell::new(0_usize));
        let wakes = woken.clone();
        outer
            .handle()
            .insert_source(
                Generic::new(wake, Interest::READ, Mode::Edge),
                move |_, _, ()| {
                    wakes.set(wakes.get().saturating_add(1));
                    Ok(PostAction::Continue)
                },
            )
            .expect("outer source");
        Nested {
            outer,
            inner,
            sender,
            source,
            received,
            woken,
        }
    }

    impl Nested {
        fn plug_burst(&self, messages: usize) {
            for _ in 0..messages {
                self.sender.send(b"add").expect("send uevent");
            }
        }

        /// One compositor iteration: wait for the edge, then dispatch the
        /// inner loop the way `DrmRuntime::dispatch_pending` does.
        fn wake_and_dispatch(&mut self, drain: bool) -> bool {
            let before = self.woken.get();
            self.outer
                .dispatch(Duration::ZERO, &mut ())
                .expect("outer dispatch");
            if self.woken.get() == before {
                return false;
            }
            self.inner
                .dispatch(Duration::ZERO, &mut ())
                .expect("inner dispatch");
            if drain {
                let inner = &mut self.inner;
                redispatch_while_readable(self.source.as_fd(), 64, || {
                    inner
                        .dispatch(Duration::ZERO, &mut ())
                        .expect("inner dispatch");
                });
            }
            true
        }
    }

    #[test]
    fn one_pass_per_edge_strands_the_rest_of_a_plug_burst() {
        let mut nested = nested();
        nested.plug_burst(4);
        assert!(nested.wake_and_dispatch(false));
        assert_eq!(nested.received.get(), 1);
        assert!(
            !nested.wake_and_dispatch(false),
            "the outer edge must not fire again for messages already queued"
        );
        assert_eq!(nested.received.get(), 1);
        assert!(fd_readable(nested.source.as_fd()));
    }

    #[test]
    fn draining_after_the_edge_delivers_every_message_in_the_burst() {
        let mut nested = nested();
        nested.plug_burst(4);
        assert!(nested.wake_and_dispatch(true));
        assert_eq!(nested.received.get(), 4);
        assert!(!fd_readable(nested.source.as_fd()));
        nested.plug_burst(4);
        assert!(
            nested.wake_and_dispatch(true),
            "the next plug wakes the loop again"
        );
        assert_eq!(nested.received.get(), 8);
    }

    #[test]
    fn a_source_that_never_drains_is_bounded() {
        let (sender, source) = UnixDatagram::pair().expect("socket pair");
        sender.send(b"stuck").expect("send");
        let mut passes = 0_usize;
        let extra = redispatch_while_readable(source.as_fd(), 5, || {
            passes = passes.saturating_add(1);
        });
        assert_eq!(extra, 5);
        assert_eq!(passes, 5);
    }

    #[test]
    fn an_idle_source_costs_no_extra_pass() {
        let (_sender, source) = UnixDatagram::pair().expect("socket pair");
        let extra = redispatch_while_readable(source.as_fd(), 5, || panic!("nothing queued"));
        assert_eq!(extra, 0);
    }
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "deterministic output geometry fixtures use exactly representable values"
)]
mod tests {
    use super::*;

    fn test_keyboard_settings() -> KeyboardSettings {
        KeyboardSettings::default()
    }

    fn test_touch_key(device: &str, slot: u32) -> TouchContactKey {
        touch_contact_key(device.into(), slot)
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
    fn dispatch_state_and_connector_enumeration() {
        // Phase `dispatch_state_starts_empty`.
        {
            let s = DrmDispatchState::default();
            assert!(s.events.is_empty());
            assert!(s.outputs.is_empty());
            assert!(s.output_meta.is_empty());
        }
        // Phase `push_appends_event`.
        {
            let mut s = DrmDispatchState::default();
            // `BackendEvent::Input` carries the actual `WindowEvent`; synthesise a minimal CursorMoved so the variant constructs.
            let fake = winit::event::WindowEvent::CursorMoved {
                device_id: winit::event::DeviceId::dummy(),
                position: winit::dpi::PhysicalPosition::new(0.0, 0.0),
            };
            s.push(BackendEvent::Input(fake));
            s.push(BackendEvent::Redraw);
            assert_eq!(s.events.len(), 2);
            assert!(matches!(s.events[0], BackendEvent::Input(_)));
            assert!(matches!(s.events[1], BackendEvent::Redraw));
        }
        // Phase `enumerate_connectors_returns_empty_when_no_devices`.
        {
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
        // Phase `enumerate_connectors_skips_devices_that_fail_to_open`.
        {
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
    }

    // Keyboard: xkb keycode → winit key translation

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario, one labelled phase per merged case"
    )]
    fn keysyms_map_to_named_keys_and_modifier_aware_text() {
        // Phase `keysym_mapping_named_and_character`.
        // Pure mapping: named special keys and printable characters resolve to the winit `Key` the chrome matches on — no real keymap needed, mirroring the `preferred_mode`/pointer-helper tests.
        {
            use smithay::input::keyboard::keysyms as ks;
            assert!(matches!(
                keysym_to_winit_key(ks::KEY_Return),
                Key::Named(NamedKey::Enter)
            ));
            assert!(matches!(
                keysym_to_winit_key(ks::KEY_Escape),
                Key::Named(NamedKey::Escape)
            ));
            assert!(matches!(
                keysym_to_winit_key(ks::KEY_Tab),
                Key::Named(NamedKey::Tab)
            ));
            assert!(matches!(
                keysym_to_winit_key(ks::KEY_BackSpace),
                Key::Named(NamedKey::Backspace)
            ));
            assert!(matches!(
                keysym_to_winit_key(ks::KEY_Left),
                Key::Named(NamedKey::ArrowLeft)
            ));
            assert!(matches!(
                keysym_to_winit_key(ks::KEY_space),
                Key::Named(NamedKey::Space)
            ));
            assert_eq!(
                keysym_to_winit_key(ks::KEY_Print),
                Key::Named(NamedKey::PrintScreen)
            );
            // Letter keysyms carry case from the keysym itself (US 'a' vs 'A').
            match keysym_to_winit_key(ks::KEY_a) {
                Key::Character(s) => assert_eq!(s.as_str(), "a"),
                other => panic!("KEY_a mapped to {other:?}"),
            }
            match keysym_to_winit_key(ks::KEY_A) {
                Key::Character(s) => assert_eq!(s.as_str(), "A"),
                other => panic!("KEY_A mapped to {other:?}"),
            }
            assert_eq!(
                keysym_to_winit_key(ks::KEY_Control_L),
                Key::Named(NamedKey::Control)
            );
            assert_eq!(
                keysym_to_winit_key(ks::KEY_Shift_L),
                Key::Named(NamedKey::Shift)
            );
            assert_eq!(keysym_to_winit_key(ks::KEY_F11), Key::Named(NamedKey::F11));
            assert_eq!(
                keysym_to_winit_key(ks::KEY_Caps_Lock),
                Key::Named(NamedKey::CapsLock)
            );
            assert_eq!(
                keysym_to_winit_key(ks::KEY_Multi_key),
                Key::Named(NamedKey::Compose)
            );
            assert_eq!(
                keysym_to_winit_key(ks::KEY_ISO_Level3_Shift),
                Key::Named(NamedKey::AltGraph)
            );
            assert!(matches!(
                keysym_to_winit_key(0xffff_0bad),
                Key::Unidentified(NativeKey::Xkb(0xffff_0bad))
            ));
        }
        // Phase `media_and_brightness_keysyms_reach_a_named_key`.
        // The function row has to survive the keysym translation — these keysyms have no character, so anything not in this table falls through `key_char()` and is dropped before a handler runs.
        {
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
                (ks::KEY_XF86AudioStop, NamedKey::MediaStop),
                (ks::KEY_XF86Sleep, NamedKey::Standby),
                (ks::KEY_XF86Suspend, NamedKey::Standby),
                (ks::KEY_XF86Standby, NamedKey::Standby),
                (ks::KEY_XF86Calculator, NamedKey::LaunchApplication2),
            ] {
                assert_eq!(
                    keysym_to_winit_key(raw),
                    Key::Named(want),
                    "keysym {raw:#x} must map to {want:?}"
                );
            }
        }
        // Phase `text_for_respects_modifiers_and_control_chars`.
        // Text is produced for plain printable keys, suppressed under Ctrl (chords) and for control characters.
        {
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
    }

    /// Alt+Print arrives as `Sys_Req` under the stock keymap; it must still
    /// reach the Print Screen handler, as it does under winit.
    #[test]
    fn alt_print_is_still_print_screen() {
        use smithay::input::keyboard::keysyms as ks;
        for raw in [ks::KEY_Print, ks::KEY_Sys_Req] {
            assert_eq!(
                keysym_to_winit_key(raw),
                Key::Named(NamedKey::PrintScreen),
                "keysym {raw:#x}"
            );
        }
    }

    /// The machine keys winit has no name for arrive as the raw keysym —
    /// exactly what the nested backend delivers — and the hardware-key
    /// classifier routes them from there.
    #[test]
    fn unnamed_machine_keysyms_reach_the_hardware_key_classifier() {
        use crate::shell::media_keys::{MediaKey, classify};
        use smithay::input::keyboard::keysyms as ks;
        for (raw, want) in [
            (ks::KEY_XF86KbdBrightnessUp, MediaKey::KeyboardBacklightUp),
            (
                ks::KEY_XF86KbdBrightnessDown,
                MediaKey::KeyboardBacklightDown,
            ),
            (ks::KEY_XF86KbdLightOnOff, MediaKey::KeyboardBacklightToggle),
            (ks::KEY_XF86WLAN, MediaKey::Wireless),
            (0x1008_ffb5, MediaKey::Wireless),
            (ks::KEY_XF86Display, MediaKey::DisplayMode),
            (ks::KEY_XF86TouchpadToggle, MediaKey::TouchpadToggle),
            (ks::KEY_XF86Sleep, MediaKey::Sleep),
            (ks::KEY_XF86Calculator, MediaKey::Calculator),
            (ks::KEY_XF86AudioStop, MediaKey::Stop),
        ] {
            let key = keysym_to_winit_key(raw);
            let event = keyboard_input_event(key.clone(), None, true, 0);
            assert_eq!(
                classify(&event),
                Some(want),
                "keysym {raw:#x} ({key:?}) must reach {want:?}"
            );
        }
    }

    #[test]
    fn xkb_shift_state_tracks_modifier_events_and_character_input() {
        // Phase `xkb_left_shift_toggles_modifier_mask`.
        // End-to-end through a real keymap: pressing/releasing Left Shift flips the modifier mask — layout-independent; soft-skips if no xkb data installed.
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
        // Phase `xkb_shift_event_itself_is_named_and_following_text_is_shifted`.
        fn xkb_shift_event_itself_is_named_and_following_text_is_shifted() {
            let Some(mut kbd) = KeyboardXkb::from_settings(&test_keyboard_settings()) else {
                eprintln!("xkb keymap unavailable; skipping");
                return;
            };
            let down = kbd.process(xkb::Keycode::new(50), true);
            let WindowEvent::KeyboardInput { event, .. } = down.input.expect("shift input") else {
                panic!("expected keyboard input")
            };
            assert_eq!(event.logical_key, Key::Named(NamedKey::Shift));

            let letter = kbd.process(xkb::Keycode::new(38), true);
            let WindowEvent::KeyboardInput { event, .. } = letter.input.expect("letter input")
            else {
                panic!("expected keyboard input")
            };
            assert_eq!(event.logical_key, Key::Character("A".into()));
            assert_eq!(event.text.as_deref(), Some("A"));
        }
        // Phase `xkb_alpha_key_produces_character_input`.
        // End-to-end: an alpha-row keycode yields a `Character` `KeyboardInput` carrying text — QWERTY 'a' is a letter on every Latin layout, so layout-independent; soft-skips without xkb data.
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
        // Phase `xkb_left_shift_toggles_modifier_mask` (defined above).
        xkb_left_shift_toggles_modifier_mask();
        // Phase `xkb_shift_event_itself_is_named_and_following_text_is_shifted` (defined above).
        xkb_shift_event_itself_is_named_and_following_text_is_shifted();
        // Phase `xkb_alpha_key_produces_character_input` (defined above).
        xkb_alpha_key_produces_character_input();
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn xkb_layout_sources_and_german_symbols_keep_physical_positions() {
        // Phase `xkb_uses_the_persisted_active_group_without_reordering_sources`.
        fn xkb_uses_the_persisted_active_group_without_reordering_sources() {
            let settings = KeyboardSettings {
                model: "pc105".into(),
                source_ids: vec!["us".into(), "de".into()],
                layouts: "us,de".into(),
                variants: ",".into(),
                options: None,
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
        // Phase `german_xkb_shift_and_punctuation_keep_physical_positions`.
        // Exercise the exact German symbols and Shift handling that failed in
        // Electron/Xwayland during the host-session trial. This runs through the
        // production xkb translator using physical evdev positions, not a
        // pre-translated character fixture.
        fn german_xkb_shift_and_punctuation_keep_physical_positions() {
            let settings = KeyboardSettings {
                model: "pc105".into(),
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
                let WindowEvent::KeyboardInput { event, .. } = outcome.input.expect("key input")
                else {
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
        // Phase `switching_the_active_source_keeps_held_modifiers`.
        fn switching_the_active_source_keeps_held_modifiers() {
            let settings = KeyboardSettings {
                model: "pc105".into(),
                source_ids: vec!["us".into(), "de".into()],
                layouts: "us,de".into(),
                variants: ",".into(),
                options: None,
                active_index: 0,
            };
            let Some(mut keyboard) = KeyboardXkb::from_settings(&settings) else {
                eprintln!("xkb keymap unavailable; skipping");
                return;
            };
            // Hold Left Super (evdev 125 + XKB offset 8), as the chord does.
            let _ = keyboard.process(xkb::Keycode::new(133), true);
            keyboard.set_active_layout(1);
            assert_eq!(keyboard.active_layout(), 1);
            assert!(
                keyboard.current_mods().super_key(),
                "the chord's own modifier survives the switch"
            );
            let _ = keyboard.process(xkb::Keycode::new(133), false);
            // AD06 is Y in US and Z in German.
            let output = keyboard.process(xkb::Keycode::new(29), true);
            let WindowEvent::KeyboardInput { event, .. } = output.input.expect("key input") else {
                panic!("expected keyboard input")
            };
            assert_eq!(event.logical_key, Key::Character("z".into()));

            keyboard.set_active_layout(7);
            assert_eq!(keyboard.active_layout(), 1, "an unknown source is ignored");
            assert!(keyboard.compiles_to_same_keymap(&KeyboardSettings {
                active_index: 0,
                ..settings
            }));
        }
        // Phase `xkb_german_nodeadkeys_preserves_shifted_symbols_and_physical_codes`.
        fn xkb_german_nodeadkeys_preserves_shifted_symbols_and_physical_codes() {
            let settings = KeyboardSettings {
                model: "pc105".into(),
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
        // Phase `german_altgr_produces_text_without_becoming_a_shell_alt_chord`.
        fn german_altgr_produces_text_without_becoming_a_shell_alt_chord() {
            let settings = KeyboardSettings {
                model: "pc105".into(),
                source_ids: vec!["de:nodeadkeys".into()],
                layouts: "de".into(),
                variants: "nodeadkeys".into(),
                options: None,
                active_index: 0,
            };
            let Some(mut keyboard) = KeyboardXkb::from_settings(&settings) else {
                eprintln!("German xkb keymap unavailable; skipping");
                return;
            };

            // Right Alt is ISO_Level3_Shift in the German map. It must select
            // level three for Q → @ without being represented as the ordinary
            // Alt modifier used by shell shortcuts such as Alt+Tab/Alt+F4.
            let altgr = xkb::Keycode::new(100 + 8);
            let down = keyboard.process(altgr, true);
            let WindowEvent::KeyboardInput { event, .. } = down.input.expect("AltGr key input")
            else {
                panic!("expected keyboard input")
            };
            assert_eq!(event.logical_key, Key::Named(NamedKey::AltGraph));
            assert!(
                down.modifiers_changed.is_none(),
                "AltGr is a level selector, not the shell's Alt modifier"
            );

            let q = keyboard.process(xkb::Keycode::new(16 + 8), true);
            let WindowEvent::KeyboardInput { event, .. } = q.input.expect("AltGr+Q input") else {
                panic!("expected keyboard input")
            };
            assert_eq!(event.logical_key, Key::Character("@".into()));
            assert_eq!(event.text.as_deref(), Some("@"));
            assert_eq!(event.physical_key.to_scancode(), Some(16));
        }
        // Phase `xkb_uses_the_persisted_active_group_without_reordering_sources` (defined above).
        xkb_uses_the_persisted_active_group_without_reordering_sources();
        // Phase `german_xkb_shift_and_punctuation_keep_physical_positions` (defined above).
        german_xkb_shift_and_punctuation_keep_physical_positions();
        // Phase `switching_the_active_source_keeps_held_modifiers` (defined above).
        switching_the_active_source_keeps_held_modifiers();
        // Phase `xkb_german_nodeadkeys_preserves_shifted_symbols_and_physical_codes` (defined above).
        xkb_german_nodeadkeys_preserves_shifted_symbols_and_physical_codes();
        // Phase `german_altgr_produces_text_without_becoming_a_shell_alt_chord` (defined above).
        german_altgr_produces_text_without_becoming_a_shell_alt_chord();
    }

    #[test]
    fn side_by_side_layout_packs_outputs_left_to_right() {
        // Phase `side_by_side_layout_single_output_at_origin`.
        {
            // The single-output case must place the one output at (0, 0).
            let positions = side_by_side_layout(&[(1920, 1080)]);
            assert_eq!(positions, vec![(0, 0)]);
        }
        // Phase `side_by_side_layout_places_outputs_left_to_right`.
        {
            // Each output's x is the sum of prior widths; y stays 0.
            let positions = side_by_side_layout(&[(1920, 1080), (1280, 1024), (2560, 1440)]);
            assert_eq!(positions, vec![(0, 0), (1920, 0), (3200, 0)]);
        }
        // Phase `side_by_side_layout_empty_is_empty`.
        assert!(side_by_side_layout(&[]).is_empty());
    }

    fn touch_geometry(
        connector_name: &'static str,
        config_key: &'static str,
        position: (i32, i32),
        physical_size: (u32, u32),
        transform: settings_store::hardware::OutputTransform,
    ) -> TouchOutputGeometry<'static> {
        TouchOutputGeometry {
            connector_name,
            config_key,
            position,
            physical_size,
            transform,
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn touchscreen_binding_follows_the_named_output_and_calibration() {
        // Phase `touchscreen_output_binding_routes_panel_corners_without_crossing_monitors`.
        {
            use settings_store::hardware::OutputTransform::Normal;

            // eDP-1's right edge is adjacent to HDMI-A-1, which is the precise
            // layout that exposed a panel spanning the combined desktop.
            let outputs = [
                touch_geometry("eDP-1", "eDP-1", (0, 0), (1920, 1080), Normal),
                touch_geometry("HDMI-A-1", "HDMI-A-1", (1920, 0), (2560, 1440), Normal),
            ];
            let panel = touch_output_for_name(outputs, Some("eDP-1")).expect("bound panel");

            assert_eq!(calibrate_touch_position(panel, 0.0, 0.0), (0.0, 0.0));
            assert_eq!(
                calibrate_touch_position(panel, 1.0, 1.0),
                (1919.0, 1079.0),
                "the panel corner remains inside eDP-1, not on HDMI's first pixel"
            );
            assert_eq!(calibrate_touch_position(panel, 0.5, 0.5), (959.5, 539.5));
        }
        // Phase `touchscreen_binding_uses_the_named_output_in_a_nontrivial_layout`.
        {
            use settings_store::hardware::OutputTransform::Normal;

            let outputs = [
                touch_geometry("eDP-1", "eDP-1", (0, 360), (1920, 1080), Normal),
                touch_geometry("DP-1", "DP-1", (1920, 0), (2560, 1440), Normal),
            ];
            let dock_touch = touch_output_for_name(outputs, Some("DP-1")).expect("bound dock");

            assert_eq!(
                calibrate_touch_position(dock_touch, 0.0, 0.0),
                (1920.0, 0.0)
            );
            assert_eq!(
                calibrate_touch_position(dock_touch, 1.0, 1.0),
                (4479.0, 1439.0)
            );
        }
        // Phase `missing_or_hot_unplugged_touch_output_falls_back_to_current_primary`.
        {
            use settings_store::hardware::OutputTransform::Normal;

            // A connected eDP touchscreen loses its named output during a
            // hot-unplug. Resolving against the *current* layout must not retain
            // an old output id or re-expand across a now-gone desktop extent.
            let after_unplug = [touch_geometry(
                "HDMI-A-1",
                "HDMI-A-1",
                (0, 0),
                (2560, 1440),
                Normal,
            )];
            let fallback =
                touch_output_for_name(after_unplug, Some("eDP-1")).expect("primary fallback");
            assert_eq!(fallback.connector_name, "HDMI-A-1");
            assert_eq!(
                calibrate_touch_position(fallback, 0.5, 0.5),
                (1279.5, 719.5)
            );

            // Libinput is also allowed to report no association. It follows the
            // same deterministic primary rule instead of the combined extent.
            let no_metadata = [
                touch_geometry("eDP-1", "eDP-1", (0, 0), (1920, 1080), Normal),
                touch_geometry("HDMI-A-1", "HDMI-A-1", (1920, 0), (2560, 1440), Normal),
            ];
            let fallback = touch_output_for_name(no_metadata, None).expect("primary fallback");
            assert_eq!(calibrate_touch_position(fallback, 0.5, 0.5), (959.5, 539.5));

            // An unassociated touchscreen is the laptop's own panel, even
            // when a docked monitor is the primary display.
            let docked = [
                touch_geometry("HDMI-A-1", "HDMI-A-1", (0, 0), (2560, 1440), Normal),
                touch_geometry("eDP-1", "eDP-1", (2560, 0), (2944, 1840), Normal),
            ];
            let panel = touch_output_for_name(docked, None).expect("built-in panel");
            assert_eq!(panel.connector_name, "eDP-1");
            assert_eq!(calibrate_touch_position(panel, 0.0, 0.0), (2560.0, 0.0));
        }
        // Phase `ambiguous_libinput_connector_name_never_picks_a_secondary_gpu_by_order`.
        {
            use settings_store::hardware::OutputTransform::Normal;

            // The raw libinput metadata has no card/bus component. Even though
            // the second entry happens to be an HDMI match too, it must not win
            // merely because it was encountered last (or first after a map
            // re-order); the configured primary is the safe fallback.
            let outputs = [
                touch_geometry("eDP-1", "eDP-1", (0, 0), (1920, 1080), Normal),
                touch_geometry(
                    "HDMI-A-1",
                    "pci:0000:01:00.0/HDMI-A-1",
                    (1920, 0),
                    (2560, 1440),
                    Normal,
                ),
                touch_geometry(
                    "HDMI-A-1",
                    "pci:0000:03:00.0/HDMI-A-1",
                    (4480, 0),
                    (2560, 1440),
                    Normal,
                ),
            ];
            let fallback = touch_output_for_name(outputs, Some("HDMI-A-1"))
                .expect("primary fallback for ambiguous raw connector");
            assert_eq!(fallback.config_key, "eDP-1");

            // A caller that can supply the compositor's qualified key is still
            // precise; only libinput's raw legacy metadata is ambiguous.
            let qualified = touch_output_for_name(outputs, Some("pci:0000:03:00.0/HDMI-A-1"))
                .expect("qualified output");
            assert_eq!(qualified.position, (4480, 0));
        }
        // Phase `rotated_touch_calibration_and_synthesized_pointer_share_coordinates`.
        {
            use settings_store::hardware::OutputTransform::Rotate90;

            let output = touch_geometry("eDP-1", "eDP-1", (100, 50), (1920, 1080), Rotate90);
            // The physical panel's top-left maps to the post-transform logical
            // top-right corner; this is the inverse of scanout's rotation.
            let (x, y) = calibrate_touch_position(output, 0.0, 0.0);
            assert_eq!((x, y), (100.0, 1969.0));

            // The runtime feeds these exact values to both delivery paths.
            let touch = TiloTouchEvent::Motion { slot: 4, x, y };
            let pointer = cursor_moved(x, y);
            assert_eq!(
                touch,
                TiloTouchEvent::Motion {
                    slot: 4,
                    x: 100.0,
                    y: 1969.0
                }
            );
            let WindowEvent::CursorMoved { position, .. } = pointer else {
                panic!("expected synthesized cursor movement")
            };
            assert_eq!((position.x, position.y), (x, y));
        }
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
        entry_on_device(1, id, connector, size, mm)
    }

    /// Fake one modeset output without a real card.  Giving the fixture a
    /// device id lets topology tests cover the exact identity boundary where
    /// DRM connector and CRTC handles restart on a second GPU.
    pub(super) fn entry_on_device(
        device: u64,
        id: u32,
        connector: u32,
        size: (u32, u32),
        mm: Option<(u32, u32)>,
    ) -> LiveEntry {
        LiveEntry {
            key: DrmOutputKey { device, connector },
            id: OutputId(id),
            connector_name: format!("TEST-{connector}"),
            config_key: format!("TEST-{connector}"),
            size,
            mode: settings_store::hardware::OutputMode {
                width: size.0,
                height: size.1,
                refresh_mhz: 60_000,
            },
            mode_intent: None,
            physical_mm: mm,
            monitor: None,
            // Test ids are always small literals; `saturating_add` just
            // avoids a raw `+` clippy can't see is bounded.
            crtc: crtc(id.saturating_add(1)),
            position: (0, 0),
            logical_position: (0, 0),
            scale: 1.0,
            transform: settings_store::hardware::OutputTransform::Normal,
            refresh: None,
            adaptive_sync_supported: false,
            adaptive_sync_allowed: true,
            adaptive_sync_engaged: false,
            hdr_display: None,
            hdr_driver_ready: false,
            hdr_setting: None,
            hdr_requested: None,
        }
    }

    #[test]
    fn connector_replug_rekeys_and_refreshes_without_scanout_work() {
        // Phase `duplicate_plug_rekeys_an_unchanged_live_output_before_layout_reads_config`.
        {
            use settings_store::hardware::{DisplayConfig, OutputConfig, OutputMode};

            let key = DrmOutputKey {
                device: 11,
                connector: 7,
            };
            let qualified = "pci:0000:01:00.0/HDMI-A-1";
            let intent = OutputConfig {
                enabled: true,
                primary: true,
                x: 640,
                y: 240,
                scale: 1.5,
                mode: Some(OutputMode {
                    width: 2560,
                    height: 1440,
                    refresh_mhz: 120_000,
                }),
                mirror_of: Some("pci:0000:03:00.0/HDMI-A-1".into()),
                ..Default::default()
            };
            let mut config = DisplayConfig::default();
            config.outputs.insert(qualified.into(), intent.clone());

            // Card A was already modeset before card B's same-named connector
            // appeared. Its OutputId/CRTC/size stay untouched; only settings
            // identity changes before `relayout_outputs` reads it.
            let mut live = vec![entry_on_device(11, 4, 7, (2560, 1440), None)];
            live[0].connector_name = "HDMI-A-1".into();
            live[0].config_key = "HDMI-A-1".into();
            let keys = BTreeMap::from([(key, qualified.to_string())]);
            assert!(reconcile_live_config_keys(&mut live, &keys));
            assert_eq!(live[0].id, OutputId(4), "no output replug/re-id");
            assert_eq!(live[0].crtc, crtc(5), "no re-modeset required");
            assert_eq!(live[0].config_key, qualified);
            assert_eq!(config.output_for(&live[0].config_key), Some(&intent));

            // The resolver retains the existing qualified key after card B is
            // removed, so the unchanged survivor never falls back to raw intent.
            assert!(!reconcile_live_config_keys(&mut live, &keys));
            assert_eq!(config.output_for(&live[0].config_key), Some(&intent));
        }
        // Phase `disabled_connector_replug_and_removal_refresh_capabilities_without_scanout_work`.
        {
            let disabled = DrmOutputKey {
                device: 12,
                connector: 9,
            };
            let mut published = BTreeSet::new();

            // A disabled connector is absent from `ConnectedOutput`, so there is
            // no active plan to add/remove. Capability topology still changes and
            // drives the Settings snapshot publication path.
            assert!(hotplug::plan(&[], &[]).is_noop());
            assert!(reconcile_capability_topology(
                &mut published,
                BTreeSet::from([disabled])
            ));
            assert!(published.contains(&disabled));
            assert!(hotplug::plan(&[], &[]).is_noop());
            assert!(reconcile_capability_topology(
                &mut published,
                BTreeSet::new()
            ));
            assert!(published.is_empty());
        }
    }

    #[test]
    fn card_remove_readd_and_close_errors_gate_reopening() {
        // Phase `fake_card_remove_readd_waits_for_presenter_before_closing_and_reopening_libseat`.
        {
            // This models an eGPU/cardN that disappears and comes back before a
            // render frame drains its removal command.  The actual fd stays in
            // `retiring_cards`; the pure state machine proves the required
            // ownership order without a real seat or presenter thread.
            let mut cards = CardLeaseLifecycle::from_active([42]);
            assert_eq!(cards.remove_requested(42), CardLeaseAction::DetachPresenter);
            assert_eq!(cards.add_requested(42), CardLeaseAction::DeferReadd);
            assert_eq!(
                cards.presenter_detached(42),
                CardLeaseAction::CloseSessionFd,
                "the original Session::open fd must remain owned until presenter scanouts are gone"
            );
            assert_eq!(
                cards.session_close_finished(42, true),
                CardLeaseAction::ReopenDeferred
            );
            cards.opened(42);
            assert_eq!(cards.add_requested(42), CardLeaseAction::AlreadyOpen);
        }
        // Phase `fake_card_close_failure_discards_deferred_readd_until_fresh_session`.
        {
            let mut cards = CardLeaseLifecycle::from_active([42]);
            assert_eq!(cards.remove_requested(42), CardLeaseAction::DetachPresenter);
            assert_eq!(cards.add_requested(42), CardLeaseAction::DeferReadd);
            assert_eq!(
                cards.presenter_detached(42),
                CardLeaseAction::CloseSessionFd
            );
            assert_eq!(
                cards.session_close_finished(42, false),
                CardLeaseAction::Noop,
                "a failed libseat close must never reopen a possibly orphaned device"
            );
            assert_eq!(
                cards.add_requested(42),
                CardLeaseAction::BlockedByCloseFailure,
                "only a fresh session clears a failed card close"
            );
        }
        // Phase `duplicate_open_close_error_blocks_the_same_physical_card_after_cardn_churn`.
        {
            // The duplicate-open cleanup and retired-card close share this guard.
            // A later card1/dev_t must be blocked by physical identity even
            // though the failed duplicate was card0/dev_t 226.
            let mut state = DrmDispatchState::default();
            state.block_card_after_close_failure(
                226,
                Path::new("/dev/dri/card0"),
                Some("sysfs-0000:03:00.0"),
            );
            let reenumerated_identity = "sysfs-0000:03:00.0";
            assert!(
                state
                    .failed_card_close_identities
                    .contains(reenumerated_identity),
                "card1's new path/dev_t cannot bypass a failed card0 close"
            );
            assert_eq!(
                state.card_leases.add_requested(226),
                CardLeaseAction::BlockedByCloseFailure
            );
        }
        // Phase `unidentified_close_error_blocks_all_card_adds_until_fresh_session`.
        {
            let mut state = DrmDispatchState::default();
            state.block_card_after_close_failure(226, Path::new("/dev/dri/card0"), None);
            assert!(state.unidentified_card_close_failure);
            assert!(
                state
                    .failed_card_close_paths
                    .contains(Path::new("/dev/dri/card0")),
                "the original path stays diagnostic, but unknown new paths are blocked too"
            );
        }
    }

    #[test]
    fn client_cursor_hotspot_is_not_scaled_twice_by_the_cursor_plane() {
        use crate::compositor::cursor::{CursorBitmap, CursorShape, PointerImage};
        for scale in [1.0, 1.5, 2.0] {
            let client =
                CursorBitmap::from_client(&vec![255; 32 * 32 * 4], 32, 32, (4, 6), 2.0, scale)
                    .expect("a valid client cursor buffer");
            let expected = (
                i32::try_from(client.hotspot.0).unwrap(),
                i32::try_from(client.hotspot.1).unwrap(),
            );
            let mut state = DrmDispatchState {
                pointer_image: PointerImage::Client(std::sync::Arc::new(client)),
                ..Default::default()
            };
            state.sync_cursor_hotspot();
            assert_eq!(
                state.hw_cursor.hotspot_pixels(scale),
                expected,
                "the plane must honor the client bitmap's actual pixel hotspot at {scale}x"
            );
            state.pointer_image = PointerImage::Shape(CursorShape::Text);
            state.sync_cursor_hotspot();
            let named = crate::compositor::cursor::bitmap(
                CursorShape::Text,
                crate::compositor::cursor::pixel_size(
                    state.accessibility.effective_cursor_size(),
                    scale,
                ),
            );
            assert_eq!(
                state.hw_cursor.hotspot_pixels(scale),
                (
                    i32::try_from(named.hotspot.0).unwrap(),
                    i32::try_from(named.hotspot.1).unwrap(),
                ),
                "named cursors must still follow the output density"
            );
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

    fn adaptive_sync_commands(rx: &Receiver<ScanoutCommand>) -> Vec<(u32, bool)> {
        rx.try_iter()
            .filter_map(|command| match command {
                ScanoutCommand::SetAdaptiveSync { output, on } => Some((output.0, on)),
                _ => None,
            })
            .collect()
    }

    fn hdr_commands(rx: &Receiver<ScanoutCommand>) -> Vec<(u32, Option<f32>)> {
        rx.try_iter()
            .filter_map(|command| match command {
                ScanoutCommand::SetHdr { output, request } => {
                    Some((output.0, request.map(|request| request.sdr_white_nits)))
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn hdr_is_requested_only_where_sink_driver_and_setting_agree() {
        let hdr_sink = crate::wayland::edid::HdrDisplayInfo {
            supported_transfers: vec![
                crate::collab::colorimetry::TransferCharacteristics::SmpteSt2084,
            ],
            ..crate::wayland::edid::HdrDisplayInfo::default_sdr()
        };
        // Phase `each_missing_piece_withholds_the_request`.
        assert!(hdr_request_for(None, true, Some(203.0), false).is_none());
        assert!(hdr_request_for(Some(&hdr_sink), false, Some(203.0), false).is_none());
        assert!(hdr_request_for(Some(&hdr_sink), true, None, false).is_none());
        assert!(hdr_request_for(Some(&hdr_sink), true, Some(203.0), true).is_none());
        assert_eq!(
            hdr_request_for(Some(&hdr_sink), true, Some(250.0), false)
                .map(|request| request.sdr_white_nits),
            Some(250.0)
        );

        let (mut state, rx) = dispatch_with_channel();
        let mut monitor = entry(0, 10, (3840, 2160), None);
        monitor.hdr_display = Some(hdr_sink);
        monitor.hdr_driver_ready = true;
        state.live = vec![monitor, entry(1, 11, (1920, 1080), None)];

        // Phase `turning_hdr_on_requests_it_once_and_brightness_resends`.
        state.reconcile_hdr();
        assert!(hdr_commands(&rx).is_empty(), "off by default");
        for live in &mut state.live {
            live.hdr_setting = Some(203.0);
        }
        state.reconcile_hdr();
        assert_eq!(
            hdr_commands(&rx),
            vec![(0, Some(203.0))],
            "SDR sink ignored"
        );
        state.reconcile_hdr();
        assert!(hdr_commands(&rx).is_empty(), "no repeat");
        if let Some(first) = state.live.first_mut() {
            first.hdr_setting = Some(320.0);
        }
        state.reconcile_hdr();
        assert_eq!(hdr_commands(&rx), vec![(0, Some(320.0))]);

        // Phase `a_failure_report_returns_the_output_to_sdr_for_good`.
        let (report_tx, report_rx) = channel();
        state.signal_reports = Some(report_rx);
        report_tx
            .send(SignalReport {
                output: OutputId(0),
                status: OutputSignalStatus {
                    hdr_failure: Some(settings_store::hardware::HdrUnavailable::DriverRejected),
                    ..OutputSignalStatus::default()
                },
            })
            .expect("report channel open");
        assert!(state.drain_signal_reports());
        assert_eq!(hdr_commands(&rx), vec![(0, None)]);
        state.reconcile_hdr();
        assert!(hdr_commands(&rx).is_empty(), "not retried");
    }

    #[test]
    fn adaptive_sync_follows_the_fullscreen_output_and_respects_refusals() {
        let (mut state, rx) = dispatch_with_channel();
        let mut capable = entry(0, 10, (2560, 1440), None);
        capable.adaptive_sync_supported = true;
        let plain = entry(1, 11, (1920, 1080), None);
        state.live = vec![capable, plain];

        // Phase `desktop_content_keeps_the_fixed_rate`.
        state.reconcile_adaptive_sync();
        assert!(adaptive_sync_commands(&rx).is_empty());
        assert!(!state.adaptive_sync_engaged());

        // Phase `fullscreen_on_a_capable_output_engages_it_once`.
        state.set_fullscreen_output(Some(OutputId(0)));
        assert_eq!(adaptive_sync_commands(&rx), vec![(0, true)]);
        assert!(state.adaptive_sync_engaged());
        state.set_fullscreen_output(Some(OutputId(0)));
        assert!(adaptive_sync_commands(&rx).is_empty(), "no repeat");

        // Phase `moving_fullscreen_to_an_output_without_vrr_releases_it`.
        state.set_fullscreen_output(Some(OutputId(1)));
        assert_eq!(adaptive_sync_commands(&rx), vec![(0, false)]);
        assert!(!state.adaptive_sync_engaged());

        // Phase `turning_it_off_in_settings_wins`.
        if let Some(first) = state.live.first_mut() {
            first.adaptive_sync_allowed = false;
        }
        state.set_fullscreen_output(Some(OutputId(0)));
        assert!(adaptive_sync_commands(&rx).is_empty());

        // Phase `a_driver_refusal_releases_and_is_not_retried`.
        if let Some(first) = state.live.first_mut() {
            first.adaptive_sync_allowed = true;
        }
        state.reconcile_adaptive_sync();
        assert_eq!(adaptive_sync_commands(&rx), vec![(0, true)]);
        let (report_tx, report_rx) = channel();
        state.signal_reports = Some(report_rx);
        report_tx
            .send(SignalReport {
                output: OutputId(0),
                status: OutputSignalStatus {
                    adaptive_sync_rejected: true,
                    ..OutputSignalStatus::default()
                },
            })
            .expect("report channel open");
        assert!(state.drain_signal_reports());
        assert_eq!(adaptive_sync_commands(&rx), vec![(0, false)]);
        assert_eq!(
            state
                .signal_status
                .get("TEST-10")
                .map(|status| status.adaptive_sync_rejected),
            Some(true)
        );
        state.set_fullscreen_output(None);
        state.set_fullscreen_output(Some(OutputId(0)));
        assert!(adaptive_sync_commands(&rx).is_empty());
    }

    #[derive(Clone)]
    struct FakeMasterCard {
        id: u64,
        calls: std::rc::Rc<std::cell::RefCell<Vec<(u64, MasterAction)>>>,
    }

    impl KmsMasterControl for FakeMasterCard {
        fn release_master(&self) {
            self.calls
                .borrow_mut()
                .push((self.id, MasterAction::Release));
        }

        fn acquire_master(&self) {
            self.calls
                .borrow_mut()
                .push((self.id, MasterAction::Acquire));
        }
    }

    #[test]
    fn drm_master_transitions_wait_for_presenter_retirement() {
        // Phase `session_master_transition_is_planned_for_every_retained_card`.
        {
            // No DRM fd or libseat session: this fake records the exact fan-out
            // used by `perform` between PauseScanout and Resume. A one-card loop
            // here would silently reproduce the hybrid-GPU VT/resume regression.
            let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
            let cards = [
                FakeMasterCard {
                    id: 11,
                    calls: std::rc::Rc::clone(&calls),
                },
                FakeMasterCard {
                    id: 22,
                    calls: std::rc::Rc::clone(&calls),
                },
            ];
            apply_master_action(cards.iter(), MasterAction::Release);
            apply_master_action(cards.iter(), MasterAction::Acquire);
            assert_eq!(
                *calls.borrow(),
                vec![
                    (11, MasterAction::Release),
                    (22, MasterAction::Release),
                    (11, MasterAction::Acquire),
                    (22, MasterAction::Acquire),
                ]
            );
        }
        // Phase `vt_pause_after_card_remove_releases_retiring_master_before_presenter_ack`.
        {
            // Card 22 was removed from the active map, but its presenter
            // scanout/fd has not acknowledged teardown yet. Dropping master on
            // only card 11 would leave the old KMS lease alive across the VT
            // handoff. Resume acquires only active card 11; card 22 is closing.
            let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
            let active = [FakeMasterCard {
                id: 11,
                calls: std::rc::Rc::clone(&calls),
            }];
            let retiring = [FakeMasterCard {
                id: 22,
                calls: std::rc::Rc::clone(&calls),
            }];
            release_masters_for_pause(active.iter(), retiring.iter());
            apply_master_action(active.iter(), MasterAction::Acquire);
            assert_eq!(
                *calls.borrow(),
                vec![
                    (11, MasterAction::Release),
                    (22, MasterAction::Release),
                    (11, MasterAction::Acquire),
                ]
            );
        }
        // Phase `dropping_master_waits_for_an_in_flight_present_to_retire`.
        {
            // The presenter and the libseat callback run on different threads in
            // production. A queued Pause alone is insufficient: the callback
            // must wait for a frame that already entered KMS before releasing
            // DRM master, or the kernel can reject that flip with EACCES.
            let gate = Arc::new(Mutex::new(()));
            let in_flight = gate.lock().expect("hold a present in flight");
            let (released_tx, released_rx) = channel();
            let callback_gate = Arc::clone(&gate);
            let callback = std::thread::spawn(move || {
                DrmDispatchState::with_presentations_quiesced(Some(&callback_gate), || {
                    released_tx
                        .send(())
                        .expect("callback receiver remains alive");
                });
            });

            assert!(
                released_rx
                    .recv_timeout(std::time::Duration::from_millis(20))
                    .is_err(),
                "master release must not run while a present still owns the gate"
            );
            drop(in_flight);
            tilo_test_env::recv(
                "master release proceeds once the present retires",
                &released_rx,
            );
            callback.join().expect("callback must finish");
        }
    }

    #[test]
    fn relayout_after_unplug_repacks_outputs_and_rehomes_the_pointer() {
        // Phase `relayout_packs_outputs_side_by_side_and_publishes_the_table`.
        {
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
        // Phase `unplugging_the_right_monitor_brings_the_pointer_back_onto_the_left_one`.
        {
            // The concrete "windows stranded off-screen" symptom that is actually load-bearing: windows re-flow (not pinned to an output), but the pointer lives in absolute combined coordinates and would be left on a gone panel.
            let (mut state, _rx) = dispatch_with_channel();
            state.live = vec![
                entry(0, 10, (1920, 1080), None),
                entry(1, 11, (1920, 1080), None),
            ];
            state.relayout_outputs();
            state.set_pointer_scene((3000.0, 500.0));

            state.live.retain(|l| l.id == OutputId(0));
            state.relayout_outputs();

            assert_eq!(state.output_size, (1920, 1080));
            assert_eq!(
                state.pointer,
                (1919.0, 500.0),
                "the pointer must land on the surviving panel, not off it"
            );
        }
        // Phase `unplugging_the_left_monitor_slides_the_survivor_to_the_origin`.
        {
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
        // Phase `fake_hybrid_gpu_layout_reflows_after_secondary_card_unplug`.
        {
            // Pure fake KMS state: connector and CRTC ids intentionally overlap
            // across the two cards.  The runtime's card-scoped identity must
            // still publish both outputs, route the combined layout across them,
            // and retract only the dock output when that card disappears.
            let (mut state, _rx) = dispatch_with_channel();
            state.live = vec![
                entry_on_device(41, 0, 10, (1920, 1080), Some((294, 165))),
                entry_on_device(99, 1, 10, (2560, 1440), Some((597, 336))),
            ];
            state.relayout_outputs();

            let table = state.output_table();
            assert_eq!(
                table.iter().map(|output| output.id).collect::<Vec<_>>(),
                [OutputId(0), OutputId(1)]
            );
            assert_eq!(table[0].position, (0, 0));
            assert_eq!(table[1].position, (1920, 0));
            assert_eq!(state.output_size, (4480, 1440));

            // This is the same layout record used by wl_output globals and by
            // cursor/touch routing.  A secondary-card unplug must leave no gap
            // and clamp an input position that was on the dock back onto the
            // laptop panel.
            state.set_pointer_scene((3300.0, 900.0));
            state.live.retain(|output| output.key.device == 41);
            state.relayout_outputs();
            assert_eq!(
                state
                    .output_table()
                    .iter()
                    .map(|output| output.id)
                    .collect::<Vec<_>>(),
                [OutputId(0)]
            );
            assert_eq!(state.output_size, (1920, 1080));
            // The 1.5x panel is 1280x720 logical; the pointer lands on its
            // nearest logical pixel and is drawn at that pixel's scene position.
            assert_eq!(state.logical_pointer, (1279.0, 719.0));
            assert_eq!(state.pointer, (1918.5, 1078.5));
        }
    }

    /// A 13.3" 2560x1600 panel (scale 2) docked beside a 24" 1080p monitor.
    fn docked_hidpi_laptop() -> DrmDispatchState {
        let (mut state, _rx) = dispatch_with_channel();
        state.live = vec![
            entry(0, 10, (2560, 1600), Some((286, 179))),
            entry(1, 11, (1920, 1080), Some((527, 296))),
        ];
        state.relayout_outputs();
        state
    }

    #[test]
    fn outputs_are_arranged_at_their_logical_size() {
        let state = docked_hidpi_laptop();
        assert_eq!(state.live[0].scale, 2.0);
        assert_eq!(state.live[1].scale, 1.0);
        let table = state.output_table();
        // Logically the monitor starts where the 1280-wide panel ends ...
        assert_eq!(table[0].logical_position, (0, 0));
        assert_eq!(table[1].logical_position, (1280, 0));
        // ... while in the painted scene it sits past the panel's pixels.
        assert_eq!(table[0].position, (0, 0));
        assert_eq!(table[1].position, (2560, 0));
        assert_eq!(state.output_size, (4480, 1600));
    }

    #[test]
    fn relative_motion_is_logical_and_crosses_a_mixed_scale_seam_on_the_same_row() {
        let mut state = docked_hidpi_laptop();
        state.set_pointer_logical((1270.0, 400.0));
        // Logical row 400 on the 2x panel is scene row 800.
        assert_eq!(state.pointer, (2540.0, 800.0));
        // Five logical pixels on the panel are ten of its physical ones.
        assert_eq!(state.move_pointer_by((5.0, 0.0)), (2550.0, 800.0));
        // Crossing the seam keeps logical row 400: scene row 400 on the 1x
        // monitor, not row 800 — and not a jump by the panel's density.
        assert_eq!(state.move_pointer_by((10.0, 0.0)), (2565.0, 400.0));
        assert_eq!(state.logical_pointer, (1285.0, 400.0));
        // And back again.
        assert_eq!(state.move_pointer_by((-10.0, 0.0)), (2550.0, 800.0));
    }

    #[test]
    fn motion_off_the_panel_s_bottom_edge_stops_at_its_last_row() {
        // Nothing lies below the panel; the monitor beside it is taller, but
        // motion straight down on the panel stops at the panel's last row
        // rather than jumping sideways onto the monitor.
        let mut state = docked_hidpi_laptop();
        state.set_pointer_logical((600.0, 790.0));
        state.move_pointer_by((0.0, 50.0));
        assert_eq!(state.logical_pointer, (600.0, 799.0));
    }

    #[test]
    fn output_scale_and_metadata_come_from_panel_or_setting() {
        // Phase `each_output_gets_the_scale_its_panel_calls_for`.
        {
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
        // Phase `a_display_scale_setting_overrides_every_output`.
        {
            let (mut state, _rx) = dispatch_with_channel();
            state.scale_override = Some(1.25);
            state.live = vec![entry(0, 10, (1920, 1080), Some((294, 165)))];
            state.relayout_outputs();
            assert_eq!(
                state.live[0].scale, 1.25,
                "a user who set a scale is not asking for a second opinion"
            );
        }
        // Phase `synthesize_for_tests_publishes_its_outputs`.
        {
            let state =
                DrmRuntime::synthesize_for_tests(&[("eDP-1", (1920, 1080)), ("DP-2", (1280, 720))]);
            assert_eq!(state.output_meta.len(), 2);
            assert_eq!(
                state
                    .output_table()
                    .iter()
                    .map(|output| (output.id, output.size))
                    .collect::<Vec<_>>(),
                [(OutputId(0), (1920, 1080)), (OutputId(1), (1280, 720))],
                "the seam must publish through the live set, as hardware bring-up does"
            );
        }
    }

    #[test]
    fn output_table_publishes_live_outputs_in_layout_order() {
        let (mut state, _rx) = dispatch_with_channel();
        // The external monitor enumerated first.
        state.live = vec![
            named(entry(0, 10, (2560, 1440), None), "DP-2"),
            named(entry(1, 11, (1920, 1080), None), "eDP-1"),
        ];
        state.relayout_with(&settings_store::hardware::DisplayConfig::default());
        // A record for an output no longer driven — unplugged or powered off since the last
        // relayout — must not be advertised to clients.
        state
            .output_meta
            .insert(OutputId(7), Output::new(OutputId(7), (3840, 2160), 1.0));

        let table = state.output_table();
        assert_eq!(
            table
                .iter()
                .map(|output| (output.id, output.size))
                .collect::<Vec<_>>(),
            [(OutputId(1), (1920, 1080)), (OutputId(0), (2560, 1440))],
            "the built-in panel is published first, the external after it, and only live outputs"
        );
        assert_eq!(table[0].position, (0, 0));
        assert_eq!(table[1].position, (1920, 0));
    }

    pub(super) fn named(mut entry: LiveEntry, connector: &str) -> LiveEntry {
        entry.connector_name = connector.into();
        entry.config_key = connector.into();
        entry
    }

    fn monitor(make: &str, model: &str, serial: &str) -> settings_store::hardware::MonitorIdentity {
        settings_store::hardware::MonitorIdentity {
            make: make.into(),
            model: model.into(),
            serial: serial.into(),
            physical_size_mm: None,
        }
    }

    #[test]
    fn an_unarranged_set_puts_the_built_in_panel_left_at_its_own_scale() {
        let (mut state, _rx) = dispatch_with_channel();
        // The external monitor enumerated first, yet the laptop panel is
        // still the leftmost display.
        state.live = vec![
            named(entry(0, 10, (2560, 1440), Some((597, 336))), "DP-1"),
            named(entry(1, 11, (1920, 1080), Some((294, 165))), "eDP-1"),
        ];
        state.relayout_with(&settings_store::hardware::DisplayConfig::default());
        let panel = state
            .live
            .iter()
            .find(|l| l.connector_name == "eDP-1")
            .unwrap();
        let external = state
            .live
            .iter()
            .find(|l| l.connector_name == "DP-1")
            .unwrap();
        assert_eq!(panel.logical_position, (0, 0));
        // 1920 px at 1.5 is 1280 logical pixels wide.
        assert_eq!(external.logical_position, (1280, 0));
        assert_eq!(panel.scale, 1.5);
        assert_eq!(external.scale, 1.0);
    }

    #[test]
    fn the_published_snapshot_carries_each_live_outputs_own_scale() {
        use settings_store::hardware::{DisplayConfig, DisplayOutputSnapshot, OutputDescriptor};

        let (mut state, _rx) = dispatch_with_channel();
        state.live = vec![
            named(entry(0, 10, (2560, 1440), Some((597, 336))), "DP-1"),
            named(entry(1, 11, (1920, 1080), Some((294, 165))), "eDP-1"),
        ];
        // Nothing saved: the compositor picks each scale itself.
        state.relayout_with(&DisplayConfig::default());
        let capability = |connector: &str| DisplayOutputSnapshot {
            descriptor: OutputDescriptor {
                connector: connector.into(),
                connector_label: connector.into(),
                modes: Vec::new(),
                hdr: false,
                vrr: false,
                color_profiles: Vec::new(),
                monitor: None,
            },
            current_mode: settings_store::hardware::OutputMode {
                width: 1920,
                height: 1080,
                refresh_mhz: 60_000,
            },
            scale: 1.0,
            builtin: false,
            sink_hdr: false,
            hdr_unavailable_reason: None,
            sink_vrr: false,
            vrr_unavailable_reason: None,
            color_profile_labels: BTreeMap::new(),
        };
        let mut outputs = vec![
            capability("DP-1"),
            capability("eDP-1"),
            capability("HDMI-A-1"),
        ];
        apply_live_scales(
            &mut outputs,
            state
                .live
                .iter()
                .map(|entry| (entry.config_key.as_str(), entry.scale)),
        );
        assert_eq!(outputs[1].scale, 1.5, "the panel's own 1.5x");
        assert_eq!(outputs[0].scale, 1.0);
        assert_eq!(outputs[2].scale, 1.0, "an output that is off keeps its own");
    }

    #[test]
    fn the_output_table_lists_the_primary_chosen_in_settings_first() {
        use settings_store::hardware::{DisplayConfig, OutputConfig};

        let (mut state, _rx) = dispatch_with_channel();
        state.live = vec![
            entry(0, 10, (1920, 1080), Some((527, 296))),
            entry(1, 11, (2560, 1440), Some((597, 336))),
        ];
        let mut config = DisplayConfig::default();
        config.outputs.insert(
            "TEST-11".into(),
            OutputConfig {
                primary: true,
                ..OutputConfig::default()
            },
        );
        state.relayout_with(&config);
        let table = state.output_table();
        assert_eq!(
            table.iter().map(|output| output.id).collect::<Vec<_>>(),
            [OutputId(1), OutputId(0)],
            "the shell, the lock screen and pointer clamping take the first output as primary"
        );
    }

    #[test]
    fn swapping_the_monitor_on_a_port_swaps_to_that_sets_layout() {
        use settings_store::hardware::{
            AttachedOutput, DisplayConfig, DisplayProfiles, DisplaySet, OutputConfig,
        };

        let panel = monitor("BOE", "NE135FBM-N41", "");
        let office = monitor("Dell", "DELL U2720Q", "7XK2P13");
        let visitor = monitor("Samsung", "LS27A600U", "H4ZR");
        let set_with = |external: &settings_store::hardware::MonitorIdentity| {
            DisplaySet::of([
                AttachedOutput {
                    connector: "eDP-1",
                    connector_label: "eDP-1",
                    monitor: Some(&panel),
                },
                AttachedOutput {
                    connector: "DP-2",
                    connector_label: "DP-2",
                    monitor: Some(external),
                },
            ])
        };

        // At the office the external monitor stands left of the laptop.
        let mut office_layout = DisplayConfig::default();
        office_layout.outputs.insert(
            "DP-2".into(),
            OutputConfig {
                x: -2560,
                primary: true,
                ..OutputConfig::default()
            },
        );
        office_layout.outputs.insert(
            "eDP-1".into(),
            OutputConfig {
                scale: 1.25,
                ..OutputConfig::default()
            },
        );
        let mut profiles = DisplayProfiles::default();
        profiles.remember(&set_with(&office), &office_layout);

        let (mut state, _rx) = dispatch_with_channel();
        state.live = vec![
            named(entry(0, 10, (1920, 1080), Some((294, 165))), "eDP-1"),
            named(entry(1, 11, (2560, 1440), Some((597, 336))), "DP-2"),
        ];
        let at = |state: &DrmDispatchState, connector: &str| {
            let entry = state
                .live
                .iter()
                .find(|l| l.connector_name == connector)
                .expect("live output");
            (entry.logical_position, entry.scale)
        };

        let office_config = profiles.layout_for(&set_with(&office)).expect("saved");
        state.relayout_with(&office_config);
        assert_eq!(at(&state, "DP-2"), ((0, 0), 1.0));
        assert_eq!(at(&state, "eDP-1"), ((2560, 0), 1.25));

        // Someone else's monitor on the same port inherits none of it.
        let visitor_config = profiles.layout_for(&set_with(&visitor)).unwrap_or_default();
        state.relayout_with(&visitor_config);
        assert_eq!(at(&state, "eDP-1"), ((0, 0), 1.5));
        assert_eq!(at(&state, "DP-2"), ((1280, 0), 1.0));

        // Back at the office, the office arrangement returns.
        state.relayout_with(&profiles.layout_for(&set_with(&office)).expect("saved"));
        assert_eq!(at(&state, "DP-2"), ((0, 0), 1.0));
        assert_eq!(at(&state, "eDP-1"), ((2560, 0), 1.25));
    }

    #[test]
    fn physical_key_timestamp_survives_translation_but_not_synthetic_repeat() {
        let mut state = keyboard_dispatch(AccessibilityInputSettings::default());
        let a = xkb::Keycode::new(38);
        let origin = 123_456_789_000;
        state.handle_keyboard_key(a, true, Some(origin));
        assert!(state.events.iter().any(|event| matches!(event,
            BackendEvent::PhysicalKey { event: WindowEvent::KeyboardInput { event, .. }, input_time_ns }
                if *input_time_ns == origin && event.state == ElementState::Pressed)));
        state.events.clear();
        state.process_keyboard_key(a, false);
        assert!(state.events.iter().any(|event| matches!(
            event,
            BackendEvent::Input(WindowEvent::KeyboardInput { .. })
        )));
        assert!(
            !state
                .events
                .iter()
                .any(|event| matches!(event, BackendEvent::PhysicalKey { .. }))
        );
        state.key_repeat.press(
            a,
            std::time::Instant::now()
                .checked_sub(std::time::Duration::from_secs(1))
                .expect("fixture clock permits an earlier repeat deadline"),
            std::time::Duration::from_millis(1),
            25,
        );
        state.events.clear();
        state.pump_keyboard_repeat(std::time::Instant::now());
        assert!(state.events.iter().any(|event| matches!(event, BackendEvent::Input(WindowEvent::KeyboardInput { event, .. }) if event.repeat)));
        assert!(
            !state
                .events
                .iter()
                .any(|event| matches!(event, BackendEvent::PhysicalKey { .. }))
        );
    }

    fn keyboard_dispatch(accessibility: AccessibilityInputSettings) -> DrmDispatchState {
        let (mut state, _rx) = dispatch_with_channel();
        state.keyboard = Some(
            KeyboardXkb::from_settings(&KeyboardSettings::default())
                .expect("the default test keymap must compile"),
        );
        state.accessibility = accessibility;
        state
    }

    #[test]
    fn keyboard_assignments_switch_before_first_key_and_retire_old_modifiers() {
        use settings_store::keyboard_sources::KeyboardProfile;
        let mut state = keyboard_dispatch(AccessibilityInputSettings::default());
        state.keyboard_config.keyboard_assignments.insert(
            "german".into(),
            KeyboardProfile {
                sources: vec!["de".into()],
                active: "de".into(),
                options: BTreeMap::default(),
            },
        );
        state.handle_keyboard_device_key("default", xkb::Keycode::new(50), true, None);
        state.events.clear();
        state.handle_keyboard_device_key("german", xkb::Keycode::new(29), true, None);
        let reset = state
            .events
            .iter()
            .position(|event| matches!(event, BackendEvent::InputStateReset))
            .unwrap();
        let configuration = state.events.iter().position(|event| matches!(event, BackendEvent::KeyboardConfiguration { scope, .. } if scope == "german")).unwrap();
        let typed = state
            .events
            .iter()
            .position(|event| {
                matches!(
                    event,
                    BackendEvent::Input(WindowEvent::KeyboardInput { .. })
                )
            })
            .unwrap();
        assert!(reset < configuration && configuration < typed);
        let BackendEvent::Input(WindowEvent::KeyboardInput { event, .. }) = &state.events[typed]
        else {
            unreachable!()
        };
        assert_eq!(event.logical_key, Key::Character("z".into()));
        state.events.clear();
        state.handle_keyboard_device_key("default", xkb::Keycode::new(50), false, None);
        assert_eq!(
            typed_keys(&state),
            0,
            "a retired modifier release cannot alter the new map"
        );
        state.handle_keyboard_device_key("german", xkb::Keycode::new(29), false, None);
        assert_eq!(typed_keys(&state), 1);
    }

    #[test]
    fn keyboard_assignments_share_keys_until_both_devices_release() {
        let mut state = keyboard_dispatch(AccessibilityInputSettings::default());
        let key = xkb::Keycode::new(38);
        state.handle_keyboard_device_key("first", key, true, None);
        state.handle_keyboard_device_key("second", key, true, None);
        assert_eq!(typed_keys(&state), 1);
        state.handle_keyboard_device_key("first", key, false, None);
        assert_eq!(typed_keys(&state), 1);
        state.handle_keyboard_device_key("second", key, false, None);
        assert_eq!(typed_keys(&state), 2);
        assert!(state.keyboard_key_owners.is_empty());
    }

    #[test]
    fn keyboard_assignments_unplug_releases_only_the_vanished_devices_keys() {
        let mut state = keyboard_dispatch(AccessibilityInputSettings::default());
        let key = xkb::Keycode::new(38);
        let shift = xkb::Keycode::new(50);
        state.handle_keyboard_device_key("first", shift, true, None);
        state.handle_keyboard_device_key("first", key, true, None);
        state.handle_keyboard_device_key("second", key, true, None);
        state.events.clear();
        state.retire_keyboard_device("first");
        assert_eq!(typed_keys(&state), 1, "unplugging releases the owned Shift");
        assert_eq!(state.keyboard_key_owners[&key.raw()].len(), 1);
        assert!(!state.keyboard_key_owners.contains_key(&shift.raw()));
        state.retire_keyboard_device("second");
        assert_eq!(
            typed_keys(&state),
            2,
            "the shared key releases with its last owner"
        );
        assert!(state.keyboard_key_owners.is_empty());
    }

    fn typed_keys(state: &DrmDispatchState) -> usize {
        state
            .events
            .iter()
            .filter(|e| matches!(e, BackendEvent::Input(WindowEvent::KeyboardInput { .. })))
            .count()
    }

    #[test]
    fn bounce_keys_swallow_a_rebound_press_and_flash_when_asked() {
        let mut state = keyboard_dispatch(AccessibilityInputSettings {
            bounce_keys: true,
            bounce_keys_alert: true,
            ..Default::default()
        });
        let a = xkb::Keycode::new(38); // KEY_A + 8
        state.handle_keyboard_key(a, true, None);
        state.handle_keyboard_key(a, false, None);
        assert_eq!(typed_keys(&state), 2, "the first keystroke is real");
        state.events.clear();

        // Pressed again well inside the 300 ms default window.
        state.handle_keyboard_key(a, true, None);
        state.handle_keyboard_key(a, false, None);
        assert_eq!(typed_keys(&state), 0, "press and release both swallowed");
        assert_eq!(
            state
                .events
                .iter()
                .filter(|e| matches!(e, BackendEvent::VisualAlert))
                .count(),
            1,
            "one flash per swallowed press"
        );
    }

    #[test]
    fn mouse_keys_turn_the_keypad_into_clicks_instead_of_typing() {
        let mut state = keyboard_dispatch(AccessibilityInputSettings {
            mouse_keys: true,
            ..Default::default()
        });
        let kp5 = xkb::Keycode::new(76 + 8);
        state.handle_keyboard_key(kp5, true, None);
        state.handle_keyboard_key(kp5, false, None);
        let clicks: Vec<_> = state
            .events
            .iter()
            .filter_map(|e| match e {
                BackendEvent::Input(WindowEvent::MouseInput { state, button, .. }) => {
                    Some((*state, *button))
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            clicks,
            [
                (ElementState::Pressed, MouseButton::Left),
                (ElementState::Released, MouseButton::Left)
            ]
        );
        assert_eq!(typed_keys(&state), 0);

        state.events.clear();
        state.accessibility.mouse_keys = false;
        state.handle_keyboard_key(kp5, true, None);
        state.handle_keyboard_key(kp5, false, None);
        assert_eq!(
            typed_keys(&state),
            2,
            "with mouse keys off the keypad types"
        );
    }

    #[test]
    fn switching_mouse_keys_off_mid_drag_releases_the_button() {
        let mut state = keyboard_dispatch(AccessibilityInputSettings {
            mouse_keys: true,
            ..Default::default()
        });
        let kp0 = xkb::Keycode::new(82 + 8);
        state.handle_keyboard_key(kp0, true, None);
        state.events.clear();
        state.release_mouse_keys();
        assert!(state.events.iter().any(|e| matches!(
            e,
            BackendEvent::Input(WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            })
        )));
    }

    #[test]
    fn vt_switch_and_resume_pause_and_rebuild_scanout() {
        // Phase `a_vt_switch_away_pauses_the_scanouts`.
        {
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
        // Phase `a_vt_switch_back_tells_the_scanouts_to_re_modeset_and_forces_a_repaint`.
        {
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
        // Phase `a_session_blanked_for_idle_resumes_blanked`.
        {
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
        // Phase `resume_from_sleep_asks_for_the_same_rebuild_a_vt_switch_does`.
        {
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
        // Phase `a_headless_drm_session_survives_a_vt_switch_with_no_channel`.
        {
            // No output came up, so there is nobody to send commands to — performing the sequence anyway must not panic.
            let mut state = DrmDispatchState::default();
            let session = SessionState::new_active();
            state.session = Some(session.clone());
            state.perform_all(session.apply_with_actions(SeatTick::Pause));
            state.perform_all(session.apply_with_actions(SeatTick::Activate));
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn key_repeat_and_held_keys_follow_the_winit_contract() {
        // Phase `drm_repeat_event_matches_winit_contract_without_repressing_xkb`.
        fn drm_repeat_event_matches_winit_contract_without_repressing_xkb() {
            let Some(mut keyboard) = KeyboardXkb::from_settings(&KeyboardSettings::default())
            else {
                return;
            };
            let key = xkb::Keycode::new(38); // evdev KEY_A + 8
            assert!(keyboard.key_repeats(key));
            let _ = keyboard.process(key, true);
            let WindowEvent::KeyboardInput { event, .. } = keyboard.repeat_event(key) else {
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
        // Phase `key_repeat_waits_for_delay_ticks_at_rate_and_stops_on_release`.
        {
            let start = std::time::Instant::now();
            let key = xkb::Keycode::new(38); // evdev KEY_A + 8
            let mut repeat = KeyRepeat::default();
            assert_eq!(repeat.next_deadline(), None);
            repeat.press(key, start, std::time::Duration::from_millis(300), 20);
            assert_eq!(
                repeat.next_deadline(),
                Some(start + std::time::Duration::from_millis(300))
            );
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
                repeat.next_deadline(),
                Some(start + std::time::Duration::from_millis(350))
            );
            assert_eq!(
                repeat.due(start + std::time::Duration::from_millis(401)),
                [key, key]
            );
            repeat.release(key);
            assert_eq!(repeat.next_deadline(), None);
            assert!(
                repeat
                    .due(start + std::time::Duration::from_secs(1))
                    .is_empty()
            );
        }
        // Phase `key_repeat_rate_zero_is_disabled_and_transient_loss_cancels`.
        {
            let start = std::time::Instant::now();
            let key = xkb::Keycode::new(22); // evdev KEY_BACKSPACE + 8
            let mut repeat = KeyRepeat::default();

            repeat.press(key, start, std::time::Duration::ZERO, 0);
            assert!(
                repeat
                    .due(start + std::time::Duration::from_secs(2))
                    .is_empty(),
                "rate 0 is the protocol-level disabled value, not a 1 Hz repeat"
            );

            repeat.press(key, start, std::time::Duration::ZERO, 30);
            assert_eq!(repeat.due(start), [key]);
            repeat.cancel();
            assert!(
                repeat
                    .due(start + std::time::Duration::from_secs(2))
                    .is_empty(),
                "seat/device loss must retire a held destructive key"
            );
        }
        // Phase `drm_repeat_event_matches_winit_contract_without_repressing_xkb` (defined above).
        drm_repeat_event_matches_winit_contract_without_repressing_xkb();
        // Phase `resume_retires_held_shift_and_backspace_before_the_next_key`.
        {
            // Model the failure mode exactly: both keys go down, then the machine
            // sleeps before libinput can report either release.  The first input
            // after resume must be a clean `a`, not Shift+A, and the pending
            // Backspace repeat must be gone.
            let (mut state, _rx) = dispatch_with_channel();
            state.session = Some(SessionState::new_active());
            state.keyboard = Some(
                KeyboardXkb::from_settings(&KeyboardSettings::default())
                    .expect("the default test keymap must compile"),
            );

            state.handle_keyboard_key(xkb::Keycode::new(50), true, None); // KEY_LEFTSHIFT + 8
            state.handle_keyboard_key(xkb::Keycode::new(22), true, None); // KEY_BACKSPACE + 8
            assert!(
                state
                    .keyboard
                    .as_ref()
                    .is_some_and(|keyboard| keyboard.current_mods().shift_key()),
                "the adapter's XKB state must see the held Shift before sleep"
            );
            assert!(
                state.key_repeat.held.is_some(),
                "Backspace must be repeating before sleep"
            );
            state.events.clear(); // The pre-sleep input was already dispatched.

            state.resume_from_sleep();

            assert!(
                matches!(state.events.first(), Some(BackendEvent::InputStateReset)),
                "the compositor's canonical seat reset must precede resumed input"
            );
            assert!(state.physical_modifiers.is_empty());
            assert!(state.sticky_latched.is_empty());
            assert!(
                state
                    .key_repeat
                    .due(std::time::Instant::now() + std::time::Duration::from_secs(2))
                    .is_empty(),
                "a Backspace held across sleep must not repeat after resume"
            );

            state.events.clear();
            state.handle_keyboard_key(xkb::Keycode::new(38), true, None); // KEY_A + 8
            assert!(
                !state
                    .events
                    .iter()
                    .any(|event| matches!(event, BackendEvent::ModifiersChanged(_))),
                "the first post-resume key must not inherit Shift"
            );
            let Some(BackendEvent::Input(WindowEvent::KeyboardInput { event, .. })) =
                state.events.first()
            else {
                panic!("the first post-resume key must be dispatched")
            };
            assert_eq!(event.logical_key, Key::Character("a".into()));
            assert_eq!(event.text.as_deref(), Some("a"));
            assert!(!event.repeat, "the first post-resume key is a fresh press");
        }
    }

    // Gesture routing, on the production `handle_gesture`

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn touchpad_gestures_route_between_shell_and_client() {
        // Phase `a_three_finger_swipe_becomes_a_shell_gesture_and_never_reaches_a_client`.
        {
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
            assert_eq!(shell, vec![GestureAction::SwitchStrip(-1)]);
            assert!(
                !state
                    .events
                    .iter()
                    .any(|e| matches!(e, BackendEvent::Gesture(_))),
                "a gesture the shell acted on must not also be delivered to a \
                 client — the app would scroll under the workspace switch"
            );
        }
        // Phase `horizontal_three_finger_updates_stream_to_the_shell_until_end`.
        {
            let mut state = DrmDispatchState::default();
            for event in [
                GestureEvent::SwipeBegin { fingers: 3 },
                GestureEvent::SwipeUpdate { dx: -12.0, dy: 0.0 },
                GestureEvent::SwipeUpdate { dx: -28.0, dy: 2.0 },
                GestureEvent::SwipeEnd { cancelled: false },
            ] {
                state.handle_gesture(event);
            }

            let shell: Vec<_> = state
                .events
                .iter()
                .filter_map(|event| match event {
                    BackendEvent::ShellGesture(action) => Some(*action),
                    _ => None,
                })
                .collect();
            assert_eq!(
                shell,
                vec![
                    GestureAction::StripPanUpdate {
                        travel: 12.0,
                        began: true,
                    },
                    GestureAction::StripPanUpdate {
                        travel: 40.0,
                        began: false,
                    },
                    GestureAction::StripPanEnd { cancelled: false },
                ]
            );
            assert!(
                !state
                    .events
                    .iter()
                    .any(|event| matches!(event, BackendEvent::Gesture(_))),
                "no frame of a shell-owned direct pan may leak to the client"
            );
        }
        // Phase `the_closing_event_of_a_shell_gesture_does_not_leak_to_the_client`.
        {
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
        // Phase `a_hold_gesture_ends_where_it_began`.
        {
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
        // Phase `a_two_finger_pinch_is_forwarded_to_the_client_and_drives_nothing`.
        {
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
        // Phase `natural_scroll_flips_the_gesture_direction_through_the_dispatch_state`.
        {
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
                    BackendEvent::ShellGesture(GestureAction::SwitchStrip(1))
                )),
                "the same preference that inverts scrolling inverts gestures"
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn touch_streams_split_client_touch_from_chrome_clicks() {
        // Phase `three_finger_touch_cancels_the_partial_client_stream_and_never_clicks_or_drags`.
        {
            let mut state = DrmDispatchState::default();
            for (slot, x) in [(1, 100.0), (2, 140.0), (3, 180.0)] {
                state.handle_direct_touch_down(test_touch_key("panel", slot), x, 80.0);
            }
            for (slot, x) in [(1, 40.0), (2, 80.0), (3, 120.0)] {
                state.handle_direct_touch_motion(&test_touch_key("panel", slot), x, 80.0);
            }
            for slot in [1, 2, 3] {
                state.handle_direct_touch_up(&test_touch_key("panel", slot));
            }

            let client_touch: Vec<_> = state
                .events
                .iter()
                .filter_map(|event| match event {
                    BackendEvent::Touch(touch) => Some(*touch),
                    _ => None,
                })
                .collect();
            assert_eq!(
                client_touch,
                [
                    TiloTouchEvent::Down {
                        slot: 0,
                        x: 100.0,
                        y: 80.0,
                    },
                    TiloTouchEvent::Down {
                        slot: 1,
                        x: 140.0,
                        y: 80.0,
                    },
                    TiloTouchEvent::Cancel,
                ],
                "the early contacts are explicitly cancelled; no third contact, motion, or release reaches the client"
            );
            assert!(
                !state.events.iter().any(|event| matches!(
                    event,
                    BackendEvent::Input(
                        WindowEvent::CursorMoved { .. } | WindowEvent::MouseInput { .. }
                    )
                )),
                "a shell-owned multi-touch stream must not synthesize client/chrome pointer motion, press, drag, or click"
            );
            let shell_touch_phases: Vec<_> = state
                .events
                .iter()
                .filter_map(|event| match event {
                    BackendEvent::Input(WindowEvent::Touch(touch)) => Some(touch.phase),
                    _ => None,
                })
                .collect();
            assert_eq!(
                shell_touch_phases,
                [
                    TouchPhase::Started,
                    TouchPhase::Started,
                    TouchPhase::Started,
                    TouchPhase::Moved,
                    TouchPhase::Moved,
                    TouchPhase::Moved,
                    TouchPhase::Ended,
                    TouchPhase::Ended,
                    TouchPhase::Ended,
                ],
                "the shell still receives the complete raw stream needed by its three-finger arena"
            );
        }
        // Phase `a_single_touch_reaches_the_client_and_never_moves_the_pointer`.
        {
            let mut state = DrmDispatchState::default();
            let pointer = state.pointer;
            let key = test_touch_key("panel", 7);
            state.handle_direct_touch_down(key.clone(), 320.0, 240.0);
            state.handle_direct_touch_motion(&key, 324.0, 242.0);
            state.handle_direct_touch_up(&key);

            let client_touch: Vec<_> = state
                .events
                .iter()
                .filter_map(|event| match event {
                    BackendEvent::Touch(touch) => Some(*touch),
                    _ => None,
                })
                .collect();
            assert_eq!(
                client_touch,
                [
                    TiloTouchEvent::Down {
                        slot: 0,
                        x: 320.0,
                        y: 240.0,
                    },
                    TiloTouchEvent::Motion {
                        slot: 0,
                        x: 324.0,
                        y: 242.0,
                    },
                    TiloTouchEvent::Up { slot: 0 },
                ]
            );
            assert_eq!(
                state.pointer, pointer,
                "a finger is not the mouse: touch leaves the pointer where it was"
            );
            assert!(
                !state.events.iter().any(|event| matches!(
                    event,
                    BackendEvent::Input(
                        WindowEvent::CursorMoved { .. } | WindowEvent::MouseInput { .. }
                    )
                )),
                "the backend must not click on its own: only the compositor knows whether the touch reached a client that understands it"
            );
        }
        // Phase `moved_single_touch_never_becomes_a_release_position_click`.
        {
            let mut state = DrmDispatchState::default();
            let key = test_touch_key("panel", 0);
            state.handle_direct_touch_down(key.clone(), 10.0, 20.0);
            state.handle_direct_touch_motion(&key, 80.0, 20.0);
            state.handle_direct_touch_up(&key);
            assert!(
                !state.events.iter().any(|event| matches!(
                    event,
                    BackendEvent::Input(
                        WindowEvent::CursorMoved { .. } | WindowEvent::MouseInput { .. }
                    )
                )),
                "a drag is never turned into pointer input by the backend"
            );
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn touch_slots_are_global_and_cancelled_before_reuse() {
        // Phase `touch_ids_are_global_across_devices_that_both_report_slot_zero`.
        {
            let mut state = DrmDispatchState::default();
            state.handle_direct_touch_down(test_touch_key("panel-a", 0), 10.0, 20.0);
            state.handle_direct_touch_down(test_touch_key("panel-b", 0), 30.0, 40.0);

            let ids: Vec<_> = state
                .events
                .iter()
                .filter_map(|event| match event {
                    BackendEvent::Touch(TiloTouchEvent::Down { slot, .. }) => Some(*slot),
                    _ => None,
                })
                .collect();
            assert_eq!(ids, [0, 1]);
            let shell_ids: Vec<_> = state
                .events
                .iter()
                .filter_map(|event| match event {
                    BackendEvent::Input(WindowEvent::Touch(touch)) => Some(touch.id),
                    _ => None,
                })
                .collect();
            assert_eq!(shell_ids, [0, 1]);
        }
        // Phase `final_touch_up_and_cancel_each_keep_their_closing_frame`.
        {
            let mut state = DrmDispatchState::default();
            let key = test_touch_key("panel", 0);
            state.handle_direct_touch_down(key.clone(), 10.0, 20.0);
            state.flush_direct_touch_frame();
            state.handle_direct_touch_up(&key);
            state.flush_direct_touch_frame();
            let client_events: Vec<_> = state
                .events
                .iter()
                .filter(|event| matches!(event, BackendEvent::Touch(_)))
                .collect();
            assert!(matches!(
                client_events.as_slice(),
                [
                    BackendEvent::Touch(TiloTouchEvent::Down { .. }),
                    BackendEvent::Touch(TiloTouchEvent::Frame),
                    BackendEvent::Touch(TiloTouchEvent::Up { .. }),
                    BackendEvent::Touch(TiloTouchEvent::Frame),
                ]
            ));

            let mut state = DrmDispatchState::default();
            state.handle_direct_touch_down(test_touch_key("panel", 0), 10.0, 20.0);
            state.flush_direct_touch_frame();
            state.handle_direct_touch_cancel();
            state.flush_direct_touch_frame();
            assert!(matches!(
                state.events.as_slice(),
                [
                    BackendEvent::Touch(TiloTouchEvent::Down { .. }),
                    BackendEvent::Input(WindowEvent::Touch(_)),
                    BackendEvent::Touch(TiloTouchEvent::Frame),
                    BackendEvent::Input(WindowEvent::Touch(touch)),
                    BackendEvent::Touch(TiloTouchEvent::Cancel),
                ] if touch.phase == TouchPhase::Cancelled
            ));
        }
        // Phase `session_pause_and_suspend_resume_cancel_active_touch_before_slot_reuse`.
        {
            let mut state = DrmDispatchState::default();
            let key = test_touch_key("panel", 0);
            state.handle_direct_touch_down(key, 10.0, 20.0);
            state.handle_session_event(smithay::backend::session::Event::PauseSession);
            assert!(state.events.iter().any(|event| matches!(
                event,
                BackendEvent::Input(WindowEvent::Touch(touch)) if touch.phase == TouchPhase::Cancelled
            )));
            assert!(
                state
                    .events
                    .iter()
                    .any(|event| matches!(event, BackendEvent::Touch(TiloTouchEvent::Cancel)))
            );
            assert!(
                !state
                    .events
                    .iter()
                    .any(|event| matches!(event, BackendEvent::Touch(TiloTouchEvent::Frame)))
            );

            state.events.clear();
            state.handle_direct_touch_down(test_touch_key("panel", 0), 30.0, 40.0);
            state.resume_from_sleep();
            assert!(state.events.iter().any(|event| matches!(
                event,
                BackendEvent::Input(WindowEvent::Touch(touch)) if touch.phase == TouchPhase::Cancelled
            )));
            assert!(
                state
                    .events
                    .iter()
                    .any(|event| matches!(event, BackendEvent::Touch(TiloTouchEvent::Cancel)))
            );
            assert!(
                !state
                    .events
                    .iter()
                    .any(|event| matches!(event, BackendEvent::Touch(TiloTouchEvent::Frame)))
            );
        }
        // Phase `touchscreen_removal_cancels_the_seat_stream_before_reusing_its_slot`.
        {
            let mut state = DrmDispatchState::default();
            state.handle_direct_touch_down(test_touch_key("removed-panel", 0), 10.0, 20.0);
            state.handle_direct_touch_source_removed();
            state.flush_direct_touch_frame();
            state.handle_direct_touch_down(test_touch_key("replacement-panel", 0), 30.0, 40.0);
            let touches: Vec<_> = state
                .events
                .iter()
                .filter_map(|event| match event {
                    BackendEvent::Touch(touch) => Some(*touch),
                    _ => None,
                })
                .collect();
            assert!(matches!(
                touches.as_slice(),
                [
                    TiloTouchEvent::Down { slot: 0, .. },
                    TiloTouchEvent::Cancel,
                    TiloTouchEvent::Down { slot: 1, .. },
                ]
            ));
        }
    }

    #[test]
    fn pointer_buttons_scroll_units_and_touchpad_plan() {
        // Phase `drm_button_mapping_preserves_back_and_forward`.
        assert_eq!(map_mouse_button(0x110), Some(MouseButton::Left));
        assert_eq!(map_mouse_button(0x111), Some(MouseButton::Right));
        assert_eq!(map_mouse_button(0x112), Some(MouseButton::Middle));
        assert_eq!(map_mouse_button(0x113), Some(MouseButton::Back));
        assert_eq!(map_mouse_button(0x114), Some(MouseButton::Forward));
        assert_eq!(map_mouse_button(0x115), Some(MouseButton::Forward));
        assert_eq!(map_mouse_button(0x116), Some(MouseButton::Back));
        assert_eq!(map_mouse_button(0x117), None);
        // Phase `touchpad_scroll_keeps_pixel_precision_and_sign`.
        {
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
        // Phase `wheel_scroll_uses_line_units_without_losing_high_resolution_steps`.
        {
            let WindowEvent::MouseWheel { delta, .. } =
                mouse_wheel_event(AxisSource::Wheel, 7.5, -30.0)
            else {
                panic!("expected a mouse-wheel event")
            };
            assert_eq!(delta, MouseScrollDelta::LineDelta(-0.5, 2.0));
        }
        // Phase `a_touchpad_plan_turns_tapping_on_by_default`.
        {
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
                ..DeviceCaps::default()
            };
            let plan = input_config::plan_for(&state.input, &caps);
            assert_eq!(plan.tap, Some(true));
            assert_eq!(plan.disable_while_typing, Some(true));
            assert_eq!(plan.accel_speed(), Some(0.0));
        }
    }

    #[test]
    fn display_reload_never_force_probes() {
        // A display settings write — Settings, Super+P, output management,
        // or this backend's own transaction write — re-applies the
        // arrangement from the last hotplug probe. Force-probing every KMS
        // connector here stalled pointer and keyboard dispatch for seconds
        // on DP-MST docks.
        let mut state = DrmDispatchState::default();
        state.reload_display_settings();
        assert!(
            state
                .events
                .iter()
                .any(|event| matches!(event, BackendEvent::OutputsChanged)),
            "the reload ran and told the compositor the outputs were re-applied"
        );
        state.reapply_outputs_from_cache("display settings changed");
        state.publish_display_snapshot();
        assert_eq!(
            state.forced_probes, 0,
            "a settings reload and capability publication must reuse the last probe"
        );

        // A udev edge is when connector state is stale: that, and only
        // that, asks the hardware.
        state.rescan_outputs("udev device changed");
        assert_eq!(state.forced_probes, 1);
        state.reload_display_settings();
        assert_eq!(
            state.forced_probes, 1,
            "and the next settings reload reuses it"
        );
    }

    // libinput device configuration, on the production plan
}
