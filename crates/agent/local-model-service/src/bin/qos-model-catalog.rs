//! Compile and sign the local-model release catalog consumed by
//! `qos-model-manager`.
//!
//! The source document names build artifacts on disk. This tool hashes those
//! bytes, compares any declared receipt, rejects floating model revisions and
//! mutable download URLs, signs runtime extension manifests, then signs the
//! complete catalog. Only the derived Ed25519 public key is written to trust
//! output; the supplied release key is read once and never copied.

#![allow(
    clippy::missing_errors_doc,
    reason = "the release tool reports one contextual string error contract"
)]
#![cfg_attr(
    test,
    allow(
        clippy::indexing_slicing,
        clippy::panic,
        clippy::unwrap_used,
        reason = "tests use direct access and unwraps as assertions"
    )
)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::Read as _;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use agent_provider_config::local::{
    LocalModelRelease, ModelArtifactFormat, PinnedArtifact as ModelArtifact, RuntimeKind,
    RuntimeRelease,
};
use clap::Parser;
use ed25519_dalek::{Signer as _, SigningKey};
use local_model_service::SignedRuntime;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use system_extension::{
    AcceptedExtension, ArtifactReceipt, ExtensionKind, ExtensionManifest, ImageConstraint,
    PinnedArtifact as ExtensionArtifact, SignedExtensionManifest,
};

const CATALOG_SCHEMA: u16 = 1;
const CATALOG_SIGNATURE_DOMAIN: &str = "qos.local-model.catalog.v1";

