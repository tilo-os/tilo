//! The desktop's side: one ordinary Wayland client on the socket the session handed over, with
//! one object on it for every held object of the app.

use std::cell::RefCell;
use std::collections::HashMap;
use std::os::unix::net::UnixStream;

use smithay::reexports::wayland_server::Resource as _;
use smithay::reexports::wayland_server::backend::{ClientId, ObjectId};
use smithay::wayland::shell::xdg::PositionerState;
use wayland_client::protocol as c;
use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle, WEnum, event_created_child,
};
use wayland_protocols::wp::linux_dmabuf::zv1::client::{
    zwp_linux_buffer_params_v1, zwp_linux_dmabuf_feedback_v1, zwp_linux_dmabuf_v1,
};
use wayland_protocols::xdg::shell::client as cx;
use wayland_protocols::xdg::xdg_output::zv1::client::zxdg_output_manager_v1;

use crate::dmabuf::FeedbackParts;
use crate::outputs::UpOutput;
use crate::seat::SEAT_VERSION;
use crate::state::State;
use crate::surfaces::{BufferRef, Rect, Role, UpRole, UpSurface, positioner_rules};

/// The user data of a desktop object that mirrors one of the app's surfaces (its surface and
/// role objects): the app surface's id.
pub(crate) struct SurfaceRef(pub ObjectId);

/// A frame callback on the desktop, done when the app's is.
pub(crate) struct FrameRef {
    pub surface: ObjectId,
    pub callback: smithay::reexports::wayland_server::protocol::wl_callback::WlCallback,
}

/// The two round trips of an attach: the globals are bound, then their first state arrived.
#[derive(Clone, Copy, Debug)]
pub(crate) enum SyncStage {
    Globals,
    Ready,
}

/// One desktop connection and its globals.
pub(crate) struct Upstream {
    pub conn: Connection,
    pub qh: QueueHandle<State>,
    pub generation: u64,
    pub compositor: Option<c::wl_compositor::WlCompositor>,
    pub subcompositor: Option<c::wl_subcompositor::WlSubcompositor>,
    pub shm: Option<c::wl_shm::WlShm>,
    pub shm_formats: Vec<u32>,
    pub wm_base: Option<cx::xdg_wm_base::XdgWmBase>,
    pub dmabuf: Option<zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1>,
    pub feedback: FeedbackParts,
    pub xdg_outputs: Option<zxdg_output_manager_v1::ZxdgOutputManagerV1>,
    pub seat: Option<c::wl_seat::WlSeat>,
    pub keyboard: Option<c::wl_keyboard::WlKeyboard>,
    pub pointer: Option<c::wl_pointer::WlPointer>,
    pub touch: Option<c::wl_touch::WlTouch>,
    pub outputs: Vec<UpOutput>,
    /// Bound: surfaces can be made.
    pub globals_bound: bool,
    /// The globals' first state arrived (outputs, seat, keymap).
    pub ready: bool,
    /// Which app connection each desktop object was made for, by protocol id: a protocol error
    /// the desktop reports names the object, and so the app whose state caused it.
    owners: RefCell<HashMap<u32, ClientId>>,
    /// The app connection the keeper last sent requests for: a protocol error the desktop
    /// posts on a global (`xdg_wm_base`) names no object of the app's, and is the last
    /// sender's.
    last_sender: RefCell<Option<ClientId>>,
}

impl Upstream {
    pub(crate) fn connect(
        socket: UnixStream,
        generation: u64,
    ) -> std::io::Result<(Self, EventQueue<State>)> {
        let conn = Connection::from_socket(socket).map_err(std::io::Error::other)?;
        let queue = conn.new_event_queue();
        let qh = queue.handle();
        let display = conn.display();
        display.get_registry(&qh, ());
        display.sync(&qh, SyncStage::Globals);
        Ok((
            Self {
                conn,
                qh,
                generation,
                compositor: None,
                subcompositor: None,
                shm: None,
                shm_formats: Vec::new(),
                wm_base: None,
                dmabuf: None,
                feedback: FeedbackParts::default(),
                xdg_outputs: None,
                seat: None,
                keyboard: None,
                pointer: None,
                touch: None,
                outputs: Vec::new(),
                globals_bound: false,
                ready: false,
                owners: RefCell::new(HashMap::new()),
                last_sender: RefCell::new(None),
            },
            queue,
        ))
    }

    /// Record that `proxy` was made for the app connection `owner`.
    pub(crate) fn own(&self, proxy: &impl Proxy, owner: Option<&ClientId>) {
        if let Some(owner) = owner {
            self.owners
                .borrow_mut()
                .insert(proxy.id().protocol_id(), owner.clone());
        }
    }

