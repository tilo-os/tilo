//! Words on the deck: speaker notes and text boxes edited on the canvas —
//! each through the deck's dispatcher, so typing is attributed, undone a
//! word at a time, and refused for someone who may only view. Also the help overlay and what the
//! editor tells assistive technology.

use winit::keyboard::ModifiersState;

use super::bench::{Bench, key_event};
use super::*;

/// Press `key` (typing `text`) through the window while `modifiers` are held.
fn press(bench: &mut Bench, modifiers: ModifiersState, key: Key, text: Option<&str>) {
    bench.send(&WindowEvent::ModifiersChanged(Modifiers::from(modifiers)));
    bench.send(&key_event(key.clone(), text, ElementState::Pressed));
    bench.send(&key_event(key, text, ElementState::Released));
    bench.send(&WindowEvent::ModifiersChanged(Modifiers::default()));
}

fn named(bench: &mut Bench, key: NamedKey) {
    press(bench, ModifiersState::empty(), Key::Named(key), None);
}

fn type_chars(bench: &mut Bench, text: &str) {
    for ch in text.chars() {
        let ch = ch.to_string();
        press(
            bench,
            ModifiersState::empty(),
            Key::Character(ch.as_str().into()),
            Some(&ch),
        );
    }
}

fn chord(bench: &mut Bench, modifiers: ModifiersState, ch: &str) {
    press(bench, modifiers, Key::Character(ch.into()), Some(ch));
}

fn press_named(app: &mut SlidesApp, key: NamedKey) -> bool {
    let mut handled = false;
    app.on_key(&Key::Named(key), None, Modifiers::default(), &mut handled);
    handled
}

fn press_char(app: &mut SlidesApp, ch: &str) -> bool {
    let mut handled = false;
    app.on_key(
        &Key::Character(ch.into()),
        Some(ch),
        Modifiers::default(),
        &mut handled,
    );
    handled
}

/// The editor over one slide whose single text box reads `words` and is
/// selected, and the box's id.
fn selected_text(words: &str, align: TextAlign) -> (Bench, Id) {
    let mut s = Slide::new(Layout::Blank);
    let id =
        s.add(Element::text(words, 100.0, 100.0, 600.0, 120.0, 40.0).with(|e| e.align = align));
    let mut bench = Bench::one_slide(s);
    let selected = id.clone();
    bench.with(|app| app.sel = Some(selected));
    bench.focus();
    (bench, id)
}

/// [`selected_text`] with the box open for editing.
fn editing(words: &str, align: TextAlign) -> (Bench, Id) {
    let (mut bench, id) = selected_text(words, align);
    bench.with(|app| app.begin_text_edit(false));
    bench.frame();
    (bench, id)
}

fn text_box(app: &SlidesApp) -> &ui::widget::BlockEditor {
    app.text_editor
        .as_ref()
        .expect("the text box is being edited")
        .host
        .widget()
        .editor()
}

/// The text selected in the text box being edited.
fn edit_selection(app: &SlidesApp) -> String {
    let editor = text_box(app);
    if let Some(text) = editor.selected_text() {
        return text;
    }
    editor
        .blocks()
        .iter()
        .find(|block| block.line.is_focused())
        .and_then(|block| {
            let (start, end) = block.line.selection_range()?;
            block.text().get(start..end).map(str::to_owned)
        })
        .unwrap_or_default()
}

/// The caret's byte offset in the text box's focused paragraph.
fn edit_caret(app: &SlidesApp) -> usize {
    text_box(app)
        .blocks()
        .iter()
        .find(|block| block.line.is_focused())
        .map_or(0, |block| block.line.cursor_pos())
}

fn notes(bench: &Bench) -> String {
    bench.deck().slides[0].notes.clone()
}

/// The editor over one slide whose notes read `words`, editing them.
fn editing_notes(words: &str) -> Bench {
    let mut s = Slide::new(Layout::TitleContent);
    s.notes = words.into();
    let mut bench = Bench::one_slide(s);
    bench.focus();
    bench.with(|app| app.apply_hit(Hit::EditNotes));
    bench.frame();
    bench
}

