//! Safe, native archive operations used by Files.
//!
//! Archive entry names are untrusted.  Extraction never joins an absolute path,
//! follows an archive symlink, or permits `..` to escape the chosen directory.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, Write};
use std::path::{Component, Path, PathBuf};

const MAX_ENTRIES: usize = 100_000;
const MAX_EXPANDED_BYTES: u64 = 16 * 1024 * 1024 * 1024;

struct StagingDirectory(PathBuf);

impl Drop for StagingDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct PartialArchive<'a> {
    path: &'a Path,
    committed: bool,
}

impl Drop for PartialArchive<'_> {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::remove_file(self.path);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArchiveEntry {
    pub path: PathBuf,
    pub size: u64,
    pub is_dir: bool,
}

/// Resolve a Files navigation path to an archive and an in-archive prefix.
/// The archive itself is the root; descendants use ordinary-looking paths so
/// Back, Forward, Up, breadcrumbs, keyboard navigation and touch activation do
/// not need a second navigation model.
pub fn location(path: &Path) -> Option<(PathBuf, PathBuf)> {
    for candidate in path.ancestors() {
        if candidate.is_file() && is_supported(candidate) {
            let prefix = path.strip_prefix(candidate).ok()?.to_path_buf();
            return Some((candidate.to_path_buf(), prefix));
        }
    }
    None
}

/// List only the direct children at `prefix`, synthesizing directory rows for
/// archives that omit explicit directory members.
///
/// # Errors
///
/// Returns an error when the archive cannot be opened or decoded, an entry is
/// unsafe, or the configured expansion limits are exceeded.
pub fn list_directory(path: &Path, prefix: &Path) -> io::Result<Vec<ArchiveEntry>> {
    let prefix = if prefix.as_os_str().is_empty() {
        PathBuf::new()
    } else {
        safe_relative(prefix)?
    };
    let mut children: BTreeMap<PathBuf, ArchiveEntry> = BTreeMap::new();
    for entry in list(path)? {
        let Ok(relative) = entry.path.strip_prefix(&prefix) else {
            continue;
        };
        let mut parts = relative.components();
        let Some(Component::Normal(first)) = parts.next() else {
            continue;
        };
        let child_path = prefix.join(first);
        let has_descendants = parts.next().is_some();
        let child = ArchiveEntry {
            path: child_path.clone(),
            size: if has_descendants { 0 } else { entry.size },
            is_dir: has_descendants || entry.is_dir,
        };
        children
            .entry(child_path)
            .and_modify(|existing| {
                if child.is_dir {
                    existing.is_dir = true;
                    existing.size = 0;
                }
            })
            .or_insert(child);
    }
    Ok(children.into_values().collect())
}

fn safe_relative(name: &Path) -> io::Result<PathBuf> {
    let mut clean = PathBuf::new();
    for part in name.components() {
        match part {
            Component::Normal(value) => clean.push(value),
            Component::CurDir => {}
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "archive entry escapes its destination",
                ));
            }
        }
    }
    if clean.as_os_str().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "empty archive entry",
        ));
    }
    Ok(clean)
}

fn account(count: usize, total: u64, size: u64) -> io::Result<u64> {
    if count >= MAX_ENTRIES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "archive has too many entries",
        ));
    }
    let next = total
        .checked_add(size)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "archive size overflow"))?;
    if next > MAX_EXPANDED_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "archive expands beyond the safety limit",
        ));
    }
    Ok(next)
}

fn zip_mode_is_symlink(mode: Option<u32>) -> bool {
    mode.is_some_and(|mode| mode & 0o170_000 == 0o120_000)
}

pub fn is_supported(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|v| v.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let named = Path::new(&name);
    let extension = named
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default();
    matches!(extension, "zip" | "tar" | "tgz")
        || (extension == "gz"
            && named
                .file_stem()
                .and_then(|stem| Path::new(stem).extension())
                .is_some_and(|inner| inner.eq_ignore_ascii_case("tar")))
}

