//! `tilo-installer-offline-target verify|import|receipt`: authenticates and
//! imports the separately signed, offline installer target.
//!
//! The live medium is transport, not the installed image. `verify` runs
//! before any disk mutation: the target's metadata, its OCI manifest and
//! config, its Cosign signature and its CI SLSA provenance must all bind the
//! one selected image, checked against the image's own Cosign key, and the
//! verified facts are saved as a private receipt. `import` rechecks the
//! layout against that receipt, copies the exact manifest into local
//! container storage and prints the immutable image ID.
//!
//! An unsigned development target is admitted only by the VM-only
//! development installer, after the transport checks of
//! [`crate::target_transport`].

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use serde_json::{Map, Value, json};

use crate::py::Dumps;
use crate::target_transport::{is_digest, is_hex, loads, repository};
use crate::{Error, Result, require};

const OFFLINE_SCHEMA: &str = "tilo.installer-offline-target.v1";
const DEV_SCHEMA: &str = "tilo.installer-dev-target.v1";
const IMAGE: [&str; 2] = [
    "application/vnd.oci.image.manifest.v1+json",
    "application/vnd.docker.distribution.manifest.v2+json",
];
/// The SLSA build type of a Tilo OS release.
pub const BUILD_TYPE: &str = "https://tilo.sh/buildtypes/bootc-os/v1";
/// The builder every Tilo OS release names.
pub const BUILDER: &str = "https://code.tilo.so/tilo/forgejo/actions/bootc-release-v1";
const MIB: u64 = 1024 * 1024;

/// The installer the target is checked in. The provided methods are the
/// production behaviour; tests replace them.
pub trait Host {
    /// Where the live medium stages the target.
    fn root(&self) -> PathBuf {
        PathBuf::from("/var/lib/tilo-installer/target-oci")
    }

    /// The Cosign public key the image trusts.
    fn key(&self) -> PathBuf {
        PathBuf::from("/usr/share/tilo/cosign.pub")
    }

    /// The image's Cosign verifier.
    fn cosign(&self) -> PathBuf {
        PathBuf::from("/usr/libexec/tilo-cosign")
    }

    /// Where the verified facts are kept for `import` and the install proof.
    fn receipt(&self) -> PathBuf {
        PathBuf::from("/run/tilo-installer/offline-target.json")
    }

    /// The marker only the VM-only development installer carries.
    fn dev_vm_marker(&self) -> PathBuf {
        PathBuf::from("/usr/share/tilo/unverified-vm-install")
    }

    /// The running image's flavour.
    fn dev_flavour(&self) -> PathBuf {
        PathBuf::from("/usr/lib/tilo/image-flavour")
    }

    /// Whether the install proof's dedicated virtio port is present.
    fn proof_port(&self) -> bool {
        fs::metadata("/dev/virtio-ports/org.tilo.vm-install-proof")
            .is_ok_and(|info| info.file_type().is_char_device())
    }

    /// `os.access(path, os.X_OK)`.
    fn executable(&self, path: &Path) -> bool {
        rustix::fs::access(path, rustix::fs::Access::EXEC_OK).is_ok()
    }

    /// A tool's stripped standard output; a nonzero exit is refused with its
    /// status and stderr, never its arguments.
    ///
    /// # Errors
    /// The tool failing, missing or timing out.
    fn run(&self, argv: &[&str], timeout: Duration) -> Result<String> {
        let output = crate::process::run(argv, None, timeout)?;
        let program = crate::process::name(argv.first().copied().unwrap_or_default());
        if !output.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stderr: String = crate::py::strip(&stderr).chars().take(1024).collect();
            return Err(Error::new(format!(
                "{program} failed (exit {}): {}",
                output.returncode(),
                if stderr.is_empty() {
                    "no diagnostic stderr"
                } else {
                    stderr.as_str()
                }
            )));
        }
        Ok(crate::py::strip(&String::from_utf8_lossy(&output.stdout)).to_owned())
    }
}

/// The live installer as it is.
#[derive(Debug, Clone, Copy, Default)]
pub struct System;

impl Host for System {}

fn seconds(count: u64) -> Duration {
    Duration::from_secs(count)
}

/// The bytes of a regular file of at most `limit` bytes, never through a link.
fn checked_file(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let info = fs::symlink_metadata(path)?;
    require(
        info.file_type().is_file() && info.len() <= limit,
        &format!("unsafe or oversized target input: {}", path.display()),
    )?;
    Ok(fs::read(path)?)
}

