//! What Chat shows a person may do, as the device's dispatch decided it.
//!
//! Chat decides no permission itself: the dispatch decides every action the window publishes and
//! sends the verdicts, which the window's [`PermissionContext`] holds. These read them:
//! * [`ChatPermissions::can_embed`] — may the window show the bytes behind an attachment? Read
//!   of Chat's records, checked before the image cache asks for the blob and before it paints.
//! * [`ChatPermissions::can_participate`] — may the person post, react or speak in a call? A
//!   change of Chat's records.
//! * [`ChatPermissions::can_moderate`] — may they remove other people's messages?
//! * [`ChatPermissions::allows`] — the call actions (join, speak, camera, screen).
//!
//! Until the dispatch said, nothing is offered.

use ui::permissions::{ActionKind, MemberId, PermissionContext, ShownAction};
use ui::tilo_actions::ActionNeed;

use crate::call::CallAction;

/// The app Chat's records are kept under.
const CHAT: &str = "so.tilo.chat";

/// The verdicts the window holds, read for Chat's controls.
#[derive(Clone, Debug)]
pub struct ChatPermissions {
    ctx: PermissionContext,
}

impl Default for ChatPermissions {
    fn default() -> Self {
        Self::from_context(PermissionContext::stranger(&MemberId::person("")))
    }
}

impl ChatPermissions {
    /// Read the verdicts of the context the window acts under this frame.
    pub fn from_context(ctx: PermissionContext) -> Self {
        Self { ctx }
    }

    pub fn context(&self) -> &PermissionContext {
        &self.ctx
    }

    /// May the window show the bytes behind an attachment `blob`? A valid content hash (or a
    /// demo identity) the person may read Chat's records for.
    pub fn can_embed(&self, blob: &str) -> bool {
        let valid_hash = tilo_content::valid_reference(blob);
        let valid_demo = matches!(
            blob,
            crate::images::DEMO_CALL_GRID
                | crate::images::DEMO_SHARED_SHOT
                | crate::images::DEMO_NOTES
                | crate::images::DEMO_CLIP
        );
        (valid_hash || valid_demo)
            && self
                .ctx
                .allows(&ShownAction::of_app(CHAT, ActionKind::Read))
    }

    /// May the person post, react or join a call in the window's workspace?
    pub fn can_participate(&self) -> bool {
        self.ctx
            .allows(&ShownAction::of_app(CHAT, ActionKind::Undoable))
    }

    /// The workspace the window is in: the one its launch names, else this device's own.
    pub fn workspace(&self) -> String {
        self.ctx.host().map_or_else(
            || {
                std::env::var("TILO_WORKSPACE_ID")
                    .unwrap_or_else(|_| ui::application::PERSONAL_WORKSPACE.to_owned())
            },
            |host| host.access().workspace().to_owned(),
        )
    }

    /// May the person remove other people's messages: the dispatch's verdict of moderating a
    /// conversation's messages.
    pub fn can_moderate(&self) -> bool {
        self.ctx.may_on_some(
            CHAT,
            ("chat.workspace", "remove-message"),
            &ActionNeed {
                kind: ActionKind::Undoable,
                field: Some(ui::tilo_actions::MODERATION_FIELD.to_owned()),
                privileged: false,
            },
        )
    }

    /// Joining and listening need reading; speaking and sharing need changing. Leaving is
    /// always open.
    pub fn allows(&self, action: CallAction) -> bool {
        match action {
            CallAction::Leave => true,
            CallAction::Join => self
                .ctx
                .allows(&ShownAction::of_app(CHAT, ActionKind::Read)),
            _ => self.can_participate(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::permissions::Role;

    /// A read-only member with no file capabilities — the restricted peer a
    /// real invite without write or file access would produce.
    fn restricted_member() -> ChatPermissions {
        ChatPermissions::from_context(PermissionContext::member(
            &ui::permissions::MemberId::person("u_guest"),
            Role::Viewer,
        ))
    }

    tilo_test_env::scenario! {
        /// What a role and the workspace's grants let a member do.
        fn roles_and_grants_decide_what_a_member_may_do;

        phase permissions_check_writes_by_role_but_never_reading_or_leaving;
        {
            let closed = ChatPermissions::default();
            assert!(!closed.can_embed(&"a".repeat(64)), "default fails closed: embed");
            assert!(!closed.can_participate(), "default fails closed: participate");
            assert!(!closed.can_moderate(), "default fails closed: moderate");
            assert!(!closed.allows(CallAction::Join), "default fails closed: join");
            assert!(!closed.allows(CallAction::Speak), "default fails closed: speak");
            assert!(closed.allows(CallAction::Leave), "leaving a call is never blocked");

            let p = ChatPermissions::from_context(PermissionContext::member(
                &MemberId::person(crate::model::ME),
                Role::Owner,
            ));
            assert!(p.can_embed(&"a".repeat(64)), "owner: embed");
            assert!(p.can_participate(), "owner: participate");
            assert!(p.allows(CallAction::Join), "owner: join");
            assert!(p.allows(CallAction::ShareScreen), "owner: share screen");

            // Restricted member reads verified embeds but cannot write.
            let p = restricted_member();
            assert!(p.can_embed(&"a".repeat(64)), "restricted: verified embed");
            assert!(
                !p.can_embed("/var/workspace/secret.png"),
                "restricted: raw path embed"
            );
            // No workspace-admin → can't post / react / start a call…
            assert!(!p.can_participate(), "restricted: participate");
            assert!(
                p.allows(CallAction::Join),
                "restricted: workspace readers may listen"
            );
            assert!(!p.allows(CallAction::Speak), "restricted: speak");
            // …but a View-only member can still read the workspace, and leaving a call
            // is never blocked.
            assert!(p.allows(CallAction::Leave), "restricted: leave");

            // A member without admin still passes the View check on a workspace
            // (they can read channels) — it's Edit that's guarded.
            let p = restricted_member();
            let view_ok = p.allows(CallAction::Join);
            assert!(view_ok, "a member can read the workspace");
            assert!(!p.can_participate(), "but cannot write to it");
        }

        phase only_moderators_and_owners_moderate;
        {
            let as_role =
                |role| ChatPermissions::from_context(PermissionContext::member(&ui::permissions::MemberId::person("u_someone"), role));
            assert!(as_role(Role::Moderator).can_moderate());
            assert!(as_role(Role::Owner).can_moderate());
            assert!(!as_role(Role::Editor).can_moderate());
            assert!(!as_role(Role::Viewer).can_moderate());
            assert!(as_role(Role::Moderator).can_participate());
        }

        phase a_reader_the_workspace_granted_no_files_cannot_open_attachments;
        {
            let p = ChatPermissions::from_context(PermissionContext::stranger(&ui::permissions::MemberId::person(crate::model::ME)));
            assert!(
                !p.can_embed(crate::images::DEMO_CLIP),
                "attachments stay closed"
            );
        }
    }
}
