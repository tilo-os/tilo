//! The Cloud Store owner opens applications for people and agent tasks.
use super::*;
use qos_peer_state::desktop_command::{BoundCommand, CommandSource};

fn request(store: &Store, caller: &DeviceCert, command: DesktopCommand) -> OwnerCommitRequest {
    OwnerCommitRequest {
        room: store.state.room.clone(),
        operation: CommitOperation::DesktopCommand {
            invocation: uuid::Uuid::new_v4(),
            caller: caller.clone(),
            command: BoundCommand {
                source: CommandSource {
                    room: store.state.room.clone(),
                    heads: store
                        .peer()
                        .unwrap()
                        .admitted_room()
                        .unwrap()
                        .clone()
                        .get_heads()
                        .iter()
                        .map(|head| head.0)
                        .collect(),
                },
                command,
            },
        },
    }
}

fn notes_records(store: &Store) -> Vec<String> {
    store
        .peer()
        .unwrap()
        .rows()
        .into_iter()
        .filter(|row| row.app_id == "so.tilo.notes")
        .map(|row| row.record_id)
        .collect()
}

type Owner = std::sync::Arc<crate::agent_runtime_adapter::RuntimeOwner>;
fn owner_store() -> (tempfile::TempDir, Owner, Store, DeviceCert) {
    let (directory, owner) = crate::agent_workers::tests::selected();
    let store = owner.store().unwrap();
    let caller = store
        .peer()
        .unwrap()
        .certificates()
        .into_iter()
        .find(|cert| cert.device_public_key != store.key().verifying_key().to_bytes())
        .expect("an enrolled person device");
    (directory, owner, store, caller)
}

#[test]
fn a_person_opens_notes_in_a_new_window_with_a_new_record() {
    let (_directory, _owner, mut store, caller) = owner_store();
    assert!(
        notes_records(&store).is_empty(),
        "the Cloud seed has no Notes"
    );
    let open = request(
        &store,
        &caller,
        DesktopCommand::OpenApp {
            app: "so.tilo.notes".into(),
        },
    );
    let receipt = store.commit_desktop_command(&open, None).unwrap();
    receipt.validate_for(&open).unwrap();
    let records = notes_records(&store);
    assert_eq!(records.len(), 1, "one checked Notes record");
    let tree = crate::agent_owner::workspace_tree(&store.peer().unwrap()).unwrap();
    assert!(
        tree.workspaces[0]
            .strips
            .iter()
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .any(|tab| tab.title == "Notes"),
        "a Notes tab on the desktop"
    );
    // The same invocation is answered from its receipt: no second window.
    let replay = store.commit_desktop_command(&open, None).unwrap();
    assert_eq!(replay.generation, receipt.generation);
    assert_eq!(notes_records(&store).len(), 1);
    // A person's open is not an agent task's open.
    assert!(store.task_opened_records("any-task").is_empty());
}

#[test]
fn an_agent_task_open_is_recorded_for_that_task_only() {
    let (_directory, _owner, mut store, caller) = owner_store();
    let opener = DesktopOpener {
        agent: "agent-1".into(),
        task: "11111111-1111-4111-8111-111111111111".into(),
    };
    let open = request(
        &store,
        &caller,
        DesktopCommand::OpenApp {
            app: "so.tilo.notes".into(),
        },
    );
    store.commit_desktop_command(&open, Some(&opener)).unwrap();
    let record = notes_records(&store).pop().unwrap();
    assert_eq!(
        store.task_opened_records(&opener.task),
        vec![qos_agent_control_protocol::OpenedRecord {
            app_id: "so.tilo.notes".into(),
            record_id: record,
        }]
    );
    assert!(
        store
            .task_opened_records("22222222-2222-4222-8222-222222222222")
            .is_empty()
    );
    // An agent task may only open applications, never rearrange the desktop.
    let strip = crate::agent_owner::workspace_tree(&store.peer().unwrap())
        .unwrap()
        .workspaces[0]
        .strips[0]
        .id
        .0;
    let rename = request(
        &store,
        &caller,
        DesktopCommand::RenameStrip {
            strip,
            name: "Agent".into(),
        },
    );
    assert!(matches!(
        store.commit_desktop_command(&rename, Some(&opener)),
        Err(DesktopCommandError::Rejected(_))
    ));
}

