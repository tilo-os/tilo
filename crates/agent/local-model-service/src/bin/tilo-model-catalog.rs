//! Compile and sign the local-model release catalog consumed by
//! `tilo-model-manager`.
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

use std::collections::{BTreeMap, BTreeSet};
#[cfg(target_os = "linux")]
use std::ffi::CString;
#[cfg(not(target_os = "linux"))]
use std::fs::OpenOptions;
use std::fs::{self, File};
use std::io::Read as _;
#[cfg(target_os = "linux")]
use std::os::fd::FromRawFd as _;
#[cfg(target_os = "linux")]
use std::os::unix::ffi::OsStrExt as _;
#[cfg(all(unix, not(target_os = "linux")))]
use std::os::unix::fs::OpenOptionsExt as _;
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use agent_provider_config::local::{
    LocalModelRelease, ModelArtifactFormat, PinnedArtifact as ModelArtifact, RuntimeExecution,
    RuntimeKind, RuntimeRelease,
};
use clap::Parser;
use ed25519_dalek::{Signer as _, SigningKey};
use local_model_service::SignedRuntime;
use local_model_service::catalog::{
    CATALOG_SIGNATURE_DOMAIN, CatalogPayload, SignedCatalog, catalog_signing_bytes,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use system_extension::{
    AcceptedExtension, ArtifactReceipt, ExtensionKind, ExtensionManifest, ImageConstraint,
    PinnedArtifact as ExtensionArtifact, SignedExtensionManifest,
};
use zeroize::Zeroizing;

const CATALOG_SCHEMA: u16 = 1;

#[cfg(target_os = "linux")]
fn open_readonly_without_symlinks(path: &Path) -> std::io::Result<File> {
    const RESOLVE_NO_MAGICLINKS: u64 = 0x02;
    const RESOLVE_NO_SYMLINKS: u64 = 0x04;

    #[repr(C)]
    struct OpenHow {
        flags: u64,
        mode: u64,
        resolve: u64,
    }

    let path = CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "path contains an embedded NUL byte",
        )
    })?;
    let flags = u64::try_from(libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .map_err(|_| std::io::Error::other("open flags are outside the Linux u64 ABI"))?;
    let how = OpenHow {
        flags,
        mode: 0,
        resolve: RESOLVE_NO_MAGICLINKS | RESOLVE_NO_SYMLINKS,
    };
    // SAFETY: path is a live NUL-terminated string and `how` is the exact
    // three-u64 ABI accepted by openat2. The returned descriptor is uniquely
    // transferred into File on success.
    let descriptor = unsafe {
        libc::syscall(
            libc::SYS_openat2,
            libc::AT_FDCWD,
            path.as_ptr(),
            &raw const how,
            std::mem::size_of::<OpenHow>(),
        )
    };
    if descriptor < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let descriptor = i32::try_from(descriptor)
        .map_err(|_| std::io::Error::other("openat2 returned an invalid descriptor"))?;
    // SAFETY: openat2 returned this owned descriptor and no other File owns it.
    Ok(unsafe { File::from_raw_fd(descriptor) })
}

#[cfg(not(target_os = "linux"))]
fn open_readonly_without_symlinks(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW | libc::O_NONBLOCK);
    options.open(path)
}

