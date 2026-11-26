//! What a frame shows: the paragraphs a keystroke prepares at every width,
//! the typography a theme sets, read-only text and its links, contrast, a
//! replacement, the purpose handed to the input method, and the menus over it.
use super::super::*;
use super::draw_to_populate;
use super::drive;
use super::mods;
use super::suggesting_field;
use super::x_for_byte;
use crate::synthetic_input::{char_key, cursor_moved, mouse_press, named_key};
use crate::testing::dispatch;
use crate::text_edit::SUGGESTION_SETTLE;

tilo_test_env::scenario! {
    fn a_field_shapes_measures_and_lays_out_its_text;


    phase a_prompt_wider_than_its_field_shows_what_fits_and_says_where_it_stopped;
    {
        let prompt = "Reply to Launch plan for the autumn release";
        let drawn = |width: f64| {
            let mut field = TextInput::local().with_placeholder(prompt);
            field.apply_layout_external(Rect::new(0.0, 0.0, width, 40.0));
            draw_to_populate(&mut field);
            field
                .placeholder_fit()
                .expect("a field showing a prompt reports how it fit")
        };
        let wide = drawn(600.0);
        assert_eq!(
            wide.shown, prompt,
            "a field with area shows the whole prompt"
        );
        assert!(wide.width <= wide.limit + 0.5, "{wide:?}");

        let narrow = drawn(160.0);
        assert!(narrow.shown.len() < prompt.len(), "{narrow:?}");
        assert!(
            prompt.starts_with(narrow.shown.trim_end_matches('…')),
            "{narrow:?}"
        );
        assert!(narrow.shown.ends_with('…'), "{narrow:?}");
        assert!(
            narrow.width <= narrow.limit + 0.5,
            "what it shows fits the area it has: {narrow:?}"
        );
    }
}

tilo_test_env::scenario! {
    fn spelling_and_corrections_follow_the_typing;

    phase spelling_marks_follow_edits_like_a_full_check;
    {
        let _settled = crate::motion::testing::settled();
        let text = "teh first paragraph\n\nsecond wrold here\nthird is fine\n\nlast lnie";
        let mut input = TextInput::local()
            .with_spell_language("en_US")
            .with_initial_value(text);
        input.set_focus(true);
        input.refresh_spelling();
        let edits: [(&str, usize); 6] = [
            ("x", 4),
            (" mispeled", 19),
            ("\nnew paragrph\n", 21),
            ("", 0),
            ("Teh", 0),
            ("ok", 10),
        ];
        for (insert, at) in edits {
            let mut edited = input.value().to_owned();
            let at = at.min(edited.len());
            if insert.is_empty() {
                if let Some(newline) = edited.find('\n') {
                    edited.remove(newline);
                }
            } else {
                edited.insert_str(at, insert);
            }
            input.text.clone_from(&edited);
            input.refresh_spelling();
            let mut fresh = TextInput::local()
                .with_spell_language("en_US")
                .with_initial_value(&edited);
            fresh.set_focus(true);
            fresh.refresh_spelling();
            assert_eq!(input.spell_spans, fresh.spell_spans, "{edited:?}");
            assert!(!input.spell_spans.is_empty(), "{edited:?}");
        }
    }

    phase a_replacement_lands_only_on_the_text_it_was_made_for_and_is_left_selected;
    {
        let _settled = crate::motion::testing::settled();
        let mut ti = TextInput::local().with_initial_value("hello world");
        assert!(
            !ti.replace_range(0..5, "hullo", "HELLO"),
            "a range that no longer holds the expected text is refused"
        );
        assert!(!ti.replace_range(6..40, "world", "there"));
        assert_eq!(ti.value(), "hello world");
        assert!(ti.replace_range(6..11, "world", "there"));
        assert_eq!(ti.value(), "hello there");
        assert_eq!(ti.selection_range(), Some((6, 11)));

        let mut read_only = TextInput::local()
            .with_initial_value("hello")
            .with_read_only(true);
        assert!(!read_only.replace_range(0..5, "hello", "HELLO"));
        assert_eq!(read_only.value(), "hello");

        let mut ti = TextInput::local()
            .with_spell_language("en_US")
            .with_initial_value("teh");
        ti.core.set_selection(0, 3);
        ti.open_context_menu([10.0, 10.0]);
        let fix = ti
            .context_actions
            .iter()
            .position(|action| matches!(action, Some(CtxAction::ApplySpelling { .. })))
            .expect("a correction is offered");
        ti.set_text("ten");
        assert!(!ti.dispatch_context_action(fix));
        assert_eq!(ti.value(), "ten");
    }


    phase sure_corrections_apply_at_the_boundary_and_backspace_takes_one_back;
    // "i" and "im" are corrected although word lists hold "i"; "cant" and "wont"
    // become "can't" and "won't" at the next space, as a phone keyboard does.
    // Backspace right after takes the correction back, and the word taken back
    // stays as typed when the space comes again.
    {
        let _settled = crate::motion::testing::settled();
        for (typed, corrected) in [("so i", "so I "), ("im", "I'm ")] {
            let mut ti = suggesting_field(typed);
            let start = crate::time::now();
            ti.refresh_suggestion_at(start);
            ti.refresh_suggestion_at(start + SUGGESTION_SETTLE);
            let preview = ti
                .autocorrection_preview
                .clone()
                .expect("the pronoun is corrected although word lists hold \"i\"");
            assert!(preview.automatic, "{typed}: a sure correction");
            ti.insert_word_boundary(" ");
            assert_eq!(ti.value(), corrected);
        }
        for (typed, corrected) in [("I cant", "I can't "), ("it wont", "it won't ")] {
            let mut ti = TextInput::local()
                .with_spellcheck(true)
                .with_autocorrect(true)
                .with_spell_language("en_US")
                .with_initial_value(typed);
            ti.core.set_caret(ti.text.len());
            ti.insert_word_boundary(" ");
            assert_eq!(ti.value(), corrected);

            let mut handled = false;
            ti.handle_backspace_key(false, &mut handled);
            assert!(handled);
            assert_eq!(ti.value(), typed, "Backspace restores the word as typed");
            ti.insert_word_boundary(" ");
            assert_eq!(
                ti.value(),
                format!("{typed} "),
                "a correction taken back is not made again"
            );
        }
    }
}

