//! Keep the shared app runtime's pages in memory.
//!
//! Every thin app maps the same shared runtime, `libruntime_dylib.so`, and
//! the libraries it needs from its bundle (Rust's `libstd`, the C library,
//! the loader). Warm, those pages are already in the page cache and an app's
//! start only relocates them; cold, the loader pages them in from disk before
//! `main`, which measured 167 ms of a cold launch on the Yoga, more than the
//! whole warm launch. Pages nobody maps are also the first the kernel
//! reclaims, so an app started after a while of other work pays part of that
//! again.
//!
//! `tilo-runtime-resident.service` runs `tilo-launcher runtime-resident`
//! once per boot. It maps those libraries of every runtime bundle the
//! system's registry names, read-only, and locks them into memory, then
//! sleeps until it is stopped. That reads each file once, sequentially, at
//! idle I/O priority, instead of in scattered faults on an app's critical
//! path, and the locked pages stay in the page cache, where every app's own
//! mapping of the same file finds them: one copy, shared by every app of
//! every person on the machine.
//!
//! It is a mapping and nothing else. The process maps the files without
//! execute permission, never runs their code, holds no app state and starts
//! no app: what it changes is only where the bytes the loader reads come
//! from. A bundle the registry no longer names is unmapped when the service
//! restarts (after an extension merge replaces `/usr`).
//!
//! A runtime a person installed into their own store is kept by the same
//! command in their session (`tilo-launcher runtime-resident --user`, the
//! user unit `tilo-runtime-resident.service`, restarted by
//! `tilo-runtime-resident.path` when their registry changes). Their store is
//! writable by their own processes, so nothing there is mapped by path:
//!
//! - a runtime is chosen by its verified identity. An ABI-bound version names
//!   its payload's digest, and the launcher runs apps of such a version on
//!   the system's bundle of it when there is one (`run::runtime_candidates`),
//!   which this service already keeps; only the person's other runtimes are
//!   theirs to keep, and two names for one root hash are kept once;
//! - the bundle is verified as a launch verifies it (signature, trusted
//!   publisher, every byte);
//! - each library is opened once, and kept only when the kernel measures, on
//!   that descriptor, the fs-verity digest the publisher signed for it. Such
//!   a file is immutable, so the pages kept are the signed pages for as long
//!   as they are kept. A file without fs-verity is left to the page cache: the
//!   bytes a path names could change after any check.
//!
//! A person's session cannot lock more than its `RLIMIT_MEMLOCK` (8 MiB by
//! default); past that the files stay mapped and read in, which keeps them
//! in the page cache as long as memory is not short, without pinning them.
//!
//! Engine libraries (`crates/runtime/engines/`) and the media libraries the
//! runtime loads on first use stay out: only the apps that use them map them.

use std::collections::{BTreeSet, VecDeque};
use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

use crate::loader_preflight::read_dynamic_section;

/// Where the image installs runtime bundles.
pub const SYSTEM_RUNTIME_ROOT: &str = "/usr/lib/tilo-runtime";
/// The runtime library every thin app names `DT_NEEDED`.
pub const RUNTIME_LIBRARY: &str = "libruntime_dylib.so";
/// The prefix of a registry entry's runtime reference (`tilo-runtime/<version>`).
const RUNTIME_REFERENCE_PREFIX: &str = "tilo-runtime/";
/// A bound on one bundle's library closure; a runtime needs about a dozen.
const MAX_LIBRARIES: usize = 64;

/// The runtime versions the registry's entries name, each once.
pub fn registered_versions(registry: &package_store::Registry) -> BTreeSet<String> {
    registry
        .entries()
        .iter()
        .filter_map(|entry| entry.runtime.strip_prefix(RUNTIME_REFERENCE_PREFIX))
        .filter(|version| crate::run::valid_runtime_version(version))
        .map(str::to_owned)
        .collect()
}

