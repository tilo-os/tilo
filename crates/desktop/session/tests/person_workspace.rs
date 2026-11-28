//! The person's own workspace holds what is theirs alone: the settings that follow them and the
//! list of the workspaces they have. A device that founded it writes them there, reads them back,
//! and finds them again after a restart — from the workspace, not from what its session kept.
//!
//! The session here is a real one: its own home, its own identity root, its own workspace service.
//! The person's own workspace is never shown on the shell, so what proves this device holds it is
//! reading its records back through the session's record API, as any app or agent reads a
//! workspace's records.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the device lives until its \
              session ends"
)]

use std::time::Instant;

use serde_json::{Value, json};

use crate::common::people::{CONFIRMATION_REQUIRED, Device, Person, new_workspace, setting};

/// The settings that follow the person: their appearance, as their own workspace keeps it.
const SETTINGS: (&str, &str) = ("so.tilo.settings", "person");
/// The list of the workspaces they have, as their own workspace keeps it.
const LIST: (&str, &str) = ("so.tilo.identity", "identity.workspaces");

/// The id of `person`'s own workspace: the one whose only member they are.
fn own_workspace(person: &Person) -> String {
    tilo_server_protocol::servers::person_workspace_id(&person.public)
}

/// The record `record` of `app` in `workspace` as this device's workspace service holds it, read
/// outside its app: `None` while the workspace holds none of it, or it is still arriving.
fn held(device: &Device, workspace: &str, (app, record): (&str, &str)) -> Option<Value> {
    let answer = device
        .read(workspace, (app, record), None, Some(250))
        .unwrap_or_else(|error| {
            panic!(
                "{} reads {app}/{record} of {workspace}: {error}\n{}",
                device.label,
                device.diagnostic()
            )
        });
    let held = answer["records"].as_array()?.first()?;
    held.get("value").cloned()
}

/// Wait until `workspace` holds `wanted` somewhere in the record `record` of `app`.
fn wait_for_record(device: &Device, workspace: &str, route: (&str, &str), wanted: &str) {
    let until = tilo_test_env::hang_deadline();
    let mut pause = std::time::Duration::from_millis(20);
    loop {
        let found = held(device, workspace, route);
        if found
            .as_ref()
            .is_some_and(|value| value.to_string().contains(wanted))
        {
            return;
        }
        assert!(
            Instant::now() < until,
            "{} never reads {wanted:?} in {}/{} of the person's own workspace; it reads \
             {found:?}\n{}",
            device.label,
            route.0,
            route.1,
            device.diagnostic()
        );
        std::thread::sleep(pause);
        pause = pause
            .saturating_mul(2)
            .min(std::time::Duration::from_millis(500));
    }
}

/// Choose `mode` for the appearance on `device`, as a person does in Settings: the setter of the
/// section's mode, once the session offers it, which it does once it reads the person's sections.
///
/// The edit is followed only as far as the session admits it: what proves it took is the
/// workspace holding it ([`wait_for_record`]).
///
/// The mode is `light`, never the `dark` a computer starts with: a field set to what the section
/// already reads as is no change, and is kept nowhere.
fn choose_mode(device: &Device, mode: &str) {
    let target = setting(device, "appearance", "mode");
    let edit = json!({ "value": mode });
    let until = tilo_test_env::hang_deadline();
    loop {
        let answer = device.act(&target, &edit);
        match answer["err"]["code"].as_str() {
            Some("unknown_action") => assert!(
                Instant::now() < until,
                "{} is never offered appearance's setter of its mode: {answer}\n{}",
                device.label,
                device.diagnostic()
            ),
            Some(CONFIRMATION_REQUIRED) => {
                let answered = device.act_confirming(&target, &edit);
                assert!(
                    answered.get("err").is_none(),
                    "{} sets appearance.mode: {answered}\n{}",
                    device.label,
                    device.diagnostic()
                );
                return;
            }
            Some(_) => panic!(
                "{} sets appearance.mode: {answer}\nits own workspace is {}, its daemon names \
                 {:?}, it shows {:?}, it holds {:?}, and its service keeps {}\n{}",
                device.label,
                tilo_server_protocol::servers::person_workspace_id(&device.cert.user_public_key),
                device.daemon_workspace(),
                device.showing(),
                device.held(),
                device.workspace_files(),
                device.diagnostic()
            ),
            None => return,
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

#[test]
fn a_fresh_device_names_one_persons_workspace_for_its_daemon_and_its_session() {
    let ada = Person::new("Ada");
    let mut laptop = ada.device("Ada's laptop");
    laptop.install_passwords();
    laptop.start();
    // What the session derives from the person's key, and what the daemon founds: no fixture here
    // picks a workspace.
    let own = own_workspace(&ada);

    // The daemon founds that one and holds it, and the session reads it as the person's own: what
    // its records are read through.
    laptop.wait_reading(&own);
    assert_eq!(
        laptop.daemon_workspace(),
        own,
        "{}'s daemon names the person's own workspace: {}\n{}",
        laptop.label,
        laptop.daemon_workspace(),
        laptop.diagnostic()
    );

    // The one own workspace a plain ask answers is proved on a founding host's own service in the
    // workspace service's tests; here the daemon and the real session name it the same.

    // An app window opens in the workspace the shell started with, and its collaboration identity
    // is accepted there. Its store is that workspace's, kept once the device's daemon holds it too.
    let showing = laptop.showing().expect("a workspace in front");
    laptop.wait_holding(&showing);
    laptop.open("so.tilo.passwords");
    laptop.wait_for_window("so.tilo.passwords");
    assert!(
        !laptop
            .warnings()
            .contains("collaboration identity rejected"),
        "{}'s window was admitted: {}\n{}",
        laptop.label,
        laptop.warnings(),
        laptop.diagnostic()
    );
    laptop.stop();
}

#[test]
fn a_device_writes_its_person_workspace_and_reads_it_back_after_a_restart() {
    let ada = Person::new("Ada");
    let mut laptop = ada.device("Ada's laptop");
    laptop.start();
    let own = own_workspace(&ada);

    // What the person chooses in Settings is written into the workspace that holds what is theirs
    // alone, and read back from there. The workspace is never on the shell, so reading this record
    // back is also what shows this device holds it.
    choose_mode(&laptop, "light");
    wait_for_record(&laptop, &own, SETTINGS, "light");

    // A workspace they make is in the list that workspace keeps for them.
    let before = laptop.on_shell();
    let asked = laptop.act(&new_workspace(), &json!({}));
    assert!(
        asked.get("err").is_none(),
        "{} makes a workspace: {asked}\n{}",
        laptop.label,
        laptop.diagnostic()
    );
    let until = tilo_test_env::hang_deadline();
    let made = loop {
        if let Some(made) = laptop
            .on_shell()
            .into_iter()
            .find(|workspace| !before.contains(workspace) && *workspace != own)
        {
            break made;
        }
        assert!(
            Instant::now() < until,
            "{} never shows the workspace it made: {asked}\n{}",
            laptop.label,
            laptop.diagnostic()
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    wait_for_record(&laptop, &own, LIST, &made);

    // Both are the workspace's: a session that starts again reads them from there, not from what
    // the session before it kept.
    laptop.stop();
    laptop.start();
    wait_for_record(&laptop, &own, SETTINGS, "light");
    wait_for_record(&laptop, &own, LIST, &made);
}
