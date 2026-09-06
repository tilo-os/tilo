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
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;

use automerge::{AutoCommit, ObjId};
use taffy::prelude::*;
use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use vello::peniko::{Blob, ImageAlphaType, ImageData, ImageFormat};

use ui::a11y::{AccessNode, AccessState, Action as AccessAction, Politeness, Role};
use ui::icons::Icon;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{Button, EmptyState, ImageViewer, TextInput, Widget};
use winit::keyboard::{Key, NamedKey};

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
    Library,
    RotateLeft,
    RotateRight,
    CropToView,
    Reset,
    Copy,
    Save,
}

/// Controls inside the in-grid album editor. They are intentionally a small
/// local presentation state machine; every durable outcome is still one
/// `PhotosAction::SaveAlbum` action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AlbumEditorHit {
    New,
    Edit,
    Save,
    Cancel,
    Delete,
    ConfirmDelete,
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

/// What the canvas can honestly render right now. A shared document can name
/// an image before this device has received its content, so "no pixels yet"
/// is distinct from both an empty library and corrupt local bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum CanvasState {
    #[default]
    Empty,
    Loading,
    Error,
    Ready,
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
    (Hit::Library, "photos-library", Icon::Image, Face::Labelled),
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
    Slot::Item(1),
    Slot::Separator,
    Slot::Item(2),
    Slot::Item(3),
    Slot::Item(4),
    Slot::Item(5),
    Slot::Spacer,
    Slot::Item(6),
    Slot::Item(7),
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

