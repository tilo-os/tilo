//! The Wayland side of the compositor: protocol globals, their handlers, the
//! mapped windows and the seat.
//!
//! The state answers clients and keeps the window stack; it decides nothing
//! about where windows go or who gets focus: that is the [`WindowPolicy`]'s.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "smithay geometry: logical and physical coordinates of outputs and windows, far from i32 limits"
)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use smithay::backend::input::KeyState;
use smithay::backend::input::{Axis, ButtonState};
use smithay::backend::renderer::utils::{on_commit_buffer_handler, with_renderer_surface_state};
use smithay::desktop::{PopupKind, PopupManager, Space, Window, WindowSurfaceType};
use smithay::input::keyboard::{FilterResult, XkbConfig};
use smithay::input::pointer::{AxisFrame, ButtonEvent, CursorImageStatus, MotionEvent};
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::output::Output;
use smithay::reexports::wayland_server::backend::{ClientData, ClientId, DisconnectReason};
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_seat::WlSeat;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Client, DisplayHandle, Resource as _};
use smithay::utils::{IsAlive, Logical, Point, Rectangle, SERIAL_COUNTER, Serial};
use smithay::wayland::buffer::BufferHandler;
use smithay::wayland::compositor::{
    CompositorClientState, CompositorHandler, CompositorState, get_parent, is_sync_subsurface,
};
use smithay::wayland::output::{OutputHandler, OutputManagerState};
use smithay::wayland::selection::SelectionHandler;
use smithay::wayland::selection::data_device::{
    ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler,
};
use smithay::wayland::shell::xdg::{
    PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
};
use smithay::wayland::shm::{ShmHandler, ShmState};

use crate::host::SurfaceHost;
use crate::input::{HostKey, InputEvent, KeyModifiers};
use crate::keys::{HostKeys, KeyClaim};
use crate::policy::{WindowInfo, WindowPolicy};

/// Per-client data every Wayland client carries.
#[derive(Default)]
pub struct ClientState {
    compositor: CompositorClientState,
    /// The session started this client and trusts it with privileged
    /// protocols (locking the session).
    trusted: bool,
}

impl ClientState {
    /// Whether the client may use privileged protocols.
    pub(crate) const fn trusted(&self) -> bool {
        self.trusted
    }
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}
    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {}
}

/// Where the pointer's events go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PointerTarget {
    /// A client surface (or nothing): the seat's own focus tracks which.
    Clients,
    /// The hosted surface at this index of [`State::hosts`].
    Host(usize),
}

/// The compositor's state: protocol globals, windows, seat, policy and hosts.
#[allow(
    clippy::struct_field_names,
    reason = "smithay's handler traits name each protocol's state `*_state`"
)]
pub struct State {
    pub(crate) display: DisplayHandle,
    started: Instant,
    compositor_state: CompositorState,
    xdg_shell_state: XdgShellState,
    shm_state: ShmState,
    seat_state: SeatState<Self>,
    data_device_state: DataDeviceState,
    _output_manager_state: OutputManagerState,
    pub(crate) seat: Seat<Self>,
    /// The mapped windows, bottom first, in the logical layout.
    pub(crate) space: Space<Window>,
    /// Toplevels a client created that have not shown a buffer yet.
    unmapped: Vec<Window>,
    pub(crate) popups: PopupManager,
    pub(crate) policy: Box<dyn WindowPolicy>,
    pub(crate) hosts: Vec<Box<dyn SurfaceHost>>,
    /// The outputs the hosted surfaces were last told of.
    told_outputs: Vec<(i32, i32, i32, i32)>,
    pub(crate) pointer: Point<f64, Logical>,
    pointer_target: PointerTarget,
    pub(crate) protocols: crate::protocols::Protocols,
    pub(crate) lock: crate::lock::Lock,
    pub(crate) text_input: crate::text_input::TextInput,
    pub(crate) cursor: crate::cursor::Cursor,
    pub(crate) output_management: crate::output_management::OutputManagement,
    pub(crate) xwayland_shell: smithay::wayland::xwayland_shell::XWaylandShellState,
    /// The X11 window manager, once Xwayland is ready.
    pub(crate) xwm: Option<smithay::xwayland::X11Wm>,
    pub(crate) touch: crate::touch::Touches,
    pub(crate) tablets: crate::tablet::Tablets,
    pub(crate) output_power: crate::output_power::OutputPower,
    /// Outputs turned off by a display configuration: known, not shown.
    pub(crate) disabled_outputs: Vec<Output>,
    pub(crate) idle: crate::power::Idle,
    /// The keys hosted surfaces hold, and their repeat.
    pub(crate) host_keys: HostKeys,
    /// The magnifier's zoom about the pointer; 1 is off.
    pub(crate) zoom: f64,
}

