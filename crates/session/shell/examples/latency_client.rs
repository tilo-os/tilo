//! A minimal `wl_shm` client that answers every key press with one commit.
//!
//! It is the app side of `scripts/perf/headless_key_latency.py`. Each key types
//! one cell of a text grid. With `--damage full` (the default) the commit damages
//! the whole surface, as a client that redraws its grid would; with
//! `--damage line` it damages only the text line the cell is on, as a terminal
//! or a text editor reports a typed character. It draws at once, without waiting
//! for a frame callback, so the compositor's own scheduling is what the harness
//! measures. It prints `READY <width>x<height>` once its first frame is
//! committed and exits when the window is closed.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::print_stdout,
    clippy::too_many_lines,
    clippy::panic,
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "a measurement tool: a failed protocol step or a bad argument must stop the run, its grid arithmetic stays within the configured surface, and stdout is its interface"
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
    /// Cells typed so far; the buffer always shows exactly these.
    typed: u32,
    damage: Damage,
}

/// What a commit reports as changed.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Damage {
    Full,
    Line,
}

/// One typed cell of the grid, in buffer pixels: a text line is `CELL_H` high.
const CELL_W: u32 = 10;
const CELL_H: u32 = 20;
const BACKGROUND: [u8; 4] = [0x20, 0x20, 0x20, 0xFF];
const INK: [u8; 4] = [0xE0, 0xE0, 0xE0, 0xFF];

fn main() {
    let mut app_id = "so.tilo.latency-client".to_owned();
    let mut damage = Damage::Full;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--damage" {
            damage = match args.next().as_deref() {
                Some("line") => Damage::Line,
                Some("full") => Damage::Full,
                other => panic!("--damage takes `line` or `full`, not {other:?}"),
            };
        } else {
            app_id = arg;
        }
    }
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
        typed: 0,
        damage,
    };
    while !client.exit {
        queue.blocking_dispatch(&mut client).expect("dispatch");
    }
}

impl Client {
    /// The grid cell the `index`th typed key fills, as `(column, line)`.
    fn cell(&self, index: u32) -> (u32, u32) {
        let columns = (self.width / CELL_W).max(1);
        let lines = (self.height / CELL_H).max(1);
        (index % columns, (index / columns) % lines)
    }

    /// Draw the grid with `self.typed` cells into a free buffer and commit it,
    /// damaging the whole surface or only the line of the newest cell. Every
    /// buffer is drawn whole, so whichever one the pool hands out shows the
    /// same grid; only the reported damage differs between the modes.
    fn draw(&mut self, qh: &QueueHandle<Self>, key: bool) {
        let (width, height) = (self.width, self.height);
        let stride = i32::try_from(width.saturating_mul(4)).expect("stride");
        let (w, h) = (
            i32::try_from(width).expect("w"),
            i32::try_from(height).expect("h"),
        );
        if key {
            self.typed = self.typed.wrapping_add(1);
        }
        // A full grid starts over on an empty screen, as a cleared terminal would.
        let columns = (width / CELL_W).max(1);
        let lines = (height / CELL_H).max(1);
        let shown = self.typed % columns.saturating_mul(lines).max(1);
        let newest = self.cell(self.typed.saturating_sub(1));
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
        for chunk in canvas.chunks_exact_mut(4) {
            chunk.copy_from_slice(&BACKGROUND);
        }
        let row_bytes = usize::try_from(width).expect("width") * 4;
        for index in 0..shown {
            let (column, line) = (index % columns, index / columns);
            // The ink of a cell: inset by one pixel so cells stay apart.
            for y in line * CELL_H + 2..(line + 1) * CELL_H - 2 {
                let start = usize::try_from(y).expect("y") * row_bytes
                    + usize::try_from(column * CELL_W + 1).expect("x") * 4;
                let end = start + usize::try_from(CELL_W - 2).expect("cell") * 4;
                for chunk in canvas[start..end].chunks_exact_mut(4) {
                    chunk.copy_from_slice(&INK);
                }
            }
        }
        let surface = self.window.wl_surface();
        if key && self.damage == Damage::Line && shown != 0 {
            let line_y = i32::try_from(newest.1 * CELL_H).expect("line");
            surface.damage_buffer(0, line_y, w, i32::try_from(CELL_H).expect("cell"));
        } else {
            surface.damage_buffer(0, 0, w, h);
        }
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
        self.draw(qh, false);
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
        self.draw(qh, true);
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
