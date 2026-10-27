//! What a change does to a room's layout. No UI dependency.
use automerge::AutoCommit;
use tilo_desktop_model::shared_record;

/// Whether `after` puts anything into the room's layout that `before` does
/// not hold as it is: a workspace, strip, group or window that is new,
/// changed or moved. Taking entries out adds nothing.
#[must_use]
pub fn adds_to(before: Option<&AutoCommit>, after: &AutoCommit) -> bool {
    let old = before.map(shared_record::entries).unwrap_or_default();
    shared_record::entries(after)
        .iter()
        .any(|(entry, value)| old.get(entry) != Some(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_desktop_model::ownership::SharedDesktop;
    use tilo_desktop_model::{DesktopTree, RoomId, Strip, StripId, Workspace, WorkspaceId};

    fn desktop(strips: &[(u64, &str)]) -> SharedDesktop {
        let room = RoomId::parse("6f1f2d3c-1111-4222-8333-944445555666").unwrap();
        let mut workspace = Workspace::in_room(WorkspaceId(1), room, "Home");
        workspace.strips = strips
            .iter()
            .map(|(id, name)| Strip::new(StripId(*id), *name))
            .collect();
        SharedDesktop {
            tree: DesktopTree {
                active: workspace.id,
                workspaces: vec![workspace],
                ..DesktopTree::default()
            },
            ..SharedDesktop::default()
        }
    }

    #[test]
    fn adding_or_changing_an_entry_adds_and_taking_one_out_does_not() {
        let mut before = AutoCommit::new();
        shared_record::write(&mut before, &desktop(&[(10, "Main"), (11, "Later")])).unwrap();
        assert!(adds_to(None, &before));
        assert!(!adds_to(Some(&before), &before.clone()));

        let mut removed = before.clone();
        shared_record::write(&mut removed, &desktop(&[(10, "Main")])).unwrap();
        assert!(!adds_to(Some(&before), &removed));

        let mut renamed = before.clone();
        shared_record::write(&mut renamed, &desktop(&[(10, "Main"), (11, "Soon")])).unwrap();
        assert!(adds_to(Some(&before), &renamed));
    }
}
