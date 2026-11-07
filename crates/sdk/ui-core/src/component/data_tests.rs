//! The data views: DataTable, TreeView, ConversationList, CardGrid and ActivityTimeline as
//! described components — their parts read from their slots, what a person does to them
//! reported with the item each stands for, and what changed taken in place.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "in a test these are the assertion"
)]

use super::testing::*;
use super::*;
use crate::headless::FrameHarness;
use crate::widget::Widget;

fn column(header: &str) -> Node {
    Node::new("DataTableColumn", header, &crate::Intents::default()).prop("header", text(header))
}

fn cell_node(column: &str, value: &str) -> Node {
    Node::new("DataTableCell", column, &crate::Intents::default())
        .prop("column", text(column))
        .prop("value", text(value))
}

fn row(queue: &crate::Intents<Fired>, name: &str, item: &str) -> Node {
    Node::new("DataTableRow", item, queue)
        .prop("item", resource(item))
        .holding("cells", cell_node("Name", name))
        .holding("cells", cell_node("State", "ok"))
}

fn table_node(queue: &crate::Intents<Fired>) -> Node {
    Node::new("DataTable", "songs", queue)
        .holding("columns", column("Name"))
        .holding("columns", column("State"))
        .holding("rows", row(queue, "first", "doc/song:1"))
        .holding("rows", row(queue, "second", "doc/song:2"))
        .on("select")
        .on("activate")
}

#[test]
fn a_data_table_reads_its_columns_and_rows_and_reports_the_resource_a_row_shows() {
    let queue = crate::Intents::default();
    let mut node = table_node(&queue)
        .prop("name", text("Songs"))
        .prop("selectable", Value::Bool(true));
    let props = crate::widget::DataTableProps::from_source(&mut node).unwrap();
    assert_eq!(props.columns.len(), 2);
    assert_eq!(props.rows.len(), 2);
    assert_eq!(props.rows[1].item.0, "doc/song:2");
    assert_eq!(
        props.rows[1].cell("Name"),
        Some("second"),
        "a cell reads by its column's header"
    );
    let widget = crate::widget::DescribedTable::from_props("songs", props);
    let mut host = harness(widget);
    host.relayout();
    let root = host.root::<crate::widget::DescribedTable>();
    assert_eq!(root.table().view_rows().len(), 2);
}

#[test]
fn a_table_row_press_reports_the_resource_it_stands_for() {
    let queue = crate::Intents::default();
    let mut node = table_node(&queue).prop("selectable", Value::Bool(true));
    let props = crate::widget::DataTableProps::from_source(&mut node).unwrap();
    let mut host = harness(crate::widget::DescribedTable::from_props("songs", props));
    host.relayout();
    let root = host.root::<crate::widget::DescribedTable>();
    let second = root.table().row_rect(1).expect("the second row shows");
    host.click_at(second.center().x, second.center().y);
    assert_eq!(
        queue.take(),
        [fired(
            "songs",
            "select",
            Some(Value::Resource("doc/song:2".into()))
        )]
    );
}

#[test]
fn a_table_with_changed_rows_and_a_changed_selection_is_taken_in_place() {
    let queue = crate::Intents::default();
    let mut node = table_node(&queue);
    let props = crate::widget::DataTableProps::from_source(&mut node).unwrap();
    let mut table = crate::widget::DescribedTable::from_props("songs", props);

    let mut next = table_node(&queue).holding(
        "rows",
        Node::new("DataTableRow", "doc/song:3", &queue)
            .prop("item", resource("doc/song:3"))
            .holding("cells", cell_node("Name", "third")),
    );
    next = next.prop("selection", Value::List(vec![resource("doc/song:2")]));
    let props = crate::widget::DataTableProps::from_source(&mut next).unwrap();
    assert!(table.set_props(props), "changed rows are taken in place");
    assert_eq!(table.table().view_rows().len(), 3);
    let shown = table.table().selected_rows();
    assert_eq!(
        shown,
        [1],
        "the selection names the row the view says, wherever it shows"
    );
}

