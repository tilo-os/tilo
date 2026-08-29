use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_output, delegate_pointer, delegate_registry, delegate_seat,
    delegate_shm, delegate_xdg_shell, delegate_xdg_window,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
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
    protocol::{wl_output, wl_pointer, wl_seat, wl_shm, wl_surface},
};

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
}

impl ClientApp {
    fn new(globals: &wayland_client::globals::GlobalList, qh: &QueueHandle<Self>) -> Self {
        let registry_state = RegistryState::new(globals);
        let seat_state = SeatState::new(globals, qh);
        let output_state = OutputState::new(globals, qh);
        let compositor_state =
            CompositorState::bind(globals, qh).expect("wl_compositor not available");
        let shm_state = Shm::bind(globals, qh).expect("wl_shm not available");
        let xdg_shell_state = XdgShell::bind(globals, qh).expect("xdg_shell not available");

        let pool = SlotPool::new(256 * 256 * 4, &shm_state).expect("Failed to create pool");

        Self {
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
        }
    }
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
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
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
        let (buffer, canvas) = self
            .pool
            .create_buffer(
                self.width as i32,
                self.height as i32,
                self.width as i32 * 4,
                wl_shm::Format::Argb8888,
            )
            .expect("create buffer");
        for chunk in canvas.chunks_exact_mut(4) {
            chunk[0] = 0x00;
            chunk[1] = 0x00;
            chunk[2] = 0xFF;
            chunk[3] = 0xFF;
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
            self.seat_state
                .get_pointer(qh, &seat)
                .expect("Failed to get pointer");
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
                    println!("EVENT:ENTER");
                }
                PointerEventKind::Press { .. } => {
                    let (x, y) = event.position;
                    println!("EVENT:CLICK AT {x:.1},{y:.1}");
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
delegate_xdg_shell!(ClientApp);
delegate_xdg_window!(ClientApp);
delegate_registry!(ClientApp);

fn main() {
    let conn = Connection::connect_to_env().unwrap();
    let (globals, mut event_queue) = registry_queue_init(&conn).unwrap();
    let qh = event_queue.handle();
    let mut app = ClientApp::new(&globals, &qh);
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
        event_queue.blocking_dispatch(&mut app).unwrap();
    }
}
