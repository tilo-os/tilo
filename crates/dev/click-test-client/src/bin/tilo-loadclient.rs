//! A Wayland client that keeps the desktop busy drawing: every window commits a frame for every
//! frame the compositor shows, so it has work to composite and a tool can measure what that costs.
//! `--windows N` keeps N windows drawing at once, `--fullscreen` fills the display as a video player
//! does. At the end it answers `LOADCLIENT frames=<n> windows=<n> seconds=<s>`.
//!
//! It never starts a session and reads no session state: it connects to `$WAYLAND_DISPLAY` on
//! `$XDG_RUNTIME_DIR`, like an app.
//!
//!     cargo build -p click-test-client --bin tilo-loadclient
//!     WAYLAND_DISPLAY=wayland-0 tilo-loadclient --seconds 8 --windows 4

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "a measurement client: a protocol step that fails or a size it cannot draw stops the run, and its lines are its interface"
)]

#[cfg(target_os = "linux")]
mod linux {
    use std::io::Write as _;
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
        shm::{
            Shm, ShmHandler,
            slot::{Buffer, SlotPool},
        },
    };
    use wayland_client::{
        Connection, QueueHandle,
        globals::registry_queue_init,
        protocol::{wl_output, wl_shm, wl_surface},
    };

    const HELP: &str = "\
usage: tilo-loadclient [--seconds S] [--windows N] [--fullscreen]
                       [--size WxH] [--app-id ID] [--title TITLE]

  --seconds S    how long the windows keep drawing (default 8)
  --windows N    how many windows draw at once (default 1)
  --fullscreen   the first window fills the display
  --size WxH     the size a window asks for (default 640x480)
  --app-id ID    the application id its windows carry
