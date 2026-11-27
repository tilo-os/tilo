//! Installing a native runtime package: its payload is a signed runtime bundle, which lands where
//! the launcher looks for the runtime it provides (`runtimes/<version>` in the device's own
//! store, [`tilo_paths::device_data_dir`]).

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use tilo_app_package::BlobFetcher;

use super::{
    Candidate, Checked, Error, Outcome, Replacement, Report, checked_admission, commit,
    held_below_image, layers,
};
use crate::{Catalog, InstalledPackage, PackageStore};

#[derive(Clone, Copy)]
pub(super) struct Options {
    pub replacement: Replacement,
    pub pinned: bool,
    pub dry_run: bool,
}

/// Payload blobs as files named by hash, streamed: a runtime's libraries are hundreds of megabytes.
pub(super) struct BlobDir(pub PathBuf);

impl tilo_app_package::BlobSink for BlobDir {
    fn accept(
        &mut self,
        hash: &tilo_app_package::BlobHash,
        len: u64,
        bytes: &mut dyn Read,
    ) -> std::io::Result<()> {
        let path = self.0.join(hash.to_hex());
        let mut file = fs::File::create(&path)?;
        std::io::copy(&mut bytes.take(len), &mut file)?;
        Ok(())
    }
}

impl tilo_app_package::BlobSource for BlobDir {
    fn open_blob(
        &self,
        hash: &tilo_app_package::BlobHash,
    ) -> std::io::Result<Option<tilo_app_package::blobs::OpenBlob<'_>>> {
        let path = self.0.join(hash.to_hex());
        match fs::File::open(&path) {
            Ok(file) => Ok(Some(tilo_app_package::blobs::OpenBlob {
                len: file.metadata()?.len(),
                reader: Box::new(std::io::BufReader::new(file)),
            })),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }
}

impl BlobFetcher for BlobDir {
    fn fetch(
        &self,
        hashes: &[tilo_app_package::BlobHash],
        sink: &mut dyn tilo_app_package::BlobSink,
    ) -> std::io::Result<()> {
        for hash in hashes {
            let mut file = fs::File::open(self.0.join(hash.to_hex()))?;
            sink.accept(hash, file.metadata()?.len(), &mut file)?;
        }
        Ok(())
    }
}

/// Record the verified package in the store from blobs already staged, keeping pins and, with
/// `preserve_held`, a version the person holds. False when the hold won.
pub(super) fn record_package(
    store: &PackageStore,
    catalog: &Catalog,
    candidate: &Candidate,
    checked: &Checked,
    pinned: bool,
    preserve_held: bool,
    blobs: &BlobDir,
    report: &mut Report,
) -> Result<bool, Error> {
    let admission = checked_admission(
        checked,
        pinned,
        held_below_image(&layers(catalog, checked.id())?, checked.version()),
        candidate.source.clone(),
    );
    Ok(commit(
        store,
        candidate,
        admission,
        preserve_held,
        Some(blobs),
        report,
    )?
    .is_some())
}

/// Where the launcher reads the runtime publishers it accepts. The launcher's developer override
/// names another list, and the installer follows it so both judge a bundle alike.
fn trusted_runtime_publishers(
    bundle: &Path,
) -> Result<runtime_bundle::component_pins::BundleTrust, Error> {
    let path = runtime_bundle::component_pins::trusted_publishers_file();
    runtime_bundle::component_pins::load_for_bundle(&path, bundle)
        .map_err(|error| Error::RuntimeBundle(error.to_string()))
}

