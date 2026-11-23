//! The model runtime an app's model module links against, fetched when this
//! device lacks it.
//!
//! An app's `model.wasm` is split against a model runtime it names by hash
//! (`tilo-model-engine`), and the runtime ships once, in the runtime bundle
//! the app's package names (`[package].runtime`, `tilo-runtime/<version>`),
//! as `lib/app-model-runtime`. A device built from another release has
//! another runtime, so [`fetch`] takes that directory out of the runtime
//! package the registry offers for the version the app names: the package is
//! the one the registry's signed entry names (its length, BLAKE3, root and
//! publisher), its signature verifies, the bundle manifest it carries is
//! signed by a runtime publisher this device trusts
//! ([`runtime_bundle::component_pins`]), and each file taken is the one that
//! manifest lists. Only those files are downloaded: the registry serves a
//! runtime package's payload as blobs ([`app_registry::HttpBlobs`]), and the
//! rest of the bundle (hundreds of megabytes of libraries) is never asked for.
//!
//! The runtime is kept under [`cache_root`], in a directory named by its
//! hash, so a device asks the registry once per runtime, across restarts. At
//! most [`KEPT`] are kept; the least recently used goes first. A runtime that
//! could not be had is not asked for again for [`RETRY_AFTER`], so an app
//! whose runtime no registry offers does not cost a download per action.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use tilo_app_package::{BlobFetcher as _, BlobHash, BlobSink};

/// The directory of a runtime bundle the model runtime is kept in, and the
/// name of the cache under the store's cache directory.
pub const RUNTIME_DIR: &str = "app-model-runtime";

/// How many fetched runtimes are kept on disk: one per release whose apps a
/// workspace still uses, and a release's apps all name the same one.
pub const KEPT: usize = 4;

/// How long a runtime that could not be fetched is refused without asking
/// the registry again.
pub const RETRY_AFTER: Duration = Duration::from_secs(5 * 60);

/// The most bytes a model runtime may hold: its modules are a few megabytes.
const MAX_RUNTIME_BYTES: u64 = 64 * 1024 * 1024;

/// Where fetched runtimes are kept: `<store cache>/app-model-runtime`.
#[must_use]
pub fn cache_root() -> Option<PathBuf> {
    crate::config::cache_dir().map(|dir| dir.join(RUNTIME_DIR))
}

/// The kept runtime `hash`, when one was fetched before.
#[must_use]
pub fn cached(hash: &str) -> Option<PathBuf> {
    if !is_hash(hash) {
        return None;
    }
    let dir = cache_root()?.join(hash);
    dir.join("runtime.json").is_file().then_some(dir)
}

/// The directory holding model runtime `hash`, as the runtime bundle
/// `runtime` (`tilo-runtime/<version>`, what the app's package names)
/// carries it: kept from before, else fetched from the first enabled
/// registry that offers that runtime, verified, and kept.
///
/// `check` is handed the runtime's directory before it is kept, and refuses
/// one that is not runtime `hash` (the engine reads it and computes its hash
/// from its bytes); a refused runtime is not kept.
///
/// Blocking, and it starts its own runtime for the blob downloads: call it
/// off an async executor's threads.
///
/// # Errors
/// Why the runtime could not be had: no registry offers it, the download
/// failed, or what was offered does not verify.
pub fn fetch(
    runtime: &str,
    hash: &str,
    check: &dyn Fn(&Path) -> Result<(), String>,
) -> Result<PathBuf, String> {
    static FAILED: Mutex<Option<HashMap<String, (Instant, String)>>> = Mutex::new(None);
    static FETCHING: Mutex<()> = Mutex::new(());
    if !is_hash(hash) {
        return Err(format!("{hash:?} does not name a model runtime"));
    }
    if let Some(dir) = cached(hash) {
        touch(&dir);
        return Ok(dir);
    }
    let key = format!("{runtime}\0{hash}");
    let refused_before = FAILED
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        .and_then(|failed| failed.get(&key).cloned())
        .filter(|(at, _)| at.elapsed() < RETRY_AFTER);
    if let Some((_, why)) = refused_before {
        return Err(why);
    }
    // One fetch at a time: the second asker of a runtime finds it kept.
    let _fetching = FETCHING.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some(dir) = cached(hash) {
        return Ok(dir);
    }
    let root = cache_root().ok_or("this device has no cache directory")?;
    let fetched = fetch_into(&root, runtime, hash, check);
    match &fetched {
        Ok(_) => prune(&root),
        Err(why) => {
            FAILED
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get_or_insert_with(HashMap::new)
                .insert(key, (Instant::now(), why.clone()));
        }
    }
    fetched
}

