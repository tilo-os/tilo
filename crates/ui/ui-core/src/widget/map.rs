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
use vello::kurbo::{Affine, Line, Point, Rect, Vec2};
use vello::peniko::{BlendMode, Fill};

use crate::component::{Component, Described, Event, PartType, PartViews, ResourceId, Source};
use crate::gesture::{GesturePhase, TouchGestureArena, TouchGestureEvent};

use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::DrawChild;
use crate::widget::composition_parts::Parts;
use crate::widget::{
    Element, Manual, ManualInvalidation, MeasureConstraint, Widget, WidgetLayoutError,
};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

/// The smallest and largest zoom a canvas is shown at: far enough out to see a whole canvas, far
/// enough in to read a note.
pub const MAP_ZOOM_MIN: f64 = 0.2;
pub const MAP_ZOOM_MAX: f64 = 2.5;

/// The zoom a map of the world draws back to: the whole world across about a screen and a half,
/// so a map of every place stands in view at once.
pub const MAP_ZOOM_WORLD_MIN: f64 = 1_600.0 / MERCATOR_WORLD;

/// The closest a map of the world draws in: the tile servers' closest level (19), where a screen
/// shows a street. A canvas keeps [`MAP_ZOOM_MAX`], which is far enough to read a note.
pub const MAP_ZOOM_WORLD_MAX: f64 = 128.0;

/// How much one wheel notch zooms, when a gesture zooms over the map.
const ZOOM_PER_DETENT: f64 = 1.1;

/// How long the camera must stand still before a move of the person's is over: the notches of one
/// wheel turn, a zoom's last frame and a drag's last pixels arrive together.
const REST_AFTER: crate::time::Duration = crate::time::Duration::from_millis(150);

/// A basemap tile's side, in pixels of the tile server's own imagery: the size a tile is at when
/// the camera takes a tile's pixel for a screen pixel.
const TILE_PIXELS: f64 = 256.0;

/// The closest basemap level a map asks for: the tile servers stop at 19.
const TILE_ZOOM_MOST: i64 = 19;

/// How many levels up a map looks for a loaded tile to stand under one still on its way: at a
/// fresh zoom a coarser tile covers the screen while the sharp ones load.
const TILE_FALLBACK_LEVELS: i64 = 4;

/// A marker's dot, and the ring it stands in, in logical pixels on screen: a place a person
/// points at, the same size at every zoom.
const MARKER_RADIUS: f64 = 5.0;
const MARKER_RING: f64 = 2.0;

/// Draw `image` into `rect`: the tile scaled to the screen the camera puts it at.
fn draw_tile(scene: &mut Scene, transform: Affine, image: &vello::peniko::ImageData, rect: Rect) {
    let width = f64::from(image.width).max(1.0);
    let height = f64::from(image.height).max(1.0);
    let scale = Affine::scale_non_uniform(rect.width() / width, rect.height() / height);
    scene.draw_image(
        image,
        transform * Affine::translate(rect.origin().to_vec2()) * scale,
    );
}

/// The ink a line of the map draws in: a route the person takes, a way walked, a public
/// transport leg, or a quiet line — theme roles, never colours of the map's own.
fn track_ink(tone: MapTrackTone, theme: &Theme) -> vello::peniko::Color {
    match tone {
        MapTrackTone::Route => theme.tokens.colors.intent.accent,
        MapTrackTone::Walk => theme.tokens.colors.text_dim,
        MapTrackTone::Transit => theme.tokens.colors.semantic.info,
        MapTrackTone::Quiet => theme.tokens.colors.text_tertiary,
    }
}