#[derive(Parser)]
#[command(
    name = "tilo-model-catalog",
    about = "Compile a pinned, signed Tilo local-model catalog"
)]
struct Cli {
    /// Declarative JSON release source.
    #[arg(long)]
    source: PathBuf,
    /// File containing exactly one 32-byte Ed25519 seed as 64 hex digits.
    #[arg(long)]
    signing_key: PathBuf,
    /// Signed catalog output consumed by tilo-model-manager.
    #[arg(long)]
    catalog_out: PathBuf,
    /// Public-only image trust output consumed as keys.json.
    #[arg(long)]
    trust_out: PathBuf,
    /// Separate offline developer channel; never produces an image release catalog.
    #[arg(long)]
    developer_offline: bool,
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
    #[serde(default)]
    execution: Option<RuntimeExecution>,
    artifact: ArtifactSource,
    base_image_digest: String,
    architecture: Option<String>,
    kernel_abi: Option<String>,
    requires_reboot: bool,
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is an independent signed declaration in the catalog wire format"
)]
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
    /// Signed vLLM runtimes that may serve this model. Empty, and only
    /// empty, for a model that runs in-process (`onnx_file`).
    #[serde(default)]
    allowed_runtimes: BTreeSet<RuntimeKind>,
    required_memory_mib: u64,
    required_accelerator_mib: u64,
    /// Zero, or omitted, for an in-process model: it is not a chat model.
    #[serde(default)]
    context_window: u32,
    /// Signed declaration: this exact release accepts bounded image input.
    #[serde(default)]
    vision: bool,
    /// This must stay false for a general visual-language model.  The Photos
    /// biometric route is reserved for a separately evaluated, signed face
    /// runtime; it is never inferred from `vision`.
    #[serde(default)]
    face_recognition: bool,
    /// Salient-object segmentation, for an in-process model only.
    #[serde(default)]
    segmentation: bool,
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
struct PublicTrust {
    keys: BTreeMap<String, String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Receipt {
    sha256: String,
    size_bytes: u64,
}

fn main() -> std::process::ExitCode {
    if let Err(error) = run(&Cli::parse()) {
        eprintln!("tilo-model-catalog: {error}");
        return std::process::ExitCode::FAILURE;
    }
    std::process::ExitCode::SUCCESS
}

fn run(cli: &Cli) -> Result<(), String> {
    let source_bytes = fs::read(&cli.source)
        .map_err(|error| format!("could not read {}: {error}", cli.source.display()))?;
    let source: CatalogSource = serde_json::from_slice(&source_bytes)
        .map_err(|error| format!("invalid catalog source: {error}"))?;
    let signing_key = read_signing_key(&cli.signing_key)?;
    let source_dir = cli.source.parent().unwrap_or_else(|| Path::new("."));
    let (catalog, trust) = compile_mode(source, source_dir, &signing_key, cli.developer_offline)?;
    let mut catalog_bytes = serde_json::to_vec_pretty(&catalog)
        .map_err(|error| format!("could not encode catalog: {error}"))?;
    catalog_bytes.push(b'\n');
    let mut trust_bytes = serde_json::to_vec_pretty(&trust)
        .map_err(|error| format!("could not encode trust: {error}"))?;
    trust_bytes.push(b'\n');
    tilo_files::native::atomic::write_creating_parent_with_mode(
        &cli.catalog_out,
        &catalog_bytes,
        0o644,
    )
    .map_err(|error| format!("could not write {}: {error}", cli.catalog_out.display()))?;
    tilo_files::native::atomic::write_creating_parent_with_mode(
        &cli.trust_out,
        &trust_bytes,
        0o644,
    )
    .map_err(|error| format!("could not write {}: {error}", cli.trust_out.display()))?;
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "one linear release transaction validates, resolves, signs, and assembles two output documents"
)]
fn compile_mode(
    mut source: CatalogSource,
    source_dir: &Path,
    signing_key: &SigningKey,
    developer_offline: bool,
) -> Result<(SignedCatalog, PublicTrust), String> {
    if developer_offline && source.production {
        return Err("offline developer catalogs must declare production:false".into());
    }
    if source.schema != CATALOG_SCHEMA {
        return Err(format!("unsupported source schema {}", source.schema));
    }
    validate_identifier("key id", &source.key_id)?;
    if source.models.is_empty() {
        return Err("catalog needs at least one model".into());
    }
    source
        .runtimes
        .sort_by(|left, right| left.id.cmp(&right.id));
    source.models.sort_by(|left, right| left.id.cmp(&right.id));

    let mut runtime_ids = BTreeSet::new();
    let mut runtimes = Vec::with_capacity(source.runtimes.len());
    for runtime in source.runtimes {
        validate_identifier("runtime id", &runtime.id)?;
        if !runtime_ids.insert(runtime.id.clone()) {
            return Err(format!("duplicate runtime id {}", runtime.id));
        }
        validate_oci_digest(&runtime.oci_image)?;
        let receipt = resolve_artifact_mode(
            &runtime.artifact,
            source_dir,
            source.production,
            None,
            developer_offline,
        )?;
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
                execution: runtime.execution,
            },
        });
    }

    for runtime in &runtimes {
        runtime
            .validate()
            .map_err(|error| format!("invalid runtime {}: {error}", runtime.release.id))?;
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
        {
            return Err(format!(
                "model {} has incomplete release metadata",
                model.id
            ));
        }
        validate_immutable_url(&model.license_url, source.production, Some(&model.revision))?;
        let receipt = resolve_artifact_mode(
            &model.artifact,
            source_dir,
            source.production,
            Some(&model.revision),
            developer_offline,
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
            vision: model.vision,
            face_recognition: model.face_recognition,
            segmentation: model.segmentation,
        };
        release
            .validate_shape()
            .map_err(|error| format!("invalid model {}: {error}", release.id))?;
        // An in-process model names no runtime at all; every runtime a served
        // model names must be one this catalog signs.
        if !release.allowed_runtimes.iter().all(|kind| {
            runtimes.iter().any(|runtime| {
                runtime.release.kind == *kind
                    && runtime.release.supports_format(release.artifact_format)
            })
        }) {
            return Err(format!(
                "model {} names a runtime kind absent from this catalog",
                release.id
            ));
        }
        validate_release_artifact(&release.artifact, developer_offline)
            .map_err(|error| format!("invalid model {}: {error}", release.id))?;
        models.push(release);
    }
    let has_served_models = models.iter().any(|model| !model.runs_in_process());
    if (has_served_models || !source.recommendation.is_empty())
        && !models
            .iter()
            .any(|model| model.id == source.recommendation && !model.runs_in_process())
    {
        return Err("recommended model is not a runtime-served model in this catalog".into());
    }
    let payload = CatalogPayload {
        schema: CATALOG_SCHEMA,
        models,
        runtimes,
        recommendation: source.recommendation,
    };
    let domain = if developer_offline {
        local_model_service::catalog::DEVELOPER_SIGNATURE_DOMAIN
    } else {
        CATALOG_SIGNATURE_DOMAIN
    };
    let payload_bytes = if developer_offline {
        local_model_service::catalog::developer_signing_bytes(&payload)?
    } else {
        catalog_signing_bytes(&payload)?
    };
    let signature = signing_key.sign(&payload_bytes).to_bytes().to_vec();
    let public_key = hex::encode(signing_key.verifying_key().to_bytes());
    let trust = PublicTrust {
        keys: BTreeMap::from([(source.key_id.clone(), public_key)]),
    };
    Ok((
        SignedCatalog {
            schema: CATALOG_SCHEMA,
            signature_domain: domain.into(),
            key_id: source.key_id,
            signature,
            payload,
        },
        trust,
    ))
}