impl State {
    pub(crate) fn new(display: &DisplayHandle, policy: Box<dyn WindowPolicy>) -> Self {
        let compositor_state = CompositorState::new::<Self>(display);
        let xdg_shell_state = XdgShellState::new::<Self>(display);
        let shm_state = ShmState::new::<Self>(display, Vec::new());
        let output_manager_state = OutputManagerState::new_with_xdg_output::<Self>(display);
        let data_device_state = DataDeviceState::new::<Self>(display);
        let mut seat_state = SeatState::new();
        let mut seat = seat_state.new_wl_seat(display, "seat0");
        // The US layout always compiles; a person's layout replaces it once
        // settings reach the compositor.
        if let Err(error) = seat.add_keyboard(XkbConfig::default(), 400, 30) {
            tracing::warn!(?error, "keyboard: default keymap did not compile");
        }
        seat.add_pointer();
        seat.add_touch();
        Self {
            display: display.clone(),
            started: Instant::now(),
            compositor_state,
            xdg_shell_state,
            shm_state,
            seat_state,
            data_device_state,
            _output_manager_state: output_manager_state,
            seat,
            space: Space::default(),
            unmapped: Vec::new(),
            popups: PopupManager::default(),
            policy,
            hosts: Vec::new(),
            told_outputs: Vec::new(),
            pointer: Point::from((0.0, 0.0)),
            pointer_target: PointerTarget::Clients,
            protocols: crate::protocols::Protocols::new(display),
            lock: crate::lock::Lock::new(display),
            text_input: crate::text_input::TextInput::new(display),
            cursor: crate::cursor::Cursor::new(),
            output_management: crate::output_management::OutputManagement::new(display),
            xwayland_shell: smithay::wayland::xwayland_shell::XWaylandShellState::new::<Self>(
                display,
            ),
            xwm: None,
            touch: crate::touch::Touches::default(),
            tablets: crate::tablet::Tablets::default(),
            output_power: crate::output_power::OutputPower::new(display),
            disabled_outputs: Vec::new(),
            idle: crate::power::Idle::new(),
            host_keys: HostKeys::default(),
            zoom: 1.0,
        }
    }

    /// Time since the compositor started, the clock of input events and
    /// frame callbacks.
    pub(crate) fn now(&self) -> Duration {
        self.started.elapsed()
    }

    pub(crate) fn now_ms(&self) -> u32 {
        // Wayland timestamps are milliseconds that wrap.
        u32::try_from(self.now().as_millis() & u128::from(u32::MAX)).unwrap_or_default()
    }

    pub(crate) fn add_output(&mut self, output: &Output, location: Point<i32, Logical>) {
        self.space.map_output(output, location);
        self.rearrange();
    }

    /// Stop showing anything on `output` (it was disconnected) and
    /// withdraw its global.
    pub(crate) fn remove_output(&mut self, output: &Output) {
        self.space.unmap_output(output);
        self.space.refresh();
        self.disabled_outputs.retain(|disabled| disabled != output);
        crate::output::withdraw(output, &self.display);
        self.rearrange();
    }

    /// Whether a hosted surface has the pointer (not a client).
    pub(crate) fn pointer_over_host(&self) -> bool {
        matches!(self.pointer_target, PointerTarget::Host(_))
    }

    /// The outputs' rectangles `(x, y, w, h)`, as hosted surfaces are
    /// told them.
    pub(crate) fn hosted_outputs(&self) -> Vec<(i32, i32, i32, i32)> {
        self.output_geometries()
            .iter()
            .map(|rect| (rect.loc.x, rect.loc.y, rect.size.w, rect.size.h))
            .collect()
    }

    /// Tell every hosted surface where the outputs are, when that changed
    /// since they were last told (an output came or went, changed mode,
    /// scale or place).
    fn outputs_changed(&mut self) {
        let outputs = self.hosted_outputs();
        if outputs == self.told_outputs {
            return;
        }
        for host in &mut self.hosts {
            host.outputs_changed(&outputs);
        }
        self.told_outputs = outputs;
    }

