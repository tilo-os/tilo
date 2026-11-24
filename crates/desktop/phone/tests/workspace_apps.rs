//! A fresh phone that joins a workspace lists the workspace's apps and opens one: the apps a
//! workspace installs are in its tree (`workspace/apps#install`, its package kept by the
//! workspace's host), and every device of a member reads that record, a phone as much as a
//! laptop. Here a phone nobody signed in to before joins a workspace that runs tilolang Notes,
//! lists it by name, and opens it through the path its view opens an app with: the app runs in
//! the phone's own process, in tilolang-process's in-process window, and its window shows Notes.
//! A phone starts no process for an app, and one it does not carry it runs from the package its
//! device installed, verified as a launch verifies it.
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
use ui::widget::Widget as _;

#[test]
fn a_fresh_phone_lists_its_workspaces_apps() {
    let mut home = support::Home::new();
    let server = Server::start();
    home.relay(&server.relay_url());
    let scratch = tempfile::tempdir().unwrap();
    // The workspace runs Notes before the phone ever joins it; nothing is installed on the phone.
    server.install(&server::notes_tapp(scratch.path()));
    carried_runtimes();
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
            if apps.iter().any(|app| app.key == server::NOTES) {
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

    // And it opens: the app the workspace runs, from the package the phone's device installed,
    // in the window the phone's view shows it in. The phone lists Notes and opens it; the
    // window that comes up is Notes'.
    let storage = tempfile::tempdir().unwrap();
    let bundles = std::collections::BTreeMap::new();
    let mut window = support::until("the phone opens Notes, which its workspace runs", || {
        tilo_phone::view::open(
            server::NOTES,
            &server.workspace,
            &bundles,
            Some(storage.path().to_path_buf()),
        )
        .map_err(|error| error)
    });
    assert_eq!(
        window.app(),
        Some(server::NOTES),
        "the window runs the app the person opened"
    );
    support::until("Notes' window shows Notes", || {
        window.update(&(), &());
        let view = window.view().ok_or("the window is not connected")?;
        match view.title() {
            Some("Notes") => Ok(()),
            other => Err(format!("the window shows {other:?}")),
        }
    });

    phone.stop().expect("the phone's daemon stops");
    drop(laptop);
}

/// Declare the runtime builds this test's phone carries in itself, as the phone declares the ones
/// its own signed manifest names: the exact Tilo runtime build it was built against and the
/// tilolang runtime it runs tilolang apps on. A phone's manifest is stamped by packaging; this
/// build has no APK, so the test states the same manifest and signs it.
fn carried_runtimes() {
    let identity = tilo_app_package::pack::SigningIdentity::from_signing_key(support::key(7));
    let mut manifest = tilo_app_package::Manifest::from_toml_str(&format!(
        r#"
[package]
id = "so.tilo.phone"
name = "Tilo"
category = "System"
version = "0.1.0"
kind = "app"
runtime = "{tapp}"
optional-runtimes = ["{tilolang}"]

[package.isa]
x86_64 = "v3"
aarch64 = "aarch64"

[publisher]
identity = "ed25519:{publisher}"
signature = ""

[sandbox]
namespaces = ["user"]
seccomp = "tilo-default"
landlock = "strict"

[sandbox.net]
mode = "host"

[entrypoint]
bin = "bin/tilo-phone"
type = "gui"
"#,
        tapp = TILO_RUNTIME,
        tilolang = tilolang_tilo::RUNTIME,
        publisher = hex::encode(identity.public_key_bytes()),
    ))
    .unwrap();
    manifest
        .sign(&identity)
        .expect("the test's phone signs its own manifest");
    manifest
        .verify_signature()
        .expect("the test's phone's manifest verifies");
    tilo_runtime::device::packages::carry_runtimes(&manifest)
        .expect("the phone declares the runtimes it carries");
}

/// The exact Tilo runtime build this test's phone links: what packaging stamps into a packaged
/// phone's manifest, its digest the build's own.
const TILO_RUNTIME: &str = "tilo-runtime/2026.07+native.0123456789abcdef0123456789abcdef";
