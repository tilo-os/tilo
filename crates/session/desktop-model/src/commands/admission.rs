//! The room, freshness and topology-namespace checks every owner runs before
//! it reduces a command: an open resolved to the exact tab a person would
//! make, every strip, group and tab kept inside the owner's namespace, and
//! the command bound to the source revision the requester actually saw.
//!
//! Authority is the host's: a browser asks [`super::may_command`], a native
//! owner decides by the id namespace it writes. This module only decides
//! what every host must decide alike, before any mutation.

use automerge::AutoCommit;

use super::placement::{OpenContext, open_app_placement};
use super::{CommandSource, DesktopCommand, TabLocation, TabPlacement};
use crate::ids::RoomId;
use crate::projection::{
    ROOM_CONTENTS_KEY, ROOM_CONTENTS_OWNERS_KEY, ROOM_DESKTOP_COUNTER_KEY, ROOM_ID_KEY,
    ROOM_NAME_KEY, ROOM_OWNER_KEY,
};
use crate::workspace::Workspace;

/// `command` with [`DesktopCommand::OpenApp`] resolved to the
/// [`DesktopCommand::OpenTab`] a person would make, placed in the owner's own
/// turn inside the strips `owned` accepts. Every other command is returned
/// unchanged; the owner checks and applies that exact command.
///
/// # Errors
///
/// Returns a description when no owned strip can receive an opened app.
pub fn resolve_open_app(
    workspace: &Workspace,
    command: DesktopCommand,
    owned: impl Fn(u64) -> bool,
) -> Result<DesktopCommand, String> {
    Ok(match command {
        DesktopCommand::OpenApp { app, beside } => {
            let at = beside
                .filter(|group| owned(*group))
                .and_then(|group| OpenContext::beside(workspace, group))
                .unwrap_or_default();
            DesktopCommand::OpenTab {
                app,
                placement: open_app_placement(workspace, &at, owned)?,
            }
        }
        other => other,
    })
}

/// Never touch another topology owner's namespace (native peers, other
/// hosts, process-local surface notices): every strip, group and tab
/// `command` names must be one `owned` accepts.
///
/// # Errors
///
/// Returns a description when a target belongs to another owner, or when an
/// [`DesktopCommand::OpenApp`] was not resolved first ([`resolve_open_app`]).
pub fn check_owned(command: &DesktopCommand, owned: impl Fn(u64) -> bool) -> Result<(), String> {
    let own = |id: u64| {
        if owned(id) {
            Ok(())
        } else {
            Err("target belongs to another topology owner".to_owned())
        }
    };
    let own_all = |ids: &[Option<u64>]| ids.iter().flatten().try_for_each(|id| own(*id));
    let placement = |p: &TabPlacement| match p {
        TabPlacement::EmptyWorkspace => Ok(()),
        TabPlacement::InGroup {
            strip,
            group,
            after,
        } => own_all(&[Some(*strip), Some(*group), *after]),
        TabPlacement::NewGroup { strip, after } => own_all(&[Some(*strip), *after]),
    };
    let location = |l: &TabLocation| own_all(&[Some(l.strip), Some(l.group), Some(l.tab)]);
    match command {
        DesktopCommand::OpenApp { .. } => Err("open-app must be resolved to a placement".into()),
        DesktopCommand::OpenTab { placement: p, .. } => placement(p),
        DesktopCommand::CloseTab { location: l } | DesktopCommand::DuplicateTab { location: l } => {
            location(l)
        }
        DesktopCommand::ReorderTab {
            location: l,
            before,
        }
        | DesktopCommand::SplitTab {
            location: l,
            before,
        } => {
            location(l)?;
            own_all(&[*before])
        }
        DesktopCommand::ReorderGroup {
            strip,
            group,
            before,
        }
        | DesktopCommand::MoveGroup {
            strip,
            group,
            before,
        } => own_all(&[Some(*strip), Some(*group), *before]),
        DesktopCommand::MoveTab {
            location: l,
            placement: p,
        } => {
            location(l)?;
            placement(p)
        }
        DesktopCommand::RenameStrip { strip, .. }
        | DesktopCommand::RemoveStrip { strip }
        | DesktopCommand::MoveStrip { strip, .. } => own(*strip),
        DesktopCommand::CreateStrip { .. }
        | DesktopCommand::RenameWorkspace { .. }
        | DesktopCommand::SaveShortcut { .. }
        | DesktopCommand::DeleteShortcut { .. }
        | DesktopCommand::ClaimNamespace { .. }
        | DesktopCommand::SavePreset { .. }
        | DesktopCommand::AssignPreset { .. }
        | DesktopCommand::RecordEffectReceipt { .. }
        | DesktopCommand::GrantAppAction { .. }
        | DesktopCommand::RevokeAppAction { .. } => Ok(()),
    }
}

