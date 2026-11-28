//! The harness the headless tests are written against: a client with its one toplevel, what the
//! compositor told it, and the pumps that let the two talk until a condition holds.

use std::io::{Read as _, Write as _};
use std::os::fd::AsFd as _;

/// `wl_keyboard.keymap_format` 's XKB v1: what a virtual keyboard is given.
const KEYMAP_FORMAT_XKB_V1: u32 = 1;
use std::time::Duration;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use tilo_compositor::{Compositor, InputEvent, OutputConfig, WindowPolicy};
use tilo_session_core::surface::{
    ShellSurface, SurfaceFrame, SurfaceIme, SurfaceKey, SurfaceLayer, SurfaceTextInput,
};
use wayland_client::protocol::wl_data_device_manager::DndAction;
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_callback::WlCallback, wl_compositor::WlCompositor, wl_data_device,
    wl_data_device::WlDataDevice, wl_data_device_manager::WlDataDeviceManager, wl_data_offer,
    wl_data_offer::WlDataOffer, wl_data_source, wl_data_source::WlDataSource, wl_keyboard,
    wl_keyboard::WlKeyboard, wl_pointer, wl_pointer::WlPointer, wl_region::WlRegion, wl_registry,
    wl_registry::WlRegistry, wl_seat, wl_seat::WlSeat, wl_shm, wl_shm::WlShm,
    wl_shm_pool::WlShmPool, wl_surface::WlSurface, wl_touch, wl_touch::WlTouch,
};
use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy as _, QueueHandle, WEnum, delegate_noop,
    event_created_child,
};
use wayland_protocols::ext::session_lock::v1::client::{
    ext_session_lock_manager_v1::ExtSessionLockManagerV1, ext_session_lock_v1,
    ext_session_lock_v1::ExtSessionLockV1,
};
use wayland_protocols::wp::color_management::v1::client::{
    wp_color_management_surface_v1::WpColorManagementSurfaceV1,
    wp_color_manager_v1::{self, WpColorManagerV1},
    wp_image_description_creator_icc_v1::WpImageDescriptionCreatorIccV1,
    wp_image_description_creator_params_v1::WpImageDescriptionCreatorParamsV1,
    wp_image_description_v1::{self, WpImageDescriptionV1},
};
use wayland_protocols::wp::cursor_shape::v1::client::{
    wp_cursor_shape_device_v1::{Shape, WpCursorShapeDeviceV1},
    wp_cursor_shape_manager_v1::WpCursorShapeManagerV1,
};
use wayland_protocols::wp::idle_inhibit::zv1::client::{
    zwp_idle_inhibit_manager_v1::ZwpIdleInhibitManagerV1, zwp_idle_inhibitor_v1::ZwpIdleInhibitorV1,
};
use wayland_protocols::wp::keyboard_shortcuts_inhibit::zv1::client::{
    zwp_keyboard_shortcuts_inhibit_manager_v1::ZwpKeyboardShortcutsInhibitManagerV1,
    zwp_keyboard_shortcuts_inhibitor_v1::{self, ZwpKeyboardShortcutsInhibitorV1},
};
use wayland_protocols::wp::pointer_constraints::zv1::client::{
    zwp_confined_pointer_v1::{self, ZwpConfinedPointerV1},
    zwp_locked_pointer_v1::{self, ZwpLockedPointerV1},
    zwp_pointer_constraints_v1::{Lifetime, ZwpPointerConstraintsV1},
};
use wayland_protocols::wp::relative_pointer::zv1::client::{
    zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1,
    zwp_relative_pointer_v1::{self, ZwpRelativePointerV1},
};
use wayland_protocols::wp::tablet::zv2::client::{
    zwp_tablet_manager_v2::ZwpTabletManagerV2,
    zwp_tablet_pad_group_v2::{self, ZwpTabletPadGroupV2},
    zwp_tablet_pad_ring_v2::{self, ZwpTabletPadRingV2},
    zwp_tablet_pad_strip_v2::ZwpTabletPadStripV2,
    zwp_tablet_pad_v2::{self, ZwpTabletPadV2},
    zwp_tablet_seat_v2::{self, ZwpTabletSeatV2},
    zwp_tablet_tool_v2::{self, ZwpTabletToolV2},
    zwp_tablet_v2::ZwpTabletV2,
};
use wayland_protocols::wp::text_input::zv3::client::{
    zwp_text_input_manager_v3::ZwpTextInputManagerV3,
    zwp_text_input_v3,
    zwp_text_input_v3::{ContentHint, ContentPurpose, ZwpTextInputV3},
};
use wayland_protocols::xdg::decoration::zv1::client::{
    zxdg_decoration_manager_v1::ZxdgDecorationManagerV1,
    zxdg_toplevel_decoration_v1::{self, Mode as XdgMode, ZxdgToplevelDecorationV1},
};
use wayland_protocols::xdg::dialog::v1::client::{
    xdg_dialog_v1::XdgDialogV1, xdg_wm_dialog_v1::XdgWmDialogV1,
};
use wayland_protocols::xdg::shell::client::{
    xdg_surface, xdg_surface::XdgSurface, xdg_toplevel, xdg_toplevel::XdgToplevel, xdg_wm_base,
    xdg_wm_base::XdgWmBase,
};
use wayland_protocols::xdg::toplevel_icon::v1::client::{
    xdg_toplevel_icon_manager_v1, xdg_toplevel_icon_manager_v1::XdgToplevelIconManagerV1,
    xdg_toplevel_icon_v1::XdgToplevelIconV1,
};
use wayland_protocols::xdg::toplevel_tag::v1::client::xdg_toplevel_tag_manager_v1::XdgToplevelTagManagerV1;
use wayland_protocols_misc::server_decoration::client::{
    org_kde_kwin_server_decoration::{self as kde_decoration, Mode as KdeMode},
    org_kde_kwin_server_decoration_manager::{
        self as kde_manager, Mode as KdeDefaultMode, OrgKdeKwinServerDecorationManager,
    },
};
use wayland_protocols_misc::zwp_input_method_v2::client::{
    zwp_input_method_keyboard_grab_v2::{self, ZwpInputMethodKeyboardGrabV2},
    zwp_input_method_manager_v2::ZwpInputMethodManagerV2,
    zwp_input_method_v2,
    zwp_input_method_v2::ZwpInputMethodV2,
    zwp_input_popup_surface_v2::{self, ZwpInputPopupSurfaceV2},
};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
    zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
};
use wayland_protocols_wlr::layer_shell::v1::client::{
    zwlr_layer_shell_v1::{self, ZwlrLayerShellV1},
    zwlr_layer_surface_v1::{self, ZwlrLayerSurfaceV1},
};
use x11rb::connection::Connection as _;
use x11rb::protocol::xproto::Window;
use x11rb::rust_connection::RustConnection;

