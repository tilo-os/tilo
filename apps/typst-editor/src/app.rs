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
//! The workspace's files — names, paths and sources — are the resource's
//! state ([`crate::resource::TypstEditor`]); every edit is one of its actions,
//! so undo/redo and live collaboration come for free. Which file is open,
//! the caret, scroll, zoom, the split ratio and cached page bitmaps are view
//! state.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use ui::time::Instant;
use ui::widget::TaffyTreeExt;
use ui::widget::{AppModel, ModelPath};

#[cfg(test)]
use automerge::AutoCommit;
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
use ui::widget::{SplitHandle, SplitOrientation, SplitRatio, Widget};

use crate::chrome::{Chrome, Command};
use crate::compile::{self, CompileOutput, WorldParts};
use crate::diagnostics::DiagnosticsPanel;
use crate::editor::SourceEditor;
use crate::preview::PreviewPane;
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

/// A document whose final line is very long, used only by the `scrolled`
/// capture to show the source editor scrolled sideways with the caret kept in
/// view at the end of the line. The long line is last (no trailing newline)
/// so placing the caret at the document end lands it there.
pub(crate) const SCROLL_DEMO: &str = "= Horizontal scroll\n\n#let note = \"This is a single very long line of source text that runs well past the right edge of the editor pane, so the view must scroll sideways to keep the caret in view as you type toward the end.\"";

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

    // --- workspace ---
    /// The workspace as last presented.
    files: Rc<crate::resource::TypstState>,
    /// The id of the file shown, shared with the editor, which reads its
    /// source.
    current: Rc<RefCell<Option<String>>>,
    /// A file this window added, shown once it lands in the workspace.
    pending_current: Option<String>,
    actions: Option<ui::framework::Dispatcher<crate::resource::TypstEditor>>,
    /// An in-flight host file dialog: the operation and the channel its chosen
    /// path arrives on, drained once per frame in [`Self::drain_io`]. `None`
    /// when no Open / Save / Export chooser is open.
    io: Option<(PendingIo, PortalChannel)>,
    background_poll_deadline: Cell<Option<Instant>>,
    /// The answer owed to the file effect still running, given when its
    /// chooser or its write settles.
    file_reply: Option<ui::framework::EffectReply>,
    /// A file effect's read or write, done on the next sync.
    pending_resource_io: Option<ResourceIo>,
    /// Whether the first (synchronous) compile + worker spawn has happened.
    initialized: bool,
    /// Last source text we highlighted / compiled — drives change detection.
    last_source: String,
    /// Mirror of the source read each `sync`, used by `draw` (which has no
    /// doc access) for the caret readout and change detection. The editor body
    /// and its syntax highlighting live in the shared `SourceEditor` widget.
    source_cache: String,

    // --- view state ---
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
    /// Capture only: on the next layout pass, drop the caret at the end of
    /// the source and scroll it into view. Never set in a live window.
    demo_scroll_caret_end: bool,
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
    /// The transient bottom-trailing bubble: a compile result, an export.
    status: ui::StatusBubble,
    /// Whether the document saved: "Saved" once, or a failure to act on.
    save_status: ui::SaveStatusSurface,
    /// Where the last save went, so a failed save can be tried again.
    save_path: Option<std::path::PathBuf>,
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
    /// The divider between the editor and the preview, and the share of
    /// the width the editor takes.
    splitter: SplitHandle,
    /// Set when a pointer *move* changed something visible (hover, live drag)
    /// so `needs_redraw` asks the host to repaint — moves are otherwise gated.
    dirty: bool,
    /// The pointer has moved over the window, so the bubble follows it.
    pointer_seen: bool,
}

impl TypstApp {
    /// The editor over the workspace it is presented; it shows the first
    /// file until another is chosen.
    pub fn new() -> Self {
        let current: Rc<RefCell<Option<String>>> = Rc::default();
        let shown = Rc::clone(&current);
        let editor = SourceEditor::new(move |doc| {
            shown
                .borrow()
                .as_deref()
                .and_then(|id| crate::resource::source_text(doc, id))
        });
        let theme = Theme::dark();
        Self {
            parts: WorldParts::new(),
            worker: None,
            applied_seq: 0,
            editor,
            preview: PreviewPane::new(),
            diagnostics: DiagnosticsPanel::new(),
            files: Rc::default(),
            current,
            pending_current: None,
            io: None,
            background_poll_deadline: Cell::new(None),
            file_reply: None,
            pending_resource_io: None,
            initialized: false,
            last_source: String::new(),
            source_cache: String::new(),
            show_diagnostics: false,
            err_count: 0,
            warn_count: 0,
            elapsed_ms: 0,
            compact: false,
            compact_view: CompactPane::Editor,
            text_k: 1.0,
            demo_scroll_caret_end: false,
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
            save_status: ui::SaveStatusSurface::new(),
            save_path: None,
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
            splitter: SplitHandle::new(
                SplitOrientation::Horizontal,
                SplitRatio::new(0.46).with_bounds(0.18, 0.82),
            )
            .with_label("Resize the source and the preview"),
            dirty: true,
            pointer_seen: false,
            actions: None,
        }
    }

    /// Attach the workspace's dispatcher; a capture's first file.
    pub(crate) fn attach_surface(
        &mut self,
        cx: &ui::framework::SurfaceCx<crate::resource::TypstEditor>,
    ) {
        self.chrome.attach_actions(&cx.actions());
        self.actions = Some(cx.dispatcher());
        if let Some(file) = cx.fixture::<crate::resource::CaptureFile>() {
            *self.current.borrow_mut() = Some(file.0.clone());
        }
        self.follow_current();
    }

    /// Show the workspace as it is now: the file shown stays open unless it
    /// left the workspace; a file this window added opens once it lands.
    pub(crate) fn present_state(&mut self, state: &Rc<crate::resource::TypstState>) {
        self.files = Rc::clone(state);
        let pending = self
            .pending_current
            .as_deref()
            .filter(|id| state.file(id).is_some())
            .map(str::to_owned);
        if let Some(id) = pending {
            self.pending_current = None;
            self.open_file(&id);
            return;
        }
        let shown = self.current.borrow().clone();
        if shown.as_deref().is_none_or(|id| state.file(id).is_none()) {
            let first = state.files.first().map(|file| file.id.clone());
            *self.current.borrow_mut() = first;
            self.follow_current();
        }
        self.dirty = true;
    }

    /// The file shown, if the workspace has one.
    pub(crate) fn current_file(&self) -> Option<&crate::resource::TypstFile> {
        let current = self.current.borrow();
        let id = current.as_deref()?;
        self.files.files.iter().find(|file| file.id == id)
    }

