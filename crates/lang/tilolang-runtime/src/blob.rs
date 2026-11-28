//! Files: a person's file, or a program's, as what the device's content store holds.
//!
//! A `File` value is a reference and never the bytes ([`crate::value::FileValue`]): its name,
//! its media type, its size and the hash of its content, which a picture reads as an address
//! (`tilo-blob://<hash>`, `ui.Image`). The bytes go to the content store once, when the file
//! is taken in, and the record keeps the address: an action's argument is the address, so
//! history, replay and undo carry the file without carrying a picture.
//!
//! What needs the device, and only this: taking the files a person chose in
//! ([`BlobHost::choose`], one, several, or a folder's), reading a file's content as text
//! (`.text()`, bounded by [`MAX_TEXT`]) or as the files an archive holds (`.files()`, bounded by
//! [`MAX_ARCHIVE_FILES`]), a preview of it ([`BlobHost::thumbnail`]), and putting what an action
//! made where the person wants it ([`BlobHost::save`]). A host that holds no content store
//! answers that it holds no files; the types and the records work all the same.
//!
//! An action makes a file of what it holds (`File.of_text`, `File.of_bytes`, `File.at`,
//! `File.zip`): its bytes are kept in the content store, under a name the app chooses, and the
//! `File` value answers that name, media type, size and address. What is made is bounded like
//! what is read: a file the action holds by [`MAX_CHOSEN`], an archive by [`MAX_ARCHIVE_BYTES`],
//! under the names [`bounded_zip::is_safe_name`] admits.
//!
//! Reading a file is a read, not an effect: its content is what its address names and never
//! changes, so an invocation that reads one is computed again from the same bytes. A device
//! that does not hold the content answers a value saying so.

use std::rc::Rc;

use crate::value::Value;

/// The scheme of the address a file's content reads at: `tilo-blob://<hash>`.
pub const SCHEME: &str = tilo_content_types::BLOB_SCHEME;

/// The most a file a person chooses may be, as the size a `File` value reads it
/// (`tilo_content_types::MAX_FILE_BYTES`): what is larger is refused as a value, with the size
/// it had. It is also the most a file an action holds may be made into (`File.of_text`,
/// `File.of_bytes`, `File.at`): a file made here is one a person could choose.
pub const MAX_CHOSEN: usize = tilo_content_types::MAX_FILE_BYTES;

/// [`MAX_CHOSEN`] as a file's size reads it: what a chosen file's `size` is compared with.
#[expect(
    clippy::cast_possible_wrap,
    reason = "a file's limit is a megabyte, far inside i64"
)]
pub const MAX_CHOSEN_BYTES: i64 = MAX_CHOSEN as i64;

/// The most of a file an action reads as text (`.text()`): an importer reads a document, never
/// a disk image. A file larger than it answers an error, as one that is not text does.
pub const MAX_TEXT: usize = 256 * 1024;

/// [`MAX_TEXT`] as a file's size reads it.
pub const MAX_TEXT_BYTES: i64 = 256 * 1024;

/// The most files one archive an action reads (`.files()`) or writes (`File.zip`) may hold: the
/// notes of a notebook and the pictures they show, which is what an export writes whole.
pub const MAX_ARCHIVE_FILES: usize = 4096;

/// The most bytes one file in an archive may be, and all of them together: a person chooses an
/// archive of documents, never a disk image, and what it holds is read into memory.
pub const MAX_ARCHIVE_PART: u64 = 1024 * 1024;

/// [`MAX_ARCHIVE_PART`] for every file of one archive together.
pub const MAX_ARCHIVE_BYTES: u64 = 16 * 1024 * 1024;

