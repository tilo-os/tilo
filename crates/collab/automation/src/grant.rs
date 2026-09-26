//! The authority a shortcut is allowed to exercise.
//!
//! A shortcut's grant is a list of ordinary permission-profile rules, the
//! same `(scope, grant|restrict, verb)` triples a person assigns to a
//! collaborator. It is evaluated here only as a ceiling on the shortcut: the
//! host still decides, from the invoking person's own capabilities, whether
//! that person may take the action at all. The effective authority is the
//! intersection of the two.

use collab_types::{Effect, PermissionProfileRule, ResourceRef, Verb};
use tilo_actions::contract::PermissionRequirement;

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

/// Whether `grant` lets a shortcut invoke `action` on `app_id`.
///
/// Fail-closed: some rule must grant a verb implying the requirement for a
/// scope that covers the action, and no restriction covering it may cap the
/// verb below the requirement.
#[must_use]
pub fn grant_permits(
    grant: &[PermissionProfileRule],
    app_id: &str,
    action: &str,
    requirement: PermissionRequirement,
) -> bool {
    let needed = required_verb(requirement);
    let target = ResourceRef::Action {
        app: app_id.to_string(),
        action: action.to_string(),
    };
    let covers = |rule: &&PermissionProfileRule| rule.scope.matches_resource(&target);
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
