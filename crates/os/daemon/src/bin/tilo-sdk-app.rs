//! The native SDK app the daemon's process tests install as a development build.
//!
//! It is what a native SDK program does: names the app this computer launched it as, attaches
//! itself to the daemon that started it, publishes the actions it offers, and stays until the
//! daemon ends. Its package may name a lifetime (`tilo-app-lifetime-ms` beside the program), so a
//! test can let one app end while another keeps running.
//!
//! Nothing here ships: the daemon package installs `tilo-runtime` alone.

use std::time::{Duration, Instant};

use collaboration::{CollaborationManager, CollaborationState};

/// How long the app waits for its connection to the daemon before giving up.
const CONNECT_BOUND: Duration = Duration::from_secs(60);

fn main() -> std::process::ExitCode {
    let Ok(app) = std::env::var("TILO_PACKAGE_ID") else {
        eprintln!("tilo-sdk-app: this program is an app: it is started by the launcher for one");
        return std::process::ExitCode::FAILURE;
    };
    if let Err(error) = runtime_portal::native::ensure_attached(&app) {
        eprintln!("tilo-sdk-app: attaching {app}: {error}");
        return std::process::ExitCode::FAILURE;
    }
    let mut manager = CollaborationManager::new();
    let started = Instant::now();
    while manager.state() == CollaborationState::Connecting && started.elapsed() < CONNECT_BOUND {
        let _ = manager.poll();
        std::thread::sleep(Duration::from_millis(10));
    }
    if !manager.is_connected() {
        eprintln!("tilo-sdk-app: {app} never connected: {:?}", manager.state());
        return std::process::ExitCode::FAILURE;
    }
    manager.publish_action_directory(collaboration::local_app_id());
    // A package may ask for a second process of the same app: a program that starts another, as
    // an app's own child does. The child inherits the services this process was handed, and
    // attaches as an instance of its own.
    let child = spawn_child();
    let deadline = lifetime().map(|life| Instant::now() + life);
    // An app ends with the session it was launched for: the daemon going away is this app's
    // computer stopping. A lifetime a test names ends it while the app it runs beside stays.
    while !matches!(manager.state(), CollaborationState::Offline(_))
        && deadline.is_none_or(|deadline| Instant::now() < deadline)
    {
        let _ = manager.poll();
        std::thread::sleep(Duration::from_millis(10));
    }
    if let Some(mut child) = child {
        let _ = child.kill();
        let _ = child.wait();
    }
    std::process::ExitCode::SUCCESS
}

/// A second process of this app, when the package beside the program asks for one
/// (`tilo-app-spawn-child`): the child is this program again, told it is the child, so it spawns
/// no third.
fn spawn_child() -> Option<std::process::Child> {
    if std::env::var_os("TILO_SDK_APP_CHILD").is_some() || beside("tilo-app-spawn-child").is_none() {
        return None;
    }
    let program = std::env::current_exe().ok()?;
    std::process::Command::new(program)
        .env("TILO_SDK_APP_CHILD", "1")
        .spawn()
        .ok()
}

/// The file `name` beside this program, in the package's payload.
fn beside(name: &str) -> Option<std::path::PathBuf> {
    let path = std::env::current_exe().ok()?.parent()?.join(name);
    path.is_file().then_some(path)
}

/// How long the package beside this program says it lives: `tilo-app-lifetime-ms`, when present.
fn lifetime() -> Option<Duration> {
    let text = std::fs::read_to_string(beside("tilo-app-lifetime-ms")?).ok()?;
    let millis = text.trim().parse::<u64>().ok()?;
    Some(Duration::from_millis(millis))
}
