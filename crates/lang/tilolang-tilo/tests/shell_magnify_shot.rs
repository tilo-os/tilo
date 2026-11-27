//! Captures of a dock magnifying under the pointer, in both themes, on the foot and on the
//! leading edge: the item under the pointer larger than its neighbours, the run giving it the
//! room.

#![cfg(target_os = "linux")]

use std::collections::HashMap;

use ui::vello::kurbo::{Point, Rect};

use crate::shell_staged::StagedWindow;

/// The directory a capture is written to: the one the check names (`TILO_SHELL_MAGNIFY_SHOTS`),
/// or else beside the test's built binary, as the other shots are, since a source tree read from a
/// test may be read-only.
fn captures() -> std::path::PathBuf {
    if let Some(dir) = std::env::var_os("TILO_SHELL_MAGNIFY_SHOTS") {
        return std::path::PathBuf::from(dir);
    }
    std::env::current_exe()
        .expect("the test's binary")
        .parent()
        .expect("its directory")
        .parent()
        .expect("the profile's directory")
        .parent()
        .expect("the build's directory")
        .join("shell-magnify-shot")
}

/// The dock's tiles among `targets`: the magnified items standing at the dock's edge, leading
/// first — along the foot for `foot`, down the leading edge otherwise.
fn tiles(targets: &HashMap<String, Rect>, foot: bool) -> Vec<Rect> {
    let mut out: Vec<Rect> = targets
        .values()
        .copied()
        .filter(|rect| {
            (rect.width() - rect.height()).abs() < 1.5
                && rect.width() > 26.0
                && rect.width() < 160.0
        })
        .filter(|rect| {
            if foot {
                rect.y0 > 600.0
            } else {
                rect.x0 < 160.0 && rect.y0 > 120.0
            }
        })
        .collect();
    // Along the edge, then across it: two items never share a place, so the order a capture is
    // taken in is the dock's own and not the map's.
    out.sort_by(|a, b| {
        if foot {
            a.x0.total_cmp(&b.x0).then(a.y0.total_cmp(&b.y0))
        } else {
            a.y0.total_cmp(&b.y0).then(a.x0.total_cmp(&b.x0))
        }
    });
    // One rect per item: an item and its wrapper stand at the same box, and a glyph or a count
    // inside one is no item of the dock.
    out.dedup();
    let items = out.clone();
    out.retain(|rect| {
        !items.iter().any(|other| {
            other != rect
                && other.x0 <= rect.x0
                && other.y0 <= rect.y0
                && other.x1 >= rect.x1
                && other.y1 >= rect.y1
                && (other.width() > rect.width() + 0.5 || other.height() > rect.height() + 0.5)
        })
    });
    out
}

/// The tile whose middle stands nearest `at`: the one under the pointer.
fn nearest(tiles: &[Rect], at: Point) -> Option<Rect> {
    let mut best: Option<(f64, Rect)> = None;
    for rect in tiles {
        let distance = rect.center().distance(at);
        if best.is_none_or(|(near, _)| distance < near) {
            best = Some((distance, *rect));
        }
    }
    best.map(|(_, rect)| rect)
}

/// Whether the tile under the pointer is the widest, and wider than the ones away from it.
fn grew_in_place(tiles: &[Rect], at: Point) -> bool {
    let widest = tiles.iter().map(Rect::width).fold(0.0, f64::max);
    let narrowest = tiles.iter().map(Rect::width).fold(f64::INFINITY, f64::min);
    nearest(tiles, at).is_some_and(|rect| (rect.width() - widest).abs() < 0.5)
        && widest > narrowest + 4.0
}

/// Run frames until the window stands still, as it does a moment after the pointer stops.
fn settle(shell: &mut StagedWindow) {
    assert!(
        shell
            .harness
            .settle_for_capture(ui::time::Duration::from_secs(10)),
        "the window comes to rest"
    );
}

