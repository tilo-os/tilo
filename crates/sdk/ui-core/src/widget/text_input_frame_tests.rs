//! What a frame shows: the paragraphs a keystroke prepares at every width,
//! the typography a theme sets, read-only text and its links, contrast, a
//! replacement, the purpose handed to the input method, and the menus over it.
use super::super::*;
use super::body_size;
use super::draw_to_populate;
use super::draw_with_theme;
use super::drive;
use super::mods;
use super::suggesting_field;
use super::typing_field;
use super::typing_frame;
use super::x_for_byte;
use crate::synthetic_input::{char_key, cursor_moved, mouse_press, named_key};
use crate::testing::dispatch;
use crate::text_edit::SUGGESTION_SETTLE;

#[test]
fn multiline_keystroke_prepares_only_the_edited_paragraph_at_every_width() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let mut tr = crate::font_catalog::fixture_renderer();
    let theme = Theme::dark();
    tr.set_text_scale(theme.text_scale);
    let text = (0..200)
        .map(|index| format!("Paragraph {index} of a long note, with enough words to wrap."))
        .collect::<Vec<_>>()
        .join("\n");
    let mut input = typing_field(&text, &mut tr, &theme);
    let at = text.find("Paragraph 100 ").unwrap() + "Paragraph 100".len();
    input.set_cursor_pos(at);
    typing_frame(&mut input, &mut tr, &theme);
    crate::richtext::take_prepared_paragraphs();
    let mut doc = AutoCommit::new();
    for (step, key) in ["x", "y", " ", "z"].into_iter().enumerate() {
        assert!(drive(
            &mut input,
            &mut doc,
            &char_key(key),
            Modifiers::default()
        ));
        typing_frame(&mut input, &mut tr, &theme);
        assert_eq!(
            crate::richtext::take_prepared_paragraphs(),
            1,
            "keystroke {step}: one paragraph prepared and shaped for every width"
        );
    }
    // The paragraph measured and painted is the one a fresh field builds.
    let fresh = typing_field(input.value(), &mut tr, &theme);
    let (typed, fresh) = (
        input.single_layout.as_ref().unwrap(),
        fresh.single_layout.as_ref().unwrap(),
    );
    assert_eq!(typed.height(), fresh.height());
    assert_eq!(typed.logical_line_origins(), fresh.logical_line_origins());
    for byte in (0..=input.value().len()).filter(|byte| input.value().is_char_boundary(*byte)) {
        assert_eq!(
            typed.caret_geometry(byte),
            fresh.caret_geometry(byte),
            "byte {byte}"
        );
    }
    for (a, b) in typed.buffer.lines.iter().zip(&fresh.buffer.lines) {
        assert_eq!(
            format!("{:?}", a.layout_opt()),
            format!("{:?}", b.layout_opt())
        );
    }
}

#[test]
fn spelling_marks_follow_edits_like_a_full_check() {
    // Motion reads at its end: this test checks the resting frame.
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
            // Join the first two lines.
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

#[test]
fn single_line_batch_metrics_preserve_graphemes_preedit_and_theme_geometry() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    for text in ["", "ordinary ffi text", "abc אבג xyz", "é 👩‍💻 end"] {
        for scale in [1.0, 2.0] {
            let theme = Theme::dark().with_text_scale(scale);
            let mut input = TextInput::local().with_initial_value(text);
            input.apply_layout_external(Rect::new(20.0, 30.0, 420.0, 70.0));
            for preedit_at in [None, Some(0), Some(text.len() / 2), Some(text.len())] {
                input.core.clear_preedit();
                if let Some(at) = preedit_at {
                    input.set_cursor_pos(at);
                    input.core.set_preedit("編", Some((0, "編".len())));
                }
                draw_with_theme(&mut input, &theme);
                let layout = input
                    .single_layout
                    .as_ref()
                    .expect("production draw shapes the field");
                let mut reference = vec![0.0; text.len().saturating_add(1)];
                for byte in grapheme_indices(text)
                    .map(|(byte, _)| byte)
                    .chain([text.len()])
                {
                    reference[byte] =
                        f64::from(layout.caret_geometry(input.text_to_layout_byte(byte)).0);
                }
                assert_eq!(
                    input.char_x_positions, reference,
                    "{text:?}, scale={scale}, preedit_at={preedit_at:?}"
                );
            }
        }
    }
}

