//! What the widgets that paint their own content show a page: the element each stands as, the
//! words it reads and the values it carries.

use crate::State;
use crate::composition::{Composed, Composition, Own};
use crate::headless::FrameHarness;
use crate::theme::Theme;
use crate::widget::Widget;

/// `root` laid out in a window of `viewport` and composed as a page shows it.
fn composed(root: Box<dyn Widget>, viewport: (u32, u32)) -> Composition {
    let theme = Theme::dark();
    let mut harness = FrameHarness::new_with_theme(State::new(), root, viewport, theme);
    harness.relayout();
    harness.composition()
}

/// The widget the composition shows: its root stands in an anonymous node the window lays it
/// out in.
fn shown(composition: &Composition) -> &Composed {
    let mut node = &composition.root;
    while node.widget.is_empty() {
        match node.children.first() {
            Some(child) => node = child,
            None => break,
        }
    }
    node
}

fn all<'a>(node: &'a Composed, out: &mut Vec<&'a Composed>) {
    out.push(node);
    for child in &node.children {
        all(child, out);
    }
}

fn nodes(root: &Composed) -> Vec<&Composed> {
    let mut out = Vec::new();
    all(root, &mut out);
    out
}

fn words(node: &Composed) -> Option<String> {
    match &node.own {
        Some(Own::Text(look)) => Some(look.text.clone()),
        _ => None,
    }
}

fn texts(node: &Composed) -> Vec<String> {
    nodes(node).into_iter().filter_map(words).collect()
}

fn tagged<'a>(root: &'a Composed, tag: &str) -> Vec<&'a Composed> {
    nodes(root)
        .into_iter()
        .filter(|node| node.tag == Some(tag))
        .collect()
}

fn attribute<'a>(node: &'a Composed, name: &str) -> Option<&'a str> {
    node.attrs
        .iter()
        .find(|(key, _)| *key == name)
        .map(|(_, value)| value.as_str())
}

/// A property list is a description list: each name a term, its value the description.
#[test]
fn a_property_list_composes_as_terms_and_descriptions() {
    let list = crate::widget::PropertyList::new(vec![
        crate::widget::Property::new("Owner", "Ada Lovelace"),
        crate::widget::Property::new("Size", "2.4 MB"),
    ]);
    let composition = composed(Box::new(list), (400, 300));
    let root = &composition.root;
    assert_eq!(tagged(root, "dl").len(), 1, "a description list: {root:?}");
    let terms = tagged(root, "dt");
    let values = tagged(root, "dd");
    assert!(
        terms
            .iter()
            .filter_map(|node| words(node))
            .any(|words| words == "Owner"),
        "the name is a term: {root:?}"
    );
    assert!(
        values
            .iter()
            .filter_map(|node| words(node))
            .any(|words| words == "Ada Lovelace"),
        "the value is its description: {root:?}"
    );
}

/// A data table is a table: column headers, then a cell per value, in the order they stand.
#[test]
fn a_data_table_composes_as_a_table_with_headers_and_cells() {
    #[derive(Clone)]
    struct Item {
        name: String,
    }
    let column = crate::widget::data_table::ColumnDef::new(
        "name",
        "Name",
        crate::widget::data_table::ColumnWidth::Flex(1.0),
    )
    .with_text(|row: &Item| row.name.clone());
    let mut table = crate::widget::data_table::DataTable::new(vec![column]);
    table.set_rows(vec![
        Item {
            name: "Grocery list".into(),
        },
        Item {
            name: "Trip plan".into(),
        },
    ]);
    let composition = composed(Box::new(table), (400, 400));
    let root = &composition.root;
    let tables = tagged(root, "table");
    assert_eq!(tables.len(), 1, "the table: {root:?}");
    let headers = tagged(root, "th");
    assert!(
        headers
            .iter()
            .filter_map(|node| words(node))
            .any(|words| words == "Name"),
        "the column's header is a header cell: {root:?}"
    );
    assert_eq!(
        attribute(headers[0], "scope"),
        Some("col"),
        "the header names its column"
    );
    let cells: Vec<String> = tagged(root, "td")
        .iter()
        .filter_map(|node| words(node))
        .collect();
    assert!(
        cells.iter().any(|cell| cell == "Grocery list")
            && cells.iter().any(|cell| cell == "Trip plan"),
        "each row's value is a cell: {cells:?}"
    );
}

/// A trail of folders is a navigation list; the page a person is on says so.
#[test]
fn a_breadcrumb_composes_as_a_path_with_the_current_page_marked() {
    let breadcrumb = crate::widget::Breadcrumb::new(vec![
        "home".to_owned(),
        "docs".to_owned(),
        "readme.md".to_owned(),
    ]);
    let composition = composed(Box::new(breadcrumb), (500, 40));
    let root = &composition.root;
    assert_eq!(tagged(root, "nav").len(), 1, "a navigation: {root:?}");
    assert_eq!(tagged(root, "ol").len(), 1, "a list: {root:?}");
    let items = tagged(root, "li");
    let labels: Vec<String> = items
        .iter()
        .filter_map(|item| item.children.iter().find_map(words))
        .collect();
    assert_eq!(labels, ["home", "docs", "readme.md"]);
    let chevrons = items
        .iter()
        .filter(|item| {
            item.children
                .iter()
                .any(|part| part.path.ends_with("#chevron"))
        })
        .count();
    assert_eq!(chevrons, 2, "a chevron between each two places, as painted");
    assert_eq!(
        attribute(items[2], "aria-current"),
        Some("page"),
        "the page a person is on is marked"
    );
    assert_eq!(attribute(items[0], "aria-current"), None);
}

