//! Will the sandboxed dynamic loader find every library an app links against?
//!
//! A thin app links against the shared libraries of the runtime bundle it was
//! built with, down to the exact hash in Rust's `libstd-<hash>.so`. Launched
//! with any other bundle it dies in the dynamic loader before `main`, and the
//! only trace is one loader line on stderr. This module answers the question
//! on the host before `bwrap` runs, from the directories the sandbox hands the
//! loader, so such a launch fails with the missing library named instead.
//!
//! The check is conservative in one direction only: whenever it cannot tell
//! (an object it cannot parse, a search path it cannot map to the host, a
//! loader cache in an unknown format) it treats the library as found and lets
//! the real loader decide. A launch it refuses is one the loader would have
//! refused too.

use std::collections::{HashSet, VecDeque};
use std::fs::File;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;
const PT_INTERP: u32 = 3;
const DT_NULL: u64 = 0;
const DT_NEEDED: u64 = 1;
const DT_STRTAB: u64 = 5;
const DT_STRSZ: u64 = 10;
const DT_RPATH: u64 = 15;
const DT_RUNPATH: u64 = 29;

const ELF64_HEADER_LEN: usize = 64;
const ELF64_PHDR_LEN: usize = 56;
const ELF64_DYN_LEN: usize = 16;
/// Bounds on what one object may make the launcher read: real binaries stay
/// far below these, and a hostile file cannot make a launch allocate more.
const MAX_PROGRAM_HEADERS: usize = 512;
const MAX_DYNAMIC_BYTES: u64 = 64 * 1024;
const MAX_NAME_BYTES: u64 = 4096;
/// Enough for an app, its bundled libraries and the runtime libraries they
/// pull in; past that the check stops rather than walking forever.
const MAX_OBJECTS: usize = 256;
const MAX_LD_CACHE_BYTES: u64 = 64 * 1024 * 1024;

/// The host's loader cache, which the system read profile binds into every
/// sandbox at the same path.
pub const LD_SO_CACHE: &str = "/etc/ld.so.cache";

/// The loader's built-in search directories on the architectures Tilo ships
/// (glibc's `SYSTEM_DIRS`, plus the multiarch spellings), all reachable in the
/// sandbox through the system read profile's `/lib`, `/lib64`, `/usr/lib` and
/// `/usr/lib64` binds at their host paths.
pub const SYSTEM_LIBRARY_DIRS: &[&str] = &[
    "/lib64",
    "/usr/lib64",
    "/lib",
    "/usr/lib",
    "/lib/x86_64-linux-gnu",
    "/usr/lib/x86_64-linux-gnu",
    "/lib/aarch64-linux-gnu",
    "/usr/lib/aarch64-linux-gnu",
];

/// Sandbox paths that do not show the host's tree at the same place: the
/// launcher's own mounts live here, and anything else here is either private
/// to the sandbox or not there at all.
const SANDBOX_PRIVATE_PREFIXES: &[&str] = &["/run", "/var/tilo-apps", "/tmp", "/proc", "/dev"];

/// What the loader reads from an object's `PT_DYNAMIC` segment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DynamicSection {
    /// `DT_NEEDED` entries, in order.
    pub needed: Vec<String>,
    /// `DT_RPATH` entries, split on `:`.
    pub rpath: Vec<String>,
    /// `DT_RUNPATH` entries, split on `:`.
    pub runpath: Vec<String>,
}

/// Read the dynamic section of the 64-bit little-endian ELF object at `path`.
///
/// Returns `Ok(None)` for anything this reader does not understand (not ELF,
/// another class or byte order, a string table outside every loaded segment,
/// names that run past their table): the caller then knows nothing about the
/// object, which is different from knowing it needs nothing. A statically
/// linked object has no dynamic section and returns an empty one.
///
/// # Errors
///
/// Returns the I/O error of opening or reading the file. A file shorter than
/// its headers claim is `Ok(None)`, not an error.
pub fn read_dynamic_section(path: &Path) -> std::io::Result<Option<DynamicSection>> {
    read_dynamic_section_of(&File::open(path)?)
}

/// [`read_dynamic_section`] of a file already open: the object read is the
/// one this descriptor holds, whatever its path names by then.
///
/// # Errors
///
/// Returns the I/O error of reading the file.
pub fn read_dynamic_section_of(file: &File) -> std::io::Result<Option<DynamicSection>> {
    match parse_dynamic(file) {
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => Ok(None),
        other => other,
    }
}

/// A segment as `(file offset, virtual address, size in the file)`.
type Segment = (u64, u64, u64);

