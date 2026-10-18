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
//! restarts (after an extension merge replaces `/usr`); a per-user runtime
//! bundle (one a person installed into their own store) is not covered, as a
//! system service cannot know which person's will run.
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
pub fn resident_files(versions: &BTreeSet<String>, runtime_root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for version in versions {
        files.extend(bundle_libraries(&runtime_root.join(version))?);
    }
    Ok(files)
}

/// One file mapped read-only and locked into memory.
#[derive(Debug)]
pub struct ResidentFile {
    path: PathBuf,
    address: *mut libc::c_void,
    len: usize,
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
    let file = File::open(path)?;
    let len = usize::try_from(file.metadata()?.len())
        .map_err(|_| io::Error::other("file larger than the address space"))?;
    if len == 0 {
        return Ok(ResidentFile {
            path: path.to_owned(),
            address: std::ptr::null_mut(),
            len,
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
    let mapped = ResidentFile {
        path: path.to_owned(),
        address,
        len,
    };
    // Read ahead in large requests before the lock faults page by page.
    // SAFETY: advice on the mapping made above.
    unsafe { libc::madvise(address, len, libc::MADV_WILLNEED) };
    // SAFETY: locks the mapping made above.
    if unsafe { libc::mlock(address, len) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(mapped)
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
            &elf_fixture(&["libstd-0123.so", "libc.so.6", "libnot-in-bundle.so.1"], None),
        );
        write(&lib.join("libstd-0123.so"), &elf_fixture(&["libc.so.6"], None));
        write(&lib.join("libc.so.6"), &elf_fixture(&["ld-linux-aarch64.so.1"], None));
        write(&lib.join("ld-linux-aarch64.so.1"), &elf_fixture(&[], None));
        // An engine library sits beside the runtime; no runtime library names it.
        write(&lib.join("libtilo_typst_engine.so"), &elf_fixture(&[RUNTIME_LIBRARY], None));

        let names: Vec<String> = bundle_libraries(dir.path())
            .unwrap()
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            [RUNTIME_LIBRARY, "libstd-0123.so", "libc.so.6", "ld-linux-aarch64.so.1"]
        );
    }

    #[test]
    fn a_bundle_without_the_runtime_library_has_nothing_to_keep() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("lib/libcef.so"), &elf_fixture(&[], None));
        assert!(bundle_libraries(dir.path()).unwrap().is_empty());
        assert!(bundle_libraries(&dir.path().join("absent")).unwrap().is_empty());
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
            registered_versions(&registry).into_iter().collect::<Vec<_>>(),
            ["2026.07+native.abc"]
        );
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
        assert!(locked_kb.saturating_mul(1024) >= wanted, "VmLck {locked_kb} kB");
    }
}