fn get<'a>(map: &'a Map<String, Value>, key: &str) -> &'a Value {
    map.get(key).unwrap_or(&Value::Null)
}

fn text<'a>(map: &'a Map<String, Value>, key: &str) -> &'a str {
    get(map, key).as_str().unwrap_or_default()
}

fn attribute_error(what: &str) -> Error {
    Error::new(format!("{what} has no attribute 'get'"))
}

fn as_map<'a>(value: &'a Value, what: &str) -> Result<&'a Map<String, Value>> {
    value.as_object().ok_or_else(|| attribute_error(what))
}

/// The development target behind the VM-only gate.
fn development_target(host: &dyn Host, root: &Path) -> Result<Map<String, Value>> {
    let marker = host.dev_vm_marker();
    let flavour = host.dev_flavour();
    require(
        marker.is_file()
            && !marker.is_symlink()
            && flavour.is_file()
            && !flavour.is_symlink()
            && fs::read(&flavour).is_ok_and(|bytes| bytes == b"dev\n")
            && host.proof_port(),
        "unsigned development target requires the development VM installer",
    )?;
    host.run(&["systemd-detect-virt", "--vm", "--quiet"], seconds(180))?;
    crate::target_transport::verify(root, true)
}

/// `(repository)@(sha256:…)` of a signed target reference.
fn split_reference(reference: &str) -> Option<(&str, &str)> {
    let (repository_name, digest) = reference.split_once('@')?;
    (matches!(
        repository_name,
        "registry.tilo.so/tilo/tilo" | "code.tilo.so/tilo/tilo"
    ) && is_digest(digest))
    .then_some((repository_name, digest))
}

/// `[1-9][0-9]*`.
fn is_generation(text: &str) -> bool {
    let mut bytes = text.bytes();
    bytes
        .next()
        .is_some_and(|first| matches!(first, b'1'..=b'9'))
        && bytes.all(|b| b.is_ascii_digit())
}

fn valid_origin(profile: &str, origin: &str) -> bool {
    match profile {
        "lenovo-83ed" => origin
            .strip_prefix("code.tilo.so/tilo/tilo:lenovo-83ed-")
            .is_some_and(|revision| is_hex(revision, 40)),
        "generic" => origin
            .strip_prefix("registry.tilo.so/tilo/tilo:gen")
            .is_some_and(is_generation),
        _ => false,
    }
}

/// The checked metadata and OCI layout of the staged target.
///
/// # Errors
/// Anything that differs from one signed-target layout of the pinned image.
pub fn read_target(host: &dyn Host, root: &Path) -> Result<(Map<String, Value>, PathBuf)> {
    require(!root.is_symlink(), "offline target directory is a symlink")?;
    let root = fs::canonicalize(root)?;
    require(root.is_dir(), "offline target directory is unsafe")?;
    let metadata = loads(&checked_file(&root.join("target.json"), 8192)?)?;
    if let Value::Object(map) = &metadata
        && get(map, "schema").as_str() == Some(DEV_SCHEMA)
    {
        return Ok((development_target(host, &root)?, root.join("oci")));
    }
    let metadata = signed_metadata(metadata)?;
    let layout = root.join("oci");
    require(
        layout.is_dir() && !layout.is_symlink(),
        "offline OCI layout is unsafe",
    )?;
    require(
        crate::py::equal(
            &loads(&checked_file(&layout.join("oci-layout"), 256)?)?,
            &json!({"imageLayoutVersion": "1.0.0"}),
        ),
        "offline OCI layout version is invalid",
    )?;
    let target_digest = text(&metadata, "target_digest");
    let descriptor = target_descriptor(&layout, target_digest)?;
    let target_hex = target_digest.strip_prefix("sha256:").unwrap_or_default();
    let blobs = layout.join("blobs/sha256");
    let manifest = checked_file(&blobs.join(target_hex), 16 * MIB)?;
    require(
        crate::py::equal(get(&descriptor, "size"), &json!(manifest.len()))
            && crate::py::sha256_hex(&manifest) == target_hex,
        "offline OCI manifest differs from pinned CI digest",
    )?;
    check_config(&blobs, &loads(&manifest)?)?;
    Ok((metadata, layout))
}