struct ProgramHeaders {
    loads: Vec<Segment>,
    dynamic: Option<Segment>,
    interpreter: Option<Segment>,
}

/// Read an ELF interpreter without executing the object. Absence is distinct
/// from a malformed/unsupported ELF, which is an error.
///
/// # Errors
///
/// Returns I/O errors for inaccessible or truncated objects, and rejects
/// unsupported ELF headers or an invalid interpreter string.
pub fn read_interpreter(path: &Path) -> std::io::Result<Option<String>> {
    let file = File::open(path)?;
    let malformed = || std::io::Error::other("invalid ELF interpreter");
    let headers = program_headers(&file)?.ok_or_else(malformed)?;
    let Some((offset, _, size)) = headers.interpreter else {
        return Ok(None);
    };
    if !(2..=MAX_NAME_BYTES).contains(&size) {
        return Err(malformed());
    }
    let mut bytes = vec![0; usize::try_from(size).map_err(|_| malformed())?];
    file.read_exact_at(&mut bytes, offset)?;
    if bytes.pop() != Some(0) || bytes.contains(&0) {
        return Err(malformed());
    }
    String::from_utf8(bytes).map(Some).map_err(|_| malformed())
}

/// The `PT_LOAD` segments and the first `PT_DYNAMIC` one, or `None` for a
/// file this reader does not understand.
fn program_headers(file: &File) -> std::io::Result<Option<ProgramHeaders>> {
    let mut header = [0u8; ELF64_HEADER_LEN];
    file.read_exact_at(&mut header, 0)?;
    // ELFCLASS64, ELFDATA2LSB.
    if header.get(..6) != Some(b"\x7fELF\x02\x01".as_slice()) {
        return Ok(None);
    }
    let (Some(phoff), Some(phentsize), Some(phnum)) = (
        le_u64(&header, 0x20),
        le_u16(&header, 0x36).map(usize::from),
        le_u16(&header, 0x38).map(usize::from),
    ) else {
        return Ok(None);
    };
    if phentsize < ELF64_PHDR_LEN || phnum > MAX_PROGRAM_HEADERS {
        return Ok(None);
    }
    let Some(table_len) = phentsize.checked_mul(phnum) else {
        return Ok(None);
    };
    let mut table = vec![0u8; table_len];
    file.read_exact_at(&mut table, phoff)?;

    let mut loads = Vec::new();
    let mut dynamic = None;
    let mut interpreter = None;
    for entry in table.chunks_exact(phentsize) {
        let (Some(kind), Some(offset), Some(vaddr), Some(filesz)) = (
            le_u32(entry, 0),
            le_u64(entry, 8),
            le_u64(entry, 16),
            le_u64(entry, 32),
        ) else {
            return Ok(None);
        };
        match kind {
            PT_LOAD => loads.push((offset, vaddr, filesz)),
            PT_DYNAMIC if dynamic.is_none() => dynamic = Some((offset, vaddr, filesz)),
            PT_INTERP if interpreter.is_none() => interpreter = Some((offset, vaddr, filesz)),
            PT_INTERP => return Ok(None),
            _ => {}
        }
    }
    Ok(Some(ProgramHeaders {
        loads,
        dynamic,
        interpreter,
    }))
}

