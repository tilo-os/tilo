//! Layer surfaces: the session's own surfaces are Wayland clients that place themselves on a layer
//! of an output, above or below the windows. They are composited like every other client's buffer,
//! and take the pointer and the keyboard that lands on them. Only a client the session connected as
//! trusted is offered the protocol.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::arithmetic_side_effects,
    reason = "a failed step in a test is the assertion"
)]

use std::time::Duration;

use tilo_compositor::{Compositor, InputEvent, OutputConfig, StackPolicy};
use wayland_protocols_wlr::layer_shell::v1::client::{zwlr_layer_shell_v1, zwlr_layer_surface_v1};

use crate::harness::{TestClient, pump};

const BTN_LEFT: u32 = 0x110;
const KEY_A: u32 = 30;
const BACKGROUND: [u8; 4] = [26, 27, 38, 255];

fn headless() -> Compositor {
    crate::harness::headless(Box::new(StackPolicy))
}

/// A trusted client with a surface on `layer` , drawn at the size the compositor gives it.
fn layer_client(
    compositor: &mut Compositor,
    layer: zwlr_layer_shell_v1::Layer,
    anchor: zwlr_layer_surface_v1::Anchor,
    size: (u32, u32),
    exclusive: i32,
    rgba: [u8; 4],
) -> TestClient {
    let mut client = TestClient::connect_trusted(compositor);
    pump(compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.compositor.is_some()
            && clients[0].seen.shm.is_some()
            && clients[0].seen.layer_shell.is_some()
    });
    client.layer_surface(
        layer,
        anchor,
        size,
        exclusive,
        zwlr_layer_surface_v1::KeyboardInteractivity::None,
    );
    pump(compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.layer_size.is_some()
    });
    let (width, height) = client.seen.layer_size.expect("a configured size");
    client.show(
        i32::try_from(width).expect("a width"),
        i32::try_from(height).expect("a height"),
        rgba,
    );
    client
}

