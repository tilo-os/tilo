use quick_ui::permissions::{
    AppKind, FileId, Op, PermissionContext, PermissionScope, ResourceId,
    Role, RoleId, RolePermissions, UserId, WorkspaceId, DenyReason,
};

fn viewer_role() -> Role {
    Role {
        id: RoleId("viewer".into()),
        name: "Viewer".into(),
        permissions: RolePermissions {
            app_access: vec![(AppKind::FileExplorer, Op::View)],
            file_ops: vec![],
            workspace_admin: false,
        },
    }
}

fn editor_role() -> Role {
    Role {
        id: RoleId("editor".into()),
        name: "Editor".into(),
        permissions: RolePermissions {
            app_access: vec![
                (AppKind::FileExplorer, Op::Edit),
                (AppKind::Terminal, Op::Edit),
            ],
            file_ops: vec![(FileId("report.md".into()), Op::Edit)],
            workspace_admin: false,
        },
    }
}

#[test]
fn viewer_cannot_edit_terminal() {
    let ctx = PermissionContext::new(
        UserId("alice".into()),
        &[viewer_role(), editor_role()],
        RoleId("viewer".into()),
    );
    let scope = PermissionScope {
        resource: ResourceId::App(AppKind::Terminal),
        op: Op::Edit,
    };
    assert!(!ctx.check(&scope));
    assert!(matches!(
        ctx.check_with_denied_feedback(&scope),
        Some(DenyReason::Role { .. })
    ));
}

#[test]
fn editor_can_edit_file() {
    let ctx = PermissionContext::new(
        UserId("bob".into()),
        &[viewer_role(), editor_role()],
        RoleId("editor".into()),
    );
    let scope = PermissionScope {
        resource: ResourceId::File(FileId("report.md".into())),
        op: Op::Edit,
    };
    assert!(ctx.check(&scope));
    assert_eq!(ctx.check_with_denied_feedback(&scope), None);
}

#[test]
fn workspace_admin_requires_admin_flag() {
    let ctx = PermissionContext::new(
        UserId("bob".into()),
        &[editor_role()],
        RoleId("editor".into()),
    );
    let scope = PermissionScope {
        resource: ResourceId::Workspace(WorkspaceId("ws-42".into())),
        op: Op::Admin,
    };
    assert!(!ctx.check(&scope));
}

#[test]
fn unknown_role_denies_everything() {
    let ctx = PermissionContext::new(
        UserId("x".into()),
        &[viewer_role()],
        RoleId("does-not-exist".into()),
    );
    let scope = PermissionScope {
        resource: ResourceId::App(AppKind::FileExplorer),
        op: Op::View,
    };
    assert!(!ctx.check(&scope));
    assert!(matches!(
        ctx.check_with_denied_feedback(&scope),
        Some(DenyReason::RoleNotAssigned)
    ));
}