    /// The logical rectangles of every output.
    pub(crate) fn output_geometries(&self) -> Vec<Rectangle<i32, Logical>> {
        self.space
            .outputs()
            .filter_map(|output| self.space.output_geometry(output))
            .collect()
    }

    /// The mapped windows' descriptions, bottom first.
    pub(crate) fn windows(&self) -> Vec<(WindowInfo, Rectangle<i32, Logical>)> {
        self.space
            .elements()
            .filter_map(|window| {
                let info = describe(window)?;
                let location = self.space.element_location(window)?;
                let size = window.geometry().size;
                Some((info, Rectangle::new(location, size)))
            })
            .collect()
    }

    /// The window under a logical point, with its surface and that surface's
    /// origin in the logical layout.
    pub(crate) fn surface_under(
        &self,
        point: Point<f64, Logical>,
    ) -> Option<(Window, WlSurface, Point<f64, Logical>)> {
        let (window, location) = self.space.element_under(point)?;
        let origin = location - window.geometry().loc;
        let (surface, offset) =
            window.surface_under(point - origin.to_f64(), WindowSurfaceType::ALL)?;
        Some((window.clone(), surface, (origin + offset).to_f64()))
    }

    /// The hosted surface the pointer at `point` belongs to: one above the
    /// windows where it drew, else, where no window is, one beneath them.
    pub(crate) fn host_under(&self, point: Point<f64, Logical>) -> Option<usize> {
        // The last host draws on top, so it is asked first.
        let under = |below: bool| {
            self.hosts.iter().rposition(|host| {
                host.below_windows() == below && host.accepts_pointer(point.x, point.y)
            })
        };
        under(false).or_else(|| {
            self.space
                .element_under(point)
                .is_none()
                .then(|| under(true))
                .flatten()
        })
    }

    /// Route one input event to the hosted surface or client it belongs to.
    pub(crate) fn handle_input(&mut self, event: InputEvent) {
        self.note_input();
        let time = self.now_ms();
        let serial = SERIAL_COUNTER.next_serial();
        match event {
            InputEvent::PointerMotion { x, y } => {
                self.pointer_motion(Point::from((x, y)), serial, time);
            }
            InputEvent::PointerMotionRelative { dx, dy } => {
                let to = self.pointer + Point::from((dx, dy));
                self.pointer_motion(to, serial, time);
            }
            InputEvent::PointerButton { button, pressed } => {
                self.pointer_button(button, pressed, serial, time);
            }
            InputEvent::PointerAxis {
                horizontal,
                vertical,
            } => self.pointer_axis(horizontal, vertical, time),
            InputEvent::Key { keycode, pressed } => self.key(keycode, pressed, serial, time),
            InputEvent::TouchDown { slot, x, y } => {
                self.touch_down(slot, Point::from((x, y)), serial, time);
            }
            InputEvent::TouchMotion { slot, x, y } => {
                self.touch_motion(slot, Point::from((x, y)), time);
            }
            InputEvent::TouchUp { slot } => self.touch_up(slot, serial, time),
            InputEvent::TouchFrame => self.touch_frame(),
            InputEvent::TouchCancel => self.touch_cancel(),
            InputEvent::Tablet(event) => self.tablet_input(&event, serial, time),
            InputEvent::Gesture(gesture) => self.gesture(gesture, serial, time),
            InputEvent::Switch { switch, on } => self.policy.switch(switch, on),
        }
    }

