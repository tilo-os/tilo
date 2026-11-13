//! `WindowWell`: where a place's window goes (`docs/designs/app-language/shell.md` §8).
//!
//! A shell view never draws a place's content: it puts a well where the content goes, and the
//! app's own window shows there, composited above the desk layer. The well draws nothing and
//! takes no input. After each layout, and on each frame an enclosing component moves it (a
//! canvas's pan and zoom, a strip row's scroll), it reports its rectangle in layout coordinates
//! to the host's [`Wells`], keyed by the place it stands for. The window policy that places
//! windows reads those rectangles, so a well that moves without changing size moves its window
//! in the same frame.
//!
//! The well is a leaf that fills the room its frame leaves it: a frame's bar and chrome stand
//! outside it, in the layout around it.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use taffy::{NodeId, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use crate::component::{Component, Described};
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::{TaffyTreeExt, Widget};

/// Where the wells of a tree on this thread report, shared with the window policy that places
/// the windows they stand for: the same handle, so a well that moves moves its window.
///
/// A host that shows a shell's views in the compositor's process installs one handle
/// ([`install`]) before it lays the tree out; the well reports into it on every layout. With
/// none installed, a well reports nowhere and costs one thread-local read.
#[derive(Clone, Default)]
pub struct Wells {
    placed: Rc<RefCell<BTreeMap<String, Rect>>>,
    generation: Rc<std::cell::Cell<u64>>,
}

impl Wells {
    /// A handle with no well on it.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Where the well of place `place` stands, in the layout's logical coordinates, as of the
    /// last layout that placed it.
    #[must_use]
    pub fn well(&self, place: &str) -> Option<Rect> {
        self.placed.borrow().get(place).copied()
    }

    /// Every well on screen, by place, as of the last layout.
    #[must_use]
    pub fn wells(&self) -> BTreeMap<String, Rect> {
        self.placed.borrow().clone()
    }

    /// Whether any well stands on a tree at all: empty after a layout that placed none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.placed.borrow().is_empty()
    }

    /// Moves with every layout that placed a well, so a reader of [`Self::well`] knows whether
    /// the last answer still stands: a canvas pan, a scroll and a resize each move it.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation.get()
    }

    /// Record `rect` as the well of `place`.
    pub fn place(&self, place: &str, rect: Rect) {
        let mut placed = self.placed.borrow_mut();
        if placed.get(place).copied() == Some(rect) {
            return;
        }
        placed.insert(place.to_owned(), rect);
        self.generation.set(self.generation.get().wrapping_add(1));
    }

    /// Forget the well of `place`: it left the screen (its place scrolled away, or the shell
    /// shows another workspace and does not draw it).
    pub fn remove(&self, place: &str) {
        if self.placed.borrow_mut().remove(place).is_some() {
            self.generation.set(self.generation.get().wrapping_add(1));
        }
    }

    /// Forget every well: a layout that placed none leaves nothing on screen.
    pub fn clear(&self) {
        let mut placed = self.placed.borrow_mut();
        if placed.is_empty() {
            return;
        }
        placed.clear();
        self.generation.set(self.generation.get().wrapping_add(1));
    }
}

thread_local! {
    /// The wells of the tree the host on this thread lays out ([`install`]).
    static CURRENT: RefCell<Option<Wells>> = const { RefCell::new(None) };
}

/// Report every well laid out on this thread from now on into `wells`, until the returned
/// guard is dropped: how a host shares one handle with the policy placing its windows. A guard
/// that is already installed nests and restores the one before.
#[must_use = "the wells hold only while the guard lives"]
pub fn install(wells: Wells) -> Installed {
    let previous = CURRENT.with(|current| current.borrow_mut().replace(wells));
    Installed { previous }
}

/// The wells installed by [`install`], restored when dropped.
pub struct Installed {
    previous: Option<Wells>,
}

impl Drop for Installed {
    fn drop(&mut self) {
        let previous = self.previous.take();
        CURRENT.with(|current| *current.borrow_mut() = previous);
    }
}

/// Report `rect` as the well of `place` to the wells installed on this thread, if any.
fn report(place: &str, rect: Rect) {
    CURRENT.with(|current| {
        if let Some(wells) = current.borrow().as_ref() {
            wells.place(place, rect);
        }
    });
}

/// Forget the well of `place` on the wells installed on this thread, if any: it left the screen.
fn forget(place: &str) {
    CURRENT.with(|current| {
        if let Some(wells) = current.borrow().as_ref() {
            wells.remove(place);
        }
    });
}

