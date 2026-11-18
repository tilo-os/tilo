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
#[expect(
    clippy::significant_drop_tightening,
    reason = "Hosted::stop consumes each holder after the assertions"
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
#[expect(
    clippy::significant_drop_tightening,
    reason = "Hosted::stop consumes each holder after the assertions"
)]
async fn the_server_a_workspace_was_made_on_is_named_among_its_holders() {
    let site = TestUser::fresh("Site");
    let made_on_server = site.enrol("server", DeviceCapabilities::member());
    let other = TestUser::fresh("Other");
    let other_server = other.enrol("server", DeviceCapabilities::member());
    let made_on = serve_on(&site, &made_on_server, WorkSetting::own_person()).await;
    let workspaces = made_on.host.workspaces().clone();
    let workspace = workspaces.create("Notes").await.unwrap();
    let mut takes = WorkSetting::own_person();
    takes.from =
        tilo_server_protocol::WorkFrom::Workspaces([workspace.workspace_id].into_iter().collect());
    let holder = serve_on(&other, &other_server, takes).await;
    workspaces
        .attach_server(
            &workspace.workspace,
            other_server.device_pubkey(),
            holder.host.node_addr(),
            CopyRole::KeyHolder,
        )
        .await
        .unwrap();
    let named = holder
        .host
        .workspaces()
        .hosts(&workspace.workspace)
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
    workspaces
        .choose_copies(&workspace.workspace, Some(wanted))
        .await
        .unwrap();
    copies_once(
        &holder,
        &workspace.workspace,
        "the holder takes in what the server it was made on decides",
        |status| status.chosen && status.policy == wanted,
    )
    .await;
    holder.stop().await;
    made_on.stop().await;
}
