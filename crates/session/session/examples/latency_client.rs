//! A minimal client that answers every key press with one commit. It is the app side of
//! `scripts/perf/headless_key_latency.py` : each key types one cell of a text grid. Its buffers are
//! `wl_shm` or LINEAR `XRGB8888` dma-bufs (`--buffers`), which the compositor draws without a CPU
//! copy; `--damage full` damages the whole surface and `--damage line` only the cell's text line.
//! It draws at once, without waiting for a frame callback, so the compositor's own scheduling is
//! what the harness measures, and `--scale N` draws N buffer pixels per surface pixel. Two lines
//! per key say how long it took on this client's own clock:
//! `CLIENT-KEY n=<key> key_to_commit_us=<us> at_us=<us>` and
//! `CLIENT-FRAME n=<key> key_to_frame_us=<us> at_us=<us>` . Nothing in it depends on the session's
//! instrumentation.

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

use smithay_client_toolkit::reexports::protocols::wp::linux_dmabuf::zv1::client::{
    zwp_linux_buffer_params_v1, zwp_linux_dmabuf_feedback_v1,
};
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
use smithay_client_toolkit::{
    delegate_dmabuf,
    dmabuf::{DmabufFeedback, DmabufHandler, DmabufState},
};
use std::collections::VecDeque;
use std::io::Write as _;
use std::time::Instant;
use wayland_client::protocol::wl_buffer;
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
    configured: bool,
    exit: bool,
    /// Cells typed so far; the buffer always shows exactly these.
    typed: u32,
    damage: Damage,
    dmabuf: Option<Dmabufs>,
    /// Buffer pixels per surface pixel.
    scale: u32,
    /// The key being drawn: its number and when its press was dispatched.
    pressing: Option<(u32, Instant)>,
    /// Frame callbacks asked for, in the order the compositor answers them: the key each commit was
    /// answering, when it was answering one.
    showing: VecDeque<Option<(u32, Instant)>>,
    /// Keys typed so far, the number on its line.
    keys: u32,
    /// When this process started: the clock both lines also stamp, so every key's commit and frame
    /// can be placed on one timeline.
    started: Instant,
}

/// LINEAR dma-bufs the client draws into with the CPU through GBM.
struct Dmabufs {
    state: DmabufState,
    gbm: gbm::Device<std::fs::File>,
    buffers: Vec<DmabufBuffer>,
}

struct DmabufBuffer {
    bo: gbm::BufferObject<()>,
    wl: wl_buffer::WlBuffer,
    size: (u32, u32),
    /// The compositor holds it until it releases it.
    busy: bool,
    /// Cells it shows, in the grid cycle it shows them; `None` : nothing yet.
    shows: Option<u32>,
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
/// Bounds the frame callbacks waited on if a session never answers one.
const MAX_SHOWING: usize = 64;
/// The window it asks for: one size on every session, so what a session composites for a key is the
/// same amount of pixels on each.
const WINDOW_W: u32 = 640;
const WINDOW_H: u32 = 480;

fn main() {
    let mut app_id = "so.tilo.latency-client".to_owned();
    let mut damage = Damage::Full;
    let mut dmabufs = false;
    let mut scale = 1_u32;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--buffers" {
            dmabufs = match args.next().as_deref() {
                Some("dmabuf") => true,
                Some("shm") => false,
                other => panic!("--buffers takes `shm` or `dmabuf`, not {other:?}"),
            };
        } else if arg == "--scale" {
            scale = args
                .next()
                .and_then(|value| value.parse().ok())
                .filter(|scale| *scale >= 1)
                .expect("--scale takes a whole number of buffer pixels per surface pixel");
        } else if arg == "--damage" {
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
    surface.set_buffer_scale(i32::try_from(scale).expect("scale"));
    let window = xdg_shell.create_window(surface, WindowDecorations::RequestServer, &qh);
    window.set_title("latency client");
    window.set_app_id(app_id);
    // Only the largest size, never a smallest: a desk reads a client that pins both to one value as
    // a dialog and floats it over its parent instead of giving it a tab, and a benchmark needs the
    // same tab on every session.
    window.set_max_size(Some((WINDOW_W, WINDOW_H)));
    window.commit();
    let pool = SlotPool::new(
        usize::try_from(WINDOW_W * WINDOW_H * 4).expect("pool bytes"),
        &shm,
    )
    .expect("shm pool");
    let dmabuf = dmabufs.then(|| {
        let state = DmabufState::new(&globals, &qh);
        assert!(
            state.version().is_some(),
            "the compositor offers no zwp_linux_dmabuf_v1"
        );
        Dmabufs {
            state,
            gbm: gbm::Device::new(render_node()).expect("a GBM device on the render node"),
            buffers: Vec::new(),
        }
    });
    let mut client = Client {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        shm,
        pool,
        window,
        keyboard: None,
        buffer: None,
        configured: false,
        exit: false,
        typed: 0,
        started: Instant::now(),
        damage,
        dmabuf,
        scale,
        pressing: None,
        showing: VecDeque::new(),
        keys: 0,
    };
    while !client.exit {
        queue.blocking_dispatch(&mut client).expect("dispatch");
    }
}

/// The first render node that opens.
fn render_node() -> std::fs::File {
    let mut nodes: Vec<_> = std::fs::read_dir("/dev/dri")
        .expect("/dev/dri")
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("renderD"))
        })
        .collect();
    nodes.sort();
    nodes
        .iter()
        .find_map(|path| {
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)
                .ok()
        })
        .expect("no render node to allocate dma-bufs on")
}

