//! A phone a person signed in to is theirs: launched again, its own window joins their workspace
//! as the window of an app their computer launches does, and what it keeps is the workspace's.
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
    let mut home = support::Home::new();
    // Both launches run in this one process, where the daemon that ended has not let go of the
    // fixed port a device takes its workspaces' work on by the time the second starts; a phone
    // launched again is a process of its own, which finds it free. Each launch here binds a
    // port of its own instead.
    let mut port = tilo_test_env::ScopedEnv::new();
    port.set("TILO_DEVICE_PORT", "0");
    let server = Server::start();
    home.relay(&server.relay_url());
    let laptop = support::Issuing::start(
        server.runtime.handle(),
        &server.person,
        server.entry(),
        server.workspaces(),
    )
    .expect("the laptop's ticket and endpoint are made");

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

    // The window's own record is the workspace's: the person's server, which keeps the workspace
    // for their other devices, holds it.
    let kept = support::until(
        "the phone's window record is on the person's server",
        || {
            let _ = window.poll_frame();
            let reply = server
                .runtime
                .block_on(server.workspaces().own_resource_command(
                    &server.workspace,
                    PHONE,
                    Command::Count {
                        kind: tilolang_process::window::RESOURCE_TYPE.to_owned(),
                        filters: Vec::new(),
                    },
                    Vec::new(),
                ))?;
            match reply {
                Reply::Count(count) if count > 0 => Ok(count),
                other => Err(format!("the server answers {other:?}")),
            }
        },
    );
    assert!(kept > 0);
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
