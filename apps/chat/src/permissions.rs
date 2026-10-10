//! The chat app's permission gate.
//!
//! Sharing a channel and embedding a blob both cross the Tilo permission
//! boundary: a peer only sees a workspace's channels and can only hydrate a
//! content-addressed embed if the OS has granted it access. This module
//! routes those decisions through the toolkit's [`ui::permissions`] model
//! (the `PermissionContext` the compositor hands each app in `draw`), rather
//! than a local allow-all stub.
//!
//! Three concrete gates:
//! * [`ChatPermissions::can_embed`] — may this peer hydrate the bytes behind a
//!   file/image/video attachment? Checked before the image cache requests the
//!   blob **and** before the renderer paints it, so an ungranted embed shows a
//!   locked card and never asks a collaborator for bytes.
//! * [`ChatPermissions::can_participate`] — may this peer send / react / join a
//!   call in the current workspace? (`Write` on the room and Chat app.)
//! * [`ChatPermissions::allows`] — the call-action seam (join / speak / camera
//!   / screen-share), backed by the same context.
//!
//! The default is [`ChatPermissions::permissive`] (the local single-user
//! admin), so the demo, `--shots`, and a normal solo install are unrestricted;
//! a restricted `PermissionContext` (a member role without the relevant grant)
//! gates exactly the actions it should.

use ui::permissions::{PermissionContext, PermissionScope, Verb};

use crate::call::CallAction;

