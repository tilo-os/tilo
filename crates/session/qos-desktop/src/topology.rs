//! Pure desktop mutations. Hosts mirror successful changes through their
//! admitted session path and own persistence or rollback on failed commits.
use crate::identity::next_id;
use crate::*;
use std::collections::HashMap;

/// The exact changed group and source information needed by a host's existing
/// replication transaction. Contains no service or authority decision.
#[derive(Clone, Debug)]
pub struct TabMove {
    pub source_group: GroupId,
    pub pruned_source_group: bool,
    pub group: WindowGroup,
    pub destination: usize,
}

impl WorkspaceTree {
    pub fn bootstrap_default() -> Self {
        let ws_id = WorkspaceId::new();
        let strip_id = StripId::new();
        let home = Workspace {
            id: ws_id,
            room: RoomId::new(),
            name: "Home".into(),
            strips: vec![Strip {
                id: strip_id,
                name: "Main".into(),
                window_groups: Vec::new(),
            }],
            collaborators: Vec::new(),
            owner: None,
            pending_ownership_transfer: None,
            ownership_transfer_audit: Vec::new(),
            ownership_transfer_cancellations: Vec::new(),
            capabilities: Vec::new(),
            permission_profiles: Vec::new(),
            default_apps: desktop_policy::WorkspaceDefaults::default(),
            tasks: Vec::new(),
            effect_receipts: Vec::new(),
            shared_surfaces: SharedSurfaces::default(),
        };

        let mut active_strip = HashMap::new();
        active_strip.insert(ws_id, strip_id);
        let tree = Self {
            workspaces: vec![home],
            active: ws_id,
            active_strip,
            active_group: HashMap::new(),
            collaborators: vec![],
            ..Default::default()
        };
        tree
    }

    pub fn append_empty_workspace(&mut self, name: &str) -> WorkspaceId {
        let ws_id = WorkspaceId::new();
        let strip_id = StripId::new();
        // A workspace the user just created is a room that now exists;
        // it gets its identity at the same moment it gets its id, not
        // the first time someone tries to share it.
        let room = RoomId::new();
        let ws = Workspace {
            id: ws_id,
            room,
            name: name.into(),
            strips: vec![Strip {
                id: strip_id,
                name: "Main".into(),
                window_groups: vec![],
            }],
            collaborators: vec![],
            owner: None,
            pending_ownership_transfer: None,
            ownership_transfer_audit: Vec::new(),
            ownership_transfer_cancellations: Vec::new(),
            capabilities: Vec::new(),
            permission_profiles: Vec::new(),
            default_apps: desktop_policy::WorkspaceDefaults::default(),
            tasks: Vec::new(),
            effect_receipts: Vec::new(),
            shared_surfaces: SharedSurfaces::default(),
        };
        self.active_strip.insert(ws_id, strip_id);
        self.workspaces.push(ws);

        ws_id
    }

