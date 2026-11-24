//! A fresh phone that joins a workspace runs the workspace's apps: the apps a workspace installs
//! are in its tree (`workspace/apps#install`, its package kept by the workspace's host), and every
//! device of a member fetches each package into its own cache and lists it, a phone as much as a
//! laptop. Here a phone nobody signed in to before joins a workspace that runs tilolang Notes,
//! lists the workspace's apps through the call its view reads them with, and opens Notes.
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
    let _home = support::Home::new();
    let server = Server::start();
    let scratch = tempfile::tempdir().unwrap();
    // The workspace runs Notes before the phone ever joins it; nothing is installed on the phone.
    server.install(&server::notes_tapp(scratch.path()));
    let laptop = support::Issuing::start(server.runtime.handle(), &server.person, server.entry());
    let mut phone = tilo_phone::core::Device::start(&support::profile()).expect("the phone's daemon");
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

    // What the phone's view lists under the workspace (`device.apps`): the workspace's apps, which
    // its device fetched from the workspace's tree into its own package cache.
    let apps = support::until(
        "the phone lists Notes, which the workspace runs: its device fetches and installs the \
         packages the workspace installs",
        || {
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

    // And it opens: the device runs the app it installed and answers what its window shows.
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let shown = runtime
        .block_on(async {
            tilo_api::calls::Calls::open(tilo_api::calls::Reach::default())
                .await?
                .view(&server.workspace, "notes", false)
                .await
        })
        .unwrap_or_else(|error| panic!("Notes opens on the phone: {error:#}"));
    assert!(
        shown.to_string().contains("Notes"),
        "Notes' window shows Notes: {shown}"
    );

    phone.stop().expect("the phone's daemon stops");
    drop(laptop);
}
