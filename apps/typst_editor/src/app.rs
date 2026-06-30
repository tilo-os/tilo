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
//! ├───────────────────────────┴──────────────────────────────┤
//! │ status: Ln/Col · pages · zoom            Compiled in N ms │
//! └──────────────────────────────────────────────────────────┘
//! ```
//!
//! State that belongs in the CRDT — the document source — lives in an
//! Automerge `Text` object under `root["source"]`; the editor splices into it
//! directly, so undo/redo and live collaboration come for free. Everything
//! else here (caret, scroll, zoom, the split ratio, cached page bitmaps) is
//! ephemeral view state.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjId, ObjType, ROOT, ReadDoc, Value};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::{Color, Fill};
use winit::event::{ElementState, KeyEvent, Modifiers, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use ui::permissions::PermissionContext;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::Widget;

use crate::compile::{self, CompileOutput, WorldParts};
use crate::diagnostics::DiagnosticsPanel;
use crate::editor::SourceEditor;
use crate::highlight::{self, Token};
use crate::preview::PreviewPane;
use crate::samples::{DEFAULT_INDEX, SAMPLES};
use crate::worker::CompileWorker;

/// Pixels-per-point the document is rasterised at. 2.0 keeps pages crisp on
/// a HiDPI surface; preview zoom only scales the *display*, never the render
/// resolution (matching the behaviour the welcome doc advertises).
const PPP: f32 = 2.0;

const TOOLBAR_H: f64 = 46.0;
const STATUS_H: f64 = 26.0;
const DIVIDER_W: f64 = 6.0;
const DIAG_H: f64 = 168.0;
const PILL_H: f64 = 28.0;
const PILL_FONT: f32 = 12.5;

/// How long a transient toast (Save / Open feedback) stays on screen.
const FLASH_MS: u128 = 3200;

/// A document the switcher can cycle to: either a bundled sample or a `.typ`
/// file discovered on disk.
#[derive(Clone)]
struct Document {
    name: String,
    source: String,
    from_disk: bool,
}

/// A toolbar button's logical action, resolved by hit-testing the cached
/// per-frame button rectangles.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TbAction {
    PrevDoc,
    NextDoc,
    Compile,
    Save,
    Open,
    ZoomOut,
    ZoomIn,
    Fit,
    ToggleProblems,
}

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
    /// Whether the first (synchronous) compile + worker spawn has happened.
    initialized: bool,
    /// Last source text we highlighted / compiled — drives change detection.
    last_source: String,
    /// Mirror of the source read each `sync`, used by `draw` (which has no
    /// doc access) for the editor body, syntax tokens and the caret readout.
    source_cache: String,
    tokens: Vec<Vec<Token>>,

    // --- view state ---
    /// Editor↔preview horizontal split, 0..1 of the content width.
    split_ratio: f64,
    show_diagnostics: bool,
    err_count: usize,
    warn_count: usize,
    elapsed_ms: u128,

    // --- layout rects (filled by apply_layout) ---
    rect: Rect,
    toolbar_rect: Rect,
    status_rect: Rect,
    divider_rect: Rect,
    editor_rect: Rect,
    preview_rect: Rect,
    diag_rect: Rect,

    // --- interaction ---
    mouse: Point,
    dragging_divider: bool,
    dragging_editor: bool,
    divider_hover: bool,
    /// Cached toolbar hit regions from the last draw.
    toolbar_hits: Vec<(TbAction, Rect)>,
    flash: Option<(String, Instant)>,
}

