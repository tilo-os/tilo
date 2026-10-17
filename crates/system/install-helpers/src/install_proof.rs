//! `tilo-vm-install-proof --target-root … --output OUTPUT`: binds a completed
//! VM install to its booted medium and installed deployment.
//!
//! The caller writes the result to /run before closing the encrypted target
//! and emits it only after the install backend finishes successfully. The
//! record is guest evidence, not a signature or a substitute for host
//! readback. Its schema is printed on stdout.

use std::fs;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Map, Value, json};

use crate::args::{Arg, Command, Values};
use crate::ini::Ini;
use crate::py::Dumps;
use crate::target_transport::{is_digest, is_hex};
use crate::{Error, Result, require};

/// The command line.
pub const COMMAND: Command = Command {
    prog: "tilo-vm-install-proof",
    usage: "usage: tilo-vm-install-proof [-h] --target-root TARGET_ROOT --deploy-etc DEPLOY_ETC
                             --efi-root EFI_ROOT --target-uki TARGET_UKI --disk
                             DISK --efi-partition EFI_PARTITION
                             --boot-partition BOOT_PARTITION --root-partition
                             ROOT_PARTITION --output OUTPUT
                             [--offline-target-receipt OFFLINE_TARGET_RECEIPT]
",
    help: "
Bind a completed VM install to its booted medium and installed deployment.

The caller writes the result to /run before closing the encrypted target and
emits it only after the install backend finishes successfully. The record is
guest evidence, not a signature or a substitute for host readback.

options:
  -h, --help            show this help message and exit
  --target-root TARGET_ROOT
  --deploy-etc DEPLOY_ETC
  --efi-root EFI_ROOT
  --target-uki TARGET_UKI
  --disk DISK
  --efi-partition EFI_PARTITION
  --boot-partition BOOT_PARTITION
  --root-partition ROOT_PARTITION
  --output OUTPUT
  --offline-target-receipt OFFLINE_TARGET_RECEIPT
",
    args: &[
        Arg::Option {
            flag: "--target-root",
            required: true,
        },
        Arg::Option {
            flag: "--deploy-etc",
            required: true,
        },
        Arg::Option {
            flag: "--efi-root",
            required: true,
        },
        Arg::Option {
            flag: "--target-uki",
            required: true,
        },
        Arg::Option {
            flag: "--disk",
            required: true,
        },
        Arg::Option {
            flag: "--efi-partition",
            required: true,
        },
        Arg::Option {
            flag: "--boot-partition",
            required: true,
        },
        Arg::Option {
            flag: "--root-partition",
            required: true,
        },
        Arg::Option {
            flag: "--output",
            required: true,
        },
        Arg::Option {
            flag: "--offline-target-receipt",
            required: false,
        },
    ],
};

const TIMEOUT: Duration = Duration::from_secs(30);
const OFFLINE_RECEIPT: &str = "/run/tilo-installer/offline-target.json";
const DEV_SCHEMA: &str = "tilo.installer-dev-target.v1";
const OFFLINE_SCHEMA: &str = "tilo.installer-offline-target.v1";

/// What the proof binds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub target_root: PathBuf,
    pub deploy_etc: PathBuf,
    pub efi_root: PathBuf,
    pub target_uki: PathBuf,
    pub disk: PathBuf,
    pub efi_partition: PathBuf,
    pub boot_partition: PathBuf,
    pub root_partition: PathBuf,
    pub offline_target_receipt: PathBuf,
}

/// The installer VM the proof reads. The provided methods are the production
/// behaviour; tests replace them.
pub trait Host {
    /// A tool's stripped standard output (30 s timeout, nonzero exit refused).
    ///
    /// # Errors
    /// The tool failing, missing or timing out.
    fn command(&self, argv: &[&str]) -> Result<String> {
        crate::process::checked_text(argv, TIMEOUT)
    }

    /// The marker only the VM-only development installer carries.
    fn dev_vm_marker(&self) -> PathBuf {
        PathBuf::from("/usr/share/tilo/unverified-vm-install")
    }

    /// Whether the proof's dedicated virtio port is present.
    fn proof_port(&self) -> bool {
        fs::metadata("/dev/virtio-ports/org.tilo.vm-install-proof")
            .is_ok_and(|info| info.file_type().is_char_device())
    }

