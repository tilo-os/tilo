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
use ui::tilo_resource::order;
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
    /// Fractional position among siblings ([`ui::tilo_resource::order`]).
    /// Siblings sort by [`sibling_order`]: by this key, then by id, so two
    /// pages that got the same key on two devices still have one order and
    /// a gap left by a removed sibling never matters.
    pub order: String,
}

/// The one order of a sibling group: fractional key first, page id as the
/// tiebreak for keys two replicas minted concurrently.
pub fn sibling_order(a: &PageMeta, b: &PageMeta) -> std::cmp::Ordering {
    a.order.cmp(&b.order).then_with(|| a.id.cmp(&b.id))
}

/// The key after every one of `siblings`, for a page appended to them.
pub fn key_after<'a>(siblings: impl IntoIterator<Item = &'a PageMeta>) -> String {
    let last = siblings.into_iter().map(|page| page.order.as_str()).max();
    order::between(last, None)
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

/// The body-only projection adopted between two committed revisions.
/// This receipt is ephemeral and lets presentation caches reject missed updates.
#[derive(Clone, Debug)]
pub struct BodyProjectionRefresh {
    pub before: Vec<automerge::ChangeHash>,
    pub after: Vec<automerge::ChangeHash>,
    pub pages: Vec<PageId>,
}

/// Convenience wrapper around [`WorkspaceData`] with the runtime ops
/// the UI needs. Pure logic — no IO, no rendering.
#[derive(Clone, Debug, Default)]
pub struct Workspace {
    pub data: WorkspaceData,
    /// Committed document revision whose canonical schema this projection
    /// validated. Ephemeral: neither notebook storage nor collaboration state.
    pub canonical_projection_heads: Option<Vec<automerge::ChangeHash>>,
    /// Body-only change receipt; never serialized or used without matching heads.
    pub body_projection_refresh: Option<BodyProjectionRefresh>,
    /// What every note says, when the workspace was read from a notebook.
    pub bodies: std::sync::Arc<crate::actions::Bodies>,
    /// Each note's body as the rich text it is, published as the note's
    /// body resource.
    pub body_states: std::rc::Rc<std::collections::HashMap<PageId, tilo_document::RichTextState>>,
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
    /// in [`sibling_order`]. Walks the flat list once.
    pub fn children_of(&self, parent: Option<PageId>) -> Vec<&PageMeta> {
        let mut out: Vec<&PageMeta> = self
            .data
            .pages
            .iter()
            .filter(|p| p.parent == parent)
            .collect();
        out.sort_by(|a, b| sibling_order(a, b));
        out
    }

    /// Create a new page as a child of `parent`. Returns the new id.
    /// The page is appended at the end of `parent`'s children. The
    /// title defaults to `"Untitled"` — callers wanting a different
    /// initial value should rename right after creation.
    pub fn create_page(&mut self, parent: Option<PageId>) -> PageId {
        let id = Uuid::new_v4();
        let order = key_after(self.data.pages.iter().filter(|p| p.parent == parent));
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
    /// delete the corresponding `.md` files. The surviving siblings keep
    /// their keys: a gap in a fractional order changes nothing.
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

        self.data.pages.retain(|p| !removed_set.contains(&p.id));
        self.data
            .tag_assignments
            .retain(|edge| !removed_set.contains(&edge.page));
        if self.data.selected.is_some_and(|s| removed_set.contains(&s)) {
            self.data.selected = None;
        }

        to_remove
    }

    /// Move `id` up (`delta < 0`) or down (`delta > 0`) past the adjacent
    /// sibling in that direction. Returns `true` if the page moved
    /// (i.e. it existed and wasn't already at the requested edge).
    ///
    /// Only reorders within a single sibling group — re-parenting is a
    /// separate concern. Backs the sidebar's Alt-↑ / Alt-↓ reorder.
    pub fn move_sibling(&mut self, id: PageId, delta: i32) -> bool {
        let Some(plan) = self.plan_move_sibling(id, delta) else {
            return false;
        };
        self.apply_page_move_plan(plan);
        true
    }

    pub(crate) fn plan_move_sibling(&self, id: PageId, delta: i32) -> Option<Vec<PageMeta>> {
        if delta == 0 {
            return None;
        }
        let parent = self.get(id)?.parent;
        // Siblings sorted by order; find where `id` sits.
        let siblings: Vec<PageId> = self.children_of(parent).iter().map(|p| p.id).collect();
        let pos = num::i32_of_usize(siblings.iter().position(|s| *s == id)?);
        let target = pos + delta.signum();
        if target < 0 {
            return None;
        }
        let target = num::usize_of_i32(target);
        let &other = siblings.get(target)?;
        // Up: in front of the sibling above. Down: in front of the one after
        // the sibling below, or last.
        let before = if delta < 0 {
            Some(other)
        } else {
            siblings.get(target.saturating_add(1)).copied()
        };
        self.plan_move_to(id, parent, before)
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
    /// `id`). Only the moved page gets a new key, between its new
    /// neighbours; the group is re-keyed only when those neighbours share
    /// a key (two replicas minted it concurrently) and nothing fits between.
    pub fn move_to(
        &mut self,
        id: PageId,
        new_parent: Option<PageId>,
        before: Option<PageId>,
    ) -> bool {
        let Some(plan) = self.plan_move_to(id, new_parent, before) else {
            return false;
        };
        self.apply_page_move_plan(plan);
        true
    }

    pub(crate) fn apply_page_move_plan(&mut self, plan: Vec<PageMeta>) {
        for after in plan {
            if let Some(page) = self.get_mut(after.id) {
                *page = after;
            }
        }
    }

    pub(crate) fn plan_move_to(
        &self,
        id: PageId,
        new_parent: Option<PageId>,
        before: Option<PageId>,
    ) -> Option<Vec<PageMeta>> {
        self.get(id)?;
        // Cycle guard: a page can't become a child of itself or of one
        // of its own descendants.
        if let Some(np) = new_parent
            && (np == id || self.is_ancestor(id, np))
        {
            return None;
        }
        // The new group in display order, without `id`, so it can be slotted
        // back in at the requested position.
        let mut siblings: Vec<(PageId, &str)> = self
            .children_of(new_parent)
            .iter()
            .filter(|p| p.id != id)
            .map(|p| (p.id, p.order.as_str()))
            .collect();
        let insert_at = match before {
            Some(b) if b != id => siblings
                .iter()
                .position(|(s, _)| *s == b)
                .unwrap_or(siblings.len()),
            _ => siblings.len(),
        };
        let low = insert_at
            .checked_sub(1)
            .and_then(|i| siblings.get(i))
            .map(|(_, key)| *key);
        let high = siblings.get(insert_at).map(|(_, key)| *key);
        let keys: Vec<(PageId, String)> = match (low, high) {
            (Some(low), Some(high)) if low >= high => {
                siblings.insert(insert_at, (id, ""));
                let fresh = order::sequence(None, None, siblings.len());
                siblings
                    .into_iter()
                    .zip(fresh)
                    .map(|((sid, _), key)| (sid, key))
                    .collect()
            }
            _ => vec![(id, order::between(low, high))],
        };
        Some(
            keys.into_iter()
                .filter_map(|(sid, key)| {
                    let mut page = self.get(sid)?.clone();
                    if sid == id {
                        page.parent = new_parent;
                    }
                    page.order = key;
                    Some(page)
                })
                .collect(),
        )
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
                order: meta.order.clone(),
            });
        }
        self.data.pages.extend(new_metas);
        // Position the copy immediately after the source among its
        // siblings.
        self.move_to(new_root, src.parent, after_sibling);
        Some((new_root, mapping))
    }

    /// True when every page's `parent` points at a real page (or is
    /// None), parents form no cycle and every page's `order` is a
    /// fractional key. Gaps and shared keys among siblings are fine:
    /// [`sibling_order`] orders them. Used by tests + the storage layer
    /// post-load to detect corruption.
    ///
    /// # Errors
    ///
    /// Returns a message naming the first broken invariant found (a
    /// dangling parent, a parent cycle, or a malformed `order` key).
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

        if let Some(page) = self.data.pages.iter().find(|p| !order::is_key(&p.order)) {
            return Err(format!(
                "page {} has a malformed order key {:?}",
                page.id, page.order
            ));
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
    fn pages_create_rename_and_delete_in_key_order_with_valid_invariants() {
        let mut w = Workspace::new();
        let a = w.create_page(None);
        let b = w.create_page(None);
        let c = w.create_page(None);
        assert_eq!(
            ids(&w, None),
            vec![a, b, c],
            "created pages append to the end"
        );
        let orders: Vec<_> = w
            .children_of(None)
            .iter()
            .map(|p| p.order.clone())
            .collect();
        assert!(
            orders.windows(2).all(|pair| pair[0] < pair[1]),
            "{orders:?}"
        );

        assert!(w.rename(a, "Hello"));
        assert_eq!(w.get(a).unwrap().title, "Hello");
        // Unknown id is a no-op.
        assert!(!w.rename(Uuid::new_v4(), "nope"));

        // Deleting leaves the survivors' keys alone; the gap is harmless.
        w.delete_page(b);
        assert_eq!(w.get(a).unwrap().order, orders[0]);
        assert_eq!(w.get(c).unwrap().order, orders[2]);
        assert_eq!(ids(&w, None), vec![a, c]);
        w.check_invariants().unwrap();

        // An orphan is an invariant violation.
        let mut w = Workspace::new();
        w.data.pages.push(PageMeta {
            id: Uuid::new_v4(),
            title: "orphan".into(),
            parent: Some(Uuid::new_v4()),
            order: "i".into(),
        });
        assert!(w.check_invariants().is_err());

        // A malformed key is an invariant violation.
        let mut w = Workspace::new();
        let id = w.create_page(None);
        w.get_mut(id).unwrap().order = "I0".into();
        assert!(w.check_invariants().is_err());
    }

    #[test]
    fn siblings_sharing_a_key_order_by_id_and_still_move_between_each_other() {
        // Two replicas appended at once: the same key, ordered by id.
        let mut w = Workspace::new();
        let [low, high] = {
            let mut ids = [Uuid::new_v4(), Uuid::new_v4()];
            ids.sort();
            ids
        };
        for id in [high, low] {
            w.data.pages.push(PageMeta {
                id,
                title: String::new(),
                parent: None,
                order: "i".into(),
            });
        }
        w.check_invariants().unwrap();
        assert_eq!(ids(&w, None), vec![low, high]);
        // A page dropped between them re-keys the group so it lands there.
        let c = w.create_page(None);
        assert!(w.move_to(c, None, Some(high)));
        assert_eq!(ids(&w, None), vec![low, c, high]);
        w.check_invariants().unwrap();
        // Moving one of two tied pages past the other works too.
        let mut w = Workspace::new();
        for id in [high, low] {
            w.data.pages.push(PageMeta {
                id,
                title: String::new(),
                parent: None,
                order: "i".into(),
            });
        }
        assert!(w.move_sibling(low, 1));
        assert_eq!(ids(&w, None), vec![high, low]);
    }

    #[test]
    fn a_move_rekeys_only_the_moved_page() {
        let mut w = Workspace::new();
        let a = w.create_page(None);
        let b = w.create_page(None);
        let c = w.create_page(None);
        let before = w.pages().to_vec();
        assert!(w.move_to(c, None, Some(b)));
        assert_eq!(ids(&w, None), vec![a, c, b]);
        let changed: Vec<_> = w
            .pages()
            .iter()
            .filter(|page| !before.contains(page))
            .map(|page| page.id)
            .collect();
        assert_eq!(changed, vec![c]);
    }

    #[test]
    fn moves_reorder_within_their_group_reparent_and_reject_cycles() {
        let mut w = Workspace::new();
        let a = w.create_page(None);
        let b = w.create_page(None);
        let c = w.create_page(None);
        // (moved page, delta, expected root order).
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
