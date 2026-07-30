//! `editor` — binds a `qdoc` canonical rich-text document into a host automerge
//! doc (e.g. `ui::State.doc`) so the document reuses the UI toolkit's
//! undo / incremental-sync / presence machinery without a message layer.
//!
//! The interactive GUI widget (draw/layout/keyboard over winit) is intentionally
//! deferred — it is app polish, not part of CRDT-native file-format support. This
//! crate currently provides the binding and proves the end-to-end integration
//! (undo, two-peer convergence, timeline, codec round-trip) against the real
//! `ui::State` via tests.

pub mod actions;
pub mod history_panel;
pub mod i18n;
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
    /// PDF — **export only** (rendered via the `qdoc-pdf` crate). Importing PDF is
    /// not supported and returns a clear error.
    Pdf,
    /// HTML — export via the `qdoc-html` crate; **import** via `qdoc::HtmlCodec` (so
    /// `.html`/`.htm` files can be opened as well as saved).
    Html,
    /// RTF — export via the `qdoc-rtf` crate; **import** via `qdoc::RtfCodec` (so
    /// `.rtf` files can be opened as well as saved).
    Rtf,
    /// EPUB — **export only** (rendered via the `qdoc-epub` crate). Importing EPUB
    /// is not supported and returns a clear error.
    Epub,
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
            Some("epub") => WpFormat::Epub,
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
            // HTML import is handled by the `qdoc::HtmlCodec` codec (export still
            // routes through `qdoc-html` below).
            WpFormat::Html => qdoc::HtmlCodec.import(bytes, dm),
            // RTF import is handled by the `qdoc::RtfCodec` codec (export still
            // routes through `qdoc-rtf` below).
            WpFormat::Rtf => qdoc::RtfCodec.import(bytes, dm),
            WpFormat::Epub => Err(qdoc::CodecError::Unsupported(
                "opening EPUB documents is not supported (EPUB is export-only)".to_string(),
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
            // snapshot via `qdoc-pdf`. Map its error into the shared `CodecError`.
            WpFormat::Pdf => {
                let snap = dr.materialize();
                qdoc_pdf::render_pdf(&snap).map_err(|e| qdoc::CodecError::Export(e.to_string()))
            }
            // HTML/RTF aren't `FormatProvider` codecs either — they render from
            // the materialized snapshot via `qdoc-html`/`qdoc-rtf`, returning UTF-8
            // bytes.
            WpFormat::Html => {
                let snap = dr.materialize();
                Ok(qdoc_html::render_html(&snap, qdoc_html::HtmlOptions::default()).into_bytes())
            }
            WpFormat::Rtf => {
                let snap = dr.materialize();
                Ok(qdoc_rtf::render_rtf(&snap).into_bytes())
            }
            // EPUB isn't a `FormatProvider` codec either — it assembles an OCF
            // ZIP container from the materialized snapshot via `qdoc-epub`. Map its
            // error into the shared `CodecError`.
            WpFormat::Epub => {
                let snap = dr.materialize();
                qdoc_epub::render_epub(&snap).map_err(|e| qdoc::CodecError::Export(e.to_string()))
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
    // If a previous session crashed before saving this document, a recovery copy
    // is still on disk — surface the crash-recovery banner (Restore / Discard).
    widget.check_for_recovery();
    Ok((state, widget, format))
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

    /// The localized, user-facing title shown in the "New ▾" template picker.
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

/// Start a fresh, untitled document seeded from a bundled `Template`.
///
/// Mirrors [`open_document`] but imports the template's embedded Markdown string
/// instead of reading a file. The returned widget has **no path** set — the
/// document is untitled and will prompt for a destination on first save. The
/// format is always [`WpFormat::Markdown`] (the templates are Markdown), though
/// the user is free to save under any export format afterwards.
pub fn new_from_template(t: Template) -> (ui::State, WordProcessorWidget, WpFormat) {
    let format = WpFormat::Markdown;
    let mut state = ui::State::new();
    let containers = qdoc::model::init(&mut state.doc, &state.root);
    let mut ids = qdoc::IdGen::new(state.doc.get_actor().to_hex_string());
    {
        let mut dm = qdoc::DocMut { doc: &mut state.doc, c: &containers, ids: &mut ids };
        // Templates are bundled and validated by tests, so import cannot fail in
        // practice; if a malformed template ever shipped we'd rather surface it
        // loudly than silently hand back an empty document.
        format
            .import(t.content().as_bytes(), &mut dm)
            .expect("bundled template imports cleanly");
    }
    // Intentionally do NOT call `widget.set_file(..)`: leaving the path unset
    // keeps the document untitled (the widget falls back to `Untitled.md` on
    // save). The format is returned so callers can wire it up if they wish.
    let widget = WordProcessorWidget::new();
    (state, widget, format)
}

/// Export the document currently in `state` to `path` using `format`.
pub fn save_document(state: &ui::State, path: &Path, format: WpFormat) -> std::io::Result<()> {
    let containers = qdoc::model::open(&state.doc, &state.root).map_err(invalid_data)?;
    let dr = qdoc::DocRef { doc: &state.doc, c: &containers };
    let bytes = format.export(dr).map_err(|e| std::io::Error::other(e.to_string()))?;
    std::fs::write(path, bytes)
}

// ---- autosave / crash-recovery -------------------------------------------

/// Best-effort autosave / crash-recovery store.
///
/// While a document is being edited the word processor periodically writes a
/// **recovery copy** (always Markdown — lossless for the canonical model and
/// trivially re-importable) to a stable per-document path under the user's state
/// directory. If the app crashes or is killed before the user saves, the next
/// session that opens the same document finds the (newer) recovery copy and can
/// offer to restore it. On a successful real save the recovery copy is deleted.
///
/// Everything here is **best-effort**: an unwritable state directory never
/// panics and never blocks editing — recovery just silently does nothing.
pub mod recovery {
    use std::path::{Path, PathBuf};

    /// Environment override for the recovery base directory. Set by tests (and
    /// available to callers that want to relocate the store) so recovery never
    /// touches the real `$HOME` / `$XDG_STATE_HOME` during a test run.
    pub const RECOVERY_BASE_ENV: &str = "QOS_WP_RECOVERY_DIR";

    /// The directory recovery copies live in. Resolution order:
    /// 1. `$QOS_WP_RECOVERY_DIR` (test / explicit override), used verbatim;
    /// 2. `$XDG_STATE_HOME/qos/word-processor/recovery`;
    /// 3. `$HOME/.local/state/qos/word-processor/recovery`;
    /// 4. `./.qos-wp-recovery` as a last resort (no HOME at all).
    pub fn base_dir() -> PathBuf {
        if let Some(dir) = std::env::var_os(RECOVERY_BASE_ENV) {
            return PathBuf::from(dir);
        }
        let state_home = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .filter(|p| !p.as_os_str().is_empty())
            .or_else(|| {
                std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("state"))
            });
        match state_home {
            Some(p) => p.join("qos").join("word-processor").join("recovery"),
            None => PathBuf::from(".qos-wp-recovery"),
        }
    }

    /// A small, dependency-free, deterministic 64-bit FNV-1a hash. Used to derive
    /// a filesystem-safe, **stable** recovery filename from a document's path
    /// (stable across runs and Rust versions, unlike `DefaultHasher`).
    fn fnv1a(bytes: &[u8]) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for &b in bytes {
            h ^= b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01B3);
        }
        h
    }

    /// The recovery file path for a document. Stable for a given target: the same
    /// document path always maps to the same recovery file, so a later session
    /// can find it. An **untitled** document (no path) maps to a single shared
    /// `untitled` slot.
    ///
    /// The on-disk name is `<16-hex-hash>.md`; the hash is over the document's
    /// absolute path string (or the literal `:untitled:` sentinel), which keeps
    /// the name short and filesystem-safe regardless of the original path.
    pub fn recovery_path_for(doc_path: Option<&Path>) -> PathBuf {
        let key = match doc_path {
            Some(p) => {
                // Canonicalise lightly: prefer an absolute path so two ways of
                // naming the same file collide on one recovery slot. We do NOT
                // require the file to exist (it may be brand-new), so fall back
                // to the path as-given when it can't be made absolute.
                let abs = std::fs::canonicalize(p).unwrap_or_else(|_| {
                    if p.is_absolute() {
                        p.to_path_buf()
                    } else {
                        std::env::current_dir().map(|d| d.join(p)).unwrap_or_else(|_| p.to_path_buf())
                    }
                });
                abs.to_string_lossy().into_owned()
            }
            None => ":untitled:".to_string(),
        };
        base_dir().join(format!("{:016x}.md", fnv1a(key.as_bytes())))
    }

    /// Write a recovery copy (Markdown bytes) for `doc_path`. Best-effort: creates
    /// the base directory if needed and swallows any I/O error. Returns the path
    /// it wrote to on success (for the caller's bookkeeping / tests).
    pub fn write(doc_path: Option<&Path>, markdown: &[u8]) -> Option<PathBuf> {
        let path = recovery_path_for(doc_path);
        if let Some(parent) = path.parent() {
            if std::fs::create_dir_all(parent).is_err() {
                return None;
            }
        }
        std::fs::write(&path, markdown).ok().map(|_| path)
    }

    /// Read back a recovery copy's Markdown bytes for `doc_path`, if one exists.
    pub fn read(doc_path: Option<&Path>) -> Option<Vec<u8>> {
        std::fs::read(recovery_path_for(doc_path)).ok()
    }

    /// Whether a recovery copy currently exists for `doc_path`.
    pub fn exists(doc_path: Option<&Path>) -> bool {
        recovery_path_for(doc_path).exists()
    }

    /// Delete the recovery copy for `doc_path` (best-effort; absence is success).
    pub fn clear(doc_path: Option<&Path>) {
        let _ = std::fs::remove_file(recovery_path_for(doc_path));
    }

    /// Render the document currently in `state` to Markdown bytes for a recovery
    /// copy. Markdown is the canonical lossless interchange format for the model,
    /// so this is what `WordProcessorWidget` writes regardless of the document's
    /// on-disk save format. Returns `None` only if the document has no schema yet.
    pub fn markdown_bytes(state: &ui::State) -> Option<Vec<u8>> {
        let containers = qdoc::model::open(&state.doc, &state.root).ok()?;
        let dr = qdoc::DocRef { doc: &state.doc, c: &containers };
        crate::WpFormat::Markdown.export(dr).ok()
    }
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

    /// Saving to a `*.pdf` path routes through `qdoc-pdf` and writes a valid PDF
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

    /// Saving to a `*.html` path routes through `qdoc-html` and writes a complete
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

    /// Saving to a `*.rtf` path routes through `qdoc-rtf` and writes a valid RTF
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

    /// Saving to a `*.epub` path routes through `qdoc-epub` and writes a valid
    /// EPUB OCF container: the bytes are a ZIP archive (`PK` magic) whose first
    /// entry is the literal `mimetype` member. EPUB is export-only.
    #[test]
    fn save_document_to_epub_writes_epub() {
        let mut s = ui::State::new();
        let mut ed = EditorDoc::bind(&mut s.doc, &s.root);
        {
            let mut dm = ed.edit(&mut s.doc);
            MarkdownCodec.import(b"# Title\n\nHello world\n", &mut dm).unwrap();
        }

        let dir = std::env::temp_dir();
        let path = dir.join(format!("editor-epub-test-{}.epub", std::process::id()));
        assert_eq!(WpFormat::from_path(&path), WpFormat::Epub, ".epub extension picks the Epub format");

        save_document(&s, &path, WpFormat::Epub).expect("save to epub");
        let bytes = std::fs::read(&path).expect("read back the epub");
        let _ = std::fs::remove_file(&path);

        // ZIP magic: every EPUB is an OCF ZIP container.
        assert!(
            bytes.starts_with(b"PK"),
            "output begins with the ZIP signature: {:?}",
            &bytes[..bytes.len().min(8)]
        );
        // OCF requires the first archive entry to be the literal `mimetype`
        // member (stored uncompressed). The local-file-header filename starts at
        // offset 30; assert it is `mimetype`.
        assert!(bytes.len() >= 38, "epub is at least one local file header long");
        assert_eq!(
            &bytes[30..38],
            b"mimetype",
            "first zip entry is the OCF `mimetype` member"
        );
    }

    /// EPUB import is unsupported and surfaces a clear error.
    #[test]
    fn epub_import_is_unsupported() {
        let mut s = ui::State::new();
        let containers = qdoc::model::init(&mut s.doc, &s.root);
        let mut ids = qdoc::IdGen::new(s.doc.get_actor().to_hex_string());
        let mut dm = qdoc::DocMut { doc: &mut s.doc, c: &containers, ids: &mut ids };
        let err = WpFormat::Epub.import(b"PK\x03\x04", &mut dm).unwrap_err();
        assert!(matches!(err, qdoc::CodecError::Unsupported(_)), "got {err:?}");
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
        assert_eq!(WpFormat::from_path(Path::new("doc.epub")), WpFormat::Epub);
        assert_eq!(WpFormat::from_path(Path::new("doc.EPUB")), WpFormat::Epub, "case-insensitive");
    }

    /// `WpFormat::Html.import` of a small HTML document produces the expected
    /// first heading (HTML import is now wired to `qdoc::HtmlCodec`).
    #[test]
    fn html_import_opens_first_heading() {
        let mut s = ui::State::new();
        let containers = qdoc::model::init(&mut s.doc, &s.root);
        let mut ids = qdoc::IdGen::new(s.doc.get_actor().to_hex_string());
        {
            let mut dm = qdoc::DocMut { doc: &mut s.doc, c: &containers, ids: &mut ids };
            WpFormat::Html
                .import(b"<h1>Opened from HTML</h1><p>body</p>", &mut dm)
                .expect("html import succeeds");
        }
        let snap = qdoc::model::materialize(&s.doc, &containers);
        let first = snap.blocks.first().expect("html import yields at least one block");
        assert_eq!(first.kind, "heading", "first block is a heading: {snap:?}");
        assert_eq!(first.text, "Opened from HTML", "first heading text: {snap:?}");
    }

    /// `WpFormat::Rtf.import` of a small `.rtf` document produces the expected
    /// first paragraph (RTF import is wired to `qdoc::RtfCodec`).
    #[test]
    fn rtf_import_opens_a_paragraph() {
        let mut s = ui::State::new();
        let containers = qdoc::model::init(&mut s.doc, &s.root);
        let mut ids = qdoc::IdGen::new(s.doc.get_actor().to_hex_string());
        {
            let mut dm = qdoc::DocMut { doc: &mut s.doc, c: &containers, ids: &mut ids };
            WpFormat::Rtf
                .import(b"{\\rtf1\\ansi\\deff0\n{\\pard\\fs22 Opened from RTF\\par}}", &mut dm)
                .expect("rtf import succeeds");
        }
        let snap = qdoc::model::materialize(&s.doc, &containers);
        let first = snap.blocks.first().expect("rtf import yields at least one block");
        assert_eq!(first.text, "Opened from RTF", "first paragraph text: {snap:?}");
    }

    /// Full round-trip: render a Markdown-built document to `.rtf` via `qdoc-rtf`
    /// (the export path), re-open it through `WpFormat::Rtf.import` (the new
    /// import path), and assert the text and a bold run survive.
    #[test]
    fn rtf_export_then_import_roundtrips_text_and_bold() {
        // Source document with a bold word.
        let mut s = ui::State::new();
        let mut ed = EditorDoc::bind(&mut s.doc, &s.root);
        {
            let mut dm = ed.edit(&mut s.doc);
            MarkdownCodec.import(b"Hello **world** today\n", &mut dm).unwrap();
        }

        // Export to RTF bytes via the editor's export path (qdoc-rtf).
        let containers = qdoc::model::open(&s.doc, &s.root).unwrap();
        let dr = qdoc::DocRef { doc: &s.doc, c: &containers };
        let rtf = WpFormat::Rtf.export(dr).expect("rtf export");

        // Re-import those bytes into a fresh document via the import path.
        let mut s2 = ui::State::new();
        let containers2 = qdoc::model::init(&mut s2.doc, &s2.root);
        let mut ids2 = qdoc::IdGen::new(s2.doc.get_actor().to_hex_string());
        {
            let mut dm = qdoc::DocMut { doc: &mut s2.doc, c: &containers2, ids: &mut ids2 };
            WpFormat::Rtf.import(&rtf, &mut dm).expect("rtf import");
        }
        let snap = qdoc::model::materialize(&s2.doc, &containers2);
        let p = snap.blocks.iter().find(|b| b.kind == "paragraph").expect("a paragraph survives");
        assert_eq!(p.text, "Hello world today", "text round-trips: {snap:?}");
        let strong = p.marks.iter().find(|m| m.name == "strong").expect("bold run survives");
        assert_eq!(&p.text[strong.start..strong.end], "world", "bold covers the right run: {:?}", p.marks);
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

    // ---- document templates -----------------------------------------------

    /// `new_from_template` seeds a fresh state whose materialized document opens
    /// with the template's first heading. (`Report` begins with "Report Title".)
    #[test]
    fn new_from_template_report_seeds_expected_heading() {
        let (state, widget, format) = new_from_template(Template::Report);
        assert_eq!(format, WpFormat::Markdown, "templates are imported as Markdown");

        let containers = qdoc::model::open(&state.doc, &state.root)
            .expect("template-seeded state has the canonical schema");
        let snap = qdoc::model::materialize(&state.doc, &containers);
        let first = snap.blocks.first().expect("report template has at least one block");
        assert_eq!(first.kind, "heading", "the report opens with a heading");
        assert_eq!(first.text, "Report Title", "the report's first heading text");

        // Untitled / unbound: a freshly returned widget hasn't run its `sync`
        // pass yet, so it has no bound containers and no file path — the
        // document will prompt for a destination on first save.
        assert!(
            widget.model().containers.is_none(),
            "a fresh template widget is unbound until the first sync"
        );
    }

    /// `Template::all()` enumerates every variant exactly once, in menu order.
    #[test]
    fn template_all_lists_every_variant() {
        let all = Template::all();
        assert_eq!(
            all,
            &[
                Template::Blank,
                Template::Letter,
                Template::Report,
                Template::Resume,
                Template::MeetingNotes,
            ]
        );
        // Titles are present and non-empty for each.
        for t in all {
            assert!(!t.title().is_empty(), "{t:?} has a title");
        }
    }

    /// Every bundled template has non-empty content and imports without error
    /// into a fresh document with at least one block.
    #[test]
    fn every_template_imports_non_empty() {
        for &t in Template::all() {
            assert!(!t.content().trim().is_empty(), "{t:?} content is non-empty");

            // Import path mirrors `new_from_template` but asserts on each variant.
            let mut s = ui::State::new();
            let containers = qdoc::model::init(&mut s.doc, &s.root);
            let mut ids = qdoc::IdGen::new(s.doc.get_actor().to_hex_string());
            {
                let mut dm = qdoc::DocMut { doc: &mut s.doc, c: &containers, ids: &mut ids };
                WpFormat::Markdown
                    .import(t.content().as_bytes(), &mut dm)
                    .unwrap_or_else(|e| panic!("{t:?} imports cleanly: {e:?}"));
            }
            let snap = qdoc::model::materialize(&s.doc, &containers);
            assert!(!snap.blocks.is_empty(), "{t:?} produces at least one block");
        }
    }

    /// The public `new_from_template` entrypoint builds a usable document for
    /// every variant (not just `Report`).
    #[test]
    fn new_from_template_builds_every_variant() {
        for &t in Template::all() {
            let (state, _widget, format) = new_from_template(t);
            assert_eq!(format, WpFormat::Markdown);
            let containers = qdoc::model::open(&state.doc, &state.root).unwrap();
            let snap = qdoc::model::materialize(&state.doc, &containers);
            assert!(!snap.blocks.is_empty(), "{t:?} yields a non-empty document");
        }
    }

    // ---- autosave / crash-recovery -----------------------------------------

    /// Serialises the tests that mutate the process-global `RECOVERY_BASE_ENV`
    /// (env vars are process-wide; cargo runs `#[test]`s in parallel by default).
    /// Each such test holds this for its whole body so the var is never observed
    /// half-set by a sibling. `lock().unwrap_or_else(|e| e.into_inner())` keeps a
    /// panicking test from poisoning the suite.
    static RECOVERY_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// A unique temp directory for a recovery test, cleaned up by the test (the
    /// recovery store never touches the real `$HOME` thanks to the base override).
    fn temp_recovery_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "editor-recovery-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// `recovery_path_for` is **stable** for a given document path: the same path
    /// (in any equivalent spelling) maps to the same recovery file, and distinct
    /// documents map to distinct files. The untitled slot is a single fixed path.
    #[test]
    fn recovery_path_is_stable_per_document() {
        let _guard = RECOVERY_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = temp_recovery_dir("stable");
        std::env::set_var(recovery::RECOVERY_BASE_ENV, &dir);

        let doc = std::path::Path::new("/home/u/My Notes.md");
        let p1 = recovery::recovery_path_for(Some(doc));
        let p2 = recovery::recovery_path_for(Some(doc));
        assert_eq!(p1, p2, "the same document maps to the same recovery file");
        assert!(p1.starts_with(&dir), "recovery file lives under the override dir");

        let other = recovery::recovery_path_for(Some(std::path::Path::new("/home/u/Other.md")));
        assert_ne!(p1, other, "different documents map to different recovery files");

        let u1 = recovery::recovery_path_for(None);
        let u2 = recovery::recovery_path_for(None);
        assert_eq!(u1, u2, "the untitled slot is a single stable path");
        assert_ne!(u1, p1, "untitled and titled documents don't collide");

        std::env::remove_var(recovery::RECOVERY_BASE_ENV);
    }

    /// `recovery::write` materializes a recovery copy under the (overridden) base
    /// dir; `read`/`exists` see it; `clear` removes it. End-to-end on the real
    /// filesystem (a temp dir), best-effort throughout.
    #[test]
    fn recovery_write_read_clear_roundtrip() {
        let _guard = RECOVERY_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = temp_recovery_dir("rwc");
        std::env::set_var(recovery::RECOVERY_BASE_ENV, &dir);
        let doc = std::path::Path::new("/tmp/whatever/doc.md");

        assert!(!recovery::exists(Some(doc)), "no recovery file before any write");
        let written = recovery::write(Some(doc), b"# Recovered\n\nbody\n").expect("write a recovery copy");
        assert!(written.exists(), "the written recovery file is on disk");
        assert!(recovery::exists(Some(doc)), "exists() sees the recovery file");
        assert_eq!(
            recovery::read(Some(doc)).expect("read back"),
            b"# Recovered\n\nbody\n",
            "read back the exact bytes written"
        );

        recovery::clear(Some(doc));
        assert!(!recovery::exists(Some(doc)), "clear() removes the recovery file");

        let _ = std::fs::remove_dir_all(&dir);
        std::env::remove_var(recovery::RECOVERY_BASE_ENV);
    }

    /// `recovery::markdown_bytes` renders the live document to recovery Markdown
    /// that re-imports to the same first heading — proving the recovery copy is a
    /// faithful, restorable snapshot of the document.
    #[test]
    fn recovery_markdown_roundtrips_through_import() {
        let mut s = ui::State::new();
        let mut ed = EditorDoc::bind(&mut s.doc, &s.root);
        {
            let mut dm = ed.edit(&mut s.doc);
            MarkdownCodec.import(b"# Draft Title\n\nUnsaved paragraph\n", &mut dm).unwrap();
        }
        let md = recovery::markdown_bytes(&s).expect("render recovery markdown");

        let mut s2 = ui::State::new();
        let containers = qdoc::model::init(&mut s2.doc, &s2.root);
        let mut ids = qdoc::IdGen::new(s2.doc.get_actor().to_hex_string());
        {
            let mut dm = qdoc::DocMut { doc: &mut s2.doc, c: &containers, ids: &mut ids };
            MarkdownCodec.import(&md, &mut dm).expect("re-import recovery markdown");
        }
        let snap = qdoc::model::materialize(&s2.doc, &containers);
        assert_eq!(first_text(&snap).as_deref(), Some("Draft Title"), "recovery copy restores the heading");
    }
}
