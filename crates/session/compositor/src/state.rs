//! The Wayland side of the compositor: protocol globals, their handlers, the mapped windows and the
//! seat.
//! The state answers clients and keeps the window stack; it decides nothing about where windows go
//! or who gets focus: that is the [`WindowPolicy`]'s.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "smithay geometry: logical and physical coordinates of outputs and windows, far from i32 limits"
)]

use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use smithay::backend::input::KeyState;
use smithay::backend::input::{Axis, ButtonState};
use smithay::backend::renderer::utils::{on_commit_buffer_handler, with_renderer_surface_state};
use smithay::desktop::{PopupKind, PopupManager, Space, Window, WindowSurfaceType};
use smithay::input::keyboard::{FilterResult, Keysym, XkbConfig};
use smithay::input::pointer::{AxisFrame, ButtonEvent, CursorImageStatus, MotionEvent};
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::output::Output;
use smithay::reexports::wayland_server::backend::protocol::ProtocolError;
use smithay::reexports::wayland_server::backend::{ClientData, ClientId, DisconnectReason};
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_seat::WlSeat;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, Resource as _,
};
use smithay::utils::{IsAlive, Logical, Point, Rectangle, SERIAL_COUNTER, Serial};
use smithay::wayland::buffer::BufferHandler;
use smithay::wayland::compositor::{
    CompositorClientState, CompositorHandler, CompositorState, get_parent, is_sync_subsurface,
};
use smithay::wayland::output::{OutputHandler, OutputManagerState};
use smithay::wayland::seat::SeatUserData;
use smithay::wayland::selection::data_device::{DataDeviceHandler, DataDeviceState};
use smithay::wayland::shell::xdg::{
    PopupSurface, PositionerState, ShellClient, ToplevelSurface, XdgShellHandler, XdgShellState,
};
use smithay::wayland::shm::{ShmHandler, ShmState};
use smithay::wayland::xwayland_shell::XWaylandShellState;

use crate::app_liveness;
use crate::input::InputEvent;
use crate::keys::HostKeys;
use crate::policy::{WindowInfo, WindowPolicy};
use tilo_session_core::surface::{HostKey, HostedOutput, KeyClaim, KeyModifiers, SurfaceHost};

/// Per-client data every Wayland client carries.
#[derive(Default)]
pub struct ClientState {
    compositor: CompositorClientState,
    /// The session started this client and trusts it with privileged protocols (locking the
    /// session).
    trusted: bool,
    /// The client's process, once the socket said; 0 while unknown.
    pid: AtomicU32,
    /// The processes whose clients disconnected, for the session to hear.
    exited: Arc<std::sync::Mutex<Vec<u32>>>,
    /// The output the session connected the client for: its layer surfaces stand there, whichever
    /// output they name themselves. The on-screen keyboard belongs on the display the person types
    /// on.
    output: Option<String>,
    /// The client bound `wl_touch` : one that did not is typed on by the pointer where a finger
    /// goes down, so a tap still works.
    touch_bound: AtomicBool,
}

impl ClientState {
    /// Whether the client may use privileged protocols.
    pub(crate) const fn trusted(&self) -> bool {
        self.trusted
    }

    /// The output the session connected the client for, when it named one.
    pub(crate) fn output(&self) -> Option<&str> {
        self.output.as_deref()
    }

    /// Whether the client bound `wl_touch` : without it, a finger reaches it as a pointer press.
    pub(crate) fn binds_touch(&self) -> bool {
        self.touch_bound.load(Ordering::Relaxed)
    }
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {}

    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {
        let pid = self.pid.load(Ordering::Relaxed);
        if pid != 0
            && let Ok(mut exited) = self.exited.lock()
        {
            exited.push(pid);
        }
    }
}

/// How many windows that closed keep their last frame: a desktop animates a handful of closes at
/// once, and older ones go.
const MOST_CLOSING: usize = 16;

/// How long after a keystroke the person still counts as typing: the pause after which a next key
/// starts a new word, and long enough that moving something between two keystrokes never takes the
/// keys from them.
const TYPING_PAUSE: Duration = Duration::from_millis(1000);

/// A window that closed, drawn from its last buffer where it stood so the desktop can animate it
/// away ([`crate::Compositor::closing_windows`]).
pub(crate) struct Closing {
    /// What the desktop knew it as.
    pub(crate) info: WindowInfo,
    /// The last frame it drew, kept before its surface went; what is drawn in its place until the
    /// desktop lets it go.
    pub(crate) snapshot: Option<crate::snapshot::Snapshot>,
    /// Where it stood, logical.
    pub(crate) rect: Rectangle<i32, Logical>,
    /// Its place in the stack, bottom first.
    pub(crate) index: usize,
    /// How it was framed.
    pub(crate) frame: crate::window_frame::WindowFrame,
}

/// Where the pointer's events go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PointerTarget {
    /// A client surface (or nothing): the seat's own focus tracks which.
    Clients,
    /// The hosted surface at this index of [`State::hosts`].
    Host(usize),
}

/// A running Xwayland server: the window manager that makes its windows toplevels the desktop
/// places, and the session's own connection to it.
pub(crate) struct X11Server {
    /// The display's window manager. Dropping it destroys its window and closes the server's window
    /// manager connection.
    pub(crate) wm: smithay::xwayland::X11Wm,
    /// What the session reads on the display: window properties, the X input focus, the display's
    /// density and pointer. `None` when the display could not be connected to.
    pub(crate) windows: Option<crate::x11_windows::X11Windows>,
    /// The display number this server serves (`DISPLAY=:N`).
    pub(crate) display: u32,
}

impl X11Server {
    /// The id of the server's window manager.
    pub(crate) fn id(&self) -> smithay::xwayland::xwm::XwmId {
        self.wm.id()
    }
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
    /// The layout the seat types in, as the session set it: X11 clients get it too, including a
    /// server started after the person chose it. `None` until the session says, when the server's
    /// own default stands.
    pub(crate) keyboard: Option<crate::config::KeyboardLayout>,
    /// The mapped windows, bottom first, in the logical layout.
    pub(crate) space: Space<Window>,
    /// Toplevels a client created that have not shown a buffer yet.
    unmapped: Vec<Window>,
    pub(crate) popups: PopupManager,
    /// The surfaces that place themselves on a layer of an output (`zwlr_layer_shell_v1`).
    pub(crate) layers: crate::layer::Layers,
    pub(crate) policy: Box<dyn WindowPolicy>,
    pub(crate) hosts: Vec<Box<dyn SurfaceHost>>,
    /// The buffers the hosted surfaces' frames are drawn from, by the frame they hold.
    pub(crate) uploads: crate::uploads::Uploads,
    /// The outputs the hosted surfaces were last told of.
    told_outputs: Vec<HostedOutput>,
    /// The connector the person made primary, as the configuration names it, whether or not it is
    /// lit right now.
    primary_output: Option<String>,
    /// The monitors the policy was last told of.
    told_monitors: Vec<(Rectangle<i32, Logical>, String)>,
    /// The primary monitor the policy was last told of.
    told_primary: Option<String>,
    pub(crate) pointer: Point<f64, Logical>,
    pub(crate) pointer_target: PointerTarget,
    /// The seats beside the person's: an agent's, a viewer's (§ seats).
    pub(crate) seats: crate::seats::Seats,
    /// A seat's keys are the app-facing seat's for this delivery: no hosted surface takes them, and
    /// they reach the client the seat aims at.
    pub(crate) seat_keys_only: bool,
    pub(crate) protocols: crate::protocols::Protocols,
    /// The GPU clients import their dma-bufs on, as `linux-dmabuf` told them
    /// ([`crate::State::enable_dmabuf`]); a render service allocates there too, so the compositor
    /// imports its frames like any client's.
    pub(crate) render_node: Option<smithay::backend::drm::DrmNode>,
    /// The icon each window shows, by window id.
    pub(crate) icons: crate::toplevel_icon::Icons,
    pub(crate) lock: crate::lock::Lock,
    pub(crate) text_input: crate::text_input::TextInput,
    pub(crate) cursor: crate::cursor::Cursor,
    pub(crate) output_management: crate::output_management::OutputManagement,
    pub(crate) xwayland_shell: smithay::wayland::xwayland_shell::XWaylandShellState,
    /// The X11 servers running now, one per workspace display with an app on it.
    pub(crate) xwms: Vec<X11Server>,
    /// The session's X11 displays, one per workspace that launched an X11 app
    /// ([`crate::X11Displays`]).
    pub(crate) x11: Option<crate::X11Displays>,
    pub(crate) touch: crate::touch::Touches,
    pub(crate) tablets: crate::tablet::Tablets,
    pub(crate) output_power: crate::output_power::OutputPower,
    pub(crate) gamma: crate::gamma::Gamma,
    pub(crate) backdrops: crate::backdrop::Backdrops,
    /// How windows are drawn this frame instead of as they stand, by id.
    pub(crate) effects: std::collections::HashMap<u64, crate::effects::WindowEffect>,
    /// How every window is framed, unless a window says otherwise.
    pub(crate) frame: crate::window_frame::WindowFrame,
    /// The frames of the windows that are framed differently, by id.
    pub(crate) window_frames: std::collections::HashMap<u64, crate::window_frame::WindowFrame>,
    /// The cards the desk draws in front of the windows, by the window each one is the card of:
    /// the desk draws a floating window's bar and its lift around it, and the window beneath the
    /// card is drawn around it, as a window under a floating one is.
    pub(crate) covers: std::collections::BTreeMap<u64, smithay::utils::Rectangle<i32, Logical>>,
    /// How many layers each hosted surface drew in the last frame, by its index in `hosts`: what a
    /// host drew and no longer draws is what the screen keeps until it is drawn again.
    pub(crate) layer_counts: Vec<(usize, usize)>,
    /// The shadows and edges drawn so far.
    pub(crate) frame_images: crate::window_frame::FrameImages,
    /// The windows that closed, kept so the desktop can animate them away.
    pub(crate) closing: Vec<Closing>,
    /// The frames the windows last drew, by id.
    pub(crate) snapshots: crate::snapshot::Snapshots,
    /// The HDR planes of the windows that carry one, by id.
    pub(crate) hdr: crate::hdr::HdrSurfaces,
    /// The streams sharing a window, by window id: each is told what the window's content is in, on
    /// the commit that changes it.
    pub(crate) share_streams: std::collections::HashMap<u64, Arc<dyn crate::ShareStream>>,
    pub(crate) capture: crate::capture::Capture,
    /// What input from another device crosses before it reaches the seat.
    pub(crate) remote_input: crate::remote_input::Firewall,
    /// Outputs turned off by a display configuration: known, not shown.
    pub(crate) disabled_outputs: Vec<Output>,
    pub(crate) idle: crate::power::Idle,
    /// The keys hosted surfaces hold, and their repeat.
    pub(crate) host_keys: HostKeys,
    /// What a dead key's or the Compose key's sequence types; `None` when the session's locale has
    /// no compose table.
    pub(crate) compose: Option<crate::compose::Compose>,
    /// The pointer buttons that are down: a drag, a resize or any press in flight is an ongoing
    /// gesture for whatever must not move under it.
    pub(crate) buttons_down: std::collections::BTreeSet<u32>,
    /// When the person last typed a key that types something. What must not move while they type
    /// reads it.
    typed_at: Option<Instant>,
    /// The pointer lock or confinement a client holds, and its region.
    pub(crate) pointer_lock: crate::pointer_lock::PointerLock,
    /// The client that holds the desktop's shortcuts, if any.
    pub(crate) shortcut_inhibit: crate::keys::ShortcutInhibit,
    /// The magnifier's zoom about the pointer; 1 is off.
    pub(crate) zoom: f64,
    pub(crate) selections: crate::selection::Selections,
    pub(crate) dnd: crate::dnd::Dnd,
    pub(crate) display_changes: crate::display_change::DisplayChanges,
    /// What is offered every input before the desktop sees it, and takes what it answers `true` to.
    pub(crate) input_observer:
        Option<Arc<dyn Fn(crate::input::ObservedInput) -> bool + Send + Sync>>,
    /// Pongs counted per `xdg_wm_base` , the only way a client answers a ping.
    pub(crate) shell_pongs: app_liveness::Pongs,
    /// The pings in flight, one per client that was given input.
    pub(crate) app_watches: app_liveness::Watches,
    /// The windows whose app was not responding at the last look, by id.
    not_responding: Vec<u64>,
    /// The processes whose clients disconnected since the session last looked: the apps that
    /// exited.
    pub(crate) exited: Arc<std::sync::Mutex<Vec<u32>>>,
    /// The outputs whose next frame must be drawn: something that shows changed on them since the
    /// last one. An idle session draws nothing.
    pub(crate) redraw: Vec<Output>,
    /// The compositor's event loop, for work the window manager schedules.
    pub(crate) loop_handle:
        Option<smithay::reexports::calloop::LoopHandle<'static, crate::LoopData>>,
    /// Each typed key's way from its client to the frame that shows it, and the `KEY-LATENCY` lines
    /// the session log keeps.
    pub(crate) key_latency: tilo_frame_metrics::key_latency::KeyLatency,
    /// The workspace switch in flight, and its `WORKSPACE-SWITCH` line.
    pub(crate) switch_latency: tilo_frame_metrics::switch_latency::SwitchLatency,
    /// Whether the switch in flight still owes frames: the person is watching the desk move. The
    /// session says, frame by frame.
    pub(crate) switch_holding: AtomicBool,
    /// The frames each window's client committed, by window id: a window still shows the same count,
    /// and one whose app is drawing moves. A tool reads it to prove a desk is quiet or to count an
    /// app's frames in a real session. The entry goes with the window.
    pub(crate) committed: std::collections::HashMap<u64, u64>,
    /// The serial of the next frame drawn. Frames are retired in order, so a presenter says how far
    /// the display got by its serial.
    frame_serial: std::sync::atomic::AtomicU64,
    /// The frames the displays presented since this session began.
    frames_presented: std::sync::atomic::AtomicU64,
    /// The last frame a display presented: its serial and when it was shown.
    retired: std::sync::Mutex<Option<(u64, Instant)>>,
}