    /// Submit the editor's edits to the file shown.
    fn follow_current(&mut self) {
        let (Some(actions), Some(id)) = (&self.actions, self.current.borrow().clone()) else {
            return;
        };
        let file = id.clone();
        self.editor
            .attach(actions.child::<ui::tilo_resource::PlainText>(
                &crate::resource::source_resource_id(&id),
                move |edit| crate::resource::TypstAction::EditSource {
                    file: file.clone(),
                    edit,
                },
            ));
    }

    /// Show file `id`, with the caret at its start.
    fn open_file(&mut self, id: &str) {
        *self.current.borrow_mut() = Some(id.to_owned());
        self.follow_current();
        self.editor.reset_cursor();
        // Keep focus consistent with the visible pane in compact mode.
        self.editor.focused = !(self.compact && matches!(self.compact_view, CompactPane::Preview));
        self.preview.fit_width();
        let name = self.current_file().map(|file| file.name.clone());
        if let Some(name) = name {
            self.set_flash(format!("Opened {name}"));
        }
        self.dirty = true;
    }

    /// Submit `action` to the workspace.
    fn apply(&self, action: &crate::resource::TypstAction) {
        if let Some(actions) = &self.actions {
            actions.apply(action);
        }
    }

    /// Replace the source of the file shown with `text`, as one edit.
    fn replace_source(&self, text: &str) {
        if let Some(file) = self.current_file() {
            self.apply(&crate::resource::TypstAction::EditSource {
                file: file.id.clone(),
                edit: ui::tilo_resource::TextEdit::ReplaceText {
                    text: text.to_owned(),
                },
            });
        }
    }

    /// Stage a capture: the view state each scenario shows once its files
    /// are laid out.
    pub(crate) fn stage_scenario(&mut self, scenario: Option<&str>) {
        match scenario {
            Some("preview") => self.compact_view = CompactPane::Preview,
            Some("menu") => {
                self.compact_view = CompactPane::Editor;
                self.chrome.open_picker();
            }
            Some("problems" | "errors") => self.show_diagnostics = true,
            Some("scrolled") => {
                self.editor.focused = true;
                self.demo_scroll_caret_end = true;
            }
            Some("editor-select") => {
                self.editor.focused = true;
                self.demo_editor_select = true;
            }
            Some("preview-zoom") => self.demo_preview_zoom = true,
            Some("preview-pan") => self.demo_preview_pan = true,
            Some("select") => self.demo_preview_select = true,
            Some("stale") => self.demo_stale = true,
            Some("status") => {
                self.status.show(
                    &format!(
                        "{}; {}",
                        self.status_left_label(),
                        self.status_right_label()
                    ),
                    ui::time::now(),
                );
            }
            _ => {}
        }
        self.dirty = true;
    }

