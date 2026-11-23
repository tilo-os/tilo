//! The patches that bring one tree to another, by key.
//!
//! A tree sent over [`super`] is diffed against the copy the window holds, and the patches
//! alone are sent: a node kept by key keeps its widget, and with it focus, scroll position and
//! text state. Keyed by `Node::key`, which is stable for as long as the node shows the same
//! thing.
//!
//! Both sides of the protocol use this: a tilolang view's tree ([`crate::patch::Node`]) is
//! diffed by the runtime, and so is a Rust app's — a view is a function of a resource whichever
//! language wrote it, and the window draws both the same way (one-app-model.md §2).

use std::collections::HashSet;

use super::{Node, Patch};

/// The patches that bring `old`, the tree the window holds, to `new`, appended to `out`.
///
/// The whole tree is sent (`SetRoot`) when the root changed kind or key, or when the keys of
/// some node's children are not unique: patches by key need both to hold.
pub fn diff(old: &Node, new: &Node, out: &mut Vec<Patch>) {
    if old.kind != new.kind || old.key != new.key || !unique(old) || !unique(new) {
        out.push(Patch::SetRoot(new.clone()));
        return;
    }
    diff_node(old, new, &mut Vec::new(), out);
}

/// The node at `path` (keys below the root) of `node`, when each key on the way names one child.
#[must_use]
pub fn at<'a>(node: &'a Node, path: &[String]) -> Option<&'a Node> {
    path.iter().try_fold(node, |node, key| {
        let mut named = node.children.iter().filter(|child| child.key == *key);
        let child = named.next()?;
        named.next().is_none().then_some(child)
    })
}

/// The node at `path` (keys below the root) of `node`.
pub fn at_mut<'a>(node: &'a mut Node, path: &[String]) -> Option<&'a mut Node> {
    path.iter().try_fold(node, |node, key| {
        node.children.iter_mut().find(|child| child.key == *key)
    })
}

/// The patches that bring `old`, the node at `path` of the tree the window holds, to `new`,
/// appended to `out`; `false`, and nothing appended, when `new` is another node (another kind
/// or key) or its children's keys are not unique: then the tree is diffed whole.
pub fn diff_at(old: &Node, new: &Node, path: &[String], out: &mut Vec<Patch>) -> bool {
    if old.kind != new.kind || old.key != new.key || !unique(old) || !unique(new) {
        return false;
    }
    diff_node(old, new, &mut path.to_vec(), out);
    true
}

/// Whether the keys of each node's children are unique, all the way down: what patches by key
/// need.
fn unique(node: &Node) -> bool {
    let mut seen = HashSet::with_capacity(node.children.len());
    node.children.iter().all(|child| seen.insert(&child.key)) && node.children.iter().all(unique)
}

fn diff_node(old: &Node, new: &Node, path: &mut Vec<String>, out: &mut Vec<Patch>) {
    for (name, value) in &new.props {
        if old.props.get(name) != Some(value) {
            out.push(Patch::SetProp {
                path: path.clone(),
                name: name.clone(),
                value: Some(value.clone()),
            });
        }
    }
    for name in old
        .props
        .keys()
        .filter(|name| !new.props.contains_key(*name))
    {
        out.push(Patch::SetProp {
            path: path.clone(),
            name: name.clone(),
            value: None,
        });
    }
    if old.binding != new.binding {
        out.push(Patch::SetBinding {
            path: path.clone(),
            binding: new.binding.clone(),
        });
    }
    for (event, action) in &new.events {
        if old.events.get(event) != Some(action) {
            out.push(Patch::SetEvent {
                path: path.clone(),
                event: event.clone(),
                action: Some(action.clone()),
            });
        }
    }
    for event in old
        .events
        .keys()
        .filter(|event| !new.events.contains_key(*event))
    {
        out.push(Patch::SetEvent {
            path: path.clone(),
            event: event.clone(),
            action: None,
        });
    }
    if old.anchor != new.anchor {
        out.push(Patch::SetAnchor {
            path: path.clone(),
            anchor: new.anchor.clone(),
        });
    }
    diff_children(old, new, path, out);
}

