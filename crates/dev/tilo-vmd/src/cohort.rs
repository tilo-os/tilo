//! Evidence that an authenticated component carries its own complete thin graph.
//! Neither a component label nor a claimed profile grants this exception.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::io::Read as _;
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _};
use std::path::{Component, Path, PathBuf};

use package_store::registry::{InstalledPackage, Registry};
use runtime_bundle::{VerifyOptions, verify_bundle_with};
use tilo_app_package::{BlobHash, BlobSource, OpenBlob, PackageMeta};

use crate::error::DaemonError;

pub(crate) struct VerifiedCohort {
    pub(crate) version: String,
    pub(crate) packages: Vec<CohortPackage>,
    /// Holds the derived archives `packages` name until the cohort is dropped.
    _archives: Staging,
}

pub(crate) struct CohortPackage {
    row: InstalledPackage,
    archive: PathBuf,
}

/// A private directory of derived archives, removed with the cohort.
struct Staging(PathBuf);

impl Staging {
    fn create(parent: &Path) -> Result<Self, DaemonError> {
        let path = parent.join(format!("tilo-vmd-cohort-{}", uuid::Uuid::new_v4().simple()));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .map_err(invalid)?;
        Ok(Self(path))
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// An installed package `application` admitted: its signed record, and its
/// payload tree at its path in the layer.
struct Installed {
    package: InstalledPackage,
    record: package_store::integrity::Record,
}

/// Payload blobs read from an installed tree, by the hashes its signed
/// collection names. The archive writer hashes each one as it streams, so a
/// file that differs from its signed hash fails the write.
struct InstalledBlobs(BTreeMap<BlobHash, PathBuf>);

impl BlobSource for InstalledBlobs {
    fn open_blob(&self, hash: &BlobHash) -> std::io::Result<Option<OpenBlob<'_>>> {
        let Some(path) = self.0.get(hash) else {
            return Ok(None);
        };
        let file = fs::File::open(path)?;
        let len = file.metadata()?.len();
        Ok(Some(OpenBlob {
            len,
            reader: Box::new(file),
        }))
    }
}

/// The complete signed archive of an installed cohort package, written into
/// `staging` from the layer's own signed record and its verified payload.
///
/// An image ships each app once, as its installed tree. The archive the
/// installer takes is the same package (the same signed root, so the same
/// package id), rebuilt here instead of carried a second time as a `.tapp`.
fn archive(installed: &Installed, publisher: &str, staging: &Path) -> Result<PathBuf, DaemonError> {
    let Installed { package, record } = installed;
    let meta = PackageMeta::from_root(
        record.root.clone(),
        record.manifest_bytes.clone(),
        record.collection.encode(),
    )
    .map_err(invalid)?;
    let key: [u8; 32] = hex::decode(publisher)
        .map_err(invalid)?
        .try_into()
        .map_err(|_| invalid("malformed cohort publisher"))?;
    if meta.id.to_hex() != package.package_id_hex
        || !matches!(
            tilo_app_package::verify(&meta, &[key]),
            tilo_app_package::Verification::Trusted { .. }
        )
    {
        return Err(invalid(
            "an installed cohort package is not the authenticated package",
        ));
    }
    let blobs = meta
        .collection
        .entries
        .iter()
        .map(|(relative, hash)| {
            let path = within(&package.payload_root, &package.payload_root.join(relative))?;
            Ok((*hash, path))
        })
        .collect::<Result<BTreeMap<_, _>, DaemonError>>()?;
    let path = staging.join(format!("{}.tapp", meta.id.to_hex()));
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .map_err(invalid)?;
    let written = tilo_app_package::tapp::write(
        std::io::BufWriter::new(file),
        &meta,
        Some(&InstalledBlobs(blobs)),
    )
    .map_err(invalid)?
    .into_inner()
    .map_err(|error| invalid(error.error()))?;
    written.sync_all().map_err(invalid)?;
    if written.metadata().map_err(invalid)?.len() > tilo_app_package::MAX_APP_TAPP_BYTES {
        return Err(invalid("a cohort archive exceeds the application bound"));
    }
    Ok(path)
}

fn invalid(detail: impl std::fmt::Display) -> DaemonError {
    DaemonError::InvalidParams(format!("apps+runtime cohort: {detail}"))
}

fn within(root: &Path, path: &Path) -> Result<PathBuf, DaemonError> {
    let canonical = path.canonicalize().map_err(invalid)?;
    if !canonical.starts_with(root.canonicalize().map_err(invalid)?) {
        return Err(invalid("a cohort path escapes its authenticated layer"));
    }
    Ok(canonical)
}

fn bounded(path: &Path, limit: u64) -> Result<Vec<u8>, DaemonError> {
    let file = fs::File::open(path).map_err(invalid)?;
    if file.metadata().map_err(invalid)?.len() > limit {
        return Err(invalid("metadata exceeds the cohort bound"));
    }
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(invalid)?;
    if u64::try_from(bytes.len()).map_err(invalid)? > limit {
        return Err(invalid("metadata changed beyond the cohort bound"));
    }
    Ok(bytes)
}

fn publisher(root: &Path, name: &str) -> Result<String, DaemonError> {
    let path = root.join("usr/share/tilo").join(name);
    let metadata = fs::symlink_metadata(&path).map_err(invalid)?;
    if !metadata.is_file() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
        return Err(invalid("a publisher pin is not a root-owned regular file"));
    }
    within(root, &path)?;
    let bytes = bounded(&path, 258)?;
    let key = std::str::from_utf8(&bytes).map_err(invalid)?.trim();
    if key.len() != 64 || !key.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid("a required publisher pin is malformed"));
    }
    Ok(key.to_ascii_lowercase())
}

