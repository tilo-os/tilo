//! Speaker groups between two of the person's devices: the group one leads, the consent the
//! other gives and the playback it receives are typed actions on the record of their devices, and
//! each session reads and writes its part there.

#![cfg(target_os = "linux")]

use crate::common::people::{Device, Person, Role, Site, join_together};
use serde_json::json;
use tilo_actions::ActionAddress;
use tilo_actions::remote::RemoteActionTarget;
use tilo_session::platform::fabric::{DeviceId, EndpointSelector};
use tilo_workspace_service::workspaces::person_workspace::speaker::{
    SpeakerEndpoint, SpeakerLedMember,
};

/// The key a device writes the record under, lowercase hex.
fn key(device: &Device) -> String {
    hex::encode(device.cert.device_public_key)
}

/// The person's devices record, as `device` reads it.
fn record_of(device: &Device) -> serde_json::Value {
    let workspace =
        tilo_server_protocol::servers::person_workspace_id(&device.cert.user_public_key);
    let read = device
        .read(&workspace, ("so.tilo.devices", "devices"), None, None)
        .unwrap_or_else(|error| {
            panic!(
                "{} reads the person's devices: {error}\n{}",
                device.label,
                device.diagnostic()
            )
        });
    tilo_server_protocol::devices::record_value(&read)
}

/// Apply one typed action to the person's devices record through `device`'s daemon.
fn write(device: &Device, action: &str, arguments: serde_json::Value) {
    let target = RemoteActionTarget {
        provider: "record:so.tilo.devices:devices".into(),
        address: ActionAddress::new("tilo.devices", action),
    };
    let answer = device.act_wait(&target, &arguments);
    assert!(
        answer.get("err").is_none(),
        "{} ran {action}: {answer}\n{}",
        device.label,
        device.diagnostic()
    );
}

fn output(device: &Device) -> EndpointSelector {
    EndpointSelector::mint(
        &DeviceId::new(key(device)).expect("a device id"),
        "speakers",
    )
}

/// Say the one output `device` offers, as its compositor does.
fn offer_output(device: &Device) {
    let endpoints = vec![SpeakerEndpoint {
        selector: output(device).to_hex(),
        microphone: false,
        label: "Speakers".into(),
        is_default: true,
    }];
    write(
        device,
        "say-speaker-endpoints",
        json!({
            "device": key(device),
            "endpoints": hex::encode(postcard::to_allocvec(&endpoints).expect("endpoints encode")),
        }),
    );
}

/// The playback entries the record holds, by their route.
fn playbacks(record: &serde_json::Value) -> Vec<(String, serde_json::Value)> {
    let Some(entries) = record
        .get("speaker_playbacks")
        .and_then(serde_json::Value::as_object)
    else {
        return Vec::new();
    };
    entries
        .iter()
        .map(|(route, entry)| {
            (
                route.clone(),
                entry.get("v").cloned().unwrap_or_else(|| entry.clone()),
            )
        })
        .collect()
}

/// Two devices of one person: the laptop leads a group, and the shelf joined to it receives the
/// playback the laptop offers.
#[test]
fn a_group_led_on_one_device_plays_on_another_of_their_devices() {
    let ada = Person::new("Ada");
    let site = Site::start(&ada, "Studio");
    site.publish(Role::Editor);
    let mut laptop = ada.device("Ada's laptop");
    let mut shelf = ada.device("Ada's shelf");
    let hers = site.admit(ada.public);
    let its = site.admit(ada.public);
    join_together(
        &mut [&mut laptop, &mut shelf],
        &[hers.as_str(), its.as_str()],
    );
    for device in [&laptop, &shelf] {
        device.wait_holding(&site.workspace);
        device.wait_showing(&site.workspace);
    }

    // Both devices are in the person's record, which each of them reads.
    let (laptop_key, shelf_key) = (key(&laptop), key(&shelf));
    tilo_test_env::wait_until("both devices are in the person's record", || {
        let record = record_of(&laptop);
        let devices = record.get("devices").and_then(serde_json::Value::as_object);
        devices.is_some_and(|devices| {
            devices.contains_key(&laptop_key) && devices.contains_key(&shelf_key)
        })
    });

    // Each device offers its one output, as its compositor does.
    offer_output(&laptop);
    offer_output(&shelf);

    // The laptop leads a pair: its left side here, the shelf's right side there.
    let group = [5; 16];
    let members = vec![
        SpeakerLedMember {
            device: laptop_key.clone(),
            output: output(&laptop).to_hex(),
            role: "left".into(),
            ..SpeakerLedMember::default()
        },
        SpeakerLedMember {
            device: shelf_key.clone(),
            output: output(&shelf).to_hex(),
            role: "right".into(),
            ..SpeakerLedMember::default()
        },
    ];
    write(
        &laptop,
        "lead-speaker-group",
        json!({
            "device": laptop_key,
            "group": hex::encode(group),
            "name": "Living room",
            "members": hex::encode(postcard::to_allocvec(&members).expect("members encode")),
            "at_ms": 1,
        }),
    );

    // The shelf agrees to play its right side of that group: the person's yes on that device.
    write(
        &shelf,
        "consent-speaker-group",
        json!({
            "device": shelf_key,
            "group": hex::encode(group),
            "leader": laptop_key,
            "play": true,
            "endpoint": output(&shelf).to_hex(),
            "role": "right",
            "approved": true,
            "at_ms": 2,
        }),
    );

    // The laptop plays to the pair: one offer to the shelf for its side.
    let session = [9; 16];
    let route = tilo_speaker_service::groups::member_route_id(
        session,
        &DeviceId::new(shelf_key.clone()).expect("a device id"),
        output(&shelf),
    );
    write(
        &laptop,
        "offer-speaker-playback",
        json!({
            "route": hex::encode(route),
            "session": hex::encode(session),
            "source": laptop_key,
            "target": shelf_key,
            "group": hex::encode(group),
            "at_ms": 3,
        }),
    );

    // The shelf's own session admits it, with a capability bound to that exact source.
    let (admitted_route, admitted) =
        tilo_test_env::wait_for("the shelf admits the laptop's playback", || {
            playbacks(&record_of(&laptop))
                .into_iter()
                .find(|(_, playback)| {
                    playback["stage"].as_str() == Some("admitted")
                        && playback["source"].as_str() == Some(laptop_key.as_str())
                        && playback["target"].as_str() == Some(shelf_key.as_str())
                })
        });
    let authorization = admitted["authorization"].as_str().unwrap_or_default();
    assert_eq!(
        authorization.len(),
        32,
        "the shelf's capability: {admitted}"
    );
    assert_eq!(
        admitted_route,
        hex::encode(route),
        "the route the shelf admitted"
    );
    assert_eq!(
        admitted["session"].as_str().unwrap_or_default(),
        hex::encode(session)
    );
}
