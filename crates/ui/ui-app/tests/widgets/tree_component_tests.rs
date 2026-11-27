use std::cell::*;
use std::collections::*;
use std::path::*;
use std::rc::*;
use std::sync::atomic::*;
use std::sync::*;
use std::time::*;
use tilo_ui_app::app::driver::Driver;
use tilo_ui_app::headless::*;
use tilo_ui_app::kurbo::*;
use tilo_ui_app::retained_present::*;
use tilo_ui_app::testing::*;
use tilo_ui_app::vello::*;
use tilo_ui_app::widget::TreeMove;
use tilo_ui_app::widget::internals::tree_component::*;
use tilo_ui_app::widget::*;
use tilo_ui_base::a11y::*;
use tilo_ui_base::component::BoundValue;
use tilo_ui_base::component::Component;
use tilo_ui_base::component::Event;
use tilo_ui_base::component::Fired;
use tilo_ui_base::component::Intent;
use tilo_ui_base::component::*;
use tilo_ui_base::i18n::*;
use tilo_ui_base::icons::*;
use tilo_ui_base::interaction::*;
use tilo_ui_base::paint::*;
use tilo_ui_base::presence::*;
use tilo_ui_base::reorder::{DropPlacement, DropTarget, ReorderDrop};
use tilo_ui_base::synthetic_input::*;
use tilo_ui_base::text_selection::*;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::*;
use tilo_ui_base::widget::*;
use winit::event::*;
use winit::keyboard::*;

fn node(
    key: &str,
    parent: Option<&str>,
    branch: bool,
    queue: &tilo_ui_base::Intents<Fired>,
) -> TreeViewNodeProps {
    TreeViewNodeProps {
        item: ResourceId(key.to_owned()),
        label: key.to_owned(),
        parent: parent.map(|parent| ResourceId(parent.to_owned())),
        branch,
        check: Event::View(Intent::new(queue, key, "check")),
        ..TreeViewNodeProps::default()
    }
}

/// The three moves a tree reports, each on the tree itself.
fn moves(queue: &tilo_ui_base::Intents<Fired>) -> Moves {
    Moves {
        drop: Event::View(Intent::new(queue, "tree", "drop")),
        into: Event::View(Intent::new(queue, "tree", "into")),
        after: Event::View(Intent::new(queue, "tree", "after")),
    }
}

fn tree_of(nodes: Vec<TreeViewNodeProps>, hide_root: bool, moves: Moves) -> DescribedTreeView {
    DescribedTreeView::from_props(
        "tree",
        TreeViewProps {
            hide_root,
            nodes: nodes.into(),
            drop: moves.drop,
            into: moves.into,
            after: moves.after,
            ..TreeViewProps::default()
        },
    )
}

/// Two people moving a node into each other's subtree at the same moment can leave links
/// that close a loop. Every node is still held once: the one that comes last in the order
/// does not follow its link and stands at the top level.
#[test]
fn parents_that_close_a_loop_keep_every_node_once() {
    let queue = tilo_ui_base::Intents::default();
    let tree = tree_of(
        vec![
            node("a", Some("b"), false, &queue),
            node("b", Some("a"), true, &queue),
            node("c", Some("a"), false, &queue),
            node("itself", Some("itself"), false, &queue),
        ],
        true,
        Moves::default(),
    );
    let nodes = tree.nodes();
    assert_eq!(nodes.len(), 4, "every node is held once");
    let parent = |key: &str| {
        let at = nodes.index[key];
        nodes.tree_at(at).and_then(|row| row.parent)
    };
    assert_eq!(parent("b"), None);
    assert_eq!(parent("a"), Some("b".to_owned()));
    assert_eq!(parent("c"), Some("a".to_owned()));
    assert_eq!(parent("itself"), None);
    let top: Vec<&str> = nodes
        .nodes
        .iter()
        .filter(|node| node.parent.is_none())
        .map(|node| node.key.as_str())
        .collect();
    assert_eq!(top, ["b", "itself"]);
}

