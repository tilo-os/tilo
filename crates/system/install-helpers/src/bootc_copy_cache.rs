//! `tilo-vm-bootc-copy-cache check|record`: the opt-in VM cache for the
//! expensive bootc-to-Podman export.
//!
//! The installed image always takes the normal export path unless a
//! root-owned runtime marker exists *and* systemd identifies a virtual
//! machine. The receipt is only a retry acceleration for one booted image and
//! one exact Podman image; it is not an alternative source of installation
//! trust.
//!
//! `check` exits 0 when the receipt names exactly the booted image and the
//! cached copy, 1 otherwise; `record` writes the receipt (exit 1 with a
//! message when it cannot); anything else is a usage error (exit 2).

use std::ffi::OsString;
use std::fs::{self, DirBuilder};
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use serde_json::{Map, Value, json};

use crate::py::Dumps;
use crate::target_transport::{is_digest, is_hex};
use crate::{Error, Result, require};

const IMAGE: &str = "localhost/bootc";
const TIMEOUT: Duration = Duration::from_secs(30);

/// The system the cache reads. The provided methods are the production
/// behaviour; tests replace them.
pub trait Host {
    /// The runtime opt-in marker.
    fn marker(&self) -> PathBuf {
        PathBuf::from("/run/tilo-installer/vm-bootc-copy-cache.enabled")
    }

    fn receipt_dir(&self) -> PathBuf {
        PathBuf::from("/var/lib/tilo-installer/vm-copy-cache")
    }

    /// The kernel command line.
    fn cmdline(&self) -> PathBuf {
        PathBuf::from("/proc/cmdline")
    }

    /// The effective user.
    fn euid(&self) -> u32 {
        rustix::process::geteuid().as_raw()
    }

    /// A regular, root-owned file nobody else may read or write.
    fn owned_private_file(&self, path: &Path) -> bool {
        fs::symlink_metadata(path)
            .is_ok_and(|info| info.file_type().is_file() && info.uid() == 0 && private(info.mode()))
    }

    /// A tool's standard output (30 s timeout, nonzero exit refused).
    ///
    /// # Errors
    /// The tool failing, missing or timing out.
    fn run(&self, argv: &[&str]) -> Result<Vec<u8>> {
        crate::process::checked(argv, TIMEOUT)
    }
}

/// The installer VM as it is.
#[derive(Debug, Clone, Copy, Default)]
pub struct System;

impl Host for System {}

/// No permission bit for the group or others (`mode & 0o077 == 0`).
fn private(mode: u32) -> bool {
    mode.trailing_zeros() >= 6
}

fn receipt_path(host: &dyn Host) -> PathBuf {
    host.receipt_dir().join("vm-bootc-copy-cache-v1.json")
}

/// Root, the private marker and a VM.
pub fn enabled(host: &dyn Host) -> bool {
    host.euid() == 0
        && host.owned_private_file(&host.marker())
        && host
            .run(&["systemd-detect-virt", "--vm", "--quiet"])
            .is_ok()
}

fn field<'a>(value: &'a Value, key: &str) -> Result<&'a Value> {
    match value {
        Value::Object(map) => map.get(key).ok_or_else(|| Error::new(format!("'{key}'"))),
        Value::Array(_) => Err(Error::new(
            "list indices must be integers or slices, not str",
        )),
        _ => Err(Error::new(format!(
            "'{key}' cannot be read from this value"
        ))),
    }
}

fn json(bytes: &[u8]) -> Result<Value> {
    serde_json::from_slice(bytes).map_err(|error| Error::new(error.to_string()))
}

fn non_empty_str(value: &Value) -> Option<&str> {
    value.as_str().filter(|text| !text.is_empty())
}

/// What the booted image is: its reference, manifest digest, composefs
/// digest and ostree checksum.
///
/// # Errors
/// `bootc status` failing, or an identity missing or malformed.
pub fn booted_identity(host: &dyn Host) -> Result<Value> {
    let status = json(&host.run(&["bootc", "status", "--json"])?)?;
    let booted = field(field(&status, "status")?, "booted")?;
    let image = field(booted, "image")?;
    let digest = field(image, "imageDigest")?;
    let Some(digest) = digest.as_str() else {
        return Err(Error::new("expected string or bytes-like object"));
    };
    require(
        is_digest(digest),
        "booted image has no SHA-256 manifest digest",
    )?;
    let reference = field(image, "image")?;
    let name = field(reference, "image")?;
    let transport = field(reference, "transport")?;
    let (Some(name), Some(transport)) = (non_empty_str(name), non_empty_str(transport)) else {
        return Err(Error::new("booted image reference is missing"));
    };
    let cmdline = fs::read(host.cmdline())?;
    let cmdline = String::from_utf8(cmdline).map_err(|error| Error::new(error.to_string()))?;
    let composefs: Vec<&str> = crate::py::split(&cmdline)
        .into_iter()
        .filter_map(|token| token.strip_prefix("composefs="))
        .collect();
    let [composefs] = composefs.as_slice() else {
        return Err(Error::new(
            "booted composefs identity is missing or ambiguous",
        ));
    };
    require(
        is_hex(composefs, 128),
        "booted composefs identity is missing or ambiguous",
    )?;
    // `(booted.get('ostree') or {}).get('checksum')`
    let ostree = match booted {
        Value::Object(map) => map.get("ostree").cloned().unwrap_or(Value::Null),
        _ => return Err(Error::new("booted status is not an object")),
    };
    let checksum = if crate::py::truthy(&ostree) {
        match &ostree {
            Value::Object(map) => map.get("checksum").cloned().unwrap_or(Value::Null),
            _ => return Err(Error::new("'ostree' has no attribute 'get'")),
        }
    } else {
        Value::Null
    };
    Ok(json!({
        "image": name,
        "transport": transport,
        "image_digest": digest,
        "composefs_digest": composefs,
        "ostree_checksum": checksum,
    }))
}

