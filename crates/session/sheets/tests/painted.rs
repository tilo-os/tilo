//! What the sheets draw over the desktop, read from the pixels of their
//! frames: every scenario of the questions, of the display sheet and of the
//! file chooser draws something, the shortcut guide is a sheet over the
//! dimmed desktop, and a sheet that stands over a desktop still in view (the
//! power menu, the question about a display change, the dictation card)
//! paints itself and nothing else.
//!
//! One test draws all of them, a row a frame. The first frame a process
//! draws is the dear one, the software GPU building its pipelines for it,
//! and a renderer made for a frame builds its shaders again: drawn a test
//! each, every sheet paid for both. Here one renderer draws frame after
//! frame, as a window's does.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use tilo_sheets::dictation::DictationApp;
use tilo_sheets::display_confirm::DisplayConfirmApp;
use tilo_sheets::displays::DisplaysApp;
use tilo_sheets::file_picker::FilePickerApp;
use tilo_sheets::portal::PortalApp;
use tilo_sheets::power::PowerApp;
use tilo_sheets::shortcut_guide::ShortcutGuideApp;
use ui::framework::ResourceApp;
use ui::framework::testing::HeadlessApp;
use ui::offscreen::OffscreenRenderer;
use ui::vello::kurbo::Rect;
use ui::vello::peniko::Color;

const VIEWPORT: (u32, u32) = (1280, 800);

/// A place in a frame.
#[derive(Clone, Copy, Debug)]
enum Place {
    /// The pixel at these coordinates of the viewport.
    At(u32, u32),
    /// The middle of the sheet's first menu row, wherever the sheet put it.
    FirstMenuRow,
}

/// What a place in a frame shows.
#[derive(Clone, Copy, Debug)]
enum Shows {
    /// Nothing of the sheet: the desktop under it shows through.
    Desktop,
    /// The desktop, dimmed: the sheet's scrim and nothing solid over it.
    Scrim,
    /// What the sheet painted there.
    Sheet,
}

/// The middle of the menu row nearest the top, as a pixel.
fn first_menu_row<A: ResourceApp>(app: &mut HeadlessApp<A>) -> (u32, u32) {
    let mut rows: Vec<Rect> = app
        .harness()
        .a11y_nodes()
        .into_iter()
        .filter(|(_, node)| node.role() == ui::accesskit::Role::MenuItem)
        .filter_map(|(_, node)| {
            let bounds = node.bounds()?;
            node.label()?;
            Some(Rect::new(bounds.x0, bounds.y0, bounds.x1, bounds.y1))
        })
        .collect();
    rows.sort_by(|a, b| a.y0.total_cmp(&b.y0));
    let middle = rows
        .first()
        .unwrap_or_else(|| panic!("{}: the sheet has a menu row", A::ID))
        .center();
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a pixel inside the viewport"
    )]
    (middle.x as u32, middle.y as u32)
}

/// One row: the sheet `A` in `scenario`, staged as a capture of it is and
/// drawn over nothing by `renderer`. The frame shows something of the sheet,
/// and at each place what `shows` says. Hands back the sheet and its frame.
fn painted<A: ResourceApp>(
    renderer: &mut OffscreenRenderer,
    scenario: &str,
    shows: &[(Place, Shows)],
) -> (HeadlessApp<A>, Vec<u8>) {
    let sheet = A::ID;
    let mut app = HeadlessApp::<A>::capture_at(Some(scenario), VIEWPORT)
        .unwrap_or_else(|error| panic!("{sheet} {scenario} is staged: {error:?}"));
    let pixels = app.harness().render_rgba_with(renderer, Color::TRANSPARENT);
    assert!(
        pixels.chunks_exact(4).remainder().is_empty(),
        "{sheet} {scenario}: the frame is whole pixels"
    );
    assert!(
        pixels
            .chunks_exact(4)
            .any(|pixel| matches!(pixel, [.., alpha] if *alpha > 0)),
        "{sheet} {scenario} draws"
    );
    // Out of the frame, the toolkit's reader panics: a place is in the viewport.
    let alpha = |x: u32, y: u32| OffscreenRenderer::sample(&pixels, VIEWPORT.0, x, y).3;
    for &(place, expected) in shows {
        let (x, y) = match place {
            Place::At(x, y) => (x, y),
            Place::FirstMenuRow => first_menu_row(&mut app),
        };
        match expected {
            Shows::Desktop => assert_eq!(
                alpha(x, y),
                0,
                "{sheet} {scenario}: the desktop shows through at {place:?} ({x}, {y})"
            ),
            Shows::Scrim => assert!(
                (1..u8::MAX).contains(&alpha(x, y)),
                "{sheet} {scenario}: the desktop shows dimmed at {place:?} ({x}, {y}): alpha {}",
                alpha(x, y)
            ),
            Shows::Sheet => assert!(
                alpha(x, y) > 0,
                "{sheet} {scenario}: the sheet is painted at {place:?} ({x}, {y})"
            ),
        }
    }
    (app, pixels)
}

#[test]
fn every_sheet_draws_its_frame_over_the_desktop() {
    use Place::{At, FirstMenuRow};
    use Shows::{Desktop, Scrim, Sheet};
    let mut renderer = OffscreenRenderer::new(VIEWPORT.0, VIEWPORT.1);

    // Every question the shell asks, every state of the display sheet and
    // every chooser draws.
    for scenario in PortalApp::scenarios() {
        painted::<PortalApp>(&mut renderer, scenario, &[]);
    }
    for scenario in DisplaysApp::scenarios() {
        painted::<DisplaysApp>(&mut renderer, scenario, &[]);
    }
    for scenario in FilePickerApp::scenarios() {
        painted::<FilePickerApp>(&mut renderer, scenario, &[]);
    }

    // The guide is a sheet in the middle, the desktop dimmed around it.
    painted::<ShortcutGuideApp>(
        &mut renderer,
        "keyboard",
        &[(At(640, 400), Sheet), (At(10, 10), Scrim)],
    );
    // The power menu paints only its panel over the desktop: every row of
    // the longest menu, and no further down than the rows of the shortest.
    painted::<PowerApp>(
        &mut renderer,
        "open",
        &[
            (At(20, 400), Desktop),
            (At(20, 20), Desktop),
            (FirstMenuRow, Sheet),
            (At(1180, 280), Sheet),
        ],
    );
    painted::<PowerApp>(
        &mut renderer,
        "detached",
        &[(FirstMenuRow, Sheet), (At(1180, 280), Desktop)],
    );
    // The question about a display change: a card in the middle of every
    // display, the desktop between two cards and around a card.
    painted::<DisplayConfirmApp>(
        &mut renderer,
        "two-displays",
        &[
            (At(320, 400), Sheet),
            (At(960, 400), Sheet),
            (At(640, 400), Desktop),
            (At(10, 10), Desktop),
        ],
    );
    painted::<DisplayConfirmApp>(
        &mut renderer,
        "known-tool",
        &[(At(10, 10), Desktop), (At(640, 400), Sheet)],
    );
    // The dictation card paints only itself over the field.
    let (mut last, kept) = painted::<DictationApp>(
        &mut renderer,
        "asking",
        &[(At(10, 10), Desktop), (At(640, 400), Sheet)],
    );

    // A renderer that drew every frame above draws what a renderer of its
    // own draws: nothing of an earlier frame is in a later one.
    let alone = last.harness().render_rgba_transparent();
    assert!(
        kept == alone,
        "the last frame is the same drawn by a renderer made for it"
    );
}