/// The files of `bundle` every app of it maps at exec: the runtime library
/// and, transitively, each `DT_NEEDED` library the bundle's `lib/` holds.
/// Libraries outside the bundle (none, for a complete one) are left to the
/// system. Empty when the bundle has no runtime library.
///
/// # Errors
///
/// Returns an I/O error from reading a library's dynamic section.
pub fn bundle_libraries(bundle: &Path) -> io::Result<Vec<PathBuf>> {
    let lib = bundle.join("lib");
    let mut seen = BTreeSet::new();
    let mut found = Vec::new();
    let mut pending = VecDeque::from([RUNTIME_LIBRARY.to_owned()]);
    while let Some(name) = pending.pop_front() {
        if found.len() >= MAX_LIBRARIES || !seen.insert(name.clone()) {
            continue;
        }
        // A soname is one path component; anything else is not ours to map.
        if name.contains('/') || name == "." || name == ".." {
            continue;
        }
        let path = lib.join(&name);
        if !path.symlink_metadata().is_ok_and(|meta| meta.is_file()) {
            continue;
        }
        if let Some(dynamic) = read_dynamic_section(&path)? {
            pending.extend(dynamic.needed);
        }
        found.push(path);
    }
    Ok(found)
}

/// Every file to keep resident: the libraries of each registered runtime
/// version installed under `runtime_root`, each once.
///
/// # Errors
///
/// Returns an I/O error from reading a library's dynamic section.
pub fn resident_files(
    versions: &BTreeSet<String>,
    runtime_root: &Path,
) -> io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for version in versions {
        files.extend(bundle_libraries(&runtime_root.join(version))?);
    }
    Ok(files)
}

/// The registry of a person's own store (`$XDG_DATA_HOME/tilo-apps`), whose
/// entries name the runtimes their apps run on.
pub fn user_registry() -> Option<PathBuf> {
    package_store::layout::default_user_root()
        .map(|root| package_store::Layout::new(root).registry())
}

/// Where a person's own runtime bundles are installed: the root the launcher
/// looks in first ([`runtime_bundle::default_bundle_path_from`]).
pub fn user_runtime_root() -> Option<PathBuf> {
    runtime_bundle::bundle_root_from(package_store::dev_env::dev_override(
        runtime_bundle::RUNTIME_DIR_ENV,
    ))
    .ok()
}

/// The runtime versions of `registered` a person's session keeps itself:
/// all but the ABI-bound versions the system has a bundle of under
/// `system_root`. Such a version names its payload's digest, the launcher
/// runs its apps on the system's bundle (`run::runtime_candidates`), and the
/// system service keeps that one.
pub fn user_kept_versions(registered: &BTreeSet<String>, system_root: &Path) -> BTreeSet<String> {
    registered
        .iter()
        .filter(|version| {
            runtime_bundle::split_abi_version(version).is_none()
                || !system_root
                    .join(version)
                    .join(runtime_bundle::BUNDLE_MANIFEST_FILENAME)
                    .exists()
        })
        .cloned()
        .collect()
}

/// One library of a person's runtime, open, and proven to be the bytes the
/// publisher signed.
#[derive(Debug)]
pub struct VerifiedFile {
    /// Where it was opened; reported, never reopened.
    pub path: PathBuf,
    pub file: File,
}

/// Why a library of a person's runtime is not kept.
#[derive(Debug, PartialEq, Eq)]
pub enum Unkept {
    /// The manifest carries no fs-verity signature, so no digest vouches for
    /// any file.
    NoVeritySignature,
    /// The kernel did not measure the signed digest on the opened file (no
    /// fs-verity on it or its filesystem, or other bytes).
    NotVouched(PathBuf),
}

