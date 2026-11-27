//! Files a person hands a window: dropped on it, or chosen in a chooser. The bytes are kept in
//! the device's content store, and the view hears of the file as the runtime's `File` — its
//! name, its media type, its size and the address its bytes are at — so an app whose process
//! reads content addresses, never paths, can open it.
//!
//! A file the store would not keep, one larger than a `File` may be, or one that cannot be read
//! where it was dropped leaves a reason for the window to say, never a silent nothing.

use std::path::Path;

use tilo_ui_base::component::File;

/// The most a file a person hands a window may be: what a `File` value reads
/// (`tilo_content_types::MAX_FILE_BYTES`).
pub const MAX_FILE_BYTES: usize = tilo_content_types::MAX_FILE_BYTES;

/// Why a file a person dropped or chose was not handed over: what the window says where they did
/// it, so a drop is never lost in silence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileRefusal {
    /// The file is larger than a `File` may be.
    TooLarge,
    /// This process holds no content store (the web answers its host for one).
    NoStore,
    /// The file could not be read from where it was dropped.
    Unreadable(String),
    /// The content store refused the bytes.
    Store(String),
}

impl FileRefusal {
    /// Why the file was not handed over, in the person's words.
    #[must_use]
    pub fn words(&self) -> String {
        use tilo_ui_base::{i18n::widget_text, i18n::widget_text_with};
        match self {
            Self::TooLarge => widget_text("file-too-large"),
            Self::NoStore => widget_text("file-no-store"),
            Self::Unreadable(reason) | Self::Store(reason) => widget_text_with(
                "file-not-kept",
                Some(&fluent::FluentArgs::from_iter([(
                    "reason",
                    reason.as_str(),
                )])),
            ),
        }
    }
}

/// Keep `bytes` as the file `name` in this process's content store, and answer what a view reads
/// of it: its name, its media type (from the name's extension), its size and the address of its
/// bytes.
///
/// # Errors
///
/// [`FileRefusal`]: it is larger than [`MAX_FILE_BYTES`], this process holds no content store, or
/// the store refused it.
pub fn keep_file(name: &str, bytes: &[u8]) -> Result<File, FileRefusal> {
    if bytes.len() > MAX_FILE_BYTES {
        return Err(FileRefusal::TooLarge);
    }
    let store = device_store().ok_or(FileRefusal::NoStore)?;
    let hash = store
        .put(bytes)
        .map_err(|error| FileRefusal::Store(error.to_string()))?;
    Ok(File {
        name: name.to_owned(),
        media: tilo_content_types::media_of(name),
        size: i64::try_from(bytes.len()).unwrap_or(i64::MAX),
        url: format!("{}{hash}", crate::pictures::BLOB_SCHEME),
    })
}

/// The file at `path`, kept and answered as [`keep_file`] does: a file dropped on a window. Its
/// bytes are read up to [`MAX_FILE_BYTES`]; what is larger is refused without being read whole.
///
/// # Errors
///
/// [`FileRefusal`]: the file cannot be read, or [`keep_file`]'s reason.
pub fn dropped_file(path: &Path) -> Result<File, FileRefusal> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| FileRefusal::Unreadable("it has no name".to_owned()))?
        .to_owned();
    let (dir, file) =
        tilo_files::file_at(path).map_err(|error| FileRefusal::Unreadable(error.to_string()))?;
    let bytes = tilo_files::blocking::read_bounded(&dir, &file, MAX_FILE_BYTES.saturating_add(1))
        .map_err(|error| match error {
        tilo_files::FsError::FileTooLarge => FileRefusal::TooLarge,
        other => FileRefusal::Unreadable(other.to_string()),
    })?;
    keep_file(&name, &bytes)
}

/// The store a file is kept in: the device content store the launcher handed this process, and
/// none in a build that keeps no such store (the web), where a host keeps the file instead.
#[cfg(all(feature = "native", feature = "content"))]
fn device_store() -> Option<crate::content::ContentStore> {
    crate::content::device()
}

#[cfg(not(all(feature = "native", feature = "content")))]
fn device_store() -> Option<crate::content::ContentStore> {
    None
}

#[cfg(test)]
mod tests {

    use super::*;

    /// A file is what the person's name says it is, and what a view can read of it without its
    /// bytes; this process holds no device store, so it answers no file here.
    #[test]
    fn a_file_larger_than_one_may_be_is_refused_before_it_is_read() {
        let _env = tilo_test_env::lock();
        assert_eq!(
            keep_file("plan.md", &vec![b'#'; MAX_FILE_BYTES.saturating_add(1)]),
            Err(FileRefusal::TooLarge),
            "a file past what a `File` reads is refused"
        );
        assert_eq!(
            keep_file("plan.md", b"# Plan\n"),
            Err(FileRefusal::NoStore),
            "no store, no address"
        );
        assert_eq!(
            tilo_content_types::media_of("plan.md"),
            "text/markdown",
            "a name says what the file is"
        );
    }

    /// A file dropped where it cannot be read says so, rather than failing without words.
    #[test]
    fn a_file_that_cannot_be_read_says_why() {
        let _env = tilo_test_env::lock();
        let directory = tilo_test_env::runtime_tempdir().expect("a place for the drop");
        let path = directory.path().join("gone.md");
        assert!(matches!(
            dropped_file(&path),
            Err(FileRefusal::Unreadable(_))
        ));
    }
}
