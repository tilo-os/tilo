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

use crate::common::people::{Device, Person, Site, show, two_devices_of_one_person, wait_shown};
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
    // The channel state on the copied disk names the device it was made for: the copy's session
    // drops all of it as it opens its vault on a device key of its own, before it opens any
    // workspace's channel.
    let dropped = "the channel state here was made for the device this machine was copied from";
    let admitted = format!(
        "the host admitted this device and the workspace is held here workspace={}",
        site.workspace
    );
    tilo_test_env::wait_until(
        format!("the copy rejoining {} as its own device", site.workspace),
        || {
            let log = copy.log();
            log.contains(dropped)
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

/// A copy started while the workspace's host is away is not the workspace's member yet: only the
/// host admits a device, and it is away. The copy's apps take part in nothing there until then, so
/// an action asked of one is refused at once, saying why, rather than waiting on an app that never
/// answers; and the desk tells the person the workspace waits for its host (`e2e-branch` S07-b,
/// where the branch booted while its source, the host, was stopped).
#[test]
fn a_copy_started_while_the_host_is_away_refuses_its_apps_actions_saying_why() {
    let ada = Person::new("Ada");
    let mut site = Site::start(&ada, "Studio");
    site.author_apps(&[tilo_desktop_model::ownership::DESKTOP_APP, CHAT]);
    let mut laptop = ada.device("Ada's laptop");
    laptop.install_chat();
    let hers = site.invite(ada.public, WorkspaceRole::Editor);
    laptop.join(&hers);
    laptop.wait_holding(&site.workspace);
    show(&laptop, &site);
    laptop.open(CHAT);
    laptop.wait_for_window(CHAT);
    site.wait_for_record_of(CHAT, &laptop);
    // Admitted, Chat runs what it is asked.
    let me = hex::encode(ada.public);
    let answer = laptop.chat().settled(
        "open-direct-conversation",
        &serde_json::json!({"public_key": me}),
    );
    let state = answer["answer"]["receipt"]["update"]["state"].as_str();
    assert!(
        matches!(state, Some("succeeded" | "failed")),
        "the laptop's Chat did not run its action: {answer}"
    );

    laptop.stop();
    let (mut copy, _) = ada.branch(&laptop);
    site.stop();
    copy.start();
    copy.wait_holding(&site.workspace);
    show(&copy, &site);
    copy.open(CHAT);
    copy.wait_for_window(CHAT);

    // What the copy's desk offers of Chat is refused at once, with the reason.
    let (provider, id) = chat_row(&copy);
    let started = std::time::Instant::now();
    let refused = copy.act(&provider, &id, &serde_json::json!({"public_key": me}));
    let said = refused["err"]["message"].as_str().unwrap_or_default();
    assert_eq!(
        refused["err"]["code"], "remote_actions_unavailable",
        "{refused}"
    );
    assert!(
        said.contains("hasn't been admitted to") && said.contains("must be reachable once"),
        "the refusal says why: {refused}"
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "refused at once, not after waiting on the app: {:?}",
        started.elapsed()
    );
    // The desk tells the person, once, that the workspace waits for its host.
    let told = "the person is told this device waits for the workspace's host to take it in";
    tilo_test_env::wait_until(
        format!(
            "{} telling the person the workspace waits for its host",
            copy.label
        ),
        || copy.log().contains(told),
    );
}

/// The app whose actions the copy is asked to run.
const CHAT: &str = "so.tilo.chat";

/// A row the desk lists of Chat's `open-direct-conversation`, by its provider and id.
fn chat_row(device: &crate::common::people::Device) -> (String, String) {
    let mut found = None;
    tilo_test_env::wait_until(format!("{} listing Chat's actions", device.label), || {
        found = device.actions()["answer"]["actions"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|row| {
                row["application"] == CHAT && row["operation"] == "open-direct-conversation"
            })
            .map(|row| {
                (
                    row["provider"].as_str().unwrap_or_default().to_owned(),
                    row["id"].as_str().unwrap_or_default().to_owned(),
                )
            });
        found.is_some()
    });
    found.expect("a row of Chat's")
}
