use smithay::{
    delegate_compositor, delegate_data_device, delegate_layer_shell, delegate_output,
    delegate_seat, delegate_shm, delegate_xdg_shell,
    desktop::{Space, Window},
    input::{Seat, SeatState, SeatHandler, pointer::{CursorImageStatus, MotionEvent}, keyboard::FilterResult},
    reexports::{
        wayland_server::{
            backend::{ClientData, ClientId, DisconnectReason},
            protocol::{wl_surface::WlSurface, wl_shm},
            Client, DisplayHandle, Resource,
        },
    },
    wayland::{
        compositor::{
            CompositorClientState, CompositorHandler, CompositorState, SurfaceAttributes,
            with_states, Cacheable,
        },
        buffer::BufferHandler,
        selection::{
            data_device::{ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler},
            SelectionHandler, SelectionTarget,
        },
        shell::{
            wlr_layer::{Layer, LayerSurface, WlrLayerShellHandler, WlrLayerShellState},
            xdg::{PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState, XdgToplevelSurfaceData, SurfaceCachedState},
        },
        shm::{ShmHandler, ShmState, with_buffer_contents},
        seat::WaylandFocus,
    },
    output::{Output, PhysicalProperties, Subpixel, Mode as OutputMode},
    utils::{Transform, Size as SmithaySize, Serial},
};
use tokio::sync::mpsc;
use std::time::Instant;

#[derive(Debug, Clone)]
pub enum WaylandEvent {
    Frame { surface_id: u32, app_id: String, pixels: Vec<u8>, width: u32, height: u32 },
    NewWindow { surface_id: u32, app_id: String, suggested_width: Option<u32>, suggested_height: Option<u32> },
    WindowGeometry { surface_id: u32, x: i32, y: i32, width: i32, height: i32 },
    WindowClosed { surface_id: u32 },
}

#[derive(Debug)]
pub enum WaylandInput {
    Motion { x: f64, y: f64, surface_id: Option<u32>, surface_width: f32, surface_height: f32 },
    Button { button: u32, state: smithay::backend::input::ButtonState, surface_id: Option<u32>, surface_width: f32, surface_height: f32 },
    Key { key: u32, state: smithay::backend::input::KeyState },
    Resize { surface_id: u32, width: u32, height: u32 },
}

pub struct WaylandServer {
    pub compositor_state: CompositorState,
    pub xdg_shell_state: XdgShellState,
    pub layer_shell_state: WlrLayerShellState,
    pub shm_state: ShmState,
    pub seat_state: SeatState<Self>,
    pub data_device_state: DataDeviceState,
    pub seat: Seat<Self>,
    pub space: Space<Window>,
    pub output: Output,
    pub start_time: Instant,
    pub serial_counter: u32,
    pub pointer_location: smithay::utils::Point<f64, smithay::utils::Logical>,
    pub event_tx: mpsc::UnboundedSender<WaylandEvent>,
}

pub struct ClientState { pub compositor_state: CompositorClientState }
impl ClientData for ClientState { fn initialized(&self, _: ClientId) {} fn disconnected(&self, _: ClientId, _: DisconnectReason) {} }

impl WaylandServer {
    pub fn new(dh: &DisplayHandle, event_tx: mpsc::UnboundedSender<WaylandEvent>) -> Self {
        let compositor_state = CompositorState::new::<Self>(dh);
        let xdg_shell_state = XdgShellState::new::<Self>(dh);
        let layer_shell_state = WlrLayerShellState::new::<Self>(dh);
        let shm_state = ShmState::new::<Self>(dh, vec![wl_shm::Format::Argb8888, wl_shm::Format::Xrgb8888]);
        let mut seat_state = SeatState::new();
        let data_device_state = DataDeviceState::new::<Self>(dh);
        let mut seat = seat_state.new_wl_seat(dh, "quick-desktop");
        seat.add_pointer();
        seat.add_keyboard(Default::default(), 200, 25).expect("Failed to add keyboard");
        let output = Output::new("quick-desktop-0".into(), PhysicalProperties { size: SmithaySize::from((0, 0)), subpixel: Subpixel::Unknown, make: "QuickDesktop".into(), model: "Virtual".into() });
        let mode = OutputMode { size: SmithaySize::from((1920, 1080)), refresh: 60_000 };
        output.change_current_state(Some(mode), Some(Transform::Normal), None, Some((0, 0).into()));
        output.set_preferred(mode);
        output.create_global::<Self>(dh);
        Self { compositor_state, xdg_shell_state, layer_shell_state, shm_state, seat_state, data_device_state, seat, space: Space::default(), output, start_time: Instant::now(), serial_counter: 0, pointer_location: (0.0, 0.0).into(), event_tx }
    }

