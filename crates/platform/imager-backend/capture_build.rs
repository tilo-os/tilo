// Capture trust inputs are selected at build time and embedded in the signed host.
use std::{env, fs, io, path::PathBuf, process::Command};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-env-changed=TILO_REQUIRE_CAPTURE");
    println!("cargo:rerun-if-env-changed=TILO_CAPTURE_PINS");
    println!("cargo:rerun-if-env-changed=TILO_CAPTURE_SIDECAR");
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR")?);
    let manifest = if env::var("CARGO_PKG_NAME").as_deref() == Ok("imager-backend") {
        manifest.join("../../../apps/imager")
    } else {
        manifest
    };
    for input in ["capture-service.json", "packaging/admit-capture-sidecar.py"] {
        println!("cargo:rerun-if-changed={}", manifest.join(input).display());
    }
    let bytes = if env::var_os("CARGO_FEATURE_CAPTURE_WORKER").is_some() {
        if env::var("TILO_REQUIRE_CAPTURE").as_deref() == Ok("1") {
            return Err(io::Error::other(
                "a release host must be built separately from the capture worker bootstrap graph",
            )
            .into());
        }
        b"null\n".to_vec()
    } else {
        match (
            env::var_os("TILO_CAPTURE_PINS"),
            env::var_os("TILO_CAPTURE_SIDECAR"),
        ) {
            (None, None) => {
                if env::var("TILO_REQUIRE_CAPTURE").as_deref() == Ok("1") {
                    return Err(io::Error::other(
                        "release migration requires signed capture pins and sidecar",
                    )
                    .into());
                }
                let defaults = fs::read(manifest.join("capture-service.json"))?;
                if defaults
                    .iter()
                    .copied()
                    .filter(|byte| !byte.is_ascii_whitespace())
                    .collect::<Vec<_>>()
                    != b"null"
                {
                    return Err(io::Error::other(
                        "default capture pins must not bypass signed sidecar admission",
                    )
                    .into());
                }
                defaults
            }
            (Some(pins), Some(sidecar)) => {
                if env::var("CARGO_CFG_TARGET_OS")? != "linux" {
                    return Err(io::Error::other("capture is Linux-only").into());
                }
                println!("cargo:rerun-if-changed={}", PathBuf::from(&pins).display());
                println!(
                    "cargo:rerun-if-changed={}",
                    PathBuf::from(&sidecar).display()
                );
                let output = Command::new("python3")
                    .arg(manifest.join("packaging/admit-capture-sidecar.py"))
                    .arg("--pins")
                    .arg(pins)
                    .arg("--sidecar")
                    .arg(sidecar)
                    .arg("--arch")
                    .arg(env::var("CARGO_CFG_TARGET_ARCH")?)
                    .output()?;
                if !output.status.success() {
                    return Err(io::Error::other(format!(
                        "capture admission failed: {}",
                        String::from_utf8_lossy(&output.stderr)
                    ))
                    .into());
                }
                if output.stdout.is_empty() || output.stdout.len() > 16 * 1024 {
                    return Err(io::Error::other("capture admission output exceeds bounds").into());
                }
                output.stdout
            }
            _ => {
                return Err(io::Error::other(
                    "capture pins and their exact sidecar must both be selected at build time",
                )
                .into());
            }
        }
    };
    fs::write(
        PathBuf::from(env::var("OUT_DIR")?).join("capture-service.json"),
        bytes,
    )?;
    Ok(())
}
