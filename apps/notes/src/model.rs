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
    fn select_clears_when_page_deleted() {
        let mut w = Workspace::new();
        let id = w.create_page(None);
        w.set_selected(Some(id));
        w.delete_page(id);
        assert!(w.selected().is_none());
    }
}
