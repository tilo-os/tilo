use super::*;
use crate::synthetic_input::{char_key, cursor_moved, mouse_press, mouse_release, named_key};
use crate::testing::{Dispatch, dispatch};
use winit::keyboard::ModifiersState;

fn typed(prompt: &mut SecretPrompt, text: &str) {
    for c in text.chars() {
        dispatch(prompt, &char_key(&c.to_string()));
    }
}

fn focused(kind: SecretKind) -> SecretPrompt {
    let mut prompt = SecretPrompt::new(kind)
        .with_title("Connect to TruffleBoys2026")
        .with_resource("WPA2 Personal")
        .with_primary_label("Connect");
    Widget::set_focus(&mut prompt, true);
    prompt
}

fn placed(prompt: &mut SecretPrompt, window: Rect) {
    let mut text = TextRenderer::new();
    prompt.place(window, &mut text, &Theme::dark());
}

fn touch(phase: TouchPhase, id: u64, at: Point) -> WindowEvent {
    WindowEvent::Touch(winit::event::Touch {
        device_id: winit::event::DeviceId::dummy(),
        phase,
        location: winit::dpi::PhysicalPosition::new(at.x, at.y),
        force: None,
        id,
    })
}

fn submitted(prompt: &mut SecretPrompt) -> SecretAnswer {
    match prompt.take_outcome() {
        Some(PromptOutcome::Submit(answer)) => answer,
        other => panic!("expected an answer, got {other:?}"),
    }
}

#[test]
fn focus_starts_in_the_first_field_that_needs_typing() {
    let prompt = focused(SecretKind::Secret);
    assert_eq!(prompt.focused_field(), Some(FieldRole::Secret));
    assert!(
        prompt
            .field(FieldRole::Secret)
            .is_some_and(TextInput::is_focused)
    );

    let mut login = SecretPrompt::new(SecretKind::UsernameAndSecret);
    Widget::set_focus(&mut login, true);
    assert_eq!(login.focused_field(), Some(FieldRole::Username));

    let mut known = SecretPrompt::new(SecretKind::UsernameAndSecret).with_username("quinn");
    Widget::set_focus(&mut known, true);
    assert_eq!(
        known.focused_field(),
        Some(FieldRole::Secret),
        "a known user name leaves only the secret to type"
    );
}

#[test]
fn enter_answers_with_the_secret_and_escape_cancels() {
    let mut prompt = focused(SecretKind::Secret);
    typed(&mut prompt, "hunter22");
    assert!(
        prompt.take_outcome().is_none(),
        "nothing is answered while typing"
    );
    assert!(dispatch(&mut prompt, &named_key(NamedKey::Enter)));
    let answer = submitted(&mut prompt);
    assert_eq!(answer.secret.as_str(), "hunter22");
    assert_eq!(answer.username, None);

    let mut prompt = focused(SecretKind::Secret);
    typed(&mut prompt, "abc");
    assert!(dispatch(&mut prompt, &named_key(NamedKey::Escape)));
    assert!(matches!(prompt.take_outcome(), Some(PromptOutcome::Cancel)));
    assert!(
        prompt
            .field(FieldRole::Secret)
            .is_some_and(|field| field.value().is_empty()),
        "cancelling wipes what was typed"
    );
}

#[test]
fn enter_with_nothing_typed_answers_nothing() {
    let mut prompt = focused(SecretKind::Secret);
    dispatch(&mut prompt, &named_key(NamedKey::Enter));
    assert!(prompt.take_outcome().is_none());
    assert!(!prompt.can_submit());
}

#[test]
fn enter_in_the_user_name_moves_on_to_the_secret() {
    let mut prompt = focused(SecretKind::UsernameAndSecret);
    typed(&mut prompt, "quinn");
    dispatch(&mut prompt, &named_key(NamedKey::Enter));
    assert!(prompt.take_outcome().is_none());
    assert_eq!(prompt.focused_field(), Some(FieldRole::Secret));
    typed(&mut prompt, "pw");
    dispatch(&mut prompt, &named_key(NamedKey::Enter));
    let answer = submitted(&mut prompt);
    assert_eq!(answer.username.as_deref(), Some("quinn"));
    assert_eq!(answer.secret.as_str(), "pw");
}

