//! A slide's text box edited as a person does it, through the window: the
//! shared editor's caret, selection, input method, clipboard, undo,
//! wrapping, formatting and collaboration, by mouse and by finger.
//!
// Text in a slide's text box behaves as in Writer and Notes: it is the
// shared text editing widget and engine.
#![allow(
    clippy::float_cmp,
    reason = "exact geometry and values the editor computes, not measurements"
)]

use winit::event::TouchPhase;
use winit::keyboard::ModifiersState;

use super::bench::{Bench, key_event};
use super::*;

/// The editor over one slide holding one text box reading `words` at
/// `(100, 100)`, sized `w × h` at type size 40, selected; and the box's id.
fn box_of(words: &str, w: f64, h: f64) -> (Bench, Id) {
    let mut slide = Slide::new(Layout::Blank);
    let id = slide.add(Element::text(words, 100.0, 100.0, w, h, 40.0));
    let mut bench = Bench::one_slide(slide);
    let selected = id.clone();
    bench.with(|app| app.sel = Some(selected));
    bench.focus();
    (bench, id)
}

/// [`box_of`] with the box open for editing, the caret after its words.
fn editing(words: &str) -> (Bench, Id) {
    let (mut bench, id) = box_of(words, 900.0, 120.0);
    bench.with(|app| app.begin_text_edit(false));
    bench.frame();
    (bench, id)
}

fn press(bench: &mut Bench, modifiers: ModifiersState, key: Key, text: Option<&str>) {
    bench.send(&WindowEvent::ModifiersChanged(Modifiers::from(modifiers)));
    bench.send(&key_event(key.clone(), text, ElementState::Pressed));
    bench.send(&key_event(key, text, ElementState::Released));
    bench.send(&WindowEvent::ModifiersChanged(Modifiers::default()));
}

fn named(bench: &mut Bench, modifiers: ModifiersState, key: NamedKey) {
    press(bench, modifiers, Key::Named(key), None);
}

fn chord(bench: &mut Bench, modifiers: ModifiersState, ch: &str) {
    press(bench, modifiers, Key::Character(ch.into()), Some(ch));
}

fn element(bench: &Bench, id: &str) -> Element {
    bench.deck().slides[0].element(id).unwrap().clone()
}

fn words_editor(app: &SlidesApp) -> &ui::widget::BlockEditor {
    app.text_editor
        .as_ref()
        .filter(|editor| editor.focused)
        .expect("the text box is being edited")
        .host
        .widget()
        .editor()
}