#[test]
fn help_overlay_and_accessibility_expose_the_deck() {
    // Case `help_overlay_is_visible_and_dismissable_in_present_mode`.
    Bench::sample().with(|app| {
        app.enter_present();
        assert!(!app.help_overlay.is_visible());
        assert!(press_char(app, "?"));
        assert!(
            app.help_overlay.is_visible(),
            "the help overlay is shown in Present mode (the exact gate `draw` uses)"
        );
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        app.help_overlay
            .draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, app.rect);
        // While open it is modal: a nav key is captured, not applied.
        let before = app.cur_slide;
        assert!(press_named(app, NamedKey::ArrowRight));
        assert_eq!(app.cur_slide, before, "help is modal");
        assert!(press_char(app, "?"));
        assert!(!app.help_overlay.is_visible(), "'?' dismisses the overlay");
        assert!(press_named(app, NamedKey::ArrowRight));
        assert_eq!(
            app.cur_slide,
            before + 1,
            "navigation resumes once help is closed"
        );
    });

    // Case `accessibility_exposes_deck_and_current_slide`.
    Bench::sample().with(|app| {
        let tree = app.accessibility().expect("presentation semantics");
        assert_eq!(tree.role, ui::a11y::Role::Document);
        let slides = tree
            .children
            .iter()
            .find(|node| node.role == ui::a11y::Role::List)
            .expect("semantic slide list");
        assert_eq!(slides.children.len(), app.deck.slides.len());
        assert!(slides.children.iter().any(|slide| slide.state.selected));
        assert!(
            slides
                .children
                .iter()
                .all(|slide| slide.action_target.is_some())
        );
    });

    // Case `accessibility_activation_uses_stable_slide_identity_and_modal_help_owns_tree`.
    {
        let mut bench = Bench::sample();
        let target_id = bench.deck().slides[3].id.clone();
        bench
            .app
            .harness()
            .perform_semantic_named(
                "slides-editor",
                Some(&format!("slides.slide:{target_id}")),
                &ui::a11y::Action::Activate,
            )
            .expect("the slide row activates");
        assert_eq!(
            bench.read(|app| app.slide().map(|slide| slide.id.clone())),
            Some(target_id)
        );
        bench.with(|app| {
            app.help_overlay.show();
            let tree = app.accessibility().expect("modal help semantics");
            assert_eq!(tree.role, ui::a11y::Role::Dialog);
            assert!(tree.state.modal);
        });
        bench
            .app
            .harness()
            .perform_semantic_named(
                "slides-editor",
                Some("slides.help.close"),
                &ui::a11y::Action::Activate,
            )
            .expect("the help closes");
        assert!(!bench.read(|app| app.help_overlay.is_visible()));
    }
}

/// Caret placement under alignment and word-wise moves.
#[test]
fn caret_navigation_tracks_alignment_and_lines() {
    // Case `caret_tracks_center_and_right_alignment_not_just_left`.
    {
        let caret_x = |align: TextAlign| {
            let (mut bench, _id) = editing("Hello", align);
            bench.read(|app| {
                assert_eq!(text_box(app).text_align(), render::text_align(align));
                app.ime_cursor_rect().expect("the caret is shown").x0
            })
        };
        let (left, center, right) = (
            caret_x(TextAlign::Left),
            caret_x(TextAlign::Center),
            caret_x(TextAlign::Right),
        );
        assert!(
            left < center && center < right,
            "l={left} c={center} r={right}"
        );
    }

    // Case `ctrl_arrow_moves_word_wise`.
    {
        let (mut bench, _id) = editing("Hello world", TextAlign::Left);
        press(
            &mut bench,
            ModifiersState::CONTROL,
            Key::Named(NamedKey::ArrowLeft),
            None,
        );
        bench.read(|app| {
            assert_eq!(
                edit_caret(app),
                6,
                "ctrl+left jumps to the start of the last word"
            );
            assert!(
                edit_selection(app).is_empty(),
                "a word move collapses the selection"
            );
        });
    }
}

#[test]
fn the_notes_box_reads_out_the_slides_notes() {
    fn find(node: &ui::a11y::AccessNode) -> Option<&ui::a11y::AccessNode> {
        if node.name.as_deref() == Some("Edit speaker notes") {
            return Some(node);
        }
        node.children.iter().find_map(find)
    }
    let mut s = Slide::new(Layout::Blank);
    s.notes = "thank the hosts".into();
    Bench::one_slide(s).with(|app| {
        let tree = app.accessibility().expect("editor semantics");
        let notes = find(&tree).expect("the notes box is exposed");
        assert_eq!(notes.value.as_deref(), Some("thank the hosts"));
    });
}

