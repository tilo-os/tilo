//! The root widget: a toolbar, the image canvas, and a status line.
//!
//! # Composition
//!
//! Every control here is a shared toolkit widget. The buttons are
//! [`ui::widget::Button`], the canvas is [`ui::widget::ImageViewer`], and the
//! nothing-open state is [`ui::widget::EmptyState`]. Nothing is hand-rolled:
//! a bespoke button would have to re-earn focus, hover, disabled contrast,
//! accessibility roles and theming, and would drift from the rest of the
//! system the first time any of those changed.
//!
//! There is no title bar. The compositor owns window chrome; an app that drew
//! its own name at the top would be drawing it twice.
//!
//! # Where the pixels live
//!
//! The document holds a path and an [`Edits`] value. This struct holds the
//! decoded original and the rendered result, both outside the CRDT, and
//! re-derives the rendering whenever the path or the edits change. That check
//! is what keeps a rotate from re-decoding the file, and what keeps the
//! decoded buffer from being rebuilt on every frame.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use automerge::{AutoCommit, ObjId};
use taffy::prelude::*;
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{Blob, ImageAlphaType, ImageData, ImageFormat};

use ui::a11y::AccessNode;
use ui::icons::Icon;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{Button, EmptyState, ImageViewer, Widget};

use crate::doc::PhotosDoc;
use crate::model::{Edits, OpenImage, Rect01};
use crate::pixels::{self, Rgba};
use crate::strings::tr;

/// Toolbar height, and the status bar's. Both are chrome around the canvas,
/// which takes everything left over.
const BAR_H: f32 = 48.0;
const PAD: f32 = 12.0;

/// Button height. Deliberately shorter than the bar so the controls sit in
/// the strip rather than filling it edge to edge.
const BTN_H: f32 = 34.0;

/// Per-character label width used to size a button in `build_layout`, which
/// gets a [`Theme`] but no [`TextRenderer`] to shape text with.
///
/// An **upper bound**, not an average. The shared [`ui::widget::Toolbar`]
/// estimates 7.0 for the same job, which is too tight for this one: at 14 px
/// Medium, "Open" shapes to 36.5 px — 9.13 a character — so a 7.0 budget left
/// the label 8.5 px wider than the button drawn around it, and `Button::paint`
/// centres the cluster, so it spilled out of both ends onto its neighbours.
/// `toolbar_labels_fit_inside_the_buttons_they_are_sized_for` in
/// `tests/end_to_end.rs` shapes the real glyphs and holds this bound.
pub const APPROX_CHAR_W: f32 = 10.0;

/// Glyph box, and the gap to a label beside it. `Button::paint` derives the
/// icon from the button height as `(h - 14).clamp(12, 22)`, which at
/// [`BTN_H`] is exactly this.
const ICON_W: f32 = BTN_H - 14.0;
const ICON_GAP: f32 = 6.0;
/// Padding either side of an icon-and-label cluster.
const LABEL_PAD_X: f32 = 12.0;
/// Gap between adjacent buttons.
const BTN_GAP: f32 = 6.0;
/// Horizontal room a group separator occupies, rule included.
const SEP_W: f32 = 17.0;

/// Breathing room between the photograph and the chrome above and below it.
/// Without it a contain-fit image whose limiting axis is the height butts
/// straight against the toolbar rule and looks wedged under it.
const CANVAS_INSET: f32 = 10.0;

/// One toolbar affordance. Buttons post their intent here rather than
/// mutating the document from inside a closure, because the document is not
/// reachable from a `FnMut()` — it arrives in `handle_event`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hit {
    Open,
    RotateLeft,
    RotateRight,
    CropToView,
    Reset,
    Copy,
    Save,
}

/// How a toolbar item presents itself.
///
/// An [`Face::IconOnly`] button is not a mute one: the label rides along as
/// the tooltip, which [`ui::widget::Button`] also adopts as the accessible
/// name, so the glyph stays reachable by pointer-dwell and by screen reader.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Face {
    /// Glyph and words together.
    Labelled,
    /// Glyph alone.
    IconOnly,
}

/// A slot in the toolbar strip, in painting order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Slot {
    /// Index into [`PhotosApp::buttons`].
    Item(usize),
    /// A hairline dividing two groups.
    Separator,
    /// Greedy gap: soaks the leftover width so what follows sits hard right.
    Spacer,
}

/// The toolbar, in order: what opens a file, what adjusts it, what gets the
/// result out.
///
/// The four adjusters are glyph-only at every width. They are the operations
/// whose icons are unambiguous, and spelling all seven out is what pushed the
/// strip to a fixed 912 px — wider than a half-tiled window, which silently
/// clipped Save off the right-hand edge.
///
/// Three of the seven glyphs below stand in for icons the shared set does not
/// have yet (`RotateCcw`, a crop mark, a disk): `RotateLeft` reuses
/// [`Icon::RotateCw`] (the same glyph as its neighbour, told apart by tooltip
/// and accessible name — a real but bounded gap), `CropToView` reuses
/// [`Icon::Scissors`], and `Save` reuses [`Icon::Download`]. Adding the exact
/// glyphs is `ui::icons` work, out of this app's scope.
const TOOLBAR: &[(Hit, &str, Icon, Face)] = &[
    (Hit::Open, "photos-open", Icon::Folder, Face::Labelled),
    (
        Hit::RotateLeft,
        "photos-rotate-left",
        Icon::RotateCw,
        Face::IconOnly,
    ),
    (
        Hit::RotateRight,
        "photos-rotate-right",
        Icon::RotateCw,
        Face::IconOnly,
    ),
    (
        Hit::CropToView,
        "photos-crop",
        Icon::Scissors,
        Face::IconOnly,
    ),
    (Hit::Reset, "photos-reset", Icon::Refresh, Face::IconOnly),
    (Hit::Copy, "photos-copy", Icon::Copy, Face::Labelled),
    (Hit::Save, "photos-save", Icon::Download, Face::Labelled),
];

