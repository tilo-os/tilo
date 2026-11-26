//! The host calls on files ([`crate::blob`]): a file's parts, its content read as text or as
//! the files an archive holds, the files a person chooses, and the files an action makes and
//! saves. They need the device's content store ([`crate::blob::BlobHost`]); the file records
//! and their bounds do not, and live beside the value they are.

use std::rc::Rc;

use crate::abi::Imp;
use crate::blob::{
    BlobHost, FileValue, MAX_ARCHIVE_BYTES, MAX_ARCHIVE_FILES, MAX_ARCHIVE_PART, MAX_CHOSEN,
    MAX_TEXT, MAX_TEXT_BYTES, SCHEME, media_of,
};
use crate::core::{Args, Core, HResult, ret_i32, ret_i64, trap};
use crate::engine::Wasm;
use crate::value::Value;

/// The bounds one archive is read under: its files, each of them, and all of them together.
const ARCHIVE: bounded_zip::ZipLimits = bounded_zip::ZipLimits {
    max_parts: MAX_ARCHIVE_FILES,
    max_part_bytes: MAX_ARCHIVE_PART,
    max_total_bytes: MAX_ARCHIVE_BYTES,
};

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
            Imp::FileHere => {
                let file = self.file_arg(a.h(0)?)?;
                let here = self
                    .blobs
                    .borrow()
                    .as_ref()
                    .and_then(|blobs| blobs.read(&file.hash))
                    .is_some();
                ret_i32(i32::from(here))
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
            Imp::PresentationFile => {
                let file = self.file_arg(a.h(0)?)?;
                self.result(self.presentation_import(&file).map(Value::text))
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
            Imp::PresentationOfFile => {
                let deck = self.text(a.h(1)?)?;
                let format = self.text(a.h(2)?)?;
                match self.presentation_export(&deck, &format) {
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
            // A handle to what an address names, whether or not this device holds the bytes yet:
            // a picture's record reaches a device before its bytes do, and saying that is what
            // `File.here` and a viewer waiting for it are for. What reads the bytes says when
            // they are not here, in its own words.
            Ok(FileValue::new(name, media_of(name), -1, hash))
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
