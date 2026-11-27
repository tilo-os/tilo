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
//! A phone nobody signed in to before keeps what its apps wrote on the phone alone. Here one
//! opens Notes over those documents and makes a note, signs in, and carries what it kept into the
//! person's own workspace, where the phone's joined window shows it.
//!
//! The daemon reads its places from the process's environment, so each test here is one process,
//! as `ticket.rs` is.

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
use tilo_phone::view::Records;
use tilolang_process::window::hosted::HostedWindow;
use ui::widget::Widget as _;

#[test]
fn a_fresh_phone_lists_its_workspaces_apps() {
    let mut home = support::Home::new();
    let server = Server::start();
    home.relay(&server.relay_url());
    // The person's server runs the language its program links, as a computer's daemon does: what
    // reads the workspace's Notes record there, as their other devices read it.
    let tilolang = tilolang_workspace::host();
    server
        .workspaces()
        .provide_tilolang(tilolang.clone())
        .expect("the build's tilolang apps declare their surfaces");
    {
        // A record's writes wait on the reactor its records were registered on: the server's.
        let _entered = server.runtime.enter();
        tilolang.records.serve(&server.workspaces());
    }
    let scratch = tempfile::tempdir().unwrap();
    // The workspace runs Notes before the phone ever joins it; nothing is installed on the phone.
    server.install(&server::notes_tapp(scratch.path()));
    carried_runtimes();
    let laptop = support::Issuing::start(
        server.runtime.handle(),
        &server.person,
        server.entry(),
        server.workspaces(),
    )
    .expect("the laptop's ticket and endpoint are made");
    let mut phone =
        tilo_runtime::in_process::Device::start(&support::profile()).expect("the phone's daemon");
    phone
        .log_in(&laptop.ticket(), "Pixel")
        .expect("the phone redeems the ticket its person's laptop printed");
    let devices = support::until("the phone lists the workspace it joined", || {
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
    // Before the person picks one, the phone is in their own workspace: the one their root
    // names, whatever order the workspaces are listed in.
    assert_eq!(
        devices.own,
        Some(tilo_server_protocol::servers::person_workspace_id(
            &server.person.verifying_key().to_bytes()
        )),
        "the phone is in the person's own workspace: {devices:?}"
    );

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
    });
    assert!(
        apps.iter().any(|app| app.name == "Notes"),
        "Notes is listed by its name: {apps:?}"
    );
    // The view's row shows each app by its glyph, and a row naming none is not built.
    assert!(
        apps.iter().all(|app| !app.icon.is_empty()),
        "every app is listed with its glyph: {apps:?}"
    );

    opens_what_the_workspace_runs(&server, &phone);
    phone.stop().expect("the phone's daemon stops");
    drop(laptop);
}

