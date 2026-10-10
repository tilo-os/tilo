//! Explicit Linux qualification setup: real signed package and runtime only.
//! This creates no process witness or authority; callers must use normal
//! launcher, private-channel and kernel admission. Never shipped by default.
use super::{InstalledManagedPackage, ManagedRuntime, PackageTrust};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
};

/// Build and independently verify the signed executable fixture.
///
/// # Errors
/// The executable is absent, signing/verification fails, or storage is unavailable.
pub fn fixture_package(
    root: &Path,
    executable: &Path,
) -> Result<(InstalledManagedPackage, PackageTrust), String> {
    let mut seed = [0; 32];
    File::open("/dev/urandom")
        .map_err(|error| error.to_string())?
        .read_exact(&mut seed)
        .map_err(|error| error.to_string())?;
    let key = ed25519_dalek::SigningKey::from_bytes(&seed);
    let public = key.verifying_key().to_bytes();
    let public_hex = hex::encode(public);
    let manifest = tilo_app_package::Manifest::from_toml_str(&format!(
        r#"
[package]
id = "so.tilo.agent"
version = "1.0.0"
kind = "service"
runtime = "tilo-runtime/2026.07"
[package.isa]
x86_64 = "baseline"
aarch64 = "armv8-a"
[publisher]
identity = "ed25519:{public_hex}"
signature = ""
[sandbox]
namespaces = ["user", "pid", "uts", "ipc", "mount"]
seccomp = "tilo-default"
landlock = "strict"
[sandbox.net]
mode = "off"
[sandbox.services]
collab = "required@1"
model-gateway = "required@1"
[entrypoint]
bin = "bin/agent"
type = "cli"
"#
    ))
    .map_err(|error| error.to_string())?;
    let source = root.join("package-source");
    std::fs::create_dir_all(source.join("bin")).map_err(|error| error.to_string())?;
    std::fs::copy(executable, source.join("bin/agent")).map_err(|error| error.to_string())?;
    let packed = tilo_app_package::pack(
        &source,
        &manifest,
        &tilo_app_package::SigningIdentity::from_signing_key(key),
    )
    .map_err(|error| error.to_string())?;
    let id = packed.id();
    let trust = PackageTrust {
        publishers: vec![public],
        attestors: vec![],
        revoked: tilo_app_package::trust::RevocationList::default(),
    };
    if !matches!(
        tilo_app_package::verify_with_trust(
            &packed.meta,
            &trust.publishers,
            &trust.attestors,
            &trust.revoked
        ),
        tilo_app_package::Verification::Trusted { .. }
    ) {
        return Err("fixture package publisher was not trusted".into());
    }
    let payload = root.join("installed-payload");
    tilo_app_package::unpack(&packed.meta, &packed, &payload).map_err(|error| error.to_string())?;
    std::fs::write(payload.join("manifest.toml"), &packed.meta.manifest_bytes)
        .map_err(|error| error.to_string())?;
    let evidence = package_store::integrity::legacy_record_dir(root, "so.tilo.agent");
    std::fs::create_dir_all(&evidence).map_err(|error| error.to_string())?;
    for (name, bytes) in [
        ("signed-root", packed.meta.signed_root_bytes().to_vec()),
        ("manifest.toml", packed.meta.manifest_bytes.clone()),
        ("collection.qcl", packed.meta.collection_bytes),
    ] {
        std::fs::write(evidence.join(name), bytes).map_err(|error| error.to_string())?;
    }
    let package = InstalledManagedPackage::verify(
        super::agent_package_row(id.to_hex(), payload.clone(), payload.join("manifest.toml")),
        root.to_path_buf(),
        id,
        &trust,
    )?;
    Ok((package, trust))
}

/// Build a signed runtime with a task-private trust root.
///
/// # Errors
/// Runtime assembly, private-key storage or independent publisher verification fails.
pub fn fixture_runtime(root: &Path, sandbox_shim: &Path) -> Result<ManagedRuntime, String> {
    let identity = runtime_bundle::Identity::generate();
    let identity_path = root.join("runtime-development-identity.json");
    let mut key_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&identity_path)
        .map_err(|error| error.to_string())?;
    key_file
        .write_all(&identity.to_json_bytes())
        .map_err(|error| error.to_string())?;
    if key_file
        .metadata()
        .map_err(|error| error.to_string())?
        .permissions()
        .mode()
        & 0o777
        != 0o600
    {
        return Err("fixture runtime identity permissions changed".into());
    }
    let bundle_root = root.join("runtime");
    let bundle = runtime_bundle::build_bundle_with(
        &bundle_root,
        "2026.07",
        &runtime_bundle::BuildOptions {
            from: None,
            arch: Some(std::env::consts::ARCH),
            identity: Some(&identity_path),
            extra_libs: &[],
        },
    )
    .map_err(|error| error.to_string())?;
    // Trust is separately selected from the freshly generated task-private key.
    if bundle.publisher_hex.as_deref() != Some(identity.public_key_hex().as_str()) {
        return Err("fixture runtime publisher changed".into());
    }
    Ok(ManagedRuntime {
        bundle_root,
        runtime_publisher: identity.public_key_hex(),
        sandbox_shim: sandbox_shim.into(),
        bwrap: std::env::var_os("TILO_MANAGED_TEST_BWRAP")
            .unwrap_or_else(|| "/usr/bin/bwrap".into())
            .into(),
    })
}
