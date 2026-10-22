//! Execute the installed account consumers before sealing the target rootfs.
//!
//! Only the final image build produces the receipt. Support is proven by the
//! installed credential helper and the SDK-linked probe from the verified
//! signed native runtime, never by a host environment variable or a declared
//! image version.
use super::{
    field, insert, invalid,
    invoke::{self, ENV},
    json, require, service,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::{File, Metadata, OpenOptions},
    io::{self, Read, Seek, SeekFrom, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::Duration,
};

pub const RECEIPT: &str = "/usr/share/tilo/account-consumer-capabilities.json";
const PROBE_BOUND: u64 = 64 * 1024 * 1024;
const LIBRARY_BOUND: u64 = 2048 * 1024 * 1024;
const RECEIPT_BOUND: u64 = 64 * 1024;
/// The admission reader's limit on the public check result.
const PUBLIC_BOUND: usize = 16 * 1024;

/// Probe executor: command and its environment.
pub type Runner<'a> = dyn FnMut(&[OsString], &[(&str, OsString)]) -> io::Result<Vec<u8>> + 'a;

/// The production executor: 60 seconds, 4 KiB per output stream.
pub fn invoke(command: &[OsString], env: &[(&str, OsString)]) -> io::Result<Vec<u8>> {
    invoke::run(&invoke::Request {
        command,
        env,
        input: None,
        limit: 4096,
        timeout: Duration::from_secs(60),
    })
}

/// The fixed probe environment.
pub fn env() -> Vec<(&'static str, OsString)> {
    ENV.iter()
        .map(|(key, value)| (*key, OsString::from(value)))
        .collect()
}

pub(super) fn version_ok(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._+-".contains(&b))
}

pub(super) fn hex_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A bounded, nonempty regular file, observed without following a symlink.
pub fn regular(path: &Path, limit: u64) -> io::Result<Metadata> {
    let info = std::fs::symlink_metadata(path)?;
    require(
        info.is_file() && info.size() > 0 && info.size() <= limit,
        &format!(
            "capability artifact is not a bounded regular file: {}",
            path.display()
        ),
    )?;
    Ok(info)
}

pub fn digest(path: &Path, limit: u64) -> io::Result<String> {
    regular(path, limit)?;
    super::sha256_hex(&mut File::open(path)?)
}

fn executable(path: &Path, limit: u64) -> io::Result<()> {
    require(
        regular(path, limit)?.mode() & 0o111 != 0,
        "account consumer is not executable",
    )
}

pub(super) fn protocol(output: &[u8]) -> io::Result<()> {
    let value = json::strict(output)?;
    require(
        value.as_object().is_some_and(|fields| fields.len() == 2)
            && field(&value, "schema").as_i64() == Some(1)
            && field(&value, "protected_grant_protocol").as_i64() == Some(1),
        "installed account consumer lacks protocol 1",
    )
}

fn le<const N: usize>(bytes: &[u8], at: usize, message: &str) -> io::Result<[u8; N]> {
    at.checked_add(N)
        .and_then(|end| bytes.get(at..end))
        .and_then(|field| field.try_into().ok())
        .ok_or_else(|| invalid(message))
}

/// The probe's one canonical ELF interpreter, as a path inside the bundle.
pub fn interpreter(probe: &Path) -> io::Result<String> {
    const ELF: &str = "capability probe is not a little-endian ELF64 binary";
    const TABLE: &str = "truncated probe ELF program table";
    let mut source = File::open(probe)?;
    let mut header = [0_u8; 64];
    let read = read_full(&mut source, &mut header)?;
    require(read == 64 && header.starts_with(b"\x7fELF\x02\x01"), ELF)?;
    let expected = match std::env::consts::ARCH {
        "x86_64" => Some(62),
        "aarch64" => Some(183),
        _ => None,
    };
    require(
        Some(u16::from_le_bytes(le(&header, 18, ELF)?)) == expected,
        "capability probe architecture differs from target image",
    )?;
    let offset = u64::from_le_bytes(le(&header, 32, ELF)?);
    let size = u16::from_le_bytes(le(&header, 54, ELF)?);
    let count = u16::from_le_bytes(le(&header, 56, ELF)?);
    require(
        size == 56 && count > 0 && count <= 128,
        "invalid probe ELF program table",
    )?;
    let mut matches = Vec::new();
    for index in 0..u64::from(count) {
        let at = index
            .checked_mul(u64::from(size))
            .and_then(|delta| offset.checked_add(delta))
            .ok_or_else(|| invalid(TABLE))?;
        source.seek(SeekFrom::Start(at))?;
        let mut entry = [0_u8; 56];
        require(read_full(&mut source, &mut entry)? == 56, TABLE)?;
        if u32::from_le_bytes(le(&entry, 0, TABLE)?) != 3 {
            continue;
        }
        let start = u64::from_le_bytes(le(&entry, 8, TABLE)?);
        let length = u64::from_le_bytes(le(&entry, 32, TABLE)?);
        require(
            length > 1 && length <= 4096,
            "invalid probe ELF interpreter",
        )?;
        source.seek(SeekFrom::Start(start))?;
        let mut name = vec![0_u8; usize::try_from(length).unwrap_or_default()];
        let read = read_full(&mut source, &mut name)?;
        let terminated = read == name.len() && name.pop() == Some(0);
        let name = String::from_utf8(name)
            .ok()
            .filter(|name| terminated && name.is_ascii() && !name.contains('\0'))
            .ok_or_else(|| invalid("invalid probe ELF interpreter name"))?;
        matches.push(name);
    }
    let (Some(name), None) = (matches.pop(), matches.pop()) else {
        return Err(invalid("probe must have one canonical ELF interpreter"));
    };
    let relative = name
        .strip_prefix('/')
        .filter(|rest| rest.split('/').all(|part| !matches!(part, "" | "." | "..")))
        .ok_or_else(|| invalid("unsafe probe ELF interpreter path"))?;
    Ok(format!("lib/{relative}"))
}

fn read_full(source: &mut File, buffer: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while let Some(rest) = buffer.get_mut(filled..) {
        match source.read(rest)? {
            0 => break,
            count => filled = filled.saturating_add(count),
        }
        if rest.is_empty() {
            break;
        }
    }
    Ok(filled)
}

/// An installer overlay can add a certified account SDK beside the base's app
/// runtimes. It never retargets existing app manifests or the primary SDK.
pub(super) fn runtime_markers(share: &Path) -> io::Result<(&'static str, &'static str)> {
    let account = (
        "account-consumer-runtime-version",
        "account-consumer-runtime-publisher.hex",
    );
    let present = [account.0, account.1].map(|name| lexists(&share.join(name)));
    require(
        present[0] == present[1],
        "incomplete account runtime selection",
    )?;
    Ok(if present[0] {
        account
    } else {
        ("runtime-version", "runtime-bundle-publisher.hex")
    })
}

pub(super) fn lexists(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

pub(super) fn read_trimmed(path: &Path) -> io::Result<String> {
    Ok(std::fs::read_to_string(path)?.trim().to_owned())
}

pub(super) fn load_toml(path: &Path) -> io::Result<toml::Table> {
    std::fs::read_to_string(path)?
        .parse::<toml::Table>()
        .map_err(|error| invalid(&format!("invalid TOML {}: {error}", path.display())))
}

pub(super) fn verify_bundle(
    root: &Path,
    bundle: &Path,
    publisher: &str,
    runner: &mut Runner<'_>,
    env: &[(&str, OsString)],
) -> io::Result<Vec<u8>> {
    runner(
        &[
            root.join("usr/libexec/tilo/tilo-runtime-bundle").into(),
            "verify".into(),
            bundle.into(),
            "--require-publisher".into(),
            publisher.into(),
            "--require-signature".into(),
            "--require-arch".into(),
            std::env::consts::ARCH.into(),
        ],
        env,
    )
}

/// Inputs of one producer or check run; `root` is a fixture seam only.
pub struct Produce<'a> {
    pub root: &'a Path,
    pub runner: &'a mut Runner<'a>,
    pub qualify_services: bool,
    pub service_evidence: Option<&'a Value>,
    pub qualifier: &'a mut service::Qualifier<'a>,
    pub route_qualifier: &'a mut service::RouteQualifier<'a>,
}

/// Measure the installed account consumers into a receipt.
#[allow(
    clippy::too_many_lines,
    reason = "one linear contract: each step gates the next"
)]
pub fn produce(run: Produce<'_>) -> io::Result<Value> {
    let Produce {
        root,
        runner,
        qualify_services,
        service_evidence,
        qualifier,
        route_qualifier,
    } = run;
    let share = root.join("usr/share/tilo");
    let (version_name, publisher_name) = runtime_markers(&share)?;
    let version = read_trimmed(&share.join(version_name))?;
    require(version_ok(&version), "invalid primary runtime version")?;
    let publisher = read_trimmed(&share.join(publisher_name))?;
    require(hex_digest(&publisher), "invalid pinned runtime publisher")?;
    let bundle = root.join("usr/lib/tilo-runtime").join(&version);
    let env = env();
    verify_bundle(root, &bundle, &publisher, runner, &env)?;
    let manifest = load_toml(&bundle.join("bundle.toml"))?;
    let root_hash = manifest
        .get("root_hash")
        .and_then(toml::Value::as_str)
        .ok_or_else(|| invalid("verified runtime has no root hash"))?
        .to_owned();
    require(
        manifest.get("version").and_then(toml::Value::as_str) == Some(version.as_str())
            && hex_digest(&root_hash),
        "verified runtime identity differs from selected primary runtime",
    )?;
    let files = manifest
        .get("files")
        .and_then(toml::Value::as_array)
        .ok_or_else(|| invalid("verified runtime has no inventory"))?;
    let mut entries = BTreeMap::new();
    for entry in files {
        let path = entry
            .get("path")
            .and_then(toml::Value::as_str)
            .ok_or_else(|| invalid("verified runtime inventory entry has no path"))?;
        entries.insert(path.to_owned(), entry);
    }
    require(
        entries.len() == files.len(),
        "duplicate runtime inventory path",
    )?;
    let probe_name = "bin/tilo-runtime-account-capability";
    let library_name = "lib/libruntime_dylib.so";
    let probe = bundle.join(probe_name);
    let helper = root.join("usr/libexec/tilo-account-credential");
    for name in [probe_name, library_name] {
        require(
            entries.contains_key(name),
            "account consumer is absent from signed runtime inventory",
        )?;
    }
    regular(&probe, PROBE_BOUND)?;
    let loader_name = interpreter(&probe)?;
    require(
        entries.contains_key(&loader_name),
        "account probe interpreter is absent from signed inventory",
    )?;
    let loader = bundle.join(&loader_name);
    for path in [&probe, &helper, &loader] {
        executable(path, PROBE_BOUND)?;
    }
    // glibc's explicit loader prevents host cache or host SDK lookup. The
    // producer already verified every DT_NEEDED edge is in this signed lib/.
    protocol(&runner(
        &[
            loader.into(),
            "--inhibit-cache".into(),
            "--library-path".into(),
            bundle.join("lib").into(),
            probe.clone().into(),
        ],
        &env,
    )?)?;
    protocol(&runner(
        &[helper.clone().into(), "capabilities".into()],
        &env,
    )?)?;
    let probe_digest = digest(&probe, PROBE_BOUND)?;
    let library_digest = digest(&bundle.join(library_name), LIBRARY_BOUND)?;
    verify_bundle(root, &bundle, &publisher, runner, &env)?;
    let mut receipt = json!({
        "schema": 1, "protected_grant_protocol": 1,
        "credential_helper_sha256": digest(&helper, PROBE_BOUND)?,
        "runtime_root_hash": root_hash,
        "runtime_probe_sha256": probe_digest,
        "runtime_library_sha256": library_digest,
        "runtime_version": version,
        "runtime_library": library_name,
    });
    let sdk = service::Sdk::from_receipt(&receipt)?;
    let proof = service::produce(service::Produce {
        root,
        runner,
        sdk: &sdk,
        qualify: qualify_services,
        evidence: service_evidence,
        qualifier,
        route_qualifier,
    })?;
    if let Some(proof) = proof {
        insert(&mut receipt, "service_sessions", proof);
    }
    Ok(receipt)
}

