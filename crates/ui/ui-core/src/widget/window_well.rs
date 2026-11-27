//! `WindowWell`: where a place's window goes (`docs/designs/app-language/shell.md` §8).
//!
//! A shell view never draws a place's content: it puts a well where the content goes, and the
//! app's own window shows there, composited above the shell layer. The well draws nothing and
//! takes no input. After each layout, and on each frame an enclosing component moves it (a
//! canvas's pan and zoom, a strip row's scroll), it reports its rectangle in layout coordinates
//! to the host's [`Wells`], keyed by the place it stands for. The window policy that places
//! windows reads those rectangles, so a well that moves without changing size moves its window
//! in the same frame.
//!
//! The well is a leaf that fills the rect its frame leaves: a frame's bar and chrome stand
//! outside it, in the layout around it.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use taffy::{NodeId, Style, TaffyTree};
use vello::kurbo::{Point, Rect, Vec2};

use tilo_ui_base::{component::Component, component::Described};

use tilo_ui_base::theme::Theme;
use tilo_ui_base::{widget::TaffyTreeExt, widget::Widget};

/// Where the wells of a tree on this thread report, shared with the window policy that places
/// the windows they stand for: the same handle, so a well that moves moves its window.
///
/// A host that shows a shell's views in the compositor's process installs one handle
/// ([`install`]) before it lays the tree out; the well reports into it on every layout. With
/// none installed, a well reports nowhere and costs one thread-local read.
#[derive(Clone, Default)]
pub struct Wells {
    /// Every well, keyed by the part that laid it out and the place it stands for: a shell
    /// drawing several displays lays one place out on each, so the part is what tells the two
    /// apart. A handle with no part keys its wells by the place alone.
    placed: Rc<RefCell<BTreeMap<WellKey, Rect>>>,
    /// Which well last reported each place ([`WindowWell`]'s own number): a well that leaves
    /// forgets its place only while the place is still its own, so a view rebuilt in place,
    /// whose new well reported the place before the old one was let go, keeps the new one.
    reporters: Rc<RefCell<BTreeMap<WellKey, u64>>>,
    generation: Rc<std::cell::Cell<u64>>,
    /// The places whose own window has mapped here ([`Self::note_mapped`]): a well shows the
    /// skeleton until then, and fades it out after.
    mapped: Rc<RefCell<std::collections::BTreeSet<String>>>,
    /// The other people's pointers each part's views drew over the window of a place
    /// ([`crate::widget::PeerPointers`]), by the part and the place: each one's name and where
    /// it stands across that window.
    pointers: Rc<RefCell<BTreeMap<WellKey, Pointed>>>,
    /// The frame each part's views drew round the work while the person follows someone
    /// ([`crate::widget::FollowFrame`]), by the part: the followed person's colour, `rrggbb`, and
    /// the rectangle it runs round, in the layout's logical coordinates.
    frames: Rc<RefCell<BTreeMap<String, (String, Rect)>>>,
    /// The part this handle's wells belong to (a display's id, as the shell names its parts):
    /// empty for a handle that is not a part's.
    part: Rc<String>,
    /// Where the tree reporting through this handle stands in the layout: a tree lays its wells
    /// out in its own coordinates, and they are kept in the layout's, where the windows go.
    origin: Vec2,
}

/// A well by the part that laid it out and the place it stands for.
type WellKey = (String, String);

impl Wells {
    /// A handle with no well on it.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// This handle as `part`'s: what it places is the well that part laid out, so two parts
    /// drawing one place keep both wells instead of the last one winning.
    #[must_use]
    pub fn for_part(&self, part: impl Into<String>) -> Self {
        Self {
            placed: Rc::clone(&self.placed),
            reporters: Rc::clone(&self.reporters),
            generation: Rc::clone(&self.generation),
            mapped: Rc::clone(&self.mapped),
            pointers: Rc::clone(&self.pointers),
            frames: Rc::clone(&self.frames),
            part: Rc::new(part.into()),
            origin: Vec2::ZERO,
        }
    }

