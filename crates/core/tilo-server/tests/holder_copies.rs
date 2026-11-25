//! A workspace says how many copies it wants, and its holders repair each
//! other: one that was away takes in what the others hold without any
//! member's help, and every holder says which of them is short of what
//! until it has.

#![allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]

use std::time::Duration;

use automerge::{AutoCommit, ROOT, transaction::Transactable as _};
use tilo_actions::InvocationId;
use tilo_actions::remote::{
    ActionCallError, CatalogueQuery, InvokeArgument, InvokeRequest, ReceiptStatus,
    RemoteActionMessage, RemoteActionTarget, WORKSPACE_GOVERNANCE_PROVIDER,
    workspace_governance_child, workspace_governance_resource,
};
use tilo_peer_state::actions::RemoteDevice;
use tilo_peer_state::members::MembershipOpCommand;
use tilo_peer_state::workspace::GovernanceRow;
use tilo_peer_state::{CopyPolicy, CopyRole, CopyStatus, Missing, Shortfall};
use tilo_server::workspaces::WorkspaceSummary;
use tilo_server_protocol::WorkSetting;
use tilo_user_identity::DeviceCapabilities;

use crate::common::{
    Hosted, Linked, RECORDS_APP as APP, TestDevice, TestUser, holds_writes, keys_of, receives,
    serve_in, serve_on, write_sealed,
};
use tilo_server::WorkspaceId;

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

/// The changes to who is in `workspace` that `devices` signed, as `holder`
/// keeps its member list: what it admits, takes out or lets back, and every
/// governance change to device rows, invitations or the owner.
async fn membership_signed_by(
    holder: &Hosted,
    workspace: &str,
    devices: &[[u8; 32]],
) -> Vec<(
    tilo_peer_state::members::OpId,
    [u8; 32],
    MembershipOpCommand,
)> {
    let state = holder
        .host
        .workspaces()
        .state(workspace)
        .await
        .expect("the holder keeps the workspace");
    let doc = state
        .admitted_workspace()
        .expect("the workspace's member list is admitted");
    tilo_peer_state::members::ops(doc)
        .into_iter()
        .filter(|(_, op)| devices.contains(&op.author.device_public_key))
        .filter(|(_, op)| match &op.command {
            MembershipOpCommand::Governance { rows } => rows.iter().any(|row| {
                matches!(
                    row,
                    GovernanceRow::Identities | GovernanceRow::Membership | GovernanceRow::Owner
                )
            }),
            MembershipOpCommand::Compact { .. } => false,
            _ => true,
        })
        .map(|(id, op)| (id, op.author.device_public_key, op.command.clone()))
        .collect()
}

