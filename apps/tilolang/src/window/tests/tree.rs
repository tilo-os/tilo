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

/// The rows of the tree under `node`, in its order.
fn rows(node: &UiNode, out: &mut Vec<String>) {
    if node.kind == "ui.TreeViewNode"
        && let Some(label) = node.props.get("label")
    {
        out.push(tilo_ui_host::reconcile::display(label));
    }
    for child in &node.children {
        rows(child, out);
    }
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

/// The node of the tree labelled `label`, as the row it draws.
fn row_of<'a>(node: &'a UiNode, label: &str) -> Option<&'a UiNode> {
    let says = node
        .props
        .get("label")
        .is_some_and(|said| tilo_ui_host::reconcile::display(said) == label);
    if node.kind == "ui.TreeViewNode" && says {
        return Some(node);
    }
    node.children.iter().find_map(|child| row_of(child, label))
}

/// The first field of the page under `node`.
fn first_input(node: &UiNode) -> Option<&UiNode> {
    if node.kind == "ui.TextInput" {
        return Some(node);
    }
    node.children.iter().find_map(first_input)
}

impl Workspace {
    /// The labels of the tree the root's shell holds, in its order: the rows of a window that
    /// lists its items as an outline (a notebook's notes).
    pub(super) fn tree_labels(&self) -> Vec<String> {
        let tree = self.window.root_tree().expect("the root view runs");
        let mut found = Vec::new();
        trees(&tree, &mut found);
        let mut out = Vec::new();
        // The shelf of the notes a filter names stands over the notebook's own tree.
        if let Some(notebook) = found.last() {
            rows(notebook, &mut out);
        }
        out
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
    pub(super) fn tree_row_note(&self, label: &str) -> ResourceRef {
        let tree = self.window.root_tree().expect("the root view runs");
        match row_of(&tree, label).and_then(|row| row.props.get("item")) {
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