";

    /// What the command line asked for.
    struct Options {
        size: (u32, u32),
        app_id: String,
        title: String,
        seconds: f64,
        fullscreen: bool,
        windows: u32,
    }

    impl Default for Options {
        fn default() -> Self {
            Self {
                size: (640, 480),
                app_id: "dev.tilo.loadclient".to_owned(),
                title: "Frame load".to_owned(),
                seconds: 8.0,
                fullscreen: false,
                windows: 1,
            }
        }
    }

    impl Options {
        fn parse() -> Result<Self, String> {
            let mut options = Self::default();
            let mut args = std::env::args().skip(1);
            while let Some(flag) = args.next() {
                let mut value = |flag: &str| -> Result<String, String> {
                    args.next().ok_or_else(|| format!("{flag} takes a value"))
                };
                match flag.as_str() {
                    "--size" => {
                        let size = value("--size")?;
                        let (width, height) = size
                            .split_once('x')
                            .ok_or_else(|| format!("--size takes WxH, not {size}"))?;
                        options.size = (
                            width
                                .parse()
                                .map_err(|_| format!("--size takes WxH, not {size}"))?,
                            height
                                .parse()
                                .map_err(|_| format!("--size takes WxH, not {size}"))?,
                        );
                    }
                    "--app-id" => options.app_id = value("--app-id")?,
                    "--title" => options.title = value("--title")?,
                    "--seconds" => {
                        let seconds = value("--seconds")?;
                        options.seconds = seconds
                            .parse()
                            .map_err(|_| format!("--seconds takes a number, not {seconds}"))?;
                    }
                    "--windows" => {
                        let windows = value("--windows")?;
                        options.windows = windows
                            .parse()
                            .map_err(|_| format!("--windows takes a count, not {windows}"))?;
                    }
                    "--fullscreen" => options.fullscreen = true,
                    "-h" | "--help" => {
                        print!("{HELP}");
                        std::process::exit(0);
                    }
                    other => return Err(format!("unknown option {other}")),
                }
            }
            if options.windows == 0 {
                return Err("--windows takes at least one window".to_owned());
            }
            Ok(options)
        }
    }

    /// One toplevel this client draws in.
    struct LoadWindow {
        window: Window,
        /// What it draws at: the compositor's configure, or the requested size until it arrives.
        size: (u32, u32),
        /// The buffer it is drawing into, kept from one frame to the next.
        buffer: Option<Buffer>,
        configured: bool,
    }

    struct Client {
        options: Options,
        registry_state: RegistryState,
        output_state: OutputState,
        shm: Shm,
        pool: SlotPool,
        windows: Vec<LoadWindow>,
        /// Frames committed on every window together.
        frames: u64,
        /// When the first window was ready: the run is as long from there.
        started: Option<Instant>,
        exit: bool,
    }

    impl Client {
        /// Draw window `index` and commit it: one frame the compositor counts, with content that
        /// changes every frame so every commit is a repaint. It draws in a buffer the compositor is
        /// not holding, so a frame never waits for the previous one to come back.
        fn draw(&mut self, qh: &QueueHandle<Self>, index: usize) {
            let Some((surface, (width, height))) = self
                .windows
                .get(index)
                .filter(|window| window.configured)
                .map(|window| (window.window.wl_surface().clone(), window.size))
            else {
                return;
            };
            let across = i32::try_from(width).expect("width");
            let down = i32::try_from(height).expect("height");
            let stride = across * 4;
            // ARGB8888 in memory is B, G, R, A: a blue whose shade walks with the frame count.
            let shade = u8::try_from(self.frames % 256).unwrap_or(0);
            let pixel = [shade, 0x40, 0x20, 0xFF];
            let mut held = self
                .windows
                .get_mut(index)
                .and_then(|window| window.buffer.take());
            let canvas =
                if let Some(canvas) = held.as_ref().and_then(|buffer| self.pool.canvas(buffer)) {
                    canvas
                } else {
                    let (buffer, canvas) = self
                        .pool
                        .create_buffer(across, down, stride, wl_shm::Format::Argb8888)
                        .expect("a buffer of the window's size");
                    held = Some(buffer);
                    canvas
                };
            for chunk in canvas.chunks_exact_mut(4) {
                chunk.copy_from_slice(&pixel);
            }
            if let Some(buffer) = held.as_ref() {
                buffer.attach_to(&surface).expect("attach the frame");
            }
            surface.damage_buffer(0, 0, across, down);
            // Every commit asks for the frame that shows it: that answer is when the next one is
            // drawn.
            surface.frame(qh, surface.clone());
            if let Some(window) = self.windows.get_mut(index) {
                window.window.commit();
                window.buffer = held;
            }
            self.frames = self.frames.saturating_add(1);
        }

        fn index_of(&self, surface: &wl_surface::WlSurface) -> Option<usize> {
            self.windows
                .iter()
                .position(|window| window.window.wl_surface() == surface)
        }

        /// Stop when the run's time is up; `false` when it is not.
        fn finished(&mut self) -> bool {
            let Some(started) = self.started else {
                return false;
            };
            if started.elapsed() < Duration::from_secs_f64(self.options.seconds.max(0.0)) {
                return false;
            }
            self.exit = true;
            true
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
        /// The compositor showed the frame this window's last commit carries: drawing the next one
        /// now keeps exactly one commit in flight, at the display's pace.
        fn frame(
            &mut self,
            _: &Connection,
            qh: &QueueHandle<Self>,
            surface: &wl_surface::WlSurface,
            _: u32,
        ) {
            if self.finished() {
                return;
            }
            if let Some(index) = self.index_of(surface) {
                self.draw(qh, index);
            }
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

    impl WindowHandler for Client {
        fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window) {
            self.exit = true;
        }
        fn configure(
            &mut self,
            _: &Connection,
            qh: &QueueHandle<Self>,
            window: &Window,
            configure: WindowConfigure,
            _: u32,
        ) {
            let Some(index) = self
                .windows
                .iter()
                .position(|candidate| candidate.window.wl_surface() == window.wl_surface())
            else {
                return;
            };
            if let Some(entry) = self.windows.get_mut(index) {
                entry.size = match (configure.new_size.0, configure.new_size.1) {
                    (Some(width), Some(height)) => (width.get(), height.get()),
                    _ => self.options.size,
                };
                entry.buffer = None;
                entry.configured = true;
            }
            // A window is on screen only with a buffer in it, so the first frame is drawn as soon
            // as the compositor says what size it has.
            self.draw(qh, index);
            if self.started.is_none() {
                self.started = Some(Instant::now());
            }
        }
    }

    impl ShmHandler for Client {
        fn shm_state(&mut self) -> &mut Shm {
            &mut self.shm
        }
    }

    impl OutputHandler for Client {
        fn output_state(&mut self) -> &mut OutputState {
            &mut self.output_state
        }
        fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
        fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {
        }
        fn output_destroyed(
            &mut self,
            _: &Connection,
            _: &QueueHandle<Self>,
            _: wl_output::WlOutput,
        ) {
        }
    }

    impl ProvidesRegistryState for Client {
        fn registry(&mut self) -> &mut RegistryState {
            &mut self.registry_state
        }
        registry_handlers![OutputState];
    }

    delegate_compositor!(Client);
    delegate_output!(Client);
    delegate_shm!(Client);
    delegate_xdg_shell!(Client);
    delegate_xdg_window!(Client);
    delegate_registry!(Client);

    pub(super) fn main() -> Result<(), String> {
        let options = Options::parse()?;
        let conn =
            Connection::connect_to_env().map_err(|error| format!("connect_to_env: {error}"))?;
        let (globals, mut queue) =
            registry_queue_init(&conn).map_err(|error| format!("registry_queue_init: {error}"))?;
        let qh = queue.handle();
        let compositor = CompositorState::bind(&globals, &qh)
            .map_err(|error| format!("wl_compositor not available: {error}"))?;
        let shell = XdgShell::bind(&globals, &qh)
            .map_err(|error| format!("xdg_wm_base not available: {error}"))?;
        let shm =
            Shm::bind(&globals, &qh).map_err(|error| format!("wl_shm not available: {error}"))?;
        let pool =
            SlotPool::new(256 * 256 * 4, &shm).map_err(|error| format!("shm pool: {error}"))?;
        let mut windows = Vec::new();
        for index in 0..options.windows {
            let surface = compositor.create_surface(&qh);
            let window = shell.create_window(surface, WindowDecorations::RequestServer, &qh);
            window.set_title(options.title.clone());
            window.set_app_id(options.app_id.clone());
            if index == 0 && options.fullscreen {
                // A fullscreen window takes the display's size, so it pins none of its own.
                window.set_fullscreen(None);
            } else {
                // The size it asks for, not a range: a desk gives a window that pins its size a tab
                // of that size instead of half the display.
                window.set_max_size(Some(options.size));
            }
            window.commit();
            windows.push(LoadWindow {
                window,
                size: options.size,
                buffer: None,
                configured: false,
            });
        }
        let mut client = Client {
            options,
            registry_state: RegistryState::new(&globals),
            output_state: OutputState::new(&globals, &qh),
            shm,
            pool,
            windows,
            frames: 0,
            started: None,
            exit: false,
        };
        while !client.exit {
            // Frames keep arriving while the windows draw, so each dispatch either draws the next
            // one or is the run's last.
            match queue.blocking_dispatch(&mut client) {
                Ok(_) => {}
                // The session ended under the run: the frames it drew are the answer, not a failure.
                Err(error) if client.frames > 0 => {
                    eprintln!("tilo-loadclient: the session closed: {error}");
                    break;
                }
                Err(error) => return Err(format!("dispatch: {error}")),
            }
        }
        println!(
            "LOADCLIENT frames={} windows={} seconds={}",
            client.frames, client.options.windows, client.options.seconds
        );
        std::io::stdout().flush().expect("flush");
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn main() -> Result<(), String> {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() -> Result<(), String> {
    Err("tilo-loadclient draws on Wayland, which needs Linux".into())
}
