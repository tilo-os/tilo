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
use ui::widget::context_menu::{ContextMenu, MenuItem};
use ui::widget::{Button, EmptyState, ImageViewer, RemarkCompose, Widget};

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
const TOOLBAR: &[(Hit, &str, Icon, Face)] = &[
    (Hit::Open, "photos-open", Icon::Folder, Face::Labelled),
    (Hit::RotateLeft, "photos-rotate-left", Icon::RotateCcw, Face::IconOnly),
    (Hit::RotateRight, "photos-rotate-right", Icon::RotateCw, Face::IconOnly),
    (Hit::CropToView, "photos-crop", Icon::Crop, Face::IconOnly),
    (Hit::Reset, "photos-reset", Icon::Refresh, Face::IconOnly),
    (Hit::Copy, "photos-copy", Icon::Copy, Face::Labelled),
    (Hit::Save, "photos-save", Icon::Save, Face::Labelled),
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
            Slot::Item(n) => item_width(faces[n], &tr(TOOLBAR[n].1)),
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
    /// Where the pointer last was, in logical pixels. Tracked because a press
    /// carries no position of its own.
    pointer: Point,
    /// The right-click menu. The shared [`ContextMenu`], painted in the
    /// overlay pass so it floats above the canvas.
    context_menu: ContextMenu,
    /// Per-row intent parallel to `context_menu`'s items (separators map to
    /// `None`). A row posts the same [`Hit`] a toolbar button does, so the two
    /// routes cannot drift into doing different things.
    context_actions: Vec<Option<Hit>>,
    /// "Say something about this photograph" — the shared compose surface,
    /// kept as a field rather than built per-frame so a half-written remark
    /// survives a redraw.
    remark: RemarkCompose,
    /// Where a remark about this photograph can go. Filled by the host that
    /// knows the room; empty until it is, so the app never invents a roster.
    remark_destinations: Vec<ui::widget::DestinationOption>,
    /// A finished remark waiting to be taken by the host.
    remark_outbox: Option<qos_reference::Remark>,
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
            pointer: Point::ZERO,
            context_menu: ContextMenu::new(),
            context_actions: Vec::new(),
            remark: RemarkCompose::new(),
            remark_destinations: Vec::new(),
            remark_outbox: None,
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
            TOOLBAR.iter().zip(want.iter()).zip(enabled.into_iter())
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
                    .with_content(ui::blobstore::put(bytes.as_deref().unwrap_or_default()));
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
        self.buttons
            .iter()
            .enumerate()
            .map(|(i, (_, b))| (tr(TOOLBAR[i].1), b.get_rect()))
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
    /// without synthesising wheel events.
    pub fn set_zoom(&mut self, zoom: f32) {
        if let Some(v) = self.viewer.as_mut() {
            v.set_zoom(zoom);
        }
    }

    /// The visible region of the canvas in normalized image coordinates, or
    /// the whole image when the viewer has not been laid out yet.
    pub fn visible_rect(&self) -> Rect01 {
        match self.viewer.as_ref().and_then(|v| v.visible_image_rect()) {
            Some((x, y, w, h)) => Rect01 { x, y, w, h },
            None => Rect01::FULL,
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
        match std::fs::write(path, &bytes) {
            Ok(()) => {
                self.notice = Some(format!("{} {path}", tr("photos-saved")));
                true
            }
            Err(_) => {
                self.notice = Some(tr("photos-save-failed"));
                false
            }
        }
    }

    /// Default save destination: the source file with `-edited` before the
    /// extension, so Save never silently overwrites the original.
    pub fn suggested_save_path(open: &OpenImage) -> String {
        let p = std::path::Path::new(&open.path);
        let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let parent = p.parent().map(|s| s.to_path_buf()).unwrap_or_default();
        parent.join(format!("{stem}-edited.png")).to_string_lossy().into_owned()
    }

    /// Apply `next` to the document as an undoable edit.
    fn commit_edits(&mut self, doc: &mut AutoCommit, next: Edits) {
        PhotosDoc::set_edits(doc, &next);
        self.notice = None;
    }

    /// Re-derive the rendering if the path or the edits have moved.
    /// The bytes of the picture `open` names, from wherever this machine has
    /// them.
    ///
    /// The path first: when this session is the one that opened the picture,
    /// the path is its own and reading it is a file read rather than a lookup.
    /// Then the content, which is the half that works for everybody else. The
    /// document is shared and the path in it is not — the same photograph is
    /// staged under a different absolute path in every session that receives
    /// it, so every participant except the one whose path won is holding a
    /// path into somebody else's runtime directory. Resolving the hash is what
    /// turns "a photograph a collaborator opened" into pixels here rather than
    /// into an empty canvas.
    ///
    /// Missing both is not an error to report: `blobstore::get` asks the room
    /// for the content on its way through, so the honest answer now is no
    /// picture, and the frame after the bytes land draws one.
    fn bytes_of(open: &OpenImage) -> Option<Vec<u8>> {
        if let Ok(bytes) = std::fs::read(&open.path) {
            return Some(bytes);
        }
        ui::blobstore::get(open.content.as_deref()?)
    }

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
            match Self::bytes_of(&open).as_deref().and_then(pixels::decode) {
                Some(img) => self.original = Some((open.path.clone(), img)),
                None => {
                    self.original = None;
                    self.rendered = None;
                    self.viewer = None;
                    return;
                }
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
        match self.viewer.as_mut() {
            Some(v) => v.set_image(image),
            None => self.viewer = Some(ImageViewer::new(image).with_test_id("photos-canvas")),
        }
        self.rendered = Some(Rendered {
            path: open.path.clone(),
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
                self.copy_to_clipboard();
            }
            Hit::Save => {
                if let Some(open) = PhotosDoc::image(doc) {
                    let path = Self::suggested_save_path(&open);
                    self.save_to(&path);
                }
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
            Ok(_) => self.picker = None,
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(_) => self.picker = None,
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

    // -- say something about this photograph -------------------------------

    /// Where a remark about this photograph can be sent. Supplied by the host,
    /// which is what knows the room; empty until it is, so the app never
    /// invents a roster of its own.
    pub fn set_remark_destinations(&mut self, destinations: Vec<ui::widget::DestinationOption>) {
        self.remark_destinations = destinations;
    }

    /// Take a finished remark, once. The host dispatches it — resolving any
    /// `@mention` against the roster it holds and this app does not.
    pub fn take_remark(&mut self) -> Option<qos_reference::Remark> {
        self.remark_outbox.take()
    }

    /// Whether the compose surface is up.
    pub fn remark_is_open(&self) -> bool {
        self.remark.is_open()
    }

    /// What an open compose is about — the address, not the words on the chip.
    pub fn remark_reference(&self) -> Option<&qos_reference::ResourceRef> {
        self.remark.reference()
    }

    /// The destination the selector is showing, and therefore the one a remark
    /// sent now would go to.
    pub fn remark_destination(&self) -> Option<&qos_reference::Destination> {
        self.remark.destination()
    }

    /// "Say something about this" without a pointer (`Ctrl+Shift+M`).
    ///
    /// The same surface the right-click raises, over the same photograph and
    /// minting the same address. Anchored on the picture itself rather than on
    /// a menu, because a chord has no cursor to sit beside — and the picture is
    /// what the panel has to stay clear of.
    pub fn open_remark_for_selection(&mut self, doc: &AutoCommit) -> bool {
        let subject = self.photograph_rect();
        // A zero-width rect at the photograph's trailing edge: "beside the
        // picture", which in a contain-fit canvas is the letterbox band.
        let neighbour = Rect::new(subject.x1, subject.y0, subject.x1, subject.y0);
        self.open_remark_beside(doc, neighbour)
    }

    /// Where the photograph is painted, or the canvas when none is open.
    ///
    /// This is the subject the compose surface is placed around, so it is read
    /// from the viewer's own geometry rather than assumed to be the canvas: a
    /// contain-fit photograph leaves letterbox bands, and those are exactly
    /// where a floating panel belongs.
    pub fn photograph_rect(&self) -> Rect {
        self.viewer
            .as_ref()
            .and_then(|v| v.image_screen_rect())
            .unwrap_or(self.canvas_rect)
    }

    /// Right-click at `at`: the ordinary context menu, and the compose surface
    /// beside it. Drives the production path, for the headless shots demo.
    #[doc(hidden)]
    pub fn debug_right_click(&mut self, doc: &AutoCommit, at: Point) {
        self.pointer = at;
        self.open_context_menu(doc, at);
    }

    /// The menu's labels while it is open, so a test can hold that the menu is
    /// still doing its own job beside the compose.
    pub fn context_menu_labels(&self) -> Vec<String> {
        self.context_menu
            .items()
            .iter()
            .filter_map(|i| i.label().map(|s| s.to_string()))
            .collect()
    }

    /// A point that lands on the menu row reading `label`, found through the
    /// menu's own hit-test rather than by re-deriving its row geometry — so a
    /// caller clicking here clicks where the row actually is.
    pub fn context_menu_point_for(&self, label: &str) -> Option<Point> {
        let want = self
            .context_menu
            .items()
            .iter()
            .position(|i| i.label() == Some(label))?;
        let r = self.context_menu.rect();
        let x = r.x0 + r.width() * 0.5;
        let mut y = r.y0;
        while y < r.y1 {
            if self.context_menu.item_at([x, y]) == Some(want) {
                return Some(Point::new(x, y));
            }
            y += 1.0;
        }
        None
    }

    /// The compose panel's laid-out rect while it is open. Exposed so a test
    /// can ask where the surface went — whether it covers the photograph, and
    /// whether it stayed on the window — rather than reading pixels back.
    pub fn remark_rect(&self) -> Rect {
        self.remark.get_rect()
    }

    /// Open the right-click menu on the photograph, and the compose surface
    /// beside it.
    ///
    /// The menu offers what can be *done* to the picture — the same operations
    /// the toolbar and the action door publish — and the compose is a second
    /// thing to do with it, not a replacement. It declines silently when the
    /// host has named no destinations, so a session with no room behind it
    /// right-clicks exactly as it always did.
    fn open_context_menu(&mut self, doc: &AutoCommit, at: Point) {
        let edited = !PhotosDoc::edits(doc).is_identity();
        let mut menu = ContextMenu::new();
        let mut actions: Vec<Option<Hit>> = Vec::new();
        let row = |menu: &mut ContextMenu, actions: &mut Vec<Option<Hit>>, hit: Hit, key: &str| {
            menu.push(MenuItem::action(tr(key), || {}));
            actions.push(Some(hit));
        };
        row(&mut menu, &mut actions, Hit::RotateLeft, "photos-rotate-left");
        row(&mut menu, &mut actions, Hit::RotateRight, "photos-rotate-right");
        row(&mut menu, &mut actions, Hit::CropToView, "photos-crop");
        // Only when there is something to discard, so the menu never offers an
        // operation that would do nothing.
        if edited {
            row(&mut menu, &mut actions, Hit::Reset, "photos-reset");
        }
        menu.push(MenuItem::separator());
        actions.push(None);
        row(&mut menu, &mut actions, Hit::Copy, "photos-copy");
        row(&mut menu, &mut actions, Hit::Save, "photos-save");

        menu.open_at([at.x as f32, at.y as f32]);
        layout_menu_widget(&mut menu);
        self.context_menu = menu;
        self.context_actions = actions;

        self.open_remark_beside(doc, self.context_menu.rect());
    }

    /// Run the menu row at `idx`, then close the menu. Separators and
    /// out-of-range rows are ignored. Returns whether a row ran.
    fn dispatch_context_action(&mut self, idx: usize, doc: &mut AutoCommit) -> bool {
        let Some(Some(hit)) = self.context_actions.get(idx).copied() else {
            self.context_menu.close();
            return false;
        };
        // Posted, not performed here: `drain_pending` is the one place a
        // decision becomes an edit, so a menu row and a toolbar button cannot
        // come to mean different things.
        self.pending.set(Some(hit));
        self.drain_pending(doc);
        self.context_menu.close();
        true
    }

    /// Open the compose surface beside `neighbour`.
    ///
    /// Returns `false`, having opened nothing, when there is nowhere to send or
    /// nothing open: a compose box over no subject would ask for a sentence
    /// about nothing.
    fn open_remark_beside(&mut self, doc: &AutoCommit, neighbour: Rect) -> bool {
        if self.remark_destinations.is_empty() {
            return false;
        }
        let Some(reference) = <Self as qos_reference::Referent>::selection_reference(self, doc)
        else {
            return false;
        };
        self.remark.open_for(
            reference,
            self.remark_destinations.clone(),
            0,
            Point::new(
                neighbour.x1 + ui::widget::remark_compose::BESIDE_GAP,
                neighbour.y0,
            ),
            &ui::widget::remark_compose::placeholder_text(),
            &ui::widget::remark_compose::send_label(),
        );
        // Measure, then place: the panel's size is only known once it has been
        // laid out, and its size is what decides where it fits.
        self.layout_remark();
        self.place_remark_beside(neighbour);
        self.layout_remark();
        true
    }

    /// Put the panel beside the menu, preferring a side that leaves the
    /// photograph clear.
    ///
    /// The word processor's rule is right-of-menu, then below, then left,
    /// because a text selection runs *back* from the click and the menu's left
    /// is the one place the panel must not cover. A photograph has no such
    /// direction — its subject is a rectangle in the middle of the canvas — so
    /// the letter of that rule does not transfer, but the reason does: keep the
    /// subject visible, because this gesture is about looking. Somebody judges a
    /// photograph and says something to it, and a panel parked over the picture
    /// defeats that as surely as one over a selected sentence would.
    ///
    /// So: the familiar order stands, and is departed from only when departing
    /// actually buys the whole picture — when another placement clears the
    /// photograph completely and the first does not. A contain-fit picture in a
    /// window of a different shape leaves a letterbox band, and that is the case
    /// worth moving for. Deciding instead by whichever placement happens to
    /// cover a few per cent less would make the panel jump sides on a click a
    /// finger's width further along, which is its own kind of unusable.
    fn place_remark_beside(&mut self, neighbour: Rect) {
        use ui::widget::remark_compose::{BESIDE_GAP, EDGE_MARGIN};

        let panel = self.remark.get_rect();
        let (w, h) = (panel.width(), panel.height());
        let bounds = self.rect;
        let subject = self.photograph_rect();

        // Top-aligned with the neighbour, lifted only as far as staying on
        // screen needs, so the panel still reads as belonging to the click.
        let level = neighbour
            .y0
            .min(bounds.y1 - EDGE_MARGIN - h)
            .max(bounds.y0 + EDGE_MARGIN);
        let clamped_x = neighbour
            .x0
            .min(bounds.x1 - EDGE_MARGIN - w)
            .max(bounds.x0 + EDGE_MARGIN);

        let candidates = [
            Point::new(neighbour.x1 + BESIDE_GAP, level),
            Point::new(clamped_x, neighbour.y1 + BESIDE_GAP),
            Point::new(neighbour.x0 - BESIDE_GAP - w, level),
        ];
        let fits = |at: Point| {
            at.x >= bounds.x0 + EDGE_MARGIN
                && at.y >= bounds.y0 + EDGE_MARGIN
                && at.x + w <= bounds.x1 - EDGE_MARGIN
                && at.y + h <= bounds.y1 - EDGE_MARGIN
        };
        let clears = |at: Point| {
            overlap_area(Rect::new(at.x, at.y, at.x + w, at.y + h), subject) == 0.0
        };

        let mut usable = candidates.into_iter().filter(|&at| fits(at));
        let at = match usable.next() {
            Some(first) => {
                if clears(first) {
                    first
                } else {
                    // Only a placement that shows the whole photograph is worth
                    // leaving the familiar order for.
                    usable.find(|&at| clears(at)).unwrap_or(first)
                }
            }
            // Nothing fits: clamp rather than let the panel run off the window,
            // which is the one place a floating surface stops being usable at
            // all.
            None => Point::new(clamped_x, level),
        };
        self.remark.move_to(at);
    }
    /// Resolve the compose panel's rects in a throwaway taffy tree: it is
    /// anchored in window coordinates rather than being a node of the app's
    /// tree. Deliberately the same pass for painting and for hit-testing, so a
    /// control can never be drawn somewhere other than where it is clickable.
    fn remark_layout_pass(&mut self) -> (TaffyTree<()>, NodeId) {
        // The panel's own metrics come from its constants and spacing tokens do
        // not vary by palette, so one theme here keeps draw and hit-test
        // measuring with the same ruler under every theme.
        let theme = Theme::dark();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut nm: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = self.remark.build_layout(&mut taffy, &theme, &mut nm);
        let _ = taffy.compute_layout(
            node,
            taffy::Size {
                width: taffy::AvailableSpace::Definite(2000.0),
                height: taffy::AvailableSpace::Definite(1400.0),
            },
        );
        self.remark.apply_layout(&taffy, node, Point::ZERO);
        (taffy, node)
    }

    fn layout_remark(&mut self) {
        if self.remark.is_open() {
            let _ = self.remark_layout_pass();
        }
    }

    /// While the compose surface is open, route input to it.
    ///
    /// The panel owns the keyboard — it is a field that just appeared under the
    /// hands, and a keystroke meant for the message must never land in the app
    /// instead. The pointer stays shared: a press inside the panel is the
    /// panel's, a press on the menu beside it still works that menu, and a
    /// press anywhere else puts the compose away the way clicking off any
    /// composer does.
    ///
    /// Consuming the press inside the panel is also what keeps a drag in the
    /// message field from panning the photograph underneath it: the canvas
    /// fills the window, so every surface floating over it is over the viewer
    /// too. Returns whether the event was consumed.
    fn pump_remark(
        &mut self,
        event: &winit::event::WindowEvent,
        scale: f64,
        mods: &winit::event::Modifiers,
        doc: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
    ) -> bool {
        use winit::event::{ElementState, MouseButton, WindowEvent};

        if !self.remark.is_open() {
            return false;
        }
        match event {
            WindowEvent::CursorMoved { .. } => {
                // Hover feedback only. A move is never reported as consumed, or
                // the rest of the app stops tracking the pointer while the
                // panel is up — and the send button needs the hover it carries.
                let mut hover = false;
                self.forward_to_remark(event, scale, mods, doc, path, &mut hover);
                false
            }
            WindowEvent::KeyboardInput { .. } => {
                self.forward_to_remark(event, scale, mods, doc, path, handled);
                *handled = true;
                true
            }
            WindowEvent::MouseInput {
                button: MouseButton::Left,
                state: press,
                ..
            } => {
                // The expanded destination list paints its rows outside the
                // panel's own rect, so while it is down every press is the
                // panel's — including the one that dismisses it.
                let mine = self.remark.get_rect().contains(self.pointer)
                    || self.remark.destination_list_is_open();
                if mine {
                    self.forward_to_remark(event, scale, mods, doc, path, handled);
                    *handled = true;
                    return true;
                }
                if *press == ElementState::Pressed
                    && !self.context_menu.contains([self.pointer.x, self.pointer.y])
                {
                    self.remark.close();
                }
                false
            }
            // A wheel over the panel would otherwise zoom the canvas beneath
            // it, moving the photograph out from under the sentence being
            // written about it.
            WindowEvent::MouseWheel { .. } if self.remark.get_rect().contains(self.pointer) => {
                *handled = true;
                true
            }
            _ => false,
        }
    }

    /// Hand one event to the open panel and drain anything it finished.
    fn forward_to_remark(
        &mut self,
        event: &winit::event::WindowEvent,
        scale: f64,
        mods: &winit::event::Modifiers,
        doc: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
    ) {
        let (taffy, node) = self.remark_layout_pass();
        let mut ft: Option<NodeId> = None;
        self.remark
            .handle_event(event, scale, mods, doc, path, handled, &mut ft, node, &taffy);
        if let Some(remark) = self.remark.take_sent() {
            // Say where it went. A send whose only evidence is that the panel
            // vanished is indistinguishable from a send that failed.
            self.notice = Some(crate::strings::tr_args(
                "photos-remark-sent",
                &[("to", remark.destination.display().name.to_string())],
            ));
            self.remark_outbox = Some(remark);
        }
    }
}

/// Area two rects share, or zero when they do not meet.
fn overlap_area(a: Rect, b: Rect) -> f64 {
    let i = a.intersect(b);
    if i.width() > 0.0 && i.height() > 0.0 {
        i.width() * i.height()
    } else {
        0.0
    }
}

/// Resolve a standalone [`ContextMenu`]'s rect from its anchor: it is not part
/// of the app's taffy tree, so drive a throwaway layout pass.
fn layout_menu_widget(menu: &mut ContextMenu) {
    if !menu.is_open() {
        return;
    }
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut nm: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let theme = Theme::dark();
    let node = menu.build_layout(&mut taffy, &theme, &mut nm);
    let _ = taffy.compute_layout(
        node,
        taffy::Size {
            width: taffy::AvailableSpace::Definite(400.0),
            height: taffy::AvailableSpace::Definite(800.0),
        },
    );
    menu.apply_layout(&taffy, node, Point::ZERO);
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

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // Refit against the width the last layout produced. The first frame
        // has no rect yet and lays out roomy; the frame after corrects it.
        self.refit_toolbar(self.rect.width() as f32);

        let mut bar_children = Vec::new();
        for slot in SLOTS {
            let node = match *slot {
                Slot::Item(n) => {
                    let (_, button) = &mut self.buttons[n];
                    button.build_layout(taffy, theme, node_map)
                }
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
        let bar = taffy
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
            .unwrap();

        // The canvas takes every pixel the two bars do not. `flex_basis: 0`
        // with `min_size: 0` is what stops a large image's intrinsic size from
        // pushing the layout wider than the window — the failure mode where
        // content is silently clipped because the container grew past its
        // parent instead of the content being scaled to fit.
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

        // A status line with nothing to say takes no room. Reserving the band
        // anyway leaves a rule and a strip of empty chrome under an empty
        // state, which reads as a control that failed to draw.
        let status = taffy
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
            .unwrap();

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
        node_map.insert(root, self as *mut dyn Widget);
        root
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        let x = origin.x + layout.location.x as f64;
        let y = origin.y + layout.location.y as f64;
        self.rect = Rect::new(x, y, x + layout.size.width as f64, y + layout.size.height as f64);
        let here = Point::new(x, y);

        let children = taffy.children(node).unwrap();
        let Some(&bar) = children.first() else { return };
        let bl = taffy.layout(bar).unwrap();
        self.toolbar_rect = Rect::new(
            x + bl.location.x as f64,
            y + bl.location.y as f64,
            x + bl.location.x as f64 + bl.size.width as f64,
            y + bl.location.y as f64 + bl.size.height as f64,
        );
        let bar_origin = Point::new(x + bl.location.x as f64, y + bl.location.y as f64);
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
                    let sx = bar_origin.x + sl.location.x as f64 + sl.size.width as f64 / 2.0;
                    let sy = bar_origin.y + sl.location.y as f64;
                    self.separator_rects
                        .push(Rect::new(sx, sy, sx + 1.0, sy + sl.size.height as f64));
                }
                Slot::Spacer => {}
            }
        }

        if let Some(&canvas) = children.get(1) {
            let cl = taffy.layout(canvas).unwrap();
            self.canvas_rect = Rect::new(
                x + cl.location.x as f64,
                y + cl.location.y as f64,
                x + cl.location.x as f64 + cl.size.width as f64,
                y + cl.location.y as f64 + cl.size.height as f64,
            );
            if let Some(v) = self.viewer.as_mut() {
                v.apply_layout(taffy, canvas, here);
            }
        }
        if let Some(&status) = children.get(2) {
            let sl = taffy.layout(status).unwrap();
            self.status_rect = Rect::new(
                x + sl.location.x as f64,
                y + sl.location.y as f64,
                x + sl.location.x as f64 + sl.size.width as f64,
                y + sl.location.y as f64 + sl.size.height as f64,
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
                transform * Affine::translate((inner.x0 + PAD as f64, inner.y0 + 4.0)),
                Some((inner.width() - 2.0 * PAD as f64) as f32),
                TextWeight::Regular,
            );
        }
    }

    /// The floating tier: the right-click menu and the compose surface beside
    /// it, painted after everything else so nothing drawn later occludes them.
    fn draw_overlay(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        if self.context_menu.is_open() {
            layout_menu_widget(&mut self.context_menu);
            self.context_menu
                .draw(scene, transform, text_renderer, theme, presence);
        }
        if self.remark.is_open() {
            self.layout_remark();
            self.remark
                .draw(scene, transform, text_renderer, theme, presence);
            // The expanded destination list composites above the panel itself.
            self.remark
                .draw_overlay(scene, transform, text_renderer, theme, presence);
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
        use winit::event::{ElementState, MouseButton, WindowEvent};
        use winit::keyboard::{Key, NamedKey};

        // The pointer position arrives on moves and nowhere else; a press
        // carries none, so it is tracked before anything hit-tests with it.
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.pointer = Point::new(position.x / scale_factor, position.y / scale_factor);
        }

        // The compose surface, while it is up, owns the keyboard and its own
        // patch of the screen: a keystroke meant for the message must not reach
        // the app, and a press inside the panel must not reach the canvas.
        if self.remark.is_open()
            && self.pump_remark(event, scale_factor, modifiers, doc, path, handled)
        {
            return;
        }
        // ...and the chord that raises it, so the gesture is not mouse-only. An
        // address is exactly what somebody working without a pointer cannot
        // produce by describing what they mean.
        if let WindowEvent::KeyboardInput { event: key, .. } = event {
            if key.state.is_pressed()
                && ui::widget::remark_compose::is_compose_chord(key, modifiers)
                && self.open_remark_for_selection(doc)
            {
                *handled = true;
                return;
            }
            if key.state.is_pressed()
                && matches!(key.logical_key, Key::Named(NamedKey::Escape))
                && self.context_menu.is_open()
            {
                self.context_menu.close();
                *handled = true;
                return;
            }
        }

        match event {
            // A secondary press on the photograph: the ordinary menu, and the
            // compose surface beside it. Nowhere else — a right-click on the
            // toolbar belongs to the toolbar, and one over an empty canvas has
            // no subject to be about.
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } => {
                if self.viewer.is_some() && self.canvas_rect.contains(self.pointer) {
                    self.open_context_menu(doc, self.pointer);
                    *handled = true;
                }
                return;
            }
            // While the menu is open, route the primary press to it: a row runs
            // its operation, a press elsewhere dismisses it. Either way the
            // press is consumed, so it never reaches the canvas beneath — which
            // would otherwise begin a pan under the menu.
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if self.context_menu.is_open() => {
                match self.context_menu.item_at([self.pointer.x, self.pointer.y]) {
                    Some(idx) => {
                        self.dispatch_context_action(idx, doc);
                    }
                    None => self.context_menu.close(),
                }
                *handled = true;
                return;
            }
            _ => {}
        }

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

    fn name(&self) -> &str {
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


    /// Take the roster the host supplies as remark destinations.
    ///
    /// Delegates to the inherent method so the compose surface has a
    /// single way to be told where a remark can go.
    fn set_remark_destinations(
        &mut self,
        destinations: Vec<ui::widget::remark_compose::DestinationOption>,
    ) {
        Self::set_remark_destinations(self, destinations);
    }

    /// Hand the finished remark to the host, which addresses and sends it.
    ///
    /// Delegates to the inherent method for the same reason
    /// `set_remark_destinations` does: one way in, one way out.
    fn take_remark(&mut self) -> Option<qos_reference::Remark> {
        Self::take_remark(self)
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

    /// Where the published photograph actually is on screen.
    ///
    /// The photograph's own rect, not the canvas's: a contain-fit image leaves
    /// letterbox bands, and a mark drawn round the canvas would be drawn round
    /// those too — claiming a participant touched a region of empty background.
    /// `photograph_rect` is the viewer's answer to where the pixels ended up,
    /// which is the same rect the compose surface is already placed against.
    ///
    /// Keyed by the id [`crate::actions::resource_id`] mints from the path, so
    /// the address a caller invokes — `photos.image:/…/harbour.png#crop` — is
    /// the address that resolves here. Nothing open publishes nothing, exactly
    /// as the resource tree does.
    fn resource_rects(&self) -> Vec<(String, Rect)> {
        let Some(path) = self.open_image_path() else {
            return Vec::new();
        };
        vec![(crate::actions::resource_id(path), self.photograph_rect())]
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

/// Lets the compositor host Photos in a tab directly, the same way it hosts
/// the terminal, the file explorer and the word processor, rather than
/// standing a placeholder card where the photograph should be.
///
/// The point is not convenience. An action addressed at
/// `photos.image:/…/harbour.png` can only be *shown* landing if something on
/// this desktop is drawing that photograph; a placeholder tile publishes no
/// resource rects, so the address would resolve to no region and the mark
/// would have nowhere to go.
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
