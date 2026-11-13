//! The root widget of the Typst editor.
//!
//! `TypstApp` is a single full-bleed [`Widget`]: the library controls of
//! [`Chrome`] above two panes it paints itself, the source editor and the
//! rendered preview, laid out in rects computed by `TypstApp::layout_in`. The
//! editor opens on the window's region and the preview on the inspector's; the
//! caret position, pages and zoom are the status bar, and a change worth
//! noticing once is said in the window's bar for a while.
//!
//! The workspace's files — names, paths and sources — are the resource's state
//! ([`crate::resource::Workspace`]), so undo/redo and collaboration come from
//! its field edits and verbs. Which file is open, the caret, scroll, zoom, the
//! split ratio and cached page bitmaps are view state.

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

mod store;
pub(crate) use store::window;
use store::{Shared, Store};

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
    FileFilter::new(&crate::resource::CATALOG.t("typst-filter-source")).glob("*.typ")
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
        .filter(FileFilter::new(&crate::resource::CATALOG.t("typst-filter-pdf")).glob("*.pdf"))
}
use ui::widget::{SplitHandle, SplitOrientation, SplitRatio, Widget};

use crate::chrome::{Chrome, Command};
use crate::compile::{self, CompileOutput, WorldParts};
use crate::diagnostics::DiagnosticsPanel;
use crate::editor::SourceEditor;
use crate::preview::PreviewPane;
use crate::resource::TypstFilePaths;
use crate::worker::CompileWorker;

#[derive(Clone, Copy, PartialEq, Eq)]
enum CompactPane {
    Editor,
    Preview,
}

#[cfg(feature = "samples")]
pub(crate) const SCROLL_DEMO: &str = "= Horizontal scroll\n\n#let note = \"This is a single very long line of source text that runs well past the right edge of the editor pane, so the view must scroll sideways to keep the caret in view as you type toward the end.\"";

#[derive(Clone, Copy, PartialEq)]
struct Frame {
    control_h: f64,
    status_h: f64,
    compact_w: f64,
}

impl Frame {
    fn of(theme: &Theme, control_h: f64, status_line_h: f64) -> Self {
        let metrics = &theme.tokens.metrics;
        Self {
            control_h: control_h + 2.0 * f64::from(theme.tokens.spacing.s1),
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
    parts: Arc<WorldParts>,
    worker: Option<CompileWorker>,
    applied_seq: u64,

    editor: SourceEditor,
    preview: PreviewPane,
    diagnostics: DiagnosticsPanel,
    outline: crate::outline::Outline,

    files: Rc<crate::resource::Workspace>,
    current: Option<Id>,
    kept: std::collections::HashMap<Id, ui::portal::SaveDestination>,
    pending_open: bool,
    known_files: Vec<Id>,
    actions: Option<ui::framework::Dispatcher<crate::resource::Workspace>>,
    io: Option<PendingIo>,
    background_poll_deadline: Cell<Option<Instant>>,
    store: Shared,
    live: Option<ui::framework::Live<crate::resource::Workspace>>,
    presented: Option<(u64, u64)>,
    initialized: bool,
    last_source: String,
    source_cache: String,
    saved: Option<(Id, String)>,

    show_diagnostics: bool,
    err_count: usize,
    warn_count: usize,
    elapsed_ms: u128,
    compact: bool,
    compact_view: CompactPane,
    text_k: f64,
    demo_scroll_caret_end: bool,
    demo_editor_select: bool,
    demo_preview_zoom: bool,
    demo_preview_pan: bool,
    demo_preview_select: bool,
    demo_stale: bool,

    chrome: Chrome,
    frame: Frame,
    status: ui::widget::toast::Transient<String>,
    saves: ui::framework::Saves,
    save_path: Option<std::path::PathBuf>,
    compile_feedback: bool,

    rect: Rect,
    editor_rect: Rect,
    preview_rect: Rect,
    diag_rect: Rect,
    status_rect: Rect,

    mouse: Point,
    dragging_editor: bool,
    dragging_preview: bool,
    splitter: SplitHandle,
    dirty: bool,
    window_shown: ui::scroll_copy::ShownFrames<damage::WindowShown>,
}

impl TypstApp {
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
            store: Rc::default(),
            live: None,
            presented: None,
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
            .with_label(crate::resource::CATALOG.t("typst-a11y-resize")),
            dirty: true,
            window_shown: ui::scroll_copy::ShownFrames::default(),
            actions: None,
        }
    }

