//! The root widget of the Typst editor.
//!
//! `TypstApp` is a single full-bleed [`Widget`]. It hosts the library controls
//! of [`Chrome`] above two content panes it paints itself (the source editor
//! and the rendered preview, which are genuine content drawing), and lays them
//! out in plain rects computed in `TypstApp::layout_in`:
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
//! │ Ln 4, Col 28                          2 pages, fit width │
//! └──────────────────────────────────────────────────────────┘
//! ```
//!
//! The editor stands on the window ground and the preview on the inspector
//! region; neither fills a ground of its own. What a person reads while
//! working (caret position, pages, zoom) is the status bar, which reserves its
//! band; a change worth noticing once ("Compiled in 42 ms") is said in the
//! window's bar for a while.
//!
//! The workspace's files — names, paths and sources — are the resource's
//! state ([`crate::resource::Workspace`]); every edit is a field edit or one
//! of its verbs, so undo/redo and live collaboration come for free. Which file is open,
//! the caret, scroll, zoom, the split ratio and cached page bitmaps are view
//! state.

use std::cell::Cell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use ui::tilo_resource::Id;
use ui::time::Instant;
use ui::widget::TaffyTreeExt;
use ui::widget::{AppModel, ModelPath};

#[cfg(test)]
use automerge::AutoCommit;
use taffy::{NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::portal::{FileFilter, PickRequest};
use ui::presence::PresenceCtx;
use ui::surface::Region;
use ui::text_render::TextRenderer;
use ui::theme::Theme;

/// An Open, Save or Export in flight: its chooser, then for a save or an
/// export the write to where the person chose.
enum PendingIo {
    OpenTyp(ui::portal::ContentsChannel),
    SaveTyp(ui::portal::DestinationChannel),
    ExportPdf(ui::portal::DestinationChannel),
    Writing {
        export: bool,
        name: String,
        written: ui::portal::SaveChannel,
    },
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
use crate::resource::TypstFilePaths;
use crate::worker::CompileWorker;

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
    /// The height of one control, which the Problems panel keeps as the
    /// least worth opening for.
    control_h: f64,
    status_h: f64,
    compact_w: f64,
}

impl Frame {
    /// `control_h` is the height of one control and `status_line_h` of one
    /// line of status text, both measured at the current text scale.
    fn of(theme: &Theme, control_h: f64, status_line_h: f64) -> Self {
        let metrics = &theme.tokens.metrics;
        Self {
            control_h: control_h + 2.0 * f64::from(theme.tokens.spacing.s1),
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
    /// The open document's headings, in a drawer beside the source.
    outline: crate::outline::Outline,

    // --- workspace ---
    /// The workspace as last presented.
    files: Rc<crate::resource::Workspace>,
    current: Option<Id>,
    /// Where each file was saved when that place has no path here (a
    /// browser page's Save dialog file): every later Save writes there
    /// without asking, and only Save As asks again.
    kept: std::collections::HashMap<Id, ui::portal::SaveDestination>,
    /// This window added a file to an empty workspace: it opens once it
    /// lands.
    pending_open: bool,
    /// The files the workspace held when last presented, to tell a file
    /// that just arrived from one that was there.
    known_files: Vec<Id>,
    actions: Option<ui::framework::Dispatcher<crate::resource::Workspace>>,
    /// An in-flight host file dialog or the write that follows it, drained
    /// once per frame in [`Self::drain_io`]. `None` when no Open / Save /
    /// Export is under way.
    io: Option<PendingIo>,
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
    /// The source last written to disk (or read from it) and the file it
    /// belongs to. The source differing from it is unsaved work, which the
    /// window's bar says in words beside the document's name.
    saved: Option<(Id, String)>,

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

    // --- chrome ---
    chrome: Chrome,
    frame: Frame,
    /// A change worth noticing once, said in the window's bar for a while: a
    /// compile result, an export.
    status: ui::widget::toast::Transient<String>,
    /// The window's report of this window's file saves: the framework says
    /// nothing while they land and offers "Try again" when one fails.
    saves: ui::framework::Saves,
    /// Where the last save went, so a failed save can be tried again.
    save_path: Option<std::path::PathBuf>,
    /// The next compile result is feedback for an explicit Compile.
    compile_feedback: bool,

    // --- layout rects (filled by layout_in) ---
    rect: Rect,
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
    /// What the frame being drawn shows around the panes, and what the frame
    /// on screen showed.
    window_shown: ui::scroll_copy::ShownFrames<damage::WindowShown>,
}

impl TypstApp {
    /// The editor over the workspace it is presented; it shows the first
    /// file until another is chosen.
    pub fn new() -> Self {
        let editor = SourceEditor::new();
        let theme = Theme::dark();
        Self {
            parts: WorldParts::new(),
            worker: None,
            applied_seq: 0,
            editor,
            preview: PreviewPane::new(),
            diagnostics: DiagnosticsPanel::new(),
            outline: crate::outline::Outline::new(),
            files: Rc::default(),
            current: None,
            kept: std::collections::HashMap::new(),
            pending_open: false,
            known_files: Vec::new(),
            io: None,
            background_poll_deadline: Cell::new(None),
            file_reply: None,
            pending_resource_io: None,
            initialized: false,
            last_source: String::new(),
            source_cache: String::new(),
            saved: None,
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
            chrome: Chrome::new(),
            frame: Frame::of(
                &theme,
                f64::from(theme.tokens.metrics.min_pointer_target),
                0.0,
            ),
            status: ui::widget::toast::Transient::new(),
            saves: ui::framework::Saves::default(),
            save_path: None,
            compile_feedback: false,
            rect: Rect::ZERO,
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
            window_shown: ui::scroll_copy::ShownFrames::default(),
            actions: None,
        }
    }

    /// Attach the workspace's dispatcher; a capture's first file.
    pub(crate) fn attach_surface(
        &mut self,
        cx: &ui::framework::SurfaceCx<crate::resource::Workspace>,
    ) {
        self.chrome.attach_actions(&cx.actions());
        self.actions = Some(cx.dispatcher());
        self.saves = cx.saves();
        self.editor.attach(cx.dispatcher());
        if let Some(file) = cx.fixture::<crate::resource::CaptureFile>() {
            self.current = Some(file.0.clone());
        }
        self.follow_current();
    }

    /// Show the workspace as it is now: the file shown stays open unless it
    /// left the workspace; a file this window added opens once it lands.
    pub(crate) fn present_state(&mut self, state: &Rc<crate::resource::Workspace>) {
        // A file that arrives (written by an action, an agent, the palette)
        // while the window shows a blank, never-saved one opens: there is
        // nothing on screen to keep, and the new file is what was asked for.
        let arrived = state
            .files
            .ids()
            .rfind(|id| !self.known_files.contains(id))
            .cloned();
        let blank = self
            .current
            .as_ref()
            .and_then(|id| self.files.file(id))
            .is_some_and(|file| file.source.trim().is_empty() && file.path.is_none());
        let seen_before = !self.known_files.is_empty();
        self.known_files = state.files.ids().cloned().collect();
        self.files = Rc::clone(state);
        if seen_before
            && blank
            && let Some(arrived) = arrived
        {
            self.open_file(&arrived);
            self.dirty = true;
            return;
        }
        let shown = self.current.as_ref().filter(|id| state.file(id).is_some());
        if shown.is_none() {
            let first = state.files.ids().next().cloned();
            if self.pending_open
                && let Some(first) = first
            {
                self.pending_open = false;
                self.open_file(&first);
                return;
            }
            self.current = first;
            self.follow_current();
        }
        self.dirty = true;
    }

    /// Whether the document shown holds changes no one has written yet: the
    /// source differs from what was last saved or read from disk, or a
    /// document that never had a file has been typed into.
    pub(crate) fn unsaved(&self) -> bool {
        let Some(file) = self.current_file() else {
            return false;
        };
        match (&self.saved, &self.current) {
            (Some((saved_id, source)), Some(id)) if saved_id == id => source != &file.source,
            _ => file.path.is_none() && !file.source.trim().is_empty(),
        }
    }

    /// Say in the window's bar where the window is and what it holds, and
    /// take back what the person did there.
    fn declare_window_slots(&mut self) {
        let name = self.current_name();
        self.chrome.sync(
            &name,
            self.unsaved(),
            self.err_count + self.warn_count,
            self.show_diagnostics,
            self.outline.visible(),
            matches!(self.compact_view, CompactPane::Preview),
        );
        self.chrome.declare();
    }

    /// The file shown, if the workspace has one.
    pub(crate) fn current_file(&self) -> Option<&crate::resource::TypstFile> {
        self.files.file(self.current.as_ref()?)
    }

    /// The paths of the file shown's fields.
    fn current_paths(&self) -> Option<TypstFilePaths<crate::resource::Workspace>> {
        let id = self.current.as_ref()?;
        Some(crate::resource::Workspace::paths().files.item(id))
    }

    /// Show the source of the file shown in the editor: each keystroke is a
    /// splice there.
    fn follow_current(&mut self) {
        if let Some(paths) = self.current_paths() {
            self.editor.follow(paths.source);
        }
    }

    /// Show file `id`, with the caret at its start.
    fn open_file(&mut self, id: &Id) {
        self.current = Some(id.clone());
        // A document that lives in a file is as its file has it; one that
        // lives nowhere yet has nothing written to compare it with.
        self.saved = self
            .files
            .file(id)
            .filter(|file| file.path.is_some())
            .map(|file| (id.clone(), file.source.clone()));
        self.follow_current();
        self.editor.reset_cursor();
        // Keep focus consistent with the visible pane in compact mode.
        self.editor.focused = !(self.compact && matches!(self.compact_view, CompactPane::Preview));
        self.preview.fit_width();
        let name = self.current_file().map(|file| file.name.0.clone());
        if let Some(name) = name {
            self.set_flash(format!("Opened {name}"));
        }
        self.dirty = true;
    }

    /// Submit a field edit or a verb to the workspace.
    fn submit(&self, submit: impl ui::framework::Submit<crate::resource::Workspace>) {
        if let Some(actions) = &self.actions {
            actions.submit(submit);
        }
    }

    /// Replace the source of the file shown with `text`, as one edit.
    fn replace_source(&self, text: &str) {
        if let Some(paths) = self.current_paths() {
            self.submit(paths.source.replace(text));
        }
    }

    /// Stage a capture: the view state each scenario shows once its files
    /// are laid out.
    pub(crate) fn stage_scenario(&mut self, scenario: Option<&str>) {
        match scenario {
            Some("preview") => self.compact_view = CompactPane::Preview,
            Some("menu") => self.compact_view = CompactPane::Editor,
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
                self.status.show(format!(
                    "{}; {}",
                    self.status_left_label(),
                    self.status_right_label()
                ));
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
        .chain(self.outline.resources())
        .map(ui::framework::LiveRow::from_resource)
        .collect()
    }

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
        if self.last_source.trim().is_empty() {
            // An empty source compiles to one blank page, which says nothing
            // about the document: the preview says it is empty instead. A
            // late result for text since deleted is superseded the same way.
            self.preview.set_blank();
        } else if let Some(document) = &out.document {
            self.preview.set_document(document.clone());
        } else {
            // The compile failed. Keep the last successful render on screen (so
            // the reader doesn't lose their place while fixing a typo) and flag
            // it stale; only fall back to a placeholder when nothing has ever
            // rendered.
            if self.preview.page_count() == 0 {
                self.preview.set_failed();
            }
            self.preview.set_stale(true);
        }
        self.outline.set_headings(out.headings);
        self.diagnostics.set_diagnostics(out.diagnostics);
        if self.err_count > 0 && self.compile_feedback {
            // An explicit Compile that fails opens Problems. While typing, a
            // half-written expression fails on every keystroke: the inline
            // marks and the Problems count say so without the panel taking
            // the source's room each time.
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
        // What the results that arrive from now on are for.
        self.last_source = source.to_string();
        if let Some(w) = self.worker.as_mut() {
            w.request(source.to_string());
        } else {
            let out = compile::compile_source(&self.parts, source);
            self.apply_output(out);
        }
    }

    fn force_recompile(&mut self) {
        let src = self.source_cache.clone();
        self.request_compile(&src);
        self.compile_feedback = true;
    }

    /// The name of the file shown, for a picker's suggestion.
    fn current_name(&self) -> String {
        self.current_file()
            .map_or_else(|| "untitled.typ".into(), |file| file.name.0.clone())
    }

    /// Save the file shown: where it was opened from or last saved, else to
    /// a path the host Save dialog asks for (unless a chooser is already in
    /// flight). The chosen path is handled in [`Self::drain_io`].
    fn save(&mut self) -> Result<bool, String> {
        if let Some(path) = self.current_file().and_then(|file| file.path.clone()) {
            return self
                .save_typ_to(std::path::Path::new(&path.0))
                .map(|()| true);
        }
        if self.io.is_some() {
            return Ok(false);
        }
        let kept = self
            .current
            .as_ref()
            .and_then(|id| self.kept.get(id))
            .cloned();
        if let Some(kept) = kept {
            self.write_to(&kept, false)?;
            return Ok(false);
        }
        self.save_as();
        Ok(false)
    }

    /// Ask where to save the file shown, even when it has a place already,
    /// and save it there.
    fn save_as(&mut self) {
        if self.io.is_some() {
            return;
        }
        let name = self.current_name();
        self.io = Some(PendingIo::SaveTyp(
            typ_save_request(&name).send_save_destination(),
        ));
        self.set_flash("Choose where to save…");
    }

    /// Core: write the source of the file shown to `path`, which the file
    /// then lives at. Split out so the portal drain and tests drive it
    /// against a real file.
    fn save_typ_to(&mut self, path: &std::path::Path) -> Result<(), String> {
        let Some(file) = self.current_file().cloned() else {
            return Err("there is no file to save".into());
        };
        self.save_path = Some(path.to_path_buf());
        match save_file(path, file.source.as_bytes()) {
            Ok(()) => {
                // The file is where the document lives: saving it says
                // nothing, and the window's save status ends a failure.
                self.saves.saved();
                if let Some(id) = self.current.clone() {
                    self.saved = Some((id, file.source.clone()));
                }
                let path = path.to_string_lossy().into_owned();
                if file.path.as_ref().map(|at| at.0.as_str()) != Some(path.as_str())
                    && let Some(id) = self.current.clone()
                {
                    self.submit(
                        crate::resource::Workspace::SAVED_AT
                            .with(crate::resource::workspace::SavedAt { file: id, path }),
                    );
                }
                Ok(())
            }
            Err(e) => {
                self.saves.failed(e.to_string());
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
        self.io = Some(PendingIo::ExportPdf(
            pdf_export_request(&pdf_name).send_save_destination(),
        ));
        self.set_flash("Choose where to export the PDF");
    }

    /// Compile the source shown to a PDF and write it to the exact `path`.
    /// Returns an error string on a compile failure or an I/O error.
    fn export_pdf_to_path(&self, path: &std::path::Path) -> Result<(), String> {
        let bytes = self.compiled_pdf()?;
        tilo_files::file_at(path)
            .and_then(|(dir, name)| {
                tilo_files::blocking::create_dir_all(&dir, &tilo_files::VPath::root())?;
                tilo_files::blocking::save_document(&dir, &name, &bytes)
            })
            .map_err(|e| e.to_string())
    }

    /// The source shown, compiled to a PDF; why not on a compile failure.
    fn compiled_pdf(&self) -> Result<Vec<u8>, String> {
        // File effects run before the editor pulls the latest document in
        // sync. Export the live resource, not the previous frame's cache.
        let file = self.current_file().ok_or("there is no file to export")?;
        compile::compile_pdf(&self.parts, &file.source).map_err(|diags| {
            let errs = diags
                .iter()
                .filter(|d| d.kind == compile::DiagKind::Error)
                .count();
            if errs > 0 {
                format!("{} — fix errors first", plural(errs, "error", "errors"))
            } else {
                "could not build PDF".to_string()
            }
        })
    }

    /// Open the host chooser to pick a `.typ` file to load (unless a chooser is
    /// already in flight). The chosen path is handled in [`Self::drain_io`].
    fn open(&mut self) {
        if self.io.is_some() {
            return;
        }
        self.io = Some(PendingIo::OpenTyp(typ_open_request().send_contents()));
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
        self.open_typ_source(crate::resource::file_name(&path), Some(path), src);
        Ok(())
    }

    /// Show a file the chooser delivered: its source, under its name, living
    /// at its path where files have one.
    fn open_typ_picked(&mut self, file: ui::portal::PickedContents) -> Result<(), String> {
        let Ok(src) = String::from_utf8(file.bytes) else {
            let reason = "the file is not UTF-8 text".to_owned();
            self.set_flash(format!("Open failed: {reason}"));
            return Err(reason);
        };
        let path = file
            .location
            .map(|path| path.to_string_lossy().into_owned());
        self.open_typ_source(file.name, path, src);
        Ok(())
    }

    fn open_typ_source(&mut self, name: String, path: Option<String>, src: String) {
        // What was read is what the file holds: nothing is unsaved until it
        // is typed into.
        self.saved = self
            .current
            .clone()
            .filter(|_| path.is_some())
            .map(|id| (id, src.clone()));
        // Recovery belongs to the source being replaced, not the newly opened
        // document. Leave it intact when reading the new file fails.
        self.save_path = None;
        self.saves.report(ui::SaveState::Idle);
        // A window edits one document: the file opened replaces the one shown.
        if let Some(id) = self.current_file().and_then(|_| self.current.clone()) {
            self.submit(
                crate::resource::Workspace::LOAD.with(crate::resource::workspace::Load {
                    file: id,
                    name,
                    path,
                    source: src,
                }),
            );
            self.editor.reset_cursor();
            self.preview.fit_width();
            return;
        }
        self.pending_open = true;
        self.submit(crate::resource::new_file(&name, path.as_deref(), &src));
    }

    /// Drain a finished Open / Save / Export file dialog, if one landed since
    /// the last frame, and run it against the chosen path. The UI thread never
    /// blocks: the portal round-trip lives on its own worker.
    fn drain_io(&mut self) {
        let Some(io) = self.io.take() else { return };
        let result = match io {
            PendingIo::OpenTyp(rx) => match rx.try_recv() {
                Ok(ui::portal::ContentsOutcome::Picked(files)) => match files.into_iter().next() {
                    Some(file) => self.open_typ_picked(file),
                    None => Err("cancelled".into()),
                },
                Ok(ui::portal::ContentsOutcome::Cancelled) => Err("cancelled".into()),
                Ok(ui::portal::ContentsOutcome::Failed(e)) => self.chooser_failed(e),
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    self.io = Some(PendingIo::OpenTyp(rx));
                    return;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    Err("file picker disconnected".into())
                }
            },
            PendingIo::SaveTyp(rx) => match rx.try_recv() {
                Ok(ui::portal::DestinationOutcome::Chosen(destination)) => {
                    if let Some(path) = destination.location().map(std::path::Path::to_path_buf) {
                        // A file on this device: it lives there from now on.
                        if let Some(id) = &self.current {
                            self.kept.remove(id);
                        }
                        self.save_typ_to(&path)
                    } else {
                        let written = self.write_to(&destination, false);
                        // Kept: the next Save writes here without asking.
                        if let Some(id) = self.current.clone() {
                            self.kept.insert(id, destination);
                        }
                        written
                    }
                }
                Ok(ui::portal::DestinationOutcome::Cancelled) => Err("cancelled".into()),
                Ok(ui::portal::DestinationOutcome::Failed(e)) => self.chooser_failed(e),
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    self.io = Some(PendingIo::SaveTyp(rx));
                    return;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    Err("file picker disconnected".into())
                }
            },
            PendingIo::ExportPdf(rx) => match rx.try_recv() {
                Ok(ui::portal::DestinationOutcome::Chosen(destination)) => {
                    self.write_to(&destination, true)
                }
                Ok(ui::portal::DestinationOutcome::Cancelled) => Err("cancelled".into()),
                Ok(ui::portal::DestinationOutcome::Failed(e)) => self.chooser_failed(e),
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    self.io = Some(PendingIo::ExportPdf(rx));
                    return;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    Err("file picker disconnected".into())
                }
            },
            PendingIo::Writing {
                export,
                name,
                written,
            } => match written.try_recv() {
                Ok(ui::portal::SaveOutcome::Saved) => {
                    if export {
                        self.set_flash(format!("Exported {name}"));
                    } else {
                        self.saves.saved();
                    }
                    Ok(())
                }
                Ok(ui::portal::SaveOutcome::Cancelled) => Err("cancelled".into()),
                Ok(ui::portal::SaveOutcome::Failed(e)) => {
                    if export {
                        self.set_flash(format!("Export failed: {e}"));
                    } else {
                        self.saves.failed(e.clone());
                    }
                    Err(e)
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    self.io = Some(PendingIo::Writing {
                        export,
                        name,
                        written,
                    });
                    return;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    Err("the save went away".into())
                }
            },
        };
        // A write still under way answers when it lands.
        if self.io.is_none() {
            self.finish_file_effect(result);
        }
    }

    /// Write the source shown (or, for `export`, its PDF) where the person
    /// chose; the write is drained by [`Self::drain_io`].
    fn write_to(
        &mut self,
        destination: &ui::portal::SaveDestination,
        export: bool,
    ) -> Result<(), String> {
        let bytes = if export {
            self.compiled_pdf().inspect_err(|e| {
                self.set_flash(format!("Export failed: {e}"));
            })?
        } else {
            self.current_file()
                .ok_or("there is no file to save")?
                .source
                .clone()
                .into_bytes()
        };
        let name = destination.name().to_owned();
        let location = destination.location().map(std::path::Path::to_path_buf);
        let written = destination.write(bytes);
        // A file saved on this device lives there from now on.
        if !export
            && let Some(path) = location
            && let Some(id) = self.current.clone()
        {
            self.submit(crate::resource::Workspace::SAVED_AT.with(
                crate::resource::workspace::SavedAt {
                    file: id,
                    path: path.to_string_lossy().into_owned(),
                },
            ));
        }
        self.io = Some(PendingIo::Writing {
            export,
            name,
            written,
        });
        Ok(())
    }

    /// The chooser could not be shown: say so in the person's words.
    fn chooser_failed(&mut self, reason: String) -> Result<(), String> {
        self.set_flash(format!("Couldn\u{2019}t choose a file: {reason}"));
        Err(reason)
    }

    fn run_command(&self, command: Command, _doc: &ui::automerge::AutoCommit) {
        use crate::actions::TypstEffect;
        let effect = match command {
            Command::Compile => TypstEffect::Compile,
            Command::Save => TypstEffect::Save { path: None },
            Command::Open => TypstEffect::OpenFile { path: None },
            Command::ExportPdf => TypstEffect::ExportPdf { path: None },
            Command::ZoomOut => TypstEffect::ZoomOut,
            Command::ZoomIn => TypstEffect::ZoomIn,
            Command::Fit => TypstEffect::FitWidth,
            Command::ToggleProblems => TypstEffect::Problems,
            Command::ToggleOutline => TypstEffect::Outline,
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
            TypstEffect::SaveAs => self.save_as(),
            TypstEffect::OpenFile { .. } => self.open(),
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
            TypstEffect::Outline => {
                self.outline.toggle();
                self.layout_in(self.rect);
            }
            TypstEffect::GoToHeading { line } => {
                let text = self.source_cache.clone();
                self.editor.goto_line(&text, line);
                if self.compact {
                    self.set_compact_view(CompactPane::Editor);
                } else {
                    self.editor.focused = true;
                }
                self.outline.dismiss_if_overlay();
                self.layout_in(self.rect);
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

    /// Announce something worth noticing once: the window's bar says it for
    /// a while.
    fn set_flash(&mut self, msg: impl AsRef<str>) {
        self.status.show(msg.as_ref().to_owned());
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
        // The window bar holds the editor's controls and the status bar holds
        // its readouts, so neither is drawn here: the panes have the window
        // from its top edge to the status bar.
        let status_top = (r.y1 - frame.status_h).max(r.y0);
        self.status_rect = Rect::new(r.x0, status_top, r.x1, r.y1);
        let body = self.outline.layout(Rect::new(r.x0, r.y0, r.x1, status_top));
        let content_w = body.width();

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
            self.splitter.set_bounds(body);
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
    }

    /// Carve the Problems panel out of the bottom of `col` when it is open
    /// and there is room: as tall as its rows need, at most half the column.
    fn split_off_diagnostics(&self, col: Rect) -> (Rect, Rect) {
        let cap = (col.height() * 0.5).floor();
        if !self.show_diagnostics || cap < 2.0 * self.frame.control_h {
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
            crate::chrome::control_height(tr, theme),
            f64::from(status_line),
        );
        let text_k = f64::from(tr.text_scale()).max(f64::from(f32::EPSILON));
        self.preview.apply_theme(theme);
        self.outline.scale_for(theme);
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

        let close = (self.show_diagnostics && self.diag_rect.height() > 1.0)
            .then(|| self.diagnostics.close_rect(theme));
        self.chrome.place_close(close);
    }

    /// What the person reads while working, left: where the caret is.
    fn status_left_label(&self) -> String {
        let (ln, col) = self.caret_line_col();
        format!("Ln {ln}, Col {col}")
    }

    /// What the person reads while working, right: the document's pages and
    /// the preview's zoom. Pages that follow the pane's width say so: a
    /// percentage of the fitted width read "100%" over a page too small to
    /// read in a narrow pane, as if the page were at its actual size.
    fn status_right_label(&self) -> String {
        let pages = self.preview.page_count();
        if self.preview.is_blank() {
            return "Empty document".to_string();
        }
        if pages == 0 {
            return "No pages".to_string();
        }
        if self.preview.fits_width() {
            return format!("{}, fit width", plural(pages, "page", "pages"));
        }
        format!(
            "{}, {:.0}%",
            plural(pages, "page", "pages"),
            self.preview.zoom() * 100.0
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
    ui::provides!(dyn ui::widget::controls::ActionControls);

    fn settle(&mut self, edits: &mut ui::widget::Edits<'_>, path: &automerge::ObjId) {
        for child in self.children_mut() {
            child.settle(edits, path);
        }
        for command in self.chrome.take() {
            self.run_command(command, edits.doc());
            self.dirty = true;
        }
        self.take_outline_choice();
    }

    fn sync(&mut self, doc: &AppModel, path: &ModelPath) {
        self.sync_workspace(doc, path);
        // The bar's slots say what the sync just learned: the name, whether
        // the source is unsaved, the problems, the open panels.
        self.declare_window_slots();
        self.status.expire();
        self.status.publish(|text| {
            ui::window_toolbar::ToolbarNotice::new(text.clone(), ui::widget::toast::ToastKind::Info)
        });
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        self.background_wake()
            .into_iter()
            .chain(self.status.deadline())
            .min()
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
            .children_mut()
            .into_iter()
            .map(|child| child.build_layout(taffy, theme, node_map))
            .collect();
        // `new_with_children` only fails when a child belongs to a different
        // tree; every entry in `child_nodes` was just built in this one above.
        let style = Style {
            size: Size::from_percent(1.0, 1.0),
            ..Default::default()
        };
        taffy.widget_node(style, &child_nodes, node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let rect = ui::widget::layout_rect(taffy, node, origin);
        self.layout_in(rect);
    }

    fn children(&self) -> Vec<&dyn Widget> {
        let mut children = self.chrome.children();
        children.extend(self.outline.widget());
        children
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        let mut children = self.chrome.children_mut();
        children.extend(self.outline.widget_mut());
        children
    }

    fn take_structure_dirty(&mut self) -> bool {
        self.chrome.take_structure_dirty() | self.outline.take_structure_dirty()
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
        self.declare_window_slots();

        // The window ground is the framework's. The preview stands on the
        // inspector region; the editor on the ground itself.
        if self.preview_rect.area() > 0.0 {
            ui::surface::region(scene, t, self.preview_rect, Region::Inspector, theme);
        }
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
        self.outline.draw(scene, t, tr, theme, presence);

        ui::chrome::status_bar::draw(
            scene,
            t,
            tr,
            theme,
            self.status_rect,
            &self.status_left_label(),
            &self.status_right_label(),
        );

        self.note_window(theme);

        // This frame reflects the latest interaction state; clear the move-dirty
        // flag so idle hover doesn't spin the render loop.
        self.dirty = false;
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
        if !*ctx.handled || already_handled {
            self.route_to_outline(event, ctx, is_key);
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
            || self.preview.has_pending_renders()
            || self
                .children()
                .iter()
                .any(|child| child.has_pending_visual_work())
    }

    fn needs_animation_frame(&self) -> bool {
        // Keep the App pumping frames while: the editor caret is blinking,
        // a compile is in flight (so `draw` can consume its result), or a
        // control is animating.
        self.editor.wants_animation()
            || self.compiling()
            || self.preview.wants_frames()
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

    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        self.window_damage(regions);
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        self.chrome.mark_drawn();
        self.mark_window_shown();
    }
}

impl ui::widget::controls::ActionControls for TypstApp {
    fn control_action_bindings(&self) -> Vec<ui::widget::ControlBindingMetadata> {
        ui::widget::semantic_action_bindings(self.accessibility())
    }
}

impl TypstApp {
    /// Answer the file effect's caller, if one is waiting.
    fn finish_file_effect(&mut self, result: Result<(), String>) {
        if let Some(reply) = self.file_reply.take() {
            reply.finish(result.map(|()| ui::tilo_actions::ActionOutput::default()));
        }
    }

    /// Answer `effect`: a file read or write with a path waits for the next
    /// sync, one without asks the file chooser; the rest happen now.
    pub(crate) fn answer_effect(
        &mut self,
        effect: crate::actions::TypstEffect,
        cx: &mut ui::framework::EffectCx<crate::resource::Workspace, Self>,
    ) -> ui::framework::EffectAnswer {
        use crate::actions::TypstEffect;
        let file = matches!(
            effect,
            TypstEffect::OpenFile { .. }
                | TypstEffect::Save { .. }
                | TypstEffect::SaveAs
                | TypstEffect::ExportPdf { .. }
        );
        if file && (self.file_reply.is_some() || self.pending_resource_io.is_some()) {
            return Err("another file operation is already running".into());
        }
        match effect {
            TypstEffect::OpenFile { path: Some(path) } => {
                self.pending_resource_io = Some(ResourceIo::Open(path.into()));
            }
            TypstEffect::Save { path: Some(path) } => {
                self.pending_resource_io = Some(ResourceIo::Save(path.into()));
            }
            TypstEffect::ExportPdf { path: Some(path) } => {
                self.pending_resource_io = Some(ResourceIo::ExportPdf(path.into()));
            }
            TypstEffect::OpenFile { path: None } => self.open(),
            TypstEffect::Save { path: None } => {
                if self.save()? {
                    return Ok(ui::framework::Effected::done());
                }
            }
            TypstEffect::SaveAs => self.save_as(),
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
                        // A failed save says so in the flash. Shift asks
                        // where, even when the file has a place.
                        if mods.shift_key() {
                            self.save_as();
                        } else {
                            let _ = self.save();
                        }
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
                    "a" if !self.editor.focused => {
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
                        self.editor
                            .scroll_by(dx as f32, dy as f32, &self.source_cache);
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

    /// Give the outline the event: a pointer over its drawer, or a key while
    /// it, not the source, has focus. A press on a floating drawer's scrim
    /// closes it.
    fn route_to_outline(
        &mut self,
        event: &WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
        is_key: bool,
    ) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.mouse = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
        }
        if !self.outline.visible() {
            return;
        }
        let pressed = matches!(
            event,
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                ..
            }
        );
        if pressed && self.outline.dismiss_at(self.mouse) {
            self.layout_in(self.rect);
            self.dirty = true;
            *ctx.handled = true;
            return;
        }
        let over = self.outline.rect().contains(self.mouse);
        if (is_key && self.editor.focused) || (!is_key && !over) {
            return;
        }
        if pressed {
            self.editor.focused = false;
        }
        let node = ctx
            .taffy
            .children(ctx.node_id)
            .ok()
            .and_then(|nodes| nodes.last().copied())
            .unwrap_or(ctx.node_id);
        if let Some(list) = self.outline.widget_mut() {
            list.handle_event(event, &mut ctx.reborrow(ctx.path, node));
        }
        self.take_outline_choice();
        if !is_key {
            // The drawer's rows own the pointer over it.
            *ctx.handled = true;
        }
        self.dirty = true;
    }

    /// Go to the heading chosen in the outline, through its published effect.
    fn take_outline_choice(&mut self) {
        if let Some(effect) = self.outline.take_chosen() {
            match &self.actions {
                Some(actions) => {
                    actions.effect(&effect);
                }
                None => self.perform_resource_operation(effect),
            }
            self.dirty = true;
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

impl TypstApp {
    /// The workspace and its compile, followed on every sync.
    fn sync_workspace(&mut self, doc: &AppModel, path: &ModelPath) {
        // Retained controls settle their admitted operations on sync (notably
        // opening the toolbar overflow); Chrome's presentation sync only
        // updates the document label and current toggle state.
        for child in self.children_mut() {
            child.sync(doc, path);
        }
        let (doc, _) = ui::doc::document_ref(doc, path);
        self.background_poll_deadline.set(None);
        // "Try again" on the window's save status: save to where the failed
        // save went.
        if self.saves.take_retry()
            && let Some(path) = self.save_path.clone()
            && let Some(dispatcher) = &self.actions
        {
            dispatcher.effect(&crate::actions::TypstEffect::Save {
                path: Some(path.to_string_lossy().into_owned()),
            });
        }
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
        // Pages the renderer finished while nothing else happened repaint.
        if self.preview.take_renders() {
            self.dirty = true;
        }
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
            // (a capture's included) has the pages laid out, then bring the
            // async worker online for subsequent edits.
            self.last_source = source.clone();
            let out = compile::compile_source(&self.parts, &source);
            self.apply_output(out);
            self.worker = CompileWorker::spawn(self.parts.clone());
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

    /// When background work is next polled, while there is any.
    fn background_wake(&self) -> Option<Instant> {
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
}

mod damage;
#[cfg(test)]
mod damage_tests;
#[cfg(test)]
mod long_document_tests;
#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod tests;
