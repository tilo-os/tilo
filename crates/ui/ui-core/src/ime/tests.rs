use tilo_ui_base::ime::ImeSurroundingText;
use tilo_ui_base::Widget;
use crate::widget::TextInput;

tilo_test_env::scenario! {
    fn the_keyboard_is_told_the_fields_text_purpose_and_selection;

    phase surrounding_text_is_the_fields_text_and_selection;
    {
        let mut field = TextInput::local().with_initial_value("hello world");
        field.set_focus(true);
        assert!(field.perform_action(
            &tilo_ui_base::a11y::Action::SetSelection {
                anchor: 6,
                focus: 11
            },
            &[],
            &mut tilo_ui_base::widget::Edits::lend(&mut automerge::AutoCommit::new()),
            &automerge::ROOT,
        ));
        assert_eq!(
            ImeSurroundingText::from_widget(&field),
            Some(ImeSurroundingText {
                text: "hello world".into(),
                anchor: 6,
                focus: 11,
            })
        );
    }

    phase deferred_input_fingerprint_changes_with_selection_at_the_same_caret;
    {
        let key = [7; 32];
        let mut field = TextInput::local().with_initial_value("hello");
        field.set_focus(true);
        field.set_cursor_pos(5);
        let before = field.ime_edit_fingerprint(&key).unwrap();
        let caret = field.cursor_pos();
        assert!(field.perform_action(
            &tilo_ui_base::a11y::Action::SetSelection {
                anchor: 0,
                focus: 5
            },
            &[],
            &mut tilo_ui_base::widget::Edits::lend(&mut automerge::AutoCommit::new()),
            &automerge::ROOT,
        ));
        assert_eq!(field.cursor_pos(), caret);
        assert_ne!(field.ime_edit_fingerprint(&key).unwrap(), before);
        assert!(
            TextInput::local()
                .with_secret(true)
                .ime_edit_fingerprint(&key)
                .is_none()
        );
    }

    phase a_browser_editor_is_told_each_purpose_and_keeps_free_texts_defaults;
    {
        use tilo_ui_base::{ime::Capitalization, ime::HtmlInputHints, ime::InputContent, ime::InputPurpose};
        let html = |content: InputContent| content.html();
        let attrs =
            |inputmode, enterkeyhint, autocapitalize, autocomplete, spellcheck| HtmlInputHints {
                inputmode,
                enterkeyhint,
                autocapitalize,
                autocomplete,
                spellcheck,
            };
        assert_eq!(
            html(InputContent::TEXT),
            attrs("text", "", "off", "off", false)
        );
        let of = InputContent::of;
        assert_eq!(
            html(of(InputPurpose::Email)),
            attrs("email", "", "off", "email", false)
        );
        assert_eq!(
            html(of(InputPurpose::Phone)),
            attrs("tel", "", "off", "tel", false)
        );
        assert_eq!(
            html(of(InputPurpose::Search)),
            attrs("search", "search", "off", "off", false)
        );
        assert_eq!(
            html(of(InputPurpose::OneTimeCode)),
            attrs("numeric", "", "off", "one-time-code", false)
        );
        let mut name = of(InputPurpose::Name);
        name.hints.capitalization = Capitalization::Words;
        assert_eq!(html(name), attrs("text", "", "words", "name", false));
        let mut checked = InputContent::TEXT;
        checked.hints.spellcheck = true;
        checked.hints.capitalization = Capitalization::Sentences;
        assert_eq!(html(checked), attrs("text", "", "sentences", "off", true));
        // A secret offers no autofill, capitalisation or spellchecking.
        let mut secret = of(InputPurpose::Email).secret();
        secret.hints.spellcheck = true;
        secret.hints.capitalization = Capitalization::Words;
        assert_eq!(html(secret), attrs("email", "", "off", "off", false));
        assert_eq!(
            html(InputContent::PASSWORD),
            attrs("text", "", "off", "off", false)
        );
        assert_eq!(
            html(of(InputPurpose::Decimal).own_keys()),
            attrs("none", "", "off", "off", false)
        );
    }

    phase a_password_field_gives_the_keyboard_nothing;
    {
        let mut field = TextInput::local()
            .with_initial_value("hunter2")
            .with_secret(true);
        field.set_focus(true);
        assert_eq!(ImeSurroundingText::from_widget(&field), None);
    }

    phase long_text_is_cut_to_whole_characters_around_the_selection;
    {
        let before = "é".repeat(ImeSurroundingText::CONTEXT);
        let after = "ü".repeat(ImeSurroundingText::CONTEXT);
        let text = format!("{before}|{after}");
        let caret = before.len();
        let around = ImeSurroundingText::around(&text, caret, caret + 1).unwrap();
        assert_eq!(around.text.len(), 2 * ImeSurroundingText::CONTEXT + 1);
        assert_eq!(&around.text[around.anchor..around.focus], "|");
        assert!(around.text.starts_with('é') && around.text.ends_with('ü'));
        assert_eq!(ImeSurroundingText::around("é", 1, 1), None);
    }
}