tilo_test_env::scenario! {
    /// Layer-shell surfaces.
    fn layer_surfaces_are_drawn_and_hit_on_their_layer;

    phase the_session_trusts_a_client_with_layer_surfaces_and_no_one_else;
    {
        let mut compositor = headless();
        let mut ordinary = TestClient::connect(&mut compositor);
        pump(&mut compositor, &mut [&mut ordinary], |_, clients| {
            clients[0].seen.compositor.is_some()
        });
        assert!(
            !ordinary
                .seen
                .globals
                .iter()
                .any(|global| global == "zwlr_layer_shell_v1"),
            "an ordinary client cannot place a surface on a layer"
        );

        let mut trusted = TestClient::connect_trusted(&mut compositor);
        pump(&mut compositor, &mut [&mut trusted], |_, clients| {
            clients[0].seen.layer_shell.is_some()
        });
        assert!(
            trusted
                .seen
                .globals
                .iter()
                .any(|global| global == "zwlr_layer_shell_v1")
        );
    }

    phase a_bar_on_the_top_layer_is_drawn_above_the_windows_and_keeps_its_height;
    {
        let mut compositor = headless();
        let mut bar = layer_client(
            &mut compositor,
            zwlr_layer_shell_v1::Layer::Top,
            zwlr_layer_surface_v1::Anchor::Top
                | zwlr_layer_surface_v1::Anchor::Left
                | zwlr_layer_surface_v1::Anchor::Right,
            (0, 32),
            32,
            [255, 0, 0, 255],
        );
        assert_eq!(bar.seen.layer_size, Some((640, 32)), "the whole width");

        pump(&mut compositor, &mut [&mut bar], |_, _| true);
        compositor.draw_frame().expect("a frame");
        let frame = compositor.screenshot("HEADLESS-1").unwrap();
        assert_eq!(frame.pixel(320, 10), Some([255, 0, 0, 255]), "the bar");
        assert_eq!(frame.pixel(320, 200), Some(BACKGROUND), "the shell below it");
        assert_eq!(
            compositor.reserved("HEADLESS-1"),
            vec![(0, 0, 640, 32)],
            "the desktop keeps windows out of the bar"
        );

        // A window opened under it is drawn below it.
        let mut window = TestClient::connect(&mut compositor);
        pump(&mut compositor, &mut [&mut window], |_, clients| {
            clients[0].seen.wm_base.is_some()
        });
        window.open_window("a-window");
        pump(&mut compositor, &mut [&mut window], |_, clients| {
            clients[0].seen.configured
        });
        window.show(640, 480, [0, 0, 255, 255]);
        pump(
            &mut compositor,
            &mut [&mut bar, &mut window],
            |compositor, _| !compositor.windows().is_empty(),
        );
        compositor.draw_frame().expect("a frame");
        let frame = compositor.screenshot("HEADLESS-1").unwrap();
        assert_eq!(frame.pixel(320, 10), Some([255, 0, 0, 255]), "the bar wins");
    }

    phase a_shell_on_the_background_layer_is_drawn_below_the_windows;
    {
        let mut compositor = headless();
        let mut shell = layer_client(
            &mut compositor,
            zwlr_layer_shell_v1::Layer::Background,
            zwlr_layer_surface_v1::Anchor::Top
                | zwlr_layer_surface_v1::Anchor::Bottom
                | zwlr_layer_surface_v1::Anchor::Left
                | zwlr_layer_surface_v1::Anchor::Right,
            (0, 0),
            0,
            [0, 255, 0, 255],
        );
        assert_eq!(shell.seen.layer_size, Some((640, 480)));
        pump(&mut compositor, &mut [&mut shell], |_, _| true);
        compositor.draw_frame().expect("a frame");
        let frame = compositor.screenshot("HEADLESS-1").unwrap();
        assert_eq!(frame.pixel(320, 240), Some([0, 255, 0, 255]));

        let mut window = TestClient::connect(&mut compositor);
        pump(&mut compositor, &mut [&mut window], |_, clients| {
            clients[0].seen.wm_base.is_some()
        });
        window.open_window("a-window");
        pump(&mut compositor, &mut [&mut window], |_, clients| {
            clients[0].seen.configured
        });
        window.show(200, 100, [0, 0, 255, 255]);
        pump(
            &mut compositor,
            &mut [&mut shell, &mut window],
            |compositor, _| !compositor.windows().is_empty(),
        );
        compositor.draw_frame().expect("a frame");
        let frame = compositor.screenshot("HEADLESS-1").unwrap();
        let (_, (x, y, w, h)) = compositor.windows()[0].clone();
        let (x, y) = (u32::try_from(x).unwrap(), u32::try_from(y).unwrap());
        let (w, h) = (u32::try_from(w).unwrap(), u32::try_from(h).unwrap());
        assert_eq!(
            frame.pixel(x + w / 2, y + h / 2),
            Some([0, 0, 255, 255]),
            "the window is above the shell"
        );
    }

    phase the_pointer_and_a_press_reach_the_layer_surface_that_is_above;
    {
        let mut compositor = headless();
        let mut bar = layer_client(
            &mut compositor,
            zwlr_layer_shell_v1::Layer::Top,
            zwlr_layer_surface_v1::Anchor::Top
                | zwlr_layer_surface_v1::Anchor::Left
                | zwlr_layer_surface_v1::Anchor::Right,
            (0, 32),
            32,
            [255, 0, 0, 255],
        );
        pump(&mut compositor, &mut [&mut bar], |_, clients| {
            clients[0].seen.pointer.is_some()
        });

        compositor
            .input(InputEvent::PointerMotion { x: 300.0, y: 16.0 })
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
        pump(&mut compositor, &mut [&mut bar], |_, clients| {
            clients[0].seen.buttons.len() == 2
        });
        assert!(bar.seen.pointer_inside);
        assert_eq!(bar.seen.buttons, vec![(BTN_LEFT, true), (BTN_LEFT, false)]);

        // Off the bar the pointer leaves it, even over the shell.

        compositor
            .input(InputEvent::PointerMotion { x: 300.0, y: 300.0 })
            .unwrap();
        pump(&mut compositor, &mut [&mut bar], |_, clients| {
            !clients[0].seen.pointer_inside
        });
    }

    phase keys_go_to_the_layer_surface_that_takes_them;
    {
        let mut compositor = headless();
        let mut prompt = TestClient::connect_trusted(&mut compositor);
        pump(&mut compositor, &mut [&mut prompt], |_, clients| {
            clients[0].seen.layer_shell.is_some() && clients[0].seen.seat.is_some()
        });
        prompt.layer_surface(
            zwlr_layer_shell_v1::Layer::Overlay,
            zwlr_layer_surface_v1::Anchor::Top | zwlr_layer_surface_v1::Anchor::Left,
            (320, 200),
            0,
            zwlr_layer_surface_v1::KeyboardInteractivity::Exclusive,
        );
        pump(&mut compositor, &mut [&mut prompt], |_, clients| {
            clients[0].seen.layer_size.is_some()
        });
        prompt.show(320, 200, [255, 255, 0, 255]);
        pump(&mut compositor, &mut [&mut prompt], |_, clients| {
            clients[0].seen.keyboard_focused
        });

        compositor
            .input(InputEvent::Key {
                keycode: KEY_A,
                pressed: true,
            })
            .unwrap();
        compositor
            .input(InputEvent::Key {
                keycode: KEY_A,
                pressed: false,
            })
            .unwrap();
        pump(&mut compositor, &mut [&mut prompt], |_, clients| {
            clients[0].seen.keys.len() == 2
        });
        assert_eq!(prompt.seen.keys, vec![(KEY_A, true), (KEY_A, false)]);
    }

    phase a_layer_surface_is_hit_where_it_is_drawn_and_never_shown_at_another_size;
    // A panel resting on the bottom edge that grows: until its app draws the new size, the frame on
    // screen is the one it drew, at its own size, on that edge; the pointer reaches it where
    // it is drawn, one logical pixel for one; then the frame of the new size replaces it. At scale 1
    // and 2 alike.
    {
        const TOP: [u8; 4] = [255, 0, 0, 255];
        const BOTTOM: [u8; 4] = [0, 255, 0, 255];
        for scale in [1, 2] {
            let mut compositor = Compositor::headless(Box::new(StackPolicy)).expect("compositor");
            compositor
                .add_output(&OutputConfig::new("HEADLESS-1", 640, 480).with_scale(f64::from(scale)))
                .expect("output");
            let output_h = 480 / scale;
            let mut panel = TestClient::connect_trusted(&mut compositor);
            pump(&mut compositor, &mut [&mut panel], |_, clients| {
                clients[0].seen.layer_shell.is_some() && clients[0].seen.seat.is_some()
            });
            let (surface, layer) = panel.layer_surface(
                zwlr_layer_shell_v1::Layer::Top,
                zwlr_layer_surface_v1::Anchor::Bottom | zwlr_layer_surface_v1::Anchor::Left,
                (100, 40),
                0,
                zwlr_layer_surface_v1::KeyboardInteractivity::None,
            );
            pump(&mut compositor, &mut [&mut panel], |_, clients| {
                clients[0].seen.layer_size == Some((100, 40))
            });
            panel.show_bands((100, 40), scale, TOP, BOTTOM);
            pump(&mut compositor, &mut [&mut panel], |_, clients| {
                clients[0].seen.pointer.is_some()
            });

            // The device pixel at a logical point, and the colour drawn there.
            let shown = |compositor: &mut Compositor, x: i32, y: i32| {
                compositor.draw_frame().expect("a frame");
                let frame = compositor.screenshot("HEADLESS-1").unwrap();
                frame.pixel(
                    u32::try_from(x * scale + scale / 2).unwrap(),
                    u32::try_from(y * scale + scale / 2).unwrap(),
                )
            };
            let top = output_h - 40;
            assert_eq!(
                shown(&mut compositor, 30, top + 5),
                Some(TOP),
                "at {scale}x"
            );
            assert_eq!(
                shown(&mut compositor, 30, top + 35),
                Some(BOTTOM),
                "at {scale}x"
            );

            // It asks to grow, and the compositor configures the new size before its app has drawn it.
            layer.set_size(100, 80);
            surface.commit();
            pump(&mut compositor, &mut [&mut panel], |_, clients| {
                clients[0].seen.layer_size == Some((100, 80))
            });
            surface.commit();
            pump(&mut compositor, &mut [&mut panel], |_, _| true);
            assert_eq!(
                shown(&mut compositor, 30, top + 5),
                Some(TOP),
                "the frame it drew stays at its size on the edge it rests on, at {scale}x"
            );
            assert_eq!(
                shown(&mut compositor, 30, top + 35),
                Some(BOTTOM),
                "at {scale}x"
            );
            assert_eq!(
                shown(&mut compositor, 30, top - 20),
                Some(BACKGROUND),
                "nothing stands where it has not drawn yet, at {scale}x"
            );
            // The pointer on a drawn pixel reaches that pixel of the surface.
            compositor
                .input(InputEvent::PointerMotion {
                    x: 30.5,
                    y: f64::from(top) + 35.5,
                })
                .unwrap();
            pump(&mut compositor, &mut [&mut panel], |_, clients| {
                clients[0].seen.pointer_at.is_some()
            });
            assert_eq!(panel.seen.pointer_at, Some((30.5, 35.5)), "at {scale}x");

            // The frame of the new size replaces it, grown up from the same edge.
            panel.show_bands((100, 80), scale, TOP, BOTTOM);
            pump(&mut compositor, &mut [&mut panel], |_, _| true);
            let top = output_h - 80;
            assert_eq!(
                shown(&mut compositor, 30, top + 5),
                Some(TOP),
                "at {scale}x"
            );
            assert_eq!(
                shown(&mut compositor, 30, top + 75),
                Some(BOTTOM),
                "at {scale}x"
            );
            compositor
                .input(InputEvent::PointerMotion {
                    x: 70.25,
                    y: f64::from(top) + 10.75,
                })
                .unwrap();
            pump(&mut compositor, &mut [&mut panel], |_, clients| {
                clients[0].seen.pointer_at == Some((70.25, 10.75))
            });
        }
    }
}