/// The strip's shape: Open, a rule, the adjusters, then everything that gets
/// the photograph out, pushed to the trailing edge.
const SLOTS: &[Slot] = &[
    Slot::Item(0),
    Slot::Separator,
    Slot::Item(1),
    Slot::Item(2),
    Slot::Item(3),
    Slot::Item(4),
    Slot::Spacer,
    Slot::Item(5),
    Slot::Item(6),
];

/// Width a button needs to show `face` with `label`.
fn item_width(face: Face, label: &str) -> f32 {
    match face {
        Face::IconOnly => BTN_H,
        Face::Labelled => {
            let text = label.chars().count() as f32 * APPROX_CHAR_W;
            (LABEL_PAD_X * 2.0 + ICON_W + ICON_GAP + text).max(BTN_H)
        }
    }
}

/// Width the whole strip needs, `faces` giving each item's presentation.
///
/// Exposed so a test can assert the strip fits a window rather than
/// discovering it does not by finding Save missing from a screenshot.
pub fn toolbar_width(faces: &[Face]) -> f32 {
    let mut w = PAD * 2.0;
    for (i, slot) in SLOTS.iter().enumerate() {
        if i > 0 {
            w += BTN_GAP;
        }
        w += match *slot {
            // Unreachable: `faces` and `TOOLBAR` are always built the same
            // length, one entry per `SLOTS::Item` index.
            Slot::Item(n) => match (faces.get(n), TOOLBAR.get(n)) {
                (Some(&face), Some(item)) => item_width(face, &tr(item.1)),
                _ => 0.0,
            },
            Slot::Separator => SEP_W,
            Slot::Spacer => 0.0,
        };
    }
    w
}

/// Each item's presentation at a given strip width.
///
/// The labelled items give up their words rather than the strip giving up an
/// item: a control that has shrunk to its glyph is still reachable, one that
/// has fallen off the edge is not.
fn faces_for_width(width: f32) -> Vec<Face> {
    let roomy: Vec<Face> = TOOLBAR.iter().map(|t| t.3).collect();
    // Width 0 is the first frame, before a layout has happened. Assume room;
    // the next frame corrects it with a real measurement.
    if width <= 0.0 || toolbar_width(&roomy) <= width {
        return roomy;
    }
    TOOLBAR.iter().map(|_| Face::IconOnly).collect()
}

/// The rendered image, and the inputs it was rendered from.
///
/// Keeping the inputs beside the output is what lets `sync` decide in a couple
/// of comparisons whether anything needs redoing.
struct Rendered {
    path: String,
    edits: Edits,
    /// The edited pixels, kept so Copy and Save export exactly what is shown
    /// rather than re-running the pipeline and hoping it agrees.
    rgba: Rgba,
}

pub struct PhotosApp {
    /// Canonical dispatcher shared by toolbar edits and exported document actions.
    actions: ui::ActionHost,
    /// Decoded original, before any edit. `None` until a file opens.
    original: Option<(String, Rgba)>,
    rendered: Option<Rendered>,
    viewer: Option<ImageViewer>,
    buttons: Vec<(Hit, Button)>,
    pending: Rc<Cell<Option<Hit>>>,
    /// Where the last export went, or why it failed. Shown in the status line
    /// so an action is never silent.
    notice: Option<String>,
    /// The status line, recomputed in `sync` — `draw` has no document.
    status: String,
    rect: Rect,
    toolbar_rect: Rect,
    canvas_rect: Rect,
    status_rect: Rect,
    /// Where each group rule landed, filled in by `apply_layout` so `draw`
    /// paints them without re-deriving the strip's geometry.
    separator_rects: Vec<Rect>,
    /// Presentation of each toolbar item at the width last laid out.
    faces: Vec<Face>,
    /// Portal channel while a file chooser is open.
    picker: Option<ui::portal::PortalChannel>,
}

impl Default for PhotosApp {
    fn default() -> Self {
        Self::new()
    }
}

impl PhotosApp {
    pub fn new() -> Self {
        let pending: Rc<Cell<Option<Hit>>> = Rc::new(Cell::new(None));
        let faces: Vec<Face> = TOOLBAR.iter().map(|t| t.3).collect();
        let mut buttons = Vec::new();
        for ((hit, key, icon, _), face) in TOOLBAR.iter().zip(faces.iter()) {
            buttons.push((*hit, build_button(*hit, key, *icon, *face, &pending)));
        }
        Self {
            actions: ui::ActionHost::new(),
            original: None,
            rendered: None,
            viewer: None,
            buttons,
            pending,
            notice: None,
            status: String::new(),
            rect: Rect::ZERO,
            toolbar_rect: Rect::ZERO,
            canvas_rect: Rect::ZERO,
            status_rect: Rect::ZERO,
            separator_rects: Vec::new(),
            faces,
            picker: None,
        }
    }