    /// Where an installer code refresh leaves its evidence.
    fn code_receipt(&self) -> PathBuf {
        PathBuf::from("/run/tilo-installer/code-refresh/active.json")
    }

    /// The manifest digest of the VM-only bootc export in local storage.
    ///
    /// # Errors
    /// Outside the VM-only installer, or no local manifest.
    fn copied_dev_manifest_digest(&self) -> Result<String> {
        let marker = self.dev_vm_marker();
        require(
            marker.is_file() && !marker.is_symlink() && self.proof_port(),
            "development copy proof requires the VM-only installer and proof port",
        )?;
        self.command(&["systemd-detect-virt", "--vm", "--quiet"])?;
        let raw = crate::process::checked(
            &[
                "skopeo",
                "inspect",
                "--raw",
                "containers-storage:localhost/bootc",
            ],
            TIMEOUT,
        )?;
        require(
            !raw.is_empty(),
            "development copy has no local OCI manifest",
        )?;
        Ok(format!("sha256:{}", crate::py::sha256_hex(&raw)))
    }
}

/// The installer VM as it is.
#[derive(Debug, Clone, Copy, Default)]
pub struct System;

impl Host for System {}

fn is_composefs(text: &str) -> bool {
    is_hex(text, 128)
}

/// `[0-9a-fA-F]{8}-…-[0-9a-fA-F]{12}`.
fn is_guid(text: &str) -> bool {
    let groups: Vec<&str> = text.split('-').collect();
    groups.len() == 5
        && groups.iter().zip([8, 4, 4, 4, 12]).all(|(group, length)| {
            group.len() == length && group.bytes().all(|b| b.is_ascii_hexdigit())
        })
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn field<'a>(value: &'a Value, key: &str) -> Result<&'a Value> {
    value
        .as_object()
        .and_then(|map| map.get(key))
        .ok_or_else(|| Error::new(format!("'{key}'")))
}

fn str_field<'a>(value: &'a Value, key: &str) -> Result<&'a str> {
    field(value, key)?
        .as_str()
        .ok_or_else(|| Error::new("expected string or bytes-like object"))
}

/// `dict.get(key)`: a missing key and JSON null both read as `None`.
fn get<'a>(map: &'a Map<String, Value>, key: &str) -> &'a Value {
    map.get(key).unwrap_or(&Value::Null)
}

fn composefs_from_uki(host: &dyn Host, uki: &Path) -> Result<String> {
    let cmdline = host.command(&["/usr/libexec/tilo-uki-cmdline", "show", &text(uki)])?;
    let values: Vec<&str> = crate::py::split(&cmdline)
        .into_iter()
        .filter_map(|word| word.strip_prefix("composefs="))
        .collect();
    match values.as_slice() {
        [value] if is_composefs(value) => Ok((*value).to_owned()),
        _ => Err(Error::new(
            "installed target UKI has no unique composefs digest",
        )),
    }
}

fn hash_file(path: &Path) -> Result<String> {
    crate::target_transport::file_sha256(path)
}

fn bounded_file(path: &Path, limit: u64) -> bool {
    path.is_file() && !path.is_symlink() && fs::metadata(path).is_ok_and(|info| info.len() <= limit)
}

fn present(path: &Path) -> bool {
    path.exists() || path.is_symlink()
}

/// The repository a signed offline target of `profile` comes from.
fn offline_repository(profile: &Value) -> &'static str {
    match profile.as_str() {
        Some("generic") => "registry.tilo.so/tilo/tilo",
        // Board images stay on the forge's private registry.
        Some("lenovo-83ed") => "code.tilo.so/tilo/tilo",
        _ => "",
    }
}