/// A file's content as the store holds it: its name, the media type it was taken as, how many
/// bytes it is, the hash of its content, and where it stands in what the person chose.
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
    /// Where it stands in what the person chose: a path inside the folder they chose, or the
    /// file's own name when they chose it alone. Never a host path, which a sandboxed app has
    /// no way to use.
    #[serde(default)]
    pub path: String,
    /// Whether the host opened it for writing as well as reading: a document app's Open asks,
    /// so the file saves back where it was read. False for a file a host handed over as content
    /// and for one it could only read.
    #[serde(default)]
    pub editable: bool,
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
            path: String::new(),
            editable: false,
        }
    }

    /// [`Self::new`] with where the file stands in what the person chose.
    #[must_use]
    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = path.into();
        self
    }

    /// [`Self::new`] with whether the host opened the file for writing too.
    #[must_use]
    pub fn with_editable(mut self, editable: bool) -> Self {
        self.editable = editable;
        self
    }

    /// Where its bytes are: the address a picture takes (`![](tilo-blob://…)`).
    #[must_use]
    pub fn url(&self) -> String {
        format!("{SCHEME}{}", self.hash)
    }
}

/// The size a file states when the caller named none: `.size()` answers the content's own
/// length instead.
pub const UNSTATED_SIZE: i64 = -1;

/// The `File` a caller names as JSON: the content it already put into the workspace, and what
/// the file is.
///
/// The keys are the ones a read answers ([`crate::project`]) and a caller writes:
///
/// - `content`: the reference a put answered, `tilo.content:<reference>`;
/// - `hash`: the reference itself, as [`FileValue`] is stored;
/// - `url`: the address a picture takes, `tilo-blob://<reference>`.
///
/// One of the three names the bytes; a `File` that names none has none. `name` is what the
/// file is called, `media` its media type (read from the name when left out), `path` where it
/// stands in what the person chose, and `size` how many bytes it is — [`UNSTATED_SIZE`] when the
/// caller names none, which reads the content when an action asks.
///
/// Naming a reference grants nothing: the bytes stay where they are, and an action that reads
/// what this device does not hold is told so. A caller names content its workspace holds, as
/// the put that answered the reference kept it there.
///
/// # Errors
///
/// Why the object is no file: no content, no name, or a `content` that is no reference.
pub fn file_from_json(json: &serde_json::Value) -> Result<FileValue, String> {
    use serde_json::Value as Json;
    let Json::Object(fields) = json else {
        return Err(format!("a file is an object, and this is {json}"));
    };
    let text = |key: &str| fields.get(key).and_then(Json::as_str);
    let hash = if let Some(content) = text("content") {
        tilo_actions::named_reference(content)
            .ok_or_else(|| {
                format!(
                    "`{content}` names no content: give the `tilo.content:<reference>` a put \
                     answered"
                )
            })?
            .to_owned()
    } else if let Some(hash) = text("hash") {
        hash.to_owned()
    } else if let Some(url) = text("url") {
        url.strip_prefix(SCHEME)
            .ok_or_else(|| {
                format!("`{url}` is no file's content: they read `{SCHEME}<reference>`")
            })?
            .to_owned()
    } else {
        return Err(
            "a file names its content: the `tilo.content:<reference>` a put answered".to_owned(),
        );
    };
    let name = text("name")
        .filter(|name| !name.trim().is_empty())
        .ok_or_else(|| "a file names what it is called (`name`)".to_owned())?;
    let media = text("media").map_or_else(|| media_of(name), str::to_owned);
    let path = text("path").unwrap_or_default();
    // A size named as [`UNSTATED_SIZE`] states none, as one left out does: that is how a file
    // stating no size is written, so what a record keeps of one reads back as the file it was.
    let size = match fields.get("size") {
        None => UNSTATED_SIZE,
        Some(size) => size
            .as_i64()
            .filter(|size| *size >= 0 || *size == UNSTATED_SIZE)
            .ok_or_else(|| {
                format!("a file's `size` is how many bytes it is, and {size} is none")
            })?,
    };
    let editable = fields
        .get("editable")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    Ok(FileValue::new(name, media, size, hash)
        .with_path(path)
        .with_editable(editable))
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
    /// Whether the person may choose more than one file: what an action whose parameter is a
    /// `List<File>` fills.
    pub multiple: bool,
    /// Whether the person chooses a folder: its files come as a list, each with its path inside
    /// it.
    pub folder: bool,
    /// Whether the file is opened for writing too: what a word processor's Open asks, so the
    /// document's words save back where they were read rather than asking where to put them.
    /// Only a file, never a folder or a list of them.
    pub edit: bool,
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

/// What a save came to, as the app that asked reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Saved {
    /// Written, at the place as the person reads it; empty when this host keeps the file
    /// without a path of its own (a page's download, a destination the host keeps).
    At(String),
    /// The person closed the chooser without saving.
    Cancelled,
}

