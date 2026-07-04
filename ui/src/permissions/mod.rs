use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum AppKind {
    Terminal,
    FileExplorer,
    Browser,
    Settings,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct UserId(pub String);

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct RoleId(pub String);

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct FileId(pub String);

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct WorkspaceId(pub String);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum Op {
    View,
    Edit,
    Admin,
}

impl Op {
    /// Admin implies Edit implies View.
    pub fn satisfies(self, required: Op) -> bool {
        use Op::*;
        matches!(
            (self, required),
            (Admin, _) | (Edit, View) | (Edit, Edit) | (View, View)
        )
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ResourceId {
    App(AppKind),
    File(FileId),
    Workspace(WorkspaceId),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PermissionScope {
    pub resource: ResourceId,
    pub op: Op,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RolePermissions {
    pub app_access: Vec<(AppKind, Op)>,
    pub file_ops: Vec<(FileId, Op)>,
    pub workspace_admin: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Role {
    pub id: RoleId,
    pub name: String,
    pub permissions: RolePermissions,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DenyReason {
    RoleNotAssigned,
    Role { role: RoleId, required: Op },
}

#[derive(Clone, Debug)]
pub struct PermissionContext {
    user: UserId,
    assigned: RoleId,
    role: Option<Role>,
}

impl PermissionContext {
    pub fn new(user: UserId, roles: &[Role], assignment: RoleId) -> Self {
        let role = roles.iter().find(|r| r.id == assignment).cloned();
        Self { user, assigned: assignment, role }
    }

    pub fn permissive() -> Self {
        Self {
            user: UserId("local".into()),
            assigned: RoleId("admin".into()),
            role: Some(Role {
                id: RoleId("admin".into()),
                name: "Local Admin".into(),
                permissions: RolePermissions {
                    app_access: vec![],
                    file_ops: vec![],
                    workspace_admin: true,
                },
            }),
        }
    }

    pub fn user(&self) -> &UserId { &self.user }
    pub fn role_id(&self) -> &RoleId { &self.assigned }

    pub fn check(&self, scope: &PermissionScope) -> bool {
        self.check_with_denied_feedback(scope).is_none()
    }

    pub fn from_workspace_settings(
        view: &WorkspaceSettingsView,
        user: &UserId,
    ) -> Option<Self> {
        let assignment = view
            .assignments
            .iter()
            .find(|a| &a.user == user)?
            .role
            .clone();
        Some(Self::new(user.clone(), &view.roles, assignment))
    }

    pub fn check_with_denied_feedback(&self, scope: &PermissionScope) -> Option<DenyReason> {
        let Some(role) = self.role.as_ref() else {
            return Some(DenyReason::RoleNotAssigned);
        };
        // The permissive role grants everything.
        if role.permissions.workspace_admin && matches!(scope.op, Op::Admin | Op::Edit | Op::View)
            && matches!(scope.resource, ResourceId::Workspace(_) | ResourceId::App(_) | ResourceId::File(_))
            && role.id.0 == "admin"
        {
            return None;
        }
        match &scope.resource {
            ResourceId::App(kind) => {
                if role
                    .permissions
                    .app_access
                    .iter()
                    .any(|(k, granted)| k == kind && granted.satisfies(scope.op))
                {
                    None
                } else {
                    Some(DenyReason::Role { role: role.id.clone(), required: scope.op })
                }
            }
            ResourceId::File(fid) => {
                if role
                    .permissions
                    .file_ops
                    .iter()
                    .any(|(f, granted)| f == fid && granted.satisfies(scope.op))
                {
                    None
                } else {
                    Some(DenyReason::Role { role: role.id.clone(), required: scope.op })
                }
            }
            ResourceId::Workspace(_) => {
                if role.permissions.workspace_admin || scope.op == Op::View {
                    None
                } else {
                    Some(DenyReason::Role { role: role.id.clone(), required: scope.op })
                }
            }
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PermissionAssignment {
    pub user: UserId,
    pub role: RoleId,
}

/// Read-only view of the `WorkspaceSettings` modal's persisted state
/// used to build a `PermissionContext`. The shell owns the mutable
/// counterpart; this is the contract the UI writes into.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct WorkspaceSettingsView {
    pub roles: Vec<Role>,
    pub assignments: Vec<PermissionAssignment>,
}