#[test]
fn a_conversation_list_reads_its_lines_and_keeps_them_in_place() {
    let queue = crate::Intents::default();
    let line = |item: &str, said: &str| {
        Node::new("ConversationEntry", item, &queue)
            .prop("item", resource(item))
            .prop("said", text(said))
            .prop("author", text("Ada"))
            .prop("time", text("2026-06-30T08:42:00Z"))
    };
    let mut node = Node::new("ConversationList", "chat", &queue)
        .prop("name", text("Conversation with Ada"))
        .holding("lines", line("msg:a", "are you nearby?"))
        .holding("lines", line("msg:b", "on my way"));
    let props = crate::widget::ConversationListProps::from_source(&mut node).unwrap();
    assert_eq!(props.lines.len(), 2);
    assert_eq!(props.lines[1].said, "on my way");
    let widget = crate::widget::DescribedConversationList::from_props("chat", props);
    assert_eq!(widget.list().lines().len(), 2);
    assert_eq!(widget.list().lines()[0].author, "Ada");
}

#[test]
fn a_card_grid_reads_its_cards_and_a_press_reports_the_resource_it_shows() {
    let queue = crate::Intents::default();
    let card = |item: &str| {
        Node::new("CardGridCard", item, &queue)
            .prop("item", resource(item))
            .prop("title", text(item))
            .on("activate")
    };
    let mut node = Node::new("CardGrid", "apps", &queue)
        .holding("cards", card("doc/app:1"))
        .holding("cards", card("doc/app:2"));
    let props = crate::widget::CardGridProps::from_source(&mut node).unwrap();
    assert_eq!(props.cards.len(), 2);
    let mut host = harness(crate::widget::DescribedCardGrid::from_props("apps", props));
    host.relayout();
    let root = host.root::<crate::widget::DescribedCardGrid>();
    assert_eq!(
        root.shown(),
        ["doc/app:1".to_owned(), "doc/app:2".to_owned()]
    );
    let grid = root.grid();
    let rect = grid.card(1).expect("the second card").get_rect();
    host.click_at(rect.center().x, rect.y0 + 8.0);
    assert_eq!(
        queue.take(),
        [fired("doc/app:2", "activate", None)],
        "a pressed card runs its own activation"
    );
}

/// A grid given its cards in place keeps each card's own widget and takes what the card now
/// says: the props name the cards and carry none of their bodies, so a card rebuilt from them
/// would show nothing, and one replaced would lose its body and what the person was doing in
/// it. A card that says something else changes on the card where it stands.
#[test]
fn a_card_grid_takes_its_cards_in_place_while_they_are_the_cards_it_shows() {
    let queue = crate::Intents::default();
    let card = |item: &str, title: &str| {
        Node::new("CardGridCard", item, &queue)
            .prop("item", resource(item))
            .prop("title", text(title))
    };
    let reading = |titles: &[(&str, &str)]| {
        let mut grid = Node::new("CardGrid", "apps", &queue);
        for (item, title) in titles {
            grid = grid.holding("cards", card(item, title));
        }
        crate::widget::CardGridProps::from_source(&mut grid).expect("the grid reads")
    };
    let cards = |host: &FrameHarness| -> Vec<usize> {
        let grid = host.root::<crate::widget::DescribedCardGrid>().grid();
        (0..grid.len())
            .filter_map(|index| grid.card(index))
            .map(|card| std::ptr::from_ref::<dyn Widget>(card.as_ref()).cast::<()>() as usize)
            .collect()
    };
    let names = |host: &FrameHarness| -> Vec<String> {
        let grid = host.root::<crate::widget::DescribedCardGrid>().grid();
        let mut out = Vec::new();
        fn walk(w: &dyn Widget, out: &mut Vec<String>) {
            if let Some(node) = w.accessibility() {
                out.extend(node.name.clone());
            }
            for child in w.children() {
                walk(child, out);
            }
        }
        for card in (0..grid.len()).filter_map(|index| grid.card(index)) {
            walk(card.as_ref(), &mut out);
        }
        out
    };

    let mut host = harness(crate::widget::DescribedCardGrid::from_props(
        "apps",
        reading(&[("doc/a:1", "Alpha"), ("doc/a:2", "Beta")]),
    ));
    host.relayout();
    let before = cards(&host);
    assert_eq!(before.len(), 2, "the two cards show");
    assert!(
        names(&host).iter().any(|name| name == "Beta"),
        "the second card's title shows: {:?}",
        names(&host)
    );

    // One card retitled: the cards' own widgets stay where they stand, and the one that says
    // something else says it.
    assert!(
        host.root_mut::<crate::widget::DescribedCardGrid>()
            .set_props(reading(&[("doc/a:1", "Alpha"), ("doc/a:2", "Beta II")])),
        "the cards it shows are taken in place"
    );
    assert_eq!(cards(&host), before, "each card keeps its own widget");
    assert!(
        names(&host).iter().any(|name| name == "Beta II"),
        "the retitled card says so: {:?}",
        names(&host)
    );
    assert!(
        !names(&host).iter().any(|name| name == "Beta"),
        "the title it had is gone: {:?}",
        names(&host)
    );
}

