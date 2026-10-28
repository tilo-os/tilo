//! The transport checks of one immutable CI target staged in a live
//! installer's writable `/var`: integrity, not release authority.
//!
//! The normal transport still requires the running installer to verify
//! signatures and SLSA provenance. A development transport (`development`) is
//! a separate unsigned dev-image schema with content-bound source and flavour
//! labels; CI run authority is a separate host check.
//!
//! The host stages a transport with `os/dev/installer-target-transport.py`,
//! which applies these same checks; `tilo-installer-offline-target` applies
//! them to a development target before it trusts one.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::{Error, Result, require};

const IMAGE: [&str; 2] = [
    "application/vnd.oci.image.manifest.v1+json",
    "application/vnd.docker.distribution.manifest.v2+json",
];
const MAX_BYTES: u64 = 50 * 1024 * 1024 * 1024;
const MAX_BLOBS: usize = 500;

/// `[0-9a-f]{length}`.
#[must_use]
pub fn is_hex(text: &str, length: usize) -> bool {
    text.len() == length && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// `sha256:[0-9a-f]{64}`.
#[must_use]
pub fn is_digest(text: &str) -> bool {
    text.strip_prefix("sha256:")
        .is_some_and(|hex| is_hex(hex, 64))
}

fn names(directory: &Path) -> Result<BTreeSet<String>> {
    fs::read_dir(directory)?
        .map(|entry| Ok(entry?.file_name().to_string_lossy().into_owned()))
        .collect()
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|item| (*item).to_owned()).collect()
}

/// Parses JSON as `json.loads` did, refusing what it refused.
///
/// # Errors
/// Invalid JSON.
pub fn loads(bytes: &[u8]) -> Result<Value> {
    serde_json::from_slice(bytes).map_err(|error| Error::new(error.to_string()))
}

fn bounded_json(path: &Path, limit: u64) -> Result<Value> {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    require(
        path.is_file()
            && !path.is_symlink()
            && fs::metadata(path).is_ok_and(|info| info.len() > 0 && info.len() <= limit),
        &format!("missing or oversized {name}"),
    )?;
    loads(&fs::read(path)?)
}

fn blob_path(layout: &Path, digest: Option<&Value>) -> Result<PathBuf> {
    let digest = digest
        .and_then(Value::as_str)
        .filter(|digest| is_digest(digest));
    let Some(digest) = digest else {
        return Err(Error::new("invalid OCI blob digest"));
    };
    Ok(layout
        .join("blobs")
        .join("sha256")
        .join(digest.strip_prefix("sha256:").unwrap_or(digest)))
}

fn object<'a>(value: &'a Value, what: &str) -> Result<&'a Map<String, Value>> {
    value
        .as_object()
        .ok_or_else(|| Error::new(format!("{what} is not a JSON object")))
}

fn get<'a>(map: &'a Map<String, Value>, key: &str) -> &'a Value {
    map.get(key).unwrap_or(&Value::Null)
}

fn index<'a>(map: &'a Map<String, Value>, key: &str) -> Result<&'a Value> {
    map.get(key).ok_or_else(|| Error::new(format!("'{key}'")))
}

fn is_media_type(value: &Value) -> bool {
    value.as_str().is_some_and(|media| IMAGE.contains(&media))
}

/// `[1-9][0-9]*`.
fn is_positive_decimal(text: &str) -> bool {
    let mut bytes = text.bytes();
    bytes
        .next()
        .is_some_and(|first| matches!(first, b'1'..=b'9'))
        && bytes.all(|b| b.is_ascii_digit())
}

