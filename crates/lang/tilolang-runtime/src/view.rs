//! View trees and their keyed diff (implementation design 4.5, views).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::handles::Retained;
use crate::ids::{FieldId, ModuleHash};
use crate::value::{ResourceRef, Value};
use crate::view_state::StateKey;

/// The property of a `list` in a card: how many of its rows it shows (`tilolang::prims::
/// PREVIEW_ROWS`). Only those are built.
pub(crate) const PREVIEW: &str = "preview";
/// The property a preview gets: how many rows the list has, the ones not built included.
pub(crate) const LIST_LEN: &str = "len";

/// A two-way binding: an input or checkbox edits this field or view state.
#[derive(Clone, Debug, PartialEq)]
pub struct Binding {
    pub target: BindTarget,
    /// The module whose code wrote the binding: the author of edits made through it.
    pub author: ModuleHash,
}

/// What a bound control edits.
#[derive(Clone, Debug, PartialEq)]
pub enum BindTarget {
    /// A field of a resource: an edit is an attributed change.
    Field { target: ResourceRef, field: FieldId },
    /// A `window` or `lens` declaration of the view (`bind: name`), with its name and its type
    /// as metadata writes it.
    State {
        key: StateKey,
        name: String,
        ty: String,
    },
}

impl Binding {
    /// The resource field this binding edits, when it edits one.
    pub fn field(&self) -> Option<(&ResourceRef, &FieldId)> {
        match &self.target {
            BindTarget::Field { target, field } => Some((target, field)),
            BindTarget::State { .. } => None,
        }
    }
}

/// A node of a rendered view. Events hold retained action references.
#[derive(Clone, Debug, PartialEq)]
pub struct UiNode {
    pub kind: String,
    pub key: String,
    pub props: BTreeMap<String, Value>,
    pub binding: Option<Binding>,
    pub events: BTreeMap<String, Retained>,
    pub anchor: Option<String>,
    pub children: Vec<UiNode>,
}

impl UiNode {
    /// A node of `kind` with `key` and nothing else.
    pub fn empty(kind: &str, key: &str) -> Self {
        Self {
            kind: kind.to_owned(),
            key: key.to_owned(),
            props: BTreeMap::new(),
            binding: None,
            events: BTreeMap::new(),
            anchor: None,
            children: Vec::new(),
        }
    }

    /// The node whose key is `key`, searching the whole tree. `None` when absent or when more
    /// than one node has the key. A `/`-joined path of keys from the root also works.
    pub fn find(&self, key: &str) -> Option<&Self> {
        let mut hits = Vec::new();
        self.collect(key, &mut hits);
        match hits.as_slice() {
            [one] => Some(one),
            [] => self.find_path(key),
            _ => None,
        }
    }

    fn collect<'a>(&'a self, key: &str, out: &mut Vec<&'a Self>) {
        if self.key == key {
            out.push(self);
        }
        for c in &self.children {
            c.collect(key, out);
        }
    }

    fn find_path(&self, path: &str) -> Option<&Self> {
        let mut parts = path.split('/');
        if parts.next()? != self.key {
            return None;
        }
        parts.try_fold(self, |node, part| {
            node.children.iter().find(|c| c.key == part)
        })
    }

    /// How many rows a `list` has: its children, or for a preview, which builds only the rows
    /// it shows, the count it carries (its `len` property).
    #[must_use]
    pub fn items_total(&self) -> usize {
        match self.props.get(LIST_LEN) {
            Some(Value::Int(len)) => usize::try_from(*len).unwrap_or(0),
            _ => self.children.len(),
        }
    }

    /// Number of nodes in the subtree.
    pub fn count(&self) -> usize {
        self.children
            .iter()
            .map(Self::count)
            .fold(1usize, usize::saturating_add)
    }
}

/// A node as the guest builds it.
#[derive(Clone, Debug)]
pub struct NodeBuild {
    pub kind: String,
    pub key: String,
    pub props: BTreeMap<String, Value>,
    pub children: Vec<usize>,
    pub binding: Option<Binding>,
    pub events: BTreeMap<String, Value>,
    pub anchor: Option<String>,
}