    /// Live Problems rows and preview pages, published beneath the
    /// workspace.
    pub(crate) fn live_rows(&self) -> Vec<ui::framework::LiveRow> {
        crate::actions::live_extras(&crate::actions::TypstResourceCtx {
            diagnostics: self.diagnostics.diags(),
            page_count: self.preview.page_count(),
        })
        .into_iter()
        .map(crate::actions::live_row)
        .collect()
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
                self.set_flash(format!("Compiled in {} ms", self.elapsed_ms));
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

    fn force_recompile(&mut self) {
        let src = self.source_cache.clone();
        self.request_compile(&src);
        self.compile_feedback = true;
    }

    // ----- file I/O -------------------------------------------------------

    /// The name of the file shown, for a picker's suggestion.
    fn current_name(&self) -> String {
        self.current_file()
            .map_or_else(|| "untitled.typ".into(), |file| file.name.clone())
    }

    /// Save the file shown: where it was opened from or last saved, else to
    /// a path the host Save dialog asks for (unless a chooser is already in
    /// flight). The chosen path is handled in [`Self::drain_io`].
    fn save(&mut self) -> Result<bool, String> {
        if let Some(path) = self.current_file().and_then(|file| file.path.clone()) {
            return self.save_typ_to(std::path::Path::new(&path)).map(|()| true);
        }
        if self.io.is_some() {
            return Ok(false);
        }
        let name = self.current_name();
        self.io = Some((PendingIo::SaveTyp, typ_save_request(&name).send()));
        self.set_flash("Choose where to save…");
        Ok(false)
    }

    /// Core: write the source of the file shown to `path`, which the file
    /// then lives at. Split out so the portal drain and tests drive it
    /// against a real file.
    fn save_typ_to(&mut self, path: &std::path::Path) -> Result<(), String> {
        let Some(file) = self.current_file().cloned() else {
            return Err("there is no file to save".into());
        };
        match save_file(path, file.source.as_bytes()) {
            Ok(()) => {
                self.set_flash(format!("Saved {}", path.display()));
                let path = path.to_string_lossy().into_owned();
                if file.path.as_deref() != Some(path.as_str()) {
                    self.apply(&crate::resource::TypstAction::SetPath {
                        file: file.id,
                        path,
                    });
                }
                Ok(())
            }
            Err(e) => {
                self.save_status
                    .set(ui::SaveState::Failed(e.to_string()), ui::time::now());
                Err(e.to_string())
            }
        }
    }

    /// Open the host Save dialog to choose where to export the compiled PDF,
    /// seeded with the file's name and a `.pdf` extension (unless a chooser
    /// is already in flight). The chosen path is handled in [`Self::drain_io`].
    fn export_pdf(&mut self) {
        if self.io.is_some() {
            return;
        }
        let pdf_name = std::path::Path::new(&self.current_name())
            .with_extension("pdf")
            .to_string_lossy()
            .to_string();
        self.io = Some((PendingIo::ExportPdf, pdf_export_request(&pdf_name).send()));
        self.set_flash("Choose where to export the PDF");
    }

    /// Compile the source shown to a PDF and write it to the exact `path`.
    /// Returns an error string on a compile failure or an I/O error.
    fn export_pdf_to_path(&self, path: &std::path::Path) -> Result<(), String> {
        let bytes = compile::compile_pdf(&self.parts, &self.source_cache).map_err(|diags| {
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

    /// Open the host chooser to pick a `.typ` file to load (unless a chooser is
    /// already in flight). The chosen path is handled in [`Self::drain_io`].
    fn open(&mut self) {
        if self.io.is_some() {
            return;
        }
        self.io = Some((PendingIo::OpenTyp, typ_open_request().send()));
        self.set_flash("Choose a .typ file to open");
    }

    /// Core: add the `.typ` file at `path` to the workspace and show it, or
    /// show the workspace's copy of it again with what the file holds now.
    /// Split out so the portal drain and tests drive it against a real file.
    fn open_typ_from(&mut self, path: &std::path::Path) -> Result<(), String> {
        let src = match read_text(path) {
            Ok(s) => s,
            Err(e) => {
                self.set_flash(format!("Open failed: {e}"));
                return Err(e.to_string());
            }
        };
        let path = path.to_string_lossy().into_owned();
        // A window edits one document: the file opened replaces the one shown.
        if let Some(id) = self.current_file().map(|file| file.id.clone()) {
            self.replace_source(&src);
            self.apply(&crate::resource::TypstAction::SetPath { file: id, path });
            self.editor.reset_cursor();
            self.preview.fit_width();
            return Ok(());
        }
        let action =
            crate::resource::new_file(&crate::resource::file_name(&path), Some(&path), &src);
        if let crate::resource::TypstAction::AddFile { id, .. } = &action {
            self.pending_current = Some(id.clone());
        }
        self.apply(&action);
        Ok(())
    }

    /// Drain a finished Open / Save / Export file dialog, if one landed since
    /// the last frame, and run it against the chosen path. The UI thread never
    /// blocks: the portal round-trip lives on its own worker.
    fn drain_io(&mut self) {
        let Some((op, rx)) = &self.io else { return };
        let op = *op;
        match rx.try_recv() {
            Ok(outcome) => {
                self.io = None;
                let result = match outcome {
                    PortalOutcome::Opened(path) => match op {
                        PendingIo::OpenTyp => self.open_typ_from(&path),
                        PendingIo::SaveTyp => self.save_typ_to(&path),
                        PendingIo::ExportPdf => self
                            .export_pdf_to_path(&path)
                            .inspect(|()| self.set_flash(format!("Exported {}", path.display()))),
                    },
                    PortalOutcome::Cancelled => Err("cancelled".into()),
                    PortalOutcome::Failed(e) => {
                        self.set_flash(format!("File chooser unavailable: {e}"));
                        Err(e)
                    }
                };
                self.finish_file_effect(result);
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.io = None;
                self.finish_file_effect(Err("file picker disconnected".into()));
            }
        }
    }

    fn run_command(&self, command: Command, _doc: &ui::automerge::AutoCommit) {
        use crate::actions::TypstEffect;
        let effect = match command {
            Command::Compile => TypstEffect::Compile,
            Command::Save => TypstEffect::Save { path: None },
            Command::Open => TypstEffect::Open { path: None },
            Command::ExportPdf => TypstEffect::ExportPdf { path: None },
            Command::ZoomOut => TypstEffect::ZoomOut,
            Command::ZoomIn => TypstEffect::ZoomIn,
            Command::Fit => TypstEffect::FitWidth,
            Command::ToggleProblems => TypstEffect::Problems,
            Command::ShowEditor => TypstEffect::Editor,
            Command::ShowPreview => TypstEffect::Preview,
        };
        if let Some(actions) = &self.actions {
            actions.effect(&effect);
        }
    }

    /// Execute the semantic operation shared by exported resources and direct
    /// toolbar/shortcut affordances.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "operations are decoded as owned values and dispatched immediately"
    )]
    fn perform_resource_operation(&mut self, operation: crate::actions::TypstEffect) {
        use crate::actions::TypstEffect;
        match operation {
            TypstEffect::Compile => self.force_recompile(),
            TypstEffect::Save { .. } => {
                let _ = self.save();
            }
            TypstEffect::Open { .. } => self.open(),
            TypstEffect::ExportPdf { .. } => self.export_pdf(),
            TypstEffect::ZoomOut => self.preview.zoom_out(),
            TypstEffect::ZoomIn => self.preview.zoom_in(),
            TypstEffect::FitWidth => self.preview.fit_width(),
            TypstEffect::Problems => {
                self.show_diagnostics = !self.show_diagnostics;
            }
            TypstEffect::Editor => self.set_compact_view(CompactPane::Editor),
            TypstEffect::Preview => self.set_compact_view(CompactPane::Preview),
            TypstEffect::GoToDiagnostic { index } => {
                if let Some(diag) = self.diagnostics.diags().get(index)
                    && let Some(line) = diag.line
                {
                    let text = self.source_cache.clone();
                    self.editor.goto_line(&text, line);
                    self.show_diagnostics = true;
                    if self.compact {
                        self.set_compact_view(CompactPane::Editor);
                    } else {
                        self.editor.focused = true;
                    }
                }
            }
            TypstEffect::GoToPage { page } => {
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
        self.layout_in(self.rect);
        self.dirty = true;
    }

    /// Announce something worth noticing once: the transient bubble shows it
    /// and hides it again.
    fn set_flash(&mut self, msg: impl AsRef<str>) {
        self.status.show(msg.as_ref(), ui::time::now());
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
            // One pane at a time: no divider to hit.
            self.splitter.set_bounds(Rect::ZERO);
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
            self.splitter
                .set_bounds(Rect::new(r.x0, body.y0, r.x1, body.y1));
            let (editor_col, preview) = self.splitter.pane_rects();
            self.preview_rect = preview;
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
        if self.demo_scroll_caret_end && !self.source_cache.is_empty() {
            self.demo_scroll_caret_end = false;
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

        let name = self.current_name();
        let names = [name.as_str()];
        self.chrome.sync(
            &names,
            0,
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
        dyn ui::widget::controls::ActionControls,
        dyn ui::widget::WindowHeader
    );

    fn settle(&mut self, edits: &mut ui::widget::Edits<'_>, path: &automerge::ObjId) {
        for child in self.chrome.children_mut() {
            child.settle(edits, path);
        }
        for command in self.chrome.take() {
            self.run_command(command, edits.doc());
            self.dirty = true;
        }
    }

    fn sync(&mut self, doc: &AppModel, path: &ModelPath) {
        // Retained controls settle their admitted operations on sync (notably
        // opening the toolbar overflow); Chrome's presentation sync only
        // updates the document label and current toggle state.
        for child in self.chrome.children_mut() {
            child.sync(doc, path);
        }
        let (doc, _) = ui::doc::document_ref(doc, path);
        self.background_poll_deadline.set(None);
        if let Some(operation) = self.pending_resource_io.take() {
            let result = match operation {
                ResourceIo::Open(path) => self.open_typ_from(&path),
                ResourceIo::Save(path) => self.save_typ_to(&path),
                ResourceIo::ExportPdf(path) => self.export_pdf_to_path(&path),
            };
            self.finish_file_effect(result);
        }
        // Land a finished Open/Save/Export file dialog.
        self.drain_io();
        // Show the open file's source as the document holds it.
        self.editor.pull(doc);
        let source = self.editor.text().to_owned();

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
            // (a capture's included) has rendered pages, then bring the async
            // worker online for subsequent edits.
            let out = compile::compile_source(&self.parts, &source, PPP);
            self.apply_output(out);
            self.worker = CompileWorker::spawn(self.parts.clone());
            self.last_source = source.clone();
            self.source_cache = source;
            self.initialized = true;
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
        let style = Style {
            size: Size {
                width: Dimension::percent(1.0),
                height: Dimension::percent(1.0),
            },
            ..Default::default()
        };
        taffy.widget_node(style, &child_nodes, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let rect = ui::widget::layout_rect(taffy, node, origin);
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
            self.splitter
                .paint(scene, t, theme.tokens.colors.surfaces.content, theme);
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
        self.save_status.paint(scene, t, tr, theme, area, now);

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
        let window = presence.viewport.get().unwrap_or(self.rect);
        self.editor
            .draw_tooltip(scene, transform, text_renderer, theme, window);
        for child in self.chrome.children_mut() {
            child.draw_overlay(scene, transform, text_renderer, theme, presence);
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
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
            self.run_command(command, ctx.state.doc());
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
                &mut ctx.state,
                ctx.handled,
            );
            return;
        }

        // --- pointer ---
        self.handle_pointer_event(event, ctx.scale_factor, *ctx.modifiers, ctx.handled);
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
        doc: &mut ui::widget::Edits<'_>,
        path: &ui::widget::ModelPath,
    ) -> bool {
        let mut handled = false;
        for child in self.chrome.children_mut() {
            handled |= child.perform_semantic_action(target, action, doc, path);
        }
        if handled {
            for command in self.chrome.take() {
                self.run_command(command, doc.doc());
            }
            return true;
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
                self.replace_source(source);
                self.editor.set_caret(source.len());
                true
            }
            _ => false,
        }
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn has_pending_visual_work(&self) -> bool {
        self.compiling()
            || self
                .children()
                .iter()
                .any(|child| child.has_pending_visual_work())
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
            || self.save_status.needs_frame(ui::time::now())
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

impl ui::widget::controls::ActionControls for TypstApp {
    fn control_action_bindings(&self) -> Vec<ui::widget::ControlBindingMetadata> {
        ui::widget::semantic_action_bindings(self.accessibility())
    }
}

/// The bar is the window's header row: it holds the History control the
/// host offers, and the window draws no History bar above the editor.
impl ui::widget::WindowHeader for TypstApp {
    fn set_history_control(&mut self, control: Option<ui::widget::HistoryControl>) {
        self.chrome.set_history(control);
        self.dirty = true;
    }
}

impl TypstApp {
    /// Answer the file effect's caller, if one is waiting.
    fn finish_file_effect(&mut self, result: Result<(), String>) {
        if let Some(reply) = self.file_reply.take() {
            reply.finish(result.map(|()| tilo_actions::ActionOutput::default()));
        }
    }

    /// Answer `effect`: a file read or write with a path waits for the next
    /// sync, one without asks the file chooser; the rest happen now.
    pub(crate) fn answer_effect(
        &mut self,
        effect: crate::actions::TypstEffect,
        cx: &mut ui::framework::EffectCx<crate::resource::TypstEditor, Self>,
    ) -> ui::framework::EffectAnswer {
        use crate::actions::TypstEffect;
        let file = matches!(
            effect,
            TypstEffect::Open { .. } | TypstEffect::Save { .. } | TypstEffect::ExportPdf { .. }
        );
        if file && (self.file_reply.is_some() || self.pending_resource_io.is_some()) {
            return Err("another file operation is already running".into());
        }
        match effect {
            TypstEffect::Open { path: Some(path) } => {
                self.pending_resource_io = Some(ResourceIo::Open(path.into()));
            }
            TypstEffect::Save { path: Some(path) } => {
                self.pending_resource_io = Some(ResourceIo::Save(path.into()));
            }
            TypstEffect::ExportPdf { path: Some(path) } => {
                self.pending_resource_io = Some(ResourceIo::ExportPdf(path.into()));
            }
            TypstEffect::Open { path: None } => self.open(),
            TypstEffect::Save { path: None } => {
                if self.save()? {
                    return Ok(ui::framework::Effected::done());
                }
            }
            TypstEffect::ExportPdf { path: None } => self.export_pdf(),
            other => {
                self.perform_resource_operation(other);
                return Ok(ui::framework::Effected::done());
            }
        }
        self.file_reply = Some(cx.reply());
        Ok(ui::framework::Effected::Pending)
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
        doc: &mut ui::widget::Edits<'_>,
        handled: &mut bool,
    ) {
        let mods = modifiers.state();
        let ctrl = mods.control_key() || mods.super_key();
        if ctrl {
            if matches!(logical_key, Key::Named(NamedKey::Enter)) {
                self.force_recompile();
                *handled = true;
                return;
            }
            if let Key::Character(ch) = logical_key {
                match ch.to_lowercase().as_str() {
                    "s" => {
                        // A failed save says so in the flash.
                        let _ = self.save();
                        *handled = true;
                        return;
                    }
                    "o" => {
                        self.open();
                        *handled = true;
                        return;
                    }
                    "e" => {
                        self.export_pdf();
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
        if self.editor.focused && self.editor.handle_key(event, &modifiers, doc) {
            *handled = true;
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
        handled: &mut bool,
    ) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.handle_cursor_moved(position, scale_factor, handled);
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                self.handle_left_press(modifiers, handled);
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
                            let line = f64::from(ui::tokens::metrics::Metrics::BASE.wheel_line);
                            (-f64::from(*x) * line, -f64::from(*y) * line)
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
                        let text = self.source_cache.clone();
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
        if self.splitter.handle_event(event, ctx) {
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
        handled: &mut bool,
    ) {
        let p = Point::new(position.x / scale_factor, position.y / scale_factor);
        self.mouse = p;
        self.pointer_seen = true;
        // The bubble pins open while the pointer rests on its corner: repaint
        // only while it shows so idle hover doesn't spin the loop.
        if self.status.is_visible(ui::time::now()) || self.save_status.pointer_moved(p) {
            self.dirty = true;
        }
        if self.splitter.is_dragging() {
            *handled = true;
        } else if self.dragging_editor {
            let text = self.source_cache.clone();
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
    fn handle_left_press(&mut self, modifiers: Modifiers, handled: &mut bool) {
        let p = self.mouse;
        // A failed save's card owns its clicks: try again, or dismiss it.
        if self.save_status.contains(p) {
            if self.save_status.click(p) == Some(ui::SaveAction::Retry)
                && let Some(path) = self.save_path.clone()
                && let Some(dispatcher) = &self.actions
            {
                dispatcher.effect(&crate::actions::TypstEffect::Save {
                    path: Some(path.to_string_lossy().into_owned()),
                });
            }
            self.dirty = true;
            *handled = true;
            return;
        }
        // The band of controls owns its clicks; the controls themselves took
        // theirs before this point.
        if self.bar_rect.contains(p) {
            *handled = true;
            return;
        }
        if self.editor_rect.contains(p) {
            self.editor.focused = true;
            let text = self.source_cache.clone();
            self.editor
                .on_mouse_down(p, modifiers.state().shift_key(), &text);
            self.dragging_editor = true;
            *handled = true;
            return;
        }
        if self.show_diagnostics && self.diag_rect.contains(p) {
            if let Some(line) = self.diagnostics.on_mouse_down(p) {
                let text = self.source_cache.clone();
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
pub(crate) fn read_text(path: &std::path::Path) -> Result<String, tilo_files::FsError> {
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

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod tests {
    use super::*;
    use winit::dpi::PhysicalPosition;
    use winit::event::Modifiers;

    use automerge::{ROOT, ReadDoc as _};
    use ui::tilo_actions::Action as _;
    use ui::tilo_resource::Resource as _;

    /// A workspace of the bundled examples, presented to an editor showing
    /// the first, synced once so it compiled.
    fn live_fixture(
        viewport: (u32, u32),
    ) -> ui::framework::testing::TestApp<crate::resource::TypstEditorShell> {
        use ui::framework::ResourceApp;
        let mut app =
            ui::framework::testing::TestApp::<crate::resource::TypstEditorShell>::new().unwrap();
        for invoke in crate::resource::TypstEditorShell::scenario(None) {
            app.invoke(invoke).unwrap();
        }
        app.harness().resize(viewport).unwrap();
        app.settle();
        app
    }

    fn fresh() -> (TypstApp, AutoCommit) {
        workspace(None)
    }

    /// [`fresh`] with the examples a capture of `scenario` shows.
    fn workspace(scenario: Option<&str>) -> (TypstApp, AutoCommit) {
        let mut doc = AutoCommit::new();
        crate::resource::TypstEditor::initialize(&mut doc).unwrap();
        for action in crate::resource::sample_files(scenario) {
            action.apply(&mut doc).unwrap();
        }
        doc.commit();
        let mut app = TypstApp::new();
        present(&mut app, &doc);
        app.sync(&doc, &ROOT);
        (app, doc)
    }

    fn present(app: &mut TypstApp, doc: &AutoCommit) {
        app.present_state(&Rc::new(crate::resource::TypstEditor::read(doc).unwrap()));
    }

    /// The source of the file `app` shows, as `doc` holds it.
    fn shown_source(app: &TypstApp, doc: &AutoCommit) -> String {
        app.current_file()
            .and_then(|file| crate::resource::source_text(doc, &file.id))
            .and_then(|text| doc.text(&text).ok())
            .unwrap_or_default()
    }

    #[test]
    fn hosted_capture_finishes_the_real_preview_compile_before_its_first_render() {
        let mut app =
            ui::framework::testing::TestApp::<crate::resource::TypstEditorShell>::capture_at(
                None,
                (1280, 800),
            )
            .unwrap();
        assert_compiled_welcome(&mut app);
    }

    #[test]
    fn live_preview_finishes_the_real_compile_after_published_source_actions() {
        let mut app =
            ui::framework::testing::TestApp::<crate::resource::TypstEditorShell>::new().unwrap();
        let file = app.state().files[0].id.clone();
        let crate::resource::TypstAction::AddFile { source, .. } =
            crate::resource::sample_files(None).remove(0)
        else {
            panic!("the welcome scenario adds a source file");
        };
        app.invoke_child::<ui::tilo_resource::PlainText>(
            &crate::resource::source_resource_id(&file),
            ui::tilo_resource::Invoke::apply(&ui::tilo_resource::TextEdit::Splice {
                at: 0,
                delete: 0,
                insert: source,
            })
            .unwrap(),
        )
        .unwrap();
        app.harness().resize((1280, 800)).unwrap();
        app.harness()
            .wait_for_visuals(ui::time::Duration::from_secs(10))
            .unwrap();
        assert_compiled_welcome(&mut app);
    }

    fn assert_compiled_welcome(
        app: &mut ui::framework::testing::TestApp<crate::resource::TypstEditorShell>,
    ) {
        app.with_surface::<TypstApp, _>(|view| {
            assert!(
                !view.compiling(),
                "capture returned while the real source compilation was pending"
            );
            assert!(view.editor.text().contains("Getting started with Typst"));
            assert_eq!(view.err_count, 0);
            view.preview.select_all();
            let compiled = view
                .preview
                .selected_text()
                .expect("the rendered page has real selectable glyphs");
            view.preview.clear_selection();
            assert!(
                compiled.contains("Getting started with Typst"),
                "actual compiled text: {compiled}"
            );
        })
        .unwrap();
    }

    #[test]
    fn mounted_toolbar_commands_have_native_action_admission() {
        let mut app = live_fixture((1280, 720));
        for viewport in [(1280, 720), (480, 720)] {
            app.harness().resize(viewport).unwrap();
            let audit = app.harness().native_app_audit();
            assert!(audit.is_certified(), "{viewport:?}: {audit:?}");
        }
    }

    #[test]
    fn file_effects_save_and_export_real_files() {
        // Case `export_pdf_writes_a_real_pdf_file`:
        // Exporting writes a real `.pdf` — the file on disk begins with the
        // `%PDF` magic. Driven against a scratch dir so it never touches the
        // user's documents folder.
        {
            let (app, _doc) = fresh();
            let dir = unique_dir("pdf");
            let path = dir.join("welcome.pdf");
            app.export_pdf_to_path(&path)
                .expect("the clean demo document exports");
            let bytes = std::fs::read(&path).expect("the pdf was written to disk");
            assert!(bytes.starts_with(b"%PDF"), "the written file is a real PDF");
            std::fs::remove_dir_all(&dir).ok();
        }
        // Case `portal_cancel_is_silent`:
        // A cancelled chooser clears the in-flight dialog and changes nothing.
        {
            let (mut app, doc) = fresh();
            let before = shown_source(&app, &doc);
            let (services, recorder) = ui::host::HostServices::recording();
            recorder.respond_to_pick(PortalOutcome::Cancelled);
            ui::host::scoped(&services, || app.open());
            app.sync(&doc, &ROOT);
            assert!(app.io.is_none());
            assert_eq!(shown_source(&app, &doc), before, "cancel loads nothing");
        }
    }

    fn cursor(p: Point) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: PhysicalPosition::new(p.x, p.y),
        }
    }

    use ui::synthetic_input::mouse_press;

    /// Pump one event through the production `Widget::handle_event`, returning
    /// whether it was consumed.
    fn send(app: &mut TypstApp, doc: &mut AutoCommit, ev: &WindowEvent) -> bool {
        let mut taffy = TaffyTree::<()>::new();
        let mut nodes = HashMap::new();
        let node = app.build_layout(&mut taffy, &Theme::dark(), &mut nodes);
        ui::testing::Dispatch::new()
            .doc(doc)
            .layout(&taffy, node)
            .send(app, ev)
            .handled
    }

    /// Run one real draw pass so retained control geometry is measured,
    /// exactly as they are for the live pointer path.
    fn render_once(app: &mut TypstApp) {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        let presence = PresenceCtx::empty();
        app.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
    }

    #[test]
    fn source_sync_and_one_document_per_window() {
        // Case `first_sync_shows_the_first_file_and_compiles`.
        {
            let (app, doc) = fresh();
            assert!(!shown_source(&app, &doc).is_empty(), "the welcome example");
            assert_eq!(app.editor.text(), shown_source(&app, &doc));
            assert!(app.initialized);
            // Welcome demo compiles cleanly into at least one page.
            assert!(app.preview.page_count() >= 1, "rendered a page");
            assert_eq!(app.err_count, 0, "welcome demo has no errors");
        }
        // Case `there_is_no_document_switcher`: Ctrl+] and Ctrl+[ switch
        // nothing — a window edits one document — and write nothing.
        {
            let (mut app, mut doc) = fresh();
            doc.commit();
            let heads = doc.get_heads();
            let before = shown_source(&app, &doc);
            for key in ["]", "["] {
                let mut handled = false;
                app.handle_keyboard_shortcut(
                    &Key::Character(key.into()),
                    &char_key(key),
                    Modifiers::from(ModifiersState::CONTROL),
                    &mut ui::widget::Edits::lend(&mut doc),
                    &mut handled,
                );
            }
            app.sync(&doc, &ROOT);
            assert_eq!(shown_source(&app, &doc), before);
            assert_eq!(doc.get_heads(), heads, "no switch, no write");
        }
        // Case `caret_line_col_tracks_newlines`.
        {
            let mut app = TypstApp::new();
            app.source_cache = "abc\nde\nf".to_string();
            app.editor.set_text("abc\nde\nf");
            app.editor.set_caret(5); // byte 5 → line 2, col 2 (just after 'd')
            assert_eq!(app.caret_line_col(), (2, 2));
        }
        // Case `without_a_dispatcher_the_source_cannot_change`.
        {
            use ui::widget::Widget as _;

            let (mut app, mut doc) = fresh();
            let before = shown_source(&app, &doc);
            assert!(send(&mut app, &mut doc, &char_key("x")));
            assert!(app.perform_semantic_action(
                "typst.source",
                &ui::a11y::Action::SetValue("= Accessible edit".into()),
                &mut ui::widget::Edits::lend(&mut doc),
                &ROOT,
            ));
            app.sync(&doc, &ROOT);
            assert_eq!(shown_source(&app, &doc), before);
            assert_eq!(app.editor.text(), before);
        }
    }

    #[test]
    fn idle_and_pending_work_schedule_non_animation_deadlines() {
        // Case `focused_idle_app_requests_no_animation_frame`:
        // A focused editor that isn't compiling or flashing must report no need
        // for animation frames, so the event loop stays idle (no per-vsync
        // redraw just to blink a caret).
        {
            let (app, _doc) = fresh();
            assert!(app.editor.focused, "editor focused by default");
            assert!(
                !app.compiling(),
                "the initial compile ran synchronously; nothing in flight"
            );
            assert!(
                !app.status.is_visible(ui::time::now()),
                "no transient toast pending"
            );
            assert!(
                !app.needs_animation_frame(),
                "a focused-but-idle editor must not drive continuous redraws"
            );
        }
        // Case `pending_native_dialog_has_a_stable_non_animation_deadline`.
        {
            let (mut app, doc) = fresh();
            assert!(app.next_wake_deadline().is_none());
            let (services, _recorder) = ui::host::HostServices::recording();
            ui::host::scoped(&services, || app.open());
            let deadline = app
                .next_wake_deadline()
                .expect("file dialogs must complete without animation or further input");
            assert_eq!(
                app.next_wake_deadline(),
                Some(deadline),
                "queries must not keep postponing polling"
            );
            app.sync(&doc, &ROOT);
            assert!(app.io.is_none());
            assert!(app.next_wake_deadline().is_none());
        }
        // Case `pending_compile_has_a_non_animation_deadline`.
        {
            let (mut app, _) = fresh();
            app.request_compile("= Native compile wakeup");
            assert!(app.compiling());
            assert!(
                app.next_wake_deadline().is_some(),
                "compile completion must be observed with reduced motion"
            );
        }
        // Caret/page metadata belongs to the semantic status strip; explicit
        // compile/export feedback remains transient and fades after its linger.
        {
            let (mut app, _) = fresh();
            app.layout_in(Rect::new(0.0, 0.0, 1280.0, 720.0));
            assert_eq!(app.editor_rect.y1, app.status_rect.y0);
            assert_eq!(app.preview_rect.y1, app.status_rect.y0);
            assert_eq!(app.status_rect.y1, app.rect.y1);
            assert!(!app.status_left_label().is_empty());
            assert!(!app.status_right_label().is_empty());
            let now = ui::time::now();
            app.status = ui::StatusBubble::new().with_linger(std::time::Duration::from_millis(5));
            app.status.show("Compiled", now);
            assert!(app.status.is_visible(now));
            let past = now
                .checked_sub(std::time::Duration::from_millis(10))
                .unwrap();
            app.status.poke(past);
            render_once(&mut app);
            assert!(
                !app.status.is_visible(ui::time::now()),
                "transient feedback fades"
            );
        }
    }

    // ---- helpers for the preview-interaction + diagnostics tests ----------

    use crate::compile::DiagKind;
    use winit::event::TouchPhase;
    use winit::keyboard::ModifiersState;

    use ui::synthetic_input::char_key;

    /// A vertical wheel notch (`+` = up/away). A tilt component can be supplied
    /// via `x` for the horizontal-pan path.
    fn wheel(x: f32, y: f32) -> WindowEvent {
        WindowEvent::MouseWheel {
            device_id: winit::event::DeviceId::dummy(),
            delta: MouseScrollDelta::LineDelta(x, y),
            phase: TouchPhase::Moved,
        }
    }

    /// Like [`send`] but with explicit modifiers held.
    fn send_mods(
        app: &mut TypstApp,
        doc: &mut AutoCommit,
        ev: &WindowEvent,
        mods: Modifiers,
    ) -> bool {
        let mut taffy = TaffyTree::<()>::new();
        let mut nodes = HashMap::new();
        let node = app.build_layout(&mut taffy, &Theme::dark(), &mut nodes);
        ui::testing::Dispatch::new()
            .modifiers(mods)
            .doc(doc)
            .layout(&taffy, node)
            .send(app, ev)
            .handled
    }

    fn unique_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "typst_editor_{tag}_{}_{}",
            std::process::id(),
            ui::time::try_unix_now_ns().unwrap()
        ))
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario: each former test is a labelled case block with its own fixture"
    )]
    fn preview_zoom_and_diagnostics_respond_to_input() {
        // Case `ctrl_plus_minus_keys_zoom_preview`:
        // Ctrl+= zooms the rendered preview in and Ctrl+- zooms it back out, driven
        // through the production keyboard-shortcut path.
        {
            let (mut app, mut doc) = fresh();
            app.layout_in(Rect::new(0.0, 0.0, 1280.0, 720.0));
            let ctrl = Modifiers::from(ModifiersState::CONTROL);

            let base = app.preview.zoom();
            assert!(
                send_mods(&mut app, &mut doc, &char_key("="), ctrl),
                "Ctrl+= consumed"
            );
            let zoomed_in = app.preview.zoom();
            assert!(
                zoomed_in > base,
                "Ctrl+= zoomed the preview in ({base} -> {zoomed_in})"
            );

            assert!(
                send_mods(&mut app, &mut doc, &char_key("-"), ctrl),
                "Ctrl+- consumed"
            );
            assert!(
                app.preview.zoom() < zoomed_in,
                "Ctrl+- zoomed the preview back out ({zoomed_in} -> {})",
                app.preview.zoom()
            );
        }
        // Case `ctrl_wheel_over_preview_zooms`:
        // Ctrl+mouse-wheel over the preview pane zooms it (cursor-anchored), routed
        // through the real `MouseWheel` handler.
        {
            let (mut app, mut doc) = fresh();
            app.layout_in(Rect::new(0.0, 0.0, 1280.0, 720.0));
            let ctrl = Modifiers::from(ModifiersState::CONTROL);

            let over = app.preview_rect.center();
            send_mods(&mut app, &mut doc, &cursor(over), ctrl);
            let base = app.preview.zoom();
            assert!(
                send_mods(&mut app, &mut doc, &wheel(0.0, 1.0), ctrl),
                "Ctrl+wheel consumed"
            );
            assert!(
                app.preview.zoom() > base,
                "Ctrl+wheel over the preview zoomed in ({base} -> {})",
                app.preview.zoom()
            );
        }
        // Case `diagnostic_click_jumps_editor_for_both_severities`:
        // The Problems panel shows both an error and a warning from the real
        // compiler, and clicking a row jumps the editor caret to that diagnostic's
        // line — proven for both severities through the production pointer path.
        {
            let (mut app, mut doc) = workspace(Some("errors"));
            app.stage_scenario(Some("errors"));
            app.sync(&doc, &ROOT);
            app.layout_in(Rect::new(0.0, 0.0, 1280.0, 720.0));

            render_once(&mut app);
            let diags = app.diagnostics.diags().to_vec();
            assert!(
                diags.iter().any(|d| d.kind == DiagKind::Error),
                "an error is listed"
            );
            assert!(
                diags.iter().any(|d| d.kind == DiagKind::Warning),
                "a warning is listed"
            );
            assert!(
                app.show_diagnostics && app.diag_rect.area() > 0.0,
                "Problems panel is open"
            );
            assert!(
                diags.len() >= 2 && diags[0].line != diags[1].line,
                "rows target distinct lines"
            );

            for (i, d) in diags.iter().enumerate() {
                let Some(line) = d.line else { continue };
                let p = app.diagnostics.row_rect(i).center();
                send(&mut app, &mut doc, &cursor(p));
                assert!(
                    send(&mut app, &mut doc, &mouse_press()),
                    "diagnostic row click consumed"
                );
                assert_eq!(
                    app.caret_line_col().0,
                    line,
                    "clicking the {:?} row jumped the editor to line {line}",
                    d.kind
                );
            }
        }
        // Case `failed_recompile_keeps_last_good_preview`:
        // A failed recompile keeps the last successful render on screen (flagged
        // stale) instead of blanking the preview — so a transient typo doesn't
        // throw away the reader's place. Drives the real compile → `apply_output`
        // path the background worker feeds.
        {
            let (mut app, _doc) = fresh();
            assert!(
                app.preview.page_count() >= 1,
                "the welcome demo rendered a page"
            );
            assert!(!app.preview.is_stale(), "a clean render is not stale");

            // Compile a broken source (an undefined variable is a fatal error that
            // stops layout), then feed the result exactly as the worker would.
            let broken = compile::compile_source(&app.parts, "Body #undefined_var end.\n", PPP);
            assert!(
                broken.pages.is_empty(),
                "the broken source produced no pages"
            );
            assert!(broken.error_count() >= 1, "and carries a blocking error");
            app.apply_output(broken);

            assert!(
                app.preview.page_count() >= 1,
                "the last good render is retained after a failed compile"
            );
            assert!(
                app.preview.is_stale(),
                "the retained render is flagged stale"
            );
            assert!(
                app.err_count >= 1 && app.show_diagnostics,
                "the error is surfaced"
            );

            // A subsequent clean compile clears the stale flag and refreshes pages.
            let good = compile::compile_source(&app.parts, "= Fixed\n\nBody.\n", PPP);
            assert!(!good.pages.is_empty());
            app.apply_output(good);
            assert!(
                !app.preview.is_stale(),
                "a clean recompile clears the stale flag"
            );
        }
    }

    #[test]
    fn accessibility_exposes_editor_state_and_toolbar_controls() {
        // Case `accessibility_exposes_source_and_preview_state`.
        {
            use ui::widget::Widget as _;

            let (app, _doc) = fresh();
            let tree = app.accessibility().expect("Typst semantics");
            assert_eq!(tree.role, ui::a11y::Role::Document);
            assert_eq!(tree.children[0].role, ui::a11y::Role::MultilineTextInput);
            assert_eq!(tree.children[1].role, ui::a11y::Role::Canvas);
            assert!(
                tree.children[1]
                    .value
                    .as_deref()
                    .is_some_and(|value| value.contains("Page"))
            );
            assert!(tree.children.iter().all(|node| node.semantic_key.is_some()));
        }
        // The retained toolbar's actual published controls keep file/compile
        // actions accessible; focusing a pane selector never invokes it.
        {
            let mut app = live_fixture((560, 720));
            let id = app
                .harness()
                .a11y_node_named("Preview")
                .expect("shared preview selector");
            app.harness()
                .perform_a11y_action(id, &ui::a11y::Action::Focus)
                .unwrap();
            app.settle();
            assert!(
                app.with_widget(|surface: &mut TypstApp| matches!(
                    surface.compact_view,
                    CompactPane::Editor
                ))
                .unwrap(),
                "focus does not activate"
            );
            let id = app
                .harness()
                .a11y_node_named("Preview")
                .expect("shared preview selector");
            app.harness()
                .perform_a11y_action(id, &ui::a11y::Action::Activate)
                .unwrap();
            app.settle();
            assert!(
                app.with_widget(|surface: &mut TypstApp| matches!(
                    surface.compact_view,
                    CompactPane::Preview
                ))
                .unwrap()
            );
            app.harness().resize((390, 720)).unwrap();
            let more = app
                .harness()
                .a11y_node_named("More commands")
                .expect("shared toolbar overflow");
            app.harness()
                .perform_a11y_action(more, &ui::a11y::Action::Activate)
                .unwrap();
            for command in ["Open", "Save", "Export PDF", "Compile"] {
                assert!(
                    app.harness().a11y_node_named(command).is_some(),
                    "published command {command}"
                );
            }
        }
    }
    fn control_rects(app: &TypstApp) -> Vec<(String, Rect)> {
        app.chrome
            .children()
            .iter()
            .filter(|child| child.get_rect().area() > 0.0)
            .map(|child| (child.name().to_string(), child.get_rect()))
            .collect()
    }
    #[test]
    fn controls_stay_in_the_bar_and_never_overlap_at_any_width() {
        for width in [320.0, 360.0, 390.0, 480.0, 560.0, 720.0, 800.0, 1280.0] {
            for (errors, warnings) in [(0, 0), (1, 1)] {
                let (mut app, _) = fresh();
                app.err_count = errors;
                app.warn_count = warnings;
                app.layout_in(Rect::new(0.0, 0.0, width, 760.0));
                render_once(&mut app);
                let rects = control_rects(&app);
                assert!(
                    rects.len() >= if app.compact { 3 } else { 2 },
                    "the document heading, toolbar and compact view switch show at {width}px"
                );
                for (index, (left_name, left)) in rects.iter().enumerate() {
                    assert!(
                        left.x0 >= app.bar_rect.x0
                            && left.x1 <= app.bar_rect.x1
                            && left.y0 >= app.bar_rect.y0
                            && left.y1 <= app.bar_rect.y1,
                        "{left_name} {left:?} leaves the bar {:?} at {width}px",
                        app.bar_rect
                    );
                    for (right_name, right) in rects.iter().skip(index + 1) {
                        assert!(
                            left.intersect(*right).area() <= 0.0,
                            "{left_name} {left:?} overlaps {right_name} {right:?} at {width}px"
                        );
                    }
                }
            }
        }
    }
    #[test]
    fn resize_reflows_between_split_and_compact() {
        // The responsive contract: layout is re-derived from the *current* width
        // every pass, so resizing across the threshold reflows both ways — it is
        // never frozen to the first frame's mode.
        let (mut app, _doc) = fresh();

        // Wide → split: both panes have area and the divider exists.
        app.layout_in(Rect::new(0.0, 0.0, 1280.0, 720.0));
        assert!(!app.compact);
        assert!(app.editor_rect.area() > 0.0);
        assert!(app.preview_rect.area() > 0.0);
        assert!(app.splitter.hit_rect().area() > 0.0);

        // Narrow → compact: a single pane, no divider.
        app.layout_in(Rect::new(0.0, 0.0, 560.0, 720.0));
        assert!(app.compact);
        assert_eq!(app.splitter.hit_rect().area(), 0.0);
        assert!(app.editor_rect.area() > 0.0, "editor pane shown");
        assert_eq!(app.preview_rect.area(), 0.0, "preview hidden in compact");

        // Resize back to wide → must reflow to split again (not frozen).
        app.layout_in(Rect::new(0.0, 0.0, 1280.0, 720.0));
        assert!(!app.compact, "reflowed back to split after widening");
        assert!(app.preview_rect.area() > 0.0);
    }
    #[test]
    fn text_scale_grows_the_bar_and_compacts_before_controls_collide() {
        // Enlarged system text changes the geometry as well as glyph metrics:
        // the taller controls stay in their own bar and a formerly cramped
        // split switches to the compact single-pane layout.
        let (mut app, _doc) = fresh();
        app.layout_in(Rect::new(0.0, 0.0, 1280.0, 720.0));
        let normal = app.frame.bar_h;
        let mut tr = TextRenderer::new();
        tr.set_text_scale(2.0);
        let mut theme = Theme::dark();
        theme.text_scale = 2.0;
        app.draw(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut tr,
            &theme,
            &PresenceCtx::empty(),
        );

        assert!(
            app.bar_rect.height() > normal,
            "the bar grows with the text"
        );
        assert!(app.compact, "1280px is compact at 200% text scale");
        assert_eq!(app.editor_rect.y0, app.bar_rect.y1);
        for (_, rect) in control_rects(&app) {
            assert!(
                rect.y1 <= app.bar_rect.y1,
                "every control fits the grown bar"
            );
        }
    }

    #[test]
    fn compact_view_switch_and_overflow_menu_act_and_stay_reachable() {
        let mut app = live_fixture((560, 720));
        let switch = app.harness().targets()["typst.view"];
        app.harness()
            .click_at(switch.x1 - switch.width() / 4.0, switch.center().y);
        app.settle();
        assert!(
            app.with_widget(|surface: &mut TypstApp| matches!(
                surface.compact_view,
                CompactPane::Preview
            ) && surface.preview_rect.area() > 0.0
                && surface.editor_rect.area() == 0.0)
                .unwrap()
        );
        app.harness()
            .click_at(switch.x0 + switch.width() / 4.0, switch.center().y);
        app.settle();
        assert!(
            app.with_widget(|surface: &mut TypstApp| matches!(
                surface.compact_view,
                CompactPane::Editor
            ))
            .unwrap()
        );
        for theme in [Theme::dark(), Theme::dark().with_text_scale(2.0)] {
            app.harness().relayout_themed((390, 520), theme).unwrap();
            let chip = app
                .with_widget(|surface: &mut TypstApp| surface.chrome.toolbar_chip())
                .unwrap()
                .expect("commands fold into the shared More menu");
            app.harness().click_at(chip.center().x, chip.center().y);
            let names = app
                .with_widget(|surface: &mut TypstApp| surface.chrome.toolbar_names())
                .unwrap();
            for wanted in [
                "Compile",
                "Save",
                "Open",
                "Export PDF",
                "Zoom out",
                "Zoom in",
                "Fit width",
                "Problems",
            ] {
                assert!(
                    names.iter().any(|name| name == wanted),
                    "{wanted} remains reachable in {names:?}"
                );
            }
            let zoom = app
                .harness()
                .a11y_node_named("Zoom in")
                .expect("the shared menu exposes Zoom in");
            let before = app
                .with_widget(|surface: &mut TypstApp| surface.preview.zoom())
                .unwrap();
            app.harness()
                .perform_a11y_action(zoom, &ui::a11y::Action::Activate)
                .unwrap();
            app.settle();
            assert!(
                app.with_widget(|surface: &mut TypstApp| surface.preview.zoom() > before)
                    .unwrap()
            );
        }
    }
}