fn parse_dynamic(file: &File) -> std::io::Result<Option<DynamicSection>> {
    let Some(ProgramHeaders { loads, dynamic, .. }) = program_headers(file)? else {
        return Ok(None);
    };
    let Some((dyn_offset, _, dyn_size)) = dynamic else {
        return Ok(Some(DynamicSection::default()));
    };
    let Ok(dyn_len) = usize::try_from(dyn_size.min(MAX_DYNAMIC_BYTES)) else {
        return Ok(None);
    };
    let mut entries = vec![0u8; dyn_len];
    file.read_exact_at(&mut entries, dyn_offset)?;

    let mut needed = Vec::new();
    let mut rpath = Vec::new();
    let mut runpath = Vec::new();
    let mut strtab = None;
    let mut strsz = None;
    for entry in entries.chunks_exact(ELF64_DYN_LEN) {
        let (Some(tag), Some(value)) = (le_u64(entry, 0), le_u64(entry, 8)) else {
            return Ok(None);
        };
        match tag {
            DT_NULL => break,
            DT_NEEDED => needed.push(value),
            DT_RPATH => rpath.push(value),
            DT_RUNPATH => runpath.push(value),
            DT_STRTAB => strtab = Some(value),
            DT_STRSZ => strsz = Some(value),
            _ => {}
        }
    }
    if needed.is_empty() && rpath.is_empty() && runpath.is_empty() {
        return Ok(Some(DynamicSection::default()));
    }
    let (Some(strtab), Some(strsz)) = (strtab, strsz) else {
        return Ok(None);
    };
    // DT_STRTAB is an address; find the file bytes the loader maps there.
    let Some(table_offset) = loads.iter().find_map(|&(offset, vaddr, filesz)| {
        let delta = strtab.checked_sub(vaddr)?;
        (delta < filesz).then(|| offset.checked_add(delta))?
    }) else {
        return Ok(None);
    };
    let strings = StringTable {
        file,
        offset: table_offset,
        size: strsz,
    };
    let names = |offsets: &[u64]| -> std::io::Result<Option<Vec<String>>> {
        let mut out = Vec::with_capacity(offsets.len());
        for &at in offsets {
            match strings.get(at)? {
                Some(name) => out.push(name),
                None => return Ok(None),
            }
        }
        Ok(Some(out))
    };
    let split = |paths: Vec<String>| -> Vec<String> {
        paths
            .iter()
            .flat_map(|path| path.split(':'))
            .filter(|dir| !dir.is_empty())
            .map(str::to_string)
            .collect()
    };
    let (Some(needed), Some(rpath), Some(runpath)) =
        (names(&needed)?, names(&rpath)?, names(&runpath)?)
    else {
        return Ok(None);
    };
    Ok(Some(DynamicSection {
        needed,
        rpath: split(rpath),
        runpath: split(runpath),
    }))
}

struct StringTable<'a> {
    file: &'a File,
    offset: u64,
    size: u64,
}

impl StringTable<'_> {
    /// The NUL-terminated UTF-8 string at `at`, or `None` if it does not end
    /// inside the table (and within [`MAX_NAME_BYTES`]).
    fn get(&self, at: u64) -> std::io::Result<Option<String>> {
        let Some(remaining) = self.size.checked_sub(at).filter(|left| *left > 0) else {
            return Ok(None);
        };
        let (Ok(len), Some(start)) = (
            usize::try_from(remaining.min(MAX_NAME_BYTES)),
            self.offset.checked_add(at),
        ) else {
            return Ok(None);
        };
        let mut buf = vec![0u8; len];
        let read = self.file.read_at(&mut buf, start)?;
        buf.truncate(read);
        let Some(end) = buf.iter().position(|byte| *byte == 0) else {
            return Ok(None);
        };
        buf.truncate(end);
        Ok(String::from_utf8(buf).ok())
    }
}

fn le_u16(bytes: &[u8], at: usize) -> Option<u16> {
    let raw = bytes.get(at..at.checked_add(2)?)?;
    Some(u16::from_le_bytes(raw.try_into().ok()?))
}

fn le_u32(bytes: &[u8], at: usize) -> Option<u32> {
    let raw = bytes.get(at..at.checked_add(4)?)?;
    Some(u32::from_le_bytes(raw.try_into().ok()?))
}

fn le_u64(bytes: &[u8], at: usize) -> Option<u64> {
    let raw = bytes.get(at..at.checked_add(8)?)?;
    Some(u64::from_le_bytes(raw.try_into().ok()?))
}

/// The names the host's `ld.so.cache` maps, which the sandboxed loader
/// consults after its search paths and before its built-in directories.
#[derive(Debug, Clone)]
pub enum LdCache {
    /// No cache file: the loader goes straight to its built-in directories.
    Absent,
    /// The sonames listed in a cache this module could parse.
    Names(HashSet<String>),
    /// A cache in a format this module does not parse; a soname counts as
    /// listed when its bytes appear in it NUL-terminated.
    Opaque(Vec<u8>),
}

impl LdCache {
    /// Read the cache at `path`. A missing or unreadable file is
    /// [`LdCache::Absent`].
    #[must_use]
    pub fn load(path: &Path) -> Self {
        let Ok(file) = File::open(path) else {
            return Self::Absent;
        };
        let mut bytes = Vec::new();
        if std::io::Read::read_to_end(
            &mut std::io::Read::take(file, MAX_LD_CACHE_BYTES),
            &mut bytes,
        )
        .is_err()
        {
            return Self::Absent;
        }
        Self::from_bytes(bytes)
    }

    /// Parse glibc's `glibc-ld.so.cache1.1` format, alone or after the
    /// legacy `ld.so-1.7.0` table.
    #[must_use]
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        parse_ld_cache(&bytes).map_or(Self::Opaque(bytes), Self::Names)
    }

    fn contains(&self, soname: &str) -> bool {
        match self {
            Self::Absent => false,
            Self::Names(names) => names.contains(soname),
            Self::Opaque(bytes) => {
                let mut needle = soname.as_bytes().to_vec();
                needle.push(0);
                bytes.windows(needle.len()).any(|window| window == needle)
            }
        }
    }
}