    pub(crate) fn resource_owner(&self, proxy: &impl Proxy, owner: &ClientId) {
        self.own(proxy, Some(owner));
    }

    /// The app connection whose state a protocol error on `protocol_id` came from.
    pub(crate) fn owner_of(&self, protocol_id: u32) -> Option<ClientId> {
        self.owners
            .borrow()
            .get(&protocol_id)
            .cloned()
            .or_else(|| self.last_sender.borrow().clone())
    }

    /// The requests that follow are for the app connection of `surface`.
    pub(crate) fn sending_for(
        &self,
        surface: &smithay::reexports::wayland_server::protocol::wl_surface::WlSurface,
    ) {
        *self.last_sender.borrow_mut() = surface.client().map(|client| client.id());
    }

    /// A region of these rectangles, or none for "everywhere".
    pub(crate) fn region(&self, rects: Option<&[(bool, Rect)]>) -> Option<c::wl_region::WlRegion> {
        let rects = rects?;
        let region = self.compositor.as_ref()?.create_region(&self.qh, ());
        for (add, (x, y, w, h)) in rects {
            if *add {
                region.add(*x, *y, *w, *h);
            } else {
                region.subtract(*x, *y, *w, *h);
            }
        }
        Some(region)
    }

    /// A positioner with the rules of an app's, once `xdg_wm_base` is bound.
    pub(crate) fn positioner(
        &self,
        rules: &PositionerState,
    ) -> Option<cx::xdg_positioner::XdgPositioner> {
        let positioner = self.wm_base.as_ref()?.create_positioner(&self.qh, ());
        positioner_rules(rules, &positioner);
        Some(positioner)
    }
}

impl Dispatch<c::wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &c::wl_registry::WlRegistry,
        event: c::wl_registry::Event,
        (): &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let Some(up) = state.up.as_mut() else {
            return;
        };
        match event {
            c::wl_registry::Event::Global {
                name,
                interface,
                version,
            } => match interface.as_str() {
                "wl_compositor" => {
                    up.compositor = Some(registry.bind(name, version.min(6), qh, ()));
                }
                "wl_subcompositor" => up.subcompositor = Some(registry.bind(name, 1, qh, ())),
                "wl_shm" => up.shm = Some(registry.bind(name, 1, qh, ())),
                "xdg_wm_base" => up.wm_base = Some(registry.bind(name, version.min(6), qh, ())),
                "zwp_linux_dmabuf_v1" if version >= 4 => {
                    up.dmabuf = Some(registry.bind(name, version.min(5), qh, ()));
                }
                "zxdg_output_manager_v1" => {
                    let manager: zxdg_output_manager_v1::ZxdgOutputManagerV1 =
                        registry.bind(name, version.min(3), qh, ());
                    for output in &mut up.outputs {
                        output.xdg = Some(manager.get_xdg_output(&output.wl, qh, output.global));
                    }
                    up.xdg_outputs = Some(manager);
                }
                "wl_seat" if up.seat.is_none() => {
                    up.seat = Some(registry.bind(name, version.min(SEAT_VERSION), qh, ()));
                }
                "wl_output" => {
                    let wl: c::wl_output::WlOutput = registry.bind(name, version.min(4), qh, name);
                    let xdg = up
                        .xdg_outputs
                        .as_ref()
                        .map(|manager| manager.get_xdg_output(&wl, qh, name));
                    up.outputs.push(UpOutput::new(name, wl, xdg));
                }
                _ => {}
            },
            c::wl_registry::Event::GlobalRemove { name } => state.desktop_output_removed(name),
            _ => {}
        }
    }
}

impl Dispatch<c::wl_callback::WlCallback, SyncStage> for State {
    fn event(
        state: &mut Self,
        _: &c::wl_callback::WlCallback,
        event: c::wl_callback::Event,
        step: &SyncStage,
        conn: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if !matches!(event, c::wl_callback::Event::Done { .. }) {
            return;
        }
        match step {
            SyncStage::Globals => {
                let Some(up) = state.up.as_mut() else {
                    return;
                };
                up.globals_bound = true;
                if let Some(dmabuf) = &up.dmabuf {
                    dmabuf.get_default_feedback(qh, ());
                }
                conn.display().sync(qh, SyncStage::Ready);
                state.push_all();
            }
            SyncStage::Ready => state.desktop_ready(),
        }
    }
}

