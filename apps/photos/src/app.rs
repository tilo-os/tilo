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
        let mut buttons = Vec::new();
        for (hit, key, icon) in [
            (Hit::Open, "photos-open", Icon::Folder),
            (Hit::RotateLeft, "photos-rotate-left", Icon::RotateCcw),
            (Hit::RotateRight, "photos-rotate-right", Icon::RotateCw),
            (Hit::CropToView, "photos-crop", Icon::Crop),
            (Hit::Reset, "photos-reset", Icon::Refresh),
            (Hit::Copy, "photos-copy", Icon::Copy),
            (Hit::Save, "photos-save", Icon::Save),
        ] {
            let label = tr(key);
            let slot = pending.clone();
            let button = Button::new(key, &label, None)
                .with_icon(icon)
                .on_click(move || slot.set(Some(hit)));
            buttons.push((hit, button));
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
            picker: None,
        }
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
        let mut bar_children = Vec::new();
        for (_, button) in &mut self.buttons {
            bar_children.push(button.build_layout(taffy, theme, node_map));
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
                        width: LengthPercentage::length(8.0),
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
                    ..Default::default()
                },
            )
            .unwrap();

        let status = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::length(BAR_H * 0.6),
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
        for ((_, button), &bn) in self.buttons.iter_mut().zip(bar_children.iter()) {
            button.apply_layout(taffy, bn, bar_origin);
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
        for edge in [self.toolbar_rect.y1, self.status_rect.y0] {
            scene.fill(
                Fill::NonZero,
                transform,
                theme.border_standard,
                None,
                &Rect::new(self.rect.x0, edge - 1.0, self.rect.x1, edge),
            );
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
            for ((_, button), &bn) in self.buttons.iter_mut().zip(bar_children.iter()) {
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
