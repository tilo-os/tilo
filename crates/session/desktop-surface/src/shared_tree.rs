//! Snapshot compatibility for hosts whose room carries a desktop tree.
//! Stream bindings and presentation belong to the receiving device.
use tilo_desktop_model::{
    DesktopTree, OutputPartition, TabId, TileContent, WindowWidth, WorkspaceId,
};

pub(crate) fn projection(mut tree: DesktopTree) -> DesktopTree {
    let local: Vec<_> = tree
        .workspaces
        .iter()
        .flat_map(|w| &w.strips)
        .flat_map(|s| &s.window_groups)
        .flat_map(|g| &g.tabs)
        .filter(|tab| matches!(tab.tile, TileContent::PeerWindow { .. }))
        .map(|tab| tab.id)
        .collect();
    for tab in local {
        tree.remove_tab(tab);
    }
    tree.active = tree
        .workspaces
        .first()
        .map_or_else(WorkspaceId::default, |w| w.id);
    tree.active_strip.clear();
    tree.active_group.clear();
    tree.outputs = OutputPartition::default();
    for group in tree
        .workspaces
        .iter_mut()
        .flat_map(|w| &mut w.strips)
        .flat_map(|s| &mut s.window_groups)
    {
        group.active_tab = group.tabs.first().map_or(0, |t| t.id);
        group.width = WindowWidth::Normal;
        group.maximized = false;
    }
    tree
}

pub(crate) fn receive(
    local: &DesktopTree,
    incoming: DesktopTree,
    local_tabs: &[TabId],
) -> DesktopTree {
    // Old senders also send their stream proxies. Those process-local
    // bindings have no meaning here, even when their ids happen to match.
    let mut incoming = projection(incoming);
    for workspace in &local.workspaces {
        let Some(target) = incoming
            .workspaces
            .iter_mut()
            .find(|w| w.id == workspace.id)
        else {
            continue; // A removed workspace must not retain access to its streams.
        };
        for strip in &workspace.strips {
            let Some(target) = target.strips.iter_mut().find(|s| s.id == strip.id) else {
                continue;
            };
            for (index, group) in strip.window_groups.iter().enumerate() {
                let tabs: Vec<_> = group
                    .tabs
                    .iter()
                    .filter(|t| local_tabs.contains(&t.id))
                    .cloned()
                    .collect();
                if let Some(existing) = target.window_groups.iter_mut().find(|g| g.id == group.id) {
                    for tab in tabs {
                        if !existing.tabs.iter().any(|t| t.id == tab.id) {
                            existing.tabs.push(tab);
                        }
                    }
                } else if !tabs.is_empty() {
                    let mut retained = group.clone();
                    retained.tabs = tabs;
                    if !retained.tabs.iter().any(|t| t.id == retained.active_tab) {
                        retained.active_tab = retained.tabs.first().map_or(0, |t| t.id);
                    }
                    target
                        .window_groups
                        .insert(index.min(target.window_groups.len()), retained);
                }
                if let Some(existing) = target.window_groups.iter_mut().find(|g| g.id == group.id) {
                    existing.width = group.width;
                    existing.maximized = group.maximized;
                    if existing.tabs.iter().any(|t| t.id == group.active_tab) {
                        existing.activate_tab(group.active_tab);
                    }
                }
            }
        }
    }
    if incoming.workspaces.iter().any(|w| w.id == local.active) {
        incoming.active = local.active;
    }
    for workspace in &incoming.workspaces {
        if let Some(strip) = local.active_strip.get(&workspace.id)
            && workspace.strips.iter().any(|s| s.id == *strip)
        {
            incoming.active_strip.insert(workspace.id, *strip);
        }
        for strip in &workspace.strips {
            if let Some(group) = local.active_group.get(&strip.id)
                && strip.window_groups.iter().any(|g| g.id == *group)
            {
                incoming.active_group.insert(strip.id, *group);
            }
        }
    }
    incoming.outputs = local.outputs.clone();
    incoming
}
