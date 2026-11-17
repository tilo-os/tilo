//! A desktop, a keeper and apps in one process: the headless compositor, the keeper on a
//! temporary directory, and plain Wayland clients on the keeper's socket.

use std::io::{IoSlice, Read as _, Write as _};
use std::mem::MaybeUninit;
use std::os::fd::{AsFd as _, OwnedFd};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use tilo_app_keeper::wire::{self, FromKeeper, ToKeeper};
use tilo_app_keeper::{Keeper, Options};
use tilo_compositor::{Compositor, OutputConfig, StackPolicy};
use wayland_client::backend::ObjectId;
use wayland_client::protocol::{
    wl_buffer::WlBuffer,
    wl_callback::WlCallback,
    wl_compositor::WlCompositor,
    wl_data_device::{self, WlDataDevice},
    wl_data_device_manager::{self, DndAction, WlDataDeviceManager},
    wl_data_offer::{self, WlDataOffer},
    wl_data_source::{self, WlDataSource},
    wl_keyboard,
    wl_keyboard::WlKeyboard,
    wl_output,
    wl_output::WlOutput,
    wl_pointer,
    wl_pointer::WlPointer,
    wl_registry,
    wl_registry::WlRegistry,
    wl_seat,
    wl_seat::WlSeat,
    wl_shm,
    wl_shm::WlShm,
    wl_shm_pool::WlShmPool,
    wl_subcompositor::WlSubcompositor,
    wl_subsurface::WlSubsurface,
    wl_surface::WlSurface,
};
use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy as _, QueueHandle, WEnum, delegate_noop,
    event_created_child,
};
use wayland_protocols::wp::content_type::v1::client::{
    wp_content_type_manager_v1::WpContentTypeManagerV1,
    wp_content_type_v1::{self, WpContentTypeV1},
};
use wayland_protocols::wp::cursor_shape::v1::client::{
    wp_cursor_shape_device_v1::{Shape, WpCursorShapeDeviceV1},
    wp_cursor_shape_manager_v1::WpCursorShapeManagerV1,
};
use wayland_protocols::wp::fractional_scale::v1::client::{
    wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
    wp_fractional_scale_v1::{self, WpFractionalScaleV1},
};
use wayland_protocols::wp::idle_inhibit::zv1::client::{
    zwp_idle_inhibit_manager_v1::ZwpIdleInhibitManagerV1, zwp_idle_inhibitor_v1::ZwpIdleInhibitorV1,
};
use wayland_protocols::wp::keyboard_shortcuts_inhibit::zv1::client::{
    zwp_keyboard_shortcuts_inhibit_manager_v1::ZwpKeyboardShortcutsInhibitManagerV1,
    zwp_keyboard_shortcuts_inhibitor_v1::{self, ZwpKeyboardShortcutsInhibitorV1},
};
use wayland_protocols::wp::linux_dmabuf::zv1::client::{
    zwp_linux_dmabuf_feedback_v1, zwp_linux_dmabuf_feedback_v1::ZwpLinuxDmabufFeedbackV1,
    zwp_linux_dmabuf_v1, zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1,
};
use wayland_protocols::wp::pointer_constraints::zv1::client::{
    zwp_locked_pointer_v1::{self as locked_pointer, ZwpLockedPointerV1},
    zwp_pointer_constraints_v1::{self as constraints_manager, Lifetime, ZwpPointerConstraintsV1},
};
use wayland_protocols::wp::pointer_gestures::zv1::client::{
    zwp_pointer_gesture_pinch_v1::{self as pinch_gesture, ZwpPointerGesturePinchV1},
    zwp_pointer_gesture_swipe_v1::{self as swipe_gesture, ZwpPointerGestureSwipeV1},
    zwp_pointer_gestures_v1::ZwpPointerGesturesV1,
};
use wayland_protocols::wp::presentation_time::client::{
    wp_presentation::{self, WpPresentation},
    wp_presentation_feedback::{self, WpPresentationFeedback},
};
use wayland_protocols::wp::primary_selection::zv1::client::{
    zwp_primary_selection_device_manager_v1::ZwpPrimarySelectionDeviceManagerV1,
    zwp_primary_selection_device_v1::{self, ZwpPrimarySelectionDeviceV1},
    zwp_primary_selection_offer_v1::{self, ZwpPrimarySelectionOfferV1},
    zwp_primary_selection_source_v1::{self, ZwpPrimarySelectionSourceV1},
};
use wayland_protocols::wp::relative_pointer::zv1::client::{
    zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1,
    zwp_relative_pointer_v1::{self as relative_pointer, ZwpRelativePointerV1},
};
use wayland_protocols::wp::tablet::zv2::client::{
    zwp_tablet_manager_v2::ZwpTabletManagerV2,
    zwp_tablet_pad_group_v2::{self, ZwpTabletPadGroupV2},
    zwp_tablet_pad_ring_v2::{self, ZwpTabletPadRingV2},
    zwp_tablet_pad_strip_v2::{self, ZwpTabletPadStripV2},
    zwp_tablet_pad_v2::{self, ZwpTabletPadV2},
    zwp_tablet_seat_v2::{self, ZwpTabletSeatV2},
    zwp_tablet_tool_v2::{self, ZwpTabletToolV2},
    zwp_tablet_v2::{self, ZwpTabletV2},
};
use wayland_protocols::wp::text_input::zv3::client::{
    zwp_text_input_manager_v3::{self as text_manager, ZwpTextInputManagerV3},
    zwp_text_input_v3::{
        self as text_input, ChangeCause, ContentHint, ContentPurpose, ZwpTextInputV3,
    },
};
use wayland_protocols::wp::viewporter::client::{
    wp_viewport::WpViewport, wp_viewporter::WpViewporter,
};
use wayland_protocols::xdg::activation::v1::client::{
    xdg_activation_token_v1::{self, XdgActivationTokenV1},
    xdg_activation_v1::XdgActivationV1,
};
use wayland_protocols::xdg::decoration::zv1::client::{
    zxdg_decoration_manager_v1::ZxdgDecorationManagerV1,
    zxdg_toplevel_decoration_v1::{self, Mode as DecorationMode, ZxdgToplevelDecorationV1},
};
use wayland_protocols::xdg::dialog::v1::client::{
    xdg_dialog_v1::XdgDialogV1, xdg_wm_dialog_v1::XdgWmDialogV1,
};
use wayland_protocols::xdg::shell::client::{
    xdg_popup, xdg_popup::XdgPopup, xdg_positioner::XdgPositioner, xdg_surface,
    xdg_surface::XdgSurface, xdg_toplevel, xdg_toplevel::XdgToplevel, xdg_wm_base,
    xdg_wm_base::XdgWmBase,
};
use wayland_protocols::xdg::toplevel_icon::v1::client::{
    xdg_toplevel_icon_manager_v1::{self as icon_manager, XdgToplevelIconManagerV1},
    xdg_toplevel_icon_v1::XdgToplevelIconV1,
};
use wayland_protocols_misc::server_decoration::client::{
    org_kde_kwin_server_decoration::{self as kde, Mode as KdeMode, OrgKdeKwinServerDecoration},
    org_kde_kwin_server_decoration_manager::OrgKdeKwinServerDecorationManager,
};

pub const OUTPUT: &str = "HEADLESS-1";

/// A desktop of one output.
pub fn desktop(width: i32, height: i32) -> Compositor {
    desktop_at_scale(width, height, 1.0)
}

/// A desktop whose output has `scale`.
pub fn desktop_at_scale(width: i32, height: i32, scale: f64) -> Compositor {
    let mut compositor = Compositor::headless(Box::new(StackPolicy)).expect("compositor");
    compositor
        .add_output(&OutputConfig::new(OUTPUT, width, height).with_scale(scale))
        .expect("output");
    compositor
}

/// A keeper, the desktop it shows on, and its directory.
pub struct Rig {
    pub keeper: Keeper,
    pub desktop: Option<Compositor>,
    pub dir: tempfile::TempDir,
}

impl Rig {
    pub fn new(prefix: Option<&str>) -> Self {
        Self::with_options(prefix, Vec::new())
    }

    pub fn with_options(prefix: Option<&str>, command: Vec<std::ffi::OsString>) -> Self {
        let dir = tempfile::tempdir().expect("temporary directory");
        let mut options = Options::new(dir.path().join("keeper"), "test-launch");
        options.app_id_prefix = prefix.map(str::to_owned);
        options.command = command;
        options.grace = Duration::from_millis(300);
        let keeper = Keeper::new(options).expect("keeper");
        let mut rig = Self {
            keeper,
            desktop: None,
            dir,
        };
        rig.attach_new(640, 480);
        rig
    }

    /// A new desktop, attached as a session would after a restart.
    pub fn attach_new(&mut self, width: i32, height: i32) {
        self.attach_new_at_scale(width, height, 1.0);
    }

    /// A new desktop whose output has `scale`.
    pub fn attach_new_at_scale(&mut self, width: i32, height: i32, scale: f64) {
        let mut compositor = desktop_at_scale(width, height, scale);
        let socket = compositor.connect_client().expect("desktop connection");
        self.keeper.attach(socket).expect("attach");
        self.desktop = Some(compositor);
    }

    /// The desktop goes away entirely, as it does when it restarts.
    pub fn drop_desktop(&mut self) {
        self.desktop = None;
    }

    pub fn desktop(&mut self) -> &mut Compositor {
        self.desktop.as_mut().expect("a desktop")
    }

    /// Let desktop, keeper and apps talk until `done` holds.
    pub fn pump(
        &mut self,
        apps: &mut [&mut App],
        mut done: impl FnMut(&mut Self, &[&mut App]) -> bool,
    ) {
        for _ in 0..1500 {
            self.turn(apps);
            if done(self, apps) {
                return;
            }
        }
        panic!("condition not reached");
    }

