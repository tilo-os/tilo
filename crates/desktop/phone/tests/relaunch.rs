//! A phone a person signed in to is theirs: launched again, its own window joins their own
//! workspace as the window of an app their computer launches does, and what it keeps is the workspace's.
//! The phone is its own launcher, so its launch makes the window's connection itself and hands it
//! to the window, which the launch then builds and joins exactly as it builds any window.
//!
//! The daemon reads its places from the process's environment, and a process hands its first
//! window one connection, so this binary is one test, as `ticket.rs` is.

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

use collab_types::store::wire::{Command, Reply};
use server::Server;
use ui::launch::LaunchArgs;

/// The phone app's id: the app its window joins the workspace as.
const PHONE: &str = "so.tilo.phone";

#[test]
fn a_signed_in_phone_opens_its_window_in_the_workspace() {
    let (home, server) = Server::for_phone();
    let laptop = server.laptop();

    // The phone's first launch: the person signs in with the ticket their laptop printed, and
    // the phone closes.
    {
        let mut phone = tilo_runtime::in_process::Device::start(&support::profile())
            .expect("the phone's daemon");
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
        phone.stop().expect("the phone's daemon stops");
    }

    // Launched again, as the APK's `android_main` launches it: the device starts, and its window
    // is handed its connection to the person's workspace.
    tilo_phone::start().expect("the phone's device starts as its launch starts it");
    // No launcher binds the phone to an identity service: its windows write as the person the
    // device's certificate names, which the workspace checks every change of theirs against.
    assert_eq!(
        ui::collaboration::local_user_id().id(),
        hex::encode(server.person.verifying_key().to_bytes()),
        "the phone's windows act as the person who signed in"
    );
    let spec = ui::framework::launch_spec::<tilo_phone::Phone>();
    let args = LaunchArgs::component();
    assert!(
        ui::launch::connection_handed() && spec.attaches_runtime(&args),
        "a phone someone signed in to joins its window to their workspace"
    );
    let app = spec
        .build_window(&args)
        .expect("the phone's window opens, joined to the workspace");
    let mut window = ui::headless::Driver::showing(app, (412, 915));
    assert!(
        ui::application::NativeProcess::connected(),
        "the window reads and writes its records over its connection"
    );
    let shown = window.inspection_json().expect("the window's contents");
    assert!(
        !shown.contains("did not start") && !shown.contains("couldn't connect"),
        "the window shows the phone, not why it could not: {shown}"
    );

    // Before the person picks a workspace the window is in their own: their server, which keeps
    // it for their other devices, holds the window's record.
    let own = server.own();
    let kept = support::until(
        "the phone's window record is in the person's own workspace",
        || match windows_in(&server, &own, &mut window)? {
            0 => Err("the person's own workspace holds no window of the phone yet".to_owned()),
            count => Ok(count),
        },
    );
    assert!(kept > 0);
    assert_eq!(
        windows_in(&server, &server.workspace, &mut window),
        Ok(0),
        "the window is in the person's own workspace, not in Studio, before they pick it"
    );

    let mut window = pick_studio(&server, window, &spec, &args);
    let kept = support::until(
        "the phone's window record is in Studio",
        || match windows_in(&server, &server.workspace, &mut window)? {
            0 => Err("Studio holds no window of the phone yet".to_owned()),
            count => Ok(count),
        },
    );
    assert!(kept > 0);
    assert!(
        named_rect(&window, "Back").is_some(),
        "the window built again shows Studio's page, the pick kept: {}",
        window.inspection_json().unwrap_or_default()
    );
    assert!(
        !ui::launch::window_gives_place(),
        "the window built again stays in Studio"
    );
    // The phone shows its own page in place of the record its window was built on, and goes on
    // over the connection it was handed: there is no other for it to take.
    assert!(
        ui::application::NativeProcess::connected(),
        "the window keeps its connection once it shows the phone's page"
    );

    window.app_mut().close_components();
    drop(window);
    drop(laptop);
    drop(home);
}

/// Where the window shows the control or text a person reads as `name`, as `[x, y, width,
/// height]`.
fn named_rect(window: &ui::headless::Driver, name: &str) -> Option<[f64; 4]> {
    window
        .inspection()
        .root
        .nodes()
        .into_iter()
        .find(|node| {
            node.rect[2] > 0.0
                && node.rect[3] > 0.0
                && node
                    .semantics
                    .as_ref()
                    .is_some_and(|semantics| semantics.name.as_deref() == Some(name))
        })
        .map(|node| node.rect)
}

/// The person picks Studio on the phone's page: the window gives its place and is built again
/// joined to Studio, as the launch builds it, so what it keeps from then on is Studio's. The pick
/// is the device's, so the window built again shows Studio's page, not the list.
fn pick_studio(
    server: &Server,
    mut window: ui::headless::Driver,
    spec: &ui::launch::LaunchSpec,
    args: &LaunchArgs,
) -> ui::headless::Driver {
    let studio = support::until("the phone lists Studio", || {
        let _ = window.poll_frame();
        named_rect(&window, server::WORKSPACE_NAME).ok_or_else(|| {
            format!(
                "no Studio row in {}",
                window.inspection_json().unwrap_or_default()
            )
        })
    });
    window.tap_at(studio[0] + studio[2] / 2.0, studio[1] + studio[3] / 2.0);
    support::until("the window gives its place to join Studio", || {
        let _ = window.poll_frame();
        if ui::launch::window_gives_place() {
            Ok(())
        } else {
            Err("the window keeps its place".to_owned())
        }
    });
    window.app_mut().close_components();
    drop(window);
    let app = spec
        .rejoined_window(args)
        .expect("the window gave its place to join another workspace")
        .expect("the phone's window opens again, joined to Studio");
    let window = ui::headless::Driver::showing(app, (412, 915));
    assert_eq!(
        ui::process::environment().identity.workspace_id.as_deref(),
        Some(server.workspace.as_str()),
        "the window built again takes part in Studio"
    );
    window
}

/// How many windows of the phone `workspace` keeps on the person's server, after the window's
/// next frame.
fn windows_in(
    server: &Server,
    workspace: &str,
    window: &mut ui::headless::Driver,
) -> Result<usize, String> {
    let _ = window.poll_frame();
    let reply = server
        .runtime
        .block_on(server.workspaces().own_resource_command(
            workspace,
            PHONE,
            Command::Count {
                kind: tilolang_process::window::RESOURCE_TYPE.to_owned(),
                filters: Vec::new(),
            },
            Vec::new(),
        ))?;
    match reply {
        Reply::Count(count) => Ok(count),
        other => Err(format!("the server answers {other:?}")),
    }
}
