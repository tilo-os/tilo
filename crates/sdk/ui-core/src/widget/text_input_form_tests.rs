//! A field among other controls: the text kind a field kind sets, the rows
//! it shares with buttons, its submit boundary and accessory, the personal
//! words that protect an autocorrection, and a paste that arrives late.
use super::super::*;
use super::body_size;
use super::draw_to_populate;
use super::drive;
use super::mods;
use crate::synthetic_input::{char_key, cursor_moved, mouse_press, named_key};

#[test]
fn accessible_selection_preserves_anchor_and_focus_direction() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let mut input = TextInput::local().with_initial_value("Ae\u{301}👋B");
    input.core.set_selection(8, 1);

    assert_eq!(input.selection_range_public(), Some((1, 8)));
    assert_eq!(input.selection_anchor_focus_public(), Some((8, 1)));
}

#[test]
fn insertion_collapses_an_empty_drag_selection() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    for (text, caret, expected) in [("", 0, "Nx"), ("ab", 1, "aNxb")] {
        let mut ti = TextInput::new(Vec::new());
        ti.set_text(text);
        ti.core.set_selection(caret, caret);
        ti.insert_str("N");
        assert_eq!(ti.selection_range_public(), None);
        assert_eq!(ti.cursor_pos(), caret + 1);
        ti.insert_str("x");
        assert_eq!(ti.value(), expected);
        assert_eq!(ti.selection_range_public(), None);
    }
}

#[test]
fn field_kinds_set_their_text_kind_while_typing() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    for (kind, text) in [
        (TextInputKind::Prose, TextKind::Prose),
        (TextInputKind::Search, TextKind::Prose),
        (TextInputKind::Username, TextKind::Identifier),
        (TextInputKind::Email, TextKind::Identifier),
        (TextInputKind::Url, TextKind::Url),
        (TextInputKind::Number, TextKind::Numeric),
        (TextInputKind::Code, TextKind::Code),
        (TextInputKind::Password, TextKind::Secret),
        (TextInputKind::Secret, TextKind::Secret),
    ] {
        assert_eq!(kind.text_kind(), text, "{kind:?}");
        let input = TextInput::local().with_kind(kind);
        assert_eq!(input.multiline_style(1.0).kind, text, "{kind:?}");
    }
}

