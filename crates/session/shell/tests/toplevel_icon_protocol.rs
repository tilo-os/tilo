//! A window setting its own tab icon through `xdg_toplevel_icon_v1`.
//!
//! A game, an `AppImage` or a browser's profile window has no desktop entry
//! that names its artwork; the protocol lets it hand the compositor pixels
//! (or a theme name) for its toplevel. Reads a real socket: the manager is
//! advertised with the sizes the tab pill paints at, the icon takes effect on
//! the surface's commit, and the tab's artwork is then the window's own.

use std::os::fd::{AsFd, FromRawFd, OwnedFd};

use shell::shell::state::TileContent;
use wayland_client::protocol::{wl_buffer, wl_compositor, wl_shm, wl_shm_pool, wl_surface};
use wayland_client::{Connection, Dispatch, QueueHandle, delegate_noop};
use wayland_protocols::xdg::shell::client::{xdg_toplevel, xdg_wm_base};
use wayland_protocols::xdg::toplevel_icon::v1::client::{
    xdg_toplevel_icon_manager_v1, xdg_toplevel_icon_v1,
};

#[derive(Default)]
struct Client {
    globals: Vec<(u32, String, u32)>,
    icon_sizes: Vec<i32>,
}

crate::support::wayland::test_client!(Client);

impl Dispatch<xdg_toplevel_icon_manager_v1::XdgToplevelIconManagerV1, ()> for Client {
    fn event(
        state: &mut Self,
        _: &xdg_toplevel_icon_manager_v1::XdgToplevelIconManagerV1,
        event: xdg_toplevel_icon_manager_v1::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_toplevel_icon_manager_v1::Event::IconSize { size } = event {
            state.icon_sizes.push(size);
        }
    }
}

delegate_noop!(Client: ignore wl_compositor::WlCompositor);
delegate_noop!(Client: ignore wl_surface::WlSurface);
delegate_noop!(Client: ignore wl_shm::WlShm);
delegate_noop!(Client: ignore wl_shm_pool::WlShmPool);
delegate_noop!(Client: ignore wl_buffer::WlBuffer);
delegate_noop!(Client: ignore xdg_toplevel::XdgToplevel);
delegate_noop!(Client: ignore xdg_toplevel_icon_v1::XdgToplevelIconV1);

type Harness = crate::support::wayland::Harness<Client>;

impl Harness {
    fn window_tile(&self, app_id: &str) -> TileContent {
        self.state
            .workspace_tree
            .read()
            .desktop
            .workspaces
            .iter()
            .flat_map(|workspace| workspace.strips.iter())
            .flat_map(|strip| strip.window_groups.iter())
            .flat_map(|group| group.tabs.iter())
            .find(|tab| matches!(&tab.tile, TileContent::Foreign { app_id: a, .. } if a == app_id))
            .map_or_else(
                || panic!("{app_id} was adopted into a tab"),
                |tab| tab.tile.clone(),
            )
    }

    fn external_revision(&self) -> u64 {
        self.state.workspace_tree.read().desktop.external_revision
    }
}

/// A square ARGB buffer of one colour, as a client would draw its icon.
fn solid_buffer(h: &Harness, edge: i32, argb: [u8; 4]) -> wl_buffer::WlBuffer {
    let qh = h.queue.handle();
    let stride = edge.checked_mul(4).unwrap();
    let len = usize::try_from(stride.checked_mul(edge).unwrap()).unwrap();
    // SAFETY: a null-terminated name and a valid flag word are the whole
    // contract of `memfd_create`.
    let raw = unsafe { libc::memfd_create(c"tilo-toplevel-icon-test".as_ptr(), 0) };
    assert!(
        raw >= 0,
        "memfd_create: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: `memfd_create` just handed us this descriptor.
    let file = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(raw) });
    file.set_len(u64::try_from(len).unwrap()).unwrap();
    {
        use std::os::unix::fs::FileExt;
        // Little-endian ARGB8888 is B, G, R, A in memory.
        let [alpha, red, green, blue] = argb;
        let pixels: Vec<u8> = std::iter::repeat_n([blue, green, red, alpha], len / 4)
            .flatten()
            .collect();
        file.write_at(&pixels, 0).unwrap();
    }
    let shm = h.bind::<wl_shm::WlShm>("wl_shm", 1);
    let pool = shm.create_pool(file.as_fd(), i32::try_from(len).unwrap(), &qh, ());
    pool.create_buffer(0, edge, edge, stride, wl_shm::Format::Argb8888, &qh, ())
}

