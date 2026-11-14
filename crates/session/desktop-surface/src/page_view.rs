//! A page another device publishes, as the tile this desktop draws it in.
//!
//! A web page is foreign content, so its window is the one kind whose drawing
//! leaves the device that runs the engine: the page travels as a display list
//! (`tilo_paint_stream`) over the workspace's live plane, one track per window
//! ([`tilo_live::page`] when the session publishes it). [`PageStream`] is what
//! a viewer holds of one page: the [`Receiver`] that keeps the frame the
//! publisher holds, the images the frame references by content hash, and the
//! [`PageRenderer`] that draws it with this toolkit's own Vello and text —
//! the same code the window's own page is drawn by, natively and in a page.
//!
//! [`PageView`] is that as a tile: the frame letterboxed in the window's box,
//! or the framework's arriving card until the first whole frame comes. Input
//! never reaches it: a viewer of a page watches it, and what a click means is
//! the publisher's to decide. The tile names the text its page shows, each run
//! where it is drawn, so a reader who cannot see the page is told what a
//! reader who can sees there.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::sync::Arc;

use ui::a11y::{AccessNode, Role, SharedAccessNode};
use ui::kurbo::{Affine, Point, Rect};
use ui::taffy::NodeId;
use ui::taffy::{Dimension, Size, Style};
use ui::vello::peniko::Fill;
use ui::webview::paint::stream::wire::{Payload, Receiver};
use ui::webview::paint::stream::{Frame, Item, LayerId, ROOT, Rect as PageRect};
use ui::webview::paint::{BlobSource, PageCanvas, PageRenderer};
use ui::widget::{MediaAspect, MediaGrid, TaffyTreeExt, Widget};
use ui::{TextRenderer, Theme};

use crate::page_code::PageHandle;

/// Images one page keeps: the frame names the images it needs, and a page
/// that replaced more than this since its last whole frame starts again at one
/// anyway.
const MAX_BLOBS: usize = 128;

/// Most runs of text one page's tile names. A page that paints more is named by the first of
/// them, in the order it paints them.
const MAX_TEXT_RUNS: usize = 512;

/// The text a tile last named of its page: the frame it read, the box it placed the page in, and
/// the runs.
type Named = (u64, Rect, Vec<Arc<AccessNode>>);

/// A window's page as a viewer holds it: the frame, the images it references,
/// and the renderer that draws it.
pub struct PageStream {
    receiver: Receiver,
    blobs: Rc<RefCell<Blobs>>,
    renderer: Rc<RefCell<PageRenderer>>,
    /// The frame number the receiver holds, so a caller tells when a payload
    /// changed what the tile shows.
    seq: Option<u64>,
}

impl Default for PageStream {
    fn default() -> Self {
        Self::new()
    }
}

impl PageStream {
    #[must_use]
    pub fn new() -> Self {
        Self {
            receiver: Receiver::new(),
            blobs: Rc::default(),
            renderer: Rc::new(RefCell::new(PageRenderer::new())),
            seq: None,
        }
    }

    /// Take one object of the page: the payload as its publisher encoded it.
    /// Whether the frame the tile shows changed.
    pub fn apply(&mut self, payload: &[u8]) -> bool {
        let Ok(payload) = Payload::decode(payload) else {
            return false;
        };
        if let Some((hash, bytes)) = self.receiver.apply(payload) {
            self.blobs.borrow_mut().insert(hash, bytes);
        }
        let seq = self.receiver.frame().map(|frame| frame.seq);
        let changed = seq.is_some() && seq != self.seq;
        self.seq = seq;
        changed
    }

    /// The page the viewer holds, once a whole frame reached it.
    #[must_use]
    pub fn frame(&self) -> Option<&ui::webview::paint::stream::Frame> {
        self.receiver.frame()
    }

    /// A payload went missing and the viewer waits for the publisher's next
    /// whole frame.
    #[must_use]
    pub const fn is_lost(&self) -> bool {
        self.receiver.is_lost()
    }

    /// The tile that draws this page: the frame itself, or the arriving card
    /// while the publisher's whole frame is still on its way.
    #[must_use]
    pub fn view(this: &Rc<RefCell<Self>>, alt: String) -> Box<dyn Widget> {
        if this.borrow().frame().is_some() {
            Box::new(PageView::new(Rc::clone(this), alt))
        } else {
            Box::new(MediaGrid::arriving(
                1,
                MediaAspect::Wide,
                &ui::arriving::Source::peer("the device running it"),
                ui::time::now(),
            ))
        }
    }
}