#[test]
fn an_idle_session_draws_nothing() {
    // A bar and a window on screen, both drawn once.
    let mut compositor = headless();
    let mut bar = layer_client(
        &mut compositor,
        zwlr_layer_shell_v1::Layer::Top,
        zwlr_layer_surface_v1::Anchor::Top
            | zwlr_layer_surface_v1::Anchor::Left
            | zwlr_layer_surface_v1::Anchor::Right,
        (0, 32),
        32,
        [255, 0, 0, 255],
    );
    let mut window = TestClient::connect(&mut compositor);
    pump(&mut compositor, &mut [&mut window], |_, clients| {
        clients[0].seen.wm_base.is_some()
    });
    window.open_window("a-window");
    pump(&mut compositor, &mut [&mut window], |_, clients| {
        clients[0].seen.configured
    });
    window.show(640, 480, [0, 0, 255, 255]);
    pump(
        &mut compositor,
        &mut [&mut bar, &mut window],
        |compositor, _| !compositor.windows().is_empty(),
    );
    compositor.draw_frame().expect("a frame");
    pump(&mut compositor, &mut [&mut bar, &mut window], |_, _| true);
    let (bar_frames, window_frames) = (bar.seen.frames_done, window.seen.frames_done);
    assert!(bar_frames > 0, "the bar was drawn");

    // Three seconds of a desktop asking for frames at its refresh, with nothing happening: the
    // session draws no frame and asks its clients for none.
    let idle = Duration::from_secs(3);
    let refresh = Duration::from_millis(16);
    let started = std::time::Instant::now();
    while started.elapsed() < idle {
        compositor.dispatch(refresh).unwrap();
        compositor.draw_frame().expect("a frame");
        bar.exchange();
        window.exchange();
    }
    assert_eq!(
        bar.seen.frames_done, bar_frames,
        "the bar was not asked again"
    );
    assert_eq!(
        window.seen.frames_done, window_frames,
        "the window was not asked again"
    );
    // What it shows is the frame it drew: nothing changed.
    let frame = compositor.screenshot("HEADLESS-1").unwrap();
    assert_eq!(frame.pixel(320, 10), Some([255, 0, 0, 255]), "the bar");
    assert_eq!(frame.pixel(320, 200), Some([0, 0, 255, 255]), "the window");
}