#[test]
fn unknown_callers_and_applications_are_refused_without_a_commit() {
    let (_directory, _owner, mut store, caller) = owner_store();
    let generation = store.state.generation;
    let unknown = request(
        &store,
        &caller,
        DesktopCommand::OpenApp {
            app: "dev.qos.not-installed".into(),
        },
    );
    assert!(matches!(
        store.commit_desktop_command(&unknown, None),
        Err(DesktopCommandError::Rejected(_))
    ));
    let mut stranger = caller.clone();
    stranger.device_public_key = [0x44; 32];
    let forged = request(
        &store,
        &stranger,
        DesktopCommand::OpenApp {
            app: "so.tilo.notes".into(),
        },
    );
    assert!(matches!(
        store.commit_desktop_command(&forged, None),
        Err(DesktopCommandError::Rejected(_))
    ));
    assert_eq!(
        store.state.generation, generation,
        "refusals commit nothing"
    );
}

/// A person's "Open Notes" in the Cloud browser: the certified private
/// broker publishes the desktop's open actions and runs them on the Store.
#[tokio::test(flavor = "current_thread")]
async fn a_person_opens_notes_through_the_cloud_broker() {
    use qos_relay::peer_actions::{ActionMessage, DirectedAction, ExactActionRoute};
    let (directory, owner, store, caller) = owner_store();
    let room: uuid::Uuid = store.state.room.parse().unwrap();
    let host = store.peer().unwrap().authority().device_public_key;
    drop(store);
    let time = owner.policy_time().unwrap();
    let send = |message| {
        let action = DirectedAction {
            room,
            request_id: uuid::Uuid::new_v4(),
            source: caller.device_public_key,
            target: host,
            message,
        };
        let directory = directory.path().to_owned();
        let caller = caller.clone();
        let time = time.clone();
        let policy = owner.policy_context();
        async move {
            crate::broker::handle_with_policy_time(
                &directory,
                "agent-admission",
                &caller,
                &action,
                &time,
                policy,
            )
            .await
            .unwrap()
        }
    };
    let resource = "desktop:app:so.tilo.notes".to_string();
    let ActionMessage::BrokerCatalogue { catalogue, .. } = send(ActionMessage::CatalogueRefresh {
        scope: Some(ExactActionRoute {
            app_id: "dev.qos.desktop".into(),
            record_id: String::new(),
            resource: resource.clone(),
            action: "open".into(),
        }),
    })
    .await
    else {
        panic!("no catalogue")
    };
    assert_eq!(catalogue["surfaces"][0]["app_id"], "dev.qos.desktop");
    assert_eq!(
        catalogue["surfaces"][0]["actions"][0]["action_label"],
        "Open Notes"
    );
    let invocation = qos_actions::InvocationId::new();
    let observed = send(ActionMessage::InvokeBound {
        host: room,
        invocation,
        app_peer: String::new(),
        app_id: "dev.qos.desktop".into(),
        record_id: String::new(),
        resource,
        action: "open".into(),
        argument: None,
        confirmation: None,
        revision: None,
    })
    .await;
    let ActionMessage::Observation {
        update: Some(qos_actions::InvocationUpdate::Succeeded { output, .. }),
        ..
    } = &observed
    else {
        panic!("{observed:?}")
    };
    // The receipt names the command id: the certified caller device and the
    // invocation the issuer minted.
    let id = DesktopCommandId {
        device: caller.device_public_key,
        invocation: invocation.0,
    };
    let receipt: DesktopCommandReceipt =
        serde_json::from_value(output.value.clone().unwrap()).unwrap();
    assert_eq!(receipt.command, id.to_string());
    assert_eq!(receipt.opened.len(), 1);
    assert_eq!(notes_records(&owner.store().unwrap()).len(), 1);
}

fn with_invocation(mut request: OwnerCommitRequest, id: uuid::Uuid) -> OwnerCommitRequest {
    if let CommitOperation::DesktopCommand { invocation, .. } = &mut request.operation {
        *invocation = id;
    }
    request
}

fn open_notes(store: &Store, caller: &DeviceCert) -> OwnerCommitRequest {
    request(
        store,
        caller,
        DesktopCommand::OpenApp {
            app: "so.tilo.notes".into(),
        },
    )
}

fn succeeded(update: qos_actions::InvocationUpdate) -> DesktopCommandReceipt {
    let qos_actions::InvocationUpdate::Succeeded { output, .. } = update else {
        panic!("{update:?}")
    };
    serde_json::from_value(output.value.unwrap()).unwrap()
}

