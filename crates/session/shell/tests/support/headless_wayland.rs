//! A headless Wayland server for the sandbox tests: real clients connect to
//! it over a socket, it relays their frames and windows as `WaylandEvent`s and
//! takes `WaylandInput` to drive the seat.

use smithay::{
    delegate_compositor, delegate_data_device, delegate_layer_shell, delegate_output,
    delegate_seat, delegate_shm, delegate_xdg_shell,
    desktop::{Space, Window},
    input::{
        Seat, SeatHandler, SeatState,
        keyboard::{FilterResult, XkbConfig},
        pointer::{CursorImageStatus, MotionEvent},
    },
    output::{Mode as OutputMode, Output, PhysicalProperties, Subpixel},
    reexports::wayland_server::{
        Client, DisplayHandle, Resource,
        backend::{ClientData, ClientId, DisconnectReason},
        protocol::{wl_shm, wl_surface::WlSurface},
    },
    utils::{Serial, Size as SmithaySize, Transform},
    wayland::{
        buffer::BufferHandler,
        compositor::{
            CompositorClientState, CompositorHandler, CompositorState, SurfaceAttributes,
            with_states,
        },
        seat::WaylandFocus,
        selection::{
            SelectionHandler, SelectionTarget,
            data_device::{
                ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler,
            },
        },
        shell::{
            wlr_layer::{Layer, LayerSurface, WlrLayerShellHandler, WlrLayerShellState},
            xdg::{
                PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
                XdgToplevelSurfaceData,
            },
        },
        shm::{ShmHandler, ShmState},
    },
};
use std::time::Instant;
use tokio::sync::mpsc;

#[derive(Debug, Clone)]
pub enum WaylandEvent {
    Frame {
        surface_id: u32,
        app_id: String,
    },
    NewWindow {
        surface_id: u32,
        app_id: String,
        suggested_width: Option<u32>,
        suggested_height: Option<u32>,
    },
}

#[derive(Debug)]
pub enum WaylandInput {
    Motion {
        x: f64,
        y: f64,
        surface_id: Option<u32>,
        surface_width: f32,
        surface_height: f32,
    },
    Button {
        button: u32,
        state: smithay::backend::input::ButtonState,
        surface_id: Option<u32>,
    },
    Key {
        key: u32,
        state: smithay::backend::input::KeyState,
    },
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
    pub _output: Output,
    pub start_time: Instant,
    pub serial_counter: u32,
    pub pointer_location: smithay::utils::Point<f64, smithay::utils::Logical>,
    pub event_tx: mpsc::UnboundedSender<WaylandEvent>,
}

pub struct ClientState {
    pub compositor_state: CompositorClientState,
}
impl ClientData for ClientState {
    fn initialized(&self, _: ClientId) {}
    fn disconnected(&self, _: ClientId, _: DisconnectReason) {}
}

impl WaylandServer {
    pub fn new(
        dh: &DisplayHandle,
        event_tx: mpsc::UnboundedSender<WaylandEvent>,
    ) -> Result<Self, String> {
        let compositor_state = CompositorState::new::<Self>(dh);
        let xdg_shell_state = XdgShellState::new::<Self>(dh);
        let layer_shell_state = WlrLayerShellState::new::<Self>(dh);
        let shm_state =
            ShmState::new::<Self>(dh, vec![wl_shm::Format::Argb8888, wl_shm::Format::Xrgb8888]);
        let mut seat_state = SeatState::new();
        let data_device_state = DataDeviceState::new::<Self>(dh);
        let mut seat = seat_state.new_wl_seat(dh, "tilo-desktop");
        seat.add_pointer();
        seat.add_keyboard(XkbConfig::default(), 200, 25)
            .map_err(|e| format!("failed to add keyboard: {e}"))?;
        let output = Output::new(
            "tilo-desktop-0".into(),
            PhysicalProperties {
                size: SmithaySize::from((0, 0)),
                subpixel: Subpixel::Unknown,
                make: "Tilo Desktop".into(),
                model: "Virtual".into(),
            },
        );
        let mode = OutputMode {
            size: SmithaySize::from((1920, 1080)),
            refresh: 60_000,
        };
        output.change_current_state(
            Some(mode),
            Some(Transform::Normal),
            None,
            Some((0, 0).into()),
        );
        output.set_preferred(mode);
        output.create_global::<Self>(dh);
        Ok(Self {
            compositor_state,
            xdg_shell_state,
            layer_shell_state,
            shm_state,
            seat_state,
            data_device_state,
            seat,
            space: Space::default(),
            _output: output,
            start_time: Instant::now(),
            serial_counter: 0,
            pointer_location: (0.0, 0.0).into(),
            event_tx,
        })
    }