/// Admit `command` from `source` against `workspace` and the room document
/// `room` it was read from, returning the exact command to apply: every host
/// runs this before it mutates anything.
///
/// The source room must be `workspace`'s canonical room, and every named
/// strip, group and tab must exist in that workspace and be `owned` by this
/// host. A command that is not placement-independent must still describe the
/// current desktop topology at its source: heads that advanced only for
/// receipts or enrollment still admit it, a changed strip, group, tab or
/// counter does not.
///
/// # Errors
///
/// Returns a description when the room, source revision or target namespace
/// is not the owner's to apply.
pub fn admit(
    workspace: &Workspace,
    source: &CommandSource,
    room: &mut AutoCommit,
    command: &DesktopCommand,
    owned: impl Fn(u64) -> bool,
) -> Result<DesktopCommand, String> {
    let source_room = RoomId::parse(&source.room)
        .filter(|room| room.to_string() == source.room)
        .ok_or_else(|| "desktop source does not name a canonical room".to_owned())?;
    if source_room != workspace.room() {
        return Err("desktop source room does not match the target workspace".into());
    }
    let placement_independent = command.is_placement_independent();
    if !placement_independent
        && (source.heads.is_empty()
            || (sorted(&head_bytes(room.get_heads())) != sorted(&source.heads)
                && !source_matches(room, &source.heads)?))
    {
        return Err("desktop source revision is stale".into());
    }
    // The host owns ids; the source room owns which of them this command may
    // name. A target from another workspace is never this room's to change.
    let in_owned_workspace = |id: u64| in_workspace(workspace, id) && owned(id);
    let resolved = resolve_open_app(workspace, command.clone(), in_owned_workspace)?;
    check_owned(&resolved, in_owned_workspace)?;
    Ok(resolved)
}

/// Room heads also advance for receipts, enrollment and document durability.
/// Those changes must not invalidate a gesture against an unchanged desktop.
/// Compare the topology-bearing fields at the source heads with the room's
/// current ones, without rebinding the command or weakening the caller and
/// target checks.
///
/// # Errors
///
/// Returns a description when the document cannot be read at `heads`.
fn source_matches(room: &AutoCommit, heads: &[[u8; 32]]) -> Result<bool, String> {
    use automerge::ReadDoc;
    let heads = heads
        .iter()
        .copied()
        .map(automerge::ChangeHash)
        .collect::<Vec<_>>();
    if heads.is_empty()
        || heads
            .iter()
            .any(|head| room.get_change_by_hash(head).is_none())
    {
        return Ok(false);
    }
    for key in [
        ROOM_ID_KEY,
        ROOM_OWNER_KEY,
        ROOM_NAME_KEY,
        ROOM_CONTENTS_KEY,
        ROOM_CONTENTS_OWNERS_KEY,
        // Also prevents an old launch from being reused after its tab closes.
        ROOM_DESKTOP_COUNTER_KEY,
    ] {
        let previous = room
            .get_all_at(automerge::ROOT, key, &heads)
            .map_err(|e| e.to_string())?;
        let current = room
            .get_all(automerge::ROOT, key)
            .map_err(|e| e.to_string())?;
        if previous != current {
            return Ok(false);
        }
        for (value, object) in current {
            if matches!(value, automerge::Value::Object(_))
                && room
                    .hydrate(&object, Some(&heads))
                    .map_err(|e| e.to_string())?
                    != room.hydrate(&object, None).map_err(|e| e.to_string())?
            {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

fn head_bytes(heads: Vec<automerge::ChangeHash>) -> Vec<[u8; 32]> {
    heads.into_iter().map(|head| head.0).collect()
}

fn sorted(heads: &[[u8; 32]]) -> Vec<[u8; 32]> {
    let mut heads = heads.to_vec();
    heads.sort_unstable();
    heads
}

/// Whether `id` names a strip, group or tab of `workspace`.
fn in_workspace(workspace: &Workspace, id: u64) -> bool {
    workspace.strips.iter().any(|strip| {
        strip.id.0 == id
            || strip
                .window_groups
                .iter()
                .any(|group| group.id == id || group.tabs.iter().any(|tab| tab.id == id))
    })
}

/// The next counter an owner mints from in namespace `salt`: past both the
/// stored room counter and every id of that namespace still in `workspace`.
pub fn namespace_counter(workspace: &Workspace, salt: u16, stored: u64) -> u64 {
    let maximum = workspace
        .strips
        .iter()
        .flat_map(|strip| {
            std::iter::once(strip.id.0).chain(strip.window_groups.iter().flat_map(|group| {
                std::iter::once(group.id).chain(group.tabs.iter().map(|tab| tab.id))
            }))
        })
        .filter(|id| crate::ids::peer_salt_of(*id) == salt)
        .map(crate::ids::counter_of)
        .max()
        .unwrap_or(0);
    stored.max(maximum.saturating_add(1))
}
