//! Walking an archive's entries in order, each with its contents.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::{Compression, Entry, Format, Kind, Observer, cancelled, safe_relative};

/// What a visitor wants after an entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flow {
    Continue,
    Stop,
}

/// The largest dictionary an xz stream may ask for, in KiB: what `xz -9e`
/// uses with room to spare. A stream asking for more is refused rather than
/// allowed to take the machine's memory.
const XZ_MEMORY_KIB: u32 = 1024 * 1024;

/// Reads a file, counting the bytes for the observer and stopping when it
/// asks. Seeking (zip, 7z) moves without counting.
struct Counted<'a, R> {
    inner: R,
    observer: &'a dyn Observer,
}

impl<R: Read> Read for Counted<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.observer.cancelled() {
            return Err(cancelled());
        }
        let read = self.inner.read(buf)?;
        self.observer.advance(read as u64);
        Ok(read)
    }
}

impl<R: Seek> Seek for Counted<'_, R> {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        self.inner.seek(to)
    }
}

/// The format of the archive at `path`: by its name, else by its first
/// bytes.
fn format_of(path: &Path, file: &mut File) -> io::Result<Format> {
    if let Some(format) = Format::of_path(path) {
        return Ok(format);
    }
    let mut magic = [0_u8; 262];
    let mut filled = 0;
    while filled < magic.len() {
        let Some(rest) = magic.get_mut(filled..) else {
            break;
        };
        match file.read(rest)? {
            0 => break,
            read => filled = filled.saturating_add(read),
        }
    }
    file.seek(SeekFrom::Start(0))?;
    let head = magic.get(..filled).unwrap_or_default();
    let format = if head.starts_with(b"PK\x03\x04") || head.starts_with(b"PK\x05\x06") {
        Format::Zip
    } else if head.starts_with(b"7z\xbc\xaf\x27\x1c") {
        Format::SevenZ
    } else if head.starts_with(&[0x1f, 0x8b]) {
        Format::Tar(Compression::Gzip)
    } else if head.starts_with(b"\xfd7zXZ\0") {
        Format::Tar(Compression::Xz)
    } else if head.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) {
        Format::Tar(Compression::Zstd)
    } else if head.starts_with(b"BZh") {
        Format::Tar(Compression::Bzip2)
    } else if head.get(257..262) == Some(b"ustar") {
        Format::Tar(Compression::None)
    } else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "not an archive this can read",
        ));
    };
    Ok(format)
}

/// Visit every entry of the archive at `path` in order, with a reader of its
/// contents (empty for anything but a file or, in zip and 7z, a symlink,
/// whose contents are its target). A visitor that does not read an entry
/// skips it.
///
/// An entry whose name would land outside the archive's folder fails the
/// walk ([`crate::is_unsafe`]) before any visitor sees it. Entries naming
/// only the folder itself (`./`) are left out.
///
/// # Errors
///
/// The archive cannot be opened or decoded, a name is unsafe, the observer
/// cancelled ([`crate::is_cancelled`]), or the visitor's error.
pub fn walk(
    path: &Path,
    observer: &dyn Observer,
    visit: &mut dyn FnMut(&Entry, &mut dyn Read) -> io::Result<Flow>,
) -> io::Result<()> {
    let mut file = File::open(path)?;
    let format = format_of(path, &mut file)?;
    let counted = Counted {
        inner: file,
        observer,
    };
    match format {
        Format::Zip => walk_zip(counted, visit),
        Format::SevenZ => walk_7z(counted, visit),
        Format::Tar(Compression::None) => walk_tar(counted, visit),
        Format::Tar(Compression::Gzip) => {
            walk_tar(flate2::read::MultiGzDecoder::new(counted), visit)
        }
        Format::Tar(Compression::Xz) => walk_tar(
            lzma_rust2::XzReader::new_mem_limit(counted, true, XZ_MEMORY_KIB),
            visit,
        ),
        Format::Tar(Compression::Zstd) => {
            let decoder = ruzstd::decoding::StreamingDecoder::new(counted)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
            walk_tar(decoder, visit)
        }
        Format::Tar(Compression::Bzip2) => {
            walk_tar(bzip2::read::MultiBzDecoder::new(counted), visit)
        }
    }
}