tilo_test_env::scenario! {
    fn read_only_text_selects_links_and_never_changes;

    phase read_only_text_selects_and_copies_but_never_changes;
    {
        let _settled = crate::motion::testing::settled();
        let mut ti = TextInput::local()
            .with_initial_value("read me")
            .with_read_only(true);
        ti.set_focus(true);
        ti.set_cursor_pos(7);
        let mut doc = AutoCommit::new();
        for key in [
            char_key("x"),
            named_key(NamedKey::Backspace),
            named_key(NamedKey::Enter),
        ] {
            assert!(
                !drive(&mut ti, &mut doc, &key, Modifiers::default()),
                "an edit key is left to the app"
            );
        }
        assert!(!drive(
            &mut ti,
            &mut doc,
            &crate::synthetic_input::key_press(Key::Character("v".into()), None),
            mods(true, false)
        ));
        assert_eq!(ti.value(), "read me");
        assert!(drive(
            &mut ti,
            &mut doc,
            &named_key(NamedKey::ArrowLeft),
            mods(false, true)
        ));
        assert_eq!(
            ti.selection_range(),
            Some((6, 7)),
            "shift+Left still selects"
        );
    }

    phase read_only_text_links_what_it_may_and_opens_only_the_web_and_mail;
    {
        let _settled = crate::motion::testing::settled();
        let ti = TextInput::local()
            .with_initial_value("see https://example.com/a, or www.example.org and click here")
            .with_read_only(true);
        let at = |needle: &str| ti.text.find(needle).unwrap();
        assert_eq!(
            ti.link_at(at("example.com")).as_deref(),
            Some("https://example.com/a")
        );
        assert_eq!(
            ti.link_at(at("example.org")).as_deref(),
            Some("https://www.example.org"),
            "a bare host opens over https"
        );
        assert_eq!(ti.link_at(at("see")), None);

        let here = at("click here");
        let ti = ti.with_links(vec![(here..here + 10, "https://example.net/x".into())]);
        assert_eq!(
            ti.link_at(here + 2).as_deref(),
            Some("https://example.net/x")
        );

        let editable = TextInput::local().with_initial_value("https://example.com");
        assert_eq!(editable.link_at(3), None, "an editable field links nothing");

        use crate::text_detect::Detected;
        assert_eq!(
            link_target(Detected::Url, "javascript://x/%0aalert(1)"),
            None
        );
        assert_eq!(link_target(Detected::Url, "ftp://example.com"), None);
        assert_eq!(
            link_target(Detected::Email, "me@example.com").as_deref(),
            Some("mailto:me@example.com")
        );
        assert_eq!(link_target(Detected::Mention, "@ilya"), None);
    }
}

