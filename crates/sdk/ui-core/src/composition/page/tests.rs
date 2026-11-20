use super::*;
use crate::a11y::AccessNode;

fn look(text: &str) -> TextLook {
    TextLook {
        text: text.to_owned(),
        size: 14.0,
        line_height: 1.4,
        weight: 400,
        italic: false,
        color: Some([10, 20, 30, 255]),
        align: "start",
        max_lines: Some(1),
        mono: false,
        struck: false,
    }
}

fn text(path: &str, words: &str) -> Composed {
    let mut node = Composed::new(path.to_owned(), "Label", [13.0, 27.0, 200.0, 20.0]);
    node.own = Some(Own::Text(look(words)));
    node.layout = Some("display:flex;width:120px;".to_owned());
    node
}

fn window(title: &str) -> Composition {
    let mut root = Composed::new("Root[0]".to_owned(), "Column", [0.0, 0.0, 1280.0, 720.0]);
    root.layout = Some("display:flex;flex-direction:column;gap:8px 8px;".to_owned());
    root.flows = true;
    let mut button = Composed::new(
        "Root[0]/Button[0]".to_owned(),
        "Button",
        [40.0, 60.0, 90.0, 28.0],
    );
    button.semantics = Some(AccessNode::new(Role::Button).with_name("New note".to_owned()));
    root.children = vec![text("Root[0]/Label[0]", title), button];
    Composition {
        viewport: [1280, 720],
        focus: None,
        root,
        layers: Vec::new(),
    }
}

#[test]
fn the_page_carries_no_frame_geometry() {
    let html = Page::new().render(&window("Groceries"));
    // Where the frame put each widget (13, 27, 1280 wide) is nowhere in it: the
    // browser lays it out from the containers' styles.
    for geometry in [
        "left:",
        "top:",
        "13px",
        "27px",
        "1280",
        "720",
        "position:absolute",
    ] {
        assert!(!html.contains(geometry), "{geometry} in {html}");
    }
    assert!(
        html.contains("display:flex;flex-direction:column;gap:8px 8px;"),
        "{html}"
    );
    assert!(
        html.contains("width:120px;"),
        "a style's own width stays: {html}"
    );
}

#[test]
fn widgets_render_as_their_own_elements() {
    let html = Page::new().render(&window("Fish & <chips>"));
    assert!(html.contains(">Fish &amp; &lt;chips&gt;</span>"), "{html}");
    assert!(
        html.contains("<button data-p=\"3\" role=\"button\"")
            || html.contains("<button data-p=\"3\" aria-label"),
        "a button is a <button>: {html}"
    );
    assert!(html.contains("aria-label=\"New note\""), "{html}");
    assert!(
        html.contains("data-a=\"activate\""),
        "a press on it activates it: {html}"
    );
}

#[test]
fn a_change_patches_only_what_changed() {
    let mut page = Page::new();
    page.render(&window("Groceries"));
    let patches = page.patch(&window("Groceries list"));
    assert_eq!(patches.len(), 1, "{patches:?}");
    assert_eq!(page.path(patches[0].id), Some("Root[0]/Label[0]"));
    assert!(patches[0].html.contains("Groceries list"));
    assert!(page.patch(&window("Groceries list")).is_empty());
}

#[test]
fn an_element_keeps_its_number_across_renders() {
    let mut page = Page::new();
    let first = page.render(&window("A"));
    let second = page.render(&window("A"));
    assert_eq!(first, second);
}

#[test]
fn a_document_block_is_patched_alone() {
    let block = |id: &str, words: &str| DocBlock {
        id: id.to_owned(),
        kind: "paragraph",
        level: 0,
        done: false,
        ordinal: None,
        tag: None,
        soft_break: false,
        text: words.to_owned(),
        marks: vec![super::super::InlineMark {
            start: 0,
            end: 4,
            mark: "bold",
            href: None,
        }],
        selection: None,
        editable: true,
        gap: 0.0,
        size: 15.0,
        line_height: 1.5,
        weight: 400,
    };
    let doc = |second: &str| {
        let mut composition = window("t");
        let mut editor = Composed::new(
            "Root[0]/BlockEditor[0]".to_owned(),
            "BlockEditor",
            [0.0, 0.0, 10.0, 10.0],
        );
        editor.own = Some(Own::Blocks {
            blocks: vec![block("a", "Milk and eggs"), block("b", second)],
            edges: super::super::DocEdges {
                text: 0.0,
                list: 28.0,
                indent: 28.0,
                quote: 16.0,
            },
        });
        composition.root.children.push(editor);
        composition
    };
    let mut page = Page::new();
    let html = page.render(&doc("Fish"));
    assert!(html.contains("<p data-p="), "{html}");
    assert!(html.contains("<strong>Milk</strong> and eggs"), "{html}");
    let patches = page.patch(&doc("Fish!"));
    assert_eq!(patches.len(), 1, "{patches:?}");
    assert_eq!(
        page.path(patches[0].id),
        Some("Root[0]/BlockEditor[0]#block:b")
    );
}
