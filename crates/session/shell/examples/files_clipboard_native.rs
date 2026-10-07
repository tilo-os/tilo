//! Run the signed Files entrypoint journey in the existing remote test environment.
use std::path::Path;
use std::process::{Command, ExitCode};

fn main() -> Result<ExitCode, Box<dyn std::error::Error>> {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .ok_or("shell crate must be under crates/session/shell")?;
    let executable = std::env::current_exe()?;
    let binaries = executable
        .parent()
        .and_then(Path::parent)
        .ok_or("example must live in target/profile/examples")?;
    let status = Command::new("bash")
        .arg(repo.join("tools/with-test-environment.sh"))
        .args(["dbus-run-session", "--", "python3", "-B"])
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .arg(repo.join("scripts/headless-examples/files-clipboard-native-proof.py"))
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