fn fetch_into(
    root: &Path,
    runtime: &str,
    hash: &str,
    check: &dyn Fn(&Path) -> Result<(), String>,
) -> Result<PathBuf, String> {
    let mut refusals = Vec::new();
    for registry in crate::config::load_registries().enabled() {
        let offer = match crate::runtime_offer(runtime, &registry.name) {
            Ok(Some(offer)) => offer,
            Ok(None) => continue,
            Err(error) => {
                refusals.push(format!("{}: {error}", registry.name));
                continue;
            }
        };
        let staging = Staging::new(root, hash)?;
        match take(&offer, &registry.base_url, runtime, staging.path())
            .and_then(|dir| check(&dir).map(|()| dir))
        {
            Ok(dir) => return keep(&dir, &root.join(hash)),
            Err(error) => refusals.push(format!("{}: {error}", registry.name)),
        }
    }
    Err(if refusals.is_empty() {
        format!("no enabled repository offers {runtime}")
    } else {
        refusals.join("; ")
    })
}

/// Download `offer` (a runtime package providing `runtime`) into `staging`,
/// verify it, and write its model runtime beside it; returns where.
fn take(
    offer: &crate::CatalogueApp,
    base_url: &str,
    runtime: &str,
    staging: &Path,
) -> Result<PathBuf, String> {
    let package = staging.join("package.tapp");
    crate::install::fetch_verified(offer, &package, &mut |_| {})
        .map_err(|error| error.to_string())?;
    let file = std::fs::File::open(&package).map_err(|error| error.to_string())?;
    let mut blobs = Wanted::default();
    let tapp = tilo_app_package::tapp::read(std::io::BufReader::new(file), &mut blobs)
        .map_err(|error| format!("the runtime package: {error}"))?;
    let meta = &tapp.meta;
    // The package is the one the registry's signed entry names, signed by
    // the publisher it names.
    if !meta
        .id
        .to_hex()
        .eq_ignore_ascii_case(&offer.tapp_root_hash_hex)
    {
        return Err("the runtime package is not the one the registry entry names".into());
    }
    if !hex::encode(meta.publisher()).eq_ignore_ascii_case(&offer.publisher_pubkey_hex) {
        return Err("the runtime package is signed by another publisher than its entry's".into());
    }
    let tilo_app_package::verify::Verification::Trusted { manifest } =
        tilo_app_package::verify::verify(meta, &[meta.publisher()])
    else {
        return Err("the runtime package's signature does not verify".into());
    };
    if manifest.package.kind != tilo_app_package::Kind::Runtime
        || manifest.package.provides.as_deref() != Some(runtime)
    {
        return Err(format!("the runtime package does not provide {runtime}"));
    }
    let entries = &meta.collection.entries;
    let prefix = format!("lib/{RUNTIME_DIR}/");
    let mut wanted: BTreeMap<&str, BlobHash> = entries
        .iter()
        .filter(|(path, _)| path.starts_with(&prefix))
        .map(|(path, hash)| (path.as_str(), *hash))
        .collect();
    let bundle_hash = *entries
        .get(runtime_bundle::BUNDLE_MANIFEST_FILENAME)
        .ok_or("the runtime package carries no bundle manifest")?;
    wanted.insert(runtime_bundle::BUNDLE_MANIFEST_FILENAME, bundle_hash);
    let missing: Vec<BlobHash> = wanted
        .values()
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|hash| !blobs.kept.contains_key(hash))
        .collect();
    if !missing.is_empty() {
        blobs.wanted.extend(missing.iter().copied());
        // The blob fetch starts its own runtime, and refuses to on a thread
        // that is inside one: a server calls this from a blocking task of its
        // executor, which is. A plain thread of its own is not.
        std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    app_registry::HttpBlobs::new(base_url)
                        .fetch(&missing, &mut blobs)
                        .map_err(|error| format!("the runtime's files: {error}"))
                })
                .join()
                .map_err(|_| "the runtime's files: the download thread panicked".to_owned())
        })??;
    }
    let carried = verify_bundle(&blobs.kept, &wanted, bundle_hash, runtime)?;
    let dir = staging.join(RUNTIME_DIR);
    std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    let mut total = 0_u64;
    for (name, hash) in carried {
        let bytes = blobs
            .kept
            .get(&hash)
            .ok_or_else(|| format!("the registry did not serve blob {}", hash.to_hex()))?;
        total = total.saturating_add(bytes.len() as u64);
        if total > MAX_RUNTIME_BYTES {
            return Err(format!(
                "the model runtime is larger than {MAX_RUNTIME_BYTES} bytes"
            ));
        }
        std::fs::write(dir.join(name), bytes).map_err(|error| error.to_string())?;
    }
    Ok(dir)
}