#[test]
fn fields_share_rows_with_buttons_and_state_what_they_are() {
    fn layout_height(ti: &mut TextInput, theme: &Theme, width: f32) -> f32 {
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: std::collections::HashMap<NodeId, *mut dyn Widget> =
            std::collections::HashMap::new();
        let node = ti.build_layout(&mut taffy, theme, &mut node_map);
        let mut tr = TextRenderer::new();
        tr.set_text_scale(theme.text_scale);
        let size = Widget::measure(
            ti,
            taffy::Size {
                width: Some(width),
                height: None,
            },
            taffy::Size {
                width: taffy::AvailableSpace::Definite(width),
                height: taffy::AvailableSpace::MaxContent,
            },
            &mut tr,
        );
        match taffy.style(node).unwrap().size.height.into_option() {
            Some(fixed) => fixed,
            None => size.height,
        }
    }

    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    // Case `a_small_field_is_as_tall_as_a_small_button`.
    {
        let theme = Theme::dark();
        let mut small = TextInput::local().control_size(ControlSize::Small);
        assert_eq!(layout_height(&mut small, &theme, 200.0), 28.0);
        assert!(
            (FieldFace::height(ControlSize::Regular, &theme) - 32.0).abs() < 1e-9,
            "the painted face agrees with the retained field"
        );
    }

    // Case `min_lines_holds_a_wrapping_field_open_with_the_field_inset`.
    {
        let theme = Theme::dark();
        let mut notes = TextInput::local().with_min_lines(4);
        let height = layout_height(&mut notes, &theme, 300.0);
        let line = body_size() * LINE_HEIGHT_RATIO;
        assert!(
            height >= 4.0 * line,
            "four lines tall while empty: {height} vs line {line}"
        );
        let mut one = TextInput::local().multiline(true);
        assert!(
            layout_height(&mut one, &theme, 300.0) >= 32.0,
            "a wrapping field is never shorter than a single-line one"
        );
        let single = TextInput::local();
        assert!(
            (notes.text_inset_x(&theme) - single.text_inset_x(&theme)).abs() < 1e-9,
            "wrapping and single-line fields start their text at one inset"
        );
    }

    // Case `a_plain_field_leaves_the_gutter_to_its_parent`.
    {
        let theme = Theme::dark();
        let plain = TextInput::local().multiline(true).with_plain_chrome();
        assert_eq!(plain.text_inset_x(&theme), 0.0);
        let mut document = TextInput::local().multiline(true).with_plain_chrome();
        document.keep_document_inset();
        assert_eq!(document.text_inset_x(&theme), ML_PAD_X);
    }

    // Case `the_role_sets_the_family_and_the_placeholder_follows_it`.
    {
        let reading = TextInput::local().with_text_role(TextRole::Reading);
        assert_eq!(reading.placeholder_style(16.0).family, FontChoice::Serif);
        let code = TextInput::local().with_text_role(TextRole::Code);
        assert_eq!(code.placeholder_style(13.0).family, FontChoice::Monospace);
        let body = TextInput::local();
        assert_eq!(body.placeholder_style(14.0).family, FontChoice::SansSerif);
    }

    // Case `set_text_parks_the_caret_at_the_end`.
    {
        let mut ti = TextInput::local().with_initial_value("abc");
        ti.set_cursor_pos(1);
        ti.set_text("hello");
        assert_eq!(ti.cursor_pos(), 5);
    }

    // Case `content_sized_text_does_not_grow_with_the_text_scale`.
    {
        let big = Theme::dark().with_text_scale(2.0);
        let mut document = TextInput::local().with_content_size(12.0);
        let mut chrome = TextInput::local();
        assert!(
            layout_height(&mut document, &big, 200.0) < layout_height(&mut chrome, &big, 200.0),
            "a document-space field keeps its size under a large text scale"
        );
        assert!((document.scale_for(2.0) - 1.0).abs() < f32::EPSILON);
    }
}

