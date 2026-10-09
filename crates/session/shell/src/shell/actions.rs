//! In-app actions, and the permission check that gates them.
//!
//! Apps export **resources** (a note, an ebook, a file) and **actions** on
//! them (create an annotation, turn a page, seek a video). The rule this
//! layer exists to enforce: **showing** a resource needs no permission,
//! **taking an action** on one does — so there is deliberately no "render
//! check" here. The affordance disappears for a peer without the verb, not
//! the content.
//!
//! Separately, **confirmation** is an OS-drawn "are you sure?" step for
//! actions that declare themselves destructive. It is not a grant: a user
//! confirming a delete gains no authority they didn't already hold, and a
//! *denied* action is never confirmed — [`ActionOutcome`] keeps the two
//! strictly ordered.

use crate::shell::state::WorkspaceAuthority as _;
use std::collections::HashMap;

use crate::shell::state::{Actor, CapDecision, ResourceRef, Verb, Workspace};

/// One action an app exports: what verb it needs and whether performing
/// it destroys something. Declaring an action names the verb the
/// *capability model* will be asked about — it grants no authority itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionDecl {
    /// The app exporting the action, by stable app id.
    pub app: String,
    /// The action's id, unique within the app.
    pub action: String,
    /// The verb invoking it requires.
    pub verb: Verb,
    /// Whether it warrants an OS-drawn confirmation step. Advisory to the
    /// permission answer, load-bearing for the confirmation one.
    pub destructive: bool,
}

impl ActionDecl {
    /// A non-destructive action requiring `verb`.
    pub fn new(app: impl Into<String>, action: impl Into<String>, verb: Verb) -> Self {
        Self {
            app: app.into(),
            action: action.into(),
            verb,
            destructive: false,
        }
    }

    /// Mark the action destructive, so invoking it is confirmed.
    #[must_use]
    pub fn destructive(mut self) -> Self {
        self.destructive = true;
        self
    }
}

/// What an app has declared it can do. Fail-closed by construction: an
/// action not in the registry cannot be invoked, so an app cannot smuggle
/// one past the check by simply not declaring it.
#[derive(Clone, Debug, Default)]
pub struct ActionRegistry {
    decls: HashMap<(String, String), ActionDecl>,
}

impl ActionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record an app's declaration. Re-declaring replaces.
    pub fn declare(&mut self, decl: ActionDecl) {
        self.decls
            .insert((decl.app.clone(), decl.action.clone()), decl);
    }

    /// Forget everything `app` declared — called on exit, so a dead app's
    /// actions can't be invoked by a peer whose message was in flight.
    pub fn forget_app(&mut self, app: &str) {
        self.decls.retain(|(a, _), _| a != app);
    }

    pub fn get(&self, app: &str, action: &str) -> Option<&ActionDecl> {
        self.decls.get(&(app.to_string(), action.to_string()))
    }

    /// Every action `app` exports.
    pub fn actions_of(&self, app: &str) -> Vec<&ActionDecl> {
        self.decls.values().filter(|d| d.app == app).collect()
    }
}

/// Why an action was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DenyReason {
    /// The app never declared this action. Fail-closed default.
    UndeclaredAction,
    /// The capability model refused it.
    Capability(CapDecision),
}

/// `NeedsConfirmation` is *permitted*: the capability check passed, and the
/// OS should draw its confirmation step. Kept separate from
/// [`ActionOutcome::Allow`] so callers can't treat "must confirm" as granted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionOutcome {
    /// Proceed.
    Allow,
    /// Permitted, but destructive: confirm with the user first.
    NeedsConfirmation,
    /// Refused.
    Denied(DenyReason),
}

impl ActionOutcome {
    /// Did the check pass? True for both `Allow` and `NeedsConfirmation`.
    pub fn is_permitted(self) -> bool {
        !matches!(self, ActionOutcome::Denied(_))
    }
}

