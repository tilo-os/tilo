//! Markers on a map of the world: which of them stand where two would pile on one another, so a
//! dense quarter keeps the places a map is read for, and the one the person chose above all.

use super::*;

fn marker(
    index: usize,
    kind: MapMarkerKind,
    selected: bool,
    at: (f64, f64),
) -> (usize, MapMarkerKind, bool, Point) {
    (index, kind, selected, Point::new(at.0, at.1))
}

/// Markers further apart than the gap all stand.
#[test]
fn markers_apart_all_stand() {
    let markers = [
        marker(0, MapMarkerKind::Cafe, false, (0.0, 0.0)),
        marker(1, MapMarkerKind::Cafe, false, (40.0, 0.0)),
        marker(2, MapMarkerKind::Cafe, false, (0.0, 40.0)),
    ];
    let kept: Vec<usize> = standing_markers(&markers)
        .into_iter()
        .map(|(index, _)| index)
        .collect();
    assert_eq!(kept, [0, 1, 2], "each has room of its own");
}

/// Two markers on one another keep the more salient kind, whatever order the view gave them in.
#[test]
fn the_more_salient_marker_keeps_a_contested_place() {
    let markers = [
        marker(0, MapMarkerKind::Shop, false, (0.0, 0.0)),
        marker(1, MapMarkerKind::Landmark, false, (6.0, 0.0)),
    ];
    let kept: Vec<usize> = standing_markers(&markers)
        .into_iter()
        .map(|(index, _)| index)
        .collect();
    assert_eq!(kept, [1], "the landmark stands and the shop waits");
}

/// The marker the person chose stands however close another is.
#[test]
fn the_chosen_marker_always_stands() {
    let markers = [
        marker(0, MapMarkerKind::Shop, true, (0.0, 0.0)),
        marker(1, MapMarkerKind::Landmark, false, (6.0, 0.0)),
    ];
    let kept: Vec<usize> = standing_markers(&markers)
        .into_iter()
        .map(|(index, _)| index)
        .collect();
    assert_eq!(kept, [0], "the chosen shop stands over the landmark");
}

/// The kept markers come back in the order the view gave them, so the map draws them as it lists
/// them and a press finds the topmost one as it drew it.
#[test]
fn kept_markers_keep_the_views_order() {
    let markers = [
        marker(0, MapMarkerKind::Shop, false, (0.0, 0.0)),
        marker(1, MapMarkerKind::Landmark, false, (6.0, 0.0)),
        marker(2, MapMarkerKind::Cafe, false, (80.0, 0.0)),
    ];
    let kept: Vec<usize> = standing_markers(&markers)
        .into_iter()
        .map(|(index, _)| index)
        .collect();
    assert_eq!(kept, [1, 2], "the landmark before the café it keeps");
}