    /// This handle for a tree that stands at `origin` in the layout: a display's part draws
    /// from its display's corner, and the wells it lays out stand there too, not over the first
    /// display.
    #[must_use]
    pub fn at(&self, origin: Vec2) -> Self {
        Self {
            origin,
            ..self.clone()
        }
    }

    /// Say place `place`'s own window mapped here: its well stops showing the skeleton and fades
    /// it out under the window.
    pub fn note_mapped(&self, place: &str) {
        self.mapped.borrow_mut().insert(place.to_owned());
    }

    /// Forget that place `place`'s window mapped: its well shows the skeleton again.
    pub fn note_unmapped(&self, place: &str) {
        self.mapped.borrow_mut().remove(place);
    }

    /// Whether place `place`'s own window has mapped here.
    #[must_use]
    pub fn is_mapped(&self, place: &str) -> bool {
        self.mapped.borrow().contains(place)
    }

    /// Whether `other` is this same handle: the same wells, as the same part's.
    #[must_use]
    pub(crate) fn is(&self, other: &Self) -> bool {
        Rc::ptr_eq(&self.placed, &other.placed) && self.part == other.part
    }

    /// The part this handle's wells belong to.
    #[must_use]
    pub fn part(&self) -> &str {
        &self.part
    }

    /// Where the well of place `place` stands, in the layout's logical coordinates, as of the
    /// last layout that placed it. A handle that is not a part's answers the most recent part
    /// that placed it; a part's answers its own.
    #[must_use]
    pub fn well(&self, place: &str) -> Option<Rect> {
        if !self.part.is_empty() {
            return self.well_on(&self.part, place);
        }
        self.well_anywhere(place)
    }

    /// Where the window of place `place` stands on any part: the one laid out with no part first.
    pub(crate) fn well_anywhere(&self, place: &str) -> Option<Rect> {
        let placed = self.placed.borrow();
        placed
            .get(&(String::new(), place.to_owned()))
            .or_else(|| {
                placed
                    .iter()
                    .find(|((_, held), _)| held == place)
                    .map(|(_, rect)| rect)
            })
            .copied()
    }

    /// Where `part`'s well of place `place` stands: the well that display's shell laid out.
    #[must_use]
    pub fn well_on(&self, part: &str, place: &str) -> Option<Rect> {
        self.placed
            .borrow()
            .get(&(part.to_owned(), place.to_owned()))
            .copied()
    }

    /// Every well on screen, by place, as of the last layout: this part's while it is one's.
    #[must_use]
    pub fn wells(&self) -> BTreeMap<String, Rect> {
        let placed = self.placed.borrow();
        placed
            .iter()
            .filter(|((part, _), _)| self.part.is_empty() || part.as_str() == self.part.as_str())
            .map(|((_, place), rect)| (place.clone(), *rect))
            .collect()
    }

    /// Whether any well stands on a tree at all: empty after a layout that placed none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        let placed = self.placed.borrow();
        if self.part.is_empty() {
            placed.is_empty()
        } else {
            !placed
                .keys()
                .any(|(part, _)| part.as_str() == self.part.as_str())
        }
    }

    /// Moves with every layout that placed a well, so a reader of [`Self::well`] knows whether
    /// the last answer still stands: a canvas pan, a scroll and a resize each move it.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation.get()
    }

    /// Record `rect`, in the coordinates of the tree reporting through this handle, as the well
    /// of `place`, on this handle's part.
    pub fn place(&self, place: &str, rect: Rect) {
        let rect = rect + self.origin;
        let key = ((*self.part).clone(), place.to_owned());
        let mut placed = self.placed.borrow_mut();
        if placed.get(&key).copied() == Some(rect) {
            return;
        }
        placed.insert(key, rect);
        self.generation.set(self.generation.get().wrapping_add(1));
    }

    /// Record `rect` as the well of `place`, reported by the well numbered `well`.
    fn place_from(&self, place: &str, rect: Rect, well: u64) {
        let key = ((*self.part).clone(), place.to_owned());
        self.reporters.borrow_mut().insert(key, well);
        self.place(place, rect);
    }

    /// Forget the well of `place` that the well numbered `well` reported, if the place is still
    /// that well's: another well that reported it since stands there now.
    fn remove_from(&self, place: &str, well: u64) {
        let key = ((*self.part).clone(), place.to_owned());
        if self.reporters.borrow().get(&key) == Some(&well) {
            self.remove(place);
        }
    }

    /// Forget the well of `place` on this handle's part: it left the screen (its place scrolled
    /// away, or the shell shows another workspace and does not draw it).
    pub fn remove(&self, place: &str) {
        let key = ((*self.part).clone(), place.to_owned());
        self.reporters.borrow_mut().remove(&key);
        if self.placed.borrow_mut().remove(&key).is_some() {
            self.generation.set(self.generation.get().wrapping_add(1));
        }
    }

    /// Forget every well this handle's part placed: a layout that placed none leaves nothing on
    /// screen for that part. A handle that is not a part's forgets every well.
    pub fn clear(&self) {
        let mut placed = self.placed.borrow_mut();
        let before = placed.len();
        if self.part.is_empty() {
            placed.clear();
            self.reporters.borrow_mut().clear();
        } else {
            placed.retain(|(part, _), _| part.as_str() != self.part.as_str());
            self.reporters
                .borrow_mut()
                .retain(|(part, _), _| part.as_str() != self.part.as_str());
        }
        if placed.len() != before {
            self.generation.set(self.generation.get().wrapping_add(1));
        }
    }
}

