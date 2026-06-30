//! Sample data: a registry that spans every scope and a small set of
//! user-authored roles + members. Used to seed a fresh document on first
//! launch and to populate the `--shots` screenshots.
//!
//! This doubles as living documentation of *how an app registers its
//! permissions* — see [`chat_permissions`] for the shape a real app
//! (Chat) would hand to [`crate::registry::PermissionRegistry::register_all`]
//! at startup, including a per-server [`Scope::DynamicInstance`] set.

use crate::model::{Context, ContextId, Decision, Grant, Member, PermissionSet, Role, Scope};
use crate::registry::{PermissionDef, PermissionRegistry};

/// The system's own permissions: workspace, member, and app-management
/// scopes. The compositor / workspace subsystem registers these.
pub fn system_permissions() -> Vec<PermissionDef> {
    use Decision::{Allow, Deny};
    vec![
        // ----- Workspace -----
        PermissionDef::new(
            "workspace.manage",
            "Manage Workspace",
            "Rename, reconfigure, and dissolve the workspace.",
            Scope::Workspace,
            Deny,
        ),
        PermissionDef::new(
            "workspace.invite",
            "Invite Peers",
            "Generate invites and admit new peers to the workspace.",
            Scope::Workspace,
            Deny,
        ),
        PermissionDef::new(
            "workspace.view_canvas",
            "View Canvas",
            "See the shared 2D cluster canvas and its windows.",
            Scope::Workspace,
            Allow,
        ),
        // ----- Members -----
        PermissionDef::new(
            "member.add",
            "Add Members",
            "Admit a peer as a workspace member.",
            Scope::Member,
            Deny,
        ),
        PermissionDef::new(
            "member.remove",
            "Remove Members",
            "Kick a member out of the workspace.",
            Scope::Member,
            Deny,
        ),
        PermissionDef::new(
            "member.assign_roles",
            "Assign Roles",
            "Grant or revoke roles on other members.",
            Scope::Member,
            Deny,
        ),
        // ----- App management -----
        PermissionDef::new(
            "apps.install",
            "Install Apps",
            "Add a new app to the workspace's app set.",
            Scope::AppManagement,
            Deny,
        ),
        PermissionDef::new(
            "apps.remove",
            "Remove Apps",
            "Uninstall an app from the workspace.",
            Scope::AppManagement,
            Deny,
        ),
        PermissionDef::new(
            "apps.update",
            "Update Apps",
            "Apply available updates to installed apps.",
            Scope::AppManagement,
            Deny,
        ),
    ]
}

/// The Chat app's permissions, as Chat itself would register them. The
/// first group are global app-actions; the second group are *per-server*
/// permissions ([`Scope::DynamicInstance`]) that are only decided
/// relative to a specific server [`Context`].
pub fn chat_permissions() -> Vec<PermissionDef> {
    use Decision::{Allow, Deny};
    let app = "chat";
    let action = Scope::AppAction { app: app.into() };
    let per_server = Scope::DynamicInstance {
        app: app.into(),
        kind: "server".into(),
    };
    vec![
        // Global Chat actions.
        PermissionDef::new(
            "chat.create_server",
            "Create Space",
            "Create a new chat space (server).",
            action.clone(),
            Deny,
        ),
        PermissionDef::new(
            "chat.send_message",
            "Send Messages",
            "Post messages in channels the member can see.",
            action.clone(),
            Allow,
        ),
        PermissionDef::new(
            "chat.start_call",
            "Start Call",
            "Begin a voice or video call.",
            action.clone(),
            Allow,
        ),
        PermissionDef::new(
            "chat.screen_share",
            "Screen Share",
            "Share a screen or window into a call.",
            action,
            Deny,
        ),
        // Per-server actions — decided in the context of one server.
        PermissionDef::new(
            "chat.manage_channels",
            "Manage Channels",
            "Create, rename, and delete channels in this server.",
            per_server.clone(),
            Deny,
        ),
        PermissionDef::new(
            "chat.manage_server",
            "Manage Server",
            "Edit this server's settings, roles, and invites.",
            per_server.clone(),
            Deny,
        ),
        PermissionDef::new(
            "chat.kick_members",
            "Kick Members",
            "Remove members from this server.",
            per_server,
            Deny,
        ),
    ]
}

/// A fully-populated registry spanning every scope. This is what a live
/// system would arrive at after the workspace and each app have
/// registered at startup.
pub fn sample_registry() -> PermissionRegistry {
    let mut reg = PermissionRegistry::new();
    reg.register_all(system_permissions());
    reg.register_all(chat_permissions());
    reg
}

