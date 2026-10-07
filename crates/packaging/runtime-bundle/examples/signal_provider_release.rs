//! Managed native proof of Signal's credential-free release compilation.
//! The caller supplies an admitted source commit; no signing identity is created.
use std::{error::Error, path::Path, process::Command};

fn main() -> Result<(), Box<dyn Error>> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let [source, commit] = arguments.as_slice() else {
        return Err("supply the exact revision and encoded commit from source-proof".into());
    };
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let output = format!("target/signal-provider-release-proof-{source}");
    let search_path = std::env::var("PATH")?;
    let status = Command::new("python3")
        .current_dir(&root)
        .args([
            "-I",
            "tools/ci/python.py",
            "tools/apps/signal_provider_release.py",
            "native-proof",
            "--out",
            &output,
            "--source",
            source,
            "--source-commit",
            commit,
        ])
        .env("REQUESTED_PUBLISH", "false")
        .env(
            "PATH",
            format!("/usr/lib/jvm/java-25-openjdk/bin:{search_path}"),
        )
        .env("CARGO_BUILD_JOBS", "2")
        .env("CARGO_PROFILE_RELEASE_LTO", "thin")
        .env("CARGO_PROFILE_RELEASE_CODEGEN_UNITS", "16")
        .status()?;
    if !status.success() {
        return Err(format!("native Signal release compilation failed: {status}").into());
    }
    println!("exact-source Signal provider and Java adaptations compiled without signing");
    Ok(())
}