/// Hiding the root drops the tree's own name and keeps every node at the top level, one of
/// them as much as several: the heading above the tree names it once.
#[test]
fn hiding_the_root_keeps_every_top_level_node() {
    let queue = tilo_ui_base::Intents::default();
    let named = DescribedTreeView::from_props(
        "tree",
        TreeViewProps {
            name: Some("Subtasks".to_owned()),
            nodes: vec![
                node("a", None, false, &queue),
                node("b", None, false, &queue),
            ]
            .into(),
            ..TreeViewProps::default()
        },
    );
    let labels: Vec<(String, Option<String>)> = (0..named.nodes().len())
        .map(|at| {
            let node = &named.nodes().nodes[at];
            (node.label.clone(), node.parent.clone())
        })
        .collect();
    assert_eq!(
        labels,
        [
            ("Subtasks".to_owned(), None),
            ("a".to_owned(), Some(TOP_LEVEL.to_owned())),
            ("b".to_owned(), Some(TOP_LEVEL.to_owned())),
        ],
        "the tree's name stands over its nodes when it is not hidden"
    );
    let hidden = DescribedTreeView::from_props(
        "tree",
        TreeViewProps {
            name: Some("Subtasks".to_owned()),
            hide_root: true,
            nodes: vec![
                node("a", None, false, &queue),
                node("b", None, false, &queue),
            ]
            .into(),
            ..TreeViewProps::default()
        },
    );
    let labels: Vec<(String, Option<String>)> = (0..hidden.nodes().len())
        .map(|at| {
            let node = &hidden.nodes().nodes[at];
            (node.label.clone(), node.parent.clone())
        })
        .collect();
    assert_eq!(
        labels,
        [("a".to_owned(), None), ("b".to_owned(), None),],
        "the node's rows are the nodes, not the tree's name"
    );
}

/// A view that says which branches stand open — a search opens the way to what it found —
/// says nothing of the person's own folding: what they had opened is open again when the
/// view stops saying it.
#[test]
fn a_tree_the_view_narrows_keeps_the_person_s_own_folding() {
    let queue = tilo_ui_base::Intents::default();
    let nodes = || {
        vec![
            node("plan", None, true, &queue),
            node("standups", Some("plan"), true, &queue),
            node("monday", Some("standups"), false, &queue),
            node("home", None, true, &queue),
            node("groceries", Some("home"), false, &queue),
        ]
    };
    let props = |expanded: Option<Vec<ResourceId>>| TreeViewProps {
        hide_root: true,
        nodes: nodes().into(),
        expanded,
        ..TreeViewProps::default()
    };
    let mut tree = DescribedTreeView::from_props("tree", props(None));
    tree.test_marks().fold(&"home".to_owned(), true);
    assert!(
        tree.is_open("plan"),
        "nothing is folded but what the person folded"
    );
    let found = vec![ResourceId("plan".into()), ResourceId("standups".into())];
    tree.set_props(props(Some(found)));
    assert!(tree.is_open("plan") && tree.is_open("standups"));
    assert!(
        !tree.is_open("home"),
        "a branch the search does not open stands folded"
    );
    tree.set_props(props(None));
    assert!(tree.is_open("plan"), "the branch they were reading is open");
    assert!(
        !tree.is_open("home"),
        "the branch they had folded is still folded, not the search's folding",
    );
}

