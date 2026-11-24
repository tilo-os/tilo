//! A slide's text box as the slides app composes it: the shared rich editor inside a stage pin,
//! on a slide-sized page, driven through the frame host the way a person's hand and keyboard
//! drive it. The app adds only the box's place and its look; every key, drag and composition
//! here is the shared editor's, so these tests say what a slide box does and not what it might.

use tilo_rich_edit::BlockEditor;
use tilo_ui::component::{Described, Event, Fired, Intent, Value};
use tilo_ui::headless::FrameHarness;
use tilo_ui::widget::{Stage, StagePinProps, StageProps, Widget};
use tilo_ui::{
    Intents, State, Theme, accesskit, collaboration, font_catalog, synthetic_input, text_selection,
    time, winit,
};

const DEVICE: (u32, u32) = (800, 600);
/// The page's own size, as a slide's is.
const PAGE: (i64, i64) = (1280, 720);
/// Where the box stands on the page, and how large it is.
const BOX: (i64, i64, i64, i64) = (96, 90, 600, 160);
/// The box's words start here.
const WORDS: &str = "The rail is there";

/// The scale a fitted 1280 × 720 page takes in an 800 × 600 window, and where its top-left
/// corner lands.
fn fitted() -> (f64, f64, f64) {
    let scale = (f64::from(DEVICE.0) / PAGE.0 as f64).min(f64::from(DEVICE.1) / PAGE.1 as f64);
    let left = (f64::from(DEVICE.0) - PAGE.0 as f64 * scale) / 2.0;
    let top = (f64::from(DEVICE.1) - PAGE.1 as f64 * scale) / 2.0;
    (scale, left, top)
}

/// A stage holding one text box of `markdown`, as a slide holds one, with the queue its parts
/// report to.
fn slide_box(markdown: &str) -> (FrameHarness, Intents<Fired>) {
    let queue = Intents::default();
    let stage = Stage::from_props(
        "stage",
        StageProps {
            name: Some("Slide".into()),
            canvas: vec![PAGE.0, PAGE.1],
            pins: vec![StagePinProps {
                key: "box-0".into(),
                name: Some("Title".into()),
                item: Some("box-0".into()),
                x: Some(BOX.0),
                y: Some(BOX.1),
                width: Some(BOX.2),
                height: Some(BOX.3),
                moves: true,
                menu: Vec::new(),
                body: vec![Box::new(BlockEditor::from_markdown(markdown))],
            }],
            select: Event::View(Intent::new(&queue, "stage", "select")),
            open: Event::View(Intent::new(&queue, "stage", "open")),
            moved: Event::View(Intent::new(&queue, "stage", "moved")),
            ..StageProps::default()
        },
    );
    let host = FrameHarness::new_with_controlled_inputs(
        State::new(),
        Box::new(stage),
        DEVICE,
        Theme::dark(),
        font_catalog::fixture_renderer(),
        collaboration::local_user_id(),
    );
    (host, queue)
}

/// The box's editor, the widget the stage places on its page.
fn editor(host: &mut FrameHarness) -> &mut BlockEditor {
    use tilo_ui::component::PartViews;
    host.root_widget_mut()
        .as_any_mut()
        .and_then(|any| any.downcast_mut::<Stage>())
        .expect("the root is the stage")
        .view_mut(0)
        .expect("the stage holds the box")
        .as_any_mut()
        .and_then(|any| any.downcast_mut::<BlockEditor>())
        .expect("the place's body is the editor")
}

/// Where the box's words are on the screen: a point inside the page's rectangle for the box,
/// near its first word.
fn in_the_words() -> (f64, f64) {
    let (scale, left, top) = fitted();
    let x = left + (BOX.0 as f64 + 40.0) * scale;
    let y = top + (BOX.1 as f64 + 20.0) * scale;
    (x, y)
}

/// Click in the box's words, as a person does to start typing there.
fn click_in(host: &mut FrameHarness) {
    let (x, y) = in_the_words();
    host.click_at(x, y);
}