/// The ink a marker's dot draws in, by what it is: the categories in the theme's data colours,
/// a journey's ends in its status inks, the device and what it points at in the accent and the
/// quiet ink.
fn marker_ink(kind: MapMarkerKind, theme: &Theme) -> vello::peniko::Color {
    let colors = &theme.tokens.colors;
    match kind {
        MapMarkerKind::Landmark | MapMarkerKind::Museum => colors.semantic.info,
        MapMarkerKind::Cafe | MapMarkerKind::Restaurant | MapMarkerKind::Bar => {
            colors.semantic.warning
        }
        MapMarkerKind::Park | MapMarkerKind::Start => colors.semantic.success,
        MapMarkerKind::Transit | MapMarkerKind::Current => colors.intent.accent,
        MapMarkerKind::Hotel => colors.presence.series(6),
        MapMarkerKind::Shop => colors.presence.series(3),
        MapMarkerKind::Hospital | MapMarkerKind::End => colors.semantic.danger,
        MapMarkerKind::Other | MapMarkerKind::Poi => colors.text_dim,
    }
}

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
        self.zoom_about_within(corner, about, factor, MAP_ZOOM_MIN, MAP_ZOOM_MAX);
    }

    /// [`MapCamera::zoom_about`] within `min` and `max`: a canvas keeps its own limits, a map
    /// of the world draws back to the whole world.
    pub fn zoom_about_within(
        &mut self,
        corner: Point,
        about: Point,
        factor: f64,
        min: f64,
        max: f64,
    ) {
        let held = self.to_canvas(corner, about);
        self.zoom = (self.zoom * factor).clamp(min, max);
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
#[derive(Component)]
#[component(name = "Map", role = "Group", label = "name")]
pub struct MapProps {
    /// What the map is, for assistive technology.
    pub name: Option<String>,
    /// Whether it is a desktop: the windows it holds stand on the desktop's ground
    /// ([`SurfaceLevel::Ground`](crate::widget::primitives::SurfaceLevel)), not on the content
    /// colour of the window the map is in.
    pub ground: bool,
    /// Whether it is a map of the world: a pin stands centred on the place it names and keeps
    /// the size the view gave it on screen at every zoom, and the camera draws back until the
    /// whole world fits.
    pub world: bool,
    /// Where a map of the world's imagery comes from: a tile address with `%z`, `%x` and `%y`
    /// standing for the tile's zoom and its column and row
    /// (`https://tile.openstreetmap.org/%z/%x/%y.png`). The tiles draw under everything else,
    /// scaled with the camera, at the zoom that shows them sharpest; without one the map draws
    /// its ground, and a map of the world a faint grid at the places it names. A host serves the
    /// pictures (`crate::pictures::set_remote_fetcher`).
    pub basemap: Option<String>,
    /// The pins, in the order the map draws them (the last on top).
    #[component(part(default))]
    pub pins: Vec<MapPinProps>,
    /// The lines drawn across the map under its markers and pins: a route, the way one leg of a
    /// journey goes.
    #[component(part)]
    pub tracks: Vec<MapTrackProps>,
    /// What stands on the map at a place, drawn as a marker of its own size on screen whatever
    /// the zoom: where a search found something, the device's own place. Pointing at one reports
    /// it (`marker`); the view opens what it is.
    #[component(part)]
    pub markers: Vec<MapMarkerProps>,
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
    /// The pointer's right button pressed on the map: carries the key of the pin pin_places under
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
    /// A hugging pin's view came to a height of its own and the pin is laid out at it: carries the
    /// pin's key and its drawn height in canvas pixels. Reported once each time that height
    /// changes, never every frame, so a view that reads it as free area on the canvas (`canvas.md`
    /// §1, §7) learns what the pin takes without a report a frame.
    #[component(event(value = String, at = Vec<i64>))]
    pub drawn: Event,
    /// A rectangle the view points at, in canvas pixels (`x`, `y`, width, height), drawn as the
    /// light outline a drag across the canvas draws: the rectangle the person dragged, kept while
    /// they say what fills it.
    pub outline: Vec<i64>,
    /// What the empty map says once, quietly, at the middle of what it shows: what a person
    /// does on it ("Click anywhere and say what you want"). Drawn only while it holds no pin,
    /// and read to assistive technology as the map's description.
    pub hint: Option<String>,
    /// The latitude the map is centred on, with `longitude`: the place at its middle when the
    /// view sets it, so a map of places opens on a place. `None` leaves it a canvas.
    pub latitude: Option<f64>,
    /// The longitude the map is centred on, with `latitude`.
    pub longitude: Option<f64>,
    /// The zoom the map opens at, about its centre (`latitude`, `longitude`).
    pub zoom: Option<f64>,
    /// The camera came to rest where the person left it: carries the place at its middle as its
    /// latitude then its longitude, the zoom it shows, and what it shows at its edges as its
    /// northern, southern, eastern and western degrees, in that order. Reported once each time a
    /// pan, pinch, wheel or key move ends, and never for a move the view itself set, so a view
    /// that saves what the person sees knows where they are looking.
    #[component(event(value = Vec<f64>))]
    pub rest: Event,
    /// The pointer pressed the map's empty surface and let go there, a click: carries the place
    /// under it, its latitude then its longitude, for a map centred on a place.
    #[component(event(value = Vec<f64>))]
    pub at: Event,
    /// The pointer pressed a marker and let go there, a click: carries the name the marker was
    /// given (`MapMarker`'s `name`), or its key where it has none. The view opens what the marker
    /// is; the map reports nothing of its own for that press.
    #[component(event(value = String))]
    pub marker: Event,
    /// Whether a press on its empty surface runs now.
    pub enabled: bool,
    /// Why it cannot be pressed now, where the name does not say (an action whose requirement
    /// does not hold): spoken as its description beside its name.
    pub description: Option<String>,
}

/// A map takes a press until something says otherwise.
impl Default for MapProps {
    fn default() -> Self {
        Self {
            name: None,
            ground: false,
            world: false,
            basemap: None,
            pins: Vec::new(),
            tracks: Vec::new(),
            markers: Vec::new(),
            drop: Event::None,
            press: Event::None,
            frame: Event::None,
            context: Event::None,
            resize: Event::None,
            seen: Event::None,
            drawn: Event::None,
            outline: Vec::new(),
            hint: None,
            latitude: None,
            longitude: None,
            zoom: None,
            rest: Event::None,
            at: Event::None,
            marker: Event::None,
            enabled: true,
            description: None,
        }
    }
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
    /// The latitude of the place the pin stands on, with `longitude`: the map projects it onto
    /// the canvas, so a pin stands on its place at every zoom. Given, `x` and `y` are not read.
    pub latitude: Option<f64>,
    /// The longitude of the place the pin stands on.
    pub longitude: Option<f64>,
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
    #[component(part(default))]
    pub body: Vec<Element>,
}

impl PartType for Vec<MapPinProps> {
    const MANY: bool = true;

    fn accepts() -> crate::component::Accepts {
        crate::component::Accepts::Component(MapPinProps::NAME.into())
    }

    fn fill(&mut self, source: &mut dyn Source, part: &str) -> Result<(), String> {
        let mut out = Vec::new();
        let mut failed: Option<String> = None;
        source.components(part, &mut |child| match MapPinProps::from_source(child) {
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

/// A line across a map of the world: a route, one leg of a journey, the way a walk goes.
#[derive(Component, Default)]
#[component(name = "MapTrack", role = "Image", label = "name")]
pub struct MapTrackProps {
    /// The line's stable key.
    #[component(key)]
    pub key: String,
    /// What it is, for assistive technology.
    pub name: Option<String>,
    /// The places it runs through, in degrees: each place's latitude then its longitude, in turn
    /// (`[52.52, 13.40, 52.53, 13.41]`).
    #[component(required)]
    pub points: Vec<f64>,
    /// What it says of itself, as the theme's ink: a route the person is taking, a way they walk,
    /// a public transport leg, or a quiet line.
    pub tone: Option<MapTrackTone>,
    /// How thick it draws on screen, in logical pixels; without it, one line of the theme.
    pub width: Option<f64>,
    /// Whether it draws as a dashed line: a walk, a way that is not the route itself.
    pub dashed: bool,
}

/// What a line across the map says of itself, as the theme's ink.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum MapTrackTone {
    /// A route the person is taking.
    #[default]
    Route,
    /// A way a person walks.
    Walk,
    /// A public transport leg.
    Transit,
    /// A quiet line.
    Quiet,
}

crate::component::choice!(MapTrackTone, "MapTrackTone", {
    Route => "route",
    Walk => "walk",
    Transit => "transit",
    Quiet => "quiet",
});

crate::component::part_type!(MapTrackProps);

/// What a marker is: the category of place it names, or the part a journey or the device plays in
/// it. The marker's shape says it, in the theme's inks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum MapMarkerKind {
    Landmark,
    Cafe,
    Restaurant,
    Bar,
    Museum,
    Park,
    Transit,
    Hotel,
    Shop,
    Hospital,
    #[default]
    Other,
    /// Where the device is.
    Current,
    /// Where a journey starts, and where it ends.
    Start,
    End,
    /// A place of the map waiting to be opened.
    Poi,
}

crate::component::choice!(MapMarkerKind, "MapMarkerKind", {
    Landmark => "landmark",
    Cafe => "cafe",
    Restaurant => "restaurant",
    Bar => "bar",
    Museum => "museum",
    Park => "park",
    Transit => "transit",
    Hotel => "hotel",
    Shop => "shop",
    Hospital => "hospital",
    Other => "other",
    Current => "current",
    Start => "start",
    End => "end",
    Poi => "poi",
});

/// One marker on a map of the world: a place a view names, drawn at its size on screen whatever
/// the zoom, standing on the place it names.
#[derive(Component, Default)]
#[component(name = "MapMarker", role = "Image", label = "name")]
pub struct MapMarkerProps {
    /// The marker's stable key.
    #[component(key)]
    pub key: String,
    /// What it stands for: a place's name, read to assistive technology and on pointing.
    pub name: Option<String>,
    /// The latitude of the place it stands on.
    #[component(required)]
    pub latitude: f64,
    /// The longitude of the place it stands on.
    #[component(required)]
    pub longitude: f64,
    /// What it is, which its shape and ink say.
    pub kind: Option<MapMarkerKind>,
    /// Whether it is the one the person chose: drawn a size larger, with a ring.
    pub selected: bool,
    /// Who else is at the place now: a member's or an agent's lens on it (`lenses(place)`), whose
    /// face docks on the marker as a list row wears the member at its item. The map draws them in
    /// the presence of the window it is in, and shows none for this person's own views.
    pub presence: Option<crate::component::Person>,
}

crate::component::part_type!(MapMarkerProps);

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
    /// It is a map of the world: pins stand centred on their places at the size they were given
    /// on screen, and the camera draws back to [`MAP_ZOOM_WORLD_MIN`].
    world: bool,
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
    /// The edges that resize the places on screen, for the shell's control hints.
    hint_edges: crate::control_hints::anchors::EdgesAnchor,
    /// The pins that hug what they hold but whose view said no height of its own at the last
    /// solve (a turn before its words arrive), by key: laid out at their most, which is not yet
    /// how tall they are, so the camera does not follow them there.
    unsaid: Rc<RefCell<std::collections::HashSet<String>>>,
    /// How tall each hugging pin was last reported as drawn, by key (canvas pixels): the map
    /// reports a pin's drawn height (`Self::drawn`) only when it differs from this, so a view that
    /// reads it as free area on the canvas learns of a change once, never once a frame.
    drawn_said: Rc<RefCell<HashMap<String, u32>>>,
    /// Where each pin stands on the canvas, in the order of `pins`, as the region reads it at each
    /// solve: a pin moved in place ([`Described::set_props`]) is laid out where it stands now,
    /// though the region was built while it stood elsewhere.
    pin_places: Rc<RefCell<Vec<PinPlace>>>,

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
    /// Whether a press on it runs now, and why it cannot when it does not.
    enabled: bool,
    description: Option<String>,
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
    /// What a hugging pin reports as its view comes to a height of its own: the pin's key and its
    /// drawn height in canvas pixels, once each time that height changes.
    drawn: Event,
    /// What a press on a map centred on a place reports: the place it landed on, its latitude
    /// then its longitude. A canvas has no place, so it reports nothing here.
    at: Event,
    /// The place the map is centred on, if it is a map of places: its latitude and longitude.
    /// The camera centres it when the view sets it, never after the person pans or zooms.
    focus: Option<(f64, f64)>,
    /// The zoom the map opens at about `focus`: `None` keeps the camera's own.
    focus_zoom: Option<f64>,
    /// Where a map of the world's imagery comes from: a tile address with `%z`, `%x` and `%y`.
    basemap: Option<String>,
    /// The lines drawn under the markers: each its key, the places it runs through in degrees, its
    /// ink, how thick it draws in logical pixels, and whether it is dashed.
    tracks: Vec<(String, Vec<(f64, f64)>, MapTrackTone, f64, bool)>,
    /// The markers drawn over the lines: each its key, its place in degrees, what it is, whether
    /// it is chosen, what it is called, and who else is at it.
    markers: Vec<(
        String,
        (f64, f64),
        MapMarkerKind,
        bool,
        Option<String>,
        Option<crate::component::Person>,
    )>,
    /// What the camera reports when the person's own move ends.
    rest: Event,
    /// When the camera is to be called at rest, once it has stood still that long.
    rest_due: Option<crate::time::Instant>,
    /// What a click on a marker reports.
    marker_event: Event,
    /// The view set a new centre since the map was last laid out: centre it once, next layout.
    recentre: bool,
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
    /// after it that grow from where it was asked, the one pin_places nearest that point (the turn
    /// at the caret, the thing found at a click). The camera keeps it whole in view as it grows,
    /// framed with the places arriving after it (an agent's view beside it) as far as they fit
    /// in view together (`map_answer.rs`). The person's own pan or zoom lets it go.
    answer: Option<String>,
    /// The places pin_places when the person last pointed, by key: every other place is what
    /// arrived since, the answer and what it opened beside it, framed with it (`map_answer.rs`).
    asked_among: Vec<String>,
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
    /// How tall each hugging pin was last reported as drawn, by key: a map built again around the
    /// same pins reports no height it reported already, so a view reading it as the canvas's free
    /// area is not told the same height again by a rebuild.
    drawn_said: HashMap<String, u32>,
    /// What the person's hand is doing on the map that was built again under it: a press not let
    /// go yet, a pan or an edge being dragged, and where the pointer is. The release reaches the
    /// new map, which answers it as the old one would have; without these a click made while an
    /// answer arrived put no caret, and a drag across a rebuild sized nothing.
    hand: Hand,
}

/// A gesture under way on a map, and where the pointer is: what a rebuild hands on ([`Lens`]).
#[derive(Clone, Debug, Default)]
struct Hand {
    cursor: Point,
    pan: Option<Pan>,
    free_press: Option<FreePress>,
    edge_drag: Option<EdgeDrag>,
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
                drawn_said: std::mem::take(&mut *self.drawn_said.borrow_mut()),
                hand: Hand {
                    cursor: self.cursor,
                    pan: self.pan.take(),
                    free_press: self.free_press.take(),
                    edge_drag: self.edge_drag.take(),
                },
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
            world: false,
            rect: Rect::ZERO,
            slop: f64::from(crate::tokens::Metrics::BASE.click_slop),
            key_pan: f64::from(crate::tokens::Metrics::BASE.wheel_line),
            placed: Rc::default(),
            laid: Rc::default(),
            hint_edges: crate::control_hints::anchors::EdgesAnchor::new(),
            unsaid: Rc::default(),
            drawn_said: Rc::default(),
            pin_places: Rc::default(),
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
            enabled: true,
            description: None,
            frame: Event::None,
            context: Event::None,
            resize: Event::None,
            seen: Event::None,
            drawn: Event::None,
            at: Event::None,
            focus: None,
            focus_zoom: None,
            basemap: None,
            tracks: Vec::new(),
            markers: Vec::new(),
            rest: Event::None,
            rest_due: None,
            marker_event: Event::None,
            recentre: false,
            edge_drag: None,
            outline: None,
            key: String::new(),
            items: HashMap::new(),
            editing: None,
            hint: None,
            asked: None,
            answer: None,
            asked_among: Vec::new(),
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
        // The person's own hand is on the canvas: it stands where they put it, whether or not a
        // reveal was on its way there.
        self.glide = None;
        self.shown.set(camera);
        if !self.camera.same(camera) {
            self.camera = camera;
            self.invalidation.invalidate();
        }
    }

    /// The person moved the camera: the lens is theirs now, and it comes to rest once it has
    /// stood still for [`REST_AFTER`], which the map then reports (`rest`).
    fn camera_moved(&mut self, camera: MapCamera) {
        self.pan_by_hand(camera);
        self.rest_due = Some(crate::motion::frame_now() + REST_AFTER);
    }

    /// Report where the camera came to rest: the place at its middle, its zoom, and the degrees
    /// at what it shows are its northern, southern, eastern and western edges.
    fn report_rest(&mut self) {
        let corner = self.rect.origin();
        let camera = self.camera;
        let (latitude, longitude) =
            mercator_unproject(camera.to_canvas(corner, self.rect.center()));
        let (north, west) = mercator_unproject(camera.to_canvas(corner, corner));
        let (south, east) =
            mercator_unproject(camera.to_canvas(corner, Point::new(self.rect.x1, self.rect.y1)));
        self.rest.fire(Some(crate::component::Value::List(vec![
            crate::component::Value::Float(latitude),
            crate::component::Value::Float(longitude),
            crate::component::Value::Float(camera.zoom),
            crate::component::Value::Float(north),
            crate::component::Value::Float(south),
            crate::component::Value::Float(east),
            crate::component::Value::Float(west),
        ])));
    }

    /// The closest and furthest the camera draws this map back and in: a canvas keeps
    /// [`MAP_ZOOM_MIN`], a map of the world draws back to the whole world.
    fn zoom_limits(&self) -> (f64, f64) {
        if self.world {
            (MAP_ZOOM_WORLD_MIN, MAP_ZOOM_WORLD_MAX)
        } else {
            (MAP_ZOOM_MIN, MAP_ZOOM_MAX)
        }
    }

    /// Zoom `camera` by `factor` about the screen point `about`, within this map's own limits.
    fn zoom_camera(&self, camera: &mut MapCamera, corner: Point, about: Point, factor: f64) {
        let (least, most) = self.zoom_limits();
        camera.zoom_about_within(corner, about, factor, least, most);
    }

    /// Centre the camera on the place the view set, at the zoom it set: once, on the first
    /// layout after the view changed it. The person's own pan and zoom stand after that.
    fn centre_on_focus(&mut self) {
        if !self.recentre {
            return;
        }
        let Some((latitude, longitude)) = self.focus else {
            return;
        };
        let size = self.rect.size();
        if size.width <= 0.0 || size.height <= 0.0 {
            return;
        }
        let (least, most) = self.zoom_limits();
        let zoom = self
            .focus_zoom
            .map_or(self.camera.zoom, |zoom| zoom.clamp(least, most));
        let centre = mercator_project(latitude, longitude);
        let origin = centre - (size.to_vec2() * 0.5) / zoom.max(f64::EPSILON);
        self.set_camera(MapCamera { origin, zoom });
        self.recentre = false;
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
        // The heights the map reported before are the heights it reported: a pin that did not
        // change is not reported again, and one that comes back to the canvas keeps what it said.
        self.drawn_said.borrow_mut().extend(lens.drawn_said);
        // What the person pointed at before the map was built again is still what an arriving
        // place answers.
        if self.asked.is_none() {
            self.asked = lens.asked;
            self.asked_among = lens.asked_among;
        }
        self.take_answer(&lens.pins, lens.answer);
        // The gesture under way goes on on this map: its release is answered here.
        self.cursor = lens.hand.cursor;
        self.pan = self.pan.take().or(lens.hand.pan);
        self.free_press = self.free_press.take().or(lens.hand.free_press);
        self.edge_drag = self.edge_drag.take().or(lens.hand.edge_drag);
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
        let (least, most) = self.zoom_limits();
        let zoom = (self.rect.width() / bounds.width().max(1.0))
            .min(self.rect.height() / bounds.height().max(1.0))
            .clamp(least, most);
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

    /// Tell the shell's control hints ([`crate::control_hints`]) where the edges that resize the
    /// places on screen are, and, when the places drawn since `windows` hold windows, whether some
    /// lie out of view to either side.
    fn tell_hints(&self, transform: Affine, windows: u64) {
        let origin = self.rect.origin().to_vec2();
        let laid: Vec<Rect> = self
            .laid
            .borrow()
            .values()
            .map(|rect| *rect + origin)
            .collect();
        let resizes = !self.resize.is_none();
        self.hint_edges.drawn(
            transform,
            laid.iter()
                .filter(|rect| resizes && rect.overlaps(self.rect))
                .map(|rect| Line::new((rect.x1, rect.y0), (rect.x1, rect.y1))),
        );
        if crate::control_hints::anchors::windows_drawn() != windows {
            crate::control_hints::anchors::windows_beyond(
                laid.iter().any(|rect| rect.x1 <= self.rect.x0),
                laid.iter().any(|rect| rect.x0 >= self.rect.x1),
            );
        }
    }

    /// The person moved the camera by hand to `camera`: a pan, which the shell's control hints
    /// teach ([`crate::control_hints`]).
    fn pan_by_hand(&mut self, camera: MapCamera) {
        self.set_camera(camera);
        crate::control_hints::performed::note(
            crate::control_hints::performed::Performed::PannedView,
        );
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
    /// "a place next to where it came from"). `keep` is every place pin_places wholly in the
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

    /// The key of the pin pin_places at `local` (this map's own coordinates), if one does: the
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
        // On a canvas the person points at, the pointing itself (the caret, which does not grow
        // out of anything) stands where they pointed, in view: it never moves the camera. Only
        // an answer and what it places do (`canvas.md` §7).
        if self.asked.is_some() && !place.grows {
            return;
        }
        let zoom = self.camera.zoom.max(f64::EPSILON);
        let world = self.world;
        let canvas = |place: &PinPlace| {
            // A place whose height its view says is as tall as a window is wide over tall; one
            // that hugs what it holds is shown by its top, so a short answer in a tall room
            // does not carry the camera to the room's foot. Its caption's band over it is shown
            // with it. A world pin takes the screen size its view gave it, centred on its place.
            let height = match place.height {
                Some(height) if place.hug => height.min(LEAST_SIDE),
                Some(height) => height,
                None => place.width.max(1.0) / 1.6,
            };
            let band = place.band / zoom;
            if world {
                let width = place.width.max(1.0) / zoom;
                let height = height / zoom + band;
                Rect::from_origin_size(
                    Point::new(place.x - width / 2.0, place.y - height / 2.0),
                    (width, height),
                )
            } else {
                Rect::from_origin_size(
                    Point::new(place.x, place.y - band),
                    (place.width.max(1.0), height + band),
                )
            }
        };
        // The person's answer is framed with what arrives beside it once both are laid out, at
        // the sizes they stand at ([`Self::keep_answer`]).
        if self.answer.as_deref().is_some_and(|answer| answer != key) {
            return;
        }
        let area = canvas(place);
        // What the person was looking at: every other place pin_places wholly in the viewport.
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
        *self.pin_places.borrow_mut() = self.pins.iter().map(|(_, place, _)| *place).collect();
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
        let places = Rc::clone(&self.pin_places);
        let keys: Vec<String> = self.pins.iter().map(|(key, _, _)| key.clone()).collect();
        // The camera is read at each solve, not when the region is built: a pan, a zoom or a
        // glide of the camera lays the places out again without building the region again.
        let shown = Rc::clone(&self.shown);
        let followed = Rc::clone(&self.followed);
        // A map of the world lays its pins out at the size the view gave them on screen, centred
        // on their places; a canvas scales them with the camera, from their top-leading corner.
        let world = self.world;
        let moving = Rc::clone(&self.placed);
        let motion = self.motion;
        let laid: Rc<RefCell<HashMap<String, Rect>>> = Rc::clone(&self.laid);
        let unsaid = Rc::clone(&self.unsaid);
        let drawn_said = Rc::clone(&self.drawn_said);
        let put_drawn = self.drawn.clone();
        // What each pin stands for, by its key, as its view named it: the report names the place
        // the way pointing at it does (`item_of`), never the toolkit's own key.
        let items = self.items.clone();
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
            // What each hugging pin that stands at a height of its own is drawn at this solve, in
            // canvas pixels, by its key: reported below, only where it changed.
            let mut drawn_now: Vec<(String, u32)> = Vec::new();
            for index in 0..cx.len() {
                let Some(place) = places.get(index) else {
                    continue;
                };
                // How many screen pixels a size the view gave a pin takes: its own, on a map of
                // the world, and the camera's scale over it on a canvas.
                let screen = |size: f64| {
                    if world {
                        size.max(1.0)
                    } else {
                        (size * camera.zoom).max(1.0)
                    }
                };
                let width = screen(place.width);
                // A pin that hugs is measured at its natural height and stands no taller than
                // its own: an answer as tall as its words, inside the rectangle it was asked in.
                let fixed = place.height.filter(|_| !place.hug);
                let size = cx
                    .measure_child(
                        index,
                        Size {
                            width: MeasureConstraint::Exact(f32_of(width)),
                            height: fixed.map_or(MeasureConstraint::Intrinsic, |height| {
                                MeasureConstraint::Exact(f32_of(screen(height) + place.band))
                            }),
                        },
                    )?
                    .size;
                // A world pin stands centred on its place; a canvas pin from its top-leading
                // corner, where the view said it goes.
                let left = (place.x - camera.origin.x) * camera.zoom
                    - if world { width / 2.0 } else { 0.0 };
                // A band over the view stands above the pin's point, so the view's top is there.
                // A world pin of a height its view says stands its middle on its place.
                let centred = place
                    .height
                    .filter(|_| world && !place.hug)
                    .map_or(0.0, |height| screen(height) / 2.0);
                let top = (place.y - camera.origin.y) * camera.zoom - place.band - centred;
                let natural = f64::from(size.height);
                let height = place.height.map_or(natural, |height| {
                    let most = screen(height) + place.band;
                    // What hugs what it holds never grows past what the person sees: a long
                    // answer stands as tall as the viewport leaves it, and scrolls.
                    let most = if place.hug {
                        most.min(room).max(LEAST_SIDE.min(most))
                    } else {
                        most
                    };
                    // A view that says no height of its own (a list that scrolls is as tall as
                    // the room it is given) fills its most: it cannot hug what it holds.
                    let says = natural - place.band
                        >= LEAST_NATURAL * if world { 1.0 } else { camera.zoom };
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
                    // What a hugging pin takes on the canvas, in canvas pixels: the height it is
                    // drawn at, without the handle's band over it. The report below sends it once
                    // each time it changes, so a view reading the canvas's free area knows what
                    // the person sees the pin take (`canvas.md` §1, §7).
                    if place.hug && !silent.contains(key) {
                        let view = rect.height() - place.band;
                        let drawn = view / camera.zoom;
                        drawn_now.push((
                            key.clone(),
                            u32::try_from(drawn.round() as i64).unwrap_or(1),
                        ));
                    }
                    out.insert(key.clone(), rect);
                }
                cx.place(index, rect)?;
            }
            *laid.borrow_mut() = out;
            *unsaid.borrow_mut() = silent;
            // A hugging pin whose view came to a height of its own is laid out at it, and the map
            // reports that height once, as what the pin takes on the canvas: the view that reads
            // the canvas's free area knows what the person sees, not the most the place could
            // grow to (canvas.md §1, §7). Only a change is reported, never every solve.
            {
                let mut said = drawn_said.borrow_mut();
                // A pin the map no longer holds is forgotten, so it reports its height again when
                // it comes back to the canvas.
                said.retain(|key, _| drawn_now.iter().any(|(held, _)| held == key));
                for (key, height) in drawn_now {
                    if said.insert(key.clone(), height) == Some(height) {
                        continue;
                    }
                    put_drawn.fire(Some(crate::component::Value::List(vec![
                        crate::component::Value::Text(
                            items.get(&key).cloned().unwrap_or_else(|| key.clone()),
                        ),
                        crate::component::Value::List(vec![crate::component::Value::Int(
                            i64::from(height),
                        )]),
                    ])));
                }
            }
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
            world,
            basemap,
            pins,
            tracks,
            markers,
            drop,
            press,
            frame,
            context,
            resize,
            seen,
            drawn,
            outline,
            hint,
            latitude,
            longitude,
            zoom,
            rest,
            at,
            marker,
            enabled,
            description,
        } = props;
        let mut map = Self::new();
        map.hint = hint.filter(|hint| !hint.trim().is_empty());
        map.resize = resize;
        map.seen = seen;
        map.drawn = drawn;
        map.outline = outline_of(&outline);
        map.ground = ground;
        map.world = world;
        map.name = name.unwrap_or_default();
        map.key = format!("map\u{1f}{key}");
        map.drop = drop;
        map.press = press;
        map.frame = frame;
        map.context = context;
        map.at = at;
        map.rest = rest;
        map.marker_event = marker;
        map.enabled = enabled;
        map.description = description;
        map.basemap = basemap.filter(|template| template.contains("%z"));
        map.tracks = tracks
            .into_iter()
            .map(|track| {
                let points = track
                    .points
                    .chunks_exact(2)
                    .map(|pair| (pair[0], pair[1]))
                    .collect();
                (
                    track.key,
                    points,
                    track.tone.unwrap_or(MapTrackTone::Route),
                    track.width.unwrap_or(3.0).max(0.5),
                    track.dashed,
                )
            })
            .collect();
        map.markers = markers
            .into_iter()
            .map(|held| {
                (
                    held.key,
                    (held.latitude, held.longitude),
                    held.kind.unwrap_or_default(),
                    held.selected,
                    held.name.filter(|name| !name.is_empty()),
                    held.presence,
                )
            })
            .collect();
        map.focus = latitude.zip(longitude);
        // A map of the world opens showing the world; a canvas keeps the camera's own zoom.
        map.focus_zoom = if world {
            Some(zoom.unwrap_or(MAP_ZOOM_WORLD_MIN))
        } else {
            zoom
        };
        map.recentre = map.focus.is_some();
        for pin in pins {
            let (x, y) = match (pin.latitude, pin.longitude) {
                (Some(latitude), Some(longitude)) => {
                    let at = mercator_project(latitude, longitude);
                    (at.x, at.y)
                }
                _ => (pin.x.unwrap_or(0) as f64, pin.y.unwrap_or(0) as f64),
            };
            let place = PinPlace {
                x,
                y,
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
    /// place the layout closed pin_places on screen and in the accessibility tree.
    fn set_props(&mut self, props: MapProps) -> bool {
        let MapProps {
            name: _,
            ground,
            world,
            basemap,
            pins,
            tracks,
            markers,
            drop,
            press,
            frame,
            context,
            resize,
            seen,
            drawn,
            outline,
            hint,
            latitude,
            longitude,
            zoom,
            rest,
            at,
            marker,
            enabled,
            description,
        } = props;
        let hint = hint.filter(|hint| !hint.trim().is_empty());
        if hint != self.hint {
            self.hint = hint;
            self.dirty = true;
        }
        let basemap = basemap.filter(|template| template.contains("%z"));
        if basemap != self.basemap {
            self.basemap = basemap;
            self.dirty = true;
        }
        let tracks: Vec<_> = tracks
            .into_iter()
            .map(|track| {
                (
                    track.key,
                    track
                        .points
                        .chunks_exact(2)
                        .map(|pair| (pair[0], pair[1]))
                        .collect::<Vec<_>>(),
                    track.tone.unwrap_or(MapTrackTone::Route),
                    track.width.unwrap_or(3.0).max(0.5),
                    track.dashed,
                )
            })
            .collect();
        if tracks != self.tracks {
            self.tracks = tracks;
            self.dirty = true;
        }
        let markers: Vec<_> = markers
            .into_iter()
            .map(|held| {
                (
                    held.key,
                    (held.latitude, held.longitude),
                    held.kind.unwrap_or_default(),
                    held.selected,
                    held.name.filter(|name| !name.is_empty()),
                    held.presence,
                )
            })
            .collect();
        if markers != self.markers {
            self.markers = markers;
            self.dirty = true;
        }
        self.rest = rest;
        self.marker_event = marker;
        self.resize = resize;
        self.seen = seen;
        self.drawn = drawn;
        let outline = outline_of(&outline);
        if outline != self.outline {
            self.outline = outline;
            self.dirty = true;
        }
        self.drop = drop;
        self.press = press;
        self.enabled = enabled;
        self.description = description;
        self.frame = frame;
        self.context = context;
        self.at = at;
        let focus = latitude.zip(longitude);
        let zoom = if world {
            Some(zoom.unwrap_or(MAP_ZOOM_WORLD_MIN))
        } else {
            zoom
        };
        if focus != self.focus || zoom != self.focus_zoom {
            self.focus = focus;
            self.focus_zoom = zoom;
            self.recentre = focus.is_some();
        }
        if pins.len() != self.pins.len()
            || pins
                .iter()
                .zip(&self.pins)
                .any(|(given, (key, _, _))| given.key != *key)
        {
            return false;
        }
        self.ground = ground;
        self.world = world;
        self.items.clear();
        for (pin, (key, held, _)) in pins.into_iter().zip(self.pins.iter_mut()) {
            if let Some(item) = pin.item.filter(|item| !item.is_empty()) {
                self.items.insert(key.clone(), item);
            }
            let (x, y) = match (pin.latitude, pin.longitude) {
                (Some(latitude), Some(longitude)) => {
                    let at = mercator_project(latitude, longitude);
                    (at.x, at.y)
                }
                _ => (pin.x.unwrap_or(0) as f64, pin.y.unwrap_or(0) as f64),
            };
            *held = PinPlace {
                x,
                y,
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

    fn part_views(&mut self, part: &str) -> Option<&mut dyn PartViews> {
        self.editing = None;
        (part == "pins").then_some(self as &mut dyn PartViews)
    }

    /// The view one pin shows, to change in place: a change inside a place (its handle, its
    /// menu, what its resource's view says) patches that view where it stands, and the map —
    /// its camera, a drag under way, every other place — stays as it is.
    fn item_views(&mut self, part: &str, key: &str, of: &str) -> Option<&mut dyn PartViews> {
        if part != "pins" || of != "body" {
            return None;
        }
        self.editing = Some(self.pins.iter().position(|(held, _, _)| held == key)?);
        Some(self as &mut dyn PartViews)
    }
}

/// The pins of a map: one view each. Once the map is built the widgets live in its region, so a
/// change here reaches the region's children; before that, in the map's own store. Either way
/// the pins' keys and places stay in `self.pins`, which the next layout lays out afresh.
impl PartViews for Map {
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
        } else if let Some((_, _, part)) = self.pins.get_mut(at) {
            *part = view;
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
        if let Some(part) = self.region_mut().and_then(|region| region.child_mut(index)) {
            *part = view;
        } else if let Some((_, _, part)) = self.pins.get_mut(index) {
            *part = view;
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

/// The colour a dot of the ground's grain is painted in under `theme`: the theme's quietest veneer
/// (`overlay_subtle`) laid over the canvas ground's colour once, here, so the dot is one colour
/// that covers its pixels whole. No renderer blends it: the vello pipeline, the CPU rasterizer and
/// whatever presents the frame (a software GL surface included) show the same pixel, never a
/// veneer laid over a pixel that holds nothing (bright white where its alpha was taken back out).
fn grain_colour(theme: &Theme) -> vello::peniko::Color {
    let ground = crate::widget::primitives::SurfaceLevel::Ground.color(theme);
    let veneer = theme.tokens.colors.overlay_subtle;
    let [gr, gg, gb, ga] = ground.components;
    let [vr, vg, vb, va] = veneer.components;
    let alpha = va + ga * (1.0 - va);
    if alpha <= f32::EPSILON {
        return ground;
    }
    let over = |v: f32, g: f32| (v * va + g * ga * (1.0 - va)) / alpha;
    vello::peniko::Color::new([over(vr, gr), over(vg, gg), over(vb, gb), alpha])
}

impl Map {
    /// The ground's grain: a grid of dots in the theme's quietest veneer over the ground, so faint
    /// it only says "surface" (`canvas.md` §7). The dots are canvas points, so they pan and zoom
    /// with the ground and the person sees the ground move under their hand; on screen each is the
    /// same size at every zoom. Each dot covers whole device pixels in one opaque-as-the-ground
    /// colour ([`grain_colour`]) and stands wholly inside the viewport, so every renderer draws it
    /// identically and no dot reaches past the canvas's edge (under the bar, at its top). All of
    /// them are one path, filled once.
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
        // Where a logical point lands in device pixels: the output's scale and offset, as the
        // transform carries them (a canvas is never drawn rotated).
        let [scale, _, _, _, offset_x, offset_y] = transform.as_coeffs();
        let scale = scale.abs().max(f64::EPSILON);
        let device = |x: f64, y: f64| Point::new(x * scale + offset_x, y * scale + offset_y);
        let logical = |at: Point| Point::new((at.x - offset_x) / scale, (at.y - offset_y) / scale);
        // A dot is whole device pixels, at least one.
        let side = (GRAIN_DOT * scale).round().max(1.0);
        let inside = Rect::from_points(
            device(viewport.x0, viewport.y0),
            device(viewport.x1, viewport.y1),
        );
        let inside = Rect::new(
            inside.x0.ceil(),
            inside.y0.ceil(),
            inside.x1.floor(),
            inside.y1.floor(),
        );
        let mut dots = vello::kurbo::BezPath::new();
        for row in first_y..=last_y {
            for column in first_x..=last_x {
                let at = camera.to_screen(
                    corner,
                    Point::new(column as f64 * pitch, row as f64 * pitch),
                );
                let centre = device(at.x, at.y);
                let x0 = (centre.x - side / 2.0).round();
                let y0 = (centre.y - side / 2.0).round();
                let dot = Rect::new(x0, y0, x0 + side, y0 + side);
                // Wholly inside the viewport, or not at all: half a dot past the edge would stand
                // over what is not the canvas.
                if dot.x0 < inside.x0
                    || dot.y0 < inside.y0
                    || dot.x1 > inside.x1
                    || dot.y1 > inside.y1
                {
                    continue;
                }
                let dot = Rect::from_points(
                    logical(Point::new(dot.x0, dot.y0)),
                    logical(Point::new(dot.x1, dot.y1)),
                );
                vello::kurbo::Shape::path_elements(&dot, 0.1).for_each(|element| {
                    dots.push(element);
                });
            }
        }
        scene.fill(Fill::NonZero, transform, grain_colour(theme), None, &dots);
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

    /// The zoom of the basemap whose tiles put about a tile's own pixels on a screen pixel: the
    /// level whose imagery is sharpest at this camera.
    fn tile_zoom(&self) -> i64 {
        let zoom = self.shown.get().zoom.max(f64::EPSILON);
        let exact = (MERCATOR_WORLD * zoom / TILE_PIXELS).log2();
        exact.round().clamp(0.0, TILE_ZOOM_MOST as f64) as i64
    }

    /// The tile address for a level and column and row: the basemap's template with `%z`, `%x`
    /// and `%y` filled in.
    fn tile_address(basemap: &str, z: i64, x: i64, y: i64) -> String {
        basemap
            .replace("%z", &z.to_string())
            .replace("%x", &x.to_string())
            .replace("%y", &y.to_string())
    }

    /// How many canvas pixels a tile of `z` stands for: the world's width over its column count.
    fn tile_canvas(z: i64) -> f64 {
        MERCATOR_WORLD / (1_i64 << z.clamp(0, TILE_ZOOM_MOST)) as f64
    }

    /// Where a tile of `z` stands on screen, given the camera and the viewport's corner. A tile
    /// is drawn half a pixel past its edges on each side, so two tiles that meet draw no seam.
    fn tile_screen_rect(&self, camera: MapCamera, corner: Point, z: i64, x: i64, y: i64) -> Rect {
        let tile = Self::tile_canvas(z);
        let at = camera.to_screen(corner, Point::new(x as f64 * tile, y as f64 * tile));
        let side = tile * camera.zoom.max(f64::EPSILON);
        Rect::from_origin_size(at, (side, side)).inflate(0.5, 0.5)
    }

    /// The tiles of `z` the viewport shows, each its column and row, with the columns wrapped
    /// across the date line. A row past the projection's own edge is not a tile.
    fn visible_tiles(&self, camera: MapCamera, z: i64) -> Vec<(i64, i64)> {
        let tile = Self::tile_canvas(z);
        let corner = self.rect.origin();
        let from = camera.to_canvas(corner, corner);
        let to = camera.to_canvas(corner, Point::new(self.rect.x1, self.rect.y1));
        let columns = 1_i64 << z.clamp(0, TILE_ZOOM_MOST);
        let first_x = (from.x / tile).floor() as i64;
        let last_x = (to.x / tile).ceil() as i64;
        let first_y = (from.y / tile).floor().max(0.0) as i64;
        let last_y = (to.y / tile).ceil().min(columns as f64) as i64;
        let mut out = Vec::new();
        for y in first_y..last_y {
            for x in first_x..last_x {
                if x < first_x || x >= last_x {
                    continue;
                }
                out.push((x.rem_euclid(columns), y));
            }
        }
        out
    }

    /// The basemap under everything else: the tiles the viewport shows at the sharpest level
    /// that covers it, and, where one is still on its way, the coarser tile it stands in that
    /// has loaded, so imagery never blanks when the person zooms or pans.
    fn draw_basemap(
        &self,
        scene: &mut Scene,
        transform: Affine,
        pictures: &crate::pictures::Pictures,
    ) {
        let Some(basemap) = self.basemap.clone() else {
            return;
        };
        if self.rect.width() <= 0.0 || self.rect.height() <= 0.0 {
            return;
        }
        let camera = self.shown.get();
        let corner = self.rect.origin();
        let z = self.tile_zoom();
        let tiles = self.visible_tiles(camera, z);
        // The coarser tiles that stand under the ones on their way, drawn first so a loaded tile
        // paints over them: one drawing per coarser tile, however many holes it covers.
        let mut under: Vec<(i64, i64, i64)> = Vec::new();
        for (x, y) in &tiles {
            let source = crate::widget::PictureSource {
                src: Self::tile_address(&basemap, z, *x, *y),
            };
            if pictures.loaded(&source).is_some() || pictures.failed(&source) {
                continue;
            }
            for coarser in (0..z).rev() {
                if z - coarser > TILE_FALLBACK_LEVELS {
                    break;
                }
                let step = 1_i64 << (z - coarser);
                let (parent_x, parent_y) = (x.div_euclid(step), y.div_euclid(step));
                let parent = crate::widget::PictureSource {
                    src: Self::tile_address(&basemap, coarser, parent_x, parent_y),
                };
                if pictures.loaded(&parent).is_some() {
                    if !under.contains(&(coarser, parent_x, parent_y)) {
                        under.push((coarser, parent_x, parent_y));
                    }
                    break;
                }
            }
        }
        for (coarser, x, y) in under {
            let rect = self.tile_screen_rect(camera, corner, coarser, x, y);
            let source = crate::widget::PictureSource {
                src: Self::tile_address(&basemap, coarser, x, y),
            };
            if let Some(image) = pictures.loaded(&source) {
                draw_tile(scene, transform, &image, rect);
            }
        }
        for (x, y) in tiles {
            let source = crate::widget::PictureSource {
                src: Self::tile_address(&basemap, z, x, y),
            };
            // Asking for it admits a load the first time; until it lands, the coarser tile under
            // it stands in.
            if let Some(image) = pictures.get(&source) {
                let rect = self.tile_screen_rect(camera, corner, z, x, y);
                draw_tile(scene, transform, &image, rect);
            }
        }
    }

    /// The lines across the map, under its markers and pins: each through the places it names,
    /// in its tone's ink. A line is drawn in canvas pixels, so it keeps its width on the ground
    /// and grows on screen as the person zooms in.
    fn draw_tracks(&self, scene: &mut Scene, transform: Affine, theme: &Theme) {
        if self.tracks.is_empty() {
            return;
        }
        let camera = self.shown.get();
        let corner = self.rect.origin();
        for (_, points, tone, width, dashed) in &self.tracks {
            let mut path = vello::kurbo::BezPath::new();
            let mut first = true;
            for (latitude, longitude) in points {
                let at = mercator_project(*latitude, *longitude);
                let at = camera.to_screen(corner, at);
                if first {
                    path.move_to(at);
                    first = false;
                } else {
                    path.line_to(at);
                }
            }
            if first {
                continue;
            }
            let ink = track_ink(*tone, theme);
            let mut stroke = vello::kurbo::Stroke::new(*width);
            if *dashed {
                stroke = stroke.with_dashes(0.0, vec![width * 2.0, width * 1.6]);
            }
            scene.stroke(&stroke, transform, ink, None, &path);
        }
    }

    /// What stands on the map at its places: a dot at each, in the ink of what it is, ringed
    /// with the surface's colour so it reads on any imagery, and a size larger with a ring while
    /// it is the one the person chose. A dot keeps its size on screen whatever the zoom. Who of
    /// the workspace's people has a view at a place docks on its marker.
    fn draw_markers(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        env: &crate::draw::DrawEnv<'_>,
    ) {
        if self.markers.is_empty() {
            return;
        }
        let camera = self.shown.get();
        let corner = self.rect.origin();
        let ring = theme.tokens.colors.surfaces.content;
        for (_, (latitude, longitude), kind, selected, _, _) in &self.markers {
            let at = camera.to_screen(corner, mercator_project(*latitude, *longitude));
            if !self.rect.inflate(MARKER_RING, MARKER_RING).contains(at) {
                continue;
            }
            let (radius, halo) = if *selected {
                (MARKER_RADIUS + 2.0, MARKER_RING + 1.5)
            } else {
                (MARKER_RADIUS, MARKER_RING)
            };
            if *selected {
                scene.fill(
                    Fill::NonZero,
                    transform,
                    marker_ink(*kind, theme).with_alpha(0.35),
                    None,
                    &vello::kurbo::Circle::new(at, radius + MARKER_RING + 2.0),
                );
            }
            scene.fill(
                Fill::NonZero,
                transform,
                ring,
                None,
                &vello::kurbo::Circle::new(at, radius + halo),
            );
            scene.fill(
                Fill::NonZero,
                transform,
                marker_ink(*kind, theme),
                None,
                &vello::kurbo::Circle::new(at, radius),
            );
        }
        // Who is at a place docks on its marker, as a list row wears the member at its item: the
        // view gives the lens, and the window's presence names and paints them. A person's own
        // view shows no face of its own.
        let Some(presence) = env.presence else {
            return;
        };
        for (_, (latitude, longitude), _, _, _, person) in &self.markers {
            let Some(person) = person else { continue };
            let Some(member) = person.identity.member_id() else {
                continue;
            };
            if presence.is_own_identity(member) || env.self_member.as_ref() == Some(member) {
                continue;
            }
            let at = camera.to_screen(corner, mercator_project(*latitude, *longitude));
            if !self.rect.contains(at) {
                continue;
            }
            let dock = Point::new(
                at.x + MARKER_RADIUS + MARKER_RING + 6.0,
                at.y - MARKER_RADIUS,
            );
            presence.draw_placed_tag(
                &mut crate::paint::PaintCtx::new(scene, transform, text, theme),
                (dock, self.rect),
                &[(member.clone(), Some(person.name.clone()))],
                crate::widget::primitives::SurfaceLevel::Window.color(theme),
                false,
            );
        }
    }

    /// What a press at `at` (the window's logical coordinates, as the pointer reaches the map)
    /// opens: the name of the marker whose dot it lands on, or its key where it has none; the one
    /// drawn last among those covered, as the map draws them in order.
    fn marker_under(&self, at: Point) -> Option<String> {
        let camera = self.shown.get();
        let corner = self.rect.origin();
        self.markers
            .iter()
            .rev()
            .find(|(_, (latitude, longitude), _, _, _, _)| {
                camera
                    .to_screen(corner, mercator_project(*latitude, *longitude))
                    .distance(at)
                    <= MARKER_RADIUS + MARKER_RING
            })
            .map(|(key, _, _, _, name, _)| name.clone().unwrap_or_else(|| key.clone()))
    }

    /// Draw the region's pins, each posed by its own [`crate::motion::Placed`]: a pin that just
    /// arrived fades and scales in, one the person moved glides from where it stood, and one the
    /// map no longer holds leaves before it is forgotten. Every pin not drawn this frame is
    /// forgotten once its departure ends, so a map of many places only carries motion for the
    /// ones it shows.
    fn draw_pins(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
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
                child.draw_child(scene, transform, text, theme, &env);
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
                self.camera_moved(camera);
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
                self.zoom_camera(&mut camera, corner, about, 1.0 + *delta);
                self.camera_moved(camera);
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
                self.zoom_camera(&mut camera, corner, about, ZOOM_PER_DETENT.powf(notches));
                self.camera_moved(camera);
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
                self.camera_moved(camera);
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
                    self.camera_moved(camera);
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
                        crate::control_hints::performed::note(
                            crate::control_hints::performed::Performed::ResizedWindow,
                        );
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
                    // A click on a marker opens what the marker is; the map reports nothing of
                    // its own for that press, as a click on a pin is the pin's.
                    if let Some(key) = self.marker_under(press.start) {
                        self.marker_event
                            .fire(Some(crate::component::Value::Text(key)));
                        self.dirty = true;
                        *ctx.handled = true;
                        return;
                    }
                    if self.enabled {
                        self.press.fire(Some(crate::component::Value::List(vec![
                            crate::component::Value::Int(from.x.round() as i64),
                            crate::component::Value::Int(from.y.round() as i64),
                        ])));
                    }
                    if self.focus.is_some() {
                        let (latitude, longitude) = mercator_unproject(from);
                        self.at.fire(Some(crate::component::Value::List(vec![
                            crate::component::Value::Float(latitude),
                            crate::component::Value::Float(longitude),
                        ])));
                    }
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
                            self.camera_moved(camera);
                            *ctx.handled = true;
                        }
                        TouchGestureEvent::Pinch { center, scale, .. } => {
                            let corner = self.rect.origin();
                            let mut camera = self.camera;
                            self.zoom_camera(&mut camera, corner, center, scale);
                            self.camera_moved(camera);
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
            || self.rest_due.is_some()
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

    /// The map's name, which it keeps across a rebuild (its lens is kept under it too): the
    /// keyboard a press gave the canvas is found again on the map built in its place.
    fn test_id(&self) -> Option<&str> {
        (!self.name.is_empty()).then_some(self.name.as_str())
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
        self.centre_on_focus();
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

    fn draw(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, mut env) = ctx.parts();
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
        let pictures = crate::pictures::Pictures::window();
        self.draw_basemap(scene, transform, &pictures);
        self.draw_tracks(scene, transform, theme);
        self.draw_markers(scene, transform, text, theme, &env);
        self.draw_hint(scene, transform, text, theme);
        self.draw_frame_outline(scene, transform, theme);
        let windows = crate::control_hints::anchors::windows_drawn();
        self.draw_pins(&mut crate::draw::DrawCtx::new(
            scene,
            transform,
            text,
            theme,
            env.clone(),
        ));
        if clips {
            scene.pop_layer();
        }
        self.tell_hints(transform, windows);
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
        // Why it cannot be pressed now, if it says: read beside its name, never instead.
        let described = self
            .description
            .clone()
            .filter(|why| !why.trim().is_empty())
            .or_else(|| self.shown_hint().map(str::to_owned));
        Some(
            crate::a11y::AccessNode::new(crate::a11y::Role::Group)
                .with_optional_name((!self.name.is_empty()).then(|| self.name.clone()))
                .with_optional_description(described),
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
    /// windows in the pins' wells stand where the pins are drawn. A pin arriving or leaving is
    /// drawn in its pose, which lays nothing out.
    fn update(&mut self, state: &crate::widget::AppModel, path: &crate::widget::ModelPath) -> bool {
        let now = crate::motion::frame_now();
        let gliding = self
            .placed
            .borrow()
            .values()
            .any(|placed| placed.is_gliding(now));
        let moving = self.step_camera(now) | gliding;
        if moving || self.moved {
            self.dirty = true;
            self.invalidation.invalidate();
            crate::widget::note_layout_moved();
        }
        self.moved = moving;
        let mut changed = false;
        if let Some(due) = self.rest_due
            && now >= due
        {
            self.rest_due = None;
            self.report_rest();
            changed = true;
        }
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

#[cfg(test)]
#[path = "map_presence_tests.rs"]
mod presence_tests;

/// The width a map projects the world into, in canvas pixels at scale 1: a Web Mercator plane
/// `MERCATOR_WORLD` wide, the equator at half its height. A map's camera pans and zooms over it,
/// so a latitude and longitude reach the same screen point at every zoom. It is `2^20`: at the
/// camera's widest zoom a window shows a degree or so of the world, and at its closest a city.
pub const MERCATOR_WORLD: f64 = 1_048_576.0;

/// The latitude Web Mercator stops at: the projection runs to infinity at the poles, so every
/// map of it stops here (the square the tile servers use).
pub const MERCATOR_MAX_LAT: f64 = 85.051_128_779_806_6;

/// The canvas point `latitude`, `longitude` projects to on a Web Mercator map: the longitude
/// across the world, the latitude down it, in [`MERCATOR_WORLD`] canvas pixels. Latitudes past
/// [`MERCATOR_MAX_LAT`] clamp to it, so a pole stands on the map's top or bottom edge.
#[must_use]
pub fn mercator_project(latitude: f64, longitude: f64) -> Point {
    let lat = latitude
        .clamp(-MERCATOR_MAX_LAT, MERCATOR_MAX_LAT)
        .to_radians();
    let x = (longitude + 180.0) / 360.0 * MERCATOR_WORLD;
    let sine = lat.sin();
    let y = (0.5 - (((1.0 + sine) / (1.0 - sine)).ln() / (4.0 * std::f64::consts::PI)))
        * MERCATOR_WORLD;
    Point::new(x, y)
}

/// The latitude and longitude the canvas point `at` stands for on a Web Mercator map: the
/// inverse of [`mercator_project`], so a press on the map says the place it landed on.
#[must_use]
pub fn mercator_unproject(at: Point) -> (f64, f64) {
    let longitude = at.x / MERCATOR_WORLD * 360.0 - 180.0;
    let n = std::f64::consts::PI * (1.0 - 2.0 * at.y / MERCATOR_WORLD);
    let latitude = n.sinh().atan().to_degrees();
    (latitude, longitude)
}

#[cfg(test)]
mod mercator_tests {
    use super::{MERCATOR_MAX_LAT, MERCATOR_WORLD, Point, mercator_project, mercator_unproject};

    /// A place on the map and back is the place it was, at the equator, the tropics, the
    /// mid-latitudes and the projection's own edges, east and west.
    #[test]
    fn a_place_projects_and_unprojects_to_itself() {
        for latitude in [
            0.0,
            23.436,
            45.0,
            -45.0,
            60.5,
            -60.5,
            80.0,
            -80.0,
            MERCATOR_MAX_LAT,
            -MERCATOR_MAX_LAT,
        ] {
            for longitude in [-179.9, -90.0, 0.0, 13.405, 179.9] {
                let at = mercator_project(latitude, longitude);
                assert!(
                    at.x >= 0.0 && at.x <= MERCATOR_WORLD,
                    "x in the world: {at:?}"
                );
                assert!(
                    at.y >= 0.0 && at.y <= MERCATOR_WORLD,
                    "y in the world: {at:?}"
                );
                let (lat, lon) = mercator_unproject(at);
                assert!((lat - latitude).abs() < 1e-9, "lat {latitude} -> {lat}");
                assert!((lon - longitude).abs() < 1e-9, "lon {longitude} -> {lon}");
            }
        }
    }

    /// The world's corners: the equator crosses the middle, the projection's edges reach the
    /// world's edges, and a pole clamps to the edge rather than running off it.
    #[test]
    fn the_world_is_the_projection_square() {
        let middle = mercator_project(0.0, 0.0);
        assert!((middle.x - MERCATOR_WORLD / 2.0).abs() < 1e-9);
        assert!((middle.y - MERCATOR_WORLD / 2.0).abs() < 1e-9);
        let top = mercator_project(90.0, 0.0);
        let edge = mercator_project(MERCATOR_MAX_LAT, 0.0);
        assert!((top.y - edge.y).abs() < 1e-9, "a pole clamps to the edge");
        assert!(top.y.abs() < 1e-6);
    }

    /// A map centred on a place stands that place at its middle: the camera the view's centre
    /// and zoom make places it there, so a press at the middle unprojects to the centre.
    #[test]
    fn a_centred_map_stands_its_place_at_the_middle() {
        use super::MapCamera;
        use crate::kurbo::Rect;
        let rect = Rect::new(0.0, 0.0, 800.0, 600.0);
        let zoom = 2.0;
        let centre = mercator_project(52.52, 13.405);
        let origin = centre - (rect.size().to_vec2() * 0.5) / zoom;
        let camera = MapCamera { origin, zoom };
        let middle = Point::new(rect.center().x, rect.center().y);
        let (latitude, longitude) = mercator_unproject(camera.to_canvas(rect.origin(), middle));
        assert!((latitude - 52.52).abs() < 1e-9, "lat {latitude}");
        assert!((longitude - 13.405).abs() < 1e-9, "lon {longitude}");
    }
}
