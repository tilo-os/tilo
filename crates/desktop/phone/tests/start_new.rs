//! A person whose first Tilo device is this phone starts new: the phone's own device (the daemon
//! in this process, as the app starts it) founds their person (`devices#found`) and shows them
//! signed in, with the workspace every person starts with.
//!
//! The daemon reads its places from the process's environment, so this binary is one test, as
//! `ticket.rs` is.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

#[path = "common/carried.rs"]
mod carried;
mod server;
mod support;

use tilo_phone::view::Records;
use ui::widget::Widget as _;

tilo_automerge_isolate::test_worker!();

#[test]
fn a_first_phone_starts_its_person() {
    let _home = support::Home::new();
    // The APK carries Notes: the phone's first start installs it into its store, so the workspace
    // it founds runs Notes and the phone opens it from the install it verified.
    let scratch = tempfile::tempdir().unwrap();
    server::Server::install_on_device(&server::notes_tapp(scratch.path()));
    carried::declare();
    let mut phone =
        tilo_runtime::in_process::Device::start(&support::profile()).expect("the phone's daemon");
    let before = support::shown(&phone);
    assert!(
        !before.signed_in,
        "nothing starts a person by itself: the phone shows where a ticket goes and Start new: \
         {before:?}"
    );

    phone.found().expect("the phone starts its person");

    let devices = support::until("the phone is its new person's", || {
        let devices = phone.devices().map_err(|error| format!("{error:#}"))?;
        if devices.signed_in && !devices.workspaces.is_empty() {
            Ok(devices)
        } else {
            Err(format!("{devices:?}"))
        }
    });
    // The person's own workspace started with the app this phone carries, and the phone opens it
    // from the install it verified: a carried app is installed where the workspace is founded.
    let own = devices.own.expect("the phone is in its own workspace");
    support::until("the phone's own workspace runs Notes", || {
        let apps = phone.apps(&own).map_err(|error| format!("{error:#}"))?;
        if apps.iter().any(|app| app.key == server::NOTES) {
            Ok(())
        } else {
            Err(format!("the workspace runs {apps:?}"))
        }
    });
    let mut window = support::until("the phone opens the Notes it verified", || {
        let connection = phone
            .window_connection(server::NOTES, &own)
            .map_err(|error| format!("{error:#}"))?;
        tilo_phone::view::open(server::NOTES, &own, Records::Workspace(connection))
    });
    support::until("Notes' window shows Notes", || {
        window.update(&(), &());
        let view = window.view().ok_or("the window is not connected")?;
        match view.title() {
            Some("Notes") => Ok(()),
            other => Err(format!("the window shows {other:?}")),
        }
    });
    drop(window);

    // Starting new again is no second person: the device answers as the one it started.
    phone
        .found()
        .expect("a phone that has its person goes on as that person");
    assert_eq!(
        phone
            .devices()
            .expect("the phone's device answers")
            .workspaces,
        devices.workspaces,
        "the person stays the one first started"
    );

    phone.stop().expect("the phone's daemon stops");
}