pub(super) fn read_receipt(path: &Path, expected: &Value) -> io::Result<Value> {
    let info = regular(path, RECEIPT_BOUND)?;
    require(
        info.mode() & 0o022 == 0,
        "account capability receipt permits foreign writes",
    )?;
    let value = json::strict(&std::fs::read(path)?)?;
    require(
        &value == expected,
        "installed account capability receipt differs from actual consumers",
    )?;
    Ok(value)
}

pub(super) fn image_owned(path: &Path) -> io::Result<()> {
    let info = std::fs::symlink_metadata(path)?;
    require(
        !info.file_type().is_symlink() && info.uid() == 0 && info.mode() & 0o022 == 0,
        &format!(
            "account capability trust artifact is not image-owned: {}",
            path.display()
        ),
    )
}

pub(super) fn image_owned_tree(root: &Path) -> io::Result<()> {
    image_owned(root)?;
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory)? {
            let path = entry?.path();
            image_owned(&path)?;
            if std::fs::symlink_metadata(&path)?.is_dir() {
                pending.push(path);
            }
        }
    }
    Ok(())
}

fn check_image_trust() -> io::Result<()> {
    for path in [
        "/usr/libexec/tilo-account-credential",
        "/usr/libexec/tilo/tilo-runtime-bundle",
        "/usr/share/tilo",
        "/usr/lib/tilo-runtime",
    ] {
        image_owned(Path::new(path))?;
    }
    let share = Path::new("/usr/share/tilo");
    let (version_name, publisher_name) = runtime_markers(share)?;
    for name in [
        version_name,
        publisher_name,
        "account-consumer-capabilities.json",
    ] {
        image_owned(&share.join(name))?;
    }
    let version = read_trimmed(&share.join(version_name))?;
    require(version_ok(&version), "invalid primary runtime version")?;
    // Every interpreter/library that could execute before probe main belongs
    // to the immutable image namespace, including flat and nested loader paths.
    image_owned_tree(&Path::new("/usr/lib/tilo-runtime").join(version))
}