// Stable, recognisable role colors (packed 0xRRGGBB).
const COLOR_ADMIN: u32 = 0xE6_4A_4A; // red
const COLOR_MOD: u32 = 0x4A_8FE6; // blue
const COLOR_MEMBER: u32 = 0x4A_C77A; // green
const COLOR_GUEST: u32 = 0x9A_9A_9A; // grey

/// A small set of user-authored roles, members, and one dynamic context
/// (a Chat server), wired together so the editor and the screenshots
/// open onto a meaningful state. Note: these roles are *authored data* —
/// the product spec forbids baked-in default roles, so this is seed
/// content a user could just as easily have created, not a system
/// fixture the resolver depends on.
pub fn sample_set() -> PermissionSet {
    let mut set = PermissionSet::default();

    let eng = Context::new("chat:server:eng", "chat", "server", "Engineering");
    let eng_id = eng.id.clone();
    set.contexts.push(eng);
    set.contexts.push(Context::new(
        "chat:server:design",
        "chat",
        "server",
        "Design",
    ));

    // Admin — top of the hierarchy, broad allows.
    let mut admin = Role::new("Admin", COLOR_ADMIN, 3);
    for p in [
        "workspace.manage",
        "workspace.invite",
        "member.add",
        "member.remove",
        "member.assign_roles",
        "apps.install",
        "apps.remove",
        "apps.update",
        "chat.create_server",
    ] {
        admin.set_grant(p, None, Grant::Allow);
    }

    // Moderator — member management + per-server channel management,
    // but explicitly cannot manage the workspace.
    let mut moderator = Role::new("Moderator", COLOR_MOD, 2);
    moderator.set_grant("member.remove", None, Grant::Allow);
    moderator.set_grant("workspace.invite", None, Grant::Allow);
    moderator.set_grant("workspace.manage", None, Grant::Deny);
    moderator.set_grant("chat.manage_channels", Some(eng_id.clone()), Grant::Allow);
    moderator.set_grant("chat.kick_members", Some(eng_id.clone()), Grant::Allow);

    // Member — everyday participation.
    let mut member_role = Role::new("Member", COLOR_MEMBER, 1);
    member_role.set_grant("chat.send_message", None, Grant::Allow);
    member_role.set_grant("chat.start_call", None, Grant::Allow);
    member_role.set_grant("workspace.view_canvas", None, Grant::Allow);

    // Guest — read-mostly; explicitly denied from starting calls even
    // though the app default allows it.
    let mut guest = Role::new("Guest", COLOR_GUEST, 0);
    guest.set_grant("chat.start_call", None, Grant::Deny);
    guest.set_grant("chat.send_message", None, Grant::Deny);

    let (admin_id, mod_id, member_id, guest_id) =
        (admin.id, moderator.id, member_role.id, guest.id);
    set.roles.extend([admin, moderator, member_role, guest]);

    // Members holding those roles, with a couple of illustrative
    // overrides. Each member binds its roles onto a REAL qOS identity from
    // the workspace identity model (see [`crate::identity`]) — the ids are
    // the collaboration layer's own canonical peer ids, not invented names.
    // In production the host supplies the live roster
    // (`IdentitySource::from_collaborators`); here the sample identity model
    // stands in so the screenshots show real-identity bindings.
    let idents = crate::identity::IdentitySource::sample_workspace();
    let ident = |id: &str| idents.get(id).cloned().unwrap_or_else(|| crate::identity::Identity::new(id, id));

    let mut me = Member::for_identity(&ident("me"));
    me.role_ids.push(admin_id);

    let mut alice = Member::for_identity(&ident("alice"));
    alice.role_ids.push(mod_id);
    alice.role_ids.push(member_id);
    // A per-server override: Alice may screen-share, but only in eng.
    alice.set_override("chat.screen_share", Some(ContextId::new("chat:server:eng")), Grant::Allow);

    let mut bob = Member::for_identity(&ident("bob"));
    bob.role_ids.push(member_id);
    // A global override pinning a deny on Bob regardless of role.
    bob.set_override("chat.start_call", None, Grant::Deny);

    let mut guest_user = Member::for_identity(&ident("guest"));
    guest_user.role_ids.push(guest_id);

    set.members.extend([me, alice, bob, guest_user]);
    set
}

/// The identity model the sample document is built against — the real
/// `(id, display_name, color)` identities the sample members bind onto.
/// The standalone app + `--shots` use this to resolve member names/colours;
/// a live host swaps in [`crate::identity::IdentitySource::from_collaborators`].
pub fn sample_identities() -> crate::identity::IdentitySource {
    crate::identity::IdentitySource::sample_workspace()
}
