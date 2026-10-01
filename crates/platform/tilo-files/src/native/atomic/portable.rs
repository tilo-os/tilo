//! Fallback implementation for hosts other than Unix and Windows.
//! Never compiled or reached on Windows; it has no native durability guarantee.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub(crate) fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    write_inner(path, bytes, None)
}

pub(crate) fn write_creating_parent(path: &Path, bytes: &[u8]) -> io::Result<()> {
    create_parent(path)?;
    write_inner(path, bytes, None)
}

pub(crate) fn write_creating_parent_with_mode(
    path: &Path,
    bytes: &[u8],
    mode: u32,
) -> io::Result<()> {
    create_parent(path)?;
    write_inner(path, bytes, Some(mode))
}

pub(crate) fn write_with_mode(path: &Path, bytes: &[u8], mode: u32) -> io::Result<()> {
    write_inner(path, bytes, Some(mode))
}

fn create_parent(path: &Path) -> io::Result<()> {
    if let Some(dir) = path.parent()
        && !dir.as_os_str().is_empty()
    {
        fs::create_dir_all(dir)?;
    }
    Ok(())
}

fn write_inner(path: &Path, bytes: &[u8], mode: Option<u32>) -> io::Result<()> {
    let (tmp, mut file) = create_temp(path, mode)?;
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file);
    if let Err(error) = written {
        let _ = fs::remove_file(&tmp);
        return Err(error);
    }
    if let Err(error) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(error);
    }
    let dir = path.parent().map_or_else(
        || Path::new("."),
        |parent| {
            if parent.as_os_str().is_empty() {
                Path::new(".")
            } else {
                parent
            }
        },
    );
    File::open(dir)?.sync_all()
}

fn temp_path(path: &Path, sequence: u64) -> PathBuf {
    let name = path.file_name().map_or_else(
        || "file".to_string(),
        |name| name.to_string_lossy().into_owned(),
    );
    let temp_name = format!(".{name}.{}.{sequence}.tmp", std::process::id());
    match path.parent() {
        Some(directory) if !directory.as_os_str().is_empty() => directory.join(temp_name),
        _ => PathBuf::from(temp_name),
    }
}

fn create_temp(path: &Path, mode: Option<u32>) -> io::Result<(PathBuf, File)> {
    for _ in 0..32 {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temp = temp_path(path, sequence);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        let _ = mode;
        match options.open(&temp) {
            Ok(file) => return Ok((temp, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a unique atomic-write temporary file",
    ))
}
