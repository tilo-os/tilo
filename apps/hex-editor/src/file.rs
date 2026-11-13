use ui::application::MAX_OPEN_FILE_BYTES;

/// The file's bytes, refusing one larger than a host may hand the app at launch:
/// the buffer is a record every collaborator holds whole.
///
/// A file whose size already says it is too large is refused without a byte read.
pub(crate) fn read(path: &std::path::Path) -> Result<Vec<u8>, String> {
    let (dir, name) = tilo_files::file_at(path).map_err(|error| error.to_string())?;
    let limit = u64::try_from(MAX_OPEN_FILE_BYTES).unwrap_or(u64::MAX);
    let too_large = || format!("it is larger than {}", ui::format::memory(limit));
    if tilo_files::blocking::metadata(&dir, &name).is_ok_and(|meta| meta.len > limit) {
        return Err(too_large());
    }
    tilo_files::blocking::read_bounded(&dir, &name, MAX_OPEN_FILE_BYTES)
        .map_err(|error| error.to_string())?
        .ok_or_else(too_large)
}

pub(crate) fn write(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    tilo_files::file_at(path)
        .and_then(|(dir, name)| tilo_files::blocking::save_document(&dir, &name, bytes))
        .map_err(|error| error.to_string())
}