    /// Rebuild the strip for `width` if the room available has changed which
    /// items can afford their labels.
    ///
    /// Rebuilding rather than relabelling in place because a `Button` owns its
    /// label for its lifetime. The cost lands only on a resize that crosses
    /// the threshold, and it carries the enabled state across so a rebuild
    /// never re-enables a control the document says is unavailable.
    fn refit_toolbar(&mut self, width: f32) {
        let want = faces_for_width(width);
        if want == self.faces {
            return;
        }
        let enabled: Vec<bool> = self.buttons.iter().map(|(_, b)| b.is_enabled()).collect();
        self.buttons.clear();
        for (((hit, key, icon, _), face), was_enabled) in
            TOOLBAR.iter().zip(want.iter()).zip(enabled)
        {
            let mut button = build_button(*hit, key, *icon, *face, &self.pending);
            button.set_enabled(was_enabled);
            self.buttons.push((*hit, button));
        }
        self.faces = want;
    }

    /// Open `path` by writing it into `doc`. The bytes are read on the next
    /// `sync`, so a failure to decode surfaces the same way whether the file
    /// came from the chooser, the command line, or a peer.
    pub fn open_path(&mut self, doc: &mut AutoCommit, path: &str) {
        let bytes = std::fs::read(path).ok();
        match bytes.as_deref().and_then(pixels::decode) {
            Some(img) => {
                // Put the bytes in the content store before naming them, so
                // the hash this document is about to carry is one this machine
                // can already answer for — and one the room can be handed.
                // Otherwise the document would name a picture only the path
                // could find, and the path is true on one machine.
                let open = OpenImage::from_path(path, img.w, img.h)
                    .with_content(qdoc::blobstore::put(bytes.as_deref().unwrap_or_default()));
                self.original = Some((path.to_string(), img));
                self.rendered = None;
                self.viewer = None;
                PhotosDoc::set_image(doc, Some(&open));
                self.notice = None;
            }
            None => {
                self.notice = Some(tr("photos-open-failed"));
            }
        }
    }

    /// The path of the photograph this instance is showing, if any.
    ///
    /// Read off the decoded original rather than the document, because the two
    /// answer different questions and only this one is true of the *pixels on
    /// screen*: `refresh` reloads it from the document and clears it when the
    /// file will not decode, so a path here means there is something drawn to
    /// point at. A document naming a file this machine cannot read would
    /// otherwise publish a rect around an empty canvas.
    pub fn open_image_path(&self) -> Option<&str> {
        self.original.as_ref().map(|(p, _)| p.as_str())
    }

    /// The pixels currently shown, if any. This is what Copy and Save export.
    pub fn edited_pixels(&self) -> Option<&Rgba> {
        self.rendered.as_ref().map(|r| &r.rgba)
    }

    /// The rect the canvas was laid out into. The photograph is drawn inside
    /// this and nowhere else.
    pub fn canvas_rect(&self) -> Rect {
        self.canvas_rect
    }

    /// The toolbar's rect.
    pub fn toolbar_rect(&self) -> Rect {
        self.toolbar_rect
    }

    /// The status line's rect.
    pub fn status_rect(&self) -> Rect {
        self.status_rect
    }

    /// Every toolbar control as `(accessible name, rect)`, in strip order.
    ///
    /// Exposed so a test can assert that each control is actually within the
    /// window. `canvas_rect` staying inside the frame says nothing about the
    /// strip above it, which is how a fixed-width toolbar came to push Save
    /// off the right-hand edge of any window narrower than 912 px while every
    /// geometry assertion still passed.
    pub fn toolbar_item_rects(&self) -> Vec<(String, Rect)> {
        // Unreachable: `self.buttons` is always built one entry per `TOOLBAR`
        // item, so every index here has a matching `TOOLBAR` entry.
        self.buttons
            .iter()
            .enumerate()
            .filter_map(|(i, (_, b))| TOOLBAR.get(i).map(|item| (tr(item.1), b.get_rect())))
            .collect()
    }

    /// The status line as it currently reads.
    pub fn status(&self) -> &str {
        &self.status
    }

    /// Whether a photograph is loaded and rendered.
    pub fn has_image(&self) -> bool {
        self.viewer.is_some()
    }