#[derive(Default)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "one flag per event a test client waits for"
)]
pub struct Seen {
    pub globals: Vec<String>,
    pub lock_manager: Option<ExtSessionLockManagerV1>,
    pub locked: bool,
    pub compositor: Option<WlCompositor>,
    pub shm: Option<WlShm>,
    /// The buffer formats `wl_shm` offered, in the order it sent them.
    pub shm_formats: Vec<wl_shm::Format>,
    pub wm_base: Option<XdgWmBase>,
    pub seat: Option<WlSeat>,
    pub text_input_manager: Option<ZwpTextInputManagerV3>,
    pub text_input: Option<ZwpTextInputV3>,
    /// Whether the field has the focus and has asked for typing, as the compositor told it.
    pub field_entered: bool,
    /// The strings the field was made to contain.
    pub committed: Vec<String>,
    /// How many times the compositor applied state to the field.
    pub field_done: u32,
    pub input_method_manager: Option<ZwpInputMethodManagerV2>,
    pub input_method: Option<ZwpInputMethodV2>,
    /// How many times the compositor applied the input method's state: what its next commit is
    /// numbered.
    pub ime_done: u32,
    /// Whether the compositor asked the input method to type.
    pub ime_activated: bool,
    /// The seat's keyboard, held as an input method holds it while it composes: every key reaches
    /// it, and only what it passes on reaches the field.
    pub grab: Option<ZwpInputMethodKeyboardGrabV2>,
    /// The keys the grab received, as `(keycode, pressed)` .
    pub grab_keys: Vec<(u32, bool)>,
    /// The candidate window's surface, and the rectangles the compositor told it the field's text
    /// stands at, as `x y width height` .
    pub popup: Option<ZwpInputPopupSurfaceV2>,
    pub popup_surface: Option<WlSurface>,
    pub popup_rects: Vec<(i32, i32, i32, i32)>,
    /// The keyboard an input method types through the keys it does not compose itself.
    pub virtual_keyboard_manager: Option<ZwpVirtualKeyboardManagerV1>,
    pub virtual_keyboard: Option<ZwpVirtualKeyboardV1>,
    pub pointer: Option<WlPointer>,
    /// The absolute positions the pointer was put at here, as `wl_pointer.motion` sent them.
    pub pointer_positions: Vec<(f64, f64)>,
    /// Where the pointer entered, as `wl_pointer.enter` placed it.
    pub pointer_entered_at: Option<(f64, f64)>,
    pub keyboard: Option<WlKeyboard>,
    pub touch: Option<WlTouch>,
    /// Whether this client leaves the seat's touch objects alone, as a toolkit that only knows the
    /// pointer does.
    pub no_touch: bool,
    pub pointer_constraints: Option<ZwpPointerConstraintsV1>,
    pub locked_pointer: Option<ZwpLockedPointerV1>,
    pub confined_pointer: Option<ZwpConfinedPointerV1>,
    /// Whether the compositor holds the pointer where it is.
    pub pointer_locked: bool,
    /// Whether the compositor keeps the pointer inside the region.
    pub pointer_confined: bool,
    pub relative_pointer_manager: Option<ZwpRelativePointerManagerV1>,
    pub relative_pointer: Option<ZwpRelativePointerV1>,
    /// The deltas the compositor sent, as `(dx, dy)` .
    pub relative: Vec<(f64, f64)>,
    pub inhibit_manager: Option<ZwpKeyboardShortcutsInhibitManagerV1>,
    pub idle_inhibit: Option<ZwpIdleInhibitManagerV1>,
    pub idle_inhibitor: Option<ZwpIdleInhibitorV1>,
    pub inhibitor: Option<ZwpKeyboardShortcutsInhibitorV1>,
    /// Whether the compositor lets this client answer the desktop's chords.
    pub shortcuts_inhibited: bool,
    /// Touch events as `down id x y` , `motion id x y` , `up id` , `frame` .
    pub touches: Vec<String>,
    pub configured: bool,
    pub pointer_inside: bool,
    pub buttons: Vec<(u32, bool)>,
    pub keyboard_focused: bool,
    pub keys: Vec<(u32, bool)>,
    /// The modifiers the keyboard last said are held, as their XKB mask.
    pub modifiers_held: u32,
    /// Each key pressed, with the modifiers held as it went down.
    pub presses: Vec<(u32, u32)>,
    /// The keymap the keyboard sent, as XKB text: the client's own answer to which characters its
    /// keys type.
    pub keymap: Option<String>,
    pub frames_done: u32,
    pub closed: bool,
    pub tablet_manager: Option<ZwpTabletManagerV2>,
    /// The tablet tools announced to this client, oldest first.
    pub tablet_tools: Vec<ZwpTabletToolV2>,
    /// The hardware serial a tool named, as the two words the protocol carries.
    pub tool_serial: Option<(u32, u32)>,
    /// The Wacom hardware id a tool named, as the two words the protocol carries.
    pub tool_wacom_id: Option<(u32, u32)>,
    /// The serial of the last proximity-in a tool sent.
    pub tool_proximity_serial: u32,
    /// Whether a tool is in proximity to a surface of this client.
    pub tool_entered: bool,
    pub cursor_shape: Option<WpCursorShapeManagerV1>,
    pub wm_dialog: Option<XdgWmDialogV1>,
    pub icon_manager: Option<XdgToplevelIconManagerV1>,
    pub tag_manager: Option<XdgToplevelTagManagerV1>,
    /// The icon sizes the manager tells clients a tab pill paints at.
    pub icon_sizes: Vec<i32>,
    /// Tablet pad events as `added`, `group`, `buttons 4`, `done`,
    /// `enter`, `mode 1`, `button 2 pressed`, `ring 90`, `ring stop`,
    /// `removed`.
    pub pad: Vec<String>,
    /// The surface the pad entered.
    pub pad_surface: Option<WlSurface>,
    /// The pad as this client holds it, once it entered, and the rings of its groups.
    pub pad_object: Option<ZwpTabletPadV2>,
    pub pad_rings: Vec<ZwpTabletPadRingV2>,
    /// The serial of the pad's latest `enter` or `mode_switch`, which its labels name.
    pub pad_serial: u32,
    /// The colour manager, and what it says it can build.
    pub color_manager: Option<WpColorManagerV1>,
    pub color_features: Vec<u32>,
    pub color_primaries: Vec<u32>,
    pub color_transfers: Vec<u32>,
    pub color_manager_done: bool,
    /// Whether an image description the client asked for is ready.
    pub color_description_ready: bool,
    pub data_manager: Option<WlDataDeviceManager>,
    pub data_device: Option<WlDataDevice>,
    /// The serial of the last button press on the window.
    pub pressed: Option<u32>,
    /// The serial of the last touch down on the window.
    pub touched: Option<u32>,
    /// The formats of the latest offer.
    pub offered: Vec<String>,
    /// The latest offer the data device made, whose formats are `offered`.
    pub selection: Option<WlDataOffer>,
    /// A drag entered the window with this offer.
    pub entered: Option<WlDataOffer>,
    pub dropped: bool,
    /// What this client's data source serves when asked.
    pub payload: Vec<u8>,
    /// The formats the compositor asked this client's data source for.
    pub sent: Vec<String>,
    /// How this client's drag ended: `action move`, `performed`, `finished`, `cancelled`.
    pub source_events: Vec<String>,
    pub decoration_manager: Option<ZxdgDecorationManagerV1>,
    pub kde_manager: Option<OrgKdeKwinServerDecorationManager>,
    /// Every mode the compositor configured through `xdg-decoration` , in order.
    pub xdg_modes: Vec<XdgMode>,
    /// The KDE manager's `default_mode` : what it tells a client that asks for nothing.
    pub kde_default: Option<KdeDefaultMode>,
    /// Every mode the compositor stated through the KDE protocol, in order.
    pub kde_modes: Vec<KdeMode>,
    pub layer_shell: Option<ZwlrLayerShellV1>,
    /// The size the compositor gave the layer surface.
    pub layer_size: Option<(u32, u32)>,
    /// How many configures the layer surface has had.
    pub layer_configures: u32,
    /// Where the pointer is on the surface it is over, in that surface's coordinates.
    pub pointer_at: Option<(f64, f64)>,
}

/// What a window drop takes, as a file manager does.
pub const URI_LIST: &str = "text/uri-list";

/// One client with one toplevel of a solid colour.
pub struct TestClient {
    pub connection: Connection,
    pub queue: EventQueue<Seen>,
    pub seen: Seen,
    pub surface: Option<WlSurface>,
    pub toplevel: Option<XdgToplevel>,
}

impl TestClient {
    pub fn connect(compositor: &mut Compositor) -> Self {
        Self::connect_as(compositor, false)
    }

    /// Connect as a client the session started, which may be its input method.
    pub fn connect_trusted(compositor: &mut Compositor) -> Self {
        Self::connect_as(compositor, true)
    }

    pub fn connect_as(compositor: &mut Compositor, trusted: bool) -> Self {
        let socket = if trusted {
            compositor.connect_trusted_client().expect("client socket")
        } else {
            compositor.connect_client().expect("client socket")
        };
        Self::over(socket)
    }

    /// Connect over a connection the session handed to the process `pid` itself, as it hands one
    /// to an app keeper.
    pub fn connect_for(compositor: &Compositor, pid: u32) -> Self {
        Self::over(
            compositor
                .client_connector()
                .connect_for(pid)
                .expect("client socket"),
        )
    }

    /// Connect over a connection the session made for a spare window process, which the session
    /// binds to an app through what comes back with it.
    pub fn connect_spare(compositor: &Compositor) -> (Self, tilo_compositor::SpareConnection) {
        let (socket, spare) = compositor
            .client_connector()
            .connect_spare()
            .expect("client socket");
        (Self::over(socket), spare)
    }

    fn over(socket: std::os::unix::net::UnixStream) -> Self {
        let connection = Connection::from_socket(socket).expect("connection");
        let queue = connection.new_event_queue();
        connection.display().get_registry(&queue.handle(), ());
        Self {
            connection,
            queue,
            seen: Seen::default(),
            surface: None,
            toplevel: None,
        }
    }

    /// Send what is queued, read what arrived, without blocking.
    pub fn exchange(&mut self) {
        self.connection.flush().expect("flush");
        if let Some(guard) = self.connection.prepare_read() {
            // Nothing to read is fine: WouldBlock.
            let _ = guard.read();
        }
        self.queue
            .dispatch_pending(&mut self.seen)
            .expect("dispatch");
        self.connection.flush().expect("flush");
    }