#[test]
fn tab_walks_the_fields_and_the_buttons_and_shift_tab_walks_back() {
    let mut prompt = focused(SecretKind::UsernameAndSecret);
    typed(&mut prompt, "quinn");
    dispatch(&mut prompt, &named_key(NamedKey::Tab));
    assert_eq!(prompt.focused_field(), Some(FieldRole::Secret));
    dispatch(&mut prompt, &named_key(NamedKey::Tab));
    assert_eq!(prompt.focused_field(), None, "on to Cancel");
    assert!(
        prompt.fields().all(|(_, field)| !field.is_focused()),
        "no field types while a button has focus"
    );
    Dispatch::new()
        .modifiers(ModifiersState::SHIFT)
        .send(&mut prompt, &named_key(NamedKey::Tab));
    assert_eq!(prompt.focused_field(), Some(FieldRole::Secret));
}

#[test]
fn a_rejection_keeps_focus_in_the_field_and_selects_its_text() {
    let mut prompt = focused(SecretKind::Secret);
    typed(&mut prompt, "wrong-one");
    dispatch(&mut prompt, &named_key(NamedKey::Enter));
    let _ = submitted(&mut prompt);
    prompt.set_busy(Some("Connecting…".into()));
    typed(&mut prompt, "x");
    assert_eq!(
        prompt.field(FieldRole::Secret).map(TextInput::value),
        Some("wrong-one"),
        "a busy prompt takes no typing"
    );

    prompt.reject("Wrong password");
    assert_eq!(prompt.error(), Some("Wrong password"));
    assert_eq!(prompt.busy(), None);
    let field = prompt.field(FieldRole::Secret).expect("secret field");
    assert!(field.is_focused(), "the field keeps the keyboard");
    assert_eq!(
        field.selection_range_public(),
        Some((0, "wrong-one".len())),
        "the wrong text is selected"
    );
    assert_eq!(
        prompt.notice_line().map(|(_, tone)| tone),
        Some(NoticeTone::Error)
    );

    typed(&mut prompt, "r");
    assert_eq!(
        prompt.field(FieldRole::Secret).map(TextInput::value),
        Some("r"),
        "the next keystroke replaces the wrong text"
    );
    assert_eq!(prompt.error(), None, "typing makes the old error moot");
}

#[test]
fn show_hide_reveals_only_the_paint_never_the_accessible_value() {
    let mut prompt = focused(SecretKind::Secret);
    typed(&mut prompt, "hunter22");
    let field = prompt.field(FieldRole::Secret).expect("secret field");
    assert!(field.is_secret() && !field.is_revealed());
    assert!(field.has_reveal_control(), "a real show/hide button");

    let mut doc = automerge::AutoCommit::new();
    let root = automerge::ROOT;
    let mut edits = crate::widget::Edits::lend(&mut doc);
    assert!(prompt.perform_access_action(
        &crate::a11y::Action::Activate,
        &[FIELD_KEY, REVEAL_KEY],
        &mut edits,
        &root,
    ));
    assert!(
        prompt
            .field(FieldRole::Secret)
            .is_some_and(TextInput::is_revealed)
    );
    let node = prompt.access_node();
    let field = node
        .children
        .iter()
        .find(|child| child.role == Role::TextInput)
        .expect("field node");
    assert!(field.state.protected);
    assert_eq!(field.value, None, "the value never reaches the tree");
    assert_eq!(
        field.children.first().map(|c| c.role),
        Some(Role::ToggleButton)
    );
}

#[test]
fn the_pin_takes_only_digits_up_to_its_longest() {
    let mut prompt = focused(SecretKind::Pin { min: 4, max: 6 });
    typed(&mut prompt, "12a3-4b");
    assert_eq!(
        prompt.field(FieldRole::Pin).map(TextInput::value),
        Some("1234")
    );
    typed(&mut prompt, "56789");
    assert_eq!(
        prompt.field(FieldRole::Pin).map(TextInput::value),
        Some("123456"),
        "a PIN never grows past its longest"
    );
    let ime = prompt
        .field(FieldRole::Pin)
        .and_then(Widget::ime_content)
        .expect("ime content");
    assert!(ime.is_secret(), "the input method is told it is a secret");
    assert_eq!(
        ime.purpose,
        crate::ime::InputPurpose::Pin,
        "and offers digits"
    );
}