    /// A few rounds of talk.
    pub fn settle(&mut self, apps: &mut [&mut App]) {
        for _ in 0..20 {
            self.turn(apps);
        }
    }

    fn turn(&mut self, apps: &mut [&mut App]) {
        for app in apps.iter_mut() {
            app.exchange();
        }
        self.keeper
            .dispatch(Some(Duration::from_millis(1)))
            .expect("keeper");
        if let Some(desktop) = self.desktop.as_mut() {
            desktop.dispatch(Duration::from_millis(1)).expect("desktop");
        }
        self.keeper.dispatch(Some(Duration::ZERO)).expect("keeper");
        for app in apps.iter_mut() {
            app.exchange();
        }
    }

    /// The colour of the desktop's output at `(x, y)`, freshly drawn.
    pub fn pixel(&mut self, x: u32, y: u32) -> [u8; 4] {
        let desktop = self.desktop();
        desktop.render().expect("render");
        let shot = desktop.screenshot(OUTPUT).expect("screenshot");
        shot.pixel(x, y).expect("pixel")
    }

    /// The desktop's windows: app id, title and rectangle.
    pub fn windows(&self) -> Vec<(Option<String>, Option<String>, (i32, i32, i32, i32))> {
        self.desktop
            .as_ref()
            .map(|desktop| {
                desktop
                    .windows()
                    .into_iter()
                    .map(|(info, rect)| (info.app_id, info.title, rect))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The control socket, connected and greeted at the keeper's version.
    pub fn session(&mut self) -> Session {
        let mut session = Session::connect(self);
        session.send(&ToKeeper::Hello {
            version: wire::INTERFACE_VERSION,
        });
        let hello = session.receive(self);
        assert_eq!(
            hello,
            FromKeeper::Hello {
                version: wire::INTERFACE_VERSION
            }
        );
        session
    }
}

/// A session's end of the control socket.
pub struct Session {
    pub stream: UnixStream,
    buffer: Vec<u8>,
    frames: std::collections::VecDeque<Vec<u8>>,
}

impl Session {
    pub fn connect(rig: &Rig) -> Self {
        let path = rig.dir.path().join("keeper").join("control");
        let stream = tilo_files::native::socket::connect(&path).expect("control socket");
        stream.set_nonblocking(true).expect("non-blocking");
        Self {
            stream,
            buffer: Vec::new(),
            frames: std::collections::VecDeque::new(),
        }
    }

    pub fn send(&mut self, message: &ToKeeper) {
        let frame = wire::encode(message).expect("frame");
        self.stream.set_nonblocking(false).expect("blocking");
        self.stream.write_all(&frame).expect("send");
        self.stream.set_nonblocking(true).expect("non-blocking");
    }

    /// Send `Attach` with the desktop connection beside it.
    pub fn attach(&self, socket: &UnixStream) {
        let frame = wire::encode(&ToKeeper::Attach).expect("frame");
        let fds = [socket.as_fd()];
        let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
        let mut control = rustix::net::SendAncillaryBuffer::new(&mut space);
        assert!(control.push(rustix::net::SendAncillaryMessage::ScmRights(&fds)));
        let sent = rustix::net::sendmsg(
            &self.stream,
            &[IoSlice::new(&frame)],
            &mut control,
            rustix::net::SendFlags::empty(),
        )
        .expect("sendmsg");
        assert_eq!(sent, frame.len());
    }

    /// The next message, pumping the rig until it arrives; `None` once the keeper closed.
    pub fn next(&mut self, rig: &mut Rig) -> Option<FromKeeper> {
        for _ in 0..1500 {
            self.frames
                .extend(wire::take_frames(&mut self.buffer).expect("frames"));
            if let Some(frame) = self.frames.pop_front() {
                return Some(serde_json::from_slice(&frame).expect("message"));
            }
            let mut bytes = [0_u8; 4096];
            match self.stream.read(&mut bytes) {
                Ok(0) => return None,
                Ok(count) => self.buffer.extend(&bytes[..count]),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => rig.settle(&mut []),
                Err(_) => return None,
            }
        }
        panic!("no message from the keeper");
    }

    pub fn receive(&mut self, rig: &mut Rig) -> FromKeeper {
        self.next(rig).expect("a message")
    }

    /// The next message `want` picks, skipping the rest.
    pub fn wait_for<T>(
        &mut self,
        rig: &mut Rig,
        mut want: impl FnMut(FromKeeper) -> Option<T>,
    ) -> T {
        loop {
            if let Some(found) = want(self.receive(rig)) {
                return found;
            }
        }
    }
}

/// What a test app heard.
#[derive(Default)]
pub struct Heard {
    pub compositor: Option<WlCompositor>,
    pub subcompositor: Option<WlSubcompositor>,
    pub shm: Option<WlShm>,
    pub wm_base: Option<XdgWmBase>,
    pub seat: Option<WlSeat>,
    pub keyboard: Option<WlKeyboard>,
    pub pointer: Option<WlPointer>,
    pub outputs: Vec<WlOutput>,
    pub output_modes: Vec<(u32, i32, i32)>,
    pub globals_removed: Vec<u32>,
    pub dmabuf: Option<ZwpLinuxDmabufV1>,
    pub viewporter: Option<WpViewporter>,
    pub fractional_scale: Option<WpFractionalScaleManagerV1>,
    pub preferred_scales: Vec<u32>,
    pub content_type: Option<WpContentTypeManagerV1>,
    pub decoration: Option<ZxdgDecorationManagerV1>,
    pub decoration_modes: Vec<DecorationMode>,
    pub kde_decoration: Option<OrgKdeKwinServerDecorationManager>,
    pub kde_modes: Vec<KdeMode>,
    pub activation: Option<XdgActivationV1>,
    pub activation_tokens: Vec<String>,
    pub presentation: Option<WpPresentation>,
    pub clock_ids: Vec<u32>,
    pub presented: Vec<Presented>,
    pub presentation_discarded: usize,
    pub sync_outputs: usize,
    pub cursor_shape: Option<WpCursorShapeManagerV1>,
    pub pointer_enter_serial: Option<u32>,
    pub idle_inhibit: Option<ZwpIdleInhibitManagerV1>,
    pub shortcuts_inhibit: Option<ZwpKeyboardShortcutsInhibitManagerV1>,
    pub shortcuts_inhibited: bool,
    pub dialog_manager: Option<XdgWmDialogV1>,
    pub toplevel_icon: Option<XdgToplevelIconManagerV1>,
    pub icon_sizes: Vec<i32>,
    pub dmabuf_done: usize,
    pub surface_feedback_done: usize,
    pub configures: Vec<(i32, i32, Vec<u32>)>,
    pub configured: usize,
    pub popup_configured: bool,
    pub popup_done: bool,
    pub frames_done: usize,
    pub closed: bool,
    pub keys: Vec<(u32, u32, bool)>,
    pub buttons: Vec<(u32, u32, bool)>,
    pub data_device_manager: Option<WlDataDeviceManager>,
    pub data_device: Option<WlDataDevice>,
    pub primary_manager: Option<ZwpPrimarySelectionDeviceManagerV1>,
    pub primary_device: Option<ZwpPrimarySelectionDeviceV1>,
    /// The clipboard offer the app holds now, and the formats it offered.
    pub selection: Option<WlDataOffer>,
    pub offer_mimes: Vec<(WlDataOffer, Vec<String>)>,
    pub primary_selection: Option<ZwpPrimarySelectionOfferV1>,
    pub primary_mimes: Vec<(ZwpPrimarySelectionOfferV1, Vec<String>)>,
    /// The drag offer the app holds, and where the drag is.
    pub drag_offer: Option<WlDataOffer>,
    pub drag_entered: bool,
    pub drag_left: bool,
    pub drag_motion: usize,
    pub drops: usize,
    /// What the app's own sources hold, and what became of them.
    pub sources: Vec<(WlDataSource, Vec<u8>)>,
    pub sent: Vec<String>,
    pub source_cancelled: bool,
    pub source_action: Option<DndAction>,
    pub dnd_finished: bool,
    pub primary_sources: Vec<(ZwpPrimarySelectionSourceV1, Vec<u8>)>,
    pub primary_sent: Vec<String>,
    pub primary_cancelled: bool,
    pub text_input_manager: Option<ZwpTextInputManagerV3>,
    pub text_input: Option<ZwpTextInputV3>,
    pub text_entered: bool,
    pub text_left: bool,
    pub commits: Vec<String>,
    pub preedits: Vec<String>,
    pub deletes: Vec<(u32, u32)>,
    pub text_dones: Vec<u32>,
    pub constraints: Option<ZwpPointerConstraintsV1>,
    pub locked_pointer: Option<ZwpLockedPointerV1>,
    pub locked: bool,
    pub unlocked: bool,
    pub gestures: Option<ZwpPointerGesturesV1>,
    pub swipe_gesture: Option<ZwpPointerGestureSwipeV1>,
    pub pinch_gesture: Option<ZwpPointerGesturePinchV1>,
    pub swipes: Vec<(u32, u32)>,
    pub swipe_updates: Vec<(f64, f64)>,
    pub swipe_ends: Vec<i32>,
    pub pinches: Vec<(u32, u32)>,
    pub pinch_ends: Vec<i32>,
    pub pinch_updates: Vec<(f64, f64, f64, f64)>,
    pub relative: Option<ZwpRelativePointerManagerV1>,
    pub relative_pointer: Option<ZwpRelativePointerV1>,
    pub relative_motion: Vec<(f64, f64)>,
    pub tablet_manager: Option<ZwpTabletManagerV2>,
    pub tablet_manager_version: u32,
    pub tablet_seat: Option<ZwpTabletSeatV2>,
    pub tablets: Vec<TabletSeen>,
    pub tools: Vec<ToolSeen>,
    pub pads: Vec<PadSeen>,
    pub tool: Option<ZwpTabletToolV2>,
    pub pad: Option<ZwpTabletPadV2>,
    pub ring: Option<ZwpTabletPadRingV2>,
    pub strip: Option<ZwpTabletPadStripV2>,
    pub pad_group_buttons: Vec<Vec<u8>>,
    pub pad_modes: Vec<(u32, u32)>,
    pub pad_group_done: usize,
    pub pad_group_modes: u32,
    pub ring_angles: Vec<f64>,
    pub ring_stops: usize,
    pub ring_frames: usize,
    pub strip_positions: Vec<u32>,
    pub strip_stops: usize,
    pub strip_frames: usize,
    tablet_ids: Vec<ObjectId>,
    tool_ids: Vec<ObjectId>,
    pad_ids: Vec<ObjectId>,
    pub last_serial: u32,
    pub enter_serial: u32,
    pub keyboard_entered: bool,
    pub pointer_entered: bool,
    pub buffers_released: usize,
    /// Whether the app answers a ping when it reads one.
    pub answers_pings: bool,
    /// The serials of the pings it read.
    pub pings: Vec<u32>,
    pending_size: Option<(i32, i32, Vec<u32>)>,
}

/// What a tablet, a tool and a pad told the app.
#[derive(Clone, Debug, Default)]
pub struct TabletSeen {
    pub name: Option<String>,
    pub id: Option<(u32, u32)>,
    pub path: Option<String>,
    pub done: bool,
    pub removed: bool,
}

#[derive(Clone, Debug, Default)]
pub struct ToolSeen {
    pub tool_type: Option<u32>,
    pub hardware_serial: Option<(u32, u32)>,
    pub hardware_id: Option<(u32, u32)>,
    pub capabilities: Vec<u32>,
    pub done: bool,
    pub removed: bool,
    pub proximity_in: Vec<(u32, WlSurface)>,
    pub proximity_out: usize,
    pub down: Vec<u32>,
    pub up: usize,
    pub motion: Vec<(f64, f64)>,
    pub pressure: Vec<u32>,
    pub distance: Vec<u32>,
    pub tilt: Vec<(f64, f64)>,
    pub rotation: Vec<f64>,
    pub slider: Vec<i32>,
    pub wheel: Vec<(f64, i32)>,
    pub buttons: Vec<(u32, u32, bool)>,
    pub frames: Vec<u32>,
}

#[derive(Clone, Debug, Default)]
pub struct PadSeen {
    pub groups: usize,
    pub path: Option<String>,
    pub buttons: Option<u32>,
    pub done: bool,
    pub removed: bool,
    pub entered: Option<(u32, WlSurface)>,
    pub left: Option<(u32, WlSurface)>,
    pub buttons_pressed: Vec<(u32, u32, bool)>,
}

fn index_of(ids: &[ObjectId], id: &ObjectId) -> Option<usize> {
    ids.iter().position(|known| known == id)
}

/// A Wayland app on the keeper's socket.
pub struct App {
    pub connection: Connection,
    pub queue: EventQueue<Heard>,
    pub heard: Heard,
    pub surface: Option<WlSurface>,
    pub xdg: Option<XdgSurface>,
    pub toplevel: Option<XdgToplevel>,
    pub broken: bool,
}

impl App {
    pub fn connect(rig: &mut Rig) -> Self {
        let stream =
            tilo_files::native::socket::connect(rig.keeper.socket()).expect("keeper socket");
        let connection = Connection::from_socket(stream).expect("connection");
        let queue = connection.new_event_queue();
        connection.display().get_registry(&queue.handle(), ());
        let heard = Heard {
            answers_pings: true,
            ..Heard::default()
        };
        let mut app = Self {
            connection,
            queue,
            heard,
            surface: None,
            xdg: None,
            toplevel: None,
            broken: false,
        };
        rig.pump(&mut [&mut app], |_, apps| {
            let heard = &apps[0].heard;
            heard.wm_base.is_some()
                && heard.seat.is_some()
                && heard.tablet_seat.is_some()
                && !heard.outputs.is_empty()
        });
        app
    }

    /// Send what is queued, read what arrived, without blocking.
    pub fn exchange(&mut self) {
        if self.broken {
            return;
        }
        let mut ok = self.connection.flush().is_ok();
        if let Some(guard) = self.connection.prepare_read() {
            ok &= match guard.read() {
                Ok(_) => true,
                Err(wayland_client::backend::WaylandError::Io(error)) => {
                    error.kind() == std::io::ErrorKind::WouldBlock
                }
                Err(_) => false,
            };
        }
        ok &= self.queue.dispatch_pending(&mut self.heard).is_ok();
        ok &= self.connection.flush().is_ok();
        self.broken = !ok;
    }

    fn qh(&self) -> QueueHandle<Heard> {
        self.queue.handle()
    }

    /// Open a window and wait for its first configure.
    pub fn open_window(&mut self, rig: &mut Rig, title: &str, app_id: Option<&str>) {
        let qh = self.qh();
        let surface = self
            .heard
            .compositor
            .as_ref()
            .unwrap()
            .create_surface(&qh, ());
        let xdg = self
            .heard
            .wm_base
            .as_ref()
            .unwrap()
            .get_xdg_surface(&surface, &qh, ());
        let toplevel = xdg.get_toplevel(&qh, ());
        toplevel.set_title(title.to_owned());
        if let Some(app_id) = app_id {
            toplevel.set_app_id(app_id.to_owned());
        }
        surface.commit();
        self.surface = Some(surface);
        self.xdg = Some(xdg);
        self.toplevel = Some(toplevel);
        rig.pump(&mut [self], |_, apps| apps[0].heard.configured > 0);
    }

    /// Show `rgba` on the window at `size`, and wait until the desktop shows it.
    pub fn show(&mut self, rig: &mut Rig, size: (i32, i32), rgba: [u8; 4]) {
        let surface = self.surface.clone().unwrap();
        self.draw(&surface, size, rgba);
        rig.pump(&mut [self], |rig, _| !rig.windows().is_empty());
    }

    /// Attach a fresh buffer of `rgba` to `surface` and commit.
    pub fn draw(&self, surface: &WlSurface, size: (i32, i32), rgba: [u8; 4]) -> WlBuffer {
        let buffer = self.buffer(size, rgba);
        surface.attach(Some(&buffer), 0, 0);
        surface.damage_buffer(0, 0, size.0, size.1);
        surface.commit();
        buffer
    }

    /// A shared-memory buffer of `rgba`, in its own pool.
    pub fn buffer(&self, (width, height): (i32, i32), [r, g, b, a]: [u8; 4]) -> WlBuffer {
        let qh = self.qh();
        let stride = width * 4;
        let size = stride * height;
        let fd: OwnedFd = rustix::fs::memfd_create("app", rustix::fs::MemfdFlags::CLOEXEC).unwrap();
        let pixels: Vec<u8> = std::iter::repeat_n([b, g, r, a], (width * height) as usize)
            .flatten()
            .collect();
        let mut file = std::fs::File::from(fd);
        file.write_all(&pixels).unwrap();
        let pool: WlShmPool =
            self.heard
                .shm
                .as_ref()
                .unwrap()
                .create_pool(file.as_fd(), size, &qh, ());
        let buffer =
            pool.create_buffer(0, width, height, stride, wl_shm::Format::Argb8888, &qh, ());
        pool.destroy();
        buffer
    }

    /// Answer the last ping the app read, as a client does when it gets to its event loop.
    pub fn pong(&self) {
        if let (Some(wm_base), Some(serial)) = (&self.heard.wm_base, self.heard.pings.last()) {
            wm_base.pong(*serial);
        }
    }

    /// A viewport on the window surface, with `src` and `dst` as the app asks for them.
    pub fn viewport(
        &self,
        src: Option<(f64, f64, f64, f64)>,
        dst: Option<(i32, i32)>,
    ) -> WpViewport {
        let qh = self.qh();
        let surface = self.surface.clone().unwrap();
        let viewport: WpViewport = self
            .heard
            .viewporter
            .as_ref()
            .expect("a viewporter")
            .get_viewport(&surface, &qh, ());
        match src {
            Some((x, y, w, h)) => viewport.set_source(x, y, w, h),
            None => viewport.set_source(0.0, 0.0, -1.0, -1.0),
        }
        match dst {
            Some((w, h)) => viewport.set_destination(w, h),
            None => viewport.set_destination(-1, -1),
        }
        surface.commit();
        viewport
    }

    /// Ask the desktop for this window's preferred scale.
    pub fn request_fractional_scale(&self) -> WpFractionalScaleV1 {
        let qh = self.qh();
        self.heard
            .fractional_scale
            .as_ref()
            .expect("a fractional scale manager")
            .get_fractional_scale(self.surface.as_ref().unwrap(), &qh, ())
    }

    /// Tell the desktop what kind of content the window shows.
    pub fn content_type(&self, kind: wp_content_type_v1::Type) -> WpContentTypeV1 {
        let qh = self.qh();
        let surface = self.surface.clone().unwrap();
        let object: WpContentTypeV1 = self
            .heard
            .content_type
            .as_ref()
            .expect("a content type manager")
            .get_surface_content_type(&surface, &qh, ());
        object.set_content_type(kind);
        surface.commit();
        object
    }

    /// Ask to be decorated, preferring `mode`.
    pub fn decoration(&self, mode: DecorationMode) -> ZxdgToplevelDecorationV1 {
        let qh = self.qh();
        let decoration = self
            .heard
            .decoration
            .as_ref()
            .expect("an xdg decoration manager")
            .get_toplevel_decoration(self.toplevel.as_ref().unwrap(), &qh, ());
        decoration.set_mode(mode);
        self.surface.as_ref().unwrap().commit();
        decoration
    }

    /// Ask to be decorated through the KDE spelling of the same question.
    pub fn kde_decoration(&self, mode: KdeMode) -> OrgKdeKwinServerDecoration {
        let qh = self.qh();
        let decoration = self
            .heard
            .kde_decoration
            .as_ref()
            .expect("a KDE decoration manager")
            .create(self.surface.as_ref().unwrap(), &qh, ());
        decoration.request_mode(mode);
        self.surface.as_ref().unwrap().commit();
        decoration
    }

    /// Ask the keeper for an activation token for this window, and wait for the desktop's answer.
    pub fn activation_token(&mut self, rig: &mut Rig) -> String {
        let qh = self.qh();
        let token = self
            .heard
            .activation
            .as_ref()
            .expect("an activation manager")
            .get_activation_token(&qh, ());
        if let Some(surface) = &self.surface {
            token.set_surface(surface);
        }
        token.commit();
        let seen = self.heard.activation_tokens.len();
        rig.pump(&mut [self], |_, apps| {
            apps[0].heard.activation_tokens.len() > seen
        });
        self.heard
            .activation_tokens
            .last()
            .cloned()
            .expect("a token")
    }

    /// Ask to be activated with `token` on the window.
    pub fn activate(&self, token: String) {
        let (Some(activation), Some(surface)) = (&self.heard.activation, &self.surface) else {
            panic!("an activation manager and a window");
        };
        activation.activate(token, surface);
    }

    /// Ask for presentation feedback on the window's next commit.
    pub fn request_presentation(&self) -> WpPresentationFeedback {
        let qh = self.qh();
        self.heard
            .presentation
            .as_ref()
            .expect("a presentation object")
            .feedback(self.surface.as_ref().unwrap(), &qh, FeedbackSeen)
    }

    /// Name the cursor shape the desktop should draw for this app's tablet tool.
    pub fn set_tool_cursor_shape(&self, tool: &ZwpTabletToolV2, serial: u32, shape: Shape) {
        let qh = self.qh();
        let device = self
            .heard
            .cursor_shape
            .as_ref()
            .expect("a cursor shape manager")
            .get_tablet_tool_v2(tool, &qh, ());
        device.set_shape(serial, shape);
    }

    /// Name the cursor shape the desktop should draw for this app's pointer.
    pub fn set_cursor_shape(&self, shape: Shape) {
        let qh = self.qh();
        let device = self
            .heard
            .cursor_shape
            .as_ref()
            .expect("a cursor shape manager")
            .get_pointer(self.heard.pointer.as_ref().expect("a pointer"), &qh, ());
        device.set_shape(
            self.heard
                .pointer_enter_serial
                .expect("the pointer entered"),
            shape,
        );
    }

    /// Ask the desktop not to idle while this window shows.
    pub fn inhibit_idle(&self) -> ZwpIdleInhibitorV1 {
        let qh = self.qh();
        self.heard
            .idle_inhibit
            .as_ref()
            .expect("an idle inhibit manager")
            .create_inhibitor(self.surface.as_ref().unwrap(), &qh, ())
    }

    /// Ask the desktop to stop acting on its own shortcuts while this window has the keys.
    pub fn inhibit_shortcuts(&self) -> ZwpKeyboardShortcutsInhibitorV1 {
        let qh = self.qh();
        self.heard
            .shortcuts_inhibit
            .as_ref()
            .expect("a shortcuts inhibit manager")
            .inhibit_shortcuts(
                self.surface.as_ref().unwrap(),
                self.heard.seat.as_ref().expect("a seat"),
                &qh,
                (),
            )
    }

    /// Open a second window of this app in front of `parent`, as a dialog would be.
    pub fn open_child(
        &mut self,
        rig: &mut Rig,
        title: &str,
        app_id: Option<&str>,
        parent: &XdgToplevel,
    ) {
        let qh = self.qh();
        let surface = self
            .heard
            .compositor
            .as_ref()
            .unwrap()
            .create_surface(&qh, ());
        let xdg = self
            .heard
            .wm_base
            .as_ref()
            .unwrap()
            .get_xdg_surface(&surface, &qh, ());
        let toplevel = xdg.get_toplevel(&qh, ());
        toplevel.set_title(title.to_owned());
        if let Some(app_id) = app_id {
            toplevel.set_app_id(app_id.to_owned());
        }
        toplevel.set_parent(Some(parent));
        surface.commit();
        self.surface = Some(surface);
        self.xdg = Some(xdg);
        self.toplevel = Some(toplevel);
        let configured = self.heard.configured;
        rig.pump(&mut [self], |_, apps| apps[0].heard.configured > configured);
    }

    /// Mark this window a modal dialog.
    pub fn dialog(&self) -> XdgDialogV1 {
        let qh = self.qh();
        self.heard
            .dialog_manager
            .as_ref()
            .expect("a dialog manager")
            .get_xdg_dialog(self.toplevel.as_ref().expect("a toplevel"), &qh, ())
    }

    /// Ask to be activated with `token` on `surface`.
    pub fn activate_on(&self, token: String, surface: &WlSurface) {
        self.heard
            .activation
            .as_ref()
            .expect("an activation manager")
            .activate(token, surface);
    }

    /// Give the window an icon of `rgba` pixels at `size` by `size`.
    pub fn set_window_icon(
        &self,
        size: (i32, i32),
        rgba: [u8; 4],
    ) -> (XdgToplevelIconV1, WlBuffer) {
        let qh = self.qh();
        let manager = self
            .heard
            .toplevel_icon
            .as_ref()
            .expect("a toplevel icon manager");
        let icon = manager.create_icon(&qh, ());
        let buffer = self.buffer(size, rgba);
        icon.add_buffer(&buffer, 1);
        manager.set_icon(self.toplevel.as_ref().expect("a toplevel"), Some(&icon));
        self.surface.as_ref().unwrap().commit();
        (icon, buffer)
    }

    /// Ask for the next frame callback on the window.
    pub fn request_frame(&self) -> WlCallback {
        let qh = self.qh();
        self.surface.as_ref().unwrap().frame(&qh, ())
    }

    /// A subsurface of the window at `(x, y)`, showing `rgba`.
    pub fn subsurface(
        &self,
        position: (i32, i32),
        size: (i32, i32),
        rgba: [u8; 4],
    ) -> (WlSurface, WlSubsurface) {
        let qh = self.qh();
        let surface = self
            .heard
            .compositor
            .as_ref()
            .unwrap()
            .create_surface(&qh, ());
        let parent = self.surface.clone().unwrap();
        let sub =
            self.heard
                .subcompositor
                .as_ref()
                .unwrap()
                .get_subsurface(&surface, &parent, &qh, ());
        sub.set_position(position.0, position.1);
        self.draw(&surface, size, rgba);
        parent.commit();
        (surface, sub)
    }

    /// A popup of the window, configured and drawn.
    pub fn popup(&mut self, rig: &mut Rig) -> (WlSurface, XdgPopup) {
        let qh = self.qh();
        let surface = self
            .heard
            .compositor
            .as_ref()
            .unwrap()
            .create_surface(&qh, ());
        let wm_base = self.heard.wm_base.clone().unwrap();
        let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
        let positioner: XdgPositioner = wm_base.create_positioner(&qh, ());
        positioner.set_size(40, 30);
        positioner.set_anchor_rect(0, 0, 10, 10);
        let popup = xdg.get_popup(self.xdg.as_ref(), &positioner, &qh, ());
        positioner.destroy();
        surface.commit();
        rig.pump(&mut [self], |_, apps| apps[0].heard.popup_configured);
        self.draw(&surface, (40, 30), [0, 0, 255, 255]);
        (surface, popup)
    }
}

impl Dispatch<WlRegistry, ()> for Heard {
    #[allow(
        clippy::too_many_lines,
        reason = "one arm per global the harness binds, in the registry's own order"
    )]
    fn event(
        heard: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        (): &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        match event {
            wl_registry::Event::Global {
                name,
                interface,
                version,
            } => match interface.as_str() {
                "wl_compositor" => {
                    heard.compositor = Some(registry.bind(name, version.min(6), qh, ()));
                }
                "wl_subcompositor" => heard.subcompositor = Some(registry.bind(name, 1, qh, ())),
                "wl_shm" => heard.shm = Some(registry.bind(name, 1, qh, ())),
                "xdg_wm_base" => heard.wm_base = Some(registry.bind(name, version.min(6), qh, ())),
                "wl_seat" => {
                    let seat: WlSeat = registry.bind(name, version.min(9), qh, ());
                    heard.seat = Some(seat);
                    if let Some(manager) = &heard.data_device_manager {
                        heard.data_device =
                            Some(manager.get_data_device(&heard.seat.clone().unwrap(), qh, ()));
                    }
                    if let Some(manager) = &heard.primary_manager {
                        heard.primary_device =
                            Some(manager.get_device(&heard.seat.clone().unwrap(), qh, ()));
                    }
                    if let Some(manager) = &heard.text_input_manager {
                        heard.text_input =
                            Some(manager.get_text_input(&heard.seat.clone().unwrap(), qh, ()));
                    }
                    heard.take_tablet_seat(qh);
                }
                "zwp_tablet_manager_v2" if heard.tablet_manager.is_none() => {
                    heard.tablet_manager_version = version;
                    heard.tablet_manager = Some(registry.bind(name, version.min(1), qh, ()));
                    heard.take_tablet_seat(qh);
                }
                "wl_data_device_manager" => {
                    let manager: WlDataDeviceManager = registry.bind(name, version.min(3), qh, ());
                    if let Some(seat) = &heard.seat {
                        heard.data_device = Some(manager.get_data_device(seat, qh, ()));
                    }
                    heard.data_device_manager = Some(manager);
                }
                "zwp_pointer_constraints_v1" => {
                    heard.constraints = Some(registry.bind(name, version.min(1), qh, ()));
                }
                "zwp_pointer_gestures_v1" => {
                    heard.gestures = Some(registry.bind(name, version.min(3), qh, ()));
                }
                "zwp_relative_pointer_manager_v1" => {
                    heard.relative = Some(registry.bind(name, 1, qh, ()));
                }
                "zwp_text_input_manager_v3" => {
                    let manager: ZwpTextInputManagerV3 = registry.bind(name, 1, qh, ());
                    if let Some(seat) = &heard.seat {
                        heard.text_input = Some(manager.get_text_input(seat, qh, ()));
                    }
                    heard.text_input_manager = Some(manager);
                }
                "zwp_primary_selection_device_manager_v1" => {
                    let manager: ZwpPrimarySelectionDeviceManagerV1 =
                        registry.bind(name, 1, qh, ());
                    if let Some(seat) = &heard.seat {
                        heard.primary_device = Some(manager.get_device(seat, qh, ()));
                    }
                    heard.primary_manager = Some(manager);
                }
                "wl_output" => heard
                    .outputs
                    .push(registry.bind(name, version.min(4), qh, name)),
                "wp_viewporter" => {
                    heard.viewporter = Some(registry.bind(name, 1, qh, ()));
                }
                "wp_fractional_scale_manager_v1" => {
                    heard.fractional_scale = Some(registry.bind(name, 1, qh, ()));
                }
                "wp_content_type_manager_v1" => {
                    heard.content_type = Some(registry.bind(name, 1, qh, ()));
                }
                "zxdg_decoration_manager_v1" => {
                    heard.decoration = Some(registry.bind(name, 1, qh, ()));
                }
                "org_kde_kwin_server_decoration_manager" => {
                    heard.kde_decoration = Some(registry.bind(name, 1, qh, ()));
                }
                "xdg_activation_v1" => {
                    heard.activation = Some(registry.bind(name, 1, qh, ()));
                }
                "wp_presentation" => {
                    heard.presentation = Some(registry.bind(name, version.min(2), qh, ()));
                }
                "wp_cursor_shape_manager_v1" => {
                    heard.cursor_shape = Some(registry.bind(name, version.min(2), qh, ()));
                }
                "zwp_idle_inhibit_manager_v1" => {
                    heard.idle_inhibit = Some(registry.bind(name, 1, qh, ()));
                }
                "zwp_keyboard_shortcuts_inhibit_manager_v1" => {
                    heard.shortcuts_inhibit = Some(registry.bind(name, 1, qh, ()));
                }
                "xdg_wm_dialog_v1" => {
                    heard.dialog_manager = Some(registry.bind(name, 1, qh, ()));
                }
                "xdg_toplevel_icon_manager_v1" => {
                    heard.toplevel_icon = Some(registry.bind(name, 1, qh, ()));
                }
                "zwp_linux_dmabuf_v1" if version >= 4 => {
                    let dmabuf: ZwpLinuxDmabufV1 = registry.bind(name, version.min(5), qh, ());
                    dmabuf.get_default_feedback(qh, ());
                    heard.dmabuf = Some(dmabuf);
                }
                _ => {}
            },
            wl_registry::Event::GlobalRemove { name } => heard.globals_removed.push(name),
            _ => {}
        }
    }
}

impl Dispatch<WlSeat, ()> for Heard {
    fn event(
        heard: &mut Self,
        seat: &WlSeat,
        event: wl_seat::Event,
        (): &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(capabilities),
        } = event
        {
            if capabilities.contains(wl_seat::Capability::Keyboard) && heard.keyboard.is_none() {
                heard.keyboard = Some(seat.get_keyboard(qh, ()));
            }
            if capabilities.contains(wl_seat::Capability::Pointer) && heard.pointer.is_none() {
                heard.pointer = Some(seat.get_pointer(qh, ()));
            }
            if heard.data_device.is_none()
                && let Some(manager) = &heard.data_device_manager
            {
                heard.data_device = Some(manager.get_data_device(seat, qh, ()));
            }
            if heard.primary_device.is_none()
                && let Some(manager) = &heard.primary_manager
            {
                heard.primary_device = Some(manager.get_device(seat, qh, ()));
            }
            if heard.text_input.is_none()
                && let Some(manager) = &heard.text_input_manager
            {
                heard.text_input = Some(manager.get_text_input(seat, qh, ()));
            }
        }
    }
}

