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
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        clippy::float_cmp,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

pub mod actions;
mod compact_swatch_palette;
mod find_bar;
pub mod framework;
pub mod history_panel;
pub mod i18n;
mod outline_pane;
mod paint_context;
mod recovery_banner;
mod review_overlay;
mod ruler;
pub mod semantic_read;
mod style_color_picker;
mod style_menu;
mod template_picker;
mod text_offsets;
mod unsaved_changes;
mod zoom_control;
pub use ui::num;
pub mod styles;
pub mod widget;
pub mod word_processor;
pub use styles::{StyleProps, StyleSheet};
pub use widget::WordProcessorWidget;

/// A new, empty document's editor, keeping embedded media in the app's
/// content store.
pub fn app_root() -> Box<dyn ui::widget::Widget> {
    let mut widget = WordProcessorWidget::new();
    widget
        .model_mut()
        .set_content_store(tilo_content::ContentStore::for_app());
    Box::new(widget)
}

pub use word_processor::WordProcessor;

use automerge::{AutoCommit, ObjId};

/// A document binding: the resolved container ids + the per-replica id
/// generator. Holds no copy of the doc — it operates over a borrowed host
/// `AutoCommit` (the same one `ui::State` owns, snapshots for undo, and syncs).
pub struct EditorDoc {
    pub containers: tilo_doc::Containers,
    pub ids: tilo_doc::IdGen,
}

impl EditorDoc {
    /// Bind to a host doc at `root`, initializing the canonical schema if it is
    /// not already present.
    pub fn bind(doc: &mut AutoCommit, root: &ObjId) -> Self {
        let containers =
            tilo_doc::model::open(doc, root).unwrap_or_else(|_| tilo_doc::model::init(doc, root));
        let ids = tilo_doc::IdGen::new(doc.get_actor().to_hex_string());
        Self { containers, ids }
    }

