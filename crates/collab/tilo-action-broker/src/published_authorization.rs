//! The route record a published operation acts on. The admission itself is
//! `BrokerActor::admit_published_in`.

/// Spelling of the package-primary record (route record `""`).
const PRIMARY_RECORD_ALIAS: &str = "@tilo:package-primary";
const MAX_RECORD_ID_BYTES: usize = 128;

/// The one canonical spelling of a live route record: `""` is the
/// package-primary record, reserved spellings are never literal record ids,
/// and everything else must be bounded, trimmed, printable ASCII.
pub(crate) fn canonical_record(route_record: &str) -> Result<String, String> {
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
    use crate::collab_broker_capnp::{BrokerActor, PublishedAction, PublishedSurface};
    use crate::route::CrdtRoute;
    use crate::test_support::{Workspace, WorkspaceId};
    use tilo_action_policy::{Grant, KindSet, Lifetime, Role, Subject};

    const OWNER: &str = "owner-person";
    const ROOM: &str = "room-7";
    const WRITER: &str = "so.tilo.word-processor";

    /// Admit `action` on `surface` for `person` at `now`: whether it asks.
    fn authorize_person_published(
        workspace: &Workspace,
        now: u64,
        person: &str,
        capped: bool,
        surface: &PublishedSurface,
        action: &PublishedAction,
    ) -> Result<bool, String> {
        BrokerActor::person(person)
            .capped(capped)
            .admit_published_in(workspace, now, None, (surface, action))
            .map(|(asks, _)| asks)
    }

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

    /// One room policy, one action: the broker admitting a member's
    /// invocation and a window checking its own person's admit it the same
    /// way, for every role and every kind of action, a person's own clicks
    /// on their own device and those reaching outside alike.
    #[test]
    fn a_window_admits_its_person_exactly_as_the_broker_does() {
        use tilo_actions::{ActionKind, DoorError};

        // What a broker admission answers, as a window's door answers it.
        #[derive(Debug, PartialEq)]
        enum Answered {
            Runs,
            AsksFirst,
            Refused,
        }
        let contracts = {
            let of = |kind: ActionKind, tag: Option<&str>| {
                let mut contract = tilo_actions::ActionContract {
                    kind,
                    ..tilo_actions::ActionContract::default()
                };
                contract.tags.extend(tag.map(str::to_owned));
                contract
            };
            [
                ("read", of(ActionKind::Read, None)),
                ("edit", of(ActionKind::Undoable, None)),
                ("outside", of(ActionKind::Outside, None)),
                (
                    "own-device",
                    of(ActionKind::Outside, Some(tilo_actions::OWN_DEVICE_SCOPE)),
                ),
                (
                    "attended",
                    of(ActionKind::Outside, Some(tilo_actions::ATTENDED_TAG)),
                ),
            ]
        };
        let member = "member-person";
        for role in Role::ALL {
            let mut w = workspace();
            if role != Role::Owner {
                w.admit(member, role);
            }
            let person = if role == Role::Owner { OWNER } else { member };
            let window = ui::permissions::PermissionContext::from_standing(
                &w.policy.room,
                tilo_action_policy::Standing::person(w.policy.clone(), person),
            )
            .at_unix_time(10);
            for (id, contract) in &contracts {
                let mut published = action(id);
                published.contract = contract.clone();
                let s = surface("rec-1", vec![published.clone()]);
                let broker = match authorize_person_published(&w, 10, person, false, &s, &published)
                {
                    Ok(false) => Answered::Runs,
                    Ok(true) => Answered::AsksFirst,
                    Err(_) => Answered::Refused,
                };
                let resource = tilo_actions::Resource::new(
                    published.resource.as_str(),
                    published.resource_kind.as_str(),
                    "Document",
                );
                let spec = tilo_actions::ActionSpec::operation(
                    *id,
                    *id,
                    tilo_actions::OperationKind::Effect,
                    "parity.run",
                    &(),
                )
                .unwrap()
                .with_contract(contract.clone());
                let answer = window.invocation_answer(WRITER, &resource, &spec, None, None);
                let direct = window.standing().bounds.is_empty();
                let windowed = match ui::permissions::admit_in_window(answer, &spec, direct, false)
                {
                    Ok(()) => Answered::Runs,
                    Err(DoorError::ConfirmationRequired) => Answered::AsksFirst,
                    Err(_) => Answered::Refused,
                };
                assert_eq!(windowed, broker, "{role:?} {id}");
            }
        }
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

    /// An agent the owner's device handed the session reads where reading
    /// is allowed and is asked before an outside effect; an attended action
    /// asks it every time, even where the owner allowed the agent
    /// everything, while the owner pressing it is not asked.
    #[test]
    fn an_agent_is_asked_before_an_attended_action_whatever_it_is_allowed() {
        let mut w = workspace();
        let agent = || {
            BrokerActor::agent("agent:scribe")
                .for_person(OWNER)
                .session_delegated()
        };
        let mut read = action("read-text");
        read.contract.kind = tilo_actions::ActionKind::Read;
        let mut save = action("save");
        save.contract.kind = tilo_actions::ActionKind::Outside;
        let mut choose = read.clone();
        choose.action = "open-file".into();
        choose
            .contract
            .tags
            .push(tilo_actions::ATTENDED_TAG.to_owned());
        let s = surface("rec-1", vec![read.clone(), save.clone(), choose.clone()]);
        let asks = |w: &Workspace, action: &PublishedAction| {
            agent()
                .admit_published_in(w, 10, None, (&s, action))
                .map(|(asks, _)| asks)
        };
        assert_eq!(asks(&w, &read), Ok(false));
        assert_eq!(asks(&w, &save), Ok(true), "an outside effect asks first");
        assert_eq!(asks(&w, &choose), Ok(true));
        w.grant(Grant::new(
            "scribe-everything",
            Subject::agent("scribe"),
            w.node().app(WRITER),
            KindSet::ALL,
            tilo_action_policy::Answer::Allow,
            Subject::person(OWNER),
        ));
        assert_eq!(asks(&w, &choose), Ok(true), "no grant lifts it");
        assert_eq!(
            authorize_person_published(&w, 10, OWNER, false, &s, &choose),
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
