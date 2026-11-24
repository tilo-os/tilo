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

mod support;

#[test]
fn a_first_phone_starts_its_person() {
    let _home = support::Home::new();
    let mut phone = tilo_phone::core::Device::start(&support::profile()).expect("the phone's daemon");
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
