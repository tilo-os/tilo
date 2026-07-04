//! The integration seam: how other apps and subsystems wire into the
//! permission system — registration, checking, opening the editor, and
//! (documented, not implemented) enforcement.
//!
//! This module is the public contract. The model/registry/check modules
//! are the mechanism; this module is the set of named entry points the
//! rest of qOS calls. Nothing here performs enforcement — see
//! [`mod@self`]'s "Enforcement" section for who owns that and where.
//!
//! # 1. Registration (startup)
//!
//! Each subsystem registers the permissions it defines once, at process
//! start, into a shared [`PermissionRegistry`]:
//!
//! ```
//! use permissions::registry::PermissionRegistry;
//! use permissions::sample;
//!
//! let mut registry = PermissionRegistry::new();
//! // The workspace/compositor contributes the system permissions:
//! registry.register_all(sample::system_permissions());
//! // Each app contributes its own on connect/launch:
//! registry.register_all(sample::chat_permissions());
//! ```
//!
//! In production the compositor owns the canonical registry and apps
//! contribute via the existing app↔compositor socket (the same channel
//! [`ui::collaboration`] already uses); `register_all` is the in-process
//! shape of that contribution. An app's `RegistrationManifest`
//! (`app_key` + its `Vec<PermissionDef>`) is the serializable payload.
//!
//! # 2. Checking (action sites)
//!
//! Any code about to perform a guarded action asks [`crate::check::check`]:
//!
//! ```
//! use permissions::{check::check, model::ContextId, sample};
//!
//! let registry = sample::sample_registry();
//! let set = sample::sample_set();
//! let member = &set.members[0];
//! let server = ContextId::new("chat:server:eng");
//! if check(&registry, &set, member, "chat.manage_channels", Some(&server)).allowed() {
//!     // proceed
//! }
//! ```
//!
//! The check is pure and cheap; callers do not need the editor.
//!
//! # 3. Opening the editor (in-context)
//!
//! Apps open the permission surface for a specific target via an
//! [`EditorIntent`] — a small route type. Chat's "Server settings →
//! Roles" passes [`EditorIntent::context`]; Settings hosts the workspace
//! view with [`EditorIntent::roles`]. The host turns an intent into a
//! widget with [`open_editor`].
//!
//! # 3a. Members are real identities (the identity seam)
//!
//! A [`Member`](crate::model::Member) is **not** an invented name — it is a
//! role binding placed on a real qOS identity. Members store the stable
//! identity id used everywhere else in qOS (the collaboration layer's
//! [`ui::permissions::UserId`] / [`ui::collaboration::Collaborator::id`],
//! anchored by the compositor's persisted ed25519 `PeerId`). Display name +
//! colour are resolved through a [`crate::identity::IdentitySource`] so they
//! track the live identity instead of a frozen copy.
//!
//! The single join point is [`crate::identity::IdentitySource`]:
//!
//! ```ignore
//! // Production: adapt the live roster delivered by CollabEvent::Context.
//! let source = permissions::IdentitySource::from_collaborators(&collaborators);
//! let view = permissions::open_editor(intent).with_identities(source);
//! // …and push updates as peers join/leave:
//! view.update_identities(permissions::IdentitySource::from_collaborators(&roster));
//! ```
//!
//! Headless / first-run / `--shots` use
//! [`IdentitySource::sample_workspace`](crate::identity::IdentitySource::sample_workspace)
//! — a *real* identity model whose ids are the collaboration layer's own
//! canonical peer ids with [`ui::presence::user_color`] colours, never
//! fabricated strings. The compositor owns the canonical roster (it brokers
//! the app↔compositor collab socket); when it is available the host builds
//! the source via `from_collaborators`, otherwise the sample source stands
//! in. Either way the editor only ever reads `(id, display_name, color)`.
//!
//! # 4. Enforcement (out of scope here — documented only)
//!
//! This crate deliberately never *enforces*. Enforcement lives where the
//! action lives:
//!
//! * **Workspace / Member / App-management** permissions are enforced by
//!   the **compositor** (`shell`), which already owns the per-cluster
//!   sandbox and the membership/app lifecycle. It resolves the acting
//!   peer to a [`Member`], calls [`crate::check::check`] at the action
//!   site (invite, kick, install), and refuses on `Deny`.
//! * **App-action** and **per-instance** permissions are enforced by the
//!   **owning app** (e.g. Chat refuses to create a channel when
//!   `chat.manage_channels` denies for the acting member in that
//!   server's context). The app gets the resolved decision the same way:
//!   it holds the synced [`PermissionSet`] (CRDT) plus the registry.
//! * The existing [`ui::permissions::PermissionContext`] is the
//!   *compositor-facing* coarse gate (app-access / workspace-admin) used
//!   for greying out widgets. This crate is the *authoring + resolution*
//!   layer that feeds it: a future adapter projects a member's resolved
//!   decisions into a `PermissionContext` so existing
//!   `permission_scope()`-gated widgets light up correctly. That adapter
//!   is the single wiring point and is intentionally left as a TODO so
//!   this pass does not fake enforcement.
//!
//! The covenant: **register at startup, check at the action site, enforce
//! in the subsystem that owns the action.** This crate supplies the first
//! two and the model behind the third.

use crate::editor::{EditorView, EditorTab};
use crate::model::{ContextId, MemberId, RoleId};

/// Where to open the permission/settings surface. A small, serializable
/// route type so an app can request a deep-link ("open roles for *this*
/// server") without depending on the editor's internals. This is also
/// the seam other system surfaces (e.g. sound settings) reuse: a host
/// resolves an intent to a hosted view.
#[derive(Clone, PartialEq, Debug)]
pub enum EditorIntent {
    /// The roles tab, optionally pre-selecting a role to edit.
    Roles { select: Option<RoleId> },
    /// The members tab, optionally pre-selecting a member.
    Members { select: Option<MemberId> },
    /// Scope the whole editor to a single dynamic context (a Chat
    /// server): the editor opens on that context's per-instance
    /// permissions. This is what "Server settings → Roles" passes.
    Context {
        context: ContextId,
        tab: EditorTab,
    },
}

impl EditorIntent {
    /// Open the roles editor (optionally pre-selecting a role).
    pub fn roles(select: Option<RoleId>) -> Self {
        EditorIntent::Roles { select }
    }
    /// Open the members editor (optionally pre-selecting a member).
    pub fn members(select: Option<MemberId>) -> Self {
        EditorIntent::Members { select }
    }
    /// Open the editor scoped to a dynamic context instance.
    pub fn context(context: ContextId, tab: EditorTab) -> Self {
        EditorIntent::Context { context, tab }
    }
}

/// The single entry point a host (Settings, Chat, the compositor's
/// settings surface) calls to embed the permission editor. Returns a
/// boxed [`EditorView`] ready to drop into any `ui` widget tree or hand
/// to `ui::App`. The same function backs both the standalone app and
/// the in-context surfaces, so they can never drift.
pub fn open_editor(intent: EditorIntent) -> Box<EditorView> {
    Box::new(EditorView::from_intent(intent))
}