/// Wait until `hosted` admits `device` to `workspace`: the link it opens.
async fn member_at(hosted: &Hosted, device: &TestDevice, workspace: WorkspaceId) -> Linked {
    tilo_test_env::within("the host admits the member", async {
        loop {
            if let Ok(linked) = Linked::open_hosted(hosted, device, workspace).await {
                return linked;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
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
///
/// No person tells the servers to do any of it, so neither signs a change to
/// who is in the workspace: handed the workspace, each signs at most its own
/// row, and the catch-up, the reconnect and the repair after it add
/// no membership change of theirs to what any holder keeps.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(
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
            .enter(&workspace.workspace, (&device.cert, &device.sk))
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

    // Handed the workspace, each server signed its own row and nothing else.
    let servers = [nas.device_pubkey(), server.device_pubkey()];
    let handed =
        membership_signed_by(holders[0].as_ref().unwrap(), &workspace.workspace, &servers).await;
    for device in servers {
        let own: Vec<_> = handed.iter().filter(|(_, by, _)| *by == device).collect();
        assert!(
            own.len() <= 1
                && own.iter().all(|(_, _, command)| matches!(command,
                    MembershipOpCommand::Governance { rows } if rows == &[GovernanceRow::Identities])),
            "a server handed a workspace signs its own row and nothing else: {handed:?}"
        );
    }

    let away = server.device_pubkey();
    let state = holders[2].take().unwrap().stop_keeping().await;
    let on_laptop = member_at(holders[0].as_ref().unwrap(), &phone, workspace.workspace_id).await;
    // The phone seals the change with the keys Alice's devices share.
    let keys = keys_of(
        holders[0].as_ref().unwrap(),
        workspace.workspace_id,
        (APP, "note:away"),
    )
    .await;
    let mut document = AutoCommit::new();
    document
        .put(ROOT, "body", "while the server was away")
        .unwrap();
    let written = [write_sealed(
        &on_laptop,
        &keys,
        (APP, "note:away"),
        document.save(),
    )];
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
    let on_server = member_at(
        holders[2].as_ref().unwrap(),
        &tablet,
        workspace.workspace_id,
    )
    .await;
    // A member that reaches only the returned server gets the sealed write made while it was
    // away, as the bytes the other holders carried to it.
    receives(&on_server, |peer| {
        holds_writes(peer, (APP, "note:away"), &written)
    })
    .await;
    for holder in holders.iter().flatten() {
        copies_once(
            holder,
            &workspace.workspace,
            "the shortfall clears on every holder",
            |status| status.satisfied() && status.holders.iter().all(|holder| holder.current),
        )
        .await;
        assert_eq!(
            membership_signed_by(holder, &workspace.workspace, &servers).await,
            handed,
            "catching up, reconnecting and repairing, a server signs no membership change"
        );
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

/// Another person's server holds the workspace, reads and carries what it
/// decides, and is refused when it writes as a member: a naming lets a device
/// keep and carry a workspace, never write in it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_holder_of_another_person_carries_the_workspace_and_writes_no_member_change() {
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

    // It reads the workspace: the server it was made on is named among its
    // holders there.
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
    // It carries what the workspace decides.
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

    // It writes no member's change: holding makes nobody a member.
    let refused = within(
        "the holder writes no member's change",
        holder.host.workspaces().commit_own_change(
            &workspace.workspace,
            tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::Revoke {
                grant: tilo_action_policy::GrantId::new("none"),
            },
            None,
        ),
    )
    .await;
    assert!(
        refused.is_err(),
        "a holder writes no member's change: {refused:?}"
    );
    within("the holder stops", holder.stop()).await;
    within("the server it was made on stops", made_on.stop()).await;
}

/// What the holder answers `message` on `actions`, the link its asker holds to it.
async fn ask(actions: &RemoteDevice, message: RemoteActionMessage) -> RemoteActionMessage {
    tilo_test_env::within("the holder answers", actions.ask_message(message))
        .await
        .expect("the holder answers")
        .expect("the holder answers the request")
}

/// The ids of the actions the holder offers on `workspace`'s resource
/// `child` of its governance tree: what a path such as `workspace/servers`
/// is found among.
async fn offered(actions: &RemoteDevice, workspace: &WorkspaceSummary, child: &str) -> Vec<String> {
    let RemoteActionMessage::CatalogueResponse { result, .. } = ask(
        actions,
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

/// What the holder answers `action` of `workspace`'s governance with `argument`.
async fn govern(
    actions: &RemoteDevice,
    workspace: &WorkspaceSummary,
    action: &str,
    argument: serde_json::Value,
) -> Result<serde_json::Value, ActionCallError> {
    let serde_json::Value::Object(argument) = argument else {
        panic!("arguments are an object");
    };
    let message = RemoteActionMessage::Invoke(InvokeRequest {
        invocation: InvocationId::new(),
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
    let RemoteActionMessage::Receipt(receipt) = ask(actions, message).await else {
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
/// lists its servers there, which makes the workspace hers, and from her
/// computer, which keeps her root, takes the stopped one out; the second then
/// is the only server left and makes its base, and her computer still keeps
/// its own copy.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[allow(
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
    let laptop = alice.enrol("laptop", DeviceCapabilities::primary());
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
    let key_link = within("the remaining server admits the owner's key", async {
        loop {
            match Linked::open_hosted(&remains, &key, workspace.workspace_id).await {
                Ok(linked) => break linked,
                Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
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

    let (old_key, main_key, laptop_key) = (
        hex::encode(old.device_pubkey()),
        hex::encode(main.device_pubkey()),
        hex::encode(laptop.device_pubkey()),
    );
    // The workspace is still the server's, kept for Alice: its servers are
    // offered to her all the same, at the path she finds them by, or she
    // never reaches the choice that makes it hers.
    let servers = offered(&key_link.actions, &workspace, "servers").await;
    for action in ["list", "remove"] {
        assert!(
            servers.iter().any(|offered| offered == action),
            "the person it is kept for is offered workspace/servers#{action}: {servers:?}"
        );
    }
    let listed = govern(
        &key_link.actions,
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
    // She takes the stopped server out from her computer, which keeps her root: which servers
    // keep a workspace is changed under its key alone, never a server's. It comes into the
    // workspace on the remaining server as a further device of hers, by the certificate her root
    // signed.
    let laptop_state = tempfile::tempdir().unwrap();
    let (laptop_service, _laptop_keys) =
        crate::common::own_service(laptop_state.path(), &laptop).await;
    let own = laptop_service.workspaces().clone();
    tilo_server::servers::found(&own)
        .await
        .expect("Alice's own workspace is founded on her computer");
    tilo_workspace_service::workspaces::person_workspace::devices::mark_root_holder(
        &own,
        &alice.user_sk,
        &laptop.cert,
    )
    .await
    .expect("her computer keeps her root");
    let main_hint = workspaces
        .hosts(&workspace.workspace)
        .await
        .unwrap()
        .into_iter()
        .find(|hint| hint.host == main.cert)
        .expect("the remaining server names its own address");
    within(
        "her computer holds the workspace",
        own.hold_as_member(workspace.workspace_id, &main_hint),
    )
    .await
    .expect("the remaining server admits her computer");
    let removed = crate::common::servers_action(
        &own,
        &laptop,
        &workspace.workspace,
        "remove",
        serde_json::json!({ "server": old_key }),
    )
    .await
    .expect("her computer takes the stopped server out");
    // Main is the only server left; her computer still keeps its own copy.
    let servers: Vec<_> = removed["servers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|server| {
            (
                server["device"].as_str().unwrap(),
                server["here"].as_bool().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        servers,
        [(laptop_key.as_str(), true), (main_key.as_str(), false)],
        "{removed}"
    );
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
    drop(own);
    laptop_service.shutdown().await;
    drop(key_link);
    remains.stop().await;
}