/// Every entry of the archive at `path`, in order.
///
/// # Errors
///
/// As [`walk`].
pub fn list(path: &Path, observer: &dyn Observer) -> io::Result<Vec<Entry>> {
    let mut entries = Vec::new();
    walk(path, observer, &mut |entry, _| {
        entries.push(entry.clone());
        Ok(Flow::Continue)
    })?;
    Ok(entries)
}

/// Copy the file `member` of the archive at `path` to `out`; the bytes
/// copied.
pub fn read_member(
    path: &Path,
    member: &Path,
    out: &mut dyn io::Write,
    observer: &dyn Observer,
) -> io::Result<u64> {
    let mut copied = None;
    walk(path, observer, &mut |entry, contents| {
        if entry.kind != Kind::File || entry.path != member {
            return Ok(Flow::Continue);
        }
        copied = Some(io::copy(contents, out)?);
        Ok(Flow::Stop)
    })?;
    copied.ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such file in the archive"))
}

fn walk_tar<R: Read>(
    reader: R,
    visit: &mut dyn FnMut(&Entry, &mut dyn Read) -> io::Result<Flow>,
) -> io::Result<()> {
    let mut archive = tar::Archive::new(reader);
    for item in archive.entries()? {
        let mut item = item?;
        let header = item.header();
        let kind = header.entry_type();
        if kind.is_pax_global_extensions()
            || kind.is_pax_local_extensions()
            || kind.is_gnu_longname()
            || kind.is_gnu_longlink()
        {
            continue;
        }
        let Some(path) = safe_relative(&item.path()?)? else {
            continue;
        };
        let kind = if kind.is_dir() {
            Kind::Directory
        } else if kind.is_symlink() {
            Kind::Symlink
        } else if kind.is_hard_link() {
            Kind::HardLink
        } else if kind.is_file() || kind.is_contiguous() || kind.is_gnu_sparse() {
            Kind::File
        } else {
            Kind::Special
        };
        let link = match kind {
            Kind::Symlink | Kind::HardLink => item.link_name()?.map(std::borrow::Cow::into_owned),
            _ => None,
        };
        let entry = Entry {
            path,
            size: if kind == Kind::File { item.size() } else { 0 },
            kind,
            mode: header.mode().ok(),
            modified: header.mtime().ok().and_then(|seconds| {
                SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(seconds))
            }),
            link,
        };
        let flow = if kind == Kind::File {
            visit(&entry, &mut item)?
        } else {
            visit(&entry, &mut io::empty())?
        };
        if flow == Flow::Stop {
            break;
        }
    }
    Ok(())
}

fn walk_zip<R: Read + Seek>(
    reader: R,
    visit: &mut dyn FnMut(&Entry, &mut dyn Read) -> io::Result<Flow>,
) -> io::Result<()> {
    let mut archive = zip::ZipArchive::new(reader).map_err(zip_error)?;
    for index in 0..archive.len() {
        let mut item = archive.by_index(index).map_err(zip_error)?;
        // Some Windows tools separate folders with backslashes.
        let name = item.name().map_err(zip_error)?.replace('\\', "/");
        let Some(path) = safe_relative(Path::new(&name))? else {
            continue;
        };
        let mode = item.unix_mode();
        let kind = if item.is_dir() {
            Kind::Directory
        } else if item.is_symlink() {
            Kind::Symlink
        } else if mode.is_some_and(|mode| {
            let format = mode & 0o170_000;
            format != 0 && format != 0o100_000
        }) {
            Kind::Special
        } else {
            Kind::File
        };
        let size = item.size();
        let link = if kind == Kind::Symlink {
            let mut target = Vec::new();
            (&mut item).take(4096).read_to_end(&mut target)?;
            Some(PathBuf::from(String::from_utf8_lossy(&target).into_owned()))
        } else {
            None
        };
        let entry = Entry {
            path,
            size: if kind == Kind::File { size } else { 0 },
            kind,
            mode: mode.map(|mode| mode & 0o7777),
            modified: item.last_modified().and_then(zip_time),
            link,
        };
        let flow = if kind == Kind::File {
            visit(&entry, &mut item)?
        } else {
            visit(&entry, &mut io::empty())?
        };
        if flow == Flow::Stop {
            break;
        }
    }
    Ok(())
}