/// The single entry point for in-app action permission. Fail-closed: an
/// undeclared action is refused, and a declared one unless the capability
/// model positively permits the verb. `resource` is what the capability's
/// scope is matched against, so `Write` on one document doesn't reach
/// its neighbour.
pub fn check_action(
    workspace: &Workspace,
    registry: &ActionRegistry,
    actor: &Actor,
    app: &str,
    action: &str,
    resource: &ResourceRef,
) -> ActionOutcome {
    let Some(decl) = registry.get(app, action) else {
        return ActionOutcome::Denied(DenyReason::UndeclaredAction);
    };
    match workspace.decide(actor, resource, decl.verb) {
        CapDecision::Allow if decl.destructive => ActionOutcome::NeedsConfirmation,
        CapDecision::Allow => ActionOutcome::Allow,
        other => ActionOutcome::Denied(DenyReason::Capability(other)),
    }
}

/// What chrome should paint from: a peer who lacks the verb is not offered
/// the affordance at all. Asking for more is a *request* to the owner,
/// handled out of band.
pub fn permitted_actions<'r>(
    workspace: &Workspace,
    registry: &'r ActionRegistry,
    actor: &Actor,
    app: &str,
    resource: &ResourceRef,
) -> Vec<&'r ActionDecl> {
    registry
        .actions_of(app)
        .into_iter()
        .filter(|d| {
            check_action(workspace, registry, actor, app, &d.action, resource).is_permitted()
        })
        .collect()
}