/// A layer surface that asks for another size waits for the configure that answers it before it
/// draws, so it is answered whether or not the size it gets changes: a bar that spans its output
/// keeps its width whatever width it asks for, and still hears back.
#[test]
fn a_size_asked_for_is_answered_even_when_it_changes_nothing() {
    let mut compositor = headless();
    let mut bar = TestClient::connect_trusted(&mut compositor);
    pump(&mut compositor, &mut [&mut bar], |_, clients| {
        clients[0].seen.compositor.is_some() && clients[0].seen.layer_shell.is_some()
    });
    let (surface, layer) = bar.layer_surface(
        zwlr_layer_shell_v1::Layer::Top,
        zwlr_layer_surface_v1::Anchor::Top
            | zwlr_layer_surface_v1::Anchor::Left
            | zwlr_layer_surface_v1::Anchor::Right,
        (0, 32),
        32,
        zwlr_layer_surface_v1::KeyboardInteractivity::None,
    );
    pump(&mut compositor, &mut [&mut bar], |_, clients| {
        clients[0].seen.layer_size == Some((640, 32))
    });
    let configured = bar.seen.layer_configures;

    layer.set_size(300, 32);
    surface.commit();
    pump(&mut compositor, &mut [&mut bar], |_, clients| {
        clients[0].seen.layer_configures > configured
    });
    assert_eq!(bar.seen.layer_size, Some((640, 32)), "it spans its output");

    // Asking for nothing new is not answered again.
    let configured = bar.seen.layer_configures;
    surface.commit();
    for _ in 0..20 {
        compositor.dispatch(Duration::from_millis(2)).unwrap();
        bar.exchange();
    }
    assert_eq!(bar.seen.layer_configures, configured);
}

