//! Resolve widget indices synchronously against the surface's source topology.
//! Native execution must independently validate current ownership and admission.
use crate::{WorkspaceTree, intents::SessionIntent};
pub use qos_peer_state::desktop_command::*;

/// Room heads also advance for receipts, enrollment and document durability.
/// Those changes must not invalidate a gesture against an unchanged desktop.
/// Compare the original admitted desktop with the current one, without
/// rebinding the command or weakening the current caller/target checks.
pub fn desktop_source_matches(
    doc: &automerge::AutoCommit,
    heads: &[[u8; 32]],
) -> Result<bool, String> {
    use automerge::ReadDoc;
    let heads = heads
        .iter()
        .copied()
        .map(automerge::ChangeHash)
        .collect::<Vec<_>>();
    if heads.is_empty()
        || heads
            .iter()
            .any(|head| doc.get_change_by_hash(head).is_none())
    {
        return Ok(false);
    }
    for key in [
        "room",
        "owner",
        "name",
        "contents",
        "contents_owners_v1",
        // Also prevents an old launch from being reused after its tab closes.
        "desktop_counter_v1",
    ] {
        let previous = doc
            .get_all_at(automerge::ROOT, key, &heads)
            .map_err(|e| e.to_string())?;
        let current = doc
            .get_all(automerge::ROOT, key)
            .map_err(|e| e.to_string())?;
        if previous != current {
            return Ok(false);
        }
        for (value, object) in current {
            if matches!(value, automerge::Value::Object(_))
                && doc
                    .hydrate(&object, Some(&heads))
                    .map_err(|e| e.to_string())?
                    != doc.hydrate(&object, None).map_err(|e| e.to_string())?
            {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

/// Where [`DesktopCommand::OpenApp`] opens: a new group at the end of the last
/// strip `owns` accepts, or the first strip of an empty workspace. `owns`
/// lets the owner keep placement inside its own topology namespace.
pub fn open_app_placement(
    workspace: &crate::Workspace,
    owns: impl Fn(u64) -> bool,
) -> Result<TabPlacement, String> {
    if workspace.strips.is_empty() {
        return Ok(TabPlacement::EmptyWorkspace);
    }
    workspace
        .strips
        .iter()
        .rev()
        .find(|strip| owns(strip.id.0))
        .map(|strip| TabPlacement::NewGroup {
            strip: strip.id.0,
            after: None,
        })
        .ok_or_else(|| "no desktop strip can receive the opened application".into())
}

/// Canonical topology reduction. The host checks authority and supplies fresh
/// identities and a checked record constructor before the tab enters the tree.
pub fn apply_command(
    workspace: &mut crate::Workspace,
    command: &DesktopCommand,
    mint: &mut impl FnMut() -> Result<u64, String>,
    create: &mut impl FnMut(&str, u64) -> Result<(String, crate::TileContent), String>,
) -> Result<(), String> {
    use crate::{Strip, StripId, Tab, WindowGroup, WindowWidth};
    fn strip_index(w: &crate::Workspace, id: u64) -> Result<usize, String> {
        w.strips
            .iter()
            .position(|s| s.id.0 == id)
            .ok_or("strip was removed".into())
    }
    fn location(w: &crate::Workspace, l: &TabLocation) -> Result<(usize, usize, usize), String> {
        let s = strip_index(w, l.strip)?;
        let g = w.strips[s]
            .window_groups
            .iter()
            .position(|g| g.id == l.group)
            .ok_or("group was removed")?;
        let t = w.strips[s].window_groups[g]
            .tabs
            .iter()
            .position(|t| t.id == l.tab)
            .ok_or("tab was removed or moved")?;
        Ok((s, g, t))
    }
    fn after<T>(items: &[T], anchor: Option<u64>, id: impl Fn(&T) -> u64) -> Result<usize, String> {
        anchor.map_or(Ok(items.len()), |anchor| {
            items
                .iter()
                .position(|v| id(v) == anchor)
                .map(|i| i + 1)
                .ok_or("placement anchor was removed".into())
        })
    }
    fn validate_placement(w: &crate::Workspace, placement: &TabPlacement) -> Result<(), String> {
        match placement {
            TabPlacement::EmptyWorkspace => {
                if !w.strips.is_empty() {
                    return Err("workspace is no longer empty".into());
                }
            }
            TabPlacement::InGroup {
                strip,
                group,
                after: anchor,
            } => {
                let s = strip_index(w, *strip)?;
                let g = w.strips[s]
                    .window_groups
                    .iter()
                    .find(|g| g.id == *group)
                    .ok_or("destination group was removed")?;
                after(&g.tabs, *anchor, |t| t.id)?;
            }
            TabPlacement::NewGroup {
                strip,
                after: anchor,
            } => {
                let s = strip_index(w, *strip)?;
                after(&w.strips[s].window_groups, *anchor, |g| g.id)?;
            }
        }
        Ok(())
    }
    fn place(
        w: &mut crate::Workspace,
        tab: Tab,
        placement: &TabPlacement,
        group_id: u64,
        new_strip: Option<u64>,
    ) -> Result<(), String> {
        match placement {
            TabPlacement::EmptyWorkspace => w.strips.push(Strip {
                id: StripId(new_strip.ok_or("fresh strip identity missing")?),
                name: "Main".into(),
                window_groups: vec![WindowGroup {
                    id: group_id,
                    active_tab: tab.id,
                    tabs: vec![tab],
                    width: WindowWidth::Normal,
                    maximized: false,
                }],
            }),
            TabPlacement::InGroup {
                strip,
                group,
                after: anchor,
            } => {
                let s = strip_index(w, *strip)?;
                let g = w.strips[s]
                    .window_groups
                    .iter_mut()
                    .find(|g| g.id == *group)
                    .ok_or("destination group was removed")?;
                let index = after(&g.tabs, *anchor, |t| t.id)?;
                g.tabs.insert(index, tab);
            }
            TabPlacement::NewGroup {
                strip,
                after: anchor,
            } => {
                let s = strip_index(w, *strip)?;
                let groups = &mut w.strips[s].window_groups;
                let index = after(groups, *anchor, |g| g.id)?;
                groups.insert(
                    index,
                    WindowGroup {
                        id: group_id,
                        active_tab: tab.id,
                        tabs: vec![tab],
                        width: WindowWidth::Normal,
                        maximized: false,
                    },
                );
            }
        }
        Ok(())
    }
    fn remove(w: &mut crate::Workspace, at: (usize, usize, usize)) -> Tab {
        let (s, g, t) = at;
        let group = &mut w.strips[s].window_groups[g];
        let tab = group.tabs.remove(t);
        if group.active_tab == tab.id {
            group.active_tab = group
                .tabs
                .get(t)
                .or_else(|| group.tabs.last())
                .map_or(0, |t| t.id);
        }
        if group.tabs.is_empty() {
            w.strips[s].window_groups.remove(g);
        }
        tab
    }
    fn name(value: &str) -> Result<String, String> {
        let value = value.trim();
        if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
            return Err("invalid desktop name".into());
        }
        Ok(value.into())
    }
    if let DesktopCommand::OpenApp { app } = command {
        let resolved = DesktopCommand::OpenTab {
            app: app.clone(),
            placement: open_app_placement(workspace, |_| true)?,
        };
        return apply_command(workspace, &resolved, mint, create);
    }
    match command {
        DesktopCommand::OpenApp { .. } => unreachable!("resolved to OpenTab above"),
        DesktopCommand::OpenTab { app, placement } => {
            validate_placement(workspace, placement)?;
            let id = mint()?;
            let group = mint()?;
            let (title, tile) = create(app, id)?;
            let strip = matches!(placement, TabPlacement::EmptyWorkspace)
                .then(|| mint())
                .transpose()?;
            place(workspace, Tab { id, title, tile }, placement, group, strip)?;
        }
        DesktopCommand::DuplicateTab { location: at } => {
            let (s, g, t) = location(workspace, at)?;
            let original = &workspace.strips[s].window_groups[g].tabs[t];
            if !matches!(
                original.tile,
                crate::TileContent::AppPlaceholder { .. } | crate::TileContent::Stub { .. }
            ) {
                return Err("native or foreign surface cannot be duplicated".into());
            }
            let app = crate::resolver::canonical_app_id(&original.tile)
                .ok_or("missing canonical application identity")?;
            let id = mint()?;
            let (_, tile) = create(&app, id)?;
            let tab = Tab {
                id,
                title: original.title.clone(),
                tile,
            };
            workspace.strips[s].window_groups[g].tabs.insert(t + 1, tab);
        }
        DesktopCommand::CloseTab { location: at } => {
            remove(workspace, location(workspace, at)?);
        }
        DesktopCommand::ReorderTab {
            location: at,
            before,
        } => {
            let (s, g, t) = location(workspace, at)?;
            let tabs = &mut workspace.strips[s].window_groups[g].tabs;
            if before == &Some(at.tab) {
                return Err("tab cannot be its own destination".into());
            }
            if before.is_some_and(|id| !tabs.iter().any(|t| t.id == id)) {
                return Err("destination tab was removed".into());
            }
            let tab = tabs.remove(t);
            let destination = before
                .and_then(|id| tabs.iter().position(|t| t.id == id))
                .unwrap_or(tabs.len());
            tabs.insert(destination, tab);
        }
        DesktopCommand::ReorderGroup {
            strip,
            group,
            before,
        } => {
            let s = strip_index(workspace, *strip)?;
            let groups = &mut workspace.strips[s].window_groups;
            let at = groups
                .iter()
                .position(|g| g.id == *group)
                .ok_or("group was removed")?;
            if before == &Some(*group) {
                return Err("group cannot be its own destination".into());
            }
            if before.is_some_and(|id| !groups.iter().any(|g| g.id == id)) {
                return Err("destination group was removed".into());
            }
            let group = groups.remove(at);
            let destination = before
                .and_then(|id| groups.iter().position(|g| g.id == id))
                .unwrap_or(groups.len());
            groups.insert(destination, group);
        }
        DesktopCommand::MoveTab {
            location: at,
            placement,
        } => {
            let source = location(workspace, at)?;
            validate_placement(workspace, placement)?;
            if matches!(placement, TabPlacement::InGroup { group, .. } if *group == at.group)
                || matches!(placement, TabPlacement::NewGroup { after: Some(group), .. } if *group == at.group && workspace.strips[source.0].window_groups[source.1].tabs.len() == 1)
            {
                return Err("move requires a distinct retained destination".into());
            }
            let group = mint()?;
            if let TabPlacement::NewGroup {
                strip,
                after: anchor,
            } = placement
            {
                let destination_strip = strip_index(workspace, *strip)?;
                let destination = after(
                    &workspace.strips[destination_strip].window_groups,
                    *anchor,
                    |g| g.id,
                )?;
                let mut tree = WorkspaceTree {
                    workspaces: vec![workspace.clone()],
                    ..Default::default()
                };
                let moved = if *strip == at.strip {
                    tree.split_tab_into_reserved_group(StripId(*strip), at.tab, destination, group)
                } else {
                    tree.move_tab_across_strips(
                        StripId(at.strip),
                        StripId(*strip),
                        at.tab,
                        group,
                        destination,
                    )
                };
                moved.ok_or("native topology rejected the tab move")?;
                *workspace = tree.workspaces.remove(0);
                return Ok(());
            }
            let tab = remove(workspace, source);
            place(workspace, tab, placement, group, None)?;
        }
        DesktopCommand::SplitTab {
            location: at,
            before,
        } => {
            let (s, g, _) = location(workspace, at)?;
            if workspace.strips[s].window_groups[g].tabs.len() < 2 {
                return Err("cannot split a group's only tab".into());
            }
            if before
                .is_some_and(|id| !workspace.strips[s].window_groups.iter().any(|g| g.id == id))
            {
                return Err("destination group was removed".into());
            }
            let id = mint()?;
            let groups = &workspace.strips[s].window_groups;
            let destination = before
                .and_then(|id| groups.iter().position(|g| g.id == id))
                .unwrap_or(groups.len());
            let mut tree = WorkspaceTree {
                workspaces: vec![workspace.clone()],
                ..Default::default()
            };
            tree.split_tab_into_reserved_group(StripId(at.strip), at.tab, destination, id)
                .ok_or("native topology rejected the tab split")?;
            *workspace = tree.workspaces.remove(0);
        }
        DesktopCommand::CreateStrip { name: label } => workspace.strips.push(Strip {
            id: StripId(mint()?),
            name: name(label)?,
            window_groups: vec![],
        }),
        DesktopCommand::RenameStrip { strip, name: label } => {
            let s = strip_index(workspace, *strip)?;
            workspace.strips[s].name = name(label)?;
        }
        DesktopCommand::RemoveStrip { strip } => {
            let s = strip_index(workspace, *strip)?;
            if !workspace.strips[s].window_groups.is_empty() {
                return Err("strip must be empty before removal".into());
            }
            if workspace.strips.len() == 1 {
                return Err("cannot remove the last strip".into());
            }
            workspace.strips.remove(s);
        }
        DesktopCommand::RenameWorkspace { name: label } => workspace.name = name(label)?,
    }
    Ok(())
}

pub fn bind_intent(
    tree: &WorkspaceTree,
    source: CommandSource,
    intent: SessionIntent,
) -> Result<BoundCommand, String> {
    if source.heads.is_empty() {
        return Err("desktop command has no admitted source revision".into());
    }
    let workspace = tree
        .workspaces
        .iter()
        .find(|workspace| workspace.room.to_string() == source.room)
        .ok_or("source room is not present in the rendered desktop")?;
    let group_id = match intent {
        SessionIntent::CloseTab { group, .. }
        | SessionIntent::ReorderTab { group, .. }
        | SessionIntent::ReorderGroup { group, .. }
        | SessionIntent::OpenTab { group, .. } => group,
    };
    let (strip, group) = workspace
        .strips
        .iter()
        .find_map(|strip| {
            strip
                .window_groups
                .iter()
                .find(|group| group.id == group_id)
                .map(|group| (strip, group))
        })
        .ok_or("source group is no longer present")?;
    let command = match intent {
        SessionIntent::CloseTab { tab, .. } => {
            if !group.tabs.iter().any(|candidate| candidate.id == tab) {
                return Err("source tab is no longer present in the named group".into());
            }
            DesktopCommand::CloseTab {
                location: TabLocation {
                    strip: strip.id.0,
                    group: group.id,
                    tab,
                },
            }
        }
        SessionIntent::ReorderTab { from, to, .. } => {
            let tab = group.tabs.get(from).ok_or("source tab index is stale")?.id;
            if to >= group.tabs.len() {
                return Err("destination tab index is stale".into());
            }
            let before = group
                .tabs
                .iter()
                .filter(|candidate| candidate.id != tab)
                .nth(to)
                .map(|tab| tab.id);
            DesktopCommand::ReorderTab {
                location: TabLocation {
                    strip: strip.id.0,
                    group: group.id,
                    tab,
                },
                before,
            }
        }
        SessionIntent::ReorderGroup {
            strip: source_strip,
            to,
            ..
        } => {
            if source_strip != strip.id || to >= strip.window_groups.len() {
                return Err("destination group index is stale".into());
            }
            let before = strip
                .window_groups
                .iter()
                .filter(|candidate| candidate.id != group.id)
                .nth(to)
                .map(|group| group.id);
            DesktopCommand::ReorderGroup {
                strip: strip.id.0,
                group: group.id,
                before,
            }
        }
        SessionIntent::OpenTab {
            strip: source_strip,
            ..
        } => {
            if source_strip != strip.id {
                return Err("source strip is stale".into());
            }
            let tab = group
                .tabs
                .iter()
                .find(|tab| tab.id == group.active_tab)
                .ok_or("selected source tab is no longer present")?;
            if !matches!(
                tab.tile,
                crate::TileContent::AppPlaceholder { .. } | crate::TileContent::Stub { .. }
            ) {
                return Err("native or foreign surface cannot be duplicated".into());
            }
            let app = crate::resolver::canonical_app_id(&tab.tile)
                .ok_or("source application has no canonical identity")?;
            DesktopCommand::OpenTab {
                app,
                placement: TabPlacement::InGroup {
                    strip: strip.id.0,
                    group: group.id,
                    after: Some(tab.id),
                },
            }
        }
    };
    Ok(BoundCommand { source, command })
}
