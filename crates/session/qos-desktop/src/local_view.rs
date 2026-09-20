use crate::identity::*;
use crate::*;
use std::sync::atomic::Ordering as AtomicOrdering;

impl WorkspaceTree {
    /// Set the process-wide peer salt [`next_id`] stamps into the high
    /// 16 bits of every freshly-allocated id. Default `0` keeps
    /// `WorkspaceId(1)`-style test literals working; two peers under
    /// distinct salts can never collide. Takes `&mut self` even though
    /// the salt is process-global, so a per-tree-salt refactor stays a
    /// one-line change.
    pub fn set_peer_salt(&mut self, salt: u16) {
        Self::set_process_peer_salt(salt);
    }

    /// [`Self::set_peer_salt`] without a tree to call it on. Must run
    /// **before** the tree is loaded: ids minted before the salt exists
    /// land in the shared 0-salt range, identical on every first boot,
    /// and the room document keys a window group by id — so two such
    /// peers fold two distinct windows into one. Re-seeding the counter
    /// afterward does not repair already-minted ids.
    pub fn set_process_peer_salt(salt: u16) {
        NEXT_ID_PEER_SALT.store(salt, AtomicOrdering::Relaxed);
    }

    /// Read the current process-wide peer salt. Mostly used by tests to
    /// assert on the salt without poking the static directly.
    pub fn peer_salt(&self) -> u16 {
        NEXT_ID_PEER_SALT.load(AtomicOrdering::Relaxed)
    }

    /// The salt replication treats as this tree's own — the id range
    /// this tree may write and sweep. [`Self::peer_salt_override`]
    /// first, the process-wide salt otherwise.
    pub fn local_salt(&self) -> u16 {
        self.peer_salt_override
            .unwrap_or_else(|| NEXT_ID_PEER_SALT.load(AtomicOrdering::Relaxed))
    }

    /// Give this tree a salt without touching the process-wide one, for
    /// holding two trees at once. The caller must mint matching ids
    /// itself; [`next_id`] reads the static.
    pub fn set_salt_override(&mut self, salt: u16) {
        self.peer_salt_override = Some(salt);
    }

    /// Walk every id and advance [`NEXT_ID`] past the highest counter
    /// stamped by the local peer salt — idempotent, called after
    /// deserializing. The counter restarts at `1` each process, so
    /// without this it collides with persisted ids. Only local-salt ids
    /// count; a remote peer's must not advance our counter.
    pub fn seed_next_id_from_self(&self) {
        let local_salt = NEXT_ID_PEER_SALT.load(AtomicOrdering::Relaxed);
        let mut max_counter: u64 = 0;
        let mut consider = |id: u64| {
            if peer_salt_of(id) == local_salt {
                max_counter = max_counter.max(counter_of(id));
            }
        };
        for ws in &self.workspaces {
            consider(ws.id.raw());
            for strip in &ws.strips {
                consider(strip.id.raw());
                for group in &strip.window_groups {
                    consider(group.id);
                    for tab in &group.tabs {
                        consider(tab.id);
                    }
                }
            }
        }
        // Advance the counter past `max_counter`. `fetch_max` keeps the
        // higher of the two so a parallel allocator that already
        // pushed past max_counter isn't reset backwards.
        NEXT_ID.fetch_max(max_counter.saturating_add(1), AtomicOrdering::Relaxed);
    }

    /// Every room this machine is in, deduplicated, in workspace order.
    /// A workspace whose room is still the unminted sentinel is skipped.
    pub fn rooms(&self) -> Vec<RoomId> {
        let mut seen = std::collections::HashSet::new();
        self.workspaces
            .iter()
            .map(|w| w.room)
            .filter(|r| !r.is_nil() && seen.insert(*r))
            .collect()
    }

    /// The durable room identity of `workspace_id`. What the launch path
    /// needs to turn a local handle into the value the wire speaks.
    pub fn room_of(&self, workspace_id: WorkspaceId) -> Option<RoomId> {
        self.workspaces
            .iter()
            .find(|w| w.id == workspace_id)
            .map(|w| w.room)
    }

    pub fn active_workspace(&self) -> Option<&Workspace> {
        self.workspaces.iter().find(|w| w.id == self.active)
    }

    pub fn active_workspace_mut(&mut self) -> Option<&mut Workspace> {
        self.workspaces.iter_mut().find(|w| w.id == self.active)
    }