/// Paint cells `from..to` of the grid, or the empty grid first when `clear`,
/// into `canvas`, rows `stride` bytes apart. Channels are the same in
/// `ARGB8888` and `XRGB8888` memory order for these greys.
fn paint_cells(
    canvas: &mut [u8],
    stride: usize,
    (width, height): (u32, u32),
    clear: bool,
    cells: std::ops::Range<u32>,
) {
    let columns = (width / CELL_W).max(1);
    if clear {
        let row = usize::try_from(width).expect("width") * 4;
        for y in 0..usize::try_from(height).expect("height") {
            for chunk in canvas[y * stride..y * stride + row].chunks_exact_mut(4) {
                chunk.copy_from_slice(&BACKGROUND);
            }
        }
    }
    for index in cells {
        let (column, line) = (index % columns, index / columns);
        // The ink of a cell: inset by one pixel so cells stay apart.
        for y in line * CELL_H + 2..(line + 1) * CELL_H - 2 {
            let start = usize::try_from(y).expect("y") * stride
                + usize::try_from(column * CELL_W + 1).expect("x") * 4;
            let end = start + usize::try_from(CELL_W - 2).expect("cell") * 4;
            for chunk in canvas[start..end].chunks_exact_mut(4) {
                chunk.copy_from_slice(&INK);
            }
        }
    }
}

impl Client {
    /// The buffer's size: the surface's, in buffer pixels.
    fn buffer_size(&self) -> (u32, u32) {
        (WINDOW_W * self.scale, WINDOW_H * self.scale)
    }

    /// The grid cell the `index` th typed key fills, as `(column, line)` .
    fn cell(&self, index: u32) -> (u32, u32) {
        let (width, height) = self.buffer_size();
        let columns = (width / CELL_W).max(1);
        let lines = (height / CELL_H).max(1);
        (index % columns, (index / columns) % lines)
    }

    /// Draw the grid with `self.typed` cells into a free buffer and commit it,
    /// damaging the whole surface or only the line of the newest cell. Every
    /// buffer is drawn whole, so whichever one the pool hands out shows the
    /// same grid; only the reported damage differs between the modes.
    fn draw(&mut self, qh: &QueueHandle<Self>, key: bool) {
        let (width, height) = self.buffer_size();
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
        let surface = self.window.wl_surface().clone();
        if let Some(dmabufs) = self.dmabuf.as_mut() {
            dmabufs.attach(&surface, (width, height), shown, qh);
        } else {
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
            let row_bytes = usize::try_from(width).expect("width") * 4;
            paint_cells(canvas, row_bytes, (width, height), true, 0..shown);
            buffer.attach_to(&surface).expect("attach");
        }
        if key && self.damage == Damage::Line && shown != 0 {
            let line_y = i32::try_from(newest.1 * CELL_H).expect("line");
            surface.damage_buffer(0, line_y, w, i32::try_from(CELL_H).expect("cell"));
        } else {
            surface.damage_buffer(0, 0, w, h);
        }
        surface.frame(qh, surface.clone());
        let commit_at = Instant::now();
        // Every commit asks for one frame callback, so every callback answers the commit before it:
        // a key's, or a redraw's the key did not cause.
        let answer = self.pressing.take().map(|(number, press_at)| {
            eprintln!(
                "CLIENT-KEY n={number} key_to_commit_us={} at_us={}",
                commit_at.duration_since(press_at).as_micros(),
                commit_at.duration_since(self.started).as_micros(),
            );
            (number, press_at)
        });
        if self.showing.len() == MAX_SHOWING {
            self.showing.pop_front();
        }
        self.showing.push_back(answer);
        self.window.commit();
    }
}

