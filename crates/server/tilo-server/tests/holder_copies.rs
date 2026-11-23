//! A workspace says how many copies it wants, and its holders repair each
//! other: one that was away takes in what the others hold without any
//! member's help, and every holder says which of them is short of what
//! until it has.

use tilo_peer_state::{CopyPolicy, CopyRole, CopyStatus, Missing, Shortfall};
use tilo_server::Envelope;
use tilo_server_protocol::WorkSetting;
use tilo_user_identity::DeviceCapabilities;

use crate::common::{Hosted, TestUser, receives, serve_in, serve_on, session_to};

/// Where `holder` says `workspace`'s copies stand, once `done` holds of it.
async fn copies_once(
    holder: &Hosted,
    workspace: &str,
    what: &str,
    done: impl Fn(&CopyStatus) -> bool + Sync,
) -> CopyStatus {
    tilo_test_env::within(what, async {
        loop {
            let status = holder.host.workspaces().copies(workspace).await.unwrap();
            if done(&status) {
                break status;
            }

            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
}

/// Three holders keep a workspace that wants three key holders. One goes
/// away while a member writes through another: the others say it lacks what
/// was written since it went, and that the workspace keeps one key holder
/// too few. Back from its files, it takes in what it missed from the other
/// holders by itself: a member that reaches only it gets what was written
/// meanwhile, and every holder says nothing falls short any more.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "one scenario: the holders, the outage and the return stay together"
)]
async fn a_holder_away_during_writes_catches_up_and_the_shortfall_clears() {
    let alice = TestUser::fresh("Alice");
    let laptop = alice.enrol("laptop", DeviceCapabilities::primary());
    let phone = alice.enrol("phone", DeviceCapabilities::primary());
    let tablet = alice.enrol("tablet", DeviceCapabilities::primary());
    let nas = alice.enrol("nas", DeviceCapabilities::member());
    let server = alice.enrol("server", DeviceCapabilities::member());
    let devices = [&laptop, &nas, &server];
    let settings = [
        WorkSetting::none(),
        WorkSetting::own_person(),
        WorkSetting::own_person(),
    ];
    let mut holders = Vec::new();
    for (device, work) in devices.iter().zip(&settings) {
        holders.push(Some(serve_on(&alice, device, work.clone()).await));
    }
    let made_on = holders[0].as_ref().unwrap().host.workspaces().clone();
    let workspace = made_on.create("Notes").await.unwrap();
    for at in 1..3 {
        made_on
            .attach_server(
                &workspace.workspace,
                devices[at].device_pubkey(),
                holders[at].as_ref().unwrap().host.node_addr(),
                tilo_peer_state::CopyRole::KeyHolder,
            )
            .await
            .unwrap();
    }
    for device in [&phone, &tablet] {
        made_on
            .enter(&workspace.workspace, &device.cert)
            .await
            .unwrap();
    }
    let wanted = CopyPolicy {
        key_holders: 3,
        stores: 0,
    };
    made_on
        .choose_copies(&workspace.workspace, Some(wanted))
        .await
        .unwrap();
    for holder in holders.iter().flatten() {
        let status = copies_once(
            holder,
            &workspace.workspace,
            "every copy is current",
            |status| status.policy == wanted && status.satisfied() && status.holders.len() == 3,
        )
        .await;
        assert!(status.chosen);
        assert!(
            status
                .holders
                .iter()
                .all(|holder| holder.role == CopyRole::KeyHolder)
        );
    }

    let away = server.device_pubkey();
    let state = holders[2].take().unwrap().stop_keeping().await;
    let on_laptop = session_to(&alice, &phone, holders[0].as_ref().unwrap()).await;
    on_laptop
        .send(Envelope::Subscribe {
            workspace_id: workspace.workspace_id,
        })
        .unwrap();
    on_laptop
        .send(Envelope::Delta {
            workspace_id: workspace.workspace_id,
            author_device_public_key: phone.device_pubkey(),
            authored_at_ns: 1,
            delta: b"while the server was away".to_vec(),
            signature: [0; 64],
        })
        .unwrap();
    for holder in holders.iter().flatten() {
        let status = copies_once(
            holder,
            &workspace.workspace,
            "the holders say the server is short",
            |status| {
                status.shortfall.iter().any(|short| {
                    matches!(short, Shortfall::Holder {
                        device,
                        missing: Missing::WritesSince { .. },
                    } if *device == away)
                })
            },
        )
        .await;
        assert!(
            status.shortfall.contains(&Shortfall::Copies {
                role: CopyRole::KeyHolder,
                wanted: 3,
                current: 2,
            }),
            "{status:?}"
        );
        assert!(
            status
                .holders
                .iter()
                .any(|holder| holder.device == away && !holder.current)
        );
    }

    holders[2] = Some(serve_in(&alice, &server, settings[2].clone(), state).await);
    let on_server = session_to(&alice, &tablet, holders[2].as_ref().unwrap()).await;
    on_server
        .send(Envelope::Subscribe {
            workspace_id: workspace.workspace_id,
        })
        .unwrap();
    receives(&on_server, b"while the server was away").await;
    for holder in holders.iter().flatten() {
        copies_once(
            holder,
            &workspace.workspace,
            "the shortfall clears on every holder",
            |status| status.satisfied() && status.holders.iter().all(|holder| holder.current),
        )
        .await;
    }
    drop((on_laptop, on_server));
    for holder in holders.into_iter().flatten() {
        holder.stop().await;
    }
}

/// The server a workspace was made on is one of its holders from the start,
/// named by its own signed address like any other: another person's server
/// that holds the workspace beside it reads it among the holders, and takes
/// in what it decides after the hand-over.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_server_a_workspace_was_made_on_is_named_among_its_holders() {
    use tilo_test_env::within;
    let site = TestUser::fresh("Site");
    let made_on_server = site.enrol("server", DeviceCapabilities::member());
    let other = TestUser::fresh("Other");
    let other_server = other.enrol("server", DeviceCapabilities::member());
    let made_on = within(
        "the server the workspace is made on starts",
        serve_on(&site, &made_on_server, WorkSetting::own_person()),
    )
    .await;
    let workspaces = made_on.host.workspaces().clone();
    let workspace = within("the workspace is made", workspaces.create("Notes"))
        .await
        .unwrap();
    let mut takes = WorkSetting::own_person();
    takes.from =
        tilo_server_protocol::WorkFrom::Workspaces([workspace.workspace_id].into_iter().collect());
    let holder = within("the holder starts", serve_on(&other, &other_server, takes)).await;
    within(
        "the server it was made on attaches the holder",
        workspaces.attach_server(
            &workspace.workspace,
            other_server.device_pubkey(),
            holder.host.node_addr(),
            CopyRole::KeyHolder,
        ),
    )
    .await
    .unwrap();
    let named = within(
        "the holder names the workspace's holders",
        holder.host.workspaces().hosts(&workspace.workspace),
    )
    .await
    .unwrap();
    assert!(
        named
            .iter()
            .any(|hint| hint.host.device_public_key == made_on_server.device_pubkey()),
        "the holder reads the server the workspace was made on among its holders"
    );
    let wanted = CopyPolicy {
        key_holders: 2,
        stores: 0,
    };
    within(
        "the server it was made on chooses two key holders",
        workspaces.choose_copies(&workspace.workspace, Some(wanted)),
    )
    .await
    .unwrap();
    copies_once(
        &holder,
        &workspace.workspace,
        "the holder takes in what the server it was made on decides",
        |status| status.chosen && status.policy == wanted,
    )
    .await;
    within("the holder stops", holder.stop()).await;
    within("the server it was made on stops", made_on.stop()).await;
}

