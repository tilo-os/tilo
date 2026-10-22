//! Packing files and folders into a new archive.

use std::fs::{self, File};
use std::io::{self, Read, Seek, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};

use crate::{Observer, cancelled};

/// The kind of archive [`create`] writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Packing {
    /// Zip with deflate: opens everywhere.
    Zip,
    /// Tar compressed with zstd: keeps permissions and is smaller and
    /// faster to make.
    TarZst,
}

impl Packing {
    /// The extension an archive of this kind is given.
    #[must_use]
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Zip => "zip",
            Self::TarZst => "tar.zst",
        }
    }
}

/// Reads a file being packed, counting its bytes and stopping when asked.
struct Counted<'a> {
    file: File,
    observer: &'a dyn Observer,
}

impl Read for Counted<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.observer.cancelled() {
            return Err(cancelled());
        }
        let read = self.file.read(buf)?;
        self.observer.advance(read as u64);
        Ok(read)
    }
}

/// Pack `paths`, each under its own name with folders and everything in
/// them, into `out`, a new empty file the caller made and names. Symbolic
/// links are left out: a selected link is not what it points at, and the
/// archive holds no way out.
///
/// The bytes read from the packed files reach the observer, which can stop
/// the packing ([`crate::is_cancelled`]); what was written to `out` is then
/// the caller's to remove.
///
/// # Errors
///
/// A path has no name or cannot be read, the observer cancelled, or
/// writing `out` failed.
pub fn create(
    paths: &[PathBuf],
    out: File,
    packing: Packing,
    observer: &dyn Observer,
) -> io::Result<File> {
    let mut items = Vec::new();
    for path in paths {
        let name = path
            .file_name()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "a path has no name"))?;
        collect(path, PathBuf::from(name), &mut items)?;
    }
    match packing {
        Packing::Zip => zip(&items, out, observer),
        Packing::TarZst => tar_zst(&items, out, observer),
    }
}

/// One file or folder to pack: where it is, and its name in the archive.
struct Item {
    source: PathBuf,
    name: PathBuf,
    directory: bool,
}

/// `source` and, for a folder, everything below it, in order; links left
/// out.
fn collect(source: &Path, name: PathBuf, items: &mut Vec<Item>) -> io::Result<()> {
    let kind = fs::symlink_metadata(source)?.file_type();
    if kind.is_dir() {
        items.push(Item {
            source: source.to_path_buf(),
            name: name.clone(),
            directory: true,
        });
        let mut children: Vec<_> = fs::read_dir(source)?.collect::<io::Result<_>>()?;
        children.sort_by_key(fs::DirEntry::file_name);
        for child in children {
            collect(&child.path(), name.join(child.file_name()), items)?;
        }
    } else if kind.is_file() {
        items.push(Item {
            source: source.to_path_buf(),
            name,
            directory: false,
        });
    }
    Ok(())
}

