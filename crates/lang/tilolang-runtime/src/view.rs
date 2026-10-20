//! View trees and their keyed diff (implementation design 4.5, views).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use crate::handles::Retained;
use crate::ids::{FieldId, ModuleHash};
use crate::value::{ResourceRef, Value};
use crate::view_state::StateKey;

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

/// The patches that turn `old` into `new`.
pub fn diff(old: &UiNode, new: &UiNode, same_event: EventEq<'_>) -> Vec<Patch> {
    if old.kind != new.kind || old.key != new.key {
        return vec![Patch::SetRoot(new.clone())];
    }
    let mut out = Vec::new();
    diff_node(
        old,
        new,
        std::slice::from_ref(&new.key),
        same_event,
        &mut out,
    );
    out
}

fn diff_node(
    old: &UiNode,
    new: &UiNode,
    path: &[String],
    same_event: EventEq<'_>,
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
    diff_children(old, new, path, same_event, out);
}

/// The patches that turn `old`, the only child of the node at `parent` (keys from the root),
/// into `new`.
pub fn diff_at(
    parent: &[String],
    old: &UiNode,
    new: &UiNode,
    same_event: EventEq<'_>,
    out: &mut Vec<Patch>,
) {
    if old.kind == new.kind && old.key == new.key {
        let mut path = parent.to_vec();
        path.push(new.key.clone());
        diff_node(old, new, &path, same_event, out);
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
    same_event: EventEq<'_>,
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
            j += 1;
        }
        let Some(o) = survivors.get(n.key.as_str()) else {
            out.push(Patch::Insert {
                parent: path.to_vec(),
                index: i,
                node: n.clone(),
            });
            continue;
        };
        if current.get(j).is_some_and(|c| c.key == n.key) {
            j += 1;
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
        diff_node(o, n, &child_path, same_event, out);
    }
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