/// The device's files, as its platform answers them: the content store a chosen file's bytes go
/// to, the content it holds, and the chooser a person picks with.
///
/// The runtime holds one (`Runtime::set_blobs`). A host that has none answers as one that holds
/// no files: a chooser offers nothing and a file's content is not here.
pub trait BlobHost {
    /// The content at `hash`, when this device holds it.
    fn read(&self, hash: &str) -> Option<Vec<u8>>;

    /// Ask the person for files, taking in what they pick: one, several, or the files of a
    /// folder, as `request` asks. The bytes are the host's to keep (its content store holds them
    /// at the hash each value names); a file larger than [`MAX_CHOSEN`], or of a media type
    /// `request` does not accept, is refused with the reason. An empty list is a chooser the
    /// person dismissed.
    ///
    /// # Errors
    ///
    /// Why the files could not be taken in (a size, a type, the store's failure). Nothing was
    /// stored.
    fn choose(&self, request: &FileRequest) -> Result<Vec<FileValue>, String>;

    /// Ask the person for a folder and answer the path that reaches it again: what
    /// `file.choose_folder(a)` fills when `a` takes the folder itself rather than the files it
    /// holds. The path is the reference the person's choice answers (what `File.list`,
    /// `File.read` and `File.watch` take), never a host path; `None` is a chooser the person
    /// dismissed.
    ///
    /// # Errors
    ///
    /// Why there is no folder: no chooser answers here, or it refused.
    fn choose_folder(&self, request: &FileRequest) -> Result<Option<String>, String> {
        let _ = request;
        Err("this host chooses no folders".to_owned())
    }

    /// Put `content` in a file the person names, offering `request`'s name, and answer what it
    /// came to: written where, or the person closing the chooser. A save the host refuses is an
    /// error, carrying why.
    ///
    /// # Errors
    ///
    /// Why it was not written (no chooser, a refused path, a failed write).
    fn save(&self, request: &SaveRequest, content: &[u8]) -> Result<Saved, String>;

    /// Write `content` over the file the person chose at `reference` — what [`FileValue::path`]
    /// answers for a file a chooser answered — as saving a document back to the file it was
    /// opened from. The file is replaced whole, and the person is asked nothing more: their
    /// choice of the file is what allows it.
    ///
    /// # Errors
    ///
    /// Why it could not be written: the file is not one the person chose for this app to change,
    /// it is gone, or the write failed.
    fn write_back(&self, file: &FileValue, reference: &str, content: &[u8]) -> Result<(), String> {
        let _ = (file, reference, content);
        Err("this host writes no file back".to_owned())
    }

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

    /// A preview of `file` no larger than `size` pixels a side, kept as a file of its own: what
    /// `File.thumbnail` answers. `None` when the file has no preview (it is no image), and the
    /// error when this host keeps no preview cache.
    ///
    /// # Errors
    ///
    /// Why there is no preview.
    fn thumbnail(&self, file: &FileValue, size: u32) -> Result<Option<FileValue>, String> {
        let _ = (file, size);
        Err("this host keeps no previews".to_owned())
    }
}

