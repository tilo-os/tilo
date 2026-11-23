//! The resource against a recording engine: each verb reaches the engine as
//! the certified Files route in the workspace in front, and each row it acts
//! on is one the reading listed.

use super::*;

use runtime_collab::{
    NearbyDestinationHandle, NearbyTransferDestination, NearbyTransferSnapshot,
    NearbyTransferStatus, NearbyTransferTarget,
};

/// The device's one destination and one target, and the transfer handles it
/// issues, fixed so a row's handle is one a test can name.
const TARGET: [u8; 16] = [0x11; 16];
const DESTINATION: [u8; 16] = [0x22; 16];
const TRANSFER: [u8; 16] = [0x33; 16];

#[derive(Default)]
struct Fake {
    calls: Vec<(NearbyTransferRequestContext, NearbyTransferCommand)>,
    phase: Option<NearbyTransferPhase>,
    source: Vec<u8>,
    airdrop: bool,
}

impl Fake {
    fn calls(&self) -> Vec<&NearbyTransferCommand> {
        self.calls.iter().map(|(_, command)| command).collect()
    }
}

struct Recording {
    fake: Mutex<Fake>,
}

impl NearbyTransferServices for Recording {
    fn command(
        &self,
        context: &NearbyTransferRequestContext,
        command: NearbyTransferCommand,
    ) -> Result<NearbyTransferReply, String> {
        let mut fake = self.fake.lock_recover();
        fake.calls.push((context.clone(), command.clone()));
        let reply = match command {
            NearbyTransferCommand::Snapshot => {
                let transfers = fake.phase.map_or_else(Vec::new, |phase| {
                    vec![NearbyTransferStatus {
                        handle: NearbyTransferHandle::from_issued_bytes(TRANSFER),
                        name: "notes.txt".to_owned(),
                        device_label: "Ada's laptop".to_owned(),
                        direction: NearbyTransferDirection::Sending,
                        phase,
                        transferred_bytes: 512,
                        total_bytes: 1024,
                    }]
                });
                NearbyTransferReply::Snapshot {
                    snapshot: NearbyTransferSnapshot {
                        targets: vec![NearbyTransferTarget {
                            handle: NearbyTargetHandle::from_issued_bytes(TARGET),
                            label: "Ada's laptop".to_owned(),
                            online: true,
                        }],
                        destinations: vec![NearbyTransferDestination {
                            handle: NearbyDestinationHandle::from_issued_bytes(DESTINATION),
                            label: "Documents".to_owned(),
                            remembered_for_session: false,
                        }],
                        transfers,
                    },
                }
            }
            NearbyTransferCommand::AirDropAvailability => {
                NearbyTransferReply::AirDropAvailability {
                    available: fake.airdrop,
                }
            }
            NearbyTransferCommand::Send { source_path, .. }
            | NearbyTransferCommand::SendAirDrop { source_path, .. } => {
                fake.phase = Some(NearbyTransferPhase::AwaitingConsent);
                fake.source = source_path;
                NearbyTransferReply::Queued {
                    handle: NearbyTransferHandle::from_issued_bytes(TRANSFER),
                }
            }
            NearbyTransferCommand::Decide { accept, .. } => {
                fake.phase = Some(if accept {
                    NearbyTransferPhase::Completed
                } else {
                    NearbyTransferPhase::Cancelled
                });
                NearbyTransferReply::Accepted
            }
            NearbyTransferCommand::Cancel { .. } => {
                fake.phase = Some(NearbyTransferPhase::Cancelled);
                NearbyTransferReply::Accepted
            }
            NearbyTransferCommand::Retry { .. } => {
                fake.phase = Some(NearbyTransferPhase::Transferring);
                NearbyTransferReply::Accepted
            }
        };
        drop(fake);
        Ok(reply)
    }

    fn disconnect(&self, _peer_id: u64) {}
}

/// A nearby send publishing `workspace`, sharing through `shared`.
fn nearby(fake: &Arc<Recording>, workspace: &str, shared: Arc<Mutex<Vec<String>>>) -> NearbySend {
    NearbySend::new(
        Arc::clone(fake) as Arc<dyn NearbyTransferServices>,
        "Ada",
        {
            let workspace = workspace.to_owned();
            move || workspace.clone()
        },
        move |resource| {
            shared.lock_recover().push(resource.to_owned());
            Ok(())
        },
    )
}

