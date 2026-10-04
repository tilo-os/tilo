//! The root widget of the Typst editor.
//!
//! `TypstApp` is a single full-bleed [`Widget`]. It hosts the library controls
//! of [`Chrome`] above two content panes it paints itself (the source editor
//! and the rendered preview, which are genuine content drawing), and lays them
//! out in plain rects computed in [`TypstApp::layout_in`]:
//!
//! ```text
//! ┌──────────────────────────────────────────────────────────┐
//! │ ‹ document ▾ ›  Compile Save Open Export   − + Fit  Problems │
//! ├───────────────────────────┬──────────────────────────────┤
//! │ source editor             │      rendered preview         │
//! │ (code, line numbers,      │  (scrollable pages + thumbs)  │
//! │  syntax highlight)        │                               │
//! ├───────────────────────────┤                               │
//! │ Problems panel            │                               │
//! ├───────────────────────────┴──────────────────────────────┤
//! │ Ln 4, Col 28                                 2 pages, 100% │
//! └──────────────────────────────────────────────────────────┘
//! ```
//!
//! The editor stands on the window ground and the preview on the inspector
//! region; neither fills a ground of its own. What a person reads while
//! working (caret position, pages, zoom) is the status bar, which reserves its
//! band; a change worth noticing once ("Compiled in 42 ms") is the transient
//! [`ui::StatusBubble`].
//!
//! State that belongs in the CRDT — the document source — lives in an
//! Automerge `Text` object under `root["source"]`; the editor splices into it
//! directly, so undo/redo and live collaboration come for free. Everything
//! else here (caret, scroll, zoom, the split ratio, cached page bitmaps) is
//! ephemeral view state.

use std::cell::Cell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use ui::time::Instant;
use ui::widget::{AppModel, ModelPath};

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, Value};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::permissions::PermissionContext;
use ui::portal::{FileFilter, PickRequest, PortalChannel, PortalOutcome};
use ui::presence::PresenceCtx;
use ui::surface::Region;
use ui::text_render::TextRenderer;
use ui::theme::Theme;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PendingIo {
    OpenTyp,
    SaveTyp,
    ExportPdf,
}

#[derive(Clone, Debug)]
enum ResourceIo {
    Open(PathBuf),
    Save(PathBuf),
    ExportPdf(PathBuf),
}

fn typ_filter() -> FileFilter {
    FileFilter::new("Typst source").glob("*.typ")
}

fn typ_open_request() -> PickRequest {
    PickRequest::open("Open a Typst file", "Open")
        .thread_name("typst-portal-open")
        .filter(typ_filter())
        .all_files()
}

fn typ_save_request(suggested: &str) -> PickRequest {
    PickRequest::save("Save Typst file", "Save", suggested)
        .thread_name("typst-portal-save")
        .filter(typ_filter())
}

fn pdf_export_request(suggested: &str) -> PickRequest {
    PickRequest::save("Export PDF", "Export", suggested)
        .thread_name("typst-portal-pdf")
        .filter(FileFilter::new("PDF document").glob("*.pdf"))
}
use ui::widget::{ResizeHandle, Widget};

use crate::chrome::{Chrome, Command};
use crate::compile::{self, CompileOutput, WorldParts};
use crate::diagnostics::DiagnosticsPanel;
use crate::editor::SourceEditor;
use crate::preview::PreviewPane;
use crate::samples::{DEFAULT_INDEX, SAMPLES};
use crate::worker::CompileWorker;

/// Pixels-per-point the document is rasterised at. 2.0 keeps pages crisp on
/// a `HiDPI` surface; preview zoom only scales the *display*, never the render
/// resolution (matching the behaviour the welcome doc advertises).
const PPP: f32 = 2.0;

/// Which single pane the compact (narrow-window) layout is showing.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CompactPane {
    Editor,
    Preview,
}

/// A document whose last line is a paragraph long enough to soft-wrap several
/// times, used only by the `--shots long-line` scenario to capture how the
/// source editor wraps it with the caret at the end of the line. The long line
/// is last (no trailing newline) so placing the caret at the document end lands
/// it there.
const LONG_LINE_DEMO: &str = "= Meeting notes\n\nA paragraph in Typst is just lines of text, so this one is written as a single long line of source that runs well past the right edge of the editor pane. The editor wraps it onto continuation rows and keeps the caret in view as you type toward the end.";

/// A document the picker can switch to: either a bundled sample or a `.typ`
/// file discovered on disk.
#[derive(Clone)]
struct Document {
    name: String,
    source: String,
    from_disk: bool,
}

/// The sizes of the window's own chrome, measured in the theme at paint: the
/// band holding the controls, the status bar and the width under which one
/// pane shows at a time.
#[derive(Clone, Copy, PartialEq)]
struct Frame {
    bar_h: f64,
    status_h: f64,
    compact_w: f64,
}

impl Frame {
    /// `control_h` is the height of one control and `status_line_h` of one
    /// line of status text, both measured at the current text scale.
    fn of(theme: &Theme, control_h: f64, status_line_h: f64) -> Self {
        let metrics = &theme.tokens.metrics;
        Self {
            bar_h: control_h + 2.0 * f64::from(theme.tokens.spacing.s1),
            // The bar grows with the text, so its readouts are never clipped.
            status_h: ui::chrome::status_bar::height(theme)
                .max(status_line_h + 2.0 * f64::from(theme.tokens.spacing.s2)),
            compact_w: f64::from(metrics.compact_breakpoint),
        }
    }
}

#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool tracks an independent piece of ephemeral view/drag \
              state (which demo scenario is pending, which drag is live); \
              grouping them into an enum would not read more clearly at call \
              sites like `self.dragging_editor`"
)]
pub struct TypstApp {
    // --- compilation backend ---
    parts: Arc<WorldParts>,
    worker: Option<CompileWorker>,
    /// Sequence number of the freshest compile result we've applied. Stale
    /// (lower-numbered) results from the worker are ignored.
    applied_seq: u64,

    // --- panes ---
    editor: SourceEditor,
    preview: PreviewPane,
    diagnostics: DiagnosticsPanel,

    // --- document / CRDT ---
    source_id: Option<ObjId>,
    documents: Vec<Document>,
    current: usize,
    /// An in-flight host file dialog: the operation and the channel its chosen
    /// path arrives on, drained once per frame in [`Self::drain_io`]. `None`
    /// when no Open / Save / Export chooser is open.
    io: Option<(PendingIo, PortalChannel)>,
    background_poll_deadline: Cell<Option<Instant>>,
    resource_invocation: Option<tilo_actions::InvocationId>,
    pending_resource_io: Option<(tilo_actions::InvocationId, ResourceIo)>,
    resource_result: Option<tilo_actions::InvocationUpdate>,
    /// Whether the first (synchronous) compile + worker spawn has happened.
    initialized: bool,
    /// Last source text we highlighted / compiled — drives change detection.
    last_source: String,
    /// Mirror of the source read each `sync`, used by `draw` (which has no
    /// doc access) for the caret readout and change detection. The editor body
    /// and its syntax highlighting live in the shared `SourceEditor` widget.
    source_cache: String,
    /// A native source splice is committed by the widget host rather than an
    /// action envelope; this rising edge supplies its durable provenance.
    native_edit_pending: bool,

    // --- view state ---
    /// Editor↔preview horizontal split, 0..1 of the content width.
    split_ratio: f64,
    show_diagnostics: bool,
    err_count: usize,
    warn_count: usize,
    elapsed_ms: u128,
    /// Whether the current width put us in the single-pane compact layout.
    /// Recomputed every `layout_in` from the live width — never cached from
    /// the first frame.
    compact: bool,
    /// Which pane the compact layout is showing (ignored while `compact` is
    /// false).
    compact_view: CompactPane,
    /// The renderer's accessibility text scale; width breakpoints designed at
    /// 1× are compared against `width / text_k`.
    text_k: f64,
    /// Screenshot-only: replace the library with the bundled documents on
    /// the next sync, so a capture never depends on the files on disk.
    demo_reload: bool,
    /// Screenshot-only: on the next layout pass, drop the caret at the end of
    /// the source so the capture shows it wrapped and in view.
    demo_caret_end: bool,
    /// Screenshot harness only: select a couple of editor lines for the capture.
    demo_editor_select: bool,
    /// Screenshot-only: on the next layout pass, zoom the preview in and scroll
    /// it so the capture proves cursor-zoom + scroll-when-zoomed.
    demo_preview_zoom: bool,
    /// Screenshot-only: on the next layout pass, zoom the preview past fit-width
    /// and pan it on both axes so the capture proves drag/wheel panning.
    demo_preview_pan: bool,
    /// Screenshot-only: on the next layout pass, select a run of preview text so
    /// the capture proves rendered-text selection.
    demo_preview_select: bool,
    /// Screenshot-only: after the first render, break the source so the
    /// capture shows the stale-render banner over the last good pages.
    demo_stale: bool,
    /// Screenshot-only: open the document list.
    demo_documents: bool,

    // --- chrome ---
    chrome: Chrome,
    frame: Frame,
    /// The transient bottom-trailing bubble: a compile result, a saved file.
    status: ui::StatusBubble,
    /// The next compile result is feedback for an explicit Compile.
    compile_feedback: bool,

    // --- layout rects (filled by layout_in) ---
    rect: Rect,
    bar_rect: Rect,
    editor_rect: Rect,
    preview_rect: Rect,
    diag_rect: Rect,
    status_rect: Rect,