fn parse_ld_cache(bytes: &[u8]) -> Option<HashSet<String>> {
    const MAGIC: &[u8] = b"glibc-ld.so.cache1.1";
    const HEADER_LEN: usize = 48;
    const ENTRY_LEN: usize = 24;
    let start = bytes
        .windows(MAGIC.len())
        .position(|window| window == MAGIC)?;
    // String offsets in the new format count from its own header.
    let table = bytes.get(start..)?;
    let count = usize::try_from(le_u32(table, 20)?).ok()?;
    let entries_end = count.checked_mul(ENTRY_LEN)?.checked_add(HEADER_LEN)?;
    let entries = table.get(HEADER_LEN..entries_end)?;
    let mut names = HashSet::with_capacity(count);
    for entry in entries.chunks_exact(ENTRY_LEN) {
        let key = usize::try_from(le_u32(entry, 4)?).ok()?;
        let tail = table.get(key..)?;
        let end = tail.iter().position(|byte| *byte == 0)?;
        names.insert(std::str::from_utf8(tail.get(..end)?).ok()?.to_string());
    }
    Some(names)
}

/// A library the sandboxed loader would not find.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingLibrary {
    /// The `DT_NEEDED` soname.
    pub library: String,
    /// The object that needs it, as a sandbox path.
    pub needed_by: PathBuf,
    /// Where it was looked for, as host paths, in search order.
    pub searched: Vec<PathBuf>,
}

/// The library search the sandboxed loader performs, seen from the host.
#[derive(Debug, Clone)]
pub struct LoaderView {
    /// The launcher's own binds as `(sandbox path, host path)`.
    mounts: Vec<(PathBuf, PathBuf)>,
    /// `LD_LIBRARY_PATH`, as sandbox paths.
    library_path: Vec<PathBuf>,
    cache: LdCache,
    /// The loader's built-in directories, as sandbox paths.
    system_dirs: Vec<PathBuf>,
}

enum Lookup {
    /// Found at this sandbox path, which lies in a launcher mount and so is
    /// shipped with the app or its runtime.
    Shipped(PathBuf),
    /// Found on the host system, or not decidable here.
    Elsewhere,
    Missing(Vec<PathBuf>),
}

impl LoaderView {
    /// The view of a sandbox with the app payload and its runtime bundle
    /// bound at the given mounts, the bundle's library directory on
    /// `LD_LIBRARY_PATH`, and the host's loader cache and system
    /// directories exposed by the system read profile.
    #[must_use]
    pub fn for_sandbox(
        payload: (&Path, &Path),
        runtime: (&Path, &Path),
        library_path: Vec<PathBuf>,
    ) -> Self {
        Self::new(
            vec![
                (payload.0.to_path_buf(), payload.1.to_path_buf()),
                (runtime.0.to_path_buf(), runtime.1.to_path_buf()),
            ],
            library_path,
            LdCache::load(Path::new(LD_SO_CACHE)),
            SYSTEM_LIBRARY_DIRS.iter().map(PathBuf::from).collect(),
        )
    }

    /// A view from explicit parts: `mounts` as `(sandbox, host)` pairs, and
    /// `library_path` and `system_dirs` as sandbox paths.
    #[must_use]
    pub fn new(
        mut mounts: Vec<(PathBuf, PathBuf)>,
        library_path: Vec<PathBuf>,
        cache: LdCache,
        system_dirs: Vec<PathBuf>,
    ) -> Self {
        // Longest mount first, so a nested mount wins over its parent.
        mounts.sort_by_key(|(sandbox, _)| std::cmp::Reverse(sandbox.components().count()));
        Self {
            mounts,
            library_path,
            cache,
            system_dirs,
        }
    }