/// A move is told to the tree, with the row that moved and the row it lands at: the row it
/// goes before, the row it goes inside of, the row it goes after — with all of that row's
/// own — or the end of the top level, which names no row to land at.
#[test]
fn a_move_names_the_row_that_moved_and_the_row_it_lands_at() {
    let queue = tilo_ui_base::Intents::default();
    let tree = tree_of(
        vec![
            node("a", None, true, &queue),
            node("b", Some("a"), false, &queue),
            node("c", None, false, &queue),
        ],
        true,
        moves(&queue),
    );
    let nodes = tree.rows();
    let landed = |moved: &str, at: &str, event: &str| Fired {
        key: "tree".into(),
        event: event.into(),
        value: Some(Value::List(vec![
            Value::Resource(moved.to_owned()),
            Value::Resource(at.to_owned()),
        ])),
        gesture: None,
    };

    assert!(nodes.report(&ReorderDrop {
        source: "c".to_owned(),
        target: DropTarget::new("b".to_owned(), DropPlacement::Before),
    }));
    assert_eq!(queue.take(), [landed("c", "b", "drop")]);

    assert!(nodes.report(&ReorderDrop {
        source: "c".to_owned(),
        target: DropTarget::new("a".to_owned(), DropPlacement::Inside),
    }));
    assert_eq!(queue.take(), [landed("c", "a", "into")]);

    // // After a row: the row itself is named, so the owner places it past that row's own // without
    // the tree having to find the row after them, which may be one it never built.
    assert!(nodes.report(&ReorderDrop {
        source: "c".to_owned(),
        target: DropTarget::new("a".to_owned(), DropPlacement::After),
    }));
    assert_eq!(queue.take(), [landed("c", "a", "after")]);

    assert!(nodes.report(&ReorderDrop {
        source: "b".to_owned(),
        target: DropTarget::new(String::new(), DropPlacement::After),
    }));
    assert_eq!(
        queue.take(),
        [Fired {
            key: "tree".into(),
            event: "drop".into(),
            value: Some(Value::List(vec![
                Value::Resource("b".to_owned()),
                Value::None,
            ])),
            gesture: None,
        }]
    );
}

/// A row's quick actions ride over its trailing corner while the row is hovered, as a list
/// row's do: the toolkit's buttons, built again as the row shows.
#[test]
fn a_nodes_quick_actions_ride_over_its_row() {
    use tilo_ui_app::app::driver::Driver;
    use tilo_ui_base::theme::Theme;

    fn names(node: &tilo_ui_app::headless::InspectionNode, out: &mut Vec<String>) {
        out.push(node.widget.clone());
        for child in &node.children {
            names(child, out);
        }
    }
    let queue = tilo_ui_base::Intents::default();
    let mut quick = node("a", None, true, &queue);
    quick.toolbar = vec![ButtonProps {
        label: "New sub-note".into(),
        icon: Some(tilo_ui_base::icons::Icon::Plus),
        icon_only: true,
        press: Event::View(Intent::new(&queue, "a", "quick")),
        ..ButtonProps::default()
    }];
    let tree = tree_of(vec![quick], true, Moves::default());
    let harness = Driver::for_root(tilo_ui_app::State::new(), tree, (400, 240), Theme::dark());
    let mut laid = Vec::new();
    names(&harness.inspection().root, &mut laid);
    assert!(
        laid.iter().any(|widget| widget == "HoverToolbarFace"),
        "the row carries a hover toolbar: {laid:?}"
    );
    assert!(
        laid.iter().any(|widget| widget == "Button"),
        "with its quick action in it: {laid:?}"
    );
}

/// How far a branch got is the owner's count, said of the node: its row says it whether
/// the branch is folded or open, as a list row of the same item does. A node with no count
/// says none.
#[test]
fn a_node_says_how_far_its_branch_got() {
    let queue = tilo_ui_base::Intents::default();
    let mut said = node("a", None, true, &queue);
    said.progress = Some("1/3".into());
    said.row = Some(TreeViewRowProps::default());
    let mut child = node("b", Some("a"), false, &queue);
    child.row = Some(TreeViewRowProps {
        checkbox: Some(true),
        ..TreeViewRowProps::default()
    });
    let tree = tree_of(vec![said, child], true, Moves::default());
    let harness = Driver::for_root(tilo_ui_app::State::new(), tree, (400, 240), Theme::dark());
    fn read(node: &InspectionNode, out: &mut Vec<(String, Option<String>)>) {
        if let Some(access) = &node.semantics
            && let Some(name) = &access.name
        {
            out.push((name.clone(), access.value.clone()));
        }
        for child in &node.children {
            read(child, out);
        }
    }
    let mut values = Vec::new();
    read(&harness.inspection().root, &mut values);
    let value = |words: &str| {
        values
            .iter()
            .find(|(name, _)| name == words)
            .and_then(|(_, value)| value.clone())
    };
    assert_eq!(value("a").as_deref(), Some("1/3"), "the row says the count");
    assert_eq!(value("b"), None, "a node with no count says none");
}