#[test]
fn themed_input_typography_reflows_and_centers_actual_shaped_text() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let mut input = TextInput::local()
        .with_initial_value("Themed input baseline")
        .with_spellcheck(false)
        .with_autocorrect(false);
    input.core.set_selection(6, 12);
    let mut authored = Theme::dark();
    authored.tokens.typography = authored.tokens.typography.with_base_size(18.0);
    let themes = [
        authored.clone().with_ui_scale(1.25).with_text_scale(1.5),
        authored.with_ui_scale(2.0).with_text_scale(1.5),
    ];
    let mut previous_height = 0.0;
    for theme in themes {
        let mut tree = TaffyTree::new();
        let node = input.build_layout(&mut tree, &theme, &mut Default::default());
        tree.compute_layout(
            node,
            Size {
                width: taffy::AvailableSpace::Definite(640.0),
                height: taffy::AvailableSpace::MaxContent,
            },
        )
        .unwrap();
        input.apply_layout(&tree, node, Point::ZERO);
        draw_with_theme(&mut input, &theme);
        let expected = theme.text(TextRole::Body).size;
        assert!(expected > body_size() * theme.text_scale);
        let layout = input
            .single_layout
            .as_ref()
            .expect("actual draw shapes the input");
        assert!((layout.buffer.metrics().font_size - expected).abs() < 0.001);
        let line_height = f64::from(layout.buffer.metrics().line_height);
        assert!(input.rect.height() >= line_height + 2.0 * f64::from(theme.tokens.spacing.s1));
        assert!(
            input.rect.height() > previous_height,
            "a changed theme reflows the retained field"
        );
        previous_height = input.rect.height();
        assert!((input.single_origin.y + line_height / 2.0 - input.rect.center().y).abs() < 0.001);
        assert_eq!(input.core.caret(), 12);
        assert_eq!(input.core.anchor(), 6);
        assert!(
            input.take_dirty().is_none(),
            "theme changes are not user edits"
        );
    }
    input.set_text_role(TextRole::Heading);
    let theme = Theme::dark().with_ui_scale(2.0).with_text_scale(1.5);
    input.prepare_theme(&theme);
    draw_with_theme(&mut input, &theme);
    assert!(
        (input
            .single_layout
            .as_ref()
            .unwrap()
            .buffer
            .metrics()
            .font_size
            - theme.text(TextRole::Heading).size)
            .abs()
            < 0.001,
        "a role change reads the current theme instead of a stale cached Body size"
    );
}

#[test]
fn read_only_text_selects_and_copies_but_never_changes() {
    // Motion reads at its end: this test checks the resting frame.
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

#[test]
fn embedded_input_measurement_adopts_theme_and_keeps_document_size_override() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let text = "A wrapping field must measure the same themed typography that it paints.";
    let mut theme = Theme::dark();
    theme.tokens.typography = theme.tokens.typography.with_base_size(19.0);
    let theme = theme.with_ui_scale(2.0).with_text_scale(1.5);
    let mut input = TextInput::local().multiline(true).with_initial_value(text);
    let mut renderer = TextRenderer::new();
    assert_eq!(
        renderer.text_scale(),
        1.0,
        "renderer starts at its old frame scale"
    );
    let height = input.measure_wrapped_content(280.0, &mut renderer, &theme);
    let layout = input
        .single_layout
        .as_ref()
        .expect("actual wrapped measurement shapes text");
    assert!((layout.buffer.metrics().font_size - theme.text(TextRole::Body).size).abs() < 0.001);
    assert!(height >= layout.height());
    assert!(
        layout.buffer.layout_runs().count() > 1,
        "the themed paragraph really wraps"
    );

    let mut document = TextInput::local()
        .with_content_size(17.0)
        .with_initial_value("document");
    document.prepare_theme(&theme);
    document.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 80.0));
    draw_with_theme(&mut document, &theme);
    assert_eq!(
        document
            .single_layout
            .as_ref()
            .unwrap()
            .buffer
            .metrics()
            .font_size,
        17.0
    );
}

#[test]
fn read_only_text_links_its_addresses_and_the_ranges_it_is_given() {
    // Motion reads at its end: this test checks the resting frame.
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
}

