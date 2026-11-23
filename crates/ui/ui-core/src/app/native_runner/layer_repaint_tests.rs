use super::layer_repaints;

/// A menu or box drawn over the widgets repaints the window while it stands and once more as
/// it goes, so its pixels are erased; then frames go back to the widgets' damage.
#[test]
fn the_frame_after_a_layer_erases_it() {
    let mut drawn_last = false;
    assert!(
        !layer_repaints(&mut drawn_last, false),
        "nothing over the widgets"
    );
    assert!(layer_repaints(&mut drawn_last, true), "the box opens");
    assert!(layer_repaints(&mut drawn_last, true), "the box stands");
    assert!(
        layer_repaints(&mut drawn_last, false),
        "the box went: erase it"
    );
    assert!(!layer_repaints(&mut drawn_last, false), "it is gone");
}
