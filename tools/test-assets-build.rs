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
    // What the tests that read the fixtures are told where they are not here: `…/*.rs` for a
    // test that embeds one at compile time (tilo-doc-codecs), `…*.txt` for one that reads it
    // (tilo-pdf). A build that compiles no test needs neither, so a missing set is no failure.
    let unavailable = directory.join("fixtures-unavailable.txt");
    let _ = std::fs::remove_file(&unavailable);
    let mut command = Command::new(python);
    // Isolated, as the CI helpers in `tools/ci` are run: without it Python puts the script's
    // directory on `sys.path` and lists it looking its imports up, so every package including
    // this script would read the whole `tools/` tree and rerun when any file in it changes. The
    // tool imports nothing of its own.
    command.arg("-I").arg(tool).arg(&manifest);
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
        let why = String::from_utf8_lossy(&output.stderr);
        let why: String = why.trim().chars().take(2000).collect();
        let said = format!(
            "the pinned fixtures of {} are not here, and fetching them failed: {why}\n\
             A build that compiles no test needs none of them; a test build does. Run the \
             `test-assets` check and build where its output is given, or set TILO_TEST_ASSET_SEED \
             to that output and TILO_TEST_ASSET_CACHE to a directory the tool can write.",
            manifest.display()
        );
        println!("cargo::warning={}", said.replace('\n', " "));
        std::fs::write(&unavailable, &said)?;
        let literal = said.replace('\\', "\\\\").replace('"', "\\\"");
        std::fs::write(
            directory.join("fixtures-unavailable.rs"),
            format!("compile_error!(\"{literal}\");\n"),
        )?;
        return Ok(());
    }
    for source in String::from_utf8(output.stdout)?.lines() {
        let source = PathBuf::from(source);
        let name = source.file_name().ok_or("missing fixture filename")?;
        std::fs::copy(&source, directory.join(name))?;
    }
    std::fs::write(directory.join("fixtures-unavailable.rs"), "")?;
    Ok(())
}