    // --- interaction ---
    mouse: Point,
    dragging_editor: bool,
    /// A preview interaction (text-selection drag or scrollbar drag) is live, so
    /// subsequent moves route back to the preview.
    dragging_preview: bool,
    /// The library's pane splitter between the editor and the preview.
    splitter: ResizeHandle,
    /// Set when a pointer *move* changed something visible (hover, live drag)
    /// so `needs_redraw` asks the host to repaint — moves are otherwise gated.
    dirty: bool,
    /// The pointer has moved over the window, so the bubble follows it.
    pointer_seen: bool,

    /// Framework-hosted surfaces leave the palette tree to
    /// `tilo_resource::publish`; standalone keeps the contextual builder.
    framework_hosted: bool,
    /// Present once [`ResourceSurface::attach`] has run. Source setup and
    /// whole-source replacement go through it so history records the action.
    document_actions: Option<ui::framework::Dispatcher<crate::resource::TypstEditor>>,
    /// Local host for standalone, `--shots`, and tests. The framework
    /// dispatcher replaces it once the surface is attached.
    local_actions: ui::ActionHost,
}

impl TypstApp {
    /// The live editor: its document library is the user's real `.typ` files on
    /// disk. If none exist yet it opens a single blank starter document.
    pub fn new() -> Self {
        Self::from_library(Self::save_dir().as_deref())
    }

    /// This editor with its History button left to the window's own bar.
    #[cfg(test)]
    pub(crate) fn without_history_button(mut self) -> Self {
        self.chrome.omit_history();
        self
    }

    /// A hermetic instance seeded with the bundled example documents. Used by
    /// the offscreen `--shots` renderer and tests so the output never depends
    /// on what happens to be on disk.
    pub fn with_demo_documents() -> Self {
        Self::with_documents(Self::demo_documents(), DEFAULT_INDEX)
    }

    fn demo_documents() -> Vec<Document> {
        SAMPLES
            .iter()
            .map(|s| Document {
                name: s.name.to_string(),
                source: s.source.to_string(),
                from_disk: false,
            })
            .collect()
    }

    /// Build the live library from `dir`: every `.typ` file found there, or a
    /// single blank starter when the directory is empty or absent.
    pub(crate) fn from_library(dir: Option<&std::path::Path>) -> Self {
        let mut documents = Vec::new();
        if let Some(dir) = dir {
            documents = Self::scan_dir(dir);
        }
        if documents.is_empty() {
            documents.push(Document {
                name: "untitled.typ".into(),
                source: String::new(),
                from_disk: false,
            });
        }
        Self::with_documents(documents, 0)
    }

    fn with_documents(documents: Vec<Document>, current: usize) -> Self {
        let current = current.min(documents.len().saturating_sub(1));
        let theme = Theme::dark();
        Self {
            parts: WorldParts::new(),
            worker: None,
            applied_seq: 0,
            editor: SourceEditor::new(),
            preview: PreviewPane::new(),
            diagnostics: DiagnosticsPanel::new(),
            source_id: None,
            documents,
            current,
            io: None,
            background_poll_deadline: Cell::new(None),
            resource_invocation: None,
            pending_resource_io: None,
            resource_result: None,
            initialized: false,
            last_source: String::new(),
            source_cache: String::new(),
            native_edit_pending: false,
            split_ratio: 0.46,
            show_diagnostics: false,
            err_count: 0,
            warn_count: 0,
            elapsed_ms: 0,
            compact: false,
            compact_view: CompactPane::Editor,
            text_k: 1.0,
            demo_reload: false,
            demo_caret_end: false,
            demo_editor_select: false,
            demo_preview_zoom: false,
            demo_preview_pan: false,
            demo_preview_select: false,
            demo_stale: false,
            demo_documents: false,
            chrome: Chrome::new(),
            frame: Frame::of(
                &theme,
                f64::from(theme.tokens.metrics.min_pointer_target),
                0.0,
            ),
            status: ui::StatusBubble::new(),
            compile_feedback: false,
            rect: Rect::ZERO,
            bar_rect: Rect::ZERO,
            editor_rect: Rect::ZERO,
            preview_rect: Rect::ZERO,
            diag_rect: Rect::ZERO,
            status_rect: Rect::ZERO,
            mouse: Point::ZERO,
            dragging_editor: false,
            dragging_preview: false,
            splitter: ResizeHandle::new(),
            dirty: true,
            pointer_seen: false,
            framework_hosted: false,
            document_actions: None,
            local_actions: ui::ActionHost::new(),
        }
    }

    /// Mark this editor as a framework-hosted surface so
    /// [`Widget::resources`] defers to `tilo_resource::publish`.
    #[allow(dead_code, reason = "framework migration test helper")]
    pub(crate) fn set_framework_hosted(&mut self, hosted: bool) {
        self.framework_hosted = hosted;
    }

    /// Whether [`PublishesResources`](ui::resources::PublishesResources) is
    /// publishing only live diagnostics / preview-page extras beside the
    /// framework door.
    #[allow(dead_code, reason = "framework migration test helper")]
    pub(crate) fn is_framework_hosted(&self) -> bool {
        self.framework_hosted
    }

    pub(crate) fn attach_document_actions(
        &mut self,
        dispatcher: ui::framework::Dispatcher<crate::resource::TypstEditor>,
    ) {
        self.document_actions = Some(dispatcher);
    }

    /// Source setup and replacement go through the framework dispatcher when
    /// the surface is attached, and through the local action host otherwise.
    fn apply_source_action(
        &mut self,
        doc: &mut AutoCommit,
        action: &crate::resource::TypstDocAction,
    ) {
        if let Some(dispatcher) = &self.document_actions {
            let _ = dispatcher.apply_now(doc, action);
            return;
        }
        let _ = self.local_actions.try_dispatch_with_context(
            doc,
            <crate::resource::TypstEditor as ui::tilo_resource::Resource>::ACTION_TYPE,
            action,
            ui::tilo_actions::ActionContext::for_action("so.tilo.typst-editor", "Typst", action),
        );
    }

    #[cfg(test)]
    pub(crate) fn take_source_action_for_test(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.local_actions.take_committed()
    }