/// A folder says it is a branch before its files are known: its row shows the disclosure
/// that opens it, even when no node under it is in the list yet.
#[test]
fn a_folder_with_nothing_in_it_still_shows_its_disclosure() {
    let queue = tilo_ui_base::Intents::default();
    let tree = tree_of(
        vec![
            node("folder", None, true, &queue),
            node("file", None, false, &queue),
        ],
        true,
        Moves::default(),
    );
    let nodes = tree.nodes();
    let of = |key: &str| nodes.index[key];
    assert!(
        nodes.has_subtasks(of("folder")),
        "a branch shows its disclosure with nothing under it"
    );
    assert!(!nodes.has_subtasks(of("file")), "a leaf folds nothing");
    assert_eq!(
        nodes.folded_at_first(&[]),
        vec!["folder".to_owned()],
        "a view that holds nothing open folds its branch, and the list opens it when read"
    );
}

/// A press on a tree's row chooses it whether or not the view names a choice event: the
/// tree keeps the row under the keyboard and the pointer alike, as every list does.
#[test]
fn a_press_chooses_a_trees_row_whatever_the_view_reports() {
    use tilo_ui_app::app::driver::Driver;
    use tilo_ui_base::theme::Theme;

    let queue = tilo_ui_base::Intents::default();
    for select in [
        Event::None,
        Event::View(Intent::new(
            &tilo_ui_base::Intents::default(),
            "tree",
            "select",
        )),
    ] {
        let tree = tree_of(
            vec![
                node("a", None, true, &queue),
                node("b", Some("a"), false, &queue),
            ],
            true,
            Moves {
                drop: select.clone(),
                ..Moves::default()
            },
        );
        let mut harness =
            Driver::for_root(tilo_ui_app::State::new(), tree, (400, 240), Theme::dark());
        let row = harness
            .root::<DescribedTreeView>()
            .test_list()
            .row_rect(&"b".to_owned())
            .expect("the leaf row stands somewhere");
        harness.click_at(row.center().x, row.center().y);
        assert_eq!(
            harness.root::<DescribedTreeView>().selected(),
            ["b".to_owned()],
            "a press chooses the row"
        );
    }
}