/// The media type of a file called `name`, as far as its extension tells: one reading of a name,
/// for the runtime and for the window that handed a dropped file over.
pub use tilo_content_types::media_of;

/// The media type of the file called `name` whose content is `bytes`: its name's, and its
/// content's where the name says nothing (`application/octet-stream`). A chosen document is
/// whatever its package is — an EPUB book, a deck — and an app that asks for one by its type
/// finds it by the type the file itself states.
#[must_use]
pub fn media_of_file(name: &str, bytes: &[u8]) -> String {
    let named = media_of(name);
    if named != "application/octet-stream" {
        return named;
    }
    tilo_content_types::opened_as(bytes).1.to_owned()
}

/// Whether the file called `name` of media type `media` is one of the files `accept` names: a
/// media type (`image/png`), any of a kind (`image/*`), any file (`*/*`), or the ending of its
/// name (`.epub`, `*.epub`), which an app names for a format no media type tells. Any file when
/// `accept` is empty.
#[must_use]
pub fn accepts(accept: &[String], media: &str, name: &str) -> bool {
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
                // A name's ending, as the app wrote it: `.epub`, or a pattern's `*.epub`.
                || {
                    let ending = entry.strip_prefix('*').unwrap_or(entry).trim();
                    !ending.is_empty() && name.to_lowercase().ends_with(&ending.to_lowercase())
                }
        })
}

impl Value {
    /// The file this value is, for a host that builds one.
    #[must_use]
    pub fn file(file: FileValue) -> Self {
        Self::File(Rc::new(file))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        fn files_are_addressed_by_content_and_chosen_by_type;

        phase a_files_address_is_the_content_it_names;
        {
            let file = FileValue::new("plan.png", "image/png", 12, "bafy123");
            assert_eq!(file.url(), "tilo-blob://bafy123");
            assert_eq!(media_of("plan.png"), "image/png");
            assert_eq!(media_of("notes.MD"), "text/markdown");
            assert_eq!(media_of("thing"), "application/octet-stream");
            assert_eq!(
                media_of("Letter.docx"),
                "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
            );
            assert_eq!(media_of("Letter.odt"), "application/vnd.oasis.opendocument.text");
            assert_eq!(media_of("Letter.rtf"), "application/rtf");
            assert_eq!(media_of("Page.htm"), "text/html");
            assert_eq!(media_of("Book.epub"), "application/epub+zip");
        }

        phase a_chooser_takes_the_types_it_names;
        {
            let images = vec!["image/*".to_owned()];
            assert!(accepts(&images, "image/png", "plan.png"));
            assert!(!accepts(&images, "text/plain", "plan.txt"));
            let one = vec!["image/webp".to_owned()];
            assert!(accepts(&one, "image/webp", "plan.webp"));
            assert!(!accepts(&one, "image/png", "plan.png"));
            assert!(
                accepts(&[], "anything/at-all", "thing"),
                "no type asked, any file"
            );
            // A format no media type tells is asked for by the ending of its name.
            let backup = vec![".tilopw".to_owned()];
            assert!(accepts(&backup, "application/octet-stream", "Backup.tilopw"));
            assert!(!accepts(&backup, "application/octet-stream", "notes.txt"));
            let deck = vec!["*.pptx".to_owned()];
            assert!(
                accepts(&deck, "application/octet-stream", "Pitch.pptx"),
                "a pattern's ending"
            );
        }

        phase a_documents_type_comes_from_its_bytes_where_its_name_says_nothing;
        {
            // An EPUB's own package states its type, which no extension table need repeat.
            let mut epub = Vec::new();
            epub.extend_from_slice(b"PK\x03\x04");
            for number in [20_u16, 0, 0, 0, 0] {
                epub.extend_from_slice(&number.to_le_bytes());
            }
            epub.extend_from_slice(&0_u32.to_le_bytes());
            epub.extend_from_slice(&20_u32.to_le_bytes());
            epub.extend_from_slice(&20_u32.to_le_bytes());
            epub.extend_from_slice(&8_u16.to_le_bytes());
            epub.extend_from_slice(&0_u16.to_le_bytes());
            epub.extend_from_slice(b"mimetype");
            epub.extend_from_slice(b"application/epub+zip");
            assert_eq!(media_of_file("The Tide Glass.epub", &epub), "application/epub+zip");
            assert_eq!(media_of_file("plan.png", b"not a picture"), "image/png");
            assert_eq!(media_of_file("thing", b"\x00\x01"), "application/octet-stream");
        }
    }