impl TypstApp {
    pub fn new() -> Self {
        let documents: Vec<Document> = SAMPLES
            .iter()
            .map(|s| Document {
                name: s.name.to_string(),
                source: s.source.to_string(),
                from_disk: false,
            })
            .collect();

        Self {
            parts: WorldParts::new(),
            worker: None,
            applied_seq: 0,
            editor: SourceEditor::new(),
            preview: PreviewPane::new(),
            diagnostics: DiagnosticsPanel::new(),
            source_id: None,
            documents,
            current: DEFAULT_INDEX,
            initialized: false,
            last_source: String::new(),
            source_cache: String::new(),
            tokens: Vec::new(),
            split_ratio: 0.46,
            show_diagnostics: true,
            err_count: 0,
            warn_count: 0,
            elapsed_ms: 0,
            rect: Rect::ZERO,
            toolbar_rect: Rect::ZERO,
            status_rect: Rect::ZERO,
            divider_rect: Rect::ZERO,
            editor_rect: Rect::ZERO,
            preview_rect: Rect::ZERO,
            diag_rect: Rect::ZERO,
            mouse: Point::ZERO,
            dragging_divider: false,
            dragging_editor: false,
            divider_hover: false,
            toolbar_hits: Vec::new(),
            flash: None,
        }
    }

    // ----- CRDT source plumbing -------------------------------------------