    /// Zoom the canvas, as the scroll wheel would. Lets a caller frame a crop
    /// without simulating a chain of scroll events by hand.
    ///
    /// `ImageViewer` exposes no direct zoom setter to a widget that only
    /// depends on it (only a `#[cfg(test)]` one, private to that crate), so
    /// this drives the same `MouseWheel` path a real scroll takes: park the
    /// cursor at the viewer's own centre — where the widget's screen-to-image
    /// mapping is invariant under a zoom change, so pan stays exactly where it
    /// was — then send one wheel notch sized to land on `zoom` exactly.
    pub fn set_zoom(&mut self, zoom: f32) {
        let Some(v) = self.viewer.as_mut() else {
            return;
        };
        let rect = Widget::get_rect(v);
        if rect.width() <= 0.0 || rect.height() <= 0.0 || v.zoom() <= 0.0 {
            return;
        }
        let center =
            winit::dpi::PhysicalPosition::new((rect.x0 + rect.x1) * 0.5, (rect.y0 + rect.y1) * 0.5);
        // Solves `v.zoom() * ZOOM_STEP.powf(scroll_y) == zoom` for the notch
        // the widget's own wheel handler would need to see. `1.1` mirrors the
        // private `ZOOM_STEP` constant in `ui::widget::image_viewer` — there is
        // no public way to ask the widget for it.
        let scroll_y = (zoom / v.zoom()).ln() / 1.1_f32.ln();
        let mut doc = AutoCommit::new();
        let mut handled = false;
        let mut focus: Option<NodeId> = None;
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        // A fresh leaf on a fresh tree cannot fail; bail out rather than
        // panic if it somehow does.
        let Ok(node) = taffy.new_leaf(Style::default()) else {
            return;
        };
        // SAFETY: `DeviceId` has no public constructor; a zeroed value is
        // winit's own "no device" sentinel, and every field below is
        // overwritten or unused by `ImageViewer::handle_event`. Same idiom
        // `ui::widget::image_viewer`'s own tests use.
        let device_id = unsafe { std::mem::zeroed() };
        v.handle_event(
            &winit::event::WindowEvent::CursorMoved {
                device_id,
                position: center,
            },
            1.0,
            &winit::event::Modifiers::default(),
            &mut doc,
            &automerge::ROOT,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );
        v.handle_event(
            &winit::event::WindowEvent::MouseWheel {
                device_id,
                delta: winit::event::MouseScrollDelta::LineDelta(0.0, scroll_y),
                phase: winit::event::TouchPhase::Moved,
            },
            1.0,
            &winit::event::Modifiers::default(),
            &mut doc,
            &automerge::ROOT,
            &mut handled,
            &mut focus,
            node,
            &taffy,
        );
    }

    /// The visible region of the canvas in normalized image coordinates, or
    /// the whole image when the viewer has not been laid out yet.
    ///
    /// `ImageViewer` exposes no direct query for this either, so it is derived
    /// in [`visible_image_rect`] from the same "contain fit, then zoom and
    /// pan" transform `ImageViewer::draw` applies — using only the widget's
    /// already-public `zoom()` / `pan()` / rect, and this app's own knowledge
    /// of the original image's pixel size.
    pub fn visible_rect(&self) -> Rect01 {
        match (self.viewer.as_ref(), self.original.as_ref()) {
            (Some(v), Some((_, original))) => {
                visible_image_rect(v, f64::from(original.w), f64::from(original.h))
                    .unwrap_or(Rect01::FULL)
            }
            _ => Rect01::FULL,
        }
    }

    /// Copy what is shown to the system clipboard as raw RGBA.
    ///
    /// Raw rather than PNG because that is what the clipboard carries between
    /// sandboxes here — [`ui::clipboard_copy_image`] hands the buffer to the
    /// compositor's selection, and the receiving app gets pixels without
    /// either side agreeing on a container format.
    pub fn copy_to_clipboard(&mut self) -> bool {
        let Some(img) = self.edited_pixels() else {
            return false;
        };
        let ok = ui::clipboard_copy_image(img.w as usize, img.h as usize, &img.bytes);
        self.notice = Some(if ok {
            tr("photos-copied")
        } else {
            tr("photos-copy-failed")
        });
        ok
    }

    /// Write what is shown to `path` as a PNG.
    pub fn save_to(&mut self, path: &str) -> bool {
        let Some(bytes) = self.edited_pixels().and_then(pixels::encode_png) else {
            self.notice = Some(tr("photos-save-failed"));
            return false;
        };
        if let Ok(()) = std::fs::write(path, &bytes) {
            self.notice = Some(format!("{} {path}", tr("photos-saved")));
            true
        } else {
            self.notice = Some(tr("photos-save-failed"));
            false
        }
    }

    /// Default save destination: the source file with `-edited` before the
    /// extension, so Save never silently overwrites the original.
    pub fn suggested_save_path(open: &OpenImage) -> String {
        let p = std::path::Path::new(&open.path);
        let stem = p
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let parent = p
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_default();
        parent
            .join(format!("{stem}-edited.png"))
            .to_string_lossy()
            .into_owned()
    }

    /// Apply `next` to the document as an undoable edit.
    fn commit_edits(&mut self, doc: &mut AutoCommit, next: Edits) {
        self.actions.dispatch_with_context(
            doc,
            crate::actions::PHOTOS_ACTION_TYPE,
            &crate::actions::PhotosAction::set(&next),
            qos_actions::ActionContext::for_action(
                "org.qos.Photos",
                "Photos",
                &crate::actions::PhotosAction::set(&next),
            ),
        );
        self.notice = None;
    }

    fn perform_effect(&mut self, operation: crate::actions::PhotosOperation, doc: &AutoCommit) {
        match operation {
            crate::actions::PhotosOperation::OpenPicker => {
                self.picker = Some(
                    ui::portal::PickRequest::open(&tr("photos-open-title"), &tr("photos-open"))
                        .thread_name("photos-portal-open")
                        .filter(
                            ui::portal::FileFilter::new(&tr("photos-filter-images"))
                                .mimetype("image/png")
                                .mimetype("image/jpeg")
                                .mimetype("image/webp"),
                        )
                        .all_files()
                        .send(),
                );
            }
            crate::actions::PhotosOperation::Copy => {
                self.copy_to_clipboard();
            }
            crate::actions::PhotosOperation::SaveSuggested => {
                if let Some(open) = PhotosDoc::image(doc) {
                    self.save_to(&Self::suggested_save_path(&open));
                }
            }
        }
    }

