//! The topology-namespace checks every owner runs before it reduces a
//! command: an open resolved to the exact tab a person would make, and every
//! strip, group and tab a command names kept inside the owner's namespace.

use super::placement::{OpenContext, open_app_placement};
use super::{DesktopCommand, TabLocation, TabPlacement};
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
        DesktopCommand::RenameStrip { strip, .. } | DesktopCommand::RemoveStrip { strip } => {
            own(*strip)
        }
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
