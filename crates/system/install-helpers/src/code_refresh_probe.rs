//! `tilo-vm-install-proof code-refresh-probe BACKEND [--cached-target]`:
//! read-only evidence that a refreshed installer backend executed through its
//! real transient, enforcing system service, for the development VM's code
//! refresh loop ([`crate::code_refresh`]).
//!
//! `tilo-install --dev-code-refresh-probe` execs it after its own development
//! VM guards. It prints one JSON row: the `SELinux` domain, the runtime tools
//! the development target UKI may use, the digest of the compiled profile
//! resolver, the backend's own digest, and with `--cached-target` the result
//! of validating an attached original CI boot-input fixture.
//!
//! The fixture (`os/dev/make-installer-probe-fixture.py`) is a read-only
//! virtio disk holding a tar of boot inputs and a `fixture.json` manifest.
//! It is development evidence, never CI, release or physical admission
//! authority.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs;
use std::io::Read;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use serde_json::{Map, Value, json};
use sha2::{Digest as _, Sha256};

use crate::dev_target_uki::{self, Inputs, Selected};
use crate::py::{Dumps, sha256_hex};
use crate::{Error, Result, require};

/// The compiled development target UKI validator and profile resolver.
const RESOLVER: &str = "/usr/libexec/tilo-installer-offline-target";
const UKIFY_CANDIDATES: [&str; 2] = ["/usr/lib/systemd/ukify", "/usr/bin/ukify"];
const MIB: u64 = 1024 * 1024;
const MAX_FIXTURE_FILES: usize = 16;
const MAX_MEMBER_BYTES: u64 = 256 * MIB;
const MAX_TOTAL_BYTES: u64 = 512 * MIB;
const MANIFEST_BYTES: u64 = 16384;
const TIMEOUT: Duration = Duration::from_secs(30);

/// Where the cached-target probe finds its fixture and validator. The
/// provided methods are the production behaviour; tests replace them.
pub trait Fixture {
    /// The read-only fixture disk.
    fn device(&self) -> PathBuf {
        PathBuf::from("/dev/disk/by-id/virtio-tilo-code-fixture")
    }

    /// The kernel's block device directory.
    fn sys_block(&self) -> PathBuf {
        PathBuf::from("/sys/class/block")
    }

    /// Where the fixture is unpacked for the duration of the probe.
    fn scratch(&self) -> PathBuf {
        PathBuf::from("/run")
    }

    /// Whether `path` is a block device.
    fn is_block_device(&self, path: &Path) -> bool {
        fs::metadata(path).is_ok_and(|info| info.file_type().is_block_device())
    }

    /// The size of the block device `path` in bytes.
    ///
    /// # Errors
    /// `blockdev` failing.
    fn size(&self, path: &Path) -> Result<u64> {
        let device = path.to_string_lossy();
        let output = crate::process::run(&["blockdev", "--getsize64", &device], None, TIMEOUT)?;
        require(
            output.success(),
            &format!(
                "boot-input fixture probe: {}",
                String::from_utf8_lossy(&output.stderr)
            ),
        )?;
        String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse()
            .map_err(|_| Error::new("boot-input fixture probe: invalid device size"))
    }

    /// The selected image's boot inputs, as `validate-image` checks them.
    ///
    /// # Errors
    /// See [`dev_target_uki::image_inputs`].
    fn validate(&self, root: &Path) -> Result<Inputs> {
        dev_target_uki::image_inputs(&dev_target_uki::System, root)
    }

    /// The target UKI promotion, as `promote` writes it.
    ///
    /// # Errors
    /// See [`dev_target_uki::promote`].
    fn promote(
        &self,
        image_root: &Path,
        target_root: &Path,
        esp: &Path,
        selected: &Selected,
    ) -> Result<String> {
        dev_target_uki::promote(
            &dev_target_uki::System,
            image_root,
            target_root,
            esp,
            selected,
        )
    }

    /// The validator whose digest the result names.
    fn validator(&self) -> PathBuf {
        PathBuf::from(RESOLVER)
    }
}

/// The development VM as it is.
#[derive(Debug, Clone, Copy, Default)]
pub struct System;

impl Fixture for System {}

fn read(path: &Path) -> Result<Vec<u8>> {
    fs::read(path).map_err(|error| Error::new(format!("{}: {error}", path.display())))
}