/// A conversation is a list of lines: each line's author, then what they said.
#[test]
fn a_conversation_composes_as_a_list_of_authors_and_words() {
    use crate::widget::conversation::ConversationLine;
    let conversation =
        crate::widget::conversation::ConversationList::new(vec![ConversationLine::incoming(
            "a",
            1_700_000_000_000,
            "Ada",
            "The build is green.",
        )]);
    let composition = composed(Box::new(conversation), (500, 300));
    let root = &composition.root;
    assert_eq!(tagged(root, "ul").len(), 1, "a list: {root:?}");
    let items = tagged(root, "li");
    assert!(!items.is_empty(), "a line is an item: {root:?}");
    let said = texts(root);
    assert!(
        said.iter().any(|words| words == "The build is green."),
        "the words are text: {said:?}"
    );
    assert!(
        said.iter().any(|words| words == "Ada"),
        "so is the author: {said:?}"
    );
}

/// A progress bar is the browser's own, with its value and what it is doing.
#[test]
fn a_progress_bar_composes_as_the_browsers_own_with_its_value() {
    let bar = crate::widget::ProgressBar::new(0.4).with_label("Uploading");
    let composition = composed(Box::new(bar), (300, 30));
    let root = shown(&composition);
    assert_eq!(root.tag, Some("progress"), "the browser's bar: {root:?}");
    assert_eq!(attribute(root, "value"), Some("40"));
    assert_eq!(attribute(root, "max"), Some("100"));
    assert_eq!(
        root.semantics
            .as_ref()
            .and_then(|node| node.name.as_deref()),
        Some("Uploading"),
        "it says what it is doing"
    );
}

/// An avatar is a person's mark: their initials as words, their name for a reader.
#[test]
fn an_avatar_shows_initials_as_words_and_the_person_for_a_reader() {
    let avatar = crate::widget::Avatar::person("Ada Lovelace", "ada");
    let composition = composed(Box::new(avatar), (100, 100));
    let root = shown(&composition);
    assert_eq!(
        root.semantics
            .as_ref()
            .and_then(|node| node.name.as_deref()),
        Some("Ada Lovelace"),
        "a reader hears who it is: {root:?}"
    );
    let initials: Vec<String> = texts(root);
    assert!(
        initials.iter().any(|words| words == "AL"),
        "the initials are text: {initials:?}"
    );
}

/// A chart is a picture, and the element around it says what it shows.
#[test]
fn a_chart_is_a_named_picture() {
    let chart = crate::widget::Chart::new(vec![crate::widget::Series::new(
        vec![1.0, 4.0, 2.0],
        vello::peniko::Color::WHITE,
    )])
    .with_title("Weekly revenue");
    let composition = composed(Box::new(chart), (400, 200));
    let root = shown(&composition);
    assert_eq!(root.tag, Some("figure"), "a figure: {root:?}");
    assert!(
        matches!(root.own, Some(Own::Painted { .. })),
        "its paint is the picture: {root:?}"
    );
    assert!(
        attribute(root, "aria-label").is_some()
            || root
                .semantics
                .as_ref()
                .and_then(|node| node.name.clone())
                .is_some(),
        "the figure says what it shows: {root:?}"
    );
}

/// A divider is a rule, and a skeleton stands for content without words.
#[test]
fn a_divider_is_a_rule_and_a_skeleton_has_nothing_to_read() {
    let composition = composed(Box::new(crate::widget::Divider::horizontal()), (300, 1));
    let rule = shown(&composition);
    assert_eq!(rule.tag, Some("hr"), "a rule: {rule:?}");
    let composition = composed(Box::new(crate::widget::Skeleton::text(2)), (300, 40));
    assert_eq!(attribute(shown(&composition), "aria-hidden"), Some("true"));
}

/// A window's surface is its ground on a page, floating over what is under it; a raised one
/// is a ground alone.
#[test]
fn a_surface_composes_as_its_ground_and_a_window_floats() {
    use crate::widget::{Surface, SurfaceLevel};
    let window = Surface::new(
        SurfaceLevel::Window,
        Box::new(crate::widget::Text::new("Inbox")),
    );
    let composition = composed(Box::new(window), (300, 200));
    let surface = |root: &Composed| -> Composed {
        nodes(root)
            .into_iter()
            .find(|node| node.widget == "Surface")
            .cloned()
            .expect("the surface's element")
    };
    let window = surface(&composition.root);
    let ground = window.ground.as_ref().expect("the window's ground");
    assert!(ground.fill.is_some() && ground.edge.is_some() && ground.radius > 0.0);
    assert!(window.floats, "a window floats: {window:?}");
    let raised = Surface::new(
        SurfaceLevel::Raised,
        Box::new(crate::widget::Text::new("Tag")),
    );
    let composition = composed(Box::new(raised), (300, 200));
    let raised = surface(&composition.root);
    assert!(raised.ground.is_some());
    assert!(!raised.floats, "a raised surface casts no shadow");
}
