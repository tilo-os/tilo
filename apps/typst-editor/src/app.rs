//! The root widget of the Typst editor.
//!
//! `TypstApp` is a single full-bleed [`Widget`] that lays out and drives the
//! four panes of the editor itself (it does *not* compose them as taffy
//! children — each pane is a plain struct that draws into an absolute rect we
//! compute in [`TypstApp::apply_layout`]):
//!
//! ```text
//! ┌──────────────────────────────────────────────────────────┐
//! │ toolbar: ‹ doc › | Compile Save Open      − 100% + Fit ▢  │
//! ├───────────────────────────┬──────────────────────────────┤
//! │ source editor             ║      rendered preview         │
//! │ (monospace, line numbers, ║  (scrollable pages + thumbs)  │
//! │  syntax highlight)        ║                               │
//! ├───────────────────────────┤                               │
//! │ Problems panel            ║                               │
//! │              ┌──────────┐  ║  ┌───────────────┐            │
//! │              │Ln/Col·…·%│  ║  │● Compiled …ms │            │
//! └──────────────└──────────┘──╨──└───────────────┘───────────┘
//! ```
//!
//! There is no permanent bottom status strip: the caret/page/zoom readout and
//! the compile status float as two transient [`ui::StatusBubble`] pills in the
//! bottom corners (see `TypstApp::draw_status_bubbles`), so both content
//! panes span all the way to the window bottom.
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
use ui::widget::{AppModel, ModelPath};

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::paint::PaintCtx;
use ui::permissions::PermissionContext;
use ui::portal::{FileFilter, PickRequest, PortalChannel, PortalOutcome};
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
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
use ui::widget::Widget;

use crate::compile::{self, CompileOutput, WorldParts};
use crate::diagnostics::DiagnosticsPanel;
use crate::editor::SourceEditor;
use crate::preview::PreviewPane;
use crate::worker::CompileWorker;

/// Pixels-per-point the document is rasterised at. 2.0 keeps pages crisp on
/// a `HiDPI` surface; preview zoom only scales the *display*, never the render
/// resolution (matching the behaviour the welcome doc advertises).
const PPP: f32 = 2.0;

const TOOLBAR_H: f64 = 46.0;
const DIVIDER_W: f64 = 6.0;
const DIAG_H: f64 = 168.0;
const PILL_H: f64 = 28.0;
const PILL_FONT: f32 = 12.5;

/// Below this window width the side-by-side split can't give either pane a
/// usable size, so the editor folds to a single pane with an Edit/Preview
/// switch and an overflow menu. Re-evaluated from the live width every layout
/// pass, so resizing across the threshold reflows immediately.
const COMPACT_W: f64 = 760.0;
/// Horizontal label padding of one Edit / Preview segment at rest: one `s3`
/// a side, squeezed no further than `s2` when the compact bar is short.
const SEG_PAD_X: f64 = ui::tokens::spacing::Spacing::BASE.s3 as f64;

/// How long a transient toast (Save / Open feedback) stays on screen.
const FLASH_MS: u128 = 3200;

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

/// A toolbar button's logical action, resolved by hit-testing the cached
/// per-frame button rectangles.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TbAction {
    PrevDoc,
    NextDoc,
    Compile,
    Save,
    Open,
    ExportPdf,
    ZoomOut,
    ZoomIn,
    Fit,
    ToggleProblems,
    /// Compact layout: show the source editor pane.
    ShowEditor,
    /// Compact layout: show the rendered preview pane.
    ShowPreview,
    /// Compact layout: open/close the overflow menu.
    ToggleMenu,
}

// Native accessibility invokes the same logical operations as the painted toolbar.
const TOOLBAR_ACTIONS: [(TbAction, &str, &str); 13] = [
    (
        TbAction::PrevDoc,
        "typst.previous-document",
        "Previous document",
    ),
    (TbAction::NextDoc, "typst.next-document", "Next document"),
    (TbAction::Compile, "typst.compile", "Compile"),
    (TbAction::Save, "typst.save", "Save"),
    (TbAction::Open, "typst.open", "Open"),
    (TbAction::ExportPdf, "typst.export-pdf", "Export PDF"),
    (TbAction::ZoomOut, "typst.zoom-out", "Zoom out"),
    (TbAction::ZoomIn, "typst.zoom-in", "Zoom in"),
    (TbAction::Fit, "typst.fit-width", "Fit width"),
    (
        TbAction::ToggleProblems,
        "typst.toggle-problems",
        "Toggle problems",
    ),
    (TbAction::ShowEditor, "typst.show-editor", "Show editor"),
    (TbAction::ShowPreview, "typst.show-preview", "Show preview"),
    (TbAction::ToggleMenu, "typst.toggle-menu", "Toggle menu"),
];