/// The pointers drawn over the window of one place: each one's name and where it stands across
/// that window, and the rectangle they stand in while the place has no well on screen.
struct Pointed {
    over: Rect,
    at: Vec<(String, (f64, f64))>,
}

impl Wells {
    /// Record the pointers drawn over the window of `place`, on this handle's part: each one's
    /// name and where it stands across the window, `over` (in the coordinates of the tree
    /// reporting through this handle, as a well's are) standing in for the window while the place
    /// has no well. None drawn forgets the place.
    pub fn point(&self, place: &str, over: Rect, at: Vec<(String, (f64, f64))>) {
        let key = ((*self.part).clone(), place.to_owned());
        let mut pointers = self.pointers.borrow_mut();
        if at.is_empty() {
            pointers.remove(&key);
        } else {
            pointers.insert(
                key,
                Pointed {
                    over: over + self.origin,
                    at,
                },
            );
        }
    }

    /// Every other person's pointer drawn over a window as of the last frame, by name and where
    /// it stands on the surface: measured from where the window's well stands now, so a well
    /// placed after the pointers were reported still carries them. This part's while the handle
    /// is one's.
    #[must_use]
    pub fn pointers(&self) -> Vec<(String, (f64, f64))> {
        self.pointers
            .borrow()
            .iter()
            .filter(|((part, _), _)| self.part.is_empty() || part.as_str() == self.part.as_str())
            .flat_map(|((part, place), pointed)| {
                // The window's well on the part that drew the pointers, else wherever that place's
                // window stands: a part that laid the pointers out before its display was named
                // still draws them over the window.
                let window = self
                    .well_on(part, place)
                    .or_else(|| self.well_anywhere(place))
                    .unwrap_or(pointed.over);
                pointed
                    .at
                    .iter()
                    .map(move |(name, (x, y))| (name.clone(), (window.x0 + x, window.y0 + y)))
            })
            .collect()
    }
}

impl Wells {
    /// Record the frame drawn round the work on this handle's part: the followed person's colour
    /// and the rectangle it runs round, in the coordinates of the tree reporting through this
    /// handle, as a well's are. None forgets it.
    pub fn frame(&self, drawn: Option<(String, Rect)>) {
        let mut frames = self.frames.borrow_mut();
        match drawn {
            Some((colour, rect)) => {
                frames.insert((*self.part).clone(), (colour, rect + self.origin));
            }
            None => {
                frames.remove(self.part.as_str());
            }
        }
    }

    /// Every frame drawn round the work as of the last layout, by the followed person's colour
    /// and the rectangle it runs round: this part's while the handle is one's.
    #[must_use]
    pub fn frames(&self) -> Vec<(String, Rect)> {
        self.frames
            .borrow()
            .iter()
            .filter(|(part, _)| self.part.is_empty() || part.as_str() == self.part.as_str())
            .map(|(_, drawn)| drawn.clone())
            .collect()
    }
}