/// The model runtime's files the package carries (`wanted`, by payload
/// path), by name, once the bundle manifest among `blobs` (`bundle`) is
/// signed by a runtime publisher this device trusts, exactly as an
/// installed bundle is, is the bundle of `runtime`, and signs each of them.
fn verify_bundle(
    blobs: &BTreeMap<BlobHash, Vec<u8>>,
    wanted: &BTreeMap<&str, BlobHash>,
    bundle: BlobHash,
    runtime: &str,
) -> Result<Vec<(String, BlobHash)>, String> {
    let bundle: runtime_bundle::BundleManifest = blobs
        .get(&bundle)
        .ok_or("the registry did not serve the bundle manifest")
        .map_err(str::to_owned)
        .and_then(|bytes| std::str::from_utf8(bytes).map_err(|error| error.to_string()))
        .and_then(|text| toml::from_str(text).map_err(|error| error.to_string()))
        .map_err(|error| format!("the runtime's bundle manifest: {error}"))?;
    runtime_bundle::verify_bundle_manifest(
        &bundle,
        &runtime_bundle::VerifyOptions {
            require_publisher: None,
            require_signature: true,
            require_arch: Some(runtime_bundle::host_arch().to_string()),
        },
    )
    .map_err(|error| format!("the runtime bundle: {error}"))?;
    if runtime.split_once('/').map(|(_, version)| version) != Some(bundle.version.as_str()) {
        return Err(format!(
            "the package provides {runtime}, but its bundle is {}",
            bundle.version
        ));
    }
    runtime_bundle::component_pins::load_for_manifest(
        &runtime_bundle::component_pins::trusted_publishers_file(),
        &bundle,
    )
    .and_then(|trust| trust.check(&bundle))
    .map_err(|error| format!("the runtime bundle: {error}"))?;
    let prefix = format!("lib/{RUNTIME_DIR}/");
    let listed: BTreeMap<&str, &str> = bundle
        .files
        .iter()
        .filter_map(|file| Some((file.path.strip_prefix(&prefix)?, file.hash.as_str())))
        .collect();
    let carried: BTreeMap<&str, BlobHash> = wanted
        .iter()
        .filter_map(|(path, hash)| Some((path.strip_prefix(&prefix)?, *hash)))
        .collect();
    if listed.keys().ne(carried.keys()) {
        return Err("the runtime package's model runtime is not the one its bundle lists".into());
    }
    if listed.is_empty() {
        return Err(format!("{runtime} carries no model runtime"));
    }
    carried
        .into_iter()
        .map(|(name, hash)| {
            if name.contains('/') || name.starts_with('.') {
                return Err(format!("the model runtime holds {name:?}, not a module"));
            }
            if !listed
                .get(name)
                .is_some_and(|signed| signed.eq_ignore_ascii_case(&hash.to_hex()))
            {
                return Err(format!("{name} is not the file the bundle manifest signs"));
            }
            Ok((name.to_owned(), hash))
        })
        .collect()
}

