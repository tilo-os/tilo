//! What the APK carries is installed where the phone's first start seeds it: a package whose
//! bytes are not the package its name claims is refused there, in words, and not cached.
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

/// A package in the APK's apps that is not the package its name claims is refused where the
/// device's first start installs what it carries: in words, and it is not cached.
#[test]
fn a_tampered_package_the_apk_carries_is_refused_at_seeding() {
    let _home = support::Home::new();
    let scratch = tempfile::tempdir().unwrap();
    let tapp = server::notes_tapp(scratch.path());
    let bytes = std::fs::read(&tapp).expect("the package builds");
    let root = hex::encode(
        tilo_app_package::tapp::read(bytes.as_slice(), &mut tilo_app_package::blobs::DiscardBlobs)
            .expect("the package reads")
            .meta
            .id
            .0,
    );
    let apps = scratch.path().join("apps");
    std::fs::create_dir_all(&apps).unwrap();
    let mut tampered = bytes;
    let middle = tampered.len() / 2;
    tampered[middle] ^= 0xff;
    std::fs::write(apps.join(format!("{root}.tapp")), &tampered).expect("the tampered package");

    let refused = tilo_runtime::device::packages::install_carried_apps(&apps);
    let (package, why) = refused
        .first()
        .expect("the store refuses the package its name claims");
    assert_eq!(package, &root, "the refusal names the package it refused");
    assert!(!why.is_empty(), "the refusal says why in words");
    assert!(
        tilo_runtime::device::package_runs::verified_payload(server::NOTES).is_err(),
        "a refused package is not cached"
    );

    let words = format!("{package}: {why}");
    let worker = tilo_phone::worker::Worker::start(
        &support::profile(),
        tilo_process_services::notify::system(Some("so.tilo.phone")),
        Err(words.clone()),
        || {},
    )
    .expect("the phone's device starts");
    assert_eq!(worker.seen().apps_refused.as_deref(), Some(words.as_str()));
}