    /// The currently-active [`GroupId`] for `strip_id`, or `None` if
    /// unseeded — callers compose this with their own fallback.
    pub fn active_group_for(&self, strip_id: StripId) -> Option<GroupId> {
        self.active_group.get(&strip_id).copied()
    }

    /// Mark `group_id` as the active group of `strip_id`. Not validated
    /// against the strip's actual membership — the shell's reconciler is
    /// expected to be the sole writer.
    pub fn set_active_group(&mut self, strip_id: StripId, group_id: GroupId) {
        self.active_group.insert(strip_id, group_id);
    }

    /// Resolve the active group on `strip_id` under `is_closing`: if
    /// closing, returns the next non-closing sibling right, falling
    /// back left. Pure read; `None` if no group survives, or the strip
    /// doesn't exist.
    pub fn resolve_active_group(
        &self,
        strip_id: StripId,
        is_closing: impl Fn(GroupId) -> bool,
    ) -> Option<GroupId> {
        let strip = self
            .workspaces
            .iter()
            .flat_map(|w| w.strips.iter())
            .find(|s| s.id == strip_id)?;
        let recorded = self.active_group.get(&strip_id).copied();
        // First non-closing group in the strip scanning right
        // (increasing index) from — and including — `from`.
        let scan_right = |from: usize| -> Option<GroupId> {
            strip
                .window_groups
                .get(from..)
                .unwrap_or(&[])
                .iter()
                .find(|g| !is_closing(g.id))
                .map(|g| g.id)
        };
        // Same, scanning left (decreasing index) from — and including —
        // `from`.
        let scan_left = |from: usize| -> Option<GroupId> {
            strip
                .window_groups
                .get(..=from)
                .unwrap_or(&[])
                .iter()
                .rev()
                .find(|g| !is_closing(g.id))
                .map(|g| g.id)
        };
        match recorded {
            Some(id) => {
                if !is_closing(id) {
                    return Some(id);
                }
                // Recorded group is closing — find its index, then
                // scan right for a survivor; if none, scan left.
                let Some(idx) = strip.window_groups.iter().position(|g| g.id == id) else {
                    // Recorded group no longer in the strip; fall
                    // through to "first non-closing" as a default.
                    return scan_right(0);
                };
                scan_right(idx.saturating_add(1)).or_else(|| idx.checked_sub(1).and_then(scan_left))
            }
            None => scan_right(0),
        }
    }

    /// Reconcile the active-group pointer against `closing_ids`: if the
    /// recorded group is closing, advance to the next non-closing
    /// sibling. `None` when every group is closing — the entry is
    /// removed, since an emptying strip has no active group rather than
    /// one clamped to a corpse. Idempotent.
    pub fn nudge_active_group_off_closing(
        &mut self,
        strip_id: StripId,
        closing_ids: &[GroupId],
    ) -> Option<GroupId> {
        let recorded = self.active_group.get(&strip_id).copied()?;
        if !closing_ids.contains(&recorded) {
            return Some(recorded);
        }
        let resolved = self.resolve_active_group(strip_id, |id| closing_ids.contains(&id));
        if let Some(next) = resolved {
            self.active_group.insert(strip_id, next);
            Some(next)
        } else {
            // Whole strip is fading; drop the entry rather than
            // leave a dangling pointer. The shell re-seeds when
            // fresh groups land.
            self.active_group.remove(&strip_id);
            None
        }
    }

    /// Tell the tree which records `room`'s index holds. Returns whether
    /// this was news, so the caller can skip re-asking when nothing moved.
    pub fn set_room_records(
        &mut self,
        room: RoomId,
        held: std::collections::BTreeMap<String, String>,
    ) -> bool {
        match self.room_records_held.get(&room) {
            Some(existing) if *existing == held => false,
            _ => {
                self.room_records_held.insert(room, held);
                true
            }
        }
    }

    /// Take the references peers have added since this was last called.
    /// Drained rather than read so a queue that only grows isn't a leak.
    pub fn take_inbound_room_records(&mut self) -> Vec<(RoomId, String, String)> {
        std::mem::take(&mut self.room_records_inbound)
    }

    pub fn set_active(&mut self, id: WorkspaceId) {
        if self.workspaces.iter().any(|w| w.id == id) {
            self.active = id;
        }
    }