/// An archive name with forward slashes on every host.
fn archive_name(name: &Path) -> String {
    name.components()
        .map(|part| part.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(unix)]
fn mode_of(meta: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;
    meta.permissions().mode() & 0o777
}

#[cfg(not(unix))]
fn mode_of(meta: &fs::Metadata) -> u32 {
    if meta.is_dir() { 0o755 } else { 0o644 }
}

/// `time` as zip keeps it: the local time, to the even second; none
/// before 1980, which zip cannot hold.
fn zip_time(time: std::time::SystemTime) -> Option<zip::DateTime> {
    use chrono::{Datelike as _, Timelike as _};
    let local = chrono::DateTime::<chrono::Local>::from(time).naive_local();
    zip::DateTime::from_date_and_time(
        u16::try_from(local.year()).ok()?,
        u8::try_from(local.month()).ok()?,
        u8::try_from(local.day()).ok()?,
        u8::try_from(local.hour()).ok()?,
        u8::try_from(local.minute()).ok()?,
        u8::try_from(local.second()).ok()?,
    )
    .ok()
}

fn zip_error(error: zip::result::ZipError) -> io::Error {
    match error {
        zip::result::ZipError::Io(error) => error,
        other => io::Error::other(other.to_string()),
    }
}

fn zip(items: &[Item], out: File, observer: &dyn Observer) -> io::Result<File> {
    let mut zip = zip::ZipWriter::new(out);
    for item in items {
        if observer.cancelled() {
            return Err(cancelled());
        }
        let meta = fs::symlink_metadata(&item.source)?;
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(mode_of(&meta))
            .large_file(meta.len() >= u64::from(u32::MAX));
        let options = match meta.modified().ok().and_then(zip_time) {
            Some(time) => options.last_modified_time(time),
            None => options,
        };
        let name = archive_name(&item.name);
        if item.directory {
            zip.add_directory(name, options).map_err(zip_error)?;
        } else {
            zip.start_file(name, options).map_err(zip_error)?;
            let mut source = Counted {
                file: File::open(&item.source)?,
                observer,
            };
            io::copy(&mut source, &mut zip)?;
        }
    }
    let mut file = zip.finish().map_err(zip_error)?;
    file.flush()?;
    file.rewind()?;
    Ok(file)
}

/// Tar on this thread, zstd on another, joined by a pipe of chunks.
///
/// The zstd encoder (ruzstd) panics on a read or write error, so neither
/// side of it ever fails: the pipe reads as ended once the tar side stops,
/// and the file side keeps its first error and swallows the rest. Both
/// errors are taken up after the encoder returns.
fn tar_zst(items: &[Item], out: File, observer: &dyn Observer) -> io::Result<File> {
    let stop = AtomicBool::new(false);
    let stop = &stop;
    let (sender, receiver) = sync_channel::<Vec<u8>>(4);
    let (packed, encoded) = std::thread::scope(|scope| {
        let encoder = scope.spawn(move || {
            let mut sink = Sink {
                file: out,
                error: None,
                stop,
            };
            let source = Drain {
                receiver,
                chunk: Vec::new(),
                at: 0,
            };
            ruzstd::encoding::compress(
                source,
                &mut sink,
                ruzstd::encoding::CompressionLevel::Fastest,
            );
            sink
        });
        // The builder and its pipe end here, error or not, which ends the
        // encoder's input before it is waited for.
        let packed = {
            let mut builder = tar::Builder::new(Pipe {
                sender,
                buffer: Vec::with_capacity(PIPE_CHUNK),
                stop,
            });
            builder.follow_symlinks(false);
            match append_all(&mut builder, items, observer) {
                Ok(()) => builder.into_inner().and_then(|mut pipe| pipe.flush()),
                Err(error) => {
                    stop.store(true, Ordering::Relaxed);
                    Err(error)
                }
            }
        };
        (packed, encoder.join())
    });
    let mut sink = encoded.map_err(|_| io::Error::other("packing stopped unexpectedly"))?;
    if let Some(error) = sink.error.take() {
        return Err(error);
    }
    packed?;
    let mut file = sink.file;
    file.flush()?;
    file.rewind()?;
    Ok(file)
}

fn append_all<W: Write>(
    builder: &mut tar::Builder<W>,
    items: &[Item],
    observer: &dyn Observer,
) -> io::Result<()> {
    for item in items {
        if observer.cancelled() {
            return Err(cancelled());
        }
        let meta = fs::symlink_metadata(&item.source)?;
        let mut header = tar::Header::new_gnu();
        header.set_metadata_in_mode(&meta, tar::HeaderMode::Complete);
        header.set_mode(mode_of(&meta));
        if item.directory {
            header.set_size(0);
            builder.append_data(&mut header, &item.name, io::empty())?;
        } else {
            let source = Counted {
                file: File::open(&item.source)?,
                observer,
            };
            builder.append_data(&mut header, &item.name, source)?;
        }
    }
    Ok(())
}

const PIPE_CHUNK: usize = 1 << 20;

/// The tar side of the pipe.
struct Pipe<'a> {
    sender: SyncSender<Vec<u8>>,
    buffer: Vec<u8>,
    stop: &'a AtomicBool,
}

impl Write for Pipe<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.stop.load(Ordering::Relaxed) {
            return Err(io::Error::other("packing stopped"));
        }
        self.buffer.extend_from_slice(buf);
        if self.buffer.len() >= PIPE_CHUNK {
            self.flush()?;
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        let chunk = std::mem::replace(&mut self.buffer, Vec::with_capacity(PIPE_CHUNK));
        self.sender
            .send(chunk)
            .map_err(|_| io::Error::other("packing stopped"))
    }
}

/// The encoder's side of the pipe: ends when the tar side is done or gone.
struct Drain {
    receiver: Receiver<Vec<u8>>,
    chunk: Vec<u8>,
    at: usize,
}

impl Read for Drain {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        while self.at >= self.chunk.len() {
            match self.receiver.recv() {
                Ok(chunk) => {
                    self.chunk = chunk;
                    self.at = 0;
                }
                Err(_) => return Ok(0),
            }
        }
        let rest = self.chunk.get(self.at..).unwrap_or_default();
        let count = rest.len().min(buf.len());
        if let (Some(to), Some(from)) = (buf.get_mut(..count), rest.get(..count)) {
            to.copy_from_slice(from);
        }
        self.at = self.at.saturating_add(count);
        Ok(count)
    }
}

/// The file side of the encoder: keeps the first write error, then takes
/// and drops everything so the encoder runs out without panicking.
struct Sink<'a> {
    file: File,
    error: Option<io::Error>,
    stop: &'a AtomicBool,
}

impl Write for Sink<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.error.is_none()
            && let Err(error) = self.file.write_all(buf)
        {
            self.error = Some(error);
            self.stop.store(true, Ordering::Relaxed);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
