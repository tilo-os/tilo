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
//!   call in the current workspace? (`Write` on the room.)
//! * [`ChatPermissions::allows`] — the call-action seam (join / speak / camera
//!   / screen-share), backed by the same context.
//!
//! The demo and `--shots` run [`ChatPermissions::permissive`] (the local
//! single-user admin). A live Chat starts from [`ChatPermissions::unknown`],
//! which grants nothing, and holds what it may do only once its host has
//! handed it a context: nothing it does before then — scaffolding a
//! workspace, joining the roster, bringing the conversation into the room —
//! can outrun the grant. A restricted `PermissionContext` (a member role
//! without the relevant grant) gates exactly the actions it should.

use ui::permissions::{Actor, PermissionContext, PermissionScope, Verb};

use crate::call::CallAction;

/// Wraps the OS-provided [`PermissionContext`] with the chat-specific checks.
#[derive(Clone, Debug)]
pub struct ChatPermissions {
    ctx: PermissionContext,
    /// Whether `ctx` is a context a host handed this Chat, not the
    /// grant-nothing stand-in it starts from.
    known: bool,
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
            known: true,
        }
    }

    /// Before the host has said what this peer may do: nothing is allowed.
    pub fn unknown() -> Self {
        Self {
            ctx: PermissionContext::from_capabilities(Actor::person(None, ""), Vec::new()),
            known: false,
        }
    }

    /// Adopt the context the compositor passed into `draw`.
    pub fn from_context(ctx: PermissionContext) -> Self {
        Self { ctx, known: true }
    }

    /// Whether a host has handed this Chat its context yet.
    pub fn is_known(&self) -> bool {
        self.known
    }

    /// May this peer hydrate + render the bytes behind an attachment `blob`?
    /// A valid content-hash/demo identity plus workspace Read. Attachments belong to
    /// the conversation, not a peer-local filesystem path, so a viewer who
    /// can read the room can hydrate the room's verified shared bytes.
    pub fn can_embed(&self, blob: &str) -> bool {
        let valid_hash = blob.len() == 64 && blob.bytes().all(|byte| byte.is_ascii_hexdigit());
        let valid_demo = matches!(blob, "tilo-demo://call-grid" | "tilo-demo://shared-shot");
        (valid_hash || valid_demo) && self.ctx.check(&PermissionScope::workspace(Verb::Read))
    }

    /// May this peer post / react / join a call here? `Write` on the room (a
    /// Viewer can read the channels but not write into them).
    ///
    /// "Here" is the OS's active room, not one of chat's own workspace rows.
    /// This took a `workspace: &str` and discarded it, which read as a
    /// per-workspace check and was not one — callers threaded an id out of
    /// chat's document all the way down to a parameter named with an
    /// underscore. An argument that is carried and dropped is worse than one
    /// that was never asked for: it describes a boundary that does not exist.
    pub fn can_participate(&self) -> bool {
        self.ctx.check(&PermissionScope::workspace(Verb::Write))
    }

    /// May this peer vote on a report here? `Comment` on the room — what a
    /// Commenter holds. Setting a report's status stays `Write`.
    pub fn can_comment(&self) -> bool {
        self.ctx.check(&PermissionScope::workspace(Verb::Comment))
    }

    /// Join/listen requires Read; publication requires Write. This UI check
    /// complements native authority and local consent, never replaces them.
    /// Leave is always available after a permission downgrade.
    pub fn allows(&self, action: CallAction) -> bool {
        match action {
            CallAction::Leave => true,
            CallAction::Join => self.ctx.check(&PermissionScope::workspace(Verb::Read)),
            _ => self.can_participate(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::permissions::{Actor, Role};

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
        // A chat nobody has authorised may do nothing.
        let p = ChatPermissions::unknown();
        assert!(!p.is_known(), "unknown: not known");
        assert!(!p.can_participate(), "unknown: participate");
        assert!(!p.can_comment(), "unknown: comment");
        assert!(!p.can_embed(&"a".repeat(64)), "unknown: embed");
        assert!(!p.allows(CallAction::Join), "unknown: join");
        assert!(ChatPermissions::from_context(PermissionContext::permissive()).is_known());

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
}
