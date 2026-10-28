//! One path from widget requests to the desktop's owner.
//!
//! Widget-index intents are bound to the identities of the admitted source
//! topology synchronously, before any asynchronous dispatch, and handed to
//! the host's [`DesktopCommandSink`]. The owner still validates current
//! ownership and admission when it applies the command.
use crate::intents::{DesktopIntent, LocalViewIntent, SessionIntent};
use tilo_desktop_model::apps::canonical_app_id;
use tilo_desktop_model::commands::{
    BoundCommand, CommandSource, DesktopCommand, TabLocation, TabPlacement,
};
use tilo_desktop_model::{DesktopTree, GroupId, RoomId, Strip, TileContent, WindowGroup};

/// Where a desktop's requests go. Each host implements it once: the native
/// sink commits through its device owner at once, the browser sink
/// speculates and queues for the owner it reaches over the network.
pub trait DesktopCommandSink {
    /// Run `read` over the desktop the widgets render.
    fn with_tree<R>(&self, read: impl FnOnce(&DesktopTree) -> R) -> R;
    /// The admitted revision a command against `room` binds to.
    fn source(&mut self, room: RoomId) -> Result<CommandSource, String>;
    /// Apply a per-device view change. It never changes the topology.
    fn apply_local(&mut self, intent: LocalViewIntent);
    /// Hand a bound command to the owner.
    ///
    /// # Errors
    ///
    /// Returns a description when the owner refuses or the host cannot
    /// carry the command.
    fn submit(&mut self, command: BoundCommand) -> Result<(), String>;
}

fn group_room(tree: &DesktopTree, group: GroupId) -> Option<RoomId> {
    tree.workspaces
        .iter()
        .find(|workspace| {
            workspace
                .strips
                .iter()
                .any(|strip| strip.window_groups.iter().any(|g| g.id == group))
        })
        .map(tilo_desktop_model::Workspace::room)
}

/// Route one widget intent: view changes stay on this device, session
/// changes are bound to identities and submitted to the owner.
pub fn dispatch(sink: &mut impl DesktopCommandSink, intent: DesktopIntent) -> Result<(), String> {
    match intent {
        DesktopIntent::Local(intent) => {
            sink.apply_local(intent);
            Ok(())
        }
        DesktopIntent::Session(intent) => {
            let room = sink
                .with_tree(|tree| group_room(tree, intent.group()))
                .ok_or("source group is no longer present")?;
            let source = sink.source(room)?;
            let command = sink.with_tree(|tree| bind_intent(tree, source, intent))?;
            sink.submit(command)
        }
    }
}

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
#[expect(
    clippy::needless_pass_by_value,
    reason = "public signature the browser desktop host calls with an owned intent"
)]
pub fn bind_intent(
    tree: &DesktopTree,
    source: CommandSource,
    intent: SessionIntent,
) -> Result<BoundCommand, String> {
    let workspace = tree
        .workspaces
        .iter()
        .find(|workspace| workspace.room().to_string() == source.room)
        .ok_or("source room is not present in the rendered desktop")?;
    let group_id = intent.group();
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
        SessionIntent::MoveGroup { to, before, .. } => {
            let destination = workspace
                .strips
                .iter()
                .find(|candidate| candidate.id == to)
                .ok_or("destination strip is no longer present")?;
            if before.is_some_and(|id| {
                id == group.id || !destination.window_groups.iter().any(|g| g.id == id)
            }) {
                return Err("destination group is stale".into());
            }
            DesktopCommand::MoveGroup {
                group: group.id,
                strip: to.0,
                before,
            }
        }
    };
    Ok(BoundCommand { source, command })
}