    /// Create a surface and its toplevel without committing it, so a test can ask for things the
    /// first configure answers.
    pub fn create_window(&mut self, app_id: &str) {
        let qh = self.queue.handle();
        let surface = self
            .seen
            .compositor
            .as_ref()
            .expect("wl_compositor")
            .create_surface(&qh, ());
        let xdg = self
            .seen
            .wm_base
            .as_ref()
            .expect("xdg_wm_base")
            .get_xdg_surface(&surface, &qh, ());
        let toplevel = xdg.get_toplevel(&qh, ());
        toplevel.set_app_id(app_id.into());
        self.surface = Some(surface);
        self.toplevel = Some(toplevel);
    }

    pub fn open_window(&mut self, app_id: &str) {
        self.create_window(app_id);
        self.commit();
    }

    pub fn commit(&self) {
        self.surface.as_ref().expect("surface").commit();
    }

    /// Ask for the frame to be drawn by the client, which is the request the desktop refuses.
    pub fn ask_for_client_side(&self) {
        let qh = self.queue.handle();
        let manager = self
            .seen
            .decoration_manager
            .as_ref()
            .expect("zxdg_decoration_manager_v1 is not advertised");
        let toplevel = self.toplevel.as_ref().expect("toplevel");
        manager
            .get_toplevel_decoration(toplevel, &qh, ())
            .set_mode(XdgMode::ClientSide);
    }

    /// Create a KDE decoration for the window, stating no preference on it.
    pub fn decorate_kde(&self) -> kde_decoration::OrgKdeKwinServerDecoration {
        let qh = self.queue.handle();
        let manager = self
            .seen
            .kde_manager
            .as_ref()
            .expect("org_kde_kwin_server_decoration_manager is not advertised");
        manager.create(self.surface.as_ref().expect("surface"), &qh, ())
    }

    /// Bind this client's data device, through which it copies, pastes and drags, and let the
    /// compositor have it before any input follows.
    pub fn own_data_device(&mut self, compositor: &mut Compositor) {
        let qh = self.queue.handle();
        let manager = self
            .seen
            .data_manager
            .as_ref()
            .expect("wl_data_device_manager");
        let seat = self.seen.seat.as_ref().expect("wl_seat");
        self.seen.data_device = Some(manager.get_data_device(seat, &qh, ()));
        settle(compositor, self);
    }

    /// A data source offering `formats`, serving `payload` for each.
    pub fn data_source(&mut self, formats: &[&str], payload: &[u8]) -> WlDataSource {
        let qh = self.queue.handle();
        let source = self
            .seen
            .data_manager
            .as_ref()
            .expect("wl_data_device_manager")
            .create_data_source(&qh, ());
        for mime in formats {
            source.offer((*mime).into());
        }
        self.seen.payload = payload.to_vec();
        source
    }

    /// Drag `formats` out of the window from the press or touch with `serial`, serving `payload`
    /// for each and showing `icon` under the pointer.
    pub fn start_drag(
        &mut self,
        formats: &[&str],
        payload: &[u8],
        action: DndAction,
        serial: u32,
        icon: Option<&WlSurface>,
    ) -> WlDataSource {
        let source = self.data_source(formats, payload);
        source.set_actions(action);
        self.seen
            .data_device
            .as_ref()
            .expect("a data device")
            .start_drag(
                Some(&source),
                self.surface.as_ref().expect("surface"),
                icon,
                serial,
            );
        source
    }

    /// Ask the compositor to keep the screen on while this window shows: what a video player does.
    pub fn inhibit_idle(&mut self) {
        let qh = self.queue.handle();
        let manager = self
            .seen
            .idle_inhibit
            .as_ref()
            .expect("zwp_idle_inhibit_manager_v1");
        let surface = self.surface.as_ref().expect("surface");
        self.seen.idle_inhibitor = Some(manager.create_inhibitor(surface, &qh, ()));
    }

    /// Attach a buffer of one colour (RGBA) and commit it with a frame callback.
    pub fn show(&self, width: i32, height: i32, rgba: [u8; 4]) {
        // ARGB8888 is B, G, R, A in memory.
        let word = u32::from_le_bytes([rgba[2], rgba[1], rgba[0], rgba[3]]);
        self.show_words(width, height, wl_shm::Format::Argb8888, word);
    }

    /// Attach a buffer of `format` holding `word` in every pixel, and commit it with a frame
    /// callback.
    pub fn show_words(&self, width: i32, height: i32, format: wl_shm::Format, word: u32) {
        self.show_pixels(width, height, format, &word.to_le_bytes());
    }

    /// Attach a buffer of `format` holding the bytes of `pixel` in every pixel, and commit it with
    /// a frame callback.
    pub fn show_pixels(&self, width: i32, height: i32, format: wl_shm::Format, pixel: &[u8]) {
        self.show_rows(width, height, 1, format, |_| pixel);
    }

    /// Attach a buffer of `size` logical pixels at `scale` , its top half `top` and its bottom half
    /// `bottom` (RGBA), and commit it with a frame callback.
    pub fn show_bands(&self, size: (i32, i32), scale: i32, top: [u8; 4], bottom: [u8; 4]) {
        // ARGB8888 is B, G, R, A in memory.
        let argb = |rgba: [u8; 4]| [rgba[2], rgba[1], rgba[0], rgba[3]];
        let (top, bottom) = (argb(top), argb(bottom));
        let height = size.1 * scale;
        self.show_rows(
            size.0 * scale,
            height,
            scale,
            wl_shm::Format::Argb8888,
            |y| {
                if y < height / 2 { &top } else { &bottom }
            },
        );
    }

