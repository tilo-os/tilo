//! How a widget earns its [`SemanticId`]: one derivation shared by every walk
//! that names widgets — the retained [`SemanticTree`](crate::a11y_tree::SemanticTree)
//! and the AccessKit export — so a screen reader, a pick, a recorded step and
//! an agent all name one widget by one id.
//!
//! In order of precedence a widget is identified by its [`Widget::test_id`]
//! when that is unique in the live tree, by its
//! [`semantic_key`](crate::a11y::AccessNode::semantic_key) when that is unique
//! under its accessible parent, and otherwise by its widget-tree path from the
//! nearest ancestor that has one of those ids. A layout node id is never part
//! of it: those are arena slots a rebuild reassigns.

use std::collections::HashMap;

use crate::a11y::AccessNode;
use crate::a11y_tree::SemanticId;
use crate::widget::Widget;

/// How often each candidate identity occurs this frame, so only an identity
/// that names exactly one widget is used as one.
#[derive(Default)]
struct IdentityCounts {
    /// Test ids across the live tree, counted on accessible widgets only: a
    /// forwarding wrapper that reports its inner widget's id without being
    /// accessible itself does not count against it.
    test_ids: HashMap<String, usize>,
    /// Semantic keys per nearest accessible ancestor (by data address; `0`
    /// for the window root), which is the parent a keyed id is minted under.
    keys: HashMap<(usize, String), usize>,
    /// Each widget's semantics as the counting pass read them, so a walk asks
    /// every widget once. Keyed by data address and name, queued in walk order
    /// for a wrapper that shares its child's address.
    access: HashMap<(usize, String), std::collections::VecDeque<Option<AccessNode>>>,
}

fn widget_address(w: &dyn Widget) -> usize {
    std::ptr::from_ref::<dyn Widget>(w).cast::<()>() as usize
}

fn count<const BOUND: bool>(w: &dyn Widget, owner: usize, counts: &mut IdentityCounts) {
    let Ok(_entry) = crate::bound_dispatch::enter_traversal::<BOUND>(w) else {
        return;
    };
    let mut child_owner = owner;
    let access = w.accessibility();
    counts
        .access
        .entry((widget_address(w), w.name().to_owned()))
        .or_default()
        .push_back(access.clone());
    if let Some(access) = access {
        if let Some(id) = w.test_id().filter(|id| !id.is_empty()) {
            let count = counts.test_ids.entry(id.to_owned()).or_insert(0);
            *count = count.saturating_add(1);
        }
        if let Some(key) = access.semantic_key {
            let count = counts.keys.entry((owner, key)).or_insert(0);
            *count = count.saturating_add(1);
        }
        child_owner = widget_address(w);
    }
    for child in w.children() {
        count::<BOUND>(child, child_owner, counts);
    }
}

/// A widget's identity, as [`IdentityScope::enter`] derived it.
pub(crate) struct Identified {
    pub(crate) id: SemanticId,
    /// Declared by the widget (a unique test id or key) rather than its
    /// position: safe to keep beyond this frame.
    pub(crate) durable: bool,
    /// Where the structural paths of the widgets below start.
    pub(crate) children_base: String,
    /// The semantic key this widget declared that a sibling also declared:
    /// not used as identity, and worth reporting.
    pub(crate) shared_key: Option<SemanticId>,
}

/// The state one walk over a widget forest carries to identify its widgets.
pub(crate) struct IdentityScope {
    counts: IdentityCounts,
    /// Data addresses of the accessible widgets above the one being visited,
    /// innermost last; `0` stands for the window root.
    owners: Vec<usize>,
}

impl IdentityScope {
    /// Count every candidate identity in `roots` before the walk begins.
    pub(crate) fn new<const BOUND: bool>(roots: &[&dyn Widget]) -> Self {
        let mut counts = IdentityCounts::default();
        for root in roots {
            count::<BOUND>(*root, 0, &mut counts);
        }
        Self {
            counts,
            owners: vec![0],
        }
    }

    /// `w`'s semantics, as the counting pass read them.
    pub(crate) fn access_of(&mut self, w: &dyn Widget) -> Option<AccessNode> {
        self.counts
            .access
            .get_mut(&(widget_address(w), w.name().to_owned()))
            .and_then(std::collections::VecDeque::pop_front)
            .unwrap_or_else(|| w.accessibility())
    }

    /// Identify accessible widget `w` under `semantic_parent`, at widget-tree
    /// `path`, and descend into it. Pair with [`leave`](Self::leave).
    pub(crate) fn enter(
        &mut self,
        w: &dyn Widget,
        access: &AccessNode,
        semantic_parent: &SemanticId,
        path: String,
    ) -> Identified {
        let owner = self.owners.last().copied().unwrap_or(0);
        let unique_test_id = w
            .test_id()
            .filter(|id| !id.is_empty() && self.counts.test_ids.get(*id) == Some(&1));
        let mut shared_key = None;
        let unique_key = access.semantic_key.as_ref().filter(|key| {
            let shared = self
                .counts
                .keys
                .get(&(owner, (*key).clone()))
                .is_some_and(|count| *count > 1);
            if shared {
                shared_key = Some(SemanticId::keyed(semantic_parent, key));
            }
            !shared
        });
        let identified = if let Some(test_id) = unique_test_id {
            let id = SemanticId::test_id(test_id);
            Identified {
                children_base: id.0.clone(),
                id,
                durable: true,
                shared_key,
            }
        } else if let Some(key) = unique_key {
            let id = SemanticId::keyed(semantic_parent, key);
            Identified {
                children_base: id.0.clone(),
                id,
                durable: true,
                shared_key,
            }
        } else {
            Identified {
                id: SemanticId(path.clone()),
                durable: false,
                children_base: path,
                shared_key,
            }
        };
        self.owners.push(widget_address(w));
        identified
    }

    /// Leave the accessible widget last [`enter`](Self::enter)ed.
    pub(crate) fn leave(&mut self) {
        self.owners.pop();
    }
}

/// The next `Name[index]` path segment under `path_base`, counting same-named
/// siblings exactly as the headless driver's target registry does.
pub(crate) fn structural_path(
    w: &dyn Widget,
    path_base: &str,
    sibling_counts: &mut HashMap<String, usize>,
) -> String {
    let name = w.name();
    let index = sibling_counts.entry(name.to_owned()).or_insert(0);
    let current = *index;
    *index = index.saturating_add(1);
    if path_base.is_empty() {
        format!("{name}[{current}]")
    } else {
        format!("{path_base}/{name}[{current}]")
    }
}

/// The id of a synthetic child `index` of `parent` — keyed when it declares
/// a key, positional otherwise.
pub(crate) fn synthetic_id(parent: &SemanticId, index: usize, access: &AccessNode) -> SemanticId {
    access.key.map_or_else(
        || SemanticId::synthetic(parent, index),
        |key| SemanticId::keyed(parent, key),
    )
}