#[test]
fn a_window_icon_set_by_the_client_becomes_its_tab_artwork() {
    let mut h = Harness::new();
    let compositor: wl_compositor::WlCompositor = h.bind("wl_compositor", 4);
    let wm_base: xdg_wm_base::XdgWmBase = h.bind("xdg_wm_base", 2);
    let manager: xdg_toplevel_icon_manager_v1::XdgToplevelIconManagerV1 =
        h.bind("xdg_toplevel_icon_manager_v1", 1);
    h.pump();
    let mut sizes = h.client.icon_sizes.clone();
    sizes.sort_unstable();
    assert_eq!(
        sizes,
        vec![16, 32, 48],
        "the manager tells clients the sizes the tab pill paints at"
    );

    let qh = h.queue.handle();
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_app_id("org.example.UnlistedGame".into());
    surface.commit();
    h.pump();
    let TileContent::Foreign { surface_id, .. } = h.window_tile("org.example.UnlistedGame") else {
        unreachable!("window_tile only returns foreign tiles");
    };
    assert!(
        shell::app_icons::for_window(surface_id, "org.example.UnlistedGame").is_none(),
        "no desktop entry and no theme icon: nothing to show yet"
    );

    let small = solid_buffer(&h, 16, [0xff, 0x20, 0x80, 0xc0]);
    let large = solid_buffer(&h, 32, [0xff, 0x20, 0x80, 0xc0]);
    let icon = manager.create_icon(&qh, ());
    icon.add_buffer(&small, 1);
    icon.add_buffer(&large, 1);
    manager.set_icon(&toplevel, Some(&icon));
    h.pump();
    assert!(
        shell::app_icons::for_window(surface_id, "org.example.UnlistedGame").is_none(),
        "the icon is double-buffered: nothing changes before the commit"
    );

    let before = h.external_revision();
    surface.commit();
    h.pump();
    assert_ne!(
        h.external_revision(),
        before,
        "the tab must be told to repaint when its artwork arrives"
    );
    let artwork = shell::app_icons::for_tile(&h.window_tile("org.example.UnlistedGame"))
        .expect("the tab paints the window's own icon");
    assert!(artwork.key().starts_with("toplevel:"), "{}", artwork.key());
    let pixels = artwork.pixels(28).expect("pixels for a 2x pill");
    assert_eq!((pixels.width, pixels.height), (32, 32));
    assert_eq!(
        pixels.data.data().get(0..4),
        Some(&[0x20, 0x80, 0xc0, 0xff][..])
    );
    assert_eq!(artwork.pixels(14).map(|p| p.width), Some(16));

    manager.set_icon(&toplevel, None);
    surface.commit();
    h.pump();
    assert!(
        shell::app_icons::for_window(surface_id, "org.example.UnlistedGame").is_none(),
        "unsetting the icon returns the tab to its application's artwork"
    );
    let _keep = (xdg, icon);
}

/// Chromium's order: the icon object goes first, then its buffer, then the
/// commit. The protocol only requires the buffer to outlive the icon object.
#[test]
fn a_client_that_frees_its_icon_after_set_icon_stays_connected_and_keeps_it() {
    let mut h = Harness::new();
    let compositor: wl_compositor::WlCompositor = h.bind("wl_compositor", 4);
    let wm_base: xdg_wm_base::XdgWmBase = h.bind("xdg_wm_base", 2);
    let manager: xdg_toplevel_icon_manager_v1::XdgToplevelIconManagerV1 =
        h.bind("xdg_toplevel_icon_manager_v1", 1);
    let qh = h.queue.handle();
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_app_id("org.example.Browser".into());
    surface.commit();
    h.pump();

    let buffer = solid_buffer(&h, 32, [0xff, 0x10, 0x20, 0x30]);
    let icon = manager.create_icon(&qh, ());
    icon.add_buffer(&buffer, 1);
    manager.set_icon(&toplevel, Some(&icon));
    icon.destroy();
    buffer.destroy();
    surface.commit();
    h.pump();

    assert!(
        h.conn.protocol_error().is_none(),
        "freeing the buffer after its icon object is legal: {:?}",
        h.conn.protocol_error()
    );
    let artwork = shell::app_icons::for_tile(&h.window_tile("org.example.Browser"))
        .expect("the icon survives its protocol objects");
    let pixels = artwork.pixels(32).expect("the copied pixels");
    assert_eq!(
        pixels.data.data().get(0..4),
        Some(&[0x10, 0x20, 0x30, 0xff][..])
    );
    let _keep = xdg;
}

#[test]
fn freeing_an_icon_buffer_while_its_icon_object_lives_is_a_no_buffer_error() {
    let mut h = Harness::new();
    let manager: xdg_toplevel_icon_manager_v1::XdgToplevelIconManagerV1 =
        h.bind("xdg_toplevel_icon_manager_v1", 1);
    let qh = h.queue.handle();
    let buffer = solid_buffer(&h, 16, [0xff, 0, 0, 0]);
    let icon = manager.create_icon(&qh, ());
    icon.add_buffer(&buffer, 1);
    buffer.destroy();
    h.pump();
    let error = h.conn.protocol_error().expect("a protocol error");
    assert_eq!(error.object_interface, "xdg_toplevel_icon_v1");
    assert_eq!(error.code, u32::from(xdg_toplevel_icon_v1::Error::NoBuffer));
}
