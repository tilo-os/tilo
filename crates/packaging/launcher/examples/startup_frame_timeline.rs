//! Execute the synthetic native-app startup proof in the build environment.

use std::path::Path;
use std::process::{Command, ExitCode};

fn main() -> Result<ExitCode, Box<dyn std::error::Error>> {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .ok_or("launcher crate must live under crates/packaging")?;
    let exe = std::env::current_exe()?;
    let binaries = exe
        .parent()
        .and_then(Path::parent)
        .ok_or("example must live under target/profile/examples")?;
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.first().is_some_and(|arg| arg == "--launch-probe") {
        let payload = Path::new(args.get(1).ok_or("missing probe payload")?);
        std::fs::write(payload.join("probe.pid"), std::process::id().to_string())?;
        let runtime = std::env::var_os("XDG_RUNTIME_DIR").ok_or("missing private runtime")?;
        let runtime = Path::new(&runtime);
        let display = std::fs::read_to_string(runtime.join("tilo-session/wayland-display"))?;
        let log = std::fs::File::create(payload.join("probe.log"))?;
        let status = Command::new(binaries.join("tilo-launcher"))
            .args(["run", "--manifest"])
            .arg(payload.join("manifest.toml"))
            .arg("--payload-root")
            .arg(payload)
            .arg("--wayland-socket")
            .arg(runtime.join(display.trim()))
            .env(
                "TILO_STARTUP_METRICS_PATH",
                "/run/tilo/app/.local/state/startup-frame-probe.json",
            )
            .stdout(log.try_clone()?)
            .stderr(log)
            .status()?;
        return Ok(if status.success() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        });
    }
    let status = Command::new("bash")
        .arg(repo.join("tools/with-test-environment.sh"))
        .args(["dbus-run-session", "--", "python3", "-B"])
        .arg(repo.join("scripts/headless-examples/startup-frame-timeline.py"))
        .arg("--binaries")
        .arg(binaries)
        .args(args)
        .current_dir(repo)
        .status()?;
    Ok(if status.success() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}