/// One change to a rendered tree. Paths are keys from the root down. Patches apply in order.
#[derive(Clone, Debug, PartialEq)]
pub enum Patch {
    /// The root itself changed kind or key.
    SetRoot(UiNode),
    Insert {
        parent: Vec<String>,
        index: usize,
        node: UiNode,
    },
    Remove {
        parent: Vec<String>,
        key: String,
    },
    Move {
        parent: Vec<String>,
        key: String,
        to: usize,
    },
    SetProp {
        path: Vec<String>,
        name: String,
        value: Option<Value>,
    },
    SetBinding {
        path: Vec<String>,
        binding: Option<Binding>,
    },
    SetEvent {
        path: Vec<String>,
        event: String,
        action: Option<Retained>,
    },
    SetAnchor {
        path: Vec<String>,
        anchor: Option<String>,
    },
}

/// Whether two retained action references are the same action on the same arguments.
pub type EventEq<'a> = &'a dyn Fn(Retained, Retained) -> bool;

/// One step of the rows of a virtual list (a `list` whose rows are built when shown, and are
/// absent from the tree until then), against the rows the host holds: every row, shown or not.
#[derive(Clone, Debug)]
pub(crate) enum RowStep {
    Remove(String),
    Insert(usize, Box<UiNode>),
    Move(String, usize),
}

/// What became of the rows of a virtual list when the boundary holding it ran: the steps from
/// the rows before to the rows now, or a list to read anew.
#[derive(Clone, Debug)]
pub(crate) enum RowsChange {
    Steps(Vec<RowStep>),
    Replace,
}

/// The virtual lists of a render, by key, and what became of their rows. The children of such
/// a list are the rows built, which are not all its rows: they are diffed by key, and its rows
/// change by their steps.
pub(crate) type VirtualLists = HashMap<String, RowsChange>;

/// The patches that turn `old` into `new`.
pub fn diff(old: &UiNode, new: &UiNode, same_event: EventEq<'_>) -> Vec<Patch> {
    diff_with(old, new, same_event, &VirtualLists::new())
}

/// [`diff`], for a tree that holds virtual lists.
pub(crate) fn diff_with(
    old: &UiNode,
    new: &UiNode,
    same_event: EventEq<'_>,
    lists: &VirtualLists,
) -> Vec<Patch> {
    if old.kind != new.kind || old.key != new.key || replaced(lists, new) {
        return vec![Patch::SetRoot(new.clone())];
    }
    let mut out = Vec::new();
    diff_node(
        old,
        new,
        std::slice::from_ref(&new.key),
        (same_event, lists),
        &mut out,
    );
    out
}

/// Whether `node` is a virtual list whose rows are to be read anew.
fn replaced(lists: &VirtualLists, node: &UiNode) -> bool {
    node.kind == "list" && matches!(lists.get(&node.key), Some(RowsChange::Replace))
}