    fn next_serial(&mut self) -> Serial {
        self.serial_counter = self.serial_counter.wrapping_add(1);
        Serial::from(self.serial_counter)
    }

    pub fn handle_input(&mut self, input: &WaylandInput) {
        // Wall-clock ms since server start, for the Wayland event timestamp field.
        // Masked to the low 32 bits (identical to the truncating `as u32` this
        // replaces) so the cast below is provably lossless; the field wraps every
        // ~49 days, which the protocol expects.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "masked to the low 32 bits immediately above, so it always fits u32"
        )]
        let time = (self.start_time.elapsed().as_millis() & u128::from(u32::MAX)) as u32;
        match *input {
            WaylandInput::Motion {
                x,
                y,
                surface_id,
                surface_width,
                surface_height,
            } => self.handle_motion_input(x, y, surface_id, surface_width, surface_height, time),
            WaylandInput::Button {
                button,
                state,
                surface_id,
            } => self.handle_button_input(button, state, surface_id, time),
            WaylandInput::Key { key, state } => self.handle_key_input(key, state, time),
        }
    }

    /// `WaylandInput::Motion` arm of [`Self::handle_input`], split out to keep
    /// that dispatcher short. Rescales the pointer position from surface-local
    /// logical coordinates into the client's actual buffer size when the
    /// motion lands on a known surface, else moves the bare pointer.
    fn handle_motion_input(
        &mut self,
        x: f64,
        y: f64,
        surface_id: Option<u32>,
        surface_width: f32,
        surface_height: f32,
        time: u32,
    ) {
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let serial = self.next_serial();
        let surface = surface_id.and_then(|id| self.find_surface_by_id(id));
        if let Some(s) = surface {
            let mut actual_buf_size = (f64::from(surface_width), f64::from(surface_height));
            with_states(&s, |states| {
                let mut attrs = states.cached_state.get::<SurfaceAttributes>();
                if let Some(smithay::wayland::compositor::BufferAssignment::NewBuffer(buffer)) =
                    &attrs.current().buffer
                {
                    let _ = smithay::wayland::shm::with_buffer_contents(buffer, |_, _, data| {
                        actual_buf_size = (f64::from(data.width), f64::from(data.height));
                    });
                }
            });
            let scale_x = actual_buf_size.0 / f64::from(surface_width);
            let scale_y = actual_buf_size.1 / f64::from(surface_height);
            self.pointer_location = (x * scale_x, y * scale_y).into();
            pointer.motion(
                self,
                Some((s, (0.0, 0.0).into())),
                &MotionEvent {
                    location: self.pointer_location,
                    serial,
                    time,
                },
            );
        } else {
            self.pointer_location = (x, y).into();
            pointer.motion(
                self,
                None,
                &MotionEvent {
                    location: self.pointer_location,
                    serial,
                    time,
                },
            );
        }
        pointer.frame(self);
    }

    /// `WaylandInput::Button` arm of [`Self::handle_input`], split out to
    /// keep that dispatcher short. A press on a known surface also focuses
    /// and activates it, mirroring what a real seat would do on click.
    fn handle_button_input(
        &mut self,
        button: u32,
        state: smithay::backend::input::ButtonState,
        surface_id: Option<u32>,
        time: u32,
    ) {
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let serial = self.next_serial();
        if let Some(id) = surface_id
            && state == smithay::backend::input::ButtonState::Pressed
            && let Some(surface) = self.find_surface_by_id(id)
        {
            if let Some(keyboard) = self.seat.get_keyboard() {
                keyboard.set_focus(self, Some(surface.clone()), serial);
            }
            self.activate_window_by_surface(&surface);
        }
        pointer.button(
            self,
            &smithay::input::pointer::ButtonEvent {
                button,
                state,
                serial,
                time,
            },
        );
        pointer.frame(self);
    }

    /// `WaylandInput::Key` arm of [`Self::handle_input`], split out to keep
    /// that dispatcher short.
    fn handle_key_input(&mut self, key: u32, state: smithay::backend::input::KeyState, time: u32) {
        let serial = self.next_serial();
        let Some(keyboard) = self.seat.get_keyboard() else {
            return;
        };
        keyboard.input(self, key.into(), state, serial, time, |_, _, _| {
            FilterResult::Forward::<()>
        });
    }

    fn find_surface_by_id(&self, target_id: u32) -> Option<WlSurface> {
        for window in self.space.elements() {
            if let Some(surface) = window.wl_surface()
                && surface.id().protocol_id() == target_id
            {
                return Some(surface.clone().into_owned());
            }
        }
        None
    }

    fn activate_window_by_surface(&self, surface: &WlSurface) {
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
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }
    #[allow(
        clippy::panic,
        reason = "the trait requires a direct reference, so there is no Option/Result to \
                  hand back to a caller; every accepted client is initialised with a \
                  ClientState in WaylandServer::new's insertion path, so this can't miss \
                  in practice — the panic documents that invariant rather than hiding a bug"
    )]
    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client
            .get_data::<ClientState>()
            .unwrap_or_else(|| panic!("client missing ClientState user-data"))
            .compositor_state
    }
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the `buffer` reference below borrows from the `attrs` cached-state guard \
                  and is used for the rest of the closure, including the network send inside \
                  with_buffer_contents; dropping `attrs` earlier would require cloning the \
                  buffer handle out first"
    )]
    fn commit(&mut self, surface: &WlSurface) {
        if let Some(window) = self
            .space
            .elements()
            .find(|w| w.wl_surface().as_deref() == Some(surface))
        {
            window.on_commit();
        }
        let tx = self.event_tx.clone();
        let surface_id = surface.id().protocol_id();

        let app_id = with_states(surface, |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .and_then(|m| m.lock().ok()?.app_id.clone())
        })
        .unwrap_or_else(|| "unknown".into());

        let () = with_states(surface, |states| {
            let mut attrs = states.cached_state.get::<SurfaceAttributes>();
            let buffer = attrs.current().buffer.as_ref();
            if let Some(smithay::wayland::compositor::BufferAssignment::NewBuffer(_)) = buffer {
                let _ = tx.send(WaylandEvent::Frame { surface_id, app_id });
            }
        });
    }
}

