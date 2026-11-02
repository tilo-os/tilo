//! Two windows on one screen: where the strip puts them, and the edge each
//! one wears.

use super::*;

/// A window's place on the screen: its corner and its size.
type Placed = (i32, i32, i32, i32);

/// Two windows on one screen, the second opened beside the first: the
/// session settles on the width they share, and the window that was open
/// first draws itself in it, as a person's app does when the desk resizes
/// it. Returns the two windows' clients — closing them closes the windows —
/// and their rectangles, in the order they stand in.
fn two_windows(session: &mut Session) -> (TestClient, TestClient, [Placed; 2]) {
    let mut terminal = mapped(session, "org.example.Terminal");
    let files = mapped(session, "so.tilo.file-explorer");
    // The window that opened second takes half the display from the first,
    // on the width's own clock.
    let mut at = ui::time::now();
    for _ in 0..40 {
        at = at
            .checked_add(Duration::from_millis(16))
            .expect("a frame further");
        ui::time::testing::with_monotonic_time(at, || {
            session.step(Duration::from_millis(1)).expect("step");
        });
    }
    // The window that was open first is handed the width it now shares the
    // strip at, and draws itself in it.
    pump(session, &mut [&mut terminal], |session, clients| {
        let Some((width, height)) = clients[0].seen.size else {
            return false;
        };
        clients[0].show(width, height, [200, 40, 40, 255]);
        window(session.compositor(), "org.example.Terminal")
            .is_some_and(|(_, rect)| rect.2 == width)
    });
    let (_, terminal_rect) =
        window(session.compositor(), "org.example.Terminal").expect("a window");
    let (_, files_rect) = window(session.compositor(), "so.tilo.file-explorer").expect("a window");
    let mut rects = [terminal_rect, files_rect];
    rects.sort_by_key(|rect| rect.0);
    (terminal, files, rects)
}

/// Two windows on one screen stand on the desktop's wallpaper, not in its
/// corners: the first keeps the gutter past the leading edge, the last the
/// one before the trailing edge, they share what lies between, and both
/// start a step below the bar.
#[test]
fn two_windows_keep_the_strips_gutters_and_stand_below_the_bar() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    let (_terminal, _files, [first, last]) = two_windows(&mut session);
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a gutter's width on a screen"
    )]
    let gutter = ui::widget::STRIP_EDGE_GUTTER_PX.round() as i32;
    assert!(
        first.0 >= gutter,
        "the first window keeps the leading gutter: {first:?}"
    );
    assert!(
        first.0 + first.2 <= last.0,
        "the two stand side by side: {first:?} and {last:?}"
    );
    assert!(
        last.0 + last.2 <= WIDTH - gutter,
        "the last window keeps the trailing gutter: {last:?}"
    );
    assert!(
        (first.2 - last.2).abs() <= 2,
        "they share what lies between them: {first:?} and {last:?}"
    );
    // The bar's band, then the step of wallpaper between it and the strip,
    // then the group's own row of tabs: the window's card begins under them.
    let top = f64::from(tilo_desktop::topbar::TOPBAR_HEIGHT_PX)
        + ui::widget::STRIP_TOP_GAP_PX
        + ui::widget::tabs::TAB_BAR_HEIGHT;
    for (what, rect) in [("the first", first), ("the last", last)] {
        assert!(
            (f64::from(rect.1) - top).abs() < 1.0,
            "{what} window stands below the bar's band and its tabs, a step of \
             wallpaper between: {rect:?}, the card should start at {top}"
        );
    }
}

/// Each window wears its edge once, in the desk's own colour — the bright
/// one on the window in use, the dim one on the other, as the shell drew
/// them. The compositor adds no hairline of its own around a window the
/// desk draws whole.
#[test]
fn two_windows_wear_the_desks_edge_once_each() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    let (_terminal, _files, [first, last]) = two_windows(&mut session);
    let theme = ui::theme::Theme::dark();
    let edge_over = |token: ui::peniko::Color, ground: [u8; 4]| -> [i32; 3] {
        let rgba = token.to_rgba8();
        let alpha = f64::from(rgba.a) / 255.0;
        let channel = |over: u8, under: u8| {
            #[allow(
                clippy::cast_possible_truncation,
                reason = "a colour channel of a rendered screen"
            )]
            {
                (f64::from(over) * alpha + f64::from(under) * (1.0 - alpha)).round() as i32
            }
        };
        [
            channel(rgba.r, ground[0]),
            channel(rgba.g, ground[1]),
            channel(rgba.b, ground[2]),
        ]
    };
    let shot = session.compositor().screenshot("HEADLESS-1").expect("shot");
    let at = |x: i32, y: i32| -> [u8; 4] {
        #[allow(
            clippy::cast_sign_loss,
            reason = "a pixel of the screen, above and right of zero"
        )]
        shot.pixel(x as u32, y as u32)
            .expect("a pixel of the screen")
    };
    let mut bright = 0;
    for (what, rect) in [("the first", first), ("the last", last)] {
        let row = rect.1 + rect.3 / 2;
        let ring = at(rect.0 - 1, row);
        let ring = [i32::from(ring[0]), i32::from(ring[1]), i32::from(ring[2])];
        let ground = at(rect.0 - 3, row);
        let dim = edge_over(theme.window_edge(), ground);
        let focused = edge_over(theme.tokens.colors.window_edge_focused(), ground);
        let near = |expected: [i32; 3]| (0..3).all(|i| (ring[i] - expected[i]).abs() <= 4);
        assert!(
            near(dim) || near(focused),
            "{what} window's edge is one coat of the desk's edge colour: {ring:?}, \
             not the dim one {dim:?} or the bright one {focused:?} (ground {ground:?})"
        );
        if near(focused) {
            bright += 1;
        }
    }
    assert_eq!(bright, 1, "one window is in use, and its edge says so");
}