    /// Re-derive the rendering if the path or the edits have moved.
    fn refresh(&mut self, doc: &AutoCommit) {
        let Some(open) = PhotosDoc::image(doc) else {
            self.rendered = None;
            self.viewer = None;
            self.original = None;
            return;
        };
        let edits = PhotosDoc::edits(doc);

        // Load the original if this is a picture we have not decoded yet. A
        // peer may have opened it, so this is not only the local Open path.
        if self.original.as_ref().map(|(p, _)| p.as_str()) != Some(open.path.as_str()) {
            if let Some(img) = crate::doc::bytes_of(&open)
                .as_deref()
                .and_then(pixels::decode)
            {
                self.original = Some((open.path.clone(), img));
            } else {
                self.original = None;
                self.rendered = None;
                self.viewer = None;
                return;
            }
        }
        let Some((_, original)) = self.original.as_ref() else {
            return;
        };

        let unchanged = self
            .rendered
            .as_ref()
            .is_some_and(|r| r.path == open.path && r.edits == edits);
        if unchanged {
            return;
        }

        let rgba = pixels::render(original, edits.crop, edits.rotate_quarters);
        let image = to_peniko(&rgba);
        // `ImageViewer` has no in-place image swap yet, so an edit rebuilds
        // the widget — which also resets zoom/pan to fit-to-viewport. For
        // "crop to view" that is the right outcome anyway: the new picture is
        // exactly what was framed, so showing it at zoom 1 is showing what was
        // asked for.
        self.viewer = Some(ImageViewer::new(image));
        self.rendered = Some(Rendered {
            path: open.path,
            edits,
            rgba,
        });
    }

    /// Drain a click that a button posted, and act on it.
    fn drain_pending(&mut self, doc: &mut AutoCommit) {
        let Some(hit) = self.pending.take() else {
            return;
        };
        let edits = PhotosDoc::edits(doc);
        match hit {
            Hit::Open => {
                self.perform_effect(crate::actions::PhotosOperation::OpenPicker, doc);
            }
            Hit::RotateLeft => self.commit_edits(doc, edits.rotated_left()),
            Hit::RotateRight => self.commit_edits(doc, edits.rotated_right()),
            Hit::CropToView => {
                let view = self.visible_rect();
                // Cropping to the whole image is a no-op; say so rather than
                // writing an edit that changes nothing.
                if view.is_full() {
                    self.notice = Some(tr("photos-crop-noop"));
                } else {
                    self.commit_edits(doc, edits.cropped_from_view(view));
                }
            }
            Hit::Reset => self.commit_edits(doc, Edits::default()),
            Hit::Copy => {
                self.perform_effect(crate::actions::PhotosOperation::Copy, doc);
            }
            Hit::Save => {
                self.perform_effect(crate::actions::PhotosOperation::SaveSuggested, doc);
            }
        }
    }

    /// Collect a chosen file from the portal, if one has arrived.
    fn drain_picker(&mut self, doc: &mut AutoCommit) {
        let Some(rx) = self.picker.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(ui::portal::PortalOutcome::Opened(path)) => {
                self.picker = None;
                let path = path.to_string_lossy().into_owned();
                self.open_path(doc, &path);
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            _ => self.picker = None,
        }
    }

    /// Which toolbar buttons are usable right now. Everything but Open needs
    /// an image; Reset additionally needs something to undo.
    fn enabled(hit: Hit, has_image: bool, edited: bool) -> bool {
        match hit {
            Hit::Open => true,
            Hit::Reset => has_image && edited,
            _ => has_image,
        }
    }

    /// The status line: the file, its edited size, and any notice.
    fn status_text(&self, doc: &AutoCommit) -> String {
        let Some(open) = PhotosDoc::image(doc) else {
            return String::new();
        };
        let edits = PhotosDoc::edits(doc);
        let (w, h) = open.edited_size(&edits);
        let mut s = format!("{}  ·  {w} × {h}", open.name);
        if !edits.is_identity() {
            s.push_str("  ·  ");
            s.push_str(&tr("photos-edited"));
        }
        if let Some(n) = &self.notice {
            s.push_str("  ·  ");
            s.push_str(n);
        }
        s
    }

