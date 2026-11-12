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
    let Some(Own::Blocks { blocks }) = editor.first().and_then(|editor| editor.own.as_ref()) else {
        panic!("the note's body is a document of blocks");
    };
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

    // A button is a button, its face its paint, which the page asks for.
    let buttons = find(root, "Button");
    assert!(!buttons.is_empty());
    for button in &buttons {
        assert_eq!(button.tag, Some("button"));
    }
    let path = buttons[0].path.clone();
    let (scene, rect) = app
        .harness()
        .paint_composed(&path, 2.0)
        .expect("a laid-out button paints");
    assert!(!scene.encoding().is_empty(), "its face is painted");
    assert!(
        rect.iter()
            .zip(buttons[0].rect)
            .all(|(a, b)| (a - b).abs() < f64::EPSILON),
        "its picture covers its box"
    );
}