/// The installed read-only check: revalidate every consumer against the saved
/// receipt and return it. Any caller may run it; it writes nothing.
pub fn check() -> io::Result<Value> {
    let target = Path::new(RECEIPT);
    check_image_trust()?;
    require(
        regular(target, RECEIPT_BOUND)?.uid() == 0,
        "account capability receipt is not image-owned",
    )?;
    let saved = json::strict(&std::fs::read(target)?)?;
    let saved = saved
        .as_object()
        .ok_or_else(|| invalid("invalid installed account capability receipt"))?;
    let evidence = saved
        .get("service_sessions")
        .filter(|value| !value.is_null());
    if let Some(evidence) = evidence {
        for path in [
            "/usr/libexec/tilo-service-session-qualification.py",
            "/usr/libexec/tilo/tilo-install",
            "/usr/share/tilo-apps/registry.toml",
        ] {
            image_owned(Path::new(path))?;
        }
        service::check_trust(evidence)?;
    }
    let mut runner = invoke;
    let mut qualifier = service::native_qualify;
    let mut route_qualifier = service::native_route_qualify;
    let receipt = produce(Produce {
        root: Path::new("/"),
        runner: &mut runner,
        qualify_services: false,
        service_evidence: evidence,
        qualifier: &mut qualifier,
        route_qualifier: &mut route_qualifier,
    })?;
    let receipt = read_receipt(target, &receipt)?;
    // The public result stays within the admission reader's bound.
    require(
        json::canonical_ascii(&receipt).len() < PUBLIC_BOUND,
        "account consumer capability exceeds public contract bound",
    )?;
    Ok(receipt)
}