/// The signed target's metadata: exactly its six string fields, pinning a
/// supported child by digest, repository and update origin.
fn signed_metadata(metadata: Value) -> Result<Map<String, Value>> {
    let keys = [
        "schema",
        "target_ref",
        "target_digest",
        "architecture",
        "hardware_profile",
        "update_origin",
    ];
    let metadata = match metadata {
        Value::Object(map)
            if map.len() == keys.len() && keys.iter().all(|key| map.contains_key(*key)) =>
        {
            map
        }
        _ => {
            return Err(Error::new("offline target metadata has invalid fields"));
        }
    };
    require(
        metadata.values().all(Value::is_string),
        "offline target metadata has invalid field types",
    )?;
    let profile = text(&metadata, "hardware_profile");
    let reference = split_reference(text(&metadata, "target_ref"));
    let identity = reference.is_some_and(|(repository_name, digest)| {
        text(&metadata, "schema") == OFFLINE_SCHEMA
            && text(&metadata, "target_digest") == digest
            && text(&metadata, "architecture") == "aarch64"
            && Some(repository_name) == repository(profile)
            && valid_origin(profile, text(&metadata, "update_origin"))
    });
    require(
        identity,
        "offline target metadata has invalid image identity",
    )?;
    Ok(metadata)
}

/// The one index descriptor of the pinned target, a signed image manifest.
fn target_descriptor(layout: &Path, target_digest: &str) -> Result<Map<String, Value>> {
    let index = loads(&checked_file(&layout.join("index.json"), MIB)?)?;
    let manifests = match &index {
        Value::Object(index) if crate::py::equal(get(index, "schemaVersion"), &json!(2)) => {
            match index.get("manifests") {
                Some(Value::Array(manifests)) => manifests,
                _ => return Err(Error::new("offline OCI index is invalid")),
            }
        }
        _ => return Err(Error::new("offline OCI index is invalid")),
    };
    let descriptors: Vec<&Map<String, Value>> = manifests
        .iter()
        .filter_map(Value::as_object)
        .filter(|item| get(item, "digest").as_str() == Some(target_digest))
        .collect();
    let [descriptor] = descriptors.as_slice() else {
        return Err(Error::new(
            "offline OCI index has no unique target manifest",
        ));
    };
    let annotated = get(descriptor, "annotations")
        .as_object()
        .is_some_and(|annotations| {
            get(annotations, "kind").as_str() == Some("dev.cosignproject.cosign/image")
        });
    require(
        get(descriptor, "mediaType")
            .as_str()
            .is_some_and(|media| IMAGE.contains(&media))
            && annotated,
        "offline OCI target descriptor is not the selected image",
    )?;
    Ok((*descriptor).clone())
}

/// The manifest's config descriptor is exact and names a Linux ARM64 image.
fn check_config(blobs: &Path, parsed: &Value) -> Result<()> {
    let parsed = as_map(parsed, "manifest")?;
    let config = get(parsed, "config");
    let config_digest = config
        .as_object()
        .and_then(|config| config.get("digest"))
        .and_then(Value::as_str)
        .filter(|digest| is_digest(digest));
    let config_size = config
        .as_object()
        .and_then(|config| config.get("size"))
        .filter(|size| size.is_u64() || size.is_i64())
        .and_then(Value::as_u64)
        .filter(|size| (1..=16 * MIB).contains(size));
    let (Some(config_digest), Some(config_size)) = (config_digest, config_size) else {
        return Err(Error::new(
            "offline target manifest has invalid config descriptor",
        ));
    };
    require(
        crate::py::equal(get(parsed, "schemaVersion"), &json!(2)),
        "offline target manifest has invalid config descriptor",
    )?;
    let config_hex = config_digest.strip_prefix("sha256:").unwrap_or_default();
    let config_bytes = checked_file(&blobs.join(config_hex), 16 * MIB)?;
    require(
        u64::try_from(config_bytes.len()).is_ok_and(|length| length == config_size)
            && crate::py::sha256_hex(&config_bytes) == config_hex,
        "offline target config differs from pinned manifest",
    )?;
    let image_config = loads(&config_bytes)?;
    require(
        image_config.as_object().is_some_and(|image| {
            get(image, "architecture").as_str() == Some("arm64")
                && get(image, "os").as_str() == Some("linux")
        }),
        "offline target image is not Linux ARM64",
    )
}

