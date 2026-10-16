//! Horizontal scrolling a window has no use for, handed back to the
//! compositor so it can move its window strip with it.
use sctk::globals::GlobalData;
use sctk::reexports::client::globals::{BindError, GlobalList};
use sctk::reexports::client::protocol::wl_surface::WlSurface;
use sctk::reexports::client::{delegate_dispatch, Connection, Dispatch, Proxy, QueueHandle};

use crate::platform_impl::wayland::state::WinitState;

pub mod protocol {
    use wayland_client;
    use wayland_client::protocol::*;
    pub mod __interfaces {
        use wayland_client::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!(
            "src/platform_impl/linux/wayland/types/tilo_scroll.xml"
        );
    }
    use self::__interfaces::*;
    wayland_scanner::generate_client_code!("src/platform_impl/linux/wayland/types/tilo_scroll.xml");
}
use protocol::tilo_scroll_manager_v1::TiloScrollManagerV1;
use protocol::tilo_scroll_surface_v1::TiloScrollSurfaceV1;

pub struct ScrollManager(TiloScrollManagerV1);
impl ScrollManager {
    pub fn bind(globals: &GlobalList, queue: &QueueHandle<WinitState>) -> Result<Self, BindError> {
        globals.bind(queue, 1..=1, GlobalData).map(Self)
    }
    pub fn surface(&self, surface: &WlSurface, queue: &QueueHandle<WinitState>) -> ScrollSurface {
        ScrollSurface(self.0.get_scroll_surface(surface, queue, GlobalData))
    }
}

pub struct ScrollSurface(TiloScrollSurfaceV1);
impl ScrollSurface {
    /// The sideways scroll in progress moved nothing in this window.
    pub fn horizontal_unused(&self) {
        self.0.horizontal_unused();
    }
}
impl Drop for ScrollSurface {
    fn drop(&mut self) {
        self.0.destroy();
    }
}

impl Dispatch<TiloScrollManagerV1, GlobalData, WinitState> for ScrollManager {
    fn event(
        _: &mut WinitState,
        _: &TiloScrollManagerV1,
        _: <TiloScrollManagerV1 as Proxy>::Event,
        _: &GlobalData,
        _: &Connection,
        _: &QueueHandle<WinitState>,
    ) {
    }
}
impl Dispatch<TiloScrollSurfaceV1, GlobalData, WinitState> for ScrollManager {
    fn event(
        _: &mut WinitState,
        _: &TiloScrollSurfaceV1,
        _: <TiloScrollSurfaceV1 as Proxy>::Event,
        _: &GlobalData,
        _: &Connection,
        _: &QueueHandle<WinitState>,
    ) {
    }
}
delegate_dispatch!(WinitState: [TiloScrollManagerV1: GlobalData] => ScrollManager);
delegate_dispatch!(WinitState: [TiloScrollSurfaceV1: GlobalData] => ScrollManager);