    /// Route a key: a press to the hosted surface that claims it, else to
    /// the topmost one that wants the keyboard, else to the focused client;
    /// a release to where its press went ([`crate::keys`]).
    fn key(&mut self, keycode: u32, pressed: bool, serial: Serial, time: u32) {
        let Some(keyboard) = self.seat.get_keyboard() else {
            return;
        };
        let key_state = if pressed {
            KeyState::Pressed
        } else {
            KeyState::Released
        };
        // evdev codes sit 8 below their XKB keycodes.
        let code = keycode.saturating_add(8);
        let routed = keyboard.input(
            self,
            code.into(),
            key_state,
            serial,
            time,
            |state, modifiers, handle| {
                let unmodified = handle
                    .raw_syms()
                    .first()
                    .copied()
                    .unwrap_or_else(|| handle.modified_sym());
                let key = HostKey {
                    keycode,
                    pressed,
                    repeat: false,
                    keysym: handle.modified_sym().raw(),
                    unmodified_keysym: unmodified.raw(),
                    text: handle.modified_sym().key_char().filter(|c| !c.is_control()),
                    modifiers: KeyModifiers {
                        ctrl: modifiers.ctrl,
                        alt: modifiers.alt,
                        shift: modifiers.shift,
                        logo: modifiers.logo,
                        caps_lock: modifiers.caps_lock,
                    },
                };
                if !pressed {
                    return state
                        .host_keys
                        .release(keycode)
                        .map_or(FilterResult::Forward, |host| {
                            FilterResult::Intercept((host, key))
                        });
                }
                let locked = state.locked();
                let now = Instant::now();
                for index in (0..state.hosts.len()).rev() {
                    let claim = state
                        .hosts
                        .get_mut(index)
                        .map_or(KeyClaim::Pass, |host| host.claim_key(&key, locked));
                    if claim != KeyClaim::Pass {
                        let repeats = claim == KeyClaim::TakeRepeating;
                        state.host_keys.press(index, &key, repeats, now);
                        return FilterResult::Intercept((index, key));
                    }
                }
                // The topmost hosted surface that wants the keyboard takes
                // keys from every client.
                let wanting = (!locked)
                    .then(|| state.hosts.iter().rposition(|host| host.wants_keyboard()))
                    .flatten();
                let Some(index) = wanting else {
                    return FilterResult::Forward;
                };
                let repeats = !handle.modified_sym().is_modifier_key();
                state.host_keys.press(index, &key, repeats, now);
                FilterResult::Intercept((index, key))
            },
        );
        if let Some((index, key)) = routed
            && let Some(host) = self.hosts.get_mut(index)
        {
            host.key(&key);
        }
    }

    /// Hand hosted surfaces the repeats of the keys they hold that are due.
    pub(crate) fn repeat_keys(&mut self) {
        for (index, key) in self.host_keys.due(Instant::now()) {
            if let Some(host) = self.hosts.get_mut(index) {
                host.key(&key);
            }
        }
    }

    fn pointer_axis(&mut self, horizontal: f64, vertical: f64, time: u32) {
        if let PointerTarget::Host(index) = self.pointer_target {
            if let Some(host) = self.hosts.get_mut(index) {
                host.pointer_axis(horizontal, vertical);
            }
            return;
        }
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let mut frame = AxisFrame::new(time);
        if horizontal != 0.0 {
            frame = frame.value(Axis::Horizontal, horizontal);
        }
        if vertical != 0.0 {
            frame = frame.value(Axis::Vertical, vertical);
        }
        pointer.axis(self, frame);
        pointer.frame(self);
    }

    pub(crate) fn pointer_motion(&mut self, to: Point<f64, Logical>, serial: Serial, time: u32) {
        self.pointer = self.clamp_to_outputs(to);
        let location = self.pointer;
        let target = if self.locked() {
            PointerTarget::Clients
        } else {
            self.host_under(location)
                .map_or(PointerTarget::Clients, PointerTarget::Host)
        };
        if let PointerTarget::Host(old) = self.pointer_target
            && target != self.pointer_target
            && let Some(host) = self.hosts.get_mut(old)
        {
            host.pointer_leave();
        }
        self.pointer_target = target;
        let focus = match target {
            PointerTarget::Host(index) => {
                if let Some(host) = self.hosts.get_mut(index) {
                    host.pointer_motion(location.x, location.y);
                }
                // Clients lose the pointer while a hosted surface has it.
                None
            }
            PointerTarget::Clients if self.locked() => self.lock_surface_under(location),
            PointerTarget::Clients => self
                .surface_under(location)
                .map(|(_, surface, origin)| (surface, origin)),
        };
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        pointer.motion(
            self,
            focus,
            &MotionEvent {
                location,
                serial,
                time,
            },
        );
        pointer.frame(self);
    }

    pub(crate) fn pointer_button(&mut self, button: u32, pressed: bool, serial: Serial, time: u32) {
        if pressed {
            let target = self.pointer_target;
            for (index, host) in self.hosts.iter_mut().enumerate() {
                if target != PointerTarget::Host(index) {
                    host.pressed_elsewhere();
                }
            }
        }
        if let PointerTarget::Host(index) = self.pointer_target {
            if let Some(host) = self.hosts.get_mut(index) {
                host.pointer_button(button, pressed);
            }
            return;
        }
        if pressed {
            self.focus_on_press(self.pointer, serial);
        }
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let state = if pressed {
            ButtonState::Pressed
        } else {
            ButtonState::Released
        };
        pointer.button(
            self,
            &ButtonEvent {
                button,
                state,
                serial,
                time,
            },
        );
        pointer.frame(self);
    }