fn le_u16(data: &[u8], at: usize) -> Option<usize> {
    data.get(at..at.checked_add(2)?)
        .and_then(|b| b.try_into().ok())
        .map(|b| usize::from(u16::from_le_bytes(b)))
}

fn le_u32(data: &[u8], at: usize) -> Option<usize> {
    data.get(at..at.checked_add(4)?)
        .and_then(|b| b.try_into().ok())
        .and_then(|b| usize::try_from(u32::from_le_bytes(b)).ok())
}

/// A PE image with its one `.cmdline` section's bytes and virtual size
/// zeroed: what a command-line-only rewrite must leave unchanged.
///
/// # Errors
/// Not a PE image, an unbounded section table, or not exactly one bounded
/// `.cmdline` section.
pub fn non_cmdline_bytes(raw: &[u8]) -> Result<Vec<u8>> {
    let mut data = raw.to_vec();
    require(
        data.len() > 64 && data.starts_with(b"MZ"),
        "invalid fixture PE",
    )?;
    let pe = le_u32(&data, 60).unwrap_or(usize::MAX);
    require(
        pe.checked_add(24).is_some_and(|end| end <= data.len())
            && data.get(pe..pe.saturating_add(4)) == Some(b"PE\0\0".as_slice()),
        "invalid fixture PE header",
    )?;
    let count = le_u16(&data, pe.saturating_add(6)).unwrap_or(usize::MAX);
    let optional = le_u16(&data, pe.saturating_add(20)).unwrap_or(usize::MAX);
    let table = pe.saturating_add(24).saturating_add(optional);
    require(
        count <= 96 && table.saturating_add(count.saturating_mul(40)) <= data.len(),
        "invalid fixture PE table",
    )?;
    let mut matched = 0usize;
    for number in 0..count {
        let offset = table.saturating_add(number.saturating_mul(40));
        let name = data
            .get(offset..offset.saturating_add(8))
            .unwrap_or_default();
        let trimmed = name
            .iter()
            .rposition(|&b| b != 0)
            .and_then(|end| name.get(..=end))
            .unwrap_or_default();
        if trimmed != b".cmdline" {
            continue;
        }
        matched = matched.saturating_add(1);
        let size = le_u32(&data, offset.saturating_add(16)).unwrap_or(0);
        let start = le_u32(&data, offset.saturating_add(20)).unwrap_or(usize::MAX);
        require(
            0 < size && size <= 65536 && start.saturating_add(size) <= data.len(),
            "invalid cmdline fixture bounds",
        )?;
        if let Some(section) = data.get_mut(start..start.saturating_add(size)) {
            section.fill(0);
        }
        if let Some(virtual_size) =
            data.get_mut(offset.saturating_add(8)..offset.saturating_add(12))
        {
            virtual_size.fill(0);
        }
    }
    require(
        matched == 1,
        "fixture requires exactly one command-line section",
    )?;
    Ok(data)
}

fn text_field<'a>(metadata: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    metadata
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| (1..=16384).contains(&text.chars().count()))
}

