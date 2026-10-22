// Shared build-time preparation for embedded image fixtures. Runtime stays offline.
use std::path::PathBuf;
use std::process::Command;

fn prepare_fixture_assets(
    repository: &str,
    manifest: &str,
    filename: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let package =
        PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").ok_or("missing package root")?);
    let tool = package.join(repository).join("tools/test-assets.py");
    let manifest = package.join(manifest);
    println!("cargo::rerun-if-changed={}", tool.display());
    println!("cargo::rerun-if-changed={}", manifest.display());
    for key in [
        "PYTHON",
        "TILO_TEST_ASSET_CACHE",
        "TILO_TEST_ASSET_SEED",
        "TILO_TEST_ASSETS_OFFLINE",
        "CARGO_NET_OFFLINE",
    ] {
        println!("cargo::rerun-if-env-changed={key}");
    }
    let python = std::env::var_os("PYTHON")
        .unwrap_or_else(|| if cfg!(windows) { "python" } else { "python3" }.into());
    let directory = PathBuf::from(std::env::var_os("OUT_DIR").ok_or("missing build output")?);
    let mut command = Command::new(python);
    command.arg(tool).arg(manifest);
    // TILO_TEST_ASSET_SEED, inherited from the environment, names read-only caches the
    // tool takes verified assets from before it fetches (tools/test-assets.py).
    // A build script writes only under OUT_DIR: the source tree may be read-only, as it
    // is in the check containers. TILO_TEST_ASSET_CACHE still names a shared cache.
    if std::env::var_os("TILO_TEST_ASSET_CACHE").is_none() {
        command
            .arg("--cache-root")
            .arg(directory.join("test-assets"));
    }
    if let Some(filename) = filename {
        command.args(["--file", filename]);
    }
    if std::env::var("CARGO_NET_OFFLINE").is_ok_and(|value| value == "true") {
        command.arg("--offline");
    }
    let output = command.output()?;
    if !output.status.success() {
        return Err(format!(
            "fixture preparation failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    for source in String::from_utf8(output.stdout)?.lines() {
        let source = PathBuf::from(source);
        let name = source.file_name().ok_or("missing fixture filename")?;
        std::fs::copy(&source, directory.join(name))?;
    }
    Ok(())
}
