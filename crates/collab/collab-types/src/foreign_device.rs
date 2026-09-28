//! The role cap a workspace places on foreign devices: Tilo runtimes on an
//! operating system Tilo does not control, marked as such in their device
//! certificate. The cap applies only in rooms the device joins; a room the
//! device governs itself is its own ([`cap_applies`]).
//!
//! The cap is one ordinary workspace capability, so it replicates and is
//! governed like every other: [`foreign_device_cap`] with id
//! [`FOREIGN_DEVICE_CAP_ID`]. Its subject is a principal only actors marked
//! by [`on_foreign_device`] present, so it never matches anyone else and
//! peers that predate this module leave it inert. Without it the cap is
//! [`FOREIGN_DEVICE_CEILING`]; with it the cap can only be lower — never
//! Owner, whatever it says.
//!
//! A foreign device's effective role is `min(person's role, cap)`
//! ([`foreign_device_role`]). So that a person-level grant cannot lift the
//! device past the cap either, a decision either sees the device's actor
//! marked by [`on_foreign_device`], or — where callers build the actor from
//! a person id — the capabilities extended with [`foreign_device_restriction`].

use crate::caps::{Actor, CapOverride, Effect, Principal, Scope, Subject, Verb, role_caps};
use crate::role::Role;

/// Stable id of the workspace capability holding the cap.
pub const FOREIGN_DEVICE_CAP_ID: &str = "foreign-device-cap";

/// Default and upper bound of the cap: Editor. A foreign OS can read the
/// device's keys and forge its input, so it may edit content but never
/// govern the workspace.
pub const FOREIGN_DEVICE_CEILING: Verb = Verb::Write;

const FOREIGN_DEVICE_SUBJECT: &str = "tilo:foreign-device";

/// Whether a device is held to the cap: it is foreign and does not govern
/// the room itself.
pub fn cap_applies(foreign: bool, governs_room: bool) -> bool {
    foreign && !governs_room
}

/// The workspace capability capping foreign devices at `verb`. A verb above
/// [`FOREIGN_DEVICE_CEILING`] leaves the ceiling in force.
pub fn foreign_device_cap(verb: Verb) -> CapOverride {
    CapOverride::new(
        Subject::token(FOREIGN_DEVICE_SUBJECT),
        Scope::Workspace,
        Effect::Restrict,
        verb,
    )
    .with_id(FOREIGN_DEVICE_CAP_ID)
}

/// The role a foreign device of a person holding `role` acts with.
pub fn foreign_device_role(role: Role, caps: &[CapOverride], now: u64) -> Role {
    let cap = cap_verb(caps, now);
    match role_caps(role) {
        Some(verb) if !cap.implies(verb) => role_for(cap),
        _ => role,
    }
}

/// `actor` acting from a foreign device: its role capped, and marked so that
/// every capability decision holds it to the cap.
pub fn on_foreign_device(actor: Actor, caps: &[CapOverride], now: u64) -> Actor {
    Actor {
        role: actor.role.map(|role| foreign_device_role(role, caps, now)),
        ..actor
    }
    .with_principal(Principal::Token(FOREIGN_DEVICE_SUBJECT.into()))
}

/// Whether `actor` is marked foreign and `required` exceeds the ceiling. The
/// workspace's own, lower cap matches the mark as an ordinary restriction.
pub(crate) fn exceeds_ceiling(actor: &Actor, required: Verb) -> bool {
    !FOREIGN_DEVICE_CEILING.implies(required)
        && actor
            .principals
            .iter()
            .any(|principal| *principal == Principal::Token(FOREIGN_DEVICE_SUBJECT.into()))
}

/// The restriction a foreign device of `person` is decided under, added to
/// the workspace capabilities for that device's decisions only.
pub fn foreign_device_restriction(person: &str, caps: &[CapOverride], now: u64) -> CapOverride {
    CapOverride::new(
        Subject::person(person),
        Scope::Workspace,
        Effect::Restrict,
        cap_verb(caps, now),
    )
}

fn cap_verb(caps: &[CapOverride], now: u64) -> Verb {
    let subject = Subject::Principal(Principal::Token(FOREIGN_DEVICE_SUBJECT.into()));
    caps.iter()
        .filter(|cap| {
            cap.subject == subject
                && cap.scope == Scope::Workspace
                && cap.effect == Effect::Restrict
                && cap.is_live_at(Some(now))
                && FOREIGN_DEVICE_CEILING.implies(cap.verb)
        })
        .map(|cap| cap.verb)
        .fold(FOREIGN_DEVICE_CEILING, Verb::min)
}

