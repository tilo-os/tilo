//! `editor` — binds a `qdoc` canonical rich-text document into a host automerge
//! doc (e.g. `ui::State.doc`) so the document reuses the UI toolkit's
//! undo / incremental-sync / presence machinery without a message layer.
//!
//! The interactive GUI widget (draw/layout/keyboard over winit) is intentionally
//! deferred — it is app polish, not part of CRDT-native file-format support. This
//! crate currently provides the binding and its integration with `ui::State`.

// An app runs as its own sandboxed process, so an integer that wraps here
// costs one app rather than the desktop, and proving every layout sum cannot
// overflow is a tax a third-party app author would pay too. The lints that
// produce a *crash* stay denied — indexing_slicing, string_slice,
// unwrap_used, expect_used, panic — because a panicking app is a bad app
// whoever wrote it. `shell`, `ui` and everything under `crates/` keep the
// full set: a fault there takes the session, or every app with it.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "app-local arithmetic; see the note above"
)]
#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        clippy::float_cmp,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod actions;
mod compact_swatch_palette;
pub mod effects;
mod find_bar;
pub mod framework;
pub mod i18n;
mod outline_pane;
mod paint_context;
mod review_overlay;
mod ruler;
pub mod semantic_read;
mod style_color_picker;
mod style_menu;
mod template_picker;
mod text_offsets;
mod zoom_control;
pub use ui::num;
pub mod styles;
pub mod widget;
pub mod word_processor;
pub use styles::{StyleProps, StyleSheet};
pub use widget::{PersonalDictionary, WordProcessorWidget, WriterLayout};

/// The kind of a Writer document record: the rich-text resource the native
/// app, the workspace host and the browser all open. Records stamped with the
/// former `document` read as this kind (see [`ui::RecordKind::new`]).
pub const RECORD_KIND: &str = <tilo_document::RichText as tilo_resource::Resource>::KIND;

/// Initialize a new record as an empty Writer document: the document
/// resource's genesis, for hosts that reserve a record outside the app.
///
/// # Errors
///
/// Returns why the document could not be initialized.
pub fn initialize_empty_document(doc: &mut AutoCommit) -> Result<(), String> {
    <framework::Document as tilo_resource::Resource>::initialize(doc)
        .map_err(|error| error.to_string())
}

/// A UI state holding a new Writer record, as a host hands an editor one.
#[cfg(test)]
pub(crate) fn initialized_state() -> ui::State {
    let mut state = ui::State::new();
    if let Err(error) = initialize_empty_document(&mut state.doc) {
        panic!("a new document initializes: {error}");
    }
    state
}

/// The word processor as every host mounts it: the framework app over the
/// document's rich-text record.
pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::framework::definition::<framework::WordProcessorApp>()
}

pub use word_processor::WordProcessor;

use automerge::AutoCommit;

// ---- document open / save (format-aware file I/O) -------------------------

use std::path::Path;

/// The file formats the word processor can open and save.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WpFormat {
    Odt,
    Docx,
    Markdown,
    /// Plain text: each line is a paragraph, and nothing is markup.
    Text,
    /// PDF — **export only** (rendered via the `tilo-doc-pdf` crate). Importing PDF is
    /// not supported and returns a clear error.
    Pdf,
    /// HTML — export via `tilo_doc_codecs::export_html`; **import** via `tilo_doc_codecs::HtmlCodec` (so
    /// `.html`/`.htm` files can be opened as well as saved).
    Html,
    /// RTF — export via `tilo_doc_codecs::export_rtf`; **import** via `tilo_doc_codecs::RtfCodec` (so
    /// `.rtf` files can be opened as well as saved).
    Rtf,
    /// EPUB — **export only** (rendered via `tilo_doc_codecs::export_epub`). Importing EPUB
    /// is not supported and returns a clear error.
    Epub,
}