fn registry(root: &Path) -> Result<Registry, DaemonError> {
    let path = within(root, &root.join("registry.toml"))?;
    bounded(&path, 8 * 1024 * 1024)?;
    let rows = Registry::load(&path).map_err(invalid)?;
    if rows.entries().is_empty() || rows.entries().len() > 512 {
        return Err(invalid("an empty or oversized application registry"));
    }
    Ok(rows)
}

fn installed_path(layer: &Path, path: &Path) -> Result<PathBuf, DaemonError> {
    let relative = path.strip_prefix("/usr/share/tilo-apps").map_err(invalid)?;
    if relative
        .components()
        .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(invalid(
            "an installed package path is not confined to the store",
        ));
    }
    within(
        &layer.join("usr/share/tilo-apps"),
        &layer.join("usr/share/tilo-apps").join(relative),
    )
}

fn native(path: &Path, arch: &str) -> Result<(), DaemonError> {
    let mut header = [0u8; 20];
    fs::File::open(path)
        .map_err(invalid)?
        .read_exact(&mut header)
        .map_err(invalid)?;
    let machine = header
        .get(18..20)
        .ok_or_else(|| invalid("truncated native header"))?;
    let expected = match arch {
        "x86_64" => 62_u16,
        "aarch64" => 183_u16,
        _ => return Err(invalid("unsupported native cohort architecture")),
    };
    if header.get(..6) != Some(b"\x7fELF\x02\x01".as_slice()) || machine != expected.to_le_bytes() {
        return Err(invalid(
            "an actual ELF architecture differs from its cohort",
        ));
    }
    Ok(())
}

fn record(package: &InstalledPackage) -> Result<package_store::integrity::Record, DaemonError> {
    let root = package_store::integrity::record_dir(package);
    for name in [
        package_store::layout::SIGNED_ROOT_FILE,
        package_store::layout::MANIFEST_FILE,
        package_store::layout::COLLECTION_FILE,
    ] {
        bounded(&within(&root, &root.join(name))?, 2 * 1024 * 1024)?;
    }
    let record = package_store::integrity::read_record(&root, &package.id, &package.package_id_hex)
        .map_err(invalid)?;
    if record.collection.entries.len() > tilo_app_package::MAX_APP_PAYLOAD_ENTRIES {
        return Err(invalid("package collection exceeds its normal inode bound"));
    }
    let mut bytes = 0_u64;
    for relative in record.collection.entries.keys() {
        if Path::new(relative)
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err(invalid("a signed collection path escapes its package"));
        }
        let path = within(&package.payload_root, &package.payload_root.join(relative))?;
        bytes = bytes
            .checked_add(fs::metadata(path).map_err(invalid)?.len())
            .ok_or_else(|| invalid("package size overflows"))?;
        if bytes > tilo_app_package::MAX_APP_UNPACKED_BYTES {
            return Err(invalid("package exceeds its normal materialization bound"));
        }
    }
    Ok(record)
}