fn recording(airdrop: bool) -> Arc<Recording> {
    Arc::new(Recording {
        fake: Mutex::new(Fake {
            airdrop,
            ..Fake::default()
        }),
    })
}

fn arguments(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        other => panic!("{other} is not an arguments object"),
    }
}

#[test]
fn the_reading_lists_the_devices_in_reach_the_folders_here_and_what_moved() {
    let fake = recording(false);
    let shared = Arc::new(Mutex::new(Vec::new()));
    let nearby = nearby(&fake, "workspace-a", Arc::clone(&shared));

    let reading = nearby.read().expect("the engine shows what it holds");
    assert_eq!(reading.devices.len(), 1);
    assert_eq!(reading.devices[0].name, "Ada's laptop");
    assert_eq!(reading.devices[0].kind, DeviceKind::Device);
    assert!(reading.devices[0].reachable);
    assert_eq!(reading.devices[0].handle, hex(&TARGET));
    assert_eq!(reading.destinations.len(), 1);
    assert_eq!(reading.destinations[0].name, "Documents");
    assert!(reading.transfers.is_empty());

    // Every call arrives as the certified Files route, in the workspace in
    // front.
    let fake = fake.fake.lock_recover();
    let (context, _) = &fake.calls[0];
    assert_eq!(context.app_id(), FILES_APP_ID);
    assert_eq!(context.workspace_id(), "workspace-a");
    assert_eq!(context.person(), Some("Ada"));
    drop(fake);
}

#[test]
fn a_send_then_an_accept_hands_the_file_over_and_the_row_says_so() {
    let fake = recording(false);
    let shared = Arc::new(Mutex::new(Vec::new()));
    let mut nearby = nearby(&fake, "workspace-a", shared);

    let reading = nearby.read().expect("a reading");
    let device = reading.devices[0].handle.clone();
    nearby
        .invoke(
            RESOURCE,
            "send",
            &arguments(serde_json::json!({
                "items": ["/home/ada/Documents/notes.txt"],
                "device": device,
            })),
        )
        .expect("the send is offered");
    {
        let fake = fake.fake.lock_recover();
        let sent = fake
            .calls()
            .into_iter()
            .find_map(|command| match command {
                NearbyTransferCommand::Send {
                    target,
                    source_path,
                } => Some((*target, source_path.clone())),
                _ => None,
            })
            .expect("the engine is asked to send");
        assert_eq!(sent.0, NearbyTargetHandle::from_issued_bytes(TARGET));
        assert_eq!(sent.1, b"/home/ada/Documents/notes.txt");
        drop(fake);
    }

    let reading = nearby.read().expect("a reading");
    assert_eq!(reading.transfers.len(), 1);
    let row = &reading.transfers[0];
    assert_eq!(row.state, TransferState::Offered);
    assert_eq!(row.name, "notes.txt");
    assert_eq!(row.progress, 50);

    let destination = reading.destinations[0].handle.clone();
    nearby
        .invoke(
            RESOURCE,
            "accept",
            &arguments(serde_json::json!({
                "transfer": row.handle,
                "destination": destination,
            })),
        )
        .expect("the offer is kept");
    {
        let fake = fake.fake.lock_recover();
        let decided = fake
            .calls()
            .into_iter()
            .find_map(|command| match command {
                NearbyTransferCommand::Decide {
                    handle,
                    accept,
                    destination,
                    ..
                } => Some((*handle, *accept, *destination)),
                _ => None,
            })
            .expect("the engine is asked to decide");
        assert_eq!(decided.0, NearbyTransferHandle::from_issued_bytes(TRANSFER));
        assert!(decided.1);
        assert_eq!(
            decided.2,
            Some(NearbyDestinationHandle::from_issued_bytes(DESTINATION))
        );
        drop(fake);
    }
    let reading = nearby.read().expect("a reading");
    assert_eq!(reading.transfers[0].state, TransferState::Done);
    assert_eq!(reading.transfers[0].destination, "Documents");
}

