//! The phone's own launch, as far as it goes off Android: its entry starts the device's daemon in
//! the app's process, and its window opens on that device with its records this device's own.
//! Nothing hands the phone a connection to a Tilo beside it, since the phone is Tilo here: a
//! launch that asked for one would end before the first frame. Before a person signs in the
//! device is no one's, and its daemon connects no window to a workspace.
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

use ui::launch::LaunchArgs;

tilo_automerge_isolate::test_worker!();

#[test]
fn the_phone_opens_its_window_on_the_device_it_started() {
    let home = support::Home::new();
    // What the APK's `android_main` does before it runs the window.
    tilo_phone::start().expect("the phone's device starts as its launch starts it");

    let spec = ui::framework::launch_spec::<tilo_phone::Phone>();
    let args = LaunchArgs::component();
    assert!(
        !ui::launch::connection_handed() && !spec.attaches_runtime(&args),
        "a phone no one signed in to hands its window no connection, and it attaches to none"
    );
    assert!(
        !ui::application::NativeProcess::connected(),
        "nothing handed the phone a connection"
    );
    let prepared = spec
        .prepare_hosted(&args)
        .expect("the window's records open on the device without a connection");
    let data = tilo_paths::data_dir().expect("the phone's data directory");
    let storage = prepared.context.storage();
    let app_files = storage.files();
    let paths = app_files.paths();
    let data_dir = paths.data();
    let files = data_dir
        .native_path()
        .expect("the window's files are on the device");
    assert!(
        files.starts_with(&data),
        "the window keeps its files in the app's own: {} under {}",
        files.display(),
        data.display()
    );
    let component = spec
        .prepare_component(&args, prepared.context, prepared.target, prepared.restore)
        .expect("the phone's window is built");
    let mut window =
        ui::headless::Driver::from_component(component, (412, 915), prepared.runner_inputs)
            .expect("the phone's window opens");
    window.relayout();
    let shown = window.inspection_json().expect("the window's contents");
    assert!(
        !shown.contains("did not start") && !shown.contains("daemon"),
        "the window shows the phone, not why it could not: {shown}"
    );

    // The window reads the device the launch started: the same one a file provider reaches.
    let devices = tilo_phone::files()
        .expect("the device runs")
        .ask(|device| device.devices().map_err(|error| format!("{error:#}")))
        .expect("the device answers")
        .expect("the device reads its devices");
    assert!(
        !devices.signed_in,
        "a new phone shows where a person signs in"
    );
    // Before anyone signed in, the device is no one's: it connects no window to a workspace, and
    // the sign-in window keeps what it has on the phone.
    let refused = tilo_phone::files()
        .expect("the device runs")
        .ask(|device| {
            device
                .window_connection("so.tilo.notes", "d1a4f6b2-3c58-4e07-9b6a-70e2c5d81f43")
                .err()
                .map(|error| format!("{error:#}"))
        })
        .expect("the device answers");
    assert!(
        refused
            .as_deref()
            .is_some_and(|why| why.contains("no one's")),
        "a phone no one signed in to connects no window to a workspace: {refused:?}"
    );
    assert!(
        !ui::application::NativeProcess::connected(),
        "the sign-in window holds no connection"
    );

    window.app_mut().close_components();
    drop(window);
    drop(home);
}