fn refused(update: qos_actions::InvocationUpdate) -> (String, serde_json::Value) {
    let qos_actions::InvocationUpdate::Failed { error, .. } = update else {
        panic!("{update:?}")
    };
    assert_eq!(error.code, qos_peer_state::desktop_command::REJECTED_CODE);
    assert!(!error.retryable);
    (error.message, error.details.unwrap())
}

fn tabs(store: &Store) -> Vec<(u64, u64, u64)> {
    crate::agent_owner::workspace_tree(&store.peer().unwrap())
        .unwrap()
        .workspaces[0]
        .strips
        .iter()
        .flat_map(|strip| {
            strip.window_groups.iter().flat_map(move |group| {
                group
                    .tabs
                    .iter()
                    .map(move |tab| (strip.id.0, group.id, tab.id))
            })
        })
        .collect()
}

/// The receipt carries the issuer's command id and the exact committed
/// window, so the issuer binds its optimistic window to that tab.
#[test]
fn a_receipt_carries_the_command_id_and_the_committed_window() {
    let (_directory, _owner, mut store, caller) = owner_store();
    let open = open_notes(&store, &caller);
    let id = DesktopCommandId::of(&open).unwrap();
    let before = tabs(&store);
    let receipt = succeeded(store.desktop_command_update(&open, None).unwrap());
    assert_eq!(receipt.kind, qos_peer_state::desktop_command::RECEIPT_KIND);
    assert_eq!(receipt.command, id.to_string());
    assert_eq!(receipt.command.parse::<DesktopCommandId>().unwrap(), id);
    assert_eq!(receipt.generation, store.state.generation);
    let [window] = receipt.opened.as_slice() else {
        panic!("one opened window: {receipt:?}")
    };
    assert_eq!(window.app_id, "so.tilo.notes");
    assert_eq!(notes_records(&store), vec![window.record_id.clone()]);
    let added: Vec<_> = tabs(&store)
        .into_iter()
        .filter(|tab| !before.contains(tab))
        .collect();
    assert_eq!(added, vec![(window.strip, window.group, window.tab)]);
    assert_eq!(store.desktop_command_receipt(&id), Some(receipt));
}

/// A retry reuses the command id and has one effect; the same id with a
/// different command is refused as an id conflict and changes nothing.
#[test]
fn a_duplicate_command_id_is_applied_once() {
    let (_directory, _owner, mut store, caller) = owner_store();
    let open = open_notes(&store, &caller);
    let first = succeeded(store.desktop_command_update(&open, None).unwrap());
    let windows = tabs(&store).len();
    let generation = store.state.generation;
    let again = succeeded(store.desktop_command_update(&open, None).unwrap());
    assert_eq!(again, first, "a retry is answered from the receipt");
    assert_eq!(tabs(&store).len(), windows, "no second window");
    assert_eq!(notes_records(&store).len(), 1);
    assert_eq!(
        store.state.generation, generation,
        "a retry commits nothing"
    );
    let id = DesktopCommandId::of(&open).unwrap();
    let strip = tabs(&store)[0].0;
    let substituted = with_invocation(
        request(
            &store,
            &caller,
            DesktopCommand::RenameStrip {
                strip,
                name: "Other".into(),
            },
        ),
        id.invocation,
    );
    let (_, details) = refused(store.desktop_command_update(&substituted, None).unwrap());
    assert_eq!(details["command"], id.to_string());
    assert_eq!(details["reason"], "id_conflict");
    assert_eq!(store.state.generation, generation);
    // The same invocation from another device is a different command id:
    // it grants nothing and is judged on that device's own rights.
    let mut stranger = caller.clone();
    stranger.device_public_key = [0x44; 32];
    let foreign = with_invocation(open_notes(&store, &stranger), id.invocation);
    let (_, details) = refused(store.desktop_command_update(&foreign, None).unwrap());
    assert_ne!(details["command"], id.to_string());
    assert_eq!(details["reason"], "not_permitted");
    assert_eq!(store.state.generation, generation);
}

/// A refusal names the command id and a stable reason, and commits nothing.
#[test]
fn a_refusal_carries_the_command_id_and_reason() {
    let (_directory, _owner, mut store, caller) = owner_store();
    let generation = store.state.generation;
    let unknown = request(
        &store,
        &caller,
        DesktopCommand::OpenApp {
            app: "dev.qos.not-installed".into(),
        },
    );
    let (message, details) = refused(store.desktop_command_update(&unknown, None).unwrap());
    assert!(message.contains("checked record factory"), "{message}");
    assert_eq!(
        details["command"],
        DesktopCommandId::of(&unknown).unwrap().to_string()
    );
    assert_eq!(details["reason"], "unknown_app");
    let strip = tabs(&store)[0].0;
    let mut stale = request(
        &store,
        &caller,
        DesktopCommand::RenameStrip {
            strip,
            name: "Stale".into(),
        },
    );
    if let CommitOperation::DesktopCommand { command, .. } = &mut stale.operation {
        command.source.heads = vec![[9; 32]];
    }
    let (_, details) = refused(store.desktop_command_update(&stale, None).unwrap());
    assert_eq!(details["reason"], "stale_source");
    assert_eq!(
        store.state.generation, generation,
        "refusals commit nothing"
    );
}