/// The selected target's receipt, checked against the installed deployment.
fn selected_target(
    host: &dyn Host,
    selected: &Map<String, Value>,
    installed_digest: &str,
    installed_origin: &str,
) -> Result<(Map<String, Value>, bool)> {
    let schema = get(selected, "schema");
    let development = schema.as_str() == Some(DEV_SCHEMA);
    let repository = if development {
        "registry.tilo.so/tilo/tilo-vm"
    } else {
        offline_repository(get(selected, "hardware_profile"))
    };
    let same_origin = || -> Result<bool> {
        let origin = match selected.get("update_origin") {
            None => "",
            Some(Value::String(origin)) => origin.as_str(),
            Some(_) => return Err(Error::new("can only concatenate str (not \"dict\") to str")),
        };
        Ok(installed_origin == format!("ostree-unverified-image:docker://{origin}"))
    };
    require(
        matches!(schema.as_str(), Some(OFFLINE_SCHEMA | DEV_SCHEMA))
            && get(selected, "target_digest").as_str() == Some(installed_digest)
            && get(selected, "target_ref").as_str()
                == Some(format!("{repository}@{installed_digest}").as_str())
            && same_origin()?,
        "installed deployment differs from the independently signed offline target",
    )?;
    if !development {
        return Ok((selected.clone(), false));
    }
    let marker = host.dev_vm_marker();
    require(
        marker.is_file()
            && !marker.is_symlink()
            && host.proof_port()
            && get(selected, "verifier_key_sha256").is_null(),
        "development target proof requires the VM-only development installer",
    )?;
    host.command(&["systemd-detect-virt", "--vm", "--quiet"])?;
    let provenance = match selected.get("provenance") {
        None => &Map::new(),
        Some(Value::Object(provenance)) => provenance,
        Some(_) => return Err(Error::new("'provenance' has no attribute 'get'")),
    };
    require(
        crate::py::equal(
            get(provenance, "source_revision"),
            get(selected, "source_revision"),
        ) && crate::py::equal(get(provenance, "invocation_id"), get(selected, "ci_run_id")),
        "development target source differs from its transport receipt",
    )?;
    let kept = selected
        .iter()
        .filter(|(key, _)| !matches!(key.as_str(), "source_revision" | "ci_run_id"))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    Ok((kept, true))
}

/// The installed deployment: its composefs digest, OCI manifest digest and
/// update origin, from the `.origin` file beside its `/etc`.
fn installed_deployment(request: &Request) -> Result<(String, String, String)> {
    let root = fs::canonicalize(&request.target_root)?;
    let deploy_etc = fs::canonicalize(&request.deploy_etc)?;
    let relative = deploy_etc.strip_prefix(&root).map_err(|_| {
        Error::new(format!(
            "'{}' is not in the subpath of '{}'",
            deploy_etc.display(),
            root.display()
        ))
    })?;
    let parts: Vec<String> = relative
        .components()
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect();
    let composefs = match parts.as_slice() {
        [state, deploy, digest, etc]
            if state == "state" && deploy == "deploy" && etc == "etc" && is_composefs(digest) =>
        {
            digest.clone()
        }
        _ => {
            return Err(Error::new(
                "installed deployment has an unexpected /etc path",
            ));
        }
    };
    let origin = deploy_etc
        .parent()
        .unwrap_or(&deploy_etc)
        .join(format!("{composefs}.origin"));
    require(
        origin.is_file() && !origin.is_symlink(),
        "installed deployment origin is missing or unsafe",
    )?;
    let ini = Ini::parse(&fs::read_to_string(&origin)?)?;
    let installed_digest = ini.get("image", "manifest_digest")?;
    require(
        is_digest(&installed_digest),
        "installed deployment has no OCI manifest digest",
    )?;
    let installed_origin = ini.get("origin", "container-image-reference")?;
    Ok((composefs, installed_digest, installed_origin))
}

/// The booted installer image: its manifest digest and composefs digest,
/// refused unless it is a compatible ARM64 image.
fn booted_image(host: &dyn Host) -> Result<(String, String)> {
    let status: Value =
        serde_json::from_str(&host.command(&["bootc", "status", "--format", "json"])?)
            .map_err(|error| Error::new(error.to_string()))?;
    let booted = field(field(&status, "status")?, "booted")?;
    let image = field(booted, "image")?;
    let booted_digest = field(image, "imageDigest")?;
    let architecture = field(image, "architecture")?;
    require(
        architecture.as_str() == Some("arm64"),
        "VM installer did not boot an ARM64 image",
    )?;
    let booted_digest = booted_digest
        .as_str()
        .ok_or_else(|| Error::new("expected string or bytes-like object"))?
        .to_owned();
    require(
        is_digest(&booted_digest),
        "booted image digest is malformed",
    )?;
    require(
        booted.get("incompatible") == Some(&Value::Bool(false)),
        "booted installer image is incompatible",
    )?;
    let booted_composefs = str_field(field(booted, "composefs")?, "verity")?.to_owned();
    Ok((booted_digest, booted_composefs))
}