    /// Live presentation inputs for the resource tree.
    pub(crate) fn resource_ctx(&self) -> crate::actions::TypstResourceCtx<'_> {
        crate::actions::TypstResourceCtx {
            diagnostics: self.diagnostics.diags(),
            page_count: self.preview.page_count(),
        }
    }

    /// Test-only: stage Problems rows and preview pages for hosted extras.
    #[cfg(test)]
    pub(crate) fn stage_live_extras_for_test(
        &mut self,
        diags: Vec<crate::compile::Diagnostic>,
        pages: &[crate::compile::PageImage],
    ) {
        self.diagnostics.set_diagnostics(diags);
        self.preview.set_pages(pages);
    }

    /// Test-only: drop staged diagnostics and pages so hosted extras go empty.
    #[cfg(test)]
    pub(crate) fn clear_live_extras_for_test(&mut self) {
        self.diagnostics.set_diagnostics(Vec::new());
        self.preview.set_pages(&[]);
    }

    /// Test-only: live diagnostic count after staging.
    #[cfg(test)]
    pub(crate) fn live_diagnostic_count_for_test(&self) -> usize {
        self.diagnostics.diags().len()
    }

    /// Test-only: live preview page count after staging.
    #[cfg(test)]
    pub(crate) fn live_page_count_for_test(&self) -> usize {
        self.preview.page_count()
    }

    /// Test-only: replace the local document list with unpublished source.
    #[cfg(test)]
    pub(crate) fn seed_unpublished_source_for_test(&mut self, name: &str, source: &str) {
        self.documents = vec![Document {
            name: name.into(),
            source: source.into(),
            from_disk: false,
        }];
        self.current = 0;
        self.initialized = false;
        self.source_id = None;
    }

    /// Test-only: local document names currently held on the surface.
    #[cfg(test)]
    pub(crate) fn local_document_names_for_test(&self) -> Vec<String> {
        self.documents.iter().map(|doc| doc.name.clone()).collect()
    }

    /// Test-only: current local source body held on the surface.
    #[cfg(test)]
    pub(crate) fn local_source_for_test(&self) -> String {
        self.documents
            .get(self.current)
            .map(|doc| doc.source.clone())
            .unwrap_or_default()
    }

    /// Stage the capture a `--shots` run asks for: the bundled documents in
    /// place of whatever the library holds, then the named scenario. Never
    /// called by the live event loop.
    pub fn stage_capture(&mut self, scenario: Option<&str>) {
        self.documents = Self::demo_documents();
        self.current = DEFAULT_INDEX;
        self.demo_reload = true;
        if let Some(name) = scenario {
            self.apply_demo_scenario(name);
        }
    }

    /// Screenshot-only: pre-arrange a demo instance into a named state so the
    /// offscreen renderer can capture views that normally require interaction.
    /// Never called by the live event loop.
    pub fn apply_demo_scenario(&mut self, scenario: &str) {
        match scenario {
            "preview" => self.compact_view = CompactPane::Preview,
            "documents" => self.demo_documents = true,
            // The report is two pages, so the thumbnail strip shows.
            "pages" => self.current = 1,
            "problems" => self.show_diagnostics = true,
            "long-line" => {
                if let Some(d) = self.documents.get_mut(self.current) {
                    d.name = "notes.typ".to_string();
                    d.source = LONG_LINE_DEMO.to_string();
                }
                self.editor.focused = true;
                self.demo_caret_end = true;
            }
            "errors" => {
                self.show_diagnostics = true;
                if let Some(d) = self.documents.get_mut(self.current) {
                    d.name = "draft.typ".to_string();
                    d.source = crate::samples::ERROR_WARNING.to_string();
                }
            }
            "editor-select" => {
                self.editor.focused = true;
                self.demo_editor_select = true;
            }
            "preview-zoom" => self.demo_preview_zoom = true,
            "preview-pan" => self.demo_preview_pan = true,
            "select" => self.demo_preview_select = true,
            "stale" => self.demo_stale = true,
            "status" => {
                let now = ui::time::now();
                self.status.show("Compiled in 42 ms", now);
                self.status.set_hover(true, now);
            }
            _ => {}
        }
    }

    // ----- CRDT source plumbing -------------------------------------------

    /// Get-or-create the `root["source"]` Text object, seeding it with the
    /// current sample the first time it's created.
    pub(crate) fn ensure_source_obj(&mut self, doc: &mut AutoCommit) -> ObjId {
        let seed = self
            .documents
            .get(self.current)
            .map(|d| d.source.clone())
            .unwrap_or_default();
        self.apply_source_action(doc, &crate::resource::TypstDocAction::EnsureSource { seed });
        match doc.get(&ROOT, "source") {
            Ok(Some((Value::Object(ObjType::Text), id))) => id,
            _ => ROOT,
        }
    }

    fn read_source(&self, doc: &AutoCommit) -> String {
        self.source_id
            .as_ref()
            .and_then(|id| doc.text(id).ok())
            .unwrap_or_default()
    }

    /// Replace the entire CRDT source with `documents[idx]` and reset the
    /// editor view. The recompile is picked up by the next `sync`.
    fn load_document(&mut self, doc: &mut AutoCommit, idx: usize) {
        if self.documents.is_empty() {
            return;
        }
        self.current = idx.min(self.documents.len() - 1);
        // `self.current` was just clamped to a valid index above; `.get()`
        // still avoids indexing so a future refactor can't turn this into a
        // panic.
        let Some(entry) = self.documents.get(self.current) else {
            return;
        };
        let src = entry.source.clone();
        if self.source_id.is_some() {
            self.apply_source_action(
                doc,
                &crate::resource::TypstDocAction::ReplaceSource { text: src },
            );
        }
        self.editor.reset_cursor();
        // Keep focus consistent with the visible pane in compact mode.
        self.editor.focused = !(self.compact && matches!(self.compact_view, CompactPane::Preview));
        // The new document shows its own pages (or its own failure), never the
        // last good render of the one it replaced.
        self.preview.clear();
        self.diagnostics.set_diagnostics(Vec::new());
        self.err_count = 0;
        self.warn_count = 0;
    }

    // ----- compile pipeline -----------------------------------------------

    /// Poll the background worker; apply the freshest result if any arrived.
    /// Called from `draw` so async results are consumed even on frames where
    /// `sync` doesn't run (the App only re-syncs after input).
    fn pump_worker(&mut self) {
        let Some(w) = self.worker.as_mut() else {
            return;
        };
        if let Some(res) = w.poll()
            && res.seq >= self.applied_seq
        {
            self.applied_seq = res.seq;
            self.apply_output(res.output);
        }
    }

    fn apply_output(&mut self, out: CompileOutput) {
        self.elapsed_ms = out.elapsed_ms;
        self.err_count = out.error_count();
        self.warn_count = out.warning_count();
        if out.pages.is_empty() {
            // The compile failed. Keep the last successful render on screen (so
            // the reader doesn't lose their place while fixing a typo) and flag
            // it stale; only fall back to a placeholder when nothing has ever
            // rendered.
            if self.preview.page_count() == 0 {
                self.preview.set_failed();
            }
            self.preview.set_stale(true);
        } else {
            self.preview.set_pages_with_text(&out.pages, &out.page_text);
        }
        self.diagnostics.set_diagnostics(out.diagnostics);
        if self.err_count > 0 {
            // Surface the failure: pop the panel open so the message is visible.
            self.show_diagnostics = true;
        }
        if std::mem::take(&mut self.compile_feedback) {
            if self.err_count > 0 {
                self.set_flash("Compile failed");
            } else {
                self.set_flash(&format!("Compiled in {} ms", self.elapsed_ms));
            }
        }
    }

    /// Queue a fresh compile of the current source. (Syntax highlighting now
    /// lives in the shared editor widget, so there is nothing to re-tokenize
    /// here.)
    /// Compile `source` on the worker, or here when no worker could start
    /// (the browser has no threads for it).
    fn request_compile(&mut self, source: &str) {
        if let Some(w) = self.worker.as_mut() {
            w.request(source.to_string(), PPP);
        } else {
            let out = compile::compile_source(&self.parts, source, PPP);
            self.apply_output(out);
        }
        self.last_source = source.to_string();
    }

    fn force_recompile(&mut self, doc: &AutoCommit) {
        let src = self.read_source(doc);
        self.source_cache.clone_from(&src);
        self.request_compile(&src);
        self.compile_feedback = true;
    }

    // ----- file I/O -------------------------------------------------------

    fn save_dir() -> Option<PathBuf> {
        if let Some(dir) = std::env::var_os("TILO_TYPST_LIBRARY_DIR") {
            return Some(PathBuf::from(dir));
        }
        tilo_paths::document_dir()
            .or_else(tilo_paths::data_dir)
            .map(|d| d.join("typst_editor"))
    }

    /// Open the host Save dialog to choose where to write the current source,
    /// seeded with the document's name (unless a chooser is already in flight).
    /// The chosen path is handled in [`Self::drain_io`].
    fn save(&mut self, _doc: &AutoCommit) {
        if self.io.is_some() {
            return;
        }
        let name = self
            .documents
            .get(self.current)
            .map_or_else(|| "untitled.typ".into(), |d| d.name.clone());
        self.io = Some((PendingIo::SaveTyp, typ_save_request(&name).send()));
        self.set_flash("Choose where to save");
    }

    /// Core: write the current source to `path`. Split out so the portal drain
    /// and tests drive it against a real file.
    fn save_typ_to(&mut self, doc: &AutoCommit, path: &std::path::Path) -> Result<(), String> {
        let src = self.read_source(doc);
        match save_file(path, src.as_bytes()) {
            Ok(()) => {
                self.set_flash(&format!("Saved {}", file_label(path)));
                // Adopt the chosen filename for the current document so a later
                // Save suggests it again.
                if let (Some(name), Some(cur)) = (
                    path.file_name()
                        .and_then(|n| n.to_str())
                        .map(std::string::ToString::to_string),
                    self.documents.get_mut(self.current),
                ) {
                    cur.name = name;
                }
                Ok(())
            }
            Err(e) => {
                self.set_flash(&format!("Save failed: {e}"));
                Err(e.to_string())
            }
        }
    }

    /// Open the host Save dialog to choose where to export the compiled PDF,
    /// seeded with the document's name and a `.pdf` extension (unless a chooser
    /// is already in flight). The chosen path is handled in [`Self::drain_io`].
    fn export_pdf(&mut self, _doc: &AutoCommit) {
        if self.io.is_some() {
            return;
        }
        let stem = self
            .documents
            .get(self.current)
            .map_or_else(|| "untitled.typ".into(), |d| d.name.clone());
        let pdf_name = std::path::Path::new(&stem)
            .with_extension("pdf")
            .to_string_lossy()
            .to_string();
        self.io = Some((PendingIo::ExportPdf, pdf_export_request(&pdf_name).send()));
        self.set_flash("Choose where to export the PDF");
    }

    /// Compile the current source to a PDF and write it to the exact `path`.
    /// Returns an error string on a compile failure or an I/O error.
    fn export_pdf_to_path(&self, doc: &AutoCommit, path: &std::path::Path) -> Result<(), String> {
        let src = self.read_source(doc);
        let bytes = compile::compile_pdf(&self.parts, &src).map_err(|diags| {
            let errs = diags
                .iter()
                .filter(|d| d.kind == compile::DiagKind::Error)
                .count();
            if errs > 0 {
                format!("{} — fix errors first", plural(errs, "error", "errors"))
            } else {
                "could not build PDF".to_string()
            }
        })?;
        tilo_files::file_at(path)
            .and_then(|(dir, name)| {
                tilo_files::blocking::create_dir_all(&dir, &tilo_files::VPath::root())?;
                tilo_files::blocking::save_document(&dir, &name, &bytes)
            })
            .map_err(|e| e.to_string())
    }

    /// Compile the current source to a PDF and write it into `dir`, named after
    /// the open document with a `.pdf` extension. Returns the written path.
    /// Split out so a test can drive it against a scratch directory.
    #[cfg(test)]
    fn export_pdf_to(&self, doc: &AutoCommit, dir: &std::path::Path) -> Result<PathBuf, String> {
        let stem = self
            .documents
            .get(self.current)
            .map_or_else(|| "untitled.typ".into(), |d| d.name.clone());
        let pdf_name = std::path::Path::new(&stem)
            .with_extension("pdf")
            .to_string_lossy()
            .to_string();
        let path = dir.join(pdf_name);
        self.export_pdf_to_path(doc, &path)?;
        Ok(path)
    }

    /// Open the host chooser to pick a `.typ` file to load (unless a chooser is
    /// already in flight). The chosen path is handled in [`Self::drain_io`].
    fn open(&mut self, _doc: &AutoCommit) {
        if self.io.is_some() {
            return;
        }
        self.io = Some((PendingIo::OpenTyp, typ_open_request().send()));
        self.set_flash("Choose a .typ file to open");
    }

    /// Core: load the `.typ` file at `path` into the editor as the current
    /// document. Split out so the portal drain and tests drive it against a
    /// real file.
    fn open_typ_from(
        &mut self,
        doc: &mut AutoCommit,
        path: &std::path::Path,
    ) -> Result<(), String> {
        let src = match read_text(path) {
            Ok(s) => s,
            Err(e) => {
                self.set_flash(&format!("Open failed: {e}"));
                return Err(e.to_string());
            }
        };
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("untitled.typ")
            .to_string();
        // Focus an already-loaded copy of this file, or add it to the document
        // list, then splice its source into the CRDT via `load_document`.
        let idx = if let Some(i) = self
            .documents
            .iter()
            .position(|d| d.name == name && d.from_disk)
        {
            if let Some(existing) = self.documents.get_mut(i) {
                existing.source = src;
            }
            i
        } else {
            self.documents.push(Document {
                name,
                source: src,
                from_disk: true,
            });
            self.documents.len() - 1
        };
        self.load_document(doc, idx);
        let opened = self
            .documents
            .get(idx)
            .map_or_else(String::new, |d| d.name.clone());
        self.set_flash(&format!("Opened {opened}"));
        Ok(())
    }

    /// Drain a finished Open / Save / Export file dialog, if one landed since
    /// the last frame, and run it against the chosen path. The UI thread never
    /// blocks: the portal round-trip lives on its own worker.
    fn drain_io(&mut self, doc: &mut AutoCommit) {
        let Some((op, rx)) = &self.io else { return };
        let op = *op;
        match rx.try_recv() {
            Ok(outcome) => {
                self.io = None;
                let result = match outcome {
                    PortalOutcome::Opened(path) => match op {
                        PendingIo::OpenTyp => self.open_typ_from(doc, &path),
                        PendingIo::SaveTyp => self.save_typ_to(doc, &path),
                        PendingIo::ExportPdf => self.export_pdf_to_path(doc, &path).inspect(|()| {
                            self.set_flash(&format!("Exported {}", file_label(&path)));
                        }),
                    },
                    PortalOutcome::Cancelled => Err("cancelled".into()),
                    PortalOutcome::Failed(e) => {
                        self.set_flash(&format!("File chooser unavailable: {e}"));
                        Err(e)
                    }
                };
                if let Some(invocation) = self.resource_invocation.take() {
                    self.resource_result = Some(match result {
                        Ok(()) => tilo_actions::InvocationUpdate::Succeeded {
                            invocation,
                            output: tilo_actions::ActionOutput::default(),
                        },
                        Err(message) if message == "cancelled" => {
                            tilo_actions::InvocationUpdate::Cancelled {
                                invocation,
                                message: Some(message),
                            }
                        }
                        Err(message) => tilo_actions::InvocationUpdate::Failed {
                            invocation,
                            error: tilo_actions::ActionFailure {
                                code: "typst.io_failed".into(),
                                message,
                                details: None,
                                retryable: false,
                            },
                        },
                    });
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.io = None;
                if let Some(invocation) = self.resource_invocation.take() {
                    self.resource_result = Some(tilo_actions::InvocationUpdate::Failed {
                        invocation,
                        error: tilo_actions::ActionFailure {
                            code: "typst.portal_disconnected".into(),
                            message: "file picker disconnected".into(),
                            details: None,
                            retryable: false,
                        },
                    });
                }
            }
        }
    }

    /// Read every `.typ` file in `dir` into a `Document` list (name-sorted).
    fn scan_dir(dir: &std::path::Path) -> Vec<Document> {
        let Ok(folder) = tilo_files::Dir::open(tilo_files::Location::Native(dir.to_path_buf()))
        else {
            return Vec::new();
        };
        let entries =
            tilo_files::blocking::list(&folder, &tilo_files::VPath::root()).unwrap_or_default();
        // Listings are sorted by name.
        entries
            .into_iter()
            .filter(|entry| {
                entry.kind == tilo_files::Kind::File
                    && std::path::Path::new(&entry.name)
                        .extension()
                        .is_some_and(|x| x == "typ")
            })
            .filter_map(|entry| {
                let name = tilo_files::VPath::new(&entry.name).ok()?;
                let bytes = tilo_files::blocking::read(&folder, &name).ok()?;
                Some(Document {
                    name: entry.name,
                    source: String::from_utf8(bytes).ok()?,
                    from_disk: true,
                })
            })
            .collect()
    }

    fn next_doc(&mut self, doc: &mut AutoCommit) {
        if self.documents.is_empty() {
            return;
        }
        let i = (self.current + 1) % self.documents.len();
        self.load_document(doc, i);
    }

    fn prev_doc(&mut self, doc: &mut AutoCommit) {
        let n = self.documents.len();
        if n == 0 {
            return;
        }
        let i = (self.current + n - 1) % n;
        self.load_document(doc, i);
    }

    /// Carry out what a control asked for. Any control that acts closes the
    /// document list it may have left open.
    fn run_command(&mut self, command: Command, doc: &mut AutoCommit) {
        use crate::actions::TypstOperation;
        match command {
            Command::PrevDoc => self.prev_doc(doc),
            Command::NextDoc => self.next_doc(doc),
            Command::SelectDoc(index) => {
                if index != self.current && index < self.documents.len() {
                    self.load_document(doc, index);
                }
            }
            Command::Compile => self.perform_resource_operation(TypstOperation::Compile, doc),
            Command::Save => {
                self.perform_resource_operation(TypstOperation::Save { path: None }, doc);
            }
            Command::Open => {
                self.perform_resource_operation(TypstOperation::Open { path: None }, doc);
            }
            Command::ExportPdf => {
                self.perform_resource_operation(TypstOperation::ExportPdf { path: None }, doc);
            }
            Command::ZoomOut => {
                self.perform_resource_operation(TypstOperation::ZoomOut, doc);
                self.set_compact_view(CompactPane::Preview);
            }
            Command::ZoomIn => {
                self.perform_resource_operation(TypstOperation::ZoomIn, doc);
                self.set_compact_view(CompactPane::Preview);
            }
            Command::Fit => {
                self.perform_resource_operation(TypstOperation::FitWidth, doc);
                self.set_compact_view(CompactPane::Preview);
            }
            Command::ToggleProblems => {
                self.perform_resource_operation(TypstOperation::ToggleProblems, doc);
            }
            Command::ShowEditor => {
                self.perform_resource_operation(TypstOperation::ShowEditor, doc);
            }
            Command::ShowPreview => {
                self.perform_resource_operation(TypstOperation::ShowPreview, doc);
            }
        }
    }

    /// Execute the semantic operation shared by exported resources and direct
    /// toolbar/shortcut affordances.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "operations are decoded as owned values and dispatched immediately"
    )]
    fn perform_resource_operation(
        &mut self,
        operation: crate::actions::TypstOperation,
        doc: &AutoCommit,
    ) {
        use crate::actions::TypstOperation;
        match operation {
            TypstOperation::Compile => self.force_recompile(doc),
            TypstOperation::Save { .. } => self.save(doc),
            TypstOperation::Open { .. } => self.open(doc),
            TypstOperation::ExportPdf { .. } => self.export_pdf(doc),
            TypstOperation::ZoomOut => self.preview.zoom_out(),
            TypstOperation::ZoomIn => self.preview.zoom_in(),
            TypstOperation::FitWidth => self.preview.fit_width(),
            TypstOperation::ToggleProblems => {
                self.show_diagnostics = !self.show_diagnostics;
            }
            TypstOperation::ShowEditor => self.set_compact_view(CompactPane::Editor),
            TypstOperation::ShowPreview => self.set_compact_view(CompactPane::Preview),
            TypstOperation::GoToDiagnostic { index } => {
                if let Some(diag) = self.diagnostics.diags().get(index)
                    && let Some(line) = diag.line
                {
                    let text = self.read_source(doc);
                    self.editor.goto_line(&text, line);
                    self.show_diagnostics = true;
                    if self.compact {
                        self.set_compact_view(CompactPane::Editor);
                    } else {
                        self.editor.focused = true;
                    }
                }
            }
            TypstOperation::GoToPage { page } => {
                self.preview.scroll_to_page(page);
                if self.compact {
                    self.set_compact_view(CompactPane::Preview);
                }
            }
        }
    }

    /// Switch the compact single-pane view. Keeps editor focus in sync so the
    /// caret only blinks (and only demands animation frames) while editing.
    fn set_compact_view(&mut self, view: CompactPane) {
        self.compact_view = view;
        self.editor.focused = matches!(view, CompactPane::Editor);
    }

    /// Announce something worth noticing once: the transient bubble shows it
    /// and hides it again.
    fn set_flash(&mut self, msg: &str) {
        self.status.show(msg, ui::time::now());
        self.dirty = true;
    }

    /// 1-based (line, col) of the editor caret within the cached source.
    /// Single pass over the leading chars — no allocation.
    fn caret_line_col(&self) -> (usize, usize) {
        let caret = self.editor.caret().min(self.source_cache.len());
        let mut line = 1usize;
        let mut col = 1usize;
        // Walk char boundaries directly rather than byte-slicing the source —
        // `caret` is a byte offset and isn't guaranteed to land on one.
        for (i, ch) in self.source_cache.char_indices() {
            if i >= caret {
                break;
            }
            if ch == '\n' {
                line += 1;
                col = 1;
            } else {
                col += 1;
            }
        }
        (line, col)
    }

    fn compiling(&self) -> bool {
        self.worker
            .as_ref()
            .is_some_and(super::worker::CompileWorker::in_flight)
    }

    /// Compute every pane rect from the window rect. Split out from
    /// [`Widget::apply_layout`] so resize reflow is unit-testable without a
    /// taffy pass. The layout mode is re-derived from `rect`'s width every call
    /// — never cached from the first frame.
    fn layout_in(&mut self, rect: Rect) {
        self.rect = rect;
        let r = self.rect;
        let frame = self.frame;
        // The band of controls on top and the status bar at the bottom each
        // reserve their height, so nothing is painted under either.
        self.bar_rect = Rect::new(r.x0, r.y0, r.x1, (r.y0 + frame.bar_h).min(r.y1));
        let status_top = (r.y1 - frame.status_h).max(self.bar_rect.y1);
        self.status_rect = Rect::new(r.x0, status_top, r.x1, r.y1);
        let body = Rect::new(r.x0, self.bar_rect.y1, r.x1, status_top);
        let content_w = r.width();

        // Derive the layout mode from the *current* width every pass, so a
        // resize across the threshold reflows without any cached-on-launch mode.
        // The breakpoint was designed at 1×: a window holds as much 2× text as
        // a half-width 1× window, so compare in 1× space.
        self.compact = content_w / self.text_k.max(f64::from(f32::EPSILON)) < frame.compact_w;

        if self.compact {
            self.splitter.set_neighbour_rects(Rect::ZERO, Rect::ZERO);
            let (main, diag) = self.split_off_diagnostics(body);
            self.diag_rect = diag;
            match self.compact_view {
                CompactPane::Editor => {
                    self.editor_rect = main;
                    self.preview_rect = Rect::ZERO;
                }
                CompactPane::Preview => {
                    self.preview_rect = main;
                    self.editor_rect = Rect::ZERO;
                }
            }
        } else {
            let split_x = (r.x0 + content_w * self.split_ratio).round();
            let editor_col = Rect::new(r.x0, body.y0, split_x, body.y1);
            self.preview_rect = Rect::new(split_x, body.y0, r.x1, body.y1);
            self.splitter.set_strip_usable_px(content_w);
            self.splitter
                .set_neighbour_rects(editor_col, self.preview_rect);
            let (main, diag) = self.split_off_diagnostics(editor_col);
            self.editor_rect = main;
            self.diag_rect = diag;
        }

        self.editor.set_rect(self.editor_rect);
        self.preview.set_rect(self.preview_rect);
        self.diagnostics.set_rect(self.diag_rect);
        self.stage_views();
    }

    /// Screenshot harness only: now that the panes have real rects (and the
    /// preview has pages), arrange the caret, zoom, scroll or selection the
    /// capture is meant to show.
    fn stage_views(&mut self) {
        if self.demo_caret_end && !self.source_cache.is_empty() {
            self.demo_caret_end = false;
            let src = self.source_cache.clone();
            self.editor.set_caret(src.len());
            self.editor.ensure_visible(&src);
        }
        if self.demo_editor_select && !self.source_cache.is_empty() {
            self.demo_editor_select = false;
            self.editor.demo_select();
        }
        if self.demo_preview_zoom && self.preview.page_count() > 0 {
            self.demo_preview_zoom = false;
            self.preview.demo_set_view(1.9, 0.42);
        }
        if self.demo_preview_pan && self.preview.page_count() > 0 {
            self.demo_preview_pan = false;
            // Zoom well past fit-width and pan toward the lower-right so the
            // capture shows the page displaced on both axes.
            self.preview.demo_pan_view(2.4, 0.72, 0.55);
        }
        if self.demo_preview_select && self.preview.page_count() > 0 {
            self.demo_preview_select = false;
            self.preview.demo_select_fraction(0.0, 0.03);
        }
        if self.demo_documents {
            self.demo_documents = false;
            self.chrome.open_picker();
        }
    }

    /// Screenshot-only: add an error to the source and compile it at once, so
    /// the capture shows a failed compile over the last good render.
    fn stage_broken_source(&mut self, doc: &mut AutoCommit, src_id: &ObjId) {
        if !std::mem::take(&mut self.demo_stale) || self.preview.page_count() == 0 {
            return;
        }
        // The broken line goes right under the first heading, so the squiggle
        // and the gutter marker are on screen beside the preview's last good
        // render.
        let source = self.read_source(doc);
        let mut lines: Vec<&str> = source.lines().collect();
        let at = lines
            .iter()
            .position(|line| line.starts_with("= "))
            .map_or(lines.len(), |heading| heading + 1);
        lines.splice(at..at, ["", "#undefined_value"]);
        let broken = format!("{}\n", lines.join("\n"));
        self.apply_source_action(
            doc,
            &crate::resource::TypstDocAction::ReplaceSource {
                text: broken.clone(),
            },
        );
        self.editor.bind(src_id.clone(), doc);
        let out = compile::compile_source(&self.parts, &broken, PPP);
        self.apply_output(out);
        self.last_source.clone_from(&broken);
        self.source_cache = broken;
    }

    /// Carve the Problems panel out of the bottom of `col` when it is open
    /// and there is room: as tall as its rows need, at most half the column.
    fn split_off_diagnostics(&self, col: Rect) -> (Rect, Rect) {
        let cap = (col.height() * 0.5).floor();
        if !self.show_diagnostics || cap < 2.0 * self.frame.bar_h {
            return (col, Rect::ZERO);
        }
        let wanted = self.diagnostics.preferred_height();
        let h = if wanted > 0.0 {
            wanted.min(cap).ceil()
        } else {
            cap
        };
        (
            Rect::new(col.x0, col.y0, col.x1, col.y1 - h),
            Rect::new(col.x0, col.y1 - h, col.x1, col.y1),
        )
    }

    /// Measure the chrome in `theme` and re-fit everything that follows from
    /// it: the panes, the panel's rows, and where each control sits.
    fn refresh_frame(&mut self, tr: &mut TextRenderer, theme: &Theme) {
        let status_line = tr
            .measure_style("Ag", None, &theme.text(ui::tokens::TextRole::Caption))
            .1;
        let frame = Frame::of(
            theme,
            Chrome::control_height(tr, theme),
            f64::from(status_line),
        );
        let text_k = f64::from(tr.text_scale()).max(f64::from(f32::EPSILON));
        self.preview.apply_theme(theme);
        let panel_before = self.diagnostics.preferred_height();
        self.diagnostics.measure_rows(tr, theme);
        let changed = frame != self.frame
            || (text_k - self.text_k).abs() > 1e-6
            || (panel_before - self.diagnostics.preferred_height()).abs() > 0.01;
        if changed {
            self.frame = frame;
            self.text_k = text_k;
            self.layout_in(self.rect);
        }

        let names: Vec<&str> = self.documents.iter().map(|d| d.name.as_str()).collect();
        self.chrome.sync(
            &names,
            self.current,
            self.err_count + self.warn_count,
            self.show_diagnostics,
            matches!(self.compact_view, CompactPane::Preview),
        );
        self.chrome
            .place(self.bar_rect, self.compact, names.len(), tr, theme);
        let close = (self.show_diagnostics && self.diag_rect.height() > 1.0)
            .then(|| self.diagnostics.close_rect(theme));
        self.chrome.place_close(close);
    }

    // ----- drawing --------------------------------------------------------

    /// What the person reads while working, left: where the caret is.
    fn status_left_label(&self) -> String {
        let (ln, col) = self.caret_line_col();
        format!("Ln {ln}, Col {col}")
    }

    /// What the person reads while working, right: the document's pages and
    /// the preview's zoom.
    fn status_right_label(&self) -> String {
        let pages = self.preview.page_count();
        if pages == 0 {
            return "No pages".to_string();
        }
        format!(
            "{}, {:.0}%",
            plural(pages, "page", "pages"),
            self.preview.zoom() * 100.0
        )
    }

    /// The area the transient bubble floats in: the content above the status
    /// bar.
    fn bubble_area(&self) -> Rect {
        Rect::new(
            self.rect.x0,
            self.bar_rect.y1,
            self.rect.x1,
            self.status_rect.y0,
        )
    }
}

