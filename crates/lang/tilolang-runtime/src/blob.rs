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

use crate::abi::Imp;
use crate::core::{Args, Core, HResult, ret_i64, trap};
use crate::engine::Wasm;
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

/// The bounds one archive is read under: its files, each of them, and all of them together.
const ARCHIVE: bounded_zip::ZipLimits = bounded_zip::ZipLimits {
    max_parts: MAX_ARCHIVE_FILES,
    max_part_bytes: MAX_ARCHIVE_PART,
    max_total_bytes: MAX_ARCHIVE_BYTES,
};

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
        }
    }

    /// [`Self::new`] with where the file stands in what the person chose.
    #[must_use]
    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = path.into();
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
    let size = match fields.get("size") {
        None => UNSTATED_SIZE,
        Some(size) => size.as_i64().filter(|size| *size >= 0).ok_or_else(|| {
            format!("a file's `size` is how many bytes it is, and {size} is none")
        })?,
    };
    Ok(FileValue::new(name, media, size, hash).with_path(path))
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
                    3 => {
                        if file.path.is_empty() {
                            file.name.clone()
                        } else {
                            file.path.clone()
                        }
                    }
                    other => return trap(format!("a file has no part {other}")),
                };
                self.ret_h(Value::text(part))
            }
            Imp::FileSize => {
                let file = self.file_arg(a.h(0)?)?;
                let size = if file.size >= 0 {
                    file.size
                } else {
                    // The caller named no size: the content answers how long it is.
                    let bytes = self
                        .blobs
                        .borrow()
                        .as_ref()
                        .and_then(|blobs| blobs.read(&file.hash));
                    let Some(bytes) = bytes else {
                        return trap(format!(
                            "the content of `{}` is not on this device",
                            file.name
                        ));
                    };
                    i64::try_from(bytes.len()).unwrap_or(i64::MAX)
                };
                ret_i64(size)
            }
            Imp::FileText => {
                let file = self.file_arg(a.h(0)?)?;
                let answer = match self.blobs.borrow().as_ref() {
                    Some(blobs) => read_text(blobs.as_ref(), &file),
                    None => Err("this host holds no content store".to_owned()),
                };
                self.result(answer.map(Value::text))
            }
            Imp::FileData => {
                let file = self.file_arg(a.h(0)?)?;
                self.result(
                    self.file_bytes(&file)
                        .map(|bytes| Value::Bytes(Rc::new(bytes))),
                )
            }
            Imp::SheetFile => {
                let file = self.file_arg(a.h(0)?)?;
                self.result(self.sheet_import(&file).map(Value::text))
            }
            Imp::FileFiles => {
                let file = self.file_arg(a.h(0)?)?;
                let answer = match self.blobs.borrow().as_ref() {
                    Some(blobs) => files_of(blobs.as_ref(), &file),
                    None => Err("this host holds no content store".to_owned()),
                };
                self.result(answer.map(Value::List))
            }
            Imp::FileBook | Imp::FileBookInfo | Imp::FileBookCover => self.file_book(imp, a),
            _ => trap("not a file import"),
        }
    }

    /// The book a file is: its chapters as rich text, each beginning with its title as a heading,
    /// what its package says about it, or its cover. Reading a book is a read of the content's
    /// bytes, like `.text()`: the same bytes always read the same book.
    fn file_book(&self, imp: Imp, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let file = self.file_arg(a.h(0)?)?;
        let bytes = self
            .blobs
            .borrow()
            .as_ref()
            .and_then(|blobs| blobs.read(&file.hash));
        let Some(bytes) = bytes else {
            return trap(format!(
                "the content of `{}` is not on this device",
                file.name
            ));
        };
        let book = match tilo_doc_codecs::book::read(&bytes) {
            Ok(book) => book,
            Err(why) => return trap(format!("`{}` is not a book: {why}", file.name)),
        };
        if matches!(imp, Imp::FileBookCover) {
            let Some((media, cover)) = book.cover else {
                return self.ret_h(Value::Opt(None));
            };
            let name = format!("cover.{}", media.rsplit('/').next().unwrap_or("bin"));
            let made = self
                .blobs
                .borrow()
                .as_ref()
                .and_then(|blobs| made(blobs.as_ref(), &name, Held::Bytes(cover)).ok());
            return self.ret_h(Value::Opt(made.map(|file| Box::new(Value::file(file)))));
        }
        if matches!(imp, Imp::FileBookInfo) {
            let facts = [
                ("title", book.title),
                ("author", book.author),
                ("language", book.language),
                ("subject", book.subject),
            ];
            return self.ret_h(Value::Map(
                facts
                    .into_iter()
                    .map(|(key, value)| (Value::text(key), Value::text(value)))
                    .collect(),
            ));
        }
        let mut chapters = Vec::with_capacity(book.chapters.len());
        for chapter in &book.chapters {
            chapters.push(Value::Rich(chapter_rich(chapter)));
        }
        self.ret_h(Value::List(chapters))
    }

    /// The `File` value behind the handle `h`.
    pub(crate) fn file_arg(&self, h: i32) -> HResult<Rc<FileValue>> {
        match self.val(h)? {
            Value::File(file) => Ok(file),
            other => trap(format!("expected a file, got {}", other.kind_name())),
        }
    }

    /// The content `file` names, when this device holds it.
    pub(crate) fn file_bytes(&self, file: &FileValue) -> Result<Vec<u8>, String> {
        let blobs = self.blobs.borrow();
        let Some(blobs) = blobs.as_ref() else {
            return Err("this host holds no content store".to_owned());
        };
        blobs
            .read(&file.hash)
            .ok_or_else(|| format!("the content of `{}` is not on this device", file.name))
    }

    /// `bytes` of `media` kept in the content store as a file called `name`.
    pub(crate) fn keep_bytes(
        &self,
        name: &str,
        media: &str,
        bytes: &[u8],
    ) -> Result<FileValue, String> {
        let blobs = self.blobs.borrow();
        let Some(blobs) = blobs.as_ref() else {
            return Err("this host keeps no files".to_owned());
        };
        blobs.keep(name, media, bytes)
    }

    /// A file an action makes of what it holds: the text, the bytes, the content an address
    /// names, or an archive of the files it holds, each under the name the file has. Its bytes
    /// are kept in the device's content store, under the name it is made as, and the `File` it
    /// answers has that name, its media type, its size and the address of its content.
    pub(crate) fn file_make(&self, imp: Imp, a: &Args<'_>) -> HResult<Option<Wasm>> {
        let name = self.text(a.h(0)?)?;
        let held = match imp {
            Imp::FileOfText => Held::Text(self.text(a.h(1)?)?),
            Imp::FileOfBytes => match self.val(a.h(1)?)? {
                Value::Bytes(bytes) => Held::Bytes((*bytes).clone()),
                other => {
                    return trap(format!(
                        "`File.of_bytes` holds bytes, and this is a {}",
                        other.kind_name()
                    ));
                }
            },
            Imp::FileAt => Held::At(self.text(a.h(1)?)?),
            Imp::SheetOfFile => {
                let workbook = self.text(a.h(1)?)?;
                let format = self.text(a.h(2)?)?;
                match self.sheet_export(&workbook, &format) {
                    Ok(bytes) => Held::Bytes(bytes),
                    Err(why) => return trap(why),
                }
            }
            Imp::FileZip => match self.val_whole(a.h(1)?)? {
                Value::List(items) => Held::Files(files_of_list(&items)?),
                other => {
                    return trap(format!(
                        "`File.zip` holds files, and this is a {}",
                        other.kind_name()
                    ));
                }
            },
            _ => return trap("not a file an action makes"),
        };
        let answer = match self.blobs.borrow().as_ref() {
            Some(blobs) => made(blobs.as_ref(), &name, held),
            None => Err("this host keeps no files".to_owned()),
        };
        match answer {
            Ok(file) => self.ret_h(Value::file(file)),
            Err(why) => trap(why),
        }
    }
}

