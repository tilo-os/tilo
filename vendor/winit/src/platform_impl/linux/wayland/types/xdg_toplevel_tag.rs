//! `xdg_toplevel_tag_v1`: a tag a window carries for the compositor, which the session that made
//! the client's connection reads as the window's place.

use sctk::reexports::client::globals::{BindError, GlobalList};
use sctk::reexports::client::{delegate_dispatch, Connection, Dispatch, Proxy, QueueHandle};
use sctk::reexports::protocols::xdg::shell::client::xdg_toplevel::XdgToplevel;
use wayland_protocols::xdg::toplevel_tag::v1::client::xdg_toplevel_tag_manager_v1::XdgToplevelTagManagerV1;

use sctk::globals::GlobalData;

use crate::platform_impl::wayland::state::WinitState;

pub struct XdgToplevelTagState {
    manager: XdgToplevelTagManagerV1,
}

impl XdgToplevelTagState {
    pub fn bind(
        globals: &GlobalList,
        queue_handle: &QueueHandle<WinitState>,
    ) -> Result<Self, BindError> {
        let manager = globals.bind(queue_handle, 1..=1, GlobalData)?;
        Ok(Self { manager })
    }

    /// Tag `toplevel` with `tag`, before its first commit.
    pub fn tag(&self, toplevel: &XdgToplevel, tag: String) {
        self.manager.set_toplevel_tag(toplevel, tag);
    }
}

impl Dispatch<XdgToplevelTagManagerV1, GlobalData, WinitState> for XdgToplevelTagState {
    fn event(
        _state: &mut WinitState,
        _proxy: &XdgToplevelTagManagerV1,
        _event: <XdgToplevelTagManagerV1 as Proxy>::Event,
        _data: &GlobalData,
        _conn: &Connection,
        _qhandle: &QueueHandle<WinitState>,
    ) {
    }
}

delegate_dispatch!(WinitState: [XdgToplevelTagManagerV1: GlobalData] => XdgToplevelTagState);