impl WpFormat {
    /// Pick a format from a path extension (defaults to Markdown).
    pub fn from_path(path: &Path) -> Self {
        match path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("odt") => WpFormat::Odt,
            Some("docx") => WpFormat::Docx,
            Some("pdf") => WpFormat::Pdf,
            Some("html" | "htm") => WpFormat::Html,
            Some("rtf") => WpFormat::Rtf,
            Some("epub") => WpFormat::Epub,
            Some("txt" | "text") => WpFormat::Text,
            _ => WpFormat::Markdown,
        }
    }

    /// The format [`actions::RichTextAction::ReplaceFormatted`] imports this
    /// one as; `None` for the export-only formats.
    pub fn import_format(self) -> Option<actions::ImportFormat> {
        match self {
            WpFormat::Markdown => Some(actions::ImportFormat::Markdown),
            WpFormat::Text => Some(actions::ImportFormat::Text),
            WpFormat::Html => Some(actions::ImportFormat::Html),
            WpFormat::Rtf => Some(actions::ImportFormat::Rtf),
            WpFormat::Docx => Some(actions::ImportFormat::Docx),
            WpFormat::Odt => Some(actions::ImportFormat::Odt),
            WpFormat::Pdf | WpFormat::Epub => None,
        }
    }

    /// Project `dr` into this format, embedding the images it references from `content`.
    pub(crate) fn export(
        self,
        dr: tilo_doc::DocRef,
        content: &dyn tilo_content::ContentSource,
    ) -> Result<Vec<u8>, tilo_doc_codecs::CodecError> {
        use tilo_doc_codecs::FormatProvider;
        match self {
            WpFormat::Odt => tilo_doc_codecs::OdtCodec.export(dr, content),
            WpFormat::Docx => tilo_doc_codecs::DocxCodec.export(dr, content),
            WpFormat::Markdown => tilo_doc_codecs::MarkdownCodec.export(dr, content),
            WpFormat::Text => {
                let lines: Vec<String> = dr
                    .materialize()
                    .blocks
                    .into_iter()
                    .map(|block| block.text)
                    .collect();
                Ok(format!("{}\n", lines.join("\n")).into_bytes())
            }
            // PDF isn't a `FormatProvider` codec — it renders from the materialized
            // snapshot via `tilo-doc-pdf`. Map its error into the shared `CodecError`.
            WpFormat::Pdf => {
                let snap = dr.materialize();
                tilo_doc_pdf::render_pdf(&snap, content)
                    .map_err(|e| tilo_doc_codecs::CodecError::Export(e.to_string()))
            }
            // HTML/RTF aren't `FormatProvider` codecs either — they render from
            // the materialized snapshot via `tilo_doc_codecs::export_html`/`tilo_doc_codecs::export_rtf`,
            // returning UTF-8 bytes.
            WpFormat::Html => {
                let snap = dr.materialize();
                let rendered = tilo_doc_codecs::export_html::render_html(
                    &snap,
                    tilo_doc_codecs::export_html::HtmlOptions::default(),
                    content,
                );
                Ok(rendered.into_bytes())
            }
            WpFormat::Rtf => {
                let snap = dr.materialize();
                let rendered = tilo_doc_codecs::export_rtf::render_rtf(&snap);
                Ok(rendered.into_bytes())
            }
            // EPUB isn't a `FormatProvider` codec either — it assembles an OCF
            // ZIP container from the materialized snapshot via `tilo_doc_codecs::export_epub`.
            // Map its error into the shared `CodecError`.
            WpFormat::Epub => {
                let snap = dr.materialize();
                let rendered = tilo_doc_codecs::export_epub::render_epub(&snap, content);
                rendered.map_err(|e| tilo_doc_codecs::CodecError::Export(e.to_string()))
            }
        }
    }
}

/// The action that replaces a document's body with `bytes` read as
/// `format`: the import a launch on a file takes, and Open in place. The
/// images the file embeds are kept in `content` first, since the document
/// only names them.
///
/// # Errors
///
/// Returns why the bytes cannot be imported: an export-only format, a codec
/// rejection, or images that could not be kept.
pub fn import_action(
    bytes: &[u8],
    format: WpFormat,
    content: &tilo_content::ContentStore,
) -> Result<actions::RichTextAction, String> {
    let import = format
        .import_format()
        .ok_or_else(|| format!("{format:?} documents cannot be opened"))?;
    let (action, inputs) = tilo_document::action::stage_import(import, bytes)?;
    content
        .put_inputs(&inputs)
        .map_err(|error| format!("could not keep the file's images: {error}"))?;
    Ok(action)
}

// ---- document templates (new-from-template) -------------------------------

/// A bundled starter document. The content is embedded at compile time
/// (`include_str!`) so there is no runtime file dependency — a template is just
/// Markdown that seeds a fresh, untitled document.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Template {
    /// An (almost) empty document — a single heading and a prompt to start.
    Blank,
    /// A formal letter scaffold (sender/recipient blocks, salutation, body).
    Letter,
    /// A structured report (summary, background, findings, recommendations).
    Report,
    /// A résumé / CV scaffold (summary, experience, education, skills).
    Resume,
    /// Meeting notes (attendees, agenda, decisions, action items).
    MeetingNotes,
}

