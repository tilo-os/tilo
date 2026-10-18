//! Window handles an application hands to a portal, through `xdg_foreign`
//! (`zxdg_exporter_v2`).
//!
//! A portal request names the window it comes from as `parent_window`:
//! `wayland:<handle>`, where the handle is what this exporter gave the app
//! for one of its toplevels. The shell turns it back into that window
//! ([`surface_for`]) so a dialog the app asks for is a sheet on that exact
//! window, even when the app has several.
//!
//! Only the exporter is served. The importer half of the protocol lets one
//! client parent its surfaces to another client's window; every dialog that
//! needs a parent here is drawn by the compositor itself, so no client gets
//! that reach. A handle names a window and nothing more: the shell accepts
//! it only for a window of the app that asks
//! ([`crate::shell::app_tabs::asking_tab`]).
//!
//! X11 software names its parent as `x11:<window id>`. X11 windows reach
//! Tilo through `xwayland-satellite`, which maps each to an ordinary
//! toplevel and keeps the X window ids to itself, so an X11 handle parses
//! ([`WindowHandle::parse`]) but names no window this compositor can see.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use smithay::reexports::wayland_protocols::xdg::foreign::zv2::server::{
    zxdg_exported_v2::{self, ZxdgExportedV2},
    zxdg_exporter_v2::{self, ZxdgExporterV2},
};
use smithay::reexports::wayland_server::backend::ClientId;
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};
use tilo_lock::MutexRecover as _;

use crate::compositor::state::State;

const EXPORTER_VERSION: u32 = 1;

/// A portal's `parent_window` argument, as the app wrote it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WindowHandle {
    /// `wayland:<handle>`: a handle this exporter minted.
    Wayland(String),
    /// `x11:<hex window id>`: an X11 window, unknown to this compositor.
    X11(u32),
}

impl WindowHandle {
    /// Read a `parent_window` string; empty, unknown or malformed is none.
    pub fn parse(parent_window: &str) -> Option<Self> {
        let (kind, value) = parent_window.trim().split_once(':')?;
        match kind {
            "wayland" if is_minted_shape(value) => Some(Self::Wayland(value.to_string())),
            "x11" => {
                let digits = value.strip_prefix("0x").unwrap_or(value);
                u32::from_str_radix(digits, 16).ok().map(Self::X11)
            }
            _ => None,
        }
    }

    /// The window this handle names: its [`crate::compositor::state::surface_id`].
    pub fn surface(&self) -> Option<u64> {
        match self {
            Self::Wayland(handle) => surface_for(handle),
            Self::X11(_) => None,
        }
    }
}

/// A handle as minted: 32 alphanumeric characters.
fn is_minted_shape(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

/// Every live export: handle to the window it names. An entry lives exactly
/// as long as the client's `zxdg_exported_v2` object (and the window), so
/// what a client can put here is bounded by the objects it holds, as with
/// any other protocol object.
fn registry() -> &'static Mutex<HashMap<String, u64>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();
    REGISTRY.get_or_init(Default::default)
}

/// The window `handle` was exported for, while the export lives.
pub fn surface_for(handle: &str) -> Option<u64> {
    registry().lock_recover().get(handle).copied()
}

/// A window is gone: every handle naming it is void.
pub fn forget(surface_id: u64) {
    registry()
        .lock_recover()
        .retain(|_, surface| *surface != surface_id);
}

/// Mint a fresh handle for `surface`.
fn mint(surface: u64) -> String {
    let handle = uuid::Uuid::new_v4().simple().to_string();
    registry().lock_recover().insert(handle.clone(), surface);
    handle
}

/// A handle for `surface`, as if its client had exported it.
#[cfg(test)]
pub(crate) fn export_for_tests(surface: u64) -> String {
    mint(surface)
}

fn revoke(handle: &str) {
    registry().lock_recover().remove(handle);
}

/// Advertise the `zxdg_exporter_v2` global.
pub fn exporter(display: &DisplayHandle) {
    display.create_global::<State, ZxdgExporterV2, ()>(EXPORTER_VERSION, ());
}

/// User data of a `zxdg_exported_v2`: the handle it holds, if one was minted.
#[derive(Debug)]
pub struct Exported(Option<String>);

impl GlobalDispatch<ZxdgExporterV2, ()> for State {
    fn bind(
        _state: &mut Self,
        _dh: &DisplayHandle,
        _client: &Client,
        resource: New<ZxdgExporterV2>,
        _global_data: &(),
        data_init: &mut DataInit<'_, Self>,
    ) {
        data_init.init(resource, ());
    }
}

impl Dispatch<ZxdgExporterV2, ()> for State {
    fn request(
        state: &mut Self,
        _client: &Client,
        exporter: &ZxdgExporterV2,
        request: zxdg_exporter_v2::Request,
        _data: &(),
        _dh: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        let zxdg_exporter_v2::Request::ExportToplevel { id, surface } = request else {
            return;
        };
        // Only a toplevel is a window a dialog can belong to.
        if state
            .xdg_shell_state
            .toplevel_surfaces()
            .iter()
            .all(|toplevel| toplevel.wl_surface() != &surface)
        {
            exporter.post_error(
                zxdg_exporter_v2::Error::InvalidSurface,
                "only an xdg_toplevel can be exported",
            );
            data_init.init(id, Exported(None));
            return;
        }
        let handle = mint(crate::compositor::state::surface_id(&surface));
        data_init
            .init(id, Exported(Some(handle.clone())))
            .handle(handle);
    }
}

impl Dispatch<ZxdgExportedV2, Exported> for State {
    fn request(
        _state: &mut Self,
        _client: &Client,
        _exported: &ZxdgExportedV2,
        _request: zxdg_exported_v2::Request,
        _data: &Exported,
        _dh: &DisplayHandle,
        _data_init: &mut DataInit<'_, Self>,
    ) {
    }

    fn destroyed(
        _state: &mut Self,
        _client: ClientId,
        _exported: &ZxdgExportedV2,
        data: &Exported,
    ) {
        if let Exported(Some(handle)) = data {
            revoke(handle);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_parent_window_names_a_handle_or_nothing() {
        let minted = "a".repeat(32);
        assert_eq!(
            WindowHandle::parse(&format!("wayland:{minted}")),
            Some(WindowHandle::Wayland(minted))
        );
        assert_eq!(
            WindowHandle::parse("x11:4a0000c"),
            Some(WindowHandle::X11(0x4a0_000c))
        );
        assert_eq!(
            WindowHandle::parse("x11:0x1f"),
            Some(WindowHandle::X11(0x1f))
        );
        for nothing in [
            "",
            "wayland:",
            "wayland:short",
            "x11:zz",
            "mir:1",
            "4a0000c",
        ] {
            assert_eq!(WindowHandle::parse(nothing), None, "{nothing:?}");
        }
        assert_eq!(
            WindowHandle::X11(1).surface(),
            None,
            "X11 ids name no window here"
        );
    }

    #[test]
    fn a_handle_lives_as_long_as_its_export_and_its_window() {
        let surface = 0xfeed_0000_0001;
        let first = mint(surface);
        let second = mint(surface);
        assert!(is_minted_shape(&first));
        assert_ne!(first, second, "every export is its own handle");
        assert_eq!(surface_for(&first), Some(surface));
        revoke(&first);
        assert_eq!(
            surface_for(&first),
            None,
            "a destroyed export names nothing"
        );
        assert_eq!(surface_for(&second), Some(surface));
        forget(surface);
        assert_eq!(surface_for(&second), None, "a closed window names nothing");
    }
}
