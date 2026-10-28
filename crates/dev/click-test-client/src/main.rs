#[cfg(target_os = "linux")]
mod linux {
    use smithay_client_toolkit::{
        compositor::{CompositorHandler, CompositorState},
        delegate_compositor, delegate_keyboard, delegate_output, delegate_pointer,
        delegate_registry, delegate_seat, delegate_shm, delegate_xdg_shell, delegate_xdg_window,
        output::{OutputHandler, OutputState},
        registry::{ProvidesRegistryState, RegistryState},
        registry_handlers,
        seat::{
            Capability, SeatHandler, SeatState,
            keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers},
            pointer::{PointerEvent, PointerEventKind, PointerHandler},
        },
        shell::{
            WaylandSurface,
            xdg::{
                XdgShell,
                window::{Window, WindowConfigure, WindowHandler},
            },
        },
        shm::{Shm, ShmHandler, slot::SlotPool},
    };
    use wayland_client::{
        Connection, QueueHandle,
        globals::registry_queue_init,
        protocol::{wl_keyboard, wl_output, wl_pointer, wl_seat, wl_shm, wl_surface},
    };

    /// Convert width/height to the `i32` + byte-stride triple `SlotPool::create_buffer`
    /// wants. `w`/`h` come from the compositor's `configure` event, which is not bounded
    /// by this client — `None` degrades to "skip this frame" rather than wrapping the
    /// stride into a buffer smaller than the canvas.
    fn buffer_geometry(w: u32, h: u32) -> Option<(i32, i32, i32)> {
        let w_px = i32::try_from(w).ok()?;
        let h_px = i32::try_from(h).ok()?;
        let stride = w_px.checked_mul(4)?;
        Some((w_px, h_px, stride))
    }

    struct ClientApp {
        registry_state: RegistryState,
        seat_state: SeatState,
        output_state: OutputState,
        compositor_state: CompositorState,
        shm_state: Shm,
        xdg_shell_state: XdgShell,

        window: Option<Window>,
        pool: SlotPool,
        width: u32,
        height: u32,
        first_configure: bool,
        /// Input events from the dispatch in progress. Published together
        /// after `blocking_dispatch` returns, so a click cannot become
        /// visible while key presses already read in that queue are still
        /// waiting to be delivered.
        pending_events: Vec<String>,
    }

    impl ClientApp {
        fn new(
            globals: &wayland_client::globals::GlobalList,
            qh: &QueueHandle<Self>,
        ) -> Result<Self, String> {
            let registry_state = RegistryState::new(globals);
            let seat_state = SeatState::new(globals, qh);
            let output_state = OutputState::new(globals, qh);
            let compositor_state = CompositorState::bind(globals, qh)
                .map_err(|e| format!("wl_compositor not available: {e}"))?;
            let shm_state =
                Shm::bind(globals, qh).map_err(|e| format!("wl_shm not available: {e}"))?;
            let xdg_shell_state =
                XdgShell::bind(globals, qh).map_err(|e| format!("xdg_shell not available: {e}"))?;

            let pool = SlotPool::new(256 * 256 * 4, &shm_state)
                .map_err(|e| format!("failed to create pool: {e}"))?;

            Ok(Self {
                registry_state,
                seat_state,
                output_state,
                compositor_state,
                shm_state,
                xdg_shell_state,
                window: None,
                pool,
                width: 256,
                height: 256,
                first_configure: true,
                pending_events: Vec::new(),
            })
        }

        fn record_event(&mut self, line: impl Into<String>) {
            self.pending_events.push(line.into());
        }
    }

    /// One write for every input event this dispatch delivered. Separate
    /// lines would let a reader observe the click before key presses that
    /// were already queued behind it in the same read.
    fn publish_events(app: &mut ClientApp) {
        if app.pending_events.is_empty() {
            return;
        }
        let record = app.pending_events.drain(..).collect::<Vec<_>>().join(" ");
        println!("{record}");
        let _ = std::io::Write::flush(&mut std::io::stdout());
    }

    impl CompositorHandler for ClientApp {
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
            _: &QueueHandle<Self>,
            _: &wl_surface::WlSurface,
            _: u32,
        ) {
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

    impl OutputHandler for ClientApp {
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

    impl WindowHandler for ClientApp {
        fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window) {}
        fn configure(
            &mut self,
            _conn: &Connection,
            _qh: &QueueHandle<Self>,
            _window: &Window,
            configure: WindowConfigure,
            _serial: u32,
        ) {
            self.width = configure.new_size.0.map_or(256, std::num::NonZero::get);
            self.height = configure.new_size.1.map_or(256, std::num::NonZero::get);
            let Some((w_px, h_px, stride)) = buffer_geometry(self.width, self.height) else {
                return;
            };
            let Ok((buffer, canvas)) =
                self.pool
                    .create_buffer(w_px, h_px, stride, wl_shm::Format::Argb8888)
            else {
                return;
            };
            for chunk in canvas.chunks_exact_mut(4) {
                let [blue, green, red, alpha] = chunk else {
                    continue; // chunks_exact_mut(4) guarantees len == 4
                };
                *blue = 0x00;
                *green = 0x00;
                *red = 0xFF;
                *alpha = 0xFF;
            }
            if self.first_configure {
                self.first_configure = false;
                if let Some(window) = &self.window {
                    window.wl_surface().attach(Some(buffer.wl_buffer()), 0, 0);
                    window.wl_surface().commit();
                }
            }
        }
    }

    impl SeatHandler for ClientApp {
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
            if capability == Capability::Pointer {
                // Best-effort: without a pointer this dev client just won't report clicks.
                let _ = self.seat_state.get_pointer(qh, &seat);
            } else if capability == Capability::Keyboard {
                let _ = self.seat_state.get_keyboard(qh, &seat, None);
            }
        }
        fn remove_capability(
            &mut self,
            _: &Connection,
            _: &QueueHandle<Self>,
            _: wl_seat::WlSeat,
            _: Capability,
        ) {
        }
        fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    }

    impl KeyboardHandler for ClientApp {
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
            self.record_event("EVENT:KEYBOARD ENTER");
        }

        fn leave(
            &mut self,
            _: &Connection,
            _: &QueueHandle<Self>,
            _: &wl_keyboard::WlKeyboard,
            _: &wl_surface::WlSurface,
            _: u32,
        ) {
            self.record_event("EVENT:KEYBOARD LEAVE");
        }

        fn press_key(
            &mut self,
            _: &Connection,
            _: &QueueHandle<Self>,
            _: &wl_keyboard::WlKeyboard,
            _: u32,
            event: KeyEvent,
        ) {
            self.record_event(format!("EVENT:KEY PRESS {:?}", event.utf8));
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

    impl PointerHandler for ClientApp {
        fn pointer_frame(
            &mut self,
            _: &Connection,
            _: &QueueHandle<Self>,
            _: &wl_pointer::WlPointer,
            events: &[PointerEvent],
        ) {
            for event in events {
                match event.kind {
                    PointerEventKind::Enter { .. } => {
                        self.record_event("EVENT:ENTER");
                    }
                    PointerEventKind::Leave { .. } => {
                        self.record_event("EVENT:LEAVE");
                    }
                    PointerEventKind::Press { .. } => {
                        let (x, y) = event.position;
                        self.record_event(format!("EVENT:CLICK AT {x:.1},{y:.1}"));
                    }
                    _ => {}
                }
            }
        }
    }

    impl ShmHandler for ClientApp {
        fn shm_state(&mut self) -> &mut Shm {
            &mut self.shm_state
        }
    }
    impl ProvidesRegistryState for ClientApp {
        fn registry(&mut self) -> &mut RegistryState {
            &mut self.registry_state
        }
        registry_handlers![OutputState, SeatState];
    }

    delegate_compositor!(ClientApp);
    delegate_output!(ClientApp);
    delegate_shm!(ClientApp);
    delegate_seat!(ClientApp);
    delegate_pointer!(ClientApp);
    delegate_keyboard!(ClientApp);
    delegate_xdg_shell!(ClientApp);
    delegate_xdg_window!(ClientApp);
    delegate_registry!(ClientApp);

    pub(super) fn main() -> Result<(), String> {
        let conn = Connection::connect_to_env().map_err(|e| format!("connect_to_env: {e}"))?;
        let (globals, mut event_queue) =
            registry_queue_init(&conn).map_err(|e| format!("registry_queue_init: {e}"))?;
        let qh = event_queue.handle();
        let mut app = ClientApp::new(&globals, &qh)?;
        let window = app.xdg_shell_state.create_window(
            app.compositor_state.create_surface(&qh),
            smithay_client_toolkit::shell::xdg::window::WindowDecorations::ServerDefault,
            &qh,
        );
        window.set_title("Click Test");
        window.set_app_id("click-test");
        window.commit();
        app.window = Some(window);
        loop {
            event_queue
                .blocking_dispatch(&mut app)
                .map_err(|e| format!("blocking_dispatch: {e}"))?;
            publish_events(&mut app);
        }
    }
}
#[cfg(target_os = "linux")]
fn main() -> Result<(), String> {
    linux::main()
}
#[cfg(not(target_os = "linux"))]
fn main() -> Result<(), String> {
    Err("the Wayland click test client requires Linux".into())
}