impl Dispatch<WlKeyboard, ()> for Heard {
    fn event(
        heard: &mut Self,
        _: &WlKeyboard,
        event: wl_keyboard::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_keyboard::Event::Enter { serial, .. } => {
                heard.keyboard_entered = true;
                heard.last_serial = serial;
                heard.enter_serial = serial;
            }
            wl_keyboard::Event::Leave { .. } => heard.keyboard_entered = false,
            wl_keyboard::Event::Key {
                serial,
                key,
                state: WEnum::Value(state),
                ..
            } => heard
                .keys
                .push((serial, key, state == wl_keyboard::KeyState::Pressed)),
            _ => {}
        }
    }
}

impl Dispatch<WlPointer, ()> for Heard {
    fn event(
        heard: &mut Self,
        _: &WlPointer,
        event: wl_pointer::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_pointer::Event::Enter { serial, .. } => {
                heard.pointer_entered = true;
                heard.pointer_enter_serial = Some(serial);
            }
            wl_pointer::Event::Leave { .. } => heard.pointer_entered = false,
            wl_pointer::Event::Button {
                serial,
                button,
                state: WEnum::Value(state),
                ..
            } => {
                if state == wl_pointer::ButtonState::Pressed {
                    heard.last_serial = serial;
                }
                heard
                    .buttons
                    .push((serial, button, state == wl_pointer::ButtonState::Pressed));
            }
            _ => {}
        }
    }
}