impl Template {
    /// Every template variant, in menu order.
    pub fn all() -> &'static [Template] {
        &[
            Template::Blank,
            Template::Letter,
            Template::Report,
            Template::Resume,
            Template::MeetingNotes,
        ]
    }

    /// A short, **stable English** identifier for the template. Used as a
    /// canonical name in code and tests; the user-facing menu label comes from
    /// [`Self::display_title`] (which localizes via Fluent).
    pub fn title(self) -> &'static str {
        match self {
            Template::Blank => "Blank",
            Template::Letter => "Letter",
            Template::Report => "Report",
            Template::Resume => "Resume",
            Template::MeetingNotes => "Meeting Notes",
        }
    }

    /// The localized, user-facing title shown in the "New" template picker.
    /// Resolves through Fluent; under `en-US` it matches [`Self::title`].
    pub fn display_title(self) -> String {
        let key = match self {
            Template::Blank => "template-blank",
            Template::Letter => "template-letter",
            Template::Report => "template-report",
            Template::Resume => "template-resume",
            Template::MeetingNotes => "template-meeting-notes",
        };
        crate::i18n::t(key)
    }

    /// The template's Markdown source, embedded at compile time.
    pub fn content(self) -> &'static str {
        match self {
            Template::Blank => include_str!("../templates/blank.md"),
            Template::Letter => include_str!("../templates/letter.md"),
            Template::Report => include_str!("../templates/report.md"),
            Template::Resume => include_str!("../templates/resume.md"),
            Template::MeetingNotes => include_str!("../templates/meeting-notes.md"),
        }
    }
}

