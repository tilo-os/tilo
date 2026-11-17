//! Who draws the title bar.
//! The desktop draws each window's frame itself — the tab is the frame — so an app that also draws
//! one draws a second bar inside the first. Both decoration protocols have to say so, because a
//! toolkit speaks whichever of the two it knows: `xdg-decoration` for most, the older
//! `org_kde_kwin_server_decoration` for Qt5 and some GTK and Wine builds.
//! Not only cosmetic: a client that keeps its own title bar keeps it out of the size it was
//! configured — configured 670x668 it commits 670x633 and never commits again, leaving a 35px dead
//! band of empty tab and stranding the presentation check.
//! The frame belongs to the desktop rather than to the window's state, so it is the same frame
//! focused or not: what is checked is that neither protocol changes its answer when another window
//! takes the keyboard.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    reason = "a failed step in a test is the assertion"
)]

use smithay::utils::{Logical, Point, Rectangle};
use tilo_compositor::{Compositor, InputEvent, WindowInfo, WindowPolicy};
use wayland_protocols::xdg::decoration::zv1::client::zxdg_toplevel_decoration_v1::Mode as XdgMode;
use wayland_protocols_misc::server_decoration::client::{
    org_kde_kwin_server_decoration::Mode as KdeMode,
    org_kde_kwin_server_decoration_manager::Mode as KdeDefaultMode,
};

use crate::harness::{TestClient, headless, pump};

const BTN_LEFT: u32 = 0x110;

/// Connect and wait until the registry has told the client what exists.
fn connected(compositor: &mut Compositor) -> TestClient {
    let mut client = TestClient::connect(compositor);
    pump(compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.wm_base.is_some()
    });
    client
}

/// Places windows left to right, 300 logical pixels apart, at 250x200.
#[derive(Default)]
struct Columns {
    placed: i32,
}

impl WindowPolicy for Columns {
    fn initial_size(
        &mut self,
        _: &WindowInfo,
        _: &[Rectangle<i32, Logical>],
    ) -> Option<(i32, i32)> {
        Some((250, 200))
    }

    fn place(
        &mut self,
        _: &WindowInfo,
        _: (i32, i32),
        _: &[Rectangle<i32, Logical>],
    ) -> Point<i32, Logical> {
        let x = self.placed * 300;
        self.placed += 1;
        Point::from((x, 0))
    }
}

