use super::*;
use crate::a11y::{AccessBounds, AccessNode, AccessState};

fn text(path: &str, words: &str) -> Composed {
    let mut node = Composed::new(path.to_owned(), "Label", [13.0, 27.0, 200.0, 20.0]);
    node.semantics = Some(AccessNode::new(Role::StaticText).with_value(words.to_owned()));
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

/// A window whose content flows is laid out by the browser from its containers' styles: where
/// the frame put each widget is nowhere in it.
#[test]
fn a_flowing_window_carries_no_frame_geometry() {
    let html = Page::new().render(&window("Groceries"));
    for geometry in ["left:", "top:", "13px", "27px", "position:absolute"] {
        assert!(!html.contains(geometry), "{geometry} in {html}");
    }
    assert!(
        html.contains("display:flex;flex-direction:column;gap:8px 8px;"),
        "{html}"
    );
}

/// Each node is the element its role names, with its words and its action.
#[test]
fn nodes_render_as_the_elements_their_roles_name() {
    let html = Page::new().render(&window("Fish & <chips>"));
    assert!(html.contains(">Fish &amp; &lt;chips&gt;</span>"), "{html}");
    let button = html
        .split('<')
        .find(|tag| tag.starts_with("button"))
        .unwrap_or_else(|| panic!("a button is a <button>: {html}"));
    assert!(button.contains("data-a=\"activate\""), "{button}");
    assert!(button.ends_with(">New note"), "it says its name: {button}");
}

#[test]
fn a_change_patches_only_what_changed_and_keeps_every_number() {
    let mut page = Page::new();
    let first = page.render(&window("Groceries"));
    assert_eq!(first, page.render(&window("Groceries")));
    let patches = page.patch(&window("Groceries list"));
    assert_eq!(patches.len(), 1, "{patches:?}");
    assert_eq!(page.path(patches[0].id), Some("Root[0]/Label[0]"));
    assert!(patches[0].html.contains("Groceries list"));
    assert!(page.patch(&window("Groceries list")).is_empty());
}

/// A widget that publishes its rows (a list drawing them itself) shows each as an element of
/// its own, numbered under the widget, a press on it the row's action.
#[test]
fn a_widgets_own_nodes_are_elements_a_press_acts_on() {
    let mut composition = window("Notes");
    let mut list = Composed::new(
        "Root[0]/List[0]".to_owned(),
        "List",
        [0.0, 100.0, 300.0, 400.0],
    );
    list.semantics = Some(
        AccessNode::new(Role::List)
            .with_name("Notes".to_owned())
            .with_children(
                ["Milk", "Eggs"]
                    .into_iter()
                    .map(|name| {
                        AccessNode::new(Role::ListItem)
                            .with_name(name.to_owned())
                            .with_description("Yesterday".to_owned())
                            .with_action_target(format!("note-{name}"))
                    })
                    .collect(),
            ),
    );
    composition.root.children.push(list);
    let mut page = Page::new();
    let html = page.render(&composition);
    for name in ["Milk", "Eggs"] {
        let target = format!("data-t=\"note-{name}\"");
        let row = html
            .split('<')
            .find(|tag| tag.contains(&target))
            .unwrap_or_else(|| panic!("{name} is a row: {html}"));
        assert!(row.contains("data-a=\"activate\""), "{row}");
        assert!(
            html.contains(&format!(
                "<span class=\"tilo-title\">{name}</span><span class=\"tilo-detail\">Yesterday</span>"
            )),
            "{html}"
        );
    }
    let id = numbers(&html)
        .into_iter()
        .find(|&id| page.path(id).is_some_and(|path| path.ends_with("#1")))
        .expect("the second row is numbered");
    assert_eq!(page.path(id), Some("Root[0]/List[0]#1"));
}

/// What a widget only paints is its picture, and what acts in it stands over the picture where
/// it is drawn.
#[test]
fn a_painted_widget_is_its_picture_with_what_acts_over_it() {
    let mut composition = window("Chart");
    let mut chart = Composed::new(
        "Root[0]/Chart[0]".to_owned(),
        "Chart",
        [10.0, 100.0, 400.0, 200.0],
    );
    chart.semantics = Some(
        AccessNode::new(Role::Image)
            .with_name("Sales".to_owned())
            .with_children(vec![
                AccessNode::new(Role::Button)
                    .with_name("March".to_owned())
                    .with_action_target("bar:3")
                    .with_bounds(AccessBounds {
                        x0: 50.0,
                        y0: 120.0,
                        x1: 70.0,
                        y1: 280.0,
                    })
                    .with_state(AccessState::new().focusable(true)),
            ]),
    );
    chart.picture = Some(Picture {
        version: 1,
        data: Some("data:image/png;base64,AAAA".into()),
    });
    composition.root.children.push(chart);
    let html = Page::new().render(&composition);
    assert!(
        html.contains("<img class=\"tilo-paint\" src=\"data:image/png;base64,AAAA\" alt=\"\">"),
        "{html}"
    );
    assert!(html.contains("aria-label=\"Sales\""), "{html}");
    let over = html
        .split('<')
        .find(|tag| tag.contains("data-t=\"bar:3\""))
        .unwrap_or_else(|| panic!("March stands over the picture: {html}"));
    assert!(
        over.contains("class=\"tilo-over\"")
            && over.contains("left:40px;top:20px;width:20px;height:160px;"),
        "{over}"
    );
}

/// An editable text node is the browser's own field, holding its value.
#[test]
fn a_text_field_is_an_input() {
    let mut composition = window("Title");
    let mut field = Composed::new(
        "Root[0]/TextInput[0]".to_owned(),
        "TextInput",
        [0.0, 100.0, 300.0, 30.0],
    );
    field.semantics = Some(
        AccessNode::new(Role::TextInput)
            .with_name("Title".to_owned())
            .with_value("Fish & chips".to_owned()),
    );
    composition.root.children.push(field);
    let html = Page::new().render(&composition);
    let input = html
        .split('<')
        .find(|tag| tag.starts_with("input"))
        .unwrap_or_else(|| panic!("the field is an <input>: {html}"));
    assert!(input.contains("value=\"Fish &amp; chips\""), "{input}");
    assert!(input.contains("aria-label=\"Title\""), "{input}");
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

/// A canvas at (100, 50) holding two places, the second a window's well.
fn canvas(second_at: [f64; 2]) -> Composition {
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

#[test]
fn a_canvas_places_what_it_holds_and_a_host_fills_its_wells() {
    let mut page = Page::new();
    page.wells.insert(
        "tab-7".to_owned(),
        "<a href=\"/w/ws/notes\">Notes</a>".to_owned(),
    );
    let html = page.render(&canvas([500.0, 350.0]));
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
