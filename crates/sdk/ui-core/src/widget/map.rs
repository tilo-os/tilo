//! `Map`: children laid out at points on a surface that pans and zooms, as a described
//! component.
//!
//! A canvas, a diagram, a map of devices: what the person arranged is *where things are*, in
//! whole canvas pixels, and the view is the camera that looks at it. `layout.md`'s "no absolute
//! positions" is about a view's own layout; a canvas is placed content (`shell.md` §2), so its
//! places are points and this widget is what draws them.
//!
//! The camera is the widget's own state, a lens: personal, kept on this device, never in the
//! person's layout. Panning and zooming move and scale the pins; a change of the pins or of
//! the camera lays them out again. Every motion of the map (a pin gliding to a new place, the
//! camera gliding to what it shows) is laid out on each frame it runs, as a scroll's glide is,
//! so what stands in a pin (a window's well) is where the pin is drawn. At rest the map asks
//! for no frames.
//!
//! Each child is a `MapPin`: its key, its `x` and `y` in canvas pixels, its width, and the view
//! it shows. The map changes nothing itself: the caller commits one arranging action when the
//! person moves a place.
//!
//! A place that arrives on the map is shown: the camera pans the least it takes for it to stand
//! whole inside the viewport, as a scroll shows the row it was asked for, and no place moves.
//! The lens is the person's, so a map built again (around changed places, or with whatever holds
//! it) takes the camera over from the one it replaces (`Lens`) rather than looking at the
//! canvas' origin.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use taffy::{NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Vec2};
use vello::peniko::{BlendMode, Fill};

use crate::component::{Component, Described, Event, ResourceId, SlotType, SlotViews, Source};
use crate::gesture::{GesturePhase, TouchGestureArena, TouchGestureEvent};
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::DrawChild;
use crate::widget::composition_parts::Parts;
use crate::widget::{
    Element, Manual, ManualInvalidation, MeasureConstraint, Widget, WidgetLayoutError,
};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

/// The smallest and largest zoom the map is shown at: far enough out to see a whole canvas, far
/// enough in to read a note.
pub const MAP_ZOOM_MIN: f64 = 0.2;
pub const MAP_ZOOM_MAX: f64 = 2.5;

/// How much one wheel notch zooms, when a gesture zooms over the map.
const ZOOM_PER_DETENT: f64 = 1.1;

/// The room a place that arrives keeps from the viewport's edges when the camera shows it: a
/// window's frame at the very edge of the canvas reads as cut off, so the pan leaves a gap of
/// one section step.
const ARRIVAL_MARGIN: f64 = crate::tokens::Spacing::BASE.rhythm.section as f64;

/// The band a pin keeps over its view: a place's handle band, or none.
fn band_of(band: bool) -> f64 {
    if band {
        crate::widget::canvas_place::PLACE_BAND
    } else {
        0.0
    }
}

/// A pin's place, in canvas pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PinPlace {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    /// How tall it stands, when the layout says (a window on a desktop); `None` lets its
    /// view say, as a note on a canvas is as tall as what it holds.
    pub height: Option<f64>,
    /// The room over the pin's top edge its view keeps for a handle, in screen pixels: the pin
    /// stands that much higher and taller, so its view's own top stays at `y` (a place on a
    /// canvas, whose handle shows over it). Zero for a pin with nothing over it.
    pub band: f64,
    /// Its height is the most it takes: its view is as tall as what it holds up to `height`, as
    /// an answer hugs its words inside the rectangle the person dragged for it.
    pub hug: bool,
    /// Arriving, it grows out of where the person last pointed ([`MapPinProps::grows`]).
    pub grows: bool,
}

/// The camera: which canvas point stands at the viewport's top-leading corner, and how many
/// screen pixels a canvas pixel takes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MapCamera {
    pub origin: Point,
    pub zoom: f64,
}

impl Default for MapCamera {
    fn default() -> Self {
        Self {
            origin: Point::ORIGIN,
            zoom: 1.0,
        }
    }
}

impl MapCamera {
    /// Where canvas point `p` is on screen, for a viewport whose top-leading corner is at
    /// `corner`.
    #[must_use]
    pub fn to_screen(self, corner: Point, p: Point) -> Point {
        corner + (p - self.origin) * self.zoom
    }

    /// The canvas point under screen point `p`.
    #[must_use]
    pub fn to_canvas(self, corner: Point, p: Point) -> Point {
        self.origin + (p - corner) / self.zoom.max(f64::EPSILON)
    }

    /// Zoom by `factor` about the screen point `about`, keeping the canvas point under it in
    /// place. The zoom stays within [`MAP_ZOOM_MIN`] and [`MAP_ZOOM_MAX`].
    pub fn zoom_about(&mut self, corner: Point, about: Point, factor: f64) {
        let held = self.to_canvas(corner, about);
        self.zoom = (self.zoom * factor).clamp(MAP_ZOOM_MIN, MAP_ZOOM_MAX);
        self.origin = held - (about - corner) / self.zoom;
    }

    /// Move the camera by a screen-space delta.
    pub fn pan_by(&mut self, delta: Vec2) {
        self.origin -= delta / self.zoom.max(f64::EPSILON);
    }

    /// Whether two cameras look at the same canvas the same way.
    #[must_use]
    pub fn same(self, other: Self) -> bool {
        (self.zoom - other.zoom).abs() <= f64::EPSILON
            && (self.origin - other.origin).hypot() <= f64::EPSILON
    }
}

/// Places at points on a pannable, zoomable surface, each a view of its own.
#[derive(Component, Default)]
#[component(name = "Map", role = "Group", label = "name")]
pub struct MapProps {
    /// What the map is, for assistive technology.
    pub name: Option<String>,
    /// Whether it is a desktop: the windows it holds stand on the desktop's ground
    /// ([`SurfaceLevel::Ground`](crate::widget::primitives::SurfaceLevel)), not on the content
    /// colour of the window the map is in.
    pub ground: bool,
    /// The pins, in the order the map draws them (the last on top).
    #[component(slot(default))]
    pub pins: Vec<MapPinProps>,
    /// A resource was dragged onto the map and let go: carries the resource the drag carried,
    /// as its address, and where it was let go — its canvas x, then its canvas y. The view
    /// decides what to do with it; the shell's canvas places it there.
    #[component(event(value = ResourceId, at = Vec<i64>))]
    pub drop: Event,
    /// The pointer pressed on the map's empty surface — not on a pin — and let go there, having
    /// travelled no further than a click: carries the canvas point under the press, its x then
    /// its y, kept apart from a drag and a middle drag. What it reports is where the press landed
    /// at the camera the map looks with, so the point is right at any pan or zoom; the view
    /// decides what stands there (the shell's canvas opens its caret at it).
    #[component(event(value = Vec<i64>))]
    pub press: Event,
    /// A left drag across the map's empty surface, let go after travelling: carries the dragged
    /// rectangle in canvas pixels, its x, its y, its width and its height. The rectangle is drawn
    /// as a light outline while the drag lasts (the theme's focus colour, never a colour of its
    /// own), and the view lays what it answers into that rectangle.
    #[component(event(value = Vec<i64>))]
    pub frame: Event,
    /// The pointer's right button pressed on the map: carries the key of the pin standing under
    /// it — empty text where the press landed on the map's own empty surface — and the canvas
    /// point under the pointer, its x then its y. What the view does with it is its own: the
    /// shell's canvas opens the pointed place's menu, or the canvas's own for free space
    /// (`canvas.md` §1, §6).
    #[component(event(value = String, at = Vec<i64>))]
    pub context: Event,
    /// A place's trailing edge, bottom edge or corner was dragged and let go: carries the key of
    /// the pin it resized and its new size in canvas pixels, its width then its height. Its
    /// corner stays where it stood; the view gives the place that size.
    #[component(event(value = String, at = Vec<i64>))]
    pub resize: Event,
    /// The person pointed on the map's free space (a click, a dragged rectangle, a right press):
    /// carries the part of the canvas they see as they point, in canvas pixels, its x, its y, its
    /// width and its height. It is reported before the pointing itself (`press`, `frame`,
    /// `context`), so what answers it can stand in free room where they look (canvas.md §7,
    /// "Everything stays in view").
    #[component(event(value = Vec<i64>))]
    pub seen: Event,
    /// A rectangle the view points at, in canvas pixels (`x`, `y`, width, height), drawn as the
    /// light outline a drag across the canvas draws: the rectangle the person dragged, kept while
    /// they say what fills it.
    pub outline: Vec<i64>,
    /// What the empty map says once, quietly, at the middle of what it shows: what a person
    /// does on it ("Click anywhere and say what you want"). Drawn only while it holds no pin,
    /// and read to assistive technology as the map's description.
    pub hint: Option<String>,
}

/// One pin of a map, as an instance gives it: its key, its place and the view it shows.
#[derive(Component, Default)]
#[component(name = "MapPin", role = "Group", label = "name")]
pub struct MapPinProps {
    /// The pin's stable key.
    #[component(key)]
    pub key: String,
    /// What it is, for assistive technology.
    pub name: Option<String>,
    /// The canvas point of its top-leading corner, in canvas pixels.
    pub x: Option<i64>,
    /// The canvas y of its top-leading corner, in canvas pixels.
    pub y: Option<i64>,
    /// How wide it stands, in canvas pixels.
    pub width: Option<i64>,
    /// How tall it stands, in canvas pixels; without it, as tall as its view asks.
    pub height: Option<i64>,
    /// Whether its view keeps a place's handle band over its top edge
    /// ([`crate::widget::canvas_place::PLACE_BAND`]): the pin stands that much higher, so the
    /// view's own top is at `y`.
    pub band: bool,
    /// `height` is the most it takes: its view hugs what it holds up to it (an answer in the
    /// rectangle it was asked in), rather than filling it.
    pub hug: bool,
    /// It is an answer: arriving, it grows out of where the person last pointed on the map (the
    /// caret or the rectangle it answers), rather than fading in where it stands. The pointing
    /// itself (a caret) leaves it unset and stands at once.
    pub grows: bool,
    /// What the pin stands for, as the view names it: what pointing at the pin (`context`) and
    /// sizing it (`resize`) report. Without it, the pin's key.
    #[component(data)]
    pub item: Option<String>,
    /// What it shows.
    #[component(slot(default))]
    pub body: Vec<Element>,
}

