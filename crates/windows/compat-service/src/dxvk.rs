//! Install the immutable upstream DXVK payload into a private Wine prefix.
//!
//! This runs inside the capsule, after its FEX server is ready. The returned
//! lease must outlive the Windows application and its Wine server wait. Normal
//! launches share the prefix; initialization and upgrades require no live
//! leases. No host path or DLL name is supplied by the Windows application.

use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read, Seek, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use advisory_lock::{FileLock, LockMode, TryLockError};
use rustix::fs::{AtFlags, Mode, OFlags};
use sha2::{Digest, Sha256};

const PREFIX: &str = "/run/tilo/capsule/prefix";
const PAYLOAD: &str = "/opt/tilo/x86/usr/share/tilo/dxvk";
const BUILD_METADATA: &str = "/opt/tilo/x86/usr/lib/tilo/windows-guest/installed-packages.tsv";
const GENERATION_FILE: &str = ".tilo-dxvk-generation";
const DLLS: [&str; 5] = [
    "d3d8.dll",
    "d3d9.dll",
    "d3d10core.dll",
    "d3d11.dll",
    "dxgi.dll",
];
const MAX_DLL_BYTES: u64 = 64 * 1024 * 1024;
static TEMPORARY_ID: AtomicU64 = AtomicU64::new(0);

pub(crate) const NATIVE_OVERRIDES: &str = "d3d8,d3d9,d3d10core,d3d11,dxgi=n";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WineMode {
    Win32,
    Win64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PrefixAction {
    Initialize,
    Update,
}

impl PrefixAction {
    pub(crate) const fn wineboot_argument(self) -> &'static str {
        match self {
            Self::Initialize => "-i",
            Self::Update => "-u",
        }
    }
}

impl WineMode {
    const fn architecture(self) -> &'static str {
        match self {
            Self::Win32 => "#arch=win32",
            Self::Win64 => "#arch=win64",
        }
    }

    fn destinations(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::Win32 => &[("x32", "system32")],
            Self::Win64 => &[("x64", "system32"), ("x32", "syswow64")],
        }
    }
}

/// This lease owns the shared inode lock, independently of transient references
/// to its open-file description: dropping it unlocks explicitly, so a
/// duplicated or briefly inherited descriptor cannot prolong the supervisor's
/// lease. It prevents upgrades during execution.
#[derive(Debug)]
pub(crate) struct PrefixLease {
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "held for its lock until the lease drops")
    )]
    directory: FileLock,
}

/// `initialize` must run the matching immutable Wineboot executable and wait
/// for its Wine server, returning an error unless both complete successfully.
/// It runs only with exclusive prefix ownership. It must not enable native
/// DXVK overrides before the payload has been installed.
///
/// The generation file is merely a cache hint. Every launch independently
/// checks registry architecture, regular-file ownership of the DLL inodes,
/// and every DLL hash against the immutable payload. A writable prefix cannot
/// attest historical Wineboot execution; that would require broker-held state.
pub(crate) fn prepare(
    mode: WineMode,
    initialize: impl FnOnce(PrefixAction) -> io::Result<()>,
) -> io::Result<PrefixLease> {
    validate_prefix(std::env::var_os("WINEPREFIX").as_deref())?;
    prepare_at(
        mode,
        Path::new(PREFIX),
        Path::new(PAYLOAD),
        Path::new(BUILD_METADATA),
        initialize,
    )
}

fn validate_prefix(value: Option<&OsStr>) -> io::Result<()> {
    if value != Some(OsStr::new(PREFIX)) {
        return Err(invalid(
            "DXVK requires the fixed private capsule WINEPREFIX",
        ));
    }
    Ok(())
}

