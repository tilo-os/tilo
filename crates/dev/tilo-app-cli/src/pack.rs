//! Signing and verifying a `.tapp` in process, so `tilo-app package` needs no
//! separate `tilo-app-package` binary.

use std::io::Write as _;
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use tilo_app_package::{
    BlobSource, DiscardBlobs, Manifest, SigningIdentity, Verification, pack, verify,
};

/// Reads a signing identity file, `{"secret_key_hex": "<32-byte seed>"}`: the
/// format `tilo-app-package keygen` writes.
///
/// # Errors
///
/// Returns an error when the file cannot be read or is not an identity.
pub fn load_identity(path: &Path) -> Result<SigningIdentity> {
    let bytes =
        std::fs::read(path).with_context(|| format!("reading identity {}", path.display()))?;
    let file: serde_json::Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("parsing identity {}", path.display()))?;
    let seed_hex = file
        .get("secret_key_hex")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow!("{} has no secret_key_hex", path.display()))?;
    let seed: [u8; 32] = hex::decode(seed_hex.trim())
        .context("identity secret key is not hex")?
        .try_into()
        .map_err(|_| anyhow!("identity secret key must be 32 bytes"))?;
    Ok(SigningIdentity::from_seed(&seed))
}

/// What [`pack_manifest`] produced.
pub struct Packed {
    pub id: String,
    pub bytes: u64,
}

/// Signs `payload` with `manifest` as a full `.tapp` at `output`, declaring
/// `resource_kinds` as the kinds the app's records hold, then reads it back
/// and verifies it against its own publisher key.
///
/// # Errors
///
/// Returns an error when the manifest is invalid, packing or writing fails,
/// the archive exceeds the application size ceiling, or it does not verify.
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

    let file = std::fs::File::create_new(output)
        .with_context(|| format!("creating {}", output.display()))?;
    let mut writer = tilo_app_package::tapp::write(
        std::io::BufWriter::new(file),
        &packed.meta,
        Some(&packed as &dyn BlobSource),
    )
    .context("writing package archive")?;
    writer.flush().context("flushing package archive")?;
    drop(writer);
    let bytes = std::fs::metadata(output)?.len();
    if let Some(limit) = tilo_app_package::app_size_limit(&packed.meta.manifest)
        && bytes > limit
    {
        bail!("the package is {bytes} bytes; the limit for this application is {limit} bytes");
    }

    let reader = std::io::BufReader::new(std::fs::File::open(output)?);
    let tapp = tilo_app_package::tapp::read(reader, &mut DiscardBlobs)
        .with_context(|| format!("decoding {}", output.display()))?;
    if !tapp.missing().is_empty() {
        bail!("{} lacks payload blobs", output.display());
    }
    match verify(&tapp.meta, &[identity.public_key_bytes()]) {
        Verification::Trusted { .. } => Ok(Packed {
            id: tapp.meta.id.to_string(),
            bytes,
        }),
        other => bail!("{} does not verify: {other:?}", output.display()),
    }
}