    /// Lay out the toolbar strip: one taffy node per [`SLOTS`] entry (a
    /// button, a separator rule, or the greedy spacer), wrapped in the bar
    /// row itself. Split out of `build_layout` to keep that dispatch under
    /// the line-count limit.
    #[allow(
        clippy::unwrap_used,
        reason = "these taffy nodes are freshly built with fixed, valid styles"
    )]
    fn build_toolbar_node(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let mut bar_children = Vec::new();
        for slot in SLOTS {
            let node = match *slot {
                Slot::Item(n) => match self.buttons.get_mut(n) {
                    Some((_, button)) => button.build_layout(taffy, theme, node_map),
                    // Unreachable: SLOTS only ever indexes within self.buttons.
                    None => taffy.new_leaf(Style::default()).unwrap(),
                },
                Slot::Separator => taffy
                    .new_leaf(Style {
                        size: Size {
                            width: Dimension::length(SEP_W),
                            height: Dimension::length(BTN_H * 0.6),
                        },
                        flex_shrink: 0.0,
                        ..Default::default()
                    })
                    .unwrap(),
                // Soaks the residual width so the export group sits hard
                // right instead of leaving a third of the strip bare.
                Slot::Spacer => taffy
                    .new_leaf(Style {
                        size: Size {
                            width: Dimension::length(0.0),
                            height: Dimension::length(0.0),
                        },
                        flex_grow: 1.0,
                        flex_shrink: 1.0,
                        ..Default::default()
                    })
                    .unwrap(),
            };
            bar_children.push(node);
        }
        taffy
            .new_with_children(
                Style {
                    size: Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::length(BAR_H),
                    },
                    flex_shrink: 0.0,
                    align_items: Some(AlignItems::Center),
                    gap: Size {
                        width: LengthPercentage::length(BTN_GAP),
                        height: LengthPercentage::length(0.0),
                    },
                    padding: taffy::Rect {
                        left: LengthPercentage::length(PAD),
                        right: LengthPercentage::length(PAD),
                        top: LengthPercentage::length(0.0),
                        bottom: LengthPercentage::length(0.0),
                    },
                    ..Default::default()
                },
                &bar_children,
            )
            .unwrap()
    }

    /// Lay out the canvas: the `ImageViewer` if a photograph is open, else a
    /// placeholder leaf sized to soak the remaining height. `flex_basis: 0`
    /// with `min_size: 0` is what stops a large image's intrinsic size from
    /// pushing the layout wider than the window — the failure mode where
    /// content is silently clipped because the container grew past its
    /// parent instead of the content being scaled to fit. Split out of
    /// `build_layout` to keep that dispatch under the line-count limit.
    #[allow(
        clippy::unwrap_used,
        reason = "these taffy nodes are freshly built with fixed, valid styles"
    )]
    fn build_canvas_node(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let canvas = match self.viewer.as_mut() {
            Some(v) => v.build_layout(taffy, theme, node_map),
            None => taffy
                .new_leaf(Style {
                    size: Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::auto(),
                    },
                    flex_grow: 1.0,
                    ..Default::default()
                })
                .unwrap(),
        };
        taffy
            .set_style(
                canvas,
                Style {
                    size: Size {
                        width: Dimension::auto(),
                        height: Dimension::auto(),
                    },
                    min_size: Size {
                        width: Dimension::length(0.0),
                        height: Dimension::length(0.0),
                    },
                    flex_grow: 1.0,
                    flex_shrink: 1.0,
                    flex_basis: Dimension::length(0.0),
                    margin: taffy::Rect {
                        left: LengthPercentageAuto::length(0.0),
                        right: LengthPercentageAuto::length(0.0),
                        top: LengthPercentageAuto::length(CANVAS_INSET),
                        bottom: LengthPercentageAuto::length(CANVAS_INSET),
                    },
                    ..Default::default()
                },
            )
            .unwrap();
        canvas
    }

    /// Lay out the status band: zero height with nothing to say (reserving
    /// the band anyway would leave a rule and a strip of empty chrome under
    /// an empty state, which reads as a control that failed to draw), else a
    /// fixed band. Split out of `build_layout` to keep that dispatch under
    /// the line-count limit.
    #[allow(
        clippy::unwrap_used,
        reason = "this taffy node is freshly built with a fixed, valid style"
    )]
    fn build_status_node(&self, taffy: &mut TaffyTree<()>) -> NodeId {
        taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::length(if self.status.is_empty() {
                        0.0
                    } else {
                        BAR_H * 0.6
                    }),
                },
                flex_shrink: 0.0,
                ..Default::default()
            })
            .unwrap()
    }
}

/// The region of a `w`×`h` image `viewer` currently frames, in normalized
/// `0..1` coordinates — the inverse of the transform `ImageViewer::draw`
/// applies (contain-fit, then the widget's own zoom and pan). `None` when the
/// viewer or the image has a zero dimension.
fn visible_image_rect(viewer: &ImageViewer, w: f64, h: f64) -> Option<Rect01> {
    let rect = Widget::get_rect(viewer);
    let (rw, rh) = (rect.width(), rect.height());
    if rw <= 0.0 || rh <= 0.0 || w <= 0.0 || h <= 0.0 {
        return None;
    }
    let base = (rw / w).min(rh / h);
    let total_scale = base * f64::from(viewer.zoom());
    if total_scale <= 0.0 {
        return None;
    }
    let pan = viewer.pan();
    let (cx, cy) = ((rect.x0 + rect.x1) * 0.5, (rect.y0 + rect.y1) * 0.5);
    let (dw, dh) = (w * total_scale, h * total_scale);
    let dx = cx - dw * 0.5 + pan.x * total_scale;
    let dy = cy - dh * 0.5 + pan.y * total_scale;

    let x0 = ((rect.x0 - dx) / total_scale / w).clamp(0.0, 1.0);
    let x1 = ((rect.x1 - dx) / total_scale / w).clamp(0.0, 1.0);
    let y0 = ((rect.y0 - dy) / total_scale / h).clamp(0.0, 1.0);
    let y1 = ((rect.y1 - dy) / total_scale / h).clamp(0.0, 1.0);
    Some(Rect01 {
        x: x0,
        y: y0,
        w: (x1 - x0).max(0.0),
        h: (y1 - y0).max(0.0),
    })
}

/// Build one toolbar button in the presentation `face` asks for.
///
/// A glyph-only button keeps its words as the tooltip, which
/// [`ui::widget::Button`] also adopts as its accessible name — so dropping the
/// label costs the pointer user a hover and the screen-reader user nothing.
fn build_button(
    hit: Hit,
    key: &str,
    icon: Icon,
    face: Face,
    pending: &Rc<Cell<Option<Hit>>>,
) -> Button {
    let label = tr(key);
    let slot = pending.clone();
    let shown = match face {
        Face::Labelled => label.as_str(),
        Face::IconOnly => "",
    };
    Button::new(key, shown, None)
        .with_icon(icon)
        .with_size(item_width(face, &label), BTN_H)
        .tooltip(label.clone())
        .with_accessible_name(&label)
        .on_click(move || slot.set(Some(hit)))
}

