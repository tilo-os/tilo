//! A window's app as the window-move tests start it: the session starts it through its launcher as
//! it starts any app's window, and it connects to the device's daemon the way the launcher hands an
//! app its connection, naming its app and workspace, then listens on its window's own route. When
//! the shell asks it to save what the window shows before the window moves, it asks the test how
//! the save goes and answers the daemon so, or quits mid-save.
//!
//! Arguments: the daemon's socket, the test's control socket, the app, the window's instance (empty
//! when the launch named none), then what the launcher was handed (`--workspace-id` among them).
//! On the control socket it says `listening <instance>` once it is, and reads one word per save:
//! `saved`, `failed`, or `quit`.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects,
    clippy::exit,
    reason = "a test fixture: a step it cannot take is the failure the test reads, and it quits mid-save when told to"
)]

#[cfg(target_os = "linux")]
fn main() {
    window::run();
}

#[cfg(not(target_os = "linux"))]
fn main() {}

#[cfg(target_os = "linux")]
mod window {
    use std::io::{BufRead as _, BufReader, Write as _};
    use std::os::unix::net::UnixStream;

    use collab_types::handoff::{HandoffReport, HandoffRequest};
    use tilo_broker_core::route::CrdtRoute;

    pub(super) fn run() {
        let arguments: Vec<String> = std::env::args().skip(1).collect();
        let [socket, control, app, instance, handed @ ..] = arguments.as_slice() else {
            panic!("the window probe takes its socket, control, app and instance: {arguments:?}");
        };
        let workspace = handed
            .iter()
            .position(|argument| argument == "--workspace-id")
            .and_then(|at| handed.get(at + 1))
            .expect("the launch names the workspace")
            .clone();
        let mut control = UnixStream::connect(control).ok();
        // A step it cannot take is said on the control socket, where the test reads it.
        if let Some(told) = control
            .as_ref()
            .and_then(|control| control.try_clone().ok())
        {
            let told = std::sync::Mutex::new(told);
            std::panic::set_hook(Box::new(move |panic| {
                if let Ok(mut told) = told.lock() {
                    let said = panic.to_string().replace('\n', " ");
                    let _ = writeln!(told, "panicked: {said}");
                }
            }));
        }
        let Ok(instance) = instance.parse::<u64>() else {
            if let Some(control) = control.as_mut() {
                let _ = writeln!(control, "no window instance");
            }
            return;
        };
        let mut connection = UnixStream::connect(socket).expect("the daemon's socket");
        launcher::services::write_collab_handoff(&mut connection, app, &workspace)
            .expect("the connection names the app and workspace, as the launcher's does");
        connection
            .set_nonblocking(true)
            .expect("the connection is ready for the runtime");
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("a runtime");
        runtime.block_on(async move {
            let connection =
                tokio::net::UnixStream::from_std(connection).expect("a runtime stream");
            let (reader, writer) = tokio::io::split(connection);
            let (client, carrying) = runtime_collab::CollabClient::over(reader, writer);
            tokio::spawn(carrying);
            let route =
                CrdtRoute::record(app, workspace.as_str(), format!("continuation:{instance}"));
            let window = client.record(&route.channel_key(), "");
            let asks = window
                .subscribe_context()
                .await
                .expect("the window listens on its own route");
            let mut said = control.map(|control| {
                let mut writer = control.try_clone().expect("the control socket");
                writeln!(writer, "listening {instance}").expect("the test hears the app listens");
                BufReader::new(control)
            });
            // The connection closing ends the app, as the daemon going away ends any app's.
            while let Ok(context) = asks.read().await {
                let Some(HandoffRequest::Checkpoint { token }) =
                    context.handoff.as_deref().and_then(HandoffRequest::decode)
                else {
                    continue;
                };
                let word = said.as_mut().map(|said| {
                    let mut word = String::new();
                    said.read_line(&mut word)
                        .expect("the test says how the save goes");
                    word
                });
                let report = match word.as_deref().map(str::trim) {
                    Some("saved") => HandoffReport::Done { token },
                    Some("failed") => HandoffReport::Failed {
                        token,
                        reason: "the disk is full".to_owned(),
                    },
                    // Quit mid-save, as an app that crashed does: the connection closes unanswered.
                    _ => std::process::exit(0),
                };
                window
                    .handoff(&report.encode())
                    .await
                    .expect("the daemon takes the answer");
            }
        });
    }
}