    /// Check that every `DT_NEEDED` library of `entry` (a sandbox path), and
    /// of each library it finds shipped in the app payload or the runtime
    /// bundle, resolves where the sandboxed loader looks.
    ///
    /// # Errors
    ///
    /// Returns the first library that resolves nowhere.
    pub fn check(&self, entry: &Path) -> Result<(), MissingLibrary> {
        let mut queue = VecDeque::from([entry.to_path_buf()]);
        let mut seen: HashSet<PathBuf> = HashSet::from([entry.to_path_buf()]);
        // glibc searches the DT_RPATH of every object up the loading chain;
        // the union of all seen so far is a superset of that.
        let mut rpaths: Vec<PathBuf> = Vec::new();
        while let Some(object) = queue.pop_front() {
            let Some(host) = self.host_path(&object) else {
                continue;
            };
            let Ok(Some(dynamic)) = read_dynamic_section(&host) else {
                continue;
            };
            let origin = object.parent().unwrap_or_else(|| Path::new("/"));
            let (Some(rpath), Some(runpath)) = (
                expand_search_path(&dynamic.rpath, origin),
                expand_search_path(&dynamic.runpath, origin),
            ) else {
                continue;
            };
            rpaths.extend(rpath);
            let dirs: Vec<&Path> = rpaths
                .iter()
                .chain(&self.library_path)
                .chain(&runpath)
                .map(PathBuf::as_path)
                .collect();
            for soname in &dynamic.needed {
                if !is_searched_soname(soname) {
                    continue;
                }
                match self.lookup(soname, &dirs) {
                    Lookup::Shipped(found) => {
                        if seen.len() < MAX_OBJECTS && seen.insert(found.clone()) {
                            queue.push_back(found);
                        }
                    }
                    Lookup::Elsewhere => {}
                    Lookup::Missing(searched) => {
                        return Err(MissingLibrary {
                            library: soname.clone(),
                            needed_by: object,
                            searched,
                        });
                    }
                }
            }
        }
        Ok(())
    }

    fn lookup(&self, soname: &str, dirs: &[&Path]) -> Lookup {
        let mut searched = Vec::new();
        for dir in dirs {
            let Some(host_dir) = self.host_path(dir) else {
                return Lookup::Elsewhere;
            };
            if has_library(&host_dir, soname) {
                return if self.is_mounted(dir) {
                    Lookup::Shipped(lexically_normal(&dir.join(soname)))
                } else {
                    Lookup::Elsewhere
                };
            }
            push_unique(&mut searched, host_dir);
        }
        if self.cache.contains(soname) {
            return Lookup::Elsewhere;
        }
        if !matches!(self.cache, LdCache::Absent) {
            push_unique(&mut searched, PathBuf::from(LD_SO_CACHE));
        }
        for dir in &self.system_dirs {
            let Some(host_dir) = self.host_path(dir) else {
                return Lookup::Elsewhere;
            };
            if has_library(&host_dir, soname) {
                return Lookup::Elsewhere;
            }
            push_unique(&mut searched, host_dir);
        }
        Lookup::Missing(searched)
    }

    /// Where the sandbox path `path` lives on the host, or `None` if the
    /// sandbox shows something there the host does not have at that path.
    fn host_path(&self, path: &Path) -> Option<PathBuf> {
        let path = lexically_normal(path);
        let path = path.as_path();
        for (sandbox, host) in &self.mounts {
            if let Ok(rest) = path.strip_prefix(sandbox) {
                return Some(host.join(rest));
            }
        }
        if SANDBOX_PRIVATE_PREFIXES
            .iter()
            .any(|prefix| path.starts_with(prefix))
            || !path.is_absolute()
        {
            return None;
        }
        Some(path.to_path_buf())
    }

    fn is_mounted(&self, path: &Path) -> bool {
        let path = lexically_normal(path);
        self.mounts
            .iter()
            .any(|(sandbox, _)| path.starts_with(sandbox))
    }
}