/// A zip entry's time, which zip keeps as a local time without a zone:
/// read in this device's zone, as the tool that wrote it most likely meant.
fn zip_time(time: zip::DateTime) -> Option<SystemTime> {
    let local = chrono::NaiveDate::from_ymd_opt(
        i32::from(time.year()),
        u32::from(time.month()),
        u32::from(time.day()),
    )?
    .and_hms_opt(
        u32::from(time.hour()),
        u32::from(time.minute()),
        u32::from(time.second()),
    )?;
    local
        .and_local_timezone(chrono::Local)
        .earliest()
        .map(SystemTime::from)
}

fn zip_error(error: zip::result::ZipError) -> io::Error {
    match error {
        zip::result::ZipError::Io(error) => error,
        other => io::Error::new(io::ErrorKind::InvalidData, other.to_string()),
    }
}

/// The Unix mode 7z keeps in the high half of an entry's attributes, when
/// the archiver set the flag that says so.
fn sevenz_mode(entry: &sevenz_rust2::ArchiveEntry) -> Option<u32> {
    const UNIX_EXTENSION: u32 = 0x8000;
    (entry.has_windows_attributes && entry.windows_attributes & UNIX_EXTENSION != 0)
        .then_some(entry.windows_attributes >> 16)
}

fn walk_7z<R: Read + Seek>(
    reader: R,
    visit: &mut dyn FnMut(&Entry, &mut dyn Read) -> io::Result<Flow>,
) -> io::Result<()> {
    let mut archive = sevenz_rust2::ArchiveReader::new(reader, sevenz_rust2::Password::empty())
        .map_err(sevenz_error)?;
    // The visitor's own error (a cancel, an unsafe name, a full disk) is
    // kept here and the walk stopped, so it is returned as it was rather
    // than as 7z's.
    let mut failure: Option<io::Error> = None;
    let walked = archive.for_each_entries(|item, contents| {
        if item.is_anti_item {
            return Ok(true);
        }
        let entry = match sevenz_entry(item, contents) {
            Ok(Some(entry)) => entry,
            Ok(None) => return Ok(true),
            Err(error) => {
                failure = Some(error);
                return Ok(false);
            }
        };
        let flow = if entry.kind == Kind::File {
            visit(&entry, contents)
        } else {
            visit(&entry, &mut io::empty())
        };
        match flow {
            Ok(Flow::Continue) => Ok(true),
            Ok(Flow::Stop) => Ok(false),
            Err(error) => {
                failure = Some(error);
                Ok(false)
            }
        }
    });
    if let Some(error) = failure {
        return Err(error);
    }
    walked.map_err(sevenz_error)
}

/// `item` as an entry; `None` for one naming only the folder.
fn sevenz_entry(
    item: &sevenz_rust2::ArchiveEntry,
    contents: &mut dyn Read,
) -> io::Result<Option<Entry>> {
    let name = item.name.replace('\\', "/");
    let Some(path) = safe_relative(Path::new(&name))? else {
        return Ok(None);
    };
    let mode = sevenz_mode(item);
    let format = mode.map_or(0, |mode| mode & 0o170_000);
    let kind = if item.is_directory {
        Kind::Directory
    } else if format == 0o120_000 {
        Kind::Symlink
    } else if format != 0 && format != 0o100_000 {
        Kind::Special
    } else {
        Kind::File
    };
    let link = if kind == Kind::Symlink {
        let mut target = Vec::new();
        contents.take(4096).read_to_end(&mut target)?;
        Some(PathBuf::from(String::from_utf8_lossy(&target).into_owned()))
    } else {
        None
    };
    Ok(Some(Entry {
        path,
        size: if kind == Kind::File { item.size } else { 0 },
        kind,
        mode: mode.map(|mode| mode & 0o7777),
        modified: item
            .has_last_modified_date
            .then(|| SystemTime::from(item.last_modified_date)),
        link,
    }))
}

fn sevenz_error(error: sevenz_rust2::Error) -> io::Error {
    match error {
        sevenz_rust2::Error::Io(error, _) | sevenz_rust2::Error::FileOpen(error, _) => error,
        other => io::Error::new(io::ErrorKind::InvalidData, other.to_string()),
    }
}