impl Dispatch<WlOutput, u32> for Heard {
    fn event(
        heard: &mut Self,
        _: &WlOutput,
        event: wl_output::Event,
        name: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_output::Event::Mode { width, height, .. } = event {
            heard.output_modes.push((*name, width, height));
        }
    }
}

impl Dispatch<XdgWmBase, ()> for Heard {
    fn event(
        heard: &mut Self,
        wm_base: &XdgWmBase,
        event: xdg_wm_base::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            heard.pings.push(serial);
            if heard.answers_pings {
                wm_base.pong(serial);
            }
        }
    }
}

impl Dispatch<XdgSurface, ()> for Heard {
    fn event(
        heard: &mut Self,
        xdg: &XdgSurface,
        event: xdg_surface::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg.ack_configure(serial);
            if let Some(size) = heard.pending_size.take() {
                heard.configures.push(size);
                heard.configured += 1;
            } else {
                heard.popup_configured = true;
            }
        }
    }
}

impl Dispatch<XdgToplevel, ()> for Heard {
    fn event(
        heard: &mut Self,
        _: &XdgToplevel,
        event: xdg_toplevel::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            xdg_toplevel::Event::Configure {
                width,
                height,
                states,
            } => {
                let states = states
                    .chunks_exact(4)
                    .map(|chunk| u32::from_ne_bytes(chunk.try_into().unwrap()))
                    .collect();
                heard.pending_size = Some((width, height, states));
            }
            xdg_toplevel::Event::Close => heard.closed = true,
            _ => {}
        }
    }
}