#[test]
fn notes_typing_edits_the_notes_and_up_down_move_the_caret() {
    let mut bench = editing_notes("line one\nsecond");
    let len = "line one\nsecond".len();
    bench.read(|app| {
        assert!(app.editing_notes, "clicking notes enters the notes editor");
        assert_eq!(
            app.notes.control().cursor_pos(),
            len,
            "caret opens at the end of the notes"
        );
    });
    named(&mut bench, NamedKey::ArrowUp);
    // The second line's final x lies beyond the first line's final glyph.
    // Shaped vertical navigation therefore lands at the first line's end,
    // immediately before its newline, while retaining x for ArrowDown.
    assert_eq!(
        bench.read(|app| app.notes.control().cursor_pos()),
        "line one".len(),
        "ArrowUp reaches the first shaped line's end before its newline"
    );
    named(&mut bench, NamedKey::ArrowDown);
    assert_eq!(
        bench.read(|app| app.notes.control().cursor_pos()),
        len,
        "ArrowDown returns to the second line"
    );
    type_chars(&mut bench, "!");
    assert_eq!(notes(&bench), "line one\nsecond!", "the notes edit landed");
}

/// The text box editor opens, navigates and selects on the widget's own
/// event path; notes selection, IME and multiline edit the slide's notes.
#[test]
fn text_box_keys_and_notes_editing() {
    // Case `enter_opens_the_box_and_escape_ends_editing_keeping_it_selected`.
    {
        let (mut bench, id) = selected_text("Hi", TextAlign::Left);
        assert!(bench.with(|app| press_named(app, NamedKey::Enter)));
        bench.read(|app| {
            assert!(app.editing_text);
            assert_eq!(edit_caret(app), "Hi".len());
        });
        type_chars(&mut bench, "!");
        assert_eq!(bench.deck().slides[0].element(&id).unwrap().text, "Hi!");
        named(&mut bench, NamedKey::Escape);
        bench.read(|app| {
            assert!(!app.editing_text);
            assert_eq!(app.sel.as_deref(), Some(id.as_str()));
        });
    }

    // Case `shift_arrow_extends_a_selection_over_the_text`.
    {
        let (mut bench, _id) = editing("Hi", TextAlign::Left);
        assert!(
            bench.read(|app| edit_selection(app).is_empty()),
            "no selection yet"
        );
        for _ in 0..2 {
            press(
                &mut bench,
                ModifiersState::SHIFT,
                Key::Named(NamedKey::ArrowLeft),
                None,
            );
        }
        assert_eq!(
            bench.read(edit_selection),
            "Hi",
            "the selection spans both chars"
        );
    }

    // Case `ctrl_a_selects_the_whole_box`.
    {
        let (mut bench, _id) = editing("Hi", TextAlign::Left);
        chord(&mut bench, ModifiersState::CONTROL, "a");
        assert_eq!(
            bench.read(edit_selection),
            "Hi",
            "Ctrl+A selects the whole text"
        );
    }

    // Case `notes_selection_ime_and_multiline_edit_the_slides_notes`.
    {
        let mut bench = editing_notes("abc");
        chord(&mut bench, ModifiersState::CONTROL, "a");
        assert_eq!(
            bench.read(|app| app.notes.control().selection_range_public()),
            Some((0, 3)),
            "Ctrl+A selects the notes in the shared editor"
        );
        bench.send(&WindowEvent::Ime(winit::event::Ime::Commit("東京".into())));
        named(&mut bench, NamedKey::Enter);
        type_chars(&mut bench, "agenda");
        assert_eq!(
            notes(&bench),
            "東京\nagenda",
            "IME replacement and newline insertion land in the notes"
        );
        // Each word is one undo step of the notes.
        assert!(bench.undo());
        assert_eq!(notes(&bench), "東京\n");
    }

    // Case `a_viewer_cannot_type_in_the_notes`.
    {
        let mut s = Slide::new(Layout::TitleContent);
        s.notes = "abc".into();
        let mut bench = Bench::open_as(
            &super::bench::deck_of(vec![s]),
            super::bench::WINDOW,
            ui::permissions::Role::Viewer,
        );
        bench.focus();
        bench.with(|app| app.apply_hit(Hit::EditNotes));
        bench.frame();
        type_chars(&mut bench, "nope");
        assert_eq!(notes(&bench), "abc", "a viewer's typing changes nothing");
        assert_eq!(
            bench.read(|app| app.notes.control().value().to_owned()),
            "abc",
            "the notes editor shows the stored notes again"
        );
    }
}