fn application(
    layer: &Path,
    row: &InstalledPackage,
    publisher: &str,
    runtime: &str,
    arch: &str,
) -> Result<(PathBuf, Installed), DaemonError> {
    let mut package = row.clone();
    package.manifest_path = installed_path(layer, &row.manifest_path)?;
    package.payload_root = installed_path(layer, &row.payload_root)?;
    let record = record(&package)?;
    let report =
        package_store::integrity::verify(&package, &package.payload_root).map_err(invalid)?;
    if !report.is_intact() || !report.unexpected.is_empty() {
        return Err(invalid(
            "a package payload differs from its signed collection",
        ));
    }
    let manifest = tilo_app_package::Manifest::from_toml_str(
        std::str::from_utf8(&record.manifest_bytes).map_err(invalid)?,
    )
    .map_err(invalid)?;
    tilo_app_package::validate_manifest(&manifest).map_err(invalid)?;
    let isa = &manifest.package.isa;
    // Authored manifests may omit minimum ISA requirements. The actual ELF
    // below still binds the payload to this native architecture. A nonempty
    // requirements table must declare this architecture.
    let native_isa = isa.is_empty()
        || match arch {
            "x86_64" => isa
                .for_arch(tilo_app_package::manifest::Arch::X86_64)
                .is_some(),
            "aarch64" => isa
                .for_arch(tilo_app_package::manifest::Arch::AArch64)
                .is_some(),
            _ => false,
        };
    if hex::encode(record.root.publisher_identity) != publisher
        || row.publisher_key_hex != publisher
        || manifest.package.id != row.id
        || !matches!(
            manifest.package.kind,
            tilo_app_package::Kind::App | tilo_app_package::Kind::Service
        )
        || !manifest.package.publishable_as("tilo-os")
        || manifest.package.runtime.as_deref() != Some(runtime)
        || row.runtime != runtime
        || !native_isa
        || manifest.target.is_some()
    {
        return Err(invalid(format!(
            "signed native package {} is not bound to the cohort publisher, platform, runtime and ISA",
            row.id
        )));
    }
    let entry = &manifest
        .entrypoint
        .as_ref()
        .ok_or_else(|| invalid("no native entrypoint"))?
        .bin;
    let entry = within(&package.payload_root, &package.payload_root.join(entry))?;
    if fs::metadata(&entry).map_err(invalid)?.mode() & 0o111 == 0 {
        return Err(invalid("the native app entrypoint is not executable"));
    }
    native(&entry, arch)?;
    let dynamic = launcher::loader_preflight::read_dynamic_section(&entry)
        .map_err(invalid)?
        .ok_or_else(|| invalid("the app ELF cannot be inspected"))?;
    if !dynamic
        .needed
        .iter()
        .any(|name| name == "libruntime_dylib.so")
    {
        return Err(invalid(
            "a purported thin app does not link the shipped toolkit",
        ));
    }
    Ok((entry, Installed { package, record }))
}

fn closure(runtime: &Path, entries: Vec<PathBuf>, arch: &str) -> Result<(), DaemonError> {
    let mut seen = entries.into_iter().collect::<BTreeSet<_>>();
    let mut pending = seen.iter().cloned().collect::<VecDeque<_>>();
    while let Some(path) = pending.pop_front() {
        if seen.len() > 1024 {
            return Err(invalid("native closure exceeds its bound"));
        }
        native(&path, arch)?;
        let dynamic = launcher::loader_preflight::read_dynamic_section(&path)
            .map_err(invalid)?
            .ok_or_else(|| invalid("native closure contains an uninspectable ELF"))?;
        if !dynamic.rpath.is_empty() {
            return Err(invalid(
                "native closure may not override its verified runtime through DT_RPATH",
            ));
        }
        for name in dynamic.needed {
            if name.is_empty() || name.contains(['/', '\\']) || name.starts_with('.') {
                return Err(invalid("an unsafe ELF dependency name"));
            }
            let dependency = within(runtime, &runtime.join("lib").join(name))?;
            if seen.insert(dependency.clone()) {
                if seen.len() > 1024 {
                    return Err(invalid("native closure exceeds its bound"));
                }
                pending.push_back(dependency);
            }
        }
    }
    Ok(())
}

