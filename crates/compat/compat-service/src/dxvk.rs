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
    #[expect(dead_code, reason = "held for its lock until the lease drops")]
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