/// Decode the entries in a supported archive.
///
/// # Errors
///
/// Returns an error when the archive cannot be opened or decoded, an entry is
/// unsafe, or the configured expansion limits are exceeded.
pub fn list(path: &Path) -> io::Result<Vec<ArchiveEntry>> {
    if path
        .extension()
        .and_then(|v| v.to_str())
        .is_some_and(|v| v.eq_ignore_ascii_case("zip"))
    {
        return list_zip(File::open(path)?);
    }
    let file = File::open(path)?;
    if path.file_name().and_then(|v| v.to_str()).is_some_and(|v| {
        Path::new(v).extension().is_some_and(|extension| {
            extension.eq_ignore_ascii_case("gz") || extension.eq_ignore_ascii_case("tgz")
        })
    }) {
        list_tar(flate2::read::GzDecoder::new(file))
    } else {
        list_tar(file)
    }
}

fn list_zip<R: Read + Seek>(reader: R) -> io::Result<Vec<ArchiveEntry>> {
    let mut archive = zip::ZipArchive::new(reader).map_err(io::Error::other)?;
    let mut result = Vec::with_capacity(archive.len().min(4096));
    let mut total = 0;
    for index in 0..archive.len() {
        let entry = archive.by_index(index).map_err(io::Error::other)?;
        if zip_mode_is_symlink(entry.unix_mode()) {
            continue;
        }
        total = account(result.len(), total, entry.size())?;
        result.push(ArchiveEntry {
            path: safe_relative(Path::new(&*entry.name().map_err(io::Error::other)?))?,
            size: entry.size(),
            is_dir: entry.is_dir(),
        });
    }
    Ok(result)
}

fn list_tar<R: Read>(reader: R) -> io::Result<Vec<ArchiveEntry>> {
    let mut archive = tar::Archive::new(reader);
    let mut result = Vec::new();
    let mut total = 0;
    for item in archive.entries()? {
        let entry = item?;
        let kind = entry.header().entry_type();
        if kind.is_symlink() || kind.is_hard_link() {
            continue;
        }
        let size = entry.header().size()?;
        total = account(result.len(), total, size)?;
        result.push(ArchiveEntry {
            path: safe_relative(&entry.path()?)?,
            size,
            is_dir: kind.is_dir(),
        });
    }
    Ok(result)
}

/// Extract a supported archive atomically into a new destination directory.
///
/// # Errors
///
/// Returns an error for an existing or invalid destination, unsafe archive
/// entries, size-limit violations, decode failures, or filesystem failures.
pub fn extract(path: &Path, destination: &Path) -> io::Result<Vec<PathBuf>> {
    if destination.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "the destination already exists",
        ));
    }
    let parent = destination
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "destination has no parent"))?;
    fs::create_dir_all(parent)?;
    let staging = extraction_staging_path(destination)?;
    fs::create_dir(&staging)?;
    let cleanup = StagingDirectory(staging.clone());
    let result = if path
        .extension()
        .and_then(|v| v.to_str())
        .is_some_and(|v| v.eq_ignore_ascii_case("zip"))
    {
        extract_zip(File::open(path)?, &staging)
    } else {
        let file = File::open(path)?;
        if path.file_name().and_then(|v| v.to_str()).is_some_and(|v| {
            Path::new(v).extension().is_some_and(|extension| {
                extension.eq_ignore_ascii_case("gz") || extension.eq_ignore_ascii_case("tgz")
            })
        }) {
            extract_tar(flate2::read::GzDecoder::new(file), &staging)
        } else {
            extract_tar(file, &staging)
        }
    }?;
    fs::rename(&staging, destination)?;
    std::mem::forget(cleanup);
    Ok(result)
}

