//! Offline account fixture, executed only in an isolated native build container.
use runtime_bundle::{
    BuildOptions, Identity, VerifyOptions, build_bundle_with, verify_bundle_with,
};
use std::{path::Path, process::Command};

fn fixture(root: &Path, work: &Path, operation: &str) -> Result<(), Box<dyn std::error::Error>> {
    let status = Command::new("python3")
        .current_dir(root)
        .arg("-B")
        .arg("tools/apps/signal_runtime_fixture.py")
        .arg(operation)
        .arg(work)
        .status()?;
    if !status.success() {
        return Err(format!("isolated Signal fixture {operation} failed: {status}").into());
    }
    Ok(())
}

fn package(root: &Path, work: &Path, identity: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let status = Command::new("bash")
        .arg(root.join("tools/apps/pack-runtime-package.sh"))
        .arg(work.join("bundle"))
        .args(["so.tilo.runtime.signal", "Signal provider", "0.1.0"])
        .arg(identity)
        .arg(root.join("target/debug"))
        .arg(work.join("package"))
        .status()?;
    if !status.success() {
        return Err(format!("normal signed runtime package fixture failed: {status}").into());
    }
    if !work.join("package/signal-0.1.0-x86_64.tapp").is_file() {
        return Err("normal runtime packager produced no thin Signal package".into());
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::consts::ARCH != "x86_64" {
        return Err("pinned Signal native runtime is not qualified on this architecture".into());
    }
    let identity_path = std::env::args_os()
        .nth(1)
        .map(std::path::PathBuf::from)
        .ok_or(
            "supply an existing explicitly authorized fixture signing identity; none is created",
        )?;
    let identity = Identity::load(&identity_path)?;
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .ok_or("workspace root is missing")?;
    let work = tempfile::tempdir()?;
    fixture(root, work.path(), "prepare")?;
    let extras = [
        work.path().join("signal-provider"),
        "/usr/lib64/libstdc++.so.6".into(),
        "/usr/lib64/libz.so.1".into(),
    ];
    let bundle = work.path().join("bundle");
    build_bundle_with(
        &bundle,
        "2026.10.1-signal",
        &BuildOptions {
            identity: Some(&identity_path),
            arch: Some("x86_64"),
            extra_libs: &extras,
            ..BuildOptions::default()
        },
    )?;
    verify_bundle_with(
        &bundle,
        &VerifyOptions {
            require_publisher: Some(identity.public_key_hex()),
            require_signature: true,
            require_arch: Some("x86_64".into()),
        },
    )?;
    fixture(root, work.path(), "probe")?;
    package(root, work.path(), &identity_path)?;
    // Reuse the production verifier to prove a swapped JVM is rejected before
    // any further launch. No profile/session keys or publisher are reused.
    std::fs::write(bundle.join("lib/signal-provider/java/bin/java"), b"changed")?;
    if verify_bundle_with(&bundle, &VerifyOptions::default()).is_ok() {
        return Err("runtime verifier accepted a replaced JVM".into());
    }
    println!("production runtime verifier rejected a replaced JVM");
    Ok(())
}