fn prepare_at(
    mode: WineMode,
    prefix_path: &Path,
    payload_path: &Path,
    build_metadata: &Path,
    initialize: impl FnOnce(PrefixAction) -> io::Result<()>,
) -> io::Result<PrefixLease> {
    let mut prefix = FileLock::try_shared(open_directory(prefix_path)?).map_err(busy)?;
    let mut payload = Payload::load(mode, payload_path, build_metadata)?;
    if !payload.ready(&prefix, mode)? {
        // Do not upgrade while another managed launch is reading/running from
        // this prefix. Never wait for arbitrary application lifetimes here.
        prefix = prefix.try_convert(LockMode::Exclusive).map_err(busy)?;
        // Another preparer could have finished between our two acquisitions.
        if !payload.ready(&prefix, mode)? {
            payload.remove_linked_destinations(&prefix)?;
            let action = if registry_ready(&prefix, mode)? {
                PrefixAction::Update
            } else {
                PrefixAction::Initialize
            };
            initialize(action)?;
            if !registry_ready(&prefix, mode)? {
                return Err(invalid(
                    "Wine initialization did not create the expected registry",
                ));
            }
            payload.install(&prefix)?;
            if !payload.ready(&prefix, mode)? {
                return Err(invalid("DXVK prefix changed during provisioning"));
            }
        }
        // Lock conversion is not atomic. Revalidate after acquiring the
        // shared lock before allowing application execution.
        prefix = prefix.try_convert(LockMode::Shared).map_err(busy)?;
        if !payload.ready(&prefix, mode)? {
            return Err(invalid("DXVK prefix changed before launch"));
        }
    }
    Ok(PrefixLease { directory: prefix })
}

struct Dll {
    source: File,
    hash: [u8; 32],
    size: u64,
    destination: &'static str,
    name: &'static str,
}

struct Payload {
    dlls: Vec<Dll>,
    generation: [u8; 32],
}

impl Payload {
    fn load(mode: WineMode, path: &Path, build_metadata: &Path) -> io::Result<Self> {
        let root = open_directory(path)?;
        let mut generation = Sha256::new();
        generation.update(b"tilo-dxvk-prefix-v1\0");
        generation.update(mode.architecture().as_bytes());
        generation.update(hash_file(
            &mut open_regular_at(&root, "provenance.json")?,
            Some(1024 * 1024),
        )?);
        let metadata_parent = build_metadata
            .parent()
            .ok_or_else(|| invalid("missing metadata parent"))?;
        let metadata_name = build_metadata
            .file_name()
            .and_then(OsStr::to_str)
            .ok_or_else(|| invalid("invalid metadata name"))?;
        generation.update(hash_file(
            &mut open_regular_at(&open_directory(metadata_parent)?, metadata_name)?,
            Some(16 * 1024 * 1024),
        )?);
        let mut dlls = Vec::new();
        for &(architecture, destination) in mode.destinations() {
            let directory = open_directory_at(&root, architecture)?;
            for name in DLLS {
                let mut source = open_regular_at(&directory, name)?;
                let size = source.metadata()?.len();
                let hash = hash_file(&mut source, Some(MAX_DLL_BYTES))?;
                generation.update(destination.as_bytes());
                generation.update(name.as_bytes());
                generation.update(hash);
                dlls.push(Dll {
                    source,
                    hash,
                    size,
                    destination,
                    name,
                });
            }
        }
        Ok(Self {
            dlls,
            generation: generation.finalize().into(),
        })
    }

