//! Files: a person's file, or a program's, as what the device's content store holds.
//!
//! A `File` value is a reference and never the bytes ([`crate::value::FileValue`]): its name,
//! its media type, its size and the hash of its content, which a picture reads as an address
//! (`tilo-blob://<hash>`, `ui.Image`). The bytes go to the content store once, when the file
//! is taken in, and the record keeps the address: an action's argument is the address, so
//! history, replay and undo carry the file without carrying a picture.
//!
//! Four things need the device, and only these: taking a chosen file in ([`BlobHost::choose`]),
//! reading a file's content as text (`.text()`, bounded by [`MAX_TEXT`]) or as the files an
//! archive holds (`.files()`, bounded by [`MAX_ARCHIVE_FILES`]), and putting what an action made
//! where the person wants it ([`BlobHost::save`]). A host that holds no content store answers
//! that it holds no files; the types and the records work all the same.
//!
//! Reading a file is a read, not an effect: its content is what its address names and never
//! changes, so an invocation that reads one is computed again from the same bytes. A device
//! that does not hold the content answers a value saying so.

use std::rc::Rc;

use crate::abi::Imp;
use crate::core::{Args, Core, HResult, ret_i64, trap};
use crate::engine::Wasm;
use crate::value::Value;

/// The scheme of the address a file's content reads at: `tilo-blob://<hash>`.
pub const SCHEME: &str = "tilo-blob://";

/// The most a file a person chooses may be, as the size a `File` value reads it. A picture a note takes in is well under it, and a
/// record's content is not what the store is for: what is larger is refused as a value, with
/// the size it had.
pub const MAX_CHOSEN: usize = 1024 * 1024;

/// [`MAX_CHOSEN`] as a file's size reads it: what a chosen file's `size` is compared with
/// (`i64::try_from` of a constant this shape never fails).
pub const MAX_CHOSEN_BYTES: i64 = 1024 * 1024;

/// The most of a file an action reads as text (`.text()`): an importer reads a document, never
/// a disk image. A file larger than it answers an error, as one that is not text does.
pub const MAX_TEXT: usize = 256 * 1024;

/// [`MAX_TEXT`] as a file's size reads it.
pub const MAX_TEXT_BYTES: i64 = 256 * 1024;

/// The most files one archive an action reads (`.files()`) may hold.
pub const MAX_ARCHIVE_FILES: usize = 256;

/// The most bytes one file in an archive may be, and all of them together: a person chooses an
/// archive of documents, never a disk image, and what it holds is read into memory.
pub const MAX_ARCHIVE_PART: u64 = 1024 * 1024;

/// [`MAX_ARCHIVE_PART`] for every file of one archive together.
pub const MAX_ARCHIVE_BYTES: u64 = 16 * 1024 * 1024;

/// The bounds one archive is read under: its files, each of them, and all of them together.
const ARCHIVE: bounded_zip::ZipLimits = bounded_zip::ZipLimits {
    max_parts: MAX_ARCHIVE_FILES,
    max_part_bytes: MAX_ARCHIVE_PART,
    max_total_bytes: MAX_ARCHIVE_BYTES,
};

/// A file's content as the store holds it: its name, the media type it was taken as, how many
/// bytes it is, and the hash of its content.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct FileValue {
    /// What the person's file is called, extension included (`Q3 plan.md`).
    pub name: String,
    /// Its media type (`image/png`), which is what a view tells the window to read it as.
    pub media: String,
    /// How many bytes its content is.
    pub size: i64,
    /// The hash of its content in the device's content store.
    pub hash: String,
}

impl FileValue {
    /// A file of `bytes`, as the content store holds them under `hash`.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        media: impl Into<String>,
        size: i64,
        hash: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            media: media.into(),
            size,
            hash: hash.into(),
        }
    }

    /// Where its bytes are: the address a picture takes (`![](tilo-blob://…)`).
    #[must_use]
    pub fn url(&self) -> String {
        format!("{SCHEME}{}", self.hash)
    }
}

