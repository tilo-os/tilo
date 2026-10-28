//! What changed between two inspection snapshots, as a patch a mirror of the
//! older one applies to become the newer one.
//!
//! A host that mirrors the semantics tree (the browser page's accessibility
//! DOM) needs only what changed: while typing that is the edited field and
//! its caret, not the whole window's hundreds of nodes. The patch is a JSON
//! array of operations, applied in order, each naming a node by its `p`ath:
//! `"root"` or `"presentation"`, then child indices, with `"s"` stepping from
//! a retained widget into its semantics node.
//!
//! - `{"p":P,"own":N}`: node P's own fields become N's; its `children` (and,
//!   for a retained widget, its `semantics`) stay as they are.
//! - `{"p":P,"splice":[at,removed,[N…]]}`: in P's children, `removed` nodes
//!   at `at` give way to the whole nodes listed. Later operations address
//!   the list as it is after the splice.
//! - `{"p":P,"full":N}`: the value at P (a semantics node or the
//!   presentation tree, possibly `null`) becomes N, whole.
//!
//! Children are matched by identity (keys, semantic keys, action targets
//! and roles of semantic nodes; paths of widgets) from both ends of a list,
//! so a node inserted or removed in the middle costs that node, not every
//! sibling after it.
use super::{InspectionNode, InspectionSnapshot};
use crate::a11y::AccessNode;
use serde::Serialize;

/// How much of a tree a patch carries.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InspectionPatchStats {
    /// Nodes whose own fields the patch sends again.
    pub changed: usize,
    /// Nodes the patch sends whole: new to the tree, with all they contain.
    pub added: usize,
    /// Nodes the patch removes, with all they contain.
    pub removed: usize,
}

impl InspectionPatchStats {
    /// Whether the patch changes nothing in the nodes (the snapshot's focus
    /// owner and viewport are compared apart).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.changed == 0 && self.added == 0 && self.removed == 0
    }
}

/// Write the operations that turn `old` into `new` to `out`, as a JSON
/// array (see the module documentation), and say how much they carry.
/// `new` is lent mutably only to write a node's own fields without its
/// children; it is left as it was.
pub fn write_inspection_patch(
    old: &InspectionSnapshot,
    new: &mut InspectionSnapshot,
    out: &mut Vec<u8>,
) -> InspectionPatchStats {
    let mut writer = Writer {
        out,
        path: vec![Step::Root],
        first: true,
        stats: InspectionPatchStats::default(),
    };
    writer.out.push(b'[');
    InspectionNode::diff(&mut writer, &old.root, &mut new.root);
    writer.path[0] = Step::Presentation;
    match (&old.presentation, &mut new.presentation) {
        (Some(old), Some(new)) => InspectionNode::diff(&mut writer, old, new),
        (None, None) => {}
        (old, new) => {
            writer.stats.removed += old.as_ref().map_or(0, Node::count);
            writer.full(&*new, new.as_ref().map_or(0, Node::count));
        }
    }
    writer.out.push(b']');
    writer.stats
}

#[derive(Clone, Copy)]
enum Step {
    Root,
    Presentation,
    Semantics,
    Child(usize),
}

struct Writer<'a> {
    out: &'a mut Vec<u8>,
    path: Vec<Step>,
    first: bool,
    stats: InspectionPatchStats,
}

impl Writer<'_> {
    fn start(&mut self, kind: &str) {
        if !self.first {
            self.out.push(b',');
        }
        self.first = false;
        self.out.extend_from_slice(b"{\"p\":[");
        for (index, step) in self.path.iter().enumerate() {
            if index > 0 {
                self.out.push(b',');
            }
            match step {
                Step::Root => self.out.extend_from_slice(b"\"root\""),
                Step::Presentation => self.out.extend_from_slice(b"\"presentation\""),
                Step::Semantics => self.out.extend_from_slice(b"\"s\""),
                Step::Child(child) => self.out.extend_from_slice(child.to_string().as_bytes()),
            }
        }
        self.out.extend_from_slice(b"],\"");
        self.out.extend_from_slice(kind.as_bytes());
        self.out.extend_from_slice(b"\":");
    }

    fn value(&mut self, value: &impl Serialize) {
        if serde_json::to_writer(&mut *self.out, value).is_err() {
            self.out.extend_from_slice(b"null");
        }
    }

    fn own(&mut self, node: &impl Serialize) {
        self.start("own");
        self.value(node);
        self.out.push(b'}');
        self.stats.changed += 1;
    }

    fn full(&mut self, value: &impl Serialize, nodes: usize) {
        self.start("full");
        self.value(value);
        self.out.push(b'}');
        self.stats.added += nodes;
    }

    fn splice<T: Node>(&mut self, at: usize, removed: &[T], added: &[T]) {
        self.start("splice");
        self.out
            .extend_from_slice(format!("[{at},{},", removed.len()).as_bytes());
        self.value(&added);
        self.out.extend_from_slice(b"]}");
        self.stats.removed += removed.iter().map(Node::count).sum::<usize>();
        self.stats.added += added.iter().map(Node::count).sum::<usize>();
    }

    fn child<T: Node>(&mut self, index: usize, old: &T, new: &mut T) {
        self.path.push(Step::Child(index));
        T::diff(self, old, new);
        self.path.pop();
    }

    /// Match `old` and `new` by identity from both ends; what is left in
    /// the middle is spliced, the matched pairs compared.
    fn list<T: Node>(&mut self, old: &[T], new: &mut [T]) {
        let both = old.len().min(new.len());
        let mut head = 0;
        while head < both && old[head].same(&new[head]) {
            head += 1;
        }
        let mut tail = 0;
        while tail < both - head && old[old.len() - 1 - tail].same(&new[new.len() - 1 - tail]) {
            tail += 1;
        }
        if head + tail < old.len() || head + tail < new.len() {
            self.splice(
                head,
                &old[head..old.len() - tail],
                &new[head..new.len() - tail],
            );
        }
        for index in 0..head {
            self.child(index, &old[index], &mut new[index]);
        }
        for back in 0..tail {
            let (old_index, new_index) = (old.len() - 1 - back, new.len() - 1 - back);
            self.child(new_index, &old[old_index], &mut new[new_index]);
        }
    }
}