#[test]
fn submit_accessory_and_span_highlighting() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    // Case `on_submit_reports_the_text_on_enter`.
    {
        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        let sink = seen.clone();
        let mut ti = TextInput::local()
            .with_initial_value("https://tilo.so")
            .on_submit(move |text| sink.borrow_mut().push(text.to_string()));
        ti.set_focus(true);
        let mut doc = AutoCommit::new();
        assert!(drive(
            &mut ti,
            &mut doc,
            &named_key(NamedKey::Enter),
            mods(false, false)
        ));
        assert_eq!(seen.borrow().as_slice(), ["https://tilo.so"]);
        assert!(ti.take_submit(), "the polled signal still fires");
    }

    // Case `the_clear_accessory_empties_the_field_as_an_edit`.
    {
        let mut ti = TextInput::local()
            .with_initial_value("query")
            .with_clear_button();
        ti.apply_layout_external(Rect::new(0.0, 0.0, 240.0, 32.0));
        draw_to_populate(&mut ti);
        let node = Widget::accessibility(&ti).expect("field semantics");
        assert_eq!(node.children.len(), 1);
        assert_eq!(node.children[0].name.as_deref(), Some("Clear"));
        let rect = Widget::synthetic_accessibility_rect(&ti, &[0]).expect("accessory rect");
        assert!(rect.x1 <= 240.0 && rect.x0 > 120.0, "{rect:?}");
        let mut doc = AutoCommit::new();
        drive(
            &mut ti,
            &mut doc,
            &cursor_moved(rect.center().x, rect.center().y),
            mods(false, false),
        );
        assert!(drive(&mut ti, &mut doc, &mouse_press(), mods(false, false)));
        assert_eq!(ti.value(), "");
        assert_eq!(
            ti.take_dirty().as_deref(),
            Some(""),
            "clearing is a user edit"
        );
        let node = Widget::accessibility(&ti).expect("field semantics");
        assert!(
            node.children.is_empty(),
            "an empty field hides its clear button"
        );
    }

    // Case `a_toggle_accessory_is_a_pressed_toggle_for_people_and_agents`.
    {
        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::<bool>::new()));
        let sink = seen.clone();
        let mut ti = TextInput::local().with_trailing_accessory(
            crate::icons::Icon::X,
            "Clear for every photo",
            AccessoryAction::Toggle {
                pressed: false,
                on_toggle: Box::new(move |on| sink.borrow_mut().push(on)),
            },
        );
        ti.apply_layout_external(Rect::new(0.0, 0.0, 240.0, 32.0));
        draw_to_populate(&mut ti);
        let node = Widget::accessibility(&ti).expect("field semantics");
        assert_eq!(node.children.len(), 1, "shown while the field is empty");
        let toggle = &node.children[0];
        assert_eq!(toggle.role, crate::a11y::Role::ToggleButton);
        assert_eq!(toggle.state.checked, Some(false));
        let target = toggle.action_target.clone().expect("agents can reach it");

        let rect = Widget::synthetic_accessibility_rect(&ti, &[0]).expect("accessory rect");
        let mut doc = AutoCommit::new();
        drive(
            &mut ti,
            &mut doc,
            &cursor_moved(rect.center().x, rect.center().y),
            mods(false, false),
        );
        assert!(drive(&mut ti, &mut doc, &mouse_press(), mods(false, false)));
        assert_eq!(ti.accessory_pressed(), Some(true));
        let node = Widget::accessibility(&ti).expect("field semantics");
        assert_eq!(node.children[0].state.checked, Some(true));

        let root = automerge::ROOT;
        assert!(Widget::perform_semantic_action(
            &mut ti,
            &target,
            &crate::a11y::Action::Activate,
            &mut crate::widget::Edits::lend(&mut doc),
            &root,
        ));
        assert_eq!(seen.borrow().as_slice(), [true, false]);
        assert_eq!(ti.accessory_pressed(), Some(false));

        ti.set_accessory_pressed(true);
        assert_eq!(ti.accessory_pressed(), Some(true), "the host syncs it");
        assert!(ti.take_dirty().is_none(), "toggling is not a text edit");
    }

    // Case `a_go_accessory_shows_while_editing_and_commits_the_field`.
    {
        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        let sink = seen.clone();
        let mut ti = TextInput::local()
            .with_initial_value("example.org")
            .with_trailing_accessory(
                crate::icons::Icon::ArrowRight,
                "Go",
                AccessoryAction::Submit,
            )
            .on_submit(move |text| sink.borrow_mut().push(text.to_string()));
        ti.apply_layout_external(Rect::new(0.0, 0.0, 240.0, 32.0));
        draw_to_populate(&mut ti);
        let node = Widget::accessibility(&ti).expect("field semantics");
        assert!(node.children.is_empty(), "no go button at rest");

        ti.set_focus(true);
        draw_to_populate(&mut ti);
        let node = Widget::accessibility(&ti).expect("field semantics");
        assert_eq!(node.children.len(), 1, "the go button shows while editing");
        assert_eq!(node.children[0].name.as_deref(), Some("Go"));
        assert_eq!(node.children[0].role, crate::a11y::Role::Button);
        let rect = Widget::synthetic_accessibility_rect(&ti, &[0]).expect("accessory rect");
        let mut doc = AutoCommit::new();
        drive(
            &mut ti,
            &mut doc,
            &cursor_moved(rect.center().x, rect.center().y),
            mods(false, false),
        );
        assert!(drive(&mut ti, &mut doc, &mouse_press(), mods(false, false)));
        assert_eq!(seen.borrow().as_slice(), ["example.org"]);
        assert!(ti.take_submit(), "the polled signal fires too");
        assert_eq!(ti.value(), "example.org", "going is not an edit");
    }

    // Case `a_span_highlighter_colours_the_single_line`.
    {
        let mut ti = TextInput::local()
            .with_initial_value("=A1+B2")
            .with_span_highlighter(|text, theme| {
                text.match_indices("A1")
                    .map(|(at, m)| (at..at + m.len(), theme.tokens.colors.semantic.danger))
                    .collect()
            });
        ti.apply_layout_external(Rect::new(0.0, 0.0, 240.0, 32.0));
        draw_to_populate(&mut ti);
        let spans = &ti.single_attributes.as_ref().expect("highlighted").spans;
        assert_eq!(
            spans.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(),
            ["=", "A1", "+B2"]
        );
        assert!(spans[1].color.is_some() && spans[0].color.is_none());
    }

    // Case `the_painted_field_matches_the_retained_one`.
    {
        let theme = Theme::dark();
        let mut face = Scene::new();
        FieldFace::new(Rect::new(0.0, 0.0, 200.0, 32.0), "")
            .focused(true)
            .paint_chrome(&mut face, Affine::IDENTITY, &theme);
        let mut tr = TextRenderer::new();
        FieldFace::new(Rect::new(0.0, 0.0, 200.0, 32.0), "")
            .placeholder("Search")
            .paint(&mut face, Affine::IDENTITY, &mut tr, &theme);
        let text_rect = FieldFace::new(Rect::new(0.0, 0.0, 200.0, 32.0), "").text_rect(&theme);
        assert!(
            (text_rect.x0 - TextInput::local().text_inset_x(&theme)).abs() < 1e-9,
            "a painted field insets its text like a retained one"
        );
    }
}