#[allow(
    clippy::struct_excessive_bools,
    reason = "library mode and the three independent transient album-editor flags have distinct event and reconciliation lifecycles"
)]
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
    resource_invocation: Option<qos_actions::InvocationId>,
    resource_result: Option<qos_actions::InvocationUpdate>,
    pending_resource_open: Option<(qos_actions::InvocationId, String)>,
    canvas_state: CanvasState,
    search_provider: crate::search::PhotosSearchProvider,
    /// Heads that produced the cached immutable/Arc-backed search catalogue.
    /// An unchanged `sync` must not rebuild a full-library index.
    search_heads: Vec<automerge::ChangeHash>,
    /// The library is deliberately a bounded presentation slice.  The CRDT
    /// can hold considerably more entries than a window can paint or an
    /// assistive technology should receive in one update.
    library_items: Vec<(String, String, bool)>,
    library_total: usize,
    library_offset: usize,
    library_albums: Vec<(String, String, usize)>,
    active_album: Option<String>,
    album_cards: Vec<(Option<String>, Rect)>,
    album_editor: bool,
    album_editor_id: Option<String>,
    album_editor_creating: bool,
    album_name_input: TextInput,
    album_editor_members: HashSet<String>,
    album_delete_confirm: bool,
    /// Tab order inside the transient sheet: name, cancel, delete (when
    /// editing), save. The `TextInput` remains the actual focused widget.
    album_editor_focus: usize,
    album_editor_rect: Rect,
    album_editor_hits: Vec<(AlbumEditorHit, Rect)>,
    /// At most two viewportfuls of decoded thumbnails.  Originals are never
    /// scanned at draw time, and a large library cannot turn a frame into a
    /// disk-I/O loop.
    library_thumbnails: HashMap<String, ImageData>,
    /// Stable-id hit targets for the currently materialised grid cards.
    library_cards: Vec<(String, Rect, bool)>,
    pointer: Point,
    library_mode: bool,
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
            resource_invocation: None,
            resource_result: None,
            pending_resource_open: None,
            canvas_state: CanvasState::Empty,
            search_provider: crate::search::PhotosSearchProvider::empty(),
            search_heads: Vec::new(),
            library_items: Vec::new(),
            library_total: 0,
            library_offset: 0,
            library_albums: Vec::new(),
            active_album: None,
            album_cards: Vec::new(),
            album_editor: false,
            album_editor_id: None,
            album_editor_creating: false,
            album_name_input: TextInput::local().with_id("photos-album-name"),
            album_editor_members: HashSet::new(),
            album_delete_confirm: false,
            album_editor_focus: 0,
            album_editor_rect: Rect::ZERO,
            album_editor_hits: Vec::new(),
            library_thumbnails: HashMap::new(),
            library_cards: Vec::new(),
            pointer: Point::ZERO,
            library_mode: false,
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

    /// Reveal the bounded library grid.  Kept public for the deterministic
    /// offscreen gallery; ordinary interaction reaches it through the same
    /// toolbar button and semantic action as a user.
    pub fn show_library(&mut self) {
        self.library_mode = true;
    }

    /// Pose the ordinary library editor for deterministic offscreen coverage.
    /// The production entry point is the visible “Edit album” control; shot
    /// rendering has no window event stream to click it through.
    pub fn show_album_editor_for_shot(&mut self, doc: &AutoCommit) {
        self.show_library();
        self.active_album = PhotosDoc::library(doc)
            .albums
            .first()
            .map(|album| album.id.clone());
        self.begin_edit_album(doc);
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

    /// Every card interaction routes through the same typed, replayable
    /// action registry as resources and search results.  The canvas never
    /// writes document fields directly.
    fn commit_library_action(
        &mut self,
        doc: &mut AutoCommit,
        action: &crate::actions::PhotosAction,
    ) {
        self.actions.dispatch_with_context(
            doc,
            crate::actions::PHOTOS_ACTION_TYPE,
            action,
            qos_actions::ActionContext::for_action("org.qos.Photos", "Photos", &action),
        );
    }

    fn begin_new_album(&mut self, doc: &AutoCommit) {
        let library = PhotosDoc::library(doc);
        let mut suffix = 1_u32;
        let id = loop {
            let candidate = format!("photos.album:untitled-{suffix}");
            if library.album(&candidate).is_none() {
                break candidate;
            }
            suffix += 1;
        };
        self.album_editor = true;
        self.album_editor_creating = true;
        self.album_editor_id = Some(id);
        self.album_name_input.set_text("Untitled album");
        self.album_name_input.set_focus(true);
        self.album_editor_members.clear();
        self.album_delete_confirm = false;
        self.album_editor_focus = 0;
        self.notice = Some("Name the album, choose photographs, then save".to_owned());
    }

    fn begin_edit_album(&mut self, doc: &AutoCommit) {
        let Some(id) = self.active_album.clone() else {
            self.notice = Some("Choose an album to edit".to_owned());
            return;
        };
        let Some(album) = PhotosDoc::library(doc).album(&id).cloned() else {
            self.notice = Some("That album is no longer available".to_owned());
            self.active_album = None;
            return;
        };
        self.album_editor = true;
        self.album_editor_creating = false;
        self.album_editor_id = Some(album.id);
        self.album_name_input.set_text(&album.name);
        self.album_name_input.set_focus(true);
        self.album_editor_members = album.photo_ids.into_iter().collect();
        self.album_delete_confirm = false;
        self.album_editor_focus = 0;
    }

    fn cancel_album_editor(&mut self) {
        self.album_editor = false;
        self.album_editor_id = None;
        self.album_editor_members.clear();
        self.album_delete_confirm = false;
        self.album_name_input.set_focus(false);
        self.album_editor_focus = 0;
        self.notice = Some("Album changes discarded".to_owned());
    }

    fn save_album_editor(&mut self, doc: &mut AutoCommit) {
        let Some(id) = self.album_editor_id.clone() else {
            return;
        };
        let name = self.album_name_input.value().trim().to_owned();
        let library = PhotosDoc::library(doc);
        let stale = if self.album_editor_creating {
            library.album(&id).is_some()
        } else {
            library.album(&id).is_none()
        };
        if stale
            || name.is_empty()
            || !self
                .album_editor_members
                .iter()
                .all(|photo| library.photo(photo).is_some())
        {
            self.notice = Some(if stale {
                "Album changed elsewhere; review and save again".to_owned()
            } else {
                "An album needs a name and photographs still in this library".to_owned()
            });
            return;
        }
        let action = crate::actions::PhotosAction::SaveAlbum {
            id: id.clone(),
            name,
            // Preserve the library's stable browse order. HashSet iteration
            // would make two otherwise identical UI edits serialize to
            // arbitrary membership order and create needless collaboration
            // conflicts.
            photo_ids: library
                .photos
                .iter()
                .filter(|photo| self.album_editor_members.contains(&photo.id))
                .map(|photo| photo.id.clone())
                .collect(),
            create: self.album_editor_creating,
        };
        self.commit_library_action(doc, &action);
        self.active_album = Some(id);
        self.album_editor = false;
        self.album_delete_confirm = false;
        self.album_name_input.set_focus(false);
        self.album_editor_focus = 0;
        self.notice = Some("Album saved for everyone in this workspace".to_owned());
    }

    fn delete_active_album(&mut self, doc: &mut AutoCommit) {
        let Some(id) = self
            .album_editor_id
            .clone()
            .or_else(|| self.active_album.clone())
        else {
            return;
        };
        self.commit_library_action(doc, &crate::actions::PhotosAction::RemoveAlbum { id });
        self.active_album = None;
        self.album_editor = false;
        self.album_editor_id = None;
        self.album_delete_confirm = false;
        self.album_editor_focus = 0;
        self.notice = Some("Album deleted; photographs remain in the library".to_owned());
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "embedded TextInput uses the Widget event contract"
    )]
    fn handle_album_editor_event(
        &mut self,
        event: &winit::event::WindowEvent,
        scale_factor: f64,
        modifiers: winit::event::Modifiers,
        doc: &mut AutoCommit,
        path: &ObjId,
        handled: &mut bool,
        node_id: NodeId,
        taffy: &TaffyTree<()>,
    ) -> bool {
        if !self.album_editor {
            return false;
        }
        let mut sink = None;
        self.album_name_input.handle_event(
            event,
            scale_factor,
            &modifiers,
            doc,
            path,
            handled,
            &mut sink,
            node_id,
            taffy,
        );
        if let winit::event::WindowEvent::KeyboardInput { event: key, .. } = event
            && key.state == winit::event::ElementState::Pressed
        {
            match &key.logical_key {
                Key::Named(NamedKey::Tab) => {
                    let steps = if self.album_editor_creating { 3 } else { 4 };
                    self.album_editor_focus = if modifiers.state().shift_key() {
                        (self.album_editor_focus + steps - 1) % steps
                    } else {
                        (self.album_editor_focus + 1) % steps
                    };
                    self.album_name_input
                        .set_focus(self.album_editor_focus == 0);
                    *handled = true;
                    return true;
                }
                Key::Named(NamedKey::Escape) => {
                    self.cancel_album_editor();
                    *handled = true;
                    return true;
                }
                Key::Named(NamedKey::Enter)
                    if modifiers.state().control_key() || modifiers.state().super_key() =>
                {
                    self.save_album_editor(doc);
                    *handled = true;
                    return true;
                }
                Key::Named(NamedKey::Enter) => {
                    match (self.album_editor_creating, self.album_editor_focus) {
                        (_, 1) => self.cancel_album_editor(),
                        (false, 2) if self.album_delete_confirm => self.delete_active_album(doc),
                        (false, 2) => self.album_delete_confirm = true,
                        (true, 2) | (false, 3) => self.save_album_editor(doc),
                        _ => {}
                    }
                    *handled = true;
                    return true;
                }
                _ => {}
            }
        }
        if matches!(
            event,
            winit::event::WindowEvent::MouseInput {
                state: winit::event::ElementState::Released,
                button: winit::event::MouseButton::Left,
                ..
            }
        ) {
            if let Some((hit, _)) = self
                .album_editor_hits
                .iter()
                .find(|(_, rect)| rect.contains(self.pointer))
                .copied()
            {
                match hit {
                    AlbumEditorHit::Save => self.save_album_editor(doc),
                    AlbumEditorHit::Cancel => self.cancel_album_editor(),
                    AlbumEditorHit::Delete => {
                        self.album_delete_confirm = true;
                        self.notice = Some("Press Delete album? again to keep photographs and delete only the album".to_owned());
                    }
                    AlbumEditorHit::ConfirmDelete => self.delete_active_album(doc),
                    _ => {}
                }
                *handled = true;
                return true;
            }
            if let Some((id, _, _)) = self
                .library_cards
                .iter()
                .find(|(_, rect, _)| rect.contains(self.pointer))
                .cloned()
            {
                if !self.album_editor_members.insert(id.clone()) {
                    self.album_editor_members.remove(&id);
                }
                self.album_delete_confirm = false;
                *handled = true;
                return true;
            }
        }
        true
    }

    fn perform_effect(&mut self, operation: crate::actions::PhotosOperation, doc: &AutoCommit) {
        match operation {
            // Resource/search routing has already brought this Photos
            // instance forward. Viewing does not alter document or OS state.
            crate::actions::PhotosOperation::Open { path: None } => {
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
            crate::actions::PhotosOperation::Save { path } => {
                if let Some(open) = PhotosDoc::image(doc) {
                    self.save_to(path.as_deref().unwrap_or(&Self::suggested_save_path(&open)));
                }
            }
            crate::actions::PhotosOperation::View
            | crate::actions::PhotosOperation::ViewAlbum { .. }
            | crate::actions::PhotosOperation::Open { path: Some(_) } => {}
        }
    }

    /// Navigate to an extant stable album id without producing a CRDT action.
    /// This is deliberately separate from `perform_effect`: callers need a
    /// useful stale-address error instead of silently falling back to all
    /// photographs if a collaborator removed the album after search indexed
    /// it.
    fn view_album(&mut self, doc: &AutoCommit, album_id: &str) -> Result<(), String> {
        if PhotosDoc::library(doc).album(album_id).is_none() {
            return Err("that album is no longer in the library".to_owned());
        }
        self.library_mode = true;
        self.active_album = Some(album_id.to_owned());
        self.library_offset = 0;
        self.album_editor = false;
        self.album_delete_confirm = false;
        self.notice = Some("Showing selected album".to_owned());
        Ok(())
    }

    /// Re-derive the rendering if the path or the edits have moved.
    fn refresh(&mut self, doc: &AutoCommit) {
        let Some(open) = PhotosDoc::image(doc) else {
            self.rendered = None;
            self.viewer = None;
            self.original = None;
            self.canvas_state = if self.notice.is_some() {
                CanvasState::Error
            } else {
                CanvasState::Empty
            };
            return;
        };
        let edits = PhotosDoc::edits(doc);

        // Load the original if this is a picture we have not decoded yet. A
        // peer may have opened it, so this is not only the local Open path.
        if self.original.as_ref().map(|(p, _)| p.as_str()) != Some(open.path.as_str()) {
            let bytes = crate::doc::bytes_of(&open);
            if let Some(img) = bytes.as_deref().and_then(pixels::decode) {
                self.original = Some((open.path.clone(), img));
            } else {
                self.original = None;
                self.rendered = None;
                self.viewer = None;
                self.canvas_state = if bytes.is_some() {
                    CanvasState::Error
                } else {
                    CanvasState::Loading
                };
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
        self.canvas_state = CanvasState::Ready;
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
                self.perform_effect(crate::actions::PhotosOperation::Open { path: None }, doc);
            }
            Hit::Library => {
                self.library_mode = true;
                self.notice = None;
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
                self.perform_effect(crate::actions::PhotosOperation::Save { path: None }, doc);
            }
        }
    }

    /// Synthetic semantics are necessary because Photos composes its toolbar
    /// directly into the root layout instead of exposing it through
    /// [`Widget::children`]. Keep the target based on the stable toolbar key,
    /// not its visual index: the narrow layout changes labels, but an assistive
    /// technology holding "Rotate right" must keep reaching that action.
    fn toolbar_semantics(&self) -> AccessNode {
        let children = self
            .buttons
            .iter()
            .zip(TOOLBAR)
            .map(|((_, button), (_, key, _, _))| {
                AccessNode::new(Role::Button)
                    .with_name(tr(key))
                    .with_state(AccessState {
                        focusable: button.is_focusable(),
                        focused: button.is_focused(),
                        disabled: !button.is_enabled(),
                        ..Default::default()
                    })
                    .with_bounds(button.get_rect())
                    .with_action_target(format!("photos:toolbar:{key}"))
            })
            .collect();
        AccessNode::new(Role::Toolbar)
            .with_name(tr("photos-tools"))
            .with_bounds(self.toolbar_rect)
            .with_hashed_key("photos:toolbar")
            .with_children(children)
    }

    /// The canvas has no intrinsic source name — it is raw pixel data — so the
    /// app supplies the file name and current edit state that a sighted person
    /// gets from the status bar. The status remains a polite live region for
    /// copy/save/open failures without stealing keyboard focus.
    #[allow(
        clippy::too_many_lines,
        reason = "the synthetic canvas accessibility tree keeps each visual hit target beside its stable semantic route"
    )]
    fn accessibility_tree(&self) -> AccessNode {
        let image_label = || {
            self.original.as_ref().map_or_else(
                || tr("photos-image"),
                |(path, _)| path.rsplit('/').next().unwrap_or(path).to_owned(),
            )
        };
        let (image_name, image_description) = match self.rendered.as_ref() {
            Some(rendered) if rendered.edits.is_identity() => {
                (image_label(), tr("photos-image-help"))
            }
            Some(_) => (
                format!("{} — {}", image_label(), tr("photos-edited")),
                tr("photos-image-help"),
            ),
            None => self.empty_state_text(),
        };
        let canvas = if self.library_mode {
            let mut cards: Vec<_> = self
                .library_cards
                .iter()
                .filter_map(|(id, bounds, favorite)| {
                    let (_, name, _) = self
                        .library_items
                        .iter()
                        .find(|(item_id, _, _)| item_id == id)?;
                    let open_target = format!("photos:library-photo:{id}");
                    let favorite_target = format!("photos:library-favorite:{id}");
                    let children = vec![
                        if self.album_editor {
                            AccessNode::new(Role::ToggleButton)
                                .with_name(name.clone())
                                .with_description("Included in this album")
                                .with_bounds(*bounds)
                                .with_state(AccessState {
                                    focusable: true,
                                    checked: Some(self.album_editor_members.contains(id)),
                                    ..Default::default()
                                })
                                .with_action_target(open_target)
                        } else {
                            AccessNode::new(Role::Button)
                                .with_name(name.clone())
                                .with_description(tr("photos-open"))
                                .with_bounds(*bounds)
                                .with_state(AccessState {
                                    focusable: true,
                                    ..Default::default()
                                })
                                .with_action_target(open_target)
                        },
                        AccessNode::new(Role::ToggleButton)
                            .with_name("Favorite")
                            .with_bounds(*bounds)
                            .with_state(AccessState {
                                focusable: true,
                                checked: Some(*favorite),
                                ..Default::default()
                            })
                            .with_action_target(favorite_target),
                    ];
                    Some(
                        AccessNode::new(Role::ListItem)
                            .with_name(name.clone())
                            .with_bounds(*bounds)
                            .with_hashed_key(format!("photos:library-item:{id}"))
                            .with_children(children),
                    )
                })
                .collect();
            cards.extend(self.album_cards.iter().map(|(id, bounds)| {
                let label = id
                    .as_ref()
                    .and_then(|id| {
                        self.library_albums
                            .iter()
                            .find(|(album_id, _, _)| album_id == id)
                    })
                    .map_or_else(
                        || "All photos".to_owned(),
                        |(_, name, count)| format!("{name} ({count})"),
                    );
                let target = format!("photos:library-album:{}", id.as_deref().unwrap_or("all"));
                AccessNode::new(Role::ToggleButton)
                    .with_name(label)
                    .with_bounds(*bounds)
                    .with_state(AccessState {
                        focusable: true,
                        checked: Some(id.as_ref() == self.active_album.as_ref()),
                        ..Default::default()
                    })
                    .with_action_target(target)
            }));
            if self.album_editor {
                cards.push(
                    AccessNode::new(Role::TextInput)
                        .with_name("Album name")
                        .with_value(self.album_name_input.value())
                        .with_bounds(self.album_name_input.get_rect())
                        .with_state(AccessState {
                            focusable: true,
                            focused: self.album_name_input.is_focused(),
                            ..Default::default()
                        })
                        .with_hashed_key("photos:album-editor:name"),
                );
                for (hit, bounds) in &self.album_editor_hits {
                    let (name, target) = match hit {
                        AlbumEditorHit::Save => ("Save album", "photos:album-editor:save"),
                        AlbumEditorHit::Cancel => {
                            ("Cancel album editing", "photos:album-editor:cancel")
                        }
                        AlbumEditorHit::Delete => ("Delete album", "photos:album-editor:delete"),
                        AlbumEditorHit::ConfirmDelete => (
                            "Confirm deleting album",
                            "photos:album-editor:confirm-delete",
                        ),
                        AlbumEditorHit::New | AlbumEditorHit::Edit => continue,
                    };
                    cards.push(
                        AccessNode::new(Role::Button)
                            .with_name(name)
                            .with_bounds(*bounds)
                            .with_state(AccessState {
                                focusable: true,
                                ..Default::default()
                            })
                            .with_action_target(target),
                    );
                }
            } else {
                for (hit, bounds) in &self.album_editor_hits {
                    let (name, target) = match hit {
                        AlbumEditorHit::New => ("New album", "photos:album-editor:new"),
                        AlbumEditorHit::Edit => ("Edit selected album", "photos:album-editor:edit"),
                        _ => continue,
                    };
                    cards.push(
                        AccessNode::new(Role::Button)
                            .with_name(name)
                            .with_bounds(*bounds)
                            .with_state(AccessState {
                                focusable: true,
                                ..Default::default()
                            })
                            .with_action_target(target),
                    );
                }
            }
            AccessNode::new(Role::List)
                .with_name(tr("photos-library"))
                .with_description(format!("{} photographs", self.library_total))
                .with_bounds(self.canvas_rect)
                .with_hashed_key("photos:library-grid")
                .with_children(cards)
        } else {
            AccessNode::new(Role::Image)
                .with_name(image_name)
                .with_description(image_description)
                .with_bounds(self.canvas_rect)
                .with_hashed_key("photos:canvas")
        };
        let mut children = vec![self.toolbar_semantics(), canvas];
        if !self.status.is_empty() {
            children.push(
                AccessNode::new(Role::StaticText)
                    .with_name(self.status.clone())
                    .with_live(Politeness::Polite)
                    .with_bounds(self.status_rect)
                    .with_hashed_key("photos:status"),
            );
        }
        AccessNode::new(Role::Group)
            .with_name(tr("photos-app"))
            .with_bounds(self.rect)
            .with_semantic_key("photos")
            .with_children(children)
    }

    /// Dispatch a toolbar action requested by assistive technology through the
    /// identical `drain_pending` path used by pointer and keyboard activation.
    fn perform_toolbar_semantic_action(
        &mut self,
        target: &str,
        action: &AccessAction,
        doc: &mut AutoCommit,
    ) -> bool {
        let Some(key) = target.strip_prefix("photos:toolbar:") else {
            return false;
        };
        let Some((index, (hit, _, _, _))) = TOOLBAR
            .iter()
            .enumerate()
            .find(|(_, (_, toolbar_key, _, _))| *toolbar_key == key)
        else {
            return false;
        };
        match action {
            AccessAction::Focus => {
                for (_, button) in &mut self.buttons {
                    button.set_focus(false);
                }
                if let Some((_, button)) = self.buttons.get_mut(index) {
                    button.set_focus(true);
                }
                true
            }
            AccessAction::Activate
                if self.buttons.get(index).is_some_and(|(_, b)| b.is_enabled()) =>
            {
                self.pending.set(Some(*hit));
                self.drain_pending(doc);
                true
            }
            _ => false,
        }
    }

    fn perform_library_semantic_action(
        &mut self,
        target: &str,
        action: &AccessAction,
        doc: &mut AutoCommit,
    ) -> bool {
        if let Some(command) = target.strip_prefix("photos:album-editor:") {
            if !matches!(action, AccessAction::Activate) {
                return matches!(action, AccessAction::Focus);
            }
            match command {
                "new" => self.begin_new_album(doc),
                "edit" => self.begin_edit_album(doc),
                "save" => self.save_album_editor(doc),
                "cancel" => self.cancel_album_editor(),
                "delete" => self.album_delete_confirm = true,
                "confirm-delete" => self.delete_active_album(doc),
                _ => return false,
            }
            return true;
        }
        if let Some(album_id) = target.strip_prefix("photos:library-album:") {
            if !matches!(action, AccessAction::Activate | AccessAction::Focus) {
                return false;
            }
            self.active_album = (album_id != "all").then(|| album_id.to_owned());
            self.library_offset = 0;
            return true;
        }
        let (photo_id, set_favorite) =
            if let Some(id) = target.strip_prefix("photos:library-photo:") {
                (id, false)
            } else if let Some(id) = target.strip_prefix("photos:library-favorite:") {
                (id, true)
            } else {
                return false;
            };
        if !matches!(action, AccessAction::Activate) {
            return matches!(action, AccessAction::Focus);
        }
        let Some((_, _, favorite)) = self.library_items.iter().find(|(id, _, _)| id == photo_id)
        else {
            return false;
        };
        if set_favorite {
            self.commit_library_action(
                doc,
                &crate::actions::PhotosAction::SetFavorite {
                    photo_id: photo_id.to_owned(),
                    favorite: !favorite,
                },
            );
        } else if self.album_editor {
            if !self.album_editor_members.insert(photo_id.to_owned()) {
                self.album_editor_members.remove(photo_id);
            }
        } else {
            self.commit_library_action(
                doc,
                &crate::actions::PhotosAction::SelectPhoto {
                    photo_id: photo_id.to_owned(),
                },
            );
            self.library_mode = false;
        }
        true
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
                if let Some(invocation) = self.resource_invocation.take() {
                    self.resource_result = Some(if self.open_image_path().is_some() {
                        qos_actions::InvocationUpdate::Succeeded {
                            invocation,
                            output: qos_actions::ActionOutput::default(),
                        }
                    } else {
                        qos_actions::InvocationUpdate::Failed {
                            invocation,
                            error: qos_actions::ActionFailure {
                                code: "photos.open_failed".into(),
                                message: tr("photos-open-failed"),
                                details: None,
                                retryable: false,
                            },
                        }
                    });
                }
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Ok(ui::portal::PortalOutcome::Cancelled) => {
                self.picker = None;
                if let Some(invocation) = self.resource_invocation.take() {
                    self.resource_result = Some(qos_actions::InvocationUpdate::Cancelled {
                        invocation,
                        message: Some("photo selection was cancelled".into()),
                    });
                }
            }
            Ok(ui::portal::PortalOutcome::Failed(message)) => {
                self.picker = None;
                if let Some(invocation) = self.resource_invocation.take() {
                    self.resource_result = Some(qos_actions::InvocationUpdate::Failed {
                        invocation,
                        error: qos_actions::ActionFailure {
                            code: "photos.portal_failed".into(),
                            message,
                            details: None,
                            retryable: false,
                        },
                    });
                }
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => self.picker = None,
        }
    }

    /// Which toolbar buttons are usable right now. Everything but Open needs
    /// an image; Reset additionally needs something to undo.
    fn enabled(hit: Hit, has_image: bool, edited: bool) -> bool {
        match hit {
            Hit::Open | Hit::Library => true,
            Hit::Reset => has_image && edited,
            _ => has_image,
        }
    }

    /// The status line: the file, its edited size, and any notice.
    fn status_text(&self, doc: &AutoCommit) -> String {
        let Some(open) = PhotosDoc::image(doc) else {
            return self.notice.clone().unwrap_or_default();
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

    fn empty_state_text(&self) -> (String, String) {
        match self.canvas_state {
            CanvasState::Loading => (tr("photos-loading-title"), tr("photos-loading-body")),
            CanvasState::Error => (tr("photos-error-title"), tr("photos-error-body")),
            CanvasState::Empty | CanvasState::Ready => {
                (tr("photos-empty-title"), tr("photos-empty-body"))
            }
        }
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

    #[allow(
        clippy::too_many_lines,
        reason = "the grid paint keeps its pixel geometry and bounded materialisation together"
    )]
    /// Render the visible part of the library; the remainder stays reachable
    /// through cursor-paged search and canonical resources.
    fn draw_library(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        use vello::peniko::Fill;

        self.library_cards.clear();
        self.album_cards.clear();
        if self.library_items.is_empty() {
            EmptyState::paint(
                scene,
                transform,
                text_renderer,
                theme,
                self.canvas_rect,
                Some(Icon::Image),
                &tr("photos-library-empty-title"),
                Some(&tr("photos-library-empty-body")),
                None,
                Point::ZERO,
            );
            // Empty collections are useful before the first import too; keep
            // the same New album affordance available rather than stranding
            // the user behind an empty-state dead end.
            self.draw_album_editor(scene, transform, text_renderer, theme, presence);
            return;
        }
        let bounds = self.canvas_rect;
        let columns: usize = if bounds.width() < 540.0 { 2 } else { 4 };
        let gap = if columns == 2 { 8.0 } else { 12.0 };
        let card_w =
            ((bounds.width() - f64::from(PAD * 2.0) - gap * columns.saturating_sub(1) as f64)
                / columns as f64)
                .max(1.0);
        let album_h = if self.library_albums.is_empty() {
            0.0
        } else {
            38.0
        };
        let heading_y = bounds.y0 + f64::from(PAD);
        text_renderer.draw_text_weighted(
            scene,
            &tr("photos-library"),
            18.0,
            theme.text_primary,
            transform * Affine::translate((bounds.x0 + f64::from(PAD), heading_y)),
            Some((bounds.width() - f64::from(PAD * 2.0)) as f32),
            TextWeight::Medium,
        );
        let count = if self.library_total == 0 {
            "0".to_owned()
        } else {
            let first = self.library_offset + 1;
            let last = (self.library_offset + self.library_items.len()).min(self.library_total);
            format!("{first}–{last} / {}", self.library_total)
        };
        text_renderer.draw_text_weighted(
            scene,
            &count,
            13.0,
            theme.text_dim,
            transform * Affine::translate((bounds.x1 - f64::from(PAD) - 100.0, heading_y + 3.0)),
            Some(100.0),
            TextWeight::Regular,
        );
        if !self.library_albums.is_empty() {
            let mut x = bounds.x0 + f64::from(PAD);
            let y = heading_y + 24.0;
            let mut albums = vec![(None, format!("All photos ({})", self.library_total))];
            albums.extend(
                self.library_albums
                    .iter()
                    .map(|(id, name, count)| (Some(id.clone()), format!("{name} ({count})"))),
            );
            for (album_id, label) in albums {
                let width = (label.chars().count() as f64 * 7.2 + 18.0).min(150.0);
                if x + width > bounds.x1 - f64::from(PAD) {
                    break;
                }
                let chip = Rect::new(x, y, x + width, y + 26.0);
                let selected = album_id == self.active_album;
                scene.fill(
                    Fill::NonZero,
                    transform,
                    if selected {
                        theme.tokens.colors.neutral[3]
                    } else {
                        theme.tokens.colors.surfaces.raised
                    },
                    None,
                    &chip,
                );
                scene.fill(
                    Fill::NonZero,
                    transform,
                    theme.border_standard,
                    None,
                    &Rect::new(chip.x0, chip.y1 - 1.0, chip.x1, chip.y1),
                );
                text_renderer.draw_text_weighted(
                    scene,
                    &label,
                    12.0,
                    theme.text_primary,
                    transform * Affine::translate((chip.x0 + 8.0, chip.y0 + 6.0)),
                    Some((width - 16.0) as f32),
                    TextWeight::Regular,
                );
                self.album_cards.push((album_id, chip));
                x += width + 8.0;
            }
        }
        let top = heading_y + 31.0 + album_h;
        let preview_h = (card_w * 0.62).clamp(68.0, 144.0);
        let card_h = preview_h + 38.0;
        let rows = ((bounds.y1 - top - f64::from(PAD)).max(0.0) / (card_h + gap)).floor() as usize;
        let visible = columns.saturating_mul(rows.max(1));
        for (index, (id, name, favorite)) in self.library_items.iter().take(visible).enumerate() {
            let col = index % columns;
            let row = index / columns;
            let x = bounds.x0 + f64::from(PAD) + col as f64 * (card_w + gap);
            let y = top + row as f64 * (card_h + gap);
            let card = Rect::new(x, y, x + card_w, y + card_h);
            let preview = Rect::new(x, y, x + card_w, y + preview_h);
            scene.fill(
                Fill::NonZero,
                transform,
                if card.contains(self.pointer) {
                    theme.tokens.colors.neutral[3]
                } else {
                    theme.tokens.colors.surfaces.window_card
                },
                None,
                &card,
            );
            scene.fill(
                Fill::NonZero,
                transform,
                theme.tokens.colors.neutral[2],
                None,
                &preview,
            );
            if let Some(image) = self.library_thumbnails.get(id) {
                let scale = (preview.width() / f64::from(image.width))
                    .min(preview.height() / f64::from(image.height));
                let drawn_w = f64::from(image.width) * scale;
                let drawn_h = f64::from(image.height) * scale;
                let image_transform = transform
                    * Affine::translate((
                        preview.x0 + (preview.width() - drawn_w) * 0.5,
                        preview.y0 + (preview.height() - drawn_h) * 0.5,
                    ))
                    * Affine::scale(scale);
                scene.draw_image(image, image_transform);
            }
            scene.fill(
                Fill::NonZero,
                transform,
                theme.border_standard,
                None,
                &Rect::new(card.x0, card.y1 - 1.0, card.x1, card.y1),
            );
            text_renderer.draw_text_weighted(
                scene,
                name,
                13.0,
                theme.text_primary,
                transform * Affine::translate((x + 9.0, preview.y1 + 9.0)),
                Some((card_w - 34.0) as f32),
                TextWeight::Regular,
            );
            if *favorite {
                text_renderer.draw_text_weighted(
                    scene,
                    "★",
                    14.0,
                    theme.tokens.colors.accent[6],
                    transform * Affine::translate((card.x1 - 22.0, preview.y1 + 8.0)),
                    Some(16.0),
                    TextWeight::Medium,
                );
            }
            if self.album_editor && self.album_editor_members.contains(id) {
                scene.fill(
                    Fill::NonZero,
                    transform,
                    theme.tokens.colors.accent[6],
                    None,
                    &Rect::new(card.x0, card.y0, card.x1, card.y0 + 3.0),
                );
            }
            self.library_cards.push((id.clone(), card, *favorite));
        }
        self.draw_album_editor(scene, transform, text_renderer, theme, presence);
    }

    /// Paint the album editor directly over the library, keeping the selected
    /// photographs visible behind it. On narrow screens it becomes a full
    /// width sheet, avoiding a tiny side panel with clipped controls.
    #[allow(
        clippy::too_many_lines,
        reason = "the compact/wide album-sheet geometry and its shared-widget paint path must remain auditable together"
    )]
    fn draw_album_editor(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.album_editor_hits.clear();
        if !self.library_mode {
            return;
        }
        let bounds = self.canvas_rect;
        let narrow = bounds.width() < 620.0;
        let panel_w = if narrow { bounds.width() - 16.0 } else { 360.0 };
        // A phone must keep actual photographs reachable while editing; a
        // compact bottom sheet leaves the upper grid visible instead of
        // turning “choose photographs” into an instruction with no surface.
        let panel_h = if narrow { 230.0 } else { 332.0 };
        let panel_y = if narrow {
            bounds.y1 - panel_h - 8.0
        } else {
            bounds.y0 + 10.0
        };
        let panel = Rect::new(
            if narrow {
                bounds.x0 + 8.0
            } else {
                bounds.x1 - panel_w - 12.0
            },
            panel_y,
            if narrow {
                bounds.x1 - 8.0
            } else {
                bounds.x1 - 12.0
            },
            panel_y + panel_h,
        );
        self.album_editor_rect = panel;
        if !self.album_editor {
            let new = Rect::new(panel.x0, panel.y0, panel.x0 + 110.0, panel.y0 + 36.0);
            let edit = Rect::new(new.x1 + 8.0, panel.y0, new.x1 + 116.0, panel.y0 + 36.0);
            for (hit, rect, label, enabled) in [
                (AlbumEditorHit::New, new, "New album", true),
                (
                    AlbumEditorHit::Edit,
                    edit,
                    "Edit album",
                    self.active_album.is_some(),
                ),
            ] {
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    if enabled {
                        theme.tokens.colors.surfaces.raised
                    } else {
                        theme.tokens.colors.neutral[2]
                    },
                    None,
                    &rect,
                );
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    theme.border_standard,
                    None,
                    &Rect::new(rect.x0, rect.y1 - 1.0, rect.x1, rect.y1),
                );
                text_renderer.draw_text_weighted(
                    scene,
                    label,
                    13.0,
                    if enabled {
                        theme.text_primary
                    } else {
                        theme.text_dim
                    },
                    transform * Affine::translate((rect.x0 + 10.0, rect.y0 + 9.0)),
                    Some(100.0),
                    TextWeight::Medium,
                );
                if enabled {
                    self.album_editor_hits.push((hit, rect));
                }
            }
            return;
        }
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.raised,
            None,
            &panel,
        );
        scene.fill(
            vello::peniko::Fill::NonZero,
            transform,
            theme.border_standard,
            None,
            &Rect::new(panel.x0, panel.y0, panel.x1, panel.y0 + 1.0),
        );
        text_renderer.draw_text_weighted(
            scene,
            if self.album_editor_creating {
                "New album"
            } else {
                "Edit album"
            },
            18.0,
            theme.text_primary,
            transform * Affine::translate((panel.x0 + 16.0, panel.y0 + 16.0)),
            Some((panel.width() - 32.0) as f32),
            TextWeight::Medium,
        );
        text_renderer.draw_text_weighted(
            scene,
            "Album name",
            12.0,
            theme.text_dim,
            transform * Affine::translate((panel.x0 + 16.0, panel.y0 + 48.0)),
            Some(180.0),
            TextWeight::Regular,
        );
        let name_rect = Rect::new(
            panel.x0 + 16.0,
            panel.y0 + 66.0,
            panel.x1 - 16.0,
            panel.y0 + 106.0,
        );
        self.album_name_input.apply_layout_external(name_rect);
        self.album_name_input
            .draw(scene, transform, text_renderer, theme, presence);
        text_renderer.draw_text_weighted(
            scene,
            "Choose photographs in the grid",
            12.0,
            theme.text_dim,
            transform * Affine::translate((panel.x0 + 16.0, panel.y0 + 122.0)),
            Some((panel.width() - 32.0) as f32),
            TextWeight::Regular,
        );
        text_renderer.draw_text_weighted(
            scene,
            &format!("{} selected", self.album_editor_members.len()),
            14.0,
            theme.text_primary,
            transform * Affine::translate((panel.x0 + 16.0, panel.y0 + 146.0)),
            Some(150.0),
            TextWeight::Medium,
        );
        let y = panel.y1 - 48.0;
        let cancel = Rect::new(panel.x0 + 16.0, y, panel.x0 + 88.0, y + 34.0);
        let save = Rect::new(panel.x1 - 92.0, y, panel.x1 - 16.0, y + 34.0);
        let delete = Rect::new(save.x0 - 88.0, y, save.x0 - 8.0, y + 34.0);
        for (hit, rect, label) in [
            (AlbumEditorHit::Cancel, cancel, "Cancel"),
            (AlbumEditorHit::Save, save, "Save"),
        ] {
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                theme.tokens.colors.neutral[3],
                None,
                &rect,
            );
            text_renderer.draw_text_weighted(
                scene,
                label,
                13.0,
                theme.text_primary,
                transform * Affine::translate((rect.x0 + 10.0, rect.y0 + 9.0)),
                Some((rect.width() - 16.0) as f32),
                TextWeight::Medium,
            );
            let focused = matches!(hit, AlbumEditorHit::Cancel) && self.album_editor_focus == 1
                || matches!(hit, AlbumEditorHit::Save)
                    && self.album_editor_focus == if self.album_editor_creating { 2 } else { 3 };
            if focused {
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    theme.tokens.colors.accent[6],
                    None,
                    &Rect::new(rect.x0, rect.y0, rect.x1, rect.y0 + 2.0),
                );
            }
            self.album_editor_hits.push((hit, rect));
        }
        if !self.album_editor_creating {
            let (hit, label) = if self.album_delete_confirm {
                (AlbumEditorHit::ConfirmDelete, "Delete album?")
            } else {
                (AlbumEditorHit::Delete, "Delete")
            };
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                theme.tokens.colors.semantic.danger,
                None,
                &delete,
            );
            text_renderer.draw_text_weighted(
                scene,
                label,
                12.0,
                theme.text_primary,
                transform * Affine::translate((delete.x0 + 7.0, delete.y0 + 9.0)),
                Some((delete.width() - 10.0) as f32),
                TextWeight::Medium,
            );
            if self.album_editor_focus == 2 {
                scene.fill(
                    vello::peniko::Fill::NonZero,
                    transform,
                    theme.tokens.colors.accent[6],
                    None,
                    &Rect::new(delete.x0, delete.y0, delete.x1, delete.y0 + 2.0),
                );
            }
            self.album_editor_hits.push((hit, delete));
        }
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
        .on_effect(move || slot.set(Some(hit)))
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
    fn local_resource_operation_handlers(&self) -> &'static [&'static str] {
        &[crate::actions::PHOTOS_OPERATION_TYPE]
    }

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
        match operation {
            crate::actions::PhotosOperation::ViewAlbum { album_id } => {
                self.view_album(doc, &album_id)?;
            }
            crate::actions::PhotosOperation::Open { path: Some(_) } => {
                return Err("direct open requires lifecycle dispatch".into());
            }
            crate::actions::PhotosOperation::Save { path } => {
                let open =
                    PhotosDoc::image(doc).ok_or_else(|| "no photograph is open".to_string())?;
                let destination = path.unwrap_or_else(|| Self::suggested_save_path(&open));
                if !self.save_to(&destination) {
                    return Err(tr("photos-save-failed"));
                }
            }
            other => self.perform_effect(other, doc),
        }
        Ok(true)
    }

    fn start_resource_operation(
        &mut self,
        invocation: qos_actions::InvocationId,
        kind: qos_actions::OperationKind,
        type_id: &str,
        payload: &[u8],
        doc: &AutoCommit,
        path: &ObjId,
    ) -> Result<Option<ui::widget::ResourceOperationStart>, String> {
        let Some(operation) = crate::actions::decode_operation(type_id, payload)? else {
            return Ok(None);
        };
        if let crate::actions::PhotosOperation::Open { path: Some(file) } = operation {
            self.pending_resource_open = Some((invocation, file));
            return Ok(Some(ui::widget::ResourceOperationStart::Accepted));
        }
        if !self.invoke_resource_operation(kind, type_id, payload, doc, path)? {
            return Ok(None);
        }
        if self.picker.is_some() {
            self.resource_invocation = Some(invocation);
            Ok(Some(ui::widget::ResourceOperationStart::Accepted))
        } else {
            Ok(Some(ui::widget::ResourceOperationStart::Completed(
                qos_actions::ActionOutput::default(),
            )))
        }
    }

    fn poll_resource_operation(
        &mut self,
        invocation: qos_actions::InvocationId,
        _cancellation_requested: bool,
        _doc: &AutoCommit,
        _path: &ObjId,
    ) -> Option<qos_actions::InvocationUpdate> {
        if self
            .resource_result
            .as_ref()
            .is_some_and(|u| u.invocation() == invocation)
        {
            self.resource_result.take()
        } else {
            None
        }
    }

    fn sync(&mut self, doc: &mut AutoCommit, _path: &ObjId) {
        PhotosDoc::seed(doc);
        if let Some((invocation, path)) = self.pending_resource_open.take() {
            self.open_path(doc, &path);
            self.resource_result = Some(if self.open_image_path().is_some() {
                qos_actions::InvocationUpdate::Succeeded {
                    invocation,
                    output: qos_actions::ActionOutput::default(),
                }
            } else {
                qos_actions::InvocationUpdate::Failed {
                    invocation,
                    error: qos_actions::ActionFailure {
                        code: "photos.open_failed".into(),
                        message: tr("photos-open-failed"),
                        details: None,
                        retryable: false,
                    },
                }
            });
        }
        self.drain_picker(doc);
        self.refresh(doc);

        // An address can arrive before its bytes. Do not offer Copy, Save or
        // a blind crop against an empty canvas; the durable resource remains
        // available to a headless collaborator that deliberately chooses it.
        let has_image = self.has_image();
        let edited = !PhotosDoc::edits(doc).is_identity();
        for (hit, button) in &mut self.buttons {
            button.set_enabled(Self::enabled(*hit, has_image, edited));
        }
        self.status = self.status_text(doc);
        let heads = doc.get_heads();
        if heads != self.search_heads {
            self.search_provider = crate::search::PhotosSearchProvider::new(doc);
            self.search_heads = heads;
        }
        let library = PhotosDoc::library(doc);
        let membership = self.active_album.as_ref().and_then(|id| {
            library
                .albums
                .iter()
                .find(|album| &album.id == id)
                .map(|album| &album.photo_ids)
        });
        self.library_total = library
            .photos
            .iter()
            .filter(|photo| membership.is_none_or(|ids| ids.contains(&photo.id)))
            .count();
        self.library_offset = self
            .library_offset
            .min(self.library_total.saturating_sub(1));
        self.library_items = library
            .photos
            .iter()
            .filter(|photo| membership.is_none_or(|ids| ids.contains(&photo.id)))
            .skip(self.library_offset)
            .take(120)
            .map(|photo| (photo.id.clone(), photo.image.name.clone(), photo.favorite))
            .collect();
        self.library_albums = library
            .albums
            .iter()
            .take(24)
            .map(|album| (album.id.clone(), album.name.clone(), album.photo_ids.len()))
            .collect();
        let thumbnail_sources: Vec<_> = library
            .photos
            .iter()
            .filter(|photo| membership.is_none_or(|ids| ids.contains(&photo.id)))
            .skip(self.library_offset)
            .take(24)
            .map(|photo| (photo.id.clone(), photo.image.path.clone()))
            .collect();
        self.library_thumbnails
            .retain(|id, _| thumbnail_sources.iter().any(|(wanted, _)| wanted == id));
        for (id, source) in thumbnail_sources {
            if !self.library_thumbnails.contains_key(&id)
                && let Some(decoded) = std::fs::read(source)
                    .ok()
                    .and_then(|bytes| pixels::decode(&bytes))
            {
                self.library_thumbnails.insert(id, to_peniko(&decoded));
            }
        }
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

        if self.library_mode {
            self.draw_library(scene, transform, text_renderer, theme, presence);
        } else if let Some(v) = self.viewer.as_mut() {
            v.draw(scene, transform, text_renderer, theme, presence);
        } else {
            let (title, body) = self.empty_state_text();
            EmptyState::paint(
                scene,
                transform,
                text_renderer,
                theme,
                self.canvas_rect,
                Some(Icon::Image),
                &title,
                Some(&body),
                None,
                Point::ZERO,
            );
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

    #[allow(
        clippy::too_many_lines,
        reason = "the widget event entry point preserves explicit precedence between album chips, grid cards, toolbar, and viewer"
    )]
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
        if let winit::event::WindowEvent::CursorMoved { position, .. } = event {
            self.pointer = Point::new(position.x / scale_factor, position.y / scale_factor);
        }

        if self.library_mode
            && self.handle_album_editor_event(
                event,
                scale_factor,
                *modifiers,
                doc,
                path,
                handled,
                node_id,
                taffy,
            )
        {
            return;
        }

        if self.library_mode
            && matches!(
                event,
                winit::event::WindowEvent::MouseInput {
                    state: winit::event::ElementState::Released,
                    button: winit::event::MouseButton::Left,
                    ..
                }
            )
            && let Some((hit, _)) = self
                .album_editor_hits
                .iter()
                .find(|(_, rect)| rect.contains(self.pointer))
                .copied()
        {
            match hit {
                AlbumEditorHit::New => self.begin_new_album(doc),
                AlbumEditorHit::Edit => self.begin_edit_album(doc),
                _ => {}
            }
            *handled = true;
            return;
        }

        if self.library_mode
            && matches!(
                event,
                winit::event::WindowEvent::MouseInput {
                    state: winit::event::ElementState::Released,
                    button: winit::event::MouseButton::Left,
                    ..
                }
            )
            && let Some((album_id, _)) = self
                .album_cards
                .iter()
                .find(|(_, rect)| rect.contains(self.pointer))
                .cloned()
        {
            self.active_album = album_id;
            self.library_offset = 0;
            *handled = true;
            return;
        }
        if self.library_mode
            && let winit::event::WindowEvent::MouseWheel { delta, .. } = event
        {
            let direction = match delta {
                winit::event::MouseScrollDelta::LineDelta(_, y) => y.signum(),
                winit::event::MouseScrollDelta::PixelDelta(position) => position.y.signum() as f32,
            };
            let page = 24_usize;
            if direction < 0.0 {
                self.library_offset =
                    (self.library_offset + page).min(self.library_total.saturating_sub(1));
            } else if direction > 0.0 {
                self.library_offset = self.library_offset.saturating_sub(page);
            }
            *handled = true;
            return;
        }

        // A library card is a direct manipulation surface, but it is still
        // only a front end for a canonical action.  The star has its own hit
        // area so changing a favourite does not unexpectedly open the editor.
        if self.library_mode
            && matches!(
                event,
                winit::event::WindowEvent::MouseInput {
                    state: winit::event::ElementState::Released,
                    button: winit::event::MouseButton::Left,
                    ..
                }
            )
            && let Some((photo_id, card, favorite)) = self
                .library_cards
                .iter()
                .find(|(_, card, _)| card.contains(self.pointer))
                .cloned()
        {
            if self.pointer.x >= card.x1 - 34.0 {
                self.commit_library_action(
                    doc,
                    &crate::actions::PhotosAction::SetFavorite {
                        photo_id,
                        favorite: !favorite,
                    },
                );
            } else {
                self.commit_library_action(
                    doc,
                    &crate::actions::PhotosAction::SelectPhoto { photo_id },
                );
                self.library_mode = false;
            }
            *handled = true;
            return;
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
        Some(self.accessibility_tree())
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &AccessAction,
        state: &mut AutoCommit,
        _path: &ObjId,
    ) -> bool {
        self.perform_toolbar_semantic_action(target, action, state)
            || self.perform_library_semantic_action(target, action, state)
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        match path {
            [0, button] => self
                .buttons
                .get(*button)
                .map(|(_, button)| button.get_rect()),
            [1] => Some(self.canvas_rect),
            [2] if !self.status.is_empty() => Some(self.status_rect),
            _ => None,
        }
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

    /// Keep the palette projection derived from the same fresh document
    /// resource tree as agents and assistive technology. The provider's
    /// catalog gives Ctrl+K Unicode-safe, cursor-paged bounded queries.
    fn search_providers(&self) -> Vec<Box<dyn ui::search::SearchProvider>> {
        vec![Box::new(self.search_provider.clone())]
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::{PHOTOS_OPERATION_TYPE, PhotosOperation, PhotosResources};
    use crate::model::{Album, LibraryPhoto};
    use crate::shots_pattern;
    use automerge::ROOT;
    use qos_actions::ResourceProvider;

    fn has_resource_id(resource: &qos_actions::Resource, id: &str) -> bool {
        resource.id.0 == id
            || resource
                .children
                .iter()
                .any(|child| has_resource_id(child, id))
    }

    #[test]
    fn opening_an_album_operation_only_changes_the_local_filter_and_refuses_stale_ids() {
        let mut app = PhotosApp::new();
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let photo = LibraryPhoto::from_image(OpenImage::from_path("/photos/one.png", 20, 10));
        let mut library = crate::model::PhotoLibrary::default();
        library.photos.push(photo.clone());
        library.albums.push(Album {
            id: "photos.album:trip".into(),
            name: "Trip".into(),
            photo_ids: vec![photo.id],
        });
        PhotosDoc::set_library(&mut doc, &library);
        let before = PhotosDoc::library(&doc);
        let payload = qos_actions::EncodedAction::encode_operation(
            PHOTOS_OPERATION_TYPE,
            &PhotosOperation::ViewAlbum {
                album_id: "photos.album:trip".into(),
            },
        )
        .unwrap();
        assert!(
            Widget::invoke_resource_operation(
                &mut app,
                qos_actions::OperationKind::Effect,
                PHOTOS_OPERATION_TYPE,
                &payload.payload,
                &doc,
                &ROOT,
            )
            .unwrap()
        );
        assert!(app.library_mode);
        assert_eq!(app.active_album.as_deref(), Some("photos.album:trip"));
        assert_eq!(
            PhotosDoc::library(&doc),
            before,
            "navigation must not alter membership"
        );

        let stale = qos_actions::EncodedAction::encode_operation(
            PHOTOS_OPERATION_TYPE,
            &PhotosOperation::ViewAlbum {
                album_id: "photos.album:gone".into(),
            },
        )
        .unwrap();
        assert!(
            Widget::invoke_resource_operation(
                &mut app,
                qos_actions::OperationKind::Effect,
                PHOTOS_OPERATION_TYPE,
                &stale.payload,
                &doc,
                &ROOT,
            )
            .is_err()
        );
        assert_eq!(PhotosDoc::library(&doc), before);
    }

    #[test]
    fn accessibility_exposes_and_routes_every_toolbar_control() {
        let mut app = PhotosApp::new();
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        app.sync(&mut doc, &ROOT);

        let empty = app.accessibility_tree();
        assert_eq!(empty.role, Role::Group);
        assert_eq!(empty.children[0].role, Role::Toolbar);
        assert_eq!(empty.children[0].children.len(), TOOLBAR.len());
        assert_eq!(empty.children[1].role, Role::Image);
        assert_eq!(
            empty.children[0].children[0].action_target.as_deref(),
            Some("photos:toolbar:photos-open")
        );
        assert!(
            empty.children[0]
                .children
                .iter()
                .find(|node| {
                    node.action_target.as_deref() == Some("photos:toolbar:photos-rotate-right")
                })
                .is_some_and(|node| node.state.disabled),
            "rotate must be unavailable without a photograph"
        );
        assert!(
            !app.perform_toolbar_semantic_action(
                "photos:toolbar:photos-rotate-right",
                &AccessAction::Activate,
                &mut doc,
            ),
            "a disabled semantic control must reject activation"
        );

        let image = OpenImage::from_path("/tmp/accessible.png", 100, 50);
        PhotosDoc::set_image(&mut doc, Some(&image));
        app.original = Some(("/tmp/accessible.png".into(), shots_pattern(100, 50)));
        app.refresh(&doc);
        app.sync(&mut doc, &ROOT);
        assert!(Widget::perform_semantic_action(
            &mut app,
            "photos:toolbar:photos-rotate-right",
            &AccessAction::Activate,
            &mut doc,
            &ROOT,
        ));
        assert_eq!(PhotosDoc::edits(&doc).rotate_quarters, 1);
        assert!(
            app.take_committed_action().is_some(),
            "the semantic route must create the normal undo boundary"
        );

        app.sync(&mut doc, &ROOT);
        let opened = app.accessibility_tree();
        assert_eq!(
            opened.children[1].name.as_deref(),
            Some("accessible.png — edited")
        );
        let status = opened
            .children
            .last()
            .expect("opened image has status text");
        assert_eq!(status.role, Role::StaticText);
        assert_eq!(status.live, Some(Politeness::Polite));
    }

    #[test]
    fn unavailable_bytes_and_bad_bytes_have_distinct_honest_canvas_states() {
        let mut app = PhotosApp::new();
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let waiting =
            OpenImage::from_path("/not-present/yet.png", 1200, 800).with_content("0".repeat(64));
        PhotosDoc::set_image(&mut doc, Some(&waiting));
        app.sync(&mut doc, &ROOT);
        assert_eq!(app.canvas_state, CanvasState::Loading);
        assert!(!app.has_image());
        assert_eq!(app.empty_state_text().0, tr("photos-loading-title"));
        assert!(
            app.buttons
                .iter()
                .filter(|(hit, _)| !matches!(hit, Hit::Open | Hit::Library))
                .all(|(_, button)| !button.is_enabled())
        );

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.png");
        std::fs::write(&path, b"not image data").unwrap();
        let bad = OpenImage::from_path(&path.to_string_lossy(), 1200, 800);
        PhotosDoc::set_image(&mut doc, Some(&bad));
        app.sync(&mut doc, &ROOT);
        assert_eq!(app.canvas_state, CanvasState::Error);
        assert_eq!(app.empty_state_text().0, tr("photos-error-title"));
    }

    #[test]
    fn synchronized_app_registers_the_canonical_photo_search_provider() {
        let mut app = PhotosApp::new();
        let mut doc = AutoCommit::new();
        PhotosDoc::seed(&mut doc);
        let image = OpenImage::from_path("/pictures/München-夏.png", 800, 400);
        PhotosDoc::set_image(&mut doc, Some(&image));
        app.sync(&mut doc, &ROOT);
        let before = app.search_provider.stats();
        app.sync(&mut doc, &ROOT);
        assert_eq!(
            app.search_provider.stats(),
            before,
            "unchanged sync rebuilt search"
        );

        let providers = Widget::search_providers(&app);
        assert_eq!(providers.len(), 1);
        let hit = providers[0].search("夏").pop().unwrap();
        let address = hit.action_address().unwrap();
        assert!(matches!(address.action.as_str(), "view" | "open"));
        assert!(
            PhotosResources
                .resources(&doc)
                .iter()
                .any(|resource| has_resource_id(resource, &address.resource))
        );
    }
}