/// Page images by content hash, oldest let go past [`MAX_BLOBS`].
#[derive(Default)]
struct Blobs {
    bytes: HashMap<[u8; 32], Arc<Vec<u8>>>,
    order: VecDeque<[u8; 32]>,
}

impl Blobs {
    fn insert(&mut self, hash: [u8; 32], bytes: Vec<u8>) {
        if self.bytes.insert(hash, Arc::new(bytes)).is_none() {
            self.order.push_back(hash);
        }
        while self.order.len() > MAX_BLOBS {
            if let Some(oldest) = self.order.pop_front() {
                self.bytes.remove(&oldest);
            }
        }
    }
}

/// One page as a tile, drawn in the box the desk lays it out in.
pub struct PageView {
    stream: Rc<RefCell<PageStream>>,
    rect: Rect,
    /// What the tile is, for a reader who cannot see the page.
    alt: String,
    /// The page's text as the tile last named it, kept while the frame and the box stay.
    named: RefCell<Option<Named>>,
}

impl PageView {
    /// The tile drawing `stream`'s page.
    #[must_use]
    pub fn new(stream: Rc<RefCell<PageStream>>, alt: impl Into<String>) -> Self {
        Self {
            stream,
            rect: Rect::ZERO,
            alt: alt.into(),
            named: RefCell::new(None),
        }
    }

    /// How the page stands in the tile's box: its scale, and where its top left corner is drawn.
    /// The page keeps its shape and is centred, as [`Widget::draw`] draws it. `None` for a page or
    /// a box without an area.
    fn placement(&self, frame: &Frame) -> Option<(f64, f64, f64)> {
        let (width, height) = (f64::from(frame.viewport.x), f64::from(frame.viewport.y));
        if width <= 0.0 || height <= 0.0 {
            return None;
        }
        let scale = (self.rect.width() / width).min(self.rect.height() / height);
        if !scale.is_finite() || scale <= 0.0 {
            return None;
        }
        let left = self.rect.x0 + (self.rect.width() - width * scale) / 2.0;
        let top = self.rect.y0 + (self.rect.height() - height * scale) / 2.0;
        Some((scale, left, top))
    }

    /// The tile's own node: a document, called what the tile is.
    fn document(&self) -> AccessNode {
        AccessNode::new(Role::Document).with_name(self.alt.clone())
    }