#[allow(
    clippy::struct_excessive_bools,
    reason = "each bool tracks an independent piece of ephemeral view/drag \
              state (menu open, which demo scenario is pending, which drag \
              is live); grouping them into an enum would not read more \
              clearly at call sites like `self.dragging_divider`"
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

    // --- view state ---
    /// Editor↔preview horizontal split, 0..1 of the content width.
    split_ratio: f64,
    show_diagnostics: bool,
    err_count: usize,
    warn_count: usize,
    elapsed_ms: u128,
    /// Whether the current width put us in the single-pane compact layout.
    /// Recomputed every `apply_layout` from the live width — never cached from
    /// the first frame.
    compact: bool,
    /// The current theme's text scale, captured during layout construction so
    /// compact-mode decisions reserve space for enlarged controls too.
    layout_text_scale: f64,
    /// Which pane the compact layout is showing (ignored while `compact` is
    /// false).
    compact_view: CompactPane,
    /// Whether the compact overflow menu is open.
    menu_open: bool,
    /// Where the `⋯` overflow kebab was painted this frame (`ZERO` when every
    /// action fit as a pill); the overflow menu anchors to it.
    kebab_rect: Rect,
    /// Toolbar band height for this frame: `TOOLBAR_H` grown by the pill
    /// label's measured line growth. Measured in `draw`; `layout_in` reads it.
    toolbar_h: f64,
    /// Pill height for this frame (`self.pill_h` grown the same way).
    pill_h: f64,
    /// Measured line box of `PILL_FONT` — what every pill centres its label on.
    pill_line: f64,
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
    /// Screenshot-only: on the next layout pass, apply a failed compile over the
    /// good render so the capture proves the stale-render banner.
    demo_stale: bool,

    // --- layout rects (filled by apply_layout) ---
    rect: Rect,
    toolbar_rect: Rect,
    divider_rect: Rect,
    editor_rect: Rect,
    preview_rect: Rect,
    diag_rect: Rect,

    // --- interaction ---
    mouse: Point,
    dragging_divider: bool,
    dragging_editor: bool,
    /// A preview interaction (text-selection drag or scrollbar drag) is live, so
    /// subsequent moves route back to the preview.
    dragging_preview: bool,
    divider_hover: bool,
    /// Set when a pointer *move* changed something visible (hover, live drag)
    /// so `needs_redraw` asks the host to repaint — moves are otherwise gated.
    dirty: bool,
    /// Cached toolbar hit regions from the last draw.
    toolbar_hits: Vec<(TbAction, Rect)>,
    /// Cached overflow-menu hit regions from the last draw (compact layout).
    menu_hits: Vec<(TbAction, Rect)>,
    flash: Option<(String, Instant)>,

    // --- floating status bubbles (replacing the old permanent strip) ---
    /// Bottom-left readout: caret position · page count · zoom, or the current
    /// transient action toast when one is active. Poked on any change to that
    /// composed label, and pinned open while the pointer hovers the bottom-left
    /// corner; fades otherwise.
    status_bubble_left: ui::StatusBubble,
    /// Bottom-right compile status: "Compiling…", error/warning counts, or the
    /// last compile time — with a leading dot in the matching semantic colour.
    /// Poked on a new compile result; revealed on bottom-right-corner hover.
    status_bubble_right: ui::StatusBubble,
    /// Last composed labels, so each bubble is poked only on a *real* change
    /// (`draw` recomputes them every frame).
    last_left_label: String,
    last_right_label: String,
    /// Whether the pointer currently sits in the (left, right) bottom-corner
    /// reveal zones — tracked so a repaint is requested only when it changes.
    status_hover: (bool, bool),
    /// `--shots` forces both bubbles fully visible so a still frame captures them.
    shots_status: bool,
    /// Where the workspace's edits go, once the surface is attached.
    actions: Option<ui::framework::Dispatcher<crate::resource::TypstEditor>>,
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
            resource_invocation: None,
            pending_resource_io: None,
            resource_result: None,
            initialized: false,
            last_source: String::new(),
            source_cache: String::new(),
            split_ratio: 0.46,
            show_diagnostics: false,
            err_count: 0,
            warn_count: 0,
            elapsed_ms: 0,
            compact: false,
            layout_text_scale: 1.0,
            compact_view: CompactPane::Editor,
            menu_open: false,
            kebab_rect: Rect::ZERO,
            toolbar_h: TOOLBAR_H,
            pill_h: PILL_H,
            pill_line: f64::from(PILL_FONT) * 1.25,
            text_k: 1.0,
            demo_scroll_caret_end: false,
            demo_editor_select: false,
            demo_preview_zoom: false,
            demo_preview_pan: false,
            demo_preview_select: false,
            demo_stale: false,
            rect: Rect::ZERO,
            toolbar_rect: Rect::ZERO,
            divider_rect: Rect::ZERO,
            editor_rect: Rect::ZERO,
            preview_rect: Rect::ZERO,
            diag_rect: Rect::ZERO,
            mouse: Point::ZERO,
            dragging_divider: false,
            dragging_editor: false,
            dragging_preview: false,
            divider_hover: false,
            dirty: true,
            toolbar_hits: Vec::new(),
            menu_hits: Vec::new(),
            flash: None,
            status_bubble_left: ui::StatusBubble::new(),
            status_bubble_right: ui::StatusBubble::new(),
            last_left_label: String::new(),
            last_right_label: String::new(),
            status_hover: (false, false),
            shots_status: false,
            actions: None,
        }
    }

    /// Attach the workspace's dispatcher; a capture's first file.
    pub(crate) fn attach_surface(
        &mut self,
        cx: &ui::framework::SurfaceCx<crate::resource::TypstEditor>,
    ) {
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

    /// Where the file shown is in the workspace.
    fn current_index(&self) -> Option<usize> {
        let current = self.current.borrow();
        let id = current.as_deref()?;
        self.files.files.iter().position(|file| file.id == id)
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
                self.menu_open = true;
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
                self.shots_status = true;
                let now = ui::time::now();
                self.status_bubble_left.poke(now);
                self.status_bubble_right.poke(now);
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
                self.preview
                    .set_placeholder(Some("Compilation failed — see the Problems panel.".into()));
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
        self.set_flash("Recompiling…".into());
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
        self.set_flash("Choose where to save…".into());
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
                self.set_flash(format!("Save failed: {e}"));
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
        self.set_flash("Choose where to export the PDF…".into());
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
        self.set_flash("Choose a .typ file to open…".into());
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
        let known = self
            .files
            .files
            .iter()
            .find(|file| file.path.as_deref() == Some(path.as_str()))
            .map(|file| file.id.clone());
        if let Some(id) = known {
            self.open_file(&id);
            self.replace_source(&src);
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

    /// Show the next file of the workspace (`step` 1) or the previous one
    /// (`step` -1), wrapping around.
    fn step_file(&mut self, step: isize) {
        let count = self.files.files.len();
        if count == 0 {
            return;
        }
        let at = self.current_index().unwrap_or(0);
        let next = (at as isize + step).rem_euclid(count as isize) as usize;
        if let Some(id) = self.files.files.get(next).map(|file| file.id.clone()) {
            self.open_file(&id);
        }
    }

    fn do_action(&mut self, action: TbAction) {
        // Any menu item closes the overflow menu once actioned.
        if !matches!(action, TbAction::ToggleMenu) {
            self.menu_open = false;
        }
        match action {
            TbAction::PrevDoc => self.step_file(-1),
            TbAction::NextDoc => self.step_file(1),
            TbAction::Compile => {
                self.perform_resource_operation(crate::actions::TypstOperation::Compile);
            }
            TbAction::Save => {
                self.perform_resource_operation(crate::actions::TypstOperation::Save {
                    path: None,
                });
            }
            TbAction::Open => {
                self.perform_resource_operation(crate::actions::TypstOperation::Open {
                    path: None,
                });
            }
            TbAction::ExportPdf => {
                self.perform_resource_operation(crate::actions::TypstOperation::ExportPdf {
                    path: None,
                });
            }
            TbAction::ZoomOut => {
                self.perform_resource_operation(crate::actions::TypstOperation::ZoomOut);
                self.set_compact_view(CompactPane::Preview);
            }
            TbAction::ZoomIn => {
                self.perform_resource_operation(crate::actions::TypstOperation::ZoomIn);
                self.set_compact_view(CompactPane::Preview);
            }
            TbAction::Fit => {
                self.perform_resource_operation(crate::actions::TypstOperation::FitWidth);
                self.set_compact_view(CompactPane::Preview);
            }
            TbAction::ToggleProblems => {
                self.perform_resource_operation(crate::actions::TypstOperation::ToggleProblems);
            }
            TbAction::ShowEditor => {
                self.perform_resource_operation(crate::actions::TypstOperation::ShowEditor);
            }
            TbAction::ShowPreview => {
                self.perform_resource_operation(crate::actions::TypstOperation::ShowPreview);
            }
            TbAction::ToggleMenu => self.menu_open = !self.menu_open,
        }
    }

    /// Execute the semantic operation shared by exported resources and direct
    /// toolbar/shortcut affordances.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "operations are decoded as owned values and dispatched immediately"
    )]
    fn perform_resource_operation(&mut self, operation: crate::actions::TypstOperation) {
        use crate::actions::TypstOperation;
        match operation {
            TypstOperation::Compile => self.force_recompile(),
            TypstOperation::Save { .. } => {
                let _ = self.save();
            }
            TypstOperation::Open { .. } => self.open(),
            TypstOperation::ExportPdf { .. } => self.export_pdf(),
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

    fn set_flash(&mut self, msg: String) {
        self.flash = Some((msg, ui::time::now()));
    }

    fn flash_text(&self) -> Option<&str> {
        self.flash.as_ref().and_then(|(m, t)| {
            if t.elapsed().as_millis() < FLASH_MS {
                Some(m.as_str())
            } else {
                None
            }
        })
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

    /// Compute every pane rect from a content rect. Split out from
    /// [`Widget::apply_layout`] so resize reflow is unit-testable without a
    /// taffy pass. The layout mode is re-derived from `rect`'s width every call
    /// — never cached from the first frame.
    fn layout_in(&mut self, rect: Rect) {
        self.rect = rect;
        let r = self.rect;
        // The band is the 1× `TOOLBAR_H` grown by the pill label's measured
        // line growth (cached by `draw`, which re-runs this layout when the
        // measurement changes), so the pills keep seating their text at 200 %.
        self.toolbar_rect = Rect::new(r.x0, r.y0, r.x1, r.y0 + self.toolbar_h);
        let content_top = self.toolbar_rect.y1;
        // No permanent status strip: the content spans to the window bottom and
        // the status floats as two transient bubbles (see `draw_status_bubbles`).
        let content_bottom = r.y1;
        let content_w = r.width();

        // Derive the layout mode from the *current* width every pass, so a
        // resize across the threshold reflows without any cached-on-launch mode.
        // The breakpoint was designed at 1×: a window holds as much 2× text as
        // a half-width 1× window, so compare in 1× space.
        self.compact = content_w / self.text_k.max(f64::from(f32::EPSILON)) < COMPACT_W;
        if !self.compact {
            self.menu_open = false;
        }

        if self.compact {
            self.divider_rect = Rect::ZERO;
            let pane = Rect::new(r.x0, content_top, r.x1, content_bottom);
            let (main, diag) = self.split_off_diagnostics(pane);
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
            let split_x = r.x0 + content_w * self.split_ratio;
            let editor_col = Rect::new(r.x0, content_top, split_x, content_bottom);
            self.preview_rect = Rect::new(split_x + DIVIDER_W, content_top, r.x1, content_bottom);
            self.divider_rect =
                Rect::new(split_x, content_top, split_x + DIVIDER_W, content_bottom);
            let (main, diag) = self.split_off_diagnostics(editor_col);
            self.editor_rect = main;
            self.diag_rect = diag;
        }

        self.editor.set_rect(self.editor_rect);
        self.preview.set_rect(self.preview_rect);
        self.diagnostics.set_rect(self.diag_rect);

        // Screenshot harness only: now that the editor has a real rect, place
        // the caret at the end of the source and scroll it into view so the
        // capture shows the horizontally-scrolled state.
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

        // Screenshot harness only: now that the preview has a real rect + pages,
        // arrange the zoom/scroll/selection the capture is meant to show.
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
        if self.demo_stale && self.preview.page_count() > 0 {
            self.demo_stale = false;
            // Apply a failed compile over the good render so the capture proves
            // the last-successful-render fallback + stale banner.
            let bad = compile::compile_source(&self.parts, "Body #undefined_var end.\n", PPP);
            self.apply_output(bad);
        }
    }

    /// Carve a bottom diagnostics strip out of `col` when the Problems panel is
    /// open and there's room; otherwise the whole column stays as content.
    fn split_off_diagnostics(&self, col: Rect) -> (Rect, Rect) {
        if self.show_diagnostics && col.height() > 120.0 {
            let dh = DIAG_H.min(col.height() * 0.5);
            (
                Rect::new(col.x0, col.y0, col.x1, col.y1 - dh),
                Rect::new(col.x0, col.y1 - dh, col.x1, col.y1),
            )
        } else {
            (col, Rect::ZERO)
        }
    }

    fn hit_toolbar(&self, p: Point) -> Option<TbAction> {
        self.toolbar_hits
            .iter()
            .find(|(_, r)| r.contains(p))
            .map(|(a, _)| *a)
    }

    fn hit_menu(&self, p: Point) -> Option<TbAction> {
        self.menu_hits
            .iter()
            .find(|(_, r)| r.contains(p))
            .map(|(a, _)| *a)
    }

    // ----- drawing --------------------------------------------------------

    /// Whether `action`'s toolbar control paints in its on state.
    fn is_active(&self, action: TbAction) -> bool {
        match action {
            TbAction::ToggleMenu => self.menu_open,
            TbAction::ToggleProblems => self.show_diagnostics,
            TbAction::ShowEditor => matches!(self.compact_view, CompactPane::Editor),
            TbAction::ShowPreview => !matches!(self.compact_view, CompactPane::Editor),
            TbAction::PrevDoc
            | TbAction::NextDoc
            | TbAction::Compile
            | TbAction::Save
            | TbAction::Open
            | TbAction::ExportPdf
            | TbAction::ZoomOut
            | TbAction::ZoomIn
            | TbAction::Fit => false,
        }
    }

    fn paint_pill(&mut self, ctx: &mut PaintCtx<'_>, rect: Rect, label: &str, action: TbAction) {
        let PaintCtx {
            scene,
            transform: t,
            text: tr,
            theme,
        } = ctx.reborrow();
        let active = self.is_active(action);
        let c = &theme.tokens.colors;
        if active {
            let shape = RoundedRect::from_rect(rect, 7.0);
            scene.fill(Fill::NonZero, t, theme.accent_primary, None, &shape);
        } else {
            // A rest button carries its derived edge (HIG §9.5), never the
            // declared `border_standard` that vanished on the dark toolbar.
            ui::surface::card(scene, t, rect, 7.0, c.lift(0.35));
        }
        let fg = if active {
            c.on_accent(ui::tokens::colors::ON_FILL_MIN_CONTRAST)
        } else {
            c.text_primary
        };
        // Centre on the measured line box, never `size × 1.25`.
        let (tw, th) = tr.measure_weighted(label, PILL_FONT, None, TextWeight::Medium);
        let tx = rect.x0 + (rect.width() - f64::from(tw)) / 2.0;
        let ty = rect.y0 + (rect.height() - f64::from(th)) / 2.0;
        tr.draw_text_weighted(
            scene,
            label,
            PILL_FONT,
            fg,
            t * Affine::translate((tx, ty)),
            None,
            TextWeight::Medium,
        );
        self.toolbar_hits.push((action, rect));
    }

    fn pill_width(tr: &mut TextRenderer, label: &str) -> f64 {
        let (tw, _) = tr.measure_weighted(label, PILL_FONT, None, TextWeight::Medium);
        f64::from(tw) + 22.0
    }

    fn draw_toolbar(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        self.toolbar_hits.clear();
        self.menu_hits.clear();
        let c = &theme.tokens.colors;
        let tb = self.toolbar_rect;
        scene.fill(Fill::NonZero, t, c.surfaces.raised, None, &tb);
        // Bar role: one derived hairline under the toolbar.
        scene.fill(
            Fill::NonZero,
            t,
            ui::chrome::chrome_hairline(theme),
            None,
            &Rect::new(tb.x0, tb.y1 - 1.0, tb.x1, tb.y1),
        );

        let y = tb.y0 + (tb.height() - self.pill_h) / 2.0;
        if self.compact {
            self.draw_toolbar_compact(scene, t, tr, theme, y);
        } else {
            self.draw_toolbar_full(scene, t, tr, theme, y);
        }
    }

    fn draw_toolbar_full(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        y: f64,
    ) {
        let tb = self.toolbar_rect;
        // Width breakpoints were designed at 1×; judge them in 1× space so a
        // 200 % bar folds exactly like a half-width 1× bar would.
        let w1x = tb.width() / self.text_k.max(f64::from(f32::EPSILON));
        // Show the "n/m" index only when there's plenty of room.
        let with_index = w1x >= 1000.0;
        let switcher_cap = 220.0 * self.text_k;
        let mut x = self.draw_doc_switcher(
            &mut PaintCtx::new(scene, t, tr, theme),
            Point::new(tb.x0 + 12.0, y),
            switcher_cap,
            with_index,
        );
        x += 10.0;

        // --- right group: zoom + problems (placed from the right edge) ---
        // Painted first so the action group knows exactly how much width is
        // left for its pills.
        let mut rx = tb.x1 - 12.0;
        rx = self.draw_problems_pill(scene, t, tr, theme, rx, y, true);
        rx -= 8.0;

        // zoom cluster: [ − ] [ 100% ] [ + ] [ Fit ]  (right to left)
        for (lbl, act) in [("Fit", TbAction::Fit), ("+", TbAction::ZoomIn)] {
            let pill_w = Self::pill_width(tr, lbl);
            let pill_rect = Rect::new(rx - pill_w, y, rx, y + self.pill_h);
            self.paint_pill(&mut PaintCtx::new(scene, t, tr, theme), pill_rect, lbl, act);
            rx -= pill_w + 6.0;
        }
        // zoom percentage chip (non-interactive)
        let c = &theme.tokens.colors;
        let zlabel = format!("{:.0}%", self.preview.zoom() * 100.0);
        let (zw, _) = tr.measure_weighted(&zlabel, PILL_FONT, None, TextWeight::Medium);
        let zchip_w = f64::from(zw) + 16.0;
        let zr = Rect::new(rx - zchip_w, y, rx, y + self.pill_h);
        tr.draw_text_weighted(
            scene,
            &zlabel,
            PILL_FONT,
            c.text_dim,
            t * Affine::translate((zr.x0 + 8.0, y + (self.pill_h - self.pill_line) / 2.0)),
            None,
            TextWeight::Medium,
        );
        rx -= zchip_w + 6.0;
        let zow = Self::pill_width(tr, "−");
        let zor = Rect::new(rx - zow, y, rx, y + self.pill_h);
        self.paint_pill(
            &mut PaintCtx::new(scene, t, tr, theme),
            zor,
            "−",
            TbAction::ZoomOut,
        );
        let right_edge = zor.x0 - 12.0;

        // --- action group ---
        // Every action is measured at this frame's text scale and placed left
        // to right; the first one that would run into the zoom cluster — and
        // every one after it — folds into the overflow menu behind a `⋯`
        // kebab, which is only painted when something folded. "Export PDF"
        // therefore never disappears: it is either a pill or a menu row.
        let actions: [(&str, TbAction); 4] = [
            ("Compile", TbAction::Compile),
            ("Save", TbAction::Save),
            ("Open", TbAction::Open),
            ("Export PDF", TbAction::ExportPdf),
        ];
        let kebab_w = Self::pill_width(tr, "⋯");
        let widths: Vec<f64> = actions
            .iter()
            .map(|(lbl, _)| Self::pill_width(tr, lbl))
            .collect();
        // How many leading actions fit as pills, leaving room for the kebab
        // whenever at least one action folds.
        let mut shown = actions.len();
        while shown > 0 {
            let pills_w: f64 = widths.iter().take(shown).map(|w| w + 8.0).sum();
            let kebab = if shown < actions.len() {
                kebab_w + 8.0
            } else {
                0.0
            };
            if x + pills_w + kebab - 8.0 <= right_edge {
                break;
            }
            shown -= 1;
        }
        for ((lbl, act), pill_w) in actions.iter().zip(&widths).take(shown) {
            self.paint_pill(
                &mut PaintCtx::new(scene, t, tr, theme),
                Rect::new(x, y, x + pill_w, y + self.pill_h),
                lbl,
                *act,
            );
            x += pill_w + 8.0;
        }
        if shown < actions.len() {
            let kr = Rect::new(x, y, x + kebab_w, y + self.pill_h);
            self.kebab_rect = kr;
            self.paint_pill(
                &mut PaintCtx::new(scene, t, tr, theme),
                kr,
                "⋯",
                TbAction::ToggleMenu,
            );
        } else {
            // Every action fits as a pill: there is no kebab, so an open menu
            // (from a narrower bar) closes rather than lingering unseen and
            // swallowing the next click.
            self.kebab_rect = Rect::ZERO;
            self.menu_open = false;
        }
    }

    fn draw_toolbar_compact(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        y: f64,
    ) {
        let tb = self.toolbar_rect;
        // Right edge, inward: overflow kebab, then a problems badge but only
        // when there's actually something to show (an empty badge is noise).
        let mut rx = tb.x1 - 12.0;
        let kebab_w = 30.0;
        let kr = Rect::new(rx - kebab_w, y, rx, y + self.pill_h);
        self.kebab_rect = kr;
        self.paint_pill(
            &mut PaintCtx::new(scene, t, tr, theme),
            kr,
            "⋯",
            TbAction::ToggleMenu,
        );
        rx -= kebab_w + 8.0;
        if self.err_count > 0 || self.warn_count > 0 {
            rx = self.draw_problems_pill(scene, t, tr, theme, rx, y, false);
            rx -= 10.0;
        }

        // Left cluster first. The name chip is capped so the whole `‹ name ›`
        // group leaves room for the view switch plus an `s3` on either side —
        // the arrows are part of the group and must not be forgotten, or the
        // `›` lands under the switch at phone widths.
        let gap = f64::from(theme.tokens.spacing.s3);
        let left_x = tb.x0 + 12.0;
        let switch_w = Self::view_switch_width(tr);
        let arrows = if self.files.files.len() > 1 {
            Self::pill_width(tr, "‹") + 6.0 + 6.0 + Self::pill_width(tr, "›")
        } else {
            0.0
        };
        // The chip may elide all the way down to a lone ellipsis before the
        // switch is allowed to give ground; a fixed 64 px floor left the two
        // colliding on a 360 px bar with two documents and a problems badge.
        let (ell_w, _) = tr.measure_weighted("…", PILL_FONT, None, TextWeight::Semibold);
        let min_chip = f64::from(ell_w) + 24.0;
        let max_chip = (rx - gap - switch_w - gap - left_x - arrows).max(min_chip);
        let cluster_end = self.draw_doc_switcher(
            &mut PaintCtx::new(scene, t, tr, theme),
            Point::new(left_x, y),
            max_chip,
            false,
        );

        // Segmented Edit / Preview switch: centred in the toolbar when that
        // fits, otherwise pushed right of the left cluster (never over it) and
        // kept clear of the right cluster by the same `s3`.
        let _ = self.draw_view_switch(scene, t, tr, theme, y, cluster_end + gap, rx - gap);
    }

    /// Draw the `‹ [name] ›` document switcher starting at `origin`. `max_chip` caps
    /// the name chip width (the name is elided to fit). Returns the x just past
    /// the group.
    fn draw_doc_switcher(
        &mut self,
        ctx: &mut PaintCtx<'_>,
        origin: Point,
        max_chip: f64,
        with_index: bool,
    ) -> f64 {
        let theme = ctx.theme;
        let c = &theme.tokens.colors;
        let Point { mut x, y } = origin;
        let multi = self.files.files.len() > 1;

        let prev_w = Self::pill_width(ctx.text, "‹");
        if multi {
            self.paint_pill(
                ctx,
                Rect::new(x, y, x + prev_w, y + self.pill_h),
                "‹",
                TbAction::PrevDoc,
            );
            x += prev_w + 6.0;
        }

        let name = self
            .current_file()
            .map_or_else(|| "untitled".into(), |file| file.name.clone());
        let base = if with_index && multi {
            format!(
                "{name}   {}/{}",
                self.current_index().unwrap_or(0) + 1,
                self.files.files.len()
            )
        } else {
            name
        };
        // Elide to fit the cap.
        let label = ctx.text.elide_to_width_weighted(
            &base,
            PILL_FONT,
            ui::num::f32_of_f64(max_chip - 24.0),
            TextWeight::Semibold,
        );
        let (lw, _) = ctx
            .text
            .measure_weighted(&label, PILL_FONT, None, TextWeight::Semibold);
        let chip_w = (f64::from(lw) + 24.0).min(max_chip);
        let chip = Rect::new(x, y, x + chip_w, y + self.pill_h);
        ui::surface::card(ctx.scene, ctx.transform, chip, 7.0, c.surfaces.base);
        ctx.text.draw_text_weighted(
            ctx.scene,
            &label,
            PILL_FONT,
            c.text_primary,
            ctx.transform * Affine::translate((x + 12.0, y + (self.pill_h - self.pill_line) / 2.0)),
            None,
            TextWeight::Semibold,
        );
        x += chip_w + 6.0;

        if multi {
            let next_w = Self::pill_width(ctx.text, "›");
            self.paint_pill(
                ctx,
                Rect::new(x, y, x + next_w, y + self.pill_h),
                "›",
                TbAction::NextDoc,
            );
            x += next_w;
        }
        x
    }

    /// Measured widths of the Edit / Preview segments at this frame's scale,
    /// each label plus `SEG_PAD_X` a side.
    fn view_switch_segments(tr: &mut TextRenderer) -> (f64, f64) {
        let (ew, _) = tr.measure_weighted("Edit", PILL_FONT, None, TextWeight::Medium);
        let (pw, _) = tr.measure_weighted("Preview", PILL_FONT, None, TextWeight::Medium);
        (
            f64::from(ew) + 2.0 * SEG_PAD_X,
            f64::from(pw) + 2.0 * SEG_PAD_X,
        )
    }

    /// Total width the Edit / Preview switch paints at.
    fn view_switch_width(tr: &mut TextRenderer) -> f64 {
        let (ew, pw) = Self::view_switch_segments(tr);
        ew + pw
    }

    /// A two-segment Edit / Preview toggle, centred in the toolbar when the
    /// centre lies inside `[min_x0, max_x1]`, otherwise clamped into that span
    /// so it never overlaps the clusters on either side. Returns its rect.
    fn draw_view_switch(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        y: f64,
        min_x0: f64,
        max_x1: f64,
    ) -> Rect {
        let c = &theme.tokens.colors;
        let tb = self.toolbar_rect;
        let seg_edit = "Edit";
        let seg_prev = "Preview";
        let (mut ew, mut pw) = Self::view_switch_segments(tr);
        // When even the elided document chip leaves too little span, the two
        // segments give up label padding symmetrically (down to `s2` a side)
        // before anything is allowed to overlap.
        let avail = (max_x1 - min_x0).max(0.0);
        let over = ew + pw - avail;
        if over > 0.0 {
            let min_pad = f64::from(theme.tokens.spacing.s2);
            let squeeze = (over / 4.0).min(SEG_PAD_X - min_pad).max(0.0);
            ew -= 2.0 * squeeze;
            pw -= 2.0 * squeeze;
        }
        let total = ew + pw;
        let centred = (tb.x0 + tb.width() * 0.5 - total * 0.5).round();
        // Clamp toward the left cluster last so, when the span is still too
        // short, the switch yields on the right rather than sliding under the
        // document group; `draw_toolbar_compact` sizes the chip so that only
        // happens below the narrowest supported viewport.
        let x0 = centred.min(max_x1 - total).max(min_x0).round();
        let outer = Rect::new(x0, y, x0 + total, y + self.pill_h);
        ui::surface::card(scene, t, outer, 8.0, c.lift(0.35));

        let mut ctx = PaintCtx::new(scene, t, tr, theme);
        // Edit segment.
        let er = Rect::new(x0, y, x0 + ew, y + self.pill_h);
        self.paint_segment(&mut ctx, er, seg_edit, TbAction::ShowEditor);
        // Preview segment.
        let pr = Rect::new(x0 + ew, y, x0 + ew + pw, y + self.pill_h);
        self.paint_segment(&mut ctx, pr, seg_prev, TbAction::ShowPreview);
        outer
    }

    /// A "Problems" pill placed from its right edge at `rx`. `long` uses the
    /// full "Problems (n)" label; otherwise a compact count badge. Returns the
    /// left edge just past the pill.
    fn draw_problems_pill(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rx: f64,
        y: f64,
        long: bool,
    ) -> f64 {
        let count = if self.err_count > 0 {
            self.err_count
        } else {
            self.warn_count
        };
        let label = if long {
            if count > 0 {
                format!("Problems ({count})")
            } else {
                "Problems".to_string()
            }
        } else if count > 0 {
            format!("● {count}")
        } else {
            "●".to_string()
        };
        let w = Self::pill_width(tr, &label);
        let r = Rect::new(rx - w, y, rx, y + self.pill_h);
        self.paint_pill(
            &mut PaintCtx::new(scene, t, tr, theme),
            r,
            &label,
            TbAction::ToggleProblems,
        );
        r.x0
    }

    /// Paint one segment of a segmented control (no rounded outline of its own;
    /// the container draws that).
    fn paint_segment(&mut self, ctx: &mut PaintCtx<'_>, rect: Rect, label: &str, action: TbAction) {
        let PaintCtx {
            scene,
            transform: t,
            text: tr,
            theme,
        } = ctx.reborrow();
        let active = self.is_active(action);
        let c = &theme.tokens.colors;
        if active {
            let shape = RoundedRect::from_rect(rect.inflate(-2.0, -2.0), 6.0);
            scene.fill(Fill::NonZero, t, theme.accent_primary, None, &shape);
        }
        let fg = if active {
            c.on_accent(ui::tokens::colors::ON_FILL_MIN_CONTRAST)
        } else {
            c.text_primary
        };
        // Centre on the measured line box, never `size × 1.25`.
        let (tw, th) = tr.measure_weighted(label, PILL_FONT, None, TextWeight::Medium);
        let tx = rect.x0 + (rect.width() - f64::from(tw)) / 2.0;
        let ty = rect.y0 + (rect.height() - f64::from(th)) / 2.0;
        tr.draw_text_weighted(
            scene,
            label,
            PILL_FONT,
            fg,
            t * Affine::translate((tx, ty)),
            None,
            TextWeight::Medium,
        );
        self.toolbar_hits.push((action, rect));
    }

    /// Draw the compact overflow menu as a floating panel anchored under the
    /// kebab button.
    fn draw_menu(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        self.menu_hits.clear();
        let c = &theme.tokens.colors;
        let problems_label = if self.show_diagnostics {
            "Hide problems"
        } else {
            "Show problems"
        };
        let items: [(&str, TbAction); 8] = [
            ("Recompile", TbAction::Compile),
            ("Save", TbAction::Save),
            ("Open…", TbAction::Open),
            ("Export PDF", TbAction::ExportPdf),
            ("Zoom in", TbAction::ZoomIn),
            ("Zoom out", TbAction::ZoomOut),
            ("Fit width", TbAction::Fit),
            (problems_label, TbAction::ToggleProblems),
        ];
        // Rows are the 1× 32 px grown by the label line's growth; each label
        // centres on its measured line box.
        let row_h = 32.0 + (self.pill_h - PILL_H);
        let pad = 6.0;
        let available_w = (self.rect.width() - 16.0).max(1.0);
        let mut panel_w: f64 = (150.0 * self.layout_text_scale).min(available_w);
        for (lbl, _) in &items {
            let (lw, _) = tr.measure(lbl, PILL_FONT, None);
            panel_w = panel_w.max(f64::from(lw) + 28.0).min(available_w);
        }
        let h = items.len() as f64 * row_h + pad * 2.0;
        // Right-aligned under the kebab that opened it (compact: the bar's
        // trailing pill; wide: wherever the folded action group ended), kept
        // inside the window.
        let anchor_x1 = if self.kebab_rect.width() > 0.0 {
            self.kebab_rect.x1
        } else {
            self.toolbar_rect.x1 - 10.0
        };
        let x1 = anchor_x1
            .min(self.rect.x1 - 8.0)
            .max(self.rect.x0 + 8.0 + panel_w);
        let x0 = (x1 - panel_w).max(self.rect.x0 + 8.0);
        // Prefer the conventional below-toolbar anchor, then lift the whole
        // panel just enough to keep every operation reachable in a short
        // window.  It may overlay the toolbar in a very short viewport, but
        // it never falls below the screen where keyboard and pointer users
        // cannot reach its final rows.
        let y0 = (self.toolbar_rect.y1 + 4.0)
            .min(self.rect.y1 - h - 8.0)
            .max(self.rect.y0 + 8.0);
        let panel = Rect::new(x0, y0, x1, y0 + h);

        // Panel role (HIG §9.5): the shared floating-panel ground and its
        // derived border, so this menu matches every other menu on the
        // desktop instead of a declared edge that vanished on dark.
        ui::surface::shadow(scene, t, panel, 10.0, &theme.tokens.elevation.e3);
        scene.fill(
            Fill::NonZero,
            t,
            ui::chrome::panel_ground(theme),
            None,
            &RoundedRect::from_rect(panel, 10.0),
        );
        scene.stroke(
            &Stroke::new(1.0),
            t,
            ui::chrome::panel_border(theme),
            None,
            &ui::surface::edge_outline(panel, 10.0),
        );

        let mut y = y0 + pad;
        for (lbl, act) in items {
            let row = Rect::new(x0 + pad, y, x1 - pad, y + row_h);
            if row.contains(self.mouse) {
                scene.fill(
                    Fill::NonZero,
                    t,
                    c.lift(0.35),
                    None,
                    &RoundedRect::from_rect(row, 6.0),
                );
            }
            let label = tr.elide_to_width_weighted(
                lbl,
                PILL_FONT,
                ui::num::f32_of_f64((row.width() - 20.0).max(1.0)),
                TextWeight::Semibold,
            );
            tr.draw_text(
                scene,
                &label,
                PILL_FONT,
                c.text_primary,
                t * Affine::translate((row.x0 + 10.0, y + (row_h - self.pill_line) / 2.0)),
                None,
            );
            self.menu_hits.push((act, row));
            y += row_h;
        }
    }

    fn draw_divider(&self, scene: &mut Scene, t: Affine, theme: &Theme) {
        let c = &theme.tokens.colors;
        let dr = self.divider_rect;
        let active = self.dragging_divider || self.divider_hover;
        let fill = if active {
            theme.accent_primary
        } else {
            c.surfaces.raised
        };
        scene.fill(Fill::NonZero, t, fill, None, &dr);
        // grip dots
        let cx = f64::midpoint(dr.x0, dr.x1);
        let cy = f64::midpoint(dr.y0, dr.y1);
        let dot_color = if active {
            c.on_accent(ui::tokens::colors::NON_TEXT_MIN_CONTRAST)
        } else {
            c.text_dim
        };
        for k in -1i32..=1 {
            let cyk = cy + f64::from(k) * 6.0;
            scene.fill(
                Fill::NonZero,
                t,
                dot_color,
                None,
                &vello::kurbo::Circle::new(Point::new(cx, cyk), 1.4),
            );
        }
    }

    /// Compose the bottom-left readout: the current transient action toast when
    /// one is active, otherwise `Ln L, Col C · N pages · Z%`.
    fn status_left_label(&self) -> String {
        if let Some(msg) = self.flash_text() {
            return msg.to_string();
        }
        let (ln, col) = self.caret_line_col();
        let pages = self.preview.page_count();
        format!(
            "Ln {ln}, Col {col}   ·   {}   ·   {:.0}%",
            plural(pages, "page", "pages"),
            self.preview.zoom() * 100.0
        )
    }

    /// Compose the bottom-right compile status and the semantic colour of its
    /// leading dot.
    fn status_right_label(&self, theme: &Theme) -> (String, Color) {
        let c = &theme.tokens.colors;
        if *self.parts.fonts() == compile::Fonts::Loading {
            ("Loading fonts…".to_string(), c.semantic.info)
        } else if self.compiling() {
            ("Compiling…".to_string(), c.semantic.info)
        } else if self.err_count > 0 {
            (
                format!(
                    "{}, {}",
                    plural(self.err_count, "error", "errors"),
                    plural(self.warn_count, "warning", "warnings")
                ),
                c.semantic.danger,
            )
        } else if self.warn_count > 0 {
            (
                plural(self.warn_count, "warning", "warnings"),
                c.semantic.warning,
            )
        } else {
            (
                format!("Compiled in {} ms", self.elapsed_ms),
                c.semantic.success,
            )
        }
    }

    /// The (left, right) bottom-corner reveal bands: nudging the pointer into
    /// either one re-surfaces the matching bubble on demand.
    fn status_zones(&self) -> (Rect, Rect) {
        let r = self.rect;
        let band_h = 56.0;
        let band_w = 320.0_f64.min((r.width() / 2.0).max(0.0));
        let top = (r.y1 - band_h).max(r.y0);
        let left = Rect::new(r.x0, top, r.x0 + band_w, r.y1);
        let right = Rect::new(r.x1 - band_w, top, r.x1, r.y1);
        (left, right)
    }

    /// Draw the two transient floating status bubbles in the bottom corners, in
    /// place of the old permanent status strip. The left pill carries the caret
    /// / page / zoom readout (or the active toast); the right pill carries the
    /// compile status with a leading dot in its semantic colour. Each shows on
    /// a change to its label, or while the pointer hovers its corner, then
    /// fades — all colours from the theme (via [`ui::status_bubble::draw_pill`]).
    fn draw_status_bubbles(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
    ) {
        let now = ui::time::now();
        let (left_zone, right_zone) = self.status_zones();
        let text_size = 12.0f32;
        let margin = 14.0;

        // --- bottom-left: caret / page / zoom (or the active toast) ---
        let left_label = self.status_left_label();
        if left_label != self.last_left_label {
            self.status_bubble_left.poke(now);
            self.last_left_label.clone_from(&left_label);
        }
        let (lw, lh) = ui::status_bubble::pill_size(&left_label, tr, text_size, 0.0);
        let lrect = Rect::new(
            self.rect.x0 + margin,
            self.rect.y1 - margin - lh,
            self.rect.x0 + margin + lw,
            self.rect.y1 - margin,
        );
        let left_over =
            self.shots_status || left_zone.contains(self.mouse) || lrect.contains(self.mouse);
        self.status_bubble_left.set_hover(left_over, now);
        let la = self.status_bubble_left.alpha(now);
        if la > 0.0 {
            scene.push_layer(
                Fill::NonZero,
                vello::peniko::BlendMode::default(),
                la,
                t,
                &lrect.inflate(12.0, 14.0),
            );
            ui::status_bubble::draw_pill(scene, t, tr, theme, lrect, &left_label, text_size);
            scene.pop_layer();
        }

        // --- bottom-right: compile status, with a leading semantic dot ---
        let (right_label, dot_color) = self.status_right_label(theme);
        if right_label != self.last_right_label {
            self.status_bubble_right.poke(now);
            self.last_right_label.clone_from(&right_label);
        }
        // Reserve a leading control slot for the status dot.
        let dot_slot = 16.0;
        let (rw, rh) = ui::status_bubble::pill_size(&right_label, tr, text_size, dot_slot);
        let right_bottom = if la > 0.0 && self.rect.x1 - margin - rw < lrect.x1 + 8.0 {
            lrect.y0 - 8.0
        } else {
            self.rect.y1 - margin
        };
        let rrect = Rect::new(
            self.rect.x1 - margin - rw,
            right_bottom - rh,
            self.rect.x1 - margin,
            right_bottom,
        );
        let right_over =
            self.shots_status || right_zone.contains(self.mouse) || rrect.contains(self.mouse);
        self.status_bubble_right.set_hover(right_over, now);
        let ra = self.status_bubble_right.alpha(now);
        if ra > 0.0 {
            scene.push_layer(
                Fill::NonZero,
                vello::peniko::BlendMode::default(),
                ra,
                t,
                &rrect.inflate(12.0, 14.0),
            );
            ui::status_bubble::draw_pill(scene, t, tr, theme, rrect, &right_label, text_size);
            // Semantic status dot, centred in the reserved leading slot.
            let cy = f64::midpoint(rrect.y0, rrect.y1);
            let dcx = rrect.x0 + 8.0 + dot_slot / 2.0;
            scene.fill(
                Fill::NonZero,
                t,
                dot_color,
                None,
                &vello::kurbo::Circle::new(Point::new(dcx, cy), 3.5),
            );
            scene.pop_layer();
        }
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
    ui::provides!(dyn ui::widget::controls::ActionControls);

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
        )? {
            return Ok(Some(started));
        }
        Ok(self
            .answer_resource_operation(request.kind, request.type_id, request.payload)?
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
                ResourceIo::Open(path) => self.open_typ_from(&path),
                ResourceIo::Save(path) => self.save_typ_to(&path),
                ResourceIo::ExportPdf(path) => self.export_pdf_to_path(&path),
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
        // `apply_layout` does not receive the theme, so retain its text scale
        // here.  The toolbar and compact threshold use it to prevent enlarged
        // labels from colliding with content or each other.
        self.layout_text_scale = f64::from(theme.text_scale);
        // `new_leaf` on a freshly created tree never fails.
        #[allow(clippy::unwrap_used, reason = "new_leaf on a fresh tree cannot fail")]
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            })
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

    fn draw(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        // Consume any finished async compile up front so its pages paint
        // this very frame (draw runs on animation frames even when sync
        // doesn't).
        self.pump_worker();

        // Toolbar metrics from this frame's measured pill line: the band and
        // the pills grow by exactly the line's growth over its 1× box, and the
        // width breakpoint is judged at the renderer's text scale. When the
        // measurement changes (first frame at 200 %, a live scale change) the
        // pane split is recomputed now, so this very frame paints coherently.
        let (_, pill_line) = tr.measure_weighted("0", PILL_FONT, None, TextWeight::Medium);
        let pill_line = f64::from(pill_line);
        let text_k = f64::from(tr.text_scale()).max(f64::from(f32::EPSILON));
        let growth = (pill_line - pill_line / text_k).max(0.0);
        let toolbar_h = TOOLBAR_H + growth;
        if (toolbar_h - self.toolbar_h).abs() > 0.01 || (text_k - self.text_k).abs() > 1e-6 {
            self.toolbar_h = toolbar_h;
            self.pill_h = PILL_H + growth;
            self.pill_line = pill_line;
            self.text_k = text_k;
            self.layout_in(self.rect);
        }

        let c = &theme.tokens.colors;
        scene.fill(Fill::NonZero, t, c.surfaces.base, None, &self.rect);

        self.draw_toolbar(scene, t, tr, theme);

        if self.compact {
            match self.compact_view {
                CompactPane::Editor => {
                    self.editor
                        .draw(scene, t, tr, theme, self.diagnostics.diags());
                }
                CompactPane::Preview => self.preview.draw(scene, t, tr, theme),
            }
        } else {
            self.editor
                .draw(scene, t, tr, theme, self.diagnostics.diags());
            self.preview.draw(scene, t, tr, theme);
            self.draw_divider(scene, t, theme);
        }

        if self.show_diagnostics && self.diag_rect.height() > 1.0 {
            self.diagnostics.draw(scene, t, tr, theme);
        }
        self.draw_status_bubbles(scene, t, tr, theme);

        // The overflow menu floats above everything else.
        if self.menu_open {
            self.draw_menu(scene, t, tr, theme);
        }

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

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
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

    /// Source and preview are custom-painted panes rather than retained child
    /// widgets. Export both with their current compile state so readers can
    /// understand the split/compact view without inferring it from pixels.
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
                "Compact layout; use the toolbar to switch between source and preview"
            } else {
                "Live rendered preview"
            })
            .with_semantic_key("typst.preview")
            .with_hashed_key("typst.preview");
        let mut children = vec![source, preview];
        children.extend(TOOLBAR_ACTIONS.iter().map(|(_, target, label)| {
            AccessNode::new(Role::Button)
                .with_name(*label)
                .with_state(AccessState::new().focusable(true))
                .with_action_target(*target)
        }));
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
        let _ = (doc, path);
        if let Some((operation, _, _)) = TOOLBAR_ACTIONS.iter().find(|(_, id, _)| *id == target) {
            return match action {
                ui::a11y::Action::Focus => true,
                ui::a11y::Action::Activate => {
                    self.do_action(*operation);
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
        // a compile is in flight (so `draw` can consume its result), a
        // transient toast is still fading, or either floating status bubble is
        // lingering / fading (so it hides itself without a stray pointer event).
        let now = ui::time::now();
        self.editor.wants_animation()
            || self.compiling()
            || self.flash_text().is_some()
            || self.status_bubble_left.needs_frame(now)
            || self.status_bubble_right.needs_frame(now)
    }

    fn needs_redraw(&self) -> bool {
        // Pointer *moves* are only repainted when `needs_redraw` is true (the
        // host gates them to avoid hover spam). A live drag or a hover change
        // sets `dirty`, so divider drags, preview text-selection, and inline
        // diagnostic tooltips all repaint smoothly.
        self.dirty
    }
}

impl ui::widget::controls::ActionControls for TypstApp {
    fn control_action_bindings(&self) -> Vec<ui::widget::ControlBindingMetadata> {
        ui::widget::semantic_action_bindings(self.accessibility())
    }
}

impl TypstApp {
    fn answer_resource_operation(
        &mut self,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
    ) -> Result<bool, String> {
        if kind == tilo_actions::OperationKind::Document {
            return Ok(false);
        }
        let Some(operation) = crate::actions::decode(type_id, payload)? else {
            return Ok(false);
        };
        self.perform_resource_operation(operation);
        Ok(true)
    }

    fn admit_resource_operation(
        &mut self,
        invocation: tilo_actions::InvocationId,
        kind: tilo_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        use crate::actions::TypstOperation;
        let Some(operation) = crate::actions::decode(type_id, payload)? else {
            return Ok(None);
        };
        if self.resource_invocation.is_some() || self.pending_resource_io.is_some() {
            return Err("another file operation is already running".into());
        }
        let done = || {
            Ok(Some(ui::widget::ResourceOperationStart::Completed(
                ui::tilo_actions::ActionOutput::default(),
            )))
        };
        let resource = match operation {
            TypstOperation::Open { path: Some(path) } => Some(ResourceIo::Open(path.into())),
            TypstOperation::Save { path: Some(path) } => Some(ResourceIo::Save(path.into())),
            TypstOperation::ExportPdf { path: Some(path) } => {
                Some(ResourceIo::ExportPdf(path.into()))
            }
            TypstOperation::Open { path: None } => {
                self.open();
                None
            }
            TypstOperation::Save { path: None } => {
                if self.save()? {
                    return done();
                }
                None
            }
            TypstOperation::ExportPdf { path: None } => {
                self.export_pdf();
                None
            }
            other => {
                return self
                    .answer_resource_operation(
                        kind,
                        type_id,
                        &serde_json::to_vec(&other).map_err(|e| e.to_string())?,
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
                    "]" => {
                        self.step_file(1);
                        *handled = true;
                        return;
                    }
                    "[" => {
                        self.step_file(-1);
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
                                self.set_flash("Copied selection".into());
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
                self.dragging_divider = false;
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
        let prev_hover = self.divider_hover;
        self.divider_hover = !self.compact && self.divider_rect.inflate(2.0, 0.0).contains(p);
        if self.divider_hover != prev_hover {
            self.dirty = true;
        }
        // Re-surface / hide the corner status bubbles as the pointer enters or
        // leaves their reveal bands — repaint only on a change so idle hover
        // doesn't spin the loop.
        let (lz, rz) = self.status_zones();
        let zones = (lz.contains(p), rz.contains(p));
        if zones != self.status_hover {
            self.status_hover = zones;
            self.dirty = true;
        }
        if self.dragging_divider {
            let content_w = self.rect.width().max(1.0);
            self.split_ratio = ((p.x - self.rect.x0) / content_w).clamp(0.18, 0.82);
            self.dirty = true;
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
        // The compact overflow menu grabs clicks before anything else.
        if self.menu_open {
            if let Some(a) = self.hit_menu(p) {
                self.do_action(a);
                *handled = true;
                return;
            }
            if !self.toolbar_rect.contains(p) {
                self.menu_open = false;
                *handled = true;
                return;
            }
        }
        if self.toolbar_rect.contains(p) {
            if let Some(a) = self.hit_toolbar(p) {
                self.do_action(a);
            }
            *handled = true;
            return;
        }
        if !self.compact && self.divider_rect.inflate(2.0, 0.0).contains(p) {
            self.dragging_divider = true;
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
/// The user's Typst library on this device: `TILO_TYPST_LIBRARY_DIR`, else
/// `typst_editor` in their documents (or data) directory.
pub(crate) fn library_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("TILO_TYPST_LIBRARY_DIR") {
        return Some(PathBuf::from(dir));
    }
    tilo_paths::document_dir()
        .or_else(tilo_paths::data_dir)
        .map(|d| d.join("typst_editor"))
}

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
    use winit::event::{DeviceId, ElementState, Modifiers, MouseButton};

    use automerge::{ROOT, ReadDoc as _};
    use ui::tilo_actions::Action as _;
    use ui::tilo_resource::Resource as _;

    /// A workspace of the bundled examples, presented to an editor showing
    /// the first, synced once so it compiled.
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
        app.sync(&mut doc, &ROOT);
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
    fn file_effects_save_and_export_real_files() {
        // Case `direct_save_is_accepted_then_succeeds`.
        {
            let (mut app, mut doc) = fresh();
            let dir = unique_dir("direct-lifecycle");
            std::fs::create_dir_all(&dir).unwrap();
            let destination = dir.join("saved.typ");
            let invocation = tilo_actions::InvocationId::new();
            let payload = serde_json::to_vec(&crate::actions::TypstOperation::Save {
                path: Some(destination.display().to_string()),
            })
            .unwrap();
            let started = Widget::start_operation(
                &mut app,
                &ui::widget::ResourceOperation::local(
                    invocation,
                    tilo_actions::OperationKind::Effect,
                    crate::actions::OPERATION_TYPE,
                    &payload,
                ),
                &mut doc,
                &ROOT,
            )
            .unwrap()
            .unwrap();
            assert_eq!(started, ui::widget::ResourceOperationStart::Accepted);
            app.sync(&mut doc, &ROOT);
            assert!(matches!(
                app.poll_resource_operation(invocation, false, &doc, &ROOT),
                Some(tilo_actions::InvocationUpdate::Succeeded { .. })
            ));
            assert_eq!(
                std::fs::read_to_string(&destination).unwrap(),
                shown_source(&app, &doc)
            );
            std::fs::remove_dir_all(dir).ok();
        }
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
            let (mut app, mut doc) = fresh();
            let before = shown_source(&app, &doc);
            let (services, recorder) = ui::host::HostServices::recording();
            recorder.respond_to_pick(PortalOutcome::Cancelled);
            ui::host::scoped(&services, || app.open());
            app.sync(&mut doc, &ROOT);
            assert!(app.io.is_none());
            assert_eq!(shown_source(&app, &doc), before, "cancel loads nothing");
        }
    }

    fn device_id() -> DeviceId {
        // winit exposes no public DeviceId constructor; tests only need a value.
        unsafe { std::mem::zeroed() }
    }

    fn cursor(p: Point) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: device_id(),
            position: PhysicalPosition::new(p.x, p.y),
        }
    }

    fn press() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: device_id(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        }
    }

    /// Pump one event through the production `Widget::handle_event`, returning
    /// whether it was consumed.
    fn send(app: &mut TypstApp, doc: &mut AutoCommit, ev: &WindowEvent) -> bool {
        let taffy = TaffyTree::<()>::new();
        let mut handled = false;
        let mut focus = None;
        app.handle_event(
            ev,
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &Modifiers::default(),
                state: doc,
                path: &ROOT,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: NodeId::new(0),
                taffy: &taffy,
            },
        );
        handled
    }

    /// Run one real draw pass so `toolbar_hits` / `menu_hits` are populated,
    /// exactly as they are for the live pointer path.
    fn render_once(app: &mut TypstApp) {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let mut scene = Scene::new();
        let presence = PresenceCtx::empty();
        app.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
    }

    fn hit(app: &TypstApp, action: TbAction) -> Rect {
        app.toolbar_hits
            .iter()
            .find(|(a, _)| *a == action)
            .map_or_else(|| panic!("toolbar action not drawn"), |(_, r)| *r)
    }

    fn center(r: Rect) -> Point {
        Point::new(f64::midpoint(r.x0, r.x1), f64::midpoint(r.y0, r.y1))
    }

    #[test]
    fn source_sync_and_switching_files() {
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
        // Case `switching_files_shows_the_next_without_writing`:
        // Each file keeps its own text; showing another one changes neither.
        {
            let (mut app, mut doc) = fresh();
            doc.commit();
            let heads = doc.get_heads();
            let first = shown_source(&app, &doc);
            app.step_file(1);
            app.sync(&mut doc, &ROOT);
            let second = shown_source(&app, &doc);
            assert_ne!(first, second, "the next file is shown");
            assert_eq!(app.editor.text(), second);
            assert_eq!(app.current_index(), Some(1));
            assert_eq!(doc.get_heads(), heads, "switching files writes nothing");
            app.step_file(-1);
            app.step_file(-1);
            assert_eq!(
                app.current_index(),
                Some(app.files.files.len() - 1),
                "stepping back wraps around"
            );
        }
        // Case `a_closed_file_gives_way_to_the_first`.
        {
            let (mut app, mut doc) = fresh();
            app.step_file(1);
            let shown = app.current_file().unwrap().id.clone();
            crate::resource::TypstAction::CloseFile { file: shown }
                .apply(&mut doc)
                .unwrap();
            present(&mut app, &doc);
            assert_eq!(app.current_index(), Some(0));
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
            assert!(send(&mut app, &mut doc, &char_press("x")));
            assert!(app.perform_semantic_action(
                "typst.source",
                &ui::a11y::Action::SetValue("= Accessible edit".into()),
                &mut doc,
                &ROOT,
            ));
            app.sync(&mut doc, &ROOT);
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
            assert!(app.flash_text().is_none(), "no transient toast pending");
            assert!(
                !app.needs_animation_frame(),
                "a focused-but-idle editor must not drive continuous redraws"
            );
        }
        // Case `pending_native_dialog_has_a_stable_non_animation_deadline`.
        {
            let (mut app, mut doc) = fresh();
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
            app.sync(&mut doc, &ROOT);
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
        // Case `status_bubbles_reclaim_strip_and_fade`:
        // The status now floats as two transient bubbles instead of a permanent
        // bottom strip: the content panes reclaim its height (spanning to the
        // window bottom), and each bubble shows on its first label change then
        // fades once its linger elapses. Drives the real `layout_in` +
        // `draw_status_bubbles`.
        {
            let (mut app, _doc) = fresh();
            app.layout_in(Rect::new(0.0, 0.0, 1280.0, 720.0));
            // Height reclaimed: both content panes reach the window bottom — no
            // status strip is carved off the layout.
            assert_eq!(
                app.editor_rect.y1, 720.0,
                "editor pane spans to the window bottom"
            );
            assert_eq!(
                app.preview_rect.y1, 720.0,
                "preview pane spans to the window bottom"
            );

            let theme = Theme::dark();
            let mut tr = TextRenderer::new();
            let mut scene = Scene::new();
            let now = ui::time::now();

            // First draw composes both labels (differing from the empty caches) and
            // pokes each bubble → both are visible.
            app.draw_status_bubbles(&mut scene, Affine::IDENTITY, &mut tr, &theme);
            assert!(
                app.status_bubble_left.is_visible(now),
                "left bubble shows on its first change"
            );
            assert!(
                app.status_bubble_right.is_visible(now),
                "right bubble shows on its first change"
            );

            // With an already-expired linger, no further label change, and the
            // pointer away from the corners, a redraw lets both fade to hidden —
            // proving they are transient overlays, not a permanent strip.
            app.status_bubble_left =
                ui::StatusBubble::new().with_linger(std::time::Duration::from_millis(0));
            app.status_bubble_right =
                ui::StatusBubble::new().with_linger(std::time::Duration::from_millis(0));
            let past = now
                .checked_sub(std::time::Duration::from_millis(10))
                .unwrap();
            app.status_bubble_left.poke(past);
            app.status_bubble_right.poke(past);
            app.mouse = Point::new(5.0, 5.0);
            app.draw_status_bubbles(&mut scene, Affine::IDENTITY, &mut tr, &theme);
            let after = ui::time::now();
            assert!(
                !app.status_bubble_left.is_visible(after),
                "left bubble hides after its linger"
            );
            assert!(
                !app.status_bubble_right.is_visible(after),
                "right bubble hides after its linger"
            );
        }
    }

    #[test]
    fn toolbar_reflows_between_split_and_compact_without_overlap() {
        // Case `narrow_toolbar_controls_do_not_overlap`.
        {
            let (mut app, _) = fresh();
            for width in [320.0, 360.0, 560.0] {
                for errors in [0, 1] {
                    app.err_count = errors;
                    app.layout_in(Rect::new(0.0, 0.0, width, 760.0));
                    render_once(&mut app);
                    for (index, (_, left)) in app.toolbar_hits.iter().enumerate() {
                        for (_, right) in app.toolbar_hits.iter().skip(index + 1) {
                            assert!(
                                left.intersect(*right).area() <= 0.0,
                                "toolbar controls overlap at {width}px: {left:?} and {right:?}"
                            );
                        }
                    }
                }
            }
        }
        // Case `resize_reflows_between_split_and_compact`:
        // The responsive contract: layout is re-derived from the *current* width
        // every pass, so resizing across the threshold reflows both ways — it is
        // never frozen to the first frame's mode.
        {
            let (mut app, _doc) = fresh();

            // Wide → split: both panes have area and the divider exists.
            app.layout_in(Rect::new(0.0, 0.0, 1280.0, 720.0));
            assert!(!app.compact);
            assert!(app.editor_rect.area() > 0.0);
            assert!(app.preview_rect.area() > 0.0);
            assert!(app.divider_rect.area() > 0.0);

            // Narrow → compact: a single pane, no divider.
            app.layout_in(Rect::new(0.0, 0.0, 560.0, 720.0));
            assert!(app.compact);
            assert_eq!(app.divider_rect.area(), 0.0);
            assert!(app.editor_rect.area() > 0.0, "editor pane shown");
            assert_eq!(app.preview_rect.area(), 0.0, "preview hidden in compact");

            // Resize back to wide → must reflow to split again (not frozen).
            app.layout_in(Rect::new(0.0, 0.0, 1280.0, 720.0));
            assert!(!app.compact, "reflowed back to split after widening");
            assert!(app.preview_rect.area() > 0.0);
        }
        // Case `compact_toolbar_controls_never_overlap_at_phone_width`:
        // Rule 1 of the 1× bar on the compact toolbar: with several documents
        // *and* a problems badge, no two toolbar hit rects overlap at the phone
        // viewport or wider — the document chip elides and the switch squeezes
        // before anything is painted over a neighbouring control.
        {
            for w in [390.0, 480.0, 560.0] {
                let (mut app, _doc) = fresh();
                assert!(app.files.files.len() > 1, "demo carries several documents");
                app.warn_count = 1;
                app.layout_in(Rect::new(0.0, 0.0, w, 720.0));
                assert!(app.compact);
                render_once(&mut app);
                let hits = app.toolbar_hits.clone();
                assert!(
                    hits.iter().any(|(a, _)| *a == TbAction::ShowPreview),
                    "w={w}: the view switch is painted"
                );
                assert!(
                    hits.iter().any(|(a, _)| *a == TbAction::ToggleProblems),
                    "w={w}: the problems badge is painted"
                );
                for (i, (a, ra)) in hits.iter().enumerate() {
                    for (b, rb) in hits.iter().skip(i + 1) {
                        let ix = ra.intersect(*rb);
                        assert!(
                            ix.width() <= 0.0 || ix.height() <= 0.0,
                            "w={w}: {a:?} {ra:?} overlaps {b:?} {rb:?}"
                        );
                    }
                }
            }
        }
        // Case `text_scale_grows_toolbar_and_compacts_before_controls_collide`:
        // Enlarged system text changes the geometry as well as glyph metrics:
        // the taller controls stay in their own toolbar and a formerly cramped
        // split switches to the compact single-pane layout.
        {
            let (mut app, _doc) = fresh();
            app.layout_in(Rect::new(0.0, 0.0, 1280.0, 720.0));
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

            assert!(app.toolbar_rect.height() > TOOLBAR_H);
            assert!(app.toolbar_rect.height() >= app.pill_h);
            assert!(app.compact, "1280px is compact at 200% text scale");
            assert_eq!(app.editor_rect.y0, app.toolbar_rect.y1);
        }
    }

    #[test]
    fn compact_view_switch_and_overflow_menu_act_and_stay_reachable() {
        // Case `compact_view_switch_via_pointer`:
        // Tapping the compact Edit/Preview segmented control through the real
        // pointer path swaps the visible pane.
        {
            let (mut app, mut doc) = fresh();
            app.layout_in(Rect::new(0.0, 0.0, 560.0, 720.0));
            assert!(app.compact);
            assert!(matches!(app.compact_view, CompactPane::Editor));

            render_once(&mut app);
            let target = center(hit(&app, TbAction::ShowPreview));
            send(&mut app, &mut doc, &cursor(target));
            assert!(send(&mut app, &mut doc, &press()), "click consumed");
            assert!(matches!(app.compact_view, CompactPane::Preview));

            // Relayout confirms the preview pane now occupies the content area.
            app.layout_in(Rect::new(0.0, 0.0, 560.0, 720.0));
            assert!(app.preview_rect.area() > 0.0);
            assert_eq!(app.editor_rect.area(), 0.0);
        }
        // Case `compact_overflow_menu_acts`:
        // The compact overflow menu opens from the kebab and its items act through
        // the real pointer path, then dismiss the menu.
        {
            let (mut app, mut doc) = fresh();
            app.layout_in(Rect::new(0.0, 0.0, 560.0, 720.0));
            render_once(&mut app);

            // Open the menu.
            let kebab = center(hit(&app, TbAction::ToggleMenu));
            send(&mut app, &mut doc, &cursor(kebab));
            send(&mut app, &mut doc, &press());
            assert!(app.menu_open, "kebab opened the menu");

            // Draw again so the menu rows are hit-testable, then click "Zoom in".
            render_once(&mut app);
            let zoom_in = app
                .menu_hits
                .iter()
                .find(|(a, _)| *a == TbAction::ZoomIn)
                .map(|(_, r)| center(*r))
                .expect("zoom-in row drawn");
            let before = app.preview.zoom();
            send(&mut app, &mut doc, &cursor(zoom_in));
            send(&mut app, &mut doc, &press());
            assert!(!app.menu_open, "menu closed after acting");
            assert!(app.preview.zoom() > before, "zoom applied");
            assert!(matches!(app.compact_view, CompactPane::Preview));
        }
        // Case `scaled_compact_menu_keeps_every_row_reachable`:
        // The overflow panel is its own interaction surface; even at 200% text
        // scale in a short window all rows remain inside the app viewport.
        {
            let (mut app, _doc) = fresh();
            app.layout_text_scale = 2.0;
            app.layout_in(Rect::new(0.0, 0.0, 700.0, 520.0));
            app.menu_open = true;
            let mut tr = TextRenderer::new();
            tr.set_text_scale(2.0);
            let mut scene = Scene::new();
            let presence = PresenceCtx::empty();
            app.draw(
                &mut scene,
                Affine::IDENTITY,
                &mut tr,
                &Theme::dark(),
                &presence,
            );

            assert_eq!(app.menu_hits.len(), 8);
            assert!(app.menu_hits.iter().all(|(_, row)| {
                row.x0 >= app.rect.x0
                    && row.x1 <= app.rect.x1
                    && row.y0 >= app.rect.y0
                    && row.y1 <= app.rect.y1
            }));
        }
    }

    #[test]
    fn a_new_workspace_starts_with_the_library_or_an_empty_file() {
        let names = |actions: Vec<crate::resource::TypstAction>| {
            actions
                .into_iter()
                .map(|action| match action {
                    crate::resource::TypstAction::AddFile {
                        name, path, source, ..
                    } => (name, path.is_some(), source),
                    other => panic!("a new workspace only adds files: {other:?}"),
                })
                .collect::<Vec<_>>()
        };
        // Case `the_library_is_read_from_disk`.
        {
            let dir = unique_dir("library");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("beta.typ"), "= Beta").unwrap();
            std::fs::write(dir.join("alpha.typ"), "= Alpha").unwrap();
            std::fs::write(dir.join("notes.txt"), "ignored").unwrap();
            assert_eq!(
                names(crate::resource::starting_files(Some(&dir))),
                [
                    ("alpha.typ".to_owned(), true, "= Alpha".to_owned()),
                    ("beta.typ".to_owned(), true, "= Beta".to_owned()),
                ],
                "sorted, .txt ignored, each saved where it was found"
            );
            std::fs::remove_dir_all(&dir).ok();
        }
        // Case `an_empty_library_starts_one_empty_file`.
        {
            let dir = unique_dir("empty-library");
            std::fs::create_dir_all(&dir).unwrap();
            assert_eq!(
                names(crate::resource::starting_files(Some(&dir))),
                [("untitled.typ".to_owned(), false, String::new())],
                "a new workspace never contains sample text"
            );
            std::fs::remove_dir_all(&dir).ok();
        }
    }

    // ---- helpers for the preview-interaction + diagnostics tests ----------

    use crate::compile::DiagKind;
    use winit::event::TouchPhase;
    use winit::keyboard::{KeyLocation, ModifiersState, NativeKeyCode, PhysicalKey};

    /// A character keypress through the real event path. winit's per-platform
    /// `KeyEventExtra` is `pub(crate)`, so the struct is zeroed and only its
    /// public fields set — the same trick the toolkit's own widget tests use.
    fn char_press(c: &str) -> WindowEvent {
        let mut ev: KeyEvent = unsafe { std::mem::zeroed() };
        ev.logical_key = Key::Character(c.into());
        ev.physical_key = PhysicalKey::Unidentified(NativeKeyCode::Unidentified);
        ev.location = KeyLocation::Standard;
        ev.state = ElementState::Pressed;
        ev.repeat = false;
        ev.text = Some(c.into());
        WindowEvent::KeyboardInput {
            device_id: device_id(),
            event: ev,
            is_synthetic: false,
        }
    }

    /// A vertical wheel notch (`+` = up/away). A tilt component can be supplied
    /// via `x` for the horizontal-pan path.
    fn wheel(x: f32, y: f32) -> WindowEvent {
        WindowEvent::MouseWheel {
            device_id: device_id(),
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
        let taffy = TaffyTree::<()>::new();
        let mut handled = false;
        let mut focus = None;
        app.handle_event(
            ev,
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &mods,
                state: doc,
                path: &ROOT,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: NodeId::new(0),
                taffy: &taffy,
            },
        );
        handled
    }

    fn unique_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "typst_editor_{tag}_{}_{}",
            std::process::id(),
            ui::time::wall()
                .duration_since(ui::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
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
                send_mods(&mut app, &mut doc, &char_press("="), ctrl),
                "Ctrl+= consumed"
            );
            let zoomed_in = app.preview.zoom();
            assert!(
                zoomed_in > base,
                "Ctrl+= zoomed the preview in ({base} -> {zoomed_in})"
            );

            assert!(
                send_mods(&mut app, &mut doc, &char_press("-"), ctrl),
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
            app.sync(&mut doc, &ROOT);
            app.layout_in(Rect::new(0.0, 0.0, 1280.0, 720.0));

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

            // HEADER_H (30) + ROW_H (26) mirror the diagnostics panel geometry.
            let list_top = app.diag_rect.y0 + 30.0;
            for (i, d) in diags.iter().enumerate() {
                let Some(line) = d.line else { continue };
                let y = list_top + (i as f64 + 0.5) * 26.0;
                let p = Point::new(app.diag_rect.x0 + 40.0, y);
                send(&mut app, &mut doc, &cursor(p));
                assert!(
                    send(&mut app, &mut doc, &press()),
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
        // Case `native_toolbar_controls_are_exposed_without_focus_activation`.
        {
            let (mut app, mut doc) = fresh();
            let tree = app.accessibility().expect("Typst semantics");
            for target in [
                "typst.open",
                "typst.save",
                "typst.export-pdf",
                "typst.compile",
                "typst.show-preview",
            ] {
                assert!(
                    tree.children
                        .iter()
                        .any(|node| node.action_target.as_deref() == Some(target)),
                    "missing control {target}"
                );
            }
            app.compact = true;
            assert!(matches!(app.compact_view, CompactPane::Editor));
            assert!(app.perform_semantic_action(
                "typst.show-preview",
                &ui::a11y::Action::Focus,
                &mut doc,
                &ROOT
            ));
            assert!(
                matches!(app.compact_view, CompactPane::Editor),
                "focusing a control must not activate it"
            );
            assert!(app.perform_semantic_action(
                "typst.show-preview",
                &ui::a11y::Action::Activate,
                &mut doc,
                &ROOT
            ));
            assert!(matches!(app.compact_view, CompactPane::Preview));
        }
    }
}
