use crate::compositor::backend::headless::output_fixture;
use crate::compositor::lock_screen::LockLayout;
use crate::compositor::lock_screen::lock_outputs;

#[test]
fn the_lock_screen_puts_the_password_field_on_the_focused_display() {
    let outputs = output_fixture("laptop-beside-1080p").expect("fixture");
    let theme = ui::theme::Theme::dark();
    for focused in [&outputs[1], &outputs[0]] {
        let lock = lock_outputs(&outputs, (0.0, 0.0), Some(focused.id.0));
        assert_eq!(lock.len(), outputs.len(), "every display is covered");
        let shown = focused.scene_rect();
        let password = LockLayout::for_output(lock[0], &theme).password;
        assert!(
            password.area() > 0.0 && shown.union(password) == shown,
            "the password field {password:?} is on the focused display {shown:?}"
        );
    }
    let unfocused = lock_outputs(&outputs, (0.0, 0.0), None);
    assert_eq!(
        unfocused[0].origin,
        (
            f64::from(outputs[0].position.0),
            f64::from(outputs[0].position.1)
        ),
        "without a focused display the primary carries it"
    );
}

#[test]
fn without_focus_the_built_in_panel_carries_the_password_at_its_own_scale() {
    let mut outputs = output_fixture("laptop-beside-1080p").expect("fixture");
    // The built-in panel listed second, as a dock can report it.
    outputs.swap(0, 1);
    outputs[0].identity.connector = "DP-2".into();
    outputs[1].identity.connector = "eDP-1".into();
    let theme = ui::theme::Theme::dark();
    let lock = lock_outputs(&outputs, (0.0, 0.0), None);
    assert_eq!(lock.len(), 2, "one lock surface per display");
    let builtin = outputs[1].scene_rect();
    let password = LockLayout::for_output(lock[0], &theme).password;
    assert!(
        builtin.union(password) == builtin,
        "the password field {password:?} is on the built-in panel {builtin:?}"
    );
    // The 2x panel draws the card at twice the 1x monitor's size.
    let external = LockLayout::for_output(lock[1], &theme);
    let laptop = LockLayout::for_output(lock[0], &theme);
    assert!(
        laptop.scale > external.scale * 1.5,
        "the card follows its display's scale: {} on 2x vs {} on 1x",
        laptop.scale,
        external.scale
    );
    let surfaces = crate::compositor::lock_screen::lock_surfaces(&lock, &theme, false);
    assert_eq!(
        surfaces
            .iter()
            .filter(|surface| surface.password.is_some())
            .count(),
        1,
        "one password field across every display"
    );
    assert!(surfaces[0].password.is_some() && surfaces[1].password.is_none());
}