#[test]
fn forced_color_idle_field_boundary_has_rendered_non_text_contrast() {
    let _settled = crate::motion::testing::settled();
    let mut renderer = crate::offscreen::OffscreenRenderer::new(160, 64);
    for theme in [
        Theme::high_contrast(),
        Theme::preset(crate::theme::Preset::HighContrastLight),
    ] {
        assert!(theme.colors_are_forced());
        let mut scene = Scene::new();
        paint_field_chrome(
            &mut scene,
            Affine::IDENTITY,
            &theme,
            &RoundedRect::from_rect(Rect::new(12.0, 12.0, 148.0, 52.0), 8.0),
            FieldState::default(),
        );
        let pixels = renderer.render(&scene, theme.tokens.colors.surfaces.content);
        let pixel = |x: usize, y: usize| {
            let offset = (y * 160 + x) * 4;
            vello::peniko::Color::from_rgba8(
                pixels[offset],
                pixels[offset + 1],
                pixels[offset + 2],
                pixels[offset + 3],
            )
        };
        let ground = pixel(80, 24);
        for edge in [pixel(80, 12), pixel(12, 32)] {
            assert!(
                crate::a11y::contrast_ratio(edge, ground)
                    >= crate::tokens::colors::NON_TEXT_MIN_CONTRAST,
                "actual idle field edge must remain visible when forced surfaces coincide"
            );
        }
        let enabled_contrast = crate::a11y::contrast_ratio(pixel(80, 12), ground);
        let mut disabled_scene = Scene::new();
        paint_field_chrome(
            &mut disabled_scene,
            Affine::IDENTITY,
            &theme,
            &RoundedRect::from_rect(Rect::new(12.0, 12.0, 148.0, 52.0), 8.0),
            FieldState {
                disabled: true,
                ..FieldState::default()
            },
        );
        let disabled_pixels =
            renderer.render(&disabled_scene, theme.tokens.colors.surfaces.content);
        for (x, y) in [(80, 12), (12, 32)] {
            let offset = (y * 160 + x) * 4;
            let edge = vello::peniko::Color::from_rgba8(
                disabled_pixels[offset],
                disabled_pixels[offset + 1],
                disabled_pixels[offset + 2],
                disabled_pixels[offset + 3],
            );
            let contrast = crate::a11y::contrast_ratio(edge, ground);
            assert!(
                contrast > 1.5 && contrast < enabled_contrast,
                "the actual disabled group must dim the forced outline while keeping it visible"
            );
        }
    }
}