/// What the person has selected in the box: a range across blocks, or else the words the line
/// under the caret selected within itself — both are selections the person sees.
fn selected_words(host: &mut FrameHarness) -> Option<String> {
    let editor = editor(host);
    if let Some(text) = editor.selected_text() {
        return Some(text);
    }
    editor
        .blocks()
        .iter()
        .find(|block| block.line.is_focused())
        .and_then(|block| {
            let (start, end) = block.line.selection_range()?;
            block
                .text()
                .get(start.min(end)..start.max(end))
                .map(str::to_owned)
        })
}

#[test]
fn a_slide_box_takes_the_caret_keys_a_person_moves_it_with() {
    let (mut host, _queue) = slide_box(WORDS);
    click_in(&mut host);
    assert!(
        editor(&mut host).is_focused(),
        "a click in the box puts the keyboard in it"
    );

    // Typing writes where the caret stands.
    host.type_text(" today");
    assert_eq!(
        editor(&mut host).blocks()[0].text(),
        format!("{WORDS} today"),
        "the words are the box's own"
    );

    // Home and End go to the ends of the line; Shift with them selects what was crossed.
    host.key("Home");
    assert_eq!(editor(&mut host).blocks()[0].line.cursor_pos(), 0);
    host.key("Shift+End");
    assert_eq!(
        selected_words(&mut host).as_deref(),
        Some("The rail is there today"),
        "Shift and End select the whole line"
    );

    // Shift and an arrow extend by one letter; Ctrl and an arrow move by a word.
    host.key("Home");
    host.key("Shift+ArrowRight");
    assert_eq!(
        selected_words(&mut host).as_deref(),
        Some("T"),
        "Shift and an arrow extend by one letter"
    );
    host.key("ArrowRight");
    host.key("Ctrl+ArrowRight");
    let after_word = editor(&mut host).blocks()[0].line.cursor_pos();
    assert_eq!(
        after_word, 4,
        "the command key and an arrow move to the next word's start"
    );
}

#[test]
fn a_slide_box_selects_by_dragging_and_takes_and_gives_words_through_the_clipboard() {
    let (mut host, queue) = slide_box(WORDS);
    let (scale, left, top) = fitted();
    // The words start at the line's leading edge, past the box's own inset, and stand on the
    // line's middle: that is where a hand presses to drag across them, not on its border.
    let (words_x, words_y) = {
        let line = editor(&mut host).blocks()[0].line.get_rect();
        (line.x0, line.center().y)
    };
    let point = |along: f64| (left + (words_x + along) * scale, top + words_y * scale);

    // A drag across the first words selects them and leaves the box where it stands.
    let (x, y) = point(1.0);
    host.dispatch_to_root(&synthetic_input::cursor_moved(x, y));
    host.dispatch_to_root(&synthetic_input::mouse_press());
    let (x, y) = point(68.0);
    host.dispatch_to_root(&synthetic_input::cursor_moved(x, y));
    host.dispatch_to_root(&synthetic_input::mouse_release());
    let selected = selected_words(&mut host).expect("a drag across words selects them");
    assert!(
        selected.starts_with("The rail"),
        "the drag selected the words it crossed: {selected:?}"
    );
    assert!(
        fired(&queue, "moved").is_empty(),
        "a drag in the words leaves the box where it stands"
    );

    // Cut takes them off, and paste puts them back at the caret.
    text_selection::clipboard_copy("word");
    host.key("End");
    host.key("Ctrl+V");
    assert_eq!(
        editor(&mut host).blocks()[0].text(),
        format!("{WORDS}word"),
        "a paste writes the clipboard's words at the caret"
    );
}