    fn ready(&self, prefix: &File, mode: WineMode) -> io::Result<bool> {
        // Validate present directories even when the generation is missing:
        // never invoke Wineboot on a prefix with redirected Windows directories.
        let windows = windows_directory(prefix)?;
        if let Some(windows) = &windows {
            for &(_, destination) in mode.destinations() {
                let _ = optional_directory(windows, destination)?;
            }
        }
        let registry = registry_ready(prefix, mode)?;
        let Some(windows) = windows else {
            return Ok(false);
        };
        if !registry || !matches_contents(prefix, GENERATION_FILE, &self.generation)? {
            return Ok(false);
        }
        for dll in &self.dlls {
            let Some(directory) = optional_directory(&windows, dll.destination)? else {
                return Ok(false);
            };
            let Ok(mut file) = open_regular_at(&directory, dll.name) else {
                return Ok(false);
            };
            let metadata = file.metadata()?;
            if metadata.nlink() != 1
                || metadata.len() != dll.size
                || hash_file(&mut file, Some(MAX_DLL_BYTES))? != dll.hash
            {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn install(&mut self, prefix: &File) -> io::Result<()> {
        let windows = windows_directory(prefix)?
            .ok_or_else(|| invalid("Wine did not create its Windows directory"))?;
        for dll in &mut self.dlls {
            let directory = open_directory_at(&windows, dll.destination)?;
            dll.source.rewind()?;
            replace_from(&directory, dll.name, &mut dll.source)?;
        }
        // Commit the hint last. Interrupted partial updates are retried, and
        // its presence can never bypass the DLL and registry validation.
        replace_from(prefix, GENERATION_FILE, &mut self.generation.as_slice())
    }

    fn remove_linked_destinations(&self, prefix: &File) -> io::Result<()> {
        let Some(windows) = windows_directory(prefix)? else {
            return Ok(());
        };
        for dll in &self.dlls {
            let Some(directory) = optional_directory(&windows, dll.destination)? else {
                continue;
            };
            // O_PATH|O_NOFOLLOW lets fstat inspect the directory entry itself,
            // including symlinks, without opening a target or blocking on a FIFO.
            let entry = match open_at(&directory, dll.name, OFlags::PATH) {
                Ok(entry) => entry,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            let metadata = entry.metadata()?;
            let kind = metadata.file_type();
            if kind.is_dir() || kind.is_char_device() || kind.is_block_device() {
                return Err(invalid(
                    "DXVK destination must not be a directory or device",
                ));
            }
            if !kind.is_file() || metadata.nlink() != 1 {
                // Only this retained directory's single leaf entry is removed.
                // Wineboot must never see an old redirected DLL target.
                rustix::fs::unlinkat(&directory, component(dll.name)?, AtFlags::empty())?;
            }
        }
        Ok(())
    }
}

fn registry_ready(prefix: &File, mode: WineMode) -> io::Result<bool> {
    let mut complete = true;
    for name in ["system.reg", "user.reg", "userdef.reg"] {
        let mut file = match open_regular_at(prefix, name) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                complete = false;
                continue;
            }
            Err(error) => return Err(error),
        };
        if file.metadata()?.nlink() != 1 {
            return Err(invalid("Wine registry must not be hardlinked"));
        }
        let mut bytes = Vec::new();
        Read::by_ref(&mut file).take(4096).read_to_end(&mut bytes)?;
        let header = String::from_utf8_lossy(&bytes);
        if !header.starts_with("WINE REGISTRY Version 2\n") {
            return Err(invalid("invalid Wine registry header"));
        }
        if !header.lines().any(|line| line == mode.architecture()) {
            return Err(invalid(
                "Wine registry architecture does not match the managed mode",
            ));
        }
    }
    Ok(complete)
}

fn windows_directory(prefix: &File) -> io::Result<Option<File>> {
    let Some(drive) = optional_directory(prefix, "drive_c")? else {
        return Ok(None);
    };
    optional_directory(&drive, "windows")
}

fn matches_contents(parent: &File, name: &str, expected: &[u8]) -> io::Result<bool> {
    let Ok(mut file) = open_regular_at(parent, name) else {
        return Ok(false);
    };
    let metadata = file.metadata()?;
    if metadata.nlink() != 1
        || metadata.len() != u64::try_from(expected.len()).map_err(io::Error::other)?
    {
        return Ok(false);
    }
    let mut contents = Vec::new();
    Read::by_ref(&mut file)
        .take(metadata.len().saturating_add(1))
        .read_to_end(&mut contents)?;
    Ok(contents == expected)
}

fn hash_file(file: &mut File, limit: Option<u64>) -> io::Result<[u8; 32]> {
    let size = file.metadata()?.len();
    if size == 0 || limit.is_some_and(|limit| size > limit) {
        return Err(invalid("invalid immutable runtime file size"));
    }
    file.rewind()?;
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 16 * 1024];
    let mut remaining = size;
    while remaining > 0 {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            return Err(invalid("runtime file changed while reading"));
        }
        remaining = remaining
            .checked_sub(u64::try_from(count).map_err(io::Error::other)?)
            .ok_or_else(|| invalid("runtime file grew while reading"))?;
        hash.update(
            buffer
                .get(..count)
                .ok_or_else(|| invalid("invalid read size"))?,
        );
    }
    Ok(hash.finalize().into())
}

