//! A phone signs in with a ticket another of the person's devices printed, and sees their
//! workspace and its apps: the phone's own device (the daemon in this process, as the app starts
//! it) redeems the ticket at the issuing laptop, enters the workspace on the person's server, and
//! answers the phone's view what it reads, through the same calls the view makes. A phone that
//! kept no copy of the person's own workspace takes it in from their server over the network.
//!
//! The daemon reads its places from the process's environment, so this binary is one test: one
//! phone, in a home of its own, made before anything starts.

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

use server::{Server, WORKSPACE_NAME};

#[test]
fn a_phone_signs_in_with_a_ticket_and_sees_its_workspace_and_its_apps() {
    let mut home = support::Home::new();
    let server = Server::start();
    home.relay(&server.relay_url());
    let laptop = support::Issuing::start(
        server.runtime.handle(),
        &server.person,
        server.entry(),
        server.workspaces(),
    )
    .expect("the laptop's ticket and endpoint are made");
    let scratch = tempfile::tempdir().unwrap();
    // The workspace runs Notes, its host keeping the package; the phone lists it once it joins.
    server.install(&server::notes_tapp(scratch.path()));
    let mut phone =
        tilo_runtime::in_process::Device::start(&support::profile()).expect("the phone's daemon");
    let before = support::shown(&phone);
    assert!(
        !before.signed_in,
        "a phone nobody signed in to shows where a ticket goes: {before:?}"
    );

    phone
        .log_in(&laptop.ticket(), "Pixel")
        .expect("the phone redeems the ticket its person's laptop printed");

    let devices = support::until("the phone lists the person's workspace", || {
        let devices = phone.devices().map_err(|error| format!("{error:#}"))?;
        if devices
            .workspaces
            .iter()
            .any(|workspace| workspace.key == server.workspace)
        {
            Ok(devices)
        } else {
            Err(format!("{devices:?}"))
        }
    });
    assert!(
        devices.signed_in,
        "the ticket signed the phone in: {devices:?}"
    );
    let workspace = devices
        .workspaces
        .iter()
        .find(|workspace| workspace.key == server.workspace)
        .unwrap();
    assert_eq!(
        workspace.name, WORKSPACE_NAME,
        "the phone names the workspace as the person does"
    );

    // The phone kept no copy of the person's own workspace before it signed in: it takes it in
    // from their server over the network, as any of their devices does, and the page says why
    // if that workspace did not take it in.
    let own = tilo_server_protocol::servers::person_workspace_id(
        &server.person.verifying_key().to_bytes(),
    );
    assert!(
        !before.workspaces.iter().any(|kept| kept.key == own),
        "a phone nobody signed in to keeps none of the person's own workspace: {before:?}"
    );
    let devices = support::until("the phone keeps the person's own workspace", || {
        let devices = phone.devices().map_err(|error| format!("{error:#}"))?;
        let kept = devices.workspaces.iter().any(|kept| kept.key == own);
        match (devices.own_refused.as_deref(), kept) {
            (Some(why), _) => Err(format!(
                "the person's own workspace did not take the phone in: {why}"
            )),
            (None, true) => Ok(devices),
            (None, false) => Err(format!(
                "the phone keeps no copy of {own}, only {:?}",
                devices.workspaces
            )),
        }
    });
    assert_eq!(devices.own.as_deref(), Some(own.as_str()));

    let apps = support::until("the workspace's apps reach the phone", || {
        let apps = phone
            .apps(&server.workspace)
            .map_err(|error| format!("{error:#}"))?;
        if apps.iter().any(|app| app.key == server::NOTES) {
            Ok(apps)
        } else {
            Err(format!("{apps:?}"))
        }
    });
    assert!(
        apps.iter().any(|app| app.name == "Notes"),
        "Notes is listed by its name: {apps:?}"
    );

    phone.stop().expect("the phone's daemon stops");
    drop(laptop);
}