    pub fn move_tab_across_strips(
        &mut self,
        src_strip_id: StripId,
        dest_strip_id: StripId,
        tab_id: TabId,
        new_group_id: GroupId,
        destination: usize,
    ) -> Option<TabMove> {
        if src_strip_id == dest_strip_id {
            // Return-to-source re-splices into a new group on the same
            // strip; the tab is still attributed to this strip, so
            // skipping the write is safe — a restart rebuilds it anyway.
            return None;
        }
        // Validate the destination before detaching anything. A stale drop
        // target must not make the source tab disappear.
        if !self
            .workspaces
            .iter()
            .flat_map(|workspace| workspace.strips.iter())
            .any(|strip| strip.id == dest_strip_id)
        {
            return None;
        }
        // Scan every workspace's strips: StripId is workspace-agnostic.
        let mut moved_tab: Option<Tab> = None;
        let mut prune_group_at: Option<(usize, usize, usize)> = None; // (ws_idx, strip_idx, group_idx)
        let mut src_group_id = 0;
        'outer: for (ws_idx, ws) in self.workspaces.iter_mut().enumerate() {
            for (strip_idx, strip) in ws.strips.iter_mut().enumerate() {
                if strip.id != src_strip_id {
                    continue;
                }
                for (group_idx, group) in strip.window_groups.iter_mut().enumerate() {
                    if let Some(pos) = group.tabs.iter().position(|t| t.id == tab_id) {
                        src_group_id = group.id;
                        let removed = group.tabs.remove(pos);
                        // If the moved tab was active, promote the
                        // first remaining sibling so a future re-render
                        // doesn't dereference a stale active id.
                        if group.active_tab == tab_id
                            && let Some(first) = group.tabs.first()
                        {
                            group.active_tab = first.id;
                        }
                        if group.tabs.is_empty() {
                            prune_group_at = Some((ws_idx, strip_idx, group_idx));
                        }
                        moved_tab = Some(removed);
                        break 'outer;
                    }
                }
            }
        }
        let moved = moved_tab?;
        if let Some((ws_idx, strip_idx, group_idx)) = prune_group_at {
            // `ws_idx`/`strip_idx` were captured in the scan just above and
            // nothing has mutated `self.workspaces` since, but a `.get_mut()`
            // chain costs nothing and can't panic if that ever changes.
            if let Some(groups) = self
                .workspaces
                .get_mut(ws_idx)
                .and_then(|w| w.strips.get_mut(strip_idx))
                .map(|s| &mut s.window_groups)
                && group_idx < groups.len()
            {
                groups.remove(group_idx);
            }
        }
        let active_tab = moved.id;
        let new_group = WindowGroup {
            id: new_group_id,
            tabs: vec![moved],
            active_tab,
            width: WindowWidth::Normal,
            maximized: false,
        };
        for ws in &mut self.workspaces {
            for strip in &mut ws.strips {
                if strip.id == dest_strip_id {
                    let destination = destination.min(strip.window_groups.len());
                    strip.window_groups.insert(destination, new_group.clone());
                    return Some(TabMove {
                        source_group: src_group_id,
                        pruned_source_group: prune_group_at.is_some(),
                        group: new_group,
                        destination,
                    });
                }
            }
        }
        None
    }

    pub fn split_tab_into_new_group(
        &mut self,
        strip_id: StripId,
        tab_id: TabId,
        insert_at: usize,
    ) -> Option<TabMove> {
        self.split_tab_into_new_group_using(strip_id, tab_id, insert_at, next_id)
    }

    /// Durable hosts reserve the identity in their owned namespace before
    /// publishing; native callers retain the process allocator above.
    pub fn split_tab_into_reserved_group(
        &mut self,
        strip_id: StripId,
        tab_id: TabId,
        insert_at: usize,
        new_group_id: GroupId,
    ) -> Option<TabMove> {
        self.split_tab_into_new_group_using(strip_id, tab_id, insert_at, || new_group_id)
    }

    fn split_tab_into_new_group_using(
        &mut self,
        strip_id: StripId,
        tab_id: TabId,
        insert_at: usize,
        new_group_id: impl FnOnce() -> GroupId,
    ) -> Option<TabMove> {
        let (ws_idx, strip_idx, group_idx, tab_idx) =
            self.workspaces
                .iter()
                .enumerate()
                .find_map(|(ws_idx, ws)| {
                    let (strip_idx, strip) = ws
                        .strips
                        .iter()
                        .enumerate()
                        .find(|(_, strip)| strip.id == strip_id)?;
                    strip
                        .window_groups
                        .iter()
                        .enumerate()
                        .find_map(|(group_idx, group)| {
                            group
                                .tabs
                                .iter()
                                .position(|tab| tab.id == tab_id)
                                .map(|tab_idx| (ws_idx, strip_idx, group_idx, tab_idx))
                        })
                })?;
        let (src_group_id, moved) = {
            let group = self
                .workspaces
                .get_mut(ws_idx)?
                .strips
                .get_mut(strip_idx)?
                .window_groups
                .get_mut(group_idx)?;
            if group.tabs.len() < 2 {
                return None;
            }
            let moved = group.tabs.remove(tab_idx);
            if group.active_tab == tab_id {
                group.active_tab = group
                    .tabs
                    .get(tab_idx)
                    .or_else(|| group.tabs.last())
                    .map_or(0, |tab| tab.id);
            }
            (group.id, moved)
        };
        let new_group = WindowGroup {
            id: new_group_id(),
            active_tab: moved.id,
            tabs: vec![moved],
            width: WindowWidth::Normal,
            maximized: false,
        };
        let groups = &mut self
            .workspaces
            .get_mut(ws_idx)?
            .strips
            .get_mut(strip_idx)?
            .window_groups;
        let destination = insert_at.min(groups.len());
        groups.insert(destination, new_group.clone());
        Some(TabMove {
            source_group: src_group_id,
            pruned_source_group: false,
            group: new_group,
            destination,
        })
    }
}