/// Wrap an RGBA buffer as the image type the scene draws.
fn to_peniko(img: &Rgba) -> ImageData {
    ImageData {
        data: Blob::new(Arc::new(img.bytes.clone())),
        format: ImageFormat::Rgba8,
        alpha_type: ImageAlphaType::Alpha,
        width: img.w,
        height: img.h,
    }
}

impl Widget for PhotosApp {
    fn take_committed_action(&mut self) -> Option<Vec<automerge::ChangeHash>> {
        self.actions.take_committed()
    }

    fn invoke_resource_operation(
        &mut self,
        kind: qos_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        _path: &ObjId,
    ) -> Result<bool, String> {
        if kind == qos_actions::OperationKind::Document {
            return Ok(false);
        }
        let Some(operation) = crate::actions::decode_operation(type_id, payload)? else {
            return Ok(false);
        };
        self.perform_effect(operation, doc);
        Ok(true)
    }

    fn sync(&mut self, doc: &mut AutoCommit, _path: &ObjId) {
        PhotosDoc::seed(doc);
        self.drain_picker(doc);
        self.refresh(doc);

        let has_image = PhotosDoc::image(doc).is_some();
        let edited = !PhotosDoc::edits(doc).is_identity();
        for (hit, button) in &mut self.buttons {
            button.set_enabled(Self::enabled(*hit, has_image, edited));
        }
        self.status = self.status_text(doc);
    }