/// Besides 8-bit RGB, the 10-bit and half-float buffers an HDR surface's signal is carried in.
fn shm_formats() -> Vec<smithay::reexports::wayland_server::protocol::wl_shm::Format> {
    use smithay::reexports::wayland_server::protocol::wl_shm::Format;
    vec![
        Format::Xrgb2101010,
        Format::Xbgr2101010,
        Format::Argb2101010,
        Format::Abgr2101010,
        Format::Xrgb16161616f,
        Format::Abgr16161616f,
    ]
}

/// The seat every client's input arrives on: a keyboard, a pointer and a touch screen. Its
/// keyboard starts on the US layout, which always compiles; a person's own layout replaces it once
/// settings reach the compositor. Stated outright, because an empty name would type the machine's
/// own layout (`XKB_DEFAULT_*`) until then — on a machine set up in German, the first keymap every
/// client gets would be German.
fn seat(display: &DisplayHandle) -> (SeatState<State>, Seat<State>) {
    let mut seat_state = SeatState::new();
    let mut seat = seat_state.new_wl_seat(display, "seat0");
    let us = XkbConfig {
        rules: "evdev",
        model: "pc105",
        layout: "us",
        variant: "",
        options: Some(String::new()),
    };
    if let Err(error) = seat.add_keyboard(us, 400, 30) {
        tracing::warn!(?error, "keyboard: default keymap did not compile");
    }
    seat.add_pointer();
    seat.add_touch();
    (seat_state, seat)
}

