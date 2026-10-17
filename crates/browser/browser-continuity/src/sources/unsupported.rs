//! Unsupported capture backends never read source files.

use crate::imports::{ImportBatch, ImportBundle};
use std::fs::File;
use std::io;

/// A source grant cannot be retained without a supported capture backend.
pub struct GrantedProfile {
    _private: (),
}

impl GrantedProfile {
    /// Retain an explicitly granted source directory.
    /// # Errors
    /// Always returns `Unsupported`, without inspecting the directory.
    pub fn from_directory(_file: File) -> io::Result<Self> {
        Err(unavailable())
    }
}

/// Read an explicitly reviewed source.
/// # Errors
/// Always returns `Unsupported`, without reading source files.
pub fn parse_chromium_bookmarks(_profile: &GrantedProfile) -> io::Result<ImportBatch> {
    Err(unavailable())
}

/// Read an explicitly reviewed source.
/// # Errors
/// Always returns `Unsupported`, without reading source files.
pub fn parse_chromium_bookmarks_bundle(_profile: &GrantedProfile) -> io::Result<ImportBundle> {
    Err(unavailable())
}

/// Read an explicitly reviewed source.
/// # Errors
/// Always returns `Unsupported`, without reading source files.
pub fn parse_chromium_history(_profile: &GrantedProfile) -> io::Result<ImportBatch> {
    Err(unavailable())
}

/// Read an explicitly reviewed source.
/// # Errors
/// Always returns `Unsupported`, without reading source files.
pub fn parse_chromium_history_bundle(_profile: &GrantedProfile) -> io::Result<ImportBundle> {
    Err(unavailable())
}

/// Read an explicitly reviewed source.
/// # Errors
/// Always returns `Unsupported`, without reading source files.
pub fn parse_firefox_places(_profile: &GrantedProfile) -> io::Result<ImportBatch> {
    Err(unavailable())
}

/// Read an explicitly reviewed source.
/// # Errors
/// Always returns `Unsupported`, without reading source files.
pub fn parse_firefox_places_bundle(_profile: &GrantedProfile) -> io::Result<ImportBundle> {
    Err(unavailable())
}

/// Read only the selected Firefox data domains.
/// # Errors
/// Always returns `Unsupported`, without reading source files.
pub fn parse_firefox_places_selected_bundle(
    _profile: &GrantedProfile,
    _bookmarks: bool,
    _history: bool,
) -> io::Result<ImportBundle> {
    Err(unavailable())
}

fn unavailable() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "Browser source capture is unavailable on this platform",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_backend_never_captures_sources() {
        assert!(!super::super::supported());
        let profile = GrantedProfile { _private: () };
        assert_eq!(
            parse_chromium_bookmarks(&profile).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
        assert_eq!(
            parse_chromium_history_bundle(&profile).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
        assert_eq!(
            parse_firefox_places_selected_bundle(&profile, true, true)
                .unwrap_err()
                .kind(),
            io::ErrorKind::Unsupported
        );
    }
}