fn role_for(verb: Verb) -> Role {
    match verb {
        Verb::Admin => Role::Owner,
        Verb::Write => Role::Editor,
        Verb::Comment => Role::Commenter,
        Verb::Read | Verb::Invite => Role::Viewer,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::caps::{Actor, CapDecision, ResourceRef, decide_capability};

    #[test]
    fn foreign_devices_default_to_editor_and_never_govern() {
        assert_eq!(foreign_device_role(Role::Owner, &[], 0), Role::Editor);
        assert_eq!(foreign_device_role(Role::Editor, &[], 0), Role::Editor);
        assert_eq!(foreign_device_role(Role::Viewer, &[], 0), Role::Viewer);
        assert_eq!(foreign_device_role(Role::Denied, &[], 0), Role::Denied);

        let raised = [foreign_device_cap(Verb::Admin)];
        assert_eq!(foreign_device_role(Role::Owner, &raised, 0), Role::Editor);
    }

    #[test]
    fn workspace_governance_lowers_the_cap_while_it_is_live() {
        let caps = [foreign_device_cap(Verb::Comment)];
        assert_eq!(foreign_device_role(Role::Owner, &caps, 0), Role::Commenter);
        assert_eq!(foreign_device_role(Role::Viewer, &caps, 0), Role::Viewer);

        let expired = [foreign_device_cap(Verb::Read).with_expiry(10)];
        assert_eq!(foreign_device_role(Role::Owner, &expired, 5), Role::Viewer);
        assert_eq!(foreign_device_role(Role::Owner, &expired, 10), Role::Editor);
    }

    #[test]
    fn a_person_grant_cannot_lift_a_foreign_device_past_the_cap() {
        let person = "a1";
        let mut caps = vec![CapOverride::grant(
            Subject::person(person),
            Scope::Workspace,
            Verb::Admin,
        )];
        let actor = Actor::person(Some(Role::Editor), person);
        let admin = |caps: &[CapOverride]| {
            decide_capability(&actor, &ResourceRef::Workspace, Verb::Admin, caps, Some(0))
        };
        assert_eq!(admin(&caps), CapDecision::Allow);

        caps.push(foreign_device_restriction(person, &caps, 0));
        assert_eq!(admin(&caps), CapDecision::DeniedByRestriction);
        assert_eq!(
            decide_capability(&actor, &ResourceRef::Workspace, Verb::Write, &caps, Some(0)),
            CapDecision::Allow
        );
    }

    #[test]
    fn a_marked_actor_is_held_to_the_cap_whatever_it_is_granted() {
        let person = "a1";
        let mut caps = vec![CapOverride::grant(
            Subject::person(person),
            Scope::Workspace,
            Verb::Admin,
        )];
        let owner = Actor::person(Some(Role::Owner), person);
        let decide = |actor: &Actor, caps: &[CapOverride], verb| {
            decide_capability(actor, &ResourceRef::Workspace, verb, caps, Some(0))
        };

        let foreign = on_foreign_device(owner.clone(), &caps, 0);
        assert_eq!(foreign.role, Some(Role::Editor));
        assert_eq!(decide(&owner, &caps, Verb::Admin), CapDecision::Allow);
        assert_eq!(
            decide(&foreign, &caps, Verb::Admin),
            CapDecision::DeniedByRestriction
        );
        assert_eq!(
            decide(&foreign, &caps, Verb::Invite),
            CapDecision::DeniedByRestriction
        );
        assert_eq!(decide(&foreign, &caps, Verb::Write), CapDecision::Allow);

        caps.push(foreign_device_cap(Verb::Comment));
        let foreign = on_foreign_device(owner, &caps, 0);
        assert_eq!(foreign.role, Some(Role::Commenter));
        assert_eq!(
            decide(&foreign, &caps, Verb::Write),
            CapDecision::DeniedByRestriction
        );
        assert_eq!(decide(&foreign, &caps, Verb::Comment), CapDecision::Allow);
    }

    #[test]
    fn the_stored_cap_matches_no_actor_on_its_own() {
        let caps = [foreign_device_cap(Verb::Read)];
        let actor = Actor::person(Some(Role::Editor), "a1");
        assert_eq!(
            decide_capability(&actor, &ResourceRef::Workspace, Verb::Write, &caps, Some(0)),
            CapDecision::Allow
        );
    }
}