    /// Attach a buffer whose row `y` repeats the pixel `row(y)` gives, at `scale`, and commit it
    /// with a frame callback.
    fn show_rows<'a>(
        &self,
        width: i32,
        height: i32,
        scale: i32,
        format: wl_shm::Format,
        row: impl Fn(i32) -> &'a [u8],
    ) {
        let surface = self.surface.as_ref().expect("surface");
        self.fill_rows(surface, width, height, scale, format, row);
    }

    /// Attach a buffer to `surface` whose row `y` repeats the pixel `row(y)` gives, at `scale`,
    /// and commit it with a frame callback.
    fn fill_rows<'a>(
        &self,
        surface: &WlSurface,
        width: i32,
        height: i32,
        scale: i32,
        format: wl_shm::Format,
        row: impl Fn(i32) -> &'a [u8],
    ) {
        let qh = self.queue.handle();
        let pixel_len = row(0).len();
        let stride = width * i32::try_from(pixel_len).unwrap();
        let fd = rustix::fs::memfd_create("tilo-compositor-test", rustix::fs::MemfdFlags::CLOEXEC)
            .unwrap();
        let mut file = std::fs::File::from(fd);
        for y in 0..height {
            file.write_all(&row(y).repeat(usize::try_from(width).unwrap()))
                .unwrap();
        }
        surface.set_buffer_scale(scale);
        let pool = self.seen.shm.as_ref().expect("wl_shm").create_pool(
            file.as_fd(),
            stride * height,
            &qh,
            (),
        );
        let buffer = pool.create_buffer(0, width, height, stride, format, &qh, ());
        surface.attach(Some(&buffer), 0, 0);
        surface.damage_buffer(0, 0, width, height);
        surface.frame(&qh, ());
        surface.commit();
        pool.destroy();
    }

    /// Show `rgba` in every pixel of `surface` , `size` square, and commit it.
    pub fn fill(&self, surface: &WlSurface, size: i32, rgba: [u8; 4]) {
        // ARGB8888 is B, G, R, A in memory.
        let word = u32::from_le_bytes([rgba[2], rgba[1], rgba[0], rgba[3]]);
        let pixel = word.to_le_bytes();
        self.fill_rows(surface, size, size, 1, wl_shm::Format::Argb8888, |_| {
            pixel.as_slice()
        });
    }

    /// Create a surface showing `rgba` everywhere, as the cursor image a tool names.
    pub fn tool_cursor(&self, size: i32, rgba: [u8; 4]) -> WlSurface {
        let surface = self
            .seen
            .compositor
            .as_ref()
            .expect("wl_compositor")
            .create_surface(&self.queue.handle(), ());
        self.fill(&surface, size, rgba);
        surface.commit();
        surface
    }

    /// Put `surface` under the tool's hotspot at that point, as a pen's app names its cursor.
    pub fn set_tool_cursor(&self, surface: &WlSurface, hotspot: (i32, i32)) {
        let tool = self.seen.tablet_tools.last().expect("a tablet tool");
        tool.set_cursor(
            self.seen.tool_proximity_serial,
            Some(surface),
            hotspot.0,
            hotspot.1,
        );
    }

    /// Name the cursor shape the compositor draws for the tool, as a pen's app does.
    pub fn set_tool_shape(&self, shape: Shape) {
        let qh = self.queue.handle();
        let device = self
            .seen
            .cursor_shape
            .as_ref()
            .expect("wp_cursor_shape_manager_v1")
            .get_tablet_tool_v2(
                self.seen.tablet_tools.last().expect("a tablet tool"),
                &qh,
                (),
            );
        device.set_shape(self.seen.tool_proximity_serial, shape);
    }

    /// Ask for a layer surface on `layer` : `size` (0 for the whole output along an axis), anchored
    /// to `anchor` , keeping `exclusive` pixels of the edge it hangs from.
    pub fn layer_surface(
        &mut self,
        layer: zwlr_layer_shell_v1::Layer,
        anchor: zwlr_layer_surface_v1::Anchor,
        size: (u32, u32),
        exclusive: i32,
        keyboard: zwlr_layer_surface_v1::KeyboardInteractivity,
    ) -> (WlSurface, ZwlrLayerSurfaceV1) {
        let qh = self.queue.handle();
        let surface = self
            .seen
            .compositor
            .as_ref()
            .expect("wl_compositor")
            .create_surface(&qh, ());
        let shell = self.seen.layer_shell.as_ref().expect("zwlr_layer_shell_v1");
        let layer_surface = shell.get_layer_surface(&surface, None, layer, "test".into(), &qh, ());
        layer_surface.set_anchor(anchor);
        layer_surface.set_size(size.0, size.1);
        layer_surface.set_exclusive_zone(exclusive);
        layer_surface.set_keyboard_interactivity(keyboard);
        surface.commit();
        self.surface = Some(surface.clone());
        (surface, layer_surface)
    }

    pub fn icon_buffer(&self, edge: i32, rgba: [u8; 4]) -> WlBuffer {
        let qh = self.queue.handle();
        let stride = edge * 4;
        let len = usize::try_from(stride * edge).unwrap();
        let fd =
            rustix::fs::memfd_create("tilo-icon-test", rustix::fs::MemfdFlags::CLOEXEC).unwrap();
        let mut file = std::fs::File::from(fd);
        // ARGB8888 is B, G, R, A in memory.
        let pixel = [rgba[2], rgba[1], rgba[0], rgba[3]];
        file.write_all(&pixel.repeat(len / 4)).unwrap();
        let pool = self.seen.shm.as_ref().expect("wl_shm").create_pool(
            file.as_fd(),
            stride * edge,
            &qh,
            (),
        );
        let buffer = pool.create_buffer(0, edge, edge, stride, wl_shm::Format::Argb8888, &qh, ());
        pool.destroy();
        buffer
    }

    /// Give the window a text field: a `text-input-v3` object on its surface, waiting for typing.
    pub fn own_text_field(&mut self) {
        self.own_text_field_as(ContentHint::empty(), ContentPurpose::Normal);
    }

    /// Give the window a text field that declares what it takes.
    pub fn own_text_field_as(&mut self, hint: ContentHint, purpose: ContentPurpose) {
        let qh = self.queue.handle();
        let manager = self
            .seen
            .text_input_manager
            .as_ref()
            .expect("zwp_text_input_manager_v3");
        let seat = self.seen.seat.as_ref().expect("wl_seat");
        let field = manager.get_text_input(seat, &qh, ());
        field.set_content_type(hint, purpose);
        field.enable();
        field.commit();
        self.seen.text_input = Some(field);
    }

    /// Change what the field declares itself to be, as an app does when the person moves to another
    /// box.
    pub fn set_content_type(&self, hint: ContentHint, purpose: ContentPurpose) {
        if let Some(field) = self.seen.text_input.as_ref() {
            field.set_content_type(hint, purpose);
            field.commit();
        }
        self.connection.flush().expect("flush");
    }

    /// Move to another field of the same window: the client enables the text-input object again for
    /// it, as it does when the person tabs on.
    pub fn focus_another_field(&self, hint: ContentHint, purpose: ContentPurpose) {
        if let Some(field) = self.seen.text_input.as_ref() {
            field.set_content_type(hint, purpose);
            field.enable();
            field.commit();
        }
        self.connection.flush().expect("flush");
    }

    /// Move the field's caret: a commit that changes nothing about which field this is, as any edit
    /// does.
    pub fn move_caret(&self, x: i32) {
        if let Some(field) = self.seen.text_input.as_ref() {
            field.set_cursor_rectangle(x, 0, 1, 20);
            field.commit();
        }
        self.connection.flush().expect("flush");
    }

    /// Stop the field from asking for typing.
    pub fn release_text_field(&self) {
        if let Some(field) = self.seen.text_input.as_ref() {
            field.disable();
            field.commit();
        }
        self.connection.flush().expect("flush");
    }

    /// Serve the seat as its input method (the on-screen keyboard).
    pub fn own_input_method(&mut self) {
        let qh = self.queue.handle();
        let manager = self
            .seen
            .input_method_manager
            .as_ref()
            .expect("zwp_input_method_manager_v2");
        let seat = self.seen.seat.as_ref().expect("wl_seat");
        self.seen.input_method = Some(manager.get_input_method(seat, &qh, ()));
    }

    /// Hold the seat's keyboard, as an input method does while it composes: what the person types
    /// reaches it first.
    pub fn grab_keyboard(&mut self) {
        let qh = self.queue.handle();
        let method = self.seen.input_method.as_ref().expect("input method");
        self.seen.grab = Some(method.grab_keyboard(&qh, ()));
        self.connection.flush().expect("flush");
    }

    /// Send the composition in progress: `text` , with its cursor or selection at `(begin, end)`
    /// bytes.
    pub fn preedit_with_input_method(&self, text: &str, cursor: Option<(i32, i32)>) {
        let method = self.seen.input_method.as_ref().expect("input method");
        let (begin, end) = cursor.unwrap_or((-1, -1));
        method.set_preedit_string(text.to_owned(), begin, end);
        method.commit(self.seen.ime_done);
        self.connection.flush().expect("flush");
    }

    /// Type `key` through the input method's virtual keyboard, as an engine does with the keys it
    /// does not compose itself. `keymap` is the one the person's keyboard was given: the compositor
    /// reads keys by it.
    pub fn virtual_key(&mut self, keymap: &str, key: u32, pressed: bool) {
        let qh = self.queue.handle();
        let manager = self
            .seen
            .virtual_keyboard_manager
            .as_ref()
            .expect("zwp_virtual_keyboard_manager_v1")
            .clone();
        let keyboard = if let Some(keyboard) = self.seen.virtual_keyboard.as_ref() {
            keyboard.clone()
        } else {
            let keyboard =
                manager.create_virtual_keyboard(self.seen.seat.as_ref().expect("wl_seat"), &qh, ());
            self.upload_keymap(&keyboard, keymap);
            self.seen.virtual_keyboard = Some(keyboard.clone());
            keyboard
        };
        // The protocol's key codes sit 8 above the evdev ones.
        keyboard.key(
            u32::from(pressed),
            key.saturating_add(8),
            u32::from(pressed),
        );
        self.connection.flush().expect("flush");
    }

    /// Give a virtual keyboard the keymap to type in.
    fn upload_keymap(&self, keyboard: &ZwpVirtualKeyboardV1, keymap: &str) {
        let fd = rustix::fs::memfd_create(
            "tilo-virtual-keyboard-test",
            rustix::fs::MemfdFlags::CLOEXEC,
        )
        .expect("memfd");
        let mut file = std::fs::File::from(fd);
        file.write_all(keymap.as_bytes()).expect("write keymap");
        file.write_all(&[0]).expect("terminate keymap");
        keyboard.keymap(
            KEYMAP_FORMAT_XKB_V1,
            file.as_fd(),
            u32::try_from(keymap.len() + 1).expect("a keymap small enough"),
        );
        self.connection.flush().expect("flush");
    }

    /// Open the candidate window's surface on this input method.
    pub fn own_popup(&mut self) {
        let qh = self.queue.handle();
        let surface = self
            .seen
            .compositor
            .as_ref()
            .expect("wl_compositor")
            .create_surface(&qh, ());
        let method = self.seen.input_method.as_ref().expect("input method");
        self.seen.popup = Some(method.get_input_popup_surface(&surface, &qh, ()));
        self.seen.popup_surface = Some(surface);
        self.connection.flush().expect("flush");
    }

    /// Draw the candidate window as one square of one colour, where the compositor put it.
    pub fn paint_popup(&self, edge: i32, rgba: [u8; 4]) {
        let surface = self.seen.popup_surface.as_ref().expect("popup surface");
        let buffer = self.icon_buffer(edge, rgba);
        surface.attach(Some(&buffer), 0, 0);
        surface.damage_buffer(0, 0, edge, edge);
        surface.commit();
        self.connection.flush().expect("flush");
    }

    /// Ask for the pointer to stay where it is while this window has it: a game or a remote desktop
    /// view.
    pub fn lock_pointer(&mut self) {
        let qh = self.queue.handle();
        let constraints = self
            .seen
            .pointer_constraints
            .as_ref()
            .expect("zwp_pointer_constraints_v1");
        let surface = self.surface.as_ref().expect("surface").clone();
        let pointer = self.seen.pointer.as_ref().expect("wl_pointer").clone();
        self.seen.locked_pointer =
            Some(constraints.lock_pointer(&surface, &pointer, None, Lifetime::Persistent, &qh, ()));
        self.connection.flush().expect("flush");
    }

    /// Ask for the pointer to stay inside `rect` of this window, as a remote desktop view does.
    pub fn confine_pointer(&mut self, rect: (i32, i32, i32, i32)) {
        let qh = self.queue.handle();
        let constraints = self
            .seen
            .pointer_constraints
            .as_ref()
            .expect("zwp_pointer_constraints_v1");
        let region = self
            .seen
            .compositor
            .as_ref()
            .expect("wl_compositor")
            .create_region(&qh, ());
        region.add(rect.0, rect.1, rect.2, rect.3);
        let surface = self.surface.as_ref().expect("surface").clone();
        let pointer = self.seen.pointer.as_ref().expect("wl_pointer").clone();
        self.seen.confined_pointer = Some(constraints.confine_pointer(
            &surface,
            &pointer,
            Some(&region),
            Lifetime::Persistent,
            &qh,
            (),
        ));
        region.destroy();
        // The region takes effect with the surface's next commit.
        surface.commit();
        self.connection.flush().expect("flush");
    }

    /// Read the deltas the compositor sends for the pointer of this seat.
    pub fn own_relative_pointer(&mut self) {
        let qh = self.queue.handle();
        let manager = self
            .seen
            .relative_pointer_manager
            .as_ref()
            .expect("zwp_relative_pointer_manager_v1");
        let pointer = self.seen.pointer.as_ref().expect("wl_pointer");
        self.seen.relative_pointer = Some(manager.get_relative_pointer(pointer, &qh, ()));
        self.connection.flush().expect("flush");
    }

    /// Ask to answer the desktop's own chords while this window is focused.
    pub fn inhibit_shortcuts(&mut self) {
        let qh = self.queue.handle();
        let manager = self
            .seen
            .inhibit_manager
            .as_ref()
            .expect("zwp_keyboard_shortcuts_inhibit_manager_v1");
        let surface = self.surface.as_ref().expect("surface");
        let seat = self.seen.seat.as_ref().expect("wl_seat");
        self.seen.inhibitor = Some(manager.inhibit_shortcuts(surface, seat, &qh, ()));
        self.connection.flush().expect("flush");
    }

    /// Type `text` into the field the input method serves.
    pub fn type_with_input_method(&self, text: &str) {
        let method = self.seen.input_method.as_ref().expect("input method");
        method.commit_string(text.to_owned());
        method.commit(self.seen.ime_done);
        self.connection.flush().expect("flush");
    }
}

