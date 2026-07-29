//! Continuously-painting Wayland client, used as the load generator for
//! the compositor frame-loop benchmark (`scripts/perf/frame-loop-bench.py`).
//!
//! The compositor's presentation cost is very different for an idle
//! desktop and for one hosting a client that produces a new buffer every
//! frame: the latter forces a foreign-surface import, a full-viewport
//! image into the scene, and a repaint on every tick. Measuring the loop
//! without such a client only ever measures the idle path.
//!
//! Usage:
//!
//! ```text
//! qos-loadclient [--fullscreen] [--size WxH] [--app-id ID] [--seconds N]
//! ```
//!
//! It attaches a fresh SHM buffer on every `wl_surface.frame` callback,
//! with a moving gradient so no two frames are identical (a compositor
//! that hashes buffers can't elide the work). On exit it prints the
//! number of frames it committed and the wall time, so the harness can
//! report the client-observed frame rate alongside the compositor's own.

use std::time::{Duration, Instant};

use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_output, delegate_registry, delegate_shm, delegate_xdg_shell,
    delegate_xdg_window,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    shell::{
        WaylandSurface,
        xdg::{
            XdgShell,
            window::{Window, WindowConfigure, WindowDecorations, WindowHandler},
        },
    },
    shm::{Shm, ShmHandler, slot::SlotPool},
};
use wayland_client::{
    Connection, QueueHandle,
    globals::registry_queue_init,
    protocol::{wl_output, wl_shm, wl_surface},
};

struct Opts {
    fullscreen: bool,
    width: u32,
    height: u32,
    app_id: String,
    seconds: f64,
}