/// Wraps the OS-provided [`PermissionContext`] with the chat-specific checks.
#[derive(Clone, Debug)]
pub struct ChatPermissions {
    ctx: PermissionContext,
    live: Option<ui::framework::PermissionSource>,
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
        Self {
            ctx: PermissionContext::permissive(),
            live: None,
        }
    }

    /// Adopt the context the compositor passed into `draw`.
    pub fn from_context(ctx: PermissionContext) -> Self {
        Self { ctx, live: None }
    }

    /// Follow a mounted resource's permission slot, including installation
    /// after view construction and later host replacements.
    pub(crate) fn follow(&mut self, live: ui::framework::PermissionSource) {
        self.live = Some(live);
    }

    /// Refresh the standalone fallback without dropping a mounted live source.
    pub(crate) fn adopt_context(&mut self, ctx: PermissionContext) {
        self.ctx = ctx;
    }

    fn check(&self, scope: &PermissionScope) -> bool {
        self.live
            .as_ref()
            .and_then(ui::framework::PermissionSource::current)
            .map_or_else(|| self.ctx.check(scope), |current| current.check(scope))
    }

    /// May this peer hydrate + render the bytes behind an attachment `blob`?
    /// A valid content-hash/demo identity plus workspace Read. Attachments belong to
    /// the conversation, not a peer-local filesystem path, so a viewer who
    /// can read the room can hydrate the room's verified shared bytes.
    pub fn can_embed(&self, blob: &str) -> bool {
        let valid_hash = blob.len() == 64 && blob.bytes().all(|byte| byte.is_ascii_hexdigit());
        let valid_demo = matches!(
            blob,
            crate::images::DEMO_CALL_GRID
                | crate::images::DEMO_SHARED_SHOT
                | crate::images::DEMO_NOTES
                | crate::images::DEMO_CLIP
        );
        (valid_hash || valid_demo) && self.check(&PermissionScope::workspace(Verb::Read))
    }

    /// May this peer post / react / publish a call here? `Write` on the room and Chat (a
    /// Viewer can read the channels but not write into them).
    ///
    /// "Here" is the OS's active room, not one of chat's own workspace rows.
    /// This took a `workspace: &str` and discarded it, which read as a
    /// per-workspace check and was not one — callers threaded an id out of
    /// chat's document all the way down to a parameter named with an
    /// underscore. An argument that is carried and dropped is worse than one
    /// that was never asked for: it describes a boundary that does not exist.
    pub fn can_participate(&self) -> bool {
        self.check(&PermissionScope::workspace(Verb::Write))
            && self.check(&PermissionScope::app("so.tilo.chat", Verb::Write))
    }

    /// Join/listen requires Read; publication requires Write. This UI check
    /// complements native authority and local consent, never replaces them.
    /// Leave is always available after a permission downgrade.
    pub fn allows(&self, action: CallAction) -> bool {
        match action {
            CallAction::Leave => true,
            CallAction::Join => self.check(&PermissionScope::workspace(Verb::Read)),
            _ => self.can_participate(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::permissions::{Actor, CapOverride, Effect, Role, Scope, Subject};

    /// A read-only member with no file capabilities — the restricted peer a
    /// real invite without write or file access would produce.
    fn restricted_member() -> ChatPermissions {
        ChatPermissions::from_context(PermissionContext::from_capabilities(
            Actor::person(Some(Role::Viewer), "u_guest"),
            vec![],
        ))
    }

    #[test]
    fn permissions_gate_writes_by_role_but_never_reading_or_leaving() {
        // Permissive admin allows everything.
        let p = ChatPermissions::permissive();
        assert!(p.can_embed(&"a".repeat(64)), "admin: embed");
        assert!(p.can_participate(), "admin: participate");
        assert!(p.allows(CallAction::Join), "admin: join");
        assert!(p.allows(CallAction::ShareScreen), "admin: share screen");

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
            "restricted: room readers may listen"
        );
        assert!(!p.allows(CallAction::Speak), "restricted: speak");
        // …but a View-only member can still read the room, and leaving a call
        // is never blocked.
        assert!(p.allows(CallAction::Leave), "restricted: leave");

        // A member without admin still passes the View check on a workspace
        // (they can read channels) — it's Edit that's gated.
        let p = restricted_member();
        let view_ok = p.ctx.check(&PermissionScope::workspace(Verb::Read));
        assert!(view_ok, "a member can read the workspace");
        assert!(!p.can_participate(), "but cannot write to it");
    }

    #[test]
    fn the_no_access_shot_is_a_reader_the_workspace_granted_no_files() {
        let ctx = crate::app::ChatApp::shot_permission("no-access").expect("staged context");
        let p = ChatPermissions::from_context(ctx);
        assert!(
            !p.can_embed(crate::images::DEMO_CLIP),
            "attachments stay closed"
        );
        assert!(crate::app::ChatApp::shot_permission("call").is_none());
    }

    #[test]
    fn chat_app_read_ceiling_blocks_participation_without_removing_task_or_read_rights() {
        let context = PermissionContext::from_capabilities(
            Actor::person(Some(Role::Editor), "person"),
            vec![CapOverride::new(
                Subject::person("person"),
                Scope::App("so.tilo.chat".into()),
                Effect::Restrict,
                Verb::Read,
            )],
        );
        assert!(context.check(&PermissionScope::workspace(Verb::Write)));
        assert!(context.check(&PermissionScope::app("so.tilo.desktop", Verb::Write)));
        assert!(context.check(&PermissionScope::app("so.tilo.chat", Verb::Read)));
        let permissions = ChatPermissions::from_context(context);
        assert!(!permissions.can_participate());
        assert!(permissions.can_embed(&"a".repeat(64)));
        assert!(permissions.allows(CallAction::Join));
        assert!(permissions.allows(CallAction::Leave));
        assert!(!permissions.allows(CallAction::Speak));
    }

    #[test]
    fn participation_preserves_the_workspace_gate_and_independent_app_scopes() {
        let task_restricted = PermissionContext::from_capabilities(
            Actor::person(Some(Role::Editor), "person"),
            vec![CapOverride::new(
                Subject::person("person"),
                Scope::App("so.tilo.desktop".into()),
                Effect::Restrict,
                Verb::Read,
            )],
        );
        assert!(!task_restricted.check(&PermissionScope::app("so.tilo.desktop", Verb::Write)));
        assert!(ChatPermissions::from_context(task_restricted).can_participate());
        let app_only = PermissionContext::from_capabilities(
            Actor::person(Some(Role::Viewer), "person"),
            vec![CapOverride::grant(
                Subject::person("person"),
                Scope::App("so.tilo.chat".into()),
                Verb::Write,
            )],
        );
        assert!(app_only.check(&PermissionScope::app("so.tilo.chat", Verb::Write)));
        assert!(!ChatPermissions::from_context(app_only).can_participate());
        let editor = PermissionContext::from_capabilities(
            Actor::person(Some(Role::Editor), "person"),
            vec![],
        );
        assert!(ChatPermissions::from_context(editor).can_participate());
    }

    #[test]
    fn permission_source_follows_installation_and_replacement_after_view_construction() {
        let dispatcher =
            ui::framework::Dispatcher::<crate::resource::Chat>::detached("so.tilo.chat", "person");
        let source = dispatcher.permission_source();
        assert!(source.current().is_none());
        let mut permissions = ChatPermissions::permissive();
        permissions.follow(source.clone());
        assert!(permissions.can_participate());
        let mounted = dispatcher.with_permissions(ui::permissions::LivePermissions::fixed(
            PermissionContext::from_capabilities(
                Actor::person(Some(Role::Viewer), "person"),
                vec![],
            ),
        ));
        assert_eq!(
            source.current().and_then(|current| current.role()),
            Some(Role::Viewer)
        );
        assert!(!permissions.can_participate());
        let _replaced = mounted.with_permissions(ui::permissions::LivePermissions::fixed(
            PermissionContext::from_capabilities(
                Actor::person(Some(Role::Editor), "person"),
                vec![],
            ),
        ));
        assert_eq!(
            source.current().and_then(|current| current.role()),
            Some(Role::Editor)
        );
        assert!(permissions.can_participate());
    }
}