/// Export `doc` to `path` using `format`.
///
/// # Errors
///
/// Fails if `doc` does not hold a well-formed document, if the codec cannot
/// render it (an import-only format), or if `path` cannot be written.
pub fn save_document(
    doc: &AutoCommit,
    path: &Path,
    format: WpFormat,
    content: &dyn tilo_content::ContentSource,
) -> std::io::Result<()> {
    let containers = tilo_doc::model::open(doc, &automerge::ROOT)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    let dr = tilo_doc::DocRef {
        doc,
        c: &containers,
    };
    let bytes = format
        .export(dr, content)
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    tilo_files::file_at(path)
        .and_then(|(dir, name)| tilo_files::blocking::save_document(&dir, &name, &bytes))
        .map_err(std::io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A new document with `bytes` imported as `format`, as a launch on a
    /// file of that format starts its record.
    fn imported(bytes: &[u8], format: WpFormat) -> AutoCommit {
        let mut doc = AutoCommit::new();
        initialize_empty_document(&mut doc).unwrap();
        let action = import_action(bytes, format, &tilo_content::ContentStore::memory()).unwrap();
        tilo_actions::Action::apply(&action, &mut doc).unwrap();
        doc
    }

    fn blocks(doc: &AutoCommit) -> Vec<tilo_doc::snapshot::BlockView> {
        let containers = tilo_doc::model::open(doc, &automerge::ROOT).unwrap();
        tilo_doc::model::materialize(doc, &containers).blocks
    }

    /// Export `doc` as `format` to a scratch file and read the bytes back.
    fn exported(doc: &AutoCommit, format: WpFormat, extension: &str) -> Vec<u8> {
        let path = std::env::temp_dir().join(format!(
            "editor-export-test-{}-{extension}.{extension}",
            std::process::id()
        ));
        assert_eq!(WpFormat::from_path(&path), format);
        save_document(doc, &path, format, &tilo_content::NoContent).expect("save");
        let bytes = std::fs::read(&path).expect("read back");
        let _ = std::fs::remove_file(&path);
        bytes
    }

    /// Save to PDF, HTML, RTF and EPUB write valid files; PDF and EPUB import are unsupported.
    #[test]
    fn export_formats_write_valid_files_and_refuse_unsupported_imports() {
        let doc = imported(b"# Title\n\nHello world\n", WpFormat::Markdown);

        let pdf = exported(&doc, WpFormat::Pdf, "pdf");
        assert!(pdf.starts_with(b"%PDF"), "{:?}", &pdf[..pdf.len().min(8)]);

        let html = String::from_utf8(exported(&doc, WpFormat::Html, "html")).unwrap();
        let lower = html.to_ascii_lowercase();
        assert!(
            lower.contains("<!doctype html") || lower.contains("<html"),
            "output is an HTML document: {:?}",
            html.get(..html.len().min(64))
        );

        let rtf = exported(&doc, WpFormat::Rtf, "rtf");
        assert!(rtf.starts_with(b"{\\rtf"), "{:?}", &rtf[..rtf.len().min(8)]);

        // Every EPUB is an OCF ZIP container whose first entry is the literal
        // `mimetype` member; its file name starts at offset 30.
        let epub = exported(&doc, WpFormat::Epub, "epub");
        assert!(epub.starts_with(b"PK"));
        assert!(epub.len() >= 38);
        assert_eq!(&epub[30..38], b"mimetype");

        for (bytes, format) in [
            (b"%PDF-1.7".as_slice(), WpFormat::Pdf),
            (b"PK\x03\x04".as_slice(), WpFormat::Epub),
        ] {
            assert!(
                import_action(bytes, format, &tilo_content::ContentStore::memory()).is_err(),
                "{format:?} is export-only"
            );
        }
    }

    /// A `.txt` file opens with one paragraph per line, markup characters
    /// kept as text, and saves back to the same lines.
    #[test]
    fn plain_text_opens_one_paragraph_per_line_and_saves_back() {
        let lines = [
            "Standup 2026-09-24",
            "# Maya: finished; next: review. Blocked: no",
            "- Jonas: *sync* bug",
            "",
            "Priya: done",
        ];
        assert_eq!(
            WpFormat::from_path(Path::new("standup.txt")),
            WpFormat::Text
        );
        let doc = imported(
            format!("{}\r\n", lines.join("\r\n")).as_bytes(),
            WpFormat::Text,
        );
        let blocks = blocks(&doc);
        let texts: Vec<&str> = blocks.iter().map(|b| b.text.as_str()).collect();
        assert_eq!(texts, lines);
        assert!(
            blocks.iter().all(|b| b.kind == "paragraph"),
            "nothing is read as markup"
        );
        assert_eq!(
            String::from_utf8(exported(&doc, WpFormat::Text, "txt")).unwrap(),
            format!("{}\n", lines.join("\n"))
        );
    }

    /// `from_path` maps html, htm and rtf; HTML import opens the first
    /// heading; RTF import opens a paragraph and an RTF export then import
    /// round-trips text and bold.
    #[test]
    fn imports_map_extensions_and_round_trip_text_and_bold() {
        for (name, format) in [
            ("doc.html", WpFormat::Html),
            ("doc.htm", WpFormat::Html),
            ("doc.HTML", WpFormat::Html),
            ("doc.rtf", WpFormat::Rtf),
            ("doc.RTF", WpFormat::Rtf),
            ("doc.epub", WpFormat::Epub),
            ("doc.EPUB", WpFormat::Epub),
        ] {
            assert_eq!(WpFormat::from_path(Path::new(name)), format, "{name}");
        }

        let html = imported(b"<h1>Opened from HTML</h1><p>body</p>", WpFormat::Html);
        let first = blocks(&html).into_iter().next().unwrap();
        assert_eq!(first.kind, "heading");
        assert_eq!(first.text, "Opened from HTML");

        let rtf = imported(
            b"{\\rtf1\\ansi\\deff0\n{\\pard\\fs22 Opened from RTF\\par}}",
            WpFormat::Rtf,
        );
        assert_eq!(blocks(&rtf)[0].text, "Opened from RTF");

        let source = imported(b"Hello **world** today\n", WpFormat::Markdown);
        let reopened = imported(&exported(&source, WpFormat::Rtf, "rtf"), WpFormat::Rtf);
        let p = blocks(&reopened)
            .into_iter()
            .find(|b| b.kind == "paragraph")
            .expect("a paragraph survives");
        assert_eq!(p.text, "Hello world today");
        let strong = p
            .marks
            .iter()
            .find(|m| m.name == "strong")
            .expect("bold run survives");
        assert_eq!(p.text.get(strong.start..strong.end), Some("world"));
    }

    /// Every template is listed once, in menu order, and imports real
    /// content; the report opens with its heading.
    #[test]
    fn templates_list_every_variant_and_seed_real_content() {
        assert_eq!(
            Template::all(),
            &[
                Template::Blank,
                Template::Letter,
                Template::Report,
                Template::Resume,
                Template::MeetingNotes,
            ]
        );
        for &t in Template::all() {
            assert!(!t.title().is_empty(), "{t:?} has a title");
            assert!(!t.content().trim().is_empty(), "{t:?} content is non-empty");
            let doc = imported(t.content().as_bytes(), WpFormat::Markdown);
            assert!(
                !blocks(&doc).is_empty(),
                "{t:?} produces at least one block"
            );
        }
        let report = imported(Template::Report.content().as_bytes(), WpFormat::Markdown);
        let first = blocks(&report).into_iter().next().unwrap();
        assert_eq!(first.kind, "heading");
        assert_eq!(first.text, "Report Title");
    }
}