/// A row of a tree holds what it shows: the box that ticks it off and the quick action over
/// its corner are controls of the row, reached and worked under the one node it is.
#[test]
fn a_trees_row_holds_the_controls_it_shows() {
    use accesskit::Role as AkRole;
    use tilo_ui_app::app::driver::Driver;
    use tilo_ui_base::a11y::Action;
    use tilo_ui_base::theme::Theme;

    let queue = tilo_ui_base::Intents::default();
    let mut props = node("a", None, true, &queue);
    props.row = Some(TreeViewRowProps {
        checkbox: Some(false),
        ..TreeViewRowProps::default()
    });
    props.toolbar = vec![ButtonProps {
        label: "Add".to_owned(),
        press: Event::View(Intent::new(&queue, "a", "add")),
        ..ButtonProps::default()
    }];
    let mut harness = Driver::for_root(
        tilo_ui_app::State::new(),
        tree_of(vec![props], true, Moves::default()),
        (400, 400),
        Theme::dark(),
    );

    let nodes = harness.a11y_nodes();
    let node_of = |role: AkRole, label: &str| {
        nodes
            .iter()
            .find(|(_, node)| node.role() == role && node.label() == Some(label))
            .cloned()
    };
    let (row, _) = node_of(AkRole::TreeItem, "a").expect("the tree hands over its node");
    let mut controls = Vec::new();
    let mut frontier = vec![row];
    while let Some(next) = frontier.pop() {
        let children = nodes
            .iter()
            .find(|(id, _)| *id == next)
            .map(|(_, node)| node.children().to_vec())
            .unwrap_or_default();
        for child in children {
            controls.push(child);
            frontier.push(child);
        }
    }
    let label_of = |id: accesskit::NodeId| {
        nodes
            .iter()
            .find(|(each, _)| *each == id)
            .map(|(_, node)| node.clone())
            .expect("an exported node")
    };
    let checkbox = controls
        .iter()
        .map(|id| (*id, label_of(*id)))
        .find(|(_, node)| node.role() == AkRole::CheckBox)
        .expect("the row hands over its box");
    assert_eq!(checkbox.1.toggled(), Some(accesskit::Toggled::False));
    controls
        .iter()
        .map(|id| label_of(*id))
        .find(|node| node.role() == AkRole::Button && node.label() == Some("Add"))
        .expect("the row hands over its quick action");

    harness
        .perform_a11y_action(checkbox.0, &Action::Activate)
        .expect("the box is operated");
    let heard: Vec<_> = queue
        .take()
        .into_iter()
        .map(|fired| (fired.key, fired.event))
        .collect();
    assert_eq!(
        heard,
        [("a".to_owned(), "check".to_owned())],
        "the tick reaches the row's box, and the owner hears it"
    );
}

/// The list row the tree builds for the node `key`, read as the list row it is.
fn row_face<R>(tree: &DescribedTreeView, key: &str, read: impl FnOnce(&ListRow) -> R) -> R {
    let element = row_of(
        tree.rows(),
        key,
        tilo_ui_base::motion::Feel::Standard,
        tree.test_marks(),
        None,
        0.0,
    );
    let row = element
        .as_any()
        .and_then(|any| any.downcast_ref::<ListRow>())
        .unwrap_or_else(|| panic!("the row of {key} is a list row"));
    read(row)
}

/// A tree row shows who else is at the item it stands for, as a list row shows it.
#[test]
fn a_tree_row_says_who_else_is_at_the_item() {
    let queue = tilo_ui_base::Intents::default();
    let mut props = node("a", None, false, &queue);
    props.presence = Some(Person {
        identity: tilo_actions::MemberId::person("bo").into(),
        name: "Bo".to_owned(),
        hue: tilo_ui_base::tokens::PresenceHue::of_identity("bo"),
        picture: None,
    });
    let tree = tree_of(vec![props], true, Moves::default());
    assert_eq!(
        row_face(&tree, "a", |row| row.with_face(|face| face.presence_hue())),
        Some(tilo_ui_base::tokens::PresenceHue::of_identity("bo")),
        "the row says who else is at the item"
    );
    let plain = tree_of(vec![node("b", None, false, &queue)], true, Moves::default());
    assert_eq!(
        row_face(&plain, "b", |row| row.with_face(|face| face.presence_hue())),
        None,
        "and says nothing while nobody else is"
    );
}

