//! Pure data + serde representation of the notes workspace.
//!
//! The on-wire format used inside the automerge `Text` blob is the JSON
//! serialisation of [`WorkspaceData`]. Phase 1 doesn't yet replicate the
//! tree as individual automerge fields — concurrent editing isn't a
//! real concern this phase. When it becomes one (Phase 5) we can flip
//! the storage to per-field automerge with a compatible-enough JSON
//! migration path.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Identifier for a single page. Stable across renames; backs the on-disk
/// file name (`{uuid}.md`) and every cross-reference in the workspace
/// tree.
pub type PageId = Uuid;

/// Per-page metadata. Title is mutable, parent + order define the tree.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageMeta {
    pub id: PageId,
    pub title: String,
    pub parent: Option<PageId>,
    /// Position among siblings (smaller = earlier). Re-densified by
    /// [`Workspace::renumber_siblings`] whenever a page is inserted /
    /// removed so the values stay contiguous.
    pub order: u32,
}

/// Top-level state for one notes workspace. The `pages` list is the
/// flat source of truth; the tree structure is derived from `parent`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct WorkspaceData {
    pub pages: Vec<PageMeta>,
    pub selected: Option<PageId>,
}

/// Convenience wrapper around [`WorkspaceData`] with the runtime ops
/// the UI / storage layer needs. Pure logic — no IO, no rendering.
#[derive(Clone, Debug, Default)]
pub struct Workspace {
    pub data: WorkspaceData,
}

impl Workspace {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn pages(&self) -> &[PageMeta] {
        &self.data.pages
    }

    pub fn selected(&self) -> Option<PageId> {
        self.data.selected
    }

    pub fn set_selected(&mut self, id: Option<PageId>) {
        self.data.selected = id;
    }

    /// Look a page up by id. None if unknown.
    pub fn get(&self, id: PageId) -> Option<&PageMeta> {
        self.data.pages.iter().find(|p| p.id == id)
    }

    pub fn get_mut(&mut self, id: PageId) -> Option<&mut PageMeta> {
        self.data.pages.iter_mut().find(|p| p.id == id)
    }

    /// Returns the children of `parent` (or roots when `parent` is None),
    /// sorted by `order`. Walks the flat list once.
    pub fn children_of(&self, parent: Option<PageId>) -> Vec<&PageMeta> {
        let mut out: Vec<&PageMeta> = self
            .data
            .pages
            .iter()
            .filter(|p| p.parent == parent)
            .collect();
        out.sort_by_key(|p| p.order);
        out
    }

    /// Create a new page as a child of `parent`. Returns the new id.
    /// The page is appended at the end of `parent`'s children. The
    /// title defaults to `"Untitled"` — callers wanting a different
    /// initial value should rename right after creation.
    pub fn create_page(&mut self, parent: Option<PageId>) -> PageId {
        let id = Uuid::new_v4();
        let order = self
            .children_of(parent)
            .last()
            .map(|p| p.order + 1)
            .unwrap_or(0);
        self.data.pages.push(PageMeta {
            id,
            title: "Untitled".to_string(),
            parent,
            order,
        });
        id
    }

    /// Rename the given page. Returns true if the page existed.
    pub fn rename(&mut self, id: PageId, new_title: &str) -> bool {
        if let Some(p) = self.get_mut(id) {
            p.title = new_title.to_string();
            true
        } else {
            false
        }
    }

    /// Remove the page (and recursively every descendant). Returns
    /// the set of removed ids so callers (the storage layer) can
    /// delete the corresponding `.md` files. Renumbers the surviving
    /// siblings on each affected parent so `order` stays contiguous.
    pub fn delete_page(&mut self, id: PageId) -> Vec<PageId> {
        // Collect every descendant of `id` (inclusive) into a flat
        // removal set.
        let mut to_remove: Vec<PageId> = Vec::new();
        let mut stack = vec![id];
        while let Some(cur) = stack.pop() {
            to_remove.push(cur);
            for child in self.children_of(Some(cur)) {
                stack.push(child.id);
            }
        }
        let removed_set: std::collections::HashSet<PageId> =
            to_remove.iter().copied().collect();

        // Record affected parents (so we know which sibling lists to
        // re-densify after the removal).
        let mut affected_parents: std::collections::HashSet<Option<PageId>> =
            std::collections::HashSet::new();
        for p in &self.data.pages {
            if removed_set.contains(&p.id) {
                affected_parents.insert(p.parent);
            }
        }

        self.data.pages.retain(|p| !removed_set.contains(&p.id));
        if self.data.selected.map(|s| removed_set.contains(&s)).unwrap_or(false) {
            self.data.selected = None;
        }

        for parent in affected_parents {
            self.renumber_siblings(parent);
        }

        to_remove
    }