/// The libraries of the person's runtime `bundle`, verified as `manifest`,
/// that every app of it maps at exec: the runtime library and, transitively,
/// each `DT_NEEDED` library the manifest lists under `lib/`. Each is opened
/// once (never following a link) and kept only when `vouch` proves the open
/// file is the one the manifest describes; its `DT_NEEDED` entries are read
/// from that same descriptor.
///
/// # Errors
///
/// Returns an I/O error from reading a vouched library's dynamic section.
pub fn verified_user_libraries(
    bundle: &Path,
    manifest: &runtime_bundle::BundleManifest,
    vouch: impl Fn(&File, &runtime_bundle::BundleFile) -> bool,
) -> io::Result<(Vec<VerifiedFile>, Vec<Unkept>)> {
    let mut kept = Vec::new();
    let mut unkept = Vec::new();
    if manifest.verity_signature_hex.is_none() {
        unkept.push(Unkept::NoVeritySignature);
        return Ok((kept, unkept));
    }
    let listed: std::collections::BTreeMap<&str, &runtime_bundle::BundleFile> = manifest
        .files
        .iter()
        .map(|file| (file.path.as_str(), file))
        .collect();
    let mut seen = BTreeSet::new();
    let mut pending = VecDeque::from([RUNTIME_LIBRARY.to_owned()]);
    while let Some(name) = pending.pop_front() {
        if kept.len() >= MAX_LIBRARIES || !seen.insert(name.clone()) {
            continue;
        }
        if name.contains('/') || name == "." || name == ".." {
            continue;
        }
        let relative = format!("lib/{name}");
        let Some(expected) = listed.get(relative.as_str()) else {
            // Not part of the bundle: the loader finds it elsewhere.
            continue;
        };
        let path = bundle.join(&relative);
        let Ok(file) = open_no_follow(&path) else {
            unkept.push(Unkept::NotVouched(path));
            continue;
        };
        if !vouch(&file, expected) {
            unkept.push(Unkept::NotVouched(path));
            continue;
        }
        if let Some(dynamic) = crate::loader_preflight::read_dynamic_section_of(&file)? {
            pending.extend(dynamic.needed);
        }
        kept.push(VerifiedFile { path, file });
    }
    Ok((kept, unkept))
}

/// Whether the kernel vouches for `file` as `expected`: a regular file of the
/// listed size whose fs-verity digest, measured on this descriptor, is the
/// signed one. Reads no file data.
pub fn verity_vouches(file: &File, expected: &runtime_bundle::BundleFile) -> bool {
    let Some(digest) = expected.verity.as_deref() else {
        return false;
    };
    file.metadata()
        .is_ok_and(|meta| meta.is_file() && meta.len() == expected.size)
        && runtime_bundle::verity::measure(file)
            .is_some_and(|measured| hex::encode(measured) == digest)
}

fn open_no_follow(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
}

/// What a person's session keeps: for each runtime of `versions` installed
/// under `user_root` that `verify` accepts (the launch's policy,
/// [`crate::run::verified_user_bundle`]), the libraries `vouch` proves
/// ([`verified_user_libraries`]); each runtime identity (root hash) once,
/// however many versions name it. Returns them with what was left out and
/// why, for the log.
pub fn user_resident_files(
    versions: &BTreeSet<String>,
    user_root: &Path,
    verify: impl Fn(&Path) -> Result<runtime_bundle::BundleManifest, String>,
    vouch: impl Fn(&File, &runtime_bundle::BundleFile) -> bool,
) -> (Vec<VerifiedFile>, Vec<String>) {
    let mut identities = BTreeSet::new();
    let mut files = Vec::new();
    let mut problems = Vec::new();
    for version in versions {
        let bundle = user_root.join(version);
        if !bundle
            .join(runtime_bundle::BUNDLE_MANIFEST_FILENAME)
            .exists()
        {
            continue;
        }
        let manifest = match verify(&bundle) {
            Ok(manifest) => manifest,
            Err(error) => {
                problems.push(format!("{version} not verified: {error}"));
                continue;
            }
        };
        if !identities.insert(manifest.root_hash.clone()) {
            continue;
        }
        match verified_user_libraries(&bundle, &manifest, &vouch) {
            Ok((kept, unkept)) => {
                files.extend(kept);
                problems.extend(unkept.into_iter().map(|unkept| match unkept {
                    Unkept::NoVeritySignature => {
                        format!("{version} has no fs-verity signature; its pages are left to the page cache")
                    }
                    Unkept::NotVouched(path) => format!(
                        "{} is not sealed with its signed fs-verity digest; left to the page cache",
                        path.display()
                    ),
                }));
            }
            Err(error) => problems.push(format!("{version}: reading its libraries: {error}")),
        }
    }
    (files, problems)
}