    /// The text the page shows, each run where the tile draws it. It is read again only when the
    /// frame or the tile's box changed, so a reader's tree keeps the runs of the last frame as
    /// they are.
    fn text_runs(&self) -> Vec<Arc<AccessNode>> {
        let stream = self.stream.borrow();
        let Some(frame) = stream.frame() else {
            return Vec::new();
        };
        if let Some((seq, rect, runs)) = self.named.borrow().as_ref()
            && *seq == frame.seq
            && *rect == self.rect
        {
            return runs.clone();
        }
        let runs: Vec<Arc<AccessNode>> = self
            .placement(frame)
            .map(|(scale, left, top)| {
                shown_text(frame)
                    .into_iter()
                    .map(|(text, at)| {
                        let x = f64::from(at.x).mul_add(scale, left);
                        let y = f64::from(at.y).mul_add(scale, top);
                        let bounds = Rect::new(
                            x,
                            y,
                            f64::from(at.w).mul_add(scale, x),
                            f64::from(at.h).mul_add(scale, y),
                        );
                        Arc::new(
                            AccessNode::new(Role::StaticText)
                                .with_name(text)
                                .with_bounds(bounds),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        *self.named.borrow_mut() = Some((frame.seq, self.rect, runs.clone()));
        runs
    }
}

/// The text a page shows, in the order it paints it, each run with the box it is painted in, in
/// the page's viewport: what a tile tells a reader who cannot see it. Text scrolled or clipped out
/// of view is left out, as it is on the screen, and so is text in a layer that is not drawn.
fn shown_text(frame: &Frame) -> Vec<(String, PageRect)> {
    let mut runs = Vec::new();
    text_in(frame, ROOT, 0, &mut runs);
    runs
}

/// The runs of `layer` and of the layers it paints, added to `runs` in paint order.
fn text_in(frame: &Frame, layer: LayerId, depth: usize, runs: &mut Vec<(String, PageRect)>) {
    if depth > frame.layers.len() {
        return;
    }
    let Some(held) = frame.layers.get(&layer) else {
        return;
    };
    let Some((offset, clip)) = frame.layer_to_viewport(layer) else {
        return;
    };
    if held.opacity <= 0.0 {
        return;
    }
    let viewport = PageRect::new(0.0, 0.0, frame.viewport.x, frame.viewport.y);
    let seen = clip.map_or(viewport, |clip| overlap(clip, viewport));
    for item in &held.items {
        if runs.len() >= MAX_TEXT_RUNS {
            return;
        }
        match item {
            Item::Text { rect, text, .. } => {
                let said = text.trim();
                let shown = overlap(rect.translate(offset.x, offset.y), seen);
                if !said.is_empty() && shown.w > 0.0 && shown.h > 0.0 {
                    runs.push((said.to_owned(), shown));
                }
            }
            Item::Layer(child) => text_in(frame, *child, depth.saturating_add(1), runs),
            _ => {}
        }
    }
}

/// The part of `first` that lies in `second`: a box without an area when they do not meet.
fn overlap(first: PageRect, second: PageRect) -> PageRect {
    let left = first.x.max(second.x);
    let top = first.y.max(second.y);
    let right = (first.x + first.w).min(second.x + second.w);
    let bottom = (first.y + first.h).min(second.y + second.h);
    PageRect::new(left, top, (right - left).max(0.0), (bottom - top).max(0.0))
}

impl ui::widget::AppWidget for PageView {
    fn app_kind(&self) -> ui::widget::AppKind {
        ui::widget::AppKind::Browser
    }

    fn app_title(&self) -> &str {
        &self.alt
    }

    fn app_icon(&self) -> ui::widget::IconDescriptor {
        ui::widget::IconDescriptor::Glyph("\u{f0ac}")
    }
}

impl Widget for PageView {
    fn name(&self) -> &'static str {
        "PageView"
    }

    fn build_layout(
        &mut self,
        taffy: &mut ui::taffy::TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(
            Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                min_size: Size {
                    width: Dimension::length(0.0),
                    height: Dimension::length(0.0),
                },
                ..Style::default()
            },
            node_map,
            self,
        )
    }

    fn apply_layout(&mut self, taffy: &ui::taffy::TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    /// The page for a reader who cannot see it: a document called what the tile is, holding the
    /// text the page shows, each run where the tile draws it.
    fn accessibility(&self) -> Option<AccessNode> {
        self.accessibility_shared()
            .map(SharedAccessNode::into_owned)
    }

    fn accessibility_shared(&self) -> Option<SharedAccessNode> {
        Some(SharedAccessNode {
            node: self.document(),
            children: self.text_runs(),
        })
    }

    fn accessibility_root(&self) -> Option<AccessNode> {
        Some(self.document())
    }

    #[expect(
        clippy::arithmetic_side_effects,
        reason = "a page's size and the tile's box are finite, and a scale that is not is refused above"
    )]
    fn draw(
        &mut self,
        scene: &mut ui::vello::Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &ui::presence::PresenceCtx,
    ) {
        // The page's own background is drawn with it; what shows around it is
        // the tile's.
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.surfaces.base,
            None,
            &self.rect,
        );
        let stream = self.stream.borrow();
        let Some(frame) = stream.frame() else {
            return;
        };
        let (width, height) = (f64::from(frame.viewport.x), f64::from(frame.viewport.y));
        if width <= 0.0 || height <= 0.0 {
            return;
        }
        // The page keeps its shape in the window's box: a window the person
        // resized shows the page whole, as a picture does.
        let scale = (self.rect.width() / width).min(self.rect.height() / height);
        if !scale.is_finite() || scale <= 0.0 {
            return;
        }
        let at =
            self.rect.center() - ui::kurbo::Vec2::new(width * scale / 2.0, height * scale / 2.0);
        let page = transform * Affine::translate((at.x, at.y)) * Affine::scale(scale);
        let blobs = Rc::clone(&stream.blobs);
        let source = move |hash: &[u8; 32]| blobs.borrow().bytes.get(hash).cloned();
        let mut canvas = PageCanvas {
            scene,
            transform: page,
            text: text_renderer,
            blobs: &source,
        };
        stream.renderer.borrow_mut().draw(&mut canvas, frame);
    }
}

impl BlobSource for Blobs {
    fn blob(&self, hash: &[u8; 32]) -> Option<Arc<Vec<u8>>> {
        self.bytes.get(hash).cloned()
    }
}

/// One window's page, as [`PageCode`](crate::page_code::PageCode) hands it over.
pub(crate) struct Viewer {
    stream: Rc<RefCell<PageStream>>,
}

impl Viewer {
    pub(crate) fn new() -> Self {
        Self {
            stream: Rc::new(RefCell::new(PageStream::new())),
        }
    }
}

impl PageHandle for Viewer {
    fn apply(&self, payload: &[u8]) -> bool {
        self.stream.borrow_mut().apply(payload)
    }

    fn tile(&self, alt: String) -> Box<dyn ui::widget::AppWidget> {
        Box::new(PageView::new(Rc::clone(&self.stream), alt))
    }
}