/// The payload blobs taken from a package: the bundle manifest and the
/// model runtime's files, each checked against its hash. Every other blob
/// a package carries inline is read past.
#[derive(Default)]
struct Wanted {
    wanted: BTreeSet<BlobHash>,
    kept: BTreeMap<BlobHash, Vec<u8>>,
}

impl BlobSink for Wanted {
    fn metadata(&mut self, meta: &tilo_app_package::PackageMeta) -> std::io::Result<()> {
        let prefix = format!("lib/{RUNTIME_DIR}/");
        self.wanted.extend(
            meta.collection
                .entries
                .iter()
                .filter(|(path, _)| {
                    path.starts_with(&prefix)
                        || path.as_str() == runtime_bundle::BUNDLE_MANIFEST_FILENAME
                })
                .map(|(_, hash)| *hash),
        );
        Ok(())
    }

    fn accept(&mut self, hash: &BlobHash, len: u64, bytes: &mut dyn Read) -> std::io::Result<()> {
        if !self.wanted.contains(hash) {
            std::io::copy(&mut bytes.take(len), &mut std::io::sink())?;
            return Ok(());
        }
        if len > MAX_RUNTIME_BYTES {
            return Err(std::io::Error::other(format!(
                "blob {} is larger than a model runtime",
                hash.to_hex()
            )));
        }
        let mut kept = Vec::with_capacity(usize::try_from(len).unwrap_or(0));
        bytes.take(len).read_to_end(&mut kept)?;
        if BlobHash::of(&kept) != *hash {
            return Err(std::io::Error::other(format!(
                "blob {} is not the bytes its hash names",
                hash.to_hex()
            )));
        }
        self.kept.insert(*hash, kept);
        Ok(())
    }
}

/// A fetch's own directory beside the kept runtimes, removed when it ends.
struct Staging(PathBuf);

impl Staging {
    fn new(root: &Path, hash: &str) -> Result<Self, String> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos());
        let dir = root.join(format!(".fetch-{hash}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).map_err(|error| format!("{}: {error}", dir.display()))?;
        Ok(Self(dir))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Move the runtime at `dir` (inside a staging directory) to `kept`.
fn keep(dir: &Path, kept: &Path) -> Result<PathBuf, String> {
    match std::fs::rename(dir, kept) {
        Ok(()) => Ok(kept.to_owned()),
        // Another process kept it first: that one is as good.
        Err(_) if kept.join("runtime.json").is_file() => Ok(kept.to_owned()),
        Err(error) => Err(format!("{}: {error}", kept.display())),
    }
}

/// Keep the [`KEPT`] most recently used runtimes under `root`.
fn prune(root: &Path) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    let mut kept: Vec<(std::time::SystemTime, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_str().is_some_and(is_hash))
        .filter_map(|entry| {
            let used = entry.metadata().ok()?.modified().ok()?;
            Some((used, entry.path()))
        })
        .collect();
    kept.sort_by_key(|(used, _)| std::cmp::Reverse(*used));
    for (_, dir) in kept.into_iter().skip(KEPT) {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// Mark `dir` as just used, for [`prune`].
fn touch(dir: &Path) {
    if let Ok(file) = std::fs::File::open(dir) {
        let _ = file.set_modified(std::time::SystemTime::now());
    }
}

/// Whether `name` is a runtime hash: 64 lower-case hex digits.
fn is_hash(name: &str) -> bool {
    name.len() == 64
        && name
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}