impl Dispatch<XdgPopup, ()> for Heard {
    fn event(
        heard: &mut Self,
        _: &XdgPopup,
        event: xdg_popup::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if matches!(event, xdg_popup::Event::PopupDone) {
            heard.popup_done = true;
        }
    }
}

impl Dispatch<WlCallback, ()> for Heard {
    fn event(
        heard: &mut Self,
        _: &WlCallback,
        _: wayland_client::protocol::wl_callback::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        heard.frames_done += 1;
    }
}

impl Dispatch<WlBuffer, ()> for Heard {
    fn event(
        heard: &mut Self,
        _: &WlBuffer,
        _: wayland_client::protocol::wl_buffer::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        heard.buffers_released += 1;
    }
}

/// The user data marking a surface's own dma-buf feedback request.
pub struct SurfaceFeedbackSeen;

impl Dispatch<ZwpLinuxDmabufV1, ()> for Heard {
    fn event(
        _: &mut Self,
        _: &ZwpLinuxDmabufV1,
        _: zwp_linux_dmabuf_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpLinuxDmabufFeedbackV1, ()> for Heard {
    fn event(
        heard: &mut Self,
        _: &ZwpLinuxDmabufFeedbackV1,
        event: zwp_linux_dmabuf_feedback_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if matches!(event, zwp_linux_dmabuf_feedback_v1::Event::Done) {
            heard.dmabuf_done = heard.dmabuf_done.saturating_add(1);
        }
    }
}

impl Dispatch<ZwpLinuxDmabufFeedbackV1, SurfaceFeedbackSeen> for Heard {
    fn event(
        heard: &mut Self,
        _: &ZwpLinuxDmabufFeedbackV1,
        event: zwp_linux_dmabuf_feedback_v1::Event,
        _: &SurfaceFeedbackSeen,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if matches!(event, zwp_linux_dmabuf_feedback_v1::Event::Done) {
            heard.surface_feedback_done = heard.surface_feedback_done.saturating_add(1);
        }
    }
}

impl Dispatch<WpFractionalScaleV1, ()> for Heard {
    fn event(
        heard: &mut Self,
        _: &WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            heard.preferred_scales.push(scale);
        }
    }
}

delegate_noop!(Heard: ignore WpViewporter);
delegate_noop!(Heard: ignore WpViewport);
delegate_noop!(Heard: ignore WpFractionalScaleManagerV1);
delegate_noop!(Heard: ignore WpContentTypeManagerV1);
delegate_noop!(Heard: ignore WpContentTypeV1);
delegate_noop!(Heard: ignore WlCompositor);
delegate_noop!(Heard: ignore WlSubcompositor);
delegate_noop!(Heard: ignore WlSubsurface);
delegate_noop!(Heard: ignore WlShm);
delegate_noop!(Heard: ignore WlShmPool);
delegate_noop!(Heard: ignore WlSurface);
delegate_noop!(Heard: ignore XdgPositioner);

impl Dispatch<ZxdgToplevelDecorationV1, ()> for Heard {
    fn event(
        heard: &mut Self,
        _: &ZxdgToplevelDecorationV1,
        event: zxdg_toplevel_decoration_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zxdg_toplevel_decoration_v1::Event::Configure {
            mode: WEnum::Value(mode),
        } = event
        {
            heard.decoration_modes.push(mode);
        }
    }
}

impl Dispatch<OrgKdeKwinServerDecoration, ()> for Heard {
    fn event(
        heard: &mut Self,
        _: &OrgKdeKwinServerDecoration,
        event: kde::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let kde::Event::Mode {
            mode: WEnum::Value(mode),
        } = event
        {
            heard.kde_modes.push(mode);
        }
    }
}

impl Dispatch<XdgActivationTokenV1, ()> for Heard {
    fn event(
        heard: &mut Self,
        _: &XdgActivationTokenV1,
        event: xdg_activation_token_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_activation_token_v1::Event::Done { token } = event {
            heard.activation_tokens.push(token);
        }
    }
}

/// One presentation the desktop reported.
#[derive(Clone, Copy, Debug)]
pub struct Presented {
    pub secs: u64,
    pub nsec: u32,
    pub refresh: u32,
    pub seq: u64,
    pub flags: u32,
}

/// The user data marking a presentation feedback request.
pub struct FeedbackSeen;

impl Dispatch<WpPresentation, ()> for Heard {
    fn event(
        heard: &mut Self,
        _: &WpPresentation,
        event: wp_presentation::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wp_presentation::Event::ClockId { clk_id } = event {
            heard.clock_ids.push(clk_id);
        }
    }
}

impl Dispatch<WpPresentationFeedback, FeedbackSeen> for Heard {
    fn event(
        heard: &mut Self,
        _: &WpPresentationFeedback,
        event: wp_presentation_feedback::Event,
        _: &FeedbackSeen,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wp_presentation_feedback::Event::SyncOutput { .. } => {
                heard.sync_outputs = heard.sync_outputs.saturating_add(1);
            }
            wp_presentation_feedback::Event::Presented {
                tv_sec_hi,
                tv_sec_lo,
                tv_nsec,
                refresh,
                seq_hi,
                seq_lo,
                flags: WEnum::Value(flags),
            } => heard.presented.push(Presented {
                secs: (u64::from(tv_sec_hi) << 32) | u64::from(tv_sec_lo),
                nsec: tv_nsec,
                refresh,
                seq: (u64::from(seq_hi) << 32) | u64::from(seq_lo),
                flags: flags.bits(),
            }),
            wp_presentation_feedback::Event::Discarded => {
                heard.presentation_discarded = heard.presentation_discarded.saturating_add(1);
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwpKeyboardShortcutsInhibitorV1, ()> for Heard {
    fn event(
        heard: &mut Self,
        _: &ZwpKeyboardShortcutsInhibitorV1,
        event: zwp_keyboard_shortcuts_inhibitor_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_keyboard_shortcuts_inhibitor_v1::Event::Active => heard.shortcuts_inhibited = true,
            zwp_keyboard_shortcuts_inhibitor_v1::Event::Inactive => {
                heard.shortcuts_inhibited = false;
            }
            _ => {}
        }
    }
}

impl Dispatch<XdgToplevelIconManagerV1, ()> for Heard {
    fn event(
        heard: &mut Self,
        _: &XdgToplevelIconManagerV1,
        event: icon_manager::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let icon_manager::Event::IconSize { size } = event {
            heard.icon_sizes.push(size);
        }
    }
}

delegate_noop!(Heard: ignore XdgToplevelIconV1);
delegate_noop!(Heard: ignore XdgWmDialogV1);
delegate_noop!(Heard: ignore XdgDialogV1);
delegate_noop!(Heard: ignore ZwpIdleInhibitManagerV1);
delegate_noop!(Heard: ignore ZwpIdleInhibitorV1);
delegate_noop!(Heard: ignore ZwpKeyboardShortcutsInhibitManagerV1);

delegate_noop!(Heard: ignore WpCursorShapeManagerV1);
delegate_noop!(Heard: ignore WpCursorShapeDeviceV1);
delegate_noop!(Heard: ignore XdgActivationV1);
delegate_noop!(Heard: ignore ZxdgDecorationManagerV1);
delegate_noop!(Heard: ignore OrgKdeKwinServerDecorationManager);

/// The state number of `suspended` in `xdg_toplevel.configure`.
pub const SUSPENDED: u32 = 9;

impl Dispatch<WlDataDeviceManager, ()> for Heard {
    fn event(
        _: &mut Self,
        _: &WlDataDeviceManager,
        _: wl_data_device_manager::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlDataDevice, ()> for Heard {
    fn event(
        heard: &mut Self,
        _: &WlDataDevice,
        event: wl_data_device::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_data_device::Event::DataOffer { id } => {
                heard.offer_mimes.push((id, Vec::new()));
            }
            wl_data_device::Event::Selection { id } => {
                if let Some(old) = heard.selection.take() {
                    old.destroy();
                }
                heard.selection = id;
            }
            wl_data_device::Event::Enter {
                serial, id: offer, ..
            } => {
                heard.drag_entered = true;
                heard.enter_serial = serial;
                heard.drag_offer = offer;
            }
            wl_data_device::Event::Leave => {
                heard.drag_left = true;
                if let Some(offer) = heard.drag_offer.take() {
                    offer.destroy();
                }
            }
            wl_data_device::Event::Motion { .. } => heard.drag_motion += 1,
            wl_data_device::Event::Drop => heard.drops += 1,
            _ => {}
        }
    }

    event_created_child!(Heard, WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, ()),
        wl_data_device::EVT_ENTER_OPCODE => (WlDataOffer, ()),
    ]);
}

impl Dispatch<WlDataOffer, ()> for Heard {
    fn event(
        heard: &mut Self,
        offer: &WlDataOffer,
        event: wl_data_offer::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_data_offer::Event::Offer { mime_type } = event
            && let Some((_, mimes)) = heard.offer_mimes.iter_mut().find(|(held, _)| held == offer)
        {
            mimes.push(mime_type);
        }
    }
}

impl Dispatch<WlDataSource, ()> for Heard {
    fn event(
        heard: &mut Self,
        source: &WlDataSource,
        event: wl_data_source::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_data_source::Event::Send { mime_type, fd } => {
                let bytes = heard
                    .sources
                    .iter()
                    .find(|(held, _)| held == source)
                    .map(|(_, bytes)| bytes.clone())
                    .unwrap_or_default();
                heard.sent.push(mime_type);
                let _ = std::fs::File::from(fd).write_all(&bytes);
            }
            wl_data_source::Event::Cancelled => heard.source_cancelled = true,
            wl_data_source::Event::Action {
                dnd_action: WEnum::Value(action),
            } => heard.source_action = Some(action),
            wl_data_source::Event::DndFinished => heard.dnd_finished = true,
            _ => {}
        }
    }
}

impl Dispatch<ZwpPrimarySelectionDeviceManagerV1, ()> for Heard {
    fn event(
        _: &mut Self,
        _: &ZwpPrimarySelectionDeviceManagerV1,
        _: <ZwpPrimarySelectionDeviceManagerV1 as wayland_client::Proxy>::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpPrimarySelectionDeviceV1, ()> for Heard {
    fn event(
        heard: &mut Self,
        _: &ZwpPrimarySelectionDeviceV1,
        event: zwp_primary_selection_device_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_primary_selection_device_v1::Event::DataOffer { offer } => {
                heard.primary_mimes.push((offer, Vec::new()));
            }
            zwp_primary_selection_device_v1::Event::Selection { id } => {
                if let Some(old) = heard.primary_selection.take() {
                    old.destroy();
                }
                heard.primary_selection = id;
            }
            _ => {}
        }
    }

    event_created_child!(Heard, ZwpPrimarySelectionDeviceV1, [
        zwp_primary_selection_device_v1::EVT_DATA_OFFER_OPCODE => (ZwpPrimarySelectionOfferV1, ()),
    ]);
}

impl Dispatch<ZwpPrimarySelectionOfferV1, ()> for Heard {
    fn event(
        heard: &mut Self,
        offer: &ZwpPrimarySelectionOfferV1,
        event: zwp_primary_selection_offer_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwp_primary_selection_offer_v1::Event::Offer { mime_type } = event
            && let Some((_, mimes)) = heard
                .primary_mimes
                .iter_mut()
                .find(|(held, _)| held == offer)
        {
            mimes.push(mime_type);
        }
    }
}

impl Dispatch<ZwpPrimarySelectionSourceV1, ()> for Heard {
    fn event(
        heard: &mut Self,
        source: &ZwpPrimarySelectionSourceV1,
        event: zwp_primary_selection_source_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_primary_selection_source_v1::Event::Send { mime_type, fd } => {
                let bytes = heard
                    .primary_sources
                    .iter()
                    .find(|(held, _)| held == source)
                    .map(|(_, bytes)| bytes.clone())
                    .unwrap_or_default();
                heard.primary_sent.push(mime_type);
                let _ = std::fs::File::from(fd).write_all(&bytes);
            }
            zwp_primary_selection_source_v1::Event::Cancelled => heard.primary_cancelled = true,
            _ => {}
        }
    }
}

impl App {
    /// Become the clipboard's (or the primary selection's) owner, offering `mime`.
    pub fn own_selection(&mut self, mime: &str, bytes: &[u8], primary: bool) {
        let qh = self.qh();
        let serial = self.heard.last_serial;
        if primary {
            let manager = self
                .heard
                .primary_manager
                .clone()
                .expect("a primary selection manager");
            let source = manager.create_source(&qh, ());
            source.offer(mime.to_owned());
            self.heard
                .primary_sources
                .push((source.clone(), bytes.to_vec()));
            self.heard
                .primary_device
                .as_ref()
                .expect("a primary selection device")
                .set_selection(Some(&source), serial);
        } else {
            let manager = self
                .heard
                .data_device_manager
                .clone()
                .expect("a data device manager");
            let source = manager.create_data_source(&qh, ());
            source.offer(mime.to_owned());
            source.set_actions(DndAction::Copy);
            self.heard.sources.push((source.clone(), bytes.to_vec()));
            self.heard
                .data_device
                .as_ref()
                .expect("a data device")
                .set_selection(Some(&source), serial);
        }
    }

