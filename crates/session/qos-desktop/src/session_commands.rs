//! Resolve widget indices synchronously against the surface's source topology.
//! Native execution must independently validate current ownership and admission.
use crate::{WorkspaceTree, intents::SessionIntent};
pub use qos_peer_state::desktop_command::*;

/// Room heads also advance for receipts, enrollment and document durability.
/// Those changes must not invalidate a gesture against an unchanged desktop.
/// Compare the original admitted desktop with the current one, without
/// rebinding the command or weakening the current caller/target checks.
///
/// # Errors
///
/// Returns a description when the document cannot be read at `heads`.
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
///
/// # Errors
///
/// Returns a description when no strip `owns` accepts can receive the tab.
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

use crate::{Strip, StripId, Tab, WindowGroup, WindowWidth};

/// Strip, group and tab indices of one located tab.
type TabIndex = (usize, usize, usize);

fn strip_index(workspace: &crate::Workspace, id: u64) -> Result<usize, String> {
    workspace
        .strips
        .iter()
        .position(|strip| strip.id.0 == id)
        .ok_or_else(|| "strip was removed".into())
}
fn strip_at(workspace: &crate::Workspace, index: usize) -> Result<&Strip, String> {
    workspace
        .strips
        .get(index)
        .ok_or_else(|| "strip was removed".into())
}
fn strip_at_mut(workspace: &mut crate::Workspace, index: usize) -> Result<&mut Strip, String> {
    workspace
        .strips
        .get_mut(index)
        .ok_or_else(|| "strip was removed".into())
}
fn group_at(
    workspace: &crate::Workspace,
    strip: usize,
    group: usize,
) -> Result<&WindowGroup, String> {
    strip_at(workspace, strip)?
        .window_groups
        .get(group)
        .ok_or_else(|| "group was removed".into())
}
fn group_at_mut(
    workspace: &mut crate::Workspace,
    strip: usize,
    group: usize,
) -> Result<&mut WindowGroup, String> {
    strip_at_mut(workspace, strip)?
        .window_groups
        .get_mut(group)
        .ok_or_else(|| "group was removed".into())
}
fn location(workspace: &crate::Workspace, at: &TabLocation) -> Result<TabIndex, String> {
    let strip = strip_index(workspace, at.strip)?;
    let (group, window) = strip_at(workspace, strip)?
        .window_groups
        .iter()
        .enumerate()
        .find(|(_, window)| window.id == at.group)
        .ok_or("group was removed")?;
    let tab = window
        .tabs
        .iter()
        .position(|tab| tab.id == at.tab)
        .ok_or("tab was removed or moved")?;
    Ok((strip, group, tab))
}
fn after<T>(items: &[T], anchor: Option<u64>, id: impl Fn(&T) -> u64) -> Result<usize, String> {
    anchor.map_or(Ok(items.len()), |anchor| {
        items
            .iter()
            .position(|item| id(item) == anchor)
            .map(|index| index.saturating_add(1))
            .ok_or_else(|| "placement anchor was removed".into())
    })
}
fn validate_placement(
    workspace: &crate::Workspace,
    placement: &TabPlacement,
) -> Result<(), String> {
    match placement {
        TabPlacement::EmptyWorkspace => {
            if !workspace.strips.is_empty() {
                return Err("workspace is no longer empty".into());
            }
        }
        TabPlacement::InGroup {
            strip,
            group,
            after: anchor,
        } => {
            let strip = strip_index(workspace, *strip)?;
            let window = strip_at(workspace, strip)?
                .window_groups
                .iter()
                .find(|window| window.id == *group)
                .ok_or("destination group was removed")?;
            after(&window.tabs, *anchor, |tab| tab.id)?;
        }
        TabPlacement::NewGroup {
            strip,
            after: anchor,
        } => {
            let strip = strip_index(workspace, *strip)?;
            after(
                &strip_at(workspace, strip)?.window_groups,
                *anchor,
                |window| window.id,
            )?;
        }
    }
    Ok(())
}
fn place(
    workspace: &mut crate::Workspace,
    tab: Tab,
    placement: &TabPlacement,
    group_id: u64,
    new_strip: Option<u64>,
) -> Result<(), String> {
    match placement {
        TabPlacement::EmptyWorkspace => workspace.strips.push(Strip {
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
            let strip = strip_index(workspace, *strip)?;
            let window = strip_at_mut(workspace, strip)?
                .window_groups
                .iter_mut()
                .find(|window| window.id == *group)
                .ok_or("destination group was removed")?;
            let index = after(&window.tabs, *anchor, |tab| tab.id)?;
            window.tabs.insert(index, tab);
        }
        TabPlacement::NewGroup {
            strip,
            after: anchor,
        } => {
            let strip = strip_index(workspace, *strip)?;
            let groups = &mut strip_at_mut(workspace, strip)?.window_groups;
            let index = after(groups, *anchor, |window| window.id)?;
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
fn remove(workspace: &mut crate::Workspace, at: TabIndex) -> Result<Tab, String> {
    let (strip, group, tab) = at;
    let groups = &mut strip_at_mut(workspace, strip)?.window_groups;
    let window = groups.get_mut(group).ok_or("group was removed")?;
    if tab >= window.tabs.len() {
        return Err("tab was removed or moved".into());
    }
    let removed = window.tabs.remove(tab);
    if window.active_tab == removed.id {
        window.active_tab = window
            .tabs
            .get(tab)
            .or_else(|| window.tabs.last())
            .map_or(0, |next| next.id);
    }
    if window.tabs.is_empty() {
        groups.remove(group);
    }
    Ok(removed)
}
fn name(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) {
        return Err("invalid desktop name".into());
    }
    Ok(value.into())
}
fn duplicate_tab(
    workspace: &mut crate::Workspace,
    at: &TabLocation,
    mint: &mut impl FnMut() -> Result<u64, String>,
    create: &mut impl FnMut(&str, u64) -> Result<(String, crate::TileContent), String>,
) -> Result<(), String> {
    let (strip, group, tab) = location(workspace, at)?;
    let original = group_at(workspace, strip, group)?
        .tabs
        .get(tab)
        .ok_or("tab was removed or moved")?;
    if !matches!(
        original.tile,
        crate::TileContent::AppPlaceholder { .. } | crate::TileContent::Stub { .. }
    ) {
        return Err("native or foreign surface cannot be duplicated".into());
    }
    let app = crate::resolver::canonical_app_id(&original.tile)
        .ok_or("missing canonical application identity")?;
    let title = original.title.clone();
    let id = mint()?;
    let (_, content) = create(&app, id)?;
    let duplicate = Tab {
        id,
        title,
        tile: content,
    };
    group_at_mut(workspace, strip, group)?
        .tabs
        .insert(tab.saturating_add(1), duplicate);
    Ok(())
}
fn move_tab(
    workspace: &mut crate::Workspace,
    at: &TabLocation,
    placement: &TabPlacement,
    mint: &mut impl FnMut() -> Result<u64, String>,
) -> Result<(), String> {
    let source = location(workspace, at)?;
    validate_placement(workspace, placement)?;
    let (source_strip, source_group, _) = source;
    let only_tab =
        group_at(workspace, source_strip, source_group).is_ok_and(|window| window.tabs.len() == 1);
    if matches!(placement, TabPlacement::InGroup { group, .. } if *group == at.group)
        || matches!(placement, TabPlacement::NewGroup { after: Some(group), .. } if *group == at.group && only_tab)
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
            &strip_at(workspace, destination_strip)?.window_groups,
            *anchor,
            |window| window.id,
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
    let tab = remove(workspace, source)?;
    place(workspace, tab, placement, group, None)
}
fn split_tab(
    workspace: &mut crate::Workspace,
    at: &TabLocation,
    before: Option<u64>,
    mint: &mut impl FnMut() -> Result<u64, String>,
) -> Result<(), String> {
    let (strip, group, _) = location(workspace, at)?;
    if group_at(workspace, strip, group)?.tabs.len() < 2 {
        return Err("cannot split a group's only tab".into());
    }
    let groups = &strip_at(workspace, strip)?.window_groups;
    if before.is_some_and(|id| !groups.iter().any(|window| window.id == id)) {
        return Err("destination group was removed".into());
    }
    let id = mint()?;
    let groups = &strip_at(workspace, strip)?.window_groups;
    let destination = before
        .and_then(|id| groups.iter().position(|window| window.id == id))
        .unwrap_or(groups.len());
    let mut tree = WorkspaceTree {
        workspaces: vec![workspace.clone()],
        ..Default::default()
    };
    tree.split_tab_into_reserved_group(StripId(at.strip), at.tab, destination, id)
        .ok_or("native topology rejected the tab split")?;
    *workspace = tree.workspaces.remove(0);
    Ok(())
}

/// Canonical topology reduction. The host checks authority and supplies fresh
/// identities and a checked record constructor before the tab enters the tree.
///
/// # Errors
///
/// Returns a description when the command no longer matches the topology, a
/// name is invalid, or `mint`/`create` refuses.
pub fn apply_command(
    workspace: &mut crate::Workspace,
    command: &DesktopCommand,
    mint: &mut impl FnMut() -> Result<u64, String>,
    create: &mut impl FnMut(&str, u64) -> Result<(String, crate::TileContent), String>,
) -> Result<(), String> {
    match command {
        DesktopCommand::OpenApp { app } => {
            let resolved = DesktopCommand::OpenTab {
                app: app.clone(),
                placement: open_app_placement(workspace, |_| true)?,
            };
            return apply_command(workspace, &resolved, mint, create);
        }
        DesktopCommand::OpenTab { app, placement } => {
            validate_placement(workspace, placement)?;
            let id = mint()?;
            let group = mint()?;
            let (title, content) = create(app, id)?;
            let strip = matches!(placement, TabPlacement::EmptyWorkspace)
                .then(&mut *mint)
                .transpose()?;
            let tab = Tab {
                id,
                title,
                tile: content,
            };
            place(workspace, tab, placement, group, strip)?;
        }
        DesktopCommand::DuplicateTab { location: at } => {
            duplicate_tab(workspace, at, mint, create)?;
        }
        DesktopCommand::CloseTab { location: at } => {
            remove(workspace, location(workspace, at)?)?;
        }
        DesktopCommand::ReorderTab {
            location: at,
            before,
        } => {
            let (strip, group, tab) = location(workspace, at)?;
            let tabs = &mut group_at_mut(workspace, strip, group)?.tabs;
            if before == &Some(at.tab) {
                return Err("tab cannot be its own destination".into());
            }
            if before.is_some_and(|id| !tabs.iter().any(|candidate| candidate.id == id)) {
                return Err("destination tab was removed".into());
            }
            let moved = tabs.remove(tab);
            let destination = before
                .and_then(|id| tabs.iter().position(|candidate| candidate.id == id))
                .unwrap_or(tabs.len());
            tabs.insert(destination, moved);
        }
        DesktopCommand::ReorderGroup {
            strip,
            group,
            before,
        } => {
            let strip = strip_index(workspace, *strip)?;
            let groups = &mut strip_at_mut(workspace, strip)?.window_groups;
            let at = groups
                .iter()
                .position(|window| window.id == *group)
                .ok_or("group was removed")?;
            if before == &Some(*group) {
                return Err("group cannot be its own destination".into());
            }
            if before.is_some_and(|id| !groups.iter().any(|window| window.id == id)) {
                return Err("destination group was removed".into());
            }
            let moved = groups.remove(at);
            let destination = before
                .and_then(|id| groups.iter().position(|window| window.id == id))
                .unwrap_or(groups.len());
            groups.insert(destination, moved);
        }
        DesktopCommand::MoveTab {
            location: at,
            placement,
        } => move_tab(workspace, at, placement, mint)?,
        DesktopCommand::SplitTab {
            location: at,
            before,
        } => split_tab(workspace, at, *before, mint)?,
        DesktopCommand::CreateStrip { name: label } => workspace.strips.push(Strip {
            id: StripId(mint()?),
            name: name(label)?,
            window_groups: vec![],
        }),
        DesktopCommand::RenameStrip { strip, name: label } => {
            let strip = strip_index(workspace, *strip)?;
            strip_at_mut(workspace, strip)?.name = name(label)?;
        }
        DesktopCommand::RemoveStrip { strip } => {
            let strip = strip_index(workspace, *strip)?;
            if !strip_at(workspace, strip)?.window_groups.is_empty() {
                return Err("strip must be empty before removal".into());
            }
            if workspace.strips.len() == 1 {
                return Err("cannot remove the last strip".into());
            }
            workspace.strips.remove(strip);
        }
        DesktopCommand::RenameWorkspace { name: label } => workspace.name = name(label)?,
    }
    Ok(())
}

/// Duplicate the group's selected application tab right after it.
fn duplicate_active_tab(
    strip: &crate::Strip,
    group: &crate::WindowGroup,
) -> Result<DesktopCommand, String> {
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
    Ok(DesktopCommand::OpenTab {
        app,
        placement: TabPlacement::InGroup {
            strip: strip.id.0,
            group: group.id,
            after: Some(tab.id),
        },
    })
}

/// Bind a widget-index intent to identities in the admitted source topology.
///
/// # Errors
///
/// Returns a description when the source has no revision or the intent's
/// room, group, tab or index is no longer present.
#[expect(
    clippy::needless_pass_by_value,
    reason = "public signature the browser desktop host calls with an owned intent"
)]
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
            duplicate_active_tab(strip, group)?
        }
    };
    Ok(BoundCommand { source, command })
}
