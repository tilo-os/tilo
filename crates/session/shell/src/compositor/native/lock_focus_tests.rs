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

use std::sync::Arc;

use crate::compositor::lock_auth::FixedVerifier;
use crate::compositor::lock_controller::{LockController, LockKey};
use crate::compositor::lock_screen;
use crate::compositor::shell_text_input::{NativeFieldId, field_owner};
use ui::ime::{ImeWindowState, InputContent};

fn field(tag: u32) -> ImeWindowState {
    ImeWindowState {
        content: InputContent::TEXT,
        x: tag,
        y: 0,
        width: 1,
        height: 20,
    }
}

/// The lock may serve one field and nothing else. The cases are the whole
/// truth table of the rule, not examples of it.
#[test]
fn the_input_method_serves_only_the_lock_field_while_locked() {
    let lock = ImeWindowState {
        content: InputContent::PASSWORD,
        x: 10,
        y: 20,
        width: 300,
        height: 50,
    };
    // Locked: a chrome field, a prompt, the palette and a focused widget are
    // all irrelevant — the lock field is the only answer.
    assert_eq!(
        field_owner(
            true,
            false,
            Some(lock),
            true,
            false,
            true,
            Some(field(1)),
            Some(2),
            true,
        ),
        Some((lock, NativeFieldId::Lock)),
        "while locked only the lock field is served, whatever chrome asks"
    );
    // A client that still holds the keyboard wins nothing while locked.
    assert_eq!(
        field_owner(
            true,
            true,
            Some(lock),
            true,
            false,
            true,
            Some(field(1)),
            Some(2),
            true,
        ),
        None,
        "while a client holds the keyboard the lock serves no input method"
    );
    // A frozen field (verifying) or a display without one serves nobody.
    assert_eq!(
        field_owner(
            true,
            false,
            None,
            true,
            false,
            true,
            Some(field(1)),
            Some(2),
            true
        ),
        None
    );
    // Unlocked: the lock identity is never produced, even if one is offered.
    assert_eq!(
        field_owner(
            false,
            false,
            Some(lock),
            false,
            false,
            false,
            Some(field(7)),
            Some(9),
            false,
        ),
        Some((field(7), NativeFieldId::Widget(9))),
        "the ordinary focused field is served once unlocked"
    );
    // Unlocked with a client holding the keyboard: no shell field at all.
    assert_eq!(
        field_owner(
            false,
            true,
            Some(lock),
            false,
            false,
            false,
            Some(field(7)),
            Some(9),
            false,
        ),
        None
    );
    // A shell password prompt takes the keyboard over the client beneath it.
    assert_eq!(
        field_owner(
            false,
            true,
            Some(lock),
            true,
            false,
            false,
            Some(field(3)),
            Some(9),
            false,
        ),
        Some((field(3), NativeFieldId::Prompt))
    );
    // The palette owns the input method while it is open.
    assert_eq!(
        field_owner(
            false,
            false,
            Some(lock),
            false,
            false,
            false,
            Some(field(4)),
            Some(9),
            true,
        ),
        Some((field(4), NativeFieldId::Palette))
    );
    // A field that only takes deferred receipts needs dictation to want it.
    assert_eq!(
        field_owner(
            false,
            false,
            Some(lock),
            false,
            true,
            false,
            Some(field(5)),
            Some(9),
            false,
        ),
        None,
        "an ordinary commit must not be served to a deferred-only field"
    );
    assert_eq!(
        field_owner(
            false,
            false,
            Some(lock),
            false,
            true,
            true,
            Some(field(5)),
            Some(9),
            false,
        ),
        Some((field(5), NativeFieldId::Widget(9))),
        "dictation may still take the original field"
    );
}

/// The caret rectangle handed to the input method is the password field the
/// lock actually paints, on the display that carries it, and its purpose is
/// the hidden password one.
#[test]
fn the_lock_input_field_is_the_painted_password_field_with_the_password_purpose() {
    let outputs = output_fixture("laptop-beside-1080p").expect("fixture");
    let theme = ui::theme::Theme::dark();
    for keyboard in [false, true] {
        let lock = lock_outputs(&outputs, (0.0, 0.0), None);
        let field = lock_screen::ime_field(&lock, &theme, keyboard).expect("a primary display");
        let layout = LockLayout::for_output_with_keyboard(lock[0], &theme, keyboard);
        assert_eq!(
            field.content,
            InputContent::PASSWORD,
            "hidden and sensitive"
        );
        assert!(field.content.is_secret());
        assert!(field.width > 1 && field.height > 1, "{field:?}");
        for (got, want, label) in [
            (f64::from(field.x), layout.password.x0, "x"),
            (f64::from(field.y), layout.password.y0, "y"),
            (f64::from(field.width), layout.password.width(), "width"),
            (f64::from(field.height), layout.password.height(), "height"),
        ] {
            assert!(
                (got - want).abs() <= 1.0,
                "caret {label} {got} follows the painted field {want}"
            );
        }
        if let Some(keyboard_rect) = layout.keyboard {
            assert!(
                layout.password.y1 <= keyboard_rect.y0,
                "the field stays above the touch keyboard it shares the layout with"
            );
        }
    }
}

/// Composition is secret before it commits: the field shows it as mask
/// marks, never as text, and every ending (commit, clear, lock) drops it.
#[test]
fn a_composition_is_masked_and_cleared_on_commit_focus_loss_and_lock() {
    let mut lock = LockController::new(Arc::new(FixedVerifier::new("pw")), "alice");
    lock.set_preedit("にほ");
    assert_eq!(lock.preedit(), "にほ");
    assert_eq!(lock.buffer_len(), 0, "a preedit is never a credential");
    assert_eq!(lock.mask_len(), 2, "the mask shows the composition");

    // A commit replaces the composition with credential text.
    lock.handle_key(LockKey::Text("你".into()));
    assert_eq!(lock.preedit(), "");
    assert_eq!(lock.buffer_len(), 1);

    // Escape (clear) and a fresh lock both drop a half-typed composition.
    lock.set_preedit("ab");
    lock.handle_key(LockKey::Cancel);
    assert_eq!(lock.preedit(), "");
    lock.set_preedit("ab");
    lock.reset();
    assert_eq!(lock.preedit(), "");
    assert_eq!(lock.buffer_len(), 0);

    // A verify freezes the field, including its composition.
    lock.push_str("pw");
    assert_eq!(
        lock.submit(),
        crate::compositor::lock_controller::UnlockOutcome::StillLocked
    );
    assert!(lock.is_verifying());
    lock.set_preedit("x");
    assert_eq!(lock.preedit(), "", "a frozen field takes no composition");
}

/// An input method's events reach the same `LockKey` route as a physical
/// key: a commit is text, a preedit is composition, a clear is a clear.
#[test]
fn input_method_events_reach_the_lock_route() {
    use crate::shell::input_policy::winit_event_to_lock_key;
    use winit::event::{Ime, WindowEvent};
    assert_eq!(
        winit_event_to_lock_key(&WindowEvent::Ime(Ime::Commit("你".into()))),
        LockKey::Text("你".into())
    );
    assert_eq!(
        winit_event_to_lock_key(&WindowEvent::Ime(Ime::Preedit("に".into(), None))),
        LockKey::Preedit("に".into())
    );
    assert_eq!(
        winit_event_to_lock_key(&WindowEvent::Ime(Ime::Preedit(String::new(), None))),
        LockKey::Preedit(String::new()),
        "an empty preedit must clear the composition"
    );
}