/// Verify the apps+runtime cohort in `layer`, and derive each package's
/// complete archive into a private directory under `staging`.
pub(crate) fn verify(
    layer: &Path,
    base_store: &Path,
    arch: &str,
    staging: &Path,
) -> Result<VerifiedCohort, DaemonError> {
    let runtime_key = publisher(layer, "runtime-bundle-publisher.hex")?;
    let app_key = publisher(layer, "app-publisher.hex")?;
    let runtime_root = layer.join("usr/lib/tilo-runtime");
    let bundles = fs::read_dir(&runtime_root)
        .map_err(invalid)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(invalid)?;
    if bundles.len() != 1 {
        return Err(invalid("the cohort must carry exactly one native toolkit"));
    }
    let bundle = bundles
        .first()
        .ok_or_else(|| invalid("no native toolkit"))?
        .path();
    let bundle = within(layer, &bundle)?;
    let manifest = verify_bundle_with(
        &bundle,
        &VerifyOptions {
            require_publisher: Some(runtime_key),
            require_signature: true,
            require_arch: Some(arch.into()),
        },
    )
    .map_err(invalid)?;
    if runtime_bundle::split_abi_version(&manifest.version).is_none() {
        return Err(invalid("toolkit has no payload-derived native ABI"));
    }
    if bundle.file_name().and_then(|name| name.to_str()) != Some(manifest.version.as_str()) {
        return Err(invalid(
            "runtime directory disagrees with its signed native ABI",
        ));
    }
    let rows = registry(&layer.join("usr/share/tilo-apps"))?;
    let ids = rows
        .entries()
        .iter()
        .map(|row| row.id.as_str())
        .collect::<BTreeSet<_>>();
    if ids.len() != rows.entries().len()
        || registry(base_store)?
            .entries()
            .iter()
            .any(|row| !ids.contains(row.id.as_str()))
    {
        return Err(invalid(
            "the cohort omits a system application or repeats an ID",
        ));
    }
    let runtime = format!("tilo-runtime/{}", manifest.version);
    let admitted = rows
        .entries()
        .iter()
        .map(|row| application(layer, row, &app_key, &runtime, arch))
        .collect::<Result<Vec<_>, _>>()?;
    let entries = admitted
        .iter()
        .map(|(entry, _)| entry.clone())
        .collect::<Vec<_>>();
    for entry in &entries {
        let interpreter = launcher::loader_preflight::read_interpreter(entry)
            .map_err(invalid)?
            .ok_or_else(|| invalid("a thin app has no ELF interpreter"))?;
        if !launcher::sandbox::CANONICAL_LOADER_PATHS.contains(&interpreter.as_str()) {
            return Err(invalid("the interpreter is not a canonical bundled loader"));
        }
        let loader = within(
            &bundle,
            &bundle.join("lib").join(interpreter.trim_start_matches('/')),
        )?;
        native(&loader, arch)?;
        closure(&bundle, vec![loader], arch)?;
    }
    closure(&bundle, entries, arch)?;
    let archives = Staging::create(staging)?;
    let packages = rows
        .entries()
        .iter()
        .zip(&admitted)
        .map(|(row, (_, installed))| {
            Ok(CohortPackage {
                row: row.clone(),
                archive: archive(installed, &app_key, &archives.0)?,
            })
        })
        .collect::<Result<Vec<_>, DaemonError>>()?;
    Ok(VerifiedCohort {
        version: manifest.version,
        packages,
        _archives: archives,
    })
}

#[derive(serde::Deserialize)]
struct SelectionReceipt {
    id: String,
    root: String,
    runtime: String,
    version: String,
    preserved: bool,
    pinned: bool,
    held: bool,
}

pub(crate) fn select(
    apply: &mut dyn crate::apply::Apply,
    session: &crate::apply::Session,
    cohort: &VerifiedCohort,
) -> Result<Vec<tilo_vm_protocol::CohortSelection>, DaemonError> {
    let mut selections = Vec::new();
    for package in &cohort.packages {
        let output = apply.install_cohort_app(session, &package.archive)?;
        let receipts = output
            .lines()
            .filter_map(|line| line.strip_prefix("cohort-selection: "))
            .collect::<Vec<_>>();
        let [encoded] = receipts.as_slice() else {
            return Err(invalid(
                "the installer did not produce one selection receipt",
            ));
        };
        let receipt: SelectionReceipt = serde_json::from_str(encoded).map_err(invalid)?;
        if receipt.id != package.row.id
            || (receipt.preserved && !receipt.held)
            || (!receipt.preserved
                && (receipt.root != package.row.package_id_hex
                    || receipt.version != package.row.version
                    || receipt.runtime != package.row.runtime))
        {
            return Err(invalid(
                "the installer did not select the authenticated cohort package",
            ));
        }
        selections.push(tilo_vm_protocol::CohortSelection {
            id: receipt.id,
            root: receipt.root,
            runtime: receipt.runtime,
            preserved: receipt.preserved,
            pinned: receipt.pinned,
        });
    }
    Ok(selections)
}

#[cfg(test)]
mod tests;