/// A file's name as the person knows it, for a transient announcement.
fn file_label(path: &std::path::Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .map_or_else(|| path.display().to_string(), str::to_owned)
}

impl Default for TypstApp {
    fn default() -> Self {
        Self::new()
    }
}

/// Format a count with the grammatically correct noun: `1 page`, `2 pages`.
pub(crate) fn plural(n: usize, singular: &str, plural: &str) -> String {
    if n == 1 {
        format!("{n} {singular}")
    } else {
        format!("{n} {plural}")
    }
}

impl Widget for TypstApp {
    ui::provides!(
        dyn ui::history_mode::capability::DocumentHistory,
        dyn ui::widget::controls::ActionControls,
        dyn ui::resources::PublishesResources,
        dyn ui::undo::UndoPoints
    );

    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[crate::actions::OPERATION_TYPE]
    }

    fn start_operation(
        &mut self,
        op: &ui::widget::ResourceOperation<'_>,
        doc: &mut AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        let Some(request) = op.unscoped() else {
            return ui::widget::start_in_children(self, op, doc, path);
        };
        if let Some(started) = ui::widget::start_in_children(self, op, doc, path)? {
            return Ok(Some(started));
        }
        if let Some(started) = self.admit_resource_operation(
            request.invocation,
            request.kind,
            request.type_id,
            request.payload,
            doc,
            path,
        )? {
            return Ok(Some(started));
        }
        Ok(self
            .answer_resource_operation(request.kind, request.type_id, request.payload, doc, path)?
            .then(|| {
                ui::widget::ResourceOperationStart::Completed(
                    ui::tilo_actions::ActionOutput::default(),
                )
            }))
    }

    fn poll_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        cancellation_requested: bool,
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Option<tilo_actions::InvocationUpdate> {
        if cancellation_requested {
            if self
                .pending_resource_io
                .as_ref()
                .is_some_and(|(id, _)| *id == invocation)
            {
                self.pending_resource_io = None;
                return Some(tilo_actions::InvocationUpdate::Cancelled {
                    invocation,
                    message: Some("cancelled before file I/O".into()),
                });
            }
            if self.resource_invocation == Some(invocation) {
                self.resource_invocation = None;
                self.io = None;
                return Some(tilo_actions::InvocationUpdate::Cancelled {
                    invocation,
                    message: Some("file selection cancelled".into()),
                });
            }
        }
        self.resource_result
            .as_ref()
            .is_some_and(|u| u.invocation() == invocation)
            .then(|| self.resource_result.take())
            .flatten()
    }

    fn sync(&mut self, doc: &mut AppModel, path: &ModelPath) {
        let (doc, _) = ui::doc::document(doc, path);
        self.background_poll_deadline.set(None);
        if let Some((invocation, operation)) = self.pending_resource_io.take() {
            let result = match operation {
                ResourceIo::Open(path) => self.open_typ_from(doc, &path),
                ResourceIo::Save(path) => self.save_typ_to(doc, &path),
                ResourceIo::ExportPdf(path) => self.export_pdf_to_path(doc, &path),
            };
            self.resource_result = Some(match result {
                Ok(()) => tilo_actions::InvocationUpdate::Succeeded {
                    invocation,
                    output: tilo_actions::ActionOutput::default(),
                },
                Err(message) => tilo_actions::InvocationUpdate::Failed {
                    invocation,
                    error: tilo_actions::ActionFailure {
                        code: "typst.io_failed".into(),
                        message,
                        details: None,
                        retryable: false,
                    },
                },
            });
        }
        let src_id = self.ensure_source_obj(doc);
        self.source_id = Some(src_id.clone());
        // Land a finished Open/Save/Export file dialog before reading the
        // source, so a freshly-opened document is spliced in and compiles this
        // frame.
        self.drain_io(doc);
        // A capture swaps in the bundled documents: load the one it shows and
        // compile it before the frame is drawn.
        if std::mem::take(&mut self.demo_reload) {
            self.load_document(doc, self.current);
            self.initialized = false;
        }
        // Bind the shared editor to the source object and pull its content.
        self.editor.bind(src_id.clone(), doc);
        let source = doc.text(&src_id).unwrap_or_default();

        // In the browser the fonts arrive after the editor opens; compile
        // once they have, with parts built from them.
        if *self.parts.fonts() == compile::Fonts::Loading {
            if WorldParts::fonts_loading() {
                self.source_cache = source;
                return;
            }
            self.parts = WorldParts::new();
            self.worker = None;
            self.initialized = false;
        }

        if !self.initialized {
            // First frame: compile synchronously so the very first paint
            // (including `--shots`) has rendered pages, then bring the async
            // worker online for subsequent edits.
            let out = compile::compile_source(&self.parts, &source, PPP);
            self.apply_output(out);
            self.worker = CompileWorker::spawn(self.parts.clone());
            self.last_source = source.clone();
            self.source_cache = source;
            self.initialized = true;
            self.stage_broken_source(doc, &src_id);
            return;
        }

        if source != self.last_source {
            self.request_compile(&source);
        }
        self.source_cache = source;
        // Consume any finished async compile.
        self.pump_worker();
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // Replaced controls may be dropped now: this pass rebuilds the tree
        // (and `node_map`) without them.
        self.chrome.layout_rebuilt();
        // One taffy node per hosted control, in the order `children` returns
        // them, so the host's focus walk lines up. The controls are placed by
        // hand in `draw`, so their taffy geometry is unused: the nodes exist
        // so each control is registered in `node_map` (a11y, focus, events).
        let child_nodes: Vec<NodeId> = self
            .chrome
            .children_mut()
            .into_iter()
            .map(|child| child.build_layout(taffy, theme, node_map))
            .collect();
        // `new_with_children` only fails when a child belongs to a different
        // tree; every entry in `child_nodes` was just built in this one above.
        #[allow(
            clippy::unwrap_used,
            reason = "every child node was just created in this same tree"
        )]
        let node = taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::percent(1.0),
                    },
                    ..Default::default()
                },
                &child_nodes,
            )
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let Ok(layout) = taffy.layout(node) else {
            return;
        };
        let rect = Rect::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
            origin.x + f64::from(layout.location.x) + f64::from(layout.size.width),
            origin.y + f64::from(layout.location.y) + f64::from(layout.size.height),
        );
        self.layout_in(rect);
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.chrome.children()
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.chrome.children_mut()
    }

    fn take_structure_dirty(&mut self) -> bool {
        self.chrome.take_structure_dirty()
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // Consume any finished async compile up front so its pages paint
        // this very frame (draw runs on animation frames even when sync
        // doesn't).
        self.pump_worker();
        // Measure the chrome in this frame's theme and text scale; a change
        // re-fits the panes now, so this very frame paints coherently.
        self.refresh_frame(tr, theme);

        // The window ground is the framework's. The preview stands on the
        // inspector region; the editor on the ground itself.
        if self.preview_rect.area() > 0.0 {
            ui::surface::region(scene, t, self.preview_rect, Region::Inspector, theme);
        }
        ui::widget::toolbar::paint_band(scene, t, theme, self.bar_rect);
        self.chrome.draw(scene, t, tr, theme, presence);

        if self.editor_rect.area() > 0.0 {
            self.editor
                .draw(scene, t, tr, theme, self.diagnostics.diags());
        }
        if self.preview_rect.area() > 0.0 {
            self.preview
                .draw(scene, t, tr, &theme.in_region(Region::Inspector));
        }
        if !self.compact {
            self.splitter.draw(scene, t, tr, theme, presence);
        }
        if self.show_diagnostics && self.diag_rect.height() > 1.0 {
            self.diagnostics.draw(scene, t, tr, theme);
        }

        ui::chrome::status_bar::draw(
            scene,
            t,
            tr,
            theme,
            self.status_rect,
            &self.status_left_label(),
            &self.status_right_label(),
        );
        let now = ui::time::now();
        let area = self.bubble_area();
        if self.pointer_seen {
            self.status.pointer_moved(area, self.mouse, tr, theme, now);
        }
        self.status.paint(scene, t, tr, theme, area, now);

        // This frame reflects the latest interaction state; clear the move-dirty
        // flag so idle hover doesn't spin the render loop.
        self.dirty = false;
    }

    fn draw_with_presence(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
        _permissions: &PermissionContext,
    ) {
        // Root rect is the whole window — always paint, never cull.
        self.draw(scene, transform, text_renderer, theme, presence);
    }

    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // The diagnostic's hover card and the controls' open lists float
        // above both panes.
        self.editor.draw_tooltip(
            scene,
            transform,
            text_renderer,
            theme,
            self.diagnostics.diags(),
        );
        for child in self.chrome.children_mut() {
            child.draw_overlay(scene, transform, text_renderer, theme, presence);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        let src_id = match &self.source_id {
            Some(id) => id.clone(),
            None => self.ensure_source_obj(ctx.state),
        };

        // The controls see the event first. Keys reach them only while the
        // editor is not typing, or while one holds an open list.
        let already_handled = *ctx.handled;
        let is_key = matches!(event, WindowEvent::KeyboardInput { .. });
        if !is_key || !self.editor.focused || self.chrome.captures_input() {
            // The layout tree may lag the controls by a frame when one was
            // just shown or hidden; a control with no node yet is addressed
            // by the window's own.
            let nodes = ctx.taffy.children(ctx.node_id).unwrap_or_default();
            for (index, child) in self.chrome.children_mut().into_iter().enumerate() {
                let node = nodes.get(index).copied().unwrap_or(ctx.node_id);
                child.handle_event(event, &mut ctx.reborrow(ctx.path, node));
                if *ctx.handled && !already_handled {
                    break;
                }
            }
        }
        for command in self.chrome.take() {
            self.run_command(command, ctx.state);
            self.dirty = true;
        }
        let claimed = *ctx.handled && !already_handled;
        if !self.compact && !claimed {
            self.route_to_splitter(event, ctx);
        }
        let moved = matches!(event, WindowEvent::CursorMoved { .. });
        if *ctx.handled && !already_handled && !moved {
            return;
        }

        // --- application-level keyboard shortcuts ---
        if let WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key,
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } = event
        {
            self.handle_keyboard_shortcut(
                logical_key,
                event,
                *ctx.modifiers,
                ctx.state,
                &src_id,
                ctx.handled,
            );
            return;
        }

        // --- pointer ---
        self.handle_pointer_event(
            event,
            ctx.scale_factor,
            *ctx.modifiers,
            ctx.state,
            &src_id,
            ctx.handled,
        );
    }

    fn name(&self) -> &'static str {
        "TypstEditor"
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        if self.compact {
            return None;
        }
        self.splitter.cursor_shape(point)
    }

    /// Source and preview are custom-painted panes rather than retained child
    /// widgets. Export both with their current compile state so readers can
    /// understand the split/compact view without inferring it from pixels. The
    /// controls above them are retained widgets and name themselves.
    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        use ui::a11y::{AccessNode, AccessState, Role};
        let source = AccessNode::new(Role::MultilineTextInput)
            .with_name("Typst source")
            .with_value(self.source_cache.clone())
            .with_state(AccessState::new().focusable(true).multiline(true))
            .with_action_target("typst.source");
        let preview = AccessNode::new(Role::Canvas)
            .with_name("Document preview")
            .with_value(format!(
                "Page {} of {}; zoom {:.0}%",
                self.preview.current_page().saturating_add(1),
                self.preview.page_count(),
                self.preview.zoom() * 100.0
            ))
            .with_description(if self.compact {
                "Compact layout; switch between source and preview with the Edit and Preview control"
            } else {
                "Live rendered preview"
            })
            .with_semantic_key("typst.preview")
            .with_hashed_key("typst.preview");
        let mut children = vec![source, preview];
        if self.show_diagnostics {
            children.extend(self.diagnostics.accessibility());
        }
        Some(
            AccessNode::new(Role::Document)
                .with_name("Typst editor")
                .with_description("Editable Typst source with a live document preview")
                .with_state(AccessState::new().focusable(true))
                .with_children(children),
        )
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        doc: &mut AppModel,
        path: &ModelPath,
    ) -> bool {
        let (doc, _) = ui::doc::document(doc, path);
        // A listed problem: activating it jumps to where it is.
        if let Some(index) = target
            .strip_prefix("typst.problem.")
            .and_then(|index| index.parse::<usize>().ok())
        {
            return match action {
                ui::a11y::Action::Focus => true,
                ui::a11y::Action::Activate => {
                    self.perform_resource_operation(
                        crate::actions::TypstOperation::GoToDiagnostic { index },
                        doc,
                    );
                    true
                }
                _ => false,
            };
        }
        if target != "typst.source" {
            return false;
        }
        match action {
            ui::a11y::Action::Focus => {
                if self.compact {
                    self.set_compact_view(CompactPane::Editor);
                }
                self.editor.focused = true;
                true
            }
            ui::a11y::Action::SetValue(source) => {
                self.apply_source_action(
                    doc,
                    &crate::resource::TypstDocAction::ReplaceSource {
                        text: source.clone(),
                    },
                );
                let source_id = self.ensure_source_obj(doc);
                self.source_id = Some(source_id);
                self.source_cache.clone_from(source);
                self.editor.set_caret(source.len());
                self.native_edit_pending = true;
                true
            }
            _ => false,
        }
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        if self.io.is_none() && self.pending_resource_io.is_none() && !self.compiling() {
            return None;
        }
        // Poll completed work independently of animation preferences. Repeated
        // queries retain the same deadline until sync consumes it.
        let deadline = self.background_poll_deadline.get().unwrap_or_else(|| {
            ui::time::now()
                .checked_add(std::time::Duration::from_millis(16))
                .unwrap_or_else(ui::time::now)
        });
        self.background_poll_deadline.set(Some(deadline));
        Some(deadline)
    }

    fn needs_animation_frame(&self) -> bool {
        // Keep the App pumping frames while: the editor caret is blinking,
        // a compile is in flight (so `draw` can consume its result), the
        // transient bubble is lingering or fading (so it hides itself without
        // a stray pointer event), or a control is animating.
        self.editor.wants_animation()
            || self.compiling()
            || self.status.needs_frame(ui::time::now())
            || self
                .chrome
                .children()
                .iter()
                .any(|child| child.needs_animation_frame())
    }

    fn needs_redraw(&self) -> bool {
        // Pointer *moves* are only repainted when `needs_redraw` is true (the
        // host gates them to avoid hover spam). A live drag or a hover change
        // sets `dirty`, so divider drags, preview text-selection, and inline
        // diagnostic tooltips all repaint smoothly.
        self.dirty || self.chrome.needs_redraw()
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        self.chrome.mark_drawn();
    }
}

