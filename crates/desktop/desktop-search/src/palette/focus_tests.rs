//! The shell opens and closes its search: the chord or a choice.
//! The keyboard's focus moving elsewhere does not.
use super::*;

/// The frame takes the keyboard's focus off the card in the pass the search opened in — a window's
/// field focused again under load, as the search chord arrives over it — and the search stays up.
#[test]
fn a_focus_moving_off_the_card_leaves_the_search_open() {
    let mut search = DesktopSearch::new();
    search.open(super::tests::data());
    tilo_ui_base::widget::Widget::set_focus(search.palette_mut(), false);
    assert!(
        !search.observe(),
        "the search the chord opened ended with the keyboard's focus moving off its card"
    );
    assert!(search.is_open(), "the search stands after the focus moved");
}
