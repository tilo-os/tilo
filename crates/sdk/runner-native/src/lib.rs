//! The native desktop runner of the Tilo app SDK.
//!
//! `tilo-app` depends on this crate only on native targets, so the native
//! runner (winit, AccessKit, the Wayland/X11 clipboard, tokio's full runtime,
//! the local session daemons) never reaches a web build. Apps do not depend
//! on it; they call `ui::launch::run`, which resolves here on Linux and macOS.

use std::sync::Arc;

use tilo_action_line::LineClient;
use ui_core::launch::{LaunchError, LaunchSpec};

mod audio_output;

/// Run the app `spec` names, as [`ui_core::launch::run`] does.
///
/// A process the app's own Automerge parsing started as a worker (its binary again, with the
/// worker's argument) serves that and exits here, before the command line is read as the app's.
///
/// # Errors
/// The app could not be started.
#[expect(
    clippy::exit,
    reason = "an Automerge worker is the whole run of its process"
)]
pub fn run(spec: &LaunchSpec) -> Result<(), LaunchError> {
    if let Some(code) = tilo_automerge_isolate::dispatch_early() {
        std::process::exit(i32::from(code));
    }
    install_from_launcher();
    ui_core::launch::run(spec)
}

/// [`run`], printing the error and exiting with its status.
#[expect(
    clippy::exit,
    reason = "an Automerge worker is the whole run of its process"
)]
pub fn run_or_exit(spec: &LaunchSpec) {
    if let Some(code) = tilo_automerge_isolate::dispatch_early() {
        std::process::exit(i32::from(code));
    }
    install_from_launcher();
    ui_core::launch::run_or_exit(spec);
}

/// Carry the device's sound output and the person's presence on the connection the app's
/// launcher handed it. A host process has none, keeps its own sound output, and says no presence
/// this way.
fn install_from_launcher() {
    let Some(handed) = tilo_action_line::handed::connection() else {
        return;
    };
    audio_output::install(Arc::clone(&handed.line));
    ui_core::presence_line::install(Arc::new(Presence(handed.line)));
}

/// The person's presence words on the handed connection.
struct Presence(Arc<LineClient>);

impl ui_core::presence_line::PresenceLine for Presence {
    fn say(&self, said: &serde_json::Value) -> Result<(), String> {
        self.0.say_presence(said).map_err(|error| error.to_string())
    }

    fn heard(&self) -> Vec<String> {
        self.0.presence_heard()
    }
}