impl SlotType for Vec<MapPinProps> {
    const MANY: bool = true;

    fn accepts() -> crate::component::Accepts {
        crate::component::Accepts::Component(MapPinProps::NAME.into())
    }

    fn fill(&mut self, source: &mut dyn Source, slot: &str) -> Result<(), String> {
        let mut out = Vec::new();
        let mut failed: Option<String> = None;
        source.components(slot, &mut |child| match MapPinProps::from_source(child) {
            Ok(pin) => {
                out.push(pin);
                Ok(())
            }
            Err(error) => {
                failed = Some(error.to_string());
                Err(error.to_string())
            }
        })?;
        if let Some(error) = failed {
            return Err(error);
        }
        *self = out;
        Ok(())
    }
}

/// A press that may become a pan.
#[derive(Clone, Copy, Debug)]
struct Pan {
    start: Point,
    camera: MapCamera,
    active: bool,
}

/// A left press on the map's empty surface that has not been answered yet: where it went down and
/// how far it has travelled, in this map's logical coordinates. A press that travelled no further
/// than the click slop is a click on the canvas and reports its point (`press`); one that
/// travelled further is a drag across it, and reports the rectangle it covered (`frame`). A press
/// on a pin is the pin's, and the map keeps nothing for it (canvas.md §6, "Pointing").
#[derive(Clone, Copy, Debug)]
struct FreePress {
    /// Where it went down, in this map's logical coordinates.
    start: Point,
    /// Where the pointer is now, for the rectangle a drag on the canvas draws.
    now: Point,
    /// How far it has travelled from where it went down, so a small wobble is still a click.
    travelled: f64,
}

/// A drag of a pin's trailing edge, bottom edge or corner, in this map's logical coordinates.
#[derive(Clone, Debug)]
struct EdgeDrag {
    /// The pin it resizes, by key.
    key: String,
    /// Where the pin stood when the drag began.
    from: Rect,
    /// Where the press went down, and where the pointer is now.
    start: Point,
    now: Point,
    /// Which edges it moves: the trailing one, the bottom one, or both at the corner.
    trailing: bool,
    bottom: bool,
}

impl EdgeDrag {
    /// The pin's rectangle where the pointer has taken it: its corner held, the grabbed edges
    /// following the pointer 1:1, never smaller than [`LEAST_SIDE`].
    fn rect(&self) -> Rect {
        let by = self.now - self.start;
        let width = if self.trailing {
            (self.from.width() + by.x).max(LEAST_SIDE)
        } else {
            self.from.width()
        };
        let height = if self.bottom {
            (self.from.height() + by.y).max(LEAST_SIDE)
        } else {
            self.from.height()
        };
        Rect::from_origin_size(self.from.origin(), (width, height))
    }
}

/// How close to a pin's trailing or bottom edge, in logical pixels, a press grabs that edge.
const EDGE_GRAB: f64 = crate::tokens::Layout::BASE.resize_grip as f64;
/// The least side a resized place keeps, in logical pixels.
const LEAST_SIDE: f64 = crate::tokens::Layout::BASE.place_min_side as f64;
/// The least height a hugging view's own measure must come to, in canvas pixels, for the map to
/// take it as the view's height: under it, the view said no height of its own (a list that
/// scrolls measures as nothing), and it fills its most instead.
const LEAST_NATURAL: f64 = crate::tokens::Spacing::BASE.s2 as f64;

/// See the module docs.
pub struct Map {
    /// The pins, in draw order: their key, place and view. Empty once the region that holds
    /// them is built (`the region owns the pin widgets`).
    pins: Vec<(String, PinPlace, Element)>,
    /// The map's one retained child: the explicit-placement region that lays the pins out at
    /// their canvas places through the camera. Kept here so the widgets it holds outlive the
    /// tree's pointers to them.
    parts: Parts,
    camera: MapCamera,
    /// What the layout's explicit-placement region last answered.
    invalidation: ManualInvalidation,
    pan: Option<Pan>,
    /// A left press on the empty canvas, kept until it is let go: a click on it reports its point
    /// (`press`), a drag across it the rectangle it covered (`frame`).
    free_press: Option<FreePress>,
    /// Where the pointer last was, in this map's logical coordinates.
    cursor: Point,
    touch: TouchGestureArena,
    /// It stands on the desktop's ground.
    ground: bool,
    rect: Rect,
    /// How far a press may travel and still be a click, not a pan: the theme's click slop.
    slop: f64,
    /// How far one arrow-key press pans, in screen pixels: one line, as an arrow key scrolls
    /// any scrolling region.
    key_pan: f64,
    /// Each pin's motion, by its key: it arrives where its view is laid out, leaves from there,
    /// and glides when the person moves it (`motion::Placed`). The region's solve advances it and
    /// lays the pin out where it has come to. A pin that is not in `pins` any more leaves on its
    /// own before it is forgotten.
    placed: Rc<RefCell<HashMap<String, crate::motion::Placed>>>,
    /// Where the region laid each pin out last, by key, written by its own solve.
    laid: Rc<RefCell<HashMap<String, Rect>>>,
    /// The pins that hug what they hold but whose view said no height of its own at the last
    /// solve (a turn before its words arrive), by key: laid out at their most, which is not yet
    /// how tall they are, so the camera does not follow them there.
    unsaid: Rc<RefCell<std::collections::HashSet<String>>>,
    /// Where each pin stands on the canvas, in the order of `pins`, as the region reads it at each
    /// solve: a pin moved in place ([`Described::set_props`]) is laid out where it stands now,
    /// though the region was built while it stood elsewhere.
    standing: Rc<RefCell<Vec<PinPlace>>>,

    /// The place that arrived on the map and is to be shown: the camera pans to it before the
    /// places are laid out, so a window a person just opened stands where they can see it.
    reveal: Option<String>,
    /// The camera on its way to where a reveal put it: a pan of the person's own hand moves the
    /// camera itself ([`Self::set_camera`]), and a reveal glides as a scroll to what it shows
    /// does ([`crate::motion::ScrollTo`]).
    glide: Option<crate::motion::ScrollTo>,
    /// Where the camera stands this frame, what the region lays the places out about: the
    /// glide's way to where it is going, else the camera.
    shown: Rc<Cell<MapCamera>>,
    /// The camera the region last laid the places out about: one that moved since carries every
    /// place with it rather than gliding it there.
    followed: Rc<Cell<Option<MapCamera>>>,
    /// The motion the person allows, as the theme the map was last built under says it.
    motion: crate::tokens::EffectiveMotion,
    /// Something of the map moved on the last frame: the frame after lays it out once more, so
    /// the motion's last step is laid out too.
    moved: bool,
    /// What the map is called: the map built in its place takes its lens up by it ([`Lens`]).
    name: String,
    /// It has looked for the lens of the map it was built in place of, at its first layout.
    took: bool,

    /// Input or a new document value changed what this widget paints: it
    /// repaints once, and rests after the frame draws it.
    dirty: bool,
    /// The person put the keyboard in the canvas: its arrow keys pan it.
    focused: bool,
    /// What a resource dragged onto the map and let go does: the map reports it with where it
    /// was dropped, and the view places it.
    drop: Event,
    /// What a press on the empty canvas does: the map reports the canvas point under it, and the
    /// view stands what belongs there (the shell's canvas opens its caret).
    press: Event,
    /// What a drag across the empty canvas does: the map reports the rectangle it covered, in
    /// canvas pixels, and the view lays what it answers into that rectangle.
    frame: Event,
    /// What a right press on the map does: the map reports the pin under it and the canvas point,
    /// and the view opens what the person pointed at.
    context: Event,
    /// What dragging a pin's edge or corner does: the map reports the pin and its new size.
    resize: Event,
    /// What pointing reports first: the part of the canvas the person sees as they point.
    seen: Event,
    /// The edge or corner being dragged, until it is let go.
    edge_drag: Option<EdgeDrag>,
    /// The rectangle the view points at, in canvas pixels, drawn as a light outline.
    outline: Option<Rect>,
    /// The key the map is known by on the drag bus, namespaced from the view that built it: a
    /// drag that started in one of its pins never lands back on the map it came from, and two
    /// maps never answer each other's drops.
    key: String,
    /// What each pin stands for, by its key, as its view named it: what pointing at it reports.
    items: HashMap<String, String>,
    /// The pin whose view a change inside it is patching (`part_views`), by its index: `None`
    /// while the change is to the pins themselves.
    editing: Option<usize>,
    /// What the empty map says at its middle, while it holds no pin.
    hint: Option<String>,
    /// Where the person last pointed on the map, in canvas pixels: the point a click reported,
    /// the corner of a dragged rectangle, a right press's point, where a drop landed. What
    /// arrives on the map after it is the answer to that pointing, and grows out of it.
    asked: Option<Point>,
    /// The place that answers the person's last pointing, by its key: of the places arriving
    /// after it that grow from where it was asked, the one standing nearest that point (the turn
    /// at the caret, the thing found at a click). The camera keeps it whole in view as it grows,
    /// framed with the places arriving after it (an agent's view beside it) as far as they fit
    /// in view together (`map_answer.rs`). The person's own pan or zoom lets it go.
    answer: Option<String>,
    /// The places standing when the person last pointed, by key: every other place is what
    /// arrived since, the answer and what it opened beside it, framed with it (`map_answer.rs`).
    asked_among: Vec<String>,
    /// Whether the camera has moved to frame the answer since the person pointed: it then keeps
    /// it framed as the places settle their sizes, where an answer that stood in view from the
    /// first leaves the camera where the person had it.
    framing_answer: bool,
}