    fn next_serial(&mut self) -> Serial {
        self.serial_counter += 1;
        Serial::from(self.serial_counter)
    }

    pub fn handle_input(&mut self, input: WaylandInput) {
        let time = self.start_time.elapsed().as_millis() as u32;
        match input {
            WaylandInput::Motion { x, y, surface_id, surface_width, surface_height } => {
                let pointer = self.seat.get_pointer().unwrap();
                let serial = self.next_serial();
                let surface = surface_id.and_then(|id| self.find_surface_by_id(id));
                if let Some(s) = surface {
                    let mut actual_buf_size = (surface_width as f64, surface_height as f64);
                    with_states(&s, |states| {
                        let mut attrs = states.cached_state.get::<SurfaceAttributes>();
                        if let Some(smithay::wayland::compositor::BufferAssignment::NewBuffer(buffer)) = &attrs.current().buffer {
                            let _ = smithay::wayland::shm::with_buffer_contents(buffer, |_, _, data| {
                                actual_buf_size = (data.width as f64, data.height as f64);
                            });
                        }
                    });
                    let scale_x = actual_buf_size.0 / surface_width as f64;
                    let scale_y = actual_buf_size.1 / surface_height as f64;
                    self.pointer_location = (x * scale_x, y * scale_y).into();
                    pointer.motion(self, Some((s, (0.0, 0.0).into())), &MotionEvent { location: self.pointer_location, serial, time });
                } else {
                    self.pointer_location = (x, y).into();
                    pointer.motion(self, None, &MotionEvent { location: self.pointer_location, serial, time });
                }
                pointer.frame(self);
            }
            WaylandInput::Button { button, state, surface_id, .. } => {
                let pointer = self.seat.get_pointer().unwrap();
                let serial = self.next_serial();
                if let Some(id) = surface_id {
                    if state == smithay::backend::input::ButtonState::Pressed {
                        if let Some(surface) = self.find_surface_by_id(id) {
                            let keyboard = self.seat.get_keyboard().unwrap();
                            keyboard.set_focus(self, Some(surface.clone()), serial);
                            self.activate_window_by_surface(&surface);
                        }
                    }
                }
                pointer.button(self, &smithay::input::pointer::ButtonEvent { button, state, serial, time });
                pointer.frame(self);
            }
            WaylandInput::Key { key, state } => {
                let serial = self.next_serial();
                let keyboard = self.seat.get_keyboard().unwrap();
                keyboard.input(self, key.into(), state, serial, time, |_, _, _| FilterResult::Forward::<()>);
            }
            WaylandInput::Resize { surface_id, width, height } => {
                if let Some(surface) = self.find_surface_by_id(surface_id) {
                    if let Some(window) = self.space.elements().find(|w| w.wl_surface().as_deref() == Some(&surface)) {
                        if let Some(toplevel) = window.toplevel() {
                            toplevel.with_pending_state(|state| {
                                state.size = Some((width as i32, height as i32).into());
                            });
                            toplevel.send_configure();
                        }
                    }
                }
            }
        }
    }

    fn find_surface_by_id(&self, target_id: u32) -> Option<WlSurface> {
        for window in self.space.elements() {
            if let Some(surface) = window.wl_surface() {
                if surface.id().protocol_id() == target_id { return Some(surface.clone().into_owned()); }
            }
        }
        None
    }

    fn activate_window_by_surface(&mut self, surface: &WlSurface) {
        for window in self.space.elements() {
            let is_target = window.wl_surface().as_deref() == Some(surface);
            if let Some(toplevel) = window.toplevel() {
                toplevel.with_pending_state(|state| {
                    if is_target { state.states.set(smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State::Activated); }
                    else { state.states.unset(smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State::Activated); }
                });
                toplevel.send_configure();
            }
        }
    }
}