#[test]
fn an_accept_without_a_folder_takes_the_one_the_engine_remembers() {
    let fake = recording(false);
    let shared = Arc::new(Mutex::new(Vec::new()));
    let mut nearby = nearby(&fake, "workspace-a", shared);
    let row = {
        let reading = nearby.read().expect("a reading");
        let device = reading.devices[0].handle.clone();
        nearby
            .invoke(
                RESOURCE,
                "send",
                &arguments(serde_json::json!({
                    "items": ["/home/ada/notes.txt"],
                    "device": device,
                })),
            )
            .expect("a send");
        nearby.read().expect("a reading").transfers[0]
            .handle
            .clone()
    };
    nearby
        .invoke(
            RESOURCE,
            "accept",
            &arguments(serde_json::json!({ "transfer": row })),
        )
        .expect("the remembered folder takes it");
    let fake = fake.fake.lock_recover();
    assert!(fake.calls().into_iter().any(|command| matches!(
        command,
        NearbyTransferCommand::Decide {
            accept: true,
            destination: Some(_),
            ..
        }
    )));
    drop(fake);
}

#[test]
fn a_decline_turns_the_offer_down() {
    let fake = recording(false);
    let shared = Arc::new(Mutex::new(Vec::new()));
    let mut nearby = nearby(&fake, "workspace-a", shared);
    let device = nearby.read().expect("a reading").devices[0].handle.clone();
    nearby
        .invoke(
            RESOURCE,
            "send",
            &arguments(serde_json::json!({
                "items": ["/home/ada/notes.txt"],
                "device": device,
            })),
        )
        .expect("a send");
    let row = nearby.read().expect("a reading").transfers[0]
        .handle
        .clone();
    nearby
        .invoke(
            RESOURCE,
            "decline",
            &arguments(serde_json::json!({ "transfer": row })),
        )
        .expect("the offer is turned down");
    {
        let calls = fake.fake.lock_recover();
        assert!(calls.calls().into_iter().any(|command| matches!(
            command,
            NearbyTransferCommand::Decide {
                accept: false,
                destination: None,
                ..
            }
        )));
        drop(calls);
    }
    assert_eq!(
        nearby.read().expect("a reading").transfers[0].state,
        TransferState::Declined
    );
}

#[test]
fn a_cancel_stops_a_transfer_and_a_retry_offers_it_again() {
    let fake = recording(false);
    let shared = Arc::new(Mutex::new(Vec::new()));
    let mut nearby = nearby(&fake, "workspace-a", shared);
    let device = nearby.read().expect("a reading").devices[0].handle.clone();
    nearby
        .invoke(
            RESOURCE,
            "send",
            &arguments(serde_json::json!({
                "items": ["/home/ada/notes.txt"],
                "device": device,
            })),
        )
        .expect("a send");
    let row = nearby.read().expect("a reading").transfers[0]
        .handle
        .clone();
    nearby
        .invoke(
            RESOURCE,
            "cancel",
            &arguments(serde_json::json!({ "transfer": row.as_str() })),
        )
        .expect("the transfer stops");
    assert_eq!(
        nearby.read().expect("a reading").transfers[0].state,
        TransferState::Declined
    );
    nearby
        .invoke(
            RESOURCE,
            "retry-transfer",
            &arguments(serde_json::json!({ "transfer": row })),
        )
        .expect("the transfer is offered again");
    assert_eq!(
        nearby.read().expect("a reading").transfers[0].state,
        TransferState::Sending
    );
    let fake = fake.fake.lock_recover();
    assert!(fake.calls().into_iter().any(|command| matches!(
        command,
        NearbyTransferCommand::Cancel { .. } | NearbyTransferCommand::Retry { .. }
    )));
    drop(fake);
}

