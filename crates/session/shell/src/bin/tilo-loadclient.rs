#[cfg(target_os = "linux")]
mod linux {
    //! Continuously-painting Wayland client, the load generator for the
    //! compositor frame-loop benchmark (`scripts/perf/frame-loop-bench.py`).
    //! Measuring the loop without such a client only measures the idle path.
    //!
    //! ```text
    //! tilo-loadclient [--fullscreen] [--size WxH] [--app-id ID] [--seconds N]
    //! ```
    //!
    //! Reuses released SHM buffers and changes one visible scanline on every
    //! `wl_surface.frame` callback. This keeps every commit observably different
    //! without spending the compositor's synchronous client-dispatch phase filling
    //! an entire 1600x900 CPU image. Prints frame count and wall time on exit.

    #![cfg_attr(
        test,
        allow(
            clippy::float_cmp,
            reason = "parser tests compare exact input and default values"
        )
    )]

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

    /// `width * height * 4` bytes (RGBA8), computed in `usize` and saturating rather than
    /// wrapping on the multiplication — `width`/`height` come from `--size` or the
    /// compositor's `configure` event, neither of which is bounded.
    fn pixel_buffer_size(width: u32, height: u32) -> usize {
        usize::try_from(width)
            .unwrap_or(usize::MAX)
            .saturating_mul(usize::try_from(height).unwrap_or(usize::MAX))
            .saturating_mul(4)
    }

    struct Opts {
        fullscreen: bool,
        width: u32,
        height: u32,
        app_id: String,
        seconds: f64,
    }

    impl Opts {
        /// Parse `argv`. Unknown flags are ignored rather than fatal, so a
        /// harness passing a flag an older binary doesn't know still produces
        /// load instead of exiting non-zero.
        fn parse<I: IntoIterator<Item = String>>(args: I) -> Self {
            let mut o = Opts {
                fullscreen: false,
                width: 1280,
                height: 720,
                app_id: "tilo-loadclient".to_string(),
                seconds: 10.0,
            };
            let mut it = args.into_iter();
            while let Some(a) = it.next() {
                match a.as_str() {
                    "--fullscreen" => o.fullscreen = true,
                    "--size" => {
                        if let Some(v) = it.next()
                            && let Some((w, h)) = v.split_once(['x', 'X'])
                            && let (Ok(w), Ok(h)) = (w.trim().parse(), h.trim().parse())
                            && w > 0
                            && h > 0
                        {
                            o.width = w;
                            o.height = h;
                        }
                    }
                    "--app-id" => {
                        if let Some(v) = it.next() {
                            o.app_id = v;
                        }
                    }
                    "--seconds" => {
                        if let Some(v) = it.next()
                            && let Ok(s) = v.trim().parse::<f64>()
                            && s > 0.0
                        {
                            o.seconds = s;
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
        buffer: Option<Buffer>,
        width: u32,
        height: u32,
        frames: u64,
        configured: bool,
        draw_pending: bool,
        started: Instant,
        deadline: Instant,
        done: bool,
    }

    impl LoadApp {
        /// Attach a freshly-filled buffer, request the next frame callback, and
        /// commit — one buffer per frame, contents keyed to the frame counter.
        fn draw(&mut self, qh: &QueueHandle<Self>) {
            let Some(window) = self.window.as_ref() else {
                return;
            };
            let (width, height) = (self.width.max(1), self.height.max(1));
            let Some((w_px, h_px, stride)) = Self::buffer_geometry(width, height) else {
                return;
            };
            // Reuse the released buffer; allocate a fresh one while the
            // compositor still holds the previous frame.
            if self
                .buffer
                .as_ref()
                .is_none_or(|buffer| self.pool.canvas(buffer).is_none())
            {
                let Ok((fresh, _)) =
                    self.pool
                        .create_buffer(w_px, h_px, stride, wl_shm::Format::Argb8888)
                else {
                    return;
                };
                self.buffer = Some(fresh);
            }
            let Some(buffer) = self.buffer.as_ref() else {
                return;
            };
            let Some(canvas) = self.pool.canvas(buffer) else {
                return;
            };
            // One moving scanline makes every frame observably different while
            // bounding client callback work to O(width), not O(width*height).
            let phase = (self.frames % 256) as u32;
            // `stride` is `w_px.checked_mul(4)`, so it is always > 0 here; the
            // conversion cannot fail.
            let Ok(row_len) = usize::try_from(stride) else {
                return;
            };
            let row_idx = self
                .frames
                .checked_rem(u64::from(height))
                .and_then(|row| usize::try_from(row).ok())
                .unwrap_or(0);
            let Some(row) = canvas.chunks_exact_mut(row_len).nth(row_idx) else {
                return;
            };
            for (col_idx, pixel) in row.chunks_exact_mut(4).enumerate() {
                let value = u8::try_from(
                    u32::try_from(col_idx)
                        .unwrap_or(0)
                        .wrapping_add(phase)
                        .wrapping_rem(256),
                )
                .unwrap_or(0);
                pixel.copy_from_slice(&[value, 0x40, 0xFFu8.wrapping_sub(value), 0xFF]);
            }
            let surface = window.wl_surface();
            surface.damage_buffer(0, i32::try_from(row_idx).unwrap_or(0), w_px, 1);
            // Request the frame callback *before* committing so the compositor
            // sees it with this commit; otherwise the client free-runs and
            // swamps the socket instead of pacing to presentation.
            surface.frame(qh, surface.clone());
            if buffer.attach_to(surface).is_err() {
                return;
            }
            surface.commit();
            // Frame count over a bounded run; saturating is defensive, not load-bearing.
            self.frames = self.frames.saturating_add(1);
        }

        /// Convert width/height to the `i32` + byte-stride triple `SlotPool::create_buffer`
        /// wants. `w`/`h` come from `--size` or the compositor's `configure` event, neither
        /// of which is bounded — `None` degrades to "skip this frame" rather than wrapping
        /// the stride into a buffer smaller than the canvas.
        fn buffer_geometry(w: u32, h: u32) -> Option<(i32, i32, i32)> {
            let w_px = i32::try_from(w).ok()?;
            let h_px = i32::try_from(h).ok()?;
            let stride = w_px.checked_mul(4)?;
            Some((w_px, h_px, stride))
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
        fn frame(
            &mut self,
            _: &Connection,
            qh: &QueueHandle<Self>,
            _: &wl_surface::WlSurface,
            _: u32,
        ) {
            if Instant::now() >= self.deadline {
                self.done = true;
                return;
            }
            let _ = qh;
            self.draw_pending = true;
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
            let need = pixel_buffer_size(self.width, self.height);
            let _ = self.pool.resize(need.max(4096));
            if !self.configured {
                self.configured = true;
                self.started = Instant::now();
            }
            let _ = qh;
            self.draw_pending = true;
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

    pub(super) fn main() -> Result<(), Box<dyn std::error::Error>> {
        let opts = Opts::parse(std::env::args().skip(1));
        let conn = match Connection::connect_to_env() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("tilo-loadclient: no wayland display ({e})");
                std::process::exit(1);
            }
        };
        let (globals, mut event_queue) = registry_queue_init(&conn)?;
        let qh = event_queue.handle();

        let compositor_state = CompositorState::bind(&globals, &qh)?;
        let shm_state = Shm::bind(&globals, &qh)?;
        let xdg_shell_state = XdgShell::bind(&globals, &qh)?;
        let pool = SlotPool::new(pixel_buffer_size(opts.width, opts.height), &shm_state)?;

        let now = Instant::now();
        // A `--seconds` pathological enough to overflow `Instant`'s range degrades to
        // "already expired" instead of panicking — the harness just gets a 0-frame run.
        let deadline = now
            .checked_add(Duration::from_secs_f64(opts.seconds))
            .unwrap_or(now);
        let mut app = LoadApp {
            registry_state: RegistryState::new(&globals),
            output_state: OutputState::new(&globals, &qh),
            compositor_state,
            shm_state,
            xdg_shell_state,
            window: None,
            pool,
            buffer: None,
            width: opts.width,
            height: opts.height,
            frames: 0,
            configured: false,
            draw_pending: false,
            started: now,
            deadline,
            done: false,
        };

        let window = app.xdg_shell_state.create_window(
            app.compositor_state.create_surface(&qh),
            WindowDecorations::ServerDefault,
            &qh,
        );
        window.set_title("tilo load client");
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
            // Wayland callback handlers run inside the compositor's synchronous
            // `dispatch_clients` stack in this in-process benchmark setup. Keep
            // drawing outside those callbacks so the load generator cannot make
            // compositor client dispatch look slow merely by filling SHM.
            if app.draw_pending {
                app.draw_pending = false;
                app.draw(&qh);
            }
        }
        let elapsed = app.started.elapsed().as_secs_f64();
        // Reporting-only fps figure; a run would need to sustain >2^53 frames
        // (decades at any real frame rate) before this cast could lose precision.
        #[allow(
            clippy::cast_precision_loss,
            reason = "reporting-only fps figure; frame counts never approach 2^53"
        )]
        let fps = if elapsed > 0.0 {
            app.frames as f64 / elapsed
        } else {
            0.0
        };
        // Machine-readable so the harness can parse it out of stdout.
        println!(
            "LOADCLIENT frames={} seconds={elapsed:.3} fps={fps:.2}",
            app.frames
        );
        Ok(())
    }

    #[cfg(test)]
    #[allow(
        clippy::float_cmp,
        reason = "exact deterministic load-shape fixtures are the behavior under test"
    )]
    mod tests {
        use super::Opts;

        fn v(args: &[&str]) -> Vec<String> {
            args.iter().map(std::string::ToString::to_string).collect()
        }

        #[test]
        fn load_client_options_default_parse_and_reject_degenerate_values() {
            // Phase `opts_defaults_are_windowed_720p_ten_seconds`.
            {
                let o = Opts::parse(v(&[]));
                assert!(!o.fullscreen);
                assert_eq!((o.width, o.height), (1280, 720));
                assert_eq!(o.seconds, 10.0);
                assert_eq!(o.app_id, "tilo-loadclient");
            }
            // Phase `opts_parse_fullscreen_size_and_duration`.
            {
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
            // Phase `opts_reject_degenerate_values_and_keep_defaults`.
            {
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
    }
}

#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    linux::main()
}

#[cfg(not(target_os = "linux"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    Err("the Wayland load client requires Linux".into())
}