/// Draw a frame and read back the pixel at `(x, y)` of the one output.
pub fn shown(compositor: &mut Compositor, x: u32, y: u32) -> Option<[u8; 4]> {
    compositor.draw_frame().expect("a frame");
    let frame = compositor.screenshot("HEADLESS-1").expect("the frame");
    frame.pixel(x, y)
}

/// Draw a frame and read back the pixel at `(x, y)` of the one output, with the pointer and every
/// pen's cursor in front as a display draws them.
pub fn shown_with_cursor(compositor: &mut Compositor, x: u32, y: u32) -> Option<[u8; 4]> {
    compositor.draw_frame().expect("a frame");
    let frame = compositor
        .screenshot_with_cursor("HEADLESS-1")
        .expect("the frame");
    frame.pixel(x, y)
}

pub fn input(compositor: &mut Compositor, event: InputEvent) {
    compositor.input(event).expect("input");
}

#[derive(Default)]
pub struct DropLog {
    pub offered: Vec<Vec<String>>,
    pub left: u32,
    pub dropped: Vec<(String, Vec<u8>)>,
    pub buttons: Vec<(u32, bool)>,
    pub drag_ended: Option<bool>,
}

/// A surface at (500, 20, 100, 100) that takes file lists, as a surface app does.
pub struct DropZone {
    pixels: Vec<u8>,
    log: Rc<RefCell<DropLog>>,
}

impl DropZone {
    const RECT: (f64, f64, f64, f64) = (500.0, 20.0, 100.0, 100.0);

    pub fn new(log: Rc<RefCell<DropLog>>) -> Self {
        Self {
            pixels: [0, 255, 0, 255].repeat(100 * 100),
            log,
        }
    }
}

impl ShellSurface for DropZone {
    fn layers(&self) -> Vec<SurfaceLayer> {
        vec![SurfaceLayer {
            pose: tilo_session_core::surface::LayerPose::SETTLED,
            over_windows: false,
            location: (500, 20),
            size: (100, 100),
            frame: SurfaceFrame::from_pixels((100, 100), self.pixels.clone()),
        }]
    }

    fn accepts_pointer(&self, x: f64, y: f64) -> bool {
        let (rx, ry, w, h) = Self::RECT;
        x >= rx && y >= ry && x < rx + w && y < ry + h
    }

    fn pointer_motion(&mut self, _x: f64, _y: f64) {}

    fn pointer_button(&mut self, button: u32, pressed: bool) {
        self.log.borrow_mut().buttons.push((button, pressed));
    }

    fn drop_offered(&mut self, mimes: &[String], _x: f64, _y: f64) -> Option<String> {
        self.log.borrow_mut().offered.push(mimes.to_vec());
        mimes.iter().find(|mime| *mime == URI_LIST).cloned()
    }

    fn drop_left(&mut self) {
        self.log.borrow_mut().left += 1;
    }

    fn drop_data(&mut self, mime: &str, data: Vec<u8>) {
        self.log.borrow_mut().dropped.push((mime.to_owned(), data));
    }

    fn drag_ended(&mut self, dropped: bool) {
        self.log.borrow_mut().drag_ended = Some(dropped);
    }
}

/// An X11 client on `display` : Xwayland answers the connection's setup only after it has
/// roundtripped with the compositor, so the client waits in a thread while the compositor
/// dispatches here.
pub fn x11_client(compositor: &mut Compositor, display: u32) -> (RustConnection, Window) {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(x11rb::connect(Some(&format!(":{display}"))));
    });
    let started = Instant::now();
    loop {
        compositor
            .dispatch(Duration::from_millis(2))
            .expect("dispatch");
        if let Ok(connected) = receiver.try_recv() {
            let (x11, screen) = connected.expect("X11 connection");
            let root = x11.setup().roots[screen].root;
            return (x11, root);
        }
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "Xwayland never answered the X11 client"
        );
    }
}

/// Let compositor and one client talk until `done` holds.
pub fn pump_one(
    compositor: &mut Compositor,
    client: &mut TestClient,
    mut done: impl FnMut(&Compositor, &TestClient) -> bool,
) {
    pump(compositor, &mut [client], |compositor, clients| {
        done(compositor, clients[0])
    });
}

/// Let compositor and a client exchange a few rounds.
pub fn settle(compositor: &mut Compositor, client: &mut TestClient) {
    let mut rounds = 0;
    pump_one(compositor, client, |_, _| {
        rounds += 1;
        rounds >= 5
    });
}

/// Let compositor and clients talk until `done` holds.
pub fn pump(
    compositor: &mut Compositor,
    clients: &mut [&mut TestClient],
    mut done: impl FnMut(&Compositor, &[&mut TestClient]) -> bool,
) {
    for _ in 0..500 {
        for client in clients.iter_mut() {
            client.exchange();
        }
        compositor
            .dispatch(Duration::from_millis(2))
            .expect("dispatch");
        for client in clients.iter_mut() {
            client.exchange();
        }
        if done(compositor, clients) {
            return;
        }
    }
    panic!("condition not reached");
}

pub fn headless(policy: Box<dyn WindowPolicy>) -> Compositor {
    let mut compositor = Compositor::headless(policy).expect("compositor");
    compositor
        .add_output(&OutputConfig::new("HEADLESS-1", 640, 480))
        .expect("output");
    compositor
}

/// Open a toplevel of `size` and colour, and wait until it is mapped.
pub fn mapped_client(
    compositor: &mut Compositor,
    app_id: &str,
    size: (i32, i32),
    rgba: [u8; 4],
) -> TestClient {
    map(
        TestClient::connect(compositor),
        compositor,
        app_id,
        size,
        rgba,
    )
}

/// A mapped window whose client never binds `wl_touch` , as a toolkit that only knows the pointer
/// does.
pub fn mapped_client_without_touch(
    compositor: &mut Compositor,
    app_id: &str,
    size: (i32, i32),
    rgba: [u8; 4],
) -> TestClient {
    let mut client = TestClient::connect(compositor);
    client.seen.no_touch = true;
    map(client, compositor, app_id, size, rgba)
}