impl ui::undo::UndoPoints for TypstApp {
    fn needs_commit(&self) -> bool {
        self.native_edit_pending
    }
    fn take_commit_context(&mut self) -> Option<ui::tilo_actions::ActionContext> {
        self.native_edit_pending.then(|| {
            let document = self
                .documents
                .get(self.current)
                .map_or("Untitled.typ", |document| document.name.as_str());
            ui::tilo_actions::ActionContext::for_resource(
                "Edit source",
                format!("typst.document:{document}"),
                "typst.document",
                document,
            )
            .with_app("so.tilo.typst-editor", "Typst")
            .with_facet(ui::tilo_actions::ActionFacet::new("file-type", "Typst"))
        })
    }
    fn on_commit(&mut self) {
        self.native_edit_pending = false;
    }
}

impl ui::resources::PublishesResources for TypstApp {
    fn resources(&self, _doc: &AutoCommit) -> Vec<tilo_actions::Resource> {
        let ctx = self.resource_ctx();
        // [`tilo_resource::publish`] owns the workspace door on every path.
        // Diagnostics and preview pages are live presentation only — hosted
        // returns that slice alone, never a second workspace root.
        if self.framework_hosted {
            return crate::actions::live_extras(&ctx);
        }
        crate::actions::resources(&ctx)
    }