    /// Private engine handle for import/seed helpers. Interactive edits go
    /// through [`crate::actions::RichTextAction`] / the framework dispatcher —
    /// not this API.
    #[allow(
        dead_code,
        reason = "seed/import helpers and lib tests bind through this handle"
    )]
    pub(crate) fn edit<'a>(&'a mut self, doc: &'a mut AutoCommit) -> tilo_doc::DocMut<'a> {
        tilo_doc::DocMut {
            doc,
            c: &self.containers,
            ids: &mut self.ids,
        }
    }

    pub fn blocks(&self, doc: &AutoCommit) -> Vec<String> {
        tilo_doc::model::blocks_in_order(doc, &self.containers)
    }

    pub fn text(&self, doc: &AutoCommit, id: &str) -> Option<String> {
        tilo_doc::model::block_text(doc, &self.containers, id)
    }

    pub fn snapshot(&self, doc: &AutoCommit) -> tilo_doc::DocSnapshot {
        tilo_doc::model::materialize(doc, &self.containers)
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

    /// Project `bytes` into `dm`; embedded images land in `inputs`, which the caller keeps in its
    /// content store.
    pub(crate) fn import(
        self,
        bytes: &[u8],
        dm: &mut tilo_doc::DocMut,
        inputs: &mut tilo_content::ContentInputs,
    ) -> Result<(), tilo_doc_codecs::CodecError> {
        use tilo_doc_codecs::FormatProvider;
        match self {
            WpFormat::Odt => tilo_doc_codecs::OdtCodec.import(bytes, dm, inputs),
            WpFormat::Docx => tilo_doc_codecs::DocxCodec.import(bytes, dm, inputs),
            WpFormat::Markdown => tilo_doc_codecs::MarkdownCodec.import(bytes, dm, inputs),
            WpFormat::Text => {
                let text = std::str::from_utf8(bytes)
                    .map_err(|e| tilo_doc_codecs::CodecError::Parse(e.to_string()))?;
                dm.bulk(|dm| {
                    let mut previous: Option<String> = None;
                    for line in text.lines() {
                        let block = dm.insert_block(previous.as_deref(), "paragraph");
                        dm.splice_text(&block, 0, 0, line);
                        previous = Some(block);
                    }
                });
                Ok(())
            }
            WpFormat::Pdf => Err(tilo_doc_codecs::CodecError::Unsupported(
                "opening PDF documents is not supported (PDF is export-only)".to_string(),
            )),
            // HTML import is handled by the `tilo_doc_codecs::HtmlCodec` codec (export still
            // routes through `tilo_doc_codecs::export_html` below).
            WpFormat::Html => tilo_doc_codecs::HtmlCodec.import(bytes, dm, inputs),
            // RTF import is handled by the `tilo_doc_codecs::RtfCodec` codec (export still
            // routes through `tilo_doc_codecs::export_rtf` below).
            WpFormat::Rtf => tilo_doc_codecs::RtfCodec.import(bytes, dm, inputs),
            WpFormat::Epub => Err(tilo_doc_codecs::CodecError::Unsupported(
                "opening EPUB documents is not supported (EPUB is export-only)".to_string(),
            )),
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

/// Make the document as just imported the start of its undo history: the first undo takes back
/// the first edit, not the file.
///
/// The import into the fresh document commits its content as changes (see `tilo_doc::DocMut::bulk`),
/// and the undo log brackets each action from the heads of the previous boundary, so without a
/// boundary here the first edit's entry would span the whole import.
fn start_history_here(state: &mut ui::State) {
    let mut doc = std::mem::take(&mut state.doc);
    let _ = ui::tilo_resource::history::commit_stamped(&mut doc);
    state.history_mut().rebase(&mut doc);
    state.doc = doc;
}

fn invalid_data<E: std::fmt::Display>(e: E) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())
}

/// Open `path` into a fresh `ui::State` (holding the canonical document) plus a
/// `WordProcessorWidget` to edit it. The format is chosen by extension.
///
/// # Errors
///
/// Fails if `path` cannot be read, or if the codec for its format rejects the
/// bytes (a corrupt archive, or an export-only format such as PDF or EPUB).
pub fn open_document(
    path: &Path,
    content: tilo_content::ContentStore,
) -> std::io::Result<(ui::State, WordProcessorWidget, WpFormat)> {
    let records = ui::RecordSet::for_local_files("so.tilo.word-processor").map_err(invalid_data)?;
    open_document_with_records(path, &records, content)
}

/// Open through the launch owner's retained file scope. Required history is
/// consumed before importing the selected file; the file remains its sole
/// local writer.
///
/// # Errors
///
/// Fails if the retained file history cannot be read, if `path` cannot be read,
/// or if the codec for its format rejects the bytes.
pub fn open_document_with_records(
    path: &Path,
    records: &ui::RecordSet,
    content: tilo_content::ContentStore,
) -> std::io::Result<(ui::State, WordProcessorWidget, WpFormat)> {
    let reader = records.file_history_reader().map_err(invalid_data)?;
    let want = ui::RecordCheckout::new(
        ui::RecordId::for_file(path),
        ui::RecordKind::new(RECORD_KIND),
    );
    let history = reader.read(&want).map_err(invalid_data)?;
    let (dir, name) = tilo_files::file_at(path).map_err(std::io::Error::other)?;
    let bytes = tilo_files::blocking::read(&dir, &name).map_err(std::io::Error::other)?;
    let format = WpFormat::from_path(path);
    let mut state = ui::State::new();
    let mut widget = WordProcessorWidget::new();
    widget.retain_file_history(reader);
    widget.model_mut().set_content_store(content);
    if let Some(history) = history {
        history.install_into(&mut state.doc).map_err(invalid_data)?;
        widget
            .adopt_existing_file(&state.doc, path.to_path_buf(), bytes, format)
            .map_err(invalid_data)?;
    } else {
        // Import through ReplaceFormatted, then start history so opening a
        // file is the document's baseline rather than an undo step.
        widget.model_mut().bind(&mut state.doc, &state.root);
        if !widget
            .model_mut()
            .replace_with_format(&mut state.doc, &bytes, format)
        {
            return Err(invalid_data(format!("could not import {}", path.display())));
        }
        start_history_here(&mut state);
        widget.set_file(path.to_path_buf(), format);
        widget.remember_open_document(&state.doc);
        widget.check_for_recovery();
    }
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
    // Import with a throwaway model so the widget handed back stays unbound
    // until the host's first sync, matching a new document.
    {
        let mut importer = WordProcessor::new();
        importer.bind(&mut state.doc, &state.root);
        // Templates are bundled and validated by tests, so import cannot fail
        // in practice; if a malformed one ever shipped, say so on stderr and
        // hand back the empty document rather than killing the process — the
        // release profile sets `panic = "abort"`.
        if !importer.replace_with_markdown(&mut state.doc, t.content().as_bytes()) {
            eprintln!("word-processor: bundled template {t:?} failed to import");
        }
    }
    start_history_here(&mut state);
    // Intentionally do NOT call `widget.set_file(..)`: leaving the path unset
    // keeps the document untitled (the widget falls back to `Untitled.md` on
    // save). The format is returned so callers can wire it up if they wish.
    let widget = WordProcessorWidget::new();
    (state, widget, format)
}

/// Export the document currently in `state` to `path` using `format`.
///
/// # Errors
///
/// Fails if `state` does not hold a well-formed document, if the codec cannot
/// render it (an import-only format), or if `path` cannot be written.
pub fn save_document(
    state: &ui::State,
    path: &Path,
    format: WpFormat,
    content: &dyn tilo_content::ContentSource,
) -> std::io::Result<()> {
    let containers = tilo_doc::model::open(&state.doc, &state.root).map_err(invalid_data)?;
    let dr = tilo_doc::DocRef {
        doc: &state.doc,
        c: &containers,
    };
    let bytes = format
        .export(dr, content)
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    tilo_files::file_at(path)
        .and_then(|(dir, name)| tilo_files::blocking::save_document(&dir, &name, &bytes))
        .map_err(std::io::Error::other)
}

// ---- autosave / crash-recovery -------------------------------------------

/// Best-effort autosave / crash-recovery store.
///
/// While editing, the word processor periodically writes a **recovery copy**
/// (Markdown, which does not retain every rich-document feature) to a stable per-document
/// path under the user's state directory. If the app crashes before saving, the
/// next session finds the newer recovery copy and can offer to restore it; a
/// real save deletes it. Everything here is **best-effort**: an unwritable state
/// directory never panics or blocks editing — recovery just silently does nothing.
pub mod recovery {
    use std::path::{Path, PathBuf};

    use tilo_files::{Dir, Location, VPath, blocking};
    use tilo_paths::AppPaths;

    /// Environment override for the recovery base directory. Set by tests (and
    /// available to callers that want to relocate the store) so recovery never
    /// touches the real `$HOME` / `$XDG_STATE_HOME` during a test run.
    pub const RECOVERY_BASE_ENV: &str = "TILO_WP_RECOVERY_DIR";

    /// The directory recovery copies live in, natively: [`store`]'s
    /// directory, or `./.tilo-wp-recovery` when the user has no state
    /// directory (copies are then kept in memory).
    pub fn base_dir() -> PathBuf {
        store()
            .and_then(|(dir, prefix)| dir.native_path(&prefix).ok())
            .unwrap_or_else(|| PathBuf::from(".tilo-wp-recovery"))
    }

    /// The store recovery copies live in, and the directory in it:
    /// `$TILO_WP_RECOVERY_DIR` when set, else `word-processor/recovery` in
    /// the shared `tilo` state location (on the web, the page's profile).
    pub fn store() -> Option<(Dir, VPath)> {
        let (location, prefix) = match std::env::var_os(RECOVERY_BASE_ENV) {
            Some(dir) => (Location::Native(PathBuf::from(dir)), VPath::root()),
            None => (
                AppPaths::for_app("tilo").state(),
                VPath::new("word-processor/recovery").ok()?,
            ),
        };
        Some((Dir::open(location).ok()?, prefix))
    }

    /// A recovery copy's file in [`store`].
    fn slot(doc_path: Option<&Path>) -> Option<(Dir, VPath)> {
        let (dir, prefix) = store()?;
        Some((dir, prefix.join(&VPath::new(&slot_name(doc_path)).ok()?)))
    }

    /// A deterministic 64-bit FNV-1a hash. Used to derive
    /// a filesystem-safe, **stable** recovery filename from a document's path
    /// (stable across runs and Rust versions, unlike `DefaultHasher`).
    fn fnv1a(bytes: &[u8]) -> u64 {
        let mut hasher = fnv::FnvHasher::default();
        std::hash::Hasher::write(&mut hasher, bytes);
        std::hash::Hasher::finish(&hasher)
    }

    /// The recovery file path for a document. Stable for a given target: the
    /// same document path always maps to the same file, so a later session can
    /// find it. An untitled document (no path) maps to a shared `untitled` slot.
    /// The on-disk name is `<16-hex-hash>.md`, hashed from the absolute path
    /// string (or the `:untitled:` sentinel) — short and filesystem-safe.
    pub fn recovery_path_for(doc_path: Option<&Path>) -> PathBuf {
        base_dir().join(slot_name(doc_path))
    }

    /// The file name of `doc_path`'s recovery copy.
    pub fn slot_name(doc_path: Option<&Path>) -> String {
        let key = match doc_path {
            Some(p) => {
                // Preserve the canonical identity of existing files, and resolve
                // symlinked ancestors before a new file (or directory) exists.
                // Otherwise first save can change /var/... into /private/var/...
                // and strand the recovery copy under its former hash.
                let abs = tilo_files::canonicalize(p).unwrap_or_else(|_| {
                    let absolute = if p.is_absolute() {
                        p.to_path_buf()
                    } else {
                        std::env::current_dir().map_or_else(|_| p.to_path_buf(), |d| d.join(p))
                    };
                    let mut resolved = PathBuf::new();
                    for component in absolute.components() {
                        match component {
                            std::path::Component::CurDir => {}
                            std::path::Component::ParentDir => {
                                resolved.pop();
                            }
                            component => {
                                resolved.push(component.as_os_str());
                                if let Ok(canonical) = tilo_files::canonicalize(&resolved) {
                                    resolved = canonical;
                                }
                            }
                        }
                    }
                    resolved
                });
                // Keep the recovery slot stable across the document's first
                // save, when canonicalization starts to succeed.
                ui::uri::stable_path_key(&abs)
            }
            None => ":untitled:".to_string(),
        };
        format!("{:016x}.md", fnv1a(key.as_bytes()))
    }

    #[cfg(unix)]
    #[test]
    fn recovery_identity_survives_creation_below_symlinked_ancestor() {
        // Holds the recovery base override steady while it is resolved.
        let _env_lock = tilo_test_env::lock();
        let dir =
            std::env::temp_dir().join(format!("writer-recovery-identity-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("real")).unwrap();
        std::os::unix::fs::symlink(dir.join("real"), dir.join("alias")).unwrap();
        let target = dir.join("alias/new/nested/doc.md");
        let before = recovery_path_for(Some(&target));
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        std::fs::write(&target, b"saved").unwrap();
        assert_eq!(
            before,
            recovery_path_for(Some(&target)),
            "creating a file must preserve its recovery identity"
        );
        assert_eq!(
            before,
            recovery_path_for(Some(&target.canonicalize().unwrap()))
        );
        let with_parent = dir.join("alias/new/nested/../other.md");
        let before_parent = recovery_path_for(Some(&with_parent));
        std::fs::write(&with_parent, b"saved").unwrap();
        assert_eq!(before_parent, recovery_path_for(Some(&with_parent)));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// Write a recovery copy (Markdown bytes) for `doc_path`. Best-effort: creates
    /// the base directory if needed and swallows any I/O error. Returns the path
    /// it wrote to on success (for the caller's bookkeeping / tests).
    pub fn write(doc_path: Option<&Path>, markdown: &[u8]) -> Option<PathBuf> {
        let (dir, name) = slot(doc_path)?;
        blocking::write_atomic(&dir, &name, markdown)
            .ok()
            .map(|()| recovery_path_for(doc_path))
    }

    /// Read back a recovery copy's Markdown bytes for `doc_path`, if one exists.
    pub fn read(doc_path: Option<&Path>) -> Option<Vec<u8>> {
        let (dir, name) = slot(doc_path)?;
        blocking::read(&dir, &name).ok()
    }

    /// Whether a recovery copy currently exists for `doc_path`.
    pub fn exists(doc_path: Option<&Path>) -> bool {
        slot(doc_path).is_some_and(|(dir, name)| blocking::metadata(&dir, &name).is_ok())
    }

    /// Delete the recovery copy for `doc_path` (best-effort; absence is success).
    pub fn clear(doc_path: Option<&Path>) {
        if let Some((dir, name)) = slot(doc_path) {
            let _ = blocking::remove(&dir, &name);
        }
    }

    /// Render the document currently in `state` to Markdown bytes for a recovery
    /// copy. Markdown is the canonical lossless interchange format for the model,
    /// so this is what `WordProcessorWidget` writes regardless of the document's
    /// on-disk save format. Returns `None` only if the document has no schema yet.
    pub fn markdown_bytes(state: &ui::State) -> Option<Vec<u8>> {
        let containers = tilo_doc::model::open(&state.doc, &state.root).ok()?;
        let dr = tilo_doc::DocRef {
            doc: &state.doc,
            c: &containers,
        };
        crate::WpFormat::Markdown
            .export(dr, &tilo_content::NoContent)
            .ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_doc_codecs::{FormatProvider, MarkdownCodec};

    fn first_text(snap: &tilo_doc::DocSnapshot) -> Option<String> {
        snap.blocks.first().map(|b| b.text.clone())
    }

    /// The binding reuses the UI state's undo, and two peers converge through UI state.
    #[test]
    fn the_binding_shares_ui_state_undo_and_converges_between_peers() {
        // Phase: binding_reuses_ui_state_undo.
        // A qdoc document living in `ui::State.doc` reuses ui's full-snapshot
        // undo/redo — proving the binding doesn't need its own undo layer.
        {
            let mut s = ui::State::new();
            let mut ed = EditorDoc::bind(&mut s.doc, &s.root);
            s.commit_action(None, None); // checkpoint after schema init

            {
                let mut dm = ed.edit(&mut s.doc);
                MarkdownCodec
                    .import(
                        b"# Title\n\nHello\n",
                        &mut dm,
                        &mut tilo_content::ContentInputs::new(),
                    )
                    .unwrap();
            }
            s.commit_action(None, None);
            assert_eq!(first_text(&ed.snapshot(&s.doc)), Some("Title".to_string()));

            s.undo(None, None);
            assert_eq!(
                ed.snapshot(&s.doc).blocks.len(),
                0,
                "undo reverts to the empty schema"
            );

            s.redo(None, None);
            assert_eq!(first_text(&ed.snapshot(&s.doc)), Some("Title".to_string()));
        }

        // Phase: two_peers_converge_through_ui_state.
        // Two `ui::State` replicas editing concurrently converge through ui's
        // incremental sync (`save_incremental`/`load_incremental`).
        {
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
    }

    /// Save to PDF, HTML, RTF and EPUB write valid files; PDF and EPUB import are unsupported.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
    )]
    fn export_formats_write_valid_files_and_refuse_unsupported_imports() {
        // Phase: save_document_to_pdf_writes_valid_pdf.
        // Saving to a `*.pdf` path routes through `tilo-doc-pdf` and writes a valid PDF
        // (the bytes start with the `%PDF` signature). PDF is export-only.
        {
            let mut s = ui::State::new();
            let mut ed = EditorDoc::bind(&mut s.doc, &s.root);
            {
                let mut dm = ed.edit(&mut s.doc);
                MarkdownCodec
                    .import(
                        b"# Title\n\nHello world\n",
                        &mut dm,
                        &mut tilo_content::ContentInputs::new(),
                    )
                    .unwrap();
            }

            let dir = std::env::temp_dir();
            let path = dir.join(format!("editor-pdf-test-{}.pdf", std::process::id()));
            assert_eq!(
                WpFormat::from_path(&path),
                WpFormat::Pdf,
                ".pdf extension picks the Pdf format"
            );

            save_document(&s, &path, WpFormat::Pdf, &tilo_content::NoContent).expect("save to pdf");
            let bytes = std::fs::read(&path).expect("read back the pdf");
            let _ = std::fs::remove_file(&path);
            assert!(
                bytes.starts_with(b"%PDF"),
                "output begins with the PDF signature: {:?}",
                &bytes[..bytes.len().min(8)]
            );
        }

        // Phase: pdf_import_is_unsupported.
        // PDF import is unsupported and surfaces a clear error.
        {
            let mut s = ui::State::new();
            let containers = tilo_doc::model::init(&mut s.doc, &s.root);
            let mut ids = tilo_doc::IdGen::new(s.doc.get_actor().to_hex_string());
            let mut dm = tilo_doc::DocMut {
                doc: &mut s.doc,
                c: &containers,
                ids: &mut ids,
            };
            let err = WpFormat::Pdf
                .import(
                    b"%PDF-1.7",
                    &mut dm,
                    &mut tilo_content::ContentInputs::new(),
                )
                .unwrap_err();
            assert!(
                matches!(err, tilo_doc_codecs::CodecError::Unsupported(_)),
                "got {err:?}"
            );
        }

        // Phase: save_document_to_html_writes_html.
        // Saving to a `*.html` path routes through `tilo_doc_codecs::export_html` and writes a complete
        // HTML document. HTML is export-only.
        {
            let mut s = ui::State::new();
            let mut ed = EditorDoc::bind(&mut s.doc, &s.root);
            {
                let mut dm = ed.edit(&mut s.doc);
                MarkdownCodec
                    .import(
                        b"# Title\n\nHello world\n",
                        &mut dm,
                        &mut tilo_content::ContentInputs::new(),
                    )
                    .unwrap();
            }

            let dir = std::env::temp_dir();
            let path = dir.join(format!("editor-html-test-{}.html", std::process::id()));
            assert_eq!(
                WpFormat::from_path(&path),
                WpFormat::Html,
                ".html extension picks the Html format"
            );

            save_document(&s, &path, WpFormat::Html, &tilo_content::NoContent)
                .expect("save to html");
            let bytes = std::fs::read(&path).expect("read back the html");
            let _ = std::fs::remove_file(&path);
            let text = String::from_utf8(bytes).expect("html output is valid UTF-8");
            let lower = text.to_ascii_lowercase();
            assert!(
                lower.contains("<!doctype html") || lower.contains("<html"),
                "output is an HTML document: {:?}",
                text.get(..text.len().min(64))
            );
        }

        // Phase: save_document_to_rtf_writes_rtf.
        // Saving to a `*.rtf` path routes through `tilo_doc_codecs::export_rtf` and writes a valid RTF
        // document (the bytes start with the `{\rtf` signature). RTF is export-only.
        {
            let mut s = ui::State::new();
            let mut ed = EditorDoc::bind(&mut s.doc, &s.root);
            {
                let mut dm = ed.edit(&mut s.doc);
                MarkdownCodec
                    .import(
                        b"# Title\n\nHello world\n",
                        &mut dm,
                        &mut tilo_content::ContentInputs::new(),
                    )
                    .unwrap();
            }

            let dir = std::env::temp_dir();
            let path = dir.join(format!("editor-rtf-test-{}.rtf", std::process::id()));
            assert_eq!(
                WpFormat::from_path(&path),
                WpFormat::Rtf,
                ".rtf extension picks the Rtf format"
            );

            save_document(&s, &path, WpFormat::Rtf, &tilo_content::NoContent).expect("save to rtf");
            let bytes = std::fs::read(&path).expect("read back the rtf");
            let _ = std::fs::remove_file(&path);
            assert!(
                bytes.starts_with(b"{\\rtf"),
                "output begins with the RTF signature: {:?}",
                &bytes[..bytes.len().min(8)]
            );
        }

        // Phase: save_document_to_epub_writes_epub.
        // Saving to a `*.epub` path routes through `tilo_doc_codecs::export_epub` and writes a valid
        // EPUB OCF container: the bytes are a ZIP archive (`PK` magic) whose first
        // entry is the literal `mimetype` member. EPUB is export-only.
        {
            let mut s = ui::State::new();
            let mut ed = EditorDoc::bind(&mut s.doc, &s.root);
            {
                let mut dm = ed.edit(&mut s.doc);
                MarkdownCodec
                    .import(
                        b"# Title\n\nHello world\n",
                        &mut dm,
                        &mut tilo_content::ContentInputs::new(),
                    )
                    .unwrap();
            }

            let dir = std::env::temp_dir();
            let path = dir.join(format!("editor-epub-test-{}.epub", std::process::id()));
            assert_eq!(
                WpFormat::from_path(&path),
                WpFormat::Epub,
                ".epub extension picks the Epub format"
            );

            save_document(&s, &path, WpFormat::Epub, &tilo_content::NoContent)
                .expect("save to epub");
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
            assert!(
                bytes.len() >= 38,
                "epub is at least one local file header long"
            );
            assert_eq!(
                &bytes[30..38],
                b"mimetype",
                "first zip entry is the OCF `mimetype` member"
            );
        }

        // Phase: epub_import_is_unsupported.
        // EPUB import is unsupported and surfaces a clear error.
        {
            let mut s = ui::State::new();
            let containers = tilo_doc::model::init(&mut s.doc, &s.root);
            let mut ids = tilo_doc::IdGen::new(s.doc.get_actor().to_hex_string());
            let mut dm = tilo_doc::DocMut {
                doc: &mut s.doc,
                c: &containers,
                ids: &mut ids,
            };
            let err = WpFormat::Epub
                .import(
                    b"PK\x03\x04",
                    &mut dm,
                    &mut tilo_content::ContentInputs::new(),
                )
                .unwrap_err();
            assert!(
                matches!(err, tilo_doc_codecs::CodecError::Unsupported(_)),
                "got {err:?}"
            );
        }
    }

    /// `from_path` maps html, htm and rtf; HTML import opens the first heading; RTF import opens a paragraph and
    /// an RTF export then import round-trips text and bold.
    /// A `.txt` file opens with one paragraph per line, markup characters
    /// kept as text, and saves back to the same lines.
    #[test]
    fn plain_text_opens_one_paragraph_per_line_and_saves_back() {
        let dir = std::env::temp_dir().join(format!("wp-txt-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("standup.txt");
        let lines = [
            "Standup 2026-09-24",
            "# Maya: finished; next: review. Blocked: no",
            "- Jonas: *sync* bug",
            "",
            "Priya: done",
        ];
        std::fs::write(&path, format!("{}\r\n", lines.join("\r\n"))).unwrap();
        assert_eq!(WpFormat::from_path(&path), WpFormat::Text);
        let (state, _, format) =
            open_document(&path, tilo_content::ContentStore::memory()).unwrap();
        assert_eq!(format, WpFormat::Text);
        let c = tilo_doc::model::open(&state.doc, &state.root).unwrap();
        let blocks = tilo_doc::model::materialize(&state.doc, &c).blocks;
        let texts: Vec<&str> = blocks.iter().map(|b| b.text.as_str()).collect();
        assert_eq!(texts, lines);
        assert!(
            blocks.iter().all(|b| b.kind == "paragraph"),
            "nothing is read as markup"
        );

        let out = dir.join("out.txt");
        save_document(&state, &out, WpFormat::Text, &tilo_content::NoContent).unwrap();
        assert_eq!(
            std::fs::read_to_string(&out).unwrap(),
            format!("{}\n", lines.join("\n"))
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
    )]
    fn imports_map_extensions_and_round_trip_text_and_bold() {
        // Phase: from_path_maps_html_htm_rtf.
        // `WpFormat::from_path` maps the export-only extensions to their formats,
        // including the `.htm` alias for HTML.
        {
            assert_eq!(WpFormat::from_path(Path::new("doc.html")), WpFormat::Html);
            assert_eq!(WpFormat::from_path(Path::new("doc.htm")), WpFormat::Html);
            assert_eq!(
                WpFormat::from_path(Path::new("doc.HTML")),
                WpFormat::Html,
                "case-insensitive"
            );
            assert_eq!(WpFormat::from_path(Path::new("doc.rtf")), WpFormat::Rtf);
            assert_eq!(
                WpFormat::from_path(Path::new("doc.RTF")),
                WpFormat::Rtf,
                "case-insensitive"
            );
            assert_eq!(WpFormat::from_path(Path::new("doc.epub")), WpFormat::Epub);
            assert_eq!(
                WpFormat::from_path(Path::new("doc.EPUB")),
                WpFormat::Epub,
                "case-insensitive"
            );
        }

        // Phase: html_import_opens_first_heading.
        // `WpFormat::Html.import` of a small HTML document produces the expected
        // first heading (HTML import is now wired to `tilo_doc_codecs::HtmlCodec`).
        {
            let mut s = ui::State::new();
            let containers = tilo_doc::model::init(&mut s.doc, &s.root);
            let mut ids = tilo_doc::IdGen::new(s.doc.get_actor().to_hex_string());
            {
                let mut dm = tilo_doc::DocMut {
                    doc: &mut s.doc,
                    c: &containers,
                    ids: &mut ids,
                };
                WpFormat::Html
                    .import(
                        b"<h1>Opened from HTML</h1><p>body</p>",
                        &mut dm,
                        &mut tilo_content::ContentInputs::new(),
                    )
                    .expect("html import succeeds");
            }
            let snap = tilo_doc::model::materialize(&s.doc, &containers);
            let first = snap
                .blocks
                .first()
                .expect("html import yields at least one block");
            assert_eq!(first.kind, "heading", "first block is a heading: {snap:?}");
            assert_eq!(
                first.text, "Opened from HTML",
                "first heading text: {snap:?}"
            );
        }

        // Phase: rtf_import_opens_a_paragraph.
        // `WpFormat::Rtf.import` of a small `.rtf` document produces the expected
        // first paragraph (RTF import is wired to `tilo_doc_codecs::RtfCodec`).
        {
            let mut s = ui::State::new();
            let containers = tilo_doc::model::init(&mut s.doc, &s.root);
            let mut ids = tilo_doc::IdGen::new(s.doc.get_actor().to_hex_string());
            {
                let mut dm = tilo_doc::DocMut {
                    doc: &mut s.doc,
                    c: &containers,
                    ids: &mut ids,
                };
                WpFormat::Rtf
                    .import(
                        b"{\\rtf1\\ansi\\deff0\n{\\pard\\fs22 Opened from RTF\\par}}",
                        &mut dm,
                        &mut tilo_content::ContentInputs::new(),
                    )
                    .expect("rtf import succeeds");
            }
            let snap = tilo_doc::model::materialize(&s.doc, &containers);
            let first = snap
                .blocks
                .first()
                .expect("rtf import yields at least one block");
            assert_eq!(
                first.text, "Opened from RTF",
                "first paragraph text: {snap:?}"
            );
        }

        // Phase: rtf_export_then_import_roundtrips_text_and_bold.
        // Full round-trip: render a Markdown-built document to `.rtf` via `tilo_doc_codecs::export_rtf`
        // (the export path), re-open it through `WpFormat::Rtf.import` (the new
        // import path), and assert the text and a bold run survive.
        {
            // Source document with a bold word.
            let mut s = ui::State::new();
            let mut ed = EditorDoc::bind(&mut s.doc, &s.root);
            {
                let mut dm = ed.edit(&mut s.doc);
                MarkdownCodec
                    .import(
                        b"Hello **world** today\n",
                        &mut dm,
                        &mut tilo_content::ContentInputs::new(),
                    )
                    .unwrap();
            }

            // Export to RTF bytes via the editor's export path (`tilo_doc_codecs::export_rtf`).
            let containers = tilo_doc::model::open(&s.doc, &s.root).unwrap();
            let dr = tilo_doc::DocRef {
                doc: &s.doc,
                c: &containers,
            };
            let rtf = WpFormat::Rtf
                .export(dr, &tilo_content::NoContent)
                .expect("rtf export");

            // Re-import those bytes into a fresh document via the import path.
            let mut s2 = ui::State::new();
            let containers2 = tilo_doc::model::init(&mut s2.doc, &s2.root);
            let mut ids2 = tilo_doc::IdGen::new(s2.doc.get_actor().to_hex_string());
            {
                let mut dm = tilo_doc::DocMut {
                    doc: &mut s2.doc,
                    c: &containers2,
                    ids: &mut ids2,
                };
                WpFormat::Rtf
                    .import(&rtf, &mut dm, &mut tilo_content::ContentInputs::new())
                    .expect("rtf import");
            }
            let snap = tilo_doc::model::materialize(&s2.doc, &containers2);
            let p = snap
                .blocks
                .iter()
                .find(|b| b.kind == "paragraph")
                .expect("a paragraph survives");
            assert_eq!(p.text, "Hello world today", "text round-trips: {snap:?}");
            let strong = p
                .marks
                .iter()
                .find(|m| m.name == "strong")
                .expect("bold run survives");
            assert_eq!(
                p.text.get(strong.start..strong.end),
                Some("world"),
                "bold covers the right run: {:?}",
                p.marks
            );
        }
    }

    /// The timeline works over a bound doc; the report template seeds the expected heading; every template
    /// is listed, imports non-empty and builds.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test: each phase is a former standalone test kept verbatim as a scoped block"
    )]
    fn templates_list_every_variant_and_seed_real_content() {
        // Phase: timeline_over_bound_doc.
        // The version timeline + time-travel work over a bound document.
        {
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

            let vers = ui::tilo_resource::history::versions(&mut s.doc);
            assert!(vers.len() >= 2, "multiple authored changes");
            let last = vers.len() - 1;
            assert_eq!(
                tilo_doc::snapshot::at(&mut s.doc, last).unwrap().blocks[0].text,
                "hello world"
            );
            let saw_hello = (0..vers.len()).any(|i| {
                tilo_doc::snapshot::at(&mut s.doc, i)
                    .ok()
                    .and_then(|sn| sn.blocks.first().map(|b| b.text == "hello"))
                    .unwrap_or(false)
            });
            assert!(
                saw_hello,
                "timeline should expose the intermediate 'hello' state"
            );
        }

        // Phase: new_from_template_report_seeds_expected_heading.
        // `new_from_template` seeds a fresh state whose materialized document opens
        // with the template's first heading. (`Report` begins with "Report Title".)
        {
            let (state, widget, format) = new_from_template(Template::Report);
            assert_eq!(
                format,
                WpFormat::Markdown,
                "templates are imported as Markdown"
            );

            let containers = tilo_doc::model::open(&state.doc, &state.root)
                .expect("template-seeded state has the canonical schema");
            let snap = tilo_doc::model::materialize(&state.doc, &containers);
            let first = snap
                .blocks
                .first()
                .expect("report template has at least one block");
            assert_eq!(first.kind, "heading", "the report opens with a heading");
            assert_eq!(
                first.text, "Report Title",
                "the report's first heading text"
            );

            // Untitled / unbound: a freshly returned widget hasn't run its `sync`
            // pass yet, so it has no bound containers and no file path — the
            // document will prompt for a destination on first save.
            assert!(
                widget.model().containers.is_none(),
                "a fresh template widget is unbound until the first sync"
            );
        }

        // Phase: template_all_lists_every_variant.
        // `Template::all()` enumerates every variant exactly once, in menu order.
        {
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

        // Phase: every_template_imports_non_empty.
        // Every bundled template has non-empty content and imports without error
        // into a fresh document with at least one block.
        {
            for &t in Template::all() {
                assert!(!t.content().trim().is_empty(), "{t:?} content is non-empty");

                // Import path mirrors `new_from_template` but asserts on each variant.
                let mut s = ui::State::new();
                let containers = tilo_doc::model::init(&mut s.doc, &s.root);
                let mut ids = tilo_doc::IdGen::new(s.doc.get_actor().to_hex_string());
                {
                    let mut dm = tilo_doc::DocMut {
                        doc: &mut s.doc,
                        c: &containers,
                        ids: &mut ids,
                    };
                    WpFormat::Markdown
                        .import(
                            t.content().as_bytes(),
                            &mut dm,
                            &mut tilo_content::ContentInputs::new(),
                        )
                        .unwrap_or_else(|e| panic!("{t:?} imports cleanly: {e:?}"));
                }
                let snap = tilo_doc::model::materialize(&s.doc, &containers);
                assert!(!snap.blocks.is_empty(), "{t:?} produces at least one block");
            }
        }

        // Phase: new_from_template_builds_every_variant.
        // The public `new_from_template` entrypoint builds a usable document for
        // every variant (not just `Report`).
        {
            for &t in Template::all() {
                let (state, _widget, format) = new_from_template(t);
                assert_eq!(format, WpFormat::Markdown);
                let containers = tilo_doc::model::open(&state.doc, &state.root).unwrap();
                let snap = tilo_doc::model::materialize(&state.doc, &containers);
                assert!(!snap.blocks.is_empty(), "{t:?} yields a non-empty document");
            }
        }
    }

    // ---- document templates -----------------------------------------------

    // ---- autosave / crash-recovery -----------------------------------------

    #[cfg(windows)]
    #[test]
    fn recovery_defaults_to_native_local_app_data() {
        let mut env = tilo_test_env::ScopedEnv::new();
        env.remove(recovery::RECOVERY_BASE_ENV);
        let actual = recovery::base_dir();
        drop(env);
        let local = std::path::PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap());
        assert_eq!(actual, local.join("tilo/word-processor/recovery"));
        assert!(actual.is_absolute());
    }

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

    /// The recovery path is stable per document, recovery write/read/clear round-trips, and recovery
    /// Markdown round-trips through import.
    #[test]
    fn recovery_files_are_stable_per_document_and_round_trip() {
        // Phase: recovery_path_is_stable_per_document.
        // `recovery_path_for` is **stable** for a given document path: the same path
        // (in any equivalent spelling) maps to the same recovery file, and distinct
        // documents map to distinct files. The untitled slot is a single fixed path.
        {
            let dir = temp_recovery_dir("stable");
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set(recovery::RECOVERY_BASE_ENV, &dir);

            let doc = std::path::Path::new("/home/u/My Notes.md");
            let p1 = recovery::recovery_path_for(Some(doc));
            let p2 = recovery::recovery_path_for(Some(doc));
            assert_eq!(p1, p2, "the same document maps to the same recovery file");
            assert!(
                p1.starts_with(&dir),
                "recovery file lives under the override dir"
            );

            let other = recovery::recovery_path_for(Some(std::path::Path::new("/home/u/Other.md")));
            assert_ne!(
                p1, other,
                "different documents map to different recovery files"
            );

            let u1 = recovery::recovery_path_for(None);
            let u2 = recovery::recovery_path_for(None);
            assert_eq!(u1, u2, "the untitled slot is a single stable path");
            assert_ne!(u1, p1, "untitled and titled documents don't collide");
        }

        // Phase: recovery_write_read_clear_roundtrip.
        // `recovery::write` materializes a recovery copy under the (overridden) base
        // dir; `read`/`exists` see it; `clear` removes it. End-to-end on the real
        // filesystem (a temp dir), best-effort throughout.
        {
            let dir = temp_recovery_dir("rwc");
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set(recovery::RECOVERY_BASE_ENV, &dir);
            let doc = std::path::Path::new("/tmp/whatever/doc.md");

            assert!(
                !recovery::exists(Some(doc)),
                "no recovery file before any write"
            );
            let written = recovery::write(Some(doc), b"# Recovered\n\nbody\n")
                .expect("write a recovery copy");
            assert!(written.exists(), "the written recovery file is on disk");
            assert!(
                recovery::exists(Some(doc)),
                "exists() sees the recovery file"
            );
            assert_eq!(
                recovery::read(Some(doc)).expect("read back"),
                b"# Recovered\n\nbody\n",
                "read back the exact bytes written"
            );

            recovery::clear(Some(doc));
            assert!(
                !recovery::exists(Some(doc)),
                "clear() removes the recovery file"
            );

            let _ = std::fs::remove_dir_all(&dir);
        }

        // Phase: recovery_markdown_roundtrips_through_import.
        // `recovery::markdown_bytes` renders the live document to recovery Markdown
        // that re-imports to the same first heading — proving the recovery copy is a
        // faithful, restorable snapshot of the document.
        {
            let mut s = ui::State::new();
            let mut ed = EditorDoc::bind(&mut s.doc, &s.root);
            {
                let mut dm = ed.edit(&mut s.doc);
                MarkdownCodec
                    .import(
                        b"# Draft Title\n\nUnsaved paragraph\n",
                        &mut dm,
                        &mut tilo_content::ContentInputs::new(),
                    )
                    .unwrap();
            }
            let md = recovery::markdown_bytes(&s).expect("render recovery markdown");

            let mut s2 = ui::State::new();
            let containers = tilo_doc::model::init(&mut s2.doc, &s2.root);
            let mut ids = tilo_doc::IdGen::new(s2.doc.get_actor().to_hex_string());
            {
                let mut dm = tilo_doc::DocMut {
                    doc: &mut s2.doc,
                    c: &containers,
                    ids: &mut ids,
                };
                MarkdownCodec
                    .import(&md, &mut dm, &mut tilo_content::ContentInputs::new())
                    .expect("re-import recovery markdown");
            }
            let snap = tilo_doc::model::materialize(&s2.doc, &containers);
            assert_eq!(
                first_text(&snap).as_deref(),
                Some("Draft Title"),
                "recovery copy restores the heading"
            );
        }
    }
}

pub mod component;
pub use component::definition;
/// The kind of a Writer document record: the rich-text resource the native
/// app, the workspace host and the browser all open. Records stamped with the
/// former `document` read as this kind (see [`ui::RecordKind::new`]).
pub const RECORD_KIND: &str = <tilo_document::RichText as tilo_resource::Resource>::KIND;

/// Initialize a checked New record as an empty Writer document.
///
/// # Errors
///
/// Never fails today; the `Result` matches the record-initializer contract.
pub fn initialize_empty_document(doc: &mut AutoCommit) -> Result<(), String> {
    WordProcessor::initialize_empty_document(doc)
}
