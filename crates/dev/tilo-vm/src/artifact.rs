//! Downloaded artifacts are inputs, never authority. The caller pins the receipt
//! from the authenticated same-run producer; all bytes are checked and retained
//! in private files before the daemon sees an upload. No Cargo or builder path.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{BufReader, Read, Seek, Write};
use std::path::{Component, Path};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tilo_vm_protocol::Provenance;

use crate::builderlayer::{Packed, Stream};
use crate::cli::ArtifactSwap;
use crate::error::CliError;

const COHORT: [&str; 4] = ["launcher", "install", "shell", "apps+runtime"];
const MAX_LAYER: u64 = 8 << 30;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FileReceipt {
    pub path: String,
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LayerReceipt {
    pub component: String,
    pub profile: String,
    pub features: Vec<String>,
    pub target: String,
    pub toolchain: String,
    pub source_digest: String,
    pub layer_hash: String,
    pub stream: FileReceipt,
    pub inventory: FileReceipt,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    schema: String,
    repository: String,
    source_commit: String,
    builder_image: String,
    run_id: u64,
    run_attempt: u32,
    architecture: String,
    layers: Vec<LayerReceipt>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Inventory {
    provenance: Provenance,
    files: BTreeMap<String, String>,
}

pub(crate) struct PreparedLayer {
    pub receipt: LayerReceipt,
    pub packed: Packed,
    pub provenance: Provenance,
    pub archive: tempfile::NamedTempFile,
}

pub(crate) struct Prepared {
    pub architecture: String,
    pub layers: Vec<PreparedLayer>,
}

fn invalid(message: impl Into<String>) -> CliError {
    CliError::InvalidInput(format!("layer artifact: {}", message.into()))
}

fn digest(value: &str, size: usize) -> bool {
    value.len() == size
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn relative(path: &str) -> bool {
    !path.is_empty()
        && Path::new(path)
            .components()
            .all(|p| matches!(p, Component::Normal(_)))
}

fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>, CliError> {
    let meta = std::fs::symlink_metadata(path).map_err(CliError::io("stat artifact receipt"))?;
    if !meta.is_file() || meta.len() > limit {
        return Err(invalid("receipt is not a bounded regular file"));
    }
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(CliError::io("open artifact receipt"))?
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(CliError::io("read artifact receipt"))?;
    if bytes.len() as u64 > limit {
        return Err(invalid("receipt changed beyond its bound"));
    }
    Ok(bytes)
}

fn copy_verified(
    root: &Path,
    expected: &FileReceipt,
    limit: u64,
) -> Result<tempfile::NamedTempFile, CliError> {
    if !relative(&expected.path)
        || !digest(&expected.sha256, 64)
        || expected.bytes == 0
        || expected.bytes > limit
    {
        return Err(invalid("invalid artifact path, digest or byte count"));
    }
    let path = root.join(&expected.path);
    let meta = std::fs::symlink_metadata(&path).map_err(CliError::io("stat artifact"))?;
    let actual = path
        .canonicalize()
        .map_err(CliError::io("resolve artifact"))?;
    if !meta.is_file() || meta.len() != expected.bytes || !actual.starts_with(root) {
        return Err(invalid(
            "artifact is missing, substituted or escapes its directory",
        ));
    }
    let mut input = File::open(&actual)
        .map_err(CliError::io("open artifact"))?
        .take(expected.bytes.saturating_add(1));
    let mut output =
        tempfile::NamedTempFile::new().map_err(CliError::io("stage verified artifact"))?;
    let mut hash = Sha256::new();
    let mut bytes = 0u64;
    let mut buffer = vec![0u8; 64 << 10];
    loop {
        let count = input
            .read(&mut buffer)
            .map_err(CliError::io("read artifact"))?;
        if count == 0 {
            break;
        }
        let chunk = buffer
            .get(..count)
            .ok_or_else(|| invalid("invalid artifact read count"))?;
        hash.update(chunk);
        output
            .write_all(chunk)
            .map_err(CliError::io("retain artifact"))?;
        bytes = bytes.saturating_add(count as u64);
    }
    if bytes != expected.bytes || hex::encode(hash.finalize()) != expected.sha256 {
        return Err(invalid(
            "artifact digest or byte count differs from pinned receipt",
        ));
    }
    output.rewind().map_err(CliError::io("rewind artifact"))?;
    Ok(output)
}

fn inventory(root: &Path, layer: &LayerReceipt, source: &str) -> Result<Provenance, CliError> {
    let input = copy_verified(root, &layer.inventory, 32 << 20)?;
    let parsed: Inventory = serde_json::from_reader(input.as_file())
        .map_err(|_| invalid("invalid source inventory"))?;
    if layer.source_digest != layer.inventory.sha256
        || parsed.provenance.component != layer.component
        || parsed.provenance.source_commit.as_deref() != Some(source)
        || parsed.provenance.dirty_tree_hash.is_some()
        || parsed.files.is_empty()
        || parsed.files.len() > 65_536
        || parsed
            .files
            .iter()
            .any(|(path, hash)| path.len() > 4096 || !relative(path) || !digest(hash, 64))
    {
        return Err(invalid(
            "source inventory differs from exact clean component source",
        ));
    }
    Ok(parsed.provenance)
}

fn prepare_layer(
    root: &Path,
    layer: LayerReceipt,
    receipt: &Receipt,
) -> Result<PreparedLayer, CliError> {
    let mut provenance = inventory(root, &layer, &receipt.source_commit)?;
    if layer.target != format!("{}-unknown-linux-gnu", receipt.architecture)
        || (layer.component == "apps+runtime" && layer.profile != "thin-app")
        || !digest(&layer.layer_hash, 64)
        || !digest(&layer.source_digest, 64)
        || layer.toolchain.is_empty()
        || layer.toolchain.len() > 256
        || layer.profile.is_empty()
        || layer.profile.len() > 64
        || layer.features.len() > 128
        || layer.features.iter().any(|f| f.is_empty() || f.len() > 256)
    {
        return Err(invalid("invalid layer build identity"));
    }
    let mut encoded = copy_verified(root, &layer.stream, MAX_LAYER)?;
    let mut prefix = Vec::new();
    encoded
        .as_file_mut()
        .take(16 << 10)
        .read_to_end(&mut prefix)
        .map_err(CliError::io("read bounded artifact header"))?;
    if !prefix.starts_with(crate::builderlayer::BEGIN.as_bytes()) || !prefix.contains(&b'\n') {
        return Err(invalid("artifact has no bounded initial layer header"));
    }
    encoded
        .rewind()
        .map_err(CliError::io("rewind framed artifact"))?;
    let mut stream = Stream::new(BufReader::new(encoded.as_file_mut()));
    let mut preamble = Vec::new();
    let packed = stream
        .header(&mut preamble)
        .map_err(CliError::io("read artifact layer header"))?;
    if !preamble.is_empty()
        || packed.reused
        || packed.component != layer.component
        || packed.target.as_deref() != Some(layer.target.as_str())
        || packed.toolchain.as_deref() != Some(layer.toolchain.as_str())
        || packed.source_digest.as_deref() != Some(layer.source_digest.as_str())
        || packed.layer_hash.as_deref() != Some(layer.layer_hash.as_str())
    {
        return Err(invalid("full layer header differs from pinned receipt"));
    }
    let mut archive =
        tempfile::NamedTempFile::new().map_err(CliError::io("stage layer archive"))?;
    let bytes = std::io::copy(&mut stream.by_ref().take(MAX_LAYER + 1), &mut archive)
        .map_err(CliError::io("verify artifact stream footer"))?;
    if bytes == 0 || bytes > MAX_LAYER {
        return Err(invalid("empty or oversized layer archive"));
    }
    stream
        .finish()
        .map_err(CliError::io("verify artifact stream end"))?;
    archive
        .rewind()
        .map_err(CliError::io("rewind layer archive"))?;
    // Existing development layers remain local and non-promotable. A receipt
    // does not grant a new attestor or a registry publication authority.
    provenance.authority = "local".into();
    provenance.builder = Some("ci".into());
    provenance.builder_image_digest = receipt
        .builder_image
        .split_once('@')
        .map(|(_, digest)| digest.into());
    provenance.target = Some(layer.target.clone());
    provenance.toolchain = Some(layer.toolchain.clone());
    provenance.cargo_profile = Some(layer.profile.clone());
    provenance.features.clone_from(&layer.features);
    Ok(PreparedLayer {
        receipt: layer,
        packed,
        provenance,
        archive,
    })
}

pub(crate) fn prepare(request: &ArtifactSwap) -> Result<Prepared, CliError> {
    let image_digest = request
        .builder_image
        .strip_prefix("registry.tilo.so/tilo/tilo-builder@sha256:");
    if !digest(&request.source, 40)
        || !digest(&request.receipt_sha256, 64)
        || !image_digest.is_some_and(|v| digest(v, 64))
        || request.run_id == 0
        || request.run_attempt == 0
        || !COHORT.contains(&request.component.as_str())
    {
        return Err(invalid(
            "exact source, builder, run, receipt and supported component are required",
        ));
    }
    let bytes = read_bounded(&request.receipt, 1 << 20)?;
    if crate::build::sha256_hex(&bytes) != request.receipt_sha256 {
        return Err(invalid("receipt digest mismatch"));
    }
    let receipt: Receipt =
        serde_json::from_slice(&bytes).map_err(|_| invalid("invalid receipt schema"))?;
    if receipt.schema != "tilo.vm.layer-artifacts/v1"
        || receipt.repository != "tilo/tilo"
        || receipt.source_commit != request.source
        || receipt.builder_image != request.builder_image
        || receipt.run_id != request.run_id
        || receipt.run_attempt != request.run_attempt
        || !matches!(receipt.architecture.as_str(), "x86_64" | "aarch64")
        || receipt.layers.is_empty()
        || receipt.layers.len() > COHORT.len()
    {
        return Err(invalid(
            "receipt source, builder, run or architecture mismatch",
        ));
    }
    let names: BTreeSet<_> = receipt
        .layers
        .iter()
        .map(|l| l.component.as_str())
        .collect();
    if names.len() != receipt.layers.len()
        || names.iter().any(|n| !COHORT.contains(n))
        || !names.contains(request.component.as_str())
        || (request.component == "apps+runtime" && COHORT.iter().any(|n| !names.contains(n)))
    {
        return Err(invalid(
            "missing, duplicate or incomplete cohort components",
        ));
    }
    let root = request
        .receipt
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .canonicalize()
        .map_err(CliError::io("resolve artifact directory"))?;
    let mut layers = Vec::new();
    for layer in &receipt.layers {
        layers.push(prepare_layer(&root, layer.clone(), &receipt)?);
    }
    Ok(Prepared {
        architecture: receipt.architecture,
        layers,
    })
}

#[cfg(test)]
pub(crate) mod tests;
