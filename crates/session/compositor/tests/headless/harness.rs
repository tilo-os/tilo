//! The harness the headless tests are written against: a client with its
//! one toplevel, what the compositor told it, and the pumps that let the two
//! talk until a condition holds.

use std::io::{Read as _, Write as _};
use std::os::fd::AsFd as _;
use std::time::Duration;

use tilo_compositor::{Compositor, OutputConfig, WindowPolicy};
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_callback::WlCallback, wl_compositor::WlCompositor, wl_keyboard,
    wl_keyboard::WlKeyboard, wl_pointer, wl_pointer::WlPointer, wl_region::WlRegion, wl_registry,
    wl_registry::WlRegistry, wl_seat, wl_seat::WlSeat, wl_shm, wl_shm::WlShm,
    wl_shm_pool::WlShmPool, wl_surface::WlSurface, wl_touch, wl_touch::WlTouch,
};
use wayland_client::{
    Connection, Dispatch, EventQueue, QueueHandle, WEnum, delegate_noop, event_created_child,
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
    zwp_tablet_tool_v2::ZwpTabletToolV2,
    zwp_tablet_v2::ZwpTabletV2,
};
use wayland_protocols::wp::text_input::zv3::client::{
    zwp_text_input_manager_v3::ZwpTextInputManagerV3,
    zwp_text_input_v3,
    zwp_text_input_v3::{ContentHint, ContentPurpose, ZwpTextInputV3},
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
use wayland_protocols_misc::zwp_input_method_v2::client::{
    zwp_input_method_keyboard_grab_v2::ZwpInputMethodKeyboardGrabV2,
    zwp_input_method_manager_v2::ZwpInputMethodManagerV2, zwp_input_method_v2,
    zwp_input_method_v2::ZwpInputMethodV2,
};

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
    pub wm_base: Option<XdgWmBase>,
    pub seat: Option<WlSeat>,
    pub text_input_manager: Option<ZwpTextInputManagerV3>,
    pub text_input: Option<ZwpTextInputV3>,
    /// Whether the field has the focus and has asked for typing, as the
    /// compositor told it.
    pub field_entered: bool,
    /// The strings the field was made to contain.
    pub committed: Vec<String>,
    /// How many times the compositor applied state to the field.
    pub field_done: u32,
    pub input_method_manager: Option<ZwpInputMethodManagerV2>,
    pub input_method: Option<ZwpInputMethodV2>,
    /// How many times the compositor applied the input method's state: what
    /// its next commit is numbered.
    pub ime_done: u32,
    /// Whether the compositor asked the input method to type.
    pub ime_activated: bool,
    pub pointer: Option<WlPointer>,
    /// The absolute positions the pointer was put at here, as
    /// `wl_pointer.motion` sent them.
    pub pointer_positions: Vec<(f64, f64)>,
    pub keyboard: Option<WlKeyboard>,
    pub touch: Option<WlTouch>,
    pub pointer_constraints: Option<ZwpPointerConstraintsV1>,
    pub locked_pointer: Option<ZwpLockedPointerV1>,
    pub confined_pointer: Option<ZwpConfinedPointerV1>,
    /// Whether the compositor holds the pointer where it is.
    pub pointer_locked: bool,
    /// Whether the compositor keeps the pointer inside the region.
    pub pointer_confined: bool,
    pub relative_pointer_manager: Option<ZwpRelativePointerManagerV1>,
    pub relative_pointer: Option<ZwpRelativePointerV1>,
    /// The deltas the compositor sent, as `(dx, dy)`.
    pub relative: Vec<(f64, f64)>,
    pub inhibit_manager: Option<ZwpKeyboardShortcutsInhibitManagerV1>,
    pub inhibitor: Option<ZwpKeyboardShortcutsInhibitorV1>,
    /// Whether the compositor lets this client answer the desktop's chords.
    pub shortcuts_inhibited: bool,
    /// Touch events as `down id x y`, `motion id x y`, `up id`, `frame`.
    pub touches: Vec<String>,
    pub configured: bool,
    pub pointer_inside: bool,
    pub buttons: Vec<(u32, bool)>,
    pub keyboard_focused: bool,
    pub keys: Vec<(u32, bool)>,
    /// The keymap the keyboard sent, as XKB text: the client's own answer to
    /// which characters its keys type.
    pub keymap: Option<String>,
    pub frames_done: u32,
    pub closed: bool,
    pub tablet_manager: Option<ZwpTabletManagerV2>,
    pub wm_dialog: Option<XdgWmDialogV1>,
    pub icon_manager: Option<XdgToplevelIconManagerV1>,
    /// The icon sizes the manager tells clients a tab pill paints at.
    pub icon_sizes: Vec<i32>,
    /// Tablet pad events as `added`, `group`, `buttons 4`, `done`,
    /// `enter`, `mode 1`, `button 2 pressed`, `ring 90`, `ring stop`,
    /// `removed`.
    pub pad: Vec<String>,
    /// The surface the pad entered.
    pub pad_surface: Option<WlSurface>,
    /// The colour manager, and what it says it can build.
    pub color_manager: Option<WpColorManagerV1>,
    pub color_features: Vec<u32>,
    pub color_primaries: Vec<u32>,
    pub color_transfers: Vec<u32>,
    pub color_manager_done: bool,
    /// Whether an image description the client asked for is ready.
    pub color_description_ready: bool,
}

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

    /// Connect as a client the session started, which may be its input
    /// method.
    pub fn connect_trusted(compositor: &mut Compositor) -> Self {
        Self::connect_as(compositor, true)
    }

    pub fn connect_as(compositor: &mut Compositor, trusted: bool) -> Self {
        let socket = if trusted {
            compositor.connect_trusted_client().expect("client socket")
        } else {
            compositor.connect_client().expect("client socket")
        };
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

    pub fn open_window(&mut self, app_id: &str) {
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
        surface.commit();
        self.surface = Some(surface);
        self.toplevel = Some(toplevel);
    }

    /// Attach a buffer of one colour (RGBA) and commit it with a frame
    /// callback.
    pub fn show(&self, width: i32, height: i32, rgba: [u8; 4]) {
        let qh = self.queue.handle();
        let stride = width * 4;
        let len = usize::try_from(stride * height).unwrap();
        let fd = rustix::fs::memfd_create("tilo-compositor-test", rustix::fs::MemfdFlags::CLOEXEC)
            .unwrap();
        let mut file = std::fs::File::from(fd);
        // ARGB8888 is B, G, R, A in memory.
        let pixel = [rgba[2], rgba[1], rgba[0], rgba[3]];
        file.write_all(&pixel.repeat(len / 4)).unwrap();
        let pool = self.seen.shm.as_ref().expect("wl_shm").create_pool(
            file.as_fd(),
            stride * height,
            &qh,
            (),
        );
        let buffer =
            pool.create_buffer(0, width, height, stride, wl_shm::Format::Argb8888, &qh, ());
        let surface = self.surface.as_ref().expect("surface");
        surface.attach(Some(&buffer), 0, 0);
        surface.damage_buffer(0, 0, width, height);
        surface.frame(&qh, ());
        surface.commit();
        pool.destroy();
    }

    /// A square buffer of one colour (RGBA), as a window draws its icon.
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

    /// Give the window a text field: a `text-input-v3` object on its
    /// surface, waiting for typing.
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

    /// Change what the field declares itself to be, as an app does when the
    /// person moves to another box.
    pub fn set_content_type(&self, hint: ContentHint, purpose: ContentPurpose) {
        if let Some(field) = self.seen.text_input.as_ref() {
            field.set_content_type(hint, purpose);
            field.commit();
        }
        self.connection.flush().expect("flush");
    }

    /// Move to another field of the same window: the client enables the
    /// text-input object again for it, as it does when the person tabs on.
    pub fn focus_another_field(&self, hint: ContentHint, purpose: ContentPurpose) {
        if let Some(field) = self.seen.text_input.as_ref() {
            field.set_content_type(hint, purpose);
            field.enable();
            field.commit();
        }
        self.connection.flush().expect("flush");
    }

    /// Move the field's caret: a commit that changes nothing about which
    /// field this is, as any edit does.
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

    /// Ask for the pointer to stay where it is while this window has it:
    /// a game or a remote desktop view.
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

    /// Ask for the pointer to stay inside `rect` of this window, as a
    /// remote desktop view does.
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
    let mut client = TestClient::connect(compositor);
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
                "xdg_wm_dialog_v1" => seen.wm_dialog = Some(registry.bind(name, 1, qh, ())),
                "xdg_toplevel_icon_manager_v1" => {
                    seen.icon_manager = Some(registry.bind(name, 1, qh, ()));
                }
                "zwp_text_input_manager_v3" => {
                    seen.text_input_manager = Some(registry.bind(name, 1, qh, ()));
                }
                "zwp_input_method_manager_v2" => {
                    seen.input_method_manager = Some(registry.bind(name, 1, qh, ()));
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
            if capabilities.contains(wl_seat::Capability::Touch) && seen.touch.is_none() {
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
            wl_pointer::Event::Enter { .. } => seen.pointer_inside = true,
            wl_pointer::Event::Leave { .. } => seen.pointer_inside = false,
            wl_pointer::Event::Motion {
                surface_x,
                surface_y,
                ..
            } => seen.pointer_positions.push((surface_x, surface_y)),
            wl_pointer::Event::Button { button, state, .. } => {
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
            wl_touch::Event::Down { id, x, y, .. } => {
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
                seen.keys
                    .push((key, state == WEnum::Value(wl_keyboard::KeyState::Pressed)));
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

impl Dispatch<ZwpTabletPadV2, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &ZwpTabletPadV2,
        event: zwp_tablet_pad_v2::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let line = match event {
            zwp_tablet_pad_v2::Event::Group { .. } => "group".into(),
            zwp_tablet_pad_v2::Event::Buttons { buttons } => format!("buttons {buttons}"),
            zwp_tablet_pad_v2::Event::Done => "done".into(),
            zwp_tablet_pad_v2::Event::Enter { surface, .. } => {
                seen.pad_surface = Some(surface);
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

impl Dispatch<ZwpTabletPadGroupV2, ()> for Seen {
    fn event(
        seen: &mut Self,
        _: &ZwpTabletPadGroupV2,
        event: zwp_tablet_pad_group_v2::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zwp_tablet_pad_group_v2::Event::ModeSwitch { mode, .. } = event {
            seen.pad.push(format!("mode {mode}"));
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
delegate_noop!(Seen: ignore ZwpRelativePointerManagerV1);
delegate_noop!(Seen: ignore ZwpKeyboardShortcutsInhibitManagerV1);
delegate_noop!(Seen: ignore WlRegion);
delegate_noop!(Seen: ignore ZwpTabletManagerV2);
delegate_noop!(Seen: ignore ZwpTabletV2);
delegate_noop!(Seen: ignore ZwpTabletToolV2);
delegate_noop!(Seen: ignore ZwpTabletPadStripV2);
delegate_noop!(Seen: ignore ExtSessionLockManagerV1);
delegate_noop!(Seen: ignore WlCompositor);
delegate_noop!(Seen: ignore XdgWmDialogV1);
delegate_noop!(Seen: ignore ZwpTextInputManagerV3);
delegate_noop!(Seen: ignore ZwpInputMethodManagerV2);
delegate_noop!(Seen: ignore ZwpInputMethodKeyboardGrabV2);
delegate_noop!(Seen: ignore XdgDialogV1);
delegate_noop!(Seen: ignore WlShm);
delegate_noop!(Seen: ignore WlShmPool);
delegate_noop!(Seen: ignore WlBuffer);
delegate_noop!(Seen: ignore WlSurface);
