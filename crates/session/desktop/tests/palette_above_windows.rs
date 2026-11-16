//! The palette's host stands above the windows: while it is up the compositor gives it the pointer
//! across its card, and once it closes nothing of the palette is painted there any more.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]
use std::rc::Rc;

use crate::support::layout_tree;
use crate::support::send_chord;
use tilo_desktop::about::{AboutLinks, AboutSheet, AboutVariant};
use tilo_desktop::desktop_host::{DesktopChrome, NoticeTiles, PermissiveRules};
use tilo_desktop::topbar::TopBarChrome;
use tilo_desktop_surface::{DesktopHandle, DesktopSurface};
use tilo_session_core::surface::SurfaceHost as _;
use tilo_surface_host::{Placement, WidgetHost};

const SIZE: (u32, u32) = (1280, 800);

/// The chrome as the session's own host shows it: over the windows, covering the output.
fn chrome_host() -> (WidgetHost, DesktopHandle, ui::headless::FrameHarness) {
    let tree = layout_tree();
    let (surface, handle) = DesktopSurface::new(tree.clone(), 7, Box::new(NoticeTiles));
    let mut shell =
        ui::headless::FrameHarness::new(ui::state::State::new(), Box::new(surface), SIZE);
    shell.relayout();
    let chrome = DesktopChrome::new(
        handle.clone(),
        "g".into(),
        Rc::new(PermissiveRules::new(handle.clone())),
        TopBarChrome::from_tree(&tree),
        AboutSheet::new(AboutVariant::Native, &AboutLinks::default()),
        false,
    );
    let keyboard = handle.clone();
    let host = WidgetHost::new(
        Box::new(chrome),
        Placement {
            origin: (0, 0),
            size: SIZE,
            scale: 1.0,
        },
        false,
    )
    .with_keyboard(Rc::new(move || keyboard.search_open()));
    (host, handle, shell)
}

#[test]
fn the_palettes_host_stands_above_the_windows_and_paints_nothing_once_closed() {
    let (mut host, handle, mut shell) = chrome_host();
    assert!(!host.below_windows(), "the chrome stands above the windows");
    host.relayout();
    host.prepare_frame();
    // The window is in front, where the person left it.
    assert!(
        !host.accepts_pointer(640.0, 400.0),
        "nothing of the chrome there"
    );

    send_chord(&mut host);
    shell.relayout();
    host.relayout();
    host.prepare_frame();
    assert!(handle.search_open(), "the chord opened the palette");
    let card = {
        let search = handle.search_palette();
        let search = search.borrow();
        search.palette().expect("the palette is up").get_rect()
    };
    let centre = card.center();
    assert!(
        host.accepts_pointer(centre.x, centre.y),
        "the palette is painted at {centre:?}, over the window"
    );
    // The dim behind the card reaches the rest of the display too: what the palette stands over is
    // still drawn, under it.
    assert!(
        host.accepts_pointer(640.0, 400.0),
        "the scrim covers the window"
    );

    // Closing it takes the keys back and leaves nothing painted.
    handle.toggle_search();
    shell.relayout();
    host.relayout();
    host.prepare_frame();
    assert!(!handle.search_open());
    assert!(
        !host.accepts_pointer(centre.x, centre.y),
        "nothing of the closed palette is painted at {centre:?}"
    );
    assert!(!host.accepts_pointer(640.0, 400.0), "nor its scrim");
}