impl Dispatch<c::wl_callback::WlCallback, FrameRef> for State {
    fn event(
        state: &mut Self,
        _: &c::wl_callback::WlCallback,
        event: c::wl_callback::Event,
        frame: &FrameRef,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let c::wl_callback::Event::Done { callback_data } = event else {
            return;
        };
        if let Some(kept) = state.surfaces.get_mut(&frame.surface) {
            kept.frames
                .retain(|(callback, _)| callback.id() != frame.callback.id());
        }
        if frame.callback.is_alive() {
            frame.callback.done(callback_data);
        }
    }
}

impl Dispatch<c::wl_surface::WlSurface, SurfaceRef> for State {
    fn event(
        state: &mut Self,
        _: &c::wl_surface::WlSurface,
        event: c::wl_surface::Event,
        surface: &SurfaceRef,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(kept) = state.surfaces.get(&surface.0) else {
            return;
        };
        let down = kept.surface.clone();
        match event {
            c::wl_surface::Event::Enter { output } => {
                state.surface_on_output(&down, output.data::<u32>().copied(), true);
            }
            c::wl_surface::Event::Leave { output } => {
                state.surface_on_output(&down, output.data::<u32>().copied(), false);
            }
            c::wl_surface::Event::PreferredBufferScale { factor } => {
                if down.version() >= 6 {
                    down.preferred_buffer_scale(factor);
                }
            }
            c::wl_surface::Event::PreferredBufferTransform {
                transform: WEnum::Value(transform),
            } => {
                if down.version() >= 6
                    && let Ok(transform) =
                        smithay::reexports::wayland_server::protocol::wl_output::Transform::try_from(
                            u32::from(transform),
                        )
                {
                    down.preferred_buffer_transform(transform);
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<cx::xdg_surface::XdgSurface, SurfaceRef> for State {
    fn event(
        state: &mut Self,
        _: &cx::xdg_surface::XdgSurface,
        event: cx::xdg_surface::Event,
        surface: &SurfaceRef,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let cx::xdg_surface::Event::Configure { serial } = event {
            state.desktop_configured(&surface.0, serial);
        }
    }
}

impl Dispatch<cx::xdg_toplevel::XdgToplevel, SurfaceRef> for State {
    fn event(
        state: &mut Self,
        _: &cx::xdg_toplevel::XdgToplevel,
        event: cx::xdg_toplevel::Event,
        surface: &SurfaceRef,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(kept) = state.surfaces.get_mut(&surface.0) else {
            return;
        };
        let Role::Toplevel(top) = &kept.role else {
            return;
        };
        let Some(UpSurface {
            role: UpRole::Xdg(xdg),
            ..
        }) = kept.up.as_mut()
        else {
            return;
        };
        match event {
            cx::xdg_toplevel::Event::Configure {
                width,
                height,
                states,
            } => {
                let states = states
                    .chunks_exact(4)
                    .filter_map(|chunk| chunk.try_into().ok().map(u32::from_ne_bytes))
                    .collect();
                xdg.next_top = Some((width, height, states));
            }
            cx::xdg_toplevel::Event::Close => top.toplevel.send_close(),
            cx::xdg_toplevel::Event::ConfigureBounds { width, height } => {
                xdg.next_bounds = Some((width, height));
            }
            cx::xdg_toplevel::Event::WmCapabilities { capabilities } => {
                let mut set = smithay::wayland::shell::xdg::WmCapabilitySet::default();
                for raw in capabilities
                    .chunks_exact(4)
                    .filter_map(|chunk| chunk.try_into().ok().map(u32::from_ne_bytes))
                {
                    if let Ok(capability) =
                        smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::WmCapabilities::try_from(raw)
                    {
                        set.set(capability);
                    }
                }
                top.toplevel
                    .with_pending_state(|pending| pending.capabilities = set);
            }
            _ => {}
        }
    }
}

impl Dispatch<cx::xdg_popup::XdgPopup, SurfaceRef> for State {
    fn event(
        state: &mut Self,
        _: &cx::xdg_popup::XdgPopup,
        event: cx::xdg_popup::Event,
        surface: &SurfaceRef,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            // The desktop closed it (a click outside): the app is told, and it never comes back.
            cx::xdg_popup::Event::PopupDone => state.dismiss_popup(&surface.0),
            cx::xdg_popup::Event::Configure {
                x,
                y,
                width,
                height,
            } => {
                if let Some(xdg) = state.up_xdg(&surface.0) {
                    xdg.next_popup = Some((x, y, width, height));
                }
            }
            cx::xdg_popup::Event::Repositioned { token } => {
                if let Some(xdg) = state.up_xdg(&surface.0) {
                    xdg.repositioned = Some(token);
                }
            }
            _ => {}
        }
    }
}

impl State {
    fn up_xdg(&mut self, id: &ObjectId) -> Option<&mut crate::surfaces::UpXdg> {
        match self.surfaces.get_mut(id)?.up.as_mut()? {
            UpSurface {
                role: UpRole::Xdg(xdg),
                ..
            } => Some(xdg),
            _ => None,
        }
    }
}

impl Dispatch<cx::xdg_wm_base::XdgWmBase, ()> for State {
    fn event(
        state: &mut Self,
        wm_base: &cx::xdg_wm_base::XdgWmBase,
        event: cx::xdg_wm_base::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // Answered for the app, never by the keeper alone: an app that hangs must look hung.
        if let cx::xdg_wm_base::Event::Ping { serial } = event {
            state.desktop_ping(wm_base, serial);
        }
    }
}

impl Dispatch<c::wl_buffer::WlBuffer, BufferRef> for State {
    fn event(
        state: &mut Self,
        _: &c::wl_buffer::WlBuffer,
        event: c::wl_buffer::Event,
        buffer: &BufferRef,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if matches!(event, c::wl_buffer::Event::Release) {
            state.buffers.up_released(buffer.0);
        }
    }
}

impl Dispatch<c::wl_shm::WlShm, ()> for State {
    fn event(
        state: &mut Self,
        _: &c::wl_shm::WlShm,
        event: c::wl_shm::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let c::wl_shm::Event::Format {
            format: WEnum::Value(format),
        } = event
            && let Some(up) = state.up.as_mut()
        {
            up.shm_formats.push(format.into());
        }
    }
}

impl Dispatch<zwp_linux_buffer_params_v1::ZwpLinuxBufferParamsV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &zwp_linux_buffer_params_v1::ZwpLinuxBufferParamsV1,
        event: zwp_linux_buffer_params_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if matches!(event, zwp_linux_buffer_params_v1::Event::Failed) {
            tracing::warn!("the desktop could not import an app's dma-buf");
        }
    }

    event_created_child!(State, zwp_linux_buffer_params_v1::ZwpLinuxBufferParamsV1, [
        zwp_linux_buffer_params_v1::EVT_CREATED_OPCODE => (c::wl_buffer::WlBuffer, BufferRef(0)),
    ]);
}

impl Dispatch<zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &zwp_linux_dmabuf_v1::ZwpLinuxDmabufV1,
        _: zwp_linux_dmabuf_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // Formats come with the feedback (version 4 and later), which is all the keeper binds.
    }
}