/// The root image-build producer: measure and atomically replace the receipt.
pub fn publish() -> io::Result<Value> {
    require(
        rustix::process::geteuid().is_root(),
        "capability receipt must be produced inside the root image build",
    )?;
    let mut runner = invoke;
    let mut qualifier = service::native_qualify;
    let mut route_qualifier = service::native_route_qualify;
    let receipt = produce(Produce {
        root: Path::new("/"),
        runner: &mut runner,
        qualify_services: true,
        service_evidence: None,
        qualifier: &mut qualifier,
        route_qualifier: &mut route_qualifier,
    })?;
    write_atomically(Path::new(RECEIPT), &receipt)?;
    Ok(receipt)
}

fn write_atomically(target: &Path, receipt: &Value) -> io::Result<()> {
    let parent: PathBuf = target
        .parent()
        .ok_or_else(|| invalid("account receipt has no directory"))?
        .into();
    let mut temporary = tempfile::Builder::new()
        .prefix(".account-consumer-")
        .tempfile_in(&parent)?;
    let mut bytes = json::canonical_ascii(receipt);
    bytes.push(b'\n');
    temporary.write_all(&bytes)?;
    temporary.flush()?;
    temporary
        .as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o644))?;
    temporary.as_file().sync_all()?;
    temporary.persist(target).map_err(|error| error.error)?;
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY)
        .open(&parent)?
        .sync_all()
}
