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
/// gets a [`Theme`] but no [`TextRenderer`]. The same 7.0 the shared
/// [`ui::widget::Toolbar`] uses, so a Photos toolbar reads at the density of
/// every other strip in the system. `toolbar_labels_fit_their_buttons` in
/// `tests/end_to_end.rs` measures the real glyphs against it.
const APPROX_CHAR_W: f32 = 7.0;

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
        match std::fs::read(path).ok().and_then(|b| pixels::decode(&b)) {
            Some(img) => {
                let open = OpenImage::from_path(path, img.w, img.h);
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
    fn refresh(&mut self, doc: &AutoCommit) {
        let Some(open) = PhotosDoc::image(doc) else {
            self.rendered = None;
            self.viewer = None;
            self.original = None;
            return;
        };
        let edits = PhotosDoc::edits(doc);

        // Load the original if this is a path we have not decoded yet. A peer
        // may have opened it, so this is not only the local Open path.
        if self.original.as_ref().map(|(p, _)| p.as_str()) != Some(open.path.as_str()) {
            match std::fs::read(&open.path).ok().and_then(|b| pixels::decode(&b)) {
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

    fn name(&self) -> &str {
        "PhotosApp"
    }

    fn accessibility(&self) -> Option<AccessNode> {
        use ui::a11y::Role;
        Some(AccessNode::new(Role::Group))
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}