/// The installed target UKI's hash and the disk and partition GPT
/// identities, after checking the UKI sits on the selected ESP and boots the
/// deployed composefs digest.
fn boot_identities(
    host: &dyn Host,
    request: &Request,
    composefs: &str,
) -> Result<(String, String, Map<String, Value>)> {
    require(
        !request.target_uki.is_symlink(),
        "installed target UKI is a symlink",
    )?;
    let uki = fs::canonicalize(&request.target_uki)?;
    require(
        uki.is_file() && uki.starts_with(fs::canonicalize(&request.efi_root)?),
        "installed target UKI is outside the selected ESP",
    )?;
    require(
        composefs_from_uki(host, &uki)? == composefs,
        "installed target UKI differs from deployed composefs digest",
    )?;
    let disk_guid = host.command(&[
        "blkid",
        "-p",
        "-s",
        "PTUUID",
        "-o",
        "value",
        &text(&request.disk),
    ])?;
    require(is_guid(&disk_guid), "installed disk has no GPT identity")?;
    let mut identities = Map::new();
    for (role, partition) in [
        ("efi", &request.efi_partition),
        ("boot", &request.boot_partition),
        ("root", &request.root_partition),
    ] {
        let partuuid =
            host.command(&["blkid", "-s", "PARTUUID", "-o", "value", &text(partition)])?;
        require(
            is_guid(&partuuid),
            &format!("installed {role} has no GPT identity"),
        )?;
        identities.insert(role.into(), Value::String(partuuid.to_lowercase()));
    }
    let mut distinct: Vec<&Value> = identities.values().collect();
    distinct.sort_by_key(|value| value.as_str());
    distinct.dedup();
    require(
        distinct.len() == 3,
        "installed target partition identities overlap",
    )?;
    Ok((hash_file(&uki)?, disk_guid.to_lowercase(), identities))
}

/// The installer code refresh's evidence, when a refresh ran.
fn code_override(host: &dyn Host) -> Result<Option<String>> {
    let code_receipt = host.code_receipt();
    if !present(&code_receipt) {
        return Ok(None);
    }
    require(
        bounded_file(&code_receipt, 65536),
        "unsafe installer code refresh evidence",
    )?;
    let code: Value = serde_json::from_str(&fs::read_to_string(&code_receipt)?)
        .map_err(|error| Error::new(error.to_string()))?;
    let code = code
        .as_object()
        .ok_or_else(|| Error::new("installer code refresh evidence has no attribute 'get'"))?;
    let sha256 = match code.get("sha256") {
        None => "",
        Some(Value::String(sha256)) => sha256.as_str(),
        Some(_) => return Err(Error::new("expected string or bytes-like object")),
    };
    require(
        get(code, "schema").as_str() == Some("tilo.installer-code-refresh.v1")
            && is_hex(sha256, 64),
        "invalid installer code refresh evidence",
    )?;
    Ok(Some(sha256.to_owned()))
}

