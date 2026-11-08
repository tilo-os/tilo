//! Two windows on one screen: where the strip puts them.

use super::*;

/// A window's place on the screen: its corner and its size.
type Placed = (i32, i32, i32, i32);

/// Two windows on one screen, the second opened beside the first: the session settles on the width
/// they share, and the window that was open first draws itself in it, as a person's app does when
/// the desk resizes it. Returns the two windows' clients — closing them closes the windows — and
/// their rectangles, in the order they stand in.
fn two_windows(session: &mut Session) -> (TestClient, TestClient, [Placed; 2]) {
    let mut terminal = mapped(session, "org.example.Terminal");
    let files = mapped(session, "so.tilo.file-explorer");
    // The window that opened second takes half the strip from the first, on the width's own clock.
    let mut at = ui::time::now();
    for _ in 0..40 {
        at = at
            .checked_add(Duration::from_millis(16))
            .expect("a frame further");
        ui::time::testing::with_monotonic_time(at, || {
            session.step(Duration::from_millis(1)).expect("step");
        });
    }
    // The window that was open first is handed the width it now shares the strip at, and draws
    // itself in it.
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

/// Where this test's captures are written.
fn shots_dir() -> std::path::PathBuf {
    std::env::var_os("STRIP_SHOTS_OUT").map_or_else(
        || {
            let home = std::env::var("HOME").expect("HOME");
            std::path::PathBuf::from(home).join(".cache/visual-compare/strip")
        },
        std::path::PathBuf::from,
    )
}

/// Two windows on one screen stand on the desktop's wallpaper, not in its corners: the first keeps
/// the gutter past the leading edge, the last the one before the trailing edge, they share what
/// lies between, and both start a step below the bar.
#[track_caller]
fn assert_pair_stands_in_the_strip([first, last]: [Placed; 2]) {
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
    assert!(
        WIDTH - gutter - (last.0 + last.2) < 2 && first.0 - gutter < 2,
        "the pair fills the strip's usable width, both gutters standing: \
         {first:?} and {last:?}"
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

#[test]
fn two_windows_keep_the_strips_gutters_and_stand_below_the_bar() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    let (_terminal, _files, rects) = two_windows(&mut session);
    assert_pair_stands_in_the_strip(rects);
    super::desk::capture_to(&mut session, &shots_dir(), "two-windows-dark");
}

#[test]
fn two_windows_stand_the_same_way_in_the_light_theme() {
    let _settled = ui::motion::testing::settled();
    let mut session = super::desk::light_session();
    let (_terminal, _files, rects) = two_windows(&mut session);
    assert_pair_stands_in_the_strip(rects);
    super::desk::capture_to(&mut session, &shots_dir(), "two-windows-light");
}