/// What a map leaves for the one a rebuild puts in its place ([`crate::motion::keep`]): the
/// person's lens on the canvas, and where its places were drawn.
struct Lens {
    camera: MapCamera,
    shown: MapCamera,
    followed: Option<MapCamera>,
    glide: Option<crate::motion::ScrollTo>,
    rect: Rect,
    placed: HashMap<String, crate::motion::Placed>,
    pins: Vec<String>,
    reveal: Option<String>,
    asked: Option<Point>,
    answer: Option<String>,
    asked_among: Vec<String>,
    framing_answer: bool,
}

impl Drop for Map {
    fn drop(&mut self) {
        crate::motion::keep(
            self.name.clone(),
            None,
            Lens {
                camera: self.camera,
                shown: self.shown.get(),
                followed: self.followed.get(),
                glide: self.glide,
                rect: self.rect,
                placed: std::mem::take(&mut *self.placed.borrow_mut()),
                pins: self.pins.iter().map(|(key, _, _)| key.clone()).collect(),
                reveal: self.reveal.take(),
                asked: self.asked,
                answer: self.answer.take(),
                asked_among: std::mem::take(&mut self.asked_among),
                framing_answer: self.framing_answer,
            },
        );
    }
}

impl Default for Map {
    fn default() -> Self {
        Self::new()
    }
}

impl Map {
    /// An empty map.
    #[must_use]
    pub fn new() -> Self {
        Self {
            dirty: true,
            pins: Vec::new(),
            parts: Parts::default(),
            camera: MapCamera::default(),
            invalidation: ManualInvalidation::default(),
            pan: None,
            free_press: None,
            cursor: Point::ORIGIN,
            touch: TouchGestureArena::new(),
            ground: false,
            rect: Rect::ZERO,
            slop: f64::from(crate::tokens::Metrics::BASE.click_slop),
            key_pan: f64::from(crate::tokens::Metrics::BASE.wheel_line),
            placed: Rc::default(),
            laid: Rc::default(),
            unsaid: Rc::default(),
            standing: Rc::default(),
            reveal: None,
            glide: None,
            shown: Rc::default(),
            followed: Rc::default(),
            motion: crate::tokens::EffectiveMotion::default(),
            moved: false,
            name: String::new(),
            took: false,
            focused: false,
            drop: Event::None,
            press: Event::None,
            frame: Event::None,
            context: Event::None,
            resize: Event::None,
            seen: Event::None,
            edge_drag: None,
            outline: None,
            key: String::new(),
            items: HashMap::new(),
            editing: None,
            hint: None,
            asked: None,
            answer: None,
            asked_among: Vec::new(),
            framing_answer: false,
        }
    }

    /// Add a pin.
    #[must_use]
    pub fn pin(mut self, key: impl Into<String>, place: PinPlace, view: Element) -> Self {
        self.pins.push((key.into(), place, view));
        self
    }

    /// The camera it looks with.
    #[must_use]
    pub fn camera(&self) -> MapCamera {
        self.camera
    }

    /// The key the map is known by on the drag bus: its own stable one, so a drag that started
    /// in a pin never lands back on the map it came from, and two maps never answer each other's
    /// drops.
    fn drop_key(&self) -> &str {
        &self.key
    }

    /// Look with `camera`: where the person left the canvas on this device.
    pub fn set_camera(&mut self, camera: MapCamera) {
        self.dirty = true;
        // The person moved the lens themselves: the camera no longer keeps their answer in view.
        self.answer = None;
        self.framing_answer = false;
        // The person's own hand is on the canvas: it stands where they put it, whether or not a
        // reveal was on its way there.
        self.glide = None;
        self.shown.set(camera);
        if !self.camera.same(camera) {
            self.camera = camera;
            self.invalidation.invalidate();
        }
    }

    /// Take over what the person was looking at from the map this one is built in place of (a
    /// view built again around changed places, or around anything that holds the map): its
    /// camera, and where its pins were drawn, so a rebuild neither resets the lens nor makes every
    /// window on the canvas arrive again. The place that arrived since is shown, so what a person
    /// just opened stands where they can see it.
    fn take_over(&mut self, lens: Lens) {
        self.camera = lens.camera;
        self.shown.set(lens.shown);
        self.followed.set(lens.followed);
        self.glide = lens.glide;
        // The room the places stand in was laid out last frame: a place that arrived is shown in
        // it before this layout places it.
        self.rect = lens.rect;
        self.placed.borrow_mut().extend(lens.placed);
        // What the person pointed at before the map was built again is still what an arriving
        // place answers.
        if self.asked.is_none() {
            self.asked = lens.asked;
            self.asked_among = lens.asked_among;
            self.framing_answer = lens.framing_answer;
        }
        self.take_answer(&lens.pins, lens.answer);
        self.reveal = self
            .pins
            .iter()
            .rev()
            .find(|(key, _, _)| !lens.pins.contains(key))
            .map(|(key, _, _)| key.clone())
            // One that arrived on the map this replaces and was not shown yet still is.
            .or(lens.reveal);
    }

    /// Put the camera on `area` (canvas pixels), with `margin` around it, at the viewport's
    /// size: what a canvas does when it opens, or when the person asks to see everything.
    pub fn show(&mut self, area: Rect, margin: f64) {
        if self.rect.width() <= 0.0 || self.rect.height() <= 0.0 {
            return;
        }
        let bounds = area.inflate(margin, margin);
        let zoom = (self.rect.width() / bounds.width().max(1.0))
            .min(self.rect.height() / bounds.height().max(1.0))
            .clamp(MAP_ZOOM_MIN, MAP_ZOOM_MAX);
        let origin =
            bounds.center() - Vec2::new(self.rect.width(), self.rect.height()) / (2.0 * zoom);
        self.set_camera(MapCamera { origin, zoom });
    }

    /// What the pin `key` stands for, as its view named it, else its key; empty stays empty.
    fn item_of(&self, key: String) -> String {
        self.items.get(&key).cloned().unwrap_or(key)
    }

    /// Report a right press at the pointer: the pin `pin` it pointed at (empty for free space)
    /// and the canvas point under it.
    fn point_at_context(&mut self, pin: String) {
        let pin = self.item_of(pin);
        let at = self.camera.to_canvas(self.rect.origin(), self.cursor);
        self.point_at(at);
        self.context.fire(Some(crate::component::Value::List(vec![
            crate::component::Value::Text(pin),
            crate::component::Value::List(vec![
                crate::component::Value::Int(at.x.round() as i64),
                crate::component::Value::Int(at.y.round() as i64),
            ]),
        ])));
    }

    /// The pin whose trailing edge, bottom edge or corner stands at `local` (this map's own
    /// coordinates), the topmost first: its key, where it stands, and which edges the point grabs.
    fn edge_at(&self, local: Point) -> Option<(String, Rect, bool, bool)> {
        let laid = self.laid.borrow();
        self.pins.iter().rev().find_map(|(key, _, _)| {
            let rect = *laid.get(key)?;
            let near_trailing = (local.x - rect.x1).abs() <= EDGE_GRAB;
            let near_bottom = (local.y - rect.y1).abs() <= EDGE_GRAB;
            let along_y = local.y >= rect.y0 && local.y <= rect.y1 + EDGE_GRAB;
            let along_x = local.x >= rect.x0 && local.x <= rect.x1 + EDGE_GRAB;
            let trailing = near_trailing && along_y;
            let bottom = near_bottom && along_x;
            (trailing || bottom).then(|| (key.clone(), rect, trailing, bottom))
        })
    }

    /// The smallest canvas rect holding every pin: what [`Self::show`] fits the camera to.
    #[must_use]
    pub fn bounds(&self) -> Rect {
        let mut out: Option<Rect> = None;
        for (key, place, _) in &self.pins {
            let height = place.height.unwrap_or(0.0);
            let _ = key;
            let rect = Rect::from_origin_size(
                Point::new(place.x, place.y),
                (place.width.max(1.0), height),
            );
            out = Some(out.map_or(rect, |held| held.union(rect)));
        }
        out.unwrap_or(Rect::from_origin_size(Point::ORIGIN, (0.0, 0.0)))
    }

    /// The pins' canvas places, by key.
    #[must_use]
    pub fn canvas_places(&self) -> HashMap<String, PinPlace> {
        self.pins
            .iter()
            .map(|(key, place, _)| (key.clone(), *place))
            .collect()
    }

    /// Where the camera brings `area` (canvas pixels) into the viewport, panning the least it
    /// can and keeping its zoom: what a canvas does when a place arrives off screen, so what an
    /// agent or a person just placed stands where the person can see it without losing the view
    /// they had. `None` for an area already inside the viewport, or on a map not laid out yet.
    fn revealing(&self, area: Rect, margin: f64) -> Option<MapCamera> {
        let (width, height) = (self.rect.width(), self.rect.height());
        if width <= 0.0 || height <= 0.0 {
            return None;
        }
        let area = area.inflate(margin, margin);
        let zoom = self.camera.zoom;
        // Where the area stands against the viewport, in canvas pixels: the least pan that
        // brings it inside.
        let left = self.camera.origin.x;
        let top = self.camera.origin.y;
        let right = left + width / zoom.max(f64::EPSILON);
        let bottom = top + height / zoom.max(f64::EPSILON);
        // An area wider or taller than the view shows its start, so the camera rests there
        // rather than going back and forth between its two edges.
        let least = |(low, high): (f64, f64), (start, end): (f64, f64)| {
            if high - low > end - start || low < start {
                low - start
            } else if high > end {
                high - end
            } else {
                0.0
            }
        };
        let dx = least((area.x0, area.x1), (left, right));
        let dy = least((area.y0, area.y1), (top, bottom));
        if dx.abs() < 0.5 && dy.abs() < 0.5 {
            return None;
        }
        let mut camera = self.camera;
        camera.origin += Vec2::new(dx, dy);
        Some(camera)
    }