    pub(crate) fn clamp_to_outputs(&self, point: Point<f64, Logical>) -> Point<f64, Logical> {
        let geometries = self.output_geometries();
        if geometries
            .iter()
            .any(|geometry| geometry.to_f64().contains(point))
            || geometries.is_empty()
        {
            return point;
        }
        // Outside every output: the nearest point of the nearest output.
        geometries
            .iter()
            .map(|geometry| {
                let geometry = geometry.to_f64();
                let x = point
                    .x
                    .clamp(geometry.loc.x, geometry.loc.x + geometry.size.w - 1.0);
                let y = point
                    .y
                    .clamp(geometry.loc.y, geometry.loc.y + geometry.size.h - 1.0);
                Point::from((x, y))
            })
            .min_by(|a: &Point<f64, Logical>, b: &Point<f64, Logical>| {
                let da = (a.x - point.x).hypot(a.y - point.y);
                let db = (b.x - point.x).hypot(b.y - point.y);
                da.total_cmp(&db)
            })
            .unwrap_or(point)
    }

    pub(crate) fn focus_window(&mut self, window: &Window, serial: Serial) {
        self.space.raise_element(window, true);
        for other in self.space.elements() {
            if other != window {
                other.set_activated(false);
                if let Some(toplevel) = other.toplevel() {
                    toplevel.send_pending_configure();
                }
            }
        }
        if let Some(toplevel) = window.toplevel() {
            toplevel.send_pending_configure();
        }
        let surface = window
            .toplevel()
            .map(|toplevel| toplevel.wl_surface().clone())
            .or_else(|| crate::xwayland::x11_wl_surface(window));
        if let Some(keyboard) = self.seat.get_keyboard() {
            keyboard.set_focus(self, surface, serial);
        }
    }

    /// Map a toplevel whose first buffer arrived (or an X11 window that
    /// asked to be mapped), where the policy places it.
    pub(crate) fn map_window(&mut self, window: &Window) {
        let Some(info) = describe(window) else {
            return;
        };
        let outputs = self.output_geometries();
        let size = window.geometry().size;
        let location = self.policy.place(&info, (size.w, size.h), &outputs);
        self.space.map_element(window.clone(), location, false);
        if let Some(x11) = window.x11_surface() {
            // X11 windows know their place in the root window.
            let _ = x11.configure(Rectangle::new(location, size));
        }
        if self.policy.focus_on_map(&info) {
            self.focus_window(window, SERIAL_COUNTER.next_serial());
        }
        self.rearrange();
    }

    /// Ask the policy to lay the windows out again and apply its answer,
    /// keeping the stacking order.
    pub(crate) fn rearrange(&mut self) {
        self.outputs_changed();
        let windows = self.windows();
        let outputs = self.output_geometries();
        let Some(arrangements) = self.policy.arrange(&windows, &outputs) else {
            // Focus and closes the policy asked for apply all the same.
            self.apply_policy_focus_and_closes();
            return;
        };
        let stack: Vec<Window> = self.space.elements().cloned().collect();
        for window in stack {
            let Some(id) = describe(&window).map(|info| info.id) else {
                continue;
            };
            let Some(arrangement) = arrangements.iter().find(|arrangement| arrangement.id == id)
            else {
                continue;
            };
            if let Some(x11) = window.x11_surface() {
                let size = arrangement
                    .size
                    .map_or_else(|| x11.geometry().size, Into::into);
                let _ = x11.configure(Rectangle::new(arrangement.location, size));
                self.space
                    .map_element(window.clone(), arrangement.location, false);
                continue;
            }
            let Some(toplevel) = window.toplevel() else {
                continue;
            };
            if let Some((w, h)) = arrangement.size {
                toplevel.with_pending_state(|state| state.size = Some((w, h).into()));
                toplevel.send_pending_configure();
            }
            let activated = window.toplevel().is_some_and(|toplevel| {
                toplevel.with_pending_state(|state| {
                    state.states.contains(smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State::Activated)
                })
            });
            // Re-mapping in stacking order keeps the order.
            self.space
                .map_element(window.clone(), arrangement.location, activated);
        }
        self.apply_policy_focus_and_closes();
    }

