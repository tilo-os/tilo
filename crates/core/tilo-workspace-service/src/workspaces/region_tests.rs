//! A person's zone is a resource of their own workspace: a device of theirs that holds that
//! workspace reads the zone another device of theirs chose there, and a device that holds
//! nothing of theirs reads none.

use tilo_actions::remote::{InvokeArgument, InvokeRequest, ReceiptStatus};
use tilo_user_identity::DeviceCapabilities;

use super::fixtures::{
    caller, device_cert, group_keys, hosting_device, persons_root, reach_device_at,
    test_host_holding,
};
use super::{Workspaces, person_workspace, servers};

/// The zone `workspaces` reads for `person`, or none before its device holds their own
/// workspace.
async fn zone_of(workspaces: &Workspaces, person: &[u8; 32]) -> Option<String> {
    workspaces.region(person).await.and_then(|(_, zone)| zone)
}

/// A zone chosen through one device of the person is the zone their other device reads: the
/// chosen zone is their own workspace's, and the device that chooses it answers with it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_zone_chosen_on_one_device_is_the_zone_another_device_of_theirs_reads() {
    const CHOSEN: &str = "Europe/Vienna";
    let laptop_state = tempfile::tempdir().unwrap();
    let (laptop_key, laptop_cert) = hosting_device();
    let laptop =
        test_host_holding(laptop_state.path(), &laptop_key, &laptop_cert, group_keys()).await;
    let person = laptop_cert.user_public_key;
    let own = laptop.workspaces();
    let own_id = person_workspace::found(own)
        .await
        .expect("the laptop founds her own workspace");
    // Another device of hers, holding none of her workspaces: it reads no zone of hers.
    let (desk_key, desk_cert) =
        device_cert(&persons_root(), "desktop", DeviceCapabilities::primary());
    let desk_state = tempfile::tempdir().unwrap();
    let desk = test_host_holding(desk_state.path(), &desk_key, &desk_cert, group_keys()).await;
    let reading = desk.workspaces();
    assert_eq!(
        zone_of(reading, &person).await,
        None,
        "the desktop reads no zone before it holds her own workspace"
    );

    // Handed her own workspace as any workspace of hers, the desktop reaches her laptop there
    // and chooses the zone on it.
    servers::admit(own, &desk_cert, &desk.node_addr())
        .await
        .expect("the laptop names her desktop among her servers");
    servers::attach::attach_named(own)
        .await
        .expect("the laptop hands it over");
    // The hand-over answers once the desktop's copy followed its frames.
    assert!(
        person_workspace::holds_person_workspace(reading).await,
        "the desktop holds her own workspace the laptop handed it"
    );
    let on_laptop = reach_device_at(reading, &laptop.node_addr(), &own_id);
    let answer = tilo_test_env::within(
        "the laptop answers the choice",
        tilo_actions::remote::ActionProvider::invoke(
            &on_laptop,
            &caller(&desk_cert),
            InvokeRequest {
                asks: false,
                invocation: tilo_actions::InvocationId::new(),
                target: tilo_action_services::formats::target(
                    tilo_action_services::formats::CHOOSE_ZONE,
                ),
                argument: InvokeArgument::Json(
                    serde_json::json!({ "zone": CHOSEN })
                        .as_object()
                        .cloned()
                        .expect("an object"),
                ),
                context: None,
                confirmation: None,
                revision: None,
                on_behalf_of: None,
                wait_ms: None,
                appearance: None,
                signed: None,
            },
        ),
    )
    .await
    .expect("the laptop answers")
    .expect("the choice is kept");
    let ReceiptStatus::Completed { output } = answer.status else {
        panic!("the choice is kept: {:?}", answer.status);
    };
    assert_eq!(output.value.expect("the chosen zone")["chosenZone"], CHOSEN);

    // The laptop reads the zone her desktop chose, and so does the desktop itself once the
    // choice reaches it.
    assert_eq!(zone_of(own, &person).await.as_deref(), Some(CHOSEN));
    tilo_test_env::within("the desktop to read the zone it chose", async {
        while zone_of(reading, &person).await.as_deref() != Some(CHOSEN) {
            tokio::task::yield_now().await;
        }
    })
    .await;
    drop(on_laptop);
    desk.shutdown().await;
    laptop.shutdown().await;
}