    /// [`Self::revealing`] holding `keep` (canvas pixels) as well: what a canvas does when a place
    /// arrives beside another, so what the person was looking at stays where they were looking
    /// rather than being pushed out of the viewport by the pan to what arrived (`shell.md` §3,
    /// "a place next to where it came from"). `keep` is every place standing wholly in the
    /// viewport: the camera pans the least that holds them all with what arrived, and it never
    /// changes its zoom — opening a place never resizes the places already there, so a place
    /// stands as big as the person was reading it. When what arrived and what they were looking
    /// at cannot all stand in the viewport at that zoom, the camera pans to show what arrived
    /// whole; the person pans back to the rest, which stands where it did.
    fn revealing_with(&self, area: Rect, margin: f64, keep: &[Rect]) -> Option<MapCamera> {
        let Some(together) = keep
            .iter()
            .fold(None, |held: Option<Rect>, rect| {
                let rect = rect.inflate(margin, margin);
                Some(held.map_or(rect, |held| held.union(rect)))
            })
            .map(|kept| kept.union(area.inflate(margin, margin)))
        else {
            return self.revealing(area, margin);
        };
        let (width, height) = (self.rect.width(), self.rect.height());
        if width <= 0.0 || height <= 0.0 {
            return None;
        }
        // The zoom is the person's: the camera pans to what arrived and never zooms it out of
        // the size they were reading it at.
        let zoom = self.camera.zoom;
        let span = Vec2::new(width, height) / zoom.max(f64::EPSILON);
        // What the person was looking at and what arrived cannot all stand in the viewport at
        // this zoom: show what arrived whole, and leave the rest where it stands.
        if together.width() > span.x + 0.5 || together.height() > span.y + 0.5 {
            return self.revealing(area, margin);
        }
        let origin = self.camera.origin;
        // The least pan that holds them all: where they were looking stays where it was as far
        // as that allows, and what arrived joins it, all of them in view.
        let along = |origin: f64, (low_edge, high_edge): (f64, f64), extent: f64| {
            let (low, high) = (high_edge - extent, low_edge);
            if low <= high {
                origin.clamp(low, high)
            } else {
                low_edge
            }
        };
        let x = along(origin.x, (together.x0, together.x1), span.x);
        let y = along(origin.y, (together.y0, together.y1), span.y);
        let camera = MapCamera {
            origin: Point::new(x, y),
            zoom,
        };
        (!camera.same(self.camera)).then_some(camera)
    }

    /// Whether the point `local` (in this map's own coordinates) stands on one of its pins: the
    /// surface a press there belongs to the pin, never to the canvas under it.
    fn over_pin(&self, local: Point) -> bool {
        self.laid.borrow().values().any(|rect| rect.contains(local))
    }

    /// The key of the pin standing at `local` (this map's own coordinates), if one does: the
    /// topmost pin covers the ones under it, so the last drawn wins, as a press would find it.
    fn pin_under(&self, local: Point) -> Option<String> {
        let laid = self.laid.borrow();
        self.pins
            .iter()
            .rev()
            .filter_map(|(key, _, _)| Some((key, *laid.get(key)?)))
            .find(|(_, rect)| rect.contains(local))
            .map(|(key, _)| key.clone())
    }

    /// Show the place that arrived, if any: what [`Self::take_over`] left to be shown. The
    /// camera sets off there before the places are laid out, and the places, with the windows in
    /// their wells, stand where it has carried them on every frame of the way. Every place that
    /// stood wholly in the viewport stays there: a place opened beside a conversation never
    /// pushes the conversation out of view ([`Self::revealing_with`]).
    fn show_asked(&mut self) {
        let Some(key) = self.reveal.take() else {
            return;
        };
        let (width, height) = (self.rect.width(), self.rect.height());
        if width <= 0.0 || height <= 0.0 {
            return;
        }
        let Some((_, place, _)) = self.pins.iter().find(|(held, _, _)| *held == key) else {
            return;
        };
        let zoom = self.camera.zoom.max(f64::EPSILON);
        let canvas = |place: &PinPlace| {
            // A place whose height its view says is as tall as a window is wide over tall; one
            // that hugs what it holds is shown by its top, so a short answer in a tall room
            // does not carry the camera to the room's foot. Its caption's band over it is shown
            // with it.
            let height = match place.height {
                Some(height) if place.hug => height.min(LEAST_SIDE),
                Some(height) => height,
                None => place.width.max(1.0) / 1.6,
            };
            let band = place.band / zoom;
            Rect::from_origin_size(
                Point::new(place.x, place.y - band),
                (place.width.max(1.0), height + band),
            )
        };
        // The person's answer is framed with what arrives beside it once both are laid out, at
        // the sizes they stand at ([`Self::keep_answer`]).
        if self.answer.as_deref().is_some_and(|answer| answer != key) {
            return;
        }
        let area = canvas(place);
        // What the person was looking at: every other place standing wholly in the viewport.
        let seen = Rect::from_origin_size(
            self.camera.origin,
            (
                width / self.camera.zoom.max(f64::EPSILON),
                height / self.camera.zoom.max(f64::EPSILON),
            ),
        );
        let within = |rect: &Rect| {
            rect.x0 >= seen.x0 && rect.x1 <= seen.x1 && rect.y0 >= seen.y0 && rect.y1 <= seen.y1
        };
        // A place that arrived in sight moves nothing: the person's view stays their own.
        if within(&area) {
            return;
        }
        let kept: Vec<Rect> = self
            .pins
            .iter()
            .filter(|(held, _, _)| *held != key)
            .map(|(_, place, _)| canvas(place))
            .filter(|rect| within(rect))
            .collect();
        let margin = ARRIVAL_MARGIN / self.camera.zoom.max(f64::EPSILON);
        if let Some(camera) = self.revealing_with(area, margin, &kept) {
            self.pan_to(camera);
        }
    }

    /// Take the camera to `camera` the way a scroll takes its content to what it shows: a short
    /// move glides, whoever opened what it shows and however long it took to arrive, so the person
    /// sees where it stands among the rest; a still theme or reduced motion lands. Nothing in the
    /// layout moves.
    fn pan_to(&mut self, camera: MapCamera) {
        let now = crate::motion::frame_now();
        let view = Vec2::new(self.rect.width().max(1.0), self.rect.height().max(1.0));
        let along = |at: Point| Vec2::new(at.x, at.y);
        let mut glide = self.glide.unwrap_or_default();
        glide.go_from(
            along(self.shown.get().origin),
            along(camera.origin),
            view,
            self.motion,
            crate::motion::input_source(),
            now,
        );
        self.camera = camera;
        self.glide = Some(glide);
        self.step_camera(now);
        self.dirty = true;
        self.invalidation.invalidate();
    }

    /// Bring the camera's glide up to `now`: where the region lays the places out this frame.
    /// Whether the camera moved.
    fn step_camera(&mut self, now: crate::time::Instant) -> bool {
        let Some(glide) = &mut self.glide else {
            return false;
        };
        let shown = match glide.offset(now) {
            Some(origin) if glide.is_gliding(now) => MapCamera {
                origin: Point::new(origin.x, origin.y),
                zoom: self.camera.zoom,
            },
            // It has reached where it was going, or it never glided: the camera is there.
            _ => {
                self.glide = None;
                self.camera
            }
        };
        let moved = !self.shown.get().same(shown);
        self.shown.set(shown);
        moved
    }

    /// The region that lays the pins out, when built.
    fn region_mut(&mut self) -> Option<&mut Manual> {
        self.parts
            .children
            .first_mut()
            .and_then(|child| child.as_any_mut())
            .and_then(|any| any.downcast_mut::<Manual>())
    }

    /// Say where every pin stands now to the region that lays them out.
    fn stand_pins(&self) {
        *self.standing.borrow_mut() = self.pins.iter().map(|(_, place, _)| *place).collect();
    }

