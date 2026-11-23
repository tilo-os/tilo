// The signed host and native backend use one exact capture admission.
#[path = "../../crates/os/imager-backend/capture_build.rs"]
mod capture_build;

use std::{env, fs, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    release_catalog()?;
    capture_build::main()
}

/// The catalogue of media the installer offers: the reviewed one beside the crate, or a release
/// tag's, which names the media that tag built and signed (`TILO_RELEASE_CATALOG`).
fn release_catalog() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-env-changed=TILO_RELEASE_CATALOG");
    let source = env::var_os("TILO_RELEASE_CATALOG").map_or_else(
        || {
            Ok::<_, env::VarError>(
                PathBuf::from(env::var("CARGO_MANIFEST_DIR")?).join("release-catalog.json"),
            )
        },
        |path| Ok(PathBuf::from(path)),
    )?;
    println!("cargo:rerun-if-changed={}", source.display());
    fs::copy(
        &source,
        PathBuf::from(env::var("OUT_DIR")?).join("release-catalog.json"),
    )?;
    Ok(())
}