fn diff_node(
    old: &UiNode,
    new: &UiNode,
    path: &[String],
    (same_event, lists): (EventEq<'_>, &VirtualLists),
    out: &mut Vec<Patch>,
) {
    let names: BTreeSet<&String> = old.props.keys().chain(new.props.keys()).collect();
    for name in names {
        let (a, b) = (old.props.get(name), new.props.get(name));
        if a != b {
            out.push(Patch::SetProp {
                path: path.to_vec(),
                name: name.clone(),
                value: b.cloned(),
            });
        }
    }
    if old.binding != new.binding {
        out.push(Patch::SetBinding {
            path: path.to_vec(),
            binding: new.binding.clone(),
        });
    }
    let events: BTreeSet<&String> = old.events.keys().chain(new.events.keys()).collect();
    for name in events {
        let same = match (old.events.get(name), new.events.get(name)) {
            (Some(a), Some(b)) => same_event(*a, *b),
            (None, None) => true,
            _ => false,
        };
        if !same {
            out.push(Patch::SetEvent {
                path: path.to_vec(),
                event: name.clone(),
                action: new.events.get(name).copied(),
            });
        }
    }
    if old.anchor != new.anchor {
        out.push(Patch::SetAnchor {
            path: path.to_vec(),
            anchor: new.anchor.clone(),
        });
    }
    if new.kind == "list"
        && let Some(RowsChange::Steps(steps)) = lists.get(&new.key)
    {
        virtual_rows(old, new, path, steps, (same_event, lists), out);
        return;
    }
    diff_children(old, new, path, (same_event, lists), out);
}

/// The rows of a virtual list: its steps, then what changed inside each row built before and
/// now, paired by key.
fn virtual_rows(
    old: &UiNode,
    new: &UiNode,
    path: &[String],
    steps: &[RowStep],
    cx: (EventEq<'_>, &VirtualLists),
    out: &mut Vec<Patch>,
) {
    for step in steps {
        out.push(match step {
            RowStep::Remove(key) => Patch::Remove {
                parent: path.to_vec(),
                key: key.clone(),
            },
            RowStep::Insert(index, node) => Patch::Insert {
                parent: path.to_vec(),
                index: *index,
                node: (**node).clone(),
            },
            RowStep::Move(key, to) => Patch::Move {
                parent: path.to_vec(),
                key: key.clone(),
                to: *to,
            },
        });
    }
    let before: HashMap<&str, &UiNode> = old
        .children
        .iter()
        .map(|row| (row.key.as_str(), row))
        .collect();
    for row in &new.children {
        if let Some(was) = before.get(row.key.as_str()) {
            let mut row_path = path.to_vec();
            row_path.push(row.key.clone());
            diff_node(was, row, &row_path, cx, out);
        }
    }
}

/// The patches that turn `old`, the only child of the node at `parent` (keys from the root),
/// into `new`.
pub(crate) fn diff_at(
    parent: &[String],
    old: &UiNode,
    new: &UiNode,
    cx: (EventEq<'_>, &VirtualLists),
    out: &mut Vec<Patch>,
) {
    if old.kind == new.kind && old.key == new.key && !replaced(cx.1, new) {
        let mut path = parent.to_vec();
        path.push(new.key.clone());
        diff_node(old, new, &path, cx, out);
        return;
    }
    out.push(Patch::Remove {
        parent: parent.to_vec(),
        key: old.key.clone(),
    });
    out.push(Patch::Insert {
        parent: parent.to_vec(),
        index: 0,
        node: new.clone(),
    });
}

/// The children's patches, in linear time: removals first, then each new child in order is
/// either already in place, moved there, or inserted there.
fn diff_children(
    old: &UiNode,
    new: &UiNode,
    path: &[String],
    cx: (EventEq<'_>, &VirtualLists),
    out: &mut Vec<Patch>,
) {
    if old.children.is_empty() && new.children.is_empty() {
        return;
    }
    let fresh: HashMap<(&str, &str), usize> = new
        .children
        .iter()
        .enumerate()
        .map(|(i, n)| ((n.key.as_str(), n.kind.as_str()), i))
        .collect();
    let mut current: Vec<&UiNode> = Vec::with_capacity(old.children.len());
    for o in &old.children {
        if fresh.contains_key(&(o.key.as_str(), o.kind.as_str())) {
            current.push(o);
        } else {
            out.push(Patch::Remove {
                parent: path.to_vec(),
                key: o.key.clone(),
            });
        }
    }
    let survivors: HashMap<&str, &UiNode> = current.iter().map(|o| (o.key.as_str(), *o)).collect();
    // Survivors moved ahead of their turn, which the walk over `current` then skips.
    let mut moved: HashSet<&str> = HashSet::new();
    let mut j = 0;
    for (i, n) in new.children.iter().enumerate() {
        while current
            .get(j)
            .is_some_and(|c| moved.contains(c.key.as_str()))
        {
            j = j.saturating_add(1);
        }
        let Some(o) = survivors.get(n.key.as_str()) else {
            out.push(Patch::Insert {
                parent: path.to_vec(),
                index: i,
                node: n.clone(),
            });
            continue;
        };
        if replaced(cx.1, n) {
            // A virtual list whose rows are new: the host reads it anew.
            if current.get(j).is_some_and(|c| c.key == n.key) {
                j = j.saturating_add(1);
            } else {
                moved.insert(n.key.as_str());
            }
            out.push(Patch::Remove {
                parent: path.to_vec(),
                key: n.key.clone(),
            });
            out.push(Patch::Insert {
                parent: path.to_vec(),
                index: i,
                node: n.clone(),
            });
            continue;
        }
        if current.get(j).is_some_and(|c| c.key == n.key) {
            j = j.saturating_add(1);
        } else {
            out.push(Patch::Move {
                parent: path.to_vec(),
                key: n.key.clone(),
                to: i,
            });
            moved.insert(n.key.as_str());
        }
        let mut child_path = path.to_vec();
        child_path.push(n.key.clone());
        diff_node(o, n, &child_path, cx, out);
    }
}

/// The steps from the rows keyed `old` to the rows keyed `new`, in linear time, as
/// [`diff_children`] finds them: removals first, then each row in order already in place,
/// moved there, or inserted there (`row` makes the node of an inserted one).
pub(crate) fn row_steps(
    old: &[String],
    new: &[String],
    row: &dyn Fn(&str) -> UiNode,
) -> Vec<RowStep> {
    let fresh: HashSet<&str> = new.iter().map(String::as_str).collect();
    let mut out = Vec::new();
    let mut current: Vec<&str> = Vec::with_capacity(old.len());
    for key in old {
        if fresh.contains(key.as_str()) {
            current.push(key);
        } else {
            out.push(RowStep::Remove(key.clone()));
        }
    }
    let survivors: HashSet<&str> = current.iter().copied().collect();
    let mut moved: HashSet<&str> = HashSet::new();
    let mut j = 0;
    for (i, key) in new.iter().enumerate() {
        while current.get(j).is_some_and(|c| moved.contains(c)) {
            j = j.saturating_add(1);
        }
        if !survivors.contains(key.as_str()) {
            out.push(RowStep::Insert(i, Box::new(row(key))));
            continue;
        }
        if current.get(j).is_some_and(|c| *c == key.as_str()) {
            j = j.saturating_add(1);
        } else {
            out.push(RowStep::Move(key.clone(), i));
            moved.insert(key.as_str());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(key: &str, children: Vec<UiNode>) -> UiNode {
        UiNode {
            kind: "column".into(),
            key: key.into(),
            props: BTreeMap::new(),
            binding: None,
            events: BTreeMap::new(),
            anchor: None,
            children,
        }
    }

    fn text(key: &str, v: i64) -> UiNode {
        let mut n = node(key, Vec::new());
        n.kind = "text".into();
        n.props.insert("text".into(), Value::Int(v));
        n
    }

    fn same(_: Retained, _: Retained) -> bool {
        true
    }

    #[test]
    fn one_changed_prop_is_one_patch() {
        let a = node("root", vec![text("a", 1), text("b", 1)]);
        let b = node("root", vec![text("a", 1), text("b", 2)]);
        let p = diff(&a, &b, &same);
        assert_eq!(
            p,
            vec![Patch::SetProp {
                path: vec!["root".into(), "b".into()],
                name: "text".into(),
                value: Some(Value::Int(2)),
            }]
        );
    }

    #[test]
    fn inserts_removes_and_moves_are_keyed() {
        let a = node("r", vec![text("a", 1), text("b", 1), text("c", 1)]);
        let b = node("r", vec![text("c", 1), text("a", 1), text("d", 1)]);
        let p = diff(&a, &b, &same);
        assert!(p.contains(&Patch::Remove {
            parent: vec!["r".into()],
            key: "b".into()
        }));
        assert!(
            p.iter()
                .any(|x| matches!(x, Patch::Insert { index: 2, .. }))
        );
        assert!(
            p.iter()
                .any(|x| matches!(x, Patch::Move { key, to: 0, .. } if key == "c"))
        );
        assert!(!p.iter().any(|x| matches!(x, Patch::SetProp { .. })));
    }

    #[test]
    fn find_by_key_and_path() {
        let t = node("r", vec![node("x", vec![text("t", 1)])]);
        assert!(t.find("t").is_some());
        assert!(t.find("r/x/t").is_some());
        assert!(t.find("nope").is_none());
    }
}