    fn resource(&self, _doc: &AutoCommit, id: &str) -> Option<tilo_actions::Resource> {
        if self.framework_hosted && id == "typst.workspace" {
            return None;
        }
        let ctx = self.resource_ctx();
        find_resource(&crate::actions::resources(&ctx), id).or_else(|| {
            self.framework_hosted.then(|| {
                crate::actions::live_extras(&ctx)
                    .into_iter()
                    .find(|resource| resource.id.as_str() == id)
            })?
        })
    }
}

impl ui::widget::controls::ActionControls for TypstApp {
    fn control_action_bindings(&self) -> Vec<ui::widget::ControlBindingMetadata> {
        ui::widget::semantic_action_bindings(self.accessibility())
    }
}

impl ui::history_mode::capability::DocumentHistory for TypstApp {
    fn document_history_definition(&self) -> Option<&'static ui::history_mode::HistoryDefinition> {
        Some(&crate::framework_history::HISTORY)
    }
}

impl TypstApp {
    fn answer_resource_operation(
        &mut self,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<bool, String> {
        if kind == tilo_actions::OperationKind::Document {
            return Ok(false);
        }
        let Some(operation) = crate::actions::decode(type_id, payload)? else {
            return Ok(false);
        };
        self.perform_resource_operation(operation, doc);
        Ok(true)
    }

    fn admit_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        use crate::actions::TypstOperation;
        let Some(operation) = crate::actions::decode(type_id, payload)? else {
            return Ok(None);
        };
        if self.resource_invocation.is_some() || self.pending_resource_io.is_some() {
            return Err("another file operation is already running".into());
        }
        let resource = match operation {
            TypstOperation::Open { path: Some(path) } => Some(ResourceIo::Open(path.into())),
            TypstOperation::Save { path: Some(path) } => Some(ResourceIo::Save(path.into())),
            TypstOperation::ExportPdf { path: Some(path) } => {
                Some(ResourceIo::ExportPdf(path.into()))
            }
            TypstOperation::Open { path: None } => {
                self.open(doc);
                None
            }
            TypstOperation::Save { path: None } => {
                self.save(doc);
                None
            }
            TypstOperation::ExportPdf { path: None } => {
                self.export_pdf(doc);
                None
            }
            other => {
                return self
                    .answer_resource_operation(
                        kind,
                        type_id,
                        &serde_json::to_vec(&other).map_err(|e| e.to_string())?,
                        doc,
                        path,
                    )
                    .map(|handled| handled.then(ui::tilo_actions::ActionOutput::default))
                    .map(|v| v.map(ui::widget::ResourceOperationStart::Completed));
            }
        };
        if let Some(resource) = resource {
            self.pending_resource_io = Some((invocation, resource));
        } else {
            self.resource_invocation = Some(invocation);
        }
        Ok(Some(ui::widget::ResourceOperationStart::Accepted))
    }
}

