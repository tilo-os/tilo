//! Signing and verifying a `.tapp` in process, so `tilolang package` needs no
//! separate `tilo-app-package` binary.

use std::path::Path;

use anyhow::{Context, Result, anyhow};
use tilo_app_package::{BlobSource, Manifest, SigningIdentity, pack};

/// Reads a publisher key file (see [`registry_types::key_file`]).
pub fn load_identity(path: &Path) -> Result<SigningIdentity> {
    SigningIdentity::from_key_file(path)
        .with_context(|| format!("publisher key {}", path.display()))
}

/// What [`pack_manifest`] produced.
pub struct Packed {
    pub id: String,
    pub bytes: u64,
}

/// Signs `payload` with `manifest` as a full `.tapp` at `output`, declaring
/// `resource_kinds` as the kinds the app's records hold, then reads it back
/// and verifies it against its own publisher key.
pub fn pack_manifest(
    mut manifest: Manifest,
    resource_kinds: Vec<String>,
    payload: &Path,
    output: &Path,
    identity: &SigningIdentity,
) -> Result<Packed> {
    manifest.publisher.identity = format!("ed25519:{}", hex::encode(identity.public_key_bytes()));
    manifest.resources.kinds = resource_kinds;
    let packed =
        pack(payload, &manifest, identity).map_err(|error| anyhow!("pack failed: {error}"))?;

    let written = tilo_app_package::tapp::write_file(
        output,
        &packed.meta,
        Some(&packed as &dyn BlobSource),
        tilo_app_package::tapp::WriteOptions::create_new(),
    )
    .with_context(|| format!("creating {}", output.display()))?;
    Ok(Packed {
        id: written.id.to_string(),
        bytes: written.bytes,
    })
}
