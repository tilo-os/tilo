//! Shared current capability decisions.
use super::{
    Actor, ArtifactKind, CapDecision, CapOverride, Principal, ResourceRef, Role, SystemTime, Verb,
    decide_capability, now_secs,
};

fn document_read_for_actor(
    actor: &Actor,
    caps: &[CapOverride],
    room: &str,
    app: &str,
    record: &str,
    now: SystemTime,
) -> bool {
    let actor = actor
        .clone()
        .with_principal(Principal::App(app.into()))
        .in_room(room.to_owned());
    let resource = ResourceRef::Artifact {
        app: Some(app.into()),
        kind: ArtifactKind::Note,
        instance: collab_types::document_artifact_id(app, room, record),
    };
    decide_capability(&actor, &resource, Verb::Read, caps, Some(now_secs(now)))
        == CapDecision::Allow
}
pub(in crate::state) fn document_read(
    role: Role,
    caps: &[CapOverride],
    person: &str,
    room: &str,
    app: &str,
    record: &str,
    now: SystemTime,
) -> bool {
    document_read_for_actor(
        &Actor::person(Some(role), person),
        caps,
        room,
        app,
        record,
        now,
    )
}
pub(in crate::state) fn agent_document_read(
    caps: &[CapOverride],
    agent: &str,
    room: &str,
    app: &str,
    record: &str,
    now: SystemTime,
) -> bool {
    document_read_for_actor(&Actor::agent(agent), caps, room, app, record, now)
}
pub(in crate::state) fn room(
    role: Role,
    caps: &[CapOverride],
    person: &str,
    room: &str,
    now: SystemTime,
) -> (bool, bool) {
    let actor = Actor::person(Some(role), person).in_room(room.to_owned());
    let resource = ResourceRef::Room(room.to_owned());
    let allowed = |verb| {
        decide_capability(&actor, &resource, verb, caps, Some(now_secs(now))) == CapDecision::Allow
    };
    (allowed(Verb::Read), allowed(Verb::Write))
}