/// The children of `old` brought to `new`'s by key: those gone removed, those kept moved to
/// their place and brought up to date there, those new inserted.
fn diff_children(old: &Node, new: &Node, path: &mut Vec<String>, out: &mut Vec<Patch>) {
    let wanted: HashSet<&str> = new
        .children
        .iter()
        .map(|child| child.key.as_str())
        .collect();
    let mut held: Vec<&Node> = Vec::with_capacity(old.children.len());
    for child in &old.children {
        if wanted.contains(child.key.as_str()) {
            held.push(child);
        } else {
            out.push(Patch::Remove {
                parent: path.clone(),
                key: child.key.clone(),
            });
        }
    }
    for (index, child) in new.children.iter().enumerate() {
        let at = held.iter().position(|old| old.key == child.key);
        let Some(at) = at else {
            out.push(Patch::Insert {
                parent: path.clone(),
                index: u32::try_from(index).unwrap_or(u32::MAX),
                node: child.clone(),
            });
            held.insert(index.min(held.len()), child);
            continue;
        };
        if at != index {
            out.push(Patch::Move {
                parent: path.clone(),
                key: child.key.clone(),
                to: u32::try_from(index).unwrap_or(u32::MAX),
            });
            let moved = held.remove(at);
            held.insert(index.min(held.len()), moved);
        }
        let before = held.get(index).copied();
        if let Some(before) = before
            && before != child
        {
            if before.kind == child.kind {
                path.push(child.key.clone());
                diff_node(before, child, path, out);
                path.pop();
            } else {
                out.push(Patch::Remove {
                    parent: path.clone(),
                    key: child.key.clone(),
                });
                out.push(Patch::Insert {
                    parent: path.clone(),
                    index: u32::try_from(index).unwrap_or(u32::MAX),
                    node: child.clone(),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patch::Value;

    fn text(key: &str, value: &str) -> Node {
        let mut node = Node::empty("ui.Text", key);
        node.props.insert("value".to_owned(), Value::text(value));
        node
    }

    fn stack(key: &str, children: Vec<Node>) -> Node {
        Node {
            children,
            ..Node::empty("stack", key)
        }
    }

    /// `old` patched by the diff to `new` is `new`.
    fn round_trip(old: &Node, new: &Node) -> Vec<Patch> {
        let mut patches = Vec::new();
        diff(old, new, &mut patches);
        let mut applied = old.clone();
        for patch in patches.clone() {
            patch.apply(&mut applied).expect("the diff's patches apply");
        }
        assert_eq!(&applied, new, "{patches:#?}");
        patches
    }

    #[test]
    fn the_diff_brings_the_windows_tree_to_the_views_by_key() {
        let old = stack(
            "s",
            vec![
                text("a", "a"),
                text("b", "b"),
                stack("c", vec![text("c1", "1")]),
            ],
        );
        let new = stack(
            "s",
            vec![
                text("n", "n"),
                stack("c", vec![text("c1", "2"), text("c2", "3")]),
                text("a", "a"),
            ],
        );
        let patches = round_trip(&old, &new);
        assert!(
            !patches
                .iter()
                .any(|patch| matches!(patch, Patch::SetRoot(_))),
            "a change by key, not the tree anew: {patches:#?}"
        );
        assert!(round_trip(&new, &new).is_empty(), "no change, no patch");
        // A child that changes kind under the same key is replaced.
        let swapped = stack("s", vec![Node::empty("divider", "a")]);
        round_trip(&old, &swapped);
        // A root of another kind is the tree anew.
        let other = Node::empty("page", "s");
        assert_eq!(
            round_trip(&old, &other),
            vec![Patch::SetRoot(other.clone())]
        );
    }

    #[test]
    fn siblings_sharing_a_key_are_sent_whole() {
        let old = stack("s", vec![text("a", "1"), text("a", "2")]);
        let new = stack("s", vec![text("a", "3")]);
        assert_eq!(round_trip(&old, &new), vec![Patch::SetRoot(new.clone())]);
    }
}