/// What a person is asked for when they choose a file: what it is for (the chooser's title, in
/// the person's words), a name to start from, and the media types it may be.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileRequest {
    /// The chooser's title: what the file is for, in the app's own words.
    pub title: String,
    /// A name to start the chooser with, when the app has one in mind.
    pub name: String,
    /// The media types it may be (`image/png`, or `image/*` for any of a kind); any file when
    /// it is empty.
    pub accept: Vec<String>,
}

/// What a person is asked for when they save a file: what it is called and what it holds, so
/// the chooser can offer it a name and a media type.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SaveRequest {
    /// The name to offer the file: `Notes.md`.
    pub name: String,
    /// What the content is, when the app knows: `text/markdown`.
    pub media: String,
}

/// The device's files, as its platform answers them: the content store a chosen file's bytes go
/// to, the content it holds, and the chooser a person picks with.
///
/// The runtime holds one (`Runtime::set_blobs`). A host that has none answers as one that holds
/// no files: a chooser offers nothing and a file's content is not here.
pub trait BlobHost {
    /// The content at `hash`, when this device holds it.
    fn read(&self, hash: &str) -> Option<Vec<u8>>;

    /// Ask the person for a file, taking in what they pick. The bytes are the host's to keep
    /// (its content store holds them at the hash the value names); a file larger than
    /// [`MAX_CHOSEN`], or of a media type `request` does not accept, is refused with the reason.
    ///
    /// # Errors
    ///
    /// Why the file could not be taken in (its size, its type, the store's failure). Nothing
    /// was stored.
    fn choose(&self, request: &FileRequest) -> Result<Option<FileValue>, String>;

    /// Put `content` in a file the person names, offering `request`'s name.
    ///
    /// # Errors
    ///
    /// Why it could not be written (no chooser, a refused path, a failed write).
    fn save(&self, request: &SaveRequest, content: &[u8]) -> Result<(), String>;

    /// Keep `bytes` as a file called `name` of media type `media`, answering the file whose
    /// content they now are: what a program that holds bytes makes of them, such as the files
    /// an archive a person chose holds (`.files()`). The content is kept once whatever its name:
    /// the same bytes answer the same address.
    ///
    /// # Errors
    ///
    /// Why the bytes were not kept (no content store, a failed write).
    fn keep(&self, name: &str, media: &str, bytes: &[u8]) -> Result<FileValue, String> {
        let _ = (name, media, bytes);
        Err("this host keeps no files".to_owned())
    }
}

/// The media type of a file called `name`, as far as its extension tells: what a chooser starts
/// from, and what a saved file is written as. `application/octet-stream` when it says nothing.
#[must_use]
pub fn media_of(name: &str) -> String {
    let extension = name
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase())
        .unwrap_or_default();
    let media = match extension.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        "md" | "markdown" => "text/markdown",
        "txt" => "text/plain",
        "html" => "text/html",
        "csv" => "text/csv",
        "json" => "application/json",
        "zip" => "application/zip",
        _ => "application/octet-stream",
    };
    media.to_owned()
}

/// Whether `media` is one of the types `accept` names, an entry of which may be `image/*`; any
/// media type when `accept` is empty.
#[must_use]
pub fn accepts(accept: &[String], media: &str) -> bool {
    accept.is_empty()
        || accept.iter().any(|entry| {
            let entry = entry.trim();
            entry == "*/*"
                || entry.eq_ignore_ascii_case(media)
                || entry.strip_suffix("/*").is_some_and(|kind| {
                    media
                        .split_once('/')
                        .is_some_and(|(head, _)| head.eq_ignore_ascii_case(kind))
                })
        })
}

impl Value {
    /// The file this value is, for a host that builds one.
    #[must_use]
    pub fn file(file: FileValue) -> Self {
        Self::File(Rc::new(file))
    }
}