    tilo_test_env::scenario! {
        fn a_caller_names_a_file_by_the_content_it_put;

        phase a_content_reference_names_the_file_it_is;
        {
            let hash = "a".repeat(64);
            let given = serde_json::json!({
                "content": format!("tilo.content:{hash}"),
                "name": "Plan.md",
            });
            let file = file_from_json(&given).expect("a content reference names a file");
            assert_eq!(file.hash, hash, "the reference names the content");
            assert_eq!(file.name, "Plan.md");
            assert_eq!(file.media, "text/markdown", "read from the name it came in as");
            assert_eq!(
                file.size,
                UNSTATED_SIZE,
                "a caller that states no size leaves it unstated"
            );
        }

        phase a_file_that_states_no_size_reads_back_as_it_was_made;
        {
            // A file `File.at` makes states no size: a record keeps the encoding the runtime
            // writes, which says `UNSTATED_SIZE`, and reads it back as the file it was.
            let made = Value::File(Rc::new(FileValue::new(
                "page.png",
                "image/png",
                UNSTATED_SIZE,
                "c".repeat(64),
            )));
            let kept = crate::value::to_json(&made).expect("a file is kept");
            let read = crate::value::from_json(&kept).expect("a file stating no size reads back");
            assert_eq!(read, made, "the file a record kept is the one it was given");
        }

        phase what_a_read_answers_is_what_a_caller_can_name;
        {
            // The encoded form a read answers is the one a caller passes on: same file.
            let read = serde_json::json!({
                "file": {
                    "name": "fox.png",
                    "media": "image/png",
                    "size": 12,
                    "content": tilo_actions::content_resource(&"b".repeat(64)),
                    "url": format!("{SCHEME}{}", "b".repeat(64)),
                },
            });
            let file = crate::value::from_json(&read).expect("a read is a file");
            let Value::File(file) = file else {
                panic!("a read of a file is a file");
            };
            assert_eq!((file.name.as_str(), file.size), ("fox.png", 12));
            assert_eq!(file.hash, "b".repeat(64));
            // The address a picture takes names the same content.
            let as_url = serde_json::json!({"name": "fox.png", "url": file.url()});
            assert_eq!(file_from_json(&as_url).expect("an address names it").hash, file.hash);
            // Where it stands in what the person chose comes back with the file.
            let in_a_folder = serde_json::json!({
                "name": "fox.png",
                "path": "trip/fox.png",
                "hash": file.hash,
            });
            assert_eq!(
                file_from_json(&in_a_folder).expect("a path names it").path,
                "trip/fox.png"
            );
        }

        phase no_content_or_no_name_is_refused_with_why;
        {
            let unnamed = serde_json::json!({"name": "Plan.md"});
            assert!(
                file_from_json(&unnamed)
                    .unwrap_err()
                    .contains("names its content"),
                "a file names the content it is"
            );
            let nameless = serde_json::json!({"content": format!("tilo.content:{}", "a".repeat(64))});
            assert!(
                file_from_json(&nameless)
                    .unwrap_err()
                    .contains("what it is called"),
                "a file names itself"
            );
            let no_reference = serde_json::json!({"content": "not a hash", "name": "Plan.md"});
            assert!(
                file_from_json(&no_reference)
                    .unwrap_err()
                    .contains("names no content"),
                "words that name no content are refused"
            );
        }
    }
}
