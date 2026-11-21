//! What the instance patches into a view's tree: a splice's text, its virtual lists, its patches.

use std::collections::HashSet;

use super::{BTreeMap, Convert, Patch, UiNode, ViewInstance, ui, wire};

/// `current` with `delete` bytes at `at` replaced by `insert`, both ends on a character boundary.
#[must_use]
pub fn spliced(current: &str, at: usize, delete: usize, insert: &str) -> String {
    let floor = |mut at: usize| {
        at = at.min(current.len());
        while !current.is_char_boundary(at) {
            at = at.saturating_sub(1);
        }
        at
    };
    let start = floor(at);
    let end = floor(at.saturating_add(delete)).max(start);
    let mut text = String::with_capacity(current.len().saturating_add(insert.len()));
    text.push_str(current.get(..start).unwrap_or_default());
    text.push_str(insert);
    text.push_str(current.get(end..).unwrap_or_default());
    text
}

/// The virtual lists of a view's tree, by key, with their keys from the root.
pub(super) fn virtual_list_paths(tree: &UiNode) -> Vec<(String, Vec<String>)> {
    fn walk(node: &UiNode, path: &mut Vec<String>, out: &mut Vec<(String, Vec<String>)>) {
        path.push(node.key.clone());
        if ui::is_virtual_list(&node.kind) {
            out.push((node.key.clone(), path.clone()));
        }
        for child in &node.children {
            walk(child, path, out);
        }
        path.pop();
    }
    let mut out = Vec::new();
    walk(tree, &mut Vec::new(), &mut out);
    out
}

/// Render every row of `view`'s lists that no virtual list renders, whole or as its card's preview.
pub(super) fn show_content_rows(view: &mut ViewInstance) {
    fn lists(node: &UiNode, out: &mut Vec<String>) {
        if node.kind == "list" {
            out.push(node.key.clone());
        }
        for child in &node.children {
            lists(child, out);
        }
    }
    let mut shown = HashSet::new();
    loop {
        let mut found = Vec::new();
        lists(view.tree(), &mut found);
        found.retain(|key| !shown.contains(key));
        if found.is_empty() {
            return;
        }
        for list in found {
            for index in 0..view.list_len(&list).unwrap_or(0) {
                if let Err(error) = view.row(&list, index) {
                    tracing::warn!(%error, "a row of a list did not render");
                }
            }
            shown.insert(list);
        }
    }
}

/// What a view changed of a virtual list it renders as the list shows it.
#[derive(Default)]
pub(super) struct ListChanges {
    /// Rows inserted, removed or moved, in order.
    pub(super) structure: Vec<Patch>,
    /// The rows whose content changed.
    pub(super) rows: Vec<String>,
    /// The list was replaced as a whole: its rows are read anew.
    pub(super) reread: bool,
}

/// The patches that bring `sent`, the window's tree, to the view's `tree` where its patches
/// changed it (`changed`, paths of keys from the root), each changed node converted alone. `None`
/// and `sent` untouched when that cannot be done: the caller converts the whole tree.
pub(super) fn patch_in_place(
    sent: &mut ui::Node,
    tree: &UiNode,
    changed: &[Vec<String>],
    convert: &Convert<'_>,
) -> Option<Vec<ui::Patch>> {
    let mut paths: Vec<&[String]> = changed.iter().map(Vec::as_slice).collect();
    paths.sort_unstable();
    paths.dedup();
    // A change below one already converted is in it.
    let mut tops: Vec<&[String]> = Vec::with_capacity(paths.len());
    for path in paths {
        if !tops.iter().any(|top| path.starts_with(top)) {
            tops.push(path);
        }
    }
    let mut out = Vec::new();
    let mut fresh = Vec::with_capacity(tops.len());
    for path in tops {
        let (root, below) = path.split_first()?;
        if *root != tree.key || sent.key != tree.key {
            return None;
        }
        let node = below.iter().try_fold(tree, |node, key| {
            let mut named = node.children.iter().filter(|child| child.key == *key);
            let child = named.next()?;
            named.next().is_none().then_some(child)
        })?;
        let new = convert.node(node);
        if !wire::diff_at(wire::at(sent, below)?, &new, below, &mut out) {
            return None;
        }
        fresh.push((below, new));
    }
    for (below, new) in fresh {
        *wire::at_mut(sent, below)? = new;
    }
    Some(out)
}

/// What a view's patches changed, by where it shows.
#[derive(Default)]
pub(super) struct Sorted {
    /// The virtual lists' rows, by list key.
    pub(super) lists: BTreeMap<String, ListChanges>,
    pub(super) content: bool,
    /// Where it changed, each a path of keys from the root; `None` when the root was replaced.
    pub(super) changed: Option<Vec<Vec<String>>>,
}

impl Sorted {
    fn list(&mut self, key: &str) -> &mut ListChanges {
        self.lists.entry(key.to_owned()).or_default()
    }
}

/// Sort a view's `patches` against its virtual lists `lists`: their rows and changes, else content.
pub(super) fn sort(patches: Vec<Patch>, lists: &[(String, Vec<String>)]) -> Sorted {
    let mut sorted = Sorted {
        changed: Some(Vec::new()),
        ..Sorted::default()
    };
    for patch in patches {
        let (path, structural) = match &patch {
            Patch::SetRoot(_) => {
                sorted.content = true;
                sorted.changed = None;
                for (list, _) in lists {
                    sorted.list(list).reread = true;
                }
                continue;
            }
            // What a list holds beside its rows — the parts a host fills with rows of its own,
            // such as items someone removed that the person has not seen go — is no row of the
            // list: the list's rows are the ones the steps name, and the part is content.
            Patch::Insert { parent, node, .. } if node.kind == wire::PART => {
                (parent.as_slice(), false)
            }
            // A row the steps take out says where it stood; one they do not is no row.
            Patch::Remove {
                parent, at: None, ..
            } => (parent.as_slice(), false),
            Patch::Insert { parent, .. }
            | Patch::Remove { parent, .. }
            | Patch::Move { parent, .. } => (parent.as_slice(), true),
            Patch::SetProp { path, .. }
            | Patch::SetBinding { path, .. }
            | Patch::SetEvent { path, .. }
            | Patch::SetAnchor { path, .. } => (path.as_slice(), false),
        };
        // The innermost list the patch is of: a list in a row open can hold documents, so it
        // takes the changes of its own rows, and the row's list the rest of the row.
        let of_list = lists
            .iter()
            .filter(|(_, at)| {
                (structural && path == at.as_slice())
                    || (path.len() > at.len() && path.starts_with(at))
            })
            .max_by_key(|(_, at)| at.len());
        // What holds a list changed: the list may be another, and a row moved carries its lists.
        if structural && of_list.is_none_or(|(_, at)| path != at.as_slice()) {
            for (list, at) in lists {
                if at.len() > path.len() && at.starts_with(path) {
                    sorted.list(list).reread = true;
                }
            }
        }
        match of_list {
            Some((list, at)) if path == at.as_slice() => {
                sorted.list(list).structure.push(patch.clone());
            }
            Some((list, at)) => {
                if let Some(row) = path.get(at.len()) {
                    let changes = sorted.list(list);
                    if !changes.rows.contains(row) {
                        changes.rows.push(row.clone());
                    }
                }
            }
            None => {}
        }
        if of_list.is_none() {
            sorted.content = true;
            if let Some(changed) = &mut sorted.changed {
                changed.push(path.to_vec());
            }
        }
    }
    sorted
}