impl CompositorHandler for WaylandServer {
    fn compositor_state(&mut self) -> &mut CompositorState { &mut self.compositor_state }
    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState { &client.get_data::<ClientState>().unwrap().compositor_state }
    fn commit(&mut self, surface: &WlSurface) {
        if let Some(window) = self.space.elements().find(|w| w.wl_surface().as_deref() == Some(surface)) { window.on_commit(); }
        let tx = self.event_tx.clone();
        let surface_id = surface.id().protocol_id();
        
        let app_id = with_states(surface, |states| {
            states.data_map.get::<XdgToplevelSurfaceData>().and_then(|m| m.lock().unwrap().app_id.clone())
        }).unwrap_or_else(|| "unknown".into());

        with_states(surface, |states| {
            let mut guard = states.cached_state.get::<SurfaceCachedState>();
            if let Some(g) = guard.current().geometry {
                let _ = tx.send(WaylandEvent::WindowGeometry {
                    surface_id,
                    x: g.loc.x, y: g.loc.y,
                    width: g.size.w, height: g.size.h,
                });
            }
        });

        let _ = with_states(surface, |states| {
            let mut attrs = states.cached_state.get::<SurfaceAttributes>();
            let buffer = attrs.current().buffer.as_ref();
            if let Some(smithay::wayland::compositor::BufferAssignment::NewBuffer(buffer)) = buffer {
                let app_id = app_id.clone();
                let _ = with_buffer_contents(buffer, |ptr, len, data| {
                    let mut pixels = unsafe { std::slice::from_raw_parts(ptr, len) }.to_vec();
                    for chunk in pixels.chunks_exact_mut(4) { chunk.swap(0, 2); }
                    let _ = tx.send(WaylandEvent::Frame { surface_id, app_id, pixels, width: data.width as u32, height: data.height as u32 });
                });
            }
        });
    }
}

impl BufferHandler for WaylandServer { fn buffer_destroyed(&mut self, _: &smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer) {} }

impl XdgShellHandler for WaylandServer {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState { &mut self.xdg_shell_state }
    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        let (suggested_w, suggested_h) = surface.with_pending_state(|state| {
            state.states.set(smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State::Activated);
            (state.size.map(|s| s.w as u32), state.size.map(|s| s.h as u32))
        });
        surface.send_configure();
        let window = Window::new_wayland_window(surface.clone());
        self.space.map_element(window, (0, 0), true);
        let surface_id = surface.wl_surface().id().protocol_id();
        let app_id = with_states(surface.wl_surface(), |states| { states.data_map.get::<XdgToplevelSurfaceData>().and_then(|mutex| mutex.lock().unwrap().app_id.clone()) }).unwrap_or_else(|| "unknown".into());
        let _ = self.event_tx.send(WaylandEvent::NewWindow { surface_id, app_id, suggested_width: suggested_w, suggested_height: suggested_h });
    }
    fn new_popup(&mut self, _: PopupSurface, _: PositionerState) {}
    fn grab(&mut self, _: PopupSurface, _: smithay::reexports::wayland_server::protocol::wl_seat::WlSeat, _: smithay::utils::Serial) {}
    fn reposition_request(&mut self, _: PopupSurface, _: PositionerState, _: u32) {}
}

impl WlrLayerShellHandler for WaylandServer {
    fn shell_state(&mut self) -> &mut WlrLayerShellState { &mut self.layer_shell_state }
    fn new_layer_surface(&mut self, _: LayerSurface, _: Option<smithay::reexports::wayland_server::protocol::wl_output::WlOutput>, _: Layer, _: String) {}
}

impl ShmHandler for WaylandServer { fn shm_state(&self) -> &ShmState { &self.shm_state } }

impl SeatHandler for WaylandServer {
    type KeyboardFocus = WlSurface; type PointerFocus = WlSurface; type TouchFocus = WlSurface;
    fn seat_state(&mut self) -> &mut SeatState<Self> { &mut self.seat_state }
    fn cursor_image(&mut self, _: &Seat<Self>, _: CursorImageStatus) {}
    fn focus_changed(&mut self, _: &Seat<Self>, _: Option<&WlSurface>) {}
}

impl DataDeviceHandler for WaylandServer { fn data_device_state(&self) -> &DataDeviceState { &self.data_device_state } }
impl SelectionHandler for WaylandServer {
    type SelectionUserData = ();
    fn new_selection(&mut self, _: SelectionTarget, _: Option<smithay::wayland::selection::SelectionSource>, _: Seat<Self>) {}
    fn send_selection(&mut self, _: SelectionTarget, _: String, _: std::os::unix::io::OwnedFd, _: Seat<Self>, _: &Self::SelectionUserData) {}
}
impl ClientDndGrabHandler for WaylandServer {}
impl ServerDndGrabHandler for WaylandServer { fn send(&mut self, _: String, _: std::os::unix::io::OwnedFd, _: Seat<Self>) {} }
impl smithay::wayland::output::OutputHandler for WaylandServer {}

delegate_compositor!(WaylandServer);
delegate_shm!(WaylandServer);
delegate_seat!(WaylandServer);
delegate_xdg_shell!(WaylandServer);
delegate_layer_shell!(WaylandServer);
delegate_data_device!(WaylandServer);
delegate_output!(WaylandServer);
