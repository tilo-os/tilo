//! Conversions between the native shell's desktop model
//! ([`tilo_desktop_model`]) and the portable model the shared action broker
//! speaks ([`qos_desktop`]).
//!
//! Both describe the same `Workspace → Strip → WindowGroup → Tab` data with
//! the same persisted shape, so nested values convert through their serde
//! form. Fields the serde form deliberately leaves out (live collaborators,
//! process-local epochs, the record index mirror) are copied explicitly so a
//! broker snapshot sees exactly what the shell holds.

use serde::Serialize;
use serde::de::DeserializeOwned;

fn via_serde<T: Serialize + ?Sized, U: DeserializeOwned>(value: &T) -> Result<U, String> {
    serde_json::to_value(value)
        .and_then(serde_json::from_value)
        .map_err(|error| format!("desktop model conversion failed: {error}"))
}

pub(crate) fn portable_room(room: tilo_desktop_model::RoomId) -> qos_desktop::RoomId {
    qos_desktop::RoomId::from(room.as_uuid())
}

pub(crate) fn native_room(room: qos_desktop::RoomId) -> tilo_desktop_model::RoomId {
    tilo_desktop_model::RoomId::from(room.as_uuid())
}

pub(crate) fn native_task_status(
    status: qos_desktop::WorkspaceTaskStatus,
) -> tilo_desktop_model::WorkspaceTaskStatus {
    use qos_desktop::WorkspaceTaskStatus as Portable;
    use tilo_desktop_model::WorkspaceTaskStatus as Native;
    match status {
        Portable::Queued => Native::Queued,
        Portable::InProgress => Native::InProgress,
        Portable::Waiting => Native::Waiting,
        Portable::Done => Native::Done,
        Portable::Cancelled => Native::Cancelled,
    }
}

pub(crate) fn portable_workspace(
    workspace: &tilo_desktop_model::Workspace,
) -> Result<qos_desktop::Workspace, String> {
    let mut portable: qos_desktop::Workspace = via_serde(workspace)?;
    // `collaborators` is serialized but never deserialized.
    portable.collaborators = via_serde(&workspace.collaborators)?;
    Ok(portable)
}

pub(crate) fn native_workspace(
    workspace: &qos_desktop::Workspace,
) -> Result<tilo_desktop_model::Workspace, String> {
    let mut native: tilo_desktop_model::Workspace = via_serde(workspace)?;
    native.collaborators = via_serde(&workspace.collaborators)?;
    Ok(native)
}

pub(crate) fn portable_tree(
    tree: &tilo_desktop_model::DesktopTree,
) -> Result<qos_desktop::WorkspaceTree, String> {
    Ok(qos_desktop::WorkspaceTree {
        schema_version: tree.schema_version,
        room_epoch: tree.room_epoch,
        external_revision: tree.external_revision,
        workspaces: tree
            .workspaces
            .iter()
            .map(portable_workspace)
            .collect::<Result<_, _>>()?,
        active: qos_desktop::WorkspaceId(tree.active.0),
        active_strip: tree
            .active_strip
            .iter()
            .map(|(workspace, strip)| {
                (
                    qos_desktop::WorkspaceId(workspace.0),
                    qos_desktop::StripId(strip.0),
                )
            })
            .collect(),
        active_group: tree
            .active_group
            .iter()
            .map(|(strip, group)| (qos_desktop::StripId(strip.0), *group))
            .collect(),
        collaborators: via_serde(&tree.collaborators)?,
        room_records_held: tree
            .room_records_held
            .iter()
            .map(|(room, records)| (portable_room(*room), records.clone()))
            .collect(),
        room_records_inbound: tree
            .room_records_inbound
            .iter()
            .map(|(room, id, kind)| (portable_room(*room), id.clone(), kind.clone()))
            .collect(),
        peer_salt_override: tree.peer_salt_override,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_snapshot_keeps_live_collaborators_and_ids() {
        let mut workspace =
            tilo_desktop_model::Workspace::new(tilo_desktop_model::WorkspaceId(7), "Atlas");
        let room = workspace.room;
        workspace
            .collaborators
            .push(tilo_desktop_model::Collaborator::new("user:a", "A"));
        let tree = tilo_desktop_model::DesktopTree {
            workspaces: vec![workspace],
            active: tilo_desktop_model::WorkspaceId(7),
            ..Default::default()
        };
        let portable = portable_tree(&tree).unwrap();
        assert_eq!(portable.active, qos_desktop::WorkspaceId(7));
        let converted = &portable.workspaces[0];
        assert_eq!(converted.room.as_uuid(), room.as_uuid());
        assert_eq!(converted.collaborators.len(), 1);
        let back = native_workspace(converted).unwrap();
        assert_eq!(back.collaborators, tree.workspaces[0].collaborators);
        assert_eq!(native_room(portable_room(room)), room);
    }
}