    /// The formats offered to the app by the clipboard (or the primary selection).
    pub fn selection_mimes(&self, primary: bool) -> Vec<String> {
        if primary {
            self.heard
                .primary_mimes
                .iter()
                .filter(|(offer, _)| self.heard.primary_selection.as_ref() == Some(offer))
                .flat_map(|(_, mimes)| mimes.clone())
                .collect()
        } else {
            self.heard
                .offer_mimes
                .iter()
                .filter(|(offer, _)| self.heard.selection.as_ref() == Some(offer))
                .flat_map(|(_, mimes)| mimes.clone())
                .collect()
        }
    }

    /// Read the clipboard (or the primary selection) as `mime`, pumping until the owner wrote.
    pub fn receive(&mut self, rig: &mut Rig, mime: &str, primary: bool) -> Option<Vec<u8>> {
        let (reader, writer) = std::io::pipe().expect("transfer pipe");
        if primary {
            let offer = self.heard.primary_selection.clone()?;
            offer.receive(mime.to_owned(), writer.as_fd());
        } else {
            let offer = self.heard.selection.clone()?;
            offer.receive(mime.to_owned(), writer.as_fd());
        }
        drop(writer);
        let bytes = std::sync::Arc::new(std::sync::Mutex::new(None));
        let written = std::sync::Arc::clone(&bytes);
        std::thread::spawn(move || {
            let mut read = Vec::new();
            let mut reader = reader;
            let _ = reader.read_to_end(&mut read);
            *written.lock().unwrap() = Some(read);
        });
        rig.pump(&mut [self], |_, _| bytes.lock().unwrap().is_some());
        bytes.lock().unwrap().take()
    }