impl TypstApp {
    /// Application-level keyboard shortcuts for a pressed key. Split out of
    /// `Widget::handle_event` to keep it within the line-count lint; the
    /// caller already matched `event` down to a pressed `KeyEvent` and passes
    /// its `logical_key` through. Falls through to the focused editor's own
    /// key handling for anything that isn't an app-level shortcut.
    fn handle_keyboard_shortcut(
        &mut self,
        logical_key: &Key,
        event: &WindowEvent,
        modifiers: Modifiers,
        doc: &mut AutoCommit,
        src_id: &ObjId,
        handled: &mut bool,
    ) {
        let mods = modifiers.state();
        let ctrl = mods.control_key() || mods.super_key();
        if ctrl {
            if matches!(logical_key, Key::Named(NamedKey::Enter)) {
                self.force_recompile(doc);
                *handled = true;
                return;
            }
            if let Key::Character(ch) = logical_key {
                match ch.to_lowercase().as_str() {
                    "s" => {
                        self.save(doc);
                        *handled = true;
                        return;
                    }
                    "o" => {
                        self.open(doc);
                        *handled = true;
                        return;
                    }
                    "e" => {
                        self.export_pdf(doc);
                        *handled = true;
                        return;
                    }
                    "=" | "+" => {
                        self.preview.zoom_in();
                        *handled = true;
                        return;
                    }
                    "-" => {
                        self.preview.zoom_out();
                        *handled = true;
                        return;
                    }
                    "0" => {
                        self.preview.fit_width();
                        *handled = true;
                        return;
                    }
                    "]" => {
                        self.next_doc(doc);
                        *handled = true;
                        return;
                    }
                    "[" => {
                        self.prev_doc(doc);
                        *handled = true;
                        return;
                    }
                    "j" => {
                        self.show_diagnostics = !self.show_diagnostics;
                        *handled = true;
                        return;
                    }
                    "c" => {
                        // When the rendered preview holds focus, Ctrl+C
                        // copies the selected document text; otherwise fall
                        // through to the editor's own clipboard.
                        if !self.editor.focused {
                            if let Some(txt) = self.preview.selected_text() {
                                ui::clipboard_copy(&txt);
                                self.set_flash("Copied selection");
                            }
                            *handled = true;
                            return;
                        }
                    }
                    "a"
                        // Select-all in the preview when it holds focus.
                        if !self.editor.focused => {
                            self.preview.select_all();
                            self.dirty = true;
                            *handled = true;
                            return;
                        }
                    // remaining a/c/x/v fall through to the editor's clipboard
                    _ => {}
                }
            }
        }
        // Route remaining keys to the focused editor.
        if self.editor.focused {
            let before = doc.pending_ops();
            if self.editor.handle_key(event, &modifiers, doc, src_id) {
                *handled = true;
                self.native_edit_pending |= doc.pending_ops() > before;
            }
        }
    }