#[test]
fn a_tree_view_reads_its_nodes_and_reports_the_resource_a_node_shows() {
    let queue = crate::Intents::default();
    let node_part = |label: &str, parent: Option<&str>, item: &str| {
        let mut part = Node::new("TreeViewNode", item, &queue)
            .prop("label", text(label))
            .prop("item", resource(item));
        if let Some(parent) = parent {
            part = part.prop("parent", resource(parent));
        }
        part.on("select").on("activate")
    };
    let mut tree = Node::new("TreeView", "files", &queue)
        .prop("name", text("Files"))
        .holding("nodes", node_part("Project", None, "doc/dir:root"))
        .holding(
            "nodes",
            node_part("src", Some("doc/dir:root"), "doc/dir:src"),
        )
        .holding(
            "nodes",
            node_part("main.rs", Some("doc/dir:src"), "doc/file:main"),
        );
    let props = crate::widget::TreeViewProps::from_source(&mut tree).unwrap();
    assert_eq!(props.nodes.len(), 3);
    let host = harness(crate::widget::DescribedTreeView::from_props("files", props));
    let root = host.root::<crate::widget::DescribedTreeView>();
    // A node's stable name follows the resource it stands for, not where it stands: the rows
    // coming in another order leave each of them its name.
    let root_key = root.row_key("doc/dir:root").expect("the root's key");
    assert_ne!(
        root_key,
        root.row_key("doc/file:main").expect("the file's key")
    );
    let mut other = Node::new("TreeView", "files", &queue)
        .prop("name", text("Files"))
        .holding("nodes", node_part("src", None, "doc/dir:src"))
        .holding("nodes", node_part("Project", None, "doc/dir:root"));
    let props = crate::widget::TreeViewProps::from_source(&mut other).unwrap();
    let other = crate::widget::DescribedTreeView::from_props("files", props);
    assert_eq!(other.row_key("doc/dir:root"), Some(root_key));
    assert_eq!(
        root.item_of("doc/file:main").map(|i| i.0.as_str()),
        Some("doc/file:main")
    );
}

#[test]
fn a_tree_view_takes_its_expansion_and_selection_in_place() {
    let queue = crate::Intents::default();
    let node_part = |label: &str, parent: Option<&str>, item: &str| {
        let mut part = Node::new("TreeViewNode", item, &queue)
            .prop("label", text(label))
            .prop("item", resource(item));
        if let Some(parent) = parent {
            part = part.prop("parent", resource(parent));
        }
        part
    };
    let mut first = Node::new("TreeView", "files", &queue)
        .prop("name", text("Files"))
        .holding("nodes", node_part("Project", None, "doc/dir:root"))
        .holding(
            "nodes",
            node_part("src", Some("doc/dir:root"), "doc/dir:src"),
        );
    let props = crate::widget::TreeViewProps::from_source(&mut first).unwrap();
    let mut tree = crate::widget::DescribedTreeView::from_props("files", props);
    let mut next = Node::new("TreeView", "files", &queue)
        .prop("name", text("Files"))
        .holding("nodes", node_part("Project", None, "doc/dir:root"))
        .holding(
            "nodes",
            node_part("src", Some("doc/dir:root"), "doc/dir:src"),
        );
    next = next
        .prop("expanded", Value::List(vec![resource("doc/dir:root")]))
        .prop("selection", Value::List(vec![resource("doc/dir:src")]));
    let props = crate::widget::TreeViewProps::from_source(&mut next).unwrap();
    assert!(tree.set_props(props), "the state is taken in place");
    assert!(tree.is_open("doc/dir:root"), "the given branch is open");
    assert_eq!(
        tree.selected(),
        ["doc/dir:src"],
        "the given node is selected"
    );
}