/// What an action holds that it makes a file of.
enum Held {
    /// The text the file holds, as its bytes.
    Text(String),
    /// The bytes it holds.
    Bytes(Vec<u8>),
    /// The address (`tilo-blob://<hash>`) of content the device holds.
    At(String),
    /// The files, each under the name it is called: the parts of an archive.
    Files(Vec<FileValue>),
}

/// The files `items` holds, or why one of them is not a file.
fn files_of_list(items: &[Value]) -> HResult<Vec<FileValue>> {
    let mut files = Vec::with_capacity(items.len());
    for item in items {
        match item {
            Value::File(file) => files.push((**file).clone()),
            other => {
                return trap(format!(
                    "`File.zip` holds files, and this list holds a {}",
                    other.kind_name()
                ));
            }
        }
    }
    Ok(files)
}

/// A chapter read as the rich text a reader shows: its XHTML by the one HTML importer, with its
/// title as a heading when the chapter does not already begin with that heading, so every
/// chapter's first block names it and the contents can be read off the text. Paragraphs with no
/// words are left out: markup puts whitespace between blocks, and the importer keeps it as empty
/// paragraphs a page should not hold.
fn chapter_rich(chapter: &tilo_doc_codecs::book::Chapter) -> crate::rich::RichValue {
    let mut blocks = tilo_doc_codecs::rich::blocks_of_html(&chapter.xhtml).unwrap_or_default();
    blocks.retain(|block| block.kind != "paragraph" || !block.text.trim().is_empty());
    let opens_with_title = blocks.first().is_some_and(|block| {
        block.kind == "heading" && block.text.trim().eq_ignore_ascii_case(chapter.title.trim())
    });
    if !opens_with_title && !chapter.title.trim().is_empty() {
        let mut attrs = std::collections::BTreeMap::new();
        attrs.insert("level".to_owned(), tilo_doc::snapshot::AttrVal::Int(1));
        blocks.insert(
            0,
            tilo_doc::snapshot::BlockView {
                id: "chapter-title".to_owned(),
                kind: "heading".to_owned(),
                text: chapter.title.clone(),
                marks: Vec::new(),
                attrs,
                children: Vec::new(),
            },
        );
    }
    crate::rich::RichValue {
        blocks,
        styles: None,
    }
}