/// Put `client` 's window up: a surface of `size` in `rgba` , mapped as `app_id` once the
/// compositor has it.
pub fn map(
    mut client: TestClient,
    compositor: &mut Compositor,
    app_id: &str,
    size: (i32, i32),
    rgba: [u8; 4],
) -> TestClient {
    pump(compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.wm_base.is_some() && clients[0].seen.seat.is_some()
    });
    client.open_window(app_id);
    pump(compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.configured
    });
    client.show(size.0, size.1, rgba);
    let app = app_id.to_owned();
    pump(compositor, &mut [&mut client], |compositor, _| {
        compositor
            .windows()
            .iter()
            .any(|(info, _)| info.app_id.as_deref() == Some(app.as_str()))
    });
    client
}

impl Dispatch<WlRegistry, ()> for Seen {
    fn event(
        seen: &mut Self,
        registry: &WlRegistry,
        event: wl_registry::Event,
        (): &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        {
            seen.globals.push(interface.clone());
            match interface.as_str() {
                "wl_compositor" => {
                    seen.compositor = Some(registry.bind(name, version.min(5), qh, ()));
                }
                "ext_session_lock_manager_v1" => {
                    seen.lock_manager = Some(registry.bind(name, 1, qh, ()));
                }
                "wl_shm" => seen.shm = Some(registry.bind(name, 1, qh, ())),
                "xdg_wm_base" => seen.wm_base = Some(registry.bind(name, 1, qh, ())),
                "wl_seat" => seen.seat = Some(registry.bind(name, version.min(7), qh, ())),
                "zwp_tablet_manager_v2" => {
                    seen.tablet_manager = Some(registry.bind(name, 1, qh, ()));
                }
                "wp_cursor_shape_manager_v1" => {
                    seen.cursor_shape = Some(registry.bind(name, version.min(2), qh, ()));
                }
                "xdg_wm_dialog_v1" => seen.wm_dialog = Some(registry.bind(name, 1, qh, ())),
                "xdg_toplevel_icon_manager_v1" => {
                    seen.icon_manager = Some(registry.bind(name, 1, qh, ()));
                }
                "xdg_toplevel_tag_manager_v1" => {
                    seen.tag_manager = Some(registry.bind(name, 1, qh, ()));
                }
                "zwp_text_input_manager_v3" => {
                    seen.text_input_manager = Some(registry.bind(name, 1, qh, ()));
                }
                "zwp_input_method_manager_v2" => {
                    seen.input_method_manager = Some(registry.bind(name, 1, qh, ()));
                }
                "zwp_virtual_keyboard_manager_v1" => {
                    seen.virtual_keyboard_manager = Some(registry.bind(name, 1, qh, ()));
                }
                "zwp_pointer_constraints_v1" => {
                    seen.pointer_constraints = Some(registry.bind(name, 1, qh, ()));
                }
                "zwp_relative_pointer_manager_v1" => {
                    seen.relative_pointer_manager = Some(registry.bind(name, 1, qh, ()));
                }
                "zwp_keyboard_shortcuts_inhibit_manager_v1" => {
                    seen.inhibit_manager = Some(registry.bind(name, 1, qh, ()));
                }
                "zwp_idle_inhibit_manager_v1" => {
                    seen.idle_inhibit = Some(registry.bind(name, 1, qh, ()));
                }
                "zwlr_layer_shell_v1" => {
                    seen.layer_shell = Some(registry.bind(name, 1, qh, ()));
                }
                "zxdg_decoration_manager_v1" => {
                    seen.decoration_manager = Some(registry.bind(name, 1, qh, ()));
                }
                "org_kde_kwin_server_decoration_manager" => {
                    seen.kde_manager = Some(registry.bind(name, 1, qh, ()));
                }
                "wl_data_device_manager" => {
                    seen.data_manager = Some(registry.bind(name, version.min(3), qh, ()));
                }
                "wp_color_manager_v1" => {
                    seen.color_manager = Some(registry.bind(name, version.min(1), qh, ()));
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<XdgToplevelIconManagerV1, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &XdgToplevelIconManagerV1,
        event: xdg_toplevel_icon_manager_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_toplevel_icon_manager_v1::Event::IconSize { size } = event {
            seen.icon_sizes.push(size);
        }
    }
}

delegate_noop!(Seen: ignore XdgToplevelIconV1);
delegate_noop!(Seen: ignore XdgToplevelTagManagerV1);
delegate_noop!(Seen: ignore ZwpIdleInhibitManagerV1);
delegate_noop!(Seen: ignore ZwpIdleInhibitorV1);

impl Dispatch<XdgWmBase, ()> for Seen {
    fn event(
        _: &mut Self,
        base: &XdgWmBase,
        event: xdg_wm_base::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            base.pong(serial);
        }
    }
}

impl Dispatch<XdgSurface, ()> for Seen {
    fn event(
        seen: &mut Self,
        surface: &XdgSurface,
        event: xdg_surface::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            surface.ack_configure(serial);
            seen.configured = true;
        }
    }
}

impl Dispatch<XdgToplevel, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &XdgToplevel,
        event: xdg_toplevel::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if matches!(event, xdg_toplevel::Event::Close) {
            seen.closed = true;
        }
    }
}

impl Dispatch<WlSeat, ()> for Seen {
    fn event(
        seen: &mut Self,
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
            if capabilities.contains(wl_seat::Capability::Pointer) && seen.pointer.is_none() {
                seen.pointer = Some(seat.get_pointer(qh, ()));
            }
            if capabilities.contains(wl_seat::Capability::Keyboard) && seen.keyboard.is_none() {
                seen.keyboard = Some(seat.get_keyboard(qh, ()));
            }
            if capabilities.contains(wl_seat::Capability::Touch)
                && seen.touch.is_none()
                && !seen.no_touch
            {
                seen.touch = Some(seat.get_touch(qh, ()));
            }
        }
    }
}

impl Dispatch<WlPointer, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &WlPointer,
        event: wl_pointer::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_pointer::Event::Enter {
                surface_x,
                surface_y,
                ..
            } => {
                seen.pointer_inside = true;
                seen.pointer_entered_at = Some((surface_x, surface_y));
                seen.pointer_at = Some((surface_x, surface_y));
            }
            wl_pointer::Event::Leave { .. } => seen.pointer_inside = false,
            wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => {
                seen.pointer_positions.push((surface_x, surface_y));
                seen.pointer_at = Some((surface_x, surface_y));
            }
            wl_pointer::Event::Button {
                serial,
                button,
                state,
                ..
            } => {
                if state == WEnum::Value(wl_pointer::ButtonState::Pressed) {
                    seen.pressed = Some(serial);
                }
                seen.buttons.push((
                    button,
                    state == WEnum::Value(wl_pointer::ButtonState::Pressed),
                ));
            }
            _ => {}
        }
    }
}

impl Dispatch<WlTouch, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &WlTouch,
        event: wl_touch::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_touch::Event::Down {
                serial, id, x, y, ..
            } => {
                seen.touched = Some(serial);
                seen.touches.push(format!("down {id} {x} {y}"));
            }
            wl_touch::Event::Motion { id, x, y, .. } => {
                seen.touches.push(format!("motion {id} {x} {y}"));
            }
            wl_touch::Event::Up { id, .. } => seen.touches.push(format!("up {id}")),
            wl_touch::Event::Frame => seen.touches.push("frame".into()),
            wl_touch::Event::Cancel => seen.touches.push("cancel".into()),
            _ => {}
        }
    }
}

impl Dispatch<WlKeyboard, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &WlKeyboard,
        event: wl_keyboard::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_keyboard::Event::Enter { .. } => seen.keyboard_focused = true,
            wl_keyboard::Event::Leave { .. } => seen.keyboard_focused = false,
            wl_keyboard::Event::Key { key, state, .. } => {
                let pressed = state == WEnum::Value(wl_keyboard::KeyState::Pressed);
                seen.keys.push((key, pressed));
                if pressed {
                    seen.presses.push((key, seen.modifiers_held));
                }
            }
            wl_keyboard::Event::Modifiers { mods_depressed, .. } => {
                seen.modifiers_held = mods_depressed;
            }
            wl_keyboard::Event::Keymap { fd, size, .. } => {
                let mut text = Vec::new();
                if std::fs::File::from(fd)
                    .take(u64::from(size))
                    .read_to_end(&mut text)
                    .is_ok()
                {
                    seen.keymap = Some(String::from_utf8_lossy(&text).into_owned());
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<WlCallback, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &WlCallback,
        _: wayland_client::protocol::wl_callback::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        seen.frames_done += 1;
    }
}

impl Dispatch<ExtSessionLockV1, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &ExtSessionLockV1,
        event: ext_session_lock_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if matches!(event, ext_session_lock_v1::Event::Locked) {
            seen.locked = true;
        }
    }
}

