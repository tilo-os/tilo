//! A minimal `wl_shm` client that redraws its whole window on every key press.
//!
//! It is the app side of `scripts/perf/headless_key_latency.py`: a window whose
//! answer to a key is one full-surface commit, the way a terminal or a
//! text editor answers a character. It draws at once, without waiting for a
//! frame callback, so the compositor's own scheduling is what the harness
//! measures. It prints `READY <width>x<height>` once its first frame is
//! committed and exits when the window is closed.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stdout,
    clippy::too_many_lines,
    reason = "a measurement tool: a failed protocol step must stop the run, and stdout is its interface"
)]

use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_keyboard, delegate_output, delegate_registry, delegate_seat,
    delegate_shm, delegate_xdg_shell, delegate_xdg_window,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers},
    },
    shell::{
        WaylandSurface,
        xdg::{
            XdgShell,
            window::{Window, WindowConfigure, WindowDecorations, WindowHandler},
        },
    },
    shm::{
        Shm, ShmHandler,
        slot::{Buffer, SlotPool},
    },
};
use std::io::Write as _;
use wayland_client::{
    Connection, QueueHandle,
    globals::registry_queue_init,
    protocol::{wl_keyboard, wl_output, wl_seat, wl_shm, wl_surface},
};

struct Client {
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    shm: Shm,
    pool: SlotPool,
    window: Window,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    buffer: Option<Buffer>,
    width: u32,
    height: u32,
    configured: bool,
    exit: bool,
    draws: u32,
}

fn main() {
    let app_id = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "so.tilo.latency-client".to_owned());
    let conn = Connection::connect_to_env().expect("no Wayland compositor to connect to");
    let (globals, mut queue) = registry_queue_init(&conn).expect("registry");
    let qh = queue.handle();
    let compositor = CompositorState::bind(&globals, &qh).expect("wl_compositor");
    let xdg_shell = XdgShell::bind(&globals, &qh).expect("xdg_wm_base");
    let shm = Shm::bind(&globals, &qh).expect("wl_shm");
    let surface = compositor.create_surface(&qh);
    let window = xdg_shell.create_window(surface, WindowDecorations::RequestServer, &qh);
    window.set_title("latency client");
    window.set_app_id(app_id);
    window.commit();
    let pool = SlotPool::new(256 * 256 * 4, &shm).expect("shm pool");
    let mut client = Client {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        shm,
        pool,
        window,
        keyboard: None,
        buffer: None,
        width: 256,
        height: 256,
        configured: false,
        exit: false,
        draws: 0,
    };
    while !client.exit {
        queue.blocking_dispatch(&mut client).expect("dispatch");
    }
}

impl Client {
    /// Fill the whole window with a colour that changes every draw, damage it
    /// all and commit, as a text surface that redraws its grid would.
    fn draw(&mut self, qh: &QueueHandle<Self>) {
        let (width, height) = (self.width, self.height);
        let stride = i32::try_from(width * 4).expect("stride");
        let (w, h) = (
            i32::try_from(width).expect("w"),
            i32::try_from(height).expect("h"),
        );
        let buffer = self.buffer.get_or_insert_with(|| {
            self.pool
                .create_buffer(w, h, stride, wl_shm::Format::Argb8888)
                .expect("create buffer")
                .0
        });
        let canvas = if let Some(canvas) = self.pool.canvas(buffer) {
            canvas
        } else {
            // The compositor still holds the previous buffer: draw into a second one.
            let (second, canvas) = self
                .pool
                .create_buffer(w, h, stride, wl_shm::Format::Argb8888)
                .expect("create buffer");
            *buffer = second;
            canvas
        };
        self.draws = self.draws.wrapping_add(1);
        let shade = u8::try_from(self.draws % 200)
            .unwrap_or(0)
            .saturating_add(30);
        let pixel = [shade, 0x30, 0x20, 0xFF];
        for chunk in canvas.chunks_exact_mut(4) {
            chunk.copy_from_slice(&pixel);
        }
        let surface = self.window.wl_surface();
        surface.damage_buffer(0, 0, w, h);
        surface.frame(qh, surface.clone());
        buffer.attach_to(surface).expect("attach");
        self.window.commit();
    }
}

impl CompositorHandler for Client {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: i32,
    ) {
    }
    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}
    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for Client {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl WindowHandler for Client {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window) {
        self.exit = true;
    }

    fn configure(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        _: &Window,
        configure: WindowConfigure,
        _: u32,
    ) {
        self.buffer = None;
        self.width = configure
            .new_size
            .0
            .map_or(self.width, std::num::NonZeroU32::get);
        self.height = configure
            .new_size
            .1
            .map_or(self.height, std::num::NonZeroU32::get);
        self.draw(qh);
        if !self.configured {
            self.configured = true;
            println!("READY {}x{}", self.width, self.height);
            std::io::stdout().flush().expect("flush");
        }
    }
}

impl SeatHandler for Client {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            self.keyboard = Some(
                self.seat_state
                    .get_keyboard(qh, &seat, None)
                    .expect("keyboard"),
            );
        }
    }
    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard
            && let Some(keyboard) = self.keyboard.take()
        {
            keyboard.release();
        }
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl KeyboardHandler for Client {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
        _: &[u32],
        _: &[Keysym],
    ) {
    }
    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
    ) {
    }
    fn press_key(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: KeyEvent,
    ) {
        self.draw(qh);
    }
    fn repeat_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: KeyEvent,
    ) {
    }
    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: KeyEvent,
    ) {
    }
    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: Modifiers,
        _: RawModifiers,
        _: u32,
    ) {
    }
}

impl ShmHandler for Client {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

delegate_compositor!(Client);
delegate_output!(Client);
delegate_shm!(Client);
delegate_seat!(Client);
delegate_keyboard!(Client);
delegate_xdg_shell!(Client);
delegate_xdg_window!(Client);
delegate_registry!(Client);

impl ProvidesRegistryState for Client {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState,];
}
