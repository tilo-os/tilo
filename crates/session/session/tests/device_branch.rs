//! A machine copied from one of a person's devices, in a workspace a site
//! hosts: the copy re-keys at boot through the production re-key
//! (`tilo_user_identity::branch::settle`), its person certifies the new key,
//! and both run as real sessions. What either opens on the workspace's shared
//! desk — a record sealed under the workspace's channel keys — the other
//! shows, before and after both restart, and the source stays the member it
//! was. Before any copy, the same record between two of a person's devices
//! that were never the same machine.
//!
//! The desk's windows are the edits here rather than an app's records: a
//! window is a change of the workspace's sealed desk record, and opening one
//! needs no app sandbox, which the machines these tests run on do not give.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the devices live \
              until their sessions end"
)]

mod common;

// A site admits its members' records through an Automerge worker: this binary
// is that worker, as the session's own is.
tilo_automerge_isolate::test_worker!();

use common::people::{Device, Person, Site, show, two_devices_of_one_person, wait_shown};
use tilo_relay_protocol::workspaces::WorkspaceRole;

/// The app whose window is opened on the desks here.
const NOTES: &str = "so.tilo.notes";

fn start(device: &mut Device, site: &Site) {
    device.start();
    device.wait_holding(&site.workspace);
    show(device, site);
}

/// Ada's laptop in a workspace her site hosts, with a window on its shared
/// desk the site keeps; then its disk copied while it is off, the copy
/// re-keyed at boot and certified by Ada, and both started.
fn laptop_and_its_copy() -> (Site, Person, Device, Device) {
    let ada = Person::new("Ada");
    let site = Site::start(&ada, "Studio");
    // The workspace's host lets its desk author its records.
    site.author_apps(&[tilo_desktop_model::ownership::DESKTOP_APP]);
    let mut laptop = ada.device("Ada's laptop");
    let hers = site.invite(ada.public, WorkspaceRole::Editor);
    laptop.join(&hers);
    laptop.wait_holding(&site.workspace);
    show(&laptop, &site);
    {
        let _before = laptop.window(NOTES, "Before the copy");
        wait_shown(&site, &[&laptop], "Before the copy");
    }

    // The laptop's disk is copied while it is off; the copy boots on a
    // machine of its own and re-keys.
    laptop.stop();
    let (mut copy, branched) = ada.branch(&laptop);
    assert_ne!(copy.cert.device_public_key, laptop.cert.device_public_key);
    assert_eq!(branched.previous, laptop.cert.device_public_key);
    assert!(
        copy.cert.device_label.starts_with("Ada's laptop branch "),
        "{}",
        copy.cert.device_label
    );
    assert_ne!(
        std::fs::read(copy.identity().device_privkey).unwrap(),
        std::fs::read(laptop.identity().device_privkey).unwrap(),
        "the copy holds no key of its source's"
    );
    start(&mut laptop, &site);
    start(&mut copy, &site);
    (site, ada, laptop, copy)
}

/// What either of a person's two devices writes to the workspace's desk the other reads: two
/// sessions of their own, on the route the network actor publishes a record on.
#[test]
fn an_edit_on_one_device_of_a_person_reaches_the_other() {
    let (site, _ada, laptop, phone) = two_devices_of_one_person();
    show(&laptop, &site);
    show(&phone, &site);

    let _from_laptop = laptop.window(NOTES, "From the laptop");
    wait_shown(&site, &[&laptop, &phone], "From the laptop");

    let _from_phone = phone.window(NOTES, "From the phone");
    wait_shown(&site, &[&laptop, &phone], "From the phone");
}

/// The copy takes the workspace up again as Ada's own further device: the
/// channel state it was copied with names its source and is dropped, never
/// used; the site admits the copy as a device of Ada's without an
/// invitation; the copy keeps the workspace on its desk throughout; and Ada
/// stays the member she was, her laptop holding the workspace too.
#[test]
fn a_machine_copied_from_a_device_rejoins_as_a_device_of_its_own() {
    let (site, ada, laptop, copy) = laptop_and_its_copy();
    let dropped = format!("joining as this device workspace={}", site.workspace);
    let admitted = format!(
        "the host admitted this device and the workspace is held here workspace={}",
        site.workspace
    );
    tilo_test_env::wait_until(
        format!("the copy rejoining {} as its own device", site.workspace),
        || {
            let log = copy.log();
            log.contains(&dropped)
                && log
                    .lines()
                    .any(|line| line.contains(&admitted) && line.contains("member=true"))
        },
    );
    copy.wait_holding(&site.workspace);
    laptop.wait_holding(&site.workspace);
    // Nobody was removed (Ada owns the site, so it lists her as no member of
    // its own), and a member is a person: the copy adds none.
    let members = site.members();
    assert!(members.values().all(|mark| !mark.removed), "{members:?}");
    assert!(!members.contains_key(&hex::encode(copy.cert.device_public_key)));
    assert!(!members.contains_key(&hex::encode(ada.public)) || members.len() == 1);
}

/// What either opens on the workspace's sealed desk the other shows, before
/// and after both restart.
#[test]
fn a_machine_copied_from_a_device_and_its_source_read_each_others_edits() {
    let (site, _ada, mut laptop, mut copy) = laptop_and_its_copy();
    // What the copy opens the source shows, and what the source opens the
    // copy shows: each reads the other's writes to the sealed desk.
    let from_copy = copy.window(NOTES, "From the copy");
    wait_shown(&site, &[&copy, &laptop], "From the copy");
    let from_laptop = laptop.window(NOTES, "From the laptop");
    wait_shown(&site, &[&laptop, &copy], "From the laptop");
    drop((from_copy, from_laptop));

    // Both come back as themselves.
    laptop.stop();
    copy.stop();
    start(&mut laptop, &site);
    start(&mut copy, &site);
    let _copy_again = copy.window(NOTES, "The copy again");
    wait_shown(&site, &[&copy, &laptop], "The copy again");
    let _laptop_again = laptop.window(NOTES, "The laptop again");
    wait_shown(&site, &[&laptop, &copy], "The laptop again");
}

/// One device, no copy: what it opens on the workspace's desk after a
/// restart continues what it wrote before, and the site keeps it.
#[test]
fn a_device_restarted_keeps_writing_the_workspace_desk() {
    let ada = Person::new("Ada");
    let site = Site::start(&ada, "Studio");
    site.author_apps(&[tilo_desktop_model::ownership::DESKTOP_APP]);
    let mut laptop = ada.device("Ada's laptop");
    let hers = site.invite(ada.public, WorkspaceRole::Editor);
    laptop.join(&hers);
    laptop.wait_holding(&site.workspace);
    show(&laptop, &site);
    {
        let _before = laptop.window(NOTES, "Before the restart");
        wait_shown(&site, &[&laptop], "Before the restart");
    }
    laptop.stop();
    start(&mut laptop, &site);
    let _after = laptop.window(NOTES, "After the restart");
    wait_shown(&site, &[&laptop], "After the restart");
}