fn resolve_artifact_mode(
    source: &ArtifactSource,
    source_dir: &Path,
    production: bool,
    revision: Option<&str>,
    developer_offline: bool,
) -> Result<Receipt, String> {
    let offline = developer_offline
        && source
            .url
            .starts_with(local_model_service::developer::OFFLINE_PREFIX);
    if !offline {
        validate_immutable_url(&source.url, production, revision)?;
    }
    let path = if source.file.is_absolute() {
        source.file.clone()
    } else {
        source_dir.join(&source.file)
    };
    let mut file = open_readonly_without_symlinks(&path)
        .map_err(|error| format!("could not securely open {}: {error}", path.display()))?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("could not inspect {}: {error}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!(
            "artifact {} must be a regular file",
            path.display()
        ));
    }
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
    if offline {
        local_model_service::developer::validate_offline_pin(&ModelArtifact {
            url: source.url.clone(),
            sha256: sha256.clone(),
            size_bytes,
        })
        .map_err(|error| error.to_string())?;
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
        || value.starts_with('-')
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
    #[cfg(target_os = "linux")]
    // A release-signing process should never make its key available through a
    // core dump, even if a later operation or dependency crashes.
    // SAFETY: prctl with PR_SET_DUMPABLE takes an integer flag and returns -1
    // with errno on failure.
    if unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0) } != 0 {
        return Err(format!(
            "could not disable signing-process dumps: {}",
            std::io::Error::last_os_error()
        ));
    }

    let file = open_readonly_without_symlinks(path).map_err(|error| {
        format!(
            "could not securely open signing key {}: {error}",
            path.display()
        )
    })?;
    let metadata = file
        .metadata()
        .map_err(|error| format!("could not inspect signing key {}: {error}", path.display()))?;
    if !metadata.is_file() {
        return Err("signing key must be a regular file".into());
    }
    #[cfg(unix)]
    {
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err("signing key must not be readable or writable by group/others".into());
        }
        // SAFETY: geteuid takes no arguments and has no failure mode.
        let reader = unsafe { libc::geteuid() };
        if metadata.uid() != reader {
            return Err(format!(
                "signing key is owned by uid {}; expected uid {reader}",
                metadata.uid()
            ));
        }
        if metadata.nlink() != 1 {
            return Err(format!(
                "signing key must have exactly one directory entry; found {}",
                metadata.nlink()
            ));
        }
    }
    let mut text = Zeroizing::new(String::new());
    file.take(130)
        .read_to_string(&mut text)
        .map_err(|error| format!("could not read signing key {}: {error}", path.display()))?;
    if text.len() > 129 {
        return Err("signing key file is too large".into());
    }
    let encoded = text.trim();
    if encoded.bytes().any(|byte| byte.is_ascii_uppercase()) {
        return Err("signing key must use canonical lowercase hexadecimal".into());
    }
    let decoded =
        Zeroizing::new(hex::decode(encoded).map_err(|_| "signing key is not hexadecimal")?);
    if decoded.len() != 32 {
        return Err("signing key must contain exactly 32 bytes".into());
    }
    let mut bytes = Zeroizing::new([0_u8; 32]);
    bytes.copy_from_slice(&decoded);
    Ok(SigningKey::from_bytes(&bytes))
}