/// The wells installed on this thread, if any: a widget drawn over a window keeps them, to read
/// where its window stands and report what it drew when it changes between layouts.
pub(crate) fn current() -> Option<Wells> {
    CURRENT.with(|current| current.borrow().clone())
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

/// Whether place `place`'s own window has mapped here, as the wells installed on this thread say.
fn mapped_here(place: &str) -> bool {
    CURRENT.with(|current| {
        current
            .borrow()
            .as_ref()
            .is_some_and(|wells| wells.is_mapped(place))
    })
}

thread_local! {
    /// The number the next well built on this thread takes: what tells two wells of one place
    /// apart while a rebuilt view's new well stands before the old one is let go.
    static NEXT_WELL: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
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
    /// The place whose window shows here: the tab id the shell model gives the window when
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
    /// This well's own number, which its reports carry ([`Wells`]'s reporters).
    id: u64,
    rect: Rect,
    /// What the well shows: the skeleton while the place's window has not mapped, fading out
    /// under the window once it does.
    arrival: tilo_ui_base::motion::CrossFade<bool>,
    /// The wells it reported to at its last layout, while it stands on a laid-out tree: it
    /// reports on every layout, so a host that installs another handle (a new policy, a test, the
    /// part of a display named since) sees it at once, and the handle before forgets it; it
    /// reports its leave to them when it goes, whatever is installed then.
    reported: Option<super::Wells>,
}

impl WindowWell {
    /// A well for the place `place`.
    #[must_use]
    pub fn new(place: impl Into<String>) -> Self {
        let id = NEXT_WELL.with(|next| {
            let id = next.get();
            next.set(id.wrapping_add(1));
            id
        });
        Self {
            place: place.into(),
            id,
            rect: Rect::ZERO,
            arrival: tilo_ui_base::motion::CrossFade::new(false),
            reported: None,
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
        if let Some(wells) = &self.reported {
            wells.remove_from(&self.place, self.id);
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
        if let Some(wells) = &self.reported {
            wells.remove_from(&self.place, self.id);
        }
        self.place = props.place;
        // The well stands for another window now: it shows what is coming for that one.
        self.arrival = tilo_ui_base::motion::CrossFade::new(mapped_here(&self.place));
        true
    }
}

impl Widget for WindowWell {
    fn name(&self) -> &'static str {
        "WindowWell"
    }

    /// The window of the place it shows, standing here: a page's host fills it with what stands
    /// for that window ([`crate::composition::page::well_place`]).
    fn accessibility(&self) -> Option<tilo_ui_base::a11y::AccessNode> {
        Some(
            tilo_ui_base::a11y::AccessNode::new(tilo_ui_base::a11y::Role::Window)
                .with_item(self.place.clone()),
        )
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
        self.rect = tilo_ui_base::widget::layout_rect(taffy, node, origin);
        let wells = current();
        if let Some(before) = self
            .reported
            .as_ref()
            .filter(|before| !wells.as_ref().is_some_and(|now| now.is(before)))
        {
            before.remove_from(&self.place, self.id);
        }
        if let Some(wells) = &wells {
            wells.place_from(&self.place, self.rect, self.id);
        }
        self.reported = wells;
    }

    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        let (scene, transform, text, theme, _env) = ctx.parts();
        let now = tilo_ui_base::motion::frame_now();
        // The window's own frame is composited over the well: until it maps, the well shows the
        // skeleton of what is coming; once it does, the skeleton fades out under it.
        self.arrival
            .arrive(mapped_here(&self.place), theme.motion(), now);
        let skeleton = if *self.arrival.shown() {
            self.arrival
                .outgoing(now)
                .and_then(|(was, opacity)| (!*was).then_some(opacity))
        } else {
            Some(self.arrival.incoming(now))
        };
        let Some(opacity) = skeleton.filter(|opacity| *opacity > 0.0) else {
            return;
        };
        let mut paint = tilo_ui_base::paint::PaintCtx::new(scene, transform, text, theme);
        if opacity < 1.0 {
            paint.scene.push_layer(
                vello::peniko::Fill::NonZero,
                vello::peniko::BlendMode::default(),
                opacity,
                paint.transform,
                &self.rect.inflate(1.0, 1.0),
            );
        }
        crate::widget::SkeletonFace::new(tilo_ui_base::widget::SkeletonShape::Page)
            .paint(&mut paint, self.rect);
        if opacity < 1.0 {
            paint.scene.pop_layer();
        }
    }

    /// While the skeleton fades out, the well is dirty; while unmapped its skeleton is still.
    fn needs_redraw(&self) -> bool {
        self.arrival.is_moving(tilo_ui_base::motion::frame_now())
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

    /// Two parts of a shell drawing one strip's place keep both wells, each by its own display:
    /// the window policy places each window by the well on the display it stands on, rather than
    /// by whichever part laid it out last.
    #[test]
    fn a_place_on_two_parts_keeps_a_well_on_each() {
        let wells = super::Wells::new();
        let first = wells.for_part("0");
        let second = wells.for_part("1");
        let on_first = vello::kurbo::Rect::new(0.0, 0.0, 400.0, 300.0);
        let on_second = vello::kurbo::Rect::new(1280.0, 0.0, 1680.0, 300.0);
        first.place("place", on_first);
        second.place("place", on_second);
        assert_eq!(wells.well_on("0", "place"), Some(on_first));
        assert_eq!(wells.well_on("1", "place"), Some(on_second));
        assert_eq!(first.well("place"), Some(on_first));
        assert_eq!(second.well("place"), Some(on_second));
    }

    /// Pointers reported over a place stand across its window wherever the well is placed, the
    /// well laid out after them included, and over the room they were drawn in while the place
    /// has no well; none forgets them.
    #[test]
    fn pointers_stand_over_the_well_wherever_it_is_placed() {
        let wells = super::Wells::new();
        let part = wells.for_part("0");
        let room = vello::kurbo::Rect::new(0.0, 88.0, 800.0, 600.0);
        part.point("7", room, vec![("Ada".to_owned(), (12.0, 34.0))]);
        assert_eq!(
            wells.pointers(),
            vec![("Ada".to_owned(), (12.0, 122.0))],
            "no well yet: over the room drawn in"
        );
        part.place("7", vello::kurbo::Rect::new(0.0, 80.0, 800.0, 600.0));
        assert_eq!(
            wells.pointers(),
            vec![("Ada".to_owned(), (12.0, 114.0))],
            "over the window, placed after the pointer"
        );
        part.point("7", room, Vec::new());
        assert!(wells.pointers().is_empty(), "gone with the pointer");
    }

    /// Pointers laid out before their display was named stand over the window all the same.
    #[test]
    fn pointers_of_a_part_with_no_well_stand_over_the_window() {
        let wells = super::Wells::new();
        wells
            .for_part("0")
            .place("7", vello::kurbo::Rect::new(0.0, 80.0, 800.0, 600.0));
        let room = vello::kurbo::Rect::new(0.0, 88.0, 800.0, 600.0);
        wells
            .for_part("")
            .point("7", room, vec![("Ada".to_owned(), (12.0, 34.0))]);
        assert_eq!(
            wells.pointers(),
            vec![("Ada".to_owned(), (12.0, 114.0))],
            "over the window, wherever its well was laid out"
        );
    }

    /// A well laid out before its display was named, then for that display's part, stands only
    /// where it was laid out last, and nowhere once it goes.
    #[test]
    fn a_well_laid_out_again_for_another_part_stands_only_there() {
        let wells = super::Wells::new();
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn tilo_ui_base::widget::Widget> = HashMap::new();
        let mut well = WindowWell::new("7");
        let node = well.build_layout(&mut taffy, &Theme::dark(), &mut node_map);
        taffy
            .compute_layout(
                node,
                Size {
                    width: AvailableSpace::Definite(800.0),
                    height: AvailableSpace::Definite(500.0),
                },
            )
            .unwrap();
        {
            let _installed = super::install(wells.for_part(""));
            well.apply_layout(&taffy, node, vello::kurbo::Point::new(0.0, 80.0));
        }
        {
            let _installed = super::install(wells.for_part("0"));
            well.apply_layout(&taffy, node, vello::kurbo::Point::new(0.0, 88.0));
        }
        assert_eq!(wells.well_on("", "7"), None, "the part before forgets it");
        assert_eq!(wells.well("7").map(|rect| rect.y0), Some(88.0));
        drop(well);
        assert_eq!(
            wells.well("7"),
            None,
            "gone with the well, whatever is installed"
        );
    }

    /// A display's part lays its tree out from its own corner: the well it lays out in the
    /// corner of the display beside the first stands beside it in the layout, where the window
    /// goes, not over the first display.
    #[test]
    fn a_parts_well_stands_where_the_part_does() {
        let wells = super::Wells::new();
        let beside = wells.for_part("1").at(vello::kurbo::Vec2::new(1280.0, 0.0));
        beside.place("place", vello::kurbo::Rect::new(0.0, 40.0, 400.0, 340.0));
        assert_eq!(
            wells.well_on("1", "place"),
            Some(vello::kurbo::Rect::new(1280.0, 40.0, 1680.0, 340.0))
        );
    }

    use taffy::{AvailableSpace, FlexDirection, NodeId, Size, Style, TaffyTree};

    use super::{WINDOW_ASPECT, WindowWell};

    use tilo_ui_base::theme::Theme;
    use tilo_ui_base::widget::Widget as _;

    /// The size the well takes in a column shaped by `container`.
    fn well_in(container: Style) -> (f32, f32) {
        let mut taffy = TaffyTree::<()>::new();
        let mut node_map: HashMap<NodeId, *mut dyn tilo_ui_base::widget::Widget> = HashMap::new();
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

    tilo_test_env::scenario! {
        fn a_well_stands_a_windows_shape_or_fills_its_frame;

        phase a_well_in_a_column_that_hugs_stands_a_window_height;
        // A column that hugs what it holds names the well a width and no height: the well still
        // stands a window's shape, so the window in it has a face to draw rather than a line.
        {
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

        phase a_well_in_a_framed_column_fills_the_height_it_is_given;
        // A frame that names the well a height gets it back whole: the well fills the rect it was
        // given, whatever shape a window is.
        {
            let (_, height) = well_in(column(taffy::Dimension::length(300.0)));
            assert!(
                (height - 300.0).abs() < 1.0,
                "the well fills its frame: {height}"
            );
        }
    }

    /// A well shows what is coming until its window maps, then the skeleton fades out under the
    /// window; a window that goes brings the skeleton back.
    #[test]
    fn a_well_shows_a_skeleton_until_its_window_maps_then_fades_it() {
        let wells = super::Wells::new();
        let _installed = super::install(wells.clone());
        let mut well = super::WindowWell::new("place");
        well.rect = vello::kurbo::Rect::new(0.0, 0.0, 320.0, 200.0);
        let painted = |well: &mut super::WindowWell| {
            let mut scene = vello::Scene::new();
            let mut text = tilo_ui_base::text_render::TextRenderer::new();
            let theme = tilo_ui_base::theme::Theme::dark();
            tilo_ui_base::widget::Widget::draw(
                well,
                &mut tilo_ui_base::draw::DrawCtx::new(
                    &mut scene,
                    vello::kurbo::Affine::IDENTITY,
                    &mut text,
                    &theme,
                    tilo_ui_base::presence::PresenceCtx::default().draw_env(),
                ),
            );
            scene.encoding().draw_tags.len()
        };
        assert!(
            painted(&mut well) > 0,
            "a window that has not mapped shows the skeleton of what is coming"
        );
        wells.note_mapped("place");
        assert!(
            painted(&mut well) > 0,
            "the skeleton is still drawn on the frame its window maps"
        );
        {
            let _settled = tilo_ui_base::motion::testing::settled();
            assert_eq!(
                painted(&mut well),
                0,
                "once faded, the well paints nothing: the window itself is there"
            );
        }
        wells.note_unmapped("place");
        {
            let _settled = tilo_ui_base::motion::testing::settled();
            assert!(
                painted(&mut well) > 0,
                "a window that goes shows the skeleton again"
            );
        }
    }
}
