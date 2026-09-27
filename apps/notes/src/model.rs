//! Pure data + serde representation of the notes workspace.
//!
//! [`WorkspaceData`] is the local disk snapshot: serde defaults keep old
//! workspaces readable as tags evolve. Live collaboration deliberately does
//! **not** mirror this blob as one Automerge value; [`crate::doc::NotesDoc`]
//! publishes pages, tags and note↔tag edges as independent keyed CRDT data.
//! The model is therefore pure convenience logic for the local projection,
//! while its stable UUIDs remain the same identities used on the wire.

use std::collections::HashMap;

use crate::num;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Identifier for a single page. Stable across renames; backs the on-disk
/// file name (`{uuid}.md`) and every cross-reference in the workspace
/// tree.
pub type PageId = Uuid;
/// Stable identity for a reusable workspace tag. Like pages, ids are minted
/// once at authoring time and never derived from a display name.
pub type TagId = Uuid;

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

/// A named, reusable tag. Assignments live separately so a concurrent rename
/// only edits this one record rather than rewriting every tagged note.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TagMeta {
    pub id: TagId,
    pub name: String,
}

/// One keyed edge in the note↔tag graph. The pair is unique; keeping it as a
/// value rather than a nested map leaves the persisted JSON deterministic and
/// makes old workspaces forward-compatible through serde defaults.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TagAssignment {
    pub page: PageId,
    pub tag: TagId,
}