    /// Build the region that places the pins on the camera: from the views it holds now (taken
    /// back out of the region built last), at the places and zoom the map is at.
    fn rebuild_region(&mut self, _theme: &Theme) {
        // After a build the views live in the region; take them back to build the next one.
        let mut views: Vec<Element> = match self.region_mut() {
            Some(region) => region.take_children(),
            None => Vec::new(),
        };
        // Fill up to the pins there are, from the views the map itself still holds.
        while views.len() < self.pins.len() {
            let index = views.len();
            let view = std::mem::replace(
                &mut self.pins[index].2,
                Box::new(crate::widget::Spacer::token(crate::tokens::SpacingStep::S0)),
            );
            views.push(view);
        }
        views.truncate(self.pins.len());
        self.stand_pins();
        let places = Rc::clone(&self.standing);
        let keys: Vec<String> = self.pins.iter().map(|(key, _, _)| key.clone()).collect();
        // The camera is read at each solve, not when the region is built: a pan, a zoom or a
        // glide of the camera lays the places out again without building the region again.
        let shown = Rc::clone(&self.shown);
        let followed = Rc::clone(&self.followed);
        let moving = Rc::clone(&self.placed);
        let motion = self.motion;
        let laid: Rc<RefCell<HashMap<String, Rect>>> = Rc::clone(&self.laid);
        let unsaid = Rc::clone(&self.unsaid);
        let mut region = Manual::new(move |cx| -> Result<Size<f32>, WidgetLayoutError> {
            let available = cx.available();
            // The tallest a hugging place stands: the viewport, less the room a place that
            // arrives keeps from its edges.
            let room = f64::from(available.height) - 2.0 * ARRIVAL_MARGIN;
            let camera = shown.get();
            // A camera that moved since the last solve carries the places: the ground moves under
            // them, and they stand where it took them, with the windows in their wells.
            let carried = followed
                .replace(Some(camera))
                .is_some_and(|was| !was.same(camera));
            let now = crate::motion::frame_now();
            let source = crate::motion::input_source();
            let mut moving = moving.borrow_mut();
            let places = places.borrow();
            let mut out: HashMap<String, Rect> = HashMap::with_capacity(places.len());
            let mut silent = std::collections::HashSet::new();
            for index in 0..cx.len() {
                let Some(place) = places.get(index) else {
                    continue;
                };
                let width = (place.width * camera.zoom).max(1.0);
                // A pin that hugs is measured at its natural height and stands no taller than
                // its own: an answer as tall as its words, inside the rectangle it was asked in.
                let fixed = place.height.filter(|_| !place.hug);
                let size = cx
                    .measure_child(
                        index,
                        Size {
                            width: MeasureConstraint::Exact(f32_of(width)),
                            height: fixed.map_or(MeasureConstraint::Intrinsic, |height| {
                                MeasureConstraint::Exact(f32_of(
                                    (height * camera.zoom).max(1.0) + place.band,
                                ))
                            }),
                        },
                    )?
                    .size;
                let left = (place.x - camera.origin.x) * camera.zoom;
                // A band over the view stands above the pin's point, so the view's top is there.
                let top = (place.y - camera.origin.y) * camera.zoom - place.band;
                let natural = f64::from(size.height);
                let height = place.height.map_or(natural, |height| {
                    let most = (height * camera.zoom).max(1.0) + place.band;
                    // What hugs what it holds never grows past what the person sees: a long
                    // answer stands as tall as the viewport leaves it, and scrolls.
                    let most = if place.hug {
                        most.min(room).max(LEAST_SIDE.min(most))
                    } else {
                        most
                    };
                    // A view that says no height of its own (a list that scrolls is as tall as
                    // the room it is given) fills its most: it cannot hug what it holds.
                    let says = natural - place.band >= LEAST_NATURAL * camera.zoom;
                    if place.hug
                        && !says
                        && let Some(key) = keys.get(index)
                    {
                        silent.insert(key.clone());
                    }
                    if place.hug && says {
                        natural.min(most)
                    } else {
                        most
                    }
                });
                let mut rect = Rect::from_origin_size((left, top), (width, height));
                // The pin is laid out where its motion has come to this frame, so what stands
                // in it moves with what is drawn of it.
                if let Some(key) = keys.get(index) {
                    let placed = moving
                        .entry(key.clone())
                        .or_insert_with(crate::motion::Placed::closed);
                    if !(carried && placed.carry(rect)) {
                        placed.follow(rect, true, motion, source, now);
                    }
                    rect = Rect::from_origin_size(placed.rect(now).origin(), rect.size());
                    out.insert(key.clone(), rect);
                }
                cx.place(index, rect)?;
            }
            *laid.borrow_mut() = out;
            *unsaid.borrow_mut() = silent;
            // The region is its viewport: a canvas does not grow with what it holds.
            Ok(Size {
                width: available.width.max(0.0),
                height: available.height.max(0.0),
            })
        });
        region = region.filling();
        for view in views {
            region = region.child(view);
        }
        self.invalidation = region.invalidation();
        self.parts.children = vec![Box::new(region)];
    }
}

impl Described for Map {
    type Props = MapProps;

    fn from_props(key: &str, props: MapProps) -> Self {
        let MapProps {
            name,
            ground,
            pins,
            drop,
            press,
            frame,
            context,
            resize,
            seen,
            outline,
            hint,
        } = props;
        let mut map = Self::new();
        map.hint = hint.filter(|hint| !hint.trim().is_empty());
        map.resize = resize;
        map.seen = seen;
        map.outline = outline_of(&outline);
        map.ground = ground;
        map.name = name.unwrap_or_default();
        map.key = format!("map\u{1f}{key}");
        map.drop = drop;
        map.press = press;
        map.frame = frame;
        map.context = context;
        for pin in pins {
            let place = PinPlace {
                x: pin.x.unwrap_or(0) as f64,
                y: pin.y.unwrap_or(0) as f64,
                width: (pin.width.unwrap_or(360) as f64).max(1.0),
                height: pin.height.map(|height| (height as f64).max(1.0)),
                band: band_of(pin.band),
                hug: pin.hug && pin.height.is_some(),
                grows: pin.grows,
            };
            if let Some(item) = pin.item.filter(|item| !item.is_empty()) {
                map.items.insert(pin.key.clone(), item);
            }
            let view: Element = match pin.body.into_iter().next() {
                Some(body) => body,
                None => Box::new(crate::widget::Spacer::token(crate::tokens::SpacingStep::S0)),
            };
            map = map.pin(pin.key, place, view);
        }
        map
    }

    /// Where the pins stand and which ground the map is on change where it stands: the camera
    /// keeps its place, and each pin keeps the view it shows.
    ///
    /// Props given in place name the pins and carry none of their views, so the pins they name
    /// must be the pins the map holds, one for one: a map given other pins is another canvas,
    /// and is built again around them. Patching the pins it recognises would leave the pin of a
    /// place the layout closed standing on screen and in the accessibility tree.
    fn set_props(&mut self, props: MapProps) -> bool {
        let MapProps {
            name: _,
            ground,
            pins,
            drop,
            press,
            frame,
            context,
            resize,
            seen,
            outline,
            hint,
        } = props;
        let hint = hint.filter(|hint| !hint.trim().is_empty());
        if hint != self.hint {
            self.hint = hint;
            self.dirty = true;
        }
        self.resize = resize;
        self.seen = seen;
        let outline = outline_of(&outline);
        if outline != self.outline {
            self.outline = outline;
            self.dirty = true;
        }
        self.drop = drop;
        self.press = press;
        self.frame = frame;
        self.context = context;
        if pins.len() != self.pins.len()
            || pins
                .iter()
                .zip(&self.pins)
                .any(|(given, (key, _, _))| given.key != *key)
        {
            return false;
        }
        self.ground = ground;
        self.items.clear();
        for (pin, (key, held, _)) in pins.into_iter().zip(self.pins.iter_mut()) {
            if let Some(item) = pin.item.filter(|item| !item.is_empty()) {
                self.items.insert(key.clone(), item);
            }
            *held = PinPlace {
                x: pin.x.unwrap_or(0) as f64,
                y: pin.y.unwrap_or(0) as f64,
                width: (pin.width.unwrap_or(360) as f64).max(1.0),
                height: pin.height.map(|height| (height as f64).max(1.0)),
                band: band_of(pin.band),
                hug: pin.hug && pin.height.is_some(),
                grows: pin.grows,
            };
        }
        // The region built before reads where the pins stand now at its next solve.
        self.stand_pins();
        self.dirty = true;
        self.invalidation.invalidate();
        true
    }

    fn slot_views(&mut self, slot: &str) -> Option<&mut dyn SlotViews> {
        self.editing = None;
        (slot == "pins").then_some(self as &mut dyn SlotViews)
    }

    /// The view one pin shows, to change in place: a change inside a place (its handle, its
    /// menu, what its resource's view says) patches that view where it stands, and the map —
    /// its camera, a drag under way, every other place — stays as it is.
    fn part_views(&mut self, slot: &str, key: &str, of: &str) -> Option<&mut dyn SlotViews> {
        if slot != "pins" || of != "body" {
            return None;
        }
        self.editing = Some(self.pins.iter().position(|(held, _, _)| held == key)?);
        Some(self as &mut dyn SlotViews)
    }
}

/// The pins of a map: one view each. Once the map is built the widgets live in its region, so a
/// change here reaches the region's children; before that, in the map's own store. Either way
/// the pins' keys and places stay in `self.pins`, which the next layout lays out afresh.
impl SlotViews for Map {
    fn len(&self) -> usize {
        if self.editing.is_some() {
            return 1;
        }
        self.pins.len()
    }

    fn view_mut(&mut self, index: usize) -> Option<&mut Element> {
        let index = match self.editing {
            Some(pin) if index == 0 => pin,
            Some(_) => return None,
            None => index,
        };
        // The region holds the views once built; the pins' own until then.
        if self.parts.children.is_empty() {
            return self.pins.get_mut(index).map(|(_, _, view)| view);
        }
        let region = self
            .parts
            .children
            .first_mut()?
            .as_any_mut()?
            .downcast_mut::<Manual>()?;
        region.child_mut(index)
    }

    fn insert(&mut self, index: usize, view: Element) {
        if let Some(pin) = self.editing {
            self.put_pin_view(pin, view);
            return;
        }
        let at = index.min(self.pins.len());
        let key = format!("pin-{at}");
        // Keep the pins' keys and places in step; the view goes to the region when built.
        self.pins.insert(
            at,
            (
                key,
                PinPlace {
                    x: 0.0,
                    y: 0.0,
                    width: 360.0,
                    height: None,
                    band: 0.0,
                    hug: false,
                    grows: false,
                },
                Box::new(crate::widget::Spacer::token(crate::tokens::SpacingStep::S0)),
            ),
        );
        if let Some(region) = self.region_mut() {
            region.insert_child(at, view);
        } else if let Some((_, _, slot)) = self.pins.get_mut(at) {
            *slot = view;
        }
        self.stand_pins();
    }

    fn remove(&mut self, index: usize) {
        if let Some(pin) = self.editing {
            self.put_pin_view(
                pin,
                Box::new(crate::widget::Spacer::token(crate::tokens::SpacingStep::S0)),
            );
            return;
        }
        if index < self.pins.len() {
            self.pins.remove(index);
            if let Some(region) = self.region_mut() {
                let _ = region.remove_child(index);
            }
        }
        self.stand_pins();
    }

    fn move_view(&mut self, from: usize, to: usize) {
        if self.editing.is_some() {
            return;
        }
        if from < self.pins.len() && to < self.pins.len() {
            let pin = self.pins.remove(from);
            self.pins.insert(to, pin);
            if let Some(region) = self.region_mut() {
                region.move_child(from, to);
            }
        }
        self.stand_pins();
    }

    fn replace(&mut self, index: usize, view: Element) {
        let index = self.editing.unwrap_or(index);
        self.put_pin_view(index, view);
    }
}

