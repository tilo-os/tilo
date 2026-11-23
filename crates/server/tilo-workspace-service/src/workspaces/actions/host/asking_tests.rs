//! What stands for a person on the server: they alone read it and take it
//! back, taking back holds at once, it covers exactly what was asked, and no
//! request writes one, whoever sends it.

use serde_json::{Value, json};
use tilo_actions::remote::person_asking::{
    self, ActionName, ActionTarget, AgentName, Asked, AskedBy, Thing, Waiting, WaitingAnswer, Words,
};
use tilo_actions::remote::{
    ActionCallError, ActionProvider as _, Caller, InvokeArgument, InvokeRequest, ReceiptStatus,
    RemoteActionTarget,
};
use tilo_actions::{ActionAddress, InvocationId};
use tilo_server_protocol::WorkspaceId;
use tilo_user_identity::DeviceCert;

use super::super::ServiceWorkspaceActions;
use super::super::tests::{admit, agent_caller, caller, governed, settled};
use crate::workspaces::Workspaces;
use crate::workspaces::person_asking::{Allowing, MAX_PERMISSIONS};
use crate::workspaces::tests::{governor, test_host};

/// The agent the permissions here are for, by its id in the workspace.
const AGENT: &str = "scout";

/// `target` asked by `caller`, on the device `device` certifies, in the
/// workspace `workspace_id` carries.
async fn ask_at(
    holder: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    (device, caller): (&DeviceCert, &Caller),
    target: RemoteActionTarget,
    argument: Value,
) -> Result<Value, ActionCallError> {
    let argument = match argument {
        Value::Object(map) if !map.is_empty() => InvokeArgument::Json(map),
        _ => InvokeArgument::None,
    };
    let request = InvokeRequest {
        invocation: InvocationId::new(),
        target,
        argument,
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    let receipt = settled(
        holder
            .in_workspace_as(workspace_id, device)
            .invoke(caller, request),
    )
    .await?;
    match receipt.status {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or(Value::Null)),
        other => panic!("what stands for a person is answered at once: {other:?}"),
    }
}

/// `action` of what stands, asked as [`ask_at`] asks.
async fn ask(
    holder: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    who: (&DeviceCert, &Caller),
    action: &str,
    argument: Value,
) -> Result<Value, ActionCallError> {
    ask_at(
        holder,
        workspace_id,
        who,
        person_asking::target(person_asking::PERMISSIONS, action),
        argument,
    )
    .await
}

/// Sending from the box `on` names in `workspace`, or from any box.
fn sending(workspace: &str, agent: &str, on: Option<&str>) -> Asked {
    Asked {
        workspace: workspace.to_owned(),
        agent: agent.to_owned(),
        action: ActionName {
            provider: "app.example".into(),
            kind: "letters.box".into(),
            action: "send".into(),
        },
        on: on.map(|resource| Thing {
            document: String::new(),
            resource: resource.to_owned(),
        }),
    }
}

/// What the server lets stand when it reads the person's own answer `always`
/// to its question about `scope`.
fn answered_always(workspaces: &Workspaces, person: &[u8; 32], scope: &Asked) -> String {
    workspaces
        .let_always(
            person,
            Allowing {
                scope: scope.clone(),
                name: "Scout".into(),
                words: Words {
                    action: "Send a letter".into(),
                    on: scope
                        .on
                        .as_ref()
                        .map(|_| "Outbox".into())
                        .unwrap_or_default(),
                    place: "Team".into(),
                },
                asked_by: Some(AskedBy {
                    workspace: scope.workspace.clone(),
                    conversation: "c_general".into(),
                    message: "m_question".into(),
                }),
            },
        )
        .expect("it stands")
        .id
}