impl Dispatch<ZwpTabletSeatV2, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &ZwpTabletSeatV2,
        event: zwp_tablet_seat_v2::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwp_tablet_seat_v2::Event::PadAdded { .. } = event {
            seen.pad.push("added".into());
        }
    }

    event_created_child!(Seen, ZwpTabletSeatV2, [
        zwp_tablet_seat_v2::EVT_TABLET_ADDED_OPCODE => (ZwpTabletV2, ()),
        zwp_tablet_seat_v2::EVT_TOOL_ADDED_OPCODE => (ZwpTabletToolV2, ()),
        zwp_tablet_seat_v2::EVT_PAD_ADDED_OPCODE => (ZwpTabletPadV2, ()),
    ]);
}

impl Dispatch<ZwpTabletToolV2, ()> for Seen {
    fn event(
        seen: &mut Self,
        tool: &ZwpTabletToolV2,
        event: zwp_tablet_tool_v2::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_tablet_tool_v2::Event::HardwareSerial {
                hardware_serial_hi,
                hardware_serial_lo,
            } => seen.tool_serial = Some((hardware_serial_hi, hardware_serial_lo)),
            zwp_tablet_tool_v2::Event::HardwareIdWacom {
                hardware_id_hi,
                hardware_id_lo,
            } => seen.tool_wacom_id = Some((hardware_id_hi, hardware_id_lo)),
            zwp_tablet_tool_v2::Event::ProximityIn { serial, .. } => {
                seen.tool_proximity_serial = serial;
                seen.tool_entered = true;
            }
            zwp_tablet_tool_v2::Event::ProximityOut => seen.tool_entered = false,
            _ => {}
        }
        if !seen
            .tablet_tools
            .iter()
            .any(|known| known.id() == tool.id())
        {
            seen.tablet_tools.push(tool.clone());
        }
    }
}

impl Dispatch<WpCursorShapeManagerV1, ()> for Seen {
    fn event(
        _: &mut Self,
        _: &WpCursorShapeManagerV1,
        _: <WpCursorShapeManagerV1 as wayland_client::Proxy>::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpTabletPadV2, ()> for Seen {
    fn event(
        seen: &mut Self,
        pad: &ZwpTabletPadV2,
        event: zwp_tablet_pad_v2::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let line = match event {
            zwp_tablet_pad_v2::Event::Group { .. } => "group".into(),
            zwp_tablet_pad_v2::Event::Buttons { buttons } => format!("buttons {buttons}"),
            zwp_tablet_pad_v2::Event::Done => "done".into(),
            zwp_tablet_pad_v2::Event::Enter {
                surface, serial, ..
            } => {
                seen.pad_surface = Some(surface);
                seen.pad_object = Some(pad.clone());
                seen.pad_serial = serial;
                "enter".into()
            }
            zwp_tablet_pad_v2::Event::Leave { .. } => "leave".into(),
            zwp_tablet_pad_v2::Event::Button { button, state, .. } => {
                let pressed = state == WEnum::Value(zwp_tablet_pad_v2::ButtonState::Pressed);
                format!(
                    "button {button} {}",
                    if pressed { "pressed" } else { "released" }
                )
            }
            zwp_tablet_pad_v2::Event::Removed => "removed".into(),
            _ => return,
        };
        seen.pad.push(line);
    }

    event_created_child!(Seen, ZwpTabletPadV2, [
        zwp_tablet_pad_v2::EVT_GROUP_OPCODE => (ZwpTabletPadGroupV2, ()),
    ]);
}

impl Dispatch<ZwpTextInputV3, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &ZwpTextInputV3,
        event: zwp_text_input_v3::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_text_input_v3::Event::Enter { .. } => seen.field_entered = true,
            zwp_text_input_v3::Event::Leave { .. } => seen.field_entered = false,
            zwp_text_input_v3::Event::CommitString { text: Some(text) } => {
                seen.committed.push(text);
            }
            zwp_text_input_v3::Event::Done { .. } => seen.field_done += 1,
            _ => {}
        }
    }
}

impl Dispatch<ZwpInputMethodV2, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &ZwpInputMethodV2,
        event: zwp_input_method_v2::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_input_method_v2::Event::Activate => seen.ime_activated = true,
            zwp_input_method_v2::Event::Deactivate => seen.ime_activated = false,
            zwp_input_method_v2::Event::Done => seen.ime_done += 1,
            _ => {}
        }
    }
}

impl Dispatch<ZwpInputMethodKeyboardGrabV2, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &ZwpInputMethodKeyboardGrabV2,
        event: zwp_input_method_keyboard_grab_v2::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwp_input_method_keyboard_grab_v2::Event::Key { key, state, .. } = event {
            seen.grab_keys.push((
                key,
                matches!(state, WEnum::Value(wl_keyboard::KeyState::Pressed)),
            ));
        }
    }
}

impl Dispatch<ZwpInputPopupSurfaceV2, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &ZwpInputPopupSurfaceV2,
        event: zwp_input_popup_surface_v2::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwp_input_popup_surface_v2::Event::TextInputRectangle {
            x,
            y,
            width,
            height,
        } = event
        {
            seen.popup_rects.push((x, y, width, height));
        }
    }
}

delegate_noop!(Seen: ignore ZwpVirtualKeyboardV1);
delegate_noop!(Seen: ignore ZwpVirtualKeyboardManagerV1);

impl Dispatch<ZwpTabletPadGroupV2, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &ZwpTabletPadGroupV2,
        event: zwp_tablet_pad_group_v2::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_tablet_pad_group_v2::Event::ModeSwitch { mode, serial, .. } => {
                seen.pad_serial = serial;
                seen.pad.push(format!("mode {mode}"));
            }
            zwp_tablet_pad_group_v2::Event::Ring { ring } => seen.pad_rings.push(ring),
            _ => {}
        }
    }

    event_created_child!(Seen, ZwpTabletPadGroupV2, [
        zwp_tablet_pad_group_v2::EVT_RING_OPCODE => (ZwpTabletPadRingV2, ()),
        zwp_tablet_pad_group_v2::EVT_STRIP_OPCODE => (ZwpTabletPadStripV2, ()),
    ]);
}

impl Dispatch<ZwpTabletPadRingV2, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &ZwpTabletPadRingV2,
        event: zwp_tablet_pad_ring_v2::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_tablet_pad_ring_v2::Event::Angle { degrees } => {
                seen.pad.push(format!("ring {degrees}"));
            }
            zwp_tablet_pad_ring_v2::Event::Stop => seen.pad.push("ring stop".into()),
            _ => {}
        }
    }
}

impl Dispatch<ZwpLockedPointerV1, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &ZwpLockedPointerV1,
        event: zwp_locked_pointer_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_locked_pointer_v1::Event::Locked => seen.pointer_locked = true,
            zwp_locked_pointer_v1::Event::Unlocked => seen.pointer_locked = false,
            _ => {}
        }
    }
}

impl Dispatch<ZwpConfinedPointerV1, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &ZwpConfinedPointerV1,
        event: zwp_confined_pointer_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_confined_pointer_v1::Event::Confined => seen.pointer_confined = true,
            zwp_confined_pointer_v1::Event::Unconfined => seen.pointer_confined = false,
            _ => {}
        }
    }
}

impl Dispatch<ZwpRelativePointerV1, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &ZwpRelativePointerV1,
        event: zwp_relative_pointer_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwp_relative_pointer_v1::Event::RelativeMotion { dx, dy, .. } = event {
            seen.relative.push((dx, dy));
        }
    }
}

impl Dispatch<ZwpKeyboardShortcutsInhibitorV1, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &ZwpKeyboardShortcutsInhibitorV1,
        event: zwp_keyboard_shortcuts_inhibitor_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwp_keyboard_shortcuts_inhibitor_v1::Event::Active => seen.shortcuts_inhibited = true,
            zwp_keyboard_shortcuts_inhibitor_v1::Event::Inactive => {
                seen.shortcuts_inhibited = false;
            }
            _ => {}
        }
    }
}

impl Dispatch<WpColorManagerV1, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &WpColorManagerV1,
        event: wp_color_manager_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wp_color_manager_v1::Event::SupportedFeature { feature } => {
                if let Ok(feature) = feature.into_result() {
                    seen.color_features.push(feature as u32);
                }
            }
            wp_color_manager_v1::Event::SupportedPrimariesNamed { primaries } => {
                if let Ok(primaries) = primaries.into_result() {
                    seen.color_primaries.push(primaries as u32);
                }
            }
            wp_color_manager_v1::Event::SupportedTfNamed { tf } => {
                if let Ok(tf) = tf.into_result() {
                    seen.color_transfers.push(tf as u32);
                }
            }
            wp_color_manager_v1::Event::Done => seen.color_manager_done = true,
            _ => {}
        }
    }
}