#[cfg(feature = "broker-transport")]
#[test]
fn shared_personal_words_protect_autocorrection_and_clear_stale_spelling() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    // The authenticated host it installs is process-wide: every field
    // checking spelling on another thread would read its personal words.
    if !crate::testing::in_own_process(&crate::testing::test_path(
        module_path!(),
        "shared_personal_words_protect_autocorrection_and_clear_stale_spelling",
    )) {
        return;
    }
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            crate::local_identity::install_source(None);
        }
    }
    let _guard = tilo_test_env::lock();
    let _restore = Restore;
    crate::local_identity::install_source(Some(Arc::new(crate::local_identity::FixedIdentity(
        crate::local_identity::LocalIdentity {
            group_public_key: [1; 32],
            display_name: String::new(),
            avatar_color: [0; 3],
        },
    ))));
    let directory = tempfile::tempdir().unwrap();
    let service = Arc::new(
        settings_store::personal_dictionary::Service::new(directory.path().to_owned()).unwrap(),
    );
    let source = Arc::new(settings_store::personal_dictionary::host::Host::new(
        Arc::new(settings_store::HostDocument::new(
            serde_json::Map::new(),
            |_| Ok(()),
        )),
        service,
        Arc::new(|| Some([1; 32])),
    ));
    let mut host =
        crate::personal_dictionary::TestHost::new(source, "so.tilo.notes", [1; 32]).unwrap();
    let field = |text: &str| {
        TextInput::local()
            .with_autocorrect(true)
            .with_spell_language("en_US")
            .with_initial_value(text)
    };
    let mut input = field("teh");
    input.core.set_caret(input.text.len());
    assert!(input.autocorrection_candidate().is_some());
    crate::personal_dictionary::edit(
        0,
        crate::personal_dictionary::Edit::Add(
            crate::personal_dictionary::Word::new("teh", None).unwrap(),
        ),
    )
    .unwrap();
    host.flush().unwrap();
    input.insert_word_boundary(" ");
    assert_eq!(input.value(), "teh ");
    assert!(
        crate::personal_dictionary::edit(1, crate::personal_dictionary::Edit::Reset).is_err(),
        "ordinary apps may add explicit words but cannot reset the dictionary"
    );
    let mut ordinary = field("adress");
    ordinary.core.set_caret(ordinary.text.len());
    ordinary.insert_word_boundary(" ");
    assert_eq!(
        ordinary.value(),
        "address ",
        "personal words never force ordinary replacements"
    );
    crate::personal_dictionary::edit(
        1,
        crate::personal_dictionary::Edit::Add(
            crate::personal_dictionary::Word::new("Élodie", Some("fr")).unwrap(),
        ),
    )
    .unwrap();
    host.flush().unwrap();
    let mut french = field("E\u{301}lodie").with_spell_language("fr_FR");
    french.set_focus(true);
    french.refresh_personal_spelling();
    assert!(french.personal_word("E\u{301}lodie"));
    let mut english = field("Élodie");
    english.set_focus(true);
    english.refresh_personal_spelling();
    assert!(!english.personal_word("Élodie"));
    crate::local_identity::install_source(Some(Arc::new(crate::local_identity::NoLocalIdentity)));
    french.refresh_personal_spelling();
    assert!(!french.personal_word("Élodie"));
    assert!(french.spell_dictionary.as_ref().unwrap().is_empty());
}