    /// Get-or-create the `root["source"]` Text object, seeding it with the
    /// current sample the first time it's created.
    fn ensure_source_obj(&self, doc: &mut AutoCommit) -> ObjId {
        match doc.get(&ROOT, "source") {
            Ok(Some((Value::Object(ObjType::Text), id))) => id,
            _ => {
                let id = doc
                    .put_object(&ROOT, "source", ObjType::Text)
                    .expect("put_object source on root never fails");
                let seed = self
                    .documents
                    .get(self.current)
                    .map(|d| d.source.clone())
                    .unwrap_or_default();
                let _ = doc.splice_text(&id, 0, 0, &seed);
                id
            }
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
        let src = self.documents[self.current].source.clone();
        if let Some(id) = self.source_id.clone() {
            let cur = doc.text(&id).unwrap_or_default();
            let _ = doc.splice_text(&id, 0, cur.chars().count() as isize, &src);
        }
        self.editor.caret = 0;
        self.editor.anchor = 0;
        self.editor.scroll_y = 0.0;
        self.editor.focused = true;
        self.preview.fit_width();
        let name = self.documents[self.current].name.clone();
        self.set_flash(format!("Opened {name}"));
    }

    // ----- compile pipeline -----------------------------------------------

    /// Poll the background worker; apply the freshest result if any arrived.
    /// Called from `draw` so async results are consumed even on frames where
    /// `sync` doesn't run (the App only re-syncs after input).
    fn pump_worker(&mut self) {
        let Some(w) = self.worker.as_mut() else {
            return;
        };
        if let Some(res) = w.poll() {
            if res.seq >= self.applied_seq {
                self.applied_seq = res.seq;
                self.apply_output(res.output);
            }
        }
    }

    fn apply_output(&mut self, out: CompileOutput) {
        self.elapsed_ms = out.elapsed_ms;
        self.err_count = out.error_count();
        self.warn_count = out.warning_count();
        self.preview.set_pages(&out.pages);
        if out.pages.is_empty() {
            self.preview
                .set_placeholder(Some("Compilation failed — see the Problems panel.".into()));
        }
        self.diagnostics.set_diagnostics(out.diagnostics);
        if self.err_count > 0 {
            // Surface the failure: pop the panel open so the message is visible.
            self.show_diagnostics = true;
        }
    }

    /// Re-highlight and queue a fresh compile of the current source.
    fn request_compile(&mut self, source: &str) {
        self.tokens = highlight::highlight(source);
        if let Some(w) = self.worker.as_mut() {
            w.request(source.to_string(), PPP);
        }
        self.last_source = source.to_string();
    }

    fn force_recompile(&mut self, doc: &mut AutoCommit) {
        let src = self.read_source(doc);
        self.source_cache = src.clone();
        self.request_compile(&src);
        self.set_flash("Recompiling…".into());
    }

    // ----- file I/O -------------------------------------------------------

    fn save_dir() -> Option<PathBuf> {
        dirs::document_dir()
            .or_else(dirs::data_dir)
            .map(|d| d.join("typst_editor"))
    }

    fn save(&mut self, doc: &mut AutoCommit) {
        let name = self
            .documents
            .get(self.current)
            .map(|d| d.name.clone())
            .unwrap_or_else(|| "untitled.typ".into());
        let src = self.read_source(doc);
        match Self::save_dir() {
            Some(dir) => {
                let _ = std::fs::create_dir_all(&dir);
                let path = dir.join(&name);
                match std::fs::write(&path, src) {
                    Ok(_) => {
                        self.set_flash(format!("Saved {}", path.display()));
                        self.rescan_library();
                    }
                    Err(e) => self.set_flash(format!("Save failed: {e}")),
                }
            }
            None => self.set_flash("No writable documents directory".into()),
        }
    }

    fn open(&mut self, doc: &mut AutoCommit) {
        self.rescan_library();
        if let Some(i) = self.documents.iter().position(|d| d.from_disk) {
            self.load_document(doc, i);
        } else {
            self.set_flash("No saved .typ files yet — Save one first.".into());
        }
    }

    /// Rebuild the document list: bundled samples first, then every `.typ`
    /// file found in the save directory (deduped against bundled names).
    fn rescan_library(&mut self) {
        self.documents.truncate(SAMPLES.len());
        if let Some(dir) = Self::save_dir() {
            if let Ok(rd) = std::fs::read_dir(&dir) {
                let mut entries: Vec<_> = rd
                    .flatten()
                    .filter(|e| {
                        e.path().extension().map(|x| x == "typ").unwrap_or(false)
                    })
                    .collect();
                entries.sort_by_key(|e| e.file_name());
                for e in entries {
                    let name = e.file_name().to_string_lossy().to_string();
                    if let Ok(src) = std::fs::read_to_string(e.path()) {
                        self.documents.push(Document {
                            name,
                            source: src,
                            from_disk: true,
                        });
                    }
                }
            }
        }
        if self.current >= self.documents.len() {
            self.current = self.documents.len().saturating_sub(1);
        }
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

    fn do_action(&mut self, action: TbAction, doc: &mut AutoCommit) {
        match action {
            TbAction::PrevDoc => self.prev_doc(doc),
            TbAction::NextDoc => self.next_doc(doc),
            TbAction::Compile => self.force_recompile(doc),
            TbAction::Save => self.save(doc),
            TbAction::Open => self.open(doc),
            TbAction::ZoomOut => self.preview.zoom_out(),
            TbAction::ZoomIn => self.preview.zoom_in(),
            TbAction::Fit => self.preview.fit_width(),
            TbAction::ToggleProblems => self.show_diagnostics = !self.show_diagnostics,
        }
    }

    fn set_flash(&mut self, msg: String) {
        self.flash = Some((msg, Instant::now()));
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
    fn caret_line_col(&self) -> (usize, usize) {
        let caret = self.editor.caret;
        let before: String = self.source_cache.chars().take(caret).collect();
        let line = before.matches('\n').count() + 1;
        let col = before
            .rsplit('\n')
            .next()
            .map(|s| s.chars().count())
            .unwrap_or(0)
            + 1;
        (line, col)
    }

    fn compiling(&self) -> bool {
        self.worker.as_ref().map(|w| w.in_flight()).unwrap_or(false)
    }

    fn hit_toolbar(&self, p: Point) -> Option<TbAction> {
        self.toolbar_hits
            .iter()
            .find(|(_, r)| r.contains(p))
            .map(|(a, _)| *a)
    }

    // ----- drawing --------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn paint_pill(
        &mut self,
        scene: &mut Scene,
        t: Affine,
        tr: &mut TextRenderer,
        theme: &Theme,
        rect: Rect,
        label: &str,
        action: TbAction,
        active: bool,
    ) {
        let c = &theme.tokens.colors;
        let shape = RoundedRect::from_rect(rect, 7.0);
        let bg = if active {
            theme.accent_primary
        } else {
            c.surface_white_5
        };
        scene.fill(Fill::NonZero, t, bg, None, &shape);
        if !active {
            scene.stroke(&Stroke::new(1.0), t, c.border_standard, None, &shape);
        }
        let fg = if active { Color::WHITE } else { c.text_primary };
        let (tw, _) = tr.measure_weighted(label, PILL_FONT, None, TextWeight::Medium);
        let tx = rect.x0 + (rect.width() - tw as f64) / 2.0;
        let ty = rect.y0 + (rect.height() - PILL_FONT as f64 * 1.25) / 2.0;
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
        tw as f64 + 22.0
    }

    fn draw_toolbar(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        self.toolbar_hits.clear();
        let c = &theme.tokens.colors;
        let tb = self.toolbar_rect;
        scene.fill(Fill::NonZero, t, c.surfaces.raised, None, &tb);
        // bottom hairline
        scene.fill(
            Fill::NonZero,
            t,
            c.border_standard,
            None,
            &Rect::new(tb.x0, tb.y1 - 1.0, tb.x1, tb.y1),
        );

        let y = tb.y0 + (tb.height() - PILL_H) / 2.0;
        let mut x = tb.x0 + 12.0;

        // --- left group: document switcher ---
        let prev_w = Self::pill_width(tr, "‹");
        self.paint_pill(
            scene,
            t,
            tr,
            theme,
            Rect::new(x, y, x + prev_w, y + PILL_H),
            "‹",
            TbAction::PrevDoc,
            false,
        );
        x += prev_w + 6.0;

        // doc name label (with index), drawn in a flat chip
        let name = self
            .documents
            .get(self.current)
            .map(|d| {
                if d.from_disk {
                    format!("{}  (disk)", d.name)
                } else {
                    d.name.clone()
                }
            })
            .unwrap_or_else(|| "untitled".into());
        let label = format!("{name}   {}/{}", self.current + 1, self.documents.len());
        let (lw, _) = tr.measure_weighted(&label, PILL_FONT, None, TextWeight::Semibold);
        let chip_w = lw as f64 + 24.0;
        let chip = Rect::new(x, y, x + chip_w, y + PILL_H);
        scene.fill(
            Fill::NonZero,
            t,
            c.surfaces.base,
            None,
            &RoundedRect::from_rect(chip, 7.0),
        );
        tr.draw_text_weighted(
            scene,
            &label,
            PILL_FONT,
            c.text_primary,
            t * Affine::translate((x + 12.0, y + (PILL_H - PILL_FONT as f64 * 1.25) / 2.0)),
            None,
            TextWeight::Semibold,
        );
        x += chip_w + 6.0;

        let next_w = Self::pill_width(tr, "›");
        self.paint_pill(
            scene,
            t,
            tr,
            theme,
            Rect::new(x, y, x + next_w, y + PILL_H),
            "›",
            TbAction::NextDoc,
            false,
        );
        x += next_w + 16.0;

        // --- action group ---
        for (lbl, act) in [
            ("Compile", TbAction::Compile),
            ("Save", TbAction::Save),
            ("Open", TbAction::Open),
        ] {
            let w = Self::pill_width(tr, lbl);
            self.paint_pill(
                scene,
                t,
                tr,
                theme,
                Rect::new(x, y, x + w, y + PILL_H),
                lbl,
                act,
                false,
            );
            x += w + 8.0;
        }

        // --- right group: zoom + problems (placed from the right edge) ---
        let mut rx = tb.x1 - 12.0;

        let problems_active = self.show_diagnostics;
        let problems_lbl = if self.err_count > 0 {
            format!("Problems ({})", self.err_count)
        } else if self.warn_count > 0 {
            format!("Problems ({})", self.warn_count)
        } else {
            "Problems".to_string()
        };
        let pw = Self::pill_width(tr, &problems_lbl);
        let pr = Rect::new(rx - pw, y, rx, y + PILL_H);
        self.paint_pill(
            scene,
            t,
            tr,
            theme,
            pr,
            &problems_lbl,
            TbAction::ToggleProblems,
            problems_active,
        );
        rx -= pw + 14.0;

        // zoom cluster: [ − ] [ 100% ] [ + ] [ Fit ]  (right to left)
        for (lbl, act) in [
            ("Fit", TbAction::Fit),
            ("+", TbAction::ZoomIn),
        ] {
            let w = Self::pill_width(tr, lbl);
            let r = Rect::new(rx - w, y, rx, y + PILL_H);
            self.paint_pill(scene, t, tr, theme, r, lbl, act, false);
            rx -= w + 6.0;
        }
        // zoom percentage chip (non-interactive)
        let zlabel = format!("{:.0}%", self.preview.zoom() * 100.0);
        let (zw, _) = tr.measure_weighted(&zlabel, PILL_FONT, None, TextWeight::Medium);
        let zchip_w = zw as f64 + 16.0;
        let zr = Rect::new(rx - zchip_w, y, rx, y + PILL_H);
        tr.draw_text_weighted(
            scene,
            &zlabel,
            PILL_FONT,
            c.text_dim,
            t * Affine::translate((
                zr.x0 + 8.0,
                y + (PILL_H - PILL_FONT as f64 * 1.25) / 2.0,
            )),
            None,
            TextWeight::Medium,
        );
        rx -= zchip_w + 6.0;
        let zow = Self::pill_width(tr, "−");
        let zor = Rect::new(rx - zow, y, rx, y + PILL_H);
        self.paint_pill(scene, t, tr, theme, zor, "−", TbAction::ZoomOut, false);
    }

    fn draw_divider(&mut self, scene: &mut Scene, t: Affine, theme: &Theme) {
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
        let cx = (dr.x0 + dr.x1) / 2.0;
        let cy = (dr.y0 + dr.y1) / 2.0;
        let dot_color = if active { Color::WHITE } else { c.text_dim };
        for k in -1i32..=1 {
            let cyk = cy + k as f64 * 6.0;
            scene.fill(
                Fill::NonZero,
                t,
                dot_color,
                None,
                &vello::kurbo::Circle::new(Point::new(cx, cyk), 1.4),
            );
        }
    }

    fn draw_status(&mut self, scene: &mut Scene, t: Affine, tr: &mut TextRenderer, theme: &Theme) {
        let c = &theme.tokens.colors;
        let sb = self.status_rect;
        scene.fill(Fill::NonZero, t, c.surfaces.raised, None, &sb);
        scene.fill(
            Fill::NonZero,
            t,
            c.border_standard,
            None,
            &Rect::new(sb.x0, sb.y0, sb.x1, sb.y0 + 1.0),
        );
        let ty = sb.y0 + (sb.height() - 12.0 * 1.25) / 2.0;

        // left: caret + page + zoom
        let (ln, col) = self.caret_line_col();
        let left = format!(
            "Ln {ln}, Col {col}     ·     {} page(s)     ·     {:.0}%",
            self.preview.page_count(),
            self.preview.zoom() * 100.0
        );
        tr.draw_text(
            scene,
            &left,
            12.0,
            c.text_dim,
            t * Affine::translate((sb.x0 + 12.0, ty)),
            None,
        );

        // center: transient flash
        if let Some(msg) = self.flash_text() {
            let (fw, _) = tr.measure(msg, 12.0, None);
            let fx = sb.x0 + (sb.width() - fw as f64) / 2.0;
            tr.draw_text_weighted(
                scene,
                msg,
                12.0,
                theme.accent_primary,
                t * Affine::translate((fx, ty)),
                None,
                TextWeight::Medium,
            );
        }

        // right: compile status
        let (text, color) = if self.compiling() {
            ("Compiling…".to_string(), c.semantic.info)
        } else if self.err_count > 0 {
            (
                format!(
                    "{} error(s), {} warning(s)",
                    self.err_count, self.warn_count
                ),
                c.semantic.danger,
            )
        } else if self.warn_count > 0 {
            (format!("{} warning(s)", self.warn_count), c.semantic.warning)
        } else {
            (format!("Compiled in {} ms", self.elapsed_ms), c.semantic.success)
        };
        let (rw, _) = tr.measure_weighted(&text, 12.0, None, TextWeight::Medium);
        tr.draw_text_weighted(
            scene,
            &text,
            12.0,
            color,
            t * Affine::translate((sb.x1 - 12.0 - rw as f64, ty)),
            None,
            TextWeight::Medium,
        );
    }
}

impl Default for TypstApp {
    fn default() -> Self {
        Self::new()
    }
}

impl Widget for TypstApp {
    fn sync(&mut self, doc: &mut AutoCommit, _path: &ObjId) {
        let src_id = self.ensure_source_obj(doc);
        self.source_id = Some(src_id.clone());
        let source = doc.text(&src_id).unwrap_or_default();

        if !self.initialized {
            // First frame: compile synchronously so the very first paint
            // (including `--shots`) has rendered pages, then bring the async
            // worker online for subsequent edits.
            self.tokens = highlight::highlight(&source);
            let out = compile::compile_source(&self.parts, &source, PPP);
            self.apply_output(out);
            self.worker = Some(CompileWorker::spawn(self.parts.clone()));
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
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
        );

        let r = self.rect;
        self.toolbar_rect = Rect::new(r.x0, r.y0, r.x1, r.y0 + TOOLBAR_H);
        self.status_rect = Rect::new(r.x0, r.y1 - STATUS_H, r.x1, r.y1);
        let content_top = self.toolbar_rect.y1;
        let content_bottom = self.status_rect.y0;
        let content_w = r.width();
        let split_x = r.x0 + content_w * self.split_ratio;

        let editor_col = Rect::new(r.x0, content_top, split_x, content_bottom);
        self.preview_rect = Rect::new(split_x + DIVIDER_W, content_top, r.x1, content_bottom);
        self.divider_rect = Rect::new(split_x, content_top, split_x + DIVIDER_W, content_bottom);

        if self.show_diagnostics && editor_col.height() > 80.0 {
            let dh = DIAG_H.min(editor_col.height() * 0.5);
            self.editor_rect =
                Rect::new(editor_col.x0, editor_col.y0, editor_col.x1, editor_col.y1 - dh);
            self.diag_rect =
                Rect::new(editor_col.x0, editor_col.y1 - dh, editor_col.x1, editor_col.y1);
        } else {
            self.editor_rect = editor_col;
            self.diag_rect = Rect::ZERO;
        }

        self.editor.set_rect(self.editor_rect);
        self.preview.set_rect(self.preview_rect);
        self.diagnostics.set_rect(self.diag_rect);
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

        let c = &theme.tokens.colors;
        scene.fill(Fill::NonZero, t, c.surfaces.base, None, &self.rect);

        self.draw_toolbar(scene, t, tr, theme);
        self.editor
            .draw(scene, t, tr, theme, &self.source_cache, &self.tokens);
        self.preview.draw(scene, t, tr, theme);
        self.draw_divider(scene, t, theme);
        if self.show_diagnostics && self.diag_rect.height() > 1.0 {
            self.diagnostics.draw(scene, t, tr, theme);
        }
        self.draw_status(scene, t, tr, theme);
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

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        modifiers: &Modifiers,
        doc: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        let src_id = match &self.source_id {
            Some(id) => id.clone(),
            None => self.ensure_source_obj(doc),
        };

        // --- application-level keyboard shortcuts ---
        if let WindowEvent::KeyboardInput {
            event: KeyEvent { logical_key, state: ElementState::Pressed, .. },
            ..
        } = event
        {
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
                        // a/c/x/v fall through to the editor's own clipboard
                        _ => {}
                    }
                }
            }
            // Route remaining keys to the focused editor.
            if self.editor.focused
                && self.editor.handle_key(event, modifiers, doc, &src_id)
            {
                *handled = true;
            }
            return;
        }