impl BufferHandler for WaylandServer {
    fn buffer_destroyed(
        &mut self,
        _: &smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer,
    ) {
    }
}

impl XdgShellHandler for WaylandServer {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }
    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        let (suggested_w, suggested_h) = surface.with_pending_state(|state| {
            state.states.set(smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State::Activated);
            (
                state.size.map(|s| u32::try_from(s.w).unwrap_or(0)),
                state.size.map(|s| u32::try_from(s.h).unwrap_or(0)),
            )
        });
        surface.send_configure();
        let window = Window::new_wayland_window(surface.clone());
        self.space.map_element(window, (0, 0), true);
        let surface_id = surface.wl_surface().id().protocol_id();
        let app_id = with_states(surface.wl_surface(), |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .and_then(|mutex| mutex.lock().ok()?.app_id.clone())
        })
        .unwrap_or_else(|| "unknown".into());
        let _ = self.event_tx.send(WaylandEvent::NewWindow {
            surface_id,
            app_id,
            suggested_width: suggested_w,
            suggested_height: suggested_h,
        });
    }
    fn new_popup(&mut self, _: PopupSurface, _: PositionerState) {}
    fn grab(
        &mut self,
        _: PopupSurface,
        _: smithay::reexports::wayland_server::protocol::wl_seat::WlSeat,
        _: smithay::utils::Serial,
    ) {
    }
    fn reposition_request(&mut self, _: PopupSurface, _: PositionerState, _: u32) {}
}

impl WlrLayerShellHandler for WaylandServer {
    fn shell_state(&mut self) -> &mut WlrLayerShellState {
        &mut self.layer_shell_state
    }
    fn new_layer_surface(
        &mut self,
        _: LayerSurface,
        _: Option<smithay::reexports::wayland_server::protocol::wl_output::WlOutput>,
        _: Layer,
        _: String,
    ) {
    }
}

impl ShmHandler for WaylandServer {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

impl SeatHandler for WaylandServer {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;
    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }
    fn cursor_image(&mut self, _: &Seat<Self>, _: CursorImageStatus) {}
    fn focus_changed(&mut self, _: &Seat<Self>, _: Option<&WlSurface>) {}
}

impl DataDeviceHandler for WaylandServer {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device_state
    }
}
impl SelectionHandler for WaylandServer {
    type SelectionUserData = ();
    fn new_selection(
        &mut self,
        _: SelectionTarget,
        _: Option<smithay::wayland::selection::SelectionSource>,
        _: Seat<Self>,
    ) {
    }
    fn send_selection(
        &mut self,
        _: SelectionTarget,
        _: String,
        _: std::os::unix::io::OwnedFd,
        _: Seat<Self>,
        (): &Self::SelectionUserData,
    ) {
    }
}
impl ClientDndGrabHandler for WaylandServer {}
impl ServerDndGrabHandler for WaylandServer {
    fn send(&mut self, _: String, _: std::os::unix::io::OwnedFd, _: Seat<Self>) {}
}
impl smithay::wayland::output::OutputHandler for WaylandServer {}

delegate_compositor!(WaylandServer);
delegate_shm!(WaylandServer);
delegate_seat!(WaylandServer);
delegate_xdg_shell!(WaylandServer);
delegate_layer_shell!(WaylandServer);
delegate_data_device!(WaylandServer);
delegate_output!(WaylandServer);