#[test]
fn an_agent_writes_speaker_notes_by_a_field_edit() {
    let mut bench = Bench::one_slide(Slide::new(Layout::Blank));
    let slide = bench.deck().slides[0].id.clone();
    let edit = crate::actions::slide_paths(&slide)
        .notes
        .replace("thank the hosts");
    bench
        .app
        .call("field-edit", serde_json::json!({ "edit": edit }))
        .unwrap();
    assert_eq!(notes(&bench), "thank the hosts");
    assert!(bench.undo());
    assert_eq!(notes(&bench), "");
}

/// Where the starter deck's text box reading `words` is drawn, with the
/// editor open for `role`.
fn editing_starter_box(words: &str, role: ui::permissions::Role) -> (Bench, String) {
    let mut slide = Slide::new(Layout::Blank);
    slide.add(Element::text(words, 96.0, 90.0, 1088.0, 100.0, 54.0));
    slide.add(Element::text("Subtitle", 96.0, 240.0, 1088.0, 100.0, 32.0));
    let mut bench = Bench::open_as(
        &super::bench::deck_of(vec![slide]),
        super::bench::WINDOW,
        role,
    );
    let (id, at) = bench.read(|editor| {
        let id = editor.deck.slides[0]
            .elements
            .iter()
            .find(|element| element.text == words)
            .unwrap()
            .id
            .clone();
        let chrome = editor.compute_chrome();
        let element = editor.deck.slides[0].element(&id).unwrap();
        (
            id,
            render::element_dest(chrome.slide_dest, chrome.scale, element).center(),
        )
    });
    bench.app.harness().double_click_at(at.x, at.y);
    bench.read(|editor| {
        assert!(
            editor.editing_text,
            "physical double-click opens the text box"
        );
        let retained = editor
            .text_editor
            .as_ref()
            .expect("the clicked text box has its retained rich-text editor");
        assert_eq!(
            retained.element_id, id,
            "the retained editor belongs to the physically clicked box"
        );
    });
    // The double-click selects the word under it, as in any text field;
    // the cases below write after the box's words.
    bench.key("End");
    (bench, id)
}

fn element_text(bench: &Bench, id: &str) -> Element {
    bench.deck().slides[0].element(id).unwrap().clone()
}

/// Typing in a text box edits the deck's child rich text: each edit is a
/// deck action, undo takes back a word, a mark is a mark, and a viewer
/// cannot type.
#[test]
fn typing_in_a_text_box_edits_it_through_the_decks_dispatcher() {
    use ui::permissions::Role;
    // Case `typing_edits_the_box_through_the_deck`.
    {
        let (mut bench, id) = editing_starter_box("Title", Role::Owner);
        bench.type_text(" Hi!");
        assert_eq!(element_text(&bench, &id).text, "Title Hi!");
        bench.key("Backspace");
        assert_eq!(element_text(&bench, &id).text, "Title Hi");
        bench.key("Enter");
        bench.type_text("yo");
        assert_eq!(element_text(&bench, &id).text, "Title Hi\nyo");
        assert!(bench.undo(), "the last word is one step");
        assert_eq!(element_text(&bench, &id).text, "Title Hi\n");
    }

    // Case `bold_typed_in_a_box_is_a_mark`.
    {
        let (mut bench, id) = editing_starter_box("Subtitle", Role::Owner);
        bench.type_text(" ");
        bench.chord(&["ctrl"], "b");
        bench.type_text("bold");
        let element = element_text(&bench, &id);
        assert_eq!(element.text, "Subtitle bold");
        assert!(
            element.blocks[0]
                .marks
                .iter()
                .any(|span| span.mark == ui::widget::marks::Mark::Bold && span.range == (9..13)),
            "{:?}",
            element.blocks[0].marks
        );
    }

    // Case `a_viewer_cannot_type_in_a_box`.
    {
        let (mut bench, id) = editing_starter_box("Title", Role::Viewer);
        let before = bench.app.versions().len();
        bench.type_text("nope");
        assert_eq!(element_text(&bench, &id).text, "Title");
        assert_eq!(bench.app.versions().len(), before);
    }
}

