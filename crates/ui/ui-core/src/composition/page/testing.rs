//! The compositions the page tests draw: a window with a flow and a button, a canvas with
//! places and a window's well, and the markup a render's elements are read back from.
#![allow(
    clippy::wildcard_imports,
    clippy::string_slice,
    clippy::panic,
    clippy::expect_used,
    dead_code,
    reason = "a fixture's helpers read the page's own vocabulary; each test uses the ones it needs"
)]

use super::*;
use tilo_ui_base::a11y::AccessNode;

pub fn text(path: &str, words: &str) -> Composed {
    let mut node = Composed::new(path.to_owned(), "Label", [13.0, 27.0, 200.0, 20.0]);
    node.semantics = Some(AccessNode::new(Role::StaticText).with_value(words.to_owned()));
    node.layout = Some("display:flex;width:120px;".to_owned());
    node
}

/// The element `patch` replaces and its new markup; any other step fails the test.
pub fn replaced(patch: &Patch) -> (u32, &str) {
    match patch {
        Patch::Replace { id, html } => (*id, html),
        other => panic!("an element replaced, not {other:?}"),
    }
}

pub fn window(title: &str) -> Composition {
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

/// Every `data-p` in `html`, as the number it names.
pub fn numbers(html: &str) -> Vec<u32> {
    html.match_indices("data-p=\"")
        .filter_map(|(at, _)| {
            let rest = &html[at + "data-p=\"".len()..];
            let end = rest.find('"')?;
            rest.get(..end)?.parse().ok()
        })
        .collect()
}

/// A canvas at (100, 50) holding two places, the second a window's well.
pub fn canvas(second_at: [f64; 2]) -> Composition {
    let mut map = Composed::new("Root[0]".to_owned(), "Map", [100.0, 50.0, 800.0, 600.0]);
    let mut first = text("Root[0]/MapPin[0]", "Groceries");
    first.rect = [140.0, 90.0, 300.0, 200.0];
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
    well.semantics = Some(AccessNode::new(Role::Window).with_item("tab-7".to_owned()));
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

/// The opening tag of the first element of `html` whose tag holds `mark`, and what follows it.
pub fn element_with<'a>(html: &'a str, mark: &str) -> &'a str {
    let at = html
        .find(mark)
        .unwrap_or_else(|| panic!("no element with {mark}: {html}"));
    let open = html
        .get(..at)
        .and_then(|before| before.rfind('<'))
        .expect("a tag");
    html.get(open..).expect("the element")
}

/// The number the page gave the element whose tag holds `mark`.
pub fn number_of(html: &str, mark: &str) -> u32 {
    numbers(element_with(html, mark))
        .first()
        .copied()
        .unwrap_or_else(|| panic!("the element with {mark} is numbered: {html}"))
}