/// Promotes the fixture's sealed UKI onto a scratch target and ESP and
/// requires only its command line to change.
///
/// # Errors
/// Malformed promotion metadata, a failed promotion, or one that changed the
/// origin, the loader entry or any byte outside the command line.
pub fn promotion_probe(
    fixture: &dyn Fixture,
    root: &Path,
    manifest: &Map<String, Value>,
    version: &str,
) -> Result<Value> {
    let invalid = || Error::new("invalid scratch promotion fixture");
    let metadata = manifest
        .get("promotion")
        .and_then(Value::as_object)
        .ok_or_else(invalid)?;
    let digest = metadata
        .get("composefs_digest")
        .and_then(Value::as_str)
        .filter(|digest| {
            digest.len() == 128
                && digest
                    .bytes()
                    .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        })
        .ok_or_else(invalid)?;
    let (Some(origin_text), Some(loader_text), Some(update_origin)) = (
        text_field(metadata, "origin"),
        text_field(metadata, "loader"),
        text_field(metadata, "update_origin"),
    ) else {
        return Err(invalid());
    };
    let target = root.join("promotion-target");
    let esp = root.join("promotion-esp");
    let deployment = target.join("state/deploy").join(digest);
    fs::create_dir_all(deployment.join("etc"))?;
    let origin = deployment.join(format!("{digest}.origin"));
    fs::write(&origin, origin_text)?;
    let entry = esp.join("loader/entries/fixture.conf");
    fs::create_dir_all(esp.join("loader/entries"))?;
    fs::write(&entry, loader_text)?;
    let original = root.join("boot/EFI/Linux").join(format!("{version}.efi"));
    let destination = esp
        .join("EFI/Linux/bootc")
        .join(format!("bootc_composefs-{digest}.efi"));
    fs::create_dir_all(esp.join("EFI/Linux/bootc"))?;
    fs::copy(&original, &destination)?;
    let before = read(&destination)?;
    let selected = Selected {
        target_digest: manifest
            .get("target_digest")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        update_origin: Some(update_origin.to_owned()),
    };
    let result = fixture.promote(root, &target, &esp, &selected)?;
    let after = read(&destination)?;
    require(
        result == digest
            && read(&origin)? == origin_text.as_bytes()
            && read(&entry)? == loader_text.as_bytes(),
        "scratch promotion changed OCI origin or loader",
    )?;
    require(
        non_cmdline_bytes(&before)? == non_cmdline_bytes(&after)?,
        "scratch promotion changed non-command-line PE bytes",
    )?;
    Ok(
        json!({"method": "scratch-bootc-format-origin", "physical_admission": false,
        "composefs_digest": result, "origin_preserved": true, "loader_preserved": true,
        "non_cmdline_pe_bytes_preserved": true,
        "input_uki_sha256": sha256_hex(&before), "output_uki_sha256": sha256_hex(&after)}),
    )
}

/// A fixture member's path: relative, normalized, without `.` or `..`.
fn member_name(path: &Path) -> Option<String> {
    let name = path.to_str()?;
    let normal = path
        .components()
        .all(|component| matches!(component, Component::Normal(_)));
    (normal
        && !name.is_empty()
        && name.len() <= 256
        && !name.ends_with('/')
        && !name.contains("//"))
    .then(|| name.to_owned())
}

/// The streamed SHA-256 of the fixture device.
fn device_sha256(device: &Path) -> Result<String> {
    let mut digest = Sha256::new();
    let mut stream = fs::File::open(device)?;
    let mut chunk = vec![0u8; usize::try_from(MIB).unwrap_or(1 << 20)];
    loop {
        let count = stream.read(&mut chunk)?;
        let Some(bytes) = chunk.get(..count).filter(|bytes| !bytes.is_empty()) else {
            break;
        };
        digest.update(bytes);
    }
    Ok(hex::encode(digest.finalize()))
}

/// Unpacks the fixture archive below `root`: each file's SHA-256 by name,
/// and the parsed `fixture.json`.
fn unpack(device: &Path, root: &Path) -> Result<(BTreeMap<String, String>, Option<Value>)> {
    let mut seen = BTreeSet::new();
    let mut files: BTreeMap<String, String> = BTreeMap::new();
    let mut manifest = None;
    let mut total = 0u64;
    let mut archive = tar::Archive::new(fs::File::open(device)?);
    for member in archive.entries()? {
        let mut member = member?;
        let regular = matches!(
            member.header().entry_type(),
            tar::EntryType::Regular | tar::EntryType::Continuous
        );
        let raw = member.path()?.to_string_lossy().into_owned();
        require(
            regular && !seen.contains(&raw) && seen.len() < MAX_FIXTURE_FILES,
            "fixture requires unique bounded regular files",
        )?;
        let name = member_name(Path::new(&raw)).ok_or_else(|| Error::new("unsafe fixture path"))?;
        seen.insert(name.clone());
        let member_size = member.size();
        require(
            0 < member_size && member_size <= MAX_MEMBER_BYTES,
            "fixture member outside bound",
        )?;
        total = total.saturating_add(member_size);
        require(total <= MAX_TOTAL_BYTES, "fixture total exceeds bound")?;
        let mut content = Vec::new();
        (&mut member).take(member_size).read_to_end(&mut content)?;
        let destination = root.join(&name);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&destination, &content)?;
        if name == "fixture.json" {
            require(member_size <= MANIFEST_BYTES, "oversized fixture manifest")?;
            manifest = Some(
                serde_json::from_slice::<Value>(&content)
                    .map_err(|error| Error::new(error.to_string()))?,
            );
        } else {
            files.insert(name, sha256_hex(&content));
        }
    }
    Ok((files, manifest))
}

