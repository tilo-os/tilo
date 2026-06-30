//! `editor` — binds a `qdoc` canonical rich-text document into a host automerge
//! doc (e.g. `ui::State.doc`) so the document reuses the UI toolkit's
//! undo / incremental-sync / presence machinery without a message layer.
//!
//! The interactive GUI widget (draw/layout/keyboard over winit) is intentionally
//! deferred — it is app polish, not part of CRDT-native file-format support. This
//! crate currently provides the binding and proves the end-to-end integration
//! (undo, two-peer convergence, timeline, codec round-trip) against the real
//! `ui::State` via tests.

pub mod styles;
pub mod widget;
pub mod word_processor;
pub use styles::{StyleProps, StyleSheet};
pub use widget::WordProcessorWidget;
pub use word_processor::WordProcessor;

use automerge::{AutoCommit, ObjId};

/// A document binding: the resolved container ids + the per-replica id
/// generator. Holds no copy of the doc — it operates over a borrowed host
/// `AutoCommit` (the same one `ui::State` owns, snapshots for undo, and syncs).
pub struct EditorDoc {
    pub containers: qdoc::Containers,
    pub ids: qdoc::IdGen,
}

impl EditorDoc {
    /// Bind to a host doc at `root`, initializing the canonical schema if it is
    /// not already present.
    pub fn bind(doc: &mut AutoCommit, root: &ObjId) -> Self {
        let containers =
            qdoc::model::open(doc, root).unwrap_or_else(|_| qdoc::model::init(doc, root));
        let ids = qdoc::IdGen::new(doc.get_actor().to_hex_string());
        Self { containers, ids }
    }

    /// A mutable editing view over the host doc.
    pub fn edit<'a>(&'a mut self, doc: &'a mut AutoCommit) -> qdoc::DocMut<'a> {
        qdoc::DocMut { doc, c: &self.containers, ids: &mut self.ids }
    }

    pub fn blocks(&self, doc: &AutoCommit) -> Vec<String> {
        qdoc::model::blocks_in_order(doc, &self.containers)
    }

    pub fn text(&self, doc: &AutoCommit, id: &str) -> Option<String> {
        qdoc::model::block_text(doc, &self.containers, id)
    }

    pub fn snapshot(&self, doc: &AutoCommit) -> qdoc::DocSnapshot {
        qdoc::model::materialize(doc, &self.containers)
    }
}

// ---- document open / save (format-aware file I/O) -------------------------

use std::path::Path;

/// The file formats the word processor can open and save.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WpFormat {
    Odt,
    Docx,
    Markdown,
    /// PDF — **export only** (rendered via the `docpdf` crate). Importing PDF is
    /// not supported and returns a clear error.
    Pdf,
    /// HTML — **export only** (rendered via the `dochtml` crate). Importing HTML
    /// is not supported and returns a clear error.
    Html,
    /// RTF — **export only** (rendered via the `docrtf` crate). Importing RTF is
    /// not supported and returns a clear error.
    Rtf,
}

impl WpFormat {
    /// Pick a format from a path extension (defaults to Markdown).
    pub fn from_path(path: &Path) -> Self {
        match path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref() {
            Some("odt") => WpFormat::Odt,
            Some("docx") => WpFormat::Docx,
            Some("pdf") => WpFormat::Pdf,
            Some("html") | Some("htm") => WpFormat::Html,
            Some("rtf") => WpFormat::Rtf,
            _ => WpFormat::Markdown,
        }
    }

    pub(crate) fn import(&self, bytes: &[u8], dm: &mut qdoc::DocMut) -> Result<(), qdoc::CodecError> {
        use qdoc::FormatProvider;
        match self {
            WpFormat::Odt => qdoc::OdtCodec.import(bytes, dm),
            WpFormat::Docx => qdoc::DocxCodec.import(bytes, dm),
            WpFormat::Markdown => qdoc::MarkdownCodec.import(bytes, dm),
            WpFormat::Pdf => Err(qdoc::CodecError::Unsupported(
                "opening PDF documents is not supported (PDF is export-only)".to_string(),
            )),
            WpFormat::Html => Err(qdoc::CodecError::Unsupported(
                "opening HTML documents is not supported (HTML is export-only)".to_string(),
            )),
            WpFormat::Rtf => Err(qdoc::CodecError::Unsupported(
                "opening RTF documents is not supported (RTF is export-only)".to_string(),
            )),
        }
    }