    /// Give the keyboard to the window the policy brought forward, and ask
    /// the clients of the windows it closed to close.
    fn apply_policy_focus_and_closes(&mut self) {
        let window_of = |state: &Self, id: u64| {
            state
                .space
                .elements()
                .find(|window| describe(window).is_some_and(|info| info.id == id))
                .cloned()
        };
        for id in self.policy.take_close_requests() {
            let Some(window) = window_of(self, id) else {
                continue;
            };
            if let Some(toplevel) = window.toplevel() {
                toplevel.send_close();
            } else if let Some(x11) = window.x11_surface()
                && let Err(error) = x11.close()
            {
                tracing::warn!(?error, "X11 window: close not sent");
            }
        }
        let Some(id) = self.policy.focused() else {
            return;
        };
        if self.locked() || self.focused_window().is_some_and(|info| info.id == id) {
            return;
        }
        if let Some(window) = window_of(self, id) {
            self.focus_window(&window, SERIAL_COUNTER.next_serial());
        }
    }

    /// Drop windows whose clients went away and refresh output membership.
    pub(crate) fn refresh(&mut self) {
        self.space.refresh();
        self.popups.cleanup();
        self.unmapped.retain(IsAlive::alive);
    }
}

/// A toplevel's identity, unique for the compositor's life.
struct WindowId(u64);

impl State {
    /// `window` asked for the keyboard: it gets it when the policy says so
    /// ([`WindowPolicy::activate`]).
    pub(crate) fn activation_requested(&mut self, window: &Window, fresh: bool) {
        let focus = describe(window).map_or(fresh, |info| self.policy.activate(&info, fresh));
        if focus {
            self.focus_window(window, SERIAL_COUNTER.next_serial());
        }
    }

    /// Focus the window under `point` if the policy focuses on click, as a
    /// pointer press or a touch there does.
    pub(crate) fn focus_on_press(&mut self, point: Point<f64, Logical>, serial: Serial) {
        if self.locked() {
            return;
        }
        if let Some((window, _, _)) = self.surface_under(point) {
            let info = describe(&window);
            if info.is_some_and(|info| self.policy.focus_on_click(&info)) {
                self.focus_window(&window, serial);
            }
        }
    }
}

static NEXT_WINDOW: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Where X11 window ids start in the [`WindowInfo`] id space, apart from
/// Wayland toplevels' counter.
const X11_IDS: u64 = 1 << 40;

/// What a policy learns about a window, Wayland or X11; `None` for
/// override-redirect X11 windows, which place themselves.
fn describe(window: &Window) -> Option<WindowInfo> {
    if let Some(toplevel) = window.toplevel() {
        return Some(window_info(toplevel));
    }
    let x11 = window.x11_surface()?;
    (!x11.is_override_redirect()).then(|| WindowInfo {
        id: X11_IDS | u64::from(x11.window_id()),
        app_id: Some(x11.class()),
        title: Some(x11.title()),
    })
}

/// What a policy learns about a toplevel.
fn window_info(toplevel: &ToplevelSurface) -> WindowInfo {
    smithay::wayland::compositor::with_states(toplevel.wl_surface(), |states| {
        let id = states.data_map.get::<WindowId>().map_or(0, |id| id.0);
        let attributes = states
            .data_map
            .get::<smithay::wayland::shell::xdg::XdgToplevelSurfaceData>()
            .and_then(|data| {
                data.lock()
                    .ok()
                    .map(|data| (data.app_id.clone(), data.title.clone()))
            });
        let (app_id, title) = attributes.unwrap_or_default();
        WindowInfo { id, app_id, title }
    })
}