/// What the cached Podman copy is: its image ID and the hash of its raw
/// manifest, which binds the layers.
///
/// # Errors
/// A tool failing, or an identity missing or malformed.
pub fn cached_image_identity(host: &dyn Host) -> Result<Value> {
    let details = json(&host.run(&["podman", "image", "inspect", IMAGE])?)?;
    let [detail] = details.as_array().map(Vec::as_slice).unwrap_or_default() else {
        return Err(Error::new(
            "Podman image inspect returned an unexpected image count",
        ));
    };
    let image_id = field(detail, "Id")?;
    // Podman returns a bare 64-hex config ID on some builds. It is still an
    // immutable image reference; the raw manifest hash below binds the layers.
    let Some(image_id) = image_id.as_str() else {
        return Err(Error::new("expected string or bytes-like object"));
    };
    require(
        is_hex(image_id.strip_prefix("sha256:").unwrap_or(image_id), 64),
        "cached image has no SHA-256 ID",
    )?;
    // A config ID by itself does not commit to the image's layer list. The raw
    // OCI manifest does, and is cheap to read without exporting layer bytes.
    let containers_storage = format!("containers-storage:{IMAGE}");
    let raw = host.run(&["skopeo", "inspect", "--raw", &containers_storage])?;
    let manifest = json(&raw)?;
    let Value::Object(manifest) = manifest else {
        return Err(Error::new("cached image manifest is not an object"));
    };
    require(
        manifest
            .get("layers")
            .and_then(Value::as_array)
            .is_some_and(|layers| !layers.is_empty()),
        "cached image has no OCI layers",
    )?;
    Ok(json!({
        "image_id": image_id,
        "manifest_sha256": crate::py::sha256_hex(&raw),
    }))
}

/// The receipt for the image booted now and the copy cached now.
///
/// # Errors
/// As [`booted_identity`] and [`cached_image_identity`].
pub fn current_receipt(host: &dyn Host) -> Result<Value> {
    let mut receipt = Map::new();
    receipt.insert("version".into(), json!(1));
    receipt.insert("booted".into(), booted_identity(host)?);
    receipt.insert("cached".into(), cached_image_identity(host)?);
    Ok(Value::Object(receipt))
}

/// Whether the recorded receipt names exactly what is booted and cached.
pub fn can_reuse(host: &dyn Host) -> bool {
    let receipt = receipt_path(host);
    if !enabled(host) || !host.owned_private_file(&receipt) {
        return false;
    }
    let Ok(text) = fs::read(&receipt) else {
        return false;
    };
    let Ok(recorded) = json(&text) else {
        return false;
    };
    current_receipt(host).is_ok_and(|current| crate::py::equal(&recorded, &current))
}

/// Records the receipt for what is booted and cached now.
///
/// # Errors
/// The opt-in absent, an identity unavailable, an unsafe receipt directory
/// or receipt, or a write failing.
pub fn record(host: &dyn Host) -> Result<()> {
    require(enabled(host), "VM cache opt-in is absent")?;
    let receipt = current_receipt(host)?;
    let directory = host.receipt_dir();
    if let Err(error) = DirBuilder::new().mode(0o700).create(&directory)
        && error.kind() != std::io::ErrorKind::AlreadyExists
    {
        return Err(error.into());
    }
    let info = fs::symlink_metadata(&directory)?;
    require(
        info.file_type().is_dir() && info.uid() == 0 && private(info.mode()),
        "receipt directory is not private and root-owned",
    )?;
    let path = receipt_path(host);
    require(
        !path.exists() || host.owned_private_file(&path),
        "existing receipt is not a private regular root-owned file",
    )?;
    let mut temporary = tempfile::Builder::new()
        .prefix(".vm-bootc-copy-")
        .tempfile_in(&directory)?;
    temporary.write_all(Dumps::SORTED.dumps(&receipt).as_bytes())?;
    temporary.write_all(b"\n")?;
    temporary.flush()?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(&path)
        .map_err(|error| Error::from(error.error))?;
    fs::File::open(&directory)?.sync_all()?;
    Ok(())
}

/// The command line: `check` or `record`.
pub fn main(host: &dyn Host, args: &[OsString]) -> ExitCode {
    let command = match args {
        [command] => command.to_str(),
        _ => None,
    };
    match command {
        Some("check") => {
            if can_reuse(host) {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Some("record") => match record(host) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("VM copy cache receipt unavailable: {error}");
                ExitCode::FAILURE
            }
        },
        _ => {
            eprintln!("usage: tilo-vm-bootc-copy-cache check|record");
            ExitCode::from(2)
        }
    }
}