    /// Move `id` up (`delta < 0`) or down (`delta > 0`) among its
    /// siblings by swapping `order` with the adjacent sibling in that
    /// direction, then re-densifying. Returns `true` if the page moved
    /// (i.e. it existed and wasn't already at the requested edge).
    ///
    /// Only reorders within a single sibling group — re-parenting is a
    /// separate concern. Backs the sidebar's Alt-↑ / Alt-↓ reorder.
    pub fn move_sibling(&mut self, id: PageId, delta: i32) -> bool {
        if delta == 0 {
            return false;
        }
        let parent = match self.get(id) {
            Some(p) => p.parent,
            None => return false,
        };
        // Siblings sorted by order; find where `id` sits.
        let siblings: Vec<PageId> =
            self.children_of(parent).iter().map(|p| p.id).collect();
        let pos = match siblings.iter().position(|s| *s == id) {
            Some(p) => p as i32,
            None => return false,
        };
        let target = pos + delta.signum();
        if target < 0 || target as usize >= siblings.len() {
            return false;
        }
        let other = siblings[target as usize];
        // Swap the two pages' order values, then re-densify to keep the
        // "contiguous from 0" invariant even if orders were sparse.
        let (id_order, other_order) = match (self.get(id), self.get(other)) {
            (Some(a), Some(b)) => (a.order, b.order),
            _ => return false,
        };
        if let Some(p) = self.get_mut(id) {
            p.order = other_order;
        }
        if let Some(p) = self.get_mut(other) {
            p.order = id_order;
        }
        self.renumber_siblings(parent);
        true
    }

    /// Re-densify `order` on the children of `parent`. Stable: keeps
    /// the existing relative order, just rewrites the values as 0, 1,
    /// 2, ... so the contract "siblings have contiguous order from 0"
    /// holds after a deletion or move.
    pub fn renumber_siblings(&mut self, parent: Option<PageId>) {
        // Snapshot (id, order) so we can sort without holding a
        // borrow on `self.data.pages`.
        let mut siblings: Vec<(PageId, u32)> = self
            .data
            .pages
            .iter()
            .filter(|p| p.parent == parent)
            .map(|p| (p.id, p.order))
            .collect();
        siblings.sort_by_key(|(_, o)| *o);
        let new_orders: HashMap<PageId, u32> = siblings
            .into_iter()
            .enumerate()
            .map(|(i, (id, _))| (id, i as u32))
            .collect();
        for p in &mut self.data.pages {
            if let Some(new) = new_orders.get(&p.id) {
                p.order = *new;
            }
        }
    }