/// A text box's words are a rich-text resource of their own beneath their
/// element, which an agent edits as the deck's action.
#[test]
fn an_agent_edits_a_text_box_by_a_field_edit() {
    let (mut bench, _) = editing_starter_box("Title", ui::permissions::Role::Owner);
    let slide = bench.deck().slides[0].id.clone();
    let id = bench.deck().slides[0]
        .elements
        .iter()
        .find(|element| element.text == "Subtitle")
        .unwrap()
        .id
        .clone();
    let edit = crate::actions::element_paths(&slide, &id)
        .body
        .perform(&tilo_document::RichTextAction::ReplaceBody {
            markdown: "from an agent".into(),
        })
        .unwrap();
    bench
        .app
        .call("field-edit", serde_json::json!({ "edit": edit }))
        .unwrap();
    assert_eq!(element_text(&bench, &id).text, "from an agent");
    assert!(bench.undo());
    assert_eq!(element_text(&bench, &id).text, "Subtitle");
}

/// Typing on a selected text box writes words into it, replacing what it
/// held, and never runs the single-letter shortcuts (r inserts a rectangle,
/// n a slide, p presents, l a line).
#[test]
fn typing_on_a_selected_text_box_writes_words_not_shortcuts() {
    let (mut bench, id) = selected_text("Stock words", TextAlign::Left);
    type_chars(&mut bench, "rnpl Quarterly");
    let deck = bench.deck();
    assert_eq!(deck.slides.len(), 1, "no slide was added");
    assert_eq!(deck.slides[0].elements.len(), 1, "no shape was inserted");
    assert_eq!(element_text(&bench, &id).text, "rnpl Quarterly");
    bench.read(|app| {
        assert_eq!(app.mode, Mode::Edit, "the deck is not presenting");
        assert!(app.editing_text);
        assert!(
            app.ime_content().is_some(),
            "the box being edited takes the input method"
        );
    });

    // Text from the input method opens a selected box the same way.
    let (mut bench, id) = selected_text("Old", TextAlign::Left);
    bench.send(&WindowEvent::Ime(winit::event::Ime::Commit("Neu".into())));
    assert_eq!(element_text(&bench, &id).text, "Neu");
}

/// A click on an empty box (one showing its "Add a title" prompt) opens it
/// for typing; a new slide's boxes are empty prompts, so its title is typed
/// rather than appended to stock words.
#[test]
fn an_empty_prompt_box_opens_on_one_click_and_new_slides_prompt() {
    let mut slide = Slide::new(Layout::Blank);
    let id = slide.add(Element::text("", 96.0, 90.0, 1088.0, 100.0, 54.0));
    let mut bench = Bench::one_slide(slide);
    bench.focus();
    let at = bench.read(|app| {
        let chrome = app.compute_chrome();
        let element = app.deck.slides[0].element(&id).unwrap();
        render::element_dest(chrome.slide_dest, chrome.scale, element).center()
    });
    bench.click(at);
    bench.read(|app| assert!(app.editing_text, "one click opens an empty box"));
    type_chars(&mut bench, "Q3");
    assert_eq!(element_text(&bench, &id).text, "Q3");

    let template = actions::new_slide_template();
    let texts: Vec<&Element> = template
        .elements
        .iter()
        .filter(|e| e.kind == ElementKind::Text)
        .collect();
    assert_eq!(texts.len(), 2);
    assert!(
        texts.iter().all(|e| e.is_blank_text()),
        "a new slide's boxes hold no stock words"
    );
    assert_eq!(
        render::prompt_text(&template, texts[0]),
        crate::strings::tr("slides-prompt-title")
    );
}

/// The Text tool's new box opens with its stock word selected: typing
/// replaces it.
#[test]
fn a_new_text_box_opens_ready_to_type() {
    let mut bench = Bench::one_slide(Slide::new(Layout::Blank));
    bench.focus();
    bench.with(|app| app.insert_element(ElementKind::Text));
    bench.frame();
    bench.read(|app| assert!(app.editing_text));
    type_chars(&mut bench, "Hello");
    let deck = bench.deck();
    let texts: Vec<&str> = deck.slides[0]
        .elements
        .iter()
        .map(|e| e.text.as_str())
        .collect();
    assert_eq!(texts, ["Hello"]);
}