/// What `listed` says of the one permission `id` that stands in `workspace`:
/// everything a client shows of it. The permission.
fn the_one_listed(listed: &Value, id: &str, workspace: &str) -> Value {
    let permissions = listed["permissions"].as_array().unwrap();
    assert_eq!(permissions.len(), 1, "{listed}");
    let one = &permissions[0];
    assert_eq!(one["id"], json!(id));
    assert_eq!(one["source"], json!("asking"));
    assert_eq!(one["agent"], json!({"id": AGENT, "name": "Scout"}));
    assert_eq!(one["workspace"], json!(workspace));
    assert_eq!(
        one["action"],
        json!({"provider": "app.example", "kind": "letters.box", "action": "send"})
    );
    assert_eq!(
        one["on"],
        json!({"document": "", "resource": "letters.box/out"})
    );
    assert_eq!(
        one["words"],
        json!({"action": "Send a letter", "on": "Outbox", "place": "Team"})
    );
    assert_eq!(
        one["askedBy"],
        json!({"workspace": workspace, "conversation": "c_general", "message": "m_question"})
    );
    assert_eq!(one["usedTimes"], json!(1));
    assert!(one["sinceMs"].as_u64().unwrap() > 0, "{one}");
    assert!(
        one["lastUsedMs"].as_u64().unwrap() >= one["sinceMs"].as_u64().unwrap(),
        "{one}"
    );
    one.clone()
}

/// A person reads what stands for them with everything a client shows of
/// it, and the typed action its listing names takes it back: from that
/// moment the action asks again, and a use counted late does not bring it
/// back.
#[tokio::test]
async fn a_person_reads_what_stands_for_them_and_takes_it_back_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, holder) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let as_ada = caller(&ada);
    let person = ada.user_public_key;
    let asked = sending(&workspace, AGENT, Some("letters.box/out"));

    assert_eq!(
        workspaces.stands(&person, &asked),
        None,
        "nothing stands before she said always"
    );
    let empty = ask(
        &holder,
        workspace_id,
        (&ada, &as_ada),
        person_asking::LIST,
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(empty, json!({"permissions": []}));

    let id = answered_always(workspaces, &person, &asked);
    assert_eq!(workspaces.stands(&person, &asked), Some(id.clone()));
    workspaces.permission_used(&person, &id);

    let listed = ask(
        &holder,
        workspace_id,
        (&ada, &as_ada),
        person_asking::LIST,
        json!({}),
    )
    .await
    .unwrap();
    let one = the_one_listed(&listed, &id, &workspace);

    // A client takes it back with exactly the action its listing names.
    let take_back = &one["takeBack"];
    let target = RemoteActionTarget {
        provider: take_back["provider"].as_str().unwrap().to_owned(),
        address: ActionAddress::new(
            take_back["resource"].as_str().unwrap(),
            take_back["action"].as_str().unwrap(),
        ),
    };
    let left = ask_at(
        &holder,
        workspace_id,
        (&ada, &as_ada),
        target.clone(),
        take_back["arguments"].clone(),
    )
    .await
    .unwrap();
    assert_eq!(left, json!({"permissions": []}));
    assert_eq!(
        workspaces.stands(&person, &asked),
        None,
        "taken back, the action asks again from this moment"
    );
    workspaces.permission_used(&person, &id);
    assert_eq!(
        workspaces.stands(&person, &asked),
        None,
        "a use counted late does not bring it back"
    );
    assert_eq!(workspaces.permissions_of(&person).permissions, Vec::new());

    // Taken back once, there is nothing left to take back.
    let again = ask_at(
        &holder,
        workspace_id,
        (&ada, &as_ada),
        target,
        take_back["arguments"].clone(),
    )
    .await;
    assert!(
        matches!(again, Err(ActionCallError::Refused { .. })),
        "{again:?}"
    );
    server.shutdown().await;
}