/// One file mapped read-only and, where allowed, locked into memory.
#[derive(Debug)]
pub struct ResidentFile {
    path: PathBuf,
    address: *mut libc::c_void,
    len: usize,
    locked: bool,
}

impl ResidentFile {
    /// The mapped file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Its size in bytes.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether the file is empty (and so maps nothing).
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Whether its pages are locked, rather than only mapped and read in.
    pub fn is_locked(&self) -> bool {
        self.locked
    }
}

impl Drop for ResidentFile {
    fn drop(&mut self) {
        if self.len > 0 {
            // SAFETY: `address` and `len` are the mapping `map_locked` made,
            // unmapped exactly once here.
            unsafe { libc::munmap(self.address, self.len) };
        }
    }
}

/// Map `path` read-only (never executable) and lock its pages in memory,
/// which reads every page in now and keeps it in the page cache until the
/// mapping goes away.
///
/// # Errors
///
/// Returns the error of opening, mapping or locking the file. Locking fails
/// with `ENOMEM`/`EPERM` when the process's `RLIMIT_MEMLOCK` is too small;
/// the service sets it.
pub fn map_locked(path: &Path) -> io::Result<ResidentFile> {
    map_open(path, &File::open(path)?, Lock::Required)
}

/// Whether [`map_open`] must lock the pages it maps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lock {
    /// Lock them, or fail (the system service, whose limit is raised).
    Required,
    /// Lock them if `RLIMIT_MEMLOCK` allows; else keep them mapped and read
    /// in (a person's session).
    IfAllowed,
}

/// Map the open `file` (reported as `path`) read-only, never executable, and
/// read its pages in; lock them as `lock` says. The mapping is of the file
/// this descriptor holds, whatever `path` names by then.
///
/// # Errors
///
/// Returns the error of mapping the file, or of locking it under
/// [`Lock::Required`].
pub fn map_open(path: &Path, file: &File, lock: Lock) -> io::Result<ResidentFile> {
    let len = usize::try_from(file.metadata()?.len())
        .map_err(|_| io::Error::other("file larger than the address space"))?;
    if len == 0 {
        return Ok(ResidentFile {
            path: path.to_owned(),
            address: std::ptr::null_mut(),
            len,
            locked: false,
        });
    }
    // SAFETY: a fresh read-only shared mapping of a file we opened; the
    // kernel picks the address. The file stays mapped after `file` closes.
    let address = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            len,
            libc::PROT_READ,
            libc::MAP_SHARED,
            file.as_raw_fd(),
            0,
        )
    };
    if address == libc::MAP_FAILED {
        return Err(io::Error::last_os_error());
    }
    let mut mapped = ResidentFile {
        path: path.to_owned(),
        address,
        len,
        locked: false,
    };
    // Read ahead in large requests before the lock faults page by page.
    // SAFETY: advice on the mapping made above.
    unsafe { libc::madvise(address, len, libc::MADV_WILLNEED) };
    // SAFETY: locks the mapping made above.
    if unsafe { libc::mlock(address, len) } == 0 {
        mapped.locked = true;
    } else if lock == Lock::Required {
        return Err(io::Error::last_os_error());
    } else {
        touch_pages(address, len);
    }
    Ok(mapped)
}