impl Dispatch<WpImageDescriptionV1, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &WpImageDescriptionV1,
        event: wp_image_description_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if matches!(
            event,
            wp_image_description_v1::Event::Ready { .. }
                | wp_image_description_v1::Event::Ready2 { .. }
        ) {
            seen.color_description_ready = true;
        }
    }
}

delegate_noop!(Seen: ignore WpImageDescriptionCreatorParamsV1);
delegate_noop!(Seen: ignore WpImageDescriptionCreatorIccV1);
delegate_noop!(Seen: ignore WpColorManagementSurfaceV1);
delegate_noop!(Seen: ignore ZwpPointerConstraintsV1);
delegate_noop!(Seen: ignore WpCursorShapeDeviceV1);
delegate_noop!(Seen: ignore ZwpRelativePointerManagerV1);
delegate_noop!(Seen: ignore ZwpKeyboardShortcutsInhibitManagerV1);
delegate_noop!(Seen: ignore WlRegion);
delegate_noop!(Seen: ignore ZwpTabletManagerV2);
delegate_noop!(Seen: ignore ZwpTabletV2);
delegate_noop!(Seen: ignore ZwpTabletPadStripV2);
delegate_noop!(Seen: ignore ExtSessionLockManagerV1);
delegate_noop!(Seen: ignore WlCompositor);
delegate_noop!(Seen: ignore XdgWmDialogV1);
delegate_noop!(Seen: ignore ZwpTextInputManagerV3);
delegate_noop!(Seen: ignore ZwpInputMethodManagerV2);
delegate_noop!(Seen: ignore XdgDialogV1);
impl Dispatch<WlShm, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &WlShm,
        event: wl_shm::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_shm::Event::Format {
            format: WEnum::Value(format),
        } = event
        {
            seen.shm_formats.push(format);
        }
    }
}

impl Dispatch<WlDataDevice, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &WlDataDevice,
        event: wl_data_device::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_data_device::Event::DataOffer { id } => {
                seen.selection = Some(id);
                seen.offered.clear();
            }
            wl_data_device::Event::Enter {
                serial,
                id: Some(offer),
                ..
            } => {
                offer.accept(serial, Some(URI_LIST.into()));
                offer.set_actions(DndAction::Copy, DndAction::Copy);
                seen.entered = Some(offer);
            }
            wl_data_device::Event::Drop => seen.dropped = true,
            _ => {}
        }
    }

    event_created_child!(Seen, WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, ()),
    ]);
}

impl Dispatch<WlDataOffer, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &WlDataOffer,
        event: wl_data_offer::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_data_offer::Event::Offer { mime_type } = event {
            seen.offered.push(mime_type);
        }
    }
}

impl Dispatch<WlDataSource, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &WlDataSource,
        event: wl_data_source::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_data_source::Event::Send { mime_type, fd } => {
                std::fs::File::from(fd).write_all(&seen.payload).unwrap();
                seen.sent.push(mime_type);
            }
            wl_data_source::Event::Action { dnd_action } => {
                seen.source_events.push(format!("action {dnd_action:?}"));
            }
            wl_data_source::Event::DndDropPerformed => seen.source_events.push("performed".into()),
            wl_data_source::Event::DndFinished => seen.source_events.push("finished".into()),
            wl_data_source::Event::Cancelled => seen.source_events.push("cancelled".into()),
            _ => {}
        }
    }
}

delegate_noop!(Seen: ignore WlDataDeviceManager);

impl Dispatch<ZxdgToplevelDecorationV1, ()> for Seen {
    fn event(
        seen: &mut Self,
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
            seen.xdg_modes.push(mode);
        }
    }
}

impl Dispatch<OrgKdeKwinServerDecorationManager, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &OrgKdeKwinServerDecorationManager,
        event: kde_manager::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let kde_manager::Event::DefaultMode {
            mode: WEnum::Value(mode),
        } = event
        {
            seen.kde_default = Some(mode);
        }
    }
}

impl Dispatch<kde_decoration::OrgKdeKwinServerDecoration, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &kde_decoration::OrgKdeKwinServerDecoration,
        event: kde_decoration::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let kde_decoration::Event::Mode {
            mode: WEnum::Value(mode),
        } = event
        {
            seen.kde_modes.push(mode);
        }
    }
}

delegate_noop!(Seen: ignore ZxdgDecorationManagerV1);
delegate_noop!(Seen: ignore ZwlrLayerShellV1);

impl Dispatch<ZwlrLayerSurfaceV1, ()> for Seen {
    fn event(
        seen: &mut Self,
        surface: &ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwlr_layer_surface_v1::Event::Configure {
            serial,
            width,
            height,
        } = event
        {
            surface.ack_configure(serial);
            seen.layer_size = Some((width, height));
            seen.layer_configures += 1;
        }
    }
}
delegate_noop!(Seen: ignore WlShmPool);
delegate_noop!(Seen: ignore WlBuffer);
delegate_noop!(Seen: ignore WlSurface);

#[derive(Default)]
pub struct HostLog {
    pub motions: Vec<(f64, f64)>,
    pub buttons: Vec<(u32, bool)>,
    /// The person's press each button and key was delivered as, in order: what a call the
    /// surface made then would name.
    pub handled: Vec<Option<u32>>,
    pub left: u32,
    pub keys: Vec<SurfaceKey>,
    pub wants_keyboard: bool,
    /// Each Super+X the surface was offered, and whether it was told a client holds the desktop's
    /// shortcuts.
    pub taken: Vec<(u32, bool)>,
    /// The text field the surface draws, when it draws one: what it declares and where its caret
    /// stands on the shell.
    pub field: Option<SurfaceTextInput>,
    /// What the input method did with the field, in order.
    pub ime: Vec<SurfaceIme>,
    /// Whether a press anywhere else closes the surface, as an open launcher closes.
    pub closes_on_press_elsewhere: bool,
    /// What the surface draws over the windows, by the window each part is of, in the layout.
    pub covers: Vec<tilo_session_core::surface::WindowCover>,
}

/// A surface: one square of one colour. Like any surface it keeps the frame it drew and
/// hands that same frame on every pass: a fresh one each time would say it drew everything again.
pub struct Square {
    rect: (i32, i32, i32, i32),
    frame: SurfaceFrame,
    log: Rc<RefCell<HostLog>>,
    below: bool,
}

impl Square {
    pub fn new(rect: (i32, i32, i32, i32), rgba: [u8; 4], log: Rc<RefCell<HostLog>>) -> Self {
        let frame = SurfaceFrame::from_pixels(
            (
                u32::try_from(rect.2).expect("a whole number of pixels"),
                u32::try_from(rect.3).expect("a whole number of pixels"),
            ),
            rgba.repeat(usize::try_from(rect.2 * rect.3).unwrap()),
        );
        Self {
            rect,
            frame,
            log,
            below: false,
        }
    }

    /// The same square, under every window.
    pub fn below(mut self) -> Self {
        self.below = true;
        self
    }
}

impl ShellSurface for Square {
    fn layers(&self) -> Vec<SurfaceLayer> {
        vec![SurfaceLayer {
            pose: tilo_session_core::surface::LayerPose::SETTLED,
            over_windows: false,
            location: (self.rect.0, self.rect.1),
            size: (self.rect.2, self.rect.3),
            frame: self.frame.clone(),
        }]
    }

    fn below_windows(&self) -> bool {
        self.below
    }

    fn drawn_over_windows(&self) -> Vec<tilo_session_core::surface::WindowCover> {
        self.log.borrow().covers.clone()
    }

    fn accepts_pointer(&self, x: f64, y: f64) -> bool {
        let (rx, ry, w, h) = self.rect;
        x >= f64::from(rx) && y >= f64::from(ry) && x < f64::from(rx + w) && y < f64::from(ry + h)
    }

    fn pointer_motion(&mut self, x: f64, y: f64) {
        self.log.borrow_mut().motions.push((x, y));
    }

    fn pointer_leave(&mut self) {
        self.log.borrow_mut().left += 1;
    }

    fn pointer_button(&mut self, button: u32, pressed: bool) {
        let mut log = self.log.borrow_mut();
        log.buttons.push((button, pressed));
        log.handled
            .push(tilo_session_core::surface::handled_press());
    }

    fn pressed_elsewhere(&mut self, _x: f64, _y: f64) {
        let mut log = self.log.borrow_mut();
        if log.closes_on_press_elsewhere {
            log.wants_keyboard = false;
        }
    }

    fn wants_keyboard(&self) -> bool {
        self.log.borrow().wants_keyboard
    }

    fn key(&mut self, key: &SurfaceKey) {
        let mut log = self.log.borrow_mut();
        log.keys.push(key.clone());
        log.handled
            .push(tilo_session_core::surface::handled_press());
    }

    fn text_input(&self) -> Option<SurfaceTextInput> {
        self.log.borrow().field
    }

    fn ime(&mut self, event: SurfaceIme) {
        self.log.borrow_mut().ime.push(event);
    }
}
