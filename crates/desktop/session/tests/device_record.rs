//! Devices and access reads the person's record, not this device's fleet.
//!
//! The person's hosts keep the keys their root certified beside their
//! workspace list, and every device of theirs lists those keys: a browser a
//! page's root approved in its own tab, a phone paired on their desktop. A
//! device's own fleet is not that list — a key the root certified elsewhere is
//! in no fleet of theirs but the one that signed it — so Settings reads the
//! record, and a pairing says the list again even though no workspace moved.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and a device lives until its session ends"
)]

use crate::common::people::{Device, Person, Site, join_together};
use serde_json::{Value, json};
use tilo_server_protocol::workspaces::Role;

/// The devices Settings lists on `device`: the record of the person's devices, read through the
/// record API of the device's session, as every list of them reads it.
fn listed(device: &Device) -> Vec<Value> {
    use tilo_server_protocol::devices::{DEVICES, FleetRecords, fleet, record_value};
    let person = device.cert.user_public_key;
    let workspace = tilo_action_policy::roles::person_workspace_id(&person);
    let read = device
        .read(&workspace, DEVICES, None, None)
        .unwrap_or_else(|error| {
            panic!(
                "{} reads the person's devices: {error}\n{}",
                device.label,
                device.diagnostic()
            )
        });
    let records = FleetRecords {
        devices: record_value(&read),
        ..FleetRecords::default()
    };
    fleet(&person, &device.cert.device_public_key, &records, &[])
        .into_iter()
        .map(|listed| serde_json::to_value(listed).expect("a listed device is JSON"))
        .collect()
}

/// Whether `listed` holds a device called `label`.
fn lists(listed: &[Value], label: &str) -> bool {
    listed
        .iter()
        .any(|device| device["label"].as_str() == Some(label))
}

/// The person's root certifies a browser in its own tab, as the page holding
/// the root approves one: a key of theirs no device of theirs but that browser
/// holds.
#[test]
fn a_browser_approved_on_the_web_is_in_the_desktop_s_settings_list() {
    let ada = Person::new("Ada");
    let site = Site::start(&ada, "Studio");
    site.publish(Role::Editor);
    let mut desktop = ada.device("Ada's desktop");
    desktop.join(&site.admit(ada.public));
    desktop.wait_holding(&site.workspace);
    desktop.wait_showing(&site.workspace);

    // The desktop lists itself: the record holds the key its root certified
    // for it, and this is the device the person is using.
    let own = listed(&desktop);
    assert!(
        own.iter()
            .any(|device| device["label"] == "Ada's desktop" && device["current"] == true),
        "{own:?}"
    );

    // The page's root certifies the browser it runs in, and the server keeps
    // that certificate in the person's record, as it keeps every key a root
    // issues through its pairing.
    let directory = tempfile::tempdir().expect("a directory for the browser's identity");
    let browser = ada.certify(
        &tilo_device_identity::IdentityPaths::under(directory.path()),
        "This browser",
    );
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime")
        .block_on(site.workspaces.keep_issued_device(&browser, ""));

    // The desktop lists it, though no session of its own made the key: the list
    // is the person's record, which the host keeps for all their devices.
    tilo_test_env::wait_until("the desktop lists the browser the page approved", || {
        lists(&listed(&desktop), "This browser")
    });
    let listed = listed(&desktop);
    let shown = listed
        .iter()
        .find(|device| device["label"] == "This browser")
        .expect("the browser is listed");
    assert_eq!(shown["key_hex"], hex::encode(browser.device_public_key));
    assert_eq!(shown["current"], false, "{shown}");
    assert_eq!(shown["revoked"], false, "{shown}");
    assert_eq!(shown["kind"], "Device", "{shown}");
}

/// A device the desktop pairs joins the person's record even though no
/// workspace of theirs moved: the helper signs its certificate into the
/// desktop's fleet, and the record keeps every key their root certified.
#[test]
fn a_paired_device_reaches_the_record_without_the_workspace_list_moving() {
    let ada = Person::new("Ada");
    let site = Site::start(&ada, "Studio");
    site.publish(Role::Editor);
    let mut desktop = ada.device("Ada's desktop");
    let mut laptop = ada.device("Ada's laptop");
    // Both join the workspace's entry, and neither needs the other's session first: both boot
    // together.
    let hers = site.admit(ada.public);
    let its = site.admit(ada.public);
    join_together(
        &mut [&mut desktop, &mut laptop],
        &[hers.as_str(), its.as_str()],
    );
    for device in [&desktop, &laptop] {
        device.wait_holding(&site.workspace);
        device.wait_showing(&site.workspace);
    }
    let before = listed(&laptop);
    assert!(
        !lists(&before, "Ada's phone"),
        "the phone is paired after this: {before:?}"
    );
    let workspaces = desktop.held();

    // The desktop pairs Ada's phone: the certificate their root signed for it is
    // kept in the record of the person's own workspace, which is the whole of
    // what a pairing leaves here.
    let phone = ada.device("Ada's phone");
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime")
        .block_on(site.workspaces.keep_issued_device(&phone.cert, ""));

    // The laptop lists it, which it can only read from the record: no workspace
    // of the person's moved to say so.
    tilo_test_env::wait_until("the laptop lists the phone the desktop paired", || {
        // A pass over the desktop's shell, as its next frame's is.
        desktop.ask(&json!({"op": "state_dump"}));
        lists(&listed(&laptop), "Ada's phone")
    });
    assert_eq!(
        desktop.held(),
        workspaces,
        "the workspaces of the person's list did not move\n\
         the laptop holds {:?} and shows {:?}\n\
         the desktop shows {:?}\n\
         the desktop's join flow:\n{}\n\
         the laptop's join flow:\n{}",
        laptop.held(),
        laptop.on_shell(),
        desktop.on_shell(),
        desktop.join_flow(),
        laptop.join_flow(),
    );
    assert!(
        !fleet_of(&laptop, ada.public)
            .load(&hex::encode(ada.public))
            .expect("the laptop's fleet")
            .certificates
            .iter()
            .any(|cert| cert.device_public_key == phone.cert.device_public_key),
        "the laptop holds the phone's key in no fleet of its own"
    );
}