/// Validates the attached original CI boot-input fixture: read-only,
/// bounded, every file matching its manifest, and its boot inputs accepted
/// by the development target validator.
///
/// # Errors
/// A missing or writable fixture, one outside its bounds, files that differ
/// from the manifest, or boot inputs the validator refuses.
pub fn fixture_probe(fixture: &dyn Fixture) -> Result<Value> {
    let device = fixture.device();
    require(
        fixture.is_block_device(&device),
        "read-only original CI input fixture missing",
    )?;
    let actual = fs::canonicalize(&device)?;
    let name = actual
        .file_name()
        .ok_or_else(|| Error::new("read-only original CI input fixture missing"))?;
    let ro = fs::read_to_string(fixture.sys_block().join(name).join("ro"))?;
    require(ro.trim() == "1", "fixture is writable")?;
    let size = fixture.size(&actual)?;
    require(
        (MIB..=MAX_TOTAL_BYTES).contains(&size),
        "fixture outside 1-512 MiB bound",
    )?;
    let fixture_sha256 = device_sha256(&device)?;
    let scratch = tempfile::Builder::new()
        .prefix("tilo-boot-fixture-")
        .tempdir_in(fixture.scratch())?;
    let root = scratch.path();
    let (files, manifest) = unpack(&device, root)?;
    let manifest = manifest
        .as_ref()
        .and_then(Value::as_object)
        .filter(|manifest| {
            manifest.get("schema").and_then(Value::as_str)
                == Some("tilo.installer-code-boot-fixture.v1")
                && manifest.get("physical_admission") == Some(&Value::Bool(false))
                && manifest
                    .get("target_digest")
                    .and_then(Value::as_str)
                    .and_then(|digest| digest.strip_prefix("sha256:"))
                    .is_some_and(|hex| {
                        hex.len() == 64
                            && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
                    })
        })
        .ok_or_else(|| Error::new("invalid fixture manifest"))?;
    let expected: Map<String, Value> = files
        .iter()
        .map(|(name, digest)| (name.clone(), Value::String(digest.clone())))
        .collect();
    require(
        manifest.get("files") == Some(&Value::Object(expected.clone())),
        "fixture bytes differ from input manifest",
    )?;
    let inputs = fixture.validate(root)?;
    let profile_path = root.join("usr/lib/tilo/hardware-profile.json");
    require(
        profile_path.is_file()
            && !profile_path.is_symlink()
            && fs::metadata(&profile_path)?.len() <= MANIFEST_BYTES,
        "unsafe boot-input fixture metadata",
    )?;
    let profile: Value = serde_json::from_slice(&read(&profile_path)?)
        .map_err(|error| Error::new(error.to_string()))?;
    let promotion = if manifest.contains_key("promotion") {
        promotion_probe(fixture, root, manifest, &inputs.version)?
    } else {
        Value::Null
    };
    let dtb = inputs
        .dtb
        .as_deref()
        .map(|dtb| read(dtb).map(|bytes| sha256_hex(&bytes)))
        .transpose()?;
    Ok(
        json!({"promotion": promotion, "schema": "tilo.installer-code-cached-target-probe.v1",
        "physical_admission": false, "target_digest": manifest.get("target_digest"),
        "profile": profile.get("id").cloned().unwrap_or(Value::Null),
        "kernel_version": inputs.version,
        "mount": {"method": "readonly-original-ci-boot-fixture", "cache_authority": "none",
                  "uki_fixture": {"sha256": fixture_sha256, "bytes": size, "physical_admission": false}},
        "input_sha256": expected, "kernel_sha256": sha256_hex(&read(&inputs.kernel)?),
        "initrd_sha256": sha256_hex(&read(&inputs.initrd)?), "dtb_sha256": dtb,
        "validator_sha256": sha256_hex(&read(&fixture.validator())?)}),
    )
}