/// The words selected in the text box being edited.
fn selection(app: &SlidesApp) -> String {
    let editor = words_editor(app);
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

/// The caret's byte offset in the focused paragraph.
fn caret(app: &SlidesApp) -> usize {
    words_editor(app)
        .blocks()
        .iter()
        .find(|block| block.line.is_focused())
        .map_or(0, |block| block.line.cursor_pos())
}

/// Where the selected box's first line of words starts in the window: the
/// caret's place at the start of its words, read with the box open and
/// left closed again.
fn words_start(bench: &mut Bench) -> Point {
    bench.with(|app| app.begin_text_edit(false));
    bench.frame();
    named(bench, ModifiersState::CONTROL, NamedKey::Home);
    let caret = bench.read(|app| app.ime_cursor_rect().expect("the caret is shown"));
    named(bench, ModifiersState::empty(), NamedKey::Escape);
    bench.read(|app| assert!(!app.editing_text, "Escape closes the box"));
    Point::new(caret.x0, caret.center().y)
}

/// Where the selected box stands on the canvas.
fn frame_of(bench: &mut Bench, id: &str) -> Rect {
    let id = id.to_owned();
    bench.read(|app| {
        let chrome = app.compute_chrome();
        let el = app.slide().unwrap().element(&id).unwrap().clone();
        app.element_frame(chrome.slide_dest, chrome.scale, &el)
    })
}

fn touch(bench: &mut Bench, id: u64, phase: TouchPhase, at: Point) {
    bench.app.harness().touch(id, phase, at.x, at.y);
}

#[test]
fn a_double_click_opens_a_box_at_the_word_and_a_third_selects_its_line() {
    let (mut bench, _) = box_of("Hello world", 900.0, 120.0);
    let start = words_start(&mut bench);
    let at = Point::new(start.x + 10.0, start.y);
    bench.app.harness().double_click_at(at.x, at.y);
    bench.read(|app| {
        assert!(app.editing_text, "a double-click opens the box");
        assert_eq!(
            selection(app),
            "Hello",
            "the double-click selects the word under the pointer, not the end of the box"
        );
    });
    // A click places the caret where it lands.
    bench.app.harness().click_at(start.x + 1.0, start.y);
    bench.read(|app| {
        assert!(app.editing_text);
        assert_eq!(caret(app), 0, "the caret lands at the click");
        assert!(selection(app).is_empty());
    });
    // Three clicks open a box with its line selected.
    let (mut bench, _) = box_of("Hello world", 900.0, 120.0);
    let start = words_start(&mut bench);
    let at = Point::new(start.x + 10.0, start.y);
    bench.app.harness().triple_click_at(at.x, at.y);
    bench.read(|app| {
        assert!(app.editing_text);
        assert_eq!(
            selection(app),
            "Hello world",
            "a triple-click selects the line"
        );
    });
}

#[test]
fn dragging_in_the_words_selects_them_and_leaves_the_box_in_place() {
    let (mut bench, id) = box_of("Hello world", 900.0, 120.0);
    let start = words_start(&mut bench);
    let before = element(&bench, &id);
    bench.with(|app| app.begin_text_edit(false));
    bench.frame();
    bench
        .app
        .harness()
        .drag_at((start.x + 1.0, start.y), (start.x + 400.0, start.y));
    bench.read(|app| {
        assert!(app.editing_text);
        assert!(
            selection(app).starts_with("Hello"),
            "the drag selects from where it started: {:?}",
            selection(app)
        );
    });
    let after = element(&bench, &id);
    assert_eq!(
        (after.x, after.y),
        (before.x, before.y),
        "the box stays put"
    );
}

// A finger held still on a box opens its menu, as a right-click does, and
// moves nothing.
#[test]
fn holding_a_finger_on_a_box_opens_its_menu() {
    let (mut bench, id) = box_of("Hello world", 900.0, 120.0);
    bench.with(|app| app.sel = None);
    let frame = frame_of(&mut bench, &id);
    let before = element(&bench, &id);
    let centre = frame.center();
    bench.app.harness().touch_hold_at(centre.x, centre.y);
    bench.read(|app| {
        assert!(app.ctx_menu.is_open(), "a hold opens the box's menu");
        assert_eq!(app.sel.as_deref(), Some(id.as_str()), "the box is selected");
        assert!(!app.editing_text, "a hold does not open the words");
    });
    let after = element(&bench, &id);
    assert_eq!(
        (after.x, after.y),
        (before.x, before.y),
        "a hold moves nothing"
    );
}

#[test]
fn a_finger_moves_a_box_taps_select_and_a_double_tap_opens_it_at_the_word() {
    let (mut bench, id) = box_of("Hello world", 900.0, 120.0);
    let start = words_start(&mut bench);
    let frame = frame_of(&mut bench, &id);
    let before = element(&bench, &id);

    // A finger drag moves the box with it.
    let centre = frame.center();
    touch(&mut bench, 1, TouchPhase::Started, centre);
    for step in [40.0, 80.0, 120.0] {
        touch(
            &mut bench,
            1,
            TouchPhase::Moved,
            Point::new(centre.x + step, centre.y),
        );
    }
    touch(
        &mut bench,
        1,
        TouchPhase::Ended,
        Point::new(centre.x + 120.0, centre.y),
    );
    let moved = element(&bench, &id);
    assert!(
        moved.x > before.x + 50.0,
        "the box follows the finger: {} → {}",
        before.x,
        moved.x
    );
    assert_eq!(moved.y, before.y);

    // A tap that jitters within a tap's slop selects and never nudges.
    let spot = Point::new(frame_of(&mut bench, &id).center().x + 40.0, centre.y);
    touch(&mut bench, 2, TouchPhase::Started, spot);
    let jitter = Point::new(spot.x + 3.0, spot.y + 2.0);
    touch(&mut bench, 2, TouchPhase::Moved, jitter);
    touch(&mut bench, 2, TouchPhase::Ended, jitter);
    let tapped = element(&bench, &id);
    assert_eq!(
        (tapped.x, tapped.y),
        (moved.x, moved.y),
        "a tap moves nothing"
    );
    bench.read(|app| {
        assert_eq!(app.sel.as_deref(), Some(id.as_str()));
        assert!(
            !app.editing_text,
            "one tap selects, it does not open the box"
        );
    });

    // Two taps on a word open the box with the word selected.
    let dx = moved.x - before.x;
    let scale = frame.width() / before.w;
    let word = Point::new(start.x + dx * scale + 10.0, start.y);
    bench.app.harness().double_tap_at(word.x, word.y);
    bench.read(|app| {
        assert!(app.editing_text, "a double tap opens the box");
        assert_eq!(selection(app), "Hello");
    });

    // In the words, a finger drag selects rather than moving the box.
    let opened = element(&bench, &id);
    let begin = Point::new(start.x + dx * scale + 1.0, start.y);
    touch(&mut bench, 5, TouchPhase::Started, begin);
    touch(
        &mut bench,
        5,
        TouchPhase::Moved,
        Point::new(begin.x + 400.0, begin.y),
    );
    touch(
        &mut bench,
        5,
        TouchPhase::Ended,
        Point::new(begin.x + 400.0, begin.y),
    );
    assert!(bench.read(selection).starts_with("Hello"));
    assert_eq!(element(&bench, &id).x, opened.x, "the box stays put");
}

#[test]
fn shift_arrows_extend_and_word_and_line_keys_move_the_caret() {
    let (mut bench, _) = editing("Hello brave world");
    named(&mut bench, ModifiersState::CONTROL, NamedKey::ArrowLeft);
    assert_eq!(
        bench.read(caret),
        "Hello brave ".len(),
        "ctrl+left: a word back"
    );
    named(
        &mut bench,
        ModifiersState::CONTROL | ModifiersState::SHIFT,
        NamedKey::ArrowLeft,
    );
    assert_eq!(
        bench.read(selection),
        "brave ",
        "ctrl+shift+left selects a word"
    );
    named(&mut bench, ModifiersState::empty(), NamedKey::Home);
    assert_eq!(bench.read(caret), 0, "Home: the start of the line");
    named(&mut bench, ModifiersState::SHIFT, NamedKey::End);
    assert_eq!(
        bench.read(selection),
        "Hello brave world",
        "shift+End selects the line"
    );
    named(&mut bench, ModifiersState::empty(), NamedKey::ArrowRight);
    named(&mut bench, ModifiersState::SHIFT, NamedKey::ArrowLeft);
    named(&mut bench, ModifiersState::SHIFT, NamedKey::ArrowLeft);
    assert_eq!(
        bench.read(selection),
        "ld",
        "shift+left extends a character at a time"
    );
}

#[test]
fn dead_keys_umlauts_and_the_input_method_write_composed_letters() {
    let (mut bench, id) = editing("Gr");
    // A dead key types nothing; the letter it composes is typed.
    press(
        &mut bench,
        ModifiersState::empty(),
        Key::Dead(Some('´')),
        None,
    );
    assert_eq!(
        element(&bench, &id).text,
        "Gr",
        "a dead key alone types nothing"
    );
    press(
        &mut bench,
        ModifiersState::empty(),
        Key::Character("é".into()),
        Some("é"),
    );
    // Umlauts and ß have keys of their own.
    for letter in ["ü", "ß"] {
        press(
            &mut bench,
            ModifiersState::empty(),
            Key::Character(letter.into()),
            Some(letter),
        );
    }
    assert_eq!(element(&bench, &id).text, "Gréüß");
    // An input method composes in place and commits once.
    bench.send(&WindowEvent::Ime(winit::event::Ime::Preedit(
        "¨".into(),
        Some((2, 2)),
    )));
    bench.send(&WindowEvent::Ime(winit::event::Ime::Preedit(
        "ö".into(),
        Some((2, 2)),
    )));
    bench.send(&WindowEvent::Ime(winit::event::Ime::Commit("ö".into())));
    bench.send(&WindowEvent::Ime(winit::event::Ime::Preedit(
        String::new(),
        None,
    )));
    assert_eq!(
        element(&bench, &id).text,
        "Gréüßö",
        "the composition lands once"
    );
    bench.read(|app| {
        assert!(
            app.ime_content().is_some(),
            "the box takes the input method"
        );
        assert!(app.ime_multiline(), "and the lines it commits");
    });

    // A dead key on a box that is only selected waits for its letter, which
    // opens the box and replaces its words.
    let (mut bench, id) = box_of("Alt", 900.0, 120.0);
    press(
        &mut bench,
        ModifiersState::empty(),
        Key::Dead(Some('^')),
        None,
    );
    press(
        &mut bench,
        ModifiersState::empty(),
        Key::Character("â".into()),
        Some("â"),
    );
    assert_eq!(element(&bench, &id).text, "â");
    bench.read(|app| assert!(app.editing_text));
}

#[test]
fn copy_cut_and_paste_move_words_through_the_clipboard() {
    let (mut bench, id) = editing("Hello world");
    named(
        &mut bench,
        ModifiersState::CONTROL | ModifiersState::SHIFT,
        NamedKey::ArrowLeft,
    );
    assert_eq!(bench.read(selection), "world");
    chord(&mut bench, ModifiersState::CONTROL, "c");
    named(&mut bench, ModifiersState::CONTROL, NamedKey::Home);
    chord(&mut bench, ModifiersState::CONTROL, "v");
    assert_eq!(
        element(&bench, &id).text,
        "worldHello world",
        "paste at the caret"
    );
    named(&mut bench, ModifiersState::CONTROL, NamedKey::End);
    named(
        &mut bench,
        ModifiersState::CONTROL | ModifiersState::SHIFT,
        NamedKey::ArrowLeft,
    );
    chord(&mut bench, ModifiersState::CONTROL, "x");
    assert_eq!(
        element(&bench, &id).text,
        "worldHello ",
        "cut removes the words"
    );
    named(&mut bench, ModifiersState::CONTROL, NamedKey::Home);
    chord(&mut bench, ModifiersState::CONTROL, "v");
    assert_eq!(element(&bench, &id).text, "worldworldHello ");
}

#[test]
fn undo_takes_back_a_word_at_a_time_and_redo_brings_it_back() {
    let (mut bench, id) = editing("Title");
    bench.type_text(" brave new");
    assert_eq!(element(&bench, &id).text, "Title brave new");
    assert!(bench.undo());
    assert_eq!(
        element(&bench, &id).text,
        "Title brave ",
        "one undo: one word"
    );
    assert!(bench.undo());
    assert_eq!(
        element(&bench, &id).text,
        "Title ",
        "the next: the word before"
    );
    assert!(bench.app.redo());
    assert_eq!(
        element(&bench, &id).text,
        "Title brave ",
        "redo brings it back"
    );
    bench.read(|app| assert!(app.editing_text, "undo leaves the box open"));
}

#[test]
fn words_wrap_inside_the_box_and_the_box_grows_to_hold_them() {
    let (mut bench, id) = box_of("Short", 300.0, 60.0);
    bench.with(|app| app.begin_text_edit(false));
    bench.frame();
    let before = frame_of(&mut bench, &id);
    bench.type_text(" words that cannot fit on one line of this box");
    bench.frame();
    let stored = element(&bench, &id);
    let after = frame_of(&mut bench, &id);
    assert_eq!(
        after.width(),
        before.width(),
        "the words wrap at the box's width"
    );
    assert!(
        after.height() > before.height() * 2.0,
        "the box grows to hold its lines: {before:?} → {after:?}"
    );
    assert_eq!(
        (stored.w, stored.h),
        (300.0, 60.0),
        "growing writes nothing: typing stays word edits"
    );
    // The grown part is the box's: a press there lands in the words.
    let low = Point::new(after.center().x, after.y1 - 4.0);
    bench.click(low);
    bench.read(|app| {
        assert!(app.editing_text, "a press in the grown box stays in it");
        assert_eq!(app.sel.as_deref(), Some(id.as_str()));
    });
    // The box shows grown when it is no longer edited.
    named(&mut bench, ModifiersState::empty(), NamedKey::Escape);
    bench.frame();
    let shown = frame_of(&mut bench, &id);
    assert!(
        (shown.height() - after.height()).abs() < 1.0,
        "{shown:?} vs {after:?}"
    );
}

#[test]
fn the_toolbar_formats_the_selected_words_through_the_shared_editor() {
    use ui::widget::marks::Mark;
    let (mut bench, id) = editing("Hello world");
    named(
        &mut bench,
        ModifiersState::CONTROL | ModifiersState::SHIFT,
        NamedKey::ArrowLeft,
    );
    bench.with(|app| app.apply_hit(Hit::ToggleBold));
    let bold = element(&bench, &id);
    assert_eq!(
        bold.blocks[0]
            .marks
            .iter()
            .filter(|span| span.mark == Mark::Bold)
            .map(|span| span.range.clone())
            .collect::<Vec<_>>(),
        vec![6..11],
        "Bold marks the selected word, not the box"
    );
    bench.read(|app| assert!(app.mark_on(&Mark::Bold), "the button shows it on"));
    bench.with(|app| app.apply_hit(Hit::ToggleItalic));
    assert!(
        element(&bench, &id).blocks[0]
            .marks
            .iter()
            .any(|span| span.mark == Mark::Italic && span.range == (6..11))
    );
    bench.with(|app| app.apply_hit(Hit::ToggleList(false)));
    assert!(matches!(
        element(&bench, &id).blocks[0].kind,
        ui::widget::BlockKind::Bullet { .. }
    ));
    bench.read(|app| assert!(app.list_on(false)));
    bench.with(|app| app.apply_hit(Hit::ToggleList(true)));
    assert!(matches!(
        element(&bench, &id).blocks[0].kind,
        ui::widget::BlockKind::Numbered { .. }
    ));
    bench.with(|app| app.apply_hit(Hit::ToggleList(true)));
    assert_eq!(
        element(&bench, &id).blocks[0].kind,
        ui::widget::BlockKind::Paragraph,
        "the list's button again: a paragraph"
    );
    // A format is one undo step.
    bench.with(|app| app.apply_hit(Hit::ToggleList(false)));
    assert!(bench.undo());
    assert_eq!(
        element(&bench, &id).blocks[0].kind,
        ui::widget::BlockKind::Paragraph
    );
}

#[test]
fn a_shortcut_formats_every_paragraph_the_selection_spans() {
    use ui::widget::marks::Mark;
    let (mut bench, id) = editing("First");
    named(&mut bench, ModifiersState::empty(), NamedKey::Enter);
    bench.type_text("Second");
    chord(&mut bench, ModifiersState::CONTROL, "a");
    chord(&mut bench, ModifiersState::CONTROL, "b");
    let el = element(&bench, &id);
    assert_eq!(el.blocks.len(), 2);
    for block in &el.blocks {
        assert!(
            block
                .marks
                .iter()
                .any(|span| span.mark == Mark::Bold && span.range == (0..block.text.len())),
            "{:?} is bold: {:?}",
            block.text,
            block.marks
        );
    }
    chord(&mut bench, ModifiersState::CONTROL, "b");
    assert!(
        element(&bench, &id)
            .blocks
            .iter()
            .all(|block| block.marks.iter().all(|span| span.mark != Mark::Bold)),
        "again: off everywhere"
    );
}

#[test]
fn a_selected_box_is_formatted_whole_from_the_toolbar() {
    let (mut bench, id) = editing("One");
    named(&mut bench, ModifiersState::empty(), NamedKey::Enter);
    bench.type_text("Two");
    named(&mut bench, ModifiersState::empty(), NamedKey::Escape);
    bench.with(|app| app.apply_hit(Hit::ToggleList(true)));
    let el = element(&bench, &id);
    assert!(
        el.blocks
            .iter()
            .all(|block| matches!(block.kind, ui::widget::BlockKind::Numbered { .. })),
        "{:?}",
        el.blocks
    );
    bench.with(|app| app.apply_hit(Hit::ToggleList(true)));
    assert!(
        element(&bench, &id)
            .blocks
            .iter()
            .all(|block| block.kind == ui::widget::BlockKind::Paragraph)
    );
}

#[test]
fn a_collaborators_edits_merge_with_typing_letter_by_letter() {
    use tilo_document::RichTextAction;
    use tilo_document::action::RegionKind;
    let mut slide = Slide::new(Layout::Blank);
    let mine = slide.add(Element::text("Title", 96.0, 90.0, 1088.0, 100.0, 54.0));
    let theirs = slide.add(Element::text("Subtitle", 96.0, 240.0, 1088.0, 100.0, 32.0));
    let mut bench = Bench::one_slide(slide);
    let selected = mine.clone();
    bench.with(|app| app.sel = Some(selected));
    bench.focus();
    bench.with(|app| app.begin_text_edit(false));
    bench.frame();
    let slide_id = bench.deck().slides[0].id.clone();
    let block = |bench: &Bench, id: &str| element(bench, id).blocks[0].id.clone();
    let (my_block, their_block) = (block(&bench, &mine), block(&bench, &theirs));

    // The collaborator's replica, from before this person types.
    let mut peer =
        ui::tilo_resource::Replica::<crate::deck::Deck>::from_doc(bench.app.peer_document())
            .as_author("collaborator");
    bench.type_text(" one");

    // Meanwhile they write in the other box, and take a letter out of the
    // word this person is typing after.
    let splice = |element: &str, block: String, at: usize, del: usize, ins: &str| {
        crate::actions::edit_text_action(
            &slide_id,
            element,
            RichTextAction::SpliceText {
                region: RegionKind::Body,
                block,
                at,
                del,
                ins: ins.into(),
            },
        )
    };
    peer.apply_action(&splice(&theirs, their_block, 0, 0, "Their "))
        .unwrap();
    peer.apply_action(&splice(&mine, my_block, 1, 1, ""))
        .unwrap();
    bench.app.merge_peer(peer.doc_mut()).unwrap();

    bench.type_text(" two");
    assert_eq!(
        element(&bench, &theirs).text,
        "Their Subtitle",
        "their box holds their words"
    );
    assert_eq!(
        element(&bench, &mine).text,
        "Ttle one two",
        "their deletion survives this person's typing, which spliced letters in rather than rewriting the words, and the caret stayed where they typed"
    );
    bench.read(|app| {
        assert!(app.editing_text, "the box stays open through their edit");
        assert_eq!(app.sel.as_deref(), Some(mine.as_str()));
    });
}

#[test]
fn the_words_being_edited_show_the_text_beam() {
    let (mut bench, id) = editing("Hello");
    let frame = frame_of(&mut bench, &id);
    bench.read(|app| {
        assert_eq!(
            app.cursor_shape(frame.center()),
            Some(winit::window::CursorIcon::Text)
        );
    });
}

/// The words being edited are in the accessibility tree where they are
/// drawn, under their box, and an assistive technology writes them as
/// typing does.
#[test]
fn assistive_technology_reads_and_writes_the_words_being_edited() {
    let (mut bench, id) = editing("Hello world");
    let frame = bench.read(|app| app.text_edit_box);
    let (node, bounds) = bench
        .app
        .harness()
        .a11y_nodes()
        .into_iter()
        .find(|(_, node)| node.value() == Some("Hello world"))
        .map(|(node_id, node)| (node_id, node.bounds()))
        .expect("the words are in the tree");
    let bounds = bounds.expect("the words say where they are");
    assert!(
        frame
            .inflate(1.0, 1.0)
            .contains(Point::new(bounds.x0, bounds.y0))
            && frame
                .inflate(1.0, 1.0)
                .contains(Point::new(bounds.x1, bounds.y0)),
        "the words are where the box draws them: {bounds:?} in {frame:?}"
    );
    bench
        .app
        .harness()
        .perform_a11y_action(node, &ui::a11y::Action::SetValue("Hallo Welt".into()))
        .expect("the words take a new value");
    bench.frame();
    assert_eq!(element(&bench, &id).text, "Hallo Welt");
}

/// The first paragraph's document id of box `id`: what collaborators name
/// a place in its words by.
fn first_paragraph(bench: &Bench, id: &str) -> String {
    let blocks = element(bench, id).blocks;
    let first = blocks.first().expect("the box has paragraphs");
    assert!(!first.id.is_empty(), "the paragraph keeps its document id");
    first.id.clone()
}

/// How many paths the last frame drew.
fn drawn_paths(bench: &mut Bench) -> usize {
    bench.frame();
    bench.app.harness().scene().encoding().path_tags.len()
}

// A collaborator's caret shows in the box where they are, whether or not
// this person edits it, and this person's caret is published for theirs.
#[test]
fn collaborators_carets_show_in_the_boxes_they_are_in() {
    let (mut bench, id) = box_of("Hello world", 900.0, 120.0);
    let key = render::words_presence_key(&first_paragraph(&bench, &id));
    let alone = drawn_paths(&mut bench);
    let mut presence = ui::PresenceCtx::empty();
    presence.set_remote_caret("ana".into(), key.clone(), 3);
    bench.app.harness().set_presence_context(presence.clone());
    assert!(
        drawn_paths(&mut bench) > alone,
        "Ana's caret shows in the box shown, not edited, here"
    );

    bench.with(|app| app.begin_text_edit(false));
    let editing_with_ana = drawn_paths(&mut bench);
    bench
        .app
        .harness()
        .set_presence_context(ui::PresenceCtx::empty());
    assert!(
        editing_with_ana > drawn_paths(&mut bench),
        "and in the box being edited here"
    );

    named(&mut bench, ModifiersState::CONTROL, NamedKey::Home);
    named(&mut bench, ModifiersState::empty(), NamedKey::ArrowRight);
    let published = bench.read(ui::presence::capability::presence_payload);
    assert_eq!(
        published,
        Some((key, ui::PresencePayload::Caret { pos: 1 })),
        "this person's caret is published where it is"
    );
}

/// Drag the pointer from `from` by `by` over `moves` frames, as a hand
/// does, and lift it; each move is its own frame. The frames' timings.
fn drag_frames(
    bench: &mut Bench,
    from: Point,
    by: (f64, f64),
    moves: u32,
) -> Vec<ui::profiling::FrameTiming> {
    bench.move_to(from);
    bench.press();
    bench.app.harness().begin_profiling();
    let _ = bench.app.harness().take_text_shape_stats();
    for step in 1..=moves {
        let t = f64::from(step) / f64::from(moves);
        bench.move_to(Point::new(from.x + by.0 * t, from.y + by.1 * t));
        let _ = bench.app.harness().poll_frame();
    }
    let samples = bench.app.harness().take_profile_samples();
    bench.release();
    samples
}

fn report(what: &str, samples: &[ui::profiling::FrameTiming], shapes: (u64, u64, u128)) {
    let n = u64::try_from(samples.len().max(1)).unwrap();
    let sum =
        |field: fn(&ui::profiling::FrameTiming) -> u64| samples.iter().map(field).sum::<u64>() / n;
    eprintln!(
        "slides {what}: frames={} mean total_us={} layout_us={} draw_us={} event_us={} \
         max total_us={}; shaping hits={} misses={} ns={}",
        samples.len(),
        sum(|f| f.total_us),
        sum(|f| f.build_layout_us + f.apply_layout_us),
        sum(|f| f.draw_us),
        sum(|f| f.handle_event_us),
        samples.iter().map(|f| f.total_us).max().unwrap_or(0),
        shapes.0,
        shapes.1,
        shapes.2,
    );
}

/// Once input stops, nothing asks for another frame: no queued work, no
/// animation, and no timer left once the ones due have run, as the live
/// window idles.
fn assert_idle(bench: &mut Bench, what: &str) {
    // What the input left runs out in a few wakes, each a frame of the live
    // window: the woken component taken up, and the save status moving
    // along once the record's store says the edit the gesture wrote has
    // landed. Then nothing wakes the window.
    let mut wakes = Vec::new();
    for _ in 0..12 {
        bench.app.idle();
        let schedule = bench.app.harness().frame_schedule();
        if !schedule.queued_work
            && !schedule.needs_animation_frame
            && schedule.next_wake_deadline.is_none()
        {
            eprintln!("{what}: idle after {} wakes: {wakes:?}", wakes.len());
            assert!(
                wakes.len() <= 6,
                "{what}: woke {} times: {wakes:?}",
                wakes.len()
            );
            return;
        }
        wakes.push(format!(
            "{:?} {:?}",
            bench.app.harness().queued_work_sources(),
            schedule
                .next_wake_deadline
                .map(|due| due.saturating_duration_since(ui::time::now()))
        ));
        if let Some(due) = schedule.next_wake_deadline {
            let wait = due.saturating_duration_since(ui::time::now());
            if wait < std::time::Duration::from_secs(1) {
                std::thread::sleep(wait);
            } else {
                // A far timer (the save status's stall notice) only stands
                // by: the window parks on it, and what it waits for (the
                // record's store saying the write landed) wakes it first.
                assert!(
                    bench.app.harness().park_until(due),
                    "{what}: the window slept through to a far timer, nothing woke it: {schedule:?}"
                );
            }
        }
    }
    panic!("{what}: the window keeps waking: {wakes:?}");
}

// Moving a text box shapes none of its words again, resizing it only
// re-wraps them, and once the hand stops no frame runs.
#[test]
fn moving_and_resizing_a_text_box_reshapes_nothing_and_idles_after() {
    let words = "Quarterly goals: ship the new editor, grow the team, and keep \
                 every frame at the display's own refresh rate";
    let (mut bench, id) = box_of(words, 700.0, 120.0);
    bench.frame();
    let frame = frame_of(&mut bench, &id);
    // Warm the words' shapes and the drag's own code once.
    drag_frames(&mut bench, frame.center(), (8.0, 4.0), 2);
    bench.app.idle();

    let frame = frame_of(&mut bench, &id);
    let moved = drag_frames(&mut bench, frame.center(), (240.0, 120.0), 60);
    let shapes = bench.app.harness().take_text_shape_stats();
    report("move", &moved, shapes);
    assert!(moved.len() >= 60, "each move is a frame");
    assert_eq!(shapes.1, 0, "moving a box shapes none of its words again");

    assert_idle(&mut bench, "once the hand stops, no frame runs");

    let corner = bench.read(|app| {
        let chrome = app.compute_chrome();
        let el = app.selected_element().expect("the box stays selected");
        app.handles_of(chrome.slide_dest, chrome.scale, el)
            .into_iter()
            .find(|(handle, _)| *handle == Handle::Br)
            .expect("a selected box has a corner handle")
            .1
            .center()
    });
    let resized = drag_frames(&mut bench, corner, (-160.0, 60.0), 60);
    let shapes = bench.app.harness().take_text_shape_stats();
    report("resize", &resized, shapes);
    assert!(resized.len() >= 60, "each resize step is a frame");
    let before = element(&bench, &id);
    assert!(before.w < 700.0, "the drag resized the box");

    assert_idle(&mut bench, "once the hand stops resizing, no frame runs");
}

/// The secondary button going down or up where the pointer is.
fn secondary(bench: &mut Bench, state: ElementState) {
    bench.send(&WindowEvent::MouseInput {
        device_id: winit::event::DeviceId::dummy(),
        state,
        button: MouseButton::Right,
    });
}

/// Where byte `byte` of the words being edited stands on the canvas: just
/// right of the caret placed there.
fn point_at_byte(bench: &mut Bench, byte: usize) -> Point {
    named(bench, ModifiersState::CONTROL, NamedKey::Home);
    for _ in 0..byte {
        named(bench, ModifiersState::empty(), NamedKey::ArrowRight);
    }
    let caret = bench.read(|app| app.ime_cursor_rect().expect("the caret is shown"));
    Point::new(caret.x1 + 3.0, caret.center().y)
}

/// The rows of the words' open menu, each with where it stands on the
/// canvas, and the menu's own size there and in the editor's space.
fn words_menu(bench: &mut Bench) -> (Vec<(String, Rect)>, Rect, Rect) {
    bench.read(|app| {
        let host = &app.text_editor.as_ref().expect("an editor").host;
        let menu = words_editor(app)
            .text_menu()
            .expect("the words' menu is open");
        let own = Widget::modal_capture(menu).expect("an open menu has a box");
        let shown = host
            .modal_capture()
            .expect("the editor reports its open menu");
        let offset = shown.origin() - own.origin();
        let rows = menu
            .items()
            .iter()
            .enumerate()
            .filter_map(|(index, item)| {
                Some((item.label()?.to_owned(), menu.item_rect(index)? + offset))
            })
            .collect();
        (rows, shown, own)
    })
}

fn row(rows: &[(String, Rect)], label: &str) -> Point {
    rows.iter()
        .find(|(text, _)| text == label)
        .unwrap_or_else(|| panic!("the menu has {label}: {rows:?}"))
        .1
        .center()
}

#[test]
fn right_click_or_a_held_finger_opens_the_words_menu() {
    // Case `right_click_selects_the_word_and_formats_it`.
    {
        let (mut bench, id) = editing("Hello world");
        let at = point_at_byte(&mut bench, 8);
        bench.move_to(at);
        secondary(&mut bench, ElementState::Pressed);
        secondary(&mut bench, ElementState::Released);
        bench.read(|app| {
            assert!(app.words_menu_open(), "the words' menu opens");
            assert!(!app.ctx_menu.is_open(), "not the box's menu");
            assert!(app.editing_text, "the words stay open");
        });
        assert_eq!(
            bench.read(selection),
            "world",
            "the word clicked is selected"
        );
        let (rows, shown, own) = words_menu(&mut bench);
        let labels: Vec<&str> = rows.iter().map(|(text, _)| text.as_str()).collect();
        for expected in [
            "Cut",
            "Copy",
            "Paste",
            "Select all",
            "Bold",
            "Italic",
            "Bulleted list",
        ] {
            assert!(labels.contains(&expected), "{expected} in {labels:?}");
        }
        assert!(
            (shown.width() - own.width()).abs() < 1e-6
                && (shown.height() - own.height()).abs() < 1e-6,
            "the menu is shown at the window's scale, not the slide's: {shown:?} for {own:?}"
        );
        assert!(
            (shown.x0 - at.x).abs() < 1.0 || shown.x1 <= at.x + 1.0,
            "the menu opens where the click was: {shown:?} at {at:?}"
        );
        bench.click(row(&rows, "Bold"));
        bench.read(|app| assert!(!app.words_menu_open(), "choosing closes the menu"));
        let words = element(&bench, &id);
        let bold = words
            .rich_blocks()
            .first()
            .expect("a paragraph")
            .marks
            .iter()
            .any(|span| span.mark == ui::widget::marks::Mark::Bold && span.range == (6..11));
        assert!(bold, "the clicked word is bold: {:?}", words.rich_blocks());
    }
    // Case `the_menu_cuts_the_selection_through_the_clipboard`.
    {
        let (mut bench, id) = editing("Hello world");
        let at = point_at_byte(&mut bench, 2);
        bench.move_to(at);
        secondary(&mut bench, ElementState::Pressed);
        secondary(&mut bench, ElementState::Released);
        let (rows, _, _) = words_menu(&mut bench);
        bench.click(row(&rows, "Cut"));
        assert_eq!(
            element(&bench, &id).text,
            " world",
            "cut takes the word away"
        );
        assert_eq!(
            ui::text_selection::clipboard_paste().as_deref(),
            Some("Hello"),
            "and onto the clipboard"
        );
    }
    // Case `a_held_finger_opens_it_and_a_tap_chooses`.
    {
        let (mut bench, id) = editing("Hello world");
        let at = point_at_byte(&mut bench, 8);
        bench.app.harness().touch_hold_at(at.x, at.y);
        bench.read(|app| {
            assert!(app.words_menu_open(), "a held finger opens the words' menu");
            assert!(!app.ctx_menu.is_open(), "not the box's menu");
        });
        let (rows, _, _) = words_menu(&mut bench);
        let italic = row(&rows, "Italic");
        bench.app.harness().tap_at(italic.x, italic.y);
        bench.read(|app| assert!(!app.words_menu_open(), "the tap chose"));
        let words = element(&bench, &id);
        let italic = words
            .rich_blocks()
            .first()
            .expect("a paragraph")
            .marks
            .iter()
            .any(|span| span.mark == ui::widget::marks::Mark::Italic && span.range == (6..11));
        assert!(italic, "the held word is italic: {:?}", words.rich_blocks());
    }
}