        // --- pointer ---
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.mouse = p;
                self.divider_hover = self.divider_rect.inflate(2.0, 0.0).contains(p);
                if self.dragging_divider {
                    let content_w = self.rect.width().max(1.0);
                    self.split_ratio =
                        ((p.x - self.rect.x0) / content_w).clamp(0.18, 0.82);
                    *handled = true;
                } else if self.dragging_editor {
                    let text = doc.text(&src_id).unwrap_or_default();
                    self.editor.on_mouse_drag(p, &text);
                    *handled = true;
                } else if self.show_diagnostics {
                    self.diagnostics.on_mouse_move(p);
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let p = self.mouse;
                if self.toolbar_rect.contains(p) {
                    if let Some(a) = self.hit_toolbar(p) {
                        self.do_action(a, doc);
                    }
                    *handled = true;
                    return;
                }
                if self.divider_rect.inflate(2.0, 0.0).contains(p) {
                    self.dragging_divider = true;
                    *handled = true;
                    return;
                }
                if self.editor_rect.contains(p) {
                    self.editor.focused = true;
                    let text = doc.text(&src_id).unwrap_or_default();
                    self.editor
                        .on_mouse_down(p, modifiers.state().shift_key(), &text);
                    self.dragging_editor = true;
                    *handled = true;
                    return;
                }
                if self.show_diagnostics && self.diag_rect.contains(p) {
                    if let Some(line) = self.diagnostics.on_mouse_down(p) {
                        let text = doc.text(&src_id).unwrap_or_default();
                        self.editor.goto_line(&text, line);
                        self.editor.focused = true;
                    }
                    *handled = true;
                    return;
                }
                if self.preview_rect.contains(p) {
                    self.editor.focused = false;
                    self.preview.on_mouse_down(p);
                    *handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                self.dragging_divider = false;
                self.dragging_editor = false;
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => -(*y as f64) * 48.0,
                    MouseScrollDelta::PixelDelta(pp) => -pp.y,
                };
                let p = self.mouse;
                if self.editor_rect.contains(p) {
                    let text = doc.text(&src_id).unwrap_or_default();
                    self.editor.scroll_by(dy as f32, &text);
                } else if self.show_diagnostics && self.diag_rect.contains(p) {
                    self.diagnostics.scroll_by(dy);
                } else if self.preview_rect.contains(p) {
                    self.preview.scroll_by(dy);
                }
                *handled = true;
            }
            _ => {}
        }
    }

    fn name(&self) -> &str {
        "TypstEditor"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn needs_animation_frame(&self) -> bool {
        // Keep the App pumping frames while: the editor caret is blinking,
        // a compile is in flight (so `draw` can consume its result), or a
        // transient toast is still fading.
        self.editor.wants_animation() || self.compiling() || self.flash_text().is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> (TypstApp, AutoCommit) {
        let mut app = TypstApp::new();
        let mut doc = AutoCommit::new();
        // Drive one sync to seed + compile the bundled sample.
        app.sync(&mut doc, &ROOT);
        (app, doc)
    }

    #[test]
    fn first_sync_seeds_source_and_compiles() {
        let (app, doc) = fresh();
        assert!(app.source_id.is_some(), "source object seeded");
        let src = app.read_source(&doc);
        assert!(!src.is_empty(), "seeded with sample text");
        assert!(app.initialized);
        // Welcome sample compiles cleanly into at least one page.
        assert!(app.preview.page_count() >= 1, "rendered a page");
        assert_eq!(app.err_count, 0, "welcome sample has no errors");
    }

    #[test]
    fn switching_documents_replaces_source() {
        let (mut app, mut doc) = fresh();
        let first = app.read_source(&doc);
        app.next_doc(&mut doc);
        let second = app.read_source(&doc);
        assert_ne!(first, second, "next_doc swapped the CRDT source");
        assert_eq!(app.current, 1);
    }

    #[test]
    fn caret_line_col_tracks_newlines() {
        let mut app = TypstApp::new();
        app.source_cache = "abc\nde\nf".to_string();
        app.editor.caret = 5; // line 2, col 2 (just after 'd')
        assert_eq!(app.caret_line_col(), (2, 2));
    }
}