impl Core {
    /// The parts of a file, its size and its content read as text.
    pub(crate) fn file_import(&self, imp: Imp, a: &Args<'_>) -> HResult<Option<Wasm>> {
        match imp {
            Imp::FilePart => {
                let file = self.file_arg(a.h(0)?)?;
                let part = match a.h(1)? {
                    0 => file.name.clone(),
                    1 => file.media.clone(),
                    2 => file.url(),
                    other => return trap(format!("a file has no part {other}")),
                };
                self.ret_h(Value::text(part))
            }
            Imp::FileSize => {
                let file = self.file_arg(a.h(0)?)?;
                ret_i64(file.size)
            }
            Imp::FileText => {
                let file = self.file_arg(a.h(0)?)?;
                let answer = match self.blobs.borrow().as_ref() {
                    Some(blobs) => read_text(blobs.as_ref(), &file),
                    None => Err("this host holds no content store".to_owned()),
                };
                self.result(answer.map(Value::text))
            }
            Imp::FileFiles => {
                let file = self.file_arg(a.h(0)?)?;
                let answer = match self.blobs.borrow().as_ref() {
                    Some(blobs) => files_of(blobs.as_ref(), &file),
                    None => Err("this host holds no content store".to_owned()),
                };
                self.result(answer.map(Value::List))
            }
            _ => trap("not a file import"),
        }
    }

    /// The `File` value behind the handle `h`.
    pub(crate) fn file_arg(&self, h: i32) -> HResult<Rc<FileValue>> {
        match self.val(h)? {
            Value::File(file) => Ok(file),
            other => trap(format!("expected a file, got {}", other.kind_name())),
        }
    }
}

/// The file's content as text, or why it is not: nothing this device holds, more than
/// [`MAX_TEXT`], or bytes that are not UTF-8.
fn read_text(blobs: &dyn BlobHost, file: &FileValue) -> Result<String, String> {
    if file.size > MAX_TEXT_BYTES {
        return Err(format!(
            "`{}` is {} bytes: more than the {MAX_TEXT} an action reads as text",
            file.name, file.size
        ));
    }
    let Some(bytes) = blobs.read(&file.hash) else {
        return Err(format!(
            "the content of `{}` is not on this device",
            file.name
        ));
    };
    String::from_utf8(bytes).map_err(|_| format!("`{}` is not text", file.name))
}

/// The files an archive holds, each kept in the device's content store under the name it has
/// inside it, which is what an importer of a package (an export of notes, a zip of documents)
/// reads names and nesting from. A file is read within [`ARCHIVE`]'s bounds: an archive is a
/// person's, and one that holds more than an importer reads is refused with why.
fn files_of(blobs: &dyn BlobHost, file: &FileValue) -> Result<Vec<Value>, String> {
    if file.size > MAX_CHOSEN_BYTES {
        return Err(format!(
            "`{}` is {} bytes: more than the {MAX_CHOSEN} a file may be",
            file.name, file.size
        ));
    }
    let Some(bytes) = blobs.read(&file.hash) else {
        return Err(format!(
            "the content of `{}` is not on this device",
            file.name
        ));
    };
    let parts = bounded_zip::read_zip_parts(&bytes, &ARCHIVE)
        .map_err(|why| format!("`{}` is not an archive that reads: {why}", file.name))?;
    let mut out = Vec::with_capacity(parts.len());
    for (name, content) in parts {
        let media = media_of(&name);
        out.push(Value::File(Rc::new(blobs.keep(&name, &media, &content)?)));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_files_address_is_the_content_it_names() {
        let file = FileValue::new("plan.png", "image/png", 12, "bafy123");
        assert_eq!(file.url(), "tilo-blob://bafy123");
        assert_eq!(media_of("plan.png"), "image/png");
        assert_eq!(media_of("notes.MD"), "text/markdown");
        assert_eq!(media_of("thing"), "application/octet-stream");
    }

    #[test]
    fn a_chooser_takes_the_types_it_names() {
        let images = vec!["image/*".to_owned()];
        assert!(accepts(&images, "image/png"));
        assert!(!accepts(&images, "text/plain"));
        let one = vec!["image/webp".to_owned()];
        assert!(accepts(&one, "image/webp"));
        assert!(!accepts(&one, "image/png"));
        assert!(accepts(&[], "anything/at-all"), "no type asked, any file");
    }
}