/// What `host` answers `message` about `workspace` with, asked by `client`.
async fn ask(
    client: &tilo_server::ServerClient,
    host: &crate::common::TestDevice,
    workspace: &tilo_server_protocol::workspaces::WorkspaceSummary,
    message: tilo_actions::remote::RemoteActionMessage,
) -> tilo_actions::remote::RemoteActionMessage {
    use tilo_actions::remote::RemoteActionFrame;
    client
        .send(Envelope::RemoteAction {
            workspace_id: workspace.workspace_id,
            version: tilo_server::transport::REMOTE_ACTION_ENVELOPE_VERSION,
            to_device_public_key: host.device_pubkey(),
            from_device_public_key: [0; 32],
            from_user_public_key: [0; 32],
            frame: RemoteActionFrame::new(message).encode().unwrap(),
        })
        .unwrap();
    tilo_test_env::within("the holder answers", async {
        loop {
            match client.recv().await.expect("the session stays open") {
                Envelope::RemoteAction { frame, .. } => {
                    break RemoteActionFrame::decode(&frame).unwrap().message;
                }
                Envelope::RemoteActionUndeliverable { refusal, .. } => {
                    panic!("the action was not delivered: {refusal}")
                }
                _ => {}
            }
        }
    })
    .await
}

/// The ids of the actions `host` offers `client` on `workspace`'s resource
/// `child` of its governance tree: what a path such as `workspace/servers`
/// is found among.
async fn offered(
    client: &tilo_server::ServerClient,
    host: &crate::common::TestDevice,
    workspace: &tilo_server_protocol::workspaces::WorkspaceSummary,
    child: &str,
) -> Vec<String> {
    use tilo_actions::remote::{
        CatalogueQuery, RemoteActionMessage, WORKSPACE_GOVERNANCE_PROVIDER,
        workspace_governance_child, workspace_governance_resource,
    };
    let RemoteActionMessage::CatalogueResponse { result, .. } = ask(
        client,
        host,
        workspace,
        RemoteActionMessage::CatalogueRequest {
            correlation: 1,
            query: CatalogueQuery {
                provider: Some(WORKSPACE_GOVERNANCE_PROVIDER.into()),
                parent_resource: Some(workspace_governance_resource(&workspace.workspace)),
                ..CatalogueQuery::default()
            },
        },
    )
    .await
    else {
        panic!("a catalogue request is answered with a page");
    };
    let resource = workspace_governance_child(&workspace.workspace, Some(child));
    result
        .unwrap()
        .rows
        .into_iter()
        .filter(|row| row.resource.id == resource)
        .filter_map(|row| row.action.map(|action| action.address.action))
        .collect()
}

