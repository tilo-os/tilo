//! A phone nobody signed in to yet keeps what its apps wrote on the phone alone. Here one opens
//! Notes over those documents and makes a note, signs in, and carries what it kept into the
//! person's own workspace, which runs the app the phone carries, where the phone's joined window
//! shows it. A carry whose connection cannot be made keeps the app's documents and says why in
//! words.
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

use server::Server;
use tilo_phone::view::Records;
use tilolang_process::window::hosted::HostedWindow;
use ui::widget::Widget as _;

/// A phone nobody signed in to yet opens Notes over the documents it keeps itself and makes a
/// note; the note is still there when its window opens again. At sign-in, what it kept is carried
/// into the person's own workspace, which runs the app the phone carries, and the phone's window
/// joined there shows it. A carry whose connection cannot be made keeps the app's documents and
/// says why in words.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one story: an app used before sign-in, the sign-in, the carry, and what is refused"
)]
fn a_phone_carries_what_it_kept_before_sign_in_into_the_workspace() {
    let mut home = support::Home::new();
    let scratch = tempfile::tempdir().unwrap();
    // Notes as this build makes it: the file a workspace installs and the package this phone's
    // first start installs into its store, as the APK it runs from carried it.
    let (notes, _bundle) = server::notes(scratch.path());
    // The APK carries Notes: this phone's first start installs it into its store, and the
    // person's own workspace starts with it, so what the app kept before anyone signed in is
    // carried into a workspace that runs it.
    server::Server::install_on_device(&notes.tapp);
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
    let own = tilo_server_protocol::servers::person_workspace_id(
        &server.person.verifying_key().to_bytes(),
    );
    // The workspace runs Notes before the phone ever joins it.
    server.install(&notes.tapp);
    carried::declare();

    // Before anyone signed in to the phone, Notes runs over what the phone keeps itself, under
    // the directory its window keeps an app's documents in.
    let dir = scratch.path().join("tilolang");
    let mut window = signed_out_notes(&server, &dir);
    press("Notes offers New note", &mut window, "new_note");
    support::until("the note stands in the notebook", || {
        window.update(&(), &());
        note_in(&window)
    });
    drop(window);

    // The window opens again over the same documents: the note is still there.
    let mut window = signed_out_notes(&server, &dir);
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
    // The person's own workspace started with Notes, and the phone holds the version it
    // verified in its own store.
    support::until("the phone holds the version its workspace runs", || {
        tilo_runtime::device::package_runs::verified_payload(server::NOTES).map(drop)
    });

    // At sign-in, what the phone kept is carried into the person's own workspace, through the
    // connection its window would join there.
    let workspace = uuid::Uuid::parse_str(&own).expect("the workspace is one a route names");
    let made = |app: &str| {
        let bundle = tilo_phone::view::bundle_of(app)?;
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
    };
    support::until("what the phone kept is carried", || {
        let carried = tilolang_process::carry::signed_out(&dir, made)?;
        if carried.all_carried() {
            Ok(())
        } else {
            Err(format!("{:?}", carried.refused()))
        }
    });
    // The person's own workspace runs the app the phone carries.
    support::until(
        "the person's own workspace runs the app the phone carries",
        || {
            let apps = phone.apps(&own).map_err(|error| format!("{error:#}"))?;
            if apps.iter().any(|app| app.key == server::NOTES) {
                Ok(())
            } else {
                Err(format!("the workspace runs {apps:?}"))
            }
        },
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
        tilo_phone::view::open(server::NOTES, &own, Records::Workspace(connection))
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
fn signed_out_notes(server: &Server, dir: &std::path::Path) -> HostedWindow {
    support::until("Notes opens over what the phone keeps itself", || {
        tilo_phone::view::open(
            server::NOTES,
            &server.workspace,
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