    /// True when every page's `parent` points at a real page (or is
    /// None) and every sibling group has contiguous `order` starting
    /// from 0. Used by tests + the storage layer post-load to detect
    /// corruption.
    pub fn check_invariants(&self) -> Result<(), String> {
        let ids: std::collections::HashSet<PageId> =
            self.data.pages.iter().map(|p| p.id).collect();
        for p in &self.data.pages {
            if let Some(parent) = p.parent {
                if !ids.contains(&parent) {
                    return Err(format!(
                        "page {} has parent {} which does not exist",
                        p.id, parent,
                    ));
                }
            }
        }

        // Group by parent and check the order sequence.
        let mut groups: HashMap<Option<PageId>, Vec<u32>> = HashMap::new();
        for p in &self.data.pages {
            groups.entry(p.parent).or_default().push(p.order);
        }
        for (parent, mut orders) in groups {
            orders.sort();
            for (i, o) in orders.iter().enumerate() {
                if (i as u32) != *o {
                    return Err(format!(
                        "siblings of {:?} have non-contiguous order: {:?}",
                        parent, orders,
                    ));
                }
            }
        }

        if let Some(sel) = self.data.selected {
            if !ids.contains(&sel) {
                return Err(format!("selected {} does not exist", sel));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_page_appends_to_end_of_siblings() {
        let mut w = Workspace::new();
        let a = w.create_page(None);
        let b = w.create_page(None);
        let c = w.create_page(None);
        let roots = w.children_of(None);
        let ids: Vec<_> = roots.iter().map(|p| p.id).collect();
        assert_eq!(ids, vec![a, b, c]);
        assert_eq!(roots[0].order, 0);
        assert_eq!(roots[1].order, 1);
        assert_eq!(roots[2].order, 2);
    }

    #[test]
    fn rename_changes_title() {
        let mut w = Workspace::new();
        let id = w.create_page(None);
        assert!(w.rename(id, "Hello"));
        assert_eq!(w.get(id).unwrap().title, "Hello");
        // Unknown id is a no-op.
        assert!(!w.rename(Uuid::new_v4(), "nope"));
    }

    #[test]
    fn delete_page_removes_descendants_recursively() {
        let mut w = Workspace::new();
        let root = w.create_page(None);
        let mid = w.create_page(Some(root));
        let leaf = w.create_page(Some(mid));

        let removed = w.delete_page(root);
        assert_eq!(removed.len(), 3);
        assert!(removed.contains(&root));
        assert!(removed.contains(&mid));
        assert!(removed.contains(&leaf));
        assert!(w.pages().is_empty());
    }

    #[test]
    fn delete_renumbers_surviving_siblings() {
        let mut w = Workspace::new();
        let a = w.create_page(None); // order 0
        let b = w.create_page(None); // order 1
        let c = w.create_page(None); // order 2
        w.delete_page(b);
        assert_eq!(w.get(a).unwrap().order, 0);
        assert_eq!(w.get(c).unwrap().order, 1);
        w.check_invariants().unwrap();
    }

    #[test]
    fn check_invariants_detects_orphan() {
        let mut w = Workspace::new();
        let fake = Uuid::new_v4();
        w.data.pages.push(PageMeta {
            id: Uuid::new_v4(),
            title: "orphan".into(),
            parent: Some(fake),
            order: 0,
        });
        assert!(w.check_invariants().is_err());
    }

    #[test]
    fn move_sibling_swaps_order_and_stays_contiguous() {
        let mut w = Workspace::new();
        let a = w.create_page(None); // order 0
        let b = w.create_page(None); // order 1
        let c = w.create_page(None); // order 2

        // Move `c` up one slot → [a, c, b].
        assert!(w.move_sibling(c, -1));
        let ids: Vec<_> = w.children_of(None).iter().map(|p| p.id).collect();
        assert_eq!(ids, vec![a, c, b]);
        w.check_invariants().unwrap();

        // Move `a` down one slot → [c, a, b].
        assert!(w.move_sibling(a, 1));
        let ids: Vec<_> = w.children_of(None).iter().map(|p| p.id).collect();
        assert_eq!(ids, vec![c, a, b]);
        w.check_invariants().unwrap();
    }

    #[test]
    fn move_sibling_at_edge_is_noop() {
        let mut w = Workspace::new();
        let a = w.create_page(None);
        let b = w.create_page(None);
        // `a` is already first — moving up does nothing.
        assert!(!w.move_sibling(a, -1));
        // `b` is already last — moving down does nothing.
        assert!(!w.move_sibling(b, 1));
        let ids: Vec<_> = w.children_of(None).iter().map(|p| p.id).collect();
        assert_eq!(ids, vec![a, b]);
    }

    #[test]
    fn move_sibling_only_reorders_within_parent() {
        let mut w = Workspace::new();
        let root = w.create_page(None);
        let x = w.create_page(Some(root)); // child order 0
        let y = w.create_page(Some(root)); // child order 1
        let sib = w.create_page(None); // a root sibling of `root`

        // Moving the child down swaps x/y but never touches root-level.
        assert!(w.move_sibling(x, 1));
        let kids: Vec<_> = w.children_of(Some(root)).iter().map(|p| p.id).collect();
        assert_eq!(kids, vec![y, x]);
        let roots: Vec<_> = w.children_of(None).iter().map(|p| p.id).collect();
        assert_eq!(roots, vec![root, sib]);
        w.check_invariants().unwrap();
    }

    #[test]
    fn select_clears_when_page_deleted() {
        let mut w = Workspace::new();
        let id = w.create_page(None);
        w.set_selected(Some(id));
        w.delete_page(id);
        assert!(w.selected().is_none());
    }
}