/// Hover the middle tile of `shell`'s dock, with every item at rest first, and answer the tiles
/// as they stand magnified and the point the pointer came to.
fn magnify(shell: &mut StagedWindow, foot: bool) -> (Vec<Rect>, Point) {
    // Away from the dock first, so every item rests before the pointer comes to one: the light
    // capture follows one already magnified.
    shell.harness.hover_at(4.0, 4.0);
    settle(shell);
    let at_rest = tiles(&shell.harness.targets(), foot);
    assert!(
        at_rest.len() >= 3,
        "the dock's tiles are laid out, square and at the edge: {at_rest:?}"
    );
    let centre = at_rest[at_rest.len() / 2].center();
    shell.harness.hover_at(centre.x, centre.y);
    settle(shell);
    (tiles(&shell.harness.targets(), foot), centre)
}

/// `shell`'s dock at the foot, magnified under the pointer, in both themes, saved beside the
/// build.
#[test]
fn a_dock_on_the_foot_magnifies_under_the_pointer() {
    let mut dark = tilo_ui_base::Theme::dark();
    dark.reduce_motion = false;
    let mut shell = StagedWindow::staged("the traditional desktop", (1280, 800), dark);
    let (grown, centre) = magnify(&mut shell, true);
    assert!(
        grew_in_place(&grown, centre),
        "the item under the pointer is the one that grew, larger than the items away from it: \
         {grown:?}"
    );
    let out = captures();
    std::fs::create_dir_all(&out).expect("the captures' directory");
    shell
        .harness
        .render_png(&out.join("dock-bottom-magnified-dark.png"));

    let mut light = tilo_ui_base::Theme::light();
    light.reduce_motion = false;
    shell
        .harness
        .relayout_themed((1280, 800), light)
        .expect("the light frame");
    let (grown, centre) = magnify(&mut shell, true);
    assert!(
        grew_in_place(&grown, centre),
        "the item under the pointer grew in the light theme too: {grown:?}"
    );
    shell
        .harness
        .render_png(&out.join("dock-bottom-magnified-light.png"));
    for theme in ["dark", "light"] {
        let path = out.join(format!("dock-bottom-magnified-{theme}.png"));
        assert!(
            std::fs::metadata(&path).is_ok_and(|held| held.len() > 0),
            "the {theme} capture is written: {}",
            path.display()
        );
    }
}

/// A dock on the leading edge magnifies the same way, standing up the edge, in both themes.
#[test]
fn a_dock_on_the_leading_edge_magnifies_under_the_pointer() {
    let mut dark = tilo_ui_base::Theme::dark();
    dark.reduce_motion = false;
    let mut shell = StagedWindow::staged("the dock on the left", (1280, 800), dark);
    let (grown, centre) = magnify(&mut shell, false);
    assert!(
        grew_in_place(&grown, centre),
        "the item under the pointer is the one that grew, larger than the items away from it: \
         {grown:?}"
    );
    let out = captures();
    std::fs::create_dir_all(&out).expect("the captures' directory");
    shell
        .harness
        .render_png(&out.join("dock-left-magnified-dark.png"));

    let mut light = tilo_ui_base::Theme::light();
    light.reduce_motion = false;
    shell
        .harness
        .relayout_themed((1280, 800), light)
        .expect("the light frame");
    let (grown, centre) = magnify(&mut shell, false);
    assert!(
        grew_in_place(&grown, centre),
        "the item under the pointer grew in the light theme too: {grown:?}"
    );
    shell
        .harness
        .render_png(&out.join("dock-left-magnified-light.png"));
    for theme in ["dark", "light"] {
        let path = out.join(format!("dock-left-magnified-{theme}.png"));
        assert!(
            std::fs::metadata(&path).is_ok_and(|held| held.len() > 0),
            "the {theme} capture is written: {}",
            path.display()
        );
    }
}