/// People and agents open through one command type and one owner path:
/// the same receipt shape, one window each, the agent's kept for its task.
#[test]
fn a_person_and_an_agent_open_through_the_same_path() {
    let (_directory, owner, mut store, caller) = owner_store();
    let person = open_notes(&store, &caller);
    let person_receipt = succeeded(store.desktop_command_update(&person, None).unwrap());
    let opener = DesktopOpener {
        agent: "agent-1".into(),
        task: "11111111-1111-4111-8111-111111111111".into(),
    };
    let agent = open_notes(&store, &caller);
    let agent_receipt = succeeded(store.desktop_command_update(&agent, Some(&opener)).unwrap());
    for (request, receipt) in [(&person, &person_receipt), (&agent, &agent_receipt)] {
        assert_eq!(
            receipt.command,
            DesktopCommandId::of(request).unwrap().to_string()
        );
        assert_eq!(receipt.opened.len(), 1);
        assert_eq!(receipt.opened[0].app_id, "so.tilo.notes");
    }
    assert_ne!(person_receipt.opened[0].tab, agent_receipt.opened[0].tab);
    assert_eq!(
        store.task_opened_records(&opener.task),
        vec![qos_agent_control_protocol::OpenedRecord {
            app_id: "so.tilo.notes".into(),
            record_id: agent_receipt.opened[0].record_id.clone(),
        }]
    );
    // The broker's service hands the agent the same kept receipt.
    drop(store);
    let service = CloudDesktopCommands {
        room: owner.store().unwrap().state.room.clone(),
        owner: owner.clone(),
    };
    use qos_action_broker::services::DesktopCommands as _;
    assert_eq!(service.receipt(&agent), Some(agent_receipt));
    // An agent may only open: the same refusal path, with its id.
    let mut store = owner.store().unwrap();
    let strip = tabs(&store)[0].0;
    let rename = request(
        &store,
        &caller,
        DesktopCommand::RenameStrip {
            strip,
            name: "Agent".into(),
        },
    );
    let (_, details) = refused(
        store
            .desktop_command_update(&rename, Some(&opener))
            .unwrap(),
    );
    assert_eq!(details["reason"], "not_permitted");
}