trait Node: Serialize {
    /// Whether `other` stands for the same node, whatever it now says.
    fn same(&self, other: &Self) -> bool;
    /// This node and every node inside it.
    fn count(&self) -> usize;
    fn diff(writer: &mut Writer<'_>, old: &Self, new: &mut Self);
}

impl Node for AccessNode {
    fn same(&self, other: &Self) -> bool {
        self.role == other.role
            && self.key == other.key
            && self.semantic_key == other.semantic_key
            && self.action_target == other.action_target
    }

    fn count(&self) -> usize {
        1 + self.children.iter().map(Node::count).sum::<usize>()
    }

    fn diff(writer: &mut Writer<'_>, old: &Self, new: &mut Self) {
        if !access_own_eq(old, new) {
            let children = std::mem::take(&mut new.children);
            writer.own(&*new);
            new.children = children;
        }
        writer.list(&old.children, &mut new.children);
    }
}

impl Node for InspectionNode {
    fn same(&self, other: &Self) -> bool {
        self.path == other.path && self.widget == other.widget
    }

    fn count(&self) -> usize {
        1 + self.semantics.as_ref().map_or(0, Node::count)
            + self.children.iter().map(Node::count).sum::<usize>()
    }

    fn diff(writer: &mut Writer<'_>, old: &Self, new: &mut Self) {
        if !inspection_own_eq(old, new) {
            let children = std::mem::take(&mut new.children);
            let semantics = new.semantics.take();
            writer.own(&*new);
            new.children = children;
            new.semantics = semantics;
        }
        writer.path.push(Step::Semantics);
        match (&old.semantics, &mut new.semantics) {
            (Some(old), Some(new)) => AccessNode::diff(writer, old, new),
            (None, None) => {}
            (old, new) => {
                writer.stats.removed += old.as_ref().map_or(0, Node::count);
                writer.full(&*new, new.as_ref().map_or(0, Node::count));
            }
        }
        writer.path.pop();
        writer.list(&old.children, &mut new.children);
    }
}

/// Whether two semantic nodes say the same apart from their children.
/// Every field is named, so a new one cannot be missed here.
fn access_own_eq(a: &AccessNode, b: &AccessNode) -> bool {
    let AccessNode {
        role,
        semantic_key,
        action_target,
        name,
        description,
        value,
        text_selection,
        state,
        live,
        set_size,
        pos_in_set,
        level,
        key,
        bounds,
        children: _,
        interface_name: _,
        role_description,
    } = a;
    *role == b.role
        && *semantic_key == b.semantic_key
        && *action_target == b.action_target
        && *name == b.name
        && *description == b.description
        && *value == b.value
        && *text_selection == b.text_selection
        && *state == b.state
        && *live == b.live
        && *set_size == b.set_size
        && *pos_in_set == b.pos_in_set
        && *level == b.level
        && *key == b.key
        && *bounds == b.bounds
        && *role_description == b.role_description
}

/// Whether two widget nodes say the same apart from their semantics and
/// children.
fn inspection_own_eq(a: &InspectionNode, b: &InspectionNode) -> bool {
    let InspectionNode {
        path,
        widget,
        test_id,
        focusable,
        rect,
        child_clip,
        layout,
        semantics: _,
        children: _,
    } = a;
    *path == b.path
        && *widget == b.widget
        && *test_id == b.test_id
        && *focusable == b.focusable
        && *rect == b.rect
        && *child_clip == b.child_clip
        && *layout == b.layout
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::a11y::{AccessState, Role};

    fn item(key: u64, name: &str) -> AccessNode {
        AccessNode::new(Role::ListItem)
            .with_name(name)
            .with_key(key)
    }

    fn snapshot(items: Vec<AccessNode>, field: &str) -> InspectionSnapshot {
        let list = AccessNode::new(Role::List).with_children(items);
        let input = AccessNode::new(Role::TextInput)
            .with_name("Message")
            .with_value(field)
            .with_state(AccessState::new().focusable(true).focused(true));
        let app = AccessNode::new(Role::Group).with_children(vec![input, list]);
        let node = |path: &str, semantics, children| InspectionNode {
            path: path.into(),
            widget: path.into(),
            test_id: None,
            focusable: false,
            rect: [0.0, 0.0, 100.0, 100.0],
            child_clip: None,
            layout: None,
            semantics,
            children,
        };
        InspectionSnapshot {
            viewport: [100, 100],
            keyboard_focus_owner: Some("App[0]".into()),
            root: node("Shell[0]", None, vec![node("App[0]", Some(app), vec![])]),
            presentation: None,
        }
    }

    fn items(keys: impl IntoIterator<Item = u64>) -> Vec<AccessNode> {
        keys.into_iter()
            .map(|key| item(key, &format!("Message {key}")))
            .collect()
    }

    fn patch(
        old: &InspectionSnapshot,
        new: &mut InspectionSnapshot,
    ) -> (String, InspectionPatchStats) {
        let mut out = Vec::new();
        let stats = write_inspection_patch(old, new, &mut out);
        (String::from_utf8(out).unwrap(), stats)
    }

    #[test]
    fn an_unchanged_tree_patches_nothing() {
        let old = snapshot(items(0..50), "hello");
        let mut new = snapshot(items(0..50), "hello");
        let (text, stats) = patch(&old, &mut new);
        assert_eq!(text, "[]");
        assert!(stats.is_empty());
    }

    #[test]
    fn a_keystroke_sends_the_field_alone() {
        let old = snapshot(items(0..50), "hello");
        let mut new = snapshot(items(0..50), "hello!");
        let (text, stats) = patch(&old, &mut new);
        assert_eq!(
            stats,
            InspectionPatchStats {
                changed: 1,
                added: 0,
                removed: 0
            }
        );
        assert!(text.starts_with(r#"[{"p":["root",0,"s",0],"own":{"role":"TextInput""#));
        assert!(text.contains(r#""value":"hello!""#));
        assert!(
            text.contains(r#""children":[]"#),
            "own fields go without children"
        );
        assert!(text.len() < 600, "{} bytes: {text}", text.len());
        // The input is lent back as it was.
        assert_eq!(
            new.root.children[0]
                .semantics
                .as_ref()
                .unwrap()
                .children
                .len(),
            2
        );
    }

    #[test]
    fn a_message_in_the_middle_costs_that_message() {
        let old = snapshot(items(0..50), "");
        let mut new = snapshot(items((0..20).chain([99]).chain(20..50)), "");
        let (text, stats) = patch(&old, &mut new);
        assert_eq!(
            stats,
            InspectionPatchStats {
                changed: 0,
                added: 1,
                removed: 0
            }
        );
        assert!(text.contains(r#"{"p":["root",0,"s",1],"splice":[20,0,[{"role":"ListItem""#));
        let mut gone = snapshot(items((0..10).chain(11..50)), "");
        let (text, stats) = patch(&old, &mut gone);
        assert_eq!(stats.removed, 1);
        assert_eq!(stats.added + stats.changed, 0);
        assert_eq!(text, r#"[{"p":["root",0,"s",1],"splice":[10,1,[]]}]"#);
    }

    #[test]
    fn semantics_appearing_and_the_presentation_tree_go_whole() {
        let old = snapshot(items(0..3), "");
        let mut new = snapshot(items(0..3), "");
        new.root.semantics = Some(AccessNode::new(Role::Window).with_name("Desk"));
        new.presentation = Some(InspectionNode {
            path: "History[0]".into(),
            widget: "History".into(),
            test_id: None,
            focusable: false,
            rect: [0.0; 4],
            child_clip: None,
            layout: None,
            semantics: None,
            children: vec![],
        });
        let (text, stats) = patch(&old, &mut new);
        assert_eq!(stats.added, 2);
        assert!(text.starts_with(r#"[{"p":["root","s"],"full":{"role":"Window""#));
        assert!(text.contains(r#"{"p":["presentation"],"full":{"path":"History[0]""#));
        let (text, stats) = patch(&new, &mut snapshot(items(0..3), ""));
        assert_eq!(stats.removed, 2);
        assert!(text.contains(r#"{"p":["root","s"],"full":null}"#));
        assert!(text.contains(r#"{"p":["presentation"],"full":null}"#));
    }
}