/// `[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}`: a registry tag.
fn is_tag(text: &str) -> bool {
    let mut bytes = text.bytes();
    text.len() <= 128
        && bytes
            .next()
            .is_some_and(|first| first.is_ascii_alphanumeric() || first == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

/// Whether `origin` is a tag the selected target may follow.
fn supported_origin(origin: &str, development: bool, profile: &str) -> bool {
    if development {
        origin
            .strip_prefix("registry.tilo.so/tilo/tilo-vm:")
            .is_some_and(is_tag)
    } else if profile == "lenovo-83ed" {
        origin
            .strip_prefix("code.tilo.so/tilo/tilo:lenovo-83ed-")
            .is_some_and(|revision| is_hex(revision, 40))
    } else {
        origin
            .strip_prefix("registry.tilo.so/tilo/tilo:gen")
            .is_some_and(is_positive_decimal)
    }
}

#[must_use]
pub fn repository(profile: &str) -> Option<&'static str> {
    match profile {
        "generic" => Some("registry.tilo.so/tilo/tilo"),
        // Board images carry private firmware and stay on the forge's
        // private registry.
        "lenovo-83ed" => Some("code.tilo.so/tilo/tilo"),
        _ => None,
    }
}

/// `(repository)@(sha256:…)` of a target reference this transport accepts.
fn split_reference(reference: &str, development: bool) -> Option<(&str, &str)> {
    let (repository, digest) = reference.split_once('@')?;
    let known = if development {
        repository == "registry.tilo.so/tilo/tilo-vm"
    } else {
        matches!(
            repository,
            "registry.tilo.so/tilo/tilo" | "code.tilo.so/tilo/tilo"
        )
    };
    (known && is_digest(digest)).then_some((repository, digest))
}

fn check_metadata(metadata: &Map<String, Value>, development: bool) -> Result<()> {
    let text = |key: &str| get(metadata, key).as_str().unwrap_or_default();
    let profile = text("hardware_profile");
    let expected_schema = if development {
        "tilo.installer-dev-target.v1"
    } else {
        "tilo.installer-offline-target.v1"
    };
    let reference = split_reference(text("target_ref"), development);
    let pinned = reference.is_some_and(|(repository_name, digest)| {
        text("target_digest") == digest
            && text("architecture") == "aarch64"
            && matches!(profile, "generic" | "lenovo-83ed")
            && Some(repository_name)
                == if development {
                    Some("registry.tilo.so/tilo/tilo-vm")
                } else {
                    repository(profile)
                }
            && supported_origin(text("update_origin"), development, profile)
    });
    require(
        text("schema") == expected_schema && pinned,
        "target metadata does not pin a supported CI ARM64 child",
    )?;
    if development {
        require(
            is_hex(text("source_revision"), 40) && is_positive_decimal(text("ci_run_id")),
            "development target source or CI run identity is invalid",
        )?;
    }
    Ok(())
}

/// Checks the transport in `directory` and returns its metadata.
///
/// # Errors
/// Anything that differs from one bounded, hash-exact OCI layout of the
/// pinned target (and, for a development transport, its labels).
pub fn verify(directory: &Path, development: bool) -> Result<Map<String, Value>> {
    require(
        directory.is_dir() && !directory.is_symlink(),
        "target transport is not a directory",
    )?;
    require(
        names(directory)? == set(&["oci", "target.json"]),
        "target transport has unexpected root entries",
    )?;
    let metadata = read_metadata(&directory.join("target.json"), development)?;
    let layout = directory.join("oci");
    require(
        layout.is_dir()
            && !layout.is_symlink()
            && names(&layout)? == set(&["oci-layout", "index.json", "blobs"]),
        "OCI layout entries differ",
    )?;
    require(
        crate::py::equal(
            &bounded_json(&layout.join("oci-layout"), 256)?,
            &serde_json::json!({"imageLayoutVersion": "1.0.0"}),
        ),
        "unsupported OCI layout",
    )?;
    let index_json = bounded_json(&layout.join("index.json"), 1024 * 1024)?;
    let (manifests, target_at) = index_manifests(&index_json, &metadata, development)?;
    let mut refs: Vec<Value> = Vec::new();
    for (at, descriptor) in manifests.iter().enumerate() {
        let target = (at == target_at).then_some(&metadata);
        refs.extend(manifest_refs(&layout, descriptor, target, development)?);
    }
    require(refs.len() <= MAX_BLOBS, "too many OCI blobs")?;
    check_blobs(&layout, &refs)?;
    Ok(metadata)
}

/// `target.json`: exactly the transport's fields, all strings, pinning a
/// supported child.
fn read_metadata(path: &Path, development: bool) -> Result<Map<String, Value>> {
    let metadata = bounded_json(path, 16384)?;
    let mut fields = vec![
        "schema",
        "target_ref",
        "target_digest",
        "architecture",
        "hardware_profile",
        "update_origin",
    ];
    if development {
        fields.extend(["source_revision", "ci_run_id"]);
    }
    let metadata = match metadata {
        Value::Object(map)
            if map.keys().map(String::as_str).collect::<BTreeSet<_>>()
                == fields.iter().copied().collect()
                && map.values().all(Value::is_string) =>
        {
            map
        }
        _ => return Err(Error::new("target metadata fields differ")),
    };
    check_metadata(&metadata, development)?;
    Ok(metadata)
}

/// The index's bounded manifest list and the position of the one pinned
/// target in it.
fn index_manifests<'a>(
    index_json: &'a Value,
    metadata: &Map<String, Value>,
    development: bool,
) -> Result<(&'a Vec<Value>, usize)> {
    let index_map = object(index_json, "OCI index")?;
    let manifests = match index_map.get("manifests") {
        Some(Value::Array(manifests))
            if crate::py::equal(get(index_map, "schemaVersion"), &serde_json::json!(2))
                && (1..=16).contains(&manifests.len()) =>
        {
            manifests
        }
        _ => {
            return Err(Error::new(
                "OCI layout must name bounded image and signature manifests",
            ));
        }
    };
    let target_digest = get(metadata, "target_digest");
    let mut target = Vec::new();
    for (at, row) in manifests.iter().enumerate() {
        if crate::py::equal(get(object(row, "OCI descriptor")?, "digest"), target_digest) {
            target.push(at);
        }
    }
    let [at] = target.as_slice() else {
        return Err(Error::new(
            "OCI index does not uniquely name the pinned target",
        ));
    };
    let row = manifests.get(*at).and_then(Value::as_object);
    let annotations = row
        .and_then(|row| row.get("annotations"))
        .and_then(Value::as_object);
    let named = annotations.is_some_and(|annotations| {
        if development {
            annotations.get("org.opencontainers.image.ref.name")
                == Some(&Value::String("target".into()))
        } else {
            get(annotations, "kind").as_str() == Some("dev.cosignproject.cosign/image")
        }
    });
    let media = row.is_some_and(|row| is_media_type(get(row, "mediaType")));
    require(
        media && named,
        "OCI index does not uniquely name the pinned target",
    )?;
    if development {
        require(
            manifests.len() == 1,
            "development transport must contain only the selected image",
        )?;
    }
    Ok((manifests, *at))
}