impl CompositorHandler for State {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        static FALLBACK: std::sync::OnceLock<CompositorClientState> = std::sync::OnceLock::new();
        client.get_data::<ClientState>().map_or_else(
            || FALLBACK.get_or_init(CompositorClientState::default),
            |data| &data.compositor,
        )
    }

    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);
        if !is_sync_subsurface(surface) {
            let mut root = surface.clone();
            while let Some(parent) = get_parent(&root) {
                root = parent;
            }
            let is_root = |window: &&Window| {
                window
                    .toplevel()
                    .is_some_and(|toplevel| toplevel.wl_surface() == &root)
            };
            if let Some(window) = self.space.elements().find(is_root) {
                window.on_commit();
            }
            if let Some(index) = self.unmapped.iter().position(|window| is_root(&window)) {
                let window = self.unmapped.get(index).cloned();
                if let Some(window) = window
                    && let Some(toplevel) = window.toplevel()
                {
                    window.on_commit();
                    if !toplevel.is_initial_configure_sent() {
                        let info = window_info(toplevel);
                        let outputs = self.output_geometries();
                        if let Some((w, h)) = self.policy.initial_size(&info, &outputs) {
                            toplevel.with_pending_state(|state| state.size = Some((w, h).into()));
                        }
                        toplevel.send_configure();
                    } else if with_renderer_surface_state(&root, |state| state.buffer().is_some())
                        .unwrap_or(false)
                    {
                        self.unmapped.remove(index);
                        self.map_window(&window);
                    }
                }
            }
        }
        self.popups.commit(surface);
        if let Some(PopupKind::Xdg(popup)) = self.popups.find_popup(surface)
            && !popup.is_initial_configure_sent()
            && let Err(error) = popup.send_configure()
        {
            tracing::debug!(?error, "popup: initial configure refused");
        }
    }
}

impl BufferHandler for State {
    fn buffer_destroyed(&mut self, _buffer: &WlBuffer) {}
}

impl ShmHandler for State {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

impl XdgShellHandler for State {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        let id = NEXT_WINDOW.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        smithay::wayland::compositor::with_states(surface.wl_surface(), |states| {
            states.data_map.insert_if_missing(|| WindowId(id));
        });
        self.unmapped.push(Window::new_wayland_window(surface));
    }

    fn new_popup(&mut self, surface: PopupSurface, positioner: PositionerState) {
        surface.with_pending_state(|state| state.geometry = positioner.get_geometry());
        if let Err(error) = self.popups.track_popup(PopupKind::Xdg(surface)) {
            tracing::debug!(?error, "popup: not tracked");
        }
    }

    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        surface.with_pending_state(|state| {
            state.geometry = positioner.get_geometry();
            state.positioner = positioner;
        });
        surface.send_repositioned(token);
    }

    fn grab(&mut self, _surface: PopupSurface, _seat: WlSeat, _serial: Serial) {}

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        let pending = self
            .unmapped
            .iter()
            .any(|window| window.toplevel() == Some(&surface));
        self.unmapped
            .retain(|window| window.toplevel() != Some(&surface));
        if pending {
            self.policy.withdrawn(&window_info(&surface));
        }
        let gone = self
            .space
            .elements()
            .find(|window| window.toplevel() == Some(&surface))
            .cloned();
        if let Some(window) = gone {
            self.space.unmap_elem(&window);
            self.rearrange();
        }
    }
}

impl SeatHandler for State {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }

    fn cursor_image(&mut self, _seat: &Seat<Self>, image: CursorImageStatus) {
        self.cursor.status = image;
    }

    fn focus_changed(&mut self, seat: &Seat<Self>, focused: Option<&WlSurface>) {
        let client = focused.and_then(|surface| self.display.get_client(surface.id()).ok());
        smithay::wayland::selection::data_device::set_data_device_focus(
            &self.display,
            seat,
            client,
        );
    }
}

impl SelectionHandler for State {
    type SelectionUserData = ();
}

impl DataDeviceHandler for State {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device_state
    }
}

impl ClientDndGrabHandler for State {}
impl ServerDndGrabHandler for State {}

impl OutputHandler for State {}

smithay::delegate_compositor!(State);
smithay::delegate_shm!(State);
smithay::delegate_xdg_shell!(State);
smithay::delegate_seat!(State);
smithay::delegate_data_device!(State);
smithay::delegate_output!(State);

/// A fresh client's data, for [`DisplayHandle::insert_client`].
pub(crate) fn client_data(trusted: bool) -> Arc<ClientState> {
    Arc::new(ClientState {
        trusted,
        ..ClientState::default()
    })
}

impl State {
    /// The window whose surface has keyboard focus.
    pub(crate) fn focused_window(&self) -> Option<WindowInfo> {
        let focus = self.seat.get_keyboard()?.current_focus()?;
        self.space
            .elements()
            .filter_map(Window::toplevel)
            .find(|toplevel| toplevel.wl_surface() == &focus)
            .map(window_info)
    }
}