/// A node someone removed that the person has not seen go stands struck, after the nodes,
/// at the top level: the row it stood under is one the tree may never have built.
#[test]
fn a_removed_node_stands_struck_after_the_nodes() {
    let queue = tilo_ui_base::Intents::default();
    let kept = node("a", None, false, &queue);
    let gone = TreeViewNodeProps {
        item: ResourceId("gone".to_owned()),
        label: "Gone".to_owned(),
        removed: true,
        ..TreeViewNodeProps::default()
    };
    let tree = DescribedTreeView::from_props(
        "tree",
        TreeViewProps {
            hide_root: true,
            nodes: vec![kept].into(),
            removed: vec![gone].into(),
            ..TreeViewProps::default()
        },
    );
    let rows = tree.rows();
    assert_eq!(rows.len(), 2, "the node and the row in place of it");
    let struck = rows.key_at(1).expect("the removed row");
    assert_eq!(
        rows.label(1).as_deref(),
        Some("Gone"),
        "the row says what it was called"
    );
    assert_eq!(
        rows.tree_at(1).and_then(|row| row.parent),
        None,
        "it stands at the top level"
    );
    assert!(
        !rows.has_subtasks(1),
        "a row in place of what left holds nothing"
    );
    assert!(
        row_face(&tree, &struck, |row| row
            .with_face(|face| face.title_struck())),
        "its title is struck through"
    );
    let kept = rows.key_at(0).expect("the node");
    assert!(
        !row_face(&tree, &kept, |row| row
            .with_face(|face| face.title_struck())),
        "a row the person still has is not struck"
    );
}

/// A row of a tree whose host renders its rows as the list shows them.
#[derive(Clone)]
struct ShownRow {
    key: String,
    parent: Option<String>,
    /// The row the view says stands over this one under the same parent.
    above: Option<String>,
    depth: i64,
    branch: bool,
}

/// The rows of a tree a host renders as the list shows them: every row at hand, and only the
/// rows of the window asked for rendered — a notebook of ten thousand notes gives the rows
/// on screen and forgets the rest.
struct ShownRows {
    rows: Vec<ShownRow>,
    shown: std::ops::Range<usize>,
    queue: tilo_ui_base::Intents<Fired>,
    /// The rows the host rendered anew, reported at the list's next poll.
    changed: Vec<ShownRow>,
}

impl ShownRows {
    fn at(&self, key: &str) -> Option<usize> {
        self.rows.iter().position(|row| row.key == key)
    }
}

/// A row read as its node: the props the view would give it.
struct RowSource {
    key: String,
    props: Vec<(&'static str, Value)>,
    queue: tilo_ui_base::Intents<Fired>,
}

impl tilo_ui_base::component::Source for RowSource {
    fn key(&self) -> &str {
        &self.key
    }

    fn prop(&mut self, name: &str) -> Option<Value> {
        self.props
            .iter()
            .find(|(prop, _)| *prop == name)
            .map(|(_, value)| value.clone())
    }

    fn event(&mut self, name: &str) -> Event {
        if name == "check" {
            Event::View(Intent::new(&self.queue, &self.key, name))
        } else {
            Event::None
        }
    }

    fn binding(&mut self, _name: &str) -> Option<BoundValue> {
        None
    }

    fn children(&mut self, _part: &str) -> Vec<tilo_ui_base::Element> {
        Vec::new()
    }

    fn components(
        &mut self,
        _part: &str,
        _read: &mut dyn FnMut(&mut dyn tilo_ui_base::component::Source) -> Result<(), String>,
    ) -> Result<usize, String> {
        Ok(0)
    }
}

impl tilo_ui_base::component::RowSource for ShownRows {
    fn len(&self) -> usize {
        self.rows.len()
    }

    fn key_at(&self, index: usize) -> Option<String> {
        self.rows.get(index).map(|row| row.key.clone())
    }

    fn index_of(&self, key: &str) -> Option<usize> {
        self.at(key)
    }

    fn request_range(&mut self, range: std::ops::Range<usize>, _open: Option<String>) {
        if self.shown == range {
            return;
        }
        self.shown = range;
        self.changed = self.rows.clone();
    }

    fn rendered(&self, key: &str) -> bool {
        self.at(key).is_some_and(|at| self.shown.contains(&at))
    }