#[test]
fn a_short_pin_is_refused_in_place() {
    let mut prompt = focused(SecretKind::SimPin {
        attempts_left: Some(3),
    });
    assert_eq!(
        prompt.notice_line().map(|(line, _)| line),
        Some(crate::i18n!("secret-prompt-attempts-left", "count" => 3u32))
    );
    typed(&mut prompt, "12");
    dispatch(&mut prompt, &named_key(NamedKey::Enter));
    assert!(prompt.take_outcome().is_none(), "the SIM never sees it");
    assert!(prompt.error().is_some());
    assert!(
        prompt
            .field(FieldRole::Pin)
            .is_some_and(TextInput::is_focused)
    );
    // Too short is completed, not retyped: the caret stays at the end.
    typed(&mut prompt, "34");
    dispatch(&mut prompt, &named_key(NamedKey::Enter));
    assert_eq!(submitted(&mut prompt).secret.as_str(), "1234");
}

#[test]
fn a_puk_asks_for_a_new_pin_twice_and_refuses_a_mismatch() {
    let mut prompt = focused(SecretKind::SimPuk {
        attempts_left: Some(10),
    });
    assert_eq!(
        prompt.fields().map(|(role, _)| role).collect::<Vec<_>>(),
        [FieldRole::Puk, FieldRole::NewPin, FieldRole::ConfirmPin]
    );
    typed(&mut prompt, "12345678");
    dispatch(&mut prompt, &named_key(NamedKey::Enter));
    typed(&mut prompt, "4321");
    dispatch(&mut prompt, &named_key(NamedKey::Enter));
    typed(&mut prompt, "4322");
    dispatch(&mut prompt, &named_key(NamedKey::Enter));
    assert!(prompt.take_outcome().is_none());
    assert_eq!(
        prompt.error(),
        Some(crate::i18n!("secret-prompt-pins-differ").as_str())
    );
    assert_eq!(prompt.focused_field(), Some(FieldRole::ConfirmPin));

    typed(&mut prompt, "4321");
    dispatch(&mut prompt, &named_key(NamedKey::Enter));
    let answer = submitted(&mut prompt);
    assert_eq!(answer.secret.as_str(), "12345678");
    assert_eq!(answer.new_pin.as_deref().map(String::as_str), Some("4321"));
}

#[test]
fn caps_lock_is_noticed_while_typing_a_secret() {
    let mut prompt = focused(SecretKind::Secret);
    typed(&mut prompt, "A");
    assert!(prompt.caps_lock_on());
    assert_eq!(
        prompt.notice_line(),
        Some((crate::i18n!("secret-prompt-caps-lock"), NoticeTone::Warning))
    );
    typed(&mut prompt, "a");
    assert!(!prompt.caps_lock_on());
}

#[test]
fn every_secret_is_zeroized_and_never_printed() {
    fn zeroized_on_drop<T: zeroize::ZeroizeOnDrop>() {}
    zeroized_on_drop::<Zeroizing<String>>();

    let mut prompt = focused(SecretKind::Secret);
    typed(&mut prompt, "hunter22");
    dispatch(&mut prompt, &named_key(NamedKey::Enter));
    let answer = submitted(&mut prompt);
    let shown = format!("{answer:?}");
    assert!(!shown.contains("hunter22"), "{shown}");

    prompt.wipe();
    assert!(
        prompt.fields().all(|(_, field)| field.value().is_empty()),
        "wiping empties every field"
    );
}

