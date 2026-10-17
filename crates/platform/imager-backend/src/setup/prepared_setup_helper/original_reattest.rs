//! Finite rootless verification of an already protected qualification receipt.
//! Reattestation is rootless; qualification invokes the actual owned disposable
//! factory. Neither entry point accepts caller paths, grants, or proof input.
use crate::setup::{
    home_capture,
    original_admission::{CapturePins, ConsumerCapability, PROOF_BOUND, RuntimeHelper},
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    ffi::{CString, OsString},
    fs::File,
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Component, Path},
};
const HELPER: &str = "/usr/libexec/tilo-prepared-setup";
const RECEIPT: &str = "/usr/share/tilo/original-source-consumer-capabilities.json";
const SIDECAR: &str = "/usr/share/tilo-imager/capture";
const ARTIFACT_BOUND: u64 = 1024 * 1024 * 1024;
fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "Installed original-source consumer reattestation failed",
    )
}

/// No arguments, environment paths, source descriptors, or proof input.
pub fn run(args: impl IntoIterator<Item = OsString>, output: &mut impl Write) -> io::Result<()> {
    if args.into_iter().next().is_some() || std::env::current_exe()? != Path::new(HELPER) {
        return Err(invalid());
    }
    let mut held = Vec::new();
    let receipt = ProtectedFile::open(Path::new(RECEIPT))?;
    let bytes = receipt.read(PROOF_BOUND as u64)?;
    let wire: serde_json::Value = serde_json::from_slice(&bytes)?;
    if !ConsumerCapability::wire_shape(&wire) {
        return Err(invalid());
    }
    let proof: ConsumerCapability = serde_json::from_slice(&bytes)?;
    proof.validate()?;
    held.push(receipt);
    let helper = ProtectedFile::open(Path::new(HELPER))?;
    require_executable(&helper)?;
    let running = std::fs::metadata("/proc/self/exe")?;
    let named = helper.file.metadata()?;
    if running.dev() != named.dev() || running.ino() != named.ino() {
        return Err(invalid());
    }
    require_hash(&helper, &proof.prepared_helper_sha256)?;
    held.push(helper);
    // Pin protected metadata before the canonical signature/collection verifier
    // reads it. It cannot be selected by receipt fields or caller arguments.
    for relative in [
        "service/metadata/signed-root",
        "service/metadata/manifest.toml",
        "service/metadata/collection.qcl",
        "service/payload/bin/tilo-home-export",
    ] {
        let file = ProtectedFile::open(&Path::new(SIDECAR).join(relative))?;
        if relative == "service/payload/bin/tilo-home-export" {
            require_executable(&file)?;
        }
        held.push(file);
    }
    let (service, pins, embedded_sha) = home_capture::installed_original_consumer_facts()?;
    if serde_json::to_value(&pins)? != serde_json::to_value(&proof.capture_pins)?
        || service.root
            != Path::new(SIDECAR)
                .join("runtime")
                .join(&pins.runtime_version)
    {
        return Err(invalid());
    }
    for (path, hash, executable) in [
        (
            Path::new("/usr/bin/tilo-launcher").to_path_buf(),
            &proof.installed_launcher_sha256,
            true,
        ),
        (
            service.root.join("bin/tilo-launcher"),
            &proof.runtime_launcher_sha256,
            true,
        ),
        (
            service.root.join("lib/libruntime_dylib.so"),
            &proof.runtime_sdk_sha256,
            false,
        ),
        (
            service.root.join("bin/tar"),
            &proof.runtime_tar_sha256,
            true,
        ),
        (
            service.root.join("lib/tilo-capture-openpgp.json"),
            &proof.runtime_openpgp_table_sha256,
            false,
        ),
    ] {
        let file = ProtectedFile::open(&path)?;
        if executable {
            require_executable(&file)?;
        }
        require_hash(&file, hash)?;
        held.push(file);
    }
    held.push(ProtectedFile::open(&service.root.join("bundle.toml"))?);
    let table_file = ProtectedFile::open(&service.root.join("lib/tilo-capture-openpgp.json"))?;
    let table_bytes = table_file.read(64 * 1024)?;
    require_hash(&table_file, &proof.runtime_openpgp_table_sha256)?;
    let wire: serde_json::Value = serde_json::from_slice(&table_bytes)?;
    if !wire.is_object()
        || !wire
            .get("helpers")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|rows| rows.iter().all(serde_json::Value::is_object))
    {
        return Err(invalid());
    }
    let table: Table = serde_json::from_slice(&table_bytes)?;
    table.matches(&proof)?;
    held.push(table_file);
    for helper in &proof.runtime_helpers {
        let file = ProtectedFile::open(&service.root.join(&helper.bundle))?;
        require_executable(&file)?;
        require_hash(&file, &helper.sha256)?;
        held.push(file);
    }
    // A final canonical verification covers the entire signed service/runtime,
    // including dependencies not individually listed in this receipt.
    let (again, again_pins, again_embedded_sha) =
        home_capture::installed_original_consumer_facts()?;
    if again_pins != pins || again.root != service.root || again_embedded_sha != embedded_sha {
        return Err(invalid());
    }
    for file in &held {
        file.validate()?;
    }
    serde_json::to_writer(&mut *output, &proof)?;
    output.write_all(b"\n")
}
fn executable_permissions(metadata: &std::fs::Metadata) -> bool {
    metadata.is_file() && metadata.mode() & 0o111 != 0 && metadata.mode() & 0o022 == 0
}
fn require_executable(file: &ProtectedFile) -> io::Result<()> {
    file.validate()?;
    identity(&file.file)?;
    if !executable_permissions(&file.file.metadata()?) {
        return Err(invalid());
    }
    Ok(())
}
fn require_hash(file: &ProtectedFile, expected: &str) -> io::Result<()> {
    if actual_hash(file)? != expected {
        return Err(invalid());
    }
    Ok(())
}
fn actual_hash(file: &ProtectedFile) -> io::Result<String> {
    use std::os::unix::fs::FileExt;
    let mut hash = Sha256::new();
    let mut offset = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.file.read_at(&mut buffer, offset)?;
        if count == 0 {
            break;
        }
        offset = offset.checked_add(count as u64).ok_or_else(invalid)?;
        if offset > ARTIFACT_BOUND {
            return Err(invalid());
        }
        hash.update(&buffer[..count]);
    }
    file.validate()?;
    Ok(hex::encode(hash.finalize()))
}

