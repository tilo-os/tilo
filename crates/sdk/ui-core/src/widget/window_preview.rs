//! `WindowPreview`: where a window's live picture shows in a view, as [`WindowWell`] names where
//! the window itself goes.
//!
//! A view that shows windows small (the switcher, a canvas's miniature of a desk) never draws
//! their pixels: it puts a preview where the picture goes, and the compositor draws that
//! window's own latest buffer there, in its own pixels. The preview draws nothing and takes no
//! input; what a person sees under it is the view's own, and the picture covers it.
//!
//! After each layout, and on each frame an enclosing component moves it, it reports its
//! rectangle in layout coordinates to the host's [`Previews`], keyed by the window's key in the
//! reading it stands for. The host that hands the compositor its pictures reads those
//! rectangles, so a preview that moves moves its picture in the same frame.
//!
//! The preview is a leaf with the shape of a window's own 16:10 well: it takes the width its
//! frame gives it and stands as tall as that width makes a window.
//!
//! [`WindowWell`]: crate::widget::WindowWell

use std::cell::RefCell;
use std::collections::BTreeMap;

use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use crate::component::{Component, Described};
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::window_well::Wells;
use crate::widget::{TaffyTreeExt, Widget};

/// The width of a window for every ten of its height, as every desktop shapes a window's
/// picture (the well the old switcher and the overview draw).
const WINDOW_SHAPE: f32 = 16.0 / 10.0;

/// Where the previews of a tree on this thread report, shared with the host that hands the
/// compositor each window's live picture there: the same handle, so a preview that moves moves
/// its picture.
///
/// A host that shows a shell's views in the compositor's process installs one handle
/// ([`install`]) before it lays the tree out; the preview reports into it on every layout. With
/// none installed, a preview reports nowhere and costs one thread-local read.
///
/// It keeps the rects a well keeps ([`Wells`]), apart from the tree's wells: a preview moves
/// nothing on the desk.
#[derive(Clone, Default)]
pub struct Previews {
    rects: Wells,
}

impl Previews {
    /// A handle with no preview on it.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Where the preview of window `window` stands, in the layout's logical coordinates, as of
    /// the last layout that placed it.
    #[must_use]
    pub fn picture(&self, window: &str) -> Option<Rect> {
        self.rects.well(window)
    }

    /// Every preview on screen, by window, as of the last layout.
    #[must_use]
    pub fn pictures(&self) -> BTreeMap<String, Rect> {
        self.rects.wells()
    }

    /// Whether any preview stands on a tree at all: empty after a layout that placed none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rects.is_empty()
    }

    /// Moves with every layout that placed a preview, so a reader knows whether its last
    /// answer still stands.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.rects.generation()
    }

    /// Record `rect` as the preview of window `window`.
    pub fn place(&self, window: &str, rect: Rect) {
        self.rects.place(window, rect);
    }

    /// Forget the preview of window `window`: it left the screen.
    pub fn remove(&self, window: &str) {
        self.rects.remove(window);
    }

    /// Forget every preview: a layout that placed none leaves none on screen.
    pub fn clear(&self) {
        self.rects.clear();
    }
}

thread_local! {
    /// The previews of the tree the host on this thread lays out ([`install`]).
    static CURRENT: RefCell<Option<Previews>> = const { RefCell::new(None) };
}

/// Report every preview laid out on this thread from now on into `previews`, until the returned
/// guard is dropped: how a host shares one handle with the compositor drawing its pictures. A
/// guard that is already installed nests and restores the one before.
#[must_use = "the previews hold only while the guard lives"]
pub fn install(previews: Previews) -> Installed {
    let previous = CURRENT.with(|current| current.borrow_mut().replace(previews));
    Installed { previous }
}

/// The previews installed by [`install`], restored when dropped.
pub struct Installed {
    previous: Option<Previews>,
}

impl Drop for Installed {
    fn drop(&mut self) {
        let previous = self.previous.take();
        CURRENT.with(|current| *current.borrow_mut() = previous);
    }
}

/// Report `rect` as the preview of `window` to the previews installed on this thread, if any.
fn report(window: &str, rect: Rect) {
    CURRENT.with(|current| {
        if let Some(previews) = current.borrow().as_ref() {
            previews.place(window, rect);
        }
    });
}

/// Forget the preview of `window` on the previews installed on this thread, if any: it left the
/// screen.
fn forget(window: &str) {
    CURRENT.with(|current| {
        if let Some(previews) = current.borrow().as_ref() {
            previews.remove(window);
        }
    });
}

/// A window's live picture, at the rectangle the preview stands at: the compositor draws the
/// window's own pixels there, so the preview draws nothing and takes no input.
#[derive(Component, Default)]
#[component(
    name = "WindowPreview",
    role = "Group",
    label = "window",
    not_in_gallery = "a preview draws nothing: the compositor draws the window's own pixels there, and a gallery entry has no window to show"
)]
pub struct WindowPreviewProps {
    /// The window whose picture shows here: its key in the desk's reading (`Win.key`).
    #[component(required, positional)]
    #[component(data)]
    pub window: String,
}

/// See the module docs.
pub struct WindowPreview {
    window: String,
    rect: Rect,
    /// Whether the preview stands on a laid-out tree: it reports on every layout while it does,
    /// so a host that installs another handle (a new picture source, a test) sees it at once,
    /// and reports its leave when it goes.
    on_screen: bool,
}

impl WindowPreview {
    /// A preview for window `window`.
    #[must_use]
    pub fn new(window: impl Into<String>) -> Self {
        Self {
            window: window.into(),
            rect: Rect::ZERO,
            on_screen: false,
        }
    }

    /// The window whose picture shows in it.
    #[must_use]
    pub fn window(&self) -> &str {
        &self.window
    }
}

impl Drop for WindowPreview {
    fn drop(&mut self) {
        if self.on_screen {
            forget(&self.window);
        }
    }
}

impl Described for WindowPreview {
    type Props = WindowPreviewProps;

    fn from_props(_key: &str, props: WindowPreviewProps) -> Self {
        Self::new(props.window)
    }

    fn set_props(&mut self, props: WindowPreviewProps) -> bool {
        if self.window == props.window {
            return true;
        }
        if self.on_screen {
            forget(&self.window);
        }
        self.window = props.window;
        true
    }
}

impl Widget for WindowPreview {
    fn name(&self) -> &'static str {
        "WindowPreview"
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(
            Style {
                size: taffy::Size {
                    width: taffy::Dimension::percent(1.0),
                    height: taffy::Dimension::auto(),
                },
                aspect_ratio: Some(WINDOW_SHAPE),
                min_size: taffy::Size::from_lengths(1.0, 1.0),
                ..Style::default()
            },
            node_map,
            self,
        )
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        self.on_screen = true;
        report(&self.window, self.rect);
    }

    fn draw(
        &mut self,
        _scene: &mut Scene,
        _transform: Affine,
        _text: &mut TextRenderer,
        _theme: &Theme,
        _presence: &PresenceCtx,
    ) {
    }

    /// A preview paints nothing, so it is never dirty.
    fn needs_redraw(&self) -> bool {
        false
    }

    /// Placement is the only layout the preview takes.
    fn is_layout_dirty(&self) -> bool {
        false
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}