impl Dispatch<zwp_linux_dmabuf_feedback_v1::ZwpLinuxDmabufFeedbackV1, SurfaceRef> for State {
    fn event(
        state: &mut Self,
        _: &zwp_linux_dmabuf_feedback_v1::ZwpLinuxDmabufFeedbackV1,
        event: zwp_linux_dmabuf_feedback_v1::Event,
        surface: &SurfaceRef,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        state.desktop_surface_feedback(&surface.0, event);
    }
}

impl Dispatch<zwp_linux_dmabuf_feedback_v1::ZwpLinuxDmabufFeedbackV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &zwp_linux_dmabuf_feedback_v1::ZwpLinuxDmabufFeedbackV1,
        event: zwp_linux_dmabuf_feedback_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        state.desktop_feedback(event);
    }
}

wayland_client::delegate_noop!(State: ignore c::wl_compositor::WlCompositor);
wayland_client::delegate_noop!(State: ignore c::wl_subcompositor::WlSubcompositor);
wayland_client::delegate_noop!(State: ignore c::wl_shm_pool::WlShmPool);
wayland_client::delegate_noop!(State: ignore c::wl_region::WlRegion);
wayland_client::delegate_noop!(State: ignore cx::xdg_positioner::XdgPositioner);
wayland_client::delegate_noop!(State: ignore zxdg_output_manager_v1::ZxdgOutputManagerV1);

impl Dispatch<c::wl_subsurface::WlSubsurface, SurfaceRef> for State {
    fn event(
        _: &mut Self,
        _: &c::wl_subsurface::WlSubsurface,
        _: c::wl_subsurface::Event,
        _: &SurfaceRef,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl State {
    /// The desktop's first state arrived: the app's shared-memory formats and outputs follow
    /// it, and the app may start.
    pub(crate) fn desktop_ready(&mut self) {
        let Some(up) = self.up.as_mut() else {
            return;
        };
        up.ready = true;
        let formats: Vec<smithay::reexports::wayland_server::protocol::wl_shm::Format> = up
            .shm_formats
            .iter()
            .filter_map(|format| {
                smithay::reexports::wayland_server::protocol::wl_shm::Format::try_from(*format).ok()
            })
            .collect();
        self.shm_state_mut().update_formats(formats);
        self.drop_unseen_outputs();
        self.ready_once = true;
        self.windows_changed = true;
    }
}
