//! Permissions — qOS's OS-wide permission system (CRDT-native, built on
//! the `ui` toolkit).
//!
//! This crate is two things at once:
//!
//! * **A reusable library** ([`model`], [`registry`], [`check`], [`doc`],
//!   [`seam`], [`editor`]) that other apps and subsystems depend on to
//!   *register* the permissions they define, *check* whether a member may
//!   perform an action, and *open* the role/permission editor in-context.
//! * **A standalone app**: the binary in `main.rs` wraps the reusable
//!   [`editor::EditorView`] as its root for review + `--shots`.
//!
//! # The model in one paragraph
//!
//! A workspace has user-authored [`model::Role`]s (no defaults are baked
//! in), [`model::Member`]s who hold zero or more roles, and dynamic
//! [`model::Context`]s (e.g. a Chat server). Apps register
//! [`registry::PermissionDef`]s — each with a stable id, label,
//! description, [`model::Scope`], and default decision — so the grantable
//! set is dynamic. A member's effective permission is resolved by
//! [`check::check`]: member overrides win, then roles by hierarchy, then
//! the registered default; a per-[`model::Context`] grant/override is
//! strictly more specific than a global one. Everything editable lives in
//! the Automerge CRDT via [`doc::PermissionsDoc`] so it syncs across
//! peers. Enforcement is intentionally **not** in this crate — see
//! [`seam`] for the documented contract.
//!
//! # Where to start reading
//!
//! * [`model`] — the data types.
//! * [`check`] — the precedence algorithm (the heart).
//! * [`seam`] — the public API surface other code calls
//!   (`register`, `check`, `open_editor`, and the enforcement notes).

pub mod check;
pub mod doc;
pub mod editor;
pub mod identity;
pub mod model;
pub mod registry;
pub mod sample;
pub mod seam;

pub use check::{Resolution, Source, check, explain};
pub use editor::{EditorTab, EditorView};
pub use identity::{Identity, IdentitySource};
pub use model::{
    Context, ContextId, Decision, Grant, Member, MemberId, Override, PermissionId, PermissionSet,
    Role, RoleGrant, RoleId, Scope,
};
pub use registry::{PermissionDef, PermissionRegistry};
pub use seam::{EditorIntent, open_editor};

use ui::widget::Widget;

/// Build and return the app's root widget tree — the single integration
/// point used by both the live event loop and the `--shots` offscreen
/// renderer. Opens the full editor on its Roles tab.
pub fn app_root() -> Box<dyn Widget> {
    Box::new(EditorView::from_intent(EditorIntent::roles(None)))
}

/// Build the root widget for a named `--shots` scenario. `main.rs`
/// derives the scenario from the output filename so a single binary can
/// produce both `permissions.png` (roles editor) and
/// `permissions-members.png` (member role assignment + overrides).
pub fn app_root_for_scenario(scenario: Scenario) -> Box<dyn Widget> {
    match scenario {
        // Pre-seeded so the screenshot shows the populated editor (the live
        // app seeds lazily on first sync; a static shot needs it up front).
        Scenario::Roles => Box::new(EditorView::seeded_sample(EditorTab::Roles, 0, 0)),
        // The SECOND visible role selected — proves role switching lands on a
        // different role's grid.
        Scenario::RolesSecond => Box::new(EditorView::seeded_sample(EditorTab::Roles, 1, 0)),
        // The member view, with members bound to REAL identities.
        Scenario::Members => Box::new(EditorView::seeded_sample(EditorTab::Members, 0, 1)),
    }
}

/// The screenshot scenarios the binary knows how to render.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Scenario {
    /// The role editor with the allow/deny/inherit grid (default), first role.
    Roles,
    /// The role editor with the SECOND role selected (role-switch proof).
    RolesSecond,
    /// The member view: real-identity members, role assignment + overrides.
    Members,
}

impl Scenario {
    /// Pick a scenario from a `--shots` output path. A filename containing
    /// "member" selects the members view; "second"/"role2" selects the
    /// second-role view; everything else is the first-role view. Keeps the
    /// entry point a single binary.
    pub fn from_shots_path(path: &std::path::Path) -> Scenario {
        let name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_lowercase();
        if name.contains("member") {
            Scenario::Members
        } else if name.contains("second") || name.contains("role2") {
            Scenario::RolesSecond
        } else {
            Scenario::Roles
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn scenario_from_path_picks_members() {
        assert_eq!(
            Scenario::from_shots_path(Path::new("/out/permissions-members.png")),
            Scenario::Members
        );
        assert_eq!(
            Scenario::from_shots_path(Path::new("/out/permissions.png")),
            Scenario::Roles
        );
        assert_eq!(
            Scenario::from_shots_path(Path::new("/out/permissions-second-role.png")),
            Scenario::RolesSecond
        );
    }

    #[test]
    fn app_root_builds() {
        // Construct the root widget for both scenarios — a smoke test that
        // the public entry points wire up.
        let _ = app_root();
        let _ = app_root_for_scenario(Scenario::Members);
    }
}
