//! A workspace says how many copies it wants, and its holders repair each
//! other: one that was away takes in what the others hold without any
//! member's help, and every holder says which of them is short of what
//! until it has.

use tilo_peer_state::{CopyPolicy, CopyRole, CopyStatus, Missing, Shortfall};
use tilo_server::Envelope;
use tilo_server_protocol::WorkSetting;
use tilo_user_identity::DeviceCapabilities;

use crate::common::{Hosted, TestUser, host_in, host_on, receives, session_to};

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
        holders.push(Some(host_on(&alice, device, work.clone()).await));
    }
    let made_on = holders[0].as_ref().unwrap().host.workspaces().clone();
    let workspace = made_on.create("Notes").await.unwrap();
    for at in 1..3 {
        made_on
            .attach_server(
                &workspace.workspace,
                devices[at].device_pubkey(),
                holders[at].as_ref().unwrap().host.node_addr(),
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

    holders[2] = Some(host_in(&alice, &server, settings[2].clone(), state).await);
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
