//! The chat app's permission gate.
//!
//! Sharing a channel and embedding a file both cross the qOS permission
//! boundary: a peer only sees a workspace's channels and can only hydrate a
//! `/var/workspace/...` embed if the OS has granted it access. This module
//! routes those decisions through the toolkit's [`ui::permissions`] model
//! (the `PermissionContext` the compositor hands each app in `draw`), rather
//! than a local allow-all stub.
//!
//! Three concrete gates:
//! * [`ChatPermissions::can_embed`] — may this peer hydrate the bytes behind a
//!   file/image/video attachment? Checked before the image cache reads the
//!   path **and** before the renderer paints it, so an ungranted embed shows a
//!   locked card and never touches the file.
//! * [`ChatPermissions::can_participate`] — may this peer send / react / join a
//!   call in the current workspace? (Edit on the workspace resource.)
//! * [`ChatPermissions::allows`] — the call-action seam (join / speak / camera
//!   / screen-share), backed by the same context.
//!
//! The default is [`ChatPermissions::permissive`] (the local single-user
//! admin), so the demo, `--shots`, and a normal solo install are unrestricted;
//! a restricted `PermissionContext` (a member role without the relevant grant)
//! gates exactly the actions it should.

use ui::permissions::{FileId, Op, PermissionContext, ResourceId, PermissionScope, WorkspaceId};

use crate::call::CallAction;

/// Wraps the OS-provided [`PermissionContext`] with the chat-specific checks.
#[derive(Clone, Debug)]
pub struct ChatPermissions {
    ctx: PermissionContext,
}

impl Default for ChatPermissions {
    fn default() -> Self {
        Self::permissive()
    }
}

impl ChatPermissions {
    /// The local single-user admin — grants everything. Used for the demo,
    /// `--shots`, and any session the compositor hasn't handed a restricted
    /// context.
    pub fn permissive() -> Self {
        Self { ctx: PermissionContext::permissive() }
    }

    /// Adopt the context the compositor passed into `draw`.
    pub fn from_context(ctx: PermissionContext) -> Self {
        Self { ctx }
    }

    /// May this peer hydrate + render the bytes behind an attachment at `path`?
    /// A `View` grant on the file resource. The permissive admin allows every
    /// file; a member role only sees files it was explicitly granted.
    pub fn can_embed(&self, path: &str) -> bool {
        self.ctx.check(&PermissionScope {
            resource: ResourceId::File(FileId(path.to_string())),
            op: Op::View,
        })
    }

    /// May this peer post / react / join a call in `workspace`? An `Edit` grant
    /// on the workspace resource (View-only members can read but not write).
    pub fn can_participate(&self, workspace: &str) -> bool {
        self.ctx.check(&PermissionScope {
            resource: ResourceId::Workspace(WorkspaceId(workspace.to_string())),
            op: Op::Edit,
        })
    }

    /// The call-action seam, backed by the real context. Join / Speak / Camera
    /// / ShareScreen all require Edit on the workspace (you can't transmit into
    /// a room you can't write to); Leave is always allowed so a downgraded peer
    /// can always drop out.
    pub fn allows(&self, workspace: &str, action: CallAction) -> bool {
        match action {
            CallAction::Leave => true,
            _ => self.can_participate(workspace),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::permissions::{Role, RoleId, RolePermissions, PermissionAssignment, UserId, WorkspaceSettingsView};

    /// A member role with no file grants and no workspace-admin — the restricted
    /// peer a real invite without file access would produce.
    fn restricted_member() -> ChatPermissions {
        let roles = vec![Role {
            id: RoleId("member".into()),
            name: "Member".into(),
            permissions: RolePermissions {
                app_access: vec![],
                file_ops: vec![],
                workspace_admin: false,
            },
        }];
        let view = WorkspaceSettingsView {
            roles,
            assignments: vec![PermissionAssignment {
                user: UserId("u_guest".into()),
                role: RoleId("member".into()),
            }],
        };
        let ctx = PermissionContext::from_workspace_settings(&view, &UserId("u_guest".into()))
            .expect("assignment resolves");
        ChatPermissions::from_context(ctx)
    }

    #[test]
    fn permissive_admin_allows_everything() {
        let p = ChatPermissions::permissive();
        assert!(p.can_embed("/var/workspace/design/handoff.png"));
        assert!(p.can_participate("ws_home"));
        assert!(p.allows("ws_home", CallAction::Join));
        assert!(p.allows("ws_home", CallAction::ShareScreen));
    }

    #[test]
    fn restricted_member_is_gated_on_embeds_and_writes() {
        let p = restricted_member();
        // No file grant → an embed is gated (bytes never hydrate).
        assert!(!p.can_embed("/var/workspace/secret.png"));
        // No workspace-admin → can't post / react / start a call…
        assert!(!p.can_participate("ws_home"));
        assert!(!p.allows("ws_home", CallAction::Join));
        assert!(!p.allows("ws_home", CallAction::Speak));
        // …but a View-only member can still read the room, and leaving a call
        // is never blocked.
        assert!(p.allows("ws_home", CallAction::Leave));
    }

    #[test]
    fn member_reads_workspace_but_cannot_write() {
        // A member without admin still passes the View check on a workspace
        // (they can read channels) — it's Edit that's gated.
        let p = restricted_member();
        let view_ok = p.ctx.check(&PermissionScope {
            resource: ResourceId::Workspace(WorkspaceId("ws_home".into())),
            op: Op::View,
        });
        assert!(view_ok, "a member can read the workspace");
        assert!(!p.can_participate("ws_home"), "but cannot write to it");
    }
}