/// Nobody but the person reads what stands for them or takes any of it
/// back: not their own agent, however it is admitted, not a bot or an app
/// acting for them, not their key on someone else's device, and not another
/// member, who reaches only their own. Each is told what a caller asking
/// for something that does not exist is told, and what stood still stands.
#[tokio::test]
async fn nobody_but_the_person_reads_or_takes_back_what_stands_for_them() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, holder) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let bo = admit(workspaces, &workspace, &owner, 2).await;
    let as_ada = caller(&ada);
    let person = ada.user_public_key;
    let asked = sending(&workspace, AGENT, Some("letters.box/out"));
    let id = answered_always(workspaces, &person, &asked);

    let her = hex::encode(person);
    let not_her: [(&str, &DeviceCert, Caller); 7] = [
        (
            "an agent certified under her key",
            &ada,
            Caller::delegated_agent(AGENT, ada.user_public_key, ada.device_public_key),
        ),
        (
            "an agent she certified, on a key of its own",
            &ada,
            agent_caller(&ada, AGENT),
        ),
        (
            "her agent as the server has it in that workspace",
            &ada,
            agent_caller(&ada, AGENT)
                .in_workspace(&*workspace)
                .named("Scout"),
        ),
        (
            "a bot she installed",
            &ada,
            Caller::bot("bot", ada.user_public_key, ada.device_public_key),
        ),
        (
            "an agent the server admitted itself, acting for her",
            &ada,
            Caller::local_agent(AGENT).acting_for(her.clone()),
        ),
        (
            "an app acting for her",
            &ada,
            Caller::local_app("so.tilo.chat").acting_for(her),
        ),
        ("her key on another person's device", &bo, caller(&ada)),
    ];
    for (who, device, asking) in &not_her {
        for (action, argument) in [
            (person_asking::LIST, json!({})),
            (person_asking::TAKE_BACK, json!({"permission": id})),
        ] {
            let refused = ask(&holder, workspace_id, (device, asking), action, argument).await;
            assert!(
                matches!(refused, Err(ActionCallError::UnknownAction)),
                "{who}, {action}: {refused:?}"
            );
        }
    }

    // Another member reads their own, which is empty, and takes back
    // nothing of hers by naming it.
    let as_bo = caller(&bo);
    let his = ask(
        &holder,
        workspace_id,
        (&bo, &as_bo),
        person_asking::LIST,
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(his, json!({"permissions": []}));
    let not_his = ask(
        &holder,
        workspace_id,
        (&bo, &as_bo),
        person_asking::TAKE_BACK,
        json!({"permission": id}),
    )
    .await;
    assert!(
        matches!(not_his, Err(ActionCallError::Refused { .. })),
        "{not_his:?}"
    );
    assert_eq!(
        workspaces.stands(&bo.user_public_key, &asked),
        None,
        "what stands for her stands for nobody else"
    );

    // Nothing of hers moved.
    assert_eq!(workspaces.stands(&person, &asked), Some(id.clone()));
    let hers = ask(
        &holder,
        workspace_id,
        (&ada, &as_ada),
        person_asking::LIST,
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(hers["permissions"][0]["id"], json!(id));
    server.shutdown().await;
}

/// No request lets anything stand: not the person's own, not their agent's.
/// Whatever a write might be called is no action here, on the permissions
/// or on anything else of the provider, and after all of it nothing stands.
#[tokio::test]
async fn no_request_lets_anything_stand() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, holder) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let person = ada.user_public_key;
    let asked = sending(&workspace, AGENT, None);
    let scope = serde_json::to_value(&asked).unwrap();
    let wanted = json!({
        "scope": scope,
        "permission": scope,
        "name": "Scout",
        "words": {"action": "Send a letter", "place": "Team"},
    });

    let herself = caller(&ada);
    let her_agent = agent_caller(&ada, AGENT)
        .in_workspace(&*workspace)
        .named("Scout");
    for (who, asking) in [("the person", &herself), ("her agent", &her_agent)] {
        for resource in [person_asking::PERMISSIONS, "permissions", "always", ""] {
            for action in [
                "add",
                "allow",
                "always",
                "create",
                "grant",
                "let-always",
                "put",
                "set",
                "write",
            ] {
                let refused = ask_at(
                    &holder,
                    workspace_id,
                    (&ada, asking),
                    person_asking::target(resource, action),
                    wanted.clone(),
                )
                .await;
                assert!(
                    matches!(refused, Err(ActionCallError::UnknownAction)),
                    "{who}, {resource} {action}: {refused:?}"
                );
            }
        }
    }
    assert_eq!(workspaces.stands(&person, &asked), None);
    assert_eq!(workspaces.permissions_of(&person).permissions, Vec::new());
    server.shutdown().await;
}