fn open_directory(path: &Path) -> io::Result<File> {
    Ok(rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?
    .into())
}

fn open_directory_at(parent: &File, name: &str) -> io::Result<File> {
    open_at(parent, name, OFlags::RDONLY | OFlags::DIRECTORY)
}

fn optional_directory(parent: &File, name: &str) -> io::Result<Option<File>> {
    match open_directory_at(parent, name) {
        Ok(directory) => Ok(Some(directory)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn open_regular_at(parent: &File, name: &str) -> io::Result<File> {
    // O_NONBLOCK prevents a hostile FIFO from blocking before fstat rejects it.
    let file = open_at(parent, name, OFlags::RDONLY | OFlags::NONBLOCK)?;
    if !file.metadata()?.is_file() {
        return Err(invalid("expected a regular runtime file"));
    }
    Ok(file)
}

fn open_at(parent: &File, name: &str, flags: OFlags) -> io::Result<File> {
    Ok(rustix::fs::openat(
        parent,
        component(name)?,
        flags | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )?
    .into())
}

fn component(name: &str) -> io::Result<&str> {
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\0']) {
        return Err(invalid("invalid private path component"));
    }
    Ok(name)
}

fn busy(error: TryLockError) -> io::Error {
    match error {
        TryLockError::WouldBlock => io::Error::new(
            io::ErrorKind::WouldBlock,
            "Wine prefix is busy; retry preparation after its applications exit",
        ),
        TryLockError::Error(error) => error,
    }
}

fn replace_from(parent: &File, name: &str, source: &mut impl Read) -> io::Result<()> {
    let destination = component(name)?;
    let (mut file, temporary_name) = create_temporary(parent, &TEMPORARY_ID)?;
    let result = (|| {
        io::copy(source, &mut file)?;
        file.flush()?;
        file.sync_all()?;
        // Rename replaces a link itself, never writes its target.
        rustix::fs::renameat(parent, &temporary_name, parent, destination)?;
        parent.sync_all()
    })();
    if result.is_err() {
        let _ = rustix::fs::unlinkat(parent, &temporary_name, AtFlags::empty());
    }
    result
}

fn create_temporary(parent: &File, counter: &AtomicU64) -> io::Result<(File, String)> {
    for _ in 0..128 {
        let temporary = format!(
            ".tilo-dxvk-{}-{}",
            std::process::id(),
            counter.fetch_add(1, Ordering::Relaxed)
        );
        match open_at(
            parent,
            &temporary,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL,
        ) {
            Ok(file) => return Ok((file, temporary)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a private DXVK temporary file",
    ))
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;

    struct Fixture {
        temporary: tempfile::TempDir,
        prefix: PathBuf,
        payload: PathBuf,
        metadata: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let temporary = tempfile::tempdir().unwrap();
            let prefix = temporary.path().join("prefix");
            let payload = temporary.path().join("payload");
            let metadata = temporary.path().join("installed-packages.tsv");
            fs::create_dir(&prefix).unwrap();
            for architecture in ["x32", "x64"] {
                fs::create_dir_all(payload.join(architecture)).unwrap();
                for name in DLLS {
                    fs::write(
                        payload.join(architecture).join(name),
                        format!("{architecture}:{name}:pinned-payload"),
                    )
                    .unwrap();
                }
            }
            fs::write(payload.join("provenance.json"), "{\"version\":\"test\"}").unwrap();
            fs::write(&metadata, "wine-core\t11.0\tx86_64\n").unwrap();
            Self {
                temporary,
                prefix,
                payload,
                metadata,
            }
        }

        fn initialize(&self, mode: WineMode) {
            for &(_, destination) in mode.destinations() {
                fs::create_dir_all(self.prefix.join("drive_c/windows").join(destination)).unwrap();
            }
            for name in ["system.reg", "user.reg", "userdef.reg"] {
                fs::write(
                    self.prefix.join(name),
                    format!("WINE REGISTRY Version 2\n\n{}\n", mode.architecture()),
                )
                .unwrap();
            }
        }

        fn prepare(
            &self,
            mode: WineMode,
            initialize: impl FnOnce() -> io::Result<()>,
        ) -> io::Result<PrefixLease> {
            prepare_at(mode, &self.prefix, &self.payload, &self.metadata, |_| {
                initialize()
            })
        }

        fn installed(&self, destination: &str, name: &str) -> PathBuf {
            self.prefix
                .join("drive_c/windows")
                .join(destination)
                .join(name)
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn managed_prefixes_and_leases_are_exact_and_initialize_per_architecture() {
        {
            assert!(validate_prefix(Some(OsStr::new(PREFIX))).is_ok());
            for value in [
                None,
                Some(OsStr::new("/tmp/prefix")),
                Some(OsStr::new("/run/tilo/capsule/prefix/../home")),
            ] {
                assert!(validate_prefix(value).is_err());
            }
            assert_eq!(NATIVE_OVERRIDES, "d3d8,d3d9,d3d10core,d3d11,dxgi=n");
        }
        {
            let fixture = Fixture::new();
            let lease = fixture
                .prepare(WineMode::Win32, || {
                    fixture.initialize(WineMode::Win32);
                    Ok(())
                })
                .unwrap();
            let duplicate = lease.directory.try_clone().unwrap();
            let contend = || FileLock::try_exclusive(open_directory(&fixture.prefix).unwrap());
            assert!(matches!(contend(), Err(TryLockError::WouldBlock)));
            drop(lease);
            drop(contend().unwrap());
            assert!(
                duplicate.metadata().unwrap().is_dir(),
                "duplicate remains open after explicit lease release"
            );
        }
        {
            let fixture = Fixture::new();
            let action = Cell::new(None);
            drop(
                prepare_at(
                    WineMode::Win32,
                    &fixture.prefix,
                    &fixture.payload,
                    &fixture.metadata,
                    |requested| {
                        action.set(Some(requested));
                        fixture.initialize(WineMode::Win32);
                        Ok(())
                    },
                )
                .unwrap(),
            );
            assert_eq!(action.get(), Some(PrefixAction::Initialize));
            assert_eq!(PrefixAction::Initialize.wineboot_argument(), "-i");
            fs::write(&fixture.metadata, b"updated Wine build").unwrap();
            let _lease = prepare_at(
                WineMode::Win32,
                &fixture.prefix,
                &fixture.payload,
                &fixture.metadata,
                |requested| {
                    action.set(Some(requested));
                    Ok(())
                },
            )
            .unwrap();
            assert_eq!(action.get(), Some(PrefixAction::Update));
            assert_eq!(PrefixAction::Update.wineboot_argument(), "-u");
        }
        {
            for mode in [WineMode::Win32, WineMode::Win64] {
                let fixture = Fixture::new();
                let first = fixture
                    .prepare(mode, || {
                        fixture.initialize(mode);
                        Ok(())
                    })
                    .unwrap();
                let second = fixture
                    .prepare(mode, || panic!("ready prefix must not reboot"))
                    .unwrap();
                for &(architecture, destination) in mode.destinations() {
                    for name in DLLS {
                        assert_eq!(
                            fs::read(fixture.installed(destination, name)).unwrap(),
                            fs::read(fixture.payload.join(architecture).join(name)).unwrap()
                        );
                    }
                }
                if mode == WineMode::Win32 {
                    assert!(!fixture.prefix.join("drive_c/windows/syswow64").exists());
                }
                drop((first, second));
            }
        }
        {
            let fixture = Fixture::new();
            let first = fixture
                .prepare(WineMode::Win64, || {
                    fixture.initialize(WineMode::Win64);
                    Ok(())
                })
                .unwrap();
            let second = fixture.prepare(WineMode::Win64, || panic!()).unwrap();
            fs::write(&fixture.metadata, "wine-core\t11.1\tx86_64\n").unwrap();
            let callback = Cell::new(false);
            let attempt = || {
                callback.set(true);
                Ok(())
            };
            assert_eq!(
                fixture
                    .prepare(WineMode::Win64, attempt)
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::WouldBlock
            );
            drop(first);
            assert_eq!(
                fixture
                    .prepare(WineMode::Win64, attempt)
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::WouldBlock
            );
            assert!(!callback.get());
            drop(second);
            let _updated = fixture.prepare(WineMode::Win64, attempt).unwrap();
            assert!(callback.get());
        }
    }

    #[test]
    fn dll_repairs_never_follow_links_or_redirected_directories() {
        {
            let fixture = Fixture::new();
            drop(
                fixture
                    .prepare(WineMode::Win32, || {
                        fixture.initialize(WineMode::Win32);
                        Ok(())
                    })
                    .unwrap(),
            );
            let target = fixture.installed("system32", "d3d11.dll");
            fs::write(&target, b"corrupt").unwrap();
            let callback = Cell::new(false);
            let _lease = fixture
                .prepare(WineMode::Win32, || {
                    callback.set(true);
                    Ok(())
                })
                .unwrap();
            assert!(callback.get());
            assert_eq!(
                fs::read(target).unwrap(),
                fs::read(fixture.payload.join("x32/d3d11.dll")).unwrap()
            );
        }
        {
            for use_symlink in [false, true] {
                let fixture = Fixture::new();
                fixture.initialize(WineMode::Win32);
                let outside = fixture.temporary.path().join("grant.txt");
                fs::write(&outside, b"external data must survive").unwrap();
                let target = fixture.installed("system32", "dxgi.dll");
                if use_symlink {
                    symlink(&outside, &target).unwrap();
                } else {
                    fs::hard_link(&outside, &target).unwrap();
                }
                let _lease = fixture
                    .prepare(WineMode::Win32, || {
                        assert!(!target.exists());
                        assert!(!target.is_symlink());
                        assert_eq!(fs::read(&outside).unwrap(), b"external data must survive");
                        Ok(())
                    })
                    .unwrap();
                assert_eq!(fs::read(&outside).unwrap(), b"external data must survive");
                assert!(!target.is_symlink());
                assert_eq!(fs::metadata(&target).unwrap().nlink(), 1);
                assert_eq!(
                    fs::read(target).unwrap(),
                    fs::read(fixture.payload.join("x32/dxgi.dll")).unwrap()
                );
            }
        }
        {
            for component in ["drive_c", "drive_c/windows", "drive_c/windows/system32"] {
                let fixture = Fixture::new();
                let outside = fixture.temporary.path().join("outside");
                fs::create_dir(&outside).unwrap();
                let destination = fixture.prefix.join(component);
                fs::create_dir_all(destination.parent().unwrap()).unwrap();
                symlink(&outside, destination).unwrap();
                assert!(
                    fixture
                        .prepare(WineMode::Win32, || panic!(
                            "must not initialize redirected prefix"
                        ))
                        .is_err()
                );
                assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
            }
        }
        {
            let fixture = Fixture::new();
            fixture.initialize(WineMode::Win64);
            assert!(fixture.prepare(WineMode::Win32, || panic!()).is_err());
            let registry = fixture.prefix.join("system.reg");
            let outside = fixture.temporary.path().join("registry-grant");
            fs::hard_link(&registry, &outside).unwrap();
            assert!(fixture.prepare(WineMode::Win64, || panic!()).is_err());
            fs::remove_file(&registry).unwrap();
            symlink(&outside, &registry).unwrap();
            assert!(fixture.prepare(WineMode::Win64, || panic!()).is_err());
        }
        {
            let fixture = Fixture::new();
            let source = fixture.payload.join("x32/dxgi.dll");
            fs::remove_file(&source).unwrap();
            symlink(fixture.payload.join("x64/dxgi.dll"), source).unwrap();
            assert!(fixture.prepare(WineMode::Win32, || panic!()).is_err());
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn readiness_requires_real_initialization_and_temporaries_are_safe() {
        {
            let fixture = Fixture::new();
            assert!(fixture.prepare(WineMode::Win64, || Ok(())).is_err());
            assert!(!fixture.prefix.join(GENERATION_FILE).exists());
            assert!(
                fixture
                    .prepare(WineMode::Win64, || Err(io::Error::other("wineboot failed")))
                    .is_err()
            );
            assert!(!fixture.prefix.join(GENERATION_FILE).exists());
            // Failure releases the exclusive lock, permitting a later retry.
            drop(
                fixture
                    .prepare(WineMode::Win64, || {
                        fixture.initialize(WineMode::Win64);
                        Ok(())
                    })
                    .unwrap(),
            );
        }
        {
            let fixture = Fixture::new();
            drop(
                fixture
                    .prepare(WineMode::Win32, || {
                        fixture.initialize(WineMode::Win32);
                        Ok(())
                    })
                    .unwrap(),
            );
            fs::remove_file(fixture.prefix.join("system.reg")).unwrap();
            let called = Cell::new(false);
            let _lease = fixture
                .prepare(WineMode::Win32, || {
                    called.set(true);
                    fixture.initialize(WineMode::Win32);
                    Ok(())
                })
                .unwrap();
            assert!(called.get());
        }
        {
            let fixture = Fixture::new();
            drop(
                fixture
                    .prepare(WineMode::Win32, || {
                        fixture.initialize(WineMode::Win32);
                        Ok(())
                    })
                    .unwrap(),
            );
            let target = fixture.installed("system32", "dxgi.dll");
            fs::remove_file(&target).unwrap();
            rustix::fs::mknodat(
                rustix::fs::CWD,
                &target,
                rustix::fs::FileType::Fifo,
                Mode::from_raw_mode(0o600),
                0,
            )
            .unwrap();
            let _lease = fixture.prepare(WineMode::Win32, || Ok(())).unwrap();
            assert!(fs::metadata(target).unwrap().is_file());
        }
        {
            let fixture = Fixture::new();
            let stale = fixture
                .prefix
                .join(format!(".tilo-dxvk-{}-0", std::process::id()));
            fs::write(&stale, b"left by an interrupted prior invocation").unwrap();
            let directory = open_directory(&fixture.prefix).unwrap();
            let (mut file, name) = create_temporary(&directory, &AtomicU64::new(0)).unwrap();
            file.write_all(b"new private data").unwrap();
            assert_eq!(name, format!(".tilo-dxvk-{}-1", std::process::id()));
            assert_eq!(
                fs::read(&stale).unwrap(),
                b"left by an interrupted prior invocation"
            );
            assert_eq!(
                fs::read(fixture.prefix.join(&name)).unwrap(),
                b"new private data"
            );
        }
        {
            let fixture = Fixture::new();
            for index in 0..128 {
                fs::write(
                    fixture
                        .prefix
                        .join(format!(".tilo-dxvk-{}-{index}", std::process::id())),
                    b"retained",
                )
                .unwrap();
            }
            let directory = open_directory(&fixture.prefix).unwrap();
            assert_eq!(
                create_temporary(&directory, &AtomicU64::new(0))
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::AlreadyExists
            );
            for entry in fs::read_dir(&fixture.prefix).unwrap() {
                assert_eq!(fs::read(entry.unwrap().path()).unwrap(), b"retained");
            }
        }
    }
}
