//! A video player keeping the screen on through `zwp_idle_inhibit_manager_v1`.
//!
//! The request holds the session's idle inhibitor registry — what keeps the
//! screen from blanking and locking — only while the window asking is on
//! screen, and lets go when the window is hidden, the inhibitor destroyed or
//! the client gone. Reads a real socket.

use std::ops::{Deref, DerefMut};

use shell::compositor::frame::FrameBuilder;
use shell::shell::state::{Workspace, WorkspaceId};
use tilo_power_mgmt::InhibitRegistry;
use tilo_power_mgmt::inhibit::INHIBIT_IDLE;
use wayland_client::delegate_noop;
use wayland_client::protocol::{wl_compositor, wl_surface};
use wayland_protocols::wp::idle_inhibit::zv1::client::{
    zwp_idle_inhibit_manager_v1, zwp_idle_inhibitor_v1,
};
use wayland_protocols::xdg::shell::client::{xdg_toplevel, xdg_wm_base};

use crate::support::wayland;

#[derive(Default)]
struct Client {
    globals: Vec<(u32, String, u32)>,
}

crate::support::wayland::test_client!(Client);

delegate_noop!(Client: ignore wl_compositor::WlCompositor);
delegate_noop!(Client: ignore wl_surface::WlSurface);
delegate_noop!(Client: ignore xdg_toplevel::XdgToplevel);
delegate_noop!(Client: ignore zwp_idle_inhibit_manager_v1::ZwpIdleInhibitManagerV1);
delegate_noop!(Client: ignore zwp_idle_inhibitor_v1::ZwpIdleInhibitorV1);

struct Harness {
    wl: wayland::Harness<Client>,
    frame: FrameBuilder,
    registry: InhibitRegistry,
}

impl Deref for Harness {
    type Target = wayland::Harness<Client>;
    fn deref(&self) -> &Self::Target {
        &self.wl
    }
}

impl DerefMut for Harness {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.wl
    }
}

impl Harness {
    fn new() -> Self {
        let wl = wayland::Harness::new();
        let frame = FrameBuilder::with_handle(wl.state.workspace_tree.clone(), false, (1600, 900));
        Self {
            wl,
            frame,
            registry: InhibitRegistry::new(),
        }
    }

    /// The per-tick pass the session loop runs.
    fn tick(&mut self) {
        self.pump();
        shell::compositor::idle_inhibit::sync_idle_inhibitors(
            &mut self.wl.state,
            &self.frame,
            &self.registry,
        );
    }

    fn adopted(&self, app_id: &str) -> bool {
        wayland::foreign_tabs(&self.state)
            .iter()
            .any(|(a, _)| a == app_id)
    }

    fn show_workspace(&mut self, id: WorkspaceId) {
        self.frame.with_chrome(|chrome| {
            {
                let mut tree = chrome.tree_mut();
                if !tree.desktop.workspaces.iter().any(|ws| ws.id == id) {
                    tree.desktop
                        .workspaces
                        .push(Workspace::new(id, "Elsewhere"));
                }
                tree.desktop.active = id;
            }
            chrome.bump();
        });
    }
}

#[test]
fn an_inhibitor_holds_the_screen_on_only_while_its_window_is_visible() {
    let mut h = Harness::new();
    let compositor: wl_compositor::WlCompositor = h.bind("wl_compositor", 4);
    let wm_base: xdg_wm_base::XdgWmBase = h.bind("xdg_wm_base", 2);
    let manager: zwp_idle_inhibit_manager_v1::ZwpIdleInhibitManagerV1 =
        h.bind("zwp_idle_inhibit_manager_v1", 1);
    h.pump();

    let qh = h.queue.handle();
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm_base.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_app_id("org.example.Videos".into());
    surface.commit();
    h.pump();
    assert!(h.adopted("org.example.Videos"));
    let home = h.frame.chrome().tree().desktop.active;

    h.tick();
    assert!(
        !h.registry.is_inhibited_for(INHIBIT_IDLE),
        "nothing asked yet, so nothing may hold the screen on"
    );

    let inhibitor = manager.create_inhibitor(&surface, &qh, ());
    h.tick();
    assert!(
        h.registry.is_inhibited_for(INHIBIT_IDLE),
        "a visible window's inhibitor must keep the screen from blanking and locking"
    );
    assert_eq!(h.registry.active_reasons_for(INHIBIT_IDLE).len(), 1);

    h.show_workspace(WorkspaceId(9_001));
    h.tick();
    assert!(
        !h.registry.is_inhibited_for(INHIBIT_IDLE),
        "a video nobody can see is no reason to keep the session unlocked"
    );

    h.show_workspace(home);
    h.tick();
    assert!(h.registry.is_inhibited_for(INHIBIT_IDLE));

    inhibitor.destroy();
    h.tick();
    let still_held = h.registry.is_inhibited_for(INHIBIT_IDLE);
    drop(h);
    assert!(
        !still_held,
        "destroying the inhibitor must release the hold"
    );
    let _keep = (toplevel, xdg);
}