impl Opts {
    /// Parse `argv`. Unknown flags are ignored rather than fatal — this
    /// is a bench fixture, not a user-facing tool, and a harness that
    /// passes a flag a older binary doesn't know should still produce
    /// load rather than exiting non-zero.
    fn parse<I: IntoIterator<Item = String>>(args: I) -> Self {
        let mut o = Opts {
            fullscreen: false,
            width: 1280,
            height: 720,
            app_id: "qos-loadclient".to_string(),
            seconds: 10.0,
        };
        let mut it = args.into_iter().peekable();
        while let Some(a) = it.next() {
            match a.as_str() {
                "--fullscreen" => o.fullscreen = true,
                "--size" => {
                    if let Some(v) = it.next() {
                        if let Some((w, h)) = v.split_once(|c| c == 'x' || c == 'X') {
                            if let (Ok(w), Ok(h)) = (w.trim().parse(), h.trim().parse()) {
                                if w > 0 && h > 0 {
                                    o.width = w;
                                    o.height = h;
                                }
                            }
                        }
                    }
                }
                "--app-id" => {
                    if let Some(v) = it.next() {
                        o.app_id = v;
                    }
                }
                "--seconds" => {
                    if let Some(v) = it.next() {
                        if let Ok(s) = v.trim().parse::<f64>() {
                            if s > 0.0 {
                                o.seconds = s;
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        o
    }
}

struct LoadApp {
    registry_state: RegistryState,
    output_state: OutputState,
    compositor_state: CompositorState,
    shm_state: Shm,
    xdg_shell_state: XdgShell,
    window: Option<Window>,
    pool: SlotPool,
    width: u32,
    height: u32,
    frames: u64,
    configured: bool,
    started: Instant,
    deadline: Instant,
    done: bool,
}

impl LoadApp {
    /// Attach a freshly-filled buffer, request the next frame callback,
    /// and commit. This is the whole load: one buffer per frame, contents
    /// derived from the frame counter so consecutive frames differ.
    fn draw(&mut self, qh: &QueueHandle<Self>) {
        let Some(window) = self.window.as_ref() else {
            return;
        };
        let (w, h) = (self.width.max(1), self.height.max(1));
        let stride = w as i32 * 4;
        let Ok((buffer, canvas)) =
            self.pool
                .create_buffer(w as i32, h as i32, stride, wl_shm::Format::Argb8888)
        else {
            return;
        };
        // Moving diagonal gradient: cheap to generate, different every
        // frame, and visually obvious in a snapshot when something is
        // wrong with the import path.
        let phase = (self.frames % 256) as u32;
        for y in 0..h {
            let row = &mut canvas[(y * w * 4) as usize..((y + 1) * w * 4) as usize];
            for x in 0..w {
                let px = &mut row[(x * 4) as usize..(x * 4 + 4) as usize];
                let v = ((x + y + phase) % 256) as u8;
                px[0] = v; // B
                px[1] = 0x40; // G
                px[2] = 0xFF - v; // R
                px[3] = 0xFF; // A
            }
        }
        let surface = window.wl_surface();
        surface.damage_buffer(0, 0, w as i32, h as i32);
        // Ask for a frame callback *before* committing so the compositor
        // sees the request with this commit. Without it the client would
        // free-run and swamp the socket instead of pacing to the
        // compositor's presentation.
        surface.frame(qh, surface.clone());
        if buffer.attach_to(surface).is_err() {
            return;
        }
        surface.commit();
        self.frames += 1;
    }
}

impl CompositorHandler for LoadApp {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: i32,
    ) {
    }
    fn frame(&mut self, _: &Connection, qh: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {
        if Instant::now() >= self.deadline {
            self.done = true;
            return;
        }
        self.draw(qh);
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
    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }
}

impl OutputHandler for LoadApp {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl WindowHandler for LoadApp {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window) {
        self.done = true;
    }
    fn configure(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        _window: &Window,
        configure: WindowConfigure,
        _serial: u32,
    ) {
        if let Some(w) = configure.new_size.0 {
            self.width = w.get();
        }
        if let Some(h) = configure.new_size.1 {
            self.height = h.get();
        }
        // The pool has to be big enough for the configured size; growing
        // is cheap and happens at most a couple of times.
        let need = (self.width * self.height * 4) as usize;
        let _ = self.pool.resize(need.max(4096));
        if !self.configured {
            self.configured = true;
            self.started = Instant::now();
        }
        self.draw(qh);
    }
}

impl ShmHandler for LoadApp {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm_state
    }
}

impl ProvidesRegistryState for LoadApp {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState];
}

delegate_compositor!(LoadApp);
delegate_output!(LoadApp);
delegate_shm!(LoadApp);
delegate_xdg_shell!(LoadApp);
delegate_xdg_window!(LoadApp);
delegate_registry!(LoadApp);

fn main() {
    let opts = Opts::parse(std::env::args().skip(1));
    let conn = match Connection::connect_to_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("qos-loadclient: no wayland display ({e})");
            std::process::exit(1);
        }
    };
    let (globals, mut event_queue) =
        registry_queue_init(&conn).expect("qos-loadclient: registry init");
    let qh = event_queue.handle();

    let compositor_state =
        CompositorState::bind(&globals, &qh).expect("qos-loadclient: wl_compositor missing");
    let shm_state = Shm::bind(&globals, &qh).expect("qos-loadclient: wl_shm missing");
    let xdg_shell_state = XdgShell::bind(&globals, &qh).expect("qos-loadclient: xdg_shell missing");
    let pool = SlotPool::new((opts.width * opts.height * 4) as usize, &shm_state)
        .expect("qos-loadclient: shm pool");

    let now = Instant::now();
    let mut app = LoadApp {
        registry_state: RegistryState::new(&globals),
        output_state: OutputState::new(&globals, &qh),
        compositor_state,
        shm_state,
        xdg_shell_state,
        window: None,
        pool,
        width: opts.width,
        height: opts.height,
        frames: 0,
        configured: false,
        started: now,
        deadline: now + Duration::from_secs_f64(opts.seconds),
        done: false,
    };

    let window = app.xdg_shell_state.create_window(
        app.compositor_state.create_surface(&qh),
        WindowDecorations::ServerDefault,
        &qh,
    );
    window.set_title("qos load client");
    window.set_app_id(&opts.app_id);
    if opts.fullscreen {
        window.set_fullscreen(None);
    }
    window.commit();
    app.window = Some(window);

    while !app.done {
        if event_queue.blocking_dispatch(&mut app).is_err() {
            break;
        }
    }
    let elapsed = app.started.elapsed().as_secs_f64();
    let fps = if elapsed > 0.0 {
        app.frames as f64 / elapsed
    } else {
        0.0
    };
    // Machine-readable so the harness can parse it out of stdout.
    println!("LOADCLIENT frames={} seconds={elapsed:.3} fps={fps:.2}", app.frames);
}

#[cfg(test)]
mod tests {
    use super::Opts;

    fn v(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn opts_defaults_are_windowed_720p_ten_seconds() {
        let o = Opts::parse(v(&[]));
        assert!(!o.fullscreen);
        assert_eq!((o.width, o.height), (1280, 720));
        assert_eq!(o.seconds, 10.0);
        assert_eq!(o.app_id, "qos-loadclient");
    }

    #[test]
    fn opts_parse_fullscreen_size_and_duration() {
        let o = Opts::parse(v(&[
            "--fullscreen",
            "--size",
            "1600x900",
            "--seconds",
            "3.5",
            "--app-id",
            "bench",
        ]));
        assert!(o.fullscreen, "--fullscreen must set the fullscreen request");
        assert_eq!((o.width, o.height), (1600, 900));
        assert_eq!(o.seconds, 3.5);
        assert_eq!(o.app_id, "bench");
    }

    #[test]
    fn opts_reject_degenerate_values_and_keep_defaults() {
        // A zero-area size or non-positive duration would make the client
        // useless as load; fall back rather than crash the harness.
        let o = Opts::parse(v(&["--size", "0x900", "--seconds", "0"]));
        assert_eq!((o.width, o.height), (1280, 720));
        assert_eq!(o.seconds, 10.0);
        let garbage = Opts::parse(v(&["--size", "nonsense", "--seconds", "nonsense"]));
        assert_eq!((garbage.width, garbage.height), (1280, 720));
        assert_eq!(garbage.seconds, 10.0);
    }
}