/// Opening an application hands members exactly one covered checkpoint of
/// the new record, in the same outbox as its room change: members admit it
/// and advance. (Publishing the creation genesis as well left an uncovered
/// checkpoint ahead of the covered one, and members stuck on it.)
#[test]
fn an_open_publishes_one_covered_checkpoint_members_admit() {
    let (_directory, _owner, mut store, caller) = owner_store();
    let now = store.policy_now_ns().unwrap();
    let mut host = store.peer().unwrap();
    let authority = host.authority().clone();
    let mut member = StatePeer::new(
        &store.state.room,
        authority.clone(),
        host.certificates(),
        qos_user_identity::RevocationList::new(),
        now_time(now),
    )
    .unwrap();
    // The member holds the committed room before the open.
    member
        .receive_from(
            TransportOrigin::RelayDelta(authority.device_public_key),
            host.room_message(),
            now_time(now),
        )
        .unwrap();
    let before = store.state.outbox.len();
    let open = open_notes(&store, &caller);
    let receipt = succeeded(store.desktop_command_update(&open, None).unwrap());
    let record = receipt.opened[0].record_id.clone();
    let published: Vec<_> = store.state.outbox[before..]
        .iter()
        .map(|publication| publication.message.clone())
        .collect();
    let checkpoints = published
        .iter()
        .filter(|message| {
            matches!(message, NeighbourMsg::AppDocument { app_id, record_id, .. }
                if app_id == "so.tilo.notes" && record_id == &record)
        })
        .count();
    assert_eq!(checkpoints, 1, "one checkpoint of the new record");
    let at = store.policy_now_ns().unwrap();
    for message in published {
        member
            .receive_from(
                TransportOrigin::RelayDelta(authority.device_public_key),
                message,
                now_time(at),
            )
            .unwrap();
    }
    assert!(
        member.document("so.tilo.notes", &record).is_some(),
        "the member admits the opened record"
    );
    assert_eq!(
        member.durable_covers_current("so.tilo.notes", &record),
        Ok(true),
        "the member's cursor reaches the covered checkpoint"
    );
    let tree = crate::agent_owner::workspace_tree(&member).unwrap();
    assert!(
        tree.workspaces[0]
            .strips
            .iter()
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .any(|tab| tab.id == receipt.opened[0].tab),
        "the member sees the opened window"
    );
    // Closing that window: one room change members admit, no checkpoint
    // republished for the closed record, and a receipt naming the close.
    let window = receipt.opened[0].clone();
    let before = store.state.outbox.len();
    let close = request(
        &store,
        &caller,
        DesktopCommand::CloseTab {
            location: TabLocation {
                strip: window.strip,
                group: window.group,
                tab: window.tab,
            },
        },
    );
    let closed = succeeded(store.desktop_command_update(&close, None).unwrap());
    assert_eq!(
        closed.command,
        DesktopCommandId::of(&close).unwrap().to_string()
    );
    assert!(closed.opened.is_empty());
    let published: Vec<_> = store.state.outbox[before..]
        .iter()
        .map(|publication| publication.message.clone())
        .collect();
    assert!(
        !published.iter().any(|message| matches!(message,
            NeighbourMsg::AppDocument { record_id, .. } if record_id == &record)),
        "a close republishes no checkpoint of the closed record"
    );
    assert!(!published.is_empty(), "a close publishes its room change");
    let at = store.policy_now_ns().unwrap();
    for message in published {
        member
            .receive_from(
                TransportOrigin::RelayDelta(authority.device_public_key),
                message,
                now_time(at),
            )
            .unwrap();
    }
    let tree = crate::agent_owner::workspace_tree(&member).unwrap();
    assert!(
        !tree.workspaces[0]
            .strips
            .iter()
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .any(|tab| tab.id == window.tab),
        "the member sees the window closed"
    );
    // A retried close is answered from its receipt, not refused as missing.
    let generation = store.state.generation;
    let again = succeeded(store.desktop_command_update(&close, None).unwrap());
    assert_eq!(again, closed);
    assert_eq!(store.state.generation, generation);
}

/// Every openable application (Writer included) opens and closes with one
/// covered checkpoint and a receipt each; a retried close replays.
#[test]
fn every_openable_application_opens_and_closes_once() {
    let (_directory, _owner, mut store, caller) = owner_store();
    for (app, _) in openable_applications() {
        let open = request(
            &store,
            &caller,
            DesktopCommand::OpenApp { app: app.clone() },
        );
        let before = store.state.outbox.len();
        let opened = succeeded(store.desktop_command_update(&open, None).unwrap());
        let [window] = opened.opened.as_slice() else {
            panic!("{app}: {opened:?}")
        };
        let checkpoints = store.state.outbox[before..]
            .iter()
            .filter(|publication| {
                matches!(&publication.message,
                NeighbourMsg::AppDocument { record_id, .. } if record_id == &window.record_id)
            })
            .count();
        assert!(checkpoints <= 1, "{app}: {checkpoints} checkpoints");
        let close = request(
            &store,
            &caller,
            DesktopCommand::CloseTab {
                location: TabLocation {
                    strip: window.strip,
                    group: window.group,
                    tab: window.tab,
                },
            },
        );
        let closed = store.desktop_command_update(&close, None).unwrap();
        let closed = succeeded(closed);
        assert_eq!(
            succeeded(store.desktop_command_update(&close, None).unwrap()),
            closed
        );
        assert!(
            !tabs(&store).iter().any(|(_, _, tab)| *tab == window.tab),
            "{app} closed"
        );
    }
}