fn extraction_staging_path(destination: &Path) -> io::Result<PathBuf> {
    let parent = destination
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "destination has no parent"))?;
    let name = destination
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "destination has no name"))?
        .to_string_lossy();
    for attempt in 0..1_000u16 {
        let candidate = parent.join(format!(
            ".{name}.extracting-{}-{attempt}",
            std::process::id()
        ));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not reserve a temporary extraction folder",
    ))
}

fn extract_zip<R: Read + Seek>(reader: R, destination: &Path) -> io::Result<Vec<PathBuf>> {
    let mut archive = zip::ZipArchive::new(reader).map_err(io::Error::other)?;
    let mut written = Vec::new();
    let mut total = 0;
    let mut count = 0;
    for index in 0..archive.len() {
        let entry = archive.by_index(index).map_err(io::Error::other)?;
        if zip_mode_is_symlink(entry.unix_mode()) {
            continue;
        }
        let size = entry.size();
        total = account(count, total, size)?;
        count = count.saturating_add(1);
        let relative = safe_relative(Path::new(&*entry.name().map_err(io::Error::other)?))?;
        let output = destination.join(&relative);
        if entry.is_dir() {
            fs::create_dir_all(&output)?;
            continue;
        }
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&output)?;
        let copied = io::copy(&mut entry.take(size), &mut file)?;
        if copied != size {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "truncated ZIP entry",
            ));
        }
        written.push(relative);
    }
    Ok(written)
}

fn extract_tar<R: Read>(reader: R, destination: &Path) -> io::Result<Vec<PathBuf>> {
    let mut archive = tar::Archive::new(reader);
    let mut written = Vec::new();
    let mut total = 0;
    let mut count = 0;
    for item in archive.entries()? {
        let mut entry = item?;
        let kind = entry.header().entry_type();
        if kind.is_symlink() || kind.is_hard_link() {
            continue;
        }
        let size = entry.header().size()?;
        total = account(count, total, size)?;
        count = count.saturating_add(1);
        let relative = safe_relative(&entry.path()?)?;
        let output = destination.join(&relative);
        if kind.is_dir() {
            fs::create_dir_all(&output)?;
            continue;
        }
        if !kind.is_file() {
            continue;
        }
        if let Some(parent) = output.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&output)?;
        let copied = io::copy(&mut entry, &mut file)?;
        if copied != size {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "truncated TAR entry",
            ));
        }
        written.push(relative);
    }
    Ok(written)
}

/// Create a ZIP archive atomically from the selected files and directories.
///
/// # Errors
///
/// Returns an error when an input cannot be read, the output cannot be
/// created, or ZIP encoding and finalization fail.
pub fn create_zip(paths: &[PathBuf], output: &Path) -> io::Result<()> {
    let parent = output.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "archive has no parent folder")
    })?;
    // Reserve private scratch storage without truncating a neighbour or
    // following a link left at a predictable .part name.
    let temporary = parent.join(format!(".tilo-archive-{}", uuid::Uuid::new_v4()));
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    let mut cleanup = PartialArchive {
        path: &temporary,
        committed: false,
    };
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for path in paths {
        let name = path
            .file_name()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path has no file name"))?;
        let archive_path = PathBuf::from(name);
        if path.is_dir() {
            add_directory_tree(&mut zip, path, &archive_path, options)?;
        } else if path.symlink_metadata()?.file_type().is_file() {
            add_zip_file(&mut zip, path, &archive_path, options)?;
        }
    }
    let result = zip.finish().map_err(io::Error::other).and_then(|file| {
        file.sync_all()?;
        host_fs::rename_noreplace(&temporary, output)
    });
    if result.is_ok() {
        cleanup.committed = true;
    }
    result
}

