use quick_desktop::shell::state::{Role, RolePermissions, WorkspaceFile, Workspace, WorkspaceId};

#[test]
fn role_permissions_round_trip() {
    let mut ws = Workspace::new(WorkspaceId::new(), "Design");
    ws.roles.push(Role {
        id: "role-1".into(),
        name: "Editor".into(),
        permissions: RolePermissions {
            can_invite: false,
            can_edit_files: true,
            per_app: Default::default(),
            per_file: Default::default(),
        },
    });
    ws.workspace_files.push(WorkspaceFile {
        id: "file-1".into(),
        name: "brief.md".into(),
        mime: "text/markdown".into(),
    });
    assert_eq!(ws.roles[0].name, "Editor");
    assert!(ws.roles[0].permissions.can_edit_files);
    assert!(!ws.roles[0].permissions.can_invite);
    assert_eq!(ws.workspace_files[0].name, "brief.md");
    assert_eq!(ws.workspace_files[0].mime, "text/markdown");
}
