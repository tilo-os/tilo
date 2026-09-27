//! The role cap a workspace places on foreign devices: Tilo runtimes on an
//! operating system Tilo does not control, marked as such in their device
//! certificate.
//!
//! The cap is one ordinary workspace capability, so it replicates and is
//! governed like every other: [`foreign_device_cap`] with id
//! [`FOREIGN_DEVICE_CAP_ID`]. Its subject is a principal no actor presents,
//! so plain capability decisions never match it and peers that predate this
//! module leave it inert. Without it the cap is [`FOREIGN_DEVICE_CEILING`];
//! with it the cap can only be lower — never Owner, whatever it says.
//!
//! A foreign device's effective role is `min(person's role, cap)`
//! ([`foreign_device_role`]), and its capability decisions additionally see
//! [`foreign_device_restriction`], so a person-level grant cannot lift the
//! device past the cap either.

use crate::caps::{CapOverride, Effect, Principal, Scope, Subject, Verb, role_caps};
use crate::role::Role;

/// Stable id of the workspace capability holding the cap.
pub const FOREIGN_DEVICE_CAP_ID: &str = "foreign-device-cap";

/// Default and upper bound of the cap: Editor. A foreign OS can read the
/// device's keys and forge its input, so it may edit content but never
/// govern the workspace.
pub const FOREIGN_DEVICE_CEILING: Verb = Verb::Write;

const FOREIGN_DEVICE_SUBJECT: &str = "tilo:foreign-device";

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
    fn the_stored_cap_matches_no_actor_on_its_own() {
        let caps = [foreign_device_cap(Verb::Read)];
        let actor = Actor::person(Some(Role::Editor), "a1");
        assert_eq!(
            decide_capability(&actor, &ResourceRef::Workspace, Verb::Write, &caps, Some(0)),
            CapDecision::Allow
        );
    }
}