fn validate_release_artifact(pin: &ModelArtifact, developer: bool) -> Result<(), String> {
    if developer && !pin.url.starts_with("https://") {
        local_model_service::developer::validate_offline_pin(pin).map_err(|error| error.to_string())
    } else {
        pin.validate().map_err(|error| error.to_string())
    }
}

#[cfg(test)]
fn compile(
    source: CatalogSource,
    source_dir: &Path,
    key: &SigningKey,
) -> Result<(SignedCatalog, PublicTrust), String> {
    compile_mode(source, source_dir, key, false)
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use ed25519_dalek::{Signature, Verifier as _};

    use super::*;

    #[test]
    fn offline_developer_catalog_has_a_separate_trust_domain() {
        let dir = tempfile::tempdir().unwrap();
        let mut input = source(dir.path());
        let digest = hex::encode(Sha256::digest(b"deterministic runtime fixture\n"));
        input.runtimes[0].artifact.url = format!("offline:sha256:{digest}");
        let key = SigningKey::from_bytes(&[11; 32]);
        assert!(compile(input.clone(), dir.path(), &key).is_err());
        let mut production = input.clone();
        production.production = true;
        assert!(compile_mode(production, dir.path(), &key, true).is_err());
        let (mut signed, trust) = compile_mode(input.clone(), dir.path(), &key, true).unwrap();
        let cat = dir.path().join("catalog.json");
        let keys = dir.path().join("keys.json");
        fs::write(&cat, serde_json::to_vec(&signed).unwrap()).unwrap();
        fs::write(&keys, serde_json::to_vec(&trust).unwrap()).unwrap();
        assert!(local_model_service::catalog::VerifiedCatalog::load(&cat, &keys).is_err());
        assert!(
            local_model_service::catalog::VerifiedCatalog::load_developer_offline(&cat, &keys)
                .is_ok()
        );
        signed.payload.runtimes[0].release.artifact.sha256 = "d".repeat(64);
        fs::write(&cat, serde_json::to_vec(&signed).unwrap()).unwrap();
        assert!(
            local_model_service::catalog::VerifiedCatalog::load_developer_offline(&cat, &keys)
                .is_err()
        );
        input.runtimes[0].artifact.url = format!("offline:sha256:{}", "d".repeat(64));
        assert!(compile_mode(input, dir.path(), &key, true).is_err());
    }

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
                execution: None,
                oci_image: format!(
                    "registry.example.invalid/tilo/vllm@sha256:{}",
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
                vision: true,
                face_recognition: false,
                segmentation: false,
            }],
        }
    }

    fn in_process_source(dir: &Path) -> ModelSource {
        let mut artifact = artifact(dir, "subject.onnx", b"deterministic onnx fixture\n");
        let revision = hex::encode(Sha256::digest(b"deterministic onnx fixture\n"));
        artifact.url = format!("https://release.example.invalid/subject/{revision}/subject.onnx");
        ModelSource {
            id: "example-subject".into(),
            label: "Example subject selection".into(),
            description: "Deterministic in-process test fixture".into(),
            license_url: format!("https://release.example.invalid/subject/{revision}/LICENSE"),
            revision,
            artifact,
            artifact_format: ModelArtifactFormat::OnnxFile,
            license_id: "Apache-2.0".into(),
            requires_acceptance: true,
            installed_size_bytes: 64,
            allowed_runtimes: BTreeSet::new(),
            required_memory_mib: 512,
            required_accelerator_mib: 0,
            context_window: 0,
            vision: false,
            face_recognition: false,
            segmentation: true,
        }
    }

    #[test]
    fn execution_engine_and_limits_are_signed_and_incompatible_layouts_refused() {
        let key = SigningKey::from_bytes(&[5; 32]);
        let temp = tempfile::tempdir().unwrap();
        let mut catalog = source(temp.path());
        catalog.runtimes[0].execution = Some(RuntimeExecution::LlamaCpp {
            max_context_tokens: 4096,
            gpu_layers: 0,
            parallel_slots: None,
        });
        assert!(compile(catalog.clone(), temp.path(), &key).is_err());
        catalog.models[0].artifact_format = ModelArtifactFormat::LlamaCppGguf;
        catalog.models[0].vision = false;
        let (signed, _) = compile(catalog.clone(), temp.path(), &key).unwrap();
        let signature = Signature::from_slice(&signed.signature).unwrap();
        key.verifying_key()
            .verify(&catalog_signing_bytes(&signed.payload).unwrap(), &signature)
            .unwrap();
        let mut forged = signed.payload;
        forged.runtimes[0].release.execution = Some(RuntimeExecution::LlamaCpp {
            max_context_tokens: 8192,
            gpu_layers: 0,
            parallel_slots: None,
        });
        assert!(
            key.verifying_key()
                .verify(&catalog_signing_bytes(&forged).unwrap(), &signature)
                .is_err()
        );
        catalog.runtimes[0].execution = Some(RuntimeExecution::LlamaCpp {
            max_context_tokens: 0,
            gpu_layers: 0,
            parallel_slots: None,
        });
        assert!(compile(catalog, temp.path(), &key).is_err());
    }

    #[test]
    fn parallel_slots_are_verified_by_the_catalog_consumer() {
        let key = SigningKey::from_bytes(&[5; 32]);
        let temp = tempfile::tempdir().unwrap();
        let mut input = source(temp.path());
        input.models[0].artifact_format = ModelArtifactFormat::LlamaCppGguf;
        input.models[0].vision = false;
        input.runtimes[0].execution = Some(RuntimeExecution::LlamaCpp {
            max_context_tokens: 4096,
            gpu_layers: 0,
            parallel_slots: std::num::NonZeroU16::new(1),
        });
        let (mut signed, trust) = compile(input.clone(), temp.path(), &key).unwrap();
        let cat = temp.path().join("catalog.json");
        let keys = temp.path().join("keys.json");
        fs::write(&cat, serde_json::to_vec(&signed).unwrap()).unwrap();
        fs::write(&keys, serde_json::to_vec(&trust).unwrap()).unwrap();
        assert!(local_model_service::catalog::VerifiedCatalog::load(&cat, &keys).is_ok());
        signed.payload.runtimes[0].release.execution = Some(RuntimeExecution::LlamaCpp {
            max_context_tokens: 4096,
            gpu_layers: 0,
            parallel_slots: std::num::NonZeroU16::new(2),
        });
        fs::write(&cat, serde_json::to_vec(&signed).unwrap()).unwrap();
        assert!(local_model_service::catalog::VerifiedCatalog::load(&cat, &keys).is_err());
        input.runtimes[0].execution = Some(RuntimeExecution::LlamaCpp {
            max_context_tokens: 4096,
            gpu_layers: 0,
            parallel_slots: std::num::NonZeroU16::new(65),
        });
        assert!(compile(input, temp.path(), &key).is_err());
    }

    #[test]
    fn speech_format_is_signed_without_becoming_a_chat_recommendation() {
        let key = SigningKey::from_bytes(&[5; 32]);
        let temp = tempfile::tempdir().unwrap();
        let mut catalog = source(temp.path());
        let mut speech = in_process_source(temp.path());
        speech.artifact_format = ModelArtifactFormat::WhisperGgml;
        speech.segmentation = false;
        catalog.models = vec![speech];
        catalog.runtimes.clear();
        catalog.recommendation.clear();
        let (signed, _) = compile(catalog, temp.path(), &key).unwrap();
        assert_eq!(
            signed.payload.models[0].in_process_capabilities(),
            [agent_provider_config::local::InProcessCapability::SpeechRecognition]
        );
        assert!(signed.payload.recommendation.is_empty());
        let signature = Signature::from_slice(&signed.signature).unwrap();
        key.verifying_key()
            .verify(&catalog_signing_bytes(&signed.payload).unwrap(), &signature)
            .unwrap();
        let mut forged = signed.payload;
        forged.models[0].artifact_format = ModelArtifactFormat::OnnxFile;
        assert!(
            key.verifying_key()
                .verify(&catalog_signing_bytes(&forged).unwrap(), &signature)
                .is_err()
        );
    }

    #[test]
    fn in_process_only_catalog_compiles_without_chat_recommendation() {
        let key = SigningKey::from_bytes(&[5; 32]);
        let temp = tempfile::tempdir().unwrap();
        let mut catalog = source(temp.path());
        catalog.models = vec![in_process_source(temp.path())];
        catalog.runtimes.clear();
        catalog.recommendation.clear();
        let (signed, trust) = compile(catalog.clone(), temp.path(), &key).unwrap();
        assert_eq!(signed.payload.models.len(), 1);
        assert!(signed.payload.runtimes.is_empty());
        assert!(signed.payload.recommendation.is_empty());
        let public = hex::decode(&trust.keys["test-release-1"]).unwrap();
        let verifying =
            ed25519_dalek::VerifyingKey::from_bytes(&public.try_into().unwrap()).unwrap();
        verifying
            .verify(
                &catalog_signing_bytes(&signed.payload).unwrap(),
                &Signature::from_slice(&signed.signature).unwrap(),
            )
            .unwrap();

        catalog.recommendation = "example-subject".into();
        assert!(
            compile(catalog, temp.path(), &key)
                .unwrap_err()
                .contains("runtime-served")
        );
        let mut served = source(temp.path());
        served.runtimes.clear();
        assert!(compile(served, temp.path(), &key).is_err());
    }

    /// An ONNX model compiles with no runtime and no context window, but
    /// still cannot name a runtime, claim chat or vision, or become the chat
    /// recommendation; and a served model still needs both.
    #[test]
    fn in_process_models_compile_without_a_runtime_and_keep_their_shape() {
        let key = SigningKey::from_bytes(&[5; 32]);
        let temp = tempfile::tempdir().unwrap();
        let mut catalog = source(temp.path());
        catalog.models.push(in_process_source(temp.path()));
        let (signed, _) = compile(catalog, temp.path(), &key).unwrap();
        let subject = signed
            .payload
            .models
            .iter()
            .find(|model| model.id == "example-subject")
            .unwrap();
        assert!(subject.runs_in_process());
        assert!(subject.segmentation);
        assert!(subject.allowed_runtimes.is_empty());

        let refused = |edit: fn(&mut ModelSource)| {
            let temp = tempfile::tempdir().unwrap();
            let mut catalog = source(temp.path());
            let mut model = in_process_source(temp.path());
            edit(&mut model);
            catalog.models.push(model);
            compile(catalog, temp.path(), &SigningKey::from_bytes(&[5; 32])).unwrap_err()
        };
        assert!(
            refused(|model| model.allowed_runtimes = BTreeSet::from([RuntimeKind::Cpu]))
                .contains("must not name a vLLM runtime")
        );
        assert!(refused(|model| model.context_window = 4096).contains("cannot claim chat"));
        assert!(refused(|model| model.vision = true).contains("cannot claim chat"));
        assert!(refused(|model| model.segmentation = false).contains("no capability"));
        assert!(
            refused(|model| model.artifact.url =
                "https://release.example.invalid/subject/u2net.onnx".into())
            .contains("immutable revision")
        );

        let temp = tempfile::tempdir().unwrap();
        let mut catalog = source(temp.path());
        catalog.models.push(in_process_source(temp.path()));
        catalog.recommendation = "example-subject".into();
        assert!(
            compile(catalog, temp.path(), &key)
                .unwrap_err()
                .contains("runtime-served")
        );

        let temp = tempfile::tempdir().unwrap();
        let mut served = source(temp.path());
        served.models[0].context_window = 0;
        assert!(
            compile(served, temp.path(), &key)
                .unwrap_err()
                .contains("context window")
        );
        let temp = tempfile::tempdir().unwrap();
        let mut served = source(temp.path());
        served.models[0].allowed_runtimes.clear();
        assert!(
            compile(served, temp.path(), &key)
                .unwrap_err()
                .contains("no runtime")
        );
    }

    /// Checked-in non-production source compiles without network resolution, output is deterministic
    /// and verifies with public trust only, and an artifact receipt is resolved with a declared mismatch
    /// failing.
    #[test]
    fn catalog_compilation_is_offline_deterministic_and_receipt_checked() {
        // Phase: checked_in_nonproduction_source_compiles_without_network_resolution.
        {
            let source: CatalogSource = serde_json::from_str(include_str!(
                "../../tests/fixtures/catalog-source.example.json"
            ))
            .unwrap();
            let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
            let (catalog, trust) =
                compile(source, &fixtures, &SigningKey::from_bytes(&[11; 32])).unwrap();
            assert_eq!(catalog.payload.recommendation, "example/tiny-model");
            assert_eq!(catalog.payload.models[0].artifact.size_bytes, 42);
            assert_eq!(catalog.payload.runtimes[0].release.artifact.size_bytes, 44);
            let subject = &catalog.payload.models[1];
            assert_eq!(subject.id, "example/tiny-subject");
            assert!(subject.runs_in_process() && subject.segmentation);
            assert_eq!(trust.keys.len(), 1);
        }

        // Phase: output_is_deterministic_and_verifies_with_public_trust_only.
        {
            let temp = tempfile::tempdir().unwrap();
            let key = SigningKey::from_bytes(&[7; 32]);
            let first = compile(source(temp.path()), temp.path(), &key).unwrap();
            let second = compile(source(temp.path()), temp.path(), &key).unwrap();
            assert_eq!(first, second);
            let payload = catalog_signing_bytes(&first.0.payload).unwrap();
            assert!(first.0.payload.models[0].vision);
            let signature = Signature::from_slice(&first.0.signature).unwrap();
            key.verifying_key().verify(&payload, &signature).unwrap();
            let trust_json = serde_json::to_string(&first.1).unwrap();
            assert!(trust_json.contains(&hex::encode(key.verifying_key().to_bytes())));
            assert!(!trust_json.contains(&hex::encode(key.to_bytes())));
        }

        // Phase: artifact_receipt_is_resolved_and_declared_mismatch_fails.
        {
            let temp = tempfile::tempdir().unwrap();
            let mut source = source(temp.path());
            source.models[0].artifact.sha256 = Some("f".repeat(64));
            let error =
                compile(source, temp.path(), &SigningKey::from_bytes(&[3; 32])).unwrap_err();
            assert!(error.contains("SHA-256 does not match"));
        }
    }

    /// Artifact digests never follow a symlink, mutable revisions and URLs are rejected, and a
    /// production source rejects non-production hosts.
    #[test]
    fn catalog_sources_must_be_immutable_and_production_hosted() {
        // Phase: mutable_revision_and_url_are_rejected.
        {
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

        // Phase: production_source_rejects_nonproduction_hosts.
        {
            let temp = tempfile::tempdir().unwrap();
            let mut source = source(temp.path());
            source.production = true;
            assert!(
                compile(source, temp.path(), &SigningKey::from_bytes(&[3; 32]))
                    .unwrap_err()
                    .contains("reserved .invalid")
            );
        }

        // Phase: artifact_digest_never_follows_a_symlink.
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;

            let temp = tempfile::tempdir().unwrap();
            let catalog = source(temp.path());
            let runtime = temp.path().join("runtime.bin");
            let replacement = temp.path().join("replacement.bin");
            fs::write(&replacement, b"substituted release payload").unwrap();
            fs::remove_file(&runtime).unwrap();
            symlink(&replacement, &runtime).unwrap();

            let error =
                compile(catalog, temp.path(), &SigningKey::from_bytes(&[3; 32])).unwrap_err();
            assert!(error.contains("could not securely open"));

            fs::remove_file(&runtime).unwrap();
            fs::write(&runtime, b"restored runtime payload").unwrap();
            let linked_parent = temp.path().join("linked-parent");
            symlink(temp.path(), &linked_parent).unwrap();
            let source = source(temp.path());
            let error =
                compile(source, &linked_parent, &SigningKey::from_bytes(&[3; 32])).unwrap_err();
            assert!(error.contains("could not securely open"));
        }
    }

    /// The signing key must be private on disk, rejects symlinks and aliased inodes, and its input
    /// is bounded.
    #[test]
    fn the_catalog_signing_key_must_be_a_private_bounded_single_file() {
        // Phase: signing_key_must_be_private_on_disk.
        #[cfg(unix)]
        {
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

        // Phase: signing_key_input_is_bounded.
        {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("key");
            fs::write(&path, "a".repeat(130)).unwrap();
            #[cfg(unix)]
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            assert!(read_signing_key(&path).unwrap_err().contains("too large"));
        }

        // Phase: signing_key_rejects_symlinks_and_aliased_inodes.
        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;

            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("key");
            let alias = temp.path().join("alias");
            let link = temp.path().join("link");
            fs::write(&path, hex::encode([7_u8; 32])).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();

            symlink(&path, &link).unwrap();
            assert!(
                read_signing_key(&link)
                    .unwrap_err()
                    .contains("securely open")
            );

            fs::hard_link(&path, &alias).unwrap();
            assert!(
                read_signing_key(&path)
                    .unwrap_err()
                    .contains("exactly one directory entry")
            );

            fs::remove_file(&alias).unwrap();
            let linked_parent = temp.path().join("linked-parent");
            symlink(temp.path(), &linked_parent).unwrap();
            assert!(
                read_signing_key(&linked_parent.join("key"))
                    .unwrap_err()
                    .contains("securely open")
            );
        }
    }
}