/// The file `name`, of what `held` is: the text or the bytes kept in the content store, the
/// content an address names, or an archive written of `files`, each part stored under its own
/// name and safe to extract ([`bounded_zip::is_safe_name`]).
fn made(blobs: &dyn BlobHost, name: &str, held: Held) -> Result<FileValue, String> {
    match held {
        Held::Text(text) => kept(blobs, name, text.as_bytes(), MAX_CHOSEN),
        Held::Bytes(bytes) => kept(blobs, name, &bytes, MAX_CHOSEN),
        // The content is the store's already, under the address the action named: the file only
        // says what it is called and which media type it is.
        Held::At(address) => {
            let Some(hash) = address.strip_prefix(SCHEME) else {
                return Err(format!(
                    "`{address}` is not the address of a file's content: they read `{SCHEME}<hash>`"
                ));
            };
            let bytes = blobs
                .read(hash)
                .ok_or_else(|| format!("the content of `{name}` is not on this device"))?;
            let size = within(name, bytes.len(), MAX_CHOSEN)?;
            Ok(FileValue::new(name, media_of(name), size, hash))
        }
        Held::Files(files) => {
            let mut entries: Vec<(String, Vec<u8>)> = Vec::with_capacity(files.len());
            for file in files {
                let bytes = blobs.read(&file.hash).ok_or_else(|| {
                    format!("the content of `{}` is not on this device", file.name)
                })?;
                entries.push((file.name, bytes));
            }
            let bytes = bounded_zip::write_zip(&entries, &ARCHIVE)
                .map_err(|why| format!("`{name}` is not an archive that writes: {why}"))?;
            let limit = usize::try_from(MAX_ARCHIVE_BYTES).unwrap_or(usize::MAX);
            kept(blobs, name, &bytes, limit)
        }
    }
}

/// `bytes` kept in the content store as a file called `name`, of the media type its name tells,
/// when it holds no more than a file a person picks may: a file an earlier build of an app kept,
/// which its migration carries over.
pub(crate) fn kept_file(
    blobs: &dyn BlobHost,
    name: &str,
    bytes: &[u8],
) -> Result<FileValue, String> {
    kept(blobs, name, bytes, MAX_CHOSEN)
}