    /// Pointer (cursor move / click / release / wheel) handling. Split out of
    /// `Widget::handle_event` to keep it within the line-count lint; preserves
    /// the original control flow exactly.
    fn handle_pointer_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: Modifiers,
        doc: &AutoCommit,
        src_id: &ObjId,
        handled: &mut bool,
    ) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.handle_cursor_moved(position, scale_factor, doc, src_id, handled);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.handle_left_press(modifiers, doc, src_id, handled);
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                self.dragging_editor = false;
                self.dragging_preview = false;
                self.editor.on_mouse_up();
                self.preview.on_mouse_up();
                self.dirty = true;
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let p = self.mouse;
                // The preview owns its own wheel mapping via the shared viewport
                // kernel: Ctrl/Cmd+wheel = cursor-anchored zoom, plain wheel =
                // scroll/pan (incl. a tilt wheel when the page is tall).
                if self.preview_rect.area() > 0.0 && self.preview_rect.contains(p) {
                    self.preview
                        .on_wheel(delta, &modifiers.state(), p, scale_factor);
                } else {
                    let (mut dx, mut dy) = match delta {
                        MouseScrollDelta::LineDelta(x, y) => {
                            (-f64::from(*x) * 48.0, -f64::from(*y) * 48.0)
                        }
                        MouseScrollDelta::PixelDelta(pp) => (-pp.x, -pp.y),
                    };
                    // Shift turns a vertical wheel into horizontal scroll — the
                    // usual convention for a mouse with no horizontal axis.
                    if modifiers.state().shift_key() && dx == 0.0 {
                        dx = dy;
                        dy = 0.0;
                    }
                    if self.editor_rect.contains(p) {
                        let text = doc.text(src_id).unwrap_or_default();
                        self.editor.scroll_by(dx as f32, dy as f32, &text);
                    } else if self.show_diagnostics && self.diag_rect.contains(p) {
                        self.diagnostics.scroll_by(dy);
                    }
                }
                self.dirty = true;
                *handled = true;
            }
            _ => {}
        }
    }

    /// Give the pane splitter the event and apply the split it asks for.
    fn route_to_splitter(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        let (hovered, dragging) = (self.splitter.is_hovered(), self.splitter.is_dragging());
        self.splitter.handle_event(event, ctx);
        if let Some((editor_w, _)) = self.splitter.take_pending() {
            let width = self.rect.width().max(1.0);
            self.split_ratio = (editor_w / width).clamp(0.18, 0.82);
            self.layout_in(self.rect);
        }
        if (hovered, dragging) != (self.splitter.is_hovered(), self.splitter.is_dragging()) {
            self.dirty = true;
        }
        if self.splitter.is_dragging() {
            self.dirty = true;
        }
    }

    /// `CursorMoved`: hover/drag routing. Split out of `handle_pointer_event`
    /// to keep it within the line-count lint; preserves the original control
    /// flow exactly.
    fn handle_cursor_moved(
        &mut self,
        position: &winit::dpi::PhysicalPosition<f64>,
        scale_factor: f64,
        doc: &AutoCommit,
        src_id: &ObjId,
        handled: &mut bool,
    ) {
        let p = Point::new(position.x / scale_factor, position.y / scale_factor);
        self.mouse = p;
        self.pointer_seen = true;
        // The bubble pins open while the pointer rests on its corner: repaint
        // only while it shows so idle hover doesn't spin the loop.
        if self.status.is_visible(ui::time::now()) {
            self.dirty = true;
        }
        if self.splitter.is_dragging() {
            *handled = true;
        } else if self.dragging_editor {
            let text = doc.text(src_id).unwrap_or_default();
            self.editor.on_mouse_drag(p, &text);
            self.dirty = true;
            *handled = true;
        } else if self.dragging_preview {
            self.preview.on_mouse_drag(p);
            self.dirty = true;
            *handled = true;
        } else {
            // Hover feedback: inline-diagnostic tooltip over the source,
            // scrollbar-thumb brighten over the preview, row highlight in the
            // Problems panel.
            let over_editor = self.editor_rect.area() > 0.0 && self.editor_rect.contains(p);
            if self.editor.on_mouse_move(over_editor.then_some(p)) {
                self.dirty = true;
            }
            if self.preview_rect.area() > 0.0
                && self.preview_rect.contains(p)
                && self.preview.on_mouse_move(p)
            {
                self.dirty = true;
            }
            if self.show_diagnostics && self.diag_rect.contains(p) {
                self.diagnostics.on_mouse_move(p);
                self.dirty = true;
            }
        }
    }

    /// A left-button press. Split out of `handle_pointer_event` to keep it
    /// within the line-count lint; operates on `self.mouse`, the position
    /// tracked by the most recent `CursorMoved`.
    fn handle_left_press(
        &mut self,
        modifiers: Modifiers,
        doc: &AutoCommit,
        src_id: &ObjId,
        handled: &mut bool,
    ) {
        let p = self.mouse;
        // The band of controls owns its clicks; the controls themselves took
        // theirs before this point.
        if self.bar_rect.contains(p) {
            *handled = true;
            return;
        }
        if self.editor_rect.contains(p) {
            self.editor.focused = true;
            let text = doc.text(src_id).unwrap_or_default();
            self.editor
                .on_mouse_down(p, modifiers.state().shift_key(), &text);
            self.dragging_editor = true;
            *handled = true;
            return;
        }
        if self.show_diagnostics && self.diag_rect.contains(p) {
            if let Some(line) = self.diagnostics.on_mouse_down(p) {
                let text = doc.text(src_id).unwrap_or_default();
                self.editor.goto_line(&text, line);
                // In compact mode a diagnostic click brings the source into
                // view so the jump is visible.
                if self.compact {
                    self.set_compact_view(CompactPane::Editor);
                } else {
                    self.editor.focused = true;
                }
            }
            *handled = true;
            return;
        }
        if self.preview_rect.contains(p) {
            self.editor.focused = false;
            self.editor.on_mouse_move(None);
            let shift = modifiers.state().shift_key();
            if self.preview.on_mouse_down(p, shift) {
                self.dragging_preview = true;
            }
            self.dirty = true;
            *handled = true;
        }
    }
}

/// A text file a chooser or the command line named.
fn read_text(path: &std::path::Path) -> Result<String, tilo_files::FsError> {
    let (dir, name) = tilo_files::file_at(path)?;
    String::from_utf8(tilo_files::blocking::read(&dir, &name)?)
        .map_err(|_| tilo_files::FsError::Io("the file is not UTF-8 text".into()))
}

/// Save `bytes` as the file a chooser named, keeping an existing file's
/// link and permissions.
fn save_file(path: &std::path::Path, bytes: &[u8]) -> Result<(), tilo_files::FsError> {
    let (dir, name) = tilo_files::file_at(path)?;
    tilo_files::blocking::save_document(&dir, &name, bytes)
}

fn find_resource(resources: &[tilo_actions::Resource], id: &str) -> Option<tilo_actions::Resource> {
    for resource in resources {
        if resource.id.as_str() == id {
            return Some(resource.clone());
        }
        if let Some(found) = find_resource(&resource.children, id) {
            return Some(found);
        }
    }
    None
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod tests;