/// The proof for `request`.
///
/// # Errors
/// Any identity missing, malformed or differing; a tool failing.
pub fn build_proof(host: &dyn Host, request: &Request) -> Result<Value> {
    let (composefs, installed_digest, installed_origin) = installed_deployment(request)?;
    let (booted_digest, booted_composefs) = booted_image(host)?;
    require(
        is_composefs(&booted_composefs) && is_composefs(&composefs),
        "booted or installed composefs digest is malformed",
    )?;
    let receipt_path = &request.offline_target_receipt;
    let mut selected: Option<(Map<String, Value>, bool)> = None;
    if present(receipt_path) {
        require(
            bounded_file(receipt_path, 8192),
            "offline target verification receipt is unsafe",
        )?;
        let receipt: Value = serde_json::from_str(&fs::read_to_string(receipt_path)?)
            .map_err(|error| Error::new(error.to_string()))?;
        let Value::Object(receipt) = receipt else {
            return Err(Error::new("offline target receipt has no attribute 'get'"));
        };
        selected = Some(selected_target(
            host,
            &receipt,
            &installed_digest,
            &installed_origin,
        )?);
    } else {
        require(
            booted_composefs == composefs,
            "installed deployment does not match the booted CI image",
        )?;
        if booted_digest != installed_digest {
            require(
                host.copied_dev_manifest_digest()? == installed_digest,
                "installed deployment differs from the VM-local bootc copy",
            )?;
        }
    }
    let (uki_sha256, disk_guid, identities) = boot_identities(host, request, &composefs)?;
    let selected = selected.filter(|(map, _)| !map.is_empty());
    let dev_copy = selected.is_none() && booted_digest != installed_digest;
    let schema = match &selected {
        Some((_, true)) => "tilo.vm-install-image-proof.v4",
        Some((_, false)) => "tilo.vm-install-image-proof.v2",
        None if dev_copy => "tilo.vm-install-image-proof.v3",
        None => "tilo.vm-install-image-proof.v1",
    };
    let mut result = Map::new();
    for (key, value) in [
        ("schema", json!(schema)),
        ("architecture", json!("arm64")),
        ("booted_image_digest", json!(booted_digest)),
        ("installed_image_digest", json!(installed_digest)),
        ("composefs_digest", json!(composefs)),
        ("booted_composefs_digest", json!(booted_composefs)),
        ("installed_composefs_digest", json!(composefs)),
        ("installed_update_origin", json!(installed_origin)),
        ("target_uki_sha256", json!(uki_sha256)),
        ("disk_guid", json!(disk_guid)),
        ("partuuids", Value::Object(identities)),
    ] {
        result.insert(key.into(), value);
    }
    if let Some(sha256) = code_override(host)? {
        result.insert("installer_code_override_sha256".into(), json!(sha256));
    }
    if let Some((selected, _)) = selected {
        result.insert("selected_target".into(), Value::Object(selected));
    }
    if dev_copy {
        result.insert("copied_manifest_digest".into(), json!(installed_digest));
    }
    Ok(Value::Object(result))
}

/// Writes `result` to `destination` through a private `.tmp` beside it.
///
/// # Errors
/// A missing or unsafe directory, or a write failing.
pub fn write(destination: &Path, result: &Value) -> Result<()> {
    require(
        destination.parent().is_some_and(|parent| {
            (if parent.as_os_str().is_empty() {
                Path::new(".")
            } else {
                parent
            })
            .is_dir()
        }) && !destination.is_symlink(),
        "proof output directory is missing or unsafe",
    )?;
    let temporary = destination.with_extension("tmp");
    let mut text = Dumps::CANONICAL.dumps(result);
    text.push('\n');
    fs::write(&temporary, text)?;
    fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))?;
    fs::rename(&temporary, destination)?;
    Ok(())
}

/// The command: build the proof, write it and print its schema, which names
/// the proof line the backend emits.
///
/// # Errors
/// As [`build_proof`] and [`write`].
pub fn run(host: &dyn Host, values: &Values) -> Result<()> {
    let path = |flag: &str| values.required(flag).map(PathBuf::from);
    let request = Request {
        target_root: path("--target-root")?,
        deploy_etc: path("--deploy-etc")?,
        efi_root: path("--efi-root")?,
        target_uki: path("--target-uki")?,
        disk: path("--disk")?,
        efi_partition: path("--efi-partition")?,
        boot_partition: path("--boot-partition")?,
        root_partition: path("--root-partition")?,
        offline_target_receipt: values
            .get("--offline-target-receipt")
            .map_or_else(|| PathBuf::from(OFFLINE_RECEIPT), PathBuf::from),
    };
    let result = build_proof(host, &request)?;
    write(&path("--output")?, &result)?;
    println!(
        "{}",
        result
            .get("schema")
            .and_then(Value::as_str)
            .unwrap_or_default()
    );
    Ok(())
}
