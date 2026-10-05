//! The authority a shortcut is allowed to exercise.
//!
//! A shortcut's grant is a list of ordinary permission-profile rules, the
//! same `(scope, grant|restrict, verb)` triples a person assigns to a
//! collaborator. It is evaluated here only as a ceiling on the shortcut: the
//! host still decides, from the invoking person's own capabilities, whether
//! that person may take the action at all. The effective authority is the
//! intersection of the two.

use collab_types::{Effect, PermissionProfileRule, ResourceRef, Scope, Verb};
use tilo_actions::contract::PermissionRequirement;
use tilo_actions::door::ActionAddress;

/// The capability verb an action's declared requirement corresponds to.
/// An action that needs no permission still has to be named by the grant,
/// at the weakest verb.
#[must_use]
pub const fn required_verb(requirement: PermissionRequirement) -> Verb {
    match requirement {
        PermissionRequirement::None | PermissionRequirement::Read => Verb::Read,
        PermissionRequirement::Comment => Verb::Comment,
        PermissionRequirement::Write => Verb::Write,
        PermissionRequirement::Invite => Verb::Invite,
        PermissionRequirement::Admin => Verb::Admin,
    }
}

/// Whether `grant` lets a shortcut invoke the action at `address`, on a
/// resource of kind `resource_kind`, on `app_id`. A rule naming the same
/// action on another resource, or on resources of another kind (a sub-kind
/// included), does not.
///
/// Fail-closed: some rule must grant a verb implying the requirement for a
/// scope that covers the action, and no restriction covering it may cap the
/// verb below the requirement.
#[must_use]
pub fn grant_permits(
    grant: &[PermissionProfileRule],
    app_id: &str,
    address: &ActionAddress,
    resource_kind: &str,
    requirement: PermissionRequirement,
) -> bool {
    let needed = required_verb(requirement);
    let target = ResourceRef::Action {
        app: app_id.to_string(),
        resource: address.resource.clone(),
        action: address.action.clone(),
    };
    let covers = |rule: &&PermissionProfileRule| match &rule.scope {
        Scope::ActionOnKind { app, kind, action } => {
            app == app_id && kind == resource_kind && *action == address.action
        }
        scope => scope.matches_resource(&target),
    };
    let granted = grant
        .iter()
        .filter(covers)
        .any(|rule| rule.effect == Effect::Grant && rule.verb.implies(needed));
    let restricted = grant
        .iter()
        .filter(covers)
        .any(|rule| rule.effect == Effect::Restrict && !rule.verb.implies(needed));
    granted && !restricted
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOTES: &str = "so.tilo.notes";

    fn on(kind: &str) -> PermissionProfileRule {
        PermissionProfileRule::grant(
            Scope::ActionOnKind {
                app: NOTES.into(),
                kind: kind.into(),
                action: "apply".into(),
            },
            Verb::Write,
        )
    }

    fn permits(grant: &[PermissionProfileRule], app: &str, resource: &str, kind: &str) -> bool {
        grant_permits(
            grant,
            app,
            &ActionAddress::new(resource, "apply"),
            kind,
            PermissionRequirement::Write,
        )
    }

    #[test]
    fn a_shortcut_grant_reaches_its_resource_or_exactly_its_kind() {
        // One resource's action: not the same action on another resource.
        let exact = [PermissionProfileRule::grant(
            Scope::Action {
                app: NOTES.into(),
                resource: "notes.notebook/page/1/body".into(),
                action: "apply".into(),
            },
            Verb::Write,
        )];
        assert!(permits(
            &exact,
            NOTES,
            "notes.notebook/page/1/body",
            "tilo.rich-text"
        ));
        assert!(!permits(&exact, NOTES, "notes.notebook", "notes.notebook"));
        assert!(!permits(
            &exact,
            NOTES,
            "notes.notebook/page/2/body",
            "tilo.rich-text"
        ));

        // Every resource of one kind: that kind exactly, in that app, for that
        // action. Neither a sub-kind nor a kind sharing a prefix.
        let kind = [on("notes.page")];
        assert!(permits(&kind, NOTES, "notes.page:1", "notes.page"));
        assert!(permits(&kind, NOTES, "notes.page:2", "notes.page"));
        assert!(!permits(
            &kind,
            NOTES,
            "notes.page:1/x",
            "notes.page.special"
        ));
        assert!(!permits(&kind, NOTES, "notes.pages:1", "notes.pages"));
        assert!(!permits(&kind, NOTES, "notes.notebook", "notes.notebook"));
        assert!(!permits(
            &kind,
            "so.tilo.chat",
            "notes.page:1",
            "notes.page"
        ));
        assert!(!grant_permits(
            &kind,
            NOTES,
            &ActionAddress::new("notes.page:1", "delete"),
            "notes.page",
            PermissionRequirement::Write,
        ));

        // A restriction on the kind caps it.
        let restricted = [
            on("notes.page"),
            PermissionProfileRule {
                effect: Effect::Restrict,
                verb: Verb::Read,
                ..on("notes.page")
            },
        ];
        assert!(!permits(&restricted, NOTES, "notes.page:1", "notes.page"));
    }
}