impl Map {
    /// Stand `view` in the pin at `index`: in the region once it is built, else in the pin.
    fn put_pin_view(&mut self, index: usize, view: Element) {
        if let Some(slot) = self.region_mut().and_then(|region| region.child_mut(index)) {
            *slot = view;
        } else if let Some((_, _, slot)) = self.pins.get_mut(index) {
            *slot = view;
        }
    }
}

/// How far apart the ground's dots stand, in canvas pixels at zoom 1: the rhythm's group step, so
/// the grid is the spacing a place's own content keeps.
const GRAIN_PITCH: f64 = crate::tokens::Spacing::BASE.rhythm.group as f64;
/// The closest the ground's dots stand on screen, in logical pixels: zoomed out further, every
/// other one is left out, so the ground reads as a surface and never as a texture.
const GRAIN_LEAST: f64 = crate::tokens::Spacing::BASE.rhythm.section as f64;
/// How wide a dot of the ground is on screen, in logical pixels, at any zoom.
const GRAIN_DOT: f64 = 1.5;

impl Map {
    /// The ground's grain: a grid of dots in the theme's quietest veneer, so faint it only says
    /// "surface" (`canvas.md` §7). The dots are canvas points, so they pan and zoom with the
    /// ground and the person sees the ground move under their hand; on screen each is the same
    /// size at every zoom. All of them are one path, filled once.
    fn draw_grain(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let viewport = self.rect;
        let camera = self.shown.get();
        let zoom = camera.zoom.max(f64::EPSILON);
        // Doubled as often as it takes to stand the least apart on screen.
        let doublings = (GRAIN_LEAST / (GRAIN_PITCH * zoom))
            .log2()
            .ceil()
            .clamp(0.0, 16.0);
        let pitch = GRAIN_PITCH * doublings.exp2();
        let corner = viewport.origin();
        let from = camera.to_canvas(corner, corner);
        let to = camera.to_canvas(corner, Point::new(viewport.x1, viewport.y1));
        let first_x = (from.x / pitch).floor() as i64;
        let first_y = (from.y / pitch).floor() as i64;
        let last_x = (to.x / pitch).ceil() as i64;
        let last_y = (to.y / pitch).ceil() as i64;
        let half = GRAIN_DOT / 2.0;
        let mut dots = vello::kurbo::BezPath::new();
        for row in first_y..=last_y {
            for column in first_x..=last_x {
                let at = camera.to_screen(
                    corner,
                    Point::new(column as f64 * pitch, row as f64 * pitch),
                );
                // Snapped to the pixel grid, so every dot is as sharp as the next.
                let at = Point::new(at.x.round(), at.y.round());
                if !viewport.contains(at) {
                    continue;
                }
                let dot = Rect::new(at.x - half, at.y - half, at.x + half, at.y + half);
                vello::kurbo::Shape::path_elements(&dot, 0.1).for_each(|element| {
                    dots.push(element);
                });
            }
        }
        scene.fill(
            Fill::NonZero,
            transform,
            theme.tokens.colors.overlay_subtle,
            None,
            &dots,
        );
    }

    /// What the empty map says at the middle of what it shows, in the theme's quiet ink at body
    /// size: a sentence, not a control, gone once anything stands on the map.
    fn draw_hint(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        let Some(hint) = self.shown_hint() else {
            return;
        };
        let style = theme.tokens.typography.style(crate::tokens::TextRole::Body);
        let face = crate::widget::TextLineFace::styled(hint, &style, crate::tokens::TextRole::Body);
        let mut ctx = crate::paint::PaintCtx::new(scene, transform, text, theme);
        face.paint(&mut ctx, self.rect, theme.text_tertiary);
    }

    /// The hint the map shows now: its own while it holds no pin and nothing is being pointed at.
    fn shown_hint(&self) -> Option<&str> {
        (self.pins.is_empty() && self.free_press.is_none() && self.outline.is_none())
            .then_some(self.hint.as_deref())
            .flatten()
    }

    /// The rectangle a drag across the empty canvas covers, as a light outline in the theme's
    /// focus colour (a theme role, never a colour of the map's own): what the person sees of the
    /// frame they are pointing at before they let go (`canvas.md` §1, §5). Nothing is drawn when
    /// no such drag is under way, or while it is still a click.
    fn draw_frame_outline(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        let look = OutlineLook::of(theme);
        let zoom = self.shown.get().zoom;
        let outline = |scene: &mut Scene, rect: &Rect| {
            look.stroke(scene, transform, *rect, zoom);
        };
        // A place's edge being dragged shows the size it is taking.
        if let Some(drag) = &self.edge_drag {
            let origin = self.rect.origin().to_vec2();
            outline(scene, &(drag.rect() + origin));
            return;
        }
        // The rectangle the view points at, while no drag draws one of its own.
        if self.free_press.is_none()
            && let Some(rect) = self.outline
        {
            let corner = self.rect.origin();
            let a = self.camera.to_screen(corner, rect.origin());
            let b = self.camera.to_screen(corner, Point::new(rect.x1, rect.y1));
            outline(scene, &Rect::from_points(a, b));
            return;
        }
        let Some(press) = self.free_press else {
            return;
        };
        if press.travelled <= self.slop {
            return;
        }
        let rect = Rect::from_points(press.start, press.now);
        if rect.width() < 1.0 && rect.height() < 1.0 {
            return;
        }
        outline(scene, &rect);
    }

    /// Draw the region's pins, each posed by its own [`crate::motion::Placed`]: a pin that just
    /// arrived fades and scales in, one the person moved glides from where it stood, and one the
    /// map no longer holds leaves before it is forgotten. Every pin not drawn this frame is
    /// forgotten once its departure ends, so a map of many places only carries motion for the
    /// ones it shows.
    fn draw_pins(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let now = crate::motion::frame_now();
        let source = crate::motion::input_source();
        let motion = theme.motion();
        // What the region laid out this frame, by key: each pin where its motion has come to.
        let laid = self.laid.borrow().clone();
        let keys: Vec<String> = self
            .pins
            .iter()
            .map(|(key, _, _)| key.clone())
            .filter(|key| laid.contains_key(key))
            .collect();
        let drawn: Vec<(usize, Rect, crate::motion::Pose)> = {
            let mut placed_pins = self.placed.borrow_mut();
            // A pin that left the map leaves from where it last stood, then is forgotten.
            for (key, placed) in placed_pins.iter_mut() {
                if !keys.contains(key) && placed.is_visible(now) {
                    let rect = placed.rect(now);
                    placed.follow(rect, false, motion, source, now);
                }
            }
            placed_pins.retain(|key, placed| keys.contains(key) || placed.is_visible(now));
            // Where the person last pointed, on screen: an answer arriving grows out of it.
            let corner = self.rect.origin().to_vec2();
            let camera = self.shown.get();
            let asked = self
                .asked
                .map(|at| camera.to_screen(self.rect.origin(), at));
            // Each pin the region holds is drawn where it is laid out, arriving as it does. The
            // region lays the pins out in the map's own coordinates and the pins draw where the
            // map stands, so a pose is taken about the pin as it is drawn.
            keys.iter()
                .filter_map(|key| {
                    let placed = placed_pins.get(key)?;
                    let rect = laid.get(key).copied()?;
                    let index = self.pins.iter().position(|(held, _, _)| held == key)?;
                    let grows = self
                        .pins
                        .get(index)
                        .is_some_and(|(_, place, _)| place.grows);
                    placed.is_visible(now).then(|| {
                        let drawn = rect + corner;
                        let progress = placed.progress(now);
                        let pose = match asked {
                            Some(from) if grows => crate::motion::WindowMotion::grown_pose(
                                progress, drawn, from, motion,
                            ),
                            _ => crate::motion::WindowMotion::content_pose(progress, drawn, motion),
                        };
                        (index, drawn, pose)
                    })
                })
                .collect()
        };
        for (index, rect, pose) in drawn {
            let bounds = rect.union(pose.drawn_rect(rect));
            let Some(child) = self.region_mut().and_then(|region| region.child_mut(index)) else {
                continue;
            };
            pose.paint(scene, transform, bounds, |scene, transform| {
                child.draw_child(scene, transform, text, theme, presence);
            });
        }
    }

    /// Whether any pin's motion is still moving at `now`: what asks for frames while it does.
    fn pins_moving(&self, now: crate::time::Instant) -> bool {
        self.placed
            .borrow()
            .values()
            .any(|placed| placed.is_moving(now))
    }