/// The rows of Cosign's verification output: one JSON document (a list or
/// an object), or one per line.
///
/// # Errors
/// Empty, oversized or malformed output.
pub fn wrappers(output: &str) -> Result<Vec<Map<String, Value>>> {
    let length = output.chars().count();
    require(
        length > 0 && length <= 8 * 1024 * 1024,
        "Cosign verification output is invalid",
    )?;
    let values: Vec<Value> = match serde_json::from_str::<Value>(output) {
        Ok(value) => vec![value],
        Err(_) => output
            .lines()
            .filter(|line| !crate::py::strip(line).is_empty())
            .map(|line| serde_json::from_str(line).map_err(|error| Error::new(error.to_string())))
            .collect::<Result<_>>()?,
    };
    let rows: Vec<Value> = values
        .into_iter()
        .flat_map(|value| match value {
            Value::Array(items) => items,
            other => vec![other],
        })
        .collect();
    require(
        !rows.is_empty() && rows.iter().all(Value::is_object),
        "Cosign verification output is malformed",
    )?;
    Ok(rows
        .into_iter()
        .filter_map(|row| match row {
            Value::Object(map) => Some(map),
            _ => None,
        })
        .collect())
}

/// `git+ssh://code.tilo.so/tilo/tilo@<40 hex>`: the source revision.
fn source_revision(uri: &str) -> Option<&str> {
    uri.strip_prefix("git+ssh://code.tilo.so/tilo/tilo@")
        .filter(|revision| is_hex(revision, 40))
}

/// `str(value)` of a JSON value, as far as a decimal check needs it.
fn python_str(value: &Value) -> String {
    match value {
        Value::Null => "None".into(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn is_decimal(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit())
}

/// `type(value) is int`: a JSON integer.
fn as_int(value: &Value) -> Option<i128> {
    value
        .as_i64()
        .map(i128::from)
        .or_else(|| value.as_u64().map(i128::from))
}

/// `dict.get(key, {})` that must be a dict, as `isinstance(…, dict)` checked.
fn sub_object<'a>(
    map: &'a Map<String, Value>,
    key: &str,
    empty: &'a Map<String, Value>,
) -> Option<&'a Map<String, Value>> {
    match map.get(key) {
        None => Some(empty),
        Some(Value::Object(inner)) => Some(inner),
        Some(_) => None,
    }
}

fn sub_list<'a>(
    map: &'a Map<String, Value>,
    key: &str,
    empty: &'a Vec<Value>,
) -> Option<&'a Vec<Value>> {
    match map.get(key) {
        None => Some(empty),
        Some(Value::Array(items)) => Some(items),
        Some(_) => None,
    }
}

/// The predicate of the SLSA v1 statement in `row` whose subject is the
/// target, if `row` carries one.
fn target_predicate(row: &Map<String, Value>, target_hex: &str) -> Option<Map<String, Value>> {
    let payload = row.get("payload")?.as_str()?;
    let statement = crate::py::b64decode(payload).ok()?;
    let statement: Value = serde_json::from_slice(&statement).ok()?;
    let statement = statement.as_object()?;
    if get(statement, "predicateType").as_str() != Some("https://slsa.dev/provenance/v1") {
        return None;
    }
    let subjects = get(statement, "subject").as_array()?;
    let predicate = get(statement, "predicate").as_object()?;
    subjects
        .iter()
        .any(|item| {
            item.as_object()
                .and_then(|item| item.get("digest"))
                .and_then(Value::as_object)
                .is_some_and(|digest| get(digest, "sha256").as_str() == Some(target_hex))
        })
        .then(|| predicate.clone())
}

/// The firmware candidate and manifest a board target's provenance binds
/// (both null for the generic image), if its parameters fit the profile.
fn firmware_binding(
    metadata: &Map<String, Value>,
    external: &Map<String, Value>,
    firmware: Option<Value>,
    source: &str,
    generation: i128,
) -> Option<(Value, Value)> {
    if text(metadata, "hardware_profile") == "lenovo-83ed" {
        let manifest = get(external, "firmwareManifestSha256").as_str();
        let firmware_ok = firmware
            .as_ref()
            .and_then(Value::as_str)
            .is_some_and(|firmware| {
                firmware
                    .strip_prefix("code.tilo.so/tilo/tilo-lenovo-83ed-firmware@sha256:")
                    .is_some_and(|hex| is_hex(hex, 64))
            });
        if get(external, "architecture").as_str() != Some("aarch64")
            || get(external, "hardwareProfile").as_str() != Some("lenovo-83ed")
            || !manifest.is_some_and(|manifest| is_hex(manifest, 64))
            || external.contains_key("children")
            || text(metadata, "update_origin")
                != format!("code.tilo.so/tilo/tilo:lenovo-83ed-{source}")
            || !firmware_ok
        {
            return None;
        }
        return Some((
            firmware.unwrap_or(Value::Null),
            Value::String(manifest.unwrap_or_default().to_owned()),
        ));
    }
    let unbound = [
        "hardwareProfile",
        "architecture",
        "firmwareManifestSha256",
        "children",
    ];
    (text(metadata, "update_origin") == format!("registry.tilo.so/tilo/tilo:gen{generation}")
        && !unbound.iter().any(|key| external.contains_key(*key)))
    .then_some((Value::Null, Value::Null))
}