#[test]
fn the_lab_receiver_is_a_device_kind_and_a_send_to_it_names_its_address() {
    let fake = recording(true);
    let shared = Arc::new(Mutex::new(Vec::new()));
    let mut nearby = nearby(&fake, "workspace-a", shared).with_lab_receiver(LabReceiver {
        name: "Lab receiver".to_owned(),
        address: "10.0.0.9".to_owned(),
        port: 4443,
    });
    let reading = nearby.read().expect("a reading");
    let lab = reading
        .devices
        .iter()
        .find(|device| device.kind == DeviceKind::LabReceiver)
        .expect("the lab receiver is listed");
    assert_eq!(lab.name, "Lab receiver");
    assert!(lab.reachable);
    assert_eq!(lab.address, "10.0.0.9");
    assert_eq!(lab.port, 4443);
    nearby
        .invoke(
            RESOURCE,
            "send",
            &arguments(serde_json::json!({
                "items": ["/home/ada/notes.txt"],
                "device": lab.handle,
            })),
        )
        .expect("the lab receiver takes it");
    let fake = fake.fake.lock_recover();
    let handed = fake
        .calls()
        .into_iter()
        .find_map(|command| match command {
            NearbyTransferCommand::SendAirDrop {
                source_path,
                address,
                port,
            } => Some((source_path.clone(), address.clone(), *port)),
            _ => None,
        })
        .expect("the engine is asked to hand it to the address");
    assert_eq!(handed.0, b"/home/ada/notes.txt");
    assert_eq!(handed.1, "10.0.0.9");
    assert_eq!(handed.2, 4443);
    drop(fake);
}

#[test]
fn a_lab_receiver_that_is_not_here_refuses_the_send() {
    let fake = recording(false);
    let shared = Arc::new(Mutex::new(Vec::new()));
    let mut nearby = nearby(&fake, "workspace-a", shared).with_lab_receiver(LabReceiver {
        name: "Lab receiver".to_owned(),
        address: "10.0.0.9".to_owned(),
        port: 4443,
    });
    let reading = nearby.read().expect("a reading");
    let lab = reading
        .devices
        .iter()
        .find(|device| device.kind == DeviceKind::LabReceiver)
        .expect("the lab receiver is listed");
    assert!(!lab.reachable);
    let refused = nearby
        .invoke(
            RESOURCE,
            "send",
            &arguments(serde_json::json!({
                "items": ["/home/ada/notes.txt"],
                "device": lab.handle,
            })),
        )
        .expect_err("an absent receiver takes nothing");
    assert!(
        matches!(refused, ApiError::Refused { .. }),
        "{refused:?} is not a refusal"
    );
}

#[test]
fn share_opens_the_invitation_sheet_for_the_resource_it_names() {
    let fake = recording(false);
    let shared = Arc::new(Mutex::new(Vec::new()));
    let mut nearby = nearby(&fake, "workspace-a", Arc::clone(&shared));
    nearby
        .invoke(
            RESOURCE,
            "share",
            &arguments(serde_json::json!({ "resource": "notes:note:42" })),
        )
        .expect("the sheet opens");
    assert_eq!(
        *shared.lock_recover(),
        ["notes:note:42".to_owned()],
        "the sheet names the resource it shares"
    );
}

#[test]
fn a_handle_the_reading_did_not_list_is_refused_and_never_reaches_the_engine() {
    let fake = recording(false);
    let shared = Arc::new(Mutex::new(Vec::new()));
    let mut nearby = nearby(&fake, "workspace-a", shared);
    let before = fake.fake.lock_recover().calls.len();
    let refused = nearby
        .invoke(
            RESOURCE,
            "cancel",
            &arguments(serde_json::json!({ "transfer": "not-a-handle" })),
        )
        .expect_err("a handle no reading listed names nothing");
    assert!(
        matches!(refused, ApiError::InvalidArguments(_)),
        "{refused:?}"
    );
    assert_eq!(fake.fake.lock_recover().calls.len(), before);
}

#[test]
fn a_device_no_reading_listed_is_refused() {
    let fake = recording(false);
    let shared = Arc::new(Mutex::new(Vec::new()));
    let mut nearby = nearby(&fake, "workspace-a", shared);
    let refused = nearby
        .invoke(
            RESOURCE,
            "send",
            &arguments(serde_json::json!({
                "items": ["/home/ada/notes.txt"],
                "device": hex(&[0x44; 16]),
            })),
        )
        .expect_err("a device that was never in reach takes nothing");
    assert!(matches!(refused, ApiError::Refused { .. }), "{refused:?}");
}
