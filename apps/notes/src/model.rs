//! Pure data + serde representation of the notes workspace.
//!
//! The on-wire format used inside the automerge `Text` blob is the JSON
//! serialisation of [`WorkspaceData`]. Phase 1 doesn't yet replicate the
//! tree as individual automerge fields — concurrent editing isn't a
//! real concern this phase. When it becomes one (Phase 5) we can flip
//! the storage to per-field automerge with a compatible-enough JSON
//! migration path.

use std::collections::HashMap;

use crate::num;
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
        let order = self.children_of(parent).last().map_or(0, |p| p.order + 1);
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
        let removed_set: std::collections::HashSet<PageId> = to_remove.iter().copied().collect();

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
        if self.data.selected.is_some_and(|s| removed_set.contains(&s)) {
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
        let siblings: Vec<PageId> = self.children_of(parent).iter().map(|p| p.id).collect();
        let pos = match siblings.iter().position(|s| *s == id) {
            Some(p) => num::i32_of_usize(p),
            None => return false,
        };
        let target = pos + delta.signum();
        if target < 0 || num::usize_of_i32(target) >= siblings.len() {
            return false;
        }
        let other = siblings[num::usize_of_i32(target)];
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

    /// True when `maybe_ancestor` is `id` itself or one of its
    /// ancestors. Used to reject re-parenting a page under its own
    /// descendant (which would create a cycle in the tree).
    pub fn is_ancestor(&self, maybe_ancestor: PageId, id: PageId) -> bool {
        let mut cur = Some(id);
        let mut hops = 0;
        while let Some(c) = cur {
            if c == maybe_ancestor {
                return true;
            }
            if hops > 4096 {
                break; // corrupt cycle guard
            }
            cur = self.get(c).and_then(|p| p.parent);
            hops += 1;
        }
        false
    }

    /// Move `id` under `new_parent` (None = root), positioned
    /// immediately before sibling `before` among the new parent's
    /// children — or appended when `before` is `None`. Backs the
    /// sidebar's drag-to-reorder + re-parent drop.
    ///
    /// Returns `false` (a no-op) when `id` doesn't exist or the move
    /// would create a cycle (`new_parent` is `id` or a descendant of
    /// `id`). Both the old and new sibling groups are re-densified so
    /// `order` stays contiguous.
    pub fn move_to(
        &mut self,
        id: PageId,
        new_parent: Option<PageId>,
        before: Option<PageId>,
    ) -> bool {
        if self.get(id).is_none() {
            return false;
        }
        // Cycle guard: a page can't become a child of itself or of one
        // of its own descendants.
        if let Some(np) = new_parent
            && (np == id || self.is_ancestor(id, np))
        {
            return false;
        }
        let old_parent = self.get(id).and_then(|p| p.parent);
        // Target sibling order (by id), with `id` removed so we can slot
        // it back in at the requested position.
        let mut siblings: Vec<PageId> = self
            .children_of(new_parent)
            .iter()
            .map(|p| p.id)
            .filter(|s| *s != id)
            .collect();
        let insert_at = match before {
            Some(b) if b != id => siblings
                .iter()
                .position(|s| *s == b)
                .unwrap_or(siblings.len()),
            _ => siblings.len(),
        };
        siblings.insert(insert_at, id);
        // Commit the new parent, then rewrite the whole group's order.
        if let Some(p) = self.get_mut(id) {
            p.parent = new_parent;
        }
        for (i, sid) in siblings.iter().enumerate() {
            if let Some(p) = self.get_mut(*sid) {
                p.order = num::u32_of_usize(i);
            }
        }
        // If the page left a different group, re-densify the one it left.
        if old_parent != new_parent {
            self.renumber_siblings(old_parent);
        }
        true
    }

    /// Deep-copy `id` and its entire subtree, inserting the copy as the
    /// next sibling right after `id`. Every copied page gets a fresh
    /// `PageId`; the subtree's internal parent links are rewired to the
    /// copies so the duplicate is fully independent.
    ///
    /// Returns `(new_root, mapping)` where `mapping` pairs each source
    /// page id with its fresh copy id — the storage layer walks it to
    /// duplicate the `{uuid}.md` body files. `None` when `id` is unknown.
    pub fn duplicate_page(&mut self, id: PageId) -> Option<(PageId, Vec<(PageId, PageId)>)> {
        let src = self.get(id)?.clone();
        // Visit the subtree parents-before-children so a child's rewired
        // parent id already exists in the map.
        let mut order_list: Vec<PageId> = Vec::new();
        let mut stack = vec![id];
        while let Some(cur) = stack.pop() {
            order_list.push(cur);
            for child in self.children_of(Some(cur)) {
                stack.push(child.id);
            }
        }
        let mut map: HashMap<PageId, PageId> = HashMap::new();
        let mut mapping: Vec<(PageId, PageId)> = Vec::with_capacity(order_list.len());
        for old in &order_list {
            let new = Uuid::new_v4();
            map.insert(*old, new);
            mapping.push((*old, new));
        }
        let new_root = map[&id];
        // The sibling `id` currently sits before in its group — insert the
        // copy right after it (before the sibling that follows `id`).
        let after_sibling = {
            let group: Vec<PageId> = self.children_of(src.parent).iter().map(|p| p.id).collect();
            group
                .iter()
                .position(|s| *s == id)
                .and_then(|pos| group.get(pos + 1).copied())
        };
        // Materialise every cloned meta, then append them in one pass.
        let mut new_metas: Vec<PageMeta> = Vec::with_capacity(order_list.len());
        for old in &order_list {
            let meta = self.get(*old).expect("subtree page exists");
            let new_id = map[old];
            let new_parent = if *old == id {
                src.parent
            } else {
                meta.parent.map(|p| map.get(&p).copied().unwrap_or(p))
            };
            let title = if *old == id {
                format!("{} copy", meta.title)
            } else {
                meta.title.clone()
            };
            new_metas.push(PageMeta {
                id: new_id,
                title,
                parent: new_parent,
                order: meta.order,
            });
        }
        self.data.pages.extend(new_metas);
        // Position the copy immediately after the source among its
        // siblings (move_to also re-densifies the group's order).
        self.move_to(new_root, src.parent, after_sibling);
        Some((new_root, mapping))
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
            .map(|(i, (id, _))| (id, num::u32_of_usize(i)))
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
        let ids: std::collections::HashSet<PageId> = self.data.pages.iter().map(|p| p.id).collect();
        for p in &self.data.pages {
            if let Some(parent) = p.parent
                && !ids.contains(&parent)
            {
                return Err(format!(
                    "page {} has parent {} which does not exist",
                    p.id, parent,
                ));
            }
        }

        // Group by parent and check the order sequence.
        let mut groups: HashMap<Option<PageId>, Vec<u32>> = HashMap::new();
        for p in &self.data.pages {
            groups.entry(p.parent).or_default().push(p.order);
        }
        for (parent, mut orders) in groups {
            orders.sort_unstable();
            for (i, o) in orders.iter().enumerate() {
                if (i as u32) != *o {
                    return Err(format!(
                        "siblings of {parent:?} have non-contiguous order: {orders:?}",
                    ));
                }
            }
        }

        if let Some(sel) = self.data.selected
            && !ids.contains(&sel)
        {
            return Err(format!("selected {sel} does not exist"));
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
    fn move_to_reorders_within_a_sibling_group() {
        let mut w = Workspace::new();
        let a = w.create_page(None);
        let b = w.create_page(None);
        let c = w.create_page(None);
        // Move `c` to before `a` → [c, a, b].
        assert!(w.move_to(c, None, Some(a)));
        let ids: Vec<_> = w.children_of(None).iter().map(|p| p.id).collect();
        assert_eq!(ids, vec![c, a, b]);
        w.check_invariants().unwrap();
        // Append `c` to the end (before = None) → [a, b, c].
        assert!(w.move_to(c, None, None));
        let ids: Vec<_> = w.children_of(None).iter().map(|p| p.id).collect();
        assert_eq!(ids, vec![a, b, c]);
        w.check_invariants().unwrap();
    }

    #[test]
    fn move_to_reparents_page_as_child() {
        let mut w = Workspace::new();
        let a = w.create_page(None);
        let b = w.create_page(None);
        // Re-parent `b` under `a`.
        assert!(w.move_to(b, Some(a), None));
        assert_eq!(w.get(b).unwrap().parent, Some(a));
        assert_eq!(w.children_of(None).len(), 1, "b left the root group");
        let kids: Vec<_> = w.children_of(Some(a)).iter().map(|p| p.id).collect();
        assert_eq!(kids, vec![b]);
        w.check_invariants().unwrap();
    }

    #[test]
    fn move_to_rejects_cycles() {
        let mut w = Workspace::new();
        let root = w.create_page(None);
        let child = w.create_page(Some(root));
        // Can't move `root` under its own descendant.
        assert!(!w.move_to(root, Some(child), None));
        // Can't move a page under itself.
        assert!(!w.move_to(root, Some(root), None));
        assert_eq!(w.get(root).unwrap().parent, None);
        w.check_invariants().unwrap();
    }

    #[test]
    fn duplicate_page_deep_copies_subtree_after_source() {
        let mut w = Workspace::new();
        let a = w.create_page(None);
        w.rename(a, "Projects");
        let child = w.create_page(Some(a));
        w.rename(child, "Launch");
        let grand = w.create_page(Some(child));
        w.rename(grand, "Tasks");
        let sib = w.create_page(None);
        w.rename(sib, "Inbox");

        let (new_root, mapping) = w.duplicate_page(a).expect("duplicate");
        // Fresh ids for every node in the subtree (root + child + grand).
        assert_eq!(mapping.len(), 3);
        assert!(mapping.iter().all(|(old, new)| old != new));
        // The copy sits right after the source among root siblings.
        let roots: Vec<_> = w.children_of(None).iter().map(|p| p.id).collect();
        assert_eq!(roots, vec![a, new_root, sib], "copy lands after the source");
        // Copy carries a " copy" title and an independent subtree.
        assert_eq!(w.get(new_root).unwrap().title, "Projects copy");
        let copy_children = w.children_of(Some(new_root));
        assert_eq!(copy_children.len(), 1);
        assert_eq!(copy_children[0].title, "Launch");
        let copy_grand = w.children_of(Some(copy_children[0].id));
        assert_eq!(copy_grand.len(), 1);
        assert_eq!(copy_grand[0].title, "Tasks");
        // Original subtree is untouched (still 3 originals + 3 copies + sib).
        assert_eq!(w.children_of(Some(a)).len(), 1);
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