/// `path` with `.` and `..` folded away without consulting the filesystem,
/// so `$ORIGIN/../lib` maps through the mount it lands in.
fn lexically_normal(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

/// The interpreter is mapped by the kernel before any search runs, and a
/// name with a slash is a path, not a search; neither goes through the
/// directories checked here.
fn is_searched_soname(soname: &str) -> bool {
    !soname.contains('/')
        && !soname.starts_with("ld-linux")
        && !soname.starts_with("ld64.so")
        && !soname.starts_with("ld.so")
}

/// A library directory holds `soname` directly or in one of the
/// `glibc-hwcaps/<level>/` subdirectories the loader also searches.
fn has_library(dir: &Path, soname: &str) -> bool {
    if dir.join(soname).exists() {
        return true;
    }
    std::fs::read_dir(dir.join("glibc-hwcaps")).is_ok_and(|levels| {
        levels
            .flatten()
            .any(|level| level.path().join(soname).exists())
    })
}

/// Expand `$ORIGIN` in `DT_RPATH` / `DT_RUNPATH` entries. `None` when an entry
/// uses another dynamic string token (`$LIB`, `$PLATFORM`), whose value is
/// the loader's to pick.
fn expand_search_path(entries: &[String], origin: &Path) -> Option<Vec<PathBuf>> {
    let origin = origin.to_str()?;
    entries
        .iter()
        .map(|entry| {
            let expanded = entry
                .replace("${ORIGIN}", origin)
                .replace("$ORIGIN", origin);
            (!expanded.contains('$')).then(|| PathBuf::from(expanded))
        })
        .collect()
}

fn push_unique(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if !paths.contains(&path) {
        paths.push(path);
    }
}

/// A minimal ELF64 object with the given dynamic entries, for tests: one
/// `PT_LOAD` mapping the whole file at address 0 and one `PT_DYNAMIC`.
#[doc(hidden)]
#[must_use]
#[allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    reason = "test fixture builder over a few hundred bytes"
)]
pub fn elf_fixture(needed: &[&str], runpath: Option<&str>) -> Vec<u8> {
    let phoff = ELF64_HEADER_LEN;
    let strtab_off = phoff + 2 * ELF64_PHDR_LEN;
    let mut strtab = vec![0u8];
    let mut name = |value: &str| {
        let at = strtab.len() as u64;
        strtab.extend_from_slice(value.as_bytes());
        strtab.push(0);
        at
    };
    let mut dynamic: Vec<(u64, u64)> = needed.iter().map(|n| (DT_NEEDED, name(n))).collect();
    if let Some(runpath) = runpath {
        dynamic.push((DT_RUNPATH, name(runpath)));
    }
    let strsz = strtab.len();
    let dyn_off = (strtab_off + strsz).next_multiple_of(8);
    dynamic.push((DT_STRTAB, strtab_off as u64));
    dynamic.push((DT_STRSZ, strsz as u64));
    dynamic.push((DT_NULL, 0));
    let dyn_len = dynamic.len() * ELF64_DYN_LEN;
    let total = dyn_off + dyn_len;

    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(b"\x7fELF\x02\x01\x01");
    out.resize(16, 0);
    out.extend_from_slice(&3u16.to_le_bytes()); // ET_DYN
    out.extend_from_slice(&62u16.to_le_bytes()); // EM_X86_64
    out.extend_from_slice(&1u32.to_le_bytes());
    out.extend_from_slice(&0u64.to_le_bytes()); // e_entry
    out.extend_from_slice(&(phoff as u64).to_le_bytes());
    out.extend_from_slice(&0u64.to_le_bytes()); // e_shoff
    out.extend_from_slice(&0u32.to_le_bytes()); // e_flags
    out.extend_from_slice(&(ELF64_HEADER_LEN as u16).to_le_bytes());
    out.extend_from_slice(&(ELF64_PHDR_LEN as u16).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&[0u8; 6]); // shentsize, shnum, shstrndx
    let phdr = |out: &mut Vec<u8>, kind: u32, offset: usize, size: usize| {
        out.extend_from_slice(&kind.to_le_bytes());
        out.extend_from_slice(&4u32.to_le_bytes()); // PF_R
        out.extend_from_slice(&(offset as u64).to_le_bytes());
        out.extend_from_slice(&(offset as u64).to_le_bytes()); // vaddr
        out.extend_from_slice(&(offset as u64).to_le_bytes()); // paddr
        out.extend_from_slice(&(size as u64).to_le_bytes());
        out.extend_from_slice(&(size as u64).to_le_bytes());
        out.extend_from_slice(&8u64.to_le_bytes());
    };
    phdr(&mut out, PT_LOAD, 0, total);
    phdr(&mut out, PT_DYNAMIC, dyn_off, dyn_len);
    out.extend_from_slice(&strtab);
    out.resize(dyn_off, 0);
    for (tag, value) in dynamic {
        out.extend_from_slice(&tag.to_le_bytes());
        out.extend_from_slice(&value.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, bytes: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    /// A payload at `/var/tilo-apps/app` and a runtime at
    /// `/run/tilo-runtime/1`, with no cache and no system directories unless
    /// a test adds them.
    struct Fixture {
        _dir: tempfile::TempDir,
        payload: PathBuf,
        runtime: PathBuf,
        system: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let payload = dir.path().join("payload");
            let runtime = dir.path().join("runtime");
            let system = dir.path().join("system");
            for d in [&payload, &runtime, &system] {
                std::fs::create_dir_all(d.join("lib")).unwrap();
            }
            Self {
                _dir: dir,
                payload,
                runtime,
                system,
            }
        }

        /// The fixture's system directory is seen through a mount of its
        /// own, so the view never depends on where the test's temporary
        /// directory is (a `/tmp` one is sandbox-private, and undecidable).
        fn view(&self, cache: LdCache) -> LoaderView {
            LoaderView::new(
                vec![
                    (PathBuf::from("/var/tilo-apps/app"), self.payload.clone()),
                    (PathBuf::from("/run/tilo-runtime/1"), self.runtime.clone()),
                    (
                        PathBuf::from("/usr/lib/fixture-system"),
                        self.system.clone(),
                    ),
                ],
                vec![PathBuf::from("/run/tilo-runtime/1/lib")],
                cache,
                vec![PathBuf::from("/usr/lib/fixture-system/lib")],
            )
        }
    }

    fn entry() -> PathBuf {
        PathBuf::from("/var/tilo-apps/app/bin/app")
    }

    #[test]
    fn reads_needed_and_runpath_from_a_fixture_object() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("obj");
        write(
            &path,
            &elf_fixture(&["libstd-abc.so", "libc.so.6"], Some("$ORIGIN/../lib:/opt")),
        );
        let dynamic = read_dynamic_section(&path).unwrap().unwrap();
        assert_eq!(dynamic.needed, ["libstd-abc.so", "libc.so.6"]);
        assert_eq!(dynamic.runpath, ["$ORIGIN/../lib", "/opt"]);
        assert!(dynamic.rpath.is_empty());
    }

    // Reads an ELF executable and compares it with the Linux sandbox loaders.
    #[cfg(target_os = "linux")]
    #[test]
    fn reads_the_needed_libraries_of_a_real_dynamically_linked_binary() {
        let exe = std::env::current_exe().unwrap();
        let interpreter = read_interpreter(&exe).unwrap().unwrap();
        assert!(crate::sandbox::CANONICAL_LOADER_PATHS.contains(&interpreter.as_str()));
        let loader = read_dynamic_section(Path::new(&interpreter))
            .unwrap()
            .unwrap();
        assert!(
            loader.needed.is_empty(),
            "the native loader is a valid dependency leaf"
        );
        let dynamic = read_dynamic_section(&exe).unwrap().unwrap();
        assert!(
            dynamic.needed.iter().any(|n| n.starts_with("libc.so")),
            "a test binary links libc: {dynamic:?}"
        );
        // And the host resolves it, through the real cache and directories.
        let view = LoaderView::new(
            Vec::new(),
            Vec::new(),
            LdCache::load(Path::new(LD_SO_CACHE)),
            SYSTEM_LIBRARY_DIRS.iter().map(PathBuf::from).collect(),
        );
        view.check(&exe).unwrap();
    }

    #[test]
    fn interpreter_rejects_truncated_objects_and_distinguishes_absence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("obj");
        write(&path, &elf_fixture(&[], None));
        assert_eq!(read_interpreter(&path).unwrap(), None);
        assert_eq!(
            read_dynamic_section(&path).unwrap(),
            Some(DynamicSection::default())
        );
        write(&path, b"\x7fELF\x02\x01");
        assert!(read_interpreter(&path).is_err());
    }

    #[test]
    fn non_elf_and_truncated_files_are_unknown_not_dependency_free() {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("script");
        write(&script, b"#!/bin/sh\nexit 0\n");
        assert_eq!(read_dynamic_section(&script).unwrap(), None);
        let truncated = dir.path().join("truncated");
        let mut bytes = elf_fixture(&["libfoo.so"], None);
        bytes.truncate(100);
        write(&truncated, &bytes);
        assert_eq!(read_dynamic_section(&truncated).unwrap(), None);
    }

    #[test]
    fn a_library_missing_from_the_runtime_is_named_with_where_it_was_looked_for() {
        let fx = Fixture::new();
        write(
            &fx.payload.join("bin/app"),
            &elf_fixture(&["libstd-55999f89fc36b51f.so"], None),
        );
        write(
            &fx.runtime.join("lib/libstd-0123456789abcdef.so"),
            &elf_fixture(&[], None),
        );
        let missing = fx.view(LdCache::Absent).check(&entry()).unwrap_err();
        assert_eq!(missing.library, "libstd-55999f89fc36b51f.so");
        assert_eq!(missing.needed_by, entry());
        assert_eq!(
            missing.searched,
            [fx.runtime.join("lib"), fx.system.join("lib")]
        );
    }

    #[test]
    fn libraries_resolve_through_the_runtime_runpath_cache_and_system_dirs() {
        let fx = Fixture::new();
        write(
            &fx.payload.join("bin/app"),
            &elf_fixture(
                &[
                    "libruntime_dylib.so",
                    "libbundled.so",
                    "libcached.so.1",
                    "libsystem.so.2",
                    "ld-linux-x86-64.so.2",
                ],
                Some("$ORIGIN/../lib"),
            ),
        );
        write(
            &fx.runtime.join("lib/libruntime_dylib.so"),
            &elf_fixture(&[], None),
        );
        write(
            &fx.payload.join("lib/libbundled.so"),
            &elf_fixture(&[], None),
        );
        write(&fx.system.join("lib/libsystem.so.2"), b"");
        let mut cached = HashSet::new();
        cached.insert("libcached.so.1".to_string());
        fx.view(LdCache::Names(cached)).check(&entry()).unwrap();
    }

    #[test]
    fn a_shipped_library_is_checked_for_its_own_dependencies() {
        let fx = Fixture::new();
        write(
            &fx.payload.join("bin/app"),
            &elf_fixture(&["libruntime_dylib.so"], None),
        );
        write(
            &fx.runtime.join("lib/libruntime_dylib.so"),
            &elf_fixture(&["libstd-55999f89fc36b51f.so"], None),
        );
        let missing = fx.view(LdCache::Absent).check(&entry()).unwrap_err();
        assert_eq!(missing.library, "libstd-55999f89fc36b51f.so");
        assert_eq!(
            missing.needed_by,
            Path::new("/run/tilo-runtime/1/lib/libruntime_dylib.so")
        );
    }

    #[test]
    fn hwcaps_subdirectories_count() {
        let fx = Fixture::new();
        write(
            &fx.payload.join("bin/app"),
            &elf_fixture(&["libfast.so"], None),
        );
        write(
            &fx.runtime.join("lib/glibc-hwcaps/x86-64-v3/libfast.so"),
            b"",
        );
        fx.view(LdCache::Absent).check(&entry()).unwrap();
    }

    #[test]
    fn undecidable_search_paths_and_unreadable_entries_never_refuse() {
        let fx = Fixture::new();
        // `$PLATFORM` is the loader's to expand.
        write(
            &fx.payload.join("bin/app"),
            &elf_fixture(&["libgone.so"], Some("$ORIGIN/$PLATFORM")),
        );
        fx.view(LdCache::Absent).check(&entry()).unwrap();
        // A RUNPATH into a sandbox-private tree the launcher did not mount.
        write(
            &fx.payload.join("bin/app"),
            &elf_fixture(&["libgone.so"], Some("/run/elsewhere")),
        );
        fx.view(LdCache::Absent).check(&entry()).unwrap();
        // An entrypoint that does not exist or is a script.
        fx.view(LdCache::Absent)
            .check(Path::new("/var/tilo-apps/app/bin/missing"))
            .unwrap();
    }

    #[test]
    fn ld_cache_names_are_parsed_and_unknown_formats_fall_back_to_a_byte_search() {
        // A one-entry new-format cache: header, entry, string table.
        let mut bytes = b"glibc-ld.so.cache1.1".to_vec();
        bytes.extend_from_slice(&1u32.to_le_bytes()); // nlibs
        bytes.extend_from_slice(&0u32.to_le_bytes()); // len_strings
        bytes.resize(48, 0);
        let strings = 48 + 24;
        let key = strings;
        let value = key + "libz.so.1\0".len();
        bytes.extend_from_slice(&1i32.to_le_bytes());
        bytes.extend_from_slice(&u32::try_from(key).unwrap().to_le_bytes());
        bytes.extend_from_slice(&u32::try_from(value).unwrap().to_le_bytes());
        bytes.extend_from_slice(&[0u8; 12]);
        bytes.extend_from_slice(b"libz.so.1\0/usr/lib64/libz.so.1\0");
        let cache = LdCache::from_bytes(bytes);
        assert!(matches!(cache, LdCache::Names(_)), "{cache:?}");
        assert!(cache.contains("libz.so.1"));
        assert!(!cache.contains("libstd-55999f89fc36b51f.so"));

        let opaque = LdCache::from_bytes(b"something else\0libq.so.3\0".to_vec());
        assert!(matches!(opaque, LdCache::Opaque(_)));
        assert!(opaque.contains("libq.so.3"));
        assert!(!opaque.contains("libq.so"));
    }

    #[test]
    fn the_host_ld_cache_parses_when_present() {
        match LdCache::load(Path::new(LD_SO_CACHE)) {
            LdCache::Absent => {}
            LdCache::Names(names) => assert!(!names.is_empty()),
            LdCache::Opaque(_) => panic!("the host cache is not in glibc's current format"),
        }
    }
}
