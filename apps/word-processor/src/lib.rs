//! Binds a `tilo-doc` rich-text document into a host automerge doc, so the document
//! reuses the toolkit's undo, incremental sync and presence machinery.

// An app runs sandboxed, so an integer that wraps here costs one app rather than the
// desktop, and a third-party app author would pay the same tax. The lints that produce a
// crash stay denied: indexing, string slicing, unwrap, expect and panic.
#![allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
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
#[cfg(test)]
mod agent_tests;
mod compact_swatch_palette;
mod document_find;
pub mod effects;
pub mod file_convert;
mod file_menu;
pub mod framework;
pub mod i18n;
pub mod model;
mod outline_pane;
mod paint_context;
mod review_overlay;
mod ruler;
pub mod semantic_read;
mod style_color_picker;
mod style_menu;
mod text_offsets;
mod zoom_control;
pub use ui::num;
pub mod styles;
pub mod widget;
pub mod word_processor;
pub use styles::{StyleProps, StyleSheet};
pub use widget::{WordProcessorWidget, WriterLayout};

/// The record kind of a Writer document.
pub const RECORD_KIND: &str = <model::Document as tilo_resource::Resource>::KIND;

pub fn initialize_empty_document(doc: &mut AutoCommit) -> Result<(), String> {
    <framework::Document as tilo_resource::Resource>::initialize(doc)
        .map_err(|error| error.to_string())
}

#[cfg(test)]
pub(crate) fn initialized_state() -> ui::State {
    let mut state = ui::State::new();
    if let Err(error) = initialize_empty_document(&mut state.doc) {
        panic!("a new document initializes: {error}");
    }
    state
}

pub fn definition() -> &'static ui::application::ApplicationDefinition {
    ui::built_in_definition!(framework::WordProcessorApp)
}

pub use word_processor::WordProcessor;

use automerge::AutoCommit;

use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WpFormat {
    Odt,
    Docx,
    Markdown,
    Text,
    /// Export only, rendered by `tilo-doc-pdf`; importing PDF is refused.
    Pdf,
    /// Imported and exported by `tilo_doc_codecs::HtmlCodec`.
    Html,
    /// Imported and exported by `tilo_doc_codecs::RtfCodec`.
    Rtf,
    /// Export only, rendered by `tilo_doc_codecs::export_epub`.
    Epub,
}

fn not_imported(name: &str, bytes: &[u8]) -> String {
    format!(
        "{name} is not a document Writer imports: {}",
        tilo_content::opened_by(bytes).unwrap_or_default()
    )
}

impl WpFormat {
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

    /// The format of `bytes` kept as content under `name`: the one the bytes hold,
    /// whatever the name says; only text kinds need the name.
    pub fn of_content(name: &str, bytes: &[u8]) -> Result<Self, String> {
        if let Some(format) = tilo_content::document_format(bytes) {
            return match format.extension {
                "docx" => Ok(WpFormat::Docx),
                "odt" => Ok(WpFormat::Odt),
                "rtf" => Ok(WpFormat::Rtf),
                "html" => Ok(WpFormat::Html),
                _ => Err(not_imported(name, bytes)),
            };
        }
        if tilo_content::opened_by(bytes).is_some() {
            return Err(not_imported(name, bytes));
        }
        Ok(match WpFormat::from_path(Path::new(name)) {
            WpFormat::Docx | WpFormat::Odt | WpFormat::Rtf | WpFormat::Pdf | WpFormat::Epub => {
                WpFormat::Markdown
            }
            text => text,
        })
    }

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
            // PDF renders from the snapshot through tilo-doc-pdf, not a FormatProvider codec.
            WpFormat::Pdf => {
                let snap = dr.materialize();
                tilo_doc_pdf::render_pdf(&snap, content)
                    .map_err(|e| tilo_doc_codecs::CodecError::Export(e.to_string()))
            }
            // HTML and RTF render from the snapshot, not through a FormatProvider codec.
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
            // EPUB assembles an OCF ZIP container from the snapshot.
            WpFormat::Epub => {
                let snap = dr.materialize();
                let rendered = tilo_doc_codecs::export_epub::render_epub(&snap, content);
                rendered.map_err(|e| tilo_doc_codecs::CodecError::Export(e.to_string()))
            }
        }
    }
}

/// # Errors
/// Why the bytes cannot be imported: an export-only format, a codec rejection, or
/// images that could not be kept.
pub fn import_action(
    bytes: &[u8],
    format: WpFormat,
    content: &tilo_content::ContentStore,
) -> Result<actions::RichTextAction, String> {
    let import = format
        .import_format()
        .ok_or_else(|| format!("{format:?} documents cannot be opened"))?;
    let staged = tilo_document::action::stage_import(import, bytes)?;
    let kept = content
        .put_inputs(staged.inputs())
        .map_err(|error| format!("could not keep the file's images: {error}"))?;
    Ok(staged.action(kept))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Template {
    Blank,
    Letter,
    Report,
    Resume,
    MeetingNotes,
}

impl Template {
    pub fn all() -> &'static [Template] {
        &[
            Template::Blank,
            Template::Letter,
            Template::Report,
            Template::Resume,
            Template::MeetingNotes,
        ]
    }

    pub fn title(self) -> &'static str {
        match self {
            Template::Blank => "Blank",
            Template::Letter => "Letter",
            Template::Report => "Report",
            Template::Resume => "Resume",
            Template::MeetingNotes => "Meeting Notes",
        }
    }

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

/// # Errors
/// Why the document cannot be exported, or the file cannot be written.
pub fn save_document(
    doc: &AutoCommit,
    path: &Path,
    format: WpFormat,
    content: &dyn tilo_content::ContentSource,
) -> std::io::Result<()> {
    let invalid = |e: String| std::io::Error::new(std::io::ErrorKind::InvalidData, e);
    let doc = crate::model::export_copy(doc).map_err(invalid)?;
    let bytes = format
        .export(doc.as_ref(), content)
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    tilo_files::file_at(path)
        .and_then(|(dir, name)| tilo_files::blocking::save_document(&dir, &name, &bytes))
        .map_err(std::io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn imported(bytes: &[u8], format: WpFormat) -> AutoCommit {
        let mut doc = AutoCommit::new();
        initialize_empty_document(&mut doc).unwrap();
        let action = import_action(bytes, format, &tilo_content::ContentStore::memory()).unwrap();
        model::apply(&mut doc, &action).unwrap();
        doc
    }

    fn blocks(doc: &AutoCommit) -> Vec<tilo_doc::snapshot::BlockView> {
        let containers = crate::model::open(doc).unwrap();
        tilo_doc::model::materialize(doc, &containers).blocks
    }

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