    #[allow(
        clippy::unwrap_used,
        reason = "root's taffy node always builds from freshly-built children"
    )]
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // Refit against the width the last layout produced. The first frame
        // has no rect yet and lays out roomy; the frame after corrects it.
        self.refit_toolbar(self.rect.width() as f32);

        let bar = self.build_toolbar_node(taffy, theme, node_map);
        let canvas = self.build_canvas_node(taffy, theme, node_map);
        let status = self.build_status_node(taffy);

        let root = taffy
            .new_with_children(
                Style {
                    flex_direction: FlexDirection::Column,
                    size: Size {
                        width: Dimension::percent(1.0),
                        height: Dimension::percent(1.0),
                    },
                    ..Default::default()
                },
                &[bar, canvas, status],
            )
            .unwrap();
        node_map.insert(root, std::ptr::from_mut::<dyn Widget>(self));
        root
    }

    #[allow(clippy::unwrap_used, reason = "node always belongs to this tree")]
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        let x = origin.x + f64::from(layout.location.x);
        let y = origin.y + f64::from(layout.location.y);
        self.rect = Rect::new(
            x,
            y,
            x + f64::from(layout.size.width),
            y + f64::from(layout.size.height),
        );
        let here = Point::new(x, y);

        let children = taffy.children(node).unwrap();
        let Some(&bar) = children.first() else { return };
        let bl = taffy.layout(bar).unwrap();
        self.toolbar_rect = Rect::new(
            x + f64::from(bl.location.x),
            y + f64::from(bl.location.y),
            x + f64::from(bl.location.x) + f64::from(bl.size.width),
            y + f64::from(bl.location.y) + f64::from(bl.size.height),
        );
        let bar_origin = Point::new(x + f64::from(bl.location.x), y + f64::from(bl.location.y));
        let bar_children = taffy.children(bar).unwrap();
        self.separator_rects.clear();
        for (slot, &bn) in SLOTS.iter().zip(bar_children.iter()) {
            match *slot {
                Slot::Item(n) => {
                    if let Some((_, button)) = self.buttons.get_mut(n) {
                        button.apply_layout(taffy, bn, bar_origin);
                    }
                }
                Slot::Separator => {
                    let sl = taffy.layout(bn).unwrap();
                    let sx =
                        bar_origin.x + f64::from(sl.location.x) + f64::from(sl.size.width) / 2.0;
                    let sy = bar_origin.y + f64::from(sl.location.y);
                    self.separator_rects.push(Rect::new(
                        sx,
                        sy,
                        sx + 1.0,
                        sy + f64::from(sl.size.height),
                    ));
                }
                Slot::Spacer => {}
            }
        }

        if let Some(&canvas) = children.get(1) {
            let cl = taffy.layout(canvas).unwrap();
            self.canvas_rect = Rect::new(
                x + f64::from(cl.location.x),
                y + f64::from(cl.location.y),
                x + f64::from(cl.location.x) + f64::from(cl.size.width),
                y + f64::from(cl.location.y) + f64::from(cl.size.height),
            );
            if let Some(v) = self.viewer.as_mut() {
                v.apply_layout(taffy, canvas, here);
            }
        }
        if let Some(&status) = children.get(2) {
            let sl = taffy.layout(status).unwrap();
            self.status_rect = Rect::new(
                x + f64::from(sl.location.x),
                y + f64::from(sl.location.y),
                x + f64::from(sl.location.x) + f64::from(sl.size.width),
                y + f64::from(sl.location.y) + f64::from(sl.size.height),
            );
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        use vello::peniko::Fill;

        // App background. Every colour on this path comes from the theme.
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.base,
            None,
            &self.rect,
        );
        // The toolbar sits on the raised surface so it reads as chrome
        // against the canvas below it.
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.raised,
            None,
            &self.toolbar_rect,
        );
        // …and is separated from the canvas by a real rule, not only by that
        // fill. Under the high-contrast palette `surfaces.raised` and
        // `surfaces.base` are both pure black by design — that theme
        // deliberately carries structure in borders rather than in fills — so
        // a toolbar defined only by its plate has no edge at all there. The
        // same rule above the status line, for the same reason.
        // The status line's rule is drawn only when the line has something in
        // it; an empty band would otherwise be fenced off as if it did.
        let mut edges = vec![self.toolbar_rect.y1];
        if !self.status.is_empty() {
            edges.push(self.status_rect.y0);
        }
        for edge in edges {
            scene.fill(
                Fill::NonZero,
                transform,
                theme.border_standard,
                None,
                &Rect::new(self.rect.x0, edge - 1.0, self.rect.x1, edge),
            );
        }

        // Group rules: what opens a file, what adjusts it, what exports it.
        for sep in &self.separator_rects {
            scene.fill(Fill::NonZero, transform, theme.border_standard, None, sep);
        }

        for (_, button) in &mut self.buttons {
            button.draw(scene, transform, text_renderer, theme, presence);
        }

        match self.viewer.as_mut() {
            Some(v) => v.draw(scene, transform, text_renderer, theme, presence),
            None => {
                EmptyState::paint(
                    scene,
                    transform,
                    text_renderer,
                    theme,
                    self.canvas_rect,
                    Some(Icon::Image),
                    &tr("photos-empty-title"),
                    Some(&tr("photos-empty-body")),
                    None,
                    Point::ZERO,
                );
            }
        }

        if !self.status.is_empty() {
            let inner = self.status_rect;
            text_renderer.draw_text_weighted(
                scene,
                &self.status,
                13.0,
                theme.text_primary,
                transform * Affine::translate((inner.x0 + f64::from(PAD), inner.y0 + 4.0)),
                Some((inner.width() - 2.0 * f64::from(PAD)) as f32),
                TextWeight::Regular,
            );
        }
    }

    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        scale_factor: f64,
        modifiers: &winit::event::Modifiers,
        doc: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        focus_target: &mut Option<NodeId>,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) {
        let children = taffy.children(node_id).unwrap_or_default();
        if let Some(&bar) = children.first() {
            let bar_children = taffy.children(bar).unwrap_or_default();
            for (slot, &bn) in SLOTS.iter().zip(bar_children.iter()) {
                let Slot::Item(n) = *slot else { continue };
                let Some((_, button)) = self.buttons.get_mut(n) else {
                    continue;
                };
                button.handle_event(
                    event,
                    scale_factor,
                    modifiers,
                    doc,
                    path,
                    handled,
                    focus_target,
                    bn,
                    taffy,
                );
            }
        }
        if let (Some(v), Some(&canvas)) = (self.viewer.as_mut(), children.get(1)) {
            v.handle_event(
                event,
                scale_factor,
                modifiers,
                doc,
                path,
                handled,
                focus_target,
                canvas,
                taffy,
            );
        }
        self.drain_pending(doc);
    }

    fn name(&self) -> &'static str {
        "PhotosApp"
    }

    /// Hand the concrete app back out of a `Box<dyn Widget>`.
    ///
    /// A compositor hosting this in a tab holds it as a trait object, so
    /// "open this file in the photo viewer that is already on screen" has no
    /// route in without it. Opening still goes through
    /// [`PhotosApp::open_path`] — this only recovers the type, it does not add
    /// a second way to change what is shown.
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn accessibility(&self) -> Option<AccessNode> {
        use ui::a11y::Role;
        Some(AccessNode::new(Role::Group))
    }

    /// The open photograph and the edits it supports, published to whatever is
    /// hosting this widget.
    ///
    /// The same tree [`crate::actions::PhotosResources`] serves to a packaged
    /// instance over the socket, so an app compiled into the compositor and one
    /// running as its own process offer the identical surface at the identical
    /// addresses. Without this the tree existed but only a caller already
    /// inside this process could reach it.
    fn resources(&self, doc: &AutoCommit) -> Vec<qos_actions::Resource> {
        use qos_actions::ResourceProvider;
        crate::actions::PhotosResources.resources(doc)
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

/// Lets the compositor host Photos in a tab directly, the same way it hosts
/// the terminal, the file explorer and the word processor, rather than
/// standing a placeholder card where the photograph should be.
impl ui::widget::AppWidget for PhotosApp {
    fn app_kind(&self) -> ui::widget::AppKind {
        // The toolkit's tab vocabulary has no picture variant. Photos edits a
        // file and writes the result back, which is what `Editor` names; the
        // glyph below is what actually tells the two apart in a tab pill.
        ui::widget::AppKind::Editor
    }

    fn app_title(&self) -> &str {
        // The open file's own name, so two photographs open at once are two
        // distinguishable tabs rather than two tabs called "Photos". Nothing
        // open falls back to the app's name, which is the only true thing to
        // say about an empty window.
        self.open_image_path()
            .and_then(|p| p.rsplit('/').next())
            .filter(|n| !n.is_empty())
            .unwrap_or("Photos")
    }

    fn app_icon(&self) -> ui::widget::IconDescriptor {
        // Nerd Font image glyph, matching the convention every other hosted
        // app's tab pill uses.
        ui::widget::IconDescriptor::Glyph("\u{F02E9}")
    }

    fn permission_hint(&self) -> Option<ui::widget::AppKind> {
        Some(ui::widget::AppKind::Editor)
    }
}
