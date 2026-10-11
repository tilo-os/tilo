//! Surface-bound, content-free acknowledgement beside standard text-input.
use sctk::globals::GlobalData;
use sctk::reexports::client::globals::{BindError, GlobalList};
use sctk::reexports::client::protocol::wl_surface::WlSurface;
use sctk::reexports::client::{delegate_dispatch, Connection, Dispatch, Proxy, QueueHandle};

use crate::event::{PrivateIme, PrivateImeKind, WindowEvent};
use crate::platform_impl::wayland::state::WinitState;

pub mod protocol {
    use wayland_client;
    use wayland_client::protocol::*;
    pub mod __interfaces {
        use wayland_client::protocol::__interfaces::*;
        wayland_scanner::generate_interfaces!(
            "src/platform_impl/linux/wayland/types/tilo_text_input.xml"
        );
    }
    use self::__interfaces::*;
    wayland_scanner::generate_client_code!(
        "src/platform_impl/linux/wayland/types/tilo_text_input.xml"
    );
}
use protocol::tilo_text_input_manager_v1::TiloTextInputManagerV1;
use protocol::tilo_text_input_v1::TiloTextInputV1;

pub struct PrivateTextInputManager(TiloTextInputManagerV1);
impl PrivateTextInputManager {
    pub fn bind(globals: &GlobalList, queue: &QueueHandle<WinitState>) -> Result<Self, BindError> {
        globals.bind(queue, 1..=1, GlobalData).map(Self)
    }
    pub fn field(&self, surface: &WlSurface, queue: &QueueHandle<WinitState>) -> PrivateTextField {
        PrivateTextField(self.0.get_field(surface, queue, surface.clone()))
    }
}

pub struct PrivateTextField(TiloTextInputV1);
impl PrivateTextField {
    pub fn update(&self, snapshot: Option<[u8; 32]>) {
        self.0
            .update(snapshot.map_or_else(Vec::new, |stamp| stamp.to_vec()));
    }
    pub fn acknowledge(&self, serial: u32, outcome: u32) {
        if serial != 0 && outcome <= 2 {
            let outcome = match outcome {
                0 => protocol::tilo_text_input_v1::Outcome::Accepted,
                1 => protocol::tilo_text_input_v1::Outcome::Rejected,
                _ => protocol::tilo_text_input_v1::Outcome::Unknown,
            };
            self.0.acknowledge(serial, outcome);
        }
    }
}
impl Drop for PrivateTextField {
    fn drop(&mut self) {
        self.0.destroy();
    }
}

impl Dispatch<TiloTextInputManagerV1, GlobalData, WinitState> for PrivateTextInputManager {
    fn event(
        _: &mut WinitState,
        _: &TiloTextInputManagerV1,
        _: <TiloTextInputManagerV1 as Proxy>::Event,
        _: &GlobalData,
        _: &Connection,
        _: &QueueHandle<WinitState>,
    ) {
    }
}
impl Dispatch<TiloTextInputV1, WlSurface, WinitState> for PrivateTextInputManager {
    fn event(
        state: &mut WinitState,
        _: &TiloTextInputV1,
        event: <TiloTextInputV1 as Proxy>::Event,
        surface: &WlSurface,
        _: &Connection,
        _: &QueueHandle<WinitState>,
    ) {
        let protocol::tilo_text_input_v1::Event::Operation {
            serial,
            kind,
            snapshot,
        } = event;
        let Some(kind) = PrivateImeKind::from_protocol(u32::from(kind)) else {
            return;
        };
        let Ok(snapshot) = <[u8; 32]>::try_from(snapshot) else {
            return;
        };
        if serial == 0 {
            return;
        }
        state.events_sink.push_window_event(
            WindowEvent::PrivateIme(PrivateIme {
                serial,
                kind,
                snapshot,
            }),
            crate::platform_impl::wayland::make_wid(surface),
        );
    }
}
delegate_dispatch!(WinitState: [TiloTextInputManagerV1: GlobalData] => PrivateTextInputManager);
delegate_dispatch!(WinitState: [TiloTextInputV1: WlSurface] => PrivateTextInputManager);