/// The one provenance statement in `row` that binds `metadata`, if any.
fn provenance_match(row: &Map<String, Value>, metadata: &Map<String, Value>) -> Option<Value> {
    let target_hex = text(metadata, "target_digest").get(7..).unwrap_or_default();
    let predicate = target_predicate(row, target_hex)?;
    let (empty, no_items) = (Map::new(), Vec::new());
    let definition = sub_object(&predicate, "buildDefinition", &empty)?;
    let details = sub_object(&predicate, "runDetails", &empty)?;
    let external = sub_object(definition, "externalParameters", &empty)?;
    let internal = sub_object(definition, "internalParameters", &empty)?;
    let dependencies = sub_list(definition, "resolvedDependencies", &no_items)?;
    let byproducts = sub_list(details, "byproducts", &no_items)?;
    let builder = get(details, "builder").as_object()?;
    let generation = as_int(get(external, "generation")).filter(|generation| *generation >= 1)?;
    let rebuilt = byproducts.iter().any(|item| {
        item.as_object().is_some_and(|item| {
            get(item, "name").as_str() == Some("independent-rebuild")
                && get(item, "uri").as_str() == Some("urn:tilo:verification:manifest-digest-equal")
        })
    });
    if get(definition, "buildType").as_str() != Some(BUILD_TYPE)
        || get(builder, "id").as_str() != Some(BUILDER)
        || get(internal, "expectedReleaseDigest") != get(metadata, "target_digest")
        || !rebuilt
    {
        return None;
    }
    let sources: Vec<&str> = dependencies
        .iter()
        .filter_map(Value::as_object)
        .filter_map(|item| item.get("uri").and_then(Value::as_str))
        .filter_map(source_revision)
        .collect();
    let firmware = dependencies
        .last()
        .and_then(Value::as_object)
        .map(|last| get(last, "uri").clone());
    let invocation = get(details, "metadata")
        .as_object()
        .map_or(Value::Null, |run| get(run, "invocationId").clone());
    let [source] = sources.as_slice() else {
        return None;
    };
    if !is_decimal(&python_str(&invocation)) {
        return None;
    }
    let (firmware, firmware_manifest) =
        firmware_binding(metadata, external, firmware, source, generation)?;
    let mut found = Map::new();
    found.insert(
        "source_revision".into(),
        Value::String((*source).to_owned()),
    );
    found.insert("firmware_candidate".into(), firmware);
    found.insert("firmware_manifest_sha256".into(), firmware_manifest);
    found.insert("generation".into(), get(external, "generation").clone());
    found.insert("invocation_id".into(), invocation);
    Some(Value::Object(found))
}

/// The CI SLSA provenance in Cosign's attestation output that binds
/// `metadata`: exactly one statement must.
///
/// # Errors
/// Malformed output, or no unique binding statement.
pub fn checked_provenance(output: &str, metadata: &Map<String, Value>) -> Result<Value> {
    let matches: Vec<Value> = wrappers(output)?
        .iter()
        .filter_map(|row| provenance_match(row, metadata))
        .collect();
    match <[Value; 1]>::try_from(matches) {
        Ok([found]) => Ok(found),
        Err(_) => Err(Error::new(
            "offline target has no unique CI SLSA provenance",
        )),
    }
}

fn save_receipt(host: &dyn Host, result: &Value) -> Result<()> {
    let receipt = host.receipt();
    if let Some(parent) = receipt.parent() {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)?;
    }
    let temporary = receipt.with_extension("tmp");
    let mut text = Dumps::CANONICAL.dumps(result);
    text.push('\n');
    fs::write(&temporary, text)?;
    fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))?;
    fs::rename(&temporary, &receipt)?;
    Ok(())
}

use std::os::unix::fs::DirBuilderExt as _;