    fn read(
        &mut self,
        key: &str,
        read: &mut dyn FnMut(&mut dyn tilo_ui_base::component::Source) -> Result<(), String>,
    ) -> Result<bool, String> {
        let Some(at) = self.at(key) else {
            return Ok(false);
        };
        if !self.shown.contains(&at) {
            return Ok(false);
        }
        let row = self.rows[at].clone();
        let mut props = vec![
            ("item", Value::Resource(row.key.clone())),
            ("label", Value::Text(row.key.clone())),
            ("depth", Value::Int(row.depth)),
            ("branch", Value::Bool(row.branch)),
        ];
        for (name, item) in [("parent", row.parent), ("above", row.above)] {
            if let Some(item) = item {
                props.push((name, Value::Resource(item)));
            }
        }
        read(&mut RowSource {
            key: row.key,
            props,
            queue: self.queue.clone(),
        })?;
        Ok(true)
    }

    fn key_presenting(&self, id: &str) -> Option<String> {
        self.at(id).map(|_| id.to_owned())
    }

    fn take_changes(
        &mut self,
    ) -> Vec<tilo_ui_base::collection_transition::ListModelChange<String>> {
        if self.changed.is_empty() {
            return Vec::new();
        }
        vec![
            tilo_ui_base::collection_transition::ListModelChange::RowsChanged {
                keys: std::mem::take(&mut self.changed)
                    .into_iter()
                    .map(|row| row.key)
                    .collect(),
            },
        ]
    }