    /// Take part in the view `cx` builds: edit through its dispatcher,
    /// follow the document it holds; a capture's first file.
    pub(crate) fn attach(&mut self, cx: &ui::framework::ViewCx<crate::resource::Workspace>) {
        self.chrome.attach_actions(&cx.actions());
        self.actions = Some(cx.dispatcher());
        self.saves = cx.saves();
        self.editor.attach(cx.dispatcher());
        self.live = Some(cx.live().clone());
        #[cfg(feature = "samples")]
        if let Some(file) = cx.fixture::<crate::resource::CaptureFile>() {
            self.current = Some(file.0.clone());
        }
        self.follow_current();
    }

    pub(crate) fn state(&self) -> std::cell::RefMut<'_, Store> {
        self.store.borrow_mut()
    }

    fn follow_document(&mut self) {
        let Some(shown) = self
            .live
            .as_ref()
            .and_then(|live| live.changed_since(&mut self.presented))
        else {
            return;
        };
        self.present_state(&shown);
    }

    fn answer_asks(&mut self) {
        let (asks, staged) = {
            let mut state = self.state();
            (std::mem::take(&mut state.asks), state.staged.take())
        };
        if let Some(scenario) = staged {
            self.stage_scenario(scenario.0.as_deref());
        }
        for ask in asks {
            match ask {
                crate::actions::TypstEffect::Save { path: None } => match self.save() {
                    Ok(true) => self.finish_file_effect(Ok(())),
                    // The chooser or the write answers when it settles.
                    Ok(false) => {}
                    Err(error) => self.finish_file_effect(Err(error)),
                },
                other => self.perform_resource_operation(other),
            }
            self.dirty = true;
        }
    }

    /// Publish the Problems rows, preview pages and headings as they are
    /// now.
    fn publish_rows(&self) {
        let rows = self.live_rows();
        self.state().rows = rows;
    }

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
        self.chrome.sync(crate::chrome::Held {
            name: self.current_name(),
            edited: self.unsaved(),
            problems: self.err_count + self.warn_count,
            panels: crate::chrome::Panels {
                problems: self.show_diagnostics,
                outline: self.outline.visible(),
            },
            pane: match self.compact_view {
                CompactPane::Editor => crate::chrome::Pane::Editor,
                CompactPane::Preview => crate::chrome::Pane::Preview,
            },
        });
        self.chrome.declare();
    }

    pub(crate) fn current_file(&self) -> Option<&crate::resource::TypstFile> {
        self.files.file(self.current.as_ref()?)
    }

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

    fn submit(&self, submit: impl ui::framework::Submit<crate::resource::Workspace>) {
        if let Some(actions) = &self.actions {
            actions.submit(submit);
        }
    }

    fn replace_source(&self, text: &str) {
        if let Some(paths) = self.current_paths() {
            self.submit(paths.source.replace(text));
        }
    }

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
    }

    pub(crate) fn live_rows(&self) -> Vec<ui::framework::LiveRow> {
        crate::actions::live_extras(&crate::actions::TypstResourceCtx {
            diagnostics: self.diagnostics.diags(),
            page_count: self.preview.page_count(),
        })
        .into_iter()
        .chain(self.outline.rows())
        .collect()
    }

    /// Apply the freshest compile result, if any arrived. Called from `draw`, since
    /// the app only re-syncs after input.
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
        // The Problems rows, pages and headings follow each compile.
        self.publish_rows();
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

    /// Compile `source` on the worker, or here when no worker could start (the
    /// browser has no threads for it).
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

    fn current_name(&self) -> String {
        self.current_file()
            .map_or_else(|| "untitled.typ".into(), |file| file.name.0.clone())
    }

    /// Save the file shown where it was opened from or last saved, else to a path
    /// the host's dialog asks for. The chosen path is handled in [`Self::drain_io`].
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

    fn open(&mut self) {
        if self.io.is_some() {
            return;
        }
        self.io = Some(PendingIo::OpenTyp(typ_open_request().send_contents()));
        self.set_flash("Choose a .typ file to open");
    }

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

    /// Run a finished Open, Save or Export dialog against the chosen path. The UI
    /// thread never blocks: the portal round-trip runs in a worker thread.
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
    }

    fn set_flash(&mut self, msg: impl AsRef<str>) {
        self.status.show(msg.as_ref().to_owned());
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

    /// Compute every pane rect from the window rect, split out of
    /// [`Widget::apply_layout`] so a resize is testable without a taffy pass.
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

        // The layout mode comes from the current width every pass, so a resize across
        // the threshold reflows. The breakpoint is in 1× text: a window holds as much
        // 2× text as a half-width 1× window.
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

    /// Screenshot harness only: arrange the caret, zoom, scroll or selection the
    /// capture shows, once the panes have rects and the preview has pages.
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

    /// Measure the chrome in `theme` and re-fit what follows: the panes, the
    /// panel's rows and where each control sits.
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

    fn status_left_label(&self) -> String {
        let (ln, col) = self.caret_line_col();
        format!("Ln {ln}, Col {col}")
    }

    /// What the person reads while working, right: the document's pages and the
    /// preview's zoom. Pages that follow the pane's width say so: a percentage of
    /// the fitted width reads "100%" over a page too small to read.
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

pub(crate) fn plural(n: usize, singular: &str, plural: &str) -> String {
    if n == 1 {
        format!("{n} {singular}")
    } else {
        format!("{n} {plural}")
    }
}

/// The window's search finds in the source: the editor is drawn by the app, not
/// a widget in its tree, so the offer the bar looks for is made here over it.
impl ui::widget::Findable for TypstApp {
    fn find(
        &mut self,
        query: &str,
        options: ui::widget::FindOptions,
    ) -> Vec<ui::widget::FindMatch> {
        let found = ui::widget::Findable::find(&mut self.editor, query, options);
        self.dirty = true;
        found
    }

    fn reveal(&mut self, index: usize) {
        ui::widget::Findable::reveal(&mut self.editor, index);
        self.dirty = true;
    }

    fn content_revision(&self) -> u64 {
        ui::widget::Findable::content_revision(&self.editor)
    }

    fn clear_find(&mut self) {
        ui::widget::Findable::clear_find(&mut self.editor);
        self.dirty = true;
    }
}

impl Widget for TypstApp {
    ui::provides!(
        dyn ui::widget::Findable,
        dyn ui::widget::controls::ActionControls
    );

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
        self.follow_document();
        self.answer_asks();
        self.sync_workspace(doc, path);
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

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
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

        // The preview paints into the inspector region, the editor into the window's.
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
            .with_name(crate::resource::CATALOG.t("typst-a11y-source"))
            .with_value(self.source_cache.clone())
            .with_state(AccessState::new().focusable(true).multiline(true))
            .with_action_target("typst.source");
        let preview = AccessNode::new(Role::Canvas)
            .with_name(crate::resource::CATALOG.t("typst-a11y-preview"))
            .with_value(format!(
                "Page {} of {}; zoom {:.0}%",
                self.preview.current_page().saturating_add(1),
                self.preview.page_count(),
                self.preview.zoom() * 100.0
            ))
            .with_description(if self.compact {
                crate::resource::CATALOG.t("typst-a11y-compact-layout")
            } else {
                crate::resource::CATALOG.t("typst-a11y-live-preview")
            })
            .with_semantic_key("typst.preview")
            .with_hashed_key("typst.preview");
        let mut children = vec![source, preview];
        if self.show_diagnostics {
            children.extend(self.diagnostics.accessibility());
        }

        Some(
            AccessNode::new(Role::Document)
                .with_name(crate::resource::CATALOG.t("typst-a11y-window"))
                .with_description(crate::resource::CATALOG.t("typst-a11y-window-description"))
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
        self.dirty || self.chrome.needs_redraw() || ui::widget::Widget::needs_animation_frame(self)
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
    fn finish_file_effect(&self, result: Result<(), String>) {
        let reply = self.state().file_reply.take();
        if let Some(reply) = reply {
            reply.finish(result.map(|()| ui::tilo_actions::ActionOutput::default()));
        }
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
            && let Some(Ok(path)) = self
                .save_path
                .clone()
                .map(ui::tilo_resource::AbsolutePath::new)
            && let Some(dispatcher) = &self.actions
        {
            dispatcher.effect(&crate::actions::TypstEffect::Save { path: Some(path) });
        }
        let operation = self.state().pending_resource_io.take();
        if let Some(operation) = operation {
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

    fn background_wake(&self) -> Option<Instant> {
        if self.io.is_none() && !self.store.borrow().waiting() && !self.compiling() {
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