/// `bytes` kept in the content store as a file called `name`, of the media type its name tells,
/// when it holds no more than `limit` bytes.
fn kept(blobs: &dyn BlobHost, name: &str, bytes: &[u8], limit: usize) -> Result<FileValue, String> {
    within(name, bytes.len(), limit)?;
    blobs.keep(name, &media_of(name), bytes)
}

/// How many bytes a file of that size holds, or why a file of `name` may not hold them.
fn within(name: &str, len: usize, limit: usize) -> Result<i64, String> {
    if len > limit {
        return Err(format!(
            "`{name}` is {len} bytes: more than the {limit} a file it makes may hold"
        ));
    }
    Ok(i64::try_from(len).unwrap_or(i64::MAX))
}

/// The file's content as text, or why it is not: nothing this device holds, more than
/// [`MAX_TEXT`], or bytes that are not UTF-8.
fn read_text(blobs: &dyn BlobHost, file: &FileValue) -> Result<String, String> {
    if file.size > MAX_TEXT_BYTES {
        return Err(too_large(
            &file.name,
            file.size,
            MAX_TEXT,
            "an action reads as text",
        ));
    }
    let Some(bytes) = blobs.read(&file.hash) else {
        return Err(format!(
            "the content of `{}` is not on this device",
            file.name
        ));
    };
    // The size a caller named no word of is the bound the bytes read are held to: naming none
    // never lets a larger file through.
    let len = i64::try_from(bytes.len()).unwrap_or(i64::MAX);
    if len > MAX_TEXT_BYTES {
        return Err(too_large(
            &file.name,
            len,
            MAX_TEXT,
            "an action reads as text",
        ));
    }
    String::from_utf8(bytes).map_err(|_| format!("`{}` is not text", file.name))
}

/// Why a file of `len` bytes is no file `within_a`: it holds more than `limit`.
fn too_large(name: &str, len: i64, limit: usize, within_a: &str) -> String {
    format!("`{name}` is {len} bytes: more than the {limit} {within_a}")
}

/// The files an archive holds, each kept in the device's content store under the name it has
/// inside it, which is what an importer of a package (an export of notes, a zip of documents)
/// reads names and nesting from. A file is read within [`ARCHIVE`]'s bounds: an archive is a
/// person's, and one that holds more than an importer reads is refused with why.
fn files_of(blobs: &dyn BlobHost, file: &FileValue) -> Result<Vec<Value>, String> {
    if u64::try_from(file.size).unwrap_or(u64::MAX) > MAX_ARCHIVE_BYTES {
        return Err(too_large(
            &file.name,
            file.size,
            usize::try_from(MAX_ARCHIVE_BYTES).unwrap_or(usize::MAX),
            "an archive may be",
        ));
    }
    let Some(bytes) = blobs.read(&file.hash) else {
        return Err(format!(
            "the content of `{}` is not on this device",
            file.name
        ));
    };
    // The size a caller named no word of is the bound the bytes read are held to.
    let len = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if len > MAX_ARCHIVE_BYTES {
        return Err(too_large(
            &file.name,
            i64::try_from(len).unwrap_or(i64::MAX),
            usize::try_from(MAX_ARCHIVE_BYTES).unwrap_or(usize::MAX),
            "an archive may be",
        ));
    }
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

    tilo_test_env::scenario! {
        fn files_are_addressed_by_content_and_chosen_by_type;

        phase a_files_address_is_the_content_it_names;
        {
            let file = FileValue::new("plan.png", "image/png", 12, "bafy123");
            assert_eq!(file.url(), "tilo-blob://bafy123");
            assert_eq!(media_of("plan.png"), "image/png");
            assert_eq!(media_of("notes.MD"), "text/markdown");
            assert_eq!(media_of("thing"), "application/octet-stream");
        }

        phase a_chooser_takes_the_types_it_names;
        {
            let images = vec!["image/*".to_owned()];
            assert!(accepts(&images, "image/png"));
            assert!(!accepts(&images, "text/plain"));
            let one = vec!["image/webp".to_owned()];
            assert!(accepts(&one, "image/webp"));
            assert!(!accepts(&one, "image/png"));
            assert!(accepts(&[], "anything/at-all"), "no type asked, any file");
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
