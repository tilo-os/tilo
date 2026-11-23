//! A person's region and zone follow them from device to device: a zone chosen through one
//! device of theirs is a resource of their own workspace, which every other device of theirs
//! reads once it holds that workspace.

#![allow(
    clippy::significant_drop_tightening,
    reason = "the devices and routers a test starts stay alive until each test ends"
)]

use serde_json::json;
use tilo_actions::InvocationId;
use tilo_actions::remote::{
    InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionFrame, RemoteActionMessage, formats,
};
use tilo_server::WorkspaceId;
use tilo_server::servers;
use tilo_server::transport::{Envelope, REMOTE_ACTION_ENVELOPE_VERSION};
use tilo_server_protocol::WorkSetting;
use tilo_user_identity::DeviceCapabilities;

use crate::common::{TestUser, serve_on};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_zone_chosen_on_one_device_is_the_zone_another_device_of_theirs_reads() {
    const CHOSEN: &str = "Europe/Vienna";
    let alice = TestUser::fresh("Alice");
    let laptop = alice.enrol("laptop", DeviceCapabilities::primary());
    let desktop = alice.enrol("desktop", DeviceCapabilities::member());
    let on_laptop = serve_on(&alice, &laptop, WorkSetting::own_person()).await;
    let on_desktop = serve_on(&alice, &desktop, WorkSetting::own_person()).await;
    // The laptop made her root, so it founds her own workspace; the desktop holds none yet.
    servers::found(on_laptop.host.workspaces()).await.unwrap();

    // Her page, in a workspace the laptop serves, chooses a zone through the laptop.
    let team = on_laptop.host.workspaces().create("Team").await.unwrap();
    let workspace_id = WorkspaceId::parse(&team.workspace).unwrap();
    let client = on_laptop
        .host
        .connect_local(
            laptop.cert.clone(),
            laptop.sk.clone(),
            tilo_user_identity::RevocationList::new(),
        )
        .await
        .unwrap();
    client.send(Envelope::Subscribe { workspace_id }).unwrap();
    tilo_test_env::within("the workspace is served here", async {
        while !matches!(
            client.recv().await.expect("the session stays open"),
            Envelope::Ready { .. }
        ) {}
    })
    .await;
    let serde_json::Value::Object(argument) = json!({"zone": CHOSEN}) else {
        unreachable!()
    };
    let message = RemoteActionMessage::Invoke(InvokeRequest {
        invocation: InvocationId::new(),
        target: formats::target(formats::CHOOSE_ZONE),
        argument: InvokeArgument::Json(argument),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    });
    client
        .send(Envelope::RemoteAction {
            workspace_id,
            version: REMOTE_ACTION_ENVELOPE_VERSION,
            to_device_public_key: laptop.device_pubkey(),
            from_device_public_key: [0; 32],
            from_user_public_key: [0; 32],
            frame: RemoteActionFrame::new(message).encode().unwrap(),
        })
        .unwrap();
    let answer = tilo_test_env::within("the laptop answers the choice", async {
        loop {
            match client.recv().await.expect("the session stays open") {
                Envelope::RemoteAction { frame, .. } => {
                    break RemoteActionFrame::decode(&frame).unwrap().message;
                }
                Envelope::RemoteActionUndeliverable { refusal, .. } => panic!("{refusal}"),
                _ => {}
            }
        }
    })
    .await;
    let RemoteActionMessage::Receipt(receipt) = answer else {
        panic!("an invocation is answered with a receipt: {answer:?}");
    };
    let ReceiptStatus::Completed { output } = receipt.status else {
        panic!("the choice is kept: {:?}", receipt.status);
    };
    assert_eq!(output.value.unwrap()["chosenZone"], CHOSEN);
    let zone_on = |hosted: &crate::common::Hosted| {
        let workspaces = hosted.host.workspaces().clone();
        let person = alice.user_pk;
        async move { workspaces.region(&person).await.and_then(|(_, zone)| zone) }
    };
    assert_eq!(zone_on(&on_laptop).await.as_deref(), Some(CHOSEN));
    assert_eq!(
        zone_on(&on_desktop).await,
        None,
        "the desktop reads nothing before it holds her own workspace"
    );

    // The desktop is handed her own workspace, as any workspace of hers, and reads the zone
    // she chose there.
    crate::common::join_person_workspace(&on_laptop, &desktop, &on_desktop).await;
    tilo_test_env::within("the desktop to read the zone chosen on the laptop", async {
        while zone_on(&on_desktop).await.as_deref() != Some(CHOSEN) {
            tokio::task::yield_now().await;
        }
    })
    .await;
}