/// What stands covers exactly what was asked: that agent, that action, in
/// that workspace, on that thing. A permission for every thing of a kind
/// covers each of them and no other action. The same answer again adds no
/// second permission, and a scope that is not made of names lets nothing
/// stand.
#[tokio::test]
async fn what_stands_covers_exactly_what_was_asked() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, _, _holder) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let person = ada.user_public_key;

    let out = sending(&workspace, AGENT, Some("letters.box/out"));
    let id = answered_always(workspaces, &person, &out);
    assert_eq!(workspaces.stands(&person, &out), Some(id.clone()));

    let other_box = sending(&workspace, AGENT, Some("letters.box/drafts"));
    let any_box = sending(&workspace, AGENT, None);
    let other_agent = sending(&workspace, "helper", Some("letters.box/out"));
    let elsewhere = sending("another-workspace", AGENT, Some("letters.box/out"));
    let mut purging = out.clone();
    purging.action.action = "purge".into();
    let mut other_kind = out.clone();
    other_kind.action.kind = "letters.archive".into();
    let mut other_app = out.clone();
    other_app.action.provider = "app.other".into();
    let mut other_document = out.clone();
    other_document.on = Some(Thing {
        document: "uuid:another".into(),
        resource: "letters.box/out".into(),
    });
    for (what, asked) in [
        ("another thing of the kind", &other_box),
        ("the kind as a whole", &any_box),
        ("another agent of hers", &other_agent),
        ("another workspace", &elsewhere),
        ("another action on the same thing", &purging),
        ("another kind", &other_kind),
        ("another app", &other_app),
        ("the same resource of another document", &other_document),
    ] {
        assert_eq!(workspaces.stands(&person, asked), None, "{what}");
    }

    // Said again, it is the one permission, since when it first stood.
    let since = workspaces.permissions_of(&person).permissions[0].since_ms;
    assert_eq!(answered_always(workspaces, &person, &out), id);
    let stands = workspaces.permissions_of(&person).permissions;
    assert_eq!(stands.len(), 1);
    assert_eq!(stands[0].since_ms, since);

    // One for every thing of the kind covers each thing, and nothing else.
    let any = answered_always(workspaces, &person, &any_box);
    assert_ne!(any, id);
    assert_eq!(workspaces.stands(&person, &any_box), Some(any.clone()));
    assert_eq!(workspaces.stands(&person, &other_box), Some(any.clone()));
    assert_eq!(
        workspaces.stands(&person, &out),
        Some(id.clone()),
        "the narrower one answers for its own thing"
    );
    let mut purging_any = any_box.clone();
    purging_any.action.action = "purge".into();
    assert_eq!(workspaces.stands(&person, &purging_any), None);
    assert_eq!(workspaces.stands(&person, &other_agent), None);
    assert_eq!(workspaces.stands(&person, &elsewhere), None);

    // Taking the wide one back leaves the narrow one, and the other way
    // round nothing wider comes back.
    assert_eq!(
        workspaces.take_back_permission(&person, &any).await,
        Ok(true)
    );
    assert_eq!(workspaces.stands(&person, &other_box), None);
    assert_eq!(workspaces.stands(&person, &out), Some(id));

    // What is not made of names lets nothing stand.
    for unnamed in [
        sending(&workspace, "", None),
        sending("", AGENT, None),
        sending(&workspace, "scout\nhelper", None),
        sending(&workspace, AGENT, Some("")),
    ] {
        let refused = workspaces.let_always(
            &person,
            Allowing {
                scope: unnamed.clone(),
                name: "Scout".into(),
                words: Words::default(),
                asked_by: None,
            },
        );
        assert!(refused.is_err(), "{unnamed:?}");
        assert_eq!(workspaces.stands(&person, &unnamed), None);
    }
    assert_eq!(workspaces.permissions_of(&person).permissions.len(), 1);
    server.shutdown().await;
}

/// What stands is kept, so it stands after the server restarted, and a person
/// holds a bounded number of them.
#[tokio::test]
async fn what_stands_outlives_a_restart_of_the_server_and_is_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, _, _holder) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let person = ada.user_public_key;
    let asked = sending(&workspace, AGENT, Some("letters.box/out"));
    let id = answered_always(workspaces, &person, &asked);
    workspaces.permission_used(&person, &id);
    server.shutdown().await;

    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    assert_eq!(workspaces.stands(&person, &asked), Some(id.clone()));
    let kept = workspaces.permissions_of(&person).permissions;
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].id, id);
    assert_eq!(kept[0].used_times, 1);

    for number in 1..MAX_PERMISSIONS {
        let thing = format!("letters.box/{number}");
        answered_always(
            workspaces,
            &person,
            &sending(&workspace, AGENT, Some(&thing)),
        );
    }
    assert_eq!(
        workspaces.permissions_of(&person).permissions.len(),
        MAX_PERMISSIONS
    );
    let one_more = sending(&workspace, AGENT, Some("letters.box/one-more"));
    let refused = workspaces.let_always(
        &person,
        Allowing {
            scope: one_more.clone(),
            name: "Scout".into(),
            words: Words::default(),
            asked_by: None,
        },
    );
    assert!(refused.is_err(), "{refused:?}");
    assert_eq!(workspaces.stands(&person, &one_more), None);
    // One that stands already is still answered as lasting.
    assert_eq!(answered_always(workspaces, &person, &asked), id);
    server.shutdown().await;
}

