//! Devices and access reads the person's record, not this device's fleet.
//!
//! The person's hosts keep the keys their root certified beside their
//! workspace list, and every device of theirs lists those keys: a browser a
//! page's root approved in its own tab, a phone paired on their desktop. A
//! device's own fleet is not that list — a key the root certified elsewhere is
//! in no fleet of theirs but the one that signed it — so Settings asks the
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

use crate::common::people::{Device, Person, Site};
use serde_json::{Value, json};
use tilo_actions::remote::identity_admin;
use tilo_identity_host::device_fleet::DeviceFleetStore;
use tilo_relay_protocol::workspaces::Role;

/// The Devices step of the person's identity administration, as Settings asks
/// it: `resource␟action`, as the action door spells an address.
const DEVICES: &str = "identity\u{1f}devices";

/// The devices Settings lists on `device`.
fn listed(device: &Device) -> Vec<Value> {
    let answer = device.act_wait(identity_admin::PROVIDER, DEVICES, &json!({}));
    answer["answer"]["receipt"]["update"]["output"]["value"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| {
            panic!(
                "{} lists the person's devices: {answer}\n{}",
                device.label,
                device.diagnostic()
            )
        })
}

/// Whether `listed` holds a device called `label`.
fn lists(listed: &[Value], label: &str) -> bool {
    listed
        .iter()
        .any(|device| device["label"].as_str() == Some(label))
}

/// The fleet `device` keeps under its person's root, as the helper admitting a
/// paired device writes it.
fn fleet_of(device: &Device, person: [u8; 32]) -> DeviceFleetStore {
    let store = DeviceFleetStore::at(
        device.directory().join("config/tilo/identity"),
        device.identity(),
    );
    assert!(
        store.load(&hex::encode(person)).is_ok(),
        "{}'s fleet is where its session keeps it",
        device.label
    );
    store
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

    // The desktop lists itself: its own fleet is what the record holds of it
    // so far, and this is the device the person is using.
    let own = listed(&desktop);
    assert!(
        own.iter()
            .any(|device| device["label"] == "Ada's desktop" && device["current"] == true),
        "{own:?}"
    );

    // The page's root certifies the browser it runs in, and the relay keeps
    // that certificate in the person's record, as it keeps every key a root
    // issues through its pairing.
    let directory = tempfile::tempdir().expect("a directory for the browser's identity");
    let browser = ada.certify(
        &tilo_user_identity::IdentityPaths::under(directory.path()),
        "This browser",
    );
    site.workspaces.keep_issued_device(&browser);

    // The desktop lists it, though its own fleet never saw the key: the list
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
    assert!(
        !fleet_of(&desktop, ada.public)
            .load(&hex::encode(ada.public))
            .expect("the desktop's fleet")
            .certificates
            .iter()
            .any(|cert| cert.device_public_key == browser.device_public_key),
        "the browser's key is the page's, and no fleet of the desktop's"
    );
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
    desktop.join(&site.admit(ada.public));
    desktop.wait_holding(&site.workspace);
    desktop.wait_showing(&site.workspace);
    let mut laptop = ada.device("Ada's laptop");
    laptop.join(&site.admit(ada.public));
    laptop.wait_holding(&site.workspace);
    laptop.wait_showing(&site.workspace);
    let before = listed(&laptop);
    assert!(
        !lists(&before, "Ada's phone"),
        "the phone is paired after this: {before:?}"
    );
    let workspaces = desktop.held();

    // The desktop pairs Ada's phone: `tilo-enroll` signs its certificate into
    // the desktop's fleet, which is the whole of what a pairing leaves here.
    let phone = ada.device("Ada's phone");
    fleet_of(&desktop, ada.public)
        .remember_certificate(&hex::encode(ada.public), phone.cert.clone())
        .expect("the phone is in the desktop's fleet");

    // The laptop lists it, which it can only read from the record: the key is
    // in no fleet of the laptop's, and no workspace moved to say so.
    tilo_test_env::wait_until("the laptop lists the phone the desktop paired", || {
        // A pass over the desktop's desk, as its next frame's is.
        desktop.ask(&json!({"op": "state_dump"}));
        lists(&listed(&laptop), "Ada's phone")
    });
    assert_eq!(
        desktop.held(),
        workspaces,
        "the workspaces of the person's list did not move"
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