/// Runs the actual factory once; there is no receipt or synthetic proof input.
pub fn qualify(
    args: impl IntoIterator<Item = OsString>,
    output: &mut impl Write,
) -> io::Result<()> {
    // SAFETY: these libc queries have no pointer arguments or side effects.
    if args.into_iter().next().is_some()
        || rustix::process::geteuid().as_raw() != 0
        || std::env::current_exe()? != Path::new(HELPER)
    {
        return Err(invalid());
    }
    let (before, held) = ActualFacts::observe()?;
    let cancelled = std::sync::atomic::AtomicBool::new(false);
    let qualified = home_capture::consumer_qualification::run_owned_disposable(&cancelled)?;
    if qualified.capture_pins_sha256() != before.embedded_sha {
        return Err(invalid());
    }
    let (after, after_held) = ActualFacts::observe()?;
    if before != after {
        return Err(invalid());
    }
    for file in held.iter().chain(&after_held) {
        file.validate()?;
    }
    // Only the opaque real factory result supplies this final field. The
    // factory returns after verifier exit and owned medium/source retirement.
    let proof = before.into_capability(qualified.receipt_sha256())?;
    serde_json::to_writer(&mut *output, &proof)?;
    output.write_all(b"\n")
}
/// The fixed root producer returns both separately typed capabilities only
/// after real source6 work and actual installed receiving UID qualification.
/// It never accepts paths, passwords, a receipt or a qualification DTO.
pub fn qualify_applications(
    args: impl IntoIterator<Item = OsString>,
    output: &mut impl Write,
) -> io::Result<()> {
    if args.into_iter().next().is_some()
        || rustix::process::getuid().as_raw() != 0
        || rustix::process::geteuid().as_raw() != 0
        || std::env::current_exe()? != Path::new(HELPER)
    {
        return Err(invalid());
    }
    // The producer entry point is this helper's own
    // --verify-applications-consumer-capability mode, so it binds the helper.
    let script = ProtectedFile::open(Path::new(HELPER))?;
    require_executable(&script)?;
    let script_sha = actual_hash(&script)?;
    let (before, held) = ActualFacts::observe()?;
    if before.pins.worker_protocol != 6 {
        return Err(invalid());
    }
    let cancelled = std::sync::atomic::AtomicBool::new(false);
    let qualified =
        home_capture::consumer_qualification::run_owned_applications_disposable(&cancelled)?;
    if qualified.capture_pins_sha256() != before.embedded_sha {
        return Err(invalid());
    }
    let (after, after_held) = ActualFacts::observe()?;
    if before != after || actual_hash(&script)? != script_sha {
        return Err(invalid());
    }
    for file in held.iter().chain(&after_held) {
        file.validate()?;
    }
    script.validate()?;
    let native = sorted_value(qualified.receiving_proof()?);
    let helper = ProtectedFile::open(Path::new(HELPER))?;
    let helper_sha = actual_hash(&helper)?;
    if native
        .get("prepared_helper_sha256")
        .and_then(serde_json::Value::as_str)
        != Some(helper_sha.as_str())
    {
        return Err(invalid());
    }
    let capability = crate::setup::applications_admission::ConsumerCapability {
        schema: 1,
        subset_protocol: 2,
        prepared_helper_sha256: helper_sha,
        qualification_script_sha256: script_sha,
        native_qualification_sha256: hex::encode(Sha256::digest(serde_json::to_vec(&native)?)),
    };
    capability.validate()?;
    let original = before.into_capability(qualified.receipt_sha256())?;
    let envelope = serde_json::json!({"capability":capability,"original_source":original,"native_qualification":native});
    let bytes = serde_json::to_vec(&envelope)?;
    if bytes.len() > PROOF_BOUND {
        return Err(invalid());
    }
    output.write_all(&bytes)?;
    output.write_all(b"\n")
}
// Explicitly recurse and insert sorted keys: workspace serde_json can enable
// preserve_order, so merely hashing Value serialization is not canonical.
fn sorted_value(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(fields) => {
            let mut keys = fields.keys().collect::<Vec<_>>();
            keys.sort_unstable();
            let mut result = serde_json::Map::new();
            for key in keys {
                result.insert(key.clone(), sorted_value(&fields[key]));
            }
            serde_json::Value::Object(result)
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(sorted_value).collect())
        }
        other => other.clone(),
    }
}

