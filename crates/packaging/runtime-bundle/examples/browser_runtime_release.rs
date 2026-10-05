//! Explicit native proof of the maintained Browser engine release pipeline.
//! Builds only the pinned engine and release CLIs, then verifies a signed thin package.
use std::error::Error;
use std::path::Path;
use std::process::Command;

fn main() -> Result<(), Box<dyn Error>> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let source = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&root)
            .output()?
            .stdout,
    )?;
    let architecture = std::env::consts::ARCH;
    let output = "target/browser-runtime-release-proof";
    for mode in ["compile", "seal"] {
        let mut command = Command::new("python3");
        command
            .current_dir(&root)
            .args([
                "tools/apps/browser_runtime_release.py",
                mode,
                "--out",
                output,
                "--arch",
                architecture,
                "--source",
                source.trim(),
            ])
            .env("REQUESTED_PUBLISH", "false")
            .env_remove("RUNTIME_BUNDLE_IDENTITY_FILE")
            .env_remove("TAPP_PUBLISHER_IDENTITY_FILE")
            .env_remove("TILO_ENGINE_VERIFIER_BIN_DIR");
        if mode == "seal" {
            command.args(["--sequence", "1"]);
        }
        let status = command.status()?;
        if !status.success() {
            return Err(format!("native engine release {mode} failed: {status}").into());
        }
    }
    Ok(())
}