/// What `host` answers `action` of `workspace`'s governance with `argument`,
/// asked by `client`.
async fn govern(
    client: &tilo_server::ServerClient,
    host: &crate::common::TestDevice,
    workspace: &tilo_server_protocol::workspaces::WorkspaceSummary,
    action: &str,
    argument: serde_json::Value,
) -> Result<serde_json::Value, tilo_actions::remote::ActionCallError> {
    use tilo_actions::remote::{
        InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionMessage, RemoteActionTarget,
    };
    let serde_json::Value::Object(argument) = argument else {
        panic!("arguments are an object");
    };
    let message = RemoteActionMessage::Invoke(InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        target: RemoteActionTarget::workspace_governance(&workspace.workspace, action),
        argument: InvokeArgument::Json(argument),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    });
    let RemoteActionMessage::Receipt(receipt) = ask(client, host, workspace, message).await else {
        panic!("an invocation is answered with a receipt");
    };
    match receipt.status {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or_default()),
        ReceiptStatus::Rejected { error } => Err(error),
        other => panic!("the holder's governance answers at once: {other:?}"),
    }
}

/// A workspace never stays stuck on a server that is gone. Two servers of an
/// operator keep a workspace for Alice: the first made it as an earlier build
/// did, owning it itself under a grant that names her the person it keeps it
/// for, and handed it to the second. The first then stops for good. Alice,
/// through a key of hers that reached the workspace on the second server,
/// lists its servers there, which makes the workspace hers, and takes the
/// stopped one out; the second then keeps the workspace alone and makes its
/// base.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "one story: the old build's grant, the hand-over, the stop and the owner's take-out"
)]
async fn the_owner_takes_a_stopped_server_out_through_the_one_that_remains() {
    use tilo_action_policy::{Answer, Grant, GrantId, Grantee, KindSet, Lifetime, roles};
    use tilo_test_env::within;
    let operator = TestUser::fresh("Operator");
    let old = operator.enrol("old", DeviceCapabilities::member());
    let main = operator.enrol("main", DeviceCapabilities::member());
    let alice = TestUser::fresh("Alice");
    let key = alice.enrol("key", DeviceCapabilities::server_key_grant());
    let made_on = serve_on(&operator, &old, WorkSetting::own_person()).await;
    let remains = serve_on(&operator, &main, WorkSetting::own_person()).await;
    let workspace = made_on.host.workspaces().create("Notes").await.unwrap();
    // The grant an earlier build wrote to name the person it kept a
    // workspace for while its host owned it, as it wrote it.
    let kept_for = Grant {
        id: GrantId(format!(
            "workspace-keeper-v1:{}",
            hex::encode(alice.user_pk)
        )),
        grantee: Grantee::person(hex::encode(alice.user_pk)),
        node: roles::administrators_node(&workspace.workspace),
        kinds: KindSet::UNDOABLE.union(KindSet::OUTSIDE),
        answer: Answer::Allow,
        lifetime: Lifetime::UntilRevoked,
        granted_by: Grantee::person(hex::encode(operator.user_pk)),
    };
    made_on
        .host
        .workspaces()
        .put_workspace_grants(&workspace.workspace, &old.cert, [5; 32], vec![kept_for])
        .await
        .unwrap();
    within(
        "the server it was made on hands it over",
        made_on.host.workspaces().attach_server(
            &workspace.workspace,
            main.device_pubkey(),
            remains.host.node_addr(),
            CopyRole::KeyHolder,
        ),
    )
    .await
    .unwrap();
    let workspaces = remains.host.workspaces();
    // The key joins as an API server's key of the person does, while both
    // servers keep the workspace: the server it reaches invites it and it
    // redeems the invitation there.
    let invited = crate::common::invitation(
        workspaces,
        &main,
        &workspace.workspace,
        tilo_peer_state::join::Role::Editor,
        &key,
    )
    .await;
    within(
        "the owner's key joins on the remaining server",
        crate::common::redeem(&key, &invited),
    )
    .await
    .unwrap();
    let client = tilo_server::ServerClient::connect_to_pinned_host(
        crate::common::device_endpoint(&key).await,
        remains.host.node_addr(),
        key.cert.clone(),
        key.sk.clone(),
        tilo_user_identity::RevocationList::new(),
    )
    .await
    .unwrap();
    client
        .send(Envelope::Subscribe {
            workspace_id: workspace.workspace_id,
        })
        .unwrap();
    within("the remaining server admits the owner's key", async {
        loop {
            match client.recv().await.expect("the session stays open") {
                Envelope::Ready { .. } => break,
                Envelope::NotAuthorised { .. } => panic!("the owner's key was not admitted"),
                _ => {}
            }
        }
    })
    .await;
    copies_once(
        &remains,
        &workspace.workspace,
        "the remaining server reads both servers among the holders",
        |status| status.base_maker == Some(old.device_pubkey()),
    )
    .await;
    within("the server it was made on stops", made_on.stop()).await;

    let (old_key, main_key) = (
        hex::encode(old.device_pubkey()),
        hex::encode(main.device_pubkey()),
    );
    // The workspace is still the server's, kept for Alice: its servers are
    // offered to her all the same, at the path she finds them by, or she
    // never reaches the choice that makes it hers.
    let servers = offered(&client, &main, &workspace, "servers").await;
    for action in ["list", "remove"] {
        assert!(
            servers.iter().any(|offered| offered == action),
            "the person it is kept for is offered workspace/servers#{action}: {servers:?}"
        );
    }
    let listed = govern(
        &client,
        &main,
        &workspace,
        "server.list",
        serde_json::json!({}),
    )
    .await
    .expect("the owner's key lists the workspace's servers");
    assert!(listed.to_string().contains(&old_key), "{listed}");
    assert!(
        workspaces
            .is_owner(&workspace.workspace, alice.user_pk)
            .await,
        "choosing its servers, the person it is kept for owns it"
    );
    let removed = govern(
        &client,
        &main,
        &workspace,
        "server.remove",
        serde_json::json!({ "server": old_key }),
    )
    .await
    .expect("the owner's key takes the stopped server out");
    let servers = removed["servers"].as_array().unwrap();
    assert_eq!(servers.len(), 1, "{removed}");
    assert_eq!(servers[0]["device"], main_key, "{removed}");
    let status = workspaces.copies(&workspace.workspace).await.unwrap();
    assert_eq!(
        status.base_maker,
        Some(main.device_pubkey()),
        "the remaining server makes the base: {status:?}"
    );
    assert!(
        !status
            .holders
            .iter()
            .any(|holder| holder.device == old.device_pubkey()),
        "a server the owner took out is no holder: {status:?}"
    );
    assert!(
        !status.shortfall.iter().any(|short| matches!(
            short,
            Shortfall::Holder { device, .. } if *device == old.device_pubkey()
        )),
        "and nothing falls short about it: {status:?}"
    );
    drop(client);
    remains.stop().await;
}