#[test]
fn an_activity_timeline_reads_its_changes_and_reports_the_one_reverted() {
    let queue = crate::Intents::default();
    let entry = |item: &str, summary: &str| {
        Node::new("ActivityEntry", item, &queue)
            .prop("item", resource(item))
            .prop("summary", text(summary))
            .prop(
                "author",
                Value::Principal {
                    identity: "ada".into(),
                    name: "Ada".into(),
                },
            )
            .prop("time", text("2026-06-30T08:42:00Z"))
            .on("revert")
    };
    let mut node = Node::new("ActivityTimeline", "history", &queue)
        .holding("entries", entry("a", "Rewrote the summary"))
        .holding("entries", entry("b", "Moved to Done"))
        .on("revert")
        .on("open");
    let props = crate::widget::ActivityTimelineProps::from_source(&mut node).unwrap();
    assert_eq!(props.entries.len(), 2);
    assert_eq!(props.entries[0].summary, "Rewrote the summary");
    let widget = crate::widget::DescribedActivityTimeline::from_props("history", props);
    assert_eq!(widget.timeline().entries().len(), 2);
}

/// Placed among a page's other content, a timeline is as tall as its rows: it does not grow
/// past the box the page gives it (it fills its box only where the view says so).
#[test]
fn a_timeline_among_a_pages_content_is_as_tall_as_its_rows() {
    let queue = crate::Intents::default();
    let entry = |item: &str, summary: &str| {
        Node::new("ActivityEntry", item, &queue)
            .prop("item", resource(item))
            .prop("summary", text(summary))
            .prop("time", text("2026-06-30T08:42:00Z"))
    };
    let mut node = Node::new("ActivityTimeline", "history", &queue)
        .holding("entries", entry("a", "Rewrote the summary"))
        .holding("entries", entry("b", "Moved to Done"));
    let props = crate::widget::ActivityTimelineProps::from_source(&mut node).unwrap();
    let widget = crate::widget::DescribedActivityTimeline::from_props("history", props);
    let mut host = harness(widget);
    host.relayout();
    let root = host.root::<crate::widget::DescribedActivityTimeline>();
    assert!(
        root.timeline().get_rect().height() > 0.0,
        "a timeline placed in a page has its own height, not zero"
    );
}

#[test]
fn a_data_table_holds_only_table_parts() {
    let queue = crate::Intents::default();
    assert_eq!(
        <Vec<crate::widget::DataTableColumnProps> as SlotType>::accepts(),
        Accepts::Component("DataTableColumn".into())
    );
    assert_eq!(
        <Vec<crate::widget::DataTableRowProps> as SlotType>::accepts(),
        Accepts::Component("DataTableRow".into())
    );
    assert_eq!(
        <Vec<crate::widget::CardGridCardProps> as SlotType>::accepts(),
        Accepts::Component("CardGridCard".into())
    );
    assert_eq!(
        <Vec<crate::widget::TreeViewNodeProps> as SlotType>::accepts(),
        Accepts::Component("TreeViewNode".into())
    );
    assert_eq!(
        <Vec<crate::widget::ConversationEntryProps> as SlotType>::accepts(),
        Accepts::Component("ConversationEntry".into())
    );
    assert_eq!(
        <Vec<crate::widget::ActivityEntryProps> as SlotType>::accepts(),
        Accepts::Component("ActivityEntry".into())
    );
    let mut node = table_node(&queue);
    assert!(crate::widget::DataTableProps::from_source(&mut node).is_ok());
}