    pub(crate) fn export(&self, dr: qdoc::DocRef) -> Result<Vec<u8>, qdoc::CodecError> {
        use qdoc::FormatProvider;
        match self {
            WpFormat::Odt => qdoc::OdtCodec.export(dr),
            WpFormat::Docx => qdoc::DocxCodec.export(dr),
            WpFormat::Markdown => qdoc::MarkdownCodec.export(dr),
            // PDF isn't a `FormatProvider` codec — it renders from the materialized
            // snapshot via `docpdf`. Map its error into the shared `CodecError`.
            WpFormat::Pdf => {
                let snap = dr.materialize();
                docpdf::render_pdf(&snap).map_err(|e| qdoc::CodecError::Export(e.to_string()))
            }
            // HTML/RTF aren't `FormatProvider` codecs either — they render from
            // the materialized snapshot via `dochtml`/`docrtf`, returning UTF-8
            // bytes.
            WpFormat::Html => {
                let snap = dr.materialize();
                Ok(dochtml::render_html(&snap, dochtml::HtmlOptions::default()).into_bytes())
            }
            WpFormat::Rtf => {
                let snap = dr.materialize();
                Ok(docrtf::render_rtf(&snap).into_bytes())
            }
        }
    }
}

fn invalid_data<E: std::fmt::Display>(e: E) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())
}

/// Open `path` into a fresh `ui::State` (holding the canonical document) plus a
/// `WordProcessorWidget` to edit it. The format is chosen by extension.
pub fn open_document(path: &Path) -> std::io::Result<(ui::State, WordProcessorWidget, WpFormat)> {
    let bytes = std::fs::read(path)?;
    let format = WpFormat::from_path(path);
    let mut state = ui::State::new();
    let containers = qdoc::model::init(&mut state.doc, &state.root);
    let mut ids = qdoc::IdGen::new(state.doc.get_actor().to_hex_string());
    {
        let mut dm = qdoc::DocMut { doc: &mut state.doc, c: &containers, ids: &mut ids };
        format.import(&bytes, &mut dm).map_err(invalid_data)?;
    }
    let mut widget = WordProcessorWidget::new();
    widget.set_file(path.to_path_buf(), format);
    Ok((state, widget, format))
}