/// Read one byte of every page of the mapping at `address`, so each is in
/// the page cache and mapped here: what an unlockable mapping can do.
fn touch_pages(address: *mut libc::c_void, len: usize) {
    // SAFETY: sysconf has no preconditions.
    let page = usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) })
        .ok()
        .filter(|page| *page > 0)
        .unwrap_or(4096);
    let base = address.cast::<u8>();
    let mut offset = 0usize;
    while offset < len {
        // SAFETY: `offset < len`, inside the readable mapping of `len` bytes.
        let _ = unsafe { std::ptr::read_volatile(base.add(offset)) };
        offset = offset.saturating_add(page);
    }
}

/// Map and lock every file; a file that fails is reported and skipped, so
/// one broken bundle does not cost the others their residency.
pub fn map_all(files: &[PathBuf]) -> (Vec<ResidentFile>, Vec<(PathBuf, io::Error)>) {
    let mut mapped = Vec::new();
    let mut failed = Vec::new();
    for path in files {
        match map_locked(path) {
            Ok(file) => mapped.push(file),
            Err(error) => failed.push((path.clone(), error)),
        }
    }
    (mapped, failed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loader_preflight::elf_fixture;

    fn write(path: &Path, bytes: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn the_closure_follows_needed_libraries_inside_the_bundle_only() {
        let dir = tempfile::tempdir().unwrap();
        let lib = dir.path().join("lib");
        write(
            &lib.join(RUNTIME_LIBRARY),
            &elf_fixture(
                &["libstd-0123.so", "libc.so.6", "libnot-in-bundle.so.1"],
                None,
            ),
        );
        write(
            &lib.join("libstd-0123.so"),
            &elf_fixture(&["libc.so.6"], None),
        );
        write(
            &lib.join("libc.so.6"),
            &elf_fixture(&["ld-linux-aarch64.so.1"], None),
        );
        write(&lib.join("ld-linux-aarch64.so.1"), &elf_fixture(&[], None));
        // An engine library sits beside the runtime; no runtime library names it.
        write(
            &lib.join("libtilo_typst_engine.so"),
            &elf_fixture(&[RUNTIME_LIBRARY], None),
        );

        let names: Vec<String> = bundle_libraries(dir.path())
            .unwrap()
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            [
                RUNTIME_LIBRARY,
                "libstd-0123.so",
                "libc.so.6",
                "ld-linux-aarch64.so.1"
            ]
        );
    }

    #[test]
    fn a_bundle_without_the_runtime_library_has_nothing_to_keep() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("lib/libcef.so"), &elf_fixture(&[], None));
        assert!(bundle_libraries(dir.path()).unwrap().is_empty());
        assert!(
            bundle_libraries(&dir.path().join("absent"))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn a_needed_name_with_a_path_is_never_followed_out_of_the_bundle() {
        let dir = tempfile::tempdir().unwrap();
        write(
            &dir.path().join("lib").join(RUNTIME_LIBRARY),
            &elf_fixture(&["../outside.so", "/usr/lib64/libc.so.6"], None),
        );
        write(&dir.path().join("outside.so"), &elf_fixture(&[], None));
        assert_eq!(bundle_libraries(dir.path()).unwrap().len(), 1);
    }

    #[test]
    fn registered_versions_are_the_runtime_references_of_the_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("registry.toml");
        std::fs::write(
            &path,
            r#"
            [[entry]]
            id = "so.tilo.calculator"
            version = "0.1.0"
            package_id_hex = "00"
            payload_root = "/x"
            manifest_path = "/x.pkg/manifest.toml"
            publisher_key_hex = "00"
            runtime = "tilo-runtime/2026.07+native.abc"

            [[entry]]
            id = "so.tilo.notes"
            version = "0.1.0"
            package_id_hex = "01"
            payload_root = "/y"
            manifest_path = "/y.pkg/manifest.toml"
            publisher_key_hex = "00"
            runtime = "tilo-runtime/2026.07+native.abc"

            [[entry]]
            id = "so.tilo.host"
            version = "0.1.0"
            package_id_hex = "02"
            payload_root = "/z"
            manifest_path = "/z.pkg/manifest.toml"
            publisher_key_hex = "00"
            runtime = "tilo-runtime/../escape"
            "#,
        )
        .unwrap();
        let registry = package_store::Registry::load(&path).unwrap();
        assert_eq!(
            registered_versions(&registry)
                .into_iter()
                .collect::<Vec<_>>(),
            ["2026.07+native.abc"]
        );
    }

    fn listed(bundle: &Path, paths: &[&str]) -> runtime_bundle::BundleManifest {
        runtime_bundle::BundleManifest {
            version: "1".into(),
            arch: "aarch64".into(),
            root_hash: "00".into(),
            files: paths
                .iter()
                .map(|path| runtime_bundle::BundleFile {
                    path: (*path).to_owned(),
                    hash: String::new(),
                    size: std::fs::metadata(bundle.join(path)).map_or(0, |meta| meta.len()),
                    verity: Some("signed".into()),
                })
                .collect(),
            produced_at: 0,
            publisher_hex: None,
            signature_hex: None,
            verity_signature_hex: Some("signed".into()),
        }
    }

    /// Stands in for the kernel's measurement, which needs fs-verity on the
    /// test's filesystem: the open file is the listed size.
    fn listed_size(file: &File, expected: &runtime_bundle::BundleFile) -> bool {
        file.metadata()
            .is_ok_and(|meta| meta.len() == expected.size)
    }

    #[test]
    fn a_persons_runtime_is_kept_by_its_identity_not_its_path() {
        // An ABI-bound version names its payload: the system's bundle of it
        // is the same bytes, kept by the system service; the person's own
        // runtimes of other versions, and release-only names, are theirs.
        let system = tempfile::tempdir().unwrap();
        let bound = "2026.07+native.0123456789abcdef0123456789abcdef";
        let theirs = "2026.08+native.fedcba9876543210fedcba9876543210";
        write(
            &system
                .path()
                .join(bound)
                .join(runtime_bundle::BUNDLE_MANIFEST_FILENAME),
            b"",
        );
        write(
            &system
                .path()
                .join("1")
                .join(runtime_bundle::BUNDLE_MANIFEST_FILENAME),
            b"",
        );
        let registered: BTreeSet<String> = [bound, theirs, "1"].map(str::to_owned).into();
        assert_eq!(
            user_kept_versions(&registered, system.path())
                .into_iter()
                .collect::<Vec<_>>(),
            ["1", theirs]
        );

        // Two names for one root hash are kept once; an unverified bundle
        // not at all.
        let user = tempfile::tempdir().unwrap();
        for version in ["a", "b", "c"] {
            let bundle = user.path().join(version);
            write(&bundle.join(runtime_bundle::BUNDLE_MANIFEST_FILENAME), b"");
            write(
                &bundle.join("lib").join(RUNTIME_LIBRARY),
                &elf_fixture(&[], None),
            );
        }
        let versions: BTreeSet<String> = ["a", "b", "c", "absent"].map(str::to_owned).into();
        let (files, problems) = user_resident_files(
            &versions,
            user.path(),
            |bundle| {
                if bundle.ends_with("c") {
                    return Err("a byte differs".to_owned());
                }
                Ok(listed(bundle, &[&format!("lib/{RUNTIME_LIBRARY}")]))
            },
            listed_size,
        );
        assert_eq!(files.len(), 1, "{files:?}");
        assert!(files[0].path.starts_with(user.path().join("a")));
        assert_eq!(problems, ["c not verified: a byte differs"]);
    }

    #[test]
    fn only_libraries_the_signed_digests_vouch_for_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        let lib = dir.path().join("lib");
        write(
            &lib.join(RUNTIME_LIBRARY),
            &elf_fixture(&["libstd-0123.so", "libc.so.6", "libnot-listed.so"], None),
        );
        write(
            &lib.join("libstd-0123.so"),
            &elf_fixture(&["libc.so.6"], None),
        );
        write(&lib.join("libc.so.6"), &elf_fixture(&[], None));
        write(&lib.join("libnot-listed.so"), &elf_fixture(&[], None));
        let manifest = listed(
            dir.path(),
            &[
                &format!("lib/{RUNTIME_LIBRARY}"),
                "lib/libstd-0123.so",
                "lib/libc.so.6",
            ],
        );

        let names = |files: &[VerifiedFile]| -> Vec<String> {
            files
                .iter()
                .map(|file| {
                    file.path
                        .file_name()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect()
        };
        let (kept, unkept) = verified_user_libraries(dir.path(), &manifest, listed_size).unwrap();
        assert_eq!(
            names(&kept),
            [RUNTIME_LIBRARY, "libstd-0123.so", "libc.so.6"]
        );
        assert!(unkept.is_empty(), "{unkept:?}");

        // A file the digest does not vouch for is left out, and what only it
        // needs is not followed.
        let (kept, unkept) = verified_user_libraries(dir.path(), &manifest, |file, expected| {
            listed_size(file, expected) && !expected.path.ends_with("libstd-0123.so")
        })
        .unwrap();
        assert_eq!(names(&kept), [RUNTIME_LIBRARY, "libc.so.6"]);
        assert_eq!(unkept, [Unkept::NotVouched(lib.join("libstd-0123.so"))]);

        // Without a verity signature no digest vouches for anything.
        let unsigned = runtime_bundle::BundleManifest {
            verity_signature_hex: None,
            ..manifest
        };
        let (kept, unkept) = verified_user_libraries(dir.path(), &unsigned, listed_size).unwrap();
        assert!(kept.is_empty());
        assert_eq!(unkept, [Unkept::NoVeritySignature]);

        // A link is never followed out of the bundle.
        std::fs::remove_file(lib.join("libc.so.6")).unwrap();
        std::os::unix::fs::symlink("/usr/lib64/libc.so.6", lib.join("libc.so.6")).unwrap();
        let (kept, unkept) =
            verified_user_libraries(dir.path(), &with_verity_signature(&unsigned), listed_size)
                .unwrap();
        assert_eq!(names(&kept), [RUNTIME_LIBRARY, "libstd-0123.so"]);
        assert_eq!(unkept, [Unkept::NotVouched(lib.join("libc.so.6"))]);
    }

    fn with_verity_signature(
        manifest: &runtime_bundle::BundleManifest,
    ) -> runtime_bundle::BundleManifest {
        runtime_bundle::BundleManifest {
            verity_signature_hex: Some("signed".into()),
            ..manifest.clone()
        }
    }

    #[test]
    fn a_mapped_file_is_locked_and_resident() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lib.so");
        // SAFETY: sysconf has no preconditions.
        let page = usize::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).unwrap();
        let len = page.saturating_mul(3).saturating_add(5);
        std::fs::write(&path, vec![7u8; len]).unwrap();
        // The test runner's own RLIMIT_MEMLOCK (8 MiB by default) covers this.
        let file = map_locked(&path).unwrap();
        assert_eq!(file.len(), len);
        let pages = file.len().div_ceil(page);
        let mut resident = vec![0u8; pages];
        // SAFETY: queries the mapping `file` holds.
        let status = unsafe { libc::mincore(file.address, file.len, resident.as_mut_ptr()) };
        assert_eq!(status, 0);
        assert!(resident.iter().all(|page| page & 1 == 1), "{resident:?}");
        let status = std::fs::read_to_string("/proc/self/status").unwrap();
        let locked_kb: u64 = status
            .lines()
            .find_map(|line| line.strip_prefix("VmLck:"))
            .and_then(|value| value.trim().trim_end_matches("kB").trim().parse().ok())
            .unwrap();
        let wanted = u64::try_from(page.saturating_mul(4)).unwrap();
        assert!(
            locked_kb.saturating_mul(1024) >= wanted,
            "VmLck {locked_kb} kB"
        );
    }
}