#[allow(
    unused_imports,
    reason = "The native WorkspaceExt methods are selected by host-specific workspace code and test configurations."
)]
use crate::shell::state::WorkspaceExt as _;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::state::FreshId as _;
    use crate::shell::state::{CapOverride, Principal, Scope, Subject, WorkspaceId};

    fn room() -> Workspace {
        let mut ws = Workspace::new(WorkspaceId::new(), "Reading");
        ws.set_owner("alice");
        ws
    }

    fn registry() -> ActionRegistry {
        let mut r = ActionRegistry::new();
        r.declare(ActionDecl::new("org.tilo.Ebook", "turn-page", Verb::Read));
        r.declare(ActionDecl::new("org.tilo.Ebook", "annotate", Verb::Write));
        r.declare(
            ActionDecl::new("org.tilo.Ebook", "delete-annotation", Verb::Write).destructive(),
        );
        r
    }

    fn book() -> ResourceRef {
        ResourceRef::Action {
            app: "org.tilo.Ebook".into(),
            resource: "ebook.book:dune".into(),
            action: "annotate".into(),
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn declared_actions_are_granted_only_by_role_and_confirmed_when_destructive() {
        fn an_undeclared_action_is_refused_even_for_the_owner() {
            let ws = room();
            let reg = registry();
            let owner = ws.actor_for_person(&"alice".to_string());
            assert_eq!(
                check_action(&ws, &reg, &owner, "org.tilo.Ebook", "rm-rf", &book()),
                ActionOutcome::Denied(DenyReason::UndeclaredAction),
                "an app must not be able to smuggle an operation past the check by \
                 declining to declare it"
            );
        }

        fn a_viewer_may_turn_a_page_but_not_annotate() {
            let mut ws = room();
            ws.grant_capability(CapOverride::membership(
                Subject::person("bob"),
                ws.id.raw().to_string(),
                Verb::Read,
            ));
            let reg = registry();
            let bob = ws.actor_for_person(&"bob".to_string());
            let target = ResourceRef::App("org.tilo.Ebook".into());

            assert_eq!(
                check_action(&ws, &reg, &bob, "org.tilo.Ebook", "turn-page", &target),
                ActionOutcome::Allow,
                "a read-class action is available to a Viewer"
            );
            assert_eq!(
                check_action(&ws, &reg, &bob, "org.tilo.Ebook", "annotate", &target),
                ActionOutcome::Denied(DenyReason::Capability(CapDecision::DeniedByRole)),
                "a write-class action must be denied to a Viewer"
            );

            // And chrome must not even offer it.
            let offered: Vec<&str> = permitted_actions(&ws, &reg, &bob, "org.tilo.Ebook", &target)
                .into_iter()
                .map(|d| d.action.as_str())
                .collect();
            assert!(offered.contains(&"turn-page"));
            assert!(
                !offered.contains(&"annotate"),
                "an action the peer cannot take must not be offered — no greyed-out \
                 button, no modal mid-edit"
            );
            assert!(!offered.contains(&"delete-annotation"));
        }

        fn a_destructive_action_is_confirmed_not_granted() {
            let mut ws = room();
            ws.grant_capability(CapOverride::membership(
                Subject::person("bob"),
                ws.id.raw().to_string(),
                Verb::Write,
            ));
            let reg = registry();
            let bob = ws.actor_for_person(&"bob".to_string());
            let target = ResourceRef::App("org.tilo.Ebook".into());

            assert_eq!(
                check_action(&ws, &reg, &bob, "org.tilo.Ebook", "annotate", &target),
                ActionOutcome::Allow
            );
            assert_eq!(
                check_action(
                    &ws,
                    &reg,
                    &bob,
                    "org.tilo.Ebook",
                    "delete-annotation",
                    &target
                ),
                ActionOutcome::NeedsConfirmation,
                "a destructive action an actor IS permitted still gets a confirmation step"
            );

            // Confirmation is not a grant: no Write means denial, not a prompt.
            let mut ws2 = room();
            ws2.grant_capability(CapOverride::membership(
                Subject::person("carol"),
                ws2.id.raw().to_string(),
                Verb::Read,
            ));
            let carol = ws2.actor_for_person(&"carol".to_string());
            assert_eq!(
                check_action(
                    &ws2,
                    &reg,
                    &carol,
                    "org.tilo.Ebook",
                    "delete-annotation",
                    &target
                ),
                ActionOutcome::Denied(DenyReason::Capability(CapDecision::DeniedByRole)),
                "a destructive action must be DENIED, not merely confirmed, when the \
                 actor lacks the verb"
            );
        }

        fn an_action_capability_is_scoped_to_its_resource() {
            let mut ws = room();
            // Read-only admission, then write on exactly one action.
            ws.grant_capability(CapOverride::membership(
                Subject::person("bob"),
                ws.id.raw().to_string(),
                Verb::Read,
            ));
            ws.grant_capability(CapOverride::grant(
                Subject::person("bob"),
                Scope::Action {
                    app: "org.tilo.Ebook".into(),
                    resource: "ebook.book:dune".into(),
                    action: "annotate".into(),
                },
                Verb::Write,
            ));
            let reg = registry();
            let bob = ws.actor_for_person(&"bob".to_string());

            assert_eq!(
                check_action(&ws, &reg, &bob, "org.tilo.Ebook", "annotate", &book()),
                ActionOutcome::Allow,
                "the granted action is invokable"
            );
            assert_eq!(
                check_action(
                    &ws,
                    &reg,
                    &bob,
                    "org.tilo.Ebook",
                    "delete-annotation",
                    &ResourceRef::Action {
                        app: "org.tilo.Ebook".into(),
                        resource: "ebook.book:dune".into(),
                        action: "delete-annotation".into(),
                    }
                ),
                ActionOutcome::Denied(DenyReason::Capability(CapDecision::DeniedByRole)),
                "a capability on one action must not reach a sibling action"
            );
            assert_eq!(
                check_action(
                    &ws,
                    &reg,
                    &bob,
                    "org.tilo.Ebook",
                    "annotate",
                    &ResourceRef::Action {
                        app: "org.tilo.Ebook".into(),
                        resource: "ebook.book:emma".into(),
                        action: "annotate".into(),
                    }
                ),
                ActionOutcome::Denied(DenyReason::Capability(CapDecision::DeniedByRole)),
                "a capability on one book's action must not reach the same action on another"
            );
        }

        an_undeclared_action_is_refused_even_for_the_owner();
        a_viewer_may_turn_a_page_but_not_annotate();
        a_destructive_action_is_confirmed_not_granted();
        an_action_capability_is_scoped_to_its_resource();
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn action_authority_is_never_inherited_and_ends_with_revocation() {
        fn an_app_acting_on_its_own_never_inherits_a_persons_authority() {
            let mut ws = room();
            ws.grant_capability(CapOverride::membership(
                Subject::person("bob"),
                ws.id.raw().to_string(),
                Verb::Admin,
            ));
            let reg = registry();
            let app = ws.actor_for_app("org.tilo.Ebook");
            let target = ResourceRef::App("org.tilo.Ebook".into());

            assert_eq!(
                check_action(&ws, &reg, &app, "org.tilo.Ebook", "annotate", &target),
                ActionOutcome::Denied(DenyReason::Capability(CapDecision::DeniedByRole)),
                "an app holds no role, so a room full of admins grants it nothing"
            );

            // Granting the app itself is what authorises it.
            let mut ws = ws;
            ws.grant_capability(CapOverride::grant(
                Subject::app("org.tilo.Ebook"),
                Scope::App("org.tilo.Ebook".into()),
                Verb::Write,
            ));
            assert_eq!(
                check_action(
                    &ws,
                    &reg,
                    &ws.actor_for_app("org.tilo.Ebook"),
                    "org.tilo.Ebook",
                    "annotate",
                    &target
                ),
                ActionOutcome::Allow
            );
        }

        fn forgetting_an_app_disables_its_actions() {
            let mut ws = room();
            ws.grant_capability(CapOverride::membership(
                Subject::person("bob"),
                ws.id.raw().to_string(),
                Verb::Write,
            ));
            let mut reg = registry();
            let bob = ws.actor_for_person(&"bob".to_string());
            let target = ResourceRef::App("org.tilo.Ebook".into());

            assert_eq!(
                check_action(&ws, &reg, &bob, "org.tilo.Ebook", "annotate", &target),
                ActionOutcome::Allow
            );
            reg.forget_app("org.tilo.Ebook");
            assert_eq!(
                check_action(&ws, &reg, &bob, "org.tilo.Ebook", "annotate", &target),
                ActionOutcome::Denied(DenyReason::UndeclaredAction),
                "an action of an app that has exited must not remain invokable"
            );
        }

        fn a_revoked_capability_stops_offering_the_action() {
            let mut ws = room();
            let id = ws.grant_capability(
                CapOverride::membership(
                    Subject::person("bob"),
                    ws.id.raw().to_string(),
                    Verb::Write,
                )
                .with_id("bob-write"),
            );
            assert_eq!(id, "bob-write");
            let reg = registry();
            let target = ResourceRef::App("org.tilo.Ebook".into());

            assert_eq!(
                check_action(
                    &ws,
                    &reg,
                    &ws.actor_for_person(&"bob".to_string()),
                    "org.tilo.Ebook",
                    "annotate",
                    &target
                ),
                ActionOutcome::Allow
            );

            assert!(ws.revoke_capability("bob-write"));
            assert_eq!(
                check_action(
                    &ws,
                    &reg,
                    &ws.actor_for_person(&"bob".to_string()),
                    "org.tilo.Ebook",
                    "annotate",
                    &target
                ),
                ActionOutcome::Denied(DenyReason::Capability(CapDecision::DeniedByRole)),
                "revoking takes effect on the next check, with no restart"
            );
            assert!(
                permitted_actions(
                    &ws,
                    &reg,
                    &ws.actor_for_person(&"bob".to_string()),
                    "org.tilo.Ebook",
                    &target
                )
                .iter()
                .all(|d| d.verb == Verb::Read),
                "and chrome stops offering the write-class actions immediately"
            );
        }

        fn the_subject_of_a_share_is_never_a_room_member() {
            let mut ws = room(); // a shared resource, never room membership
            ws.grant_capability(CapOverride::grant(
                Subject::person("dave"),
                Scope::Action {
                    app: "org.tilo.Ebook".into(),
                    resource: "ebook.book:dune".into(),
                    action: "annotate".into(),
                },
                Verb::Write,
            ));
            let reg = registry();
            let dave = ws.actor_for_person(&"dave".to_string());

            assert_eq!(
                check_action(&ws, &reg, &dave, "org.tilo.Ebook", "annotate", &book()),
                ActionOutcome::Allow,
                "the share works"
            );
            assert_eq!(ws.role_of(&"dave".to_string()), None, "but confers no role");
            assert!(
                !ws.roster().iter().any(|(u, _)| u == "dave"),
                "and never puts Dave in the roster"
            );
            assert!(
                ws.shares_of(&book())
                    .iter()
                    .any(|c| c.subject == Subject::Principal(Principal::Person("dave".into()))),
                "he belongs on the resource's share list instead"
            );
        }

        an_app_acting_on_its_own_never_inherits_a_persons_authority();
        forgetting_an_app_disables_its_actions();
        a_revoked_capability_stops_offering_the_action();
        the_subject_of_a_share_is_never_a_room_member();
    }
}
