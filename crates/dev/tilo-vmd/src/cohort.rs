//! Evidence that an authenticated component carries its own complete thin graph.
//! Neither a component label nor a claimed profile grants this exception.

use std::collections::{BTreeSet, VecDeque};
use std::fs;
use std::io::Read as _;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Component, Path, PathBuf};

use package_store::registry::{InstalledPackage, Registry};
use runtime_bundle::{VerifyOptions, verify_bundle_with};

use crate::error::DaemonError;

/// Maintained installer snapshot from the exact account/candidate namespace.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectedApps {
    pub apps: Vec<InstalledPackage>,
    pub runtime_paths: std::collections::BTreeMap<String, PathBuf>,
}

pub(crate) struct VerifiedCohort {
    pub(crate) version: String,
    pub(crate) source_inventory: Option<String>,
    pub(crate) packages: Vec<CohortPackage>,
}

impl VerifiedCohort {
    /// The applications this cohort selects into the session user's store.
    pub(crate) fn ids(&self) -> impl Iterator<Item = &str> {
        self.packages.iter().map(|package| package.row.id.as_str())
    }

    /// A no-op requires every authenticated cohort package to be selected,
    /// not merely present in the system store. A held or missing selection
    /// takes the ordinary installer path instead.
    pub(crate) fn selections_identical(&self, selected: &SelectedApps) -> bool {
        self.packages.iter().all(|package| {
            let mut rows = selected.apps.iter().filter(|row| row.id == package.row.id);
            rows.next().is_some_and(|row| {
                row.package_id_hex == package.row.package_id_hex
                    && row.version == package.row.version
                    && row.runtime == package.row.runtime
                    && row.publisher_key_hex == package.row.publisher_key_hex
            }) && rows.next().is_none()
        })
    }
}

pub(crate) struct CohortPackage {
    row: InstalledPackage,
    archive: PathBuf,
}