fn signature_binds(row: &Map<String, Value>, metadata: &Map<String, Value>) -> Result<bool> {
    let Some(Value::Object(critical)) = row.get("critical") else {
        return Ok(false);
    };
    let empty = Map::new();
    let image = sub_object(critical, "image", &empty).ok_or_else(|| attribute_error("image"))?;
    if get(image, "docker-manifest-digest") != get(metadata, "target_digest") {
        return Ok(false);
    }
    let identity =
        sub_object(critical, "identity", &empty).ok_or_else(|| attribute_error("identity"))?;
    Ok(
        get(identity, "docker-reference").as_str()
            == repository(text(metadata, "hardware_profile")),
    )
}

/// Verifies the staged target and saves the receipt; a development target is
/// also imported.
///
/// # Errors
/// Any check refusing, or a tool failing.
pub fn verify(host: &dyn Host) -> Result<Value> {
    let root = host.root();
    let (metadata, layout) = read_target(host, &root)?;
    if text(&metadata, "schema") == DEV_SCHEMA {
        let mut result = metadata.clone();
        result.insert("verifier_key_sha256".into(), Value::Null);
        result.insert(
            "provenance".into(),
            json!({
                "source_revision": get(&metadata, "source_revision"),
                "invocation_id": get(&metadata, "ci_run_id"),
            }),
        );
        let result = Value::Object(result);
        save_receipt(host, &result)?;
        import_image(host)?;
        return Ok(result);
    }
    let (key, cosign) = (host.key(), host.cosign());
    require(
        key.is_file() && cosign.is_file() && host.executable(&cosign),
        "immutable Cosign verifier or public key is missing",
    )?;
    let (key_text, cosign_text, layout_text) = (
        key.to_string_lossy().into_owned(),
        cosign.to_string_lossy().into_owned(),
        layout.to_string_lossy().into_owned(),
    );
    let signature = wrappers(&host.run(
        &[
            &cosign_text,
            "verify",
            "--offline",
            "--key",
            &key_text,
            "--local-image",
            &layout_text,
        ],
        seconds(300),
    )?)?;
    let mut bound = false;
    for row in &signature {
        if signature_binds(row, &metadata)? {
            bound = true;
            break;
        }
    }
    require(
        bound,
        "offline Cosign signature does not bind the selected target",
    )?;
    let provenance = checked_provenance(
        &host.run(
            &[
                &cosign_text,
                "verify-attestation",
                "--offline",
                "--key",
                &key_text,
                "--type",
                "slsaprovenance1",
                "--local-image",
                &layout_text,
            ],
            seconds(300),
        )?,
        &metadata,
    )?;
    let mut result = metadata;
    result.insert("provenance".into(), provenance);
    result.insert(
        "verifier_key_sha256".into(),
        Value::String(crate::py::sha256_hex(&fs::read(&key)?)),
    );
    let result = Value::Object(result);
    save_receipt(host, &result)?;
    Ok(result)
}

fn is_image_id(text: &str) -> bool {
    is_hex(text.strip_prefix("sha256:").unwrap_or(text), 64)
}

/// Imports the verified target into local container storage and returns its
/// immutable image ID.
///
/// # Errors
/// No verified preflight, metadata changed since, an import that differs from
/// the signed manifest, or a tool failing.
pub fn import_image(host: &dyn Host) -> Result<String> {
    let root = host.root();
    let (metadata, layout) = read_target(host, &root)?;
    let receipt_path = host.receipt();
    require(
        receipt_path.is_file() && !receipt_path.is_symlink(),
        "offline target has no verified preflight",
    )?;
    let receipt = loads(&checked_file(&receipt_path, 8192)?)?;
    require(
        receipt.as_object().is_some_and(|receipt| {
            metadata
                .iter()
                .all(|(key, value)| crate::py::equal(get(receipt, key), value))
        }),
        "offline target metadata changed after verified preflight",
    )?;
    let target_digest = text(&metadata, "target_digest");
    let destination = format!(
        "localhost/tilo-offline-target:{}",
        target_digest.get(7..).unwrap_or_default()
    );
    let descriptor = view_descriptor(&layout, target_digest)?;
    let parent = receipt_path.parent().unwrap_or_else(|| Path::new("/"));
    copy_to_storage(host, &layout, parent, &descriptor, &destination)?;
    let image_id = host.run(
        &[
            "podman",
            "image",
            "inspect",
            "--format",
            "{{.Id}}",
            &destination,
        ],
        seconds(120),
    )?;
    require(
        is_image_id(&image_id),
        "offline OCI import returned no immutable image ID",
    )?;
    let imported = host.run(
        &[
            "podman",
            "image",
            "inspect",
            "--format",
            "{{.Digest}}",
            &image_id,
        ],
        seconds(120),
    )?;
    require(
        imported == target_digest,
        "imported image manifest differs from the signed target",
    )?;
    if text(&metadata, "schema") == DEV_SCHEMA {
        check_development_image(host, &image_id)?;
    }
    host.run(
        &[
            "podman",
            "run",
            "--rm",
            "--pull=never",
            "--network=none",
            "--cap-drop=ALL",
            "--entrypoint",
            "/usr/libexec/tilo-board-successor",
            &image_id,
            "install-update-origin",
            text(&metadata, "update_origin"),
        ],
        seconds(120),
    )?;
    // Bootc consumes this immutable ID; the guest proof checks its resulting
    // deployment's manifest digest and origin against the signed receipt.
    Ok(image_id)
}

