//! Reading and writing the file a buffer was opened from, on this device.

pub(crate) fn read(path: &std::path::Path) -> Result<Vec<u8>, String> {
    tilo_files::file_at(path)
        .and_then(|(dir, name)| tilo_files::blocking::read(&dir, &name))
        .map_err(|error| error.to_string())
}

pub(crate) fn write(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    tilo_files::file_at(path)
        .and_then(|(dir, name)| tilo_files::blocking::save_document(&dir, &name, bytes))
        .map_err(|error| error.to_string())
}