#[derive(PartialEq, Eq)]
struct ActualFacts {
    pins: CapturePins,
    embedded_sha: [u8; 32],
    // Includes signed metadata and runtime manifest, not just public hashes.
    artifacts: Vec<(String, String)>,
    helpers: Vec<RuntimeHelper>,
}
impl ActualFacts {
    fn observe() -> io::Result<(Self, Vec<ProtectedFile>)> {
        let mut artifacts = Vec::new();
        let mut held = Vec::new();
        let helper = ProtectedFile::open(Path::new(HELPER))?;
        require_executable(&helper)?;
        let running = std::fs::metadata("/proc/self/exe")?;
        let named = helper.file.metadata()?;
        if running.dev() != named.dev() || running.ino() != named.ino() {
            return Err(invalid());
        }
        artifacts.push((HELPER.into(), actual_hash(&helper)?));
        held.push(helper);
        for relative in [
            "service/metadata/signed-root",
            "service/metadata/manifest.toml",
            "service/metadata/collection.qcl",
            "service/payload/bin/tilo-home-export",
        ] {
            record_artifact(
                &Path::new(SIDECAR).join(relative),
                &mut artifacts,
                &mut held,
                relative == "service/payload/bin/tilo-home-export",
            )?;
        }
        let (service, pins, embedded_sha) = home_capture::installed_original_consumer_facts()?;
        if service.root
            != Path::new(SIDECAR)
                .join("runtime")
                .join(&pins.runtime_version)
        {
            return Err(invalid());
        }
        let pins: CapturePins = serde_json::from_value(serde_json::to_value(pins)?)?;
        record_artifact(
            Path::new("/usr/bin/tilo-launcher"),
            &mut artifacts,
            &mut held,
            true,
        )?;
        for relative in [
            "bin/tilo-launcher",
            "lib/libruntime_dylib.so",
            "bin/tar",
            "lib/tilo-capture-openpgp.json",
            "bundle.toml",
        ] {
            record_artifact(
                &service.root.join(relative),
                &mut artifacts,
                &mut held,
                matches!(relative, "bin/tilo-launcher" | "bin/tar"),
            )?;
        }
        let installed_launcher = artifacts
            .iter()
            .find(|(path, _)| path == "/usr/bin/tilo-launcher")
            .ok_or_else(invalid)?;
        let runtime_launcher = artifacts
            .iter()
            .find(|(path, _)| Path::new(path) == service.root.join("bin/tilo-launcher"))
            .ok_or_else(invalid)?;
        if installed_launcher.1 != runtime_launcher.1 {
            return Err(invalid());
        }
        let table_file = ProtectedFile::open(&service.root.join("lib/tilo-capture-openpgp.json"))?;
        let bytes = table_file.read(64 * 1024)?;
        let wire: serde_json::Value = serde_json::from_slice(&bytes)?;
        if !wire.is_object()
            || !wire
                .get("helpers")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|rows| rows.iter().all(serde_json::Value::is_object))
        {
            return Err(invalid());
        }
        let table: Table = serde_json::from_slice(&bytes)?;
        table.validate()?;
        let mut helpers = Vec::new();
        for row in &table.helpers {
            let file = ProtectedFile::open(&service.root.join(&row.bundle))?;
            require_executable(&file)?;
            let sha256 = actual_hash(&file)?;
            artifacts.push((
                service
                    .root
                    .join(&row.bundle)
                    .to_string_lossy()
                    .into_owned(),
                sha256.clone(),
            ));
            held.push(file);
            helpers.push(RuntimeHelper {
                role: row.role.clone(),
                bundle: row.bundle.clone(),
                sha256,
            });
        }
        helpers.sort_by(|a, b| a.role.cmp(&b.role));
        held.push(table_file);
        let (_, again, again_sha) = home_capture::installed_original_consumer_facts()?;
        if serde_json::to_value(again)? != serde_json::to_value(&pins)? || again_sha != embedded_sha
        {
            return Err(invalid());
        }
        for file in &held {
            file.validate()?;
        }
        Ok((
            Self {
                pins,
                embedded_sha,
                artifacts,
                helpers,
            },
            held,
        ))
    }
    fn into_capability(self, receipt: [u8; 32]) -> io::Result<ConsumerCapability> {
        let runtime = Path::new(SIDECAR)
            .join("runtime")
            .join(&self.pins.runtime_version);
        let hash = |path: &Path| -> io::Result<String> {
            self.artifacts
                .iter()
                .find(|(name, _)| Path::new(name) == path)
                .map(|(_, sha)| sha.clone())
                .ok_or_else(invalid)
        };
        let proof = ConsumerCapability {
            schema: 1,
            original_source_protocol: self.pins.worker_protocol,
            prepared_helper_sha256: hash(Path::new(HELPER))?,
            installed_launcher_sha256: hash(Path::new("/usr/bin/tilo-launcher"))?,
            runtime_launcher_sha256: hash(&runtime.join("bin/tilo-launcher"))?,
            runtime_sdk_sha256: hash(&runtime.join("lib/libruntime_dylib.so"))?,
            runtime_tar_sha256: hash(&runtime.join("bin/tar"))?,
            runtime_openpgp_table_sha256: hash(&runtime.join("lib/tilo-capture-openpgp.json"))?,
            capture_pins: self.pins,
            runtime_helpers: self.helpers,
            qualifier_receipt_sha256: hex::encode(receipt),
        };
        proof.validate()?;
        Ok(proof)
    }
}
fn record_artifact(
    path: &Path,
    artifacts: &mut Vec<(String, String)>,
    held: &mut Vec<ProtectedFile>,
    executable: bool,
) -> io::Result<()> {
    let file = ProtectedFile::open(path)?;
    if executable {
        require_executable(&file)?;
    }
    artifacts.push((path.to_string_lossy().into_owned(), actual_hash(&file)?));
    held.push(file);
    Ok(())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Table {
    schema: u32,
    openpgp_version: String,
    libexecdir: String,
    helpers: Vec<TableHelper>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TableHelper {
    role: String,
    target: String,
    bundle: String,
}
fn absolute(value: &str) -> bool {
    value.starts_with('/')
        && value.len() <= 4096
        && !value.bytes().any(|b| b < 32 || b == 127)
        && !value.ends_with('/')
        && !value.contains("//")
        && !value.split('/').any(|p| p == "." || p == "..")
        && Path::new(value)
            .components()
            .all(|c| matches!(c, Component::RootDir | Component::Normal(_)))
}
impl Table {
    // This is comparison of already signed metadata, not an executable grant.
    fn validate(&self) -> io::Result<()> {
        if self.schema != 1
            || !(1..=128).contains(&self.openpgp_version.len())
            || self.openpgp_version.bytes().any(|b| b < 32 || b == 127)
            || !absolute(&self.libexecdir)
            || !(4..=5).contains(&self.helpers.len())
        {
            return Err(invalid());
        }
        let mut roles = BTreeSet::new();
        let mut targets = BTreeSet::new();
        for row in &self.helpers {
            let leaf = row.bundle.strip_prefix("bin/").ok_or_else(invalid)?;
            if !matches!(
                row.role.as_str(),
                "agent" | "gpgconf" | "keyboxd" | "openpgp" | "ostree"
            ) || !roles.insert(row.role.as_str())
                || !targets.insert(&row.target)
                || !absolute(&row.target)
                || !(row.target.starts_with("/usr/bin/") || row.target.starts_with("/usr/libexec/"))
                || (row.role == "ostree" && row.target != "/usr/bin/ostree")
                || !(1..=252).contains(&leaf.len())
                || matches!(leaf, "." | "..")
                || !leaf
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            {
                return Err(invalid());
            }
        }
        if !["agent", "gpgconf", "openpgp", "ostree"]
            .iter()
            .all(|r| roles.contains(r))
        {
            return Err(invalid());
        }
        Ok(())
    }
    fn matches(&self, proof: &ConsumerCapability) -> io::Result<()> {
        self.validate()?;
        if self.helpers.len() != proof.runtime_helpers.len()
            || self.helpers.iter().any(|row| {
                !proof
                    .runtime_helpers
                    .iter()
                    .any(|expected| expected.role == row.role && expected.bundle == row.bundle)
            })
        {
            return Err(invalid());
        }
        Ok(())
    }
}
#[derive(PartialEq, Eq)]
struct Identity {
    dev: u64,
    ino: u64,
    uid: u32,
    mode: u32,
}
fn identity(file: &File) -> io::Result<Identity> {
    let m = file.metadata()?;
    if m.uid() != 0 || m.mode() & 0o022 != 0 {
        return Err(invalid());
    }
    Ok(Identity {
        dev: m.dev(),
        ino: m.ino(),
        uid: m.uid(),
        mode: m.mode(),
    })
}
struct Edge {
    parent: File,
    name: CString,
    expected: Identity,
    directory: bool,
}
struct ProtectedFile {
    file: File,
    edges: Vec<Edge>,
    expected: std::fs::Metadata,
}
fn open_at(parent: &File, name: &CString, directory: bool) -> io::Result<File> {
    let flags = libc::O_CLOEXEC
        | libc::O_NOFOLLOW
        | libc::O_NONBLOCK
        | if directory {
            libc::O_PATH | libc::O_DIRECTORY
        } else {
            libc::O_RDONLY
        };
    // SAFETY: live held parent descriptor, NUL-terminated component, no creation.
    let raw = unsafe { libc::openat(parent.as_raw_fd(), name.as_ptr(), flags) };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful openat returned this owned descriptor.
    Ok(unsafe { File::from_raw_fd(raw) })
}
impl ProtectedFile {
    fn open(path: &Path) -> io::Result<Self> {
        if !path.is_absolute() {
            return Err(invalid());
        }
        let components: Vec<_> = path.components().collect();
        if components.first() != Some(&Component::RootDir)
            || components.len() < 2
            || components[1..]
                .iter()
                .any(|c| !matches!(c, Component::Normal(_)))
        {
            return Err(invalid());
        }
        let mut parent = std::fs::OpenOptions::new();
        use std::os::unix::fs::OpenOptionsExt;
        parent
            .read(true)
            .custom_flags(libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW);
        let mut current = parent.open("/")?;
        identity(&current)?;
        let mut edges = Vec::new();
        for (index, component) in components[1..].iter().enumerate() {
            let name = CString::new(component.as_os_str().as_bytes()).map_err(|_| invalid())?;
            let directory = index + 2 != components.len();
            let next = open_at(&current, &name, directory)?;
            let expected = identity(&next)?;
            if !directory && !next.metadata()?.is_file() {
                return Err(invalid());
            }
            edges.push(Edge {
                parent: current,
                name,
                expected,
                directory,
            });
            current = next;
        }
        let expected = current.metadata()?;
        Ok(Self {
            file: current,
            edges,
            expected,
        })
    }
    fn validate(&self) -> io::Result<()> {
        for edge in &self.edges {
            identity(&edge.parent)?;
            let current = open_at(&edge.parent, &edge.name, edge.directory)?;
            if identity(&current)? != edge.expected {
                return Err(invalid());
            }
        }
        let now = self.file.metadata()?;
        if now.len() != self.expected.len()
            || now.mtime() != self.expected.mtime()
            || now.mtime_nsec() != self.expected.mtime_nsec()
            || now.ctime() != self.expected.ctime()
            || now.ctime_nsec() != self.expected.ctime_nsec()
        {
            return Err(invalid());
        }
        Ok(())
    }
    fn read(&self, bound: u64) -> io::Result<Vec<u8>> {
        if self.expected.len() > bound {
            return Err(invalid());
        }
        let mut bytes = Vec::new();
        self.file
            .try_clone()?
            .take(bound + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > bound {
            return Err(invalid());
        }
        self.validate()?;
        Ok(bytes)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn finite_mode_refuses_caller_arguments_before_any_installed_lookup() {
        let mut out = Vec::new();
        assert!(run([OsString::from("/tmp/proof.json")], &mut out).is_err());
        assert!(out.is_empty());
        assert!(qualify([OsString::from("/tmp/proof.json")], &mut out).is_err());
        assert!(out.is_empty());
    }
    #[test]
    fn protected_reader_refuses_actual_symlink_and_nonregular_artifacts() {
        assert!(ProtectedFile::open(Path::new("/proc/self/exe")).is_err());
        assert!(ProtectedFile::open(Path::new("/dev/null")).is_err());
        assert!(ProtectedFile::open(Path::new("relative/proof")).is_err());
    }
    #[test]
    fn executable_permission_check_refuses_chmod_without_a_content_change() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("consumer");
        std::fs::write(&path, b"fixed consumer bytes").unwrap();
        let file = File::open(&path).unwrap();
        file.set_permissions(std::fs::Permissions::from_mode(0o755))
            .unwrap();
        assert!(executable_permissions(&file.metadata().unwrap()));
        let before = std::fs::read(&path).unwrap();
        file.set_permissions(std::fs::Permissions::from_mode(0o644))
            .unwrap();
        assert!(!executable_permissions(&file.metadata().unwrap()));
        assert_eq!(before, std::fs::read(&path).unwrap());
        file.set_permissions(std::fs::Permissions::from_mode(0o777))
            .unwrap();
        assert!(!executable_permissions(&file.metadata().unwrap()));
    }
    #[test]
    fn signed_table_comparison_refuses_role_and_target_substitutions() {
        let proof = crate::setup::original_admission::syntax_fixture();
        let mut table = Table {
            schema: 1,
            openpgp_version: "2.4".into(),
            libexecdir: "/usr/libexec".into(),
            helpers: proof
                .runtime_helpers
                .iter()
                .map(|h| TableHelper {
                    role: h.role.clone(),
                    target: format!("/usr/bin/{}", h.bundle.strip_prefix("bin/").unwrap()),
                    bundle: h.bundle.clone(),
                })
                .collect(),
        };
        table.matches(&proof).unwrap();
        table.helpers[0].role = "openpgp".into();
        assert!(table.matches(&proof).is_err());
        table.helpers[0].role = "agent".into();
        table.helpers[0].target = "/tmp/gpg-agent".into();
        assert!(table.matches(&proof).is_err());
    }
}

#[cfg(test)]
mod applications_encoding_tests {
    #[test]
    fn native_receiving_digest_has_explicit_recursive_sorted_encoding() {
        let value: serde_json::Value =
            serde_json::from_str(r#"{"z":{"b":2,"a":1},"a":[{"y":4,"x":3}]}"#).unwrap();
        let canonical = serde_json::to_vec(&super::sorted_value(&value)).unwrap();
        assert_eq!(canonical, br#"{"a":[{"x":3,"y":4}],"z":{"a":1,"b":2}}"#);
    }
}
