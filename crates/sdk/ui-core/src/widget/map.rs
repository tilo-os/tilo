//! `Map`: children laid out at points on a surface that pans and zooms, as a described
//! component.
//!
//! A canvas, a diagram, a map of devices: what the person arranged is *where things are*, in
//! whole canvas pixels, and the view is the camera that looks at it. `layout.md`'s "no absolute
//! positions" is about a view's own layout; a canvas is placed content (`shell.md` §2), so its
//! places are points and this widget is what draws them.
//!
//! The camera is the widget's own state, a lens: personal, kept on this device, never in the
//! person's arrangement. Panning and zooming move and scale the pins; a change of the pins or of
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

use crate::component::{Component, Described, SlotType, SlotViews, Source};
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
/// window's frame at the very edge of the canvas reads as cut off, so the pan leaves a gap.
const ARRIVAL_MARGIN: f64 = 16.0;

/// A pin's place, in canvas pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PinPlace {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    /// How tall it stands, when the arrangement says (a window on a desktop); `None` lets its
    /// view say, as a note on a canvas is as tall as what it holds.
    pub height: Option<f64>,
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
            cursor: Point::ORIGIN,
            touch: TouchGestureArena::new(),
            ground: false,
            rect: Rect::ZERO,
            slop: f64::from(crate::tokens::Metrics::BASE.click_slop),
            key_pan: f64::from(crate::tokens::Metrics::BASE.wheel_line),
            placed: Rc::default(),
            laid: Rc::default(),
            reveal: None,
            glide: None,
            shown: Rc::default(),
            followed: Rc::default(),
            motion: crate::tokens::EffectiveMotion::default(),
            moved: false,
            name: String::new(),
            took: false,
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

    /// Look with `camera`: where the person left the canvas on this device.
    pub fn set_camera(&mut self, camera: MapCamera) {
        self.dirty = true;
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

    /// Show the place that arrived, if any: what [`Self::take_over`] left to be shown. The
    /// camera sets off there before the places are laid out, and the places, with the windows in
    /// their wells, stand where it has carried them on every frame of the way.
    fn show_asked(&mut self) {
        let Some(key) = self.reveal.clone() else {
            return;
        };
        self.reveal = None;
        // The region places the places about its own origin, so the viewport is read there: the
        // map's own room, where its content starts.
        let view = Rect::from_origin_size(Point::ORIGIN, self.rect.size());
        if view.width() <= 0.0 || view.height() <= 0.0 {
            return;
        }
        let Some((_, place, _)) = self.pins.iter().find(|(held, _, _)| *held == key) else {
            return;
        };
        let place = *place;
        let camera = self.camera;
        let (x0, y0) = (
            (place.x - camera.origin.x) * camera.zoom,
            (place.y - camera.origin.y) * camera.zoom,
        );
        let width = place.width * camera.zoom;
        // A place whose height its view says is as tall as a window is wide over tall.
        let height = place
            .height
            .map_or(width / 1.6, |height| height * camera.zoom);
        // What it takes for the place to stand whole inside the viewport, with the least move: a
        // place that pokes out an edge is brought in with a gap, and one already in view stays
        // where it is. A place wider than the viewport keeps its leading edge in view.
        let margin = ARRIVAL_MARGIN
            .min(view.width() / 2.0)
            .min(view.height() / 2.0);
        let along = |start: f64, end: f64, from: f64, to: f64| {
            if start < from {
                start - from - margin
            } else if end > to {
                end - to + margin
            } else {
                0.0
            }
        };
        let (dx, dy) = (
            along(x0, x0 + width, view.x0, view.x1),
            along(y0, y0 + height, view.y0, view.y1),
        );
        if dx.abs() < 0.5 && dy.abs() < 0.5 {
            return;
        }
        // Moving the place left by `dx` on screen is the camera looking `dx` further along its
        // canvas: the places keep their points, and the lens moves over them.
        let mut camera = self.camera;
        camera.origin += Vec2::new(dx, dy) / camera.zoom.max(f64::EPSILON);
        self.pan_to(camera);
    }

    /// Take the camera to `camera` the way a scroll takes its content to what it shows: a short
    /// move glides, whoever opened what it shows and however long it took to arrive, so the person
    /// sees where it stands among the rest; a still theme or reduced motion lands. Nothing in the
    /// arrangement moves.
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
        let places: Vec<PinPlace> = self.pins.iter().map(|(_, place, _)| *place).collect();
        let keys: Vec<String> = self.pins.iter().map(|(key, _, _)| key.clone()).collect();
        // The camera is read at each solve, not when the region is built: a pan, a zoom or a
        // glide of the camera lays the places out again without building the region again.
        let shown = Rc::clone(&self.shown);
        let followed = Rc::clone(&self.followed);
        let moving = Rc::clone(&self.placed);
        let motion = self.motion;
        let laid: Rc<RefCell<HashMap<String, Rect>>> = Rc::clone(&self.laid);
        let mut region = Manual::new(move |cx| -> Result<Size<f32>, WidgetLayoutError> {
            let available = cx.available();
            let camera = shown.get();
            // A camera that moved since the last solve carries the places: the ground moves under
            // them, and they stand where it took them, with the windows in their wells.
            let carried = followed
                .replace(Some(camera))
                .is_some_and(|was| !was.same(camera));
            let now = crate::motion::frame_now();
            let source = crate::motion::input_source();
            let mut moving = moving.borrow_mut();
            let mut out: HashMap<String, Rect> = HashMap::with_capacity(places.len());
            for index in 0..cx.len() {
                let Some(place) = places.get(index) else {
                    continue;
                };
                let width = (place.width * camera.zoom).max(1.0);
                let size = cx
                    .measure_child(
                        index,
                        Size {
                            width: MeasureConstraint::Exact(f32_of(width)),
                            height: place.height.map_or(MeasureConstraint::Intrinsic, |height| {
                                MeasureConstraint::Exact(f32_of((height * camera.zoom).max(1.0)))
                            }),
                        },
                    )?
                    .size;
                let left = (place.x - camera.origin.x) * camera.zoom;
                let top = (place.y - camera.origin.y) * camera.zoom;
                let height = place.height.map_or(f64::from(size.height), |height| {
                    (height * camera.zoom).max(1.0)
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

    fn from_props(_key: &str, props: MapProps) -> Self {
        let MapProps { name, ground, pins } = props;
        let mut map = Self::new();
        map.ground = ground;
        map.name = name.unwrap_or_default();
        for pin in pins {
            let place = PinPlace {
                x: pin.x.unwrap_or(0) as f64,
                y: pin.y.unwrap_or(0) as f64,
                width: (pin.width.unwrap_or(360) as f64).max(1.0),
                height: pin.height.map(|height| (height as f64).max(1.0)),
            };
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
    /// place the arrangement closed standing on screen and in the accessibility tree.
    fn set_props(&mut self, props: MapProps) -> bool {
        let MapProps {
            name: _,
            ground,
            pins,
        } = props;
        if pins.len() != self.pins.len()
            || pins
                .iter()
                .zip(&self.pins)
                .any(|(given, (key, _, _))| given.key != *key)
        {
            return false;
        }
        self.ground = ground;
        for (pin, (_, held, _)) in pins.into_iter().zip(self.pins.iter_mut()) {
            *held = PinPlace {
                x: pin.x.unwrap_or(0) as f64,
                y: pin.y.unwrap_or(0) as f64,
                width: (pin.width.unwrap_or(360) as f64).max(1.0),
                height: pin.height.map(|height| (height as f64).max(1.0)),
            };
        }
        self.dirty = true;
        self.invalidation.invalidate();
        true
    }

    fn slot_views(&mut self, slot: &str) -> Option<&mut dyn SlotViews> {
        (slot == "pins").then_some(self as &mut dyn SlotViews)
    }
}

/// The pins of a map: one view each. Once the map is built the widgets live in its region, so a
/// change here reaches the region's children; before that, in the map's own store. Either way
/// the pins' keys and places stay in `self.pins`, which the next layout lays out afresh.
impl SlotViews for Map {
    fn len(&self) -> usize {
        self.pins.len()
    }

    fn view_mut(&mut self, index: usize) -> Option<&mut Element> {
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
                },
                Box::new(crate::widget::Spacer::token(crate::tokens::SpacingStep::S0)),
            ),
        );
        if let Some(region) = self.region_mut() {
            region.insert_child(at, view);
        } else if let Some((_, _, slot)) = self.pins.get_mut(at) {
            *slot = view;
        }
    }

    fn remove(&mut self, index: usize) {
        if index < self.pins.len() {
            self.pins.remove(index);
            if let Some(region) = self.region_mut() {
                let _ = region.remove_child(index);
            }
        }
    }

    fn move_view(&mut self, from: usize, to: usize) {
        if from < self.pins.len() && to < self.pins.len() {
            let pin = self.pins.remove(from);
            self.pins.insert(to, pin);
            if let Some(region) = self.region_mut() {
                region.move_child(from, to);
            }
        }
    }

    fn replace(&mut self, index: usize, view: Element) {
        if let Some(slot) = self.region_mut().and_then(|region| region.child_mut(index)) {
            *slot = view;
        } else if let Some((_, _, slot)) = self.pins.get_mut(index) {
            *slot = view;
        }
    }
}

impl Map {
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
            // Each pin the region holds is drawn where it is laid out, arriving as it does.
            keys.iter()
                .filter_map(|key| {
                    let placed = placed_pins.get(key)?;
                    let rect = laid.get(key).copied()?;
                    let index = self.pins.iter().position(|(held, _, _)| held == key)?;
                    placed.is_visible(now).then(|| {
                        let pose = crate::motion::WindowMotion::content_pose(
                            placed.progress(now),
                            rect,
                            motion,
                        );
                        (index, rect, pose)
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
    /// the arrow keys pan, a finger pans and two fingers pinch. It moves the lens and nothing in
    /// the arrangement; the views' own presses fall through to the pins.
    fn camera_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        match event {
            WindowEvent::MouseWheel { delta, .. } => {
                if !ctx.modifiers.state().control_key() {
                    return;
                }
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
                *ctx.handled = true;
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
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
                if event.state == ElementState::Pressed
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
                        TouchGestureEvent::Pan { delta, .. } => {
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
    }
}

/// `v` as `f32`, for the layout solver.
#[expect(
    clippy::cast_possible_truncation,
    reason = "logical pixels: a canvas coordinate of a view, far inside f32"
)]
fn f32_of(v: f64) -> f32 {
    v as f32
}

#[cfg(test)]
#[path = "map_motion_tests.rs"]
mod motion_tests;
