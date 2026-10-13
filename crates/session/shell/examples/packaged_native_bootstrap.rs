//! Run the isolated packaged desktop proof in the builder's native environment.
//! This wrapper does not compile or launch applications itself: the shared
//! headless control example installs signed packages and uses shell actions.

use std::path::Path;
use std::process::{Command, ExitCode};

#[path = "packaged_native_bootstrap/native_document.rs"]
mod native_document;

fn main() -> Result<ExitCode, Box<dyn std::error::Error>> {
    if let Some(code) = tilo_automerge_isolate::dispatch_early() {
        std::process::exit(i32::from(code));
    }
    if matches!(
        std::env::args().nth(1).as_deref(),
        Some(
            "--check-native-document"
                | "--check-native-enrollment"
                | "--check-native-chat-record"
                | "--check-native-agent"
        )
    ) {
        native_document::check()?;
        return Ok(ExitCode::SUCCESS);
    }
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .ok_or("shell crate must be under crates/session/shell")?;
    let exe = std::env::current_exe()?;
    let binaries = exe
        .parent()
        .and_then(Path::parent)
        .ok_or("example executable must live in target/profile/examples")?;
    // The AT-SPI bus must be this proof's own ordinary session bus, not the
    // operator's desktop bus or a mock AccessKit tree.
    // Bus-activated services inherit the bus daemon's environment, not the
    // individual client's scratch profile. Isolate that parent too using the
    // repository's standard test environment before starting its normal bus.
    let status = Command::new("bash")
        .arg(repo.join("tools/with-test-environment.sh"))
        .arg("dbus-run-session")
        .arg("--")
        .arg("python3")
        .arg("-B")
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .arg(repo.join("scripts/headless-examples/packaged-native-bootstrap.py"))
        .arg("--binaries")
        .arg(binaries)
        .args(std::env::args_os().skip(1))
        .current_dir(repo)
        .status()?;
    Ok(if status.success() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}
