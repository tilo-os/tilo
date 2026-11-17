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
use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_callback::WlCallback, wl_compositor::WlCompositor, wl_keyboard,
    wl_keyboard::WlKeyboard, wl_output, wl_output::WlOutput, wl_pointer, wl_pointer::WlPointer,
    wl_registry, wl_registry::WlRegistry, wl_seat, wl_seat::WlSeat, wl_shm, wl_shm::WlShm,
    wl_shm_pool::WlShmPool, wl_subcompositor::WlSubcompositor, wl_subsurface::WlSubsurface,
    wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, EventQueue, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::wp::linux_dmabuf::zv1::client::{
    zwp_linux_dmabuf_feedback_v1, zwp_linux_dmabuf_feedback_v1::ZwpLinuxDmabufFeedbackV1,
    zwp_linux_dmabuf_v1, zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1,
};
use wayland_protocols::xdg::shell::client::{
    xdg_popup, xdg_popup::XdgPopup, xdg_positioner::XdgPositioner, xdg_surface,
    xdg_surface::XdgSurface, xdg_toplevel, xdg_toplevel::XdgToplevel, xdg_wm_base,
    xdg_wm_base::XdgWmBase,
};

pub const OUTPUT: &str = "HEADLESS-1";

/// A desktop of one output.
pub fn desktop(width: i32, height: i32) -> Compositor {
    let mut compositor = Compositor::headless(Box::new(StackPolicy)).expect("compositor");
    compositor
        .add_output(&OutputConfig::new(OUTPUT, width, height))
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
        let mut compositor = desktop(width, height);
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
    pub keyboard_entered: bool,
    pub pointer_entered: bool,
    pub buffers_released: usize,
    /// Whether the app answers a ping when it reads one.
    pub answers_pings: bool,
    /// The serials of the pings it read.
    pub pings: Vec<u32>,
    pending_size: Option<(i32, i32, Vec<u32>)>,
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
            heard.wm_base.is_some() && heard.seat.is_some() && !heard.outputs.is_empty()
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
                "wl_seat" => heard.seat = Some(registry.bind(name, version.min(9), qh, ())),
                "wl_output" => heard
                    .outputs
                    .push(registry.bind(name, version.min(4), qh, name)),
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
            wl_keyboard::Event::Enter { .. } => heard.keyboard_entered = true,
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
            wl_pointer::Event::Enter { .. } => heard.pointer_entered = true,
            wl_pointer::Event::Leave { .. } => heard.pointer_entered = false,
            wl_pointer::Event::Button {
                serial,
                button,
                state: WEnum::Value(state),
                ..
            } => heard
                .buttons
                .push((serial, button, state == wl_pointer::ButtonState::Pressed)),
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

delegate_noop!(Heard: ignore WlCompositor);
delegate_noop!(Heard: ignore WlSubcompositor);
delegate_noop!(Heard: ignore WlSubsurface);
delegate_noop!(Heard: ignore WlShm);
delegate_noop!(Heard: ignore WlShmPool);
delegate_noop!(Heard: ignore WlSurface);
delegate_noop!(Heard: ignore XdgPositioner);

/// The state number of `suspended` in `xdg_toplevel.configure`.
pub const SUSPENDED: u32 = 9;
