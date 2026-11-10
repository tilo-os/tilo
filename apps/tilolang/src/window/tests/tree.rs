//! What the tests read off a window that lists its items as a tree: the rows it draws, the one
//! it shows beside them, and the controls a header holds.

use super::*;

/// Every tree the node holds, outermost first.
fn trees<'a>(node: &'a UiNode, out: &mut Vec<&'a UiNode>) {
    if node.kind == "ui.TreeView" {
        out.push(node);
    }
    for child in &node.children {
        trees(child, out);
    }
}

/// The row of a tree the window shows, as the row it draws: a tree's rows are built as the
/// window shows them, so the row names its item in its `ui.TreeViewNode`.
fn tree_node(row: &UiNode) -> Option<&UiNode> {
    if row.kind == "ui.TreeViewNode" {
        return Some(row);
    }
    row.children.iter().find_map(tree_node)
}

/// What the row of a tree says of the item it stands for.
fn label_of(row: &UiNode) -> Option<String> {
    let node = tree_node(row)?;
    node.props
        .get("label")
        .map(tilo_ui_host::reconcile::display)
}

/// The tree the root's shell shows: the shelf of the items a filter names stands over the
/// window's own tree.
fn shown_tree(tree: &UiNode) -> Option<String> {
    let mut found = Vec::new();
    trees(tree, &mut found);
    found.last().map(|node| node.key.clone())
}

/// The buttons under `node`, in its order.
fn buttons(node: &UiNode, out: &mut Vec<String>) {
    if node.kind == "ui.Button"
        && let Some(label) = node.props.get("label")
    {
        out.push(tilo_ui_host::reconcile::display(label));
    }
    for child in &node.children {
        buttons(child, out);
    }
}

/// The first field of the page under `node`.
fn first_input(node: &UiNode) -> Option<&UiNode> {
    if node.kind == "ui.TextInput" {
        return Some(node);
    }
    node.children.iter().find_map(first_input)
}

impl Workspace {
    /// The rows the app rendered for the tree the root's shell shows, in the tree's order, as
    /// the window holds them. Every row is asked for, as a window tall enough for all of them
    /// asks: a tree's rows are built as the window shows them, so a tree no layout has given a
    /// height to holds none until they are asked for.
    fn tree_rows(&mut self) -> Vec<UiNode> {
        let tree = self.window.root_tree().expect("the root view runs");
        let Some(key) = shown_tree(&tree) else {
            return Vec::new();
        };
        let Some(rows) = self.window.view().rows_of(&key) else {
            return Vec::new();
        };
        let len = rows.borrow().len();
        rows.borrow_mut().request_range(0..len, None);
        self.frame();
        (0..len)
            .filter_map(|index| rows.borrow().key_at(index))
            .filter_map(|key| rows.borrow().row(&key).cloned())
            .collect()
    }

    /// The labels of the tree the root's shell holds, in its order: the rows of a window that
    /// lists its items as an outline (a notebook's notes).
    pub(super) fn tree_labels(&mut self) -> Vec<String> {
        self.tree_rows()
            .iter()
            .filter_map(|row| label_of(row))
            .collect()
    }

    /// The labels of the buttons a header slot `slot` of the composed shell holds, however
    /// deeply the view nested them (a row of controls, a control and its menu).
    pub(super) fn header_buttons(&self, slot: &str) -> Vec<String> {
        let composed = self.composed();
        let mut out = Vec::new();
        for node in slot_of(&composed, slot) {
            buttons(node, &mut out);
        }
        out
    }

    /// The note the row of the root's tree labelled `label` stands for.
    pub(super) fn tree_row_note(&mut self, label: &str) -> ResourceRef {
        let row = self
            .tree_rows()
            .into_iter()
            .find(|row| label_of(row).as_deref() == Some(label))
            .unwrap_or_else(|| panic!("no tree row {label}"));
        match tree_node(&row).and_then(|node| node.props.get("item")) {
            Some(Value::Ref(item)) => {
                ResourceRef::parse_id(&item.id, item.ty.clone()).expect("a resource")
            }
            _ => panic!("no tree row {label}"),
        }
    }

    /// The note the notebook's window shows beside its tree: the note its page edits.
    pub(super) fn opened_note(&self) -> ResourceRef {
        let composed = self.composed();
        let input = slot_of(&composed, "content")
            .into_iter()
            .find_map(first_input)
            .expect("the page edits the note it shows");
        let field = input
            .binding
            .as_ref()
            .and_then(|binding| binding.field())
            .expect("the field is bound");
        let host = self.window.current_host().expect("the host started");
        let probe = ResourceRef::parse_id(field.0, String::new()).expect("a resource id");
        let ty = tilolang_tilo::runtime::store::Store::type_of(host.store(), &probe)
            .expect("the note's type");
        ResourceRef::parse_id(field.0, ty).expect("a resource id")
    }
}


// The window's search narrows the tree: the notes the store finds, with the notes they stand
// under shown for them, and the tree as the person left it when the search clears.
#[test]
fn typing_in_the_window_s_search_narrows_a_tree_to_what_matches() {
    let mut workspace = Workspace::staged("notes", "a week of notes");
    let notebook = workspace.tree_labels();
    assert!(notebook.len() > 4, "the notebook's tree: {notebook:?}");
    let (mut driver, in_place) = workspace.window_search();
    assert_eq!(
        in_place,
        ui::widget::InPlace::Query,
        "the frame says where the search goes, so typing narrows the tree in place"
    );
    workspace.type_in_search(&mut driver, "Monday");
    let found = workspace.tree_labels();
    assert_eq!(
        found,
        ["Q3 plan", "Standups", "Monday"],
        "the note the words name, and the notes it stands under: {found:?}"
    );
    assert_eq!(
        driver.count(),
        Some(ui::widget::FilterCount::new(3, notebook.len())),
        "the search says how many rows it left of how many the tree showed"
    );
    // A note's body is rich text: the words of one find the note wherever the tree keeps it.
    workspace.type_in_search(&mut driver, "sync bug");
    assert_eq!(
        workspace.tree_labels(),
        ["Q3 plan", "Standups", "Monday"],
        "the note whose body says it, with the way to it"
    );
    // Closing the search widens the tree again, with the branches the person had opened.
    driver.drive(
        &mut workspace.window,
        "",
        ui::widget::FindOptions::default(),
        None,
        true,
    );
    workspace.settle();
    assert_eq!(
        workspace.tree_labels(),
        notebook,
        "the tree widens back to the notebook's notes"
    );
}