fn archive(layer: &Path, row: &InstalledPackage, publisher: &str) -> Result<PathBuf, DaemonError> {
    let store = layer.join("usr/share/tilo-apps");
    let path = within(
        &store,
        &store.join("cohort").join(format!("{}.tapp", row.id)),
    )?;
    let metadata = fs::symlink_metadata(&path).map_err(invalid)?;
    if !metadata.is_file() || metadata.uid() != 0 || metadata.mode() & 0o022 != 0 {
        return Err(invalid("a cohort archive is not immutable root-owned data"));
    }
    let bytes = bounded(&path, tilo_app_package::MAX_APP_TAPP_BYTES)?;
    let tapp = tilo_app_package::tapp::read(bytes.as_slice(), &mut tilo_app_package::DiscardBlobs)
        .map_err(invalid)?;
    let key: [u8; 32] = hex::decode(publisher)
        .map_err(invalid)?
        .try_into()
        .map_err(|_| invalid("malformed cohort publisher"))?;
    if tapp.meta.id.to_hex() != row.package_id_hex
        || !matches!(
            tilo_app_package::verify(&tapp.meta, &[key]),
            tilo_app_package::Verification::Trusted { .. }
        )
        || !tapp.missing().is_empty()
    {
        return Err(invalid(
            "cohort archive is not the complete authenticated package",
        ));
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
) -> Result<PathBuf, DaemonError> {
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
    Ok(entry)
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

/// The engines a cohort ships beside its toolkit (the runtimes its apps pin):
/// each signed by the image's browser-runtime publisher, for this
/// architecture, in the directory its signed version names.
fn verify_engines(layer: &Path, engines: &[&fs::DirEntry], arch: &str) -> Result<(), DaemonError> {
    if engines.is_empty() {
        return Ok(());
    }
    let engine_key = publisher(layer, "browser-runtime-publisher.hex")?;
    for engine in engines {
        let engine = within(layer, &engine.path())?;
        let manifest = verify_bundle_with(
            &engine,
            &VerifyOptions {
                require_publisher: Some(engine_key.clone()),
                require_signature: true,
                require_arch: Some(arch.into()),
            },
        )
        .map_err(invalid)?;
        if engine.file_name().and_then(|name| name.to_str()) != Some(manifest.version.as_str()) {
            return Err(invalid(
                "an engine directory disagrees with its signed version",
            ));
        }
    }
    Ok(())
}

/// Membership is part of the signed toolkit payload, not an editable registry.
/// Replacing that read-only system registry retires obsolete image packages;
/// account-owned selections are deliberately not removed or rewritten here.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SystemMembership {
    pub(crate) schema: u32,
    pub(crate) source_inventory: String,
    pub(crate) apps: Vec<String>,
}

fn membership(bundle: &Path) -> Result<Option<SystemMembership>, DaemonError> {
    let path = bundle.join("lib/system-app-cohort.json");
    if !path.try_exists().map_err(invalid)? {
        return Ok(None);
    }
    let membership: SystemMembership =
        serde_json::from_slice(&bounded(&within(bundle, &path)?, 128 * 1024)?).map_err(invalid)?;
    if membership.schema != 1
        || membership.source_inventory.len() != 64
        || !membership
            .source_inventory
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || membership.apps.is_empty()
        || membership.apps.len() > 512
        || membership
            .apps
            .windows(2)
            .any(|pair| matches!(pair, [a, b] if a >= b))
    {
        return Err(invalid("invalid signed system membership"));
    }
    Ok(Some(membership))
}

/// The cohort's one native toolkit, named by its payload-derived ABI, after
/// verifying every other bundle as an engine the cohort's apps pin (the
/// Browser's), shipped at the exact version so the swapped apps find it.
fn toolkit_after_engines(layer: &Path, arch: &str) -> Result<PathBuf, DaemonError> {
    let bundles = fs::read_dir(layer.join("usr/lib/tilo-runtime"))
        .map_err(invalid)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(invalid)?;
    let (toolkits, engines): (Vec<_>, Vec<_>) = bundles.iter().partition(|entry| {
        entry
            .file_name()
            .to_str()
            .is_some_and(|name| runtime_bundle::split_abi_version(name).is_some())
    });
    if toolkits.len() != 1 {
        return Err(invalid("the cohort must carry exactly one native toolkit"));
    }
    verify_engines(layer, &engines, arch)?;
    Ok(toolkits
        .first()
        .ok_or_else(|| invalid("no native toolkit"))?
        .path())
}

pub(crate) fn verify(
    layer: &Path,
    base_store: &Path,
    arch: &str,
) -> Result<VerifiedCohort, DaemonError> {
    let runtime_key = publisher(layer, "runtime-bundle-publisher.hex")?;
    let app_key = publisher(layer, "app-publisher.hex")?;
    let bundle = toolkit_after_engines(layer, arch)?;
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
    let membership = membership(&bundle)?;
    let incomplete = match &membership {
        Some(membership) => {
            membership
                .apps
                .iter()
                .map(String::as_str)
                .collect::<BTreeSet<_>>()
                != ids
        }
        // Legacy layers cannot authorize retirement. Preserve their stricter
        // image-superset admission rather than silently ignoring absent apps.
        None => registry(base_store)?
            .entries()
            .iter()
            .any(|row| !ids.contains(row.id.as_str())),
    };
    if ids.len() != rows.entries().len() || incomplete {
        return Err(invalid(
            "the cohort omits a system application or repeats an ID",
        ));
    }
    let runtime = format!("tilo-runtime/{}", manifest.version);
    let entries = rows
        .entries()
        .iter()
        .map(|row| application(layer, row, &app_key, &runtime, arch))
        .collect::<Result<Vec<_>, _>>()?;
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
    let packages = rows
        .entries()
        .iter()
        .map(|row| {
            Ok(CohortPackage {
                row: row.clone(),
                archive: archive(layer, row, &app_key)?,
            })
        })
        .collect::<Result<Vec<_>, DaemonError>>()?;
    Ok(VerifiedCohort {
        version: manifest.version,
        source_inventory: membership.map(|membership| membership.source_inventory),
        packages,
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
    select_into(apply, session, cohort, None, &mut selections)?;
    Ok(selections)
}

pub(crate) fn select_into(
    apply: &mut dyn crate::apply::Apply,
    session: &crate::apply::Session,
    cohort: &VerifiedCohort,
    layers: Option<&[PathBuf]>,
    selections: &mut Vec<tilo_vm_protocol::CohortSelection>,
) -> Result<(), DaemonError> {
    for package in &cohort.packages {
        let output = match layers {
            Some(layers) => apply.install_cohort_app_in_view(session, &package.archive, layers)?,
            None => apply.install_cohort_app(session, &package.archive)?,
        };
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
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests;