/// `receipt`: what the installer backend needs from the verified preflight,
/// as `UPDATE_ORIGIN=` and `DEVELOPMENT=0|1` lines.
///
/// # Errors
/// No saved preflight, or one without an update origin.
pub fn receipt_facts(host: &dyn Host) -> Result<String> {
    let path = host.receipt();
    require(
        path.is_file() && !path.is_symlink(),
        "offline target has no verified preflight",
    )?;
    let receipt = loads(&checked_file(&path, 8192)?)?;
    let receipt = as_map(&receipt, "receipt")?;
    let origin = get(receipt, "update_origin")
        .as_str()
        .filter(|origin| !origin.is_empty() && !origin.contains('\n'))
        .ok_or_else(|| Error::new("verified preflight names no update origin"))?;
    let development = u8::from(text(receipt, "schema") == DEV_SCHEMA);
    Ok(format!("UPDATE_ORIGIN={origin}\nDEVELOPMENT={development}"))
}

/// The target's index descriptor, named `target` for the one-image view.
fn view_descriptor(layout: &Path, target_digest: &str) -> Result<Map<String, Value>> {
    let index = loads(&checked_file(&layout.join("index.json"), MIB)?)?;
    let manifests = as_map(&index, "index")?
        .get("manifests")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::new("'manifests'"))?;
    let mut descriptor = None;
    for item in manifests {
        let item = as_map(item, "descriptor")?;
        if get(item, "digest").as_str() == Some(target_digest) {
            descriptor = Some(item.clone());
            break;
        }
    }
    let mut descriptor = descriptor.ok_or_else(|| Error::new("StopIteration"))?;
    let mut annotations = match descriptor.get("annotations") {
        None => Map::new(),
        Some(Value::Object(annotations)) => annotations.clone(),
        Some(_) => return Err(Error::new("dict() argument must be a mapping")),
    };
    annotations.insert(
        "org.opencontainers.image.ref.name".into(),
        Value::String("target".into()),
    );
    descriptor.insert("annotations".into(), Value::Object(annotations));
    Ok(descriptor)
}

/// Copies the target into local container storage as `destination`.
///
/// Cosign's saved layout also contains signature/attestation descriptors.
/// A tiny one-descriptor view, in a private directory under `parent`, links
/// to the same hashed blobs. Skopeo transfers directly into the local store;
/// `podman load` would stage another archive-sized copy in /var/tmp first.
fn copy_to_storage(
    host: &dyn Host,
    layout: &Path,
    parent: &Path,
    descriptor: &Map<String, Value>,
    destination: &str,
) -> Result<()> {
    let view = tempfile::Builder::new()
        .prefix("tilo-target-view-")
        .tempdir_in(parent)?;
    fs::write(
        view.path().join("oci-layout"),
        "{\"imageLayoutVersion\":\"1.0.0\"}",
    )?;
    fs::write(
        view.path().join("index.json"),
        Dumps::COMPACT.dumps(&json!({"schemaVersion": 2, "manifests": [descriptor]})),
    )?;
    std::os::unix::fs::symlink(layout.join("blobs"), view.path().join("blobs"))?;
    // Authority was checked before. This local copy validates blob hashes and
    // preserves the exact selected manifest; no registry is contacted.
    let source = format!("oci:{}:target", view.path().display());
    let storage = format!("containers-storage:{destination}");
    host.run(
        &[
            "skopeo",
            "--insecure-policy",
            "copy",
            "--preserve-digests",
            &source,
            &storage,
        ],
        seconds(3600),
    )?;
    Ok(())
}

