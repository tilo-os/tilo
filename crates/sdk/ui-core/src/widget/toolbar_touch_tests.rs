//! A toolbar a finger reaches.

use super::*;
use crate::icons::Icon;
use crate::interaction::InteractionEnvironment;

/// Told that a finger reaches it, every command of the row is a finger's target the next time
/// it is laid out, and told of a pointer again, a pointer's.
#[test]
fn on_touch_every_command_is_a_fingers_target() {
    let _settled = crate::motion::testing::settled();
    let mut tb = Toolbar::new(Orientation::Horizontal)
        .push_icon(Icon::Pencil, "Bold", || {})
        .push_icon(Icon::Pin, "Pin", || {});
    let theme = Theme::dark();
    let mut text = TextRenderer::new();
    let lay = |tb: &mut Toolbar, text: &mut TextRenderer| {
        tb.lay_out_in(Rect::new(0.0, 0.0, 400.0, 80.0), text, &theme);
        (0..2)
            .map(|index| tb.item_rect(index).expect("each command shows"))
            .collect::<Vec<_>>()
    };
    let pointer = f64::from(InteractionEnvironment::default().minimum_target());
    for rect in lay(&mut tb, &mut text) {
        assert!(
            (rect.width() - pointer).abs() < 0.5,
            "a pointer's {pointer}: {rect:?}"
        );
    }
    Widget::set_interaction_environment(&mut tb, InteractionEnvironment::TOUCH);
    let finger = f64::from(InteractionEnvironment::TOUCH.minimum_target());
    for rect in lay(&mut tb, &mut text) {
        assert!(
            rect.width() >= finger - 0.5 && rect.height() >= finger - 0.5,
            "a finger's {finger}: {rect:?}"
        );
    }
    Widget::set_interaction_environment(&mut tb, InteractionEnvironment::default());
    for rect in lay(&mut tb, &mut text) {
        assert!(
            (rect.width() - pointer).abs() < 0.5,
            "a pointer's again: {rect:?}"
        );
    }
}