#[test]
fn a_slide_box_takes_composed_letters_and_reads_to_a_reader() {
    use winit::event::Ime;

    let (mut host, _queue) = slide_box("");
    click_in(&mut host);
    // An input method composes a letter under the caret and then commits it: "u" typed for the
    // umlaut it becomes.
    host.dispatch_to_root(&winit::event::WindowEvent::Ime(Ime::Preedit(
        "u".into(),
        None,
    )));
    host.dispatch_to_root(&winit::event::WindowEvent::Ime(Ime::Commit("ü".into())));
    assert_eq!(
        editor(&mut host).blocks()[0].text(),
        "ü",
        "the composed letter is the box's word"
    );

    // A person reading the box with assistive technology reads the words it holds.
    let values: Vec<String> = host
        .a11y_nodes()
        .into_iter()
        .filter(|(_, node)| node.role() == accesskit::Role::TextInput)
        .filter_map(|(_, node)| node.value().map(str::to_owned))
        .collect();
    assert!(
        values.iter().any(|value| value == "ü"),
        "the editor the stage places reads as the words it holds: {values:?}"
    );
}

#[test]
fn a_slide_box_formats_the_words_a_person_selected_with_the_chord() {
    let (mut host, _queue) = slide_box(WORDS);
    click_in(&mut host);
    host.key("Home");
    for _ in 0..3 {
        host.key("Shift+ArrowRight");
    }
    host.key("Ctrl+B");
    let marks = format!("{:?}", editor(&mut host).blocks()[0].line.marks());
    assert!(
        marks.contains("Bold"),
        "the chord set the selected words bold: {marks}"
    );
}

/// A slide stage holding `count` text boxes on the page, each a line of its own.
fn deck(count: i64) -> (FrameHarness, Intents<Fired>) {
    let queue = Intents::default();
    let pins = (0..count)
        .map(|index| StagePinProps {
            key: format!("box-{index}"),
            name: Some(format!("Box {index}")),
            item: Some(format!("box-{index}")),
            x: Some(96 + index * 240),
            y: Some(90),
            width: Some(200),
            height: Some(120),
            moves: true,
            menu: Vec::new(),
            body: vec![Box::new(BlockEditor::from_markdown(&format!(
                "box {index}"
            )))],
        })
        .collect();
    let stage = Stage::from_props(
        "stage",
        StageProps {
            name: Some("Slide".into()),
            canvas: vec![PAGE.0, PAGE.1],
            pins,
            select: Event::View(Intent::new(&queue, "stage", "select")),
            open: Event::View(Intent::new(&queue, "stage", "open")),
            moved: Event::View(Intent::new(&queue, "stage", "moved")),
            resized: Event::View(Intent::new(&queue, "stage", "resized")),
            ..StageProps::default()
        },
    );
    let host = FrameHarness::new(State::new(), Box::new(stage), DEVICE);
    (host, queue)
}

/// What a stage's queue fired for `event`, as its places were named.
fn fired(queue: &Intents<Fired>, event: &str) -> Vec<String> {
    queue
        .take()
        .into_iter()
        .filter(|fired| fired.event == event)
        .map(|fired| match fired.value {
            Some(Value::List(mut parts)) => match parts.remove(0) {
                Value::Text(item) => item,
                other => panic!("a place is named: {other:?}"),
            },
            other => panic!("a place was {event}: {other:?}"),
        })
        .collect()
}

#[test]
fn a_deck_takes_tab_and_the_arrows_on_its_boxes() {
    let (mut host, queue) = deck(2);
    host.root_widget_mut()
        .as_any_mut()
        .and_then(|any| any.downcast_mut::<Stage>())
        .expect("the root is the stage")
        .set_focus(true);

    // Tab walks the boxes, wrapping past the last.
    host.dispatch_to_root(&synthetic_input::named_key(winit::keyboard::NamedKey::Tab));
    host.dispatch_to_root(&synthetic_input::named_key(winit::keyboard::NamedKey::Tab));
    host.dispatch_to_root(&synthetic_input::named_key(winit::keyboard::NamedKey::Tab));
    assert_eq!(
        fired(&queue, "select"),
        ["box-0", "box-1", "box-0"],
        "Tab walks the boxes on the deck"
    );

    // An arrow nudges the chosen box by ten slide units, and Shift by one.
    host.dispatch_to_root(&synthetic_input::named_key(
        winit::keyboard::NamedKey::ArrowRight,
    ));
    assert_eq!(
        fired(&queue, "moved"),
        ["box-0"],
        "an arrow moves the chosen box"
    );
}