/// A place's window, at the rectangle the well stands at: the app's own window shows there, so
/// the well draws nothing and takes no input.
#[derive(Component, Default)]
#[component(
    name = "WindowWell",
    role = "Group",
    label = "place",
    not_in_gallery = "a well draws nothing: the place's own window is composited there, so a gallery window has no well to show"
)]
pub struct WindowWellProps {
    /// The place whose window shows here: the tab id the desktop model gives the window when
    /// it maps (shell.md §8).
    #[component(required, positional)]
    #[component(data)]
    pub place: String,
}

/// A window's shape, width over height: the shape a place opens at, which a well gives the
/// window it stands for when its frame names a width and no height.
const WINDOW_ASPECT: f32 = 1.6;

/// See the module docs.
pub struct WindowWell {
    place: String,
    rect: Rect,
    /// Whether the well stands on a laid-out tree: it reports on every layout while it does,
    /// so a host that installs another handle (a new policy, a test) sees it at once, and
    /// reports its leave when it goes.
    on_screen: bool,
}

impl WindowWell {
    /// A well for the place `place`.
    #[must_use]
    pub fn new(place: impl Into<String>) -> Self {
        Self {
            place: place.into(),
            rect: Rect::ZERO,
            on_screen: false,
        }
    }

    /// The place whose window shows in it.
    #[must_use]
    pub fn place(&self) -> &str {
        &self.place
    }
}

impl Drop for WindowWell {
    fn drop(&mut self) {
        if self.on_screen {
            forget(&self.place);
        }
    }
}

impl Described for WindowWell {
    type Props = WindowWellProps;

    fn from_props(_key: &str, props: WindowWellProps) -> Self {
        Self::new(props.place)
    }

    fn set_props(&mut self, props: WindowWellProps) -> bool {
        if self.place == props.place {
            return true;
        }
        if self.on_screen {
            forget(&self.place);
        }
        self.place = props.place;
        true
    }
}

impl Widget for WindowWell {
    fn name(&self) -> &'static str {
        "WindowWell"
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        taffy.widget_leaf(
            Style {
                size: taffy::Size::from_percent(1.0, 1.0),
                min_size: taffy::Size::from_lengths(1.0, 1.0),
                aspect_ratio: Some(WINDOW_ASPECT),
                ..Style::default()
            },
            node_map,
            self,
        )
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = crate::widget::layout_rect(taffy, node, origin);
        self.on_screen = true;
        report(&self.place, self.rect);
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

    /// A well paints nothing, so it is never dirty.
    fn needs_redraw(&self) -> bool {
        false
    }

    /// Placement is the only layout the well takes.
    fn is_layout_dirty(&self) -> bool {
        false
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    /// The well is found by the place it stands for, as the window it holds its place for is:
    /// a reader of the tree knows which window shows where, without the layout's wells.
    fn test_id(&self) -> Option<&str> {
        (!self.place.is_empty()).then_some(self.place.as_str())
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use taffy::{AvailableSpace, FlexDirection, NodeId, Size, Style, TaffyTree};

    use super::{WINDOW_ASPECT, WindowWell};
    use crate::theme::Theme;
    use crate::widget::Widget as _;

    /// The size the well takes in a column shaped by `container`.
    fn well_in(container: Style) -> (f32, f32) {
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn crate::widget::Widget> = HashMap::new();
        let node = WindowWell::new("place").build_layout(&mut taffy, &Theme::dark(), &mut node_map);
        let column = taffy.new_with_children(container, &[node]).unwrap();
        taffy
            .compute_layout(
                column,
                Size {
                    width: AvailableSpace::Definite(800.0),
                    height: AvailableSpace::MaxContent,
                },
            )
            .unwrap();
        let size = taffy.layout(node).unwrap().size;
        (size.width, size.height)
    }

    fn column(height: taffy::Dimension) -> Style {
        Style {
            flex_direction: FlexDirection::Column,
            size: Size {
                width: taffy::Dimension::length(800.0),
                height,
            },
            ..Style::default()
        }
    }

    /// A column that hugs what it holds names the well a width and no height: the well still
    /// stands a window's shape, so the window in it has a face to draw rather than a line.
    #[test]
    fn a_well_in_a_column_that_hugs_stands_a_window_height() {
        let (width, height) = well_in(column(taffy::Dimension::auto()));
        assert!(
            (width - 800.0).abs() < 1.0,
            "the well takes the column's width, {width}"
        );
        assert!(
            (height - 800.0 / WINDOW_ASPECT).abs() < 1.0,
            "the well stands a window's height, not a line: {height}"
        );
    }

    /// A frame that names the well a height gets it back whole: the well fills the room it was
    /// given, whatever shape a window is.
    #[test]
    fn a_well_in_a_framed_column_fills_the_height_it_is_given() {
        let (_, height) = well_in(column(taffy::Dimension::length(300.0)));
        assert!(
            (height - 300.0).abs() < 1.0,
            "the well fills its frame: {height}"
        );
    }
}
