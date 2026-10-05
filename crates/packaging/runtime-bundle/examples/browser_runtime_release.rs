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
    let package = root.join(output).join("package");
    let payload_blob = package
        .read_dir()?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|value| value == "blob"))
        .ok_or("signed engine package contains no payload blob")?;
    let hidden_blob = payload_blob.with_extension("missing");
    let thin_package = package
        .read_dir()?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|entry| entry.path())
        .find(|path| path.extension().is_some_and(|value| value == "tapp"))
        .ok_or("signed engine package contains no thin archive")?;
    std::fs::rename(&payload_blob, &hidden_blob)?;
    let result = Command::new(root.join(output).join("bin/tilo-app-package"))
        .args(["entry", "--arch", architecture, "--objects"])
        .arg(&package)
        .arg("--identity")
        .arg(root.join(output).join("work/app-identity.json"))
        .args(["--url", "https://apps.tilo.so/artifacts/engine.tapp"])
        .arg(thin_package)
        .output();
    std::fs::rename(hidden_blob, &payload_blob)?;
    let result = result?;
    if result.status.success() {
        return Err("canonical engine verifier accepted a missing signed payload object".into());
    }
    let stderr = String::from_utf8(result.stderr)?;
    let removed_hash = payload_blob
        .file_stem()
        .ok_or("payload object has no hash")?
        .to_string_lossy();
    if !stderr.contains(removed_hash.as_ref())
        || !(stderr.contains("no payload object for blob") || stderr.contains(" is missing"))
    {
        return Err(format!(
            "canonical verifier failed without identifying the missing payload: {stderr}"
        )
        .into());
    }
    println!("native signed engine pipeline passed; missing payload object rejected");
    Ok(())
}