/// A development image carries a dev flavour, OpenSSH and its kernel
/// modules, and its boot layout validates, without changing the image.
fn check_development_image(host: &dyn Host, image_id: &str) -> Result<()> {
    host.run(
        &[
            "podman",
            "run",
            "--rm",
            "--pull=never",
            "--network=none",
            "--cap-drop=ALL",
            "--entrypoint",
            "/bin/sh",
            image_id,
            "-ec",
            "test \"$(cat /usr/lib/tilo/image-flavour)\" = dev; \
             test -x /usr/sbin/sshd; test -f /usr/lib/systemd/system/sshd.service; \
             test -d /usr/lib/modules",
        ],
        seconds(120),
    )?;
    let mounted = PathBuf::from(host.run(&["podman", "image", "mount", image_id], seconds(120))?);
    let checked = check_development_mount(host, &mounted);
    let unmounted = host.run(&["podman", "image", "unmount", image_id], seconds(120));
    // As `finally`: a failed unmount replaces the check's own outcome.
    unmounted?;
    checked
}

fn check_development_mount(host: &dyn Host, mounted: &Path) -> Result<()> {
    require(
        mounted.is_absolute() && mounted.is_dir(),
        "development image mount is unsafe",
    )?;
    if !mounted.join("boot/tilo-target/tilo-target.efi").is_file() {
        let image_root = mounted.to_string_lossy().into_owned();
        host.run(
            &[
                "/usr/libexec/tilo-installer-offline-target",
                "dev-target-uki",
                "validate-image",
                "--image-root",
                &image_root,
            ],
            seconds(120),
        )?;
    }
    Ok(())
}

const USAGE: &str = "usage: tilo-installer-offline-target [-h] {verify,import,receipt}\n";
const HELP: &str = "
Authenticate and import the separately signed, offline installer target.

The live medium is transport, not the installed image.  `verify` runs before
any disk mutation; `import` rechecks the manifest after Podman loads it;
`receipt` prints the verified update origin and whether the target is an
unsigned development image. `dev-target-uki validate-image|promote` projects
a CI development target into its unsigned target UKI, in the development
installer VM only.

positional arguments:
  {verify,import,receipt}

options:
  -h, --help       show this help message and exit
";

/// The command line: `verify` prints the verified facts, `import` the image
/// ID, `receipt` the backend's facts, `dev-target-uki` is
/// [`crate::dev_target_uki::main`]; a refusal exits 1, a usage error 2.
pub fn main(host: &dyn Host, args: &[OsString]) -> ExitCode {
    if let Some(("dev-target-uki", rest)) = args
        .split_first()
        .map(|(first, rest)| (first.to_str().unwrap_or_default(), rest))
    {
        return crate::dev_target_uki::main(&crate::dev_target_uki::System, rest);
    }
    let words: Vec<String> = args
        .iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    let usage = |message: &str| {
        eprintln!("{USAGE}tilo-installer-offline-target: error: {message}");
        ExitCode::from(2)
    };
    if words.iter().any(|word| {
        word == "-h"
            || (word.starts_with("--") && word.len() > 2 && "--help".starts_with(word.as_str()))
    }) {
        print!("{USAGE}{HELP}");
        return ExitCode::SUCCESS;
    }
    let mut positionals = Vec::new();
    let mut unknown = Vec::new();
    let mut options_done = false;
    for word in &words {
        if !options_done && word == "--" {
            options_done = true;
        } else if !options_done && word.starts_with('-') && word != "-" {
            unknown.push(word.clone());
        } else {
            positionals.push(word.clone());
        }
    }
    let Some(command) = positionals.first() else {
        return usage("the following arguments are required: command");
    };
    if !["verify", "import", "receipt"].contains(&command.as_str()) {
        return usage(&format!(
            "argument command: invalid choice: '{command}' (choose from verify, import, receipt)"
        ));
    }
    unknown.extend(positionals.iter().skip(1).cloned());
    if !unknown.is_empty() {
        return usage(&format!("unrecognized arguments: {}", unknown.join(" ")));
    }
    let outcome = match command.as_str() {
        "verify" => verify(host).map(|result| Dumps::SORTED.dumps(&result)),
        "receipt" => receipt_facts(host),
        _ => import_image(host),
    };
    match outcome {
        Ok(line) => {
            println!("{line}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("offline installer target: {error}");
            ExitCode::FAILURE
        }
    }
}