#[test]
fn delayed_paste_publishes_once_and_cancels_when_the_target_changes() {
    use crate::clipboard::{ClipboardBackend, ClipboardTarget};
    use std::sync::{Arc, Mutex, mpsc};
    #[derive(Default)]
    struct Delayed(Mutex<Option<mpsc::Sender<Option<String>>>>);
    impl ClipboardBackend for Delayed {
        fn set_text(&self, _: ClipboardTarget, _: &str) -> bool {
            false
        }
        fn text(&self, _: ClipboardTarget) -> Option<String> {
            None
        }
        fn request_text(&self, _: ClipboardTarget) -> mpsc::Receiver<Option<String>> {
            let (send, receive) = mpsc::channel();
            *self.0.lock().unwrap() = Some(send);
            receive
        }
        fn name(&self) -> &'static str {
            "delayed-paste"
        }
    }
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let clipboard = Arc::new(Delayed::default());
    let services = crate::host::HostServices::null().with_clipboard(clipboard.clone());
    let _host = crate::host::enter(&services);
    let changes = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let captured = changes.clone();
    let mut input = TextInput::local()
        .with_initial_value("a")
        .on_change(move |value| captured.borrow_mut().push(value.to_owned()));
    input.set_focus(true);
    let mut document = AutoCommit::new();
    drive(&mut input, &mut document, &char_key("v"), mods(true, false));
    assert_eq!(input.value(), "a");
    clipboard
        .0
        .lock()
        .unwrap()
        .take()
        .unwrap()
        .send(Some("b\nc".into()))
        .unwrap();
    Widget::sync(&mut input, &document, &automerge::ROOT);
    assert_eq!(input.value(), "abc");
    assert_eq!(&*changes.borrow(), &["abc"]);
    Widget::sync(&mut input, &document, &automerge::ROOT);
    assert_eq!(&*changes.borrow(), &["abc"]);

    for change in 0..4 {
        drive(&mut input, &mut document, &char_key("v"), mods(true, false));
        let reply = clipboard.0.lock().unwrap().take().unwrap();
        match change {
            0 => {
                input.set_focus(false);
                input.set_focus(true);
            }
            1 => {
                input.set_cursor_pos(0);
                input.set_cursor_pos(3);
            }
            2 => {
                input.set_text("changed");
                input.set_text("abc");
            }
            _ => {
                input.set_read_only(true);
                let _ = reply.send(Some("stale".into()));
                Widget::sync(&mut input, &document, &automerge::ROOT);
                assert_eq!(
                    input.value(),
                    "abc",
                    "read-only fields reject pending paste"
                );
                input.set_read_only(false);
                Widget::sync(&mut input, &document, &automerge::ROOT);
                assert_eq!(
                    input.value(),
                    "abc",
                    "re-enabling must not revive stale paste"
                );
                continue;
            }
        }
        let _ = reply.send(Some("stale".into()));
        Widget::sync(&mut input, &document, &automerge::ROOT);
        assert_eq!(
            input.value(),
            "abc",
            "changed target must cancel delayed paste"
        );
    }
}