    /// Read the drag the desktop offers the app as `mime`.
    pub fn receive_drag(&mut self, rig: &mut Rig, mime: &str) -> Option<Vec<u8>> {
        let (reader, writer) = std::io::pipe().expect("transfer pipe");
        let offer = self.heard.drag_offer.clone()?;
        offer.set_actions(DndAction::Copy | DndAction::Move, DndAction::Copy);
        offer.accept(self.heard.enter_serial, Some(mime.to_owned()));
        offer.receive(mime.to_owned(), writer.as_fd());
        drop(writer);
        let bytes = std::sync::Arc::new(std::sync::Mutex::new(None));
        let written = std::sync::Arc::clone(&bytes);
        std::thread::spawn(move || {
            let mut read = Vec::new();
            let mut reader = reader;
            let _ = reader.read_to_end(&mut read);
            *written.lock().unwrap() = Some(read);
        });
        rig.pump(&mut [self], |_, _| bytes.lock().unwrap().is_some());
        bytes.lock().unwrap().take()
    }

    /// Start a drag of the app's own bytes, with an icon, as a button press allows.
    pub fn start_drag(&mut self, mime: &str, bytes: &[u8]) {
        let qh = self.qh();
        let serial = self.heard.last_serial;
        let manager = self
            .heard
            .data_device_manager
            .clone()
            .expect("a data device manager");
        let source = manager.create_data_source(&qh, ());
        source.offer(mime.to_owned());
        source.set_actions(DndAction::Copy);
        self.heard.sources.push((source.clone(), bytes.to_vec()));
        let icon = self
            .heard
            .compositor
            .as_ref()
            .expect("a compositor")
            .create_surface(&qh, ());
        let buffer = self.buffer((8, 8), [0, 0, 255, 255]);
        icon.attach(Some(&buffer), 0, 0);
        icon.commit();
        let origin = self.surface.clone().expect("a window");
        self.heard
            .data_device
            .as_ref()
            .expect("a data device")
            .start_drag(Some(&source), &origin, Some(&icon), serial);
    }
}

impl Dispatch<ZwpTextInputManagerV3, ()> for Heard {
    fn event(
        _: &mut Self,
        _: &ZwpTextInputManagerV3,
        _: text_manager::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpTextInputV3, ()> for Heard {
    fn event(
        heard: &mut Self,
        _: &ZwpTextInputV3,
        event: text_input::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            text_input::Event::Enter { .. } => heard.text_entered = true,
            text_input::Event::Leave { .. } => heard.text_left = true,
            text_input::Event::PreeditString { text, .. } => {
                heard.preedits.push(text.unwrap_or_default());
            }
            text_input::Event::CommitString { text } => {
                heard.commits.push(text.unwrap_or_default());
            }
            text_input::Event::DeleteSurroundingText {
                before_length,
                after_length,
            } => heard.deletes.push((before_length, after_length)),
            text_input::Event::Done { serial } => heard.text_dones.push(serial),
            _ => {}
        }
    }
}

impl App {
    /// Enable a text field and commit what it declares.
    pub fn enable_text_input(&self) {
        let input = self.heard.text_input.clone().expect("a text input");
        input.enable();
        input.set_content_type(ContentHint::empty(), ContentPurpose::Normal);
        input.set_cursor_rectangle(10, 12, 2, 20);
        input.set_text_change_cause(ChangeCause::Other);
        input.commit();
    }