tilo_test_env::scenario! {
    /// Server-side decorations.
    fn the_desktop_draws_the_frame_of_every_window;

    phase both_decoration_managers_are_advertised;
    {
        let mut compositor = headless(Box::<Columns>::default());
        let client = connected(&mut compositor);

        let names: Vec<&str> = client
            .seen
            .globals
            .iter()
            .map(String::as_str)
            .collect();
        assert!(
            !names.is_empty(),
            "the client saw no globals at all — the harness failed to talk to \
         the compositor, so this test proved nothing."
        );
        for interface in [
            "zxdg_decoration_manager_v1",
            "org_kde_kwin_server_decoration_manager",
        ] {
            assert!(
                names.contains(&interface),
                "{interface} is not advertised, so a toolkit that decorates \
             itself when it cannot see the manager draws its own title bar \
             and keeps it out of the size this compositor gave it. \
             Advertised: {names:?}"
            );
        }
    }

    phase a_client_that_asks_to_draw_its_own_frame_is_told_the_desktop_does;
    {
        let mut compositor = headless(Box::<Columns>::default());
        let mut client = connected(&mut compositor);

        // The order toolkits use: create the decoration, state a preference,
        // then commit, so the answer rides on the first configure.
        client.create_window("own-frame");
        client.ask_for_client_side();
        client.commit();
        pump(&mut compositor, &mut [&mut client], |_, clients| {
            !clients[0].seen.xdg_modes.is_empty()
        });

        assert!(
            client
                .seen
                .xdg_modes
                .iter()
                .all(|mode| *mode == XdgMode::ServerSide),
            "expected every configure to say ServerSide; got {:?}. A client \
         told ClientSide draws its own title bar inside the frame the \
         desktop already drew for it.",
            client.seen.xdg_modes
        );
    }

    phase a_kde_client_that_asks_to_draw_its_own_frame_is_told_server;
    {
        let mut compositor = headless(Box::<Columns>::default());
        let mut client = connected(&mut compositor);

        client.create_window("kde-frame");
        client
            .decorate_kde()
            .request_mode(KdeMode::Client);
        client.commit();
        // Each pump lets several requests and their replies cross: an exchange that feeds itself shows
        // up as one more mode per round.
        for _ in 0..4 {
            pump(&mut compositor, &mut [&mut client], |_, _| true);
        }

        assert_eq!(
            client.seen.kde_modes,
            vec![KdeMode::Server],
            "a KDE-protocol client is told Server once, when its decoration is \
         created; answering its request again would loop forever with a \
         client that acknowledges with the mode it wants"
        );
    }

    phase a_client_that_asks_for_no_frame_of_its_own_is_told_server;
    {
        let mut compositor = headless(Box::<Columns>::default());
        let mut client = connected(&mut compositor);

        // Binding the manager is what asks for the default, so it is one exchange after the registry
        // has been read.
        pump(&mut compositor, &mut [&mut client], |_, clients| {
            clients[0].seen.kde_default.is_some()
        });
        assert_eq!(
            client.seen.kde_default,
            Some(KdeDefaultMode::Server),
            "a client that never states a preference must be told the desktop \
         draws the frame"
        );

        client.create_window("default-frame");
        client.decorate_kde();
        client.commit();
        pump(&mut compositor, &mut [&mut client], |_, clients| {
            !clients[0].seen.kde_modes.is_empty()
        });
        assert_eq!(
            client.seen.kde_modes,
            vec![KdeMode::Server],
            "the frame of a window that states no preference is the desktop's"
        );
    }

    phase the_frame_does_not_change_when_another_window_takes_focus;
    {
        let mut compositor = headless(Box::<Columns>::default());
        let mut left = connected(&mut compositor);
        left.create_window("left");
        left.ask_for_client_side();
        left.commit();
        pump(&mut compositor, &mut [&mut left], |_, clients| {
            clients[0].seen.configured && !clients[0].seen.xdg_modes.is_empty()
        });
        left.show(250, 200, [255, 0, 0, 255]);
        pump(&mut compositor, &mut [&mut left], |compositor, _| {
            compositor.windows().len() == 1
        });

        let answer = left.seen.xdg_modes.clone();
        assert_eq!(answer, vec![XdgMode::ServerSide]);

        let mut right = connected(&mut compositor);
        right.create_window("right");
        right.ask_for_client_side();
        right.commit();
        pump(&mut compositor, &mut [&mut right], |_, clients| {
            clients[0].seen.configured && !clients[0].seen.xdg_modes.is_empty()
        });
        right.show(250, 200, [0, 0, 255, 255]);
        pump(
            &mut compositor,
            &mut [&mut left, &mut right],
            |compositor, _| compositor.windows().len() == 2,
        );

        // The right window stands at (300, 0) and has the focus; a press on the left one moves it
        // there.
        assert_eq!(
            compositor
                .focused_window()
                .and_then(|info| info.app_id)
                .as_deref(),
            Some("right")
        );
        compositor
            .input(InputEvent::PointerMotion { x: 100.0, y: 100.0 })
            .unwrap();
        compositor
            .input(InputEvent::PointerButton {
                button: BTN_LEFT,
                pressed: true,
            })
            .unwrap();
        compositor
            .input(InputEvent::PointerButton {
                button: BTN_LEFT,
                pressed: false,
            })
            .unwrap();
        pump(
            &mut compositor,
            &mut [&mut left, &mut right],
            |compositor, _| {
                compositor
                    .focused_window()
                    .and_then(|info| info.app_id)
                    .as_deref()
                    == Some("left")
            },
        );

        // Focus moved, and the frame stayed where it was: the desktop's in every case, so there is
        // nothing for the focus to change.
        assert_eq!(
            left.seen.xdg_modes, answer,
            "the decoration mode changed when another window took focus; the \
         frame is the tab the desktop draws, not a state of the window"
        );
    }
}
