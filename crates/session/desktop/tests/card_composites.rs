//! A floating card drawn over the windows reaches the composited output: the pixels the card was
//! drawn with show where the card stands, in the software path a headless session composes with.

#![cfg(target_os = "linux")]
use std::cell::RefCell;
use std::rc::Rc;

use crate::support::layout_tree;
use crate::support::send_chord;
use tilo_compositor::{Compositor, OutputConfig, StackPolicy};
use tilo_desktop::about::{AboutLinks, AboutSheet, AboutVariant};
use tilo_desktop::desktop_host::{DesktopChrome, NoticeTiles, PermissiveRules};
use tilo_desktop::topbar::TopBarChrome;
use tilo_desktop_surface::{DesktopHandle, DesktopSurface};
use tilo_session_core::surface::ShellSurface as _;
use tilo_shell_surface::{Placement, WidgetSurface};

/// The chrome over the shell at `scale` : built after `_compositor` opened its backend, as the
/// session builds its surfaces after its compositor. A host built before the backend is open draws
/// its frames for a compositor that samples textures rather than one that reads pixels, which is
/// not the path a headless session takes.
fn chrome_host(
    _compositor: &mut Compositor,
    scale: f64,
    size: (u32, u32),
) -> (WidgetSurface, DesktopHandle, ui::headless::FrameHarness) {
    let tree = layout_tree();
    let (surface, handle) = DesktopSurface::new(tree.clone(), 7, Box::new(NoticeTiles));
    let mut shell =
        ui::headless::FrameHarness::new(ui::state::State::new(), Box::new(surface), size);
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
    let host = WidgetSurface::new(
        Box::new(chrome),
        Placement {
            origin: (0, 0),
            size: (
                ui::num::u32_of_f64((f64::from(size.0) / scale).round()),
                ui::num::u32_of_f64((f64::from(size.1) / scale).round()),
            ),
            scale,
        },
        false,
    )
    .with_keyboard(Rc::new(move || keyboard.search_open()));
    (host, handle, shell)
}

/// How far apart, in card pixels, the card's samples stand.
const SAMPLE_STEP: f64 = 8.0;

/// The card the palette stands in, drawn over the output at `scale` .
fn assert_palette_shows(scale: f64, size: (u32, u32)) {
    let mut compositor = Compositor::headless(Box::new(StackPolicy)).unwrap();
    compositor
        .add_output(
            &OutputConfig::new(
                "HEADLESS-1",
                i32::try_from(size.0).unwrap_or(i32::MAX),
                i32::try_from(size.1).unwrap_or(i32::MAX),
            )
            .with_scale(scale),
        )
        .unwrap();
    let (mut host, handle, mut shell) = chrome_host(&mut compositor, scale, size);
    host.relayout();
    host.prepare_frame();
    send_chord(&mut host);
    shell.relayout();
    host.relayout();
    host.prepare_frame();
    assert!(
        handle.search_open(),
        "the chord opened the palette at scale {scale}"
    );
    let card = {
        let search = handle.search_palette();
        let search = search.borrow();
        ui::Widget::get_rect(search.palette())
    };
    compositor.add_surface(Rc::new(RefCell::new(host)));
    let mut shot = None;
    for _ in 0..8 {
        compositor.draw_frame().unwrap();
        shot = Some(compositor.screenshot("HEADLESS-1").unwrap());
    }
    let shot = shot.unwrap();
    // The card's own ground is opaque, so somewhere inside it the output differs from the scrim at
    // the edge of the display.
    let corner = shot.pixel(4, 4).unwrap();
    let mut differing = 0u32;
    let mut samples = 0u32;
    let inner = card.inflate(-24.0, -24.0);
    let columns = ui::num::usize_of_f64((inner.width() / SAMPLE_STEP).ceil());
    let rows = ui::num::usize_of_f64((inner.height() / SAMPLE_STEP).ceil());
    for row in 0..rows {
        let y = inner.y0 + ui::num::f64_of_usize(row) * SAMPLE_STEP;
        for column in 0..columns {
            let x = inner.x0 + ui::num::f64_of_usize(column) * SAMPLE_STEP;
            let pixel = shot
                .pixel(
                    ui::num::u32_of_f64(x * scale),
                    ui::num::u32_of_f64(y * scale),
                )
                .unwrap();
            if pixel != corner {
                differing = differing.saturating_add(1);
            }
            samples = samples.saturating_add(1);
        }
    }
    assert!(
        samples > 100,
        "at scale {scale} the card is {card:?}, large enough to sample"
    );
    assert!(
        differing.saturating_mul(2) > samples,
        "the card shows over the scrim at scale {scale}: {differing} of {samples} pixels \
         differ from {corner:?}"
    );
}

/// At one pixel a point and at twice the pixels, each on a compositor, an output and a shell of its
/// own: nothing of one density's session is there for the next.
#[test]
fn the_open_palettes_card_reaches_the_composited_output() {
    let _settled = ui::motion::testing::settled();
    for (scale, size) in [(1.0, (1280, 800)), (2.0, (2560, 1600))] {
        // Said before the row runs, so a failure anywhere in it names its density.
        println!("the palette's card at scale {scale}, on an output of {size:?}");
        assert_palette_shows(scale, size);
    }
}