/// Install a runtime package. The bundle must verify as the build the package names, for this
/// machine, signed by a publisher this device trusts — the same checks the launcher makes before
/// mounting it — so an install never leaves a runtime the launcher refuses.
///
/// Each install (or finding it installed) leases the bundle, so collection keeps it through the app
/// install that follows; `pinned` keeps it for good. A replaced bundle leaves the version's directory
/// in one rename before it is deleted, and stays while a running app has it mounted.
pub(super) fn install(
    store: &PackageStore,
    catalog: &Catalog,
    candidate: &Candidate,
    checked: &Checked,
    options: Options,
    payload: Option<&dyn BlobFetcher>,
    report: &mut Report,
) -> Result<(), Error> {
    let provides = checked
        .manifest
        .package
        .provides
        .as_deref()
        .ok_or_else(|| {
            Error::Policy(format!(
                "{} is a runtime package that names no `package.provides`",
                checked.id()
            ))
        })?;
    let (_, version) = provides
        .split_once('/')
        .ok_or_else(|| Error::RuntimeReferenceMalformed(provides.to_string()))?;
    let dest = runtime_bundle::default_bundle_path(version)
        .map_err(|e| Error::RuntimeBundle(e.to_string()))?;
    if options.dry_run {
        report.line(format!("plan: install runtime {provides}"));
        report.line(format!(
            "  package   : {} {}",
            checked.id(),
            checked.version()
        ));
        report.line(format!("  bundle    : {}", dest.display()));
        report.line("(nothing was written)");
        return Ok(());
    }
    let layers = layers(catalog, checked.id())?;
    let existing = layers.running();
    let already_installed =
        existing.is_some_and(|row| row.package_id_hex == checked.meta.id.to_hex());
    super::fits_os(checked)?;
    let root = dest
        .parent()
        .ok_or_else(|| Error::RuntimeBundle(format!("{} has no parent", dest.display())))?;
    fs::create_dir_all(root)?;
    let bundles = runtime_bundle::gc::BundleRoot::new(root);
    let _collection_held_off = bundles.install_lock()?;
    let staging = root.join(super::staging_name(runtime_bundle::gc::STAGING_PREFIX));
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    let result = stage(candidate, &staging, payload, report).and_then(|tree| {
        let manifest = check_bundle(&tree, version)?;
        seal(&tree, &manifest, report);
        if dest.exists()
            && existing.is_some()
            && !already_installed
            && options.replacement == Replacement::Refuse
        {
            return Err(Error::AlreadyInstalled(checked.id().to_string()));
        }
        // Publish the verified signed package through the catalogue and integrity records AppProcess
        // reads, reusing the blobs already fetched: a thin engine must not download its whole
        // payload a second time.
        let blobs = BlobDir(staging.join("blobs"));
        let preserve_held = options.replacement == Replacement::PreserveHeld;
        if !record_package(
            store,
            catalog,
            candidate,
            checked,
            options.pinned,
            preserve_held,
            &blobs,
            report,
        )? {
            return Ok(false);
        }
        let replaced = bundles.retire(version)?;
        fs::rename(&tree, &dest)?;
        if let Some(replaced) = replaced {
            bundles.purge(&replaced, &crate::gc::in_use_now())?;
        }
        Ok(true)
    });
    let _ = fs::remove_dir_all(&staging);
    if !result? {
        return Ok(());
    }
    bundles.lease(version, tilo_clock::unix_now())?;
    if options.pinned {
        bundles.pin(version)?;
    }
    installed(catalog, checked, provides, &dest, already_installed, report)
}

/// Report the runtime package now selected and where its bundle is.
fn installed(
    catalog: &Catalog,
    checked: &Checked,
    provides: &str,
    dest: &Path,
    already_installed: bool,
    report: &mut Report,
) -> Result<(), Error> {
    report.outcome = Outcome::Runtime;
    if let Some(row) = layers(catalog, checked.id())?.running() {
        report.select(row);
    }
    report.line(if already_installed {
        format!("runtime {provides} is already installed; its verified package record is current")
    } else {
        format!("installed: runtime {provides}")
    });
    report.line(format!(
        "  package  : {} {}",
        checked.id(),
        checked.version()
    ));
    report.line(format!("  bundle   : {}", dest.display()));
    Ok(())
}

/// Materialise the package's payload tree under `staging`; returns the tree.
fn stage(
    candidate: &Candidate,
    staging: &Path,
    payload: Option<&dyn BlobFetcher>,
    report: &mut Report,
) -> Result<PathBuf, Error> {
    let blobs = staging.join("blobs");
    fs::create_dir_all(&blobs)?;
    let mut sink = BlobDir(blobs);
    let tapp = tilo_app_package::tapp::read(candidate.reader()?, &mut sink)
        .map_err(|error| Error::Parse(error.to_string()))?;
    // Most of a runtime build's files are in the runtime already installed: copy those, each
    // verified against the hash the signed package names, and fetch only the rest.
    let (missing, reused) =
        crate::runtime_blobs::LocalRuntimeBlobs::installed().copy_into(&tapp.missing(), &sink.0)?;
    if reused > 0 {
        report.line(format!(
            "  reused   : {reused} bytes already on this device"
        ));
    }
    if !missing.is_empty() {
        let fetcher = payload.ok_or_else(|| {
            Error::Policy(format!(
                "the runtime package does not carry {} payload blob(s); name the registry that \
                 offers it with --payload-from",
                missing.len()
            ))
        })?;
        fetcher
            .fetch(&missing, &mut sink)
            .map_err(|error| Error::Store(format!("fetching the runtime's payload: {error}")))?;
    }
    let tree = staging.join("tree");
    tilo_app_package::unpack(&tapp.meta, &sink, &tree)
        .map_err(|error| Error::Store(format!("unpacking the runtime: {error}")))?;
    Ok(tree)
}

