//! A note as a page shows it: every widget of the notebook renders itself as
//! HTML (`Widget::html`), so the page holds the note's title as a field, its
//! blocks as headings, paragraphs and list items with their marks, the
//! sidebar's notes as list items, and the bar's buttons as buttons.

use crate::framework_support::{add_note, notes, open};
use ui::composition::{Composed, Own};

fn find<'a>(node: &'a Composed, widget: &str) -> Vec<&'a Composed> {
    let mut found: Vec<&Composed> = node
        .children
        .iter()
        .flat_map(|child| find(child, widget))
        .collect();
    if node.widget == widget {
        found.insert(0, node);
    }
    found
}

#[test]
fn a_note_renders_as_its_own_elements() {
    let mut app = notes();
    let page = add_note(
        &mut app,
        "Groceries",
        "# Milk\n\nFresh **eggs**, please.\n\n- bread\n- butter",
    );
    open(&mut app, page);
    let composition = app.harness().composition();
    let root = &composition.root;

    let title = find(root, "EditableLabel");
    let Some(Own::Field { value, .. }) = title.first().and_then(|title| title.own.as_ref()) else {
        panic!("the note's title is a field: {title:?}");
    };
    assert_eq!(value, "Groceries");

    let editor = find(root, "BlockEditor");
    let Some(Own::Blocks { blocks, edges }) = editor.first().and_then(|editor| editor.own.as_ref())
    else {
        panic!("the note's body is a document of blocks");
    };
    // The title and the body's text share one left edge.
    assert!(
        (editor[0].rect[0] + edges.text - title[0].rect[0]).abs() < 0.5,
        "the body's text starts where the title does: {} + {} vs {}",
        editor[0].rect[0],
        edges.text,
        title[0].rect[0]
    );
    let kinds: Vec<(&str, &str)> = blocks
        .iter()
        .map(|block| (block.kind, block.text.as_str()))
        .collect();
    assert_eq!(
        kinds,
        [
            ("heading", "Milk"),
            ("paragraph", "Fresh eggs, please."),
            ("bullet", "bread"),
            ("bullet", "butter"),
        ]
    );
    let bold = blocks[1]
        .marks
        .iter()
        .find(|mark| mark.mark == "bold")
        .expect("the bold run");
    assert_eq!(&blocks[1].text[bold.start..bold.end], "eggs");

    let rows = find(root, "ListRow");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].tag, Some("li"));
    assert!(
        matches!(&rows[0].own, Some(Own::Text(look)) if look.text == "Groceries"),
        "the sidebar's row says the note's title"
    );
    assert!(
        rows[0]
            .semantics
            .as_ref()
            .is_some_and(|row| row.state.selected),
        "the open note's row is the chosen one"
    );

    // A button is a button showing its face: its icon as an icon, not a
    // picture of its paint.
    let buttons = find(root, "Button");
    assert!(!buttons.is_empty());
    for button in &buttons {
        assert_eq!(button.tag, Some("button"));
        let Some(Own::Face(face)) = &button.own else {
            panic!(
                "{} shows its face, not its paint: {:?}",
                button.path, button.own
            );
        };
        let icon = face.icon.as_ref().expect("the bar's buttons are icons");
        assert!(!icon.paths.is_empty(), "its icon's outlines");
    }
}

/// On a phone a page hosts the window's bar itself: its buttons are faces
/// of their own (icons, not pictures of their paint), its title is words,
/// and a notice comes and goes over the note without moving it.
#[test]
fn the_bar_is_html_and_its_notice_does_not_move_the_note() {
    let mut app = notes();
    let page = add_note(&mut app, "Trip to Lisbon", "Flights are booked.");
    open(&mut app, page);
    app.harness().host_window_slots();
    app.harness().resize((390, 844)).unwrap();
    let _ = app.harness().poll_frame();
    let title_at = |app: &mut ui::framework::testing::TestApp<notes::NotesShell>| {
        let composition = app.harness().composition();
        find(&composition.root, "EditableLabel")
            .first()
            .map(|title| title.rect[1])
            .expect("the note's title")
    };
    let before = title_at(&mut app);

    ui::window_toolbar::notify(Some(ui::window_toolbar::ToolbarNotice::new(
        "Imported Trip to Lisbon",
        ui::widget::toast::ToastKind::Success,
    )));
    let _ = app.harness().poll_frame();
    let _ = app.harness().poll_frame();
    let after = title_at(&mut app);
    assert!(
        (after - before).abs() < 0.5,
        "the notice does not push the note down: {before} then {after}"
    );

    let composition = app.harness().composition();
    let bar = composition
        .layers
        .iter()
        .chain(std::iter::once(&composition.root))
        .flat_map(|layer| find(layer, "WindowSlots"))
        .next()
        .expect("the window's bar");
    assert!(bar.own.is_none(), "the bar is not a picture of its paint");
    let notice = find(bar, "WindowNotice");
    let Some(Own::Face(face)) = notice.first().and_then(|notice| notice.own.as_ref()) else {
        panic!("the notice says itself: {bar:#?}");
    };
    assert_eq!(
        face.label.as_ref().map(|label| label.text.as_str()),
        Some("Imported Trip to Lisbon")
    );
    let buttons: Vec<_> = find(bar, "WindowSlot")
        .into_iter()
        .filter(|slot| slot.tag == Some("button"))
        .collect();
    assert!(!buttons.is_empty());
    for button in buttons {
        assert!(
            matches!(&button.own, Some(Own::Face(face)) if face.icon.is_some() || face.label.is_some()),
            "{} shows its face: {:?}",
            button.path,
            button.own
        );
    }
}

/// On a wide window the notice does not take the title's place: the bar
/// keeps the note's location, and the notice comes on a card of its own at
/// the window's foot, no wider than a line read at a glance.
#[test]
fn a_wide_windows_notice_is_a_card_and_the_title_stays() {
    let mut app = notes();
    let page = add_note(&mut app, "Trip to Lisbon", "Flights are booked.");
    open(&mut app, page);
    app.harness().host_window_slots();
    app.harness().resize((1280, 800)).unwrap();
    let _ = app.harness().poll_frame();
    ui::window_toolbar::notify(Some(ui::window_toolbar::ToolbarNotice::new(
        "Imported Trip to Lisbon",
        ui::widget::toast::ToastKind::Success,
    )));
    let _ = app.harness().poll_frame();
    let _ = app.harness().poll_frame();

    let composition = app.harness().composition();
    let bar = composition
        .layers
        .iter()
        .chain(std::iter::once(&composition.root))
        .flat_map(|layer| find(layer, "WindowSlots"))
        .next()
        .expect("the window's bar");
    let words = |node: &Composed| match &node.own {
        Some(Own::Face(face)) => face.label.as_ref().map(|label| label.text.clone()),
        Some(Own::Text(look)) => Some(look.text.clone()),
        _ => None,
    };
    let location = find(bar, "WindowLocation");
    let location = location.first().expect("the bar keeps its location");
    let title = words(location).unwrap_or_default();
    assert!(
        !title.is_empty() && !title.contains("Imported"),
        "the title stays: {title:?}"
    );
    let card = find(bar, "WindowNoticeStrip");
    let card = card.first().expect("the notice's card");
    assert!(card.rect[2] <= 520.0, "a card, not a band: {:?}", card.rect);
    assert!(
        card.rect[1] > 600.0,
        "at the window's foot: {:?}",
        card.rect
    );
    let notice = find(card, "WindowNotice");
    assert_eq!(
        notice.first().and_then(|notice| words(notice)).as_deref(),
        Some("Imported Trip to Lisbon")
    );
}
