//! What the strip pan and tab drag recording is a recording of.
//!
//! The recording is the deliverable; this keeps it honest. The frames are
//! only worth watching if the drag really moves a tab from one window into
//! another that is already on the strip, and the pan really travels and
//! returns — so each of those is asserted from the live strip the scene
//! leaves behind, not from the picture.

use super::*;

/// The strip as each snapshot the recording takes finds it: how many tabs
/// each window on it holds, and how far it has been scrolled.
struct StripAt {
    /// One entry per window, in strip order, at each snapshot.
    shapes: Vec<(String, Vec<usize>)>,
    scrolls: Vec<(String, f64)>,
}

impl StripAt {
    fn shape(&self, name: &str) -> &[usize] {
        match self.shapes.iter().find(|(seen, ..)| seen == name) {
            Some((_, counts)) => counts,
            None => panic!("the recording should snapshot {name}"),
        }
    }

    fn scroll(&self, name: &str) -> f64 {
        match self.scrolls.iter().find(|(seen, _)| seen == name) {
            Some((_, scroll_x)) => *scroll_x,
            None => panic!("the recording should snapshot {name}"),
        }
    }
}

impl SnapshotSink for StripAt {
    fn capture(&mut self, driver: &mut ShellDriver, name: &str) {
        let strip_id = {
            let state = driver.chrome();
            let tree = state.tree();
            tree.desktop
                .active_workspace()
                .and_then(|workspace| workspace.strips.first().map(|strip| strip.id))
        };
        let Some(strip_id) = strip_id else {
            return;
        };
        // Walk the windows until the strip runs out of them, so the shape is
        // read from the live strip rather than assumed.
        let mut tab_counts = Vec::new();
        for index in 0..8 {
            let Some(group) = driver.host_mut().group_at_for_tests(strip_id, index) else {
                break;
            };
            tab_counts.push(group.tab_count());
        }
        let scroll_x = driver
            .host_mut()
            .group_row_mut_for_tests(strip_id)
            .map_or(f64::NAN, |row| row.scroll_x());
        self.shapes.push((name.to_string(), tab_counts));
        self.scrolls.push((name.to_string(), scroll_x));
    }
}

/// The drag has to move a tab out of one window and into a window that is
/// already on the strip, and leave every other window's tabs alone.
fn assert_the_drag_moved_one_tab(strip: &StripAt) {
    let before = strip.shape("01-files-two-tabs");
    assert!(
        before.len() >= 3,
        "the recording needs three windows on the strip to pan across, saw {before:?}"
    );
    let Some(dragged_from) = before.iter().position(|count| *count == 2) else {
        panic!("one window must hold two tabs to drag one out of it: {before:?}");
    };
    assert_eq!(
        before.iter().filter(|count| **count == 2).count(),
        1,
        "exactly one window grows a second tab: {before:?}"
    );

    let after = strip.shape("06-tab-in-the-next-window");
    assert_eq!(
        after.iter().sum::<usize>(),
        before.iter().sum::<usize>(),
        "the drag moves a tab, it does not open or close one: {before:?} -> {after:?}"
    );
    assert_eq!(
        after.len(),
        before.len(),
        "the tab lands in a window already on the strip instead of becoming one of its \
         own: {before:?} -> {after:?}"
    );
    assert_eq!(
        after[dragged_from], 1,
        "the window the tab was dragged out of is left with the one it had: {after:?}"
    );
    assert!(
        after.contains(&2),
        "the tab arrives beside the tabs of the window it was dropped on: {after:?}"
    );
}

/// The pan has to travel away from where the strip rests and come back.
fn assert_the_pan_travelled_and_returned(strip: &StripAt) {
    let rest = strip.scroll("06-tab-in-the-next-window");
    let panned = strip.scroll("08-panned-to-the-last-window");
    let home = strip.scroll("10-settled-back-home");
    assert!(
        panned > rest + 1.0,
        "the pan has to travel past the window it started at (rest = {rest}, panned = {panned})"
    );
    assert!(
        (home - rest).abs() < 1.0,
        "the pan back has to return the strip where it started (rest = {rest}, home = {home})"
    );
}

/// What the pan-and-drag recording has to be a recording of: a tab leaving
/// one window's bar and becoming a second tab of another window already on
/// the strip, never a window of its own, and the strip both panning away
/// and coming home.
#[test]
fn strip_pan_and_tab_drag_moves_a_tab_between_two_windows() {
    let mut driver = ShellDriver::new((1600, 900));
    let mut strip = StripAt {
        shapes: Vec::new(),
        scrolls: Vec::new(),
    };
    run(
        &mut driver,
        &lookup("strip_pan_and_tab_drag").expect("strip_pan_and_tab_drag in catalogue"),
        &mut strip,
    );

    assert_the_drag_moved_one_tab(&strip);
    assert_the_pan_travelled_and_returned(&strip);
}