    pub fn switch_up(&mut self) -> bool {
        let idx = self
            .workspaces
            .iter()
            .position(|w| w.id == self.active)
            .unwrap_or(0);
        let Some(prev_idx) = idx.checked_sub(1) else {
            return false;
        };
        let Some(prev) = self.workspaces.get(prev_idx) else {
            return false;
        };
        self.active = prev.id;
        true
    }

    pub fn switch_down(&mut self) -> bool {
        let idx = self
            .workspaces
            .iter()
            .position(|w| w.id == self.active)
            .unwrap_or(0);
        let Some(next) = self.workspaces.get(idx.saturating_add(1)) else {
            return false;
        };
        self.active = next.id;
        true
    }

    /// Reorder the window group `group_id` within `strip_id`; the whole
    /// group moves as a unit, unlike [`Self::move_tab_across_strips`].
    /// `new_index` is clamped to the post-removal index space, so a
    /// caller may pass a raw gap index. `true` only when the order
    /// actually changed. Mirrors into the doc afterwards, the same
    /// cache-then-doc shape as `move_tab_across_strips`.
    pub fn reorder_group_within_strip(
        &mut self,
        strip_id: StripId,
        group_id: u64,
        new_index: usize,
    ) -> bool {
        // Locate the strip (StripId is workspace-agnostic, so scan all).
        let mut located: Option<(usize, usize, usize)> = None; // (ws_idx, strip_idx, cur_idx)
        'scan: for (ws_idx, ws) in self.workspaces.iter().enumerate() {
            for (strip_idx, strip) in ws.strips.iter().enumerate() {
                if strip.id != strip_id {
                    continue;
                }
                if let Some(cur_idx) = strip.window_groups.iter().position(|g| g.id == group_id) {
                    located = Some((ws_idx, strip_idx, cur_idx));
                }
                break 'scan;
            }
        }
        let Some((ws_idx, strip_idx, cur_idx)) = located else {
            return false;
        };
        // `ws_idx`/`strip_idx` were captured in the scan just above and
        // nothing has mutated `self.workspaces` since, but `.get_mut()`
        // costs nothing and can't panic if that ever changes.
        let Some(groups) = self
            .workspaces
            .get_mut(ws_idx)
            .and_then(|w| w.strips.get_mut(strip_idx))
            .map(|s| &mut s.window_groups)
        else {
            return false;
        };
        // Clamp into the post-removal index space so a raw gap index
        // (which can equal `len`, "append") lands on the last slot.
        let dest = new_index.min(groups.len().saturating_sub(1));
        if dest == cur_idx || cur_idx >= groups.len() {
            return false;
        }
        let moved = groups.remove(cur_idx);
        groups.insert(dest, moved.clone());
        true
    }

    /// Move the tab at `from` to index `to` inside `group_id`'s tab
    /// list — the persisted half of an intra-group tab drag, mirroring
    /// what the widget tree already reordered. `to` is clamped to the
    /// last slot; `true` only when the order actually changed.
    pub fn reorder_tab_within_group(&mut self, group_id: u64, from: usize, to: usize) -> bool {
        // Locate the group in the cache (ids are workspace-agnostic).
        let mut located: Option<(usize, usize, usize)> = None;
        'scan: for (ws_idx, ws) in self.workspaces.iter().enumerate() {
            for (strip_idx, strip) in ws.strips.iter().enumerate() {
                if let Some(g_idx) = strip.window_groups.iter().position(|g| g.id == group_id) {
                    located = Some((ws_idx, strip_idx, g_idx));
                    break 'scan;
                }
            }
        }
        let Some((ws_idx, strip_idx, g_idx)) = located else {
            return false;
        };
        // `ws_idx`/`strip_idx`/`g_idx` were captured in the scan just
        // above and nothing has mutated `self.workspaces` since, but
        // `.get_mut()` costs nothing and can't panic if that ever changes.
        let Some(group) = self
            .workspaces
            .get_mut(ws_idx)
            .and_then(|w| w.strips.get_mut(strip_idx))
            .and_then(|s| s.window_groups.get_mut(g_idx))
        else {
            return false;
        };
        if from >= group.tabs.len() {
            return false;
        }
        let dest = to.min(group.tabs.len().saturating_sub(1));
        if dest == from {
            return false;
        }
        let moved = group.tabs.remove(from);
        group.tabs.insert(dest, moved);
        true
    }
}
