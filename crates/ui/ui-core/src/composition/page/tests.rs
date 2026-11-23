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

/// Every `data-p` in `html`, as the number it names.
fn numbers(html: &str) -> Vec<u32> {
    html.match_indices("data-p=\"")
        .filter_map(|(at, _)| {
            let rest = &html[at + "data-p=\"".len()..];
            let end = rest.find('"')?;
            rest.get(..end)?.parse().ok()
        })
        .collect()
}

/// The page's numbers, each with the path it names.
fn numbered(page: &Page, html: &str) -> Vec<(u32, String)> {
    numbers(html)
        .into_iter()
        .map(|id| (id, page.path(id).unwrap_or_default().to_owned()))
        .collect()
}

#[test]
fn an_element_keeps_its_number_across_renders() {
    let mut page = Page::new();
    let first = page.render(&window("A"));
    let second = page.render(&window("A"));
    assert_eq!(first, second);
}

#[test]
fn a_patch_names_the_element_it_replaces_and_renumbers_nothing() {
    // The page's client replaces the element numbered `id` with the patched
    // HTML and keeps every other element where it is. So a number must name
    // the same element after a patch as before it, and the patch must carry
    // the number of the element it replaces; a patch that renumbered its
    // neighbours would leave the client's page showing the old number's
    // element.
    let mut page = Page::new();
    let before = page.render(&window("Groceries"));
    let numbered_before = numbered(&page, &before);
    assert!(
        numbered_before.len() >= 4,
        "the fixture draws several elements: {before}"
    );
    let patches = page.patch(&window("Groceries list"));
    assert_eq!(patches.len(), 1, "{patches:?}");
    let patched = &patches[0];
    assert_eq!(
        page.path(patched.id),
        Some("Root[0]/Label[0]"),
        "the patch replaces the element the label is"
    );
    assert_eq!(
        numbers(&patched.html),
        vec![patched.id],
        "it carries the number of the element it replaces: {}",
        patched.html
    );
    // Nothing else moved: every element the first render numbered still names
    // the same path.
    for (id, path) in &numbered_before {
        assert_eq!(page.path(*id), Some(path.as_str()), "number {id} moved");
    }
    // And a whole render of what the patch showed numbers the same elements
    // the same way: a patch and a render never disagree about an element.
    let mut whole = Page::new();
    let after = whole.render(&window("Groceries list"));
    assert_eq!(numbered(&whole, &after), numbered_before, "{after}");
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

/// A canvas at (100, 50) holding two places, the second a window's well.
fn canvas(second_at: [f64; 2]) -> Composition {
    let mut map = Composed::new("Root[0]".to_owned(), "Map", [100.0, 50.0, 800.0, 600.0]);
    map.places = true;
    let first = text("Root[0]/MapPin[0]", "Groceries");
    let mut first = Composed {
        rect: [140.0, 90.0, 300.0, 200.0],
        ..first
    };
    first.layout = None;
    let mut second = Composed::new(
        "Root[0]/MapPin[1]".to_owned(),
        "MapPin",
        [second_at[0], second_at[1], 400.0, 300.0],
    );
    let mut well = Composed::new(
        "Root[0]/MapPin[1]/WindowWell[0]".to_owned(),
        "WindowWell",
        [second_at[0], second_at[1], 400.0, 300.0],
    );
    well.well = Some("tab-7".to_owned());
    // A window's frame lays its well out itself, as a column.
    second.flows = true;
    second.children = vec![well];
    map.children = vec![first, second];
    Composition {
        viewport: [1024, 720],
        focus: None,
        root: map,
        layers: Vec::new(),
    }
}

#[test]
fn a_canvas_places_what_it_holds_and_a_host_fills_its_wells() {
    let mut page = Page::new();
    page.wells.insert(
        "tab-7".to_owned(),
        "<a href=\"/w/ws/notes\">Notes</a>".to_owned(),
    );
    let html = page.render(&canvas([500.0, 350.0]));
    assert!(
        html.contains("position:relative;overflow:auto;"),
        "the canvas scrolls to what it places: {html}"
    );
    assert!(
        html.contains("position:absolute;left:40px;top:40px;width:300px;height:200px;"),
        "a place stands at its point from the canvas's corner: {html}"
    );
    assert!(
        html.contains("position:absolute;left:400px;top:300px;width:400px;height:300px;"),
        "{html}"
    );
    assert!(
        html.contains("data-well=\"tab-7\"><a href=\"/w/ws/notes\">Notes</a>"),
        "the host's window stands in its well: {html}"
    );
    assert!(
        page.placed.is_empty(),
        "nothing is flowed: {:?}",
        page.placed
    );
    // Another member moved the second place: only its element changes.
    let patches = page.patch(&canvas([520.0, 330.0]));
    assert_eq!(patches.len(), 1, "{patches:?}");
    assert!(
        patches[0]
            .html
            .contains("position:absolute;left:420px;top:280px;"),
        "{patches:?}"
    );
    assert_eq!(page.path(patches[0].id), Some("Root[0]/MapPin[1]"));
}
