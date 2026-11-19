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
    // Every other row wears the stripe the canvas paints, from the same token.
    let rows = tagged(root, "tr");
    let body: Vec<&&Composed> = rows.iter().skip(1).collect();
    assert_eq!(body.len(), 2, "two body rows: {rows:?}");
    let stripe = crate::widget::data_table::zebra_on(
        &Theme::dark(),
        Theme::dark().tokens.colors.surfaces.content,
    );
    assert!(body[0].ground.is_none(), "the first row stands on the page");
    assert_eq!(
        body[1].ground.as_ref().and_then(|ground| ground.fill),
        Some(crate::composition::rgba8(stripe)),
        "the second wears the stripe"
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

/// A progress bar on the page is the track and fill it paints in the window, its value and
/// what it is doing for a reader, and a shown label as words over the track.
#[test]
fn a_progress_bar_composes_its_track_fill_and_shown_label() {
    let bar = crate::widget::ProgressBar::new(0.4).with_shown_label("Uploading");
    let composition = composed(Box::new(bar), (300, 40));
    let root = shown(&composition);
    assert_eq!(attribute(root, "aria-valuenow"), Some("40"));
    assert_eq!(attribute(root, "aria-valuemax"), Some("100"));
    assert_eq!(
        root.semantics
            .as_ref()
            .and_then(|node| node.name.as_deref()),
        Some("Uploading"),
        "it says what it is doing"
    );
    assert_eq!(texts(root), vec!["Uploading".to_owned()], "the label shows");
    let ground = |key: &str| {
        nodes(root)
            .into_iter()
            .find(|node| node.path.ends_with(key))
            .and_then(|node| node.ground.as_ref())
            .and_then(|ground| ground.fill)
    };
    let theme = crate::theme::Theme::dark();
    assert_eq!(
        ground("track"),
        Some(crate::composition::rgba8(
            theme.tokens.colors.intent.value_track
        )),
        "the track is the one every progress indicator stands on"
    );
    assert!(ground("fill").is_some(), "the share done is filled");
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

/// A banner's buttons are on its page, inside it, as it places them.
#[test]
fn a_banner_composes_its_buttons_inside_it() {
    let banner = crate::widget::Banner::warning("Sync paused")
        .with_message("Files sync again when you are online.")
        .with_action(crate::widget::Button::new("retry", "Retry"))
        .arrives_in_place();
    let composition = composed(Box::new(banner), (560, 200));
    let all = nodes(&composition.root);
    let banner = all
        .iter()
        .find(|node| node.widget == "Banner")
        .unwrap_or_else(|| panic!("the banner's element: {:#?}", composition.root));
    let button = nodes(banner)
        .into_iter()
        .find(|node| node.widget == "Button")
        .unwrap_or_else(|| panic!("its button: {banner:#?}"));
    let [bx, by, bw, bh] = banner.rect;
    let [x, y, w, h] = button.rect;
    assert!(w > 0.0 && h > 0.0, "the button has its size: {button:?}");
    assert!(
        x >= bx && y >= by && x + w <= bx + bw && y + h <= by + bh,
        "the button is inside the banner: {:?} in {:?}",
        button.rect,
        banner.rect
    );
}

/// What a card holds is composed in the colours its paint hands it on the card's ground: a
/// button there stands apart from the card as it does on the canvas.
#[test]
fn a_button_on_a_card_wears_the_cards_on_ground_colours() {
    let fill = |root: &Composed| -> Option<[u8; 4]> {
        nodes(root)
            .into_iter()
            .find(|node| node.widget == "Button")
            .and_then(|node| match &node.own {
                Some(Own::Face(face)) => face.fill,
                _ => None,
            })
    };
    let alone = composed(
        Box::new(crate::widget::Button::new("message", "Message")),
        (300, 200),
    );
    let card = composed(
        Box::new(crate::widget::Card::new(Box::new(
            crate::widget::Button::new("message", "Message"),
        ))),
        (300, 200),
    );
    let card_ground = nodes(&card.root)
        .into_iter()
        .find(|node| node.widget == "Card")
        .and_then(|node| node.ground.as_ref())
        .and_then(|ground| ground.fill);
    let on_card = fill(&card.root);
    assert!(on_card.is_some(), "the button keeps its face on the card");
    assert_ne!(
        on_card, card_ground,
        "the button stands apart from the card's ground"
    );
    assert_ne!(
        on_card,
        fill(&alone.root),
        "on the card it takes the card's colours"
    );
}

/// Your own lines read together in one minute say "Read" and their time once, under the last.
#[test]
fn a_conversation_says_read_once_under_lines_read_together() {
    use crate::widget::conversation::{ConversationLine, DeliveryState};
    let at = 1_700_000_000_000;
    let lines = ["One.", "Two.", "Three."]
        .into_iter()
        .enumerate()
        .map(|(n, body)| {
            ConversationLine::outgoing(
                format!("{n}"),
                at + 1_000 * i64::try_from(n).unwrap_or(0),
                body,
            )
            .with_delivery(DeliveryState::Read)
        })
        .collect();
    let conversation = crate::widget::conversation::ConversationList::new(lines);
    let composition = composed(Box::new(conversation), (500, 400));
    let captions: Vec<&Composed> = nodes(&composition.root)
        .into_iter()
        .filter(|node| node.widget == "ConversationCaption")
        .collect();
    assert_eq!(captions.len(), 1, "one caption for the group: {captions:?}");
    let said = captions[0]
        .semantics
        .as_ref()
        .and_then(|node| node.name.clone())
        .unwrap_or_default();
    assert!(
        said.starts_with("Read"),
        "it says the state, then the time: {said}"
    );
}

/// A change stands on one line as the canvas draws it: its sentence, and its time at the
/// trailing edge beside it; a change nobody is named for wears no face.
#[test]
fn a_history_change_composes_on_one_line_with_its_time_beside_it() {
    use crate::widget::history::{EntryKind, HistoryAuthor, HistoryEntry, HistoryEntryId};
    let entry = HistoryEntry {
        id: HistoryEntryId([1; 16]),
        author: HistoryAuthor::new("", 0.0, ""),
        entry_kind: EntryKind::Details,
        artifact_kind: collab_types::ArtifactKind::Note,
        timestamp: crate::time::SystemTime::UNIX_EPOCH,
        summary: "Moved to Done".into(),
        text_delta: None,
        diff_payload: None,
        redaction: None,
        context: None,
        target: None,
    };
    let timeline = crate::widget::ActivityTimeline::with_entries(vec![entry]);
    let composition = composed(Box::new(timeline), (600, 300));
    let all = nodes(&composition.root);
    let part = |key: &str| {
        all.iter()
            .copied()
            .find(|node| node.path.ends_with(key))
            .unwrap_or_else(|| panic!("a {key} part: {all:?}"))
    };
    let (body, time) = (part("#body"), part("#time"));
    assert_eq!(words(body).as_deref(), Some("Moved to Done"));
    assert!(
        time.rect[0] >= body.rect[0] + body.rect[2] - 0.5,
        "the time stands after the sentence"
    );
    assert!(
        (body.rect[1] - time.rect[1]).abs() < 0.5,
        "on the same line: {body:?} {time:?}"
    );
    assert!(
        !all.iter().any(|node| node.widget == "ActivityAuthor"),
        "no face for nobody"
    );
}

/// A sidebar's rows stand as the canvas paints them: the chosen one's lift inset from the list's
/// edges and rounded, every row's icon and words at the canvas's indent, and only the chosen
/// row's words at full strength.
#[test]
fn a_nav_row_lifts_inset_and_indents_as_painted() {
    use crate::icons::Icon;
    use crate::widget::{NavItem, NavList, NavSection};
    let list = NavList::new(vec![NavSection::untitled(
        "pages",
        vec![
            NavItem::new("basics", "Basics").icon(Icon::Inbox),
            NavItem::new("data", "Data").icon(Icon::Send),
        ],
    )])
    .selected("data");
    let composition = composed(Box::new(list), (280, 200));
    let root = shown(&composition);
    let rows = tagged(root, "li");
    assert_eq!(rows.len(), 2, "one item per row: {root:?}");
    let theme = Theme::dark();
    let tokens = &theme.tokens;
    let s2 = f64::from(tokens.spacing.s2);
    let hairline = f64::from(tokens.metrics.chrome_rule_px);
    let face = |row: &Composed| match &row.own {
        Some(Own::Face(face)) => face.clone(),
        other => panic!("a row shows a face, not {other:?}"),
    };
    for row in &rows {
        let ground = row
            .ground
            .as_ref()
            .expect("every row keeps its inset ground");
        assert_eq!(
            ground.inset,
            [s2, hairline],
            "the lift stands in from the list's edges"
        );
        let face = face(row);
        assert_eq!(
            face.pad, s2,
            "the icon sits one step into the lift, as painted"
        );
        assert_eq!(
            face.icon.as_ref().map(|icon| icon.size),
            Some(f64::from(tokens.metrics.icon_md)),
            "the icon is the painted size"
        );
    }
    let (chosen, other) = (rows[1], rows[0]);
    assert!(
        chosen.ground.as_ref().is_some_and(|g| g.fill.is_some()),
        "the chosen row lifts"
    );
    assert!(
        other.ground.as_ref().is_some_and(|g| g.fill.is_none()),
        "the others do not"
    );
    let ink = |row: &Composed| face(row).label.and_then(|label| label.color);
    assert_eq!(
        ink(chosen),
        Some(crate::composition::rgba8(tokens.colors.text_primary))
    );
    assert_eq!(
        ink(other),
        Some(crate::composition::rgba8(tokens.colors.text_dim))
    );
}

/// A done or removed item's title is struck through on a page, as the canvas paints it, and an
/// open one's is not.
#[test]
fn a_struck_rows_title_is_struck_through_on_the_page() {
    use crate::widget::ListRow;
    let struck = |row: ListRow, title: &str| {
        let composition = composed(Box::new(row), (400, 60));
        nodes(shown(&composition))
            .into_iter()
            .find_map(|node| match &node.own {
                Some(Own::Text(look)) if look.text == title => Some(look.struck),
                _ => None,
            })
            .expect("the title shows")
    };
    assert!(struck(
        ListRow::new("stale", "Old errand").struck(),
        "Old errand"
    ));
    assert!(!struck(
        ListRow::new("milk", "Buy oat milk"),
        "Buy oat milk"
    ));
}

/// A chosen list row stands on the selection's lift as the canvas paints it: under everything it
/// shows, past its own edges by the list's bleed, so what it says at its end keeps its margin.
#[test]
fn a_chosen_list_row_stands_on_the_selection_lift() {
    let page = composed(
        Box::new(crate::widget::ListRow::new("main", "main.rs").selected(true)),
        (300, 200),
    );
    let row = nodes(&page.root)
        .into_iter()
        .find(|node| node.widget == "ListRow")
        .expect("the row composes");
    let lift = row.children.first().expect("the row holds its parts");
    assert_eq!(
        lift.widget, "ListRowLift",
        "the lift stands first, under the rest"
    );
    let theme = Theme::dark();
    let ground = theme.tokens.colors.surfaces.content;
    let chosen = crate::chrome::selection_tier_on(
        &theme,
        crate::chrome::SelectionState::new(true, false, false),
        ground,
    )
    .fill;
    assert_eq!(
        lift.ground.as_ref().and_then(|ground| ground.fill),
        Some(crate::composition::rgba8(chosen))
    );
    assert!(
        lift.rect[0] < row.rect[0] && lift.rect[2] > row.rect[2],
        "the lift reaches past the row's edges"
    );
    let rest = composed(
        Box::new(crate::widget::ListRow::new("main", "main.rs")),
        (300, 200),
    );
    assert!(
        nodes(&rest.root)
            .into_iter()
            .filter(|node| node.widget == "ListRowLift")
            .all(|node| node.ground.is_none()),
        "a row at rest stands on the page's ground"
    );
}