#[test]
fn only_the_web_and_mail_open_from_a_link() {
    use crate::text_detect::Detected;
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
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

#[test]
fn forced_color_idle_field_boundary_has_rendered_non_text_contrast() {
    // Motion reads at its end: this test checks the resting frame.
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

#[test]
fn a_replacement_lands_only_on_the_text_it_was_made_for_and_is_left_selected() {
    // Motion reads at its end: this test checks the resting frame.
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

    // A spelling fix chosen from the menu refuses a word that changed
    // while the menu was open.
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

#[test]
fn a_secret_field_never_hands_its_text_to_a_text_action() {
    // Motion reads at its end: this test checks the resting frame.
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

#[test]
fn every_kind_and_override_reaches_the_input_method_as_its_purpose_and_hints() {
    use crate::ime::InputPurpose;
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    // `zwp_text_input_v3` wire values.
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

    let kind = |kind| TextInput::local().with_kind(kind);
    let cases: Vec<(&str, TextInput, (u32, u32))> = vec![
        // Prose asks for nothing beyond the input method's defaults, as
        // before, whatever the toolkit's own checking does.
        ("prose", TextInput::local(), (0, NORMAL)),
        (
            "prose, multiline",
            TextInput::local().multiline(true),
            (MULTILINE, NORMAL),
        ),
        (
            "prose, checking off",
            TextInput::local()
                .with_spellcheck(false)
                .with_autocorrect(false),
            (0, NORMAL),
        ),
        (
            "prose, spellcheck on",
            TextInput::local().with_spellcheck(true),
            (SPELLCHECK, NORMAL),
        ),
        (
            "prose, autocorrect on",
            TextInput::local().with_autocorrect(true),
            (COMPLETION, NORMAL),
        ),
        ("name", kind(TextInputKind::Name), (TITLECASE, NAME)),
        ("search", kind(TextInputKind::Search), (0, NORMAL)),
        (
            "username",
            kind(TextInputKind::Username),
            (LOWERCASE, NORMAL),
        ),
        ("email", kind(TextInputKind::Email), (LOWERCASE, EMAIL)),
        ("url", kind(TextInputKind::Url), (LOWERCASE, URL)),
        ("path", kind(TextInputKind::Path), (0, NORMAL)),
        ("code", kind(TextInputKind::Code), (0, NORMAL)),
        (
            "code, multiline",
            kind(TextInputKind::Code).multiline(true),
            (MULTILINE, NORMAL),
        ),
        ("number", kind(TextInputKind::Number), (0, NUMBER)),
        ("phone", kind(TextInputKind::Phone), (0, PHONE)),
        ("pin", kind(TextInputKind::Pin), (0, PIN)),
        (
            "masked pin keeps its digit pad",
            TextInput::local()
                .with_secret(true)
                .with_kind(TextInputKind::Pin),
            (SECRET, PIN),
        ),
        (
            "password",
            kind(TextInputKind::Password),
            (SECRET, PASSWORD),
        ),
        ("secret", kind(TextInputKind::Secret), (SECRET, PASSWORD)),
        (
            "masked email",
            TextInput::local()
                .with_secret(true)
                .with_kind(TextInputKind::Email),
            (SECRET, PASSWORD),
        ),
        (
            "password, multiline and spellcheck on",
            kind(TextInputKind::Password)
                .multiline(true)
                .with_spellcheck(true)
                .with_autocorrect(true),
            (SECRET, PASSWORD),
        ),
    ];
    for (case, field, want) in cases {
        let mut field = field.with_initial_value("x");
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

    // Purposes no text field kind asks for, which custom editors declare.
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

#[test]
fn an_open_text_menu_owns_the_keyboard() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let mut ti = TextInput::local()
        .with_spellcheck(false)
        .with_text_action("translate", "Translate")
        .with_initial_value("hello world");
    ti.set_focus(true);
    ti.core.set_selection(0, 5);
    ti.open_context_menu([10.0, 10.0]);
    let named = |key| crate::synthetic_input::named_key(key);
    // Arrows move through the menu, not the caret or the selection.
    assert!(dispatch(&mut ti, &named(NamedKey::ArrowDown)));
    assert!(dispatch(&mut ti, &crate::synthetic_input::char_key("x")));
    assert_eq!(ti.value(), "hello world", "typing does not reach the text");
    assert_eq!(ti.selection_range(), Some((0, 5)));
    // Enter runs the highlighted row, the registered action.
    assert!(dispatch(&mut ti, &named(NamedKey::Enter)));
    assert!(!ti.context_menu.is_open());
    assert_eq!(ti.value(), "hello world", "Enter inserts no line");
    assert_eq!(
        ti.take_text_action_request().map(|request| request.id),
        Some("translate".to_owned())
    );

    // Escape closes the menu and leaves the field as it was.
    ti.open_context_menu([10.0, 10.0]);
    assert!(dispatch(&mut ti, &named(NamedKey::Escape)));
    assert!(!ti.context_menu.is_open());
    assert_eq!(ti.selection_range(), Some((0, 5)));
}

#[test]
fn a_lowercase_pronoun_is_corrected_at_the_next_word_boundary() {
    // Motion reads at its end: this test checks the resting frame.
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
}

/// "cant" and "wont" become "can't" and "won't" at the next space, as a
/// phone keyboard does; Backspace right after takes the correction back,
/// and the word taken back stays as typed when the space comes again.
#[test]
fn cant_and_wont_are_corrected_and_backspace_takes_a_correction_back_for_good() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
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

#[test]
fn arrow_keys_cross_the_rows_a_paragraph_wraps_onto() {
    let _settled = crate::motion::testing::settled();
    let text = "alpha beta gamma delta epsilon zeta eta theta";
    let mut ti = TextInput::local().multiline(true).with_initial_value(text);
    ti.set_focus(true);
    ti.apply_layout_external(Rect::new(0.0, 0.0, 120.0, 400.0));
    draw_to_populate(&mut ti);
    assert!(ti.multiline_shaped);
    let rows = ti
        .single_layout
        .as_ref()
        .map_or(0, |layout| layout.buffer.layout_runs().count());
    assert!(rows > 2, "the paragraph wraps onto {rows} rows");
    let mut doc = AutoCommit::new();
    ti.set_cursor_pos(0);
    for (key, end) in [(NamedKey::ArrowRight, text.len()), (NamedKey::ArrowLeft, 0)] {
        let mut presses = 0;
        while ti.cursor_pos() != end && presses <= 2 * text.len() {
            let before = ti.cursor_pos();
            drive(&mut ti, &mut doc, &named_key(key), mods(false, false));
            presses += 1;
            assert_ne!(
                ti.cursor_pos(),
                before,
                "{key:?} stopped at byte {before} after {presses} presses"
            );
        }
        assert_eq!(ti.cursor_pos(), end, "{key:?} reaches the end of the text");
    }
}

/// A focused field laid out too narrow for its text scrolls to its caret;
/// widened so the whole text fits, it shows the text from its start again.
#[test]
fn a_focused_field_that_widens_shows_its_text_from_the_start() {
    let _settled = crate::motion::testing::settled();
    let name = "Travel";
    let mut ti = TextInput::local().with_initial_value(name);
    ti.set_focus(true);
    ti.set_cursor_pos(name.len());
    let theme = Theme::dark();
    let presence = crate::presence::PresenceCtx::empty();
    let mut tr = TextRenderer::new();
    let mut draw_at = |ti: &mut TextInput, width: f64| {
        ti.apply_layout_external(Rect::new(0.0, 0.0, width, 32.0));
        let mut scene = Scene::new();
        Widget::draw(ti, &mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
    };
    draw_at(&mut ti, 40.0);
    assert!(ti.scroll_x() > 0.0, "narrow, it follows the caret");
    draw_at(&mut ti, 400.0);
    assert_eq!(ti.scroll_x(), 0.0, "wide, the whole name shows");
}

/// Six chrome surfaces paint a field themselves and forward its pointer
/// events ([`TextInput::handle_chrome_pointer`]); each copies the selection
/// back into the text it owns. Hover is not ours to take, a press places the
/// caret where the person pointed, a drag sweeps a selection, and a press
/// that takes the selection back reports it collapsed rather than nothing to
/// say — otherwise the surface keeps painting a selection nobody holds.
#[test]
fn a_chrome_owned_field_places_the_caret_and_reports_every_selection() {
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
        // A press back on the text collapses that selection; the surface
        // hears the collapsed pair, not silence.
        assert!(ti.handle_chrome_pointer(&mouse_press(), 1.0, mods(false, false)));
        let (anchor, caret) = ti.selection_anchor_focus_or_caret();
        assert_eq!(anchor, caret, "a click takes the selection back");
    }
    {
        // The same press at a device scale of two lands on the same letter.
        let mut ti = TextInput::local().with_initial_value("hello world");
        ti.apply_layout_external(Rect::new(0.0, 0.0, 300.0, 40.0));
        draw_to_populate(&mut ti);
        let inside = x_for_byte(&ti, 8);
        ti.handle_chrome_pointer(&cursor_moved(inside * 2.0, 40.0), 2.0, mods(false, false));
        assert!(ti.handle_chrome_pointer(&mouse_press(), 2.0, mods(false, false)));
        assert_eq!(ti.selection_anchor_focus_or_caret(), (8, 8));
    }
}