/// The cached-target probe: only in the development scenario VM, and only
/// with the original CI boot-input fixture attached.
///
/// # Errors
/// Outside the development VM, without the fixture, or see
/// [`fixture_probe`].
pub fn cached_target(fixture: &dyn Fixture) -> Result<Value> {
    let flavour = fs::read("/usr/lib/tilo/image-flavour").unwrap_or_default();
    let cmdline = fs::read_to_string("/proc/cmdline").unwrap_or_default();
    let port = |name: &str| {
        fs::metadata(format!("/dev/virtio-ports/{name}"))
            .is_ok_and(|info| info.file_type().is_char_device())
    };
    require(
        rustix::process::geteuid().is_root()
            && Path::new("/usr/share/tilo/unverified-vm-install").is_file()
            && flavour == b"dev\n"
            && port("org.tilo.installer-code")
            && port("org.tilo.vm-install-proof")
            && cmdline
                .split_whitespace()
                .any(|word| word == "tilo.install"),
        "boot-input probe requires development scenario VM",
    )?;
    let virt = crate::process::run(&["systemd-detect-virt", "--vm", "--quiet"], None, TIMEOUT)?;
    require(
        virt.success(),
        &format!(
            "boot-input fixture probe: {}",
            String::from_utf8_lossy(&virt.stderr)
        ),
    )?;
    require(
        fixture.device().exists(),
        "pure target validator probe requires original CI boot-input fixture",
    )?;
    fixture_probe(fixture)
}

/// The first executable `name` on `PATH`, as `shutil.which` finds it.
fn which(name: &str) -> Value {
    std::env::var_os("PATH")
        .and_then(|path| {
            std::env::split_paths(&path)
                .map(|directory| directory.join(name))
                .find(|candidate| {
                    fs::metadata(candidate)
                        .is_ok_and(|info| info.is_file() && info.permissions().mode() & 0o111 != 0)
                })
        })
        .map_or(Value::Null, |path| {
            Value::String(path.to_string_lossy().into_owned())
        })
}

/// The probe row for the backend at `backend`.
///
/// # Errors
/// Not running in the enforcing system service, or see [`cached_target`].
pub fn probe(backend: &Path, cached: bool) -> Result<Value> {
    let context = fs::read_to_string("/proc/self/attr/current").unwrap_or_default();
    let context = context.trim().trim_matches('\0').to_owned();
    let enforcing =
        fs::read("/sys/fs/selinux/enforce").is_ok_and(|value| value.trim_ascii() == b"1");
    require(
        enforcing
            && matches!(
                context.split(':').nth(2),
                Some("init_t" | "unconfined_service_t")
            ),
        &format!(
            "backend probe did not execute in enforcing system service: \
             enforcing={} domain={context:?}",
            if enforcing { "True" } else { "False" }
        ),
    )?;
    let target = if cached {
        cached_target(&System)?
    } else {
        Value::Null
    };
    let candidates: Map<String, Value> = UKIFY_CANDIDATES
        .iter()
        .map(|path| ((*path).to_owned(), Value::Bool(Path::new(path).is_file())))
        .collect();
    Ok(
        json!({"schema": "tilo.installer-code-backend-probe.v1", "transient": 1,
        "domain": context, "enforcing": enforcing, "tag": "initial",
        "runtime_tools": {"ukify_path": which("ukify"), "objcopy_path": which("objcopy"),
                          "ukify_candidates": candidates},
        "profile_resolver_sha256": sha256_hex(&read(Path::new(RESOLVER))?),
        "cached_target": target,
        "helper_sha256": sha256_hex(&read(backend)?)}),
    )
}

/// `code-refresh-probe BACKEND [--cached-target]`.
#[must_use]
pub fn main(args: &[OsString]) -> ExitCode {
    let (backend, cached) = match args {
        [backend] => (backend, false),
        [backend, flag] if flag == "--cached-target" => (backend, true),
        _ => {
            eprintln!("usage: tilo-vm-install-proof code-refresh-probe BACKEND [--cached-target]");
            return ExitCode::from(2);
        }
    };
    match probe(Path::new(backend), cached) {
        Ok(row) => {
            println!("{}", Dumps::SORTED.dumps(&row));
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("tilo-vm-install-proof code-refresh-probe: {error}");
            ExitCode::FAILURE
        }
    }
}
