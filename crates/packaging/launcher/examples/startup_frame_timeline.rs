//! Execute the synthetic native-app startup proof in the build environment.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// The checkout this program runs in: the tree above it that holds the tool. A build made in
/// another checkout (`/src/tilo` on a builder) names that one at compile time, and the tree beside
/// the executable is the one whose tools run here.
fn repo(exe: &Path) -> PathBuf {
    let tool = Path::new("scripts/headless-examples/startup-frame-timeline.py");
    exe.ancestors()
        .skip(1)
        .find(|directory| directory.join(tool).is_file())
        .map_or_else(
            || {
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .ancestors()
                    .nth(3)
                    .unwrap_or(Path::new("."))
                    .to_path_buf()
            },
            Path::to_path_buf,
        )
}

fn main() -> Result<ExitCode, Box<dyn std::error::Error>> {
    let exe = std::env::current_exe()?;
    let repo = repo(&exe);
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
        let session = tilo_paths::login::session_dir(std::env::var_os("XDG_RUNTIME_DIR"))
            .ok_or("the runtime directory names no session")?;
        // The display the session published for this login, as any program it starts reads it.
        let display =
            std::fs::read_to_string(session.join(tilo_paths::login::WAYLAND_DISPLAY_FILE))?;
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