/// The question `message` an agent asked, waiting since `asked_at_ms`.
fn waiting(workspace: &str, message: &str, asked_at_ms: u64) -> Waiting {
    let mut arguments = serde_json::Map::new();
    arguments.insert("message".into(), json!(message));
    arguments.insert("answer".into(), json!("yes"));
    Waiting {
        workspace: workspace.to_owned(),
        conversation: "c_general".into(),
        document: "part-0".into(),
        message: message.to_owned(),
        kind: "choice".into(),
        words: "Which one?".into(),
        by: AgentName {
            id: AGENT.into(),
            name: "Scout".into(),
        },
        asked_at_ms,
        answers: vec![WaitingAnswer {
            id: "yes".into(),
            label: "Yes".into(),
            target: ActionTarget {
                app: "so.tilo.chat".into(),
                document: "part-0".into(),
                resource: format!("chat.messages/messages/{message}"),
                action: "answer-question".into(),
            },
            arguments,
        }],
    }
}

/// The questions that wait are listed to the person they ask, over every
/// workspace the server keeps and oldest first, and to nobody else: another
/// member reads their own, and an agent, the person's own included, reads
/// none.
#[tokio::test]
async fn the_questions_that_wait_are_listed_to_the_person_they_ask_alone() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, holder) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let bo = admit(workspaces, &workspace, &owner, 2).await;
    let (hers, his) = (
        hex::encode(ada.user_public_key),
        hex::encode(bo.user_public_key),
    );
    workspaces.note_waiting(
        &workspace,
        vec![
            (hers.clone(), waiting(&workspace, "m_later", 2_000)),
            (his, waiting(&workspace, "m_his", 1_500)),
        ],
    );
    workspaces.note_waiting(
        "another-workspace",
        vec![(hers.clone(), waiting("another-workspace", "m_first", 1_000))],
    );
    let listing = person_asking::target(person_asking::WAITING, person_asking::LIST);
    let asked = |who: &DeviceCert, asking: Caller| {
        let (holder, listing, who) = (holder.clone(), listing.clone(), who.clone());
        async move { ask_at(&holder, workspace_id, (&who, &asking), listing, json!({})).await }
    };

    let listed = asked(&ada, caller(&ada)).await.unwrap();
    let questions = listed["questions"].as_array().unwrap();
    let messages: Vec<&str> = questions
        .iter()
        .map(|question| question["message"].as_str().unwrap())
        .collect();
    assert_eq!(messages, ["m_first", "m_later"], "hers alone, oldest first");
    assert_eq!(
        serde_json::to_value(waiting(&workspace, "m_later", 2_000)).unwrap(),
        questions[1],
        "as a client binds it"
    );
    assert_eq!(questions[1]["askedAtMs"], json!(2_000));
    assert_eq!(
        questions[1]["answers"][0]["arguments"],
        json!({"message": "m_later", "answer": "yes"})
    );

    let his = asked(&bo, caller(&bo)).await.unwrap();
    assert_eq!(his["questions"].as_array().unwrap().len(), 1);
    assert_eq!(his["questions"][0]["message"], json!("m_his"));
    for (who, device, asking) in [
        ("her own agent", &ada, agent_caller(&ada, AGENT)),
        ("her key on another device", &bo, caller(&ada)),
    ] {
        let refused = asked(device, asking).await;
        assert!(
            matches!(refused, Err(ActionCallError::UnknownAction)),
            "{who}: {refused:?}"
        );
    }

    // A workspace with none waiting any more says so, and the list follows.
    workspaces.note_waiting(&workspace, Vec::new());
    let left = asked(&ada, caller(&ada)).await.unwrap();
    assert_eq!(left["questions"].as_array().unwrap().len(), 1);
    assert_eq!(left["questions"][0]["message"], json!("m_first"));
    server.shutdown().await;
}