fn add_directory_tree<W: Write + Seek>(
    zip: &mut zip::ZipWriter<W>,
    source: &Path,
    archive_path: &Path,
    options: zip::write::SimpleFileOptions,
) -> io::Result<()> {
    zip.add_directory(archive_path.to_string_lossy(), options)
        .map_err(io::Error::other)?;
    for item in fs::read_dir(source)? {
        let item = item?;
        let kind = item.file_type()?;
        let child_archive = archive_path.join(item.file_name());
        if kind.is_dir() {
            add_directory_tree(zip, &item.path(), &child_archive, options)?;
        } else if kind.is_file() {
            add_zip_file(zip, &item.path(), &child_archive, options)?;
        }
    }
    Ok(())
}

fn add_zip_file<W: Write + Seek>(
    zip: &mut zip::ZipWriter<W>,
    source: &Path,
    archive_path: &Path,
    options: zip::write::SimpleFileOptions,
) -> io::Result<()> {
    zip.start_file(archive_path.to_string_lossy(), options)
        .map_err(io::Error::other)?;
    io::copy(&mut File::open(source)?, zip)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn creating_zip_preserves_neighbours_and_occupied_output() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("notes.txt");
        fs::write(&source, b"notes").unwrap();
        let output = temp.path().join("bundle.zip");
        let neighbour = temp.path().join("bundle.zip.part");
        fs::write(&neighbour, b"unrelated draft").unwrap();
        create_zip(std::slice::from_ref(&source), &output).unwrap();
        assert_eq!(fs::read(&neighbour).unwrap(), b"unrelated draft");
        let original = fs::read(&output).unwrap();
        fs::write(&source, b"changed").unwrap();
        assert!(create_zip(&[source], &output).is_err());
        assert_eq!(fs::read(&output).unwrap(), original);
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 3);
    }

    /// Archive paths reject traversal and resolve root and nested locations, listings synthesise parents, and zips round-trip.
    #[test]
    fn archive_paths_listings_and_round_trip() {
        // --- phase: traversal_is_rejected ---
        {
            assert!(safe_relative(Path::new("../secret")).is_err());
            assert!(safe_relative(Path::new("/etc/passwd")).is_err());
        }

        // --- phase: zip_round_trip ---
        {
            let temp = tempfile::tempdir().unwrap();
            fs::write(temp.path().join("hello.txt"), b"hello").unwrap();
            let archive = temp.path().join("bundle.zip");
            create_zip(&[temp.path().join("hello.txt")], &archive).unwrap();
            assert_eq!(list(&archive).unwrap()[0].path, Path::new("hello.txt"));
            let out = temp.path().join("out");
            extract(&archive, &out).unwrap();
            assert_eq!(fs::read(out.join("hello.txt")).unwrap(), b"hello");
        }

        // --- phase: archive_paths_resolve_root_and_nested_locations ---
        {
            let temp = tempfile::tempdir().unwrap();
            let archive = temp.path().join("bundle.zip");
            fs::write(&archive, b"").unwrap();
            assert_eq!(location(&archive), Some((archive.clone(), PathBuf::new())));
            assert_eq!(
                location(&archive.join("Photos/Trips")),
                Some((archive, PathBuf::from("Photos/Trips")))
            );
            assert_eq!(location(&temp.path().join("ordinary")), None);
        }

        // --- phase: archive_directory_listing_synthesizes_missing_parent_rows ---
        {
            let temp = tempfile::tempdir().unwrap();
            let source = temp.path().join("source");
            fs::create_dir(&source).unwrap();
            fs::create_dir(source.join("Photos")).unwrap();
            fs::write(source.join("Photos/one.jpg"), b"one").unwrap();
            fs::write(source.join("root.txt"), b"root").unwrap();
            let archive = temp.path().join("bundle.zip");
            create_zip(&[source.join("Photos"), source.join("root.txt")], &archive).unwrap();

            let root = list_directory(&archive, Path::new("")).unwrap();
            assert_eq!(root.len(), 2);
            assert!(
                root.iter()
                    .any(|entry| entry.path == Path::new("Photos") && entry.is_dir)
            );
            assert!(
                root.iter()
                    .any(|entry| entry.path == Path::new("root.txt") && !entry.is_dir)
            );
            let photos = list_directory(&archive, Path::new("Photos")).unwrap();
            assert_eq!(photos.len(), 1);
            assert_eq!(photos[0].path, Path::new("Photos/one.jpg"));
        }
    }

    #[test]
    #[cfg(unix)]
    fn folders_are_recursive_and_symlinks_are_not_followed() {
        let temp = tempfile::tempdir().unwrap();
        let folder = temp.path().join("Folder");
        fs::create_dir(&folder).unwrap();
        fs::write(folder.join("inside.txt"), b"inside").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("/etc/passwd", folder.join("unsafe-link")).unwrap();
        let archive = temp.path().join("folder.zip");
        create_zip(&[folder], &archive).unwrap();
        let names: Vec<_> = list(&archive)
            .unwrap()
            .into_iter()
            .map(|entry| entry.path)
            .collect();
        assert!(names.contains(&PathBuf::from("Folder/inside.txt")));
        assert!(!names.contains(&PathBuf::from("Folder/unsafe-link")));
    }

    /// Extraction never escapes, merges or overwrites, leaves no partial destination, and classifies zip links before materialising.
    #[test]
    fn extraction_is_confined_atomic_and_link_aware() {
        // --- phase: malicious_zip_never_writes_outside_destination ---
        {
            let mut bytes = Cursor::new(Vec::new());
            {
                let mut writer = zip::ZipWriter::new(&mut bytes);
                writer
                    .start_file("../escape", zip::write::SimpleFileOptions::default())
                    .unwrap();
                writer.write_all(b"bad").unwrap();
                writer.finish().unwrap();
            }
            bytes.set_position(0);
            let temp = tempfile::tempdir().unwrap();
            assert!(extract_zip(bytes, temp.path()).is_err());
            assert!(!temp.path().parent().unwrap().join("escape").exists());
        }

        // --- phase: extraction_never_merges_with_or_overwrites_an_existing_folder ---
        {
            let temp = tempfile::tempdir().unwrap();
            fs::write(temp.path().join("hello.txt"), b"new").unwrap();
            let archive = temp.path().join("bundle.zip");
            create_zip(&[temp.path().join("hello.txt")], &archive).unwrap();
            let out = temp.path().join("bundle");
            fs::create_dir(&out).unwrap();
            fs::write(out.join("hello.txt"), b"keep").unwrap();

            let error = extract(&archive, &out).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
            assert_eq!(fs::read(out.join("hello.txt")).unwrap(), b"keep");
        }

        // --- phase: failed_extraction_leaves_no_partial_destination ---
        {
            let mut bytes = Cursor::new(Vec::new());
            {
                let mut writer = zip::ZipWriter::new(&mut bytes);
                writer
                    .start_file("good.txt", zip::write::SimpleFileOptions::default())
                    .unwrap();
                writer.write_all(b"good").unwrap();
                writer
                    .start_file("../escape", zip::write::SimpleFileOptions::default())
                    .unwrap();
                writer.write_all(b"bad").unwrap();
                writer.finish().unwrap();
            }
            let temp = tempfile::tempdir().unwrap();
            let archive = temp.path().join("bad.zip");
            fs::write(&archive, bytes.into_inner()).unwrap();
            let out = temp.path().join("bad");

            assert!(extract(&archive, &out).is_err());
            assert!(!out.exists());
            assert!(fs::read_dir(temp.path()).unwrap().all(|entry| {
                !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .contains("extracting")
            }));
        }

        // --- phase: zip_unix_modes_classify_links_before_materialization ---
        {
            assert!(zip_mode_is_symlink(Some(0o120_777)));
            assert!(!zip_mode_is_symlink(Some(0o100_644)));
            assert!(!zip_mode_is_symlink(Some(0o040_755)));
            assert!(!zip_mode_is_symlink(None));
        }
    }
}
