//! Whether a person may invoke one live published operation: the current
//! workspace policy must allow the action *and* the document artifact of the
//! route record it acts on, so a person restricted on one record cannot act
//! on it through an action they may otherwise use.
use collab_types::caps::{CapDecision, ResourceRef, Verb};

use crate::collab_broker_capnp::{PublishedAction, PublishedSurface};
use tilo_desktop_model::Workspace;

/// Spelling of the package-primary record (route record `""`).
const PRIMARY_RECORD_ALIAS: &str = "@tilo:package-primary";
const MAX_RECORD_ID_BYTES: usize = 128;

/// Authorize `person` (from a device held to the foreign-device cap when
/// `capped`) for `action` on `surface` at policy time `now`.
///
/// # Errors
///
/// Returns the refusal text when the route record is reserved or malformed,
/// or when the policy denies the action or the record.
pub fn authorize_person_published(
    workspace: &Workspace,
    now: u64,
    person: &str,
    capped: bool,
    surface: &PublishedSurface,
    action: &PublishedAction,
) -> Result<(), String> {
    let record = &surface.route.record_id;
    canonical_record(record)?;
    let Some(verb) = verb(action.contract.permission) else {
        return Ok(());
    };
    let actor = workspace.actor_for_person_on_device_at(&person.to_string(), capped, now);
    let objects = [
        ResourceRef::Action {
            app: surface.app_id.clone(),
            resource: action.resource.clone(),
            action: action.action.clone(),
        },
        ResourceRef::Artifact {
            app: Some(surface.app_id.clone()),
            kind: collab_types::ArtifactKind::Note,
            instance: collab_types::document_artifact_id(
                &surface.app_id,
                &surface.route.cluster_id,
                record,
            ),
        },
    ];
    if objects
        .iter()
        .all(|object| workspace.decide_at(&actor, object, verb, now) == CapDecision::Allow)
    {
        Ok(())
    } else {
        Err("not authorized: caller no longer authorized for invocation".into())
    }
}

/// The one canonical spelling of a live route record: `""` is the
/// package-primary record, reserved spellings are never literal record ids,
/// and everything else must be bounded, trimmed, printable ASCII.
fn canonical_record(route_record: &str) -> Result<String, String> {
    if route_record.is_empty() {
        return Ok(PRIMARY_RECORD_ALIAS.into());
    }
    let reserved = route_record.starts_with("@tilo:") || route_record.starts_with('*');
    if reserved
        || route_record.len() > MAX_RECORD_ID_BYTES
        || !route_record.is_ascii()
        || route_record.trim() != route_record
        || route_record.bytes().any(|b| b.is_ascii_control())
    {
        return Err("reserved or invalid route record".into());
    }
    Ok(route_record.into())
}

fn verb(permission: tilo_actions::PermissionRequirement) -> Option<Verb> {
    use tilo_actions::PermissionRequirement as P;
    match permission {
        P::None => None,
        P::Read => Some(Verb::Read),
        P::Comment => Some(Verb::Comment),
        P::Write => Some(Verb::Write),
        P::Invite => Some(Verb::Invite),
        P::Admin => Some(Verb::Admin),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::route::CrdtRoute;
    use crate::test_support::{Workspace, WorkspaceId};
    use collab_types::caps::{CapOverride, Effect, Scope, Subject};

    const OWNER: &str = "owner-person";
    const ROOM: &str = "room-7";
    const WRITER: &str = "so.tilo.word-processor";

    fn workspace() -> Workspace {
        let mut w = Workspace::new(WorkspaceId(7), "Room");
        w.set_owner(OWNER.to_string());
        w
    }
    fn surface(record: &str, actions: Vec<PublishedAction>) -> PublishedSurface {
        PublishedSurface {
            peer_id: 9,
            app_id: WRITER.into(),
            route: CrdtRoute::record(WRITER, ROOM, record),
            actions,
            live: true,
        }
    }
    fn action(action: &str) -> PublishedAction {
        PublishedAction {
            semantic_identity: None,
            resource: "word-processor.document".into(),
            action: action.into(),
            resource_kind: "word-processor.document".into(),
            resource_label: "Document".into(),
            action_label: action.into(),
            description: None,
            operation_kind: tilo_actions::OperationKind::Document,
            params: vec![],
            contract: tilo_actions::ActionContract::default(),
            view: None,
            input: crate::collab_broker_capnp::PublishedInput::None,
        }
    }

    #[test]
    fn a_capped_device_holds_the_person_to_the_foreign_device_cap() {
        let w = workspace();
        let mut rename = action("rename");
        rename.contract.permission = tilo_actions::PermissionRequirement::Admin;
        let typing = action("type-text");
        let s = surface("rec-1", vec![rename.clone(), typing.clone()]);
        assert!(authorize_person_published(&w, 10, OWNER, false, &s, &rename).is_ok());
        assert!(authorize_person_published(&w, 10, OWNER, true, &s, &rename).is_err());
        assert!(authorize_person_published(&w, 10, OWNER, true, &s, &typing).is_ok());
    }

    #[test]
    fn a_person_is_checked_on_the_route_record_too() {
        let mut w = workspace();
        let member = "member-person";
        w.grant_capability(CapOverride::membership(
            Subject::person(member),
            w.room.to_string(),
            Verb::Write,
        ));
        let typing = action("type-text");
        let s = surface("rec-1", vec![typing.clone()]);
        assert!(authorize_person_published(&w, 10, member, false, &s, &typing).is_ok());
        w.grant_capability(CapOverride::new(
            Subject::person(member),
            Scope::ResourceInstance(collab_types::document_artifact_id(WRITER, ROOM, "rec-1")),
            Effect::Restrict,
            Verb::Read,
        ));
        assert!(authorize_person_published(&w, 10, member, false, &s, &typing).is_err());
        let s2 = surface("rec-2", vec![typing.clone()]);
        assert!(authorize_person_published(&w, 10, member, false, &s2, &typing).is_ok());
    }

    #[test]
    fn a_reserved_or_malformed_route_record_is_refused() {
        let w = workspace();
        let typing = action("type-text");
        assert!(
            authorize_person_published(&w, 10, OWNER, false, &surface("", vec![]), &typing).is_ok()
        );
        for record in [
            "@tilo:package-primary",
            "*",
            " padded",
            "é",
            &"x".repeat(129),
        ] {
            assert_eq!(
                authorize_person_published(&w, 10, OWNER, false, &surface(record, vec![]), &typing),
                Err("reserved or invalid route record".into()),
                "{record:?}"
            );
        }
    }

    #[test]
    fn a_stranger_is_refused() {
        let w = workspace();
        let s = surface("rec-1", vec![]);
        assert!(
            authorize_person_published(&w, 10, "stranger", false, &s, &action("type-text"))
                .is_err()
        );
    }
}
