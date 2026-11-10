// Shared build-time preparation for embedded image fixtures. The pinned bytes are in the
// tree (tools/test-assets/seed), so neither the build nor the runtime reaches the network.
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
    let seed = package.join(repository).join("tools/test-assets/seed");
    let manifest = package.join(manifest);
    println!("cargo::rerun-if-changed={}", tool.display());
    println!("cargo::rerun-if-changed={}", manifest.display());
    println!("cargo::rerun-if-env-changed=PYTHON");
    let python = std::env::var_os("PYTHON")
        .unwrap_or_else(|| if cfg!(windows) { "python" } else { "python3" }.into());
    let directory = PathBuf::from(std::env::var_os("OUT_DIR").ok_or("missing build output")?);
    let mut command = Command::new(python);
    // Only the tree's seed, read and verified, never written: a manifest asset the seed
    // lacks fails the build by name instead of being fetched.
    command
        .arg(tool)
        .arg(manifest)
        .arg("--offline")
        .arg("--cache-root")
        .arg(seed);
    if let Some(filename) = filename {
        command.args(["--file", filename]);
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
