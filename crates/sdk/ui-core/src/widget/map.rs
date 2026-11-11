//! `Map`: children laid out at points on a surface that pans and zooms, as a described
//! component.
//!
//! A canvas, a diagram, a map of devices: what the person arranged is *where things are*, in
//! whole canvas pixels, and the view is the camera that looks at it. `layout.md`'s "no absolute
//! positions" is about a view's own layout; a canvas is placed content (`shell.md` §2), so its
//! places are points and this widget is what draws them.
//!
//! The camera is the widget's own state, a lens: personal, kept on this device, never in the
//! person's arrangement. Panning and zooming move and scale the pins; a discrete change of the
//! pins or of the camera lays them out again. At rest the map asks for no frames.
//!
//! Each child is a `MapPin`: its key, its `x` and `y` in canvas pixels, its width, and the view
//! it shows. The map changes nothing itself: the caller commits one arranging action when the
//! person moves a place.

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
    /// A resource was dragged onto the map and let go: carries the resource the drag carried,
    /// as its address, and where it was let go — its canvas x, then its canvas y. The view
    /// decides what to do with it; the shell's canvas places it there.
    #[component(event(value = ResourceId, at = Vec<i64>))]
    pub drop: Event,
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
    /// and glides when the person moves it (`motion::Placed`). A pin that is not in `pins` any
    /// more leaves on its own before it is forgotten.
    placed: HashMap<String, crate::motion::Placed>,
    /// Where the region laid each pin out last, by key, written by its own solve: the rectangle a
    /// pin's pose is taken about.
    laid: Rc<std::cell::RefCell<HashMap<String, Rect>>>,
    /// The camera the region that places the pins reads, shared with its policy: the policy is
    /// built once and re-solved whenever the camera moves, so the map writes the camera here
    /// and the solve lays the pins out at it. `None` before the region is built.
    camera_cell: Option<Rc<std::cell::Cell<MapCamera>>>,

    /// Input or a new document value changed what this widget paints: it
    /// repaints once, and rests after the frame draws it.
    dirty: bool,
    /// The person put the keyboard in the canvas: its arrow keys pan it.
    focused: bool,
    /// What a resource dragged onto the map and let go does: the map reports it with where it
    /// was dropped, and the view places it.
    drop: Event,
    /// The key the map is known by on the drag bus, namespaced from the view that built it: a
    /// drag that started in one of its pins never lands back on the map it came from, and two
    /// maps never answer each other's drops.
    key: String,
    /// The pins this map has already brought into view: a place that arrived off screen comes
    /// into view once, and a place the person panned away from is left where their look left it.
    arrived: std::collections::HashSet<String>,
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
            focused: false,
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
            placed: HashMap::new(),
            laid: Rc::default(),
            camera_cell: None,
            drop: Event::None,
            key: String::new(),
            arrived: std::collections::HashSet::new(),
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
        if !self.camera.same(camera) {
            self.camera = camera;
            if let Some(cell) = &self.camera_cell {
                cell.set(camera);
            }
            self.invalidation.invalidate();
        }
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

    /// Bring `area` (canvas pixels) into the viewport by panning the camera the least it can,
    /// keeping its zoom: what a canvas does when a place arrives off screen, so what an agent
    /// or a person just placed stands where the person can see it without losing the view they
    /// had. An area already inside the viewport, or one with no pin on a map not yet laid out,
    /// moves nothing.
    pub fn reveal(&mut self, area: Rect, margin: f64) {
        let (width, height) = (self.rect.width(), self.rect.height());
        if width <= 0.0 || height <= 0.0 {
            return;
        }
        let area = area.inflate(margin, margin);
        let zoom = self.camera.zoom;
        // Where the area stands against the viewport, in canvas pixels: the least pan that
        // brings it inside.
        let left = self.camera.origin.x;
        let top = self.camera.origin.y;
        let right = left + width / zoom.max(f64::EPSILON);
        let bottom = top + height / zoom.max(f64::EPSILON);
        let dx = if area.x0 < left {
            area.x0 - left
        } else if area.x1 > right {
            area.x1 - right
        } else {
            0.0
        };
        let dy = if area.y0 < top {
            area.y0 - top
        } else if area.y1 > bottom {
            area.y1 - bottom
        } else {
            0.0
        };
        if dx == 0.0 && dy == 0.0 {
            return;
        }
        let mut camera = self.camera;
        camera.origin += Vec2::new(dx, dy);
        self.set_camera(camera);
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
        // The region's policy is built once and re-solved whenever the camera moves, so it reads
        // the camera from this shared cell rather than from a copy taken here: a copy would lay
        // every pin out at the camera the region was built with, and panning, zooming and the
        // arrow keys would change the lens the map draws from and nothing where its pins stand.
        let camera: Rc<std::cell::Cell<MapCamera>> = Rc::new(std::cell::Cell::new(self.camera));
        self.camera_cell = Some(Rc::clone(&camera));
        // Where the region lays each pin out, by its key: what `Map` poses the pin about this
        // frame, written by the region's own solve.
        let laid: Rc<std::cell::RefCell<HashMap<String, Rect>>> = Rc::clone(&self.laid);
        let mut region = Manual::new(move |cx| -> Result<Size<f32>, WidgetLayoutError> {
            let available = cx.available();
            let camera = camera.get();
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
                let rect = Rect::from_origin_size((left, top), (width, height));
                if let Some(key) = keys.get(index) {
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

    fn from_props(key: &str, props: MapProps) -> Self {
        let MapProps {
            name: _,
            ground,
            pins,
            drop,
        } = props;
        let mut map = Self::new();
        map.key = format!("map\u{1f}{key}");
        map.ground = ground;
        map.drop = drop;
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
            drop,
        } = props;
        self.drop = drop;
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
        // What the region laid out this frame, by key, and the keys the map holds now.
        let laid = self.laid.borrow().clone();
        let live: std::collections::HashSet<String> = self
            .pins
            .iter()
            .map(|(key, _, _)| key.clone())
            .filter(|key| laid.contains_key(key))
            .collect();
        let keys: Vec<String> = self
            .pins
            .iter()
            .map(|(key, _, _)| key.clone())
            .filter(|key| laid.contains_key(key))
            .collect();
        // Bring every pin's motion up to this frame.
        for key in &keys {
            let Some(rect) = laid.get(key).copied() else {
                continue;
            };
            let placed = self
                .placed
                .entry(key.clone())
                .or_insert_with(crate::motion::Placed::closed);
            placed.follow(rect, true, motion, source, now);
        }
        // A pin that left the map leaves from where it last stood, then is forgotten.
        let leaving: Vec<String> = self
            .placed
            .iter()
            .filter(|(key, placed)| !live.contains(*key) && placed.is_visible(now))
            .map(|(key, _)| key.clone())
            .collect();
        for key in leaving {
            let Some(rect) = self.placed.get(&key).map(|placed| placed.rect(now)) else {
                continue;
            };
            let placed = self
                .placed
                .entry(key.clone())
                .or_insert_with(crate::motion::Placed::closed);
            placed.follow(rect, false, motion, source, now);
        }
        self.placed
            .retain(|key, placed| live.contains(key) || placed.is_visible(now));
        // Draw each pin the region holds, under its own pose.
        let drawn: Vec<(String, Rect)> = keys
            .iter()
            .filter_map(|key| {
                let placed = self.placed.get(key)?;
                let rect = laid.get(key).copied()?;
                placed.is_visible(now).then_some((key.clone(), rect))
            })
            .collect();
        for (key, rect) in drawn {
            let Some(placed) = self.placed.get(&key) else {
                continue;
            };
            let pose = placed.pose(rect, now);
            let bounds = rect.union(pose.drawn_rect(rect));
            let Some(index) = self.pins.iter().position(|(held, _, _)| *held == key) else {
                continue;
            };
            let Some(child) = self.region_mut().and_then(|region| region.child_mut(index)) else {
                continue;
            };
            pose.paint(scene, transform, bounds, |scene, transform| {
                child.draw(scene, transform, text, theme, presence);
            });
        }
    }

    /// Whether any pin's motion is still moving at `now`: what asks for frames while it does.
    fn pins_moving(&self, now: crate::time::Instant) -> bool {
        self.placed.values().any(|placed| placed.is_moving(now))
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
                // A press on the canvas puts the keyboard in it, so the arrow keys pan it from
                // there without the person looking for a field to reach them from.
                *ctx.focus_target = Some(ctx.node_id);
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
        self.pins_moving(crate::motion::frame_now())
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
        // A place that arrived off screen comes into view: the canvas pans to it once the map
        // built around it is laid out (`shell.md` §3, "a place next to where it came from"; the
        // session's `open` brings the canvas to what it stood). Only the last pin — the place
        // just put on the canvas — and only when it stands wholly outside the viewport, so a
        // place already in sight moves nothing.
        let arriving = self
            .pins
            .last()
            .map(|(key, place, _)| (key.clone(), *place));
        if let Some((key, place)) = arriving
            && !self.arrived.contains(&key)
            && self.rect.width() > 0.0
            && self.rect.height() > 0.0
        {
            self.arrived.insert(key);
            let area = Rect::from_origin_size(
                Point::new(place.x, place.y),
                (place.width.max(1.0), place.height.unwrap_or(0.0)),
            );
            self.reveal(
                area,
                f64::from(crate::tokens::RhythmScale::BASE.get(crate::tokens::Rhythm::Group)),
            );
        }
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

    fn update(&mut self, state: &crate::widget::AppModel, path: &crate::widget::ModelPath) -> bool {
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
