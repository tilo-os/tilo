//! A fresh phone that joins a workspace lists the workspace's apps and opens one: the apps a
//! workspace installs are in its tree (`workspace/apps#install`, its package kept by the
//! workspace's host), and every device of a member reads that record, a phone as much as a
//! laptop. Here a phone nobody signed in to before joins a workspace that runs tilolang Notes,
//! lists it by name, and opens it through the path its view opens an app with: the app runs in
//! the phone's own process, in tilolang-process's in-process window, and its window shows Notes.
//! A phone starts no process for an app, and one it does not carry it runs from the package its
//! device installed, verified as a launch verifies it.
//!
//! What the app keeps there is the workspace's: the phone is someone's once it signed in, so the
//! window joins the workspace over the connection the phone's daemon makes for it, as any of the
//! person's devices' windows do. A note made in it reaches the person's server, which keeps the
//! workspace for their other devices.
//!
//! The daemon reads its places from the process's environment, so each phone test here is one
//! process and one binary, as `ticket.rs` is.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

// The server admits the phone's records through an Automerge worker: this binary is that
// worker, as the server host's own is.
tilo_automerge_isolate::test_worker!();

#[path = "common/carried.rs"]
mod carried;
mod server;
mod support;

use server::{Joined, Server};
use ui::widget::Widget as _;

#[test]
fn a_fresh_phone_lists_its_workspaces_apps() {
    let (_home, mut server) = Server::for_phone();
    // The person's server runs the language its program links, as a computer's daemon does: what
    // reads the workspace's Notes record there, as their other devices read it.
    server.serving_tilolang();
    let scratch = tempfile::tempdir().unwrap();
    // The workspace runs Notes before the phone ever joins it, and the phone's own start installs
    // the package its APK carries: it opens Notes from the install it verified, as any device.
    let tapp = server::notes_tapp(scratch.path());
    server.install(&tapp);
    server::Server::install_on_device(&tapp);
    carried::declare();
    let Joined {
        laptop,
        phone,
        devices,
    } = server.joined();
    // Before the person picks one, the phone is in their own workspace: the one their root
    // names, whatever order the workspaces are listed in.
    assert_eq!(
        devices.own,
        Some(server.own()),
        "the phone is in the person's own workspace: {devices:?}"
    );
    // The person's own workspace, kept on their server, lists the phone among their devices:
    // what carries it to their other devices, and what admits its windows and that workspace's
    // live session there.
    let person = server.person.verifying_key().to_bytes();
    support::until(
        "the person's own workspace lists the phone among their devices",
        || {
            let listed = server.runtime.block_on(async {
                tilo_workspace_service::workspaces::person_workspace::devices::of(
                    &server.workspaces(),
                    &person,
                )
                .await
                .listed(&person)
            });
            if listed
                .iter()
                .any(|listed| listed.device.called(&listed.cert) == "Pixel")
            {
                Ok(())
            } else {
                Err(format!("{listed:?}"))
            }
        },
    );

    // What the phone's view lists under the workspace (`device.apps`): the apps the workspace's
    // host installed into it, which its device reads from the record its follower reads too.
    let apps = server::notes_listed(&phone, &server.workspace);
    assert!(
        apps.iter().any(|app| app.name == "Notes"),
        "Notes is listed by its name: {apps:?}"
    );
    // The view's row shows each app by its glyph, and a row naming none is not built.
    assert!(
        apps.iter().all(|app| !app.icon.is_empty()),
        "every app is listed with its glyph: {apps:?}"
    );

    // A window opens with what the device says of the app's actions, as a launch hands it: what
    // opening the app runs is decided from its first frame, not refused before the device answered.
    let connection = phone.window_connection(server::NOTES, &server.workspace);
    assert!(
        connection.is_ok_and(|connection| !connection.verdicts.is_empty()),
        "Notes' window opens with the device's verdicts on its actions"
    );
    opens_what_the_workspace_runs(&server, &phone);

    // A note made just before the phone stops, with Notes open and the phone in the workspace,
    // is kept: the stop takes seconds, and the note is on the server once the phone is back.
    // Notes opens again on the note it shows last.
    let mut window = server::notes_opened(&phone, &server.workspace);
    server::press("Notes offers New note", &mut window, "new_note");
    let stopping = std::time::Instant::now();
    phone.stop().expect("the phone's daemon stops");
    let took = stopping.elapsed();
    assert!(took.as_secs() < 3, "the phone took {took:?} to stop");
    drop(window);
    let phone = tilo_runtime::in_process::Device::start(&support::profile()).unwrap();
    support::until(
        "the note made before the stop is on the server",
        || match notes_on_server(&server)? {
            2.. => Ok(()),
            kept => Err(format!("the server keeps {kept} notes")),
        },
    );

    // Started again while the server is away, the phone opens Notes on the copy it keeps, not
    // after the 30 s a wait on the server costs.
    phone.stop().expect("the phone's daemon stops");
    server.host.stop();
    let started = std::time::Instant::now();
    let phone = tilo_runtime::in_process::Device::start(&support::profile()).unwrap();
    drop(server::notes_opened(&phone, &server.workspace));
    let took = started.elapsed();
    assert!(took.as_secs() < 15, "Notes waited {took:?} on the server");
    phone.stop().expect("the phone's daemon stops");
    drop(laptop);
}

/// The app the workspace runs opens from the package the phone's device installed, shows its own
/// window, and what the person makes in it through the phone is the workspace's, which the
/// person's server keeps for their other devices.
fn opens_what_the_workspace_runs(server: &Server, phone: &tilo_runtime::in_process::Device) {
    // And it opens: the app the workspace runs, from the package the phone's device installed,
    // in the window the phone's view shows it in, joined to the workspace over the connection the
    // phone's daemon made for it. The phone lists Notes and opens it; the window that comes up is
    // Notes'.
    let mut window = server::notes_window(phone, &server.workspace);
    assert_eq!(
        window.app(),
        Some(server::NOTES),
        "the window runs the app the person opened"
    );

    // The person presses New note in it, as Notes' window offers it.
    server::press("Notes offers New note", &mut window, "new_note");

    // The note is the workspace's: the person's server, which keeps it for their other devices,
    // holds it.
    let mut said = Vec::new();
    support::until(
        "the note made on the phone is on the person's server",
        || {
            window.update(&(), &());
            if let Some(view) = window.view_mut() {
                said.extend(view.take_notices());
            }
            match notes_on_server(server)? {
                0 => Err(format!(
                    "the window said {said:?}; the server keeps no note"
                )),
                _ => Ok(()),
            }
        },
    );
}

/// How many notes the person's server keeps in the workspace: resources created under the
/// notebook's root.
fn notes_on_server(server: &Server) -> Result<usize, String> {
    let person = hex::encode(server.person.verifying_key().to_bytes());
    let who = tilolang_workspace::records::member(tilo_actions::MemberId::person(person.clone()));
    let changes = server
        .runtime
        .block_on(tilolang_workspace::records::with_tl_record(
            &server.workspaces(),
            &server.workspace,
            &server::notes_packages(),
            (who, &person),
            |opened| opened.changes(),
        ))?;
    Ok(changes
        .iter()
        .filter(|change| {
            change["happened"] == "created"
                && change["resource"]
                    .as_str()
                    .is_some_and(|resource| resource.contains(":root-") && resource.contains('/'))
        })
        .count())
}