    fn has_changes(&self) -> bool {
        !self.changed.is_empty()
    }
}

/// The tree of `rows` as a host renders them: only the rows the window shows are read, and
/// the queue its moves land in.
fn shown_tree(
    rows: Vec<ShownRow>,
) -> (
    Driver,
    tilo_ui_base::Intents<Fired>,
    std::rc::Rc<std::cell::RefCell<ShownRows>>,
) {
    let queue = tilo_ui_base::Intents::default();
    struct WithRows {
        rows: tilo_ui_base::component::SharedRows,
        queue: tilo_ui_base::Intents<Fired>,
    }
    impl tilo_ui_base::component::Source for WithRows {
        fn key(&self) -> &str {
            "tree"
        }

        fn prop(&mut self, name: &str) -> Option<Value> {
            match name {
                "reorder" | "tab_indents" | "hide_root" | "single_click" | "fill" => {
                    Some(Value::Bool(true))
                }
                _ => None,
            }
        }

        fn event(&mut self, name: &str) -> Event {
            matches!(name, "drop" | "into" | "after")
                .then(|| Event::View(Intent::new(&self.queue, "tree", name)))
                .unwrap_or(Event::None)
        }

        fn binding(&mut self, _name: &str) -> Option<BoundValue> {
            None
        }

        fn children(&mut self, _part: &str) -> Vec<tilo_ui_base::Element> {
            Vec::new()
        }

        fn components(
            &mut self,
            _part: &str,
            _read: &mut dyn FnMut(&mut dyn tilo_ui_base::component::Source) -> Result<(), String>,
        ) -> Result<usize, String> {
            Ok(0)
        }

        fn rows(&mut self, part: &str) -> Option<tilo_ui_base::component::SharedRows> {
            (part == "nodes").then(|| std::rc::Rc::clone(&self.rows))
        }
    }
    let shown = std::rc::Rc::new(std::cell::RefCell::new(ShownRows {
        rows,
        shown: 0..0,
        queue: queue.clone(),
        changed: Vec::new(),
    }));
    let rows: tilo_ui_base::component::SharedRows = shown.clone();
    let mut source = WithRows {
        rows,
        queue: queue.clone(),
    };
    let props = TreeViewProps::from_source(&mut source).expect("the tree reads");
    let tree = DescribedTreeView::from_props("tree", props);
    let harness = Driver::for_root(
        tilo_ui_app::State::new(),
        tree,
        (600, 400),
        tilo_ui_base::theme::Theme::light(),
    );
    (harness, queue, shown)
}

/// The move `movement` reports of the focused row, as the queue heard it.
fn moved(
    harness: &mut Driver,
    queue: &tilo_ui_base::Intents<Fired>,
    movement: TreeMove,
) -> Vec<(String, String, Option<Value>)> {
    harness
        .root_mut::<DescribedTreeView>()
        .test_list_mut()
        .move_focused_step(movement);
    queue
        .take()
        .into_iter()
        .map(|fired| (fired.key, fired.event, fired.value))
        .collect()
}

fn landing(moved: &str, at: &str, event: &str) -> (String, String, Option<Value>) {
    (
        "tree".to_owned(),
        event.to_owned(),
        Some(Value::List(vec![
            Value::Resource(moved.to_owned()),
            Value::Resource(at.to_owned()),
        ])),
    )
}

/// `Tab` puts a row under the one over it and `Shift-Tab` takes it back out, with the rows
/// between them never built: a branch of forty notes over the row leaves its own off the
/// window, and the view still says which row stands over which. The move names the branch,
/// which the tree has not built either, so the owner places the row all the same.
#[test]
fn tab_and_shift_tab_move_a_row_whose_neighbours_stand_off_the_window() {
    let mut rows = vec![ShownRow {
        key: "a".to_owned(),
        parent: None,
        above: None,
        depth: 0,
        branch: true,
    }];
    for at in 0..200 {
        rows.push(ShownRow {
            key: format!("a{at}"),
            parent: Some("a".to_owned()),
            above: Some(if at == 0 {
                "a".to_owned()
            } else {
                format!("a{}", at - 1)
            }),
            depth: 1,
            branch: false,
        });
    }
    rows.push(ShownRow {
        key: "b".to_owned(),
        parent: None,
        above: Some("a".to_owned()),
        depth: 0,
        branch: false,
    });
    let (mut harness, queue, shown) = shown_tree(rows);
    harness.relayout();
    {
        let mut root = harness.root_mut::<DescribedTreeView>();
        assert!(
            root.test_list_mut().reveal(&"b".to_owned()),
            "the row comes into view"
        );
        root.test_list_mut().set_focus(true);
    }
    harness.relayout();
    harness.relayout();
    let at = shown.borrow().at("b").expect("the last row");
    assert!(
        !shown.borrow().shown.contains(&0),
        "the branch over the row stands off the window: {:?}",
        shown.borrow().shown
    );
    assert!(at > 0);

    // // Tab puts it under the branch the view says stands over it: the branch's own rows // between
    // them were never built, and the branch itself was not either.
    assert_eq!(
        moved(&mut harness, &queue, TreeMove::Indent),
        [landing("b", "a", "into")],
        "Tab indents the row into the branch over it"
    );
    assert_eq!(moved(&mut harness, &queue, TreeMove::Outdent), []);
}

/// A row's branch can stand off the window as its parent does: `Shift-Tab` names it still,
/// from what the row itself says of it, and the row comes out beside it.
#[test]
fn shift_tab_lifts_a_row_out_from_a_parent_the_window_does_not_show() {
    let mut rows = vec![ShownRow {
        key: "a".to_owned(),
        parent: None,
        above: None,
        depth: 0,
        branch: true,
    }];
    for at in 0..200 {
        rows.push(ShownRow {
            key: format!("a{at}"),
            parent: Some("a".to_owned()),
            above: Some(if at == 0 {
                "a".to_owned()
            } else {
                format!("a{}", at - 1)
            }),
            depth: 1,
            branch: false,
        });
    }
    let (mut harness, queue, shown) = shown_tree(rows);
    harness.relayout();
    {
        let mut root = harness.root_mut::<DescribedTreeView>();
        assert!(
            root.test_list_mut().reveal(&"a199".to_owned()),
            "the row comes into view"
        );
        root.test_list_mut().set_focus(true);
    }
    harness.relayout();
    harness.relayout();
    assert!(
        !shown.borrow().shown.contains(&0),
        "the parent's own row stands off the window: {:?}",
        shown.borrow().shown
    );
    assert_eq!(
        moved(&mut harness, &queue, TreeMove::Outdent),
        [landing("a199", "a", "after")],
        "the row comes out beside the parent it stood under"
    );
}