#[test]
fn a_dialog_body_leaves_enter_and_escape_to_its_dialog() {
    let mut dialog = SecretPrompt::new(SecretKind::Secret)
        .with_title("Unlock the setup")
        .with_primary_label("Unlock")
        .into_dialog();
    let prompt = dialog.content_mut();
    assert_eq!(prompt.presentation(), Presentation::DialogBody);
    Widget::set_focus(prompt, true);
    typed(prompt, "pw");
    assert!(
        !dispatch(prompt, &named_key(NamedKey::Enter)),
        "the dialog's default action answers"
    );
    assert!(!dispatch(prompt, &named_key(NamedKey::Escape)));
    prompt.submit();
    assert_eq!(submitted(prompt).secret.as_str(), "pw");
}

#[test]
fn a_finger_gets_touch_targets_and_the_pin_keypad() {
    let phone = Rect::new(0.0, 0.0, 390.0, 844.0);
    let mut prompt = focused(SecretKind::SimPin {
        attempts_left: None,
    });
    placed(&mut prompt, phone);
    assert!(
        prompt.layout().keypad.is_empty(),
        "no keypad before a touch"
    );
    let field = prompt.layout().fields[0];
    dispatch(&mut prompt, &touch(TouchPhase::Started, 1, field.center()));
    dispatch(&mut prompt, &touch(TouchPhase::Ended, 1, field.center()));
    assert!(prompt.shows_keypad());
    placed(&mut prompt, phone);

    let layout = prompt.layout().clone();
    assert_eq!(layout.keypad.len(), 12);
    for rect in layout
        .fields
        .iter()
        .chain(layout.keypad.iter().map(|(_, rect)| rect))
        .chain([&layout.cancel, &layout.primary])
    {
        assert!(
            rect.height() >= f64::from(InteractionEnvironment::TOUCH.minimum_target()),
            "{rect:?} is too short for a finger"
        );
    }
    assert!(
        layout
            .modal
            .as_ref()
            .is_some_and(|modal| modal.card == phone),
        "a phone shows the prompt as a full-window page"
    );

    for digit in [1, 2, 3, 4] {
        let (_, key) = layout
            .keypad
            .iter()
            .find(|(key, _)| *key == KeypadKey::Digit(digit))
            .copied()
            .expect("digit key");
        dispatch(&mut prompt, &touch(TouchPhase::Started, 2, key.center()));
        dispatch(&mut prompt, &touch(TouchPhase::Ended, 2, key.center()));
    }
    assert_eq!(
        prompt.field(FieldRole::Pin).map(TextInput::value),
        Some("1234")
    );

    let (_, enter) = layout
        .keypad
        .iter()
        .find(|(key, _)| *key == KeypadKey::Enter)
        .copied()
        .expect("enter key");
    dispatch(&mut prompt, &touch(TouchPhase::Started, 3, enter.center()));
    dispatch(&mut prompt, &touch(TouchPhase::Ended, 3, enter.center()));
    assert_eq!(submitted(&mut prompt).secret.as_str(), "1234");

    dispatch(&mut prompt, &char_key("5"));
    assert!(
        !prompt.shows_keypad(),
        "a physical key puts the keypad away"
    );
}

#[test]
fn a_sheet_stays_above_the_on_screen_keyboard() {
    let phone = Rect::new(0.0, 0.0, 390.0, 844.0);
    let mut prompt = focused(SecretKind::Secret);
    prompt.set_keyboard_inset(320.0);
    placed(&mut prompt, phone);
    let field = prompt.layout().fields[0];
    assert!(
        field.y1 <= phone.y1 - 320.0,
        "{field:?} is under the keyboard"
    );
}

#[test]
fn the_pointer_presses_the_buttons() {
    let window = Rect::new(0.0, 0.0, 1280.0, 800.0);
    let mut prompt = focused(SecretKind::Secret);
    typed(&mut prompt, "pw");
    placed(&mut prompt, window);
    let primary = prompt.layout().primary;
    dispatch(
        &mut prompt,
        &cursor_moved(primary.center().x, primary.center().y),
    );
    dispatch(&mut prompt, &mouse_press());
    assert!(
        prompt.take_outcome().is_none(),
        "a press alone answers nothing"
    );
    dispatch(&mut prompt, &mouse_release());
    assert_eq!(submitted(&mut prompt).secret.as_str(), "pw");

    let cancel = prompt.layout().cancel;
    dispatch(
        &mut prompt,
        &cursor_moved(cancel.center().x, cancel.center().y),
    );
    dispatch(&mut prompt, &mouse_press());
    dispatch(&mut prompt, &mouse_release());
    assert!(matches!(prompt.take_outcome(), Some(PromptOutcome::Cancel)));
}

