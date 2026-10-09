//! Whether a person may invoke one live published operation: the current
//! workspace policy must allow the action *and* the document artifact of the
//! route record it acts on, so a person restricted on one record cannot act
//! on it through an action they may otherwise use.
use tilo_action_policy::{Answer, roles};
use tilo_desktop_model::Workspace;

use crate::collab_broker_capnp::{PublishedAction, PublishedSurface};

/// Spelling of the package-primary record (route record `""`).
const PRIMARY_RECORD_ALIAS: &str = "@tilo:package-primary";
const MAX_RECORD_ID_BYTES: usize = 128;

/// Whether `answer` lets an action be taken without anyone being asked:
/// allowed; or asking first when the person acts themselves (`direct`),
/// their own gesture being the answer. Someone acting for them, or a device
/// not theirs, gets past an ask only with the person's confirmation, which
/// admission requires (see [`authorize_person_published`]).
#[must_use]
pub fn permits(answer: Answer, direct: bool) -> bool {
    match answer {
        Answer::Allow => true,
        Answer::Ask => direct,
        Answer::Deny => false,
    }
}

/// Authorize `person` (from a device held to the foreign-device bound when
/// `capped`) for `action` on `surface` at policy time `now`: the room's
/// grants must allow the action where it is published *and* a change of the
/// route record it acts on. `Ok(true)` when the grants ask first: the
/// invocation then runs only on the person's confirmation in the host.
///
/// # Errors
///
/// Returns the refusal text when the route record is reserved or malformed,
/// or when the grants refuse the action or the record.
pub fn authorize_person_published(
    workspace: &Workspace,
    now: u64,
    person: &str,
    capped: bool,
    surface: &PublishedSurface,
    action: &PublishedAction,
) -> Result<bool, String> {
    let record = canonical_record(&surface.route.record_id)?;
    let bound = roles::foreign_device_bound();
    let authority = workspace.person_authority_on(person, capped, &bound);
    let cx = tilo_desktop_model::context_at(now);
    let kind = action.contract.kind;
    let nodes = [
        tilo_actions::action_node(
            &workspace.node(),
            &surface.app_id,
            &action.resource_kind,
            &action.resource,
            action.contract.field.as_deref(),
            &action.action,
        ),
        collab_types::validator::document_node(&workspace.policy.room, &surface.app_id, &record),
    ];
    let answers: Vec<Answer> = nodes
        .iter()
        .map(|node| authority.decide(node, kind, &cx).answer)
        .collect();
    if answers.contains(&Answer::Deny) {
        Err("not authorized: caller no longer authorized for invocation".into())
    } else {
        Ok(answers.contains(&Answer::Ask))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::route::CrdtRoute;
    use crate::test_support::{Workspace, WorkspaceId};
    use tilo_action_policy::{Grant, KindSet, Lifetime, Role, Subject};

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
    fn a_capped_device_holds_the_person_to_the_foreign_device_bound() {
        let w = workspace();
        let mut send = action("send");
        send.contract.kind = tilo_actions::ActionKind::Outside;
        let typing = action("type-text");
        let s = surface("rec-1", vec![send.clone(), typing.clone()]);
        assert_eq!(
            authorize_person_published(&w, 10, OWNER, false, &s, &send),
            Ok(false)
        );
        assert_eq!(
            authorize_person_published(&w, 10, OWNER, true, &s, &send),
            Ok(true),
            "reaching outside asks first on a device that is not theirs"
        );
        assert_eq!(
            authorize_person_published(&w, 10, OWNER, true, &s, &typing),
            Ok(false)
        );
    }

    #[test]
    fn an_editor_reaching_outside_is_asked_and_the_owner_is_not() {
        let mut w = workspace();
        let member = "member-person";
        w.admit(member, Role::Editor);
        let mut send = action("send");
        send.contract.kind = tilo_actions::ActionKind::Outside;
        let typing = action("type-text");
        let s = surface("rec-1", vec![send.clone(), typing.clone()]);
        assert_eq!(
            authorize_person_published(&w, 10, member, false, &s, &send),
            Ok(true),
            "an editor's outside effect runs only on their confirmation"
        );
        assert_eq!(
            authorize_person_published(&w, 10, member, false, &s, &typing),
            Ok(false)
        );
        assert_eq!(
            authorize_person_published(&w, 10, OWNER, false, &s, &send),
            Ok(false)
        );
    }

    #[test]
    fn a_person_is_checked_on_the_route_record_too() {
        let mut w = workspace();
        let member = "member-person";
        w.admit(member, Role::Editor);
        let typing = action("type-text");
        let s = surface("rec-1", vec![typing.clone()]);
        assert!(authorize_person_published(&w, 10, member, false, &s, &typing).is_ok());
        w.grant(Grant::new(
            "rec-1-read-only",
            Subject::person(member),
            collab_types::validator::document_node(&w.policy.room, WRITER, "rec-1"),
            KindSet::UNDOABLE,
            tilo_action_policy::Answer::Deny,
            Subject::person(OWNER),
        ));
        assert!(authorize_person_published(&w, 10, member, false, &s, &typing).is_err());
        let s2 = surface("rec-2", vec![typing.clone()]);
        assert!(authorize_person_published(&w, 10, member, false, &s2, &typing).is_ok());
    }

    #[test]
    fn a_grant_lapses_at_the_policy_time_it_is_decided_at() {
        let mut w = workspace();
        let member = "member-person";
        w.admit(member, Role::Viewer);
        w.grant(
            Grant::new(
                "app-for-a-while",
                Subject::person(member),
                w.node().app(WRITER),
                KindSet::UNDOABLE,
                tilo_action_policy::Answer::Allow,
                Subject::person(OWNER),
            )
            .with_lifetime(Lifetime::Until(100_000)),
        );
        let typing = action("type-text");
        let s = surface("rec-1", vec![typing.clone()]);
        assert!(authorize_person_published(&w, 50, member, false, &s, &typing).is_ok());
        assert!(authorize_person_published(&w, 100, member, false, &s, &typing).is_err());
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