/// The blobs one index descriptor references: itself, its manifest's config
/// and layers. For the target (`target` holds its metadata) the config must
/// be Linux ARM64 and, for a development target, carry its source and
/// flavour labels.
fn manifest_refs(
    layout: &Path,
    descriptor: &Value,
    target: Option<&Map<String, Value>>,
    development: bool,
) -> Result<Vec<Value>> {
    let descriptor_map = object(descriptor, "OCI descriptor")?;
    require(
        is_media_type(get(descriptor_map, "mediaType")),
        "OCI index has an unsupported descriptor",
    )?;
    let manifest_value = bounded_json(
        &blob_path(layout, Some(index(descriptor_map, "digest")?))?,
        1024 * 1024,
    )?;
    let manifest = object(&manifest_value, "OCI manifest")?;
    let (Some(Value::Object(config)), Some(Value::Array(layers))) =
        (manifest.get("config"), manifest.get("layers"))
    else {
        return Err(Error::new("OCI image manifest is invalid"));
    };
    require(
        crate::py::equal(get(manifest, "schemaVersion"), &serde_json::json!(2))
            && is_media_type(get(manifest, "mediaType")),
        "OCI image manifest is invalid",
    )?;
    if let Some(metadata) = target {
        require(!layers.is_empty(), "target image has no layers")?;
        let image_config_value =
            bounded_json(&blob_path(layout, config.get("digest"))?, 1024 * 1024)?;
        let image_config = object(&image_config_value, "OCI config")?;
        require(
            get(image_config, "architecture").as_str() == Some("arm64")
                && get(image_config, "os").as_str() == Some("linux"),
            "target OCI config is not Linux ARM64",
        )?;
        if development {
            let empty = Map::new();
            let inner = match image_config.get("config") {
                None => &empty,
                Some(value) => object(value, "OCI config.config")?,
            };
            let labels = match inner.get("Labels") {
                None => &empty,
                Some(value) => object(value, "OCI labels")?,
            };
            require(
                get(labels, "org.opencontainers.image.revision")
                    == get(metadata, "source_revision")
                    && get(labels, "sh.tilo.image.flavour").as_str() == Some("dev"),
                "development target source or flavour label differs",
            )?;
        }
    }
    let mut refs = vec![descriptor.clone(), Value::Object(config.clone())];
    refs.extend(layers.iter().cloned());
    Ok(refs)
}

/// Every referenced blob is present with its exact size and hash, within the
/// size limit, and nothing else is in the blob tree.
fn check_blobs(layout: &Path, refs: &[Value]) -> Result<()> {
    let mut expected = BTreeSet::new();
    let mut total: u64 = 0;
    for row in refs {
        let row = object(row, "OCI descriptor")?;
        let path = blob_path(layout, row.get("digest"))?;
        let size = get(row, "size");
        let size = if size.is_u64() || size.is_i64() {
            size.as_u64().filter(|size| *size > 0)
        } else {
            None
        };
        require(
            path.is_file()
                && !path.is_symlink()
                && size
                    .is_some_and(|size| fs::metadata(&path).is_ok_and(|info| info.len() == size)),
            "OCI blob size differs",
        )?;
        total = total.saturating_add(size.unwrap_or_default());
        require(total <= MAX_BYTES, "offline target exceeds the size limit")?;
        let actual = file_sha256(&path)?;
        require(
            get(row, "digest").as_str() == Some(format!("sha256:{actual}").as_str()),
            "OCI blob digest differs",
        )?;
        expected.insert(
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
        );
    }
    let blobs = layout.join("blobs");
    require(
        blobs.is_dir() && !blobs.is_symlink() && names(&blobs)? == set(&["sha256"]),
        "OCI blob tree differs",
    )?;
    let digest_dir = blobs.join("sha256");
    require(
        digest_dir.is_dir() && !digest_dir.is_symlink(),
        "OCI digest directory is unsafe",
    )?;
    require(
        names(&digest_dir)? == expected,
        "OCI blob tree has missing or extra content",
    )
}

/// The lowercase hex SHA-256 of the file at `path`, streamed.
///
/// # Errors
/// The file unreadable.
pub fn file_sha256(path: &Path) -> Result<String> {
    use sha2::Digest as _;
    use std::io::Read as _;
    let mut file = fs::File::open(path)?;
    let mut digest = sha2::Sha256::new();
    let mut chunk = vec![0u8; 1024 * 1024];
    loop {
        let count = file.read(&mut chunk)?;
        if count == 0 {
            break;
        }
        digest.update(chunk.get(..count).unwrap_or_default());
    }
    Ok(hex::encode(digest.finalize()))
}