#[derive(Parser)]
#[command(
    name = "qos-model-catalog",
    about = "Compile a pinned, signed qOS local-model catalog"
)]
struct Cli {
    /// Declarative JSON release source.
    #[arg(long)]
    source: PathBuf,
    /// File containing exactly one 32-byte Ed25519 seed as 64 hex digits.
    #[arg(long)]
    signing_key: PathBuf,
    /// Signed catalog output consumed by qos-model-manager.
    #[arg(long)]
    catalog_out: PathBuf,
    /// Public-only image trust output consumed as keys.json.
    #[arg(long)]
    trust_out: PathBuf,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogSource {
    schema: u16,
    /// Explicitly false for checked-in examples whose `.invalid` URLs must
    /// never be promoted into a release image.
    production: bool,
    key_id: String,
    recommendation: String,
    runtimes: Vec<RuntimeSource>,
    models: Vec<ModelSource>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeSource {
    id: String,
    kind: RuntimeKind,
    version: String,
    /// Digest-pinned OCI identity used by a container-aware supervisor.
    oci_image: String,
    artifact: ArtifactSource,
    base_image_digest: String,
    architecture: Option<String>,
    kernel_abi: Option<String>,
    requires_reboot: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelSource {
    id: String,
    label: String,
    description: String,
    /// Immutable upstream commit/revision, never `main`, `latest`, or a tag.
    revision: String,
    artifact: ArtifactSource,
    artifact_format: ModelArtifactFormat,
    license_id: String,
    license_url: String,
    requires_acceptance: bool,
    installed_size_bytes: u64,
    allowed_runtimes: BTreeSet<RuntimeKind>,
    required_memory_mib: u64,
    required_accelerator_mib: u64,
    context_window: u32,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactSource {
    /// Build-worker-local file whose bytes become the signed receipt.
    file: PathBuf,
    /// Immutable HTTPS retrieval location placed in the catalog.
    url: String,
    /// Optional expected receipt; when present it must match the file.
    sha256: Option<String>,
    /// Optional expected size; when present it must match the file.
    size_bytes: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct CatalogPayload {
    schema: u16,
    models: Vec<LocalModelRelease>,
    runtimes: Vec<SignedRuntime>,
    recommendation: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct SignedCatalog {
    schema: u16,
    signature_domain: String,
    key_id: String,
    signature: Vec<u8>,
    payload: CatalogPayload,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
struct PublicTrust {
    keys: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Receipt {
    sha256: String,
    size_bytes: u64,
}

fn main() -> std::process::ExitCode {
    if let Err(error) = run(Cli::parse()) {
        eprintln!("qos-model-catalog: {error}");
        return std::process::ExitCode::FAILURE;
    }
    std::process::ExitCode::SUCCESS
}

fn run(cli: Cli) -> Result<(), String> {
    let source_bytes = fs::read(&cli.source)
        .map_err(|error| format!("could not read {}: {error}", cli.source.display()))?;
    let source: CatalogSource = serde_json::from_slice(&source_bytes)
        .map_err(|error| format!("invalid catalog source: {error}"))?;
    let signing_key = read_signing_key(&cli.signing_key)?;
    let source_dir = cli.source.parent().unwrap_or_else(|| Path::new("."));
    let (catalog, trust) = compile(source, source_dir, &signing_key)?;
    let mut catalog_bytes = serde_json::to_vec_pretty(&catalog)
        .map_err(|error| format!("could not encode catalog: {error}"))?;
    catalog_bytes.push(b'\n');
    let mut trust_bytes = serde_json::to_vec_pretty(&trust)
        .map_err(|error| format!("could not encode trust: {error}"))?;
    trust_bytes.push(b'\n');
    atomic_file::write_creating_parent_with_mode(&cli.catalog_out, &catalog_bytes, 0o644)
        .map_err(|error| format!("could not write {}: {error}", cli.catalog_out.display()))?;
    atomic_file::write_creating_parent_with_mode(&cli.trust_out, &trust_bytes, 0o644)
        .map_err(|error| format!("could not write {}: {error}", cli.trust_out.display()))?;
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "one linear release transaction validates, resolves, signs, and assembles two output documents"
)]
fn compile(
    mut source: CatalogSource,
    source_dir: &Path,
    signing_key: &SigningKey,
) -> Result<(SignedCatalog, PublicTrust), String> {
    if source.schema != CATALOG_SCHEMA {
        return Err(format!("unsupported source schema {}", source.schema));
    }
    validate_identifier("key id", &source.key_id)?;
    if source.runtimes.is_empty() || source.models.is_empty() {
        return Err("catalog needs at least one runtime and model".into());
    }
    source
        .runtimes
        .sort_by(|left, right| left.id.cmp(&right.id));
    source.models.sort_by(|left, right| left.id.cmp(&right.id));

    let mut runtime_ids = BTreeSet::new();
    let mut runtime_kinds = BTreeSet::new();
    let mut runtimes = Vec::with_capacity(source.runtimes.len());
    for runtime in source.runtimes {
        validate_identifier("runtime id", &runtime.id)?;
        if !runtime_ids.insert(runtime.id.clone()) {
            return Err(format!("duplicate runtime id {}", runtime.id));
        }
        runtime_kinds.insert(runtime.kind);
        validate_oci_digest(&runtime.oci_image)?;
        let receipt = resolve_artifact(&runtime.artifact, source_dir, source.production, None)?;
        let manifest = ExtensionManifest {
            schema_version: system_extension::SCHEMA_VERSION,
            id: runtime.id.clone(),
            kind: ExtensionKind::LocalModelRuntime,
            version: runtime.version.clone(),
            artifact: ExtensionArtifact {
                source: runtime.artifact.url.clone(),
                sha256: receipt.sha256.clone(),
                size_bytes: receipt.size_bytes,
            },
            target: ImageConstraint {
                base_image_digest: runtime.base_image_digest,
                architecture: runtime.architecture,
                kernel_abi: runtime.kernel_abi,
            },
            requires_reboot: runtime.requires_reboot,
        };
        let signed = SignedExtensionManifest::sign(manifest, source.key_id.clone(), signing_key)
            .map_err(|error| format!("invalid runtime {}: {error}", runtime.id))?;
        runtimes.push(SignedRuntime {
            accepted: AcceptedExtension {
                signed,
                artifact: ArtifactReceipt {
                    sha256: receipt.sha256.clone(),
                    size_bytes: receipt.size_bytes,
                },
                committed_revision: 1,
            },
            release: RuntimeRelease {
                id: runtime.id,
                kind: runtime.kind,
                version: runtime.version,
                artifact: ModelArtifact {
                    url: runtime.artifact.url,
                    sha256: receipt.sha256,
                    size_bytes: receipt.size_bytes,
                },
                oci_image: runtime.oci_image,
            },
        });
    }

    let mut model_ids = BTreeSet::new();
    let mut models = Vec::with_capacity(source.models.len());
    for model in source.models {
        validate_identifier("model id", &model.id)?;
        if !model_ids.insert(model.id.clone()) {
            return Err(format!("duplicate model id {}", model.id));
        }
        validate_revision(&model.revision)?;
        if model.label.trim().is_empty()
            || model.description.trim().is_empty()
            || model.license_id.trim().is_empty()
            || model.context_window == 0
        {
            return Err(format!(
                "model {} has incomplete release metadata",
                model.id
            ));
        }
        validate_immutable_url(&model.license_url, source.production, Some(&model.revision))?;
        if model.allowed_runtimes.is_empty()
            || !model
                .allowed_runtimes
                .iter()
                .all(|kind| runtime_kinds.contains(kind))
        {
            return Err(format!(
                "model {} names a runtime kind absent from this catalog",
                model.id
            ));
        }
        let receipt = resolve_artifact(
            &model.artifact,
            source_dir,
            source.production,
            Some(&model.revision),
        )?;
        if model.installed_size_bytes < receipt.size_bytes {
            return Err(format!(
                "model {} installed size is smaller than its artifact",
                model.id
            ));
        }
        let release = LocalModelRelease {
            id: model.id,
            label: model.label,
            description: model.description,
            version: model.revision,
            artifact: ModelArtifact {
                url: model.artifact.url,
                sha256: receipt.sha256,
                size_bytes: receipt.size_bytes,
            },
            artifact_format: model.artifact_format,
            license_id: model.license_id,
            license_url: model.license_url,
            requires_acceptance: model.requires_acceptance,
            installed_size_bytes: model.installed_size_bytes,
            allowed_runtimes: model.allowed_runtimes,
            required_memory_mib: model.required_memory_mib,
            required_accelerator_mib: model.required_accelerator_mib,
            context_window: model.context_window,
        };
        release
            .artifact
            .validate()
            .map_err(|error| format!("invalid model {}: {error}", release.id))?;
        models.push(release);
    }
    if !model_ids.contains(&source.recommendation) {
        return Err("recommended model is not present in catalog".into());
    }
    let payload = CatalogPayload {
        schema: CATALOG_SCHEMA,
        models,
        runtimes,
        recommendation: source.recommendation,
    };
    let payload_bytes = catalog_signing_bytes(&payload)?;
    let signature = signing_key.sign(&payload_bytes).to_bytes().to_vec();
    let public_key = hex::encode(signing_key.verifying_key().to_bytes());
    let trust = PublicTrust {
        keys: BTreeMap::from([(source.key_id.clone(), public_key)]),
    };
    Ok((
        SignedCatalog {
            schema: CATALOG_SCHEMA,
            signature_domain: CATALOG_SIGNATURE_DOMAIN.into(),
            key_id: source.key_id,
            signature,
            payload,
        },
        trust,
    ))
}

fn catalog_signing_bytes(payload: &CatalogPayload) -> Result<Vec<u8>, String> {
    let encoded = serde_json::to_vec(payload)
        .map_err(|error| format!("could not encode signing payload: {error}"))?;
    let mut bytes = Vec::with_capacity(CATALOG_SIGNATURE_DOMAIN.len() + 1 + encoded.len());
    bytes.extend_from_slice(CATALOG_SIGNATURE_DOMAIN.as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(&encoded);
    Ok(bytes)
}

fn resolve_artifact(
    source: &ArtifactSource,
    source_dir: &Path,
    production: bool,
    revision: Option<&str>,
) -> Result<Receipt, String> {
    validate_immutable_url(&source.url, production, revision)?;
    let path = if source.file.is_absolute() {
        source.file.clone()
    } else {
        source_dir.join(&source.file)
    };
    let metadata = fs::symlink_metadata(&path)
        .map_err(|error| format!("could not inspect {}: {error}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(format!(
            "artifact {} must be a regular file",
            path.display()
        ));
    }
    let mut file =
        File::open(&path).map_err(|error| format!("could not open {}: {error}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    let mut size_bytes = 0_u64;
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("could not hash {}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(
            buffer
                .get(..read)
                .ok_or_else(|| "internal hash buffer boundary error".to_string())?,
        );
        size_bytes = size_bytes
            .checked_add(u64::try_from(read).map_err(|_| "artifact is too large")?)
            .ok_or_else(|| "artifact size overflow".to_string())?;
    }
    if size_bytes == 0 {
        return Err(format!("artifact {} is empty", path.display()));
    }
    let sha256 = hex::encode(hasher.finalize());
    if source
        .sha256
        .as_deref()
        .is_some_and(|expected| expected != sha256)
    {
        return Err(format!(
            "artifact {} SHA-256 does not match source",
            path.display()
        ));
    }
    if source
        .size_bytes
        .is_some_and(|expected| expected != size_bytes)
    {
        return Err(format!(
            "artifact {} size does not match source",
            path.display()
        ));
    }
    Ok(Receipt { sha256, size_bytes })
}

fn validate_immutable_url(
    url: &str,
    production: bool,
    revision: Option<&str>,
) -> Result<(), String> {
    if !url.starts_with("https://") || url.contains('?') || url.contains('#') {
        return Err("artifact URL must be an HTTPS URL without query or fragment".into());
    }
    let lowercase = url.to_ascii_lowercase();
    if ["/latest/", "/main/", "/master/", ":latest", "/tags/"]
        .iter()
        .any(|token| lowercase.contains(token))
    {
        return Err(format!("artifact URL is mutable: {url}"));
    }
    if production && lowercase.contains(".invalid/") {
        return Err("production catalogs cannot use reserved .invalid URLs".into());
    }
    if let Some(revision) = revision
        && !url.contains(revision)
    {
        return Err("model artifact URL must contain its immutable revision".into());
    }
    Ok(())
}

fn validate_revision(revision: &str) -> Result<(), String> {
    if !matches!(revision.len(), 40 | 64)
        || !revision
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err("model revision must be a 40- or 64-character lowercase hex commit".into());
    }
    Ok(())
}

fn validate_oci_digest(image: &str) -> Result<(), String> {
    let Some((repository, digest)) = image.rsplit_once("@sha256:") else {
        return Err("runtime OCI image must use a sha256 digest, not a tag".into());
    };
    if repository.is_empty()
        || digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err("runtime OCI image has an invalid sha256 digest".into());
    }
    Ok(())
}

fn validate_identifier(name: &str, value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.len() > 128
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'-' | b'_' | b'.' | b'/')
        })
    {
        return Err(format!("{name} is not a stable lowercase identifier"));
    }
    Ok(())
}

fn read_signing_key(path: &Path) -> Result<SigningKey, String> {
    let metadata = fs::metadata(path)
        .map_err(|error| format!("could not inspect signing key {}: {error}", path.display()))?;
    #[cfg(unix)]
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err("signing key must not be readable or writable by group/others".into());
    }
    let text = fs::read_to_string(path)
        .map_err(|error| format!("could not read signing key {}: {error}", path.display()))?;
    let encoded = text.trim();
    if encoded.bytes().any(|byte| byte.is_ascii_uppercase()) {
        return Err("signing key must use canonical lowercase hexadecimal".into());
    }
    let decoded = hex::decode(encoded).map_err(|_| "signing key is not hexadecimal")?;
    let bytes: [u8; 32] = decoded
        .try_into()
        .map_err(|_| "signing key must contain exactly 32 bytes")?;
    Ok(SigningKey::from_bytes(&bytes))
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use ed25519_dalek::{Signature, Verifier as _};

    use super::*;

    fn artifact(dir: &Path, name: &str, bytes: &[u8]) -> ArtifactSource {
        let path = dir.join(name);
        let mut file = File::create(&path).unwrap();
        file.write_all(bytes).unwrap();
        ArtifactSource {
            file: PathBuf::from(name),
            url: format!("https://release.example.invalid/objects/{name}"),
            sha256: None,
            size_bytes: None,
        }
    }

    fn source(dir: &Path) -> CatalogSource {
        let revision = "0123456789abcdef0123456789abcdef01234567";
        let mut model_artifact = artifact(dir, "model.bin", b"deterministic model fixture\n");
        model_artifact.url =
            format!("https://huggingface.example.invalid/acme/model/resolve/{revision}/model.bin");
        CatalogSource {
            schema: 1,
            production: false,
            key_id: "test-release-1".into(),
            recommendation: "example/model".into(),
            runtimes: vec![RuntimeSource {
                id: "vllm-cpu".into(),
                kind: RuntimeKind::Cpu,
                version: "0.1.0-example".into(),
                oci_image: format!(
                    "registry.example.invalid/qos/vllm@sha256:{}",
                    "b".repeat(64)
                ),
                artifact: artifact(dir, "runtime.bin", b"deterministic runtime fixture\n"),
                base_image_digest: format!("sha256:{}", "a".repeat(64)),
                architecture: Some("x86_64".into()),
                kernel_abi: None,
                requires_reboot: false,
            }],
            models: vec![ModelSource {
                id: "example/model".into(),
                label: "Example model — not for production".into(),
                description: "Deterministic test fixture".into(),
                revision: revision.into(),
                artifact: model_artifact,
                artifact_format: ModelArtifactFormat::VllmGguf,
                license_id: "Apache-2.0".into(),
                license_url: format!(
                    "https://huggingface.example.invalid/acme/model/blob/{revision}/LICENSE"
                ),
                requires_acceptance: true,
                installed_size_bytes: 64,
                allowed_runtimes: BTreeSet::from([RuntimeKind::Cpu]),
                required_memory_mib: 1024,
                required_accelerator_mib: 0,
                context_window: 4096,
            }],
        }
    }

    #[test]
    fn checked_in_nonproduction_source_compiles_without_network_resolution() {
        let source: CatalogSource = serde_json::from_str(include_str!(
            "../../tests/fixtures/catalog-source.example.json"
        ))
        .unwrap();
        let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let (catalog, trust) =
            compile(source, &fixtures, &SigningKey::from_bytes(&[11; 32])).unwrap();
        assert_eq!(catalog.payload.recommendation, "example/tiny-model");
        assert_eq!(catalog.payload.models[0].artifact.size_bytes, 41);
        assert_eq!(catalog.payload.runtimes[0].release.artifact.size_bytes, 43);
        assert_eq!(trust.keys.len(), 1);
    }

    #[test]
    fn output_is_deterministic_and_verifies_with_public_trust_only() {
        let temp = tempfile::tempdir().unwrap();
        let key = SigningKey::from_bytes(&[7; 32]);
        let first = compile(source(temp.path()), temp.path(), &key).unwrap();
        let second = compile(source(temp.path()), temp.path(), &key).unwrap();
        assert_eq!(first, second);
        let payload = catalog_signing_bytes(&first.0.payload).unwrap();
        let signature = Signature::from_slice(&first.0.signature).unwrap();
        key.verifying_key().verify(&payload, &signature).unwrap();
        let trust_json = serde_json::to_string(&first.1).unwrap();
        assert!(trust_json.contains(&hex::encode(key.verifying_key().to_bytes())));
        assert!(!trust_json.contains(&hex::encode(key.to_bytes())));
    }

    #[test]
    fn artifact_receipt_is_resolved_and_declared_mismatch_fails() {
        let temp = tempfile::tempdir().unwrap();
        let mut source = source(temp.path());
        source.models[0].artifact.sha256 = Some("f".repeat(64));
        let error = compile(source, temp.path(), &SigningKey::from_bytes(&[3; 32])).unwrap_err();
        assert!(error.contains("SHA-256 does not match"));
    }

    #[test]
    fn mutable_revision_and_url_are_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let mut floating = source(temp.path());
        floating.models[0].revision = "main".into();
        assert!(
            compile(floating, temp.path(), &SigningKey::from_bytes(&[3; 32]))
                .unwrap_err()
                .contains("lowercase hex commit")
        );

        let mut mutable_url = source(temp.path());
        mutable_url.runtimes[0].artifact.url =
            "https://release.example.invalid/runtime/latest/runtime.bin".into();
        assert!(
            compile(mutable_url, temp.path(), &SigningKey::from_bytes(&[3; 32]))
                .unwrap_err()
                .contains("mutable")
        );
    }

    #[test]
    fn production_source_rejects_nonproduction_hosts() {
        let temp = tempfile::tempdir().unwrap();
        let mut source = source(temp.path());
        source.production = true;
        assert!(
            compile(source, temp.path(), &SigningKey::from_bytes(&[3; 32]))
                .unwrap_err()
                .contains("reserved .invalid")
        );
    }

    #[cfg(unix)]
    #[test]
    fn signing_key_must_be_private_on_disk() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("key");
        fs::write(&path, hex::encode([9_u8; 32])).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            read_signing_key(&path)
                .unwrap_err()
                .contains("group/others")
        );
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(read_signing_key(&path).unwrap().to_bytes(), [9; 32]);
    }
}