/// Enable fs-verity on a fully verified bundle's files, so each launch trusts them by their
/// kernel-measured, publisher-signed digests instead of hashing the whole runtime again. Best
/// effort: without fs-verity on the filesystem, or for a bundle that predates its digests, launches
/// hash the payload as before, which is as safe and only slower.
pub fn seal(tree: &Path, manifest: &runtime_bundle::BundleManifest, report: &mut Report) {
    match runtime_bundle::enable_bundle_verity(tree, manifest) {
        Ok(0) => {}
        Ok(sealed) => {
            report.line(format!("  sealed   : {sealed} file(s) with fs-verity"));
            // Say what a launch will do: a file the kernel measures differently from its signed
            // digest is hashed at every launch.
            let options = runtime_bundle::VerifyOptions::default();
            if let Ok((_, check)) = runtime_bundle::verify_bundle_verity_with(tree, &options)
                && check.hashed > 0
            {
                report.line(format!(
                    "  warning  : {} file(s) do not measure their signed fs-verity digest; each \
                     launch hashes them",
                    check.hashed
                ));
            }
        }
        Err(error) => {
            report.line(format!(
                "  sealed   : no ({error}); each launch hashes the runtime instead"
            ));
        }
    }
}

/// The checks the launcher makes before it mounts a device bundle, which leave the bundle's
/// verification record: the first launch of an app that installs a runtime reads that record instead
/// of hashing what this just proved. Sealing the bundle afterwards moves the files' identities and
/// the record with them, which costs nothing: a sealed bundle is proven by the kernel, and one the
/// filesystem would not seal keeps the record.
fn check_bundle(tree: &Path, version: &str) -> Result<runtime_bundle::BundleManifest, Error> {
    let records = runtime_bundle::record::directory();
    let manifest = runtime_bundle::verify_bundle_recorded_with(
        tree,
        &runtime_bundle::VerifyOptions {
            require_publisher: None,
            require_signature: true,
            require_arch: Some(runtime_bundle::host_arch().to_string()),
        },
        records.as_deref(),
    )
    .map(|(manifest, _)| manifest)
    .map_err(|e| Error::RuntimeBundle(e.to_string()))?;
    if manifest.version != version {
        return Err(Error::RuntimeBundle(format!(
            "the package says it provides {version}, but its bundle is {}",
            manifest.version
        )));
    }
    trusted_runtime_publishers(tree)?
        .check(&manifest)
        .map_err(|error| Error::RuntimeBundle(error.to_string()))?;
    Ok(manifest)
}

/// A version string and a retained directory cannot establish that an exact runtime package remains
/// installed, so replay its signed package record and compare the mounted bundle's signed manifest
/// with that package's payload.
pub(super) fn verify_current(entry: &InstalledPackage, checked: &Checked) -> Result<(), Error> {
    if entry.package_id_hex != checked.meta.id.to_hex() {
        return Err(Error::Policy(
            "the offered runtime changes the installed version's signed root".into(),
        ));
    }
    let report = crate::integrity::verify(entry, &entry.payload_root)
        .map_err(|error| Error::Store(error.to_string()))?;
    if !report.is_intact() {
        return Err(Error::RuntimeBundle(
            "the installed runtime package payload is damaged".into(),
        ));
    }
    let version = checked
        .manifest
        .package
        .provides
        .as_deref()
        .and_then(|runtime| runtime.split_once('/').map(|(_, version)| version))
        .ok_or_else(|| {
            Error::RuntimeBundle("the runtime package names no valid provided runtime".into())
        })?;
    let bundle = runtime_bundle::default_bundle_path(version)
        .map_err(|error| Error::RuntimeBundle(error.to_string()))?;
    check_bundle(&bundle, version)?;
    let manifest = runtime_bundle::BUNDLE_MANIFEST_FILENAME;
    if fs::read(bundle.join(manifest))? != fs::read(entry.payload_root.join(manifest))? {
        return Err(Error::RuntimeBundle(
            "the installed bundle differs from its signed package".into(),
        ));
    }
    Ok(())
}
