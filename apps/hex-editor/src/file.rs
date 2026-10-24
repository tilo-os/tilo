//! Reading and writing the file a buffer was opened from, on this device.

use ui::application::MAX_OPEN_FILE_BYTES;

/// The file's bytes, refusing one larger than a file a host may hand the
/// app at launch: the buffer is a record every collaborator holds whole, so
/// a multi-gigabyte file must not be read into memory to find that out. At
/// most one byte past the limit is read.
pub(crate) fn read(path: &std::path::Path) -> Result<Vec<u8>, String> {
    let (dir, name) = tilo_files::file_at(path).map_err(|error| error.to_string())?;
    tilo_files::blocking::read_bounded(&dir, &name, MAX_OPEN_FILE_BYTES)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| {
            format!(
                "it is larger than {}",
                ui::format::memory(u64::try_from(MAX_OPEN_FILE_BYTES).unwrap_or(u64::MAX))
            )
        })
}

pub(crate) fn write(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    tilo_files::file_at(path)
        .and_then(|(dir, name)| tilo_files::blocking::save_document(&dir, &name, bytes))
        .map_err(|error| error.to_string())
}