/// A panel the session keeps running between openings stops drawing while it is hidden. Shown
/// again where the stack places it, it holds the frame it was left on, at the size it asked for:
/// the session shows it at once rather than hold the person looking at nothing until a wait for a
/// frame a hidden app never draws gives up. A show that names the commit it must follow still
/// waits for one drawn after it.
#[test]
fn a_placed_surface_holding_its_sized_frame_shows_at_once() {
    const PANEL: [u8; 4] = [255, 255, 0, 255];
    let mut compositor = headless();
    let mut panel = TestClient::connect_trusted(&mut compositor);
    pump(&mut compositor, &mut [&mut panel], |_, clients| {
        clients[0].seen.layer_shell.is_some()
    });
    let (_surface, _layer) = panel.layer_surface(
        zwlr_layer_shell_v1::Layer::Overlay,
        zwlr_layer_surface_v1::Anchor::Top | zwlr_layer_surface_v1::Anchor::Left,
        (380, 408),
        0,
        zwlr_layer_surface_v1::KeyboardInteractivity::None,
    );
    pump(&mut compositor, &mut [&mut panel], |_, clients| {
        clients[0].seen.layer_size == Some((380, 408))
    });
    // The session places it, as it places the panel under the bar's trailing end.
    compositor.place_layer_in("test", Some((40, 32, 380, 440)), false);
    panel.show(380, 408, PANEL);
    pump(&mut compositor, &mut [&mut panel], |_, _| true);

    // Hidden between openings, as the panel is: the app draws nothing more while it is hidden.
    compositor.show_layer("test", false);
    compositor.draw_frame().expect("a frame");
    pump(&mut compositor, &mut [&mut panel], |_, _| true);
    assert!(!compositor.layer_visible("test"), "hidden");

    // Opened again with no page asked of it: it is seen on the very next frame, not the one a wait
    // for another commit would leave it out of.
    compositor.show_layer("test", true);
    assert!(
        compositor.layer_visible("test"),
        "the frame it holds is at the size it asked for: shown at once"
    );
    assert_eq!(
        compositor.layer_settle_deadline(),
        None,
        "the session is not left waiting for a frame the hidden app never draws"
    );
    compositor.draw_frame().expect("a frame");
    let frame = compositor.screenshot("HEADLESS-1").unwrap();
    assert_eq!(
        frame.pixel(48, 40),
        Some(PANEL),
        "the panel's own frame, where it stands"
    );

    // A show that names a frame the app has not drawn after still waits for it.
    compositor.show_layer("test", false);
    compositor.draw_frame().expect("a frame");
    compositor.show_layer_after("test", compositor.layer_frames("test"));
    assert!(
        !compositor.layer_visible("test"),
        "a frame drawn after the ask is what the show waits for"
    );
    assert!(
        compositor.layer_settle_deadline().is_some(),
        "and it does wait for one"
    );
}
