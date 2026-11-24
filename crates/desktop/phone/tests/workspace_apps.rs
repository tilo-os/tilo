//! A fresh phone that joins a workspace lists the workspace's apps: the apps a workspace installs
//! are in its tree (`workspace/apps#install`, its package kept by the workspace's host), and every
//! device of a member reads that record, a phone as much as a laptop. Here a phone nobody signed
//! in to before joins a workspace that runs tilolang Notes and lists it, by name, through the call
//! its view reads apps with. A phone opens an app from the bundle its APK carries, never by
//! asking its daemon to launch one.
//!
//! The daemon reads its places from the process's environment, so this binary is one test, as
//! `ticket.rs` is.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

// The server admits the phone's records through an Automerge worker: this binary is that
// worker, as the server host's own is.
tilo_automerge_isolate::test_worker!();

mod server;
mod support;

use server::Server;

#[test]
fn a_fresh_phone_lists_its_workspaces_apps() {
    let mut home = support::Home::new();
    let server = Server::start();
    home.relay(&server.relay_url());
    let scratch = tempfile::tempdir().unwrap();
    // The workspace runs Notes before the phone ever joins it; nothing is installed on the phone.
    server.install(&server::notes_tapp(scratch.path()));
    let laptop = support::Issuing::start(
        server.runtime.handle(),
        &server.person,
        server.entry(),
        server.workspaces(),
    );
    let mut phone =
        tilo_runtime::in_process::Device::start(&support::profile()).expect("the phone's daemon");
    phone
        .log_in(&laptop.ticket(), "Pixel")
        .expect("the phone redeems the ticket its person's laptop printed");
    support::until("the phone lists the workspace it joined", || {
        let devices = phone.devices().map_err(|error| format!("{error:#}"))?;
        if devices
            .workspaces
            .iter()
            .any(|workspace| workspace.key == server.workspace)
        {
            Ok(())
        } else {
            Err(format!("{devices:?}"))
        }
    });

    // What the phone's view lists under the workspace (`device.apps`): the apps the workspace's
    // host installed into it, which its device reads from the record its follower reads too.
    let apps = support::until("the phone lists Notes, which the workspace runs", || {
            let apps = phone
                .apps(&server.workspace)
                .map_err(|error| format!("{error:#}"))?;
            if apps.iter().any(|app| app.key == "notes") {
                Ok(apps)
            } else {
                Err(format!("the phone lists {apps:?}"))
            }
        },
    );
    assert!(
        apps.iter().any(|app| app.name == "Notes"),
        "Notes is listed by its name: {apps:?}"
    );

    phone.stop().expect("the phone's daemon stops");
    drop(laptop);
}