#[test]
fn a_slide_box_keeps_its_words_while_it_moves_and_idles_after_the_drop() {
    let (mut host, queue) = slide_box(WORDS);
    let (scale, left, top) = fitted();
    let on_page = |page_x: f64, page_y: f64| {
        (
            left + (BOX.0 as f64 + page_x) * scale,
            top + (BOX.1 as f64 + page_y) * scale,
        )
    };
    let (x, y) = on_page(300.0, 60.0);
    host.dispatch_to_root(&synthetic_input::cursor_moved(x, y));
    host.dispatch_to_root(&synthetic_input::mouse_press());
    let (x, y) = on_page(340.0, 100.0);
    host.dispatch_to_root(&synthetic_input::cursor_moved(x, y));
    host.dispatch_to_root(&synthetic_input::mouse_release());

    assert_eq!(fired(&queue, "moved"), ["box-0"], "the hand moved the box");
    assert_eq!(
        editor(&mut host).blocks()[0].text(),
        WORDS,
        "a move leaves the words as they were"
    );
    assert!(
        host.settle_for_capture(time::Duration::from_millis(200)),
        "nothing is left to draw once the hand lets go"
    );
}

#[test]
fn a_change_that_arrives_mid_drag_shows_when_the_hand_lets_go() {
    let (mut host, queue) = slide_box(WORDS);
    let (scale, left, top) = fitted();
    let on_page = |page_x: f64, page_y: f64| {
        (
            left + (BOX.0 as f64 + page_x) * scale,
            top + (BOX.1 as f64 + page_y) * scale,
        )
    };
    let (x, y) = on_page(300.0, 60.0);
    host.dispatch_to_root(&synthetic_input::cursor_moved(x, y));
    host.dispatch_to_root(&synthetic_input::mouse_press());
    let (x, y) = on_page(340.0, 100.0);
    host.dispatch_to_root(&synthetic_input::cursor_moved(x, y));

    // Another member moves the same box while the hand still holds it: the page is told the
    // box stands somewhere else before the drop.
    let moved_elsewhere = |markdown: &str| StageProps {
        name: Some("Slide".into()),
        canvas: vec![PAGE.0, PAGE.1],
        pins: vec![StagePinProps {
            key: "box-0".into(),
            name: Some("Title".into()),
            item: Some("box-0".into()),
            x: Some(700),
            y: Some(400),
            width: Some(BOX.2),
            height: Some(BOX.3),
            moves: true,
            menu: Vec::new(),
            body: vec![Box::new(BlockEditor::from_markdown(markdown))],
        }],
        ..StageProps::default()
    };
    host.root_widget_mut()
        .as_any_mut()
        .and_then(|any| any.downcast_mut::<Stage>())
        .expect("the root is the stage")
        .set_props(moved_elsewhere(WORDS));
    host.relayout();

    // The words changed too: the window hands the place's body its new document where it
    // stands, by its key, so the drag the hand holds keeps its hold and the words show after it.
    {
        use tilo_ui::component::PartViews;
        let stage = host
            .root_widget_mut()
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<Stage>())
            .expect("the root is the stage");
        let body = stage
            .item_views("pins", "box-0", "body")
            .expect("the place's body takes views by key");
        body.replace(
            0,
            Box::new(BlockEditor::from_markdown(&format!("{WORDS}, again"))),
        );
    }
    host.relayout();

    // The drop reports what the hand did, and the page shows what the other member wrote.
    host.dispatch_to_root(&synthetic_input::mouse_release());
    assert_eq!(fired(&queue, "moved"), ["box-0"], "the drop still reports");
    assert_eq!(
        editor(&mut host).blocks()[0].text(),
        format!("{WORDS}, again"),
        "the change that arrived mid-drag shows after the drop"
    );
}
