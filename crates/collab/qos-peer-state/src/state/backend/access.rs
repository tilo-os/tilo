//! Shared current capability decisions. The legacy adapter is unqualified.
use super::*;

pub(in crate::state) fn document_read(
    role: Role,
    caps: &[CapOverride],
    person: &str,
    room: &str,
    app: &str,
    record: &str,
    now: SystemTime,
) -> bool {
    let actor = Actor::person(Some(role), person)
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
/// Source-derived peak for the exact two decision bodies above. Pinned std Vec
/// grows Actor's one-element vec to capacity four when adding App/Room; five
/// Principal slots cover realloc old/new overlap. String copies request their
/// exact byte lengths. A Subject::User comparison creates one 64-byte person
/// string at a time. Other matching branches for these Room/Artifact resources
/// allocate nothing: no filesystem or network resource matching is entered.
/// document_artifact_id streams SHA256 without a concatenation allocation.
pub(in crate::state) fn temporary_bytes(
    person: &str,
    room: &str,
    app: Option<&str>,
) -> Result<usize, automerge::resource_limits::NativeResourceError> {
    use automerge::resource_limits::NativeResourceError;
    let bytes = std::mem::size_of::<Principal>()
        .checked_mul(5)
        .and_then(|n| n.checked_add(person.len()))
        .and_then(|n| n.checked_add(64))
        .and_then(|n| n.checked_add(room.len()))
        .and_then(|n| match app {
            Some(app) => app
                .len()
                .checked_mul(2)
                .and_then(|extra| n.checked_add(extra)),
            None => n.checked_add(room.len()),
        });
    bytes.ok_or(NativeResourceError::WorkBytes)
}