    /// The camera's own gestures: a wheel with Ctrl zooms about the pointer, a middle drag pans,
    /// the arrow keys pan, and two fingers pan and pinch. It moves the lens and nothing in
    /// the layout; the views' own presses fall through to the pins.
    fn camera_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        match event {
            // Two fingers on a touchscreen or a touchpad, and a wheel, over the canvas's free
            // space pan it (`canvas.md` §6); over a place they are the place's own scrolling.
            WindowEvent::MouseWheel { delta, .. } if !ctx.modifiers.state().control_key() => {
                if self.over_pin(self.cursor - self.rect.origin().to_vec2()) {
                    return;
                }
                let by = match delta {
                    winit::event::MouseScrollDelta::LineDelta(x, y) => {
                        Vec2::new(f64::from(*x) * self.key_pan, f64::from(*y) * self.key_pan)
                    }
                    winit::event::MouseScrollDelta::PixelDelta(p) => Vec2::new(p.x, p.y),
                };
                if by == Vec2::ZERO {
                    return;
                }
                let mut camera = self.camera;
                camera.pan_by(by);
                self.set_camera(camera);
                *ctx.handled = true;
            }
            // A pinch, on a touchscreen or a touchpad, zooms about the fingers, as Ctrl and a
            // wheel zoom about the pointer.
            WindowEvent::PinchGesture { delta, .. } => {
                if *delta == 0.0 {
                    return;
                }
                let corner = self.rect.origin();
                let about = self.cursor;
                let mut camera = self.camera;
                camera.zoom_about(corner, about, 1.0 + *delta);
                self.set_camera(camera);
                *ctx.handled = true;
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let notches = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => f64::from(*y),
                    winit::event::MouseScrollDelta::PixelDelta(p) => p.y / 50.0,
                };
                if notches == 0.0 {
                    return;
                }
                let corner = self.rect.origin();
                let about = self.cursor;
                let mut camera = self.camera;
                camera.zoom_about(corner, about, ZOOM_PER_DETENT.powf(notches));
                self.set_camera(camera);
                *ctx.handled = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Middle,
                ..
            } => {
                self.pan = Some(Pan {
                    start: self.cursor,
                    camera: self.camera,
                    active: true,
                });
                // A press on the canvas puts the keyboard in it, so the arrow keys pan it from
                // there without the person looking for a field to reach them from.
                *ctx.focus_target = Some(ctx.node_id);
                *ctx.handled = true;
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                // A place's edge follows the pointer while it is dragged.
                if let Some(drag) = self.edge_drag.as_mut() {
                    drag.now = self.cursor - self.rect.origin().to_vec2();
                    self.dirty = true;
                    *ctx.handled = true;
                    return;
                }
                // A drag across the empty canvas draws its rectangle as it goes, so the person
                // sees what they will point at before they let go (`canvas.md` §1).
                if let Some(mut press) = self.free_press {
                    press.now = self.cursor;
                    press.travelled = (self.cursor - press.start).hypot();
                    self.free_press = Some(press);
                    self.dirty = true;
                }
                let Some(mut pan) = self.pan else {
                    return;
                };
                let travelled = self.cursor - pan.start;
                if !pan.active && travelled.hypot() < self.slop {
                    return;
                }
                pan.active = true;
                self.pan = Some(pan);
                let mut camera = pan.camera;
                camera.pan_by(travelled);
                self.set_camera(camera);
                *ctx.handled = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Middle,
                ..
            } => {
                if self.pan.take().is_some() {
                    *ctx.handled = true;
                }
            }
            WindowEvent::KeyboardInput { event, .. }
                if self.focused
                    && event.state == ElementState::Pressed
                    && !ctx.modifiers.state().control_key()
                    && !ctx.modifiers.state().super_key() =>
            {
                let delta = match &event.logical_key {
                    Key::Named(NamedKey::ArrowLeft) => Some(Vec2::new(self.key_pan, 0.0)),
                    Key::Named(NamedKey::ArrowRight) => Some(Vec2::new(-self.key_pan, 0.0)),
                    Key::Named(NamedKey::ArrowUp) => Some(Vec2::new(0.0, self.key_pan)),
                    Key::Named(NamedKey::ArrowDown) => Some(Vec2::new(0.0, -self.key_pan)),
                    _ => None,
                };
                if let Some(delta) = delta {
                    let mut camera = self.camera;
                    camera.pan_by(delta);
                    self.set_camera(camera);
                    *ctx.handled = true;
                }
            }
            // A right press on the map's free space points at the canvas: the map reports it
            // with the point, and the view opens what belongs there (`canvas.md` §1, §6). One on
            // a place goes to the place first, so what it holds keeps its own menu — a field's
            // copy and paste, a row's actions — and only a press the place leaves is the map's
            // pointing at the place (`Map::handle_event`).
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } if self.rect.contains(self.cursor) => {
                if self
                    .pin_under(self.cursor - self.rect.origin().to_vec2())
                    .is_some()
                {
                    return;
                }
                self.point_at_context(String::new());
                *ctx.handled = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if self.rect.contains(self.cursor)
                && self
                    .edge_at(self.cursor - self.rect.origin().to_vec2())
                    .is_some() =>
            {
                // A press on a place's trailing or bottom edge, or its corner, takes that edge:
                // dragging it sizes the place (`canvas.md` §2), and nothing under it takes it.
                if let Some((key, from, trailing, bottom)) =
                    self.edge_at(self.cursor - self.rect.origin().to_vec2())
                {
                    let local = self.cursor - self.rect.origin().to_vec2();
                    self.edge_drag = Some(EdgeDrag {
                        key,
                        from,
                        start: local,
                        now: local,
                        trailing,
                        bottom,
                    });
                }
                *ctx.handled = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } if self.edge_drag.is_some() => {
                if let Some(drag) = self.edge_drag.take() {
                    let rect = drag.rect();
                    let zoom = self.camera.zoom.max(f64::EPSILON);
                    // The band over a place's view is the handle's, not the place's size.
                    let band = self
                        .pins
                        .iter()
                        .find(|(key, _, _)| *key == drag.key)
                        .map_or(0.0, |(_, place, _)| place.band);
                    if rect != drag.from {
                        self.resize.fire(Some(crate::component::Value::List(vec![
                            crate::component::Value::Text(self.item_of(drag.key)),
                            crate::component::Value::List(vec![
                                crate::component::Value::Int((rect.width() / zoom).round() as i64),
                                crate::component::Value::Int(
                                    ((rect.height() - band) / zoom).round() as i64,
                                ),
                            ]),
                        ])));
                    }
                }
                self.dirty = true;
                *ctx.handled = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                // A left press on the empty canvas is kept until it is let go: one that travelled
                // no further than a click reports its point (`press`), and one that travelled
                // more reports the rectangle it covered (`frame`). A press on a pin is the pin's,
                // not the canvas's, and the map keeps nothing for it.
                // A press outside the map (the shell's bar over it, a panel beside it) is not one
                // on the canvas, whatever reaches the map of it.
                if !self.rect.contains(self.cursor)
                    || self.over_pin(self.cursor - self.rect.origin().to_vec2())
                {
                    self.free_press = None;
                } else {
                    self.free_press = Some(FreePress {
                        start: self.cursor,
                        now: self.cursor,
                        travelled: 0.0,
                    });
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                let Some(press) = self.free_press.take() else {
                    return;
                };
                let origin = self.rect.origin();
                let from = self.camera.to_canvas(origin, press.start);
                if press.travelled > self.slop {
                    // A drag across the canvas reports the rectangle it covered, in canvas
                    // pixels: the person pointed at a frame, and what answers is laid out to it.
                    let to = self.camera.to_canvas(origin, press.now);
                    let rect = Rect::new(
                        from.x.min(to.x),
                        from.y.min(to.y),
                        from.x.max(to.x),
                        from.y.max(to.y),
                    );
                    self.point_at(rect.origin());
                    self.frame.fire(Some(crate::component::Value::List(vec![
                        crate::component::Value::Int(rect.x0.round() as i64),
                        crate::component::Value::Int(rect.y0.round() as i64),
                        crate::component::Value::Int(rect.width().round() as i64),
                        crate::component::Value::Int(rect.height().round() as i64),
                    ])));
                } else {
                    self.point_at(from);
                    self.press.fire(Some(crate::component::Value::List(vec![
                        crate::component::Value::Int(from.x.round() as i64),
                        crate::component::Value::Int(from.y.round() as i64),
                    ])));
                }
                self.dirty = true;
                *ctx.handled = true;
            }
            WindowEvent::Touch(touch) => {
                let phase = match touch.phase {
                    winit::event::TouchPhase::Started => GesturePhase::Begin,
                    winit::event::TouchPhase::Moved => GesturePhase::Update,
                    winit::event::TouchPhase::Ended => GesturePhase::End,
                    winit::event::TouchPhase::Cancelled => GesturePhase::Cancel,
                };
                let at = Point::new(
                    touch.location.x / ctx.scale_factor,
                    touch.location.y / ctx.scale_factor,
                );
                for gesture in self.touch.on_touch(touch.id, at, phase) {
                    match gesture {
                        // One finger points (a tap, a hold, a frame: `touch_claim`); two pan.
                        TouchGestureEvent::Pan { fingers, delta } if fingers >= 2 => {
                            let mut camera = self.camera;
                            camera.pan_by(Vec2::new(delta.0, delta.1));
                            self.set_camera(camera);
                            *ctx.handled = true;
                        }
                        TouchGestureEvent::Pinch { center, scale, .. } => {
                            let corner = self.rect.origin();
                            let mut camera = self.camera;
                            camera.zoom_about(corner, center, scale);
                            self.set_camera(camera);
                            *ctx.handled = true;
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
}

impl Widget for Map {
    fn needs_redraw(&self) -> bool {
        crate::widget::redraw_or_children(self.dirty, self)
    }

    /// Over a place's trailing or bottom edge the pointer shows the edge can be dragged; over
    /// the free canvas it is a text cursor, since a click there puts a caret to write at
    /// (`canvas.md` §7, "Free space invites writing"), and while a middle drag pans the canvas
    /// it holds it. Over a place, what the place holds says.
    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        use winit::window::CursorIcon;
        if self.edge_drag.is_none() && !self.rect.contains(point) {
            return None;
        }
        let local = point - self.rect.origin().to_vec2();
        let edges = self
            .edge_drag
            .as_ref()
            .map(|drag| (drag.trailing, drag.bottom))
            .or_else(|| {
                self.edge_at(local)
                    .map(|(_, _, trailing, bottom)| (trailing, bottom))
            });
        if let Some(edges) = edges {
            return Some(match edges {
                (true, true) => CursorIcon::SeResize,
                (true, false) => CursorIcon::EResize,
                _ => CursorIcon::SResize,
            });
        }
        if self.pan.is_some_and(|pan| pan.active) {
            return Some(CursorIcon::Grabbing);
        }
        if self.over_pin(local) {
            return None;
        }
        // Pointing on the free canvas: a click writes there, a drag frames where to write.
        (!self.press.is_none()).then_some(CursorIcon::Text)
    }

    /// A finger on the canvas's free space points as the pointer does (`canvas.md` §2, "Touch"):
    /// a tap is a click (`press`), a hold is the secondary press (`context`), and a drag is a
    /// left drag that draws a frame (`frame`), never a scroll. Two fingers pan and pinch. On a
    /// place, the place's own content says what a finger does.
    fn touch_claim(&self, point: Point) -> crate::interaction::TouchClaim {
        if self.rect.contains(point) && !self.over_pin(point - self.rect.origin().to_vec2()) {
            crate::interaction::TouchClaim::Drag
        } else {
            crate::interaction::TouchClaim::Pointer
        }
    }

    /// Whether a pin is arriving, leaving or gliding: what asks for frames while it does,
    /// and what its own pins ask for. Nothing once every pin rests at its place.
    fn needs_animation_frame(&self) -> bool {
        let now = crate::motion::frame_now();
        self.moved
            || self.glide.is_some_and(|glide| glide.is_gliding(now))
            || self.pins_moving(now)
            || crate::widget::defaults::needs_animation_frame(&|visit| self.for_each_child(visit))
    }

    fn name(&self) -> &'static str {
        "Map"
    }

    /// A canvas takes the keyboard, so its own presses can pan it: a middle press puts the
    /// caret in it (`shell.md` §3, and `canvas.tl`'s "a middle drag, Ctrl and a wheel, the arrow
    /// keys"), and the arrow keys then pan it a line a press.
    fn is_focusable(&self) -> bool {
        true
    }

    fn set_focus(&mut self, focused: bool) {
        if self.focused != focused {
            self.focused = focused;
            self.dirty = true;
        }
    }

    fn is_focused(&self) -> bool {
        self.focused
    }

    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.slop = f64::from(theme.tokens.metrics.click_slop);
        self.key_pan = f64::from(theme.tokens.metrics.wheel_line);
        self.motion = theme.motion();
        if !std::mem::replace(&mut self.took, true)
            && let Some(lens) = crate::motion::take_kept::<Lens>(&self.name)
        {
            self.take_over(lens);
        }
        // What arrived is shown before the places are laid out, so this layout stands them, and
        // the windows in their wells, where the camera carried them.
        self.show_asked();
        self.rebuild_region(theme);
        let node = self.parts.build(
            Style {
                size: Size {
                    width: taffy::Dimension::percent(1.0),
                    height: taffy::Dimension::percent(1.0),
                },
                ..Style::default()
            },
            tree,
            theme,
            map,
        );
        self.rect = Rect::ZERO;
        map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, tree: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.parts.apply_layout(tree, node, origin);
        self.rect = crate::widget::layout_rect(tree, node, origin);
        // The answer the person asked for stands whole in view as it grows: laid out now, the
        // camera follows it to where it has grown, the least it takes.
        self.keep_answer();
    }

    fn layout_needs_measure(&self) -> bool {
        self.invalidation.is_pending()
            || self
                .parts
                .children
                .iter()
                .any(|child| child.layout_needs_measure())
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // A page shown as HTML shows it as one image.
        crate::scene_parts::picture();
        // A canvas clips what it holds: a place outside the viewport is neither drawn nor hit.
        let viewport = self.rect;
        let clips = viewport.width() > 0.0 && viewport.height() > 0.0;
        if self.ground && clips {
            let _ = crate::widget::primitives::paint_surface(
                scene,
                transform,
                viewport,
                crate::widget::primitives::SurfaceLevel::Ground,
                theme,
            );
            self.draw_grain(scene, transform, theme);
        }
        if clips {
            scene.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                1.0,
                transform,
                &viewport,
            );
        }
        self.draw_hint(scene, transform, text, theme);
        self.draw_frame_outline(scene, transform, theme);
        self.draw_pins(scene, transform, text, theme, presence);
        if clips {
            scene.pop_layer();
        }
    }

    fn child_clip_rect(&self) -> Option<Rect> {
        (self.rect.width() > 0.0).then_some(self.rect)
    }

    /// A canvas pans both ways: a place past the viewport is reached by panning, not cut off.
    fn layout_info(&self) -> Option<crate::widget::LayoutInfo> {
        Some(crate::widget::LayoutInfo::Scroll(
            crate::widget::ScrollLayout {
                direction: crate::widget::ScrollDirection::Both,
            },
        ))
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    /// The canvas as a group named for what it is; its pins' views are its children, walked
    /// through the region that places them.
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        Some(
            crate::a11y::AccessNode::new(crate::a11y::Role::Group)
                .with_optional_name((!self.name.is_empty()).then(|| self.name.clone()))
                .with_optional_description(self.shown_hint().map(str::to_owned)),
        )
    }

    /// A pin holding the pointer — a drag it latched, a menu open in what it shows — owns input
    /// as it does anywhere: without this the map would inherit the leaf default, and a drag a
    /// pin started would lose the pointer the moment it left the pin's box.
    fn wants_pointer_capture(&self) -> bool {
        self.parts
            .children
            .iter()
            .any(|child| child.wants_pointer_capture())
    }

    fn children(&self) -> Vec<&dyn Widget> {
        self.parts
            .children
            .iter()
            .map(|child| child.as_ref())
            .collect()
    }

    fn for_each_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) {
        for child in &self.parts.children {
            visitor(child.as_ref());
        }
    }

    fn for_each_layout_child(&self, visitor: &mut dyn FnMut(&dyn Widget)) -> bool {
        // The map lays out only its one region, which lays the pins out itself.
        for child in &self.parts.children {
            visitor(child.as_ref());
        }
        true
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.parts
            .children
            .iter_mut()
            .map(|child| child.as_mut() as &mut (dyn Widget + '_))
            .collect()
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        for child in &mut self.parts.children {
            child.mark_drawn();
        }
    }

    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        for child in &self.parts.children {
            child.collect_damage(regions);
        }
    }

    fn take_structure_dirty(&mut self) -> bool {
        let mut dirty = false;
        for child in &mut self.parts.children {
            dirty |= child.take_structure_dirty();
        }
        dirty
    }

    fn take_structure_change(&mut self) -> crate::widget::StructureChange {
        let mut change = crate::widget::StructureChange::None;
        for child in &mut self.parts.children {
            change = change.join(child.take_structure_change());
        }
        change
    }

    fn sync(&mut self, state: &crate::widget::AppModel, path: &crate::widget::ModelPath) {
        self.dirty = true;
        for child in &mut self.parts.children {
            child.sync(state, path);
        }
    }

    /// The map's motion steps here, as a scroll's glide does: the camera on its way and the pins
    /// gliding are laid out again on each frame they run, the one they land on included, so the
    /// windows in the pins' wells stand where the pins are drawn.
    fn update(&mut self, state: &crate::widget::AppModel, path: &crate::widget::ModelPath) -> bool {
        let now = crate::motion::frame_now();
        let moving = self.step_camera(now) | self.pins_moving(now);
        if moving || self.moved {
            self.dirty = true;
            self.invalidation.invalidate();
            crate::widget::note_layout_moved();
        }
        self.moved = moving;
        let mut changed = false;
        for child in &mut self.parts.children {
            changed |= child.update(state, path);
        }
        // A resource another view dragged and let go inside the map is a drop here: the map
        // reports what it carried and the canvas point under the pointer, and the view places
        // it there (the shell's own `open`, or its place at a point).
        if let Some(landed) = crate::widget::drag_bus::take(self.drop_key(), self.rect) {
            let crate::dnd::DragPayload::Custom { kind, blob } = landed.payload else {
                return changed;
            };
            if kind != crate::widget::drag_source::RESOURCE_PAYLOAD
                && kind != crate::widget::drag_run::ITEM_PAYLOAD
            {
                return changed;
            }
            if let Ok(resource) = String::from_utf8(blob)
                && !resource.is_empty()
            {
                let at = self.camera.to_canvas(self.rect.origin(), landed.pointer);
                self.point_at(at);
                self.drop.fire(Some(crate::component::Value::List(vec![
                    crate::component::Value::Resource(resource),
                    crate::component::Value::List(vec![
                        crate::component::Value::Int(at.x.round() as i64),
                        crate::component::Value::Int(at.y.round() as i64),
                    ]),
                ])));
                changed = true;
            }
        }
        changed
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        self.dirty = true;
        // The camera first: a wheel with Ctrl zooms, a middle drag pans, the arrow keys pan.
        self.camera_event(event, ctx);
        if *ctx.handled {
            return;
        }
        // Otherwise the pins take it, as they would anywhere.
        for (child, node) in self.parts.children.iter_mut().zip(self.parts.nodes.clone()) {
            let path = ctx.path;
            child.handle_event(event, &mut ctx.reborrow(path, node));
            if *ctx.handled {
                break;
            }
        }
        // A right press on a place that nothing in it took — no field, no row with a menu of its
        // own — is the person pointing at the place: the map reports it with the place's key.
        if !*ctx.handled
            && let WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } = event
            && let Some(pin) = self.pin_under(self.cursor - self.rect.origin().to_vec2())
        {
            self.point_at_context(pin);
            *ctx.handled = true;
        }
    }
}

/// The rectangle `[x, y, width, height]` names in canvas pixels, when it names one with an area.
fn outline_of(outline: &[i64]) -> Option<Rect> {
    let [x, y, width, height] = outline else {
        return None;
    };
    (*width > 0 && *height > 0)
        .then(|| Rect::from_origin_size((*x as f64, *y as f64), (*width as f64, *height as f64)))
}

/// `v` as `f32`, for the layout solver.
#[expect(
    clippy::cast_possible_truncation,
    reason = "logical pixels: a canvas coordinate of a view, far inside f32"
)]
fn f32_of(v: f64) -> f32 {
    v as f32
}

#[path = "map_answer.rs"]
mod answer;
use answer::OutlineLook;

#[cfg(test)]
#[path = "map_motion_tests.rs"]
mod motion_tests;

#[cfg(test)]
#[path = "map_pointing_tests.rs"]
mod pointing_tests;

#[cfg(test)]
#[path = "map_canvas_tests.rs"]
mod canvas_tests;

#[cfg(test)]
#[path = "map_answer_tests.rs"]
mod answer_tests;
