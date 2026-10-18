//! Window handles over a real socket: `zxdg_exporter_v2`.
//!
//! A portal request names its window as `parent_window = wayland:<handle>`.
//! Two windows of one app get two handles, each naming its own window, so a
//! dialog can hang on the exact window that asked; a destroyed export or a
//! closed window names nothing, and only a toplevel can be exported.

use shell::compositor::window_handles::WindowHandle;
use wayland_client::protocol::{wl_compositor, wl_surface};
use wayland_client::{Connection, Dispatch, Proxy as _, QueueHandle, delegate_noop};
use wayland_protocols::xdg::foreign::zv2::client::{zxdg_exported_v2, zxdg_exporter_v2};
use wayland_protocols::xdg::shell::client::{xdg_toplevel, xdg_wm_base};

#[derive(Default)]
struct Client {
    globals: Vec<(u32, String, u32)>,
    /// `(exported object id, handle)` as the compositor sent them.
    handles: Vec<(u32, String)>,
}

crate::support::wayland::test_client!(Client);

impl Dispatch<zxdg_exported_v2::ZxdgExportedV2, ()> for Client {
    fn event(
        state: &mut Self,
        exported: &zxdg_exported_v2::ZxdgExportedV2,
        event: zxdg_exported_v2::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zxdg_exported_v2::Event::Handle { handle } = event {
            state.handles.push((exported.id().protocol_id(), handle));
        }
    }
}

delegate_noop!(Client: ignore wl_compositor::WlCompositor);
delegate_noop!(Client: ignore wl_surface::WlSurface);
delegate_noop!(Client: ignore xdg_toplevel::XdgToplevel);
delegate_noop!(Client: ignore zxdg_exporter_v2::ZxdgExporterV2);

type Harness = crate::support::wayland::Harness<Client>;

fn handle_of(h: &Harness, exported: &zxdg_exported_v2::ZxdgExportedV2) -> String {
    h.client
        .handles
        .iter()
        .find(|(id, _)| *id == exported.id().protocol_id())
        .map(|(_, handle)| handle.clone())
        .expect("the export was answered with a handle")
}

fn named(handle: &str) -> Option<u64> {
    WindowHandle::parse(&format!("wayland:{handle}")).and_then(|handle| handle.surface())
}

#[test]
fn two_windows_of_one_app_get_handles_that_name_each_window() {
    let mut h = Harness::new();
    let compositor: wl_compositor::WlCompositor = h.bind("wl_compositor", 4);
    let wm_base: xdg_wm_base::XdgWmBase = h.bind("xdg_wm_base", 2);
    let exporter: zxdg_exporter_v2::ZxdgExporterV2 = h.bind("zxdg_exporter_v2", 1);
    let qh = h.queue.handle();
    let mut windows = Vec::new();
    for _ in 0..2 {
        let surface = compositor.create_surface(&qh, ());
        let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
        let toplevel = xdg.get_toplevel(&qh, ());
        toplevel.set_app_id("org.example.Editor".into());
        surface.commit();
        h.pump();
        windows.push((surface, xdg, toplevel));
    }
    let tabs: Vec<u64> = crate::support::wayland::foreign_tabs(&h.state)
        .into_iter()
        .filter(|(app, _)| app == "org.example.Editor")
        .map(|(_, surface_id)| surface_id)
        .collect();
    assert_eq!(tabs.len(), 2, "both windows are tabs: {tabs:?}");

    let first = exporter.export_toplevel(&windows[0].0, &qh, ());
    let second = exporter.export_toplevel(&windows[1].0, &qh, ());
    h.pump();
    let (first_handle, second_handle) = (handle_of(&h, &first), handle_of(&h, &second));
    assert_ne!(first_handle, second_handle);
    let (first_window, second_window) = (named(&first_handle), named(&second_handle));
    assert!(first_window.is_some() && second_window.is_some());
    assert_ne!(
        first_window, second_window,
        "each handle names its own window"
    );
    assert!(tabs.contains(&first_window.unwrap()) && tabs.contains(&second_window.unwrap()));

    first.destroy();
    h.pump();
    assert_eq!(
        named(&first_handle),
        None,
        "a destroyed export names nothing"
    );
    assert_eq!(named(&second_handle), second_window);

    let (surface, xdg, toplevel) = windows.remove(1);
    toplevel.destroy();
    xdg.destroy();
    surface.destroy();
    h.pump();
    assert_eq!(named(&second_handle), None, "a closed window names nothing");
    assert!(
        h.conn.protocol_error().is_none(),
        "{:?}",
        h.conn.protocol_error()
    );
}

#[test]
fn only_a_toplevel_can_be_exported() {
    let mut h = Harness::new();
    let compositor: wl_compositor::WlCompositor = h.bind("wl_compositor", 4);
    let exporter: zxdg_exporter_v2::ZxdgExporterV2 = h.bind("zxdg_exporter_v2", 1);
    let qh = h.queue.handle();
    let bare = compositor.create_surface(&qh, ());
    let _exported = exporter.export_toplevel(&bare, &qh, ());
    h.pump();
    assert!(
        h.conn.protocol_error().is_some(),
        "a surface with no toplevel role is an invalid_surface error"
    );
}