impl State {
    pub(crate) fn new(display: &DisplayHandle, policy: Box<dyn WindowPolicy>) -> Self {
        let compositor_state = CompositorState::new::<Self>(display);
        let xdg_shell_state = XdgShellState::new::<Self>(display);
        let shm_state = ShmState::new::<Self>(display, shm_formats());
        let output_manager_state = OutputManagerState::new_with_xdg_output::<Self>(display);
        let data_device_state = DataDeviceState::new::<Self>(display);
        let (seat_state, seat) = seat(display);
        let protocols = crate::protocols::Protocols::new(display);
        let selections = crate::selection::Selections::new(display, &protocols.primary_selection);
        crate::toplevel_icon::advertise(display);
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
            keyboard: None,
            space: Space::default(),
            unmapped: Vec::new(),
            popups: PopupManager::default(),
            layers: crate::layer::Layers::new(display),
            policy,
            hosts: Vec::new(),
            uploads: crate::uploads::Uploads::default(),
            told_outputs: Vec::new(),
            primary_output: None,
            told_monitors: Vec::new(),
            told_primary: None,
            pointer: Point::from((0.0, 0.0)),
            pointer_target: PointerTarget::Clients,
            seats: crate::seats::Seats::default(),
            seat_keys_only: false,
            protocols,
            render_node: None,
            icons: crate::toplevel_icon::Icons::default(),
            shell_pongs: app_liveness::Pongs::default(),
            app_watches: app_liveness::Watches::default(),
            not_responding: Vec::new(),
            exited: Arc::default(),
            lock: crate::lock::Lock::new(display),
            text_input: crate::text_input::TextInput::new(display),
            cursor: crate::cursor::Cursor::new(),
            output_management: crate::output_management::OutputManagement::new(display),
            xwayland_shell: XWaylandShellState::new::<Self>(display),
            xwms: Vec::new(),
            x11: None,
            touch: crate::touch::Touches::default(),
            tablets: crate::tablet::Tablets::default(),
            output_power: crate::output_power::OutputPower::new(display),
            gamma: crate::gamma::Gamma::new(display),
            backdrops: crate::backdrop::Backdrops::default(),
            effects: std::collections::HashMap::new(),
            frame: crate::window_frame::WindowFrame::default(),
            window_frames: std::collections::HashMap::new(),
            covers: std::collections::BTreeMap::new(),
            layer_counts: Vec::new(),
            frame_images: crate::window_frame::FrameImages::default(),
            closing: Vec::new(),
            snapshots: crate::snapshot::Snapshots::default(),
            hdr: crate::hdr::HdrSurfaces::default(),
            share_streams: std::collections::HashMap::new(),
            capture: crate::capture::Capture::new(display),
            remote_input: crate::remote_input::Firewall::default(),
            disabled_outputs: Vec::new(),
            idle: crate::power::Idle::new(),
            host_keys: HostKeys::default(),
            compose: crate::compose::Compose::for_session_locale(),
            buttons_down: std::collections::BTreeSet::new(),
            typed_at: None,
            pointer_lock: crate::pointer_lock::PointerLock::default(),
            shortcut_inhibit: crate::keys::ShortcutInhibit::default(),
            zoom: 1.0,
            selections,
            dnd: crate::dnd::Dnd::default(),
            display_changes: crate::display_change::DisplayChanges::default(),
            input_observer: None,
            redraw: Vec::new(),
            loop_handle: None,
            key_latency: tilo_frame_metrics::key_latency::KeyLatency::default(),
            switch_latency: tilo_frame_metrics::switch_latency::SwitchLatency::default(),
            switch_holding: AtomicBool::new(false),
            committed: std::collections::HashMap::new(),
            frame_serial: std::sync::atomic::AtomicU64::new(1),
            frames_presented: std::sync::atomic::AtomicU64::new(0),
            retired: std::sync::Mutex::new(None),
        }
    }

    /// Time since the compositor started, the clock of input events and frame callbacks.
    pub(crate) fn now(&self) -> Duration {
        self.started.elapsed()
    }

    pub(crate) fn now_ms(&self) -> u32 {
        // Wayland timestamps are milliseconds that wrap.
        u32::try_from(self.now().as_millis() & u128::from(u32::MAX)).unwrap_or_default()
    }

    /// `output` must be drawn again: something that shows on it changed since its last frame.
    pub(crate) fn mark_redraw(&mut self, output: &Output) {
        if self.redraw_owed(output) {
            return;
        }
        self.redraw.push(output.clone());
    }

    /// Every output must be drawn again.
    pub(crate) fn mark_redraw_all(&mut self) {
        let outputs: Vec<Output> = self.space.outputs().cloned().collect();
        for output in outputs {
            self.mark_redraw(&output);
        }
    }

    /// Whether `output` has something to show its last frame did not.
    pub(crate) fn redraw_owed(&self, output: &Output) -> bool {
        self.redraw.iter().any(|owed| owed == output)
    }

    /// [`State::redraw_owed`], forgetting the debt: a frame is drawn now.
    pub(crate) fn take_redraw(&mut self, output: &Output) -> bool {
        let owed = self.redraw_owed(output);
        self.redraw.retain(|owed| owed != output);
        owed
    }

    pub(crate) fn add_output(&mut self, output: &Output, location: Point<i32, Logical>) {
        self.space.map_output(output, location);
        self.place_layers();
        self.rearrange();
        self.mark_redraw(output);
    }

    /// Stop showing anything on `output` (it was disconnected) and withdraw its global.
    pub(crate) fn remove_output(&mut self, output: &Output) {
        self.space.unmap_output(output);
        self.space.refresh();
        self.disabled_outputs.retain(|disabled| disabled != output);
        crate::output::withdraw(output, &self.display);
        self.place_layers();
        self.rearrange();
        self.mark_redraw_all();
    }

    /// The hosted surface that has the pointer.
    pub(crate) fn host_under_pointer(&self) -> Option<&dyn SurfaceHost> {
        match self.pointer_target {
            PointerTarget::Host(index) => self.hosts.get(index).map(AsRef::as_ref),
            PointerTarget::Clients => None,
        }
    }

    /// The outputs as hosted surfaces are told them: each one's logical rectangle and the scale it
    /// draws at.
    pub(crate) fn hosted_outputs(&self) -> Vec<HostedOutput> {
        self.space
            .outputs()
            .filter_map(|output| {
                let rect = self.space.output_geometry(output)?;
                Some(HostedOutput {
                    rect: (rect.loc.x, rect.loc.y, rect.size.w, rect.size.h),
                    scale: output.current_scale().fractional_scale(),
                })
            })
            .collect()
    }

    /// Tell every hosted surface where the outputs are, and the policy which
    /// monitors they hold and which of them is primary, when that changed
    /// since they were last told (an output came or went, changed mode,
    /// scale or place, or the person made another display primary).
    fn outputs_changed(&mut self) {
        let monitors: Vec<(Rectangle<i32, Logical>, String)> = self
            .space
            .outputs()
            .filter_map(|output| {
                let rect = self.space.output_geometry(output)?;
                Some((rect, crate::output::monitor_of(output).0))
            })
            .collect();
        let primary = self.primary_monitor();
        if monitors != self.told_monitors || primary != self.told_primary {
            self.policy.monitors(&monitors, primary.as_deref());
            self.told_monitors = monitors;
            self.told_primary = primary;
        }
        let outputs = self.hosted_outputs();
        if outputs == self.told_outputs {
            return;
        }
        // Layer surfaces follow the output they are on.
        self.place_layers();
        // A lock screen up on a display takes the size that display has now.
        self.fit_lock_surfaces();
        for host in &mut self.hosts {
            host.outputs_changed(&outputs);
        }
        self.told_outputs = outputs;
    }

    /// The monitor the person made primary, while it is lit: the configured connector resolved to
    /// the monitor the policy follows it by. While that connector is off or gone, no monitor is
    /// primary.
    pub(crate) fn primary_monitor(&self) -> Option<String> {
        let connector = self.primary_output.as_deref()?;
        let output = self
            .space
            .outputs()
            .find(|output| output.name() == connector)?;
        self.space.output_geometry(output)?;
        Some(crate::output::monitor_of(output).0)
    }

    /// The connector the person made primary, as their settings name it,
    /// whether or not it is lit. Everything that places windows, bars and
    /// sheets for the person learns of it on the next arrangement, which
    /// this asks for when the choice changed.
    pub(crate) fn set_primary_output(&mut self, connector: Option<String>) {
        if self.primary_output == connector {
            return;
        }
        self.primary_output = connector;
        self.rearrange();
    }

    /// How long the displays hold each frame, the shortest one among them: what the frames of the
    /// surfaces above them are paced to.
    pub(crate) fn frame_interval(&self) -> std::time::Duration {
        self.space
            .outputs()
            .filter_map(|output| output.current_mode().map(|mode| mode.refresh))
            .filter(|refresh| *refresh > 0)
            .map(crate::output::refresh_interval)
            .min()
            .unwrap_or_else(|| crate::output::refresh_interval(0))
    }

    /// The logical rectangles of every output.
    pub(crate) fn output_geometries(&self) -> Vec<Rectangle<i32, Logical>> {
        self.space
            .outputs()
            .filter_map(|output| self.space.output_geometry(output))
            .collect()
    }

    /// The windows whose `zwp_idle_inhibit_v1` request is about a surface the person can see,
    /// described: an inhibitor on a window parked off every output (a hidden tab, another
    /// workspace) holds nothing.
    pub(crate) fn visible_idle_inhibitors(&self) -> Vec<WindowInfo> {
        if self.protocols.idle_inhibitors.is_empty() {
            return Vec::new();
        }
        let outputs = self.output_geometries();
        self.space
            .elements()
            .filter(|window| {
                let holding = self.protocols.idle_inhibitors.iter().any(|surface| {
                    surface.is_alive() && surface_of(window).as_ref() == Some(surface)
                });
                holding
                    && self.space.element_location(window).is_some_and(|location| {
                        on_an_output(location, window.geometry().size, &outputs)
                    })
            })
            .filter_map(|window| describe(self, window))
            .collect()
    }

    /// The mapped windows' descriptions, bottom first.
    pub(crate) fn windows(&self) -> Vec<(WindowInfo, Rectangle<i32, Logical>)> {
        self.space
            .elements()
            .filter_map(|window| {
                let info = describe(self, window)?;
                let location = self.space.element_location(window)?;
                let size = window.geometry().size;
                Some((info, Rectangle::new(location, size)))
            })
            .collect()
    }

    /// Keep `window` , which is about to go, so the desktop can animate it away: it is drawn from
    /// its last buffer as it stood, until the desktop drops it with
    /// [`crate::Compositor::release_snapshot`] or a window of the same id maps again.
    pub(crate) fn keep_closing(&mut self, window: &Window) {
        let Some(info) = describe(self, window) else {
            return;
        };
        let Some(location) = self.space.element_location(window) else {
            return;
        };
        let index = self
            .space
            .elements()
            .position(|element| element == window)
            .unwrap_or(0);
        if self.closing.len() >= MOST_CLOSING {
            self.closing.remove(0);
        }
        self.closing.retain(|closing| closing.info.id != info.id);
        self.committed.remove(&info.id);
        let snapshot = self.snapshots.take(info.id);
        self.closing.push(Closing {
            info,
            snapshot,
            rect: Rectangle::new(location, window.geometry().size),
            index,
            frame: self.framing(window).frame,
        });
        self.mark_redraw_all();
    }

    /// Ask the window `id` to close, as its own close button would: the client decides what closing
    /// means, and does it.
    pub(crate) fn close_window(&self, id: u64) {
        let Some(window) = self.mapped_window(id).cloned() else {
            return;
        };
        if let Some(toplevel) = window.toplevel() {
            toplevel.send_close();
        } else if let Some(x11) = window.x11_surface()
            && let Err(error) = x11.close()
        {
            tracing::warn!(?error, "X11 window: close not sent");
        }
    }

    /// The windows that closed and are still drawn, front first, with the place each one stands at.
    pub(crate) fn closing_windows(&self) -> Vec<(WindowInfo, Rectangle<i32, Logical>)> {
        self.closing
            .iter()
            .map(|closing| (closing.info.clone(), closing.rect))
            .collect()
    }

    /// Let go of the snapshot of the window `id` : it is gone from the next frame on.
    pub(crate) fn release_snapshot(&mut self, id: u64) {
        self.closing.retain(|closing| closing.info.id != id);
        self.effects.remove(&id);
    }

    /// Keep what `window` just drew under `renderer` , for its close.
    pub(crate) fn record_snapshot<R>(&self, renderer: &R, window: &Window)
    where
        R: smithay::backend::renderer::Renderer,
        R::TextureId: Clone + 'static,
    {
        if let Some(id) = describe(self, window).map(|info| info.id) {
            self.snapshots.record_shown(id, &self.hdr, renderer, window);
        }
    }

    /// How `window` is drawn this frame: its effect, or as it stands.
    pub(crate) fn effect_of(&self, window: &Window) -> crate::effects::WindowEffect {
        describe(self, window)
            .and_then(|info| self.effects.get(&info.id))
            .copied()
            .unwrap_or_default()
    }

    /// How `window` is framed: its own frame, or the desktop's. A card an app draws is square, and
    /// so is a window that fills a display, so the display can show that buffer itself instead of a
    /// composed picture of it.
    pub(crate) fn framing(&self, window: &Window) -> crate::effects::Framing<'_> {
        crate::effects::Framing {
            frame: describe(self, window).map_or(self.frame, |info| {
                if self.policy.overlay(&info) {
                    return crate::window_frame::WindowFrame::square();
                }
                self.window_frames
                    .get(&info.id)
                    .copied()
                    .unwrap_or_else(|| {
                        if self.filled_display(window).is_some() {
                            crate::window_frame::WindowFrame::square()
                        } else {
                            self.frame
                        }
                    })
            }),
            images: &self.frame_images,
            snapshots: &self.snapshots,
            hdr: &self.hdr,
            state: self,
        }
    }

    /// The client surface under a logical point, with where it starts in the
    /// logical layout: a layer surface above the windows, else a window's,
    /// else a layer surface below them.
    pub(crate) fn pointer_surface_under(
        &self,
        point: Point<f64, Logical>,
    ) -> Option<(WlSurface, Point<f64, Logical>)> {
        if let Some((surface, origin)) = self.layer_surface_under(point, true) {
            return Some((surface, origin));
        }
        if let Some((_, surface, origin)) = self.surface_under(point) {
            return Some((surface, origin));
        }
        self.layer_surface_under(point, false)
    }

    /// The window under a logical point, with its surface and that surface's origin in the logical
    /// layout.
    pub(crate) fn surface_under(
        &self,
        point: Point<f64, Logical>,
    ) -> Option<(Window, WlSurface, Point<f64, Logical>)> {
        let (window, location) = self.window_at(point)?;
        let origin = location - window.geometry().loc;
        let (surface, offset) =
            window.surface_under(point - origin.to_f64(), WindowSurfaceType::ALL)?;
        Some((window, surface, (origin + offset).to_f64()))
    }

    /// The window a logical point lands on, with where it stands: the frontmost one that is
    /// visible there. What a desk draws over the windows hides what it lies on, and a point on it
    /// belongs to the desk, so the window beneath it is passed over the way a window behind another
    /// is.
    pub(crate) fn window_at(
        &self,
        point: Point<f64, Logical>,
    ) -> Option<(Window, Point<i32, Logical>)> {
        let size = |cover: &smithay::utils::Rectangle<i32, Logical>| {
            smithay::utils::Rectangle::new(cover.loc.to_f64(), cover.size.to_f64())
        };
        let mut covers: Vec<smithay::utils::Rectangle<f64, Logical>> = Vec::new();
        for window in self.space.elements().rev() {
            let Some(location) = self.space.element_location(window) else {
                continue;
            };
            let element = smithay::utils::Rectangle::from_size(window.geometry().size).to_f64();
            if element.contains(point - location.to_f64())
                && !covers.iter().any(|cover| cover.contains(point))
            {
                return Some((window.clone(), location));
            }
            // What the desk covers them with for this window is over the windows behind it, from
            // here on.
            if let Some(info) = describe(self, window)
                && let Some(cover) = self.covers.get(&info.id)
            {
                covers.push(size(cover));
            }
        }
        None
    }

    /// The hosted surface the pointer at `point` belongs to: one above the windows where it drew,
    /// else, where no window is, one beneath them.
    pub(crate) fn host_under(&self, point: Point<f64, Logical>) -> Option<usize> {
        if self.locked() {
            // Only what stands over the lock shows, and it takes the pointer where it drew: the
            // session's own lock view, when the lock screen's app is not there to show the lock.
            return self
                .hosts
                .iter()
                .rposition(|host| host.above_lock() && host.accepts_pointer(point.x, point.y));
        }
        // A layer surface over the windows (quick settings, a panel's popover) is drawn over every
        // hosted surface, so the pointer on it is its own, as on the desk under it.
        if self.layer_surface_under(point, true).is_some() {
            return None;
        }
        // The last host draws on top, so it is asked first.
        let under = |below: bool| {
            self.hosts.iter().rposition(|host| {
                host.below_windows() == below && host.accepts_pointer(point.x, point.y)
            })
        };
        under(false).or_else(|| {
            self.window_at(point)
                .is_none()
                .then(|| under(true))
                .flatten()
        })
    }

    /// Route one input event to the hosted surface or client it belongs to.
    pub(crate) fn handle_input(&mut self, event: InputEvent) {
        let now = Instant::now();
        self.remote_input.note_local(now);
        // The person's own Ctrl+Alt+Escape gives back whatever a client holds — a locked pointer,
        // the desktop's shortcuts — and otherwise cuts remote input. Its press goes no further
        // either way: no window sees half a chord.
        if let InputEvent::Key { keycode, pressed } = event
            && self.remote_input.chord(keycode, pressed)
        {
            if self.pointer_lock.is_engaged() || self.shortcut_inhibit.is_active() {
                self.release_pointer_lock();
                self.suspend_shortcut_inhibit();
            } else if self.remote_input.cut() {
                tracing::info!("the person cut remote input");
            }
            return;
        }
        self.route_input(event);
    }

    /// Route one event from another device, if the person allows it now.
    pub(crate) fn handle_remote_input(&mut self, event: InputEvent) {
        let now = Instant::now();
        if let Some(refusal) = self.remote_input.refusal(now, self.locked()) {
            tracing::debug!(refusal, "remote input refused");
            return;
        }
        self.route_input(event);
    }

    pub(crate) fn route_input(&mut self, event: InputEvent) {
        self.note_input();
        // What a capture takes never reaches the desktop.
        if let Some(observed) = event.clone().observed((self.pointer.x, self.pointer.y))
            && self
                .input_observer
                .as_ref()
                .is_some_and(|observer| observer(observed))
        {
            return;
        }
        // Pointing changes what shows: hover, a press, the pointer itself. A key does not: what
        // takes it shows the answer (a client's commit, a hosted surface's frame, the focus or
        // the windows it moves), and a frame drawn for the key itself would hold that answer back
        // to the display's next refresh.
        if !matches!(event, InputEvent::Key { .. } | InputEvent::Switch { .. }) {
            self.mark_redraw_all();
        }
        let time = self.now_ms();
        let serial = SERIAL_COUNTER.next_serial();
        match event {
            InputEvent::PointerMotion { x, y } => {
                self.pointer_motion(Point::from((x, y)), serial, time);
            }
            InputEvent::PointerMotionRelative { dx, dy } => {
                // Every client with a relative pointer reads the deltas,
                // whether or not the pointer it belongs to still moves.
                self.forward_relative_motion(dx, dy, u64::from(time).saturating_mul(1_000));
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
            InputEvent::Key { keycode, pressed } => {
                // The person's own keyboard, kept so a seat's next event puts their held keys aside
                // and gives them back.
                self.seats.hold_person_key(keycode, pressed);
                self.key(keycode, pressed, serial, time);
            }
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
            InputEvent::TabletPad(event) => {
                self.tablet_pad(&event, serial, time);
            }
            InputEvent::Gesture(gesture) => self.gesture(gesture, serial, time),
            InputEvent::Switch { switch, on } => self.policy.switch(switch, on),
        }
    }

    /// Route a key: a press to the hosted surface that claims it, else to
    /// the topmost one that wants the keyboard, else to the focused client;
    /// a release to where its press went ([`crate::keys`]).
    pub(crate) fn key(&mut self, keycode: u32, pressed: bool, serial: Serial, time: u32) {
        self.sync_keyboard_owner();
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
        let moves_caret = Cell::new(false);
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
                let modifiers = KeyModifiers {
                    ctrl: modifiers.ctrl,
                    alt: modifiers.alt,
                    shift: modifiers.shift,
                    logo: modifiers.logo,
                    caps_lock: modifiers.caps_lock,
                };
                let keysym = handle.modified_sym();
                // The same key on a layout that has a Latin character there,
                // which is what a chord names it by.
                let latin = handle
                    .raw_latin_sym_or_raw_current_sym()
                    .unwrap_or(unmodified);
                let text = pressed.then(|| state.typed(keysym, modifiers)).flatten();
                let key = HostKey {
                    keycode,
                    pressed,
                    repeat: false,
                    keysym: keysym.raw(),
                    unmodified_keysym: unmodified.raw(),
                    latin_keysym: latin.raw(),
                    text,
                    modifiers,
                };
                // Typing and the plain navigation keys move a field's caret; a chord and a
                // modifier do not. What it moved waits for the field to commit before a surface
                // opens at it.
                moves_caret.set(
                    !handle.modified_sym().is_modifier_key()
                        && (key.text.is_some()
                            || !(modifiers.ctrl || modifiers.alt || modifiers.logo)),
                );
                if !pressed {
                    if let Some(host) = state.host_keys.release(keycode) {
                        // A held modifier's release also reaches what else answers it: a surface
                        // that opened while it was down (a switcher), whose press went to the
                        // window it now stands over.
                        if crate::sticky_keys::is_modifier(keycode) {
                            state.release_beside(&key, host);
                        }
                        return FilterResult::Intercept((host, key));
                    }
                    // A key no surface took the press of, let go: a surface that takes it now (a
                    // shell's switcher, which opened after Alt went down) hears it, so a held
                    // modifier's release reaches what it opened. Otherwise it goes on to the client
                    // as any release does. A client holding the desktop's shortcuts had the press,
                    // so it has the release too.
                    return match state.release_host(&key) {
                        Some(host) => FilterResult::Intercept((host, key)),
                        None => FilterResult::Forward,
                    };
                }
                // Typing is what arms the rule that nothing moves under it: a key that types
                // something, modifiers and chords aside.
                if key.text.is_some() {
                    state.typed_at = Some(Instant::now());
                }
                let locked = state.locked();
                // A client holding the desktop's shortcuts keeps them: only what must never be
                // given up is still taken below.
                let inhibited = state.shortcut_inhibit.is_active();
                let repeats = !handle.modified_sym().is_modifier_key();
                let Some(index) =
                    state.press_host(&key, locked, inhibited, repeats, Instant::now())
                else {
                    return FilterResult::Forward;
                };
                FilterResult::Intercept((index, key))
            },
        );
        match routed {
            Some((index, key)) => {
                if let Some(host) = self.hosts.get_mut(index) {
                    host.key(&key);
                }
            }
            // A press no hosted surface took reached the client that has the keyboard: its next
            // commit answers it.
            None => {
                if pressed
                    && let Some(focus) = self
                        .seat
                        .get_keyboard()
                        .and_then(|seat| seat.current_focus())
                {
                    self.key_delivered(&focus, keycode);
                    self.note_input_on(&focus);
                    if moves_caret.get() {
                        self.note_caret_may_move();
                    }
                }
            }
        }
    }

    /// A released modifier reaches the hosted surfaces that answer it besides the one that had its
    /// press: what opened while it was held hears it let go. A client holding the desktop's
    /// shortcuts has its releases too.
    fn release_beside(&mut self, key: &HostKey, recorded: usize) {
        if self.seat_keys_only || self.shortcut_inhibit.is_active() {
            return;
        }
        let locked = self.locked();
        let composing = self.composing();
        for index in (0..self.hosts.len()).rev() {
            if index == recorded || composing == Some(index) {
                continue;
            }
            let claim = self
                .hosts
                .get_mut(index)
                .map_or(KeyClaim::Pass, |host| host.claim_key(key, locked, false));
            if claim == KeyClaim::Pass {
                continue;
            }
            if let Some(host) = self.hosts.get_mut(index) {
                host.key(key);
            }
            return;
        }
    }

    /// The hosted surface a released key belongs to: one that claims it, from the top of the stack
    /// down. A held modifier's release reaches what it opened; a client holding the desktop's
    /// shortcuts has its releases too.
    fn release_host(&mut self, key: &HostKey) -> Option<usize> {
        if self.seat_keys_only || self.shortcut_inhibit.is_active() {
            return None;
        }
        let locked = self.locked();
        let composing = self.composing();
        for index in (0..self.hosts.len()).rev() {
            if composing == Some(index) {
                continue;
            }
            let claim = self
                .hosts
                .get_mut(index)
                .map_or(KeyClaim::Pass, |host| host.claim_key(key, locked, false));
            if claim != KeyClaim::Pass {
                return Some(index);
            }
        }
        None
    }

    /// The hosted surface a pressed key belongs to, its press recorded: one that claims it, else
    /// the topmost that wants the keyboard. While locked the lock client has it, and no hosted
    /// surface takes one.
    /// The field an input method composes in takes no key itself: the method has the keyboard, and
    /// gives back what it leaves (see [`State::handle_ime_key`]). Every other surface still answers
    /// its chords.
    fn press_host(
        &mut self,
        key: &HostKey,
        locked: bool,
        inhibited: bool,
        repeats: bool,
        now: Instant,
    ) -> Option<usize> {
        if self.seat_keys_only {
            // A seat's key belongs to the client the seat aims at, whatever the person has open
            // over it.
            return None;
        }
        let composing = self.composing();
        for index in (0..self.hosts.len()).rev() {
            if composing == Some(index) {
                continue;
            }
            let claim = self.hosts.get_mut(index).map_or(KeyClaim::Pass, |host| {
                host.claim_key(key, locked, inhibited)
            });
            if claim != KeyClaim::Pass {
                self.host_keys
                    .press(index, key, claim == KeyClaim::TakeRepeating, now);
                return Some(index);
            }
        }
        let wanting = self
            .hosts
            .iter()
            .enumerate()
            .rev()
            .find(|(index, host)| {
                (!locked || host.above_lock()) && Some(*index) != composing && host.wants_keyboard()
            })
            .map(|(index, _)| index)?;
        self.host_keys.press(wanting, key, repeats, now);
        Some(wanting)
    }

    /// What pressing the key with `keysym` types into a hosted surface's
    /// field, the modifiers held being `modifiers`: the key's own character,
    /// or what the dead key or the Compose key before it finishes into.
    fn typed(&mut self, keysym: Keysym, modifiers: KeyModifiers) -> Option<String> {
        // A chord types no text: it neither starts nor continues a sequence.
        if modifiers.ctrl || modifiers.logo {
            return None;
        }
        let own = keysym.key_char().filter(|ch| !ch.is_control());
        match self.compose.as_mut() {
            Some(compose) => compose.typed(keysym, own),
            None => own.map(String::from),
        }
    }

    /// Whether an arranging change must not move anything now, and why: a pointer button is down
    /// (something is being dragged or resized, or a press is in flight), or the person typed within
    /// the typing pause.
    pub(crate) fn underway(&self) -> crate::Underway {
        crate::Underway {
            gesture: !self.buttons_down.is_empty(),
            typing: self.typing(),
        }
    }

    /// Whether the person typed within the typing pause: they are typing.
    pub(crate) fn typing(&self) -> bool {
        self.typed_at.is_some_and(|at| at.elapsed() <= TYPING_PAUSE)
    }

    /// Hand hosted surfaces the repeats of the keys they hold that are due.
    pub(crate) fn repeat_keys(&mut self) {
        for (index, key) in self.host_keys.due(Instant::now()) {
            if let Some(host) = self.hosts.get_mut(index) {
                host.key(&key);
            }
        }
    }

    pub(crate) fn pointer_axis(&mut self, horizontal: f64, vertical: f64, time: u32) {
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
        let to = self.clamp_to_outputs(to);
        // A locked pointer stays where it is, and a confined one stops at the edge of its region;
        // their clients read the deltas instead.
        let Some((x, y)) = self
            .pointer_lock
            .constrain((self.pointer.x, self.pointer.y), (to.x, to.y))
        else {
            return;
        };
        self.pointer = Point::from((x, y));
        let mut location = self.pointer;
        self.policy.pointer_moved(location.x, location.y);
        if self.drag_motion(location, serial, time) {
            return;
        }
        self.x11_drag_motion(location);
        // A client's press keeps the pointer until it is released, over hosted surfaces too.
        let grabbed = self
            .seat
            .get_pointer()
            .is_some_and(|pointer| pointer.is_grabbed());
        let target = if grabbed {
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
            PointerTarget::Clients if self.locked() => {
                self.lock_surface_under(location).map(|under| {
                    // The screen covers the display, stretched to it: it reads the point where it
                    // drew what the person pointed at.
                    location = under.at;
                    (under.surface, under.origin)
                })
            }
            PointerTarget::Clients => self.pointer_surface_under(location),
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
        // Somebody else already holds this button: the one pointer the windows see stays down until
        // the last of them lets go.
        if !self.seats.hold_person_button(button, pressed) {
            return;
        }
        if pressed {
            self.buttons_down.insert(button);
        } else {
            self.buttons_down.remove(&button);
            // Every surface hears the press end, before any of them acts on the button: a press and
            // what it is still doing are over together.
            for host in &mut self.hosts {
                host.press_ended(self.pointer.x, self.pointer.y);
            }
        }
        if self.drag_button(button, pressed, serial, time) {
            return;
        }
        if pressed {
            self.policy.pointer_pressed(self.pointer.x, self.pointer.y);
            let target = self.pointer_target;
            for (index, host) in self.hosts.iter_mut().enumerate() {
                if target != PointerTarget::Host(index) {
                    host.pressed_elsewhere(self.pointer.x, self.pointer.y);
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
        if pressed
            && let Some(focus) = self
                .seat
                .get_pointer()
                .and_then(|seat| seat.current_focus())
        {
            self.note_input_on(&focus);
        }
        if !pressed {
            self.x11_drag_motion(self.pointer);
        }
    }

    /// Deliver one pointer button as a seat pressed it: to the client under the pointer, focusing
    /// nothing and raising nothing.
    pub(crate) fn seat_button(&mut self, button: u32, pressed: bool, serial: Serial, time: u32) {
        if pressed {
            self.buttons_down.insert(button);
        } else {
            self.buttons_down.remove(&button);
        }
        self.pointer_target = PointerTarget::Clients;
        let state = if pressed {
            ButtonState::Pressed
        } else {
            ButtonState::Released
        };
        let Some(pointer) = self.seat.get_pointer() else {
            return;
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

    /// Give the window with `id` the keyboard, for a seat's keys: it is not raised and stays
    /// inactive, so the person's desk looks as it did.
    pub(crate) fn focus_window_id(&mut self, id: u64, serial: Serial) {
        let surface = self.mapped_window(id).and_then(surface_of);
        self.set_keyboard_focus(surface, serial);
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
        self.mark_redraw_all();
        // A modal dialog of the window takes the focus in its place, with the window raised under
        // it.
        self.space.raise_element(window, true);
        let modal = self.modal_dialog_of(window);
        let window = modal.as_ref().unwrap_or(window);
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
        self.set_keyboard_focus(surface_of(window), serial);
    }

    /// The modal dialog in front of `window` : its own modal dialog, or that dialog's, and so on.
    fn modal_dialog_of(&self, window: &Window) -> Option<Window> {
        let mut owner = window.toplevel()?.wl_surface().clone();
        let mut front = None;
        // A chain of dialogs is never longer than the windows there are.
        for _ in 0..self.space.elements().count() {
            let Some(dialog) = self
                .space
                .elements()
                .find(|candidate| {
                    candidate.toplevel().is_some_and(|toplevel| {
                        toplevel.parent().as_ref() == Some(&owner) && is_modal(toplevel)
                    })
                })
                .cloned()
            else {
                break;
            };
            owner = dialog.toplevel()?.wl_surface().clone();
            front = Some(dialog);
        }
        front
    }

    /// Map a toplevel whose first buffer arrived (or an X11 window that asked to be mapped), where
    /// the policy places it.
    pub(crate) fn map_window(&mut self, window: &Window) {
        let Some(info) = describe(self, window) else {
            return;
        };
        // The app's first buffer is here: the launch that asked for it is timed to the frame that
        // first shows it.
        if let Some(app) = info.app_id.as_deref() {
            tilo_startup_metrics::launch_mapped(app, info.id);
        }
        // It came back before the desktop was done with its close.
        self.closing.retain(|closing| closing.info.id != info.id);
        self.snapshots.forget(info.id);
        self.hdr.forget(info.id);
        let outputs = self.output_geometries();
        let size = window.geometry().size;
        let location = self.policy.place(&info, (size.w, size.h), &outputs);
        self.space.map_element(window.clone(), location, false);
        if let Some(x11) = window.x11_surface() {
            // X11 windows know their place in the root window.
            let _ = x11.configure(Rectangle::new(location, size));
        }
        if self.policy.overlay(&info) {
            self.space.raise_element(window, true);
        } else if self.policy.focus_on_map(&info) {
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
        let mut moved = false;
        let stack: Vec<Window> = self.space.elements().cloned().collect();
        for window in stack {
            let Some(id) = describe(self, &window).map(|info| info.id) else {
                continue;
            };
            let Some(arrangement) = arrangements.iter().find(|arrangement| arrangement.id == id)
            else {
                continue;
            };
            let was = self.space.element_location(&window);
            if was != Some(arrangement.location) {
                moved = true;
            }
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
        if moved {
            // A window the policy placed elsewhere may stand on other outputs now: they are the
            // ones that show it, and what they show changed, so they owe a frame.
            self.space.refresh();
            self.mark_redraw_all();
        }
        // A window floating over the desk stands in front of the strips' windows, a focused or
        // maximized one included; the dialogs stay in front of the windows they belong to, and a
        // card an app draws stays in front of them all.
        let floats: Vec<_> = self
            .space
            .elements()
            .filter(|window| {
                describe(self, window).is_some_and(|info| self.covers.contains_key(&info.id))
            })
            .cloned()
            .collect();
        if !floats.is_empty() {
            for window in floats {
                self.space.raise_element(&window, false);
            }
            let dialogs: Vec<_> = self
                .space
                .elements()
                .filter(|window| {
                    window
                        .toplevel()
                        .is_some_and(|toplevel| toplevel.parent().is_some())
                })
                .cloned()
                .collect();
            for window in dialogs {
                self.space.raise_element(&window, false);
            }
        }
        let cards: Vec<_> = self
            .space
            .elements()
            .filter(|window| describe(self, window).is_some_and(|info| self.policy.overlay(&info)))
            .cloned()
            .collect();
        for window in cards {
            self.space.raise_element(&window, true);
        }
        self.apply_policy_focus_and_closes();
    }

    /// Give the keyboard to the window the policy brought forward, and ask the clients of the
    /// windows it closed to close.
    fn apply_policy_focus_and_closes(&mut self) {
        let window_of = |state: &Self, id: u64| state.mapped_window(id).cloned();
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
        // A panel over the windows that holds the keyboard keeps it while it shows: the window
        // gets it back when the panel is hidden.
        if self
            .keyboard_focus()
            .is_some_and(|focus| self.layers.shows_surface(&focus))
        {
            return;
        }
        if let Some(window) = window_of(self, id) {
            self.focus_window(&window, SERIAL_COUNTER.next_serial());
        }
    }

    /// Drop windows whose clients went away and refresh output membership.
    pub(crate) fn refresh(&mut self) {
        // A closed window is one the frame shows no more.
        let shown = self.space.elements().count();
        self.space.refresh();
        if self.space.elements().count() != shown {
            self.mark_redraw_all();
        }
        self.popups.cleanup();
        self.unmapped.retain(IsAlive::alive);
        self.sync_toplevels();
        self.layers.refresh();
    }
}

/// A toplevel's identity, unique for the compositor's life.
struct WindowId(u64);

impl State {
    /// `window` asked for the keyboard: it gets it when the policy says so
    /// ([`WindowPolicy::activate`]).
    pub(crate) fn activation_requested(&mut self, window: &Window, fresh: bool) {
        let focus = describe(self, window).map_or(fresh, |info| self.policy.activate(&info, fresh));
        if focus {
            self.focus_window(window, SERIAL_COUNTER.next_serial());
        }
    }

    /// Focus the window under `point` if the policy focuses on click, as a pointer press or a touch
    /// there does.
    pub(crate) fn focus_on_press(&mut self, point: Point<f64, Logical>, serial: Serial) {
        if self.locked() {
            return;
        }
        if let Some((window, _, _)) = self.surface_under(point) {
            let info = describe(self, &window);
            if info.is_some_and(|info| self.policy.focus_on_click(&info)) {
                self.focus_window(&window, serial);
            }
        }
        self.layer_focus_on_press(point);
    }
}

static NEXT_WINDOW: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

/// Where X11 window ids start in the [`WindowInfo`] id space, apart from Wayland toplevels'
/// counter.
const X11_IDS: u64 = 1 << 40;

/// How far up a display number sits in an X11 window id: every display's X identifiers start over,
/// so two workspaces' windows would otherwise share ids.
const X11_DISPLAY_SHIFT: u32 = 32;

/// The id of `window` , when it has one.
/// The `wl_surface` a window draws into: its toplevel's, or an X11 window's once Xwayland told us.
pub(crate) fn surface_of(window: &Window) -> Option<WlSurface> {
    smithay::wayland::seat::WaylandFocus::wl_surface(window).map(std::borrow::Cow::into_owned)
}

pub(crate) fn window_id(state: &State, window: &Window) -> Option<u64> {
    if let Some(toplevel) = window.toplevel() {
        return window_id_of(toplevel.wl_surface());
    }
    let x11 = window.x11_surface()?;
    if x11.is_override_redirect() {
        return None;
    }
    x11_window_id(state, x11)
}

/// The id of the toplevel drawing into `surface` , once it has one.
pub(crate) fn window_id_of(surface: &WlSurface) -> Option<u64> {
    smithay::wayland::compositor::with_states(surface, |states| {
        states.data_map.get::<WindowId>().map(|id| id.0)
    })
}

/// The [`WindowInfo`] id of the X11 `window` , on the display that shows it.
pub(crate) fn x11_window_id(state: &State, window: &smithay::xwayland::X11Surface) -> Option<u64> {
    let xwm = window.xwm_id()?;
    x11_window_id_of(state, xwm, window.window_id())
}

/// The [`WindowInfo`] id of the X11 window `window` of `xwm` 's display.
pub(crate) fn x11_window_id_of(
    state: &State,
    xwm: smithay::xwayland::xwm::XwmId,
    window: u32,
) -> Option<u64> {
    let display = state.xwms.iter().find(|server| server.id() == xwm)?.display;
    Some(X11_IDS | (u64::from(display) << X11_DISPLAY_SHIFT) | u64::from(window))
}

/// Forget the toplevels still waiting for their first buffer on `xwm` 's display: their server is
/// going, so they never map.
pub(crate) fn forget_unmapped_of(state: &mut State, xwm: smithay::xwayland::xwm::XwmId) {
    state.unmapped.retain(|window| {
        window
            .x11_surface()
            .and_then(smithay::xwayland::X11Surface::xwm_id)
            != Some(xwm)
    });
}

/// Whether a window at `location` of `size` is on one of `outputs` : the desktop parks a hidden tab
/// or a workspace nobody is on far off every output, and nothing there is something the person
/// sees.
pub(crate) fn on_an_output(
    location: Point<i32, Logical>,
    size: smithay::utils::Size<i32, Logical>,
    outputs: &[Rectangle<i32, Logical>],
) -> bool {
    size.w > 0
        && size.h > 0
        && outputs
            .iter()
            .any(|output| output.overlaps(Rectangle::new(location, size)))
}

/// What a policy learns about a window, Wayland or X11; `None` for override-redirect X11 windows,
/// which place themselves.
pub(crate) fn describe(state: &State, window: &Window) -> Option<WindowInfo> {
    if let Some(toplevel) = window.toplevel() {
        return Some(window_info(&state.display, &state.icons, toplevel));
    }
    let x11 = window.x11_surface()?;
    if x11.is_override_redirect() {
        return None;
    }
    let size = |size: Option<smithay::utils::Size<i32, Logical>>| {
        size.filter(|size| size.w > 0 && size.h > 0)
            .map(|size| (size.w, size.h))
    };
    let id = x11_window_id(state, x11)?;
    let xwm = x11.xwm_id()?;
    Some(WindowInfo {
        id,
        app_id: Some(x11.class()),
        title: Some(x11.title()),
        parent: x11
            .is_transient_for()
            .and_then(|parent| x11_window_id_of(state, xwm, parent)),
        min_size: size(x11.min_size()),
        max_size: size(x11.max_size()),
        pid: x11.pid(),
        icon: state.icons.of(id),
    })
}

/// Whether the client marked `toplevel` a modal dialog (xdg-dialog).
fn is_modal(toplevel: &ToplevelSurface) -> bool {
    smithay::wayland::compositor::with_states(toplevel.wl_surface(), |states| {
        states
            .data_map
            .get::<smithay::wayland::shell::xdg::XdgToplevelSurfaceData>()
            .and_then(|data| data.lock().ok().map(|data| data.modal))
            .unwrap_or(false)
    })
}

impl State {
    /// The mapped window [`WindowInfo::id`] `id` names: the one a policy, a tool or a client's own
    /// id is about. `None` for an id no mapped window holds.
    ///
    /// [`WindowInfo::id`]: crate::capture::WindowInfo::id
    pub(crate) fn mapped_window(&self, id: u64) -> Option<&Window> {
        self.space
            .elements()
            .find(|window| describe(self, window).is_some_and(|info| info.id == id))
    }

    /// The rectangle a shareable window holds in the layout, by its foreign-toplevel identifier.
    pub(crate) fn shareable_window_rect(&self, identifier: &str) -> Option<(i32, i32, i32, i32)> {
        let window = self.capture.window(identifier)?;
        let rect = self.space.element_geometry(window)?;
        Some((rect.loc.x, rect.loc.y, rect.size.w, rect.size.h))
    }

    /// The rectangle the mapped window `id` holds in the layout: where its content is drawn, which
    /// is what a tool driving the desk aims at. `None` for a window that is not mapped, or is not
    /// placed on a display at all.
    pub(crate) fn window_rect(&self, id: u64) -> Option<(i32, i32, i32, i32)> {
        let rect = self.space.element_geometry(self.mapped_window(id)?)?;
        Some((rect.loc.x, rect.loc.y, rect.size.w, rect.size.h))
    }

    /// The frames the window `id` 's client committed since it opened: a window that shows the same
    /// thing keeps its count, so a desk that draws nothing between two reads has not moved.
    pub(crate) fn committed_frames(&self, id: u64) -> u64 {
        self.committed.get(&id).copied().unwrap_or(0)
    }

    /// The window whose client draws with `root` , the surface of its toplevel or the surface an X11
    /// window paints into.
    fn committed_window(&self, root: &WlSurface) -> Option<u64> {
        if let Some(id) = window_id_of(root) {
            return Some(id);
        }
        self.space
            .elements()
            .chain(self.unmapped.iter())
            .find(|window| {
                window
                    .x11_surface()
                    .and_then(|x11| x11.wl_surface())
                    .is_some_and(|surface| &surface == root)
            })
            .and_then(|window| describe(self, window).map(|info| info.id))
    }

    /// The keys that type `ch` on the active layout, as the keymap itself says: what a synthesised
    /// keystroke for that character must press.
    pub(crate) fn stroke_for(&mut self, ch: char) -> Option<crate::typing::Stroke> {
        let keyboard = self.seat.get_keyboard()?;
        keyboard.with_xkb_state(self, |context| {
            let xkb = context.xkb().lock().ok()?;
            let layout = xkb.active_layout().0;
            // Safety: the keymap reference never outlives the `Xkb` it borrows from, which the lock
            // holds for this scan.
            let keymap = unsafe { xkb.keymap() };
            // Every key a real keyboard sends arrives as an evdev code in this range; a keymap also
            // binds codes no keyboard sends.
            crate::typing::stroke_for(keymap, layout, ch, |code| (1..=255).contains(&code))
        })
    }

    /// Where the active layout types each ASCII mark, by the keys [`Self::stroke_for`] presses.
    pub(crate) fn typed_marks(&mut self) -> Vec<crate::typing::TypedMark> {
        let Some(keyboard) = self.seat.get_keyboard() else {
            return Vec::new();
        };
        keyboard.with_xkb_state(self, |context| {
            let Ok(xkb) = context.xkb().lock() else {
                return Vec::new();
            };
            let layout = xkb.active_layout().0;
            // Safety: the keymap reference never outlives the `Xkb` it borrows from, which the lock
            // holds for this scan.
            let keymap = unsafe { xkb.keymap() };
            crate::typing::typed_marks(keymap, layout, |code| (1..=255).contains(&code))
        })
    }

    /// The evdev code of a key of the active layout that produces `keysym` ; `None` when the layout
    /// cannot type it.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the keymap read holds the seat's xkb lock for the whole scan"
    )]
    pub(crate) fn keycode_for_keysym(&mut self, keysym: u32) -> Option<u32> {
        use smithay::input::keyboard::{Keycode, Keysym};
        let keyboard = self.seat.get_keyboard()?;
        let wanted = Keysym::new(keysym);
        keyboard.with_xkb_state(self, |context| {
            let xkb = context.xkb().lock().ok()?;
            let layout = xkb.active_layout();
            // XKB keycodes start at 8 (evdev 0 is XKB 8); 8..=255 is a standard keyboard's range.
            (8u32..=255).find_map(|raw| {
                let keycode: Keycode = raw.into();
                let syms = xkb.raw_syms_for_key_in_layout(keycode, layout);
                syms.contains(&wanted).then(|| raw.saturating_sub(8))
            })
        })
    }
}

/// The surface a client draws a window with: the one itself, or the highest ancestor of its
/// subsurface chain.
fn root_surface(surface: &WlSurface) -> WlSurface {
    let mut root = surface.clone();
    while let Some(parent) = get_parent(&root) {
        root = parent;
    }
    root
}

/// The process of a client, as `pid` .
fn pid_of(client: Option<&Client>, display: &DisplayHandle) -> Option<u32> {
    let client = client?;
    let pid = u32::try_from(client.get_credentials(display).ok()?.pid).ok()?;
    // Noted for the client, so the session hears when its process exits.
    if let Some(data) = client.get_data::<ClientState>() {
        data.pid.store(pid, Ordering::Relaxed);
    }
    Some(pid)
}

/// What a policy learns about a toplevel.
pub(crate) fn window_info(
    display: &DisplayHandle,
    icons: &crate::toplevel_icon::Icons,
    toplevel: &ToplevelSurface,
) -> WindowInfo {
    let id_of = |surface: &WlSurface| {
        smithay::wayland::compositor::with_states(surface, |states| {
            states.data_map.get::<WindowId>().map(|id| id.0)
        })
    };
    let parent = toplevel.parent().as_ref().and_then(id_of);
    let mut info = smithay::wayland::compositor::with_states(toplevel.wl_surface(), |states| {
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
        let (min, max) = {
            let mut cached = states
                .cached_state
                .get::<smithay::wayland::shell::xdg::SurfaceCachedState>();
            let current = cached.current();
            let limits = (current.min_size, current.max_size);
            drop(cached);
            limits
        };
        let size = |size: smithay::utils::Size<i32, Logical>| {
            (size.w > 0 && size.h > 0).then_some((size.w, size.h))
        };
        WindowInfo {
            id,
            app_id,
            title,
            parent,
            min_size: size(min),
            max_size: size(max),
            pid: pid_of(toplevel.wl_surface().client().as_ref(), display),
            icon: None,
        }
    });
    info.icon = icons.of(info.id);
    info
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
        // A client drew: the frame it committed is on its way.
        self.mark_redraw_all();
        if self.layers.holds(surface) {
            self.layer_committed(surface);
        }
        self.drag_icon_commit(surface);
        // Colorimetry passthrough: a shared window's stream is told what its content is in on the
        // commit that says so, so a receiver decodes in the right space, and that the configured
        // treatment applies again once the client unsets it.
        self.tell_share_color(surface);
        if !is_sync_subsurface(surface) {
            let root = root_surface(surface);
            // A frame this client drew, counted against its window: a still window keeps one count
            // and an app that draws moves it, which is what a tool reads to tell the two apart.
            if let Some(id) = self.committed_window(&root) {
                *self.committed.entry(id).or_default() += 1;
            }
            // A key this client was given is answered by its next commit.
            self.commit_answered(&root);
            // The icon a client set takes effect on this commit, like any other double-buffered
            // state.
            if let Some((window, icon)) = crate::toplevel_icon::committed(&root) {
                self.icons.set(window, icon);
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
                        let info = window_info(&self.display, &self.icons, toplevel);
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

    fn client_pong(&mut self, client: ShellClient) {
        self.shell_pongs.note(client);
    }

    fn client_destroyed(&mut self, client: ShellClient) {
        self.shell_pongs.forget(&client);
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

    fn move_request(&mut self, surface: ToplevelSurface, _seat: WlSeat, serial: Serial) {
        if let Some(window) = self.window_of_toplevel(&surface) {
            self.window_request(&window, &crate::policy::WindowRequest::Move, Some(serial));
        }
    }

    fn resize_request(
        &mut self,
        surface: ToplevelSurface,
        _seat: WlSeat,
        serial: Serial,
        edges: smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::ResizeEdge,
    ) {
        if let Some(window) = self.window_of_toplevel(&surface) {
            let request = crate::policy::WindowRequest::Resize(crate::requests::xdg_edges(edges));
            self.window_request(&window, &request, Some(serial));
        }
    }

    fn maximize_request(&mut self, surface: ToplevelSurface) {
        match self.window_of_toplevel(&surface) {
            Some(window) => {
                self.window_request(&window, &crate::policy::WindowRequest::Maximize(true), None);
            }
            None => {
                surface.send_configure();
            }
        }
    }

    fn unmaximize_request(&mut self, surface: ToplevelSurface) {
        if let Some(window) = self.window_of_toplevel(&surface) {
            self.window_request(
                &window,
                &crate::policy::WindowRequest::Maximize(false),
                None,
            );
        }
    }

    fn fullscreen_request(
        &mut self,
        surface: ToplevelSurface,
        output: Option<smithay::reexports::wayland_server::protocol::wl_output::WlOutput>,
    ) {
        let output = output
            .as_ref()
            .and_then(Output::from_resource)
            .map(|output| output.name());
        match self.window_of_toplevel(&surface) {
            Some(window) => self.window_request(
                &window,
                &crate::policy::WindowRequest::Fullscreen { on: true, output },
                None,
            ),
            None => {
                surface.send_configure();
            }
        }
    }

    fn unfullscreen_request(&mut self, surface: ToplevelSurface) {
        if let Some(window) = self.window_of_toplevel(&surface) {
            let request = crate::policy::WindowRequest::Fullscreen {
                on: false,
                output: None,
            };
            self.window_request(&window, &request, None);
        }
    }

    fn minimize_request(&mut self, surface: ToplevelSurface) {
        if let Some(window) = self.window_of_toplevel(&surface) {
            self.window_request(&window, &crate::policy::WindowRequest::Minimize, None);
        }
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        // A share is of a window: it ends with it, and its stream hears nothing more from here.
        if let Some(window) = window_id_of(surface.wl_surface()) {
            self.share_streams.remove(&window);
        }
        let pending = self
            .unmapped
            .iter()
            .any(|window| window.toplevel() == Some(&surface));
        self.unmapped
            .retain(|window| window.toplevel() != Some(&surface));
        if pending {
            self.policy
                .withdrawn(&window_info(&self.display, &self.icons, &surface));
        }
        let gone = self
            .space
            .elements()
            .find(|window| window.toplevel() == Some(&surface))
            .cloned();
        if let Some(window) = gone {
            // A window that goes before a frame showed it ends its launch's timing: no frame is
            // coming for that surface.
            if let Some(id) = window_id_of(surface.wl_surface()) {
                tilo_startup_metrics::launch_window_closed(id);
            }
            self.keep_closing(&window);
            self.space.unmap_elem(&window);
            self.rearrange();
            // Its frames are gone for good: the heap they were in is handed back to the kernel once
            // the closes have settled.
            crate::heap_trim::window_closed();
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

    fn focus_changed(&mut self, _seat: &Seat<Self>, focused: Option<&WlSurface>) {
        let client = focused.and_then(|surface| self.display.get_client(surface.id()).ok());
        self.selection_focus(client.as_ref());
        self.pads_follow_keyboard(focused);
    }
}

impl DataDeviceHandler for State {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device_state
    }
}

impl OutputHandler for State {}

smithay::delegate_compositor!(State);
smithay::delegate_shm!(State);
smithay::delegate_xdg_shell!(State);
// The seat's requests are smithay's, but one of them says something about the client that made it:
// binding `wl_touch` decides how a finger reaches it, so the seat is dispatched here to record it.
// The global and the seat's objects keep smithay's dispatch, which `delegate_seat!` would add
// whole.
smithay::reexports::wayland_server::delegate_global_dispatch!(State: [smithay::reexports::wayland_server::protocol::wl_seat::WlSeat: smithay::wayland::seat::SeatGlobalData<State>] => SeatState<State>);
smithay::reexports::wayland_server::delegate_dispatch!(State: [smithay::reexports::wayland_server::protocol::wl_pointer::WlPointer: smithay::wayland::seat::PointerUserData<State>] => SeatState<State>);
smithay::reexports::wayland_server::delegate_dispatch!(State: [smithay::reexports::wayland_server::protocol::wl_keyboard::WlKeyboard: smithay::wayland::seat::KeyboardUserData<State>] => SeatState<State>);
smithay::reexports::wayland_server::delegate_dispatch!(State: [smithay::reexports::wayland_server::protocol::wl_touch::WlTouch: smithay::wayland::seat::TouchUserData<State>] => SeatState<State>);

impl Dispatch<WlSeat, SeatUserData<State>> for State {
    fn request(
        state: &mut Self,
        client: &Client,
        seat: &WlSeat,
        request: smithay::reexports::wayland_server::protocol::wl_seat::Request,
        data: &SeatUserData<State>,
        display: &DisplayHandle,
        init: &mut DataInit<'_, Self>,
    ) {
        if matches!(
            request,
            smithay::reexports::wayland_server::protocol::wl_seat::Request::GetTouch { .. }
        ) && let Some(client) = client.get_data::<ClientState>()
        {
            client.touch_bound.store(true, Ordering::Relaxed);
        }
        <SeatState<State> as Dispatch<WlSeat, SeatUserData<State>, State>>::request(
            state, client, seat, request, data, display, init,
        );
    }
}

smithay::delegate_data_device!(State);
smithay::delegate_output!(State);
smithay::delegate_xdg_toplevel_icon!(State);

/// A fresh client's data, for [`DisplayHandle::insert_client`], whose disconnect is recorded in
/// `exited` , and whose layer surfaces stand on `output` when the session connected it for one.
pub(crate) fn client_data(
    trusted: bool,
    exited: Arc<std::sync::Mutex<Vec<u32>>>,
    output: Option<String>,
) -> Arc<ClientState> {
    Arc::new(ClientState {
        trusted,
        exited,
        output,
        ..ClientState::default()
    })
}

impl State {
    /// The processes of the clients that disconnected since the last look.
    pub(crate) fn take_exited_pids(&self) -> Vec<u32> {
        self.exited
            .lock()
            .map(|mut exited| std::mem::take(&mut *exited))
            .unwrap_or_default()
    }
}

impl State {
    /// The `xdg_wm_base` client of window `window` .
    fn shell_client_of(&self, window: u64) -> Option<(ShellClient, ClientId)> {
        self.xdg_shell_state
            .toplevel_surfaces()
            .iter()
            .find(|toplevel| window_id_of(toplevel.wl_surface()) == Some(window))
            .and_then(|toplevel| {
                let id = toplevel.wl_surface().client()?.id();
                Some((toplevel.client(), id))
            })
    }

    /// The pong count of connection `id` 's `xdg_wm_base` ; `None` once it has no toplevel left.
    fn pongs_of(&self, id: &ClientId) -> Option<u64> {
        self.xdg_shell_state
            .toplevel_surfaces()
            .iter()
            .find(|toplevel| {
                toplevel
                    .wl_surface()
                    .client()
                    .is_some_and(|client| client.id() == *id)
            })
            .map(|toplevel| self.shell_pongs.count(&toplevel.client()))
    }

    /// Input reached the app of window `window` : ask it to answer, so an app that stopped
    /// answering is told apart from a quiet one.
    pub(crate) fn note_app_input(&mut self, window: u64) {
        self.note_app_input_at(window, Instant::now());
    }

    /// [`State::note_app_input`] as of `now` .
    pub(crate) fn note_app_input_at(&mut self, window: u64, now: Instant) {
        let Some((client, id)) = self.shell_client_of(window) else {
            return;
        };
        let pongs = self.shell_pongs.count(&client);
        self.app_watches.note_input(&client, &id, pongs, now);
    }

    /// Input reached `surface` : its window's app is pinged, if it has one.
    pub(crate) fn note_input_on(&mut self, surface: &WlSurface) {
        if let Some(window) = window_id_of(&root_surface(surface)) {
            self.note_app_input(window);
        }
    }

    /// Judge the pings in flight. Returns whether the set of windows whose app is not responding
    /// changed, so the desk repaints.
    pub(crate) fn advance_app_liveness_at(&mut self, now: Instant) -> bool {
        let mut watches = std::mem::take(&mut self.app_watches);
        watches.judge(|id| self.pongs_of(id), now);
        let hung = watches.hung();
        self.app_watches = watches;
        let mut windows: Vec<u64> = self
            .xdg_shell_state
            .toplevel_surfaces()
            .iter()
            .filter(|toplevel| {
                toplevel
                    .wl_surface()
                    .client()
                    .is_some_and(|client| hung.contains(&client.id()))
            })
            .filter_map(|toplevel| window_id_of(toplevel.wl_surface()))
            .collect();
        windows.sort_unstable();
        windows.dedup();
        let changed = windows != self.not_responding;
        self.not_responding = windows;
        changed
    }

    /// When [`State::advance_app_liveness_at`] next has something to decide.
    pub(crate) fn app_liveness_deadline(&self) -> Option<Instant> {
        self.app_watches.deadline()
    }

    /// The windows whose app is not responding now, by id.
    pub(crate) fn not_responding(&self) -> &[u64] {
        &self.not_responding
    }

    /// The person chose "Wait" on window `window` : its app gets another period to answer. Returns
    /// whether the windows that say "Not responding" changed.
    pub(crate) fn wait_for_window(&mut self, window: u64) -> bool {
        let now = Instant::now();
        if let Some((_, id)) = self.shell_client_of(window) {
            self.app_watches.wait(&id, now);
        }
        self.advance_app_liveness_at(now)
    }

    /// The person chose "Force quit" on window `window` : the client's connection ends, so its
    /// windows go, and the desktop is not left with a window nobody answers for. The process it was
    /// is returned for the session to end: a hung app cannot act on its connection closing.
    pub(crate) fn force_quit_window(&mut self, window: u64) -> Option<u32> {
        let toplevel = self
            .xdg_shell_state
            .toplevel_surfaces()
            .iter()
            .find(|toplevel| window_id_of(toplevel.wl_surface()) == Some(window))?
            .clone();
        let client = toplevel.wl_surface().client()?;
        let pid = client
            .get_data::<ClientState>()
            .map_or(0, |data| data.pid.load(Ordering::Relaxed));
        client.kill(
            &self.display,
            ProtocolError {
                code: 0,
                object_id: toplevel.wl_surface().id().protocol_id(),
                object_interface: "wl_surface".to_owned(),
                message: "the app stopped responding".to_owned(),
            },
        );
        self.app_watches.forget(&client.id());
        self.advance_app_liveness_at(Instant::now());
        (pid != 0).then_some(pid)
    }
}

impl State {
    /// The mapped window of a toplevel.
    pub(crate) fn window_of_toplevel(&self, surface: &ToplevelSurface) -> Option<Window> {
        self.space
            .elements()
            .find(|window| window.toplevel() == Some(surface))
            .cloned()
    }

    /// The window whose surface has keyboard focus.
    pub(crate) fn focused_window(&self) -> Option<WindowInfo> {
        let toplevel = self.focused_element()?.toplevel()?;
        Some(window_info(&self.display, &self.icons, toplevel))
    }

    /// The window, of a Wayland or an X11 client, whose surface the keyboard belongs to.
    pub(crate) fn focused_element(&self) -> Option<&Window> {
        let focus = self.keyboard_focus()?;
        self.space
            .elements()
            .find(|window| surface_of(window).as_ref() == Some(&focus))
    }

    /// The app of the window `surface` belongs to: its toplevel's app id. A surface of no window (a
    /// layer, the lock screen, a popup of neither) has none, and is timed by nothing.
    fn app_of_surface(&self, surface: &WlSurface) -> Option<String> {
        let root = root_surface(surface);
        let toplevel = self
            .space
            .elements()
            .filter_map(Window::toplevel)
            .find(|toplevel| toplevel.wl_surface() == &root)?;
        window_info(&self.display, &self.icons, toplevel).app_id
    }

    /// The key `keycode` reached the focused client: its next commit answers it, and the frame that
    /// follows carries the answer to the display.
    pub(crate) fn key_delivered(&self, surface: &WlSurface, keycode: u32) {
        let root = root_surface(surface);
        let Some(app) = self.app_of_surface(&root) else {
            return;
        };
        self.key_latency.key_delivered(
            u64::from(root.id().protocol_id()),
            &app,
            keycode,
            Instant::now(),
        );
    }

    /// `surface` committed a new buffer. A key waiting on the client is answered; a key the client
    /// never answered is named and dropped.
    pub(crate) fn commit_answered(&self, surface: &WlSurface) {
        let root = root_surface(surface);
        for app in self
            .key_latency
            .surface_committed(u64::from(root.id().protocol_id()), Instant::now())
        {
            eprintln!("KEY-LATENCY app={app} unanswered=1");
        }
    }

    /// The workspace in front changed at `at` ([`State::workspace_switched`]).
    pub(crate) fn workspace_switched(&self) {
        self.switch_latency.chord(Instant::now());
    }

    /// Whether the switch in flight still owes the desk's motion: its frames belong to the switch
    /// until the desk rests.
    pub(crate) fn set_switch_holding(&self, holding: bool) {
        self.switch_holding.store(holding, Ordering::Relaxed);
    }

    /// The serial the next drawn frame takes.
    pub(crate) fn next_frame_serial(&self) -> u64 {
        self.frame_serial
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    /// How many frames the displays presented: what a first paint waits for.
    pub(crate) fn frames_presented(&self) -> u64 {
        self.frames_presented
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// How many frames were drawn, presented or not: a tick that drew none answers no input.
    pub(crate) fn frames_drawn(&self) -> u64 {
        self.frame_serial
            .load(std::sync::atomic::Ordering::Relaxed)
            .saturating_sub(1)
    }

    /// The last frame a display presented, and when: the presenter says so from wherever the
    /// display's completion arrives.
    pub(crate) fn take_retired(&self) -> Option<(u64, Instant)> {
        self.retired
            .lock()
            .ok()
            .and_then(|mut retired| retired.take())
    }

    /// The frame `serial` reached the display: answer the keys it carried,
    /// and close a workspace switch when it was the resting frame.
    pub(crate) fn frame_presented(&self, serial: u64) {
        let at = Instant::now();
        self.frames_presented
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if let Ok(mut retired) = self.retired.lock() {
            *retired = Some((serial, at));
        }
        for sample in self.key_latency.frames_retired(serial, at) {
            eprintln!("{sample}");
        }
        if let Some(line) = self.key_latency.summary(at) {
            eprintln!("{line}");
        }
        if let Some(sample) = self.switch_latency.frames_retired(serial, at) {
            eprintln!("{sample}");
        }
    }
}