/// Top-level state for one notes workspace. The `pages` list is the
/// flat source of truth; the tree structure is derived from `parent`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct WorkspaceData {
    pub pages: Vec<PageMeta>,
    pub selected: Option<PageId>,
    #[serde(default)]
    pub tags: Vec<TagMeta>,
    #[serde(default)]
    pub tag_assignments: Vec<TagAssignment>,
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

    pub fn tags(&self) -> &[TagMeta] {
        &self.data.tags
    }

    pub fn get_tag(&self, id: TagId) -> Option<&TagMeta> {
        self.data.tags.iter().find(|tag| tag.id == id)
    }

    /// Mint a tag identity once. The caller dispatches the matching action,
    /// so replicas receive this exact id instead of generating their own.
    pub fn create_tag(&mut self, name: &str) -> TagId {
        let id = Uuid::new_v4();
        self.data.tags.push(TagMeta {
            id,
            name: name.trim().to_string(),
        });
        id
    }

    pub fn rename_tag(&mut self, id: TagId, name: &str) -> bool {
        let Some(tag) = self.data.tags.iter_mut().find(|tag| tag.id == id) else {
            return false;
        };
        tag.name = name.trim().to_string();
        true
    }

    /// Remove a tag and every keyed assignment to it. The corresponding
    /// document action writes Automerge map tombstones for both the tag and
    /// edges, so stale replicas cannot display dangling chips after sync.
    pub fn delete_tag(&mut self, id: TagId) -> bool {
        let before = self.data.tags.len();
        self.data.tags.retain(|tag| tag.id != id);
        self.data.tag_assignments.retain(|edge| edge.tag != id);
        before != self.data.tags.len()
    }

    /// Absolute membership update — repeated assign/unassign calls are
    /// idempotent, which is essential for retry/replay and collaboration.
    pub fn set_tag_assignment(&mut self, page: PageId, tag: TagId, assigned: bool) -> bool {
        if self.get(page).is_none() || self.get_tag(tag).is_none() {
            return false;
        }
        let existing = self
            .data
            .tag_assignments
            .iter()
            .position(|edge| edge.page == page && edge.tag == tag);
        match (assigned, existing) {
            (true, None) => {
                self.data.tag_assignments.push(TagAssignment { page, tag });
                true
            }
            (false, Some(index)) => {
                self.data.tag_assignments.remove(index);
                true
            }
            _ => false,
        }
    }

    pub fn tags_for_page(&self, page: PageId) -> Vec<&TagMeta> {
        let mut tags = self
            .data
            .tag_assignments
            .iter()
            .filter(|edge| edge.page == page)
            .filter_map(|edge| self.get_tag(edge.tag))
            .collect::<Vec<_>>();
        tags.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then(a.id.cmp(&b.id))
        });
        tags
    }

    pub fn pages_for_tag(&self, tag: TagId) -> Vec<PageId> {
        let mut pages = self
            .data
            .tag_assignments
            .iter()
            .filter(|edge| edge.tag == tag)
            .map(|edge| edge.page)
            .collect::<Vec<_>>();
        pages.sort();
        pages.dedup();
        pages
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
        self.data
            .tag_assignments
            .retain(|edge| !removed_set.contains(&edge.page));
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
        if target < 0 {
            return false;
        }
        let Some(&other) = siblings.get(num::usize_of_i32(target)) else {
            return false;
        };
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
    /// duplicate the `{uuid}.md` body files. `None` when `id` is unknown
    /// (or, defensively, if the tree changes out from under the walk).
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
        let &new_root = map.get(&id)?;
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
            let meta = self.get(*old)?;
            let &new_id = map.get(old)?;
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
    ///
    /// # Errors
    ///
    /// Returns a message naming the first broken invariant found (a
    /// dangling parent, or a non-contiguous sibling `order`).
    pub fn check_invariants(&self) -> Result<(), String> {
        let ids: std::collections::HashSet<PageId> = self.data.pages.iter().map(|p| p.id).collect();
        if ids.len() != self.data.pages.len() {
            return Err("duplicate page id".into());
        }
        if ids.iter().any(PageId::is_nil) || self.data.tags.iter().any(|tag| tag.id.is_nil()) {
            return Err("page and tag ids must be non-nil UUIDs".into());
        }
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

        let parents = self
            .data
            .pages
            .iter()
            .map(|page| (page.id, page.parent))
            .collect::<HashMap<_, _>>();
        let mut finished = std::collections::HashSet::new();
        for page in &self.data.pages {
            let mut chain = std::collections::HashSet::new();
            let mut current = Some(page.id);
            while let Some(id) = current {
                if finished.contains(&id) {
                    break;
                }
                if !chain.insert(id) {
                    return Err(format!("page {id} forms a parent cycle"));
                }
                current = parents.get(&id).copied().flatten();
            }
            finished.extend(chain);
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
        let tag_ids: std::collections::HashSet<TagId> =
            self.data.tags.iter().map(|tag| tag.id).collect();
        if tag_ids.len() != self.data.tags.len() {
            return Err("duplicate tag id".to_string());
        }
        if self.data.tags.iter().any(|tag| tag.name.trim().is_empty()) {
            return Err("tag name is empty".to_string());
        }
        let mut edges = std::collections::HashSet::new();
        for edge in &self.data.tag_assignments {
            if !ids.contains(&edge.page) || !tag_ids.contains(&edge.tag) {
                return Err(format!(
                    "tag assignment {}:{} has a missing endpoint",
                    edge.page, edge.tag
                ));
            }
            if !edges.insert((edge.page, edge.tag)) {
                return Err(format!(
                    "duplicate tag assignment {}:{}",
                    edge.page, edge.tag
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(w: &Workspace, parent: Option<Uuid>) -> Vec<Uuid> {
        w.children_of(parent).iter().map(|p| p.id).collect()
    }

    #[test]
    fn pages_create_rename_and_delete_with_contiguous_order_and_valid_invariants() {
        let mut w = Workspace::new();
        let a = w.create_page(None);
        let b = w.create_page(None);
        let c = w.create_page(None);
        assert_eq!(
            ids(&w, None),
            vec![a, b, c],
            "created pages append to the end"
        );
        let orders: Vec<_> = w.children_of(None).iter().map(|p| p.order).collect();
        assert_eq!(orders, vec![0, 1, 2]);

        assert!(w.rename(a, "Hello"));
        assert_eq!(w.get(a).unwrap().title, "Hello");
        // Unknown id is a no-op.
        assert!(!w.rename(Uuid::new_v4(), "nope"));

        // Deleting renumbers the surviving siblings.
        w.delete_page(b);
        assert_eq!(w.get(a).unwrap().order, 0);
        assert_eq!(w.get(c).unwrap().order, 1);
        w.check_invariants().unwrap();

        // Deleting removes descendants recursively.
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

        // The selection clears when its page is deleted.
        let mut w = Workspace::new();
        let id = w.create_page(None);
        w.set_selected(Some(id));
        w.delete_page(id);
        assert!(w.selected().is_none());

        // An orphan is an invariant violation.
        let mut w = Workspace::new();
        w.data.pages.push(PageMeta {
            id: Uuid::new_v4(),
            title: "orphan".into(),
            parent: Some(Uuid::new_v4()),
            order: 0,
        });
        assert!(w.check_invariants().is_err());
    }

    #[test]
    fn moves_reorder_within_their_group_reparent_and_reject_cycles() {
        let mut w = Workspace::new();
        let a = w.create_page(None);
        let b = w.create_page(None);
        let c = w.create_page(None);
        // (moved page, delta, expected root order) — each move stays contiguous.
        for (page, delta, want) in [(c, -1, vec![a, c, b]), (a, 1, vec![c, a, b])] {
            assert!(w.move_sibling(page, delta));
            assert_eq!(ids(&w, None), want);
            w.check_invariants().unwrap();
        }

        // Moving past either edge does nothing.
        let mut w = Workspace::new();
        let a = w.create_page(None);
        let b = w.create_page(None);
        assert!(!w.move_sibling(a, -1), "first cannot move up");
        assert!(!w.move_sibling(b, 1), "last cannot move down");
        assert_eq!(ids(&w, None), vec![a, b]);

        // Moving a child only reorders within its parent.
        let mut w = Workspace::new();
        let root = w.create_page(None);
        let first_child = w.create_page(Some(root));
        let second_child = w.create_page(Some(root));
        let sib = w.create_page(None);
        assert!(w.move_sibling(first_child, 1));
        assert_eq!(ids(&w, Some(root)), vec![second_child, first_child]);
        assert_eq!(ids(&w, None), vec![root, sib]);
        w.check_invariants().unwrap();

        // move_to reorders within a sibling group (before `a`, then appended).
        let mut w = Workspace::new();
        let a = w.create_page(None);
        let b = w.create_page(None);
        let c = w.create_page(None);
        for (before, want) in [(Some(a), vec![c, a, b]), (None, vec![a, b, c])] {
            assert!(w.move_to(c, None, before));
            assert_eq!(ids(&w, None), want, "move before {before:?}");
            w.check_invariants().unwrap();
        }

        // move_to re-parents a page as a child.
        let mut w = Workspace::new();
        let a = w.create_page(None);
        let b = w.create_page(None);
        assert!(w.move_to(b, Some(a), None));
        assert_eq!(w.get(b).unwrap().parent, Some(a));
        assert_eq!(w.children_of(None).len(), 1, "b left the root group");
        assert_eq!(ids(&w, Some(a)), vec![b]);
        w.check_invariants().unwrap();

        // Can't move a page under its own descendant, or under itself.
        let mut w = Workspace::new();
        let root = w.create_page(None);
        let child = w.create_page(Some(root));
        assert!(!w.move_to(root, Some(child), None));
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
    fn tags_are_keyed_idempotent_and_cleanup_with_their_endpoint() {
        let mut w = Workspace::new();
        let page = w.create_page(None);
        let tag = w.create_tag("Release");
        assert!(w.set_tag_assignment(page, tag, true));
        assert!(
            !w.set_tag_assignment(page, tag, true),
            "assign is absolute/idempotent"
        );
        assert_eq!(w.tags_for_page(page)[0].name, "Release");
        assert!(w.set_tag_assignment(page, tag, false));
        assert!(!w.set_tag_assignment(page, tag, false));
        assert!(w.set_tag_assignment(page, tag, true));
        assert!(w.delete_tag(tag));
        assert!(w.tags_for_page(page).is_empty());
        w.check_invariants().unwrap();
    }
}