/// Export the document currently in `state` to `path` using `format`.
pub fn save_document(state: &ui::State, path: &Path, format: WpFormat) -> std::io::Result<()> {
    let containers = qdoc::model::open(&state.doc, &state.root).map_err(invalid_data)?;
    let dr = qdoc::DocRef { doc: &state.doc, c: &containers };
    let bytes = format.export(dr).map_err(|e| std::io::Error::other(e.to_string()))?;
    std::fs::write(path, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use qdoc::{FormatProvider, MarkdownCodec};

    fn first_text(snap: &qdoc::DocSnapshot) -> Option<String> {
        snap.blocks.first().map(|b| b.text.clone())
    }

    /// A qdoc document living in `ui::State.doc` reuses ui's full-snapshot
    /// undo/redo — proving the binding doesn't need its own undo layer.
    #[test]
    fn binding_reuses_ui_state_undo() {
        let mut s = ui::State::new();
        let mut ed = EditorDoc::bind(&mut s.doc, &s.root);
        s.commit_action(None, None); // checkpoint after schema init

        {
            let mut dm = ed.edit(&mut s.doc);
            MarkdownCodec.import(b"# Title\n\nHello\n", &mut dm).unwrap();
        }
        s.commit_action(None, None);
        assert_eq!(first_text(&ed.snapshot(&s.doc)), Some("Title".to_string()));

        s.undo(None, None);
        assert_eq!(ed.snapshot(&s.doc).blocks.len(), 0, "undo reverts to the empty schema");

        s.redo(None, None);
        assert_eq!(first_text(&ed.snapshot(&s.doc)), Some("Title".to_string()));
    }

    /// Two `ui::State` replicas editing concurrently converge through ui's
    /// incremental sync (`save_incremental`/`load_incremental`).
    #[test]
    fn two_peers_converge_through_ui_state() {
        let mut sa = ui::State::new();
        let mut ed_a = EditorDoc::bind(&mut sa.doc, &sa.root);
        sa.commit_action(None, None);

        // Fork B from A's serialized state, with a distinct actor id.
        let bytes = sa.save();
        let mut sb = ui::State::load(&bytes).unwrap();
        sb.doc.set_actor(automerge::ActorId::random());
        let mut ed_b = EditorDoc::bind(&mut sb.doc, &sb.root);

        {
            let mut dm = ed_a.edit(&mut sa.doc);
            let id = dm.insert_block(None, "paragraph");
            dm.splice_text(&id, 0, 0, "A-edit");
        }
        {
            let mut dm = ed_b.edit(&mut sb.doc);
            let id = dm.insert_block(None, "paragraph");
            dm.splice_text(&id, 0, 0, "B-edit");
        }

        let da = sa.save_incremental();
        let db = sb.save_incremental();
        sa.load_incremental(&db).unwrap();
        sb.load_incremental(&da).unwrap();

        let snap_a = ed_a.snapshot(&sa.doc);
        let snap_b = ed_b.snapshot(&sb.doc);
        assert_eq!(snap_a, snap_b, "peers converge to identical documents");
        assert_eq!(snap_a.blocks.len(), 2, "both concurrent edits are present");
    }

    /// Saving to a `*.pdf` path routes through `docpdf` and writes a valid PDF
    /// (the bytes start with the `%PDF` signature). PDF is export-only.
    #[test]
    fn save_document_to_pdf_writes_valid_pdf() {
        let mut s = ui::State::new();
        let mut ed = EditorDoc::bind(&mut s.doc, &s.root);
        {
            let mut dm = ed.edit(&mut s.doc);
            MarkdownCodec.import(b"# Title\n\nHello world\n", &mut dm).unwrap();
        }

        let dir = std::env::temp_dir();
        let path = dir.join(format!("editor-pdf-test-{}.pdf", std::process::id()));
        assert_eq!(WpFormat::from_path(&path), WpFormat::Pdf, ".pdf extension picks the Pdf format");

        save_document(&s, &path, WpFormat::Pdf).expect("save to pdf");
        let bytes = std::fs::read(&path).expect("read back the pdf");
        let _ = std::fs::remove_file(&path);
        assert!(bytes.starts_with(b"%PDF"), "output begins with the PDF signature: {:?}", &bytes[..bytes.len().min(8)]);
    }

    /// PDF import is unsupported and surfaces a clear error.
    #[test]
    fn pdf_import_is_unsupported() {
        let mut s = ui::State::new();
        let containers = qdoc::model::init(&mut s.doc, &s.root);
        let mut ids = qdoc::IdGen::new(s.doc.get_actor().to_hex_string());
        let mut dm = qdoc::DocMut { doc: &mut s.doc, c: &containers, ids: &mut ids };
        let err = WpFormat::Pdf.import(b"%PDF-1.7", &mut dm).unwrap_err();
        assert!(matches!(err, qdoc::CodecError::Unsupported(_)), "got {err:?}");
    }

    /// Saving to a `*.html` path routes through `dochtml` and writes a complete
    /// HTML document. HTML is export-only.
    #[test]
    fn save_document_to_html_writes_html() {
        let mut s = ui::State::new();
        let mut ed = EditorDoc::bind(&mut s.doc, &s.root);
        {
            let mut dm = ed.edit(&mut s.doc);
            MarkdownCodec.import(b"# Title\n\nHello world\n", &mut dm).unwrap();
        }

        let dir = std::env::temp_dir();
        let path = dir.join(format!("editor-html-test-{}.html", std::process::id()));
        assert_eq!(WpFormat::from_path(&path), WpFormat::Html, ".html extension picks the Html format");

        save_document(&s, &path, WpFormat::Html).expect("save to html");
        let bytes = std::fs::read(&path).expect("read back the html");
        let _ = std::fs::remove_file(&path);
        let text = String::from_utf8(bytes).expect("html output is valid UTF-8");
        let lower = text.to_ascii_lowercase();
        assert!(
            lower.contains("<!doctype html") || lower.contains("<html"),
            "output is an HTML document: {:?}",
            &text[..text.len().min(64)]
        );
    }

    /// Saving to a `*.rtf` path routes through `docrtf` and writes a valid RTF
    /// document (the bytes start with the `{\rtf` signature). RTF is export-only.
    #[test]
    fn save_document_to_rtf_writes_rtf() {
        let mut s = ui::State::new();
        let mut ed = EditorDoc::bind(&mut s.doc, &s.root);
        {
            let mut dm = ed.edit(&mut s.doc);
            MarkdownCodec.import(b"# Title\n\nHello world\n", &mut dm).unwrap();
        }

        let dir = std::env::temp_dir();
        let path = dir.join(format!("editor-rtf-test-{}.rtf", std::process::id()));
        assert_eq!(WpFormat::from_path(&path), WpFormat::Rtf, ".rtf extension picks the Rtf format");

        save_document(&s, &path, WpFormat::Rtf).expect("save to rtf");
        let bytes = std::fs::read(&path).expect("read back the rtf");
        let _ = std::fs::remove_file(&path);
        assert!(
            bytes.starts_with(b"{\\rtf"),
            "output begins with the RTF signature: {:?}",
            &bytes[..bytes.len().min(8)]
        );
    }

    /// `WpFormat::from_path` maps the export-only extensions to their formats,
    /// including the `.htm` alias for HTML.
    #[test]
    fn from_path_maps_html_htm_rtf() {
        assert_eq!(WpFormat::from_path(Path::new("doc.html")), WpFormat::Html);
        assert_eq!(WpFormat::from_path(Path::new("doc.htm")), WpFormat::Html);
        assert_eq!(WpFormat::from_path(Path::new("doc.HTML")), WpFormat::Html, "case-insensitive");
        assert_eq!(WpFormat::from_path(Path::new("doc.rtf")), WpFormat::Rtf);
        assert_eq!(WpFormat::from_path(Path::new("doc.RTF")), WpFormat::Rtf, "case-insensitive");
    }

    /// HTML and RTF import are unsupported and surface a clear error.
    #[test]
    fn html_and_rtf_import_is_unsupported() {
        let mut s = ui::State::new();
        let containers = qdoc::model::init(&mut s.doc, &s.root);
        let mut ids = qdoc::IdGen::new(s.doc.get_actor().to_hex_string());
        let mut dm = qdoc::DocMut { doc: &mut s.doc, c: &containers, ids: &mut ids };
        let html_err = WpFormat::Html.import(b"<html></html>", &mut dm).unwrap_err();
        assert!(matches!(html_err, qdoc::CodecError::Unsupported(_)), "got {html_err:?}");
        let rtf_err = WpFormat::Rtf.import(b"{\\rtf1}", &mut dm).unwrap_err();
        assert!(matches!(rtf_err, qdoc::CodecError::Unsupported(_)), "got {rtf_err:?}");
    }

    /// The version timeline + time-travel work over a bound document.
    #[test]
    fn timeline_over_bound_doc() {
        let mut s = ui::State::new();
        let mut ed = EditorDoc::bind(&mut s.doc, &s.root);
        let p = {
            let mut dm = ed.edit(&mut s.doc);
            let id = dm.insert_block(None, "paragraph");
            dm.splice_text(&id, 0, 0, "hello");
            id
        };
        s.doc.commit(); // host commits each action (borrowed DocMut no longer auto-commits)
        {
            let mut dm = ed.edit(&mut s.doc);
            dm.splice_text(&p, 5, 0, " world");
        }
        s.doc.commit();

        let vers = qdoc::history::versions(&mut s.doc);
        assert!(vers.len() >= 2, "multiple authored changes");
        let last = vers.len() - 1;
        assert_eq!(
            qdoc::history::materialize_at(&mut s.doc, last).unwrap().blocks[0].text,
            "hello world"
        );
        let saw_hello = (0..vers.len()).any(|i| {
            qdoc::history::materialize_at(&mut s.doc, i)
                .ok()
                .and_then(|sn| sn.blocks.first().map(|b| b.text == "hello"))
                .unwrap_or(false)
        });
        assert!(saw_hello, "timeline should expose the intermediate 'hello' state");
    }
}