#[test]
fn a_screen_reader_hears_the_title_the_labels_and_the_error() {
    let mut prompt = focused(SecretKind::Secret).with_label(FieldRole::Secret, "Wi-Fi password");
    let node = prompt.access_node();
    assert_eq!(node.role, Role::Dialog);
    assert_eq!(node.name.as_deref(), Some("Connect to TruffleBoys2026"));
    assert_eq!(node.description.as_deref(), Some("WPA2 Personal"));
    let field = node
        .children
        .iter()
        .find(|child| child.role == Role::TextInput)
        .expect("field node");
    assert_eq!(field.name.as_deref(), Some("Wi-Fi password"));

    typed(&mut prompt, "nope");
    prompt.reject("Wrong password");
    let node = prompt.access_node();
    let notice = node
        .children
        .iter()
        .find(|child| child.key == Some(NOTICE_KEY))
        .expect("notice node");
    assert_eq!(notice.name.as_deref(), Some("Wrong password"));
    assert_eq!(
        notice.live,
        Some(Politeness::Assertive),
        "an error is announced at once"
    );
    let field = node
        .children
        .iter()
        .find(|child| child.role == Role::TextInput)
        .expect("field node");
    assert!(field.state.invalid);
    assert_eq!(field.description.as_deref(), Some("Wrong password"));
}

#[test]
fn named_fields_answer_every_value_in_order_and_wait_only_for_required_ones() {
    let mut prompt = SecretPrompt::with_fields(vec![
        FieldSpec::text("User name").with_initial("quinn"),
        FieldSpec::secret("Password"),
        FieldSpec::secret("One-time code").optional(),
    ]);
    Widget::set_focus(&mut prompt, true);
    assert_eq!(prompt.focused_field(), Some(FieldRole::Secret));
    assert!(!prompt.can_submit());
    typed(&mut prompt, "pw");
    assert!(prompt.can_submit(), "the optional code may stay empty");
    prompt.submit();
    let answer = submitted(&mut prompt);
    let values: Vec<&str> = answer.values.iter().map(|value| value.as_str()).collect();
    assert_eq!(values, ["quinn", "pw", ""]);
    assert_eq!(answer.secret.as_str(), "pw");

    prompt.reject_at(2, "The code expired");
    assert_eq!(prompt.focused_field(), Some(FieldRole::Secret));
    assert!(
        prompt
            .fields()
            .nth(2)
            .is_some_and(|(_, field)| field.is_focused()),
        "the refused field takes the keyboard"
    );
}

#[test]
fn choices_and_the_option_are_answered_with_the_secret() {
    let mut prompt = focused(SecretKind::Secret)
        .with_choices("Authenticate as", vec!["quinn".into(), "root".into()], 0)
        .with_option("Remember on this device", true);
    typed(&mut prompt, "pw");
    // Back to the choices, then the next one.
    Dispatch::new()
        .modifiers(ModifiersState::SHIFT)
        .send(&mut prompt, &named_key(NamedKey::Tab));
    dispatch(&mut prompt, &named_key(NamedKey::ArrowRight));
    assert_eq!(prompt.choice(), 1);
    // On to the option past the field, and untick it.
    dispatch(&mut prompt, &named_key(NamedKey::Tab));
    dispatch(&mut prompt, &named_key(NamedKey::Tab));
    dispatch(&mut prompt, &named_key(NamedKey::Space));
    assert_eq!(prompt.option_checked(), Some(false));
    dispatch(&mut prompt, &named_key(NamedKey::Enter));
    let answer = submitted(&mut prompt);
    assert_eq!(answer.choice, Some(1));
    assert_eq!(answer.option, Some(false));
    assert_eq!(answer.secret.as_str(), "pw");
}