tilo_test_env::scenario! {
    fn a_field_tells_its_host_what_it_holds_and_needs;

    phase a_secret_field_never_hands_its_text_to_a_text_action;
    {
        let _settled = crate::motion::testing::settled();
        for field in [
            TextInput::local().with_secret(true),
            TextInput::local().with_kind(TextInputKind::Password),
            TextInput::local().with_kind(TextInputKind::Secret),
        ] {
            let mut ti = field
                .with_text_action("translate", "Translate")
                .with_initial_value("hunter2");
            ti.core.set_selection(0, 7);
            ti.open_context_menu([10.0, 10.0]);
            assert!(
                !ti.context_actions
                    .iter()
                    .any(|action| matches!(action, Some(CtxAction::TextAction(_)))),
                "{:?}",
                ti.context_menu_labels()
            );
            assert_eq!(ti.take_text_action_request(), None);
        }
    }

    phase every_kind_and_override_reaches_the_input_method_as_its_purpose_and_hints;
    {
        use crate::ime::InputPurpose;
        let _settled = crate::motion::testing::settled();
        const NORMAL: u32 = 0;
        const DIGITS: u32 = 2;
        const NUMBER: u32 = 3;
        const PHONE: u32 = 4;
        const URL: u32 = 5;
        const EMAIL: u32 = 6;
        const NAME: u32 = 7;
        const PASSWORD: u32 = 8;
        const PIN: u32 = 9;
        const DATE: u32 = 10;
        const TIME: u32 = 11;
        const TERMINAL: u32 = 13;
        const COMPLETION: u32 = 0x1;
        const SPELLCHECK: u32 = 0x2;
        const AUTO_CAPITALIZATION: u32 = 0x4;
        const LOWERCASE: u32 = 0x8;
        const TITLECASE: u32 = 0x20;
        const SECRET: u32 = 0x40 | 0x80;
        const MULTILINE: u32 = 0x200;

        // What each kind of field tells the on-screen keyboard: the purpose it asks for, and the
        // hints that follow from what it is set to do.
        type Case = (&'static str, TextInputKind, bool, bool, bool, bool, (u32, u32));
        let cases: &[Case] = &[
            ("prose", TextInputKind::Prose, false, false, false, false, (0, NORMAL)),
            ("prose, multiline", TextInputKind::Prose, true, false, false, false, (MULTILINE, NORMAL)),
            ("prose, checking off", TextInputKind::Prose, false, false, false, false, (0, NORMAL)),
            ("prose, spellcheck on", TextInputKind::Prose, false, true, false, false, (SPELLCHECK, NORMAL)),
            ("prose, autocorrect on", TextInputKind::Prose, false, false, true, false, (COMPLETION, NORMAL)),
            ("name", TextInputKind::Name, false, false, false, false, (TITLECASE, NAME)),
            ("search", TextInputKind::Search, false, false, false, false, (0, NORMAL)),
            ("username", TextInputKind::Username, false, false, false, false, (LOWERCASE, NORMAL)),
            ("email", TextInputKind::Email, false, false, false, false, (LOWERCASE, EMAIL)),
            ("url", TextInputKind::Url, false, false, false, false, (LOWERCASE, URL)),
            ("path", TextInputKind::Path, false, false, false, false, (0, NORMAL)),
            ("code", TextInputKind::Code, false, false, false, false, (0, NORMAL)),
            ("code, multiline", TextInputKind::Code, true, false, false, false, (MULTILINE, NORMAL)),
            ("number", TextInputKind::Number, false, false, false, false, (0, NUMBER)),
            ("phone", TextInputKind::Phone, false, false, false, false, (0, PHONE)),
            ("pin", TextInputKind::Pin, false, false, false, false, (0, PIN)),
            ("masked pin keeps its digit pad", TextInputKind::Pin, false, false, false, true, (SECRET, PIN)),
            ("password", TextInputKind::Password, false, false, false, false, (SECRET, PASSWORD)),
            ("secret", TextInputKind::Secret, false, false, false, false, (SECRET, PASSWORD)),
            ("masked email", TextInputKind::Email, false, false, false, true, (SECRET, PASSWORD)),
            ("password, multiline and spellcheck on", TextInputKind::Password, true, true, true, false, (SECRET, PASSWORD)),
        ];
        for &(case, kind, multiline, spellcheck, autocorrect, secret, want) in cases {
            // The kind goes on after the mask, as a person's field does: a masked PIN keeps its
            // digit pad, and a mask over any other kind is the password keyboard.
            let mut field = TextInput::local()
                .with_secret(secret)
                .with_kind(kind)
                .multiline(multiline)
                .with_spellcheck(spellcheck)
                .with_autocorrect(autocorrect)
                .with_initial_value("x");
            field.set_focus(true);
            field.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 120.0));
            draw_to_populate(&mut field);
            let intent = crate::ime::ImeWindowState::from_widget(&field).expect(case);
            assert_eq!(intent.content.wayland(), want, "{case}");
            assert_eq!(
                intent.content.winit_purpose() == winit::window::ImePurpose::Password,
                want.0 & SECRET == SECRET,
                "{case}: platforms with three purposes still hide a secret"
            );
        }

        for (purpose, want) in [
            (InputPurpose::Decimal, NUMBER),
            (InputPurpose::OneTimeCode, DIGITS),
            (InputPurpose::Date, DATE),
            (InputPurpose::Time, TIME),
            (InputPurpose::Terminal, TERMINAL),
        ] {
            assert_eq!(crate::ime::InputContent::of(purpose).wayland(), (0, want));
        }
        let mut sentences = crate::ime::InputContent::TEXT;
        sentences.hints.capitalization = crate::ime::Capitalization::Sentences;
        assert_eq!(sentences.wayland(), (AUTO_CAPITALIZATION, NORMAL));
    }

    phase an_open_text_menu_owns_the_keyboard;
    {
        let _settled = crate::motion::testing::settled();
        let mut ti = TextInput::local()
            .with_spellcheck(false)
            .with_text_action("translate", "Translate")
            .with_initial_value("hello world");
        ti.set_focus(true);
        ti.core.set_selection(0, 5);
        ti.open_context_menu([10.0, 10.0]);
        let named = |key| crate::synthetic_input::named_key(key);
        assert!(dispatch(&mut ti, &named(NamedKey::ArrowDown)));
        assert!(dispatch(&mut ti, &crate::synthetic_input::char_key("x")));
        assert_eq!(ti.value(), "hello world", "typing does not reach the text");
        assert_eq!(ti.selection_range(), Some((0, 5)));
        assert!(dispatch(&mut ti, &named(NamedKey::Enter)));
        assert!(!ti.context_menu.is_open());
        assert_eq!(ti.value(), "hello world", "Enter inserts no line");
        assert_eq!(
            ti.take_text_action_request().map(|request| request.id),
            Some("translate".to_owned())
        );

        ti.open_context_menu([10.0, 10.0]);
        assert!(dispatch(&mut ti, &named(NamedKey::Escape)));
        assert!(!ti.context_menu.is_open());
        assert_eq!(ti.selection_range(), Some((0, 5)));
    }

    phase a_chrome_owned_field_places_the_caret_and_reports_every_selection;
    // Six chrome surfaces paint a field themselves and forward its pointer
    // events ([`TextInput::handle_chrome_pointer`]); each copies the selection
    // back into the text it owns. Hover is not ours to take, a press places the
    // caret where the person pointed, a drag sweeps a selection, and a press
    // that takes the selection back reports it collapsed rather than nothing to
    // say — otherwise the surface keeps painting a selection nobody holds.
    {
        let _settled = crate::motion::testing::settled();
        {
            let mut ti = TextInput::local().with_initial_value("hello world");
            ti.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 40.0));
            draw_to_populate(&mut ti);

            let inside = x_for_byte(&ti, 8);
            let before = ti.selection_anchor_focus_or_caret();
            assert!(
                !ti.handle_chrome_pointer(&cursor_moved(inside, 20.0), 1.0, mods(false, false)),
                "a hover belongs to the surface, not to the field"
            );
            assert_eq!(
                ti.selection_anchor_focus_or_caret(),
                before,
                "and it places no caret"
            );
            assert!(
                ti.handle_chrome_pointer(&mouse_press(), 1.0, mods(false, false)),
                "a press on the field is the field's"
            );
            assert_eq!(
                ti.selection_anchor_focus_or_caret(),
                (8, 8),
                "the caret went where the person pressed"
            );
        }
        {
            let mut ti = TextInput::local().with_initial_value("hello world");
            ti.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 40.0));
            draw_to_populate(&mut ti);

            let start = x_for_byte(&ti, 2);
            let end = x_for_byte(&ti, 9);
            ti.handle_chrome_pointer(&cursor_moved(start, 20.0), 1.0, mods(false, false));
            assert!(ti.handle_chrome_pointer(&mouse_press(), 1.0, mods(false, false)));
            ti.handle_chrome_pointer(&cursor_moved(end, 20.0), 1.0, mods(false, false));
            assert_eq!(
                ti.selection_anchor_focus_or_caret(),
                (2, 9),
                "the drag swept a selection the surface can paint"
            );
            assert!(ti.handle_chrome_pointer(&mouse_press(), 1.0, mods(false, false)));
            let (anchor, caret) = ti.selection_anchor_focus_or_caret();
            assert_eq!(anchor, caret, "a click takes the selection back");
        }
        {
            let mut ti = TextInput::local().with_initial_value("hello world");
            ti.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 40.0));
            draw_to_populate(&mut ti);
            let inside = x_for_byte(&ti, 8);
            ti.handle_chrome_pointer(&cursor_moved(inside * 2.0, 40.0), 2.0, mods(false, false));
            assert!(ti.handle_chrome_pointer(&mouse_press(), 2.0, mods(false, false)));
            assert_eq!(ti.selection_anchor_focus_or_caret(), (8, 8));
        }
    }

}