    /// Stop typing into the field.
    pub fn disable_text_input(&self) {
        let input = self.heard.text_input.clone().expect("a text input");
        input.disable();
        input.commit();
    }
}

impl Dispatch<ZwpPointerConstraintsV1, ()> for Heard {
    fn event(
        _: &mut Self,
        _: &ZwpPointerConstraintsV1,
        _: constraints_manager::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpLockedPointerV1, ()> for Heard {
    fn event(
        heard: &mut Self,
        _: &ZwpLockedPointerV1,
        event: locked_pointer::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            locked_pointer::Event::Locked => heard.locked = true,
            locked_pointer::Event::Unlocked => heard.unlocked = true,
            _ => {}
        }
    }
}

impl Dispatch<ZwpPointerGesturesV1, ()> for Heard {
    fn event(
        _: &mut Self,
        _: &ZwpPointerGesturesV1,
        _: wayland_protocols::wp::pointer_gestures::zv1::client::zwp_pointer_gestures_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpPointerGestureSwipeV1, ()> for Heard {
    fn event(
        heard: &mut Self,
        _: &ZwpPointerGestureSwipeV1,
        event: swipe_gesture::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            swipe_gesture::Event::Begin {
                serial, fingers, ..
            } => {
                heard.swipes.push((serial, fingers));
            }
            swipe_gesture::Event::Update { dx, dy, .. } => {
                heard.swipe_updates.push((dx, dy));
            }
            swipe_gesture::Event::End { cancelled, .. } => {
                heard.swipe_ends.push(cancelled);
            }
            _ => {}
        }
    }
}

impl Dispatch<ZwpPointerGesturePinchV1, ()> for Heard {
    fn event(
        heard: &mut Self,
        _: &ZwpPointerGesturePinchV1,
        event: pinch_gesture::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            pinch_gesture::Event::Begin {
                serial, fingers, ..
            } => heard.pinches.push((serial, fingers)),
            pinch_gesture::Event::Update {
                dx,
                dy,
                scale,
                rotation,
                ..
            } => heard.pinch_updates.push((dx, dy, scale, rotation)),
            pinch_gesture::Event::End { cancelled, .. } => heard.pinch_ends.push(cancelled),
            _ => {}
        }
    }
}

impl Dispatch<ZwpRelativePointerManagerV1, ()> for Heard {
    fn event(
        _: &mut Self,
        _: &ZwpRelativePointerManagerV1,
        _: wayland_protocols::wp::relative_pointer::zv1::client::zwp_relative_pointer_manager_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpRelativePointerV1, ()> for Heard {
    fn event(
        heard: &mut Self,
        _: &ZwpRelativePointerV1,
        event: relative_pointer::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let relative_pointer::Event::RelativeMotion { dx, dy, .. } = event {
            heard.relative_motion.push((dx, dy));
        }
    }
}

impl App {
    /// Lock the pointer on the app's window, persistently.
    pub fn lock_pointer(&mut self) {
        let qh = self.qh();
        let manager = self
            .heard
            .constraints
            .clone()
            .expect("a pointer constraints manager");
        let pointer = self.heard.pointer.clone().expect("a pointer");
        let surface = self.surface.clone().expect("a window");
        let locked = manager.lock_pointer(&surface, &pointer, None, Lifetime::Persistent, &qh, ());
        self.heard.locked_pointer = Some(locked);
    }

    /// Follow the pointer relatively.
    pub fn make_relative_pointer(&mut self) {
        let qh = self.qh();
        let manager = self
            .heard
            .relative
            .clone()
            .expect("a relative pointer manager");
        let pointer = self.heard.pointer.clone().expect("a pointer");
        let relative = manager.get_relative_pointer(&pointer, &qh, ());
        self.heard.relative_pointer = Some(relative);
    }

    /// Take two-finger gestures.
    pub fn make_gestures(&mut self) {
        let qh = self.qh();
        let manager = self
            .heard
            .gestures
            .clone()
            .expect("a pointer gestures manager");
        let pointer = self.heard.pointer.clone().expect("a pointer");
        if self.heard.swipe_gesture.is_none() {
            self.heard.swipe_gesture = Some(manager.get_swipe_gesture(&pointer, &qh, ()));
        }
        if self.heard.pinch_gesture.is_none() {
            self.heard.pinch_gesture = Some(manager.get_pinch_gesture(&pointer, &qh, ()));
        }
    }
}

impl Heard {
    /// Take the tablet seat once both the manager and the seat are known.
    fn take_tablet_seat(&mut self, qh: &QueueHandle<Self>) {
        if self.tablet_seat.is_some() {
            return;
        }
        let (Some(manager), Some(seat)) = (&self.tablet_manager, &self.seat) else {
            return;
        };
        self.tablet_seat = Some(manager.get_tablet_seat(seat, qh, ()));
    }
}

impl Dispatch<ZwpTabletSeatV2, ()> for Heard {
    fn event(
        heard: &mut Self,
        _: &ZwpTabletSeatV2,
        event: zwp_tablet_seat_v2::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_tablet_seat_v2::Event::TabletAdded { id } => {
                heard.tablet_ids.push(id.id());
                heard.tablets.push(TabletSeen::default());
            }
            zwp_tablet_seat_v2::Event::ToolAdded { id, .. } => {
                heard.tool_ids.push(id.id());
                heard.tools.push(ToolSeen::default());
                heard.tool = Some(id);
            }
            zwp_tablet_seat_v2::Event::PadAdded { id, .. } => {
                heard.pad_ids.push(id.id());
                heard.pads.push(PadSeen::default());
                heard.pad = Some(id);
            }
            _ => {}
        }
    }

    wayland_client::event_created_child!(Heard, ZwpTabletSeatV2, [
        zwp_tablet_seat_v2::EVT_TABLET_ADDED_OPCODE => (ZwpTabletV2, ()),
        zwp_tablet_seat_v2::EVT_TOOL_ADDED_OPCODE => (ZwpTabletToolV2, ()),
        zwp_tablet_seat_v2::EVT_PAD_ADDED_OPCODE => (ZwpTabletPadV2, ()),
    ]);
}

impl Dispatch<ZwpTabletV2, ()> for Heard {
    fn event(
        heard: &mut Self,
        tablet: &ZwpTabletV2,
        event: zwp_tablet_v2::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(index) = index_of(&heard.tablet_ids, &tablet.id()) else {
            return;
        };
        let seen = &mut heard.tablets[index];
        match event {
            zwp_tablet_v2::Event::Name { name } => seen.name = Some(name),
            zwp_tablet_v2::Event::Id { vid, pid } => seen.id = Some((vid, pid)),
            zwp_tablet_v2::Event::Path { path } => seen.path = Some(path),
            zwp_tablet_v2::Event::Done => seen.done = true,
            zwp_tablet_v2::Event::Removed => seen.removed = true,
            _ => {}
        }
    }
}

impl Dispatch<ZwpTabletToolV2, ()> for Heard {
    fn event(
        heard: &mut Self,
        tool: &ZwpTabletToolV2,
        event: zwp_tablet_tool_v2::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(index) = index_of(&heard.tool_ids, &tool.id()) else {
            return;
        };
        let seen = &mut heard.tools[index];
        match event {
            zwp_tablet_tool_v2::Event::Type {
                tool_type: WEnum::Value(tool_type),
            } => seen.tool_type = Some(u32::from(tool_type)),
            zwp_tablet_tool_v2::Event::HardwareSerial {
                hardware_serial_hi,
                hardware_serial_lo,
            } => seen.hardware_serial = Some((hardware_serial_hi, hardware_serial_lo)),
            zwp_tablet_tool_v2::Event::HardwareIdWacom {
                hardware_id_hi,
                hardware_id_lo,
            } => seen.hardware_id = Some((hardware_id_hi, hardware_id_lo)),
            zwp_tablet_tool_v2::Event::Capability {
                capability: WEnum::Value(capability),
            } => seen.capabilities.push(u32::from(capability)),
            zwp_tablet_tool_v2::Event::Done => seen.done = true,
            zwp_tablet_tool_v2::Event::Removed => seen.removed = true,
            zwp_tablet_tool_v2::Event::ProximityIn {
                serial, surface, ..
            } => seen.proximity_in.push((serial, surface)),
            zwp_tablet_tool_v2::Event::ProximityOut => seen.proximity_out += 1,
            zwp_tablet_tool_v2::Event::Down { serial } => seen.down.push(serial),
            zwp_tablet_tool_v2::Event::Up => seen.up += 1,
            zwp_tablet_tool_v2::Event::Motion { x, y } => seen.motion.push((x, y)),
            zwp_tablet_tool_v2::Event::Pressure { pressure } => seen.pressure.push(pressure),
            zwp_tablet_tool_v2::Event::Distance { distance } => seen.distance.push(distance),
            zwp_tablet_tool_v2::Event::Tilt { tilt_x, tilt_y } => seen.tilt.push((tilt_x, tilt_y)),
            zwp_tablet_tool_v2::Event::Rotation { degrees } => seen.rotation.push(degrees),
            zwp_tablet_tool_v2::Event::Slider { position } => seen.slider.push(position),
            zwp_tablet_tool_v2::Event::Wheel { degrees, clicks } => {
                seen.wheel.push((degrees, clicks));
            }
            zwp_tablet_tool_v2::Event::Button {
                serial,
                button,
                state: WEnum::Value(button_state),
            } => seen.buttons.push((
                serial,
                button,
                button_state == zwp_tablet_tool_v2::ButtonState::Pressed,
            )),
            zwp_tablet_tool_v2::Event::Frame { time } => seen.frames.push(time),
            _ => {}
        }
    }
}

impl Dispatch<ZwpTabletPadV2, ()> for Heard {
    fn event(
        heard: &mut Self,
        pad: &ZwpTabletPadV2,
        event: zwp_tablet_pad_v2::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(index) = index_of(&heard.pad_ids, &pad.id()) else {
            return;
        };
        let seen = &mut heard.pads[index];
        match event {
            zwp_tablet_pad_v2::Event::Group { .. } => seen.groups += 1,
            zwp_tablet_pad_v2::Event::Path { path } => seen.path = Some(path),
            zwp_tablet_pad_v2::Event::Buttons { buttons } => seen.buttons = Some(buttons),
            zwp_tablet_pad_v2::Event::Done => seen.done = true,
            zwp_tablet_pad_v2::Event::Removed => seen.removed = true,
            zwp_tablet_pad_v2::Event::Enter {
                serial, surface, ..
            } => {
                seen.entered = Some((serial, surface));
            }
            zwp_tablet_pad_v2::Event::Leave { serial, surface } => {
                seen.left = Some((serial, surface));
            }
            zwp_tablet_pad_v2::Event::Button {
                time,
                button,
                state: WEnum::Value(button_state),
            } => seen.buttons_pressed.push((
                time,
                button,
                button_state == zwp_tablet_pad_v2::ButtonState::Pressed,
            )),
            _ => {}
        }
    }

    wayland_client::event_created_child!(Heard, ZwpTabletPadV2, [
        zwp_tablet_pad_v2::EVT_GROUP_OPCODE => (ZwpTabletPadGroupV2, ()),
    ]);
}

impl Dispatch<ZwpTabletPadGroupV2, ()> for Heard {
    fn event(
        heard: &mut Self,
        _: &ZwpTabletPadGroupV2,
        event: zwp_tablet_pad_group_v2::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_tablet_pad_group_v2::Event::Buttons { buttons } => {
                heard.pad_group_buttons.push(buttons);
            }
            zwp_tablet_pad_group_v2::Event::Ring { ring } => heard.ring = Some(ring),
            zwp_tablet_pad_group_v2::Event::Strip { strip } => heard.strip = Some(strip),
            zwp_tablet_pad_group_v2::Event::Modes { modes } => heard.pad_group_modes = modes,
            zwp_tablet_pad_group_v2::Event::Done => heard.pad_group_done += 1,
            zwp_tablet_pad_group_v2::Event::ModeSwitch { serial, mode, .. } => {
                heard.pad_modes.push((serial, mode));
            }
            _ => {}
        }
    }

    wayland_client::event_created_child!(Heard, ZwpTabletPadGroupV2, [
        zwp_tablet_pad_group_v2::EVT_RING_OPCODE => (ZwpTabletPadRingV2, ()),
        zwp_tablet_pad_group_v2::EVT_STRIP_OPCODE => (ZwpTabletPadStripV2, ()),
    ]);
}

impl Dispatch<ZwpTabletPadRingV2, ()> for Heard {
    fn event(
        heard: &mut Self,
        _: &ZwpTabletPadRingV2,
        event: zwp_tablet_pad_ring_v2::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_tablet_pad_ring_v2::Event::Angle { degrees } => heard.ring_angles.push(degrees),
            zwp_tablet_pad_ring_v2::Event::Stop => heard.ring_stops += 1,
            zwp_tablet_pad_ring_v2::Event::Frame { .. } => heard.ring_frames += 1,
            _ => {}
        }
    }
}

impl Dispatch<ZwpTabletPadStripV2, ()> for Heard {
    fn event(
        heard: &mut Self,
        _: &ZwpTabletPadStripV2,
        event: zwp_tablet_pad_strip_v2::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_tablet_pad_strip_v2::Event::Position { position } => {
                heard.strip_positions.push(position);
            }
            zwp_tablet_pad_strip_v2::Event::Stop => heard.strip_stops += 1,
            zwp_tablet_pad_strip_v2::Event::Frame { .. } => heard.strip_frames += 1,
            _ => {}
        }
    }
}

delegate_noop!(Heard: ignore ZwpTabletManagerV2);
