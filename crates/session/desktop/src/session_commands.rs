//! Bind widget-index intents to the identities of the admitted source
//! topology, synchronously, before any asynchronous dispatch. The owner still
//! validates current ownership and admission when it applies the command.
use crate::intents::SessionIntent;
use tilo_desktop_model::apps::canonical_app_id;
use tilo_desktop_model::commands::{
    BoundCommand, CommandSource, DesktopCommand, TabLocation, TabPlacement,
};
use tilo_desktop_model::{DesktopTree, Strip, TileContent, WindowGroup};

/// Duplicate the group's selected application tab right after it.
fn duplicate_active_tab(strip: &Strip, group: &WindowGroup) -> Result<DesktopCommand, String> {
    let tab = group
        .tabs
        .iter()
        .find(|tab| tab.id == group.active_tab)
        .ok_or("selected source tab is no longer present")?;
    if !matches!(
        tab.tile,
        TileContent::AppPlaceholder { .. } | TileContent::Stub { .. }
    ) {
        return Err("native or foreign surface cannot be duplicated".into());
    }
    let app = canonical_app_id(&tab.tile).ok_or("source application has no canonical identity")?;
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
    tree: &DesktopTree,
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