/// A phone nobody signed in to yet opens Notes over the documents it keeps itself and makes a
/// note; the note is still there when its window opens again. At sign-in, what it kept is carried
/// into the person's own workspace, and the phone's window joined there shows it. A carry whose
/// connection cannot be made keeps the app's documents and says why in words.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one story: an app used before sign-in, the sign-in, the carry, and what is refused"
)]
fn a_phone_carries_what_it_kept_before_sign_in_into_the_workspace() {
    let mut home = support::Home::new();
    let server = Server::start();
    home.relay(&server.relay_url());
    let tilolang = tilolang_workspace::host();
    server
        .workspaces()
        .provide_tilolang(tilolang.clone())
        .expect("the build's tilolang apps declare their surfaces");
    {
        let _entered = server.runtime.enter();
        tilolang.records.serve(&server.workspaces());
    }
    let scratch = tempfile::tempdir().unwrap();
    // Notes as this build makes it: the file the workspace installs and the bundle the phone's
    // window runs over what it kept before anyone signed in.
    let (notes, bundle) = server::notes(scratch.path());
    let bundles = {
        let mut bundles = std::collections::BTreeMap::new();
        bundles.insert(server::NOTES.to_owned(), bundle);
        bundles
    };
    let own = tilo_server_protocol::servers::person_workspace_id(
        &server.person.verifying_key().to_bytes(),
    );
    // The workspace runs Notes before the phone ever joins it.
    server.install(&notes.tapp);
    carried_runtimes();

    // Before anyone signed in to the phone, Notes runs over what the phone keeps itself, under
    // the directory its window keeps an app's documents in.
    let dir = scratch.path().join("tilolang");
    let mut window = signed_out_notes(&server, &bundles, &dir);
    press("Notes offers New note", &mut window, "new_note");
    support::until("the note stands in the notebook", || {
        window.update(&(), &());
        note_in(&window)
    });
    drop(window);

    // The window opens again over the same documents: the note is still there.
    let mut window = signed_out_notes(&server, &bundles, &dir);
    support::until("the note kept on the phone is still there", || {
        window.update(&(), &());
        note_in(&window)
    });
    drop(window);

    // The person's laptop prints the ticket the phone signs in with.
    let laptop = support::Issuing::start(
        server.runtime.handle(),
        &server.person,
        server.entry(),
        server.workspaces(),
    )
    .expect("the laptop's ticket and endpoint are made");
    let mut phone =
        tilo_runtime::in_process::Device::start(&support::profile()).expect("the phone's daemon");
    phone
        .log_in(&laptop.ticket(), "Pixel")
        .expect("the phone redeems the ticket its person's laptop printed");
    let devices = support::until("the phone lists the workspace it joined", || {
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
    // The phone is in the person's own workspace, which every device of theirs finds by their
    // key alone: there is where what they kept before sign-in goes.
    assert_eq!(devices.own.as_deref(), Some(own.as_str()));
    // A workspace keeps an app's records only where the app is installed: the app the phone
    // carried is installed in the person's own workspace before the carry reaches it.
    server.install_in(&own, &notes.tapp);

    // At sign-in, what the phone kept is carried into the person's own workspace, through the
    // connection each app's window would join there.
    let workspace = uuid::Uuid::parse_str(&own).expect("the workspace is one a route names");
    let carried = tilolang_process::carry::signed_out(&dir, |app| {
        let bundle = bundles
            .get(app)
            .cloned()
            .ok_or_else(|| format!("{app} is not one this phone carries"))?;
        let connection = phone
            .window_connection(app, &own)
            .map_err(|error| format!("{error:#}"))?;
        let route = tilolang_process::route_of(&bundle.meta, workspace, &connection.person);
        let as_app = Some(bundle.meta.app.clone());
        Ok(tilolang_process::carry::Handed {
            bundle,
            route,
            collaboration: connection.collaboration,
            identity: connection.identity,
            person: connection.person,
            as_app,
        })
    })
    .expect("what the phone kept is read");
    assert!(
        carried.all_carried(),
        "what the phone kept is carried: {:?}",
        carried.refused()
    );
    assert!(
        !dir.join(server::NOTES).exists(),
        "the app's kept documents are gone once they are the workspace's"
    );

    // The note the person made before sign-in is a note their own workspace holds: the record
    // the phone's window wrote there, as the app whose grant covers it, for the person whose
    // device made the connection.
    let person = hex::encode(server.person.verifying_key().to_bytes());
    let packages = server::notes_packages();
    support::until(
        "the note made before sign-in is on the person's server",
        || {
            let who =
                tilolang_workspace::records::member(tilo_actions::MemberId::person(person.clone()));
            let changes = server
                .runtime
                .block_on(tilolang_workspace::records::with_tl_record(
                    &server.workspaces(),
                    &own,
                    &packages,
                    (who, &person),
                    |opened| opened.changes(),
                ))?;
            let made = changes.iter().any(|change| {
                change["happened"] == "created"
                    && change["resource"].as_str().is_some_and(|resource| {
                        resource.contains(":root-") && resource.contains('/')
                    })
            });
            if made {
                Ok(())
            } else {
                Err(format!(
                    "the server keeps {}",
                    serde_json::Value::Array(changes)
                ))
            }
        },
    );

    // The phone's window joined to the person's own workspace shows the note it kept.
    let mut joined = support::until("the phone opens Notes joined to its workspace", || {
        let connection = phone
            .window_connection(server::NOTES, &own)
            .map_err(|error| format!("{error:#}"))?;
        tilo_phone::view::open(
            server::NOTES,
            &own,
            &bundles,
            Records::Workspace(connection),
        )
    });
    support::until("the joined window shows the note the phone kept", || {
        joined.update(&(), &());
        note_in(&joined)
    });
    drop(joined);

    // A carry whose connection cannot be made keeps the app's documents and says why in words.
    let refused_dir = scratch.path().join("refused");
    std::fs::create_dir_all(refused_dir.join(server::NOTES).join("resources"))
        .expect("the app's directory is made");
    let refused = tilolang_process::carry::signed_out(&refused_dir, |app| {
        Err(format!("{app} has nowhere to go"))
    })
    .expect("the phone's kept documents are read");
    let (app, why) = refused
        .apps
        .first()
        .map(|(app, outcome)| {
            (
                app.as_str(),
                outcome
                    .as_ref()
                    .err()
                    .map(String::as_str)
                    .unwrap_or_default(),
            )
        })
        .expect("the carry found the app's documents");
    assert_eq!(
        app,
        server::NOTES,
        "the carry names the app it could not carry"
    );
    assert!(
        why.contains("nowhere to go"),
        "the carry says why in words: {why:?}"
    );
    assert!(
        refused_dir.join(server::NOTES).exists(),
        "a carry that could not run keeps the app's documents"
    );

    phone.stop().expect("the phone's daemon stops");
    drop(laptop);
}

/// Notes opened over what the phone keeps itself, in `dir`, before a person signed in.
fn signed_out_notes(
    server: &Server,
    bundles: &std::collections::BTreeMap<String, std::sync::Arc<tilolang_tilo::bundle::Bundle>>,
    dir: &std::path::Path,
) -> HostedWindow {
    support::until("Notes opens over what the phone keeps itself", || {
        tilo_phone::view::open(
            server::NOTES,
            &server.workspace,
            bundles,
            Records::Here(Some(dir.to_path_buf())),
        )
    })
}

/// The person presses the action whose id ends with `action` in the window, as its palette
/// offers it.
fn press(what: &str, window: &mut HostedWindow, action: &str) {
    let offered = support::until(what, || {
        window.update(&(), &());
        let view = window.view().ok_or("the window is not connected")?;
        view.actions()
            .iter()
            .find(|offered| offered.address.action.ends_with(action))
            .map(|offered| offered.address.clone())
            .ok_or_else(|| format!("the window offers {:?}", view.actions()))
    });
    window
        .view_mut()
        .expect("the window is connected")
        .invoke_action(offered, None);
    window.update(&(), &());
}

/// Whether a note stands in the window: an action its `Note` type declares, which the window
/// publishes only where a note is shown.
fn note_in(window: &HostedWindow) -> Result<(), String> {
    let view = window.view().ok_or("the window is not connected")?;
    let offered = view.actions();
    let stand = offered.iter().any(|offered| {
        ["retitle", "pin", "duplicate"]
            .iter()
            .any(|action| offered.address.action.ends_with(action))
    });
    if stand {
        Ok(())
    } else {
        Err(format!("the window offers {offered:?}"))
    }
}

/// The app the workspace runs opens from the package the phone's device installed, shows its own
/// window, and what the person makes in it through the phone is the workspace's, which the
/// person's server keeps for their other devices.
fn opens_what_the_workspace_runs(server: &Server, phone: &tilo_runtime::in_process::Device) {
    // And it opens: the app the workspace runs, from the package the phone's device installed,
    // in the window the phone's view shows it in, joined to the workspace over the connection the
    // phone's daemon made for it. The phone lists Notes and opens it; the window that comes up is
    // Notes'.
    let bundles = std::collections::BTreeMap::new();
    let mut window = support::until("the phone opens Notes, which its workspace runs", || {
        let connection = phone
            .window_connection(server::NOTES, &server.workspace)
            .map_err(|error| format!("{error:#}"))?;
        tilo_phone::view::open(
            server::NOTES,
            &server.workspace,
            &bundles,
            tilo_phone::view::Records::Workspace(connection),
        )
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

    // The person presses New note in it, as Notes' window offers it.
    let new_note = support::until("Notes offers New note", || {
        window.update(&(), &());
        let view = window.view().ok_or("the window is not connected")?;
        view.actions()
            .iter()
            .find(|action| action.address.action.ends_with("new_note"))
            .map(|action| action.address.clone())
            .ok_or_else(|| format!("Notes offers {:?}", view.actions()))
    });
    window
        .view_mut()
        .expect("the window is connected")
        .invoke_action(new_note, None);

    // The note is the workspace's: the person's server, which keeps it for their other devices,
    // holds it.
    let person = hex::encode(server.person.verifying_key().to_bytes());
    let packages = server::notes_packages();
    let mut said = Vec::new();
    support::until(
        "the note made on the phone is on the person's server",
        || {
            window.update(&(), &());
            if let Some(view) = window.view_mut() {
                said.extend(view.take_notices());
            }
            let who =
                tilolang_workspace::records::member(tilo_actions::MemberId::person(person.clone()));
            let changes = server
                .runtime
                .block_on(tilolang_workspace::records::with_tl_record(
                    &server.workspaces(),
                    &server.workspace,
                    &packages,
                    (who, &person),
                    |opened| opened.changes(),
                ))?;
            // A note is made in the notebook: a resource created under the notebook's root.
            let made = changes.iter().any(|change| {
                change["happened"] == "created"
                    && change["resource"].as_str().is_some_and(|resource| {
                        resource.contains(":root-") && resource.contains('/')
                    })
            });
            if made {
                Ok(())
            } else {
                Err(format!(
                    "the window said {said:?}; the server keeps {}",
                    serde_json::Value::Array(changes)
                ))
            }
        },
    );
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
        tilolang = tilolang_tilo::runtime(),
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