impl Dmabufs {
    /// Bring a buffer the compositor does not hold up to `shown` cells and attach it: only the
    /// cells it lacks are drawn, the whole grid when it is new, of another size, or from an earlier
    /// grid cycle.
    fn attach(
        &mut self,
        surface: &wl_surface::WlSurface,
        size: (u32, u32),
        shown: u32,
        qh: &QueueHandle<Client>,
    ) {
        self.buffers
            .retain(|buffer| buffer.busy || buffer.size == size);
        let index = if let Some(index) = self.buffers.iter().position(|buffer| !buffer.busy) {
            index
        } else {
            let bo = self
                .gbm
                .create_buffer_object_with_modifiers2::<()>(
                    size.0,
                    size.1,
                    gbm::Format::Xrgb8888,
                    [gbm::Modifier::Linear].into_iter(),
                    gbm::BufferObjectFlags::RENDERING,
                )
                .expect("a LINEAR dma-buf");
            let params = self.state.create_params(qh).expect("dmabuf params");
            let fd = bo.fd_for_plane(0).expect("dma-buf fd");
            params.add(
                std::os::fd::AsFd::as_fd(&fd),
                0,
                bo.offset(0),
                bo.stride_for_plane(0),
                u64::from(gbm::Modifier::Linear),
            );
            let (wl, _) = params.create_immed(
                i32::try_from(size.0).expect("w"),
                i32::try_from(size.1).expect("h"),
                gbm::Format::Xrgb8888 as u32,
                zwp_linux_buffer_params_v1::Flags::empty(),
                qh,
            );
            self.buffers.push(DmabufBuffer {
                bo,
                wl,
                size,
                busy: false,
                shows: None,
            });
            self.buffers.len() - 1
        };
        let buffer = &mut self.buffers[index];
        let (clear, from) = match buffer.shows {
            Some(drawn) if drawn <= shown => (false, drawn),
            _ => (true, 0),
        };
        buffer
            .bo
            .map_mut(0, 0, size.0, size.1, |mapped| {
                let stride = usize::try_from(mapped.stride()).expect("stride");
                paint_cells(mapped.buffer_mut(), stride, size, clear, from..shown);
            })
            .expect("map the dma-buf");
        buffer.shows = Some(shown);
        buffer.busy = true;
        surface.attach(Some(&buffer.wl), 0, 0);
    }
}

impl DmabufHandler for Client {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf.as_mut().expect("dma-buf buffers").state
    }
    fn dmabuf_feedback(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &zwp_linux_dmabuf_feedback_v1::ZwpLinuxDmabufFeedbackV1,
        _: DmabufFeedback,
    ) {
    }
    fn created(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &zwp_linux_buffer_params_v1::ZwpLinuxBufferParamsV1,
        _: wl_buffer::WlBuffer,
    ) {
    }
    fn failed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &zwp_linux_buffer_params_v1::ZwpLinuxBufferParamsV1,
    ) {
        panic!("the compositor refused a LINEAR XRGB8888 dma-buf");
    }
    fn released(&mut self, _: &Connection, _: &QueueHandle<Self>, released: &wl_buffer::WlBuffer) {
        if let Some(buffer) = self.dmabuf.as_mut().and_then(|dmabufs| {
            dmabufs
                .buffers
                .iter_mut()
                .find(|buffer| &buffer.wl == released)
        }) {
            buffer.busy = false;
        }
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
    /// The compositor showed the frame the key's commit carries: the last leg of the key's journey,
    /// on this client's own clock.
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {
        let Some(answer) = self.showing.pop_front() else {
            return;
        };
        let Some((number, press_at)) = answer else {
            return;
        };
        let shown_at = Instant::now();
        eprintln!(
            "CLIENT-FRAME n={number} key_to_frame_us={} at_us={}",
            shown_at.duration_since(press_at).as_micros(),
            shown_at.duration_since(self.started).as_micros(),
        );
    }
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
        _: WindowConfigure,
        _: u32,
    ) {
        self.buffer = None;
        // The size the session asks for is not taken: the surface keeps one size on every session,
        // so what a session composites for a key is the same number of pixels on each.
        self.draw(qh, false);
        if !self.configured {
            self.configured = true;
            let (width, height) = self.buffer_size();
            println!("READY {width}x{height}");
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
        eprintln!("CLIENT-SEAT capability={capability:?}");
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
        eprintln!("CLIENT-FOCUS key=enter");
    }
    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
    ) {
        eprintln!("CLIENT-FOCUS key=leave");
    }
    fn press_key(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: KeyEvent,
    ) {
        self.keys = self.keys.wrapping_add(1);
        self.pressing = Some((self.keys, Instant::now()));
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
delegate_dmabuf!(Client);

impl ProvidesRegistryState for Client {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState,];
}