/// The dock keeps the room it rests in: as an item magnifies, the windows over the band stay
/// exactly where they were.
#[test]
fn the_windows_do_not_move_when_the_dock_magnifies() {
    let mut dark = tilo_ui_base::Theme::dark();
    dark.reduce_motion = false;
    let mut shell = StagedWindow::staged("the traditional desktop", (1280, 800), dark);
    shell.harness.hover_at(4.0, 4.0);
    settle(&mut shell);
    let before = shell.harness.targets();
    let rest = tiles(&before, true);
    assert!(rest.len() >= 3, "the dock's tiles are laid out: {rest:?}");
    let centre = rest[rest.len() / 2].center();
    let windows: Vec<(&String, Rect)> = before
        .iter()
        .filter(|(name, _)| name.contains("WindowFrame"))
        .map(|(name, rect)| (name, *rect))
        .collect();
    assert!(
        !windows.is_empty(),
        "the staged desktop's windows are addressable: {:?}",
        before.keys().collect::<Vec<_>>()
    );
    shell.harness.hover_at(centre.x, centre.y);
    settle(&mut shell);
    let after = shell.harness.targets();
    let grown = tiles(&after, true);
    assert!(
        grew_in_place(&grown, centre),
        "the dock magnified: {grown:?}"
    );
    for (name, rect) in windows {
        let now = after.get(name).copied().unwrap_or(Rect::ZERO);
        assert!(
            (now.x0 - rect.x0).abs() < 0.5
                && (now.y0 - rect.y0).abs() < 0.5
                && (now.x1 - rect.x1).abs() < 0.5
                && (now.y1 - rect.y1).abs() < 0.5,
            "{name} stayed where it was: {now:?} against {rect:?}"
        );
    }
}

/// The dock's items stand on the edge it rests on. As one magnifies, the others keep that floor:
/// the run grows away from it, so no tile lifts off the body's edge and no badge crosses it.
#[test]
fn the_docks_items_keep_its_floor_while_one_magnifies() {
    for (scenario, foot) in [
        ("the traditional desktop", true),
        ("the dock on the left", false),
    ] {
        let mut theme = tilo_ui_base::Theme::dark();
        theme.reduce_motion = false;
        let mut shell = StagedWindow::staged(scenario, (1280, 800), theme);
        shell.harness.hover_at(4.0, 4.0);
        settle(&mut shell);
        let at_rest = tiles(&shell.harness.targets(), foot);
        assert!(
            at_rest.len() >= 3,
            "{scenario}: the dock's tiles are laid out: {at_rest:?}"
        );
        // The edge they stand on: their feet along the bottom, their leading side up an edge.
        let floor = if foot {
            let floor = at_rest[0].y1;
            for rect in &at_rest {
                assert!(
                    (rect.y1 - floor).abs() < 0.5,
                    "{scenario}: the tiles rest on one floor: {at_rest:?}"
                );
            }
            floor
        } else {
            let floor = at_rest[0].x0;
            for rect in &at_rest {
                assert!(
                    (rect.x0 - floor).abs() < 0.5,
                    "{scenario}: the tiles rest on one floor: {at_rest:?}"
                );
            }
            floor
        };
        let centre = at_rest[at_rest.len() / 2].center();
        shell.harness.hover_at(centre.x, centre.y);
        settle(&mut shell);
        let grown = tiles(&shell.harness.targets(), foot);
        assert!(
            grew_in_place(&grown, centre),
            "{scenario}: the dock magnified: {grown:?}"
        );
        // A tile's own height rounds to the pixel, so the edge itself may land within a pixel of
        // the floor; what it may not do is leave it, as a centred run lifts every other tile.
        for rect in &grown {
            let off = if foot {
                rect.y1 - floor
            } else {
                rect.x0 - floor
            };
            assert!(
                off.abs() <= 1.5,
                "{scenario}: {rect:?} keeps the floor at {floor}: {grown:?}"
            );
        }
    }
}