/// The Cloud catalogue does not grow with opens: after 50 open/close cycles
/// of Chat and Notes it is exactly as large as before (a closed
/// record leaves no actions behind), so replies stay far under the 256 KiB
/// native action reply limit.
#[tokio::test(flavor = "current_thread")]
async fn fifty_open_close_cycles_keep_the_catalogue_bounded() {
    use qos_relay::peer_actions::{ActionMessage, DirectedAction};
    let (directory, owner, store, caller) = owner_store();
    let room: uuid::Uuid = store.state.room.parse().unwrap();
    let host = store.peer().unwrap().authority().device_public_key;
    drop(store);
    let catalogue_len = || {
        let time = owner.policy_time().unwrap();
        let action = DirectedAction {
            room,
            request_id: uuid::Uuid::new_v4(),
            source: caller.device_public_key,
            target: host,
            message: ActionMessage::CatalogueRefresh { scope: None },
        };
        let directory = directory.path().to_owned();
        let caller = caller.clone();
        let policy = owner.policy_context();
        let owner = owner.clone();
        async move {
            let ActionMessage::BrokerCatalogue { catalogue, .. } =
                crate::broker::handle_with_policy_time(
                    &directory,
                    "agent-admission",
                    &caller,
                    &action,
                    &time,
                    policy,
                )
                .await
                .unwrap()
            else {
                panic!("no catalogue")
            };
            // The revision tracks the generation; compare the shape only.
            serde_json::to_string(&catalogue)
                .unwrap()
                .replace(
                    &format!("\"{}\"", owner.store().unwrap().state.generation),
                    "\"g\"",
                )
                .len()
        }
    };
    let before = catalogue_len().await;
    for _ in 0..50 {
        let mut store = owner.store().unwrap();
        // Chat and Notes: the native report saw Chat grow the most.
        for app in ["so.tilo.chat", "so.tilo.notes"] {
            let open = request(&store, &caller, DesktopCommand::OpenApp { app: app.into() });
            let window =
                succeeded(store.desktop_command_update(&open, None).unwrap()).opened[0].clone();
            let close = request(
                &store,
                &caller,
                DesktopCommand::CloseTab {
                    location: TabLocation {
                        strip: window.strip,
                        group: window.group,
                        tab: window.tab,
                    },
                },
            );
            succeeded(store.desktop_command_update(&close, None).unwrap());
        }
    }
    let after = catalogue_len().await;
    assert_eq!(after, before, "the catalogue does not grow with opens");
    assert!(after < 64 * 1024, "catalogue is {after} bytes");
}

/// With delivery storage selected, every open queues exactly what a
/// member admits: one covered checkpoint of the new record, then the close.
#[test]
fn with_delivery_selected_opens_are_admitted_by_members() {
    let (_directory, _owner, mut store, caller) = owner_store();
    store
        .activate_delivery_storage(store.policy_now_ns().unwrap())
        .unwrap();
    let now = store.policy_now_ns().unwrap();
    let host = store.peer().unwrap();
    let authority = host.authority().clone();
    let mut member = StatePeer::new(
        &store.state.room,
        authority.clone(),
        host.certificates(),
        qos_user_identity::RevocationList::new(),
        now_time(now),
    )
    .unwrap();
    let mut deliver = |store: &mut Store, member: &mut StatePeer| {
        let batch = store.delivery_relay_batch().unwrap();
        let messages: Vec<_> = batch.messages().map(|m| m.unwrap().message).collect();
        let at = store.policy_now_ns().unwrap();
        for message in &messages {
            member
                .receive_from(
                    TransportOrigin::RelayDelta(authority.device_public_key),
                    message.clone(),
                    now_time(at),
                )
                .unwrap();
        }
        store
            .retire_delivery_relay_batch(&batch, messages.len())
            .unwrap();
        messages
    };
    // The member holds the committed room; the queued baseline is retired.
    loop {
        let batch = store.delivery_relay_batch().unwrap();
        let sent = batch.messages().count();
        if sent == 0 {
            break;
        }
        store.retire_delivery_relay_batch(&batch, sent).unwrap();
    }
    let mut host = store.peer().unwrap();
    member
        .receive_from(
            TransportOrigin::RelayDelta(authority.device_public_key),
            host.room_message(),
            now_time(now),
        )
        .unwrap();
    for app in ["so.tilo.notes", "so.tilo.calculator", "so.tilo.notes"] {
        let open = request(&store, &caller, DesktopCommand::OpenApp { app: app.into() });
        let window =
            succeeded(store.desktop_command_update(&open, None).unwrap()).opened[0].clone();
        let messages = deliver(&mut store, &mut member);
        let checkpoints = messages
            .iter()
            .filter(|message| {
                matches!(message,
                NeighbourMsg::AppDocument { record_id, .. } if record_id == &window.record_id)
            })
            .count();
        assert!(checkpoints <= 1, "{app}: {checkpoints} checkpoints");
        assert!(
            member.document(app, &window.record_id).is_some(),
            "{app} admitted"
        );
        assert_eq!(
            member.durable_covers_current(app, &window.record_id),
            Ok(true),
            "{app} covered"
        );
    }
}
