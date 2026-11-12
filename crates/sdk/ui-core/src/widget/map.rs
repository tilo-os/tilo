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
use crate::widget::{Element, Manual, ManualInvalidation, TaffyTreeExt, Widget, WidgetLayoutError};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

/// The smallest and largest zoom the map is shown at: far enough out to see a whole canvas, far
/// enough in to read a note.
pub const MAP_ZOOM_MIN: f64 = 0.2;
pub const MAP_ZOOM_MAX: f64 = 2.5;

thread_local! {
    /// The cameras the maps of this thread last looked with, by the key their views gave them:
    /// the lens is the person's and it outlives the widget. A map is built again when the places
    /// it holds change — a place opened, closed or moved — and a rebuilt map that started from
    /// the default lens would throw away where the person was looking, and with it the view of
    /// what they were in. Nothing reaches a workspace or another device: the lens stays here.
    static CAMERAS: std::cell::RefCell<HashMap<String, MapCamera>> = std::cell::RefCell::new(HashMap::new());
}

/// The camera the map `key` last looked with on this thread, when it has one.
fn recalled(key: &str) -> Option<MapCamera> {
    CAMERAS.with(|cameras| cameras.borrow().get(key).copied())
}

/// Keep `camera` as the map `key`'s lens on this thread.
fn remember(key: &str, camera: MapCamera) {
    if key.is_empty() {
        return;
    }
    CAMERAS.with(|cameras| {
        cameras.borrow_mut().insert(key.to_owned(), camera);
    });
}

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

/// One pin's view, hosted by the map: laid out once at its place's own size, at no zoom, and
/// drawn, hit and reported through the camera's transform. The camera moves it and never lays
/// it out again; only its place's size or its view's own content lays it out again.
///
/// The host is the region's child, and the region places it where the camera draws the pin's
/// box: the host answers that box, transforms its view's own space onto it, and passes input,
/// cursor shape and accessibility through the inverse ([`crate::widget::Embedded`] does the
/// mapping). It paints nothing itself: the map draws it under the pin's motion pose.
struct PinHost {
    host: crate::widget::Embedded<dyn Widget>,
    /// The pin's key in the map: how the map knows the host a rebuild carries over.
    key: String,
    /// The pin's place in canvas pixels: what its view is laid out at, unzoomed.
    place: PinPlace,
    /// What the pin's view was last laid out at, in its own canvas pixels: a size the place
    /// changed to lays it out again, a camera move does not.
    laid_at: Option<(f64, f64)>,
    /// The theme its view was laid out under.
    theme: Option<Theme>,
    /// How tall the pin stands in its own space, unzoomed: the place's height when it names
    /// one, else the height its view came to.
    height: f64,
    /// Where the pin is drawn on screen, through the camera: the pose the map's own motion
    /// moves from.
    screen: Rect,
    /// From the pin's own space to the window's: the camera's zoom about the place's corner.
    transform: Affine,
    /// The heights the region places the pins' boxes from, and this pin's index in it: a view
    /// that came to another height than the region placed it at asks for another solve.
    heights: Rc<std::cell::RefCell<Vec<f64>>>,
    index: usize,
    /// A pin's view changed the height it stands: the region places its box again.
    resize: Rc<std::cell::Cell<bool>>,
    /// How often the pin's view was laid out: a test counts layouts to prove a camera move lays
    /// nothing out again.
    layouts: usize,
}

impl PinHost {
    fn new(
        key: String,
        view: Element,
        place: PinPlace,
        heights: Rc<std::cell::RefCell<Vec<f64>>>,
        index: usize,
        resize: Rc<std::cell::Cell<bool>>,
    ) -> Self {
        Self {
            // A place that names how tall it stands fills it; one that does not is as tall as
            // its view asks.
            host: crate::widget::Embedded::boxed(view).filling(),
            key,
            place,
            laid_at: None,
            height: place.height.unwrap_or(0.0),
            screen: Rect::ZERO,
            transform: Affine::IDENTITY,
            theme: None,
            heights,
            index,
            resize,
            layouts: 0,
        }
    }

    /// The pin's view, laid out again when its place's size changed or the view's own content
    /// did. Never because the camera moved. What it came to is published for the region's next
    /// solve, which places the pin's box at the height the view stands.
    fn layout_view(&mut self, theme: &Theme, text: &mut TextRenderer) {
        let width = self.place.width.max(1.0);
        let height = self.place.height.unwrap_or(0.0);
        let size = (width, height);
        // A place that names how tall it stands fills it; one that does not is as tall as its
        // view asks.
        self.host.set_filling(self.place.height.is_some());
        let theme_changed = self
            .theme
            .as_ref()
            .is_none_or(|laid| laid.text_scale != theme.text_scale || laid.tokens != theme.tokens);
        if self.laid_at != Some(size) || theme_changed || self.host.widget().layout_needs_measure()
        {
            let rect = Rect::from_origin_size(Point::ORIGIN, size);
            let _ = self.host.layout(rect, theme, text);
            self.laid_at = Some(size);
            self.theme = Some(theme.clone());
            self.layouts = self.layouts.saturating_add(1);
        }
        let stood = self.height;
        self.height = self.place.height.unwrap_or_else(|| self.host.laid_height());
        if let Some(slot) = self.heights.borrow_mut().get_mut(self.index) {
            *slot = self.height;
        }
        if (self.height - stood).abs() > f64::EPSILON {
            self.resize.set(true);
        }
    }

    /// The view this pin shows, to swap it for another (a place opened a view of its own).
    fn view_mut(&mut self) -> &mut Element {
        self.host.element_mut()
    }

    /// How often the pin's view was laid out (a test counts camera moves that lay nothing out).
    fn layouts(&self) -> usize {
        self.layouts
    }
}

impl Widget for PinHost {
    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        _theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // The region places it; nothing under it is in this tree (its view has its own). The
        // view is laid out by `Map::lay_out_views` at its own place size, not here.
        tree.widget_leaf(Style::default(), map, self)
    }

    fn apply_layout(&mut self, tree: &TaffyTree<()>, node: NodeId, origin: Point) {
        let rect = crate::widget::layout_rect(tree, node, origin);
        // The region placed the pin's box at the camera's zoom: the view's own space maps onto
        // it, so a pin reads at any zoom and hit-tests where it is drawn.
        let zoom = if self.place.width > 0.0 {
            rect.width() / self.place.width
        } else {
            1.0
        };
        self.transform = Affine::translate(rect.origin().to_vec2()) * Affine::scale(zoom);
        self.screen = Rect::from_origin_size(rect.origin(), (rect.width(), self.height * zoom));
        self.host.set_transform(self.transform);
    }

    fn name(&self) -> &str {
        "MapPin"
    }

    /// Paint the pin's view where the camera placed it: the map draws it under its own motion
    /// pose, through the transform the region gave the host.
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        self.host.draw(scene, transform, text, theme, presence);
    }

    fn get_rect(&self) -> Rect {
        self.screen
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn is_layout_dirty(&self) -> bool {
        self.host.widget().layout_needs_measure()
    }

    fn layout_needs_measure(&self) -> bool {
        // The region places the pin's box at the height the view came to: a view that needs a
        // measure, or that came to another height than it placed the box at, asks the region to
        // solve again.
        self.resize.get() || self.host.widget().layout_needs_measure()
    }

    fn needs_redraw(&self) -> bool {
        self.host.widget().needs_redraw() || self.host.widget().needs_animation_frame()
    }

    fn mark_drawn(&mut self) {
        // The map drew the view under its pose this frame: what it painted is drawn.
        self.host.widget_mut().mark_drawn();
        // The region placed the box at the height the view came to: its solve took it.
        self.resize.set(false);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        self.host.handle_event(
            event,
            ctx.scale_factor,
            ctx.modifiers,
            &mut ctx.state,
            ctx.path,
            ctx.handled,
        );
        // A press inside the pin focused something in its view: the map takes the keyboard, and
        // keeps routing keys and the input method to the pin whose view was focused.
        if self.host.take_focus_request() {
            *ctx.focus_target = Some(ctx.node_id);
        }
    }

    /// The pointer's shape over the pin's view, where the camera drew it: the view's own shape
    /// through the inverse.
    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        self.host.cursor_shape(point)
    }

    /// The pin in the accessibility tree: its view's own node, its bounds placed through the
    /// camera ([`crate::widget::Embedded::accessibility`] does the mapping), so an assistive
    /// technology reads and operates the pin where the person sees it. A view with no node of
    /// its own is presentational: the walk collapses the pin into the pin's own controls.
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        self.host.accessibility()
    }

    /// The pin's view is the host's own child in the tree walks (search, inspection,
    /// accessibility), but the host lays it out itself: it has no node of its own in the
    /// window's layout, and the walk reaches it through the host ([`Widget::lays_out_child`]).
    fn children(&self) -> Vec<&dyn Widget> {
        vec![self.host.widget()]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![self.host.widget_mut()]
    }

    /// The host's accessibility node *is* its view's, placed through the camera: the walk does
    /// not visit the view again as a child (it would export it twice).
    fn accessibility_represents_child(&self, _child_index: usize) -> bool {
        true
    }

    fn lays_out_child(&self, _child_index: usize) -> bool {
        true
    }

    fn perform_action(
        &mut self,
        action: &crate::a11y::Action,
        key_path: &[u64],
        state: &mut crate::widget::Edits<'_>,
        path: &crate::widget::ModelPath,
    ) -> bool {
        self.host.perform_action(action, key_path, state, path)
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        state: &mut crate::widget::Edits<'_>,
        path: &crate::widget::ModelPath,
    ) -> bool {
        self.host
            .perform_semantic_action(target, action, state, path)
    }

    fn wants_pointer_capture(&self) -> bool {
        self.host.widget().wants_pointer_capture()
    }

    fn modal_capture(&self) -> Option<Rect> {
        self.host.modal_capture()
    }

    fn take_structure_dirty(&mut self) -> bool {
        self.host.widget_mut().take_structure_dirty()
    }

    fn take_structure_change(&mut self) -> crate::widget::StructureChange {
        self.host.widget_mut().take_structure_change()
    }

    fn needs_animation_frame(&self) -> bool {
        self.host.needs_animation_frame()
    }

    fn next_wake_deadline(&self) -> Option<crate::time::Instant> {
        self.host.next_wake_deadline()
    }

    fn sync(&mut self, state: &crate::widget::AppModel, path: &crate::widget::ModelPath) {
        self.host.sync_boxed(state, path);
    }

    fn update(&mut self, state: &crate::widget::AppModel, path: &crate::widget::ModelPath) -> bool {
        self.host.widget_mut().update(state, path)
    }

    fn settle(&mut self, edits: &mut crate::widget::Edits<'_>, path: &automerge::ObjId) {
        self.host.settle(edits, path);
    }
}

/// See the module docs.
pub struct Map {
    /// The pins, in draw order: their key, place and view. Each pin's view moves into its host
    /// (in `parts`' region) once the map is built; this holds the pins' keys and places, and a
    /// view not yet given to a host.
    pins: Vec<(String, PinPlace, Element)>,
    /// The map's one retained child: the explicit-placement region that holds each pin's host
    /// and places its box at its canvas place through the camera. Kept here so the widgets it
    /// holds outlive the tree's pointers to them.
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
    /// How tall each pin stands in its own space, unzoomed, in `pins` order: the place's height
    /// when it names one, else what its view came to. The region places each pin's box from
    /// these, so a place that names no height still places its box where its view reaches.
    heights: Rc<std::cell::RefCell<Vec<f64>>>,
    /// A pin's view came to another height than the region placed it at: the region solves
    /// again before the frame draws.
    resize: Rc<std::cell::Cell<bool>>,
    /// The camera the region that places the pins reads, shared with its policy: the policy is
    /// built once and re-solved whenever the camera moves, so the map writes the camera here
    /// and the solve lays the pins out at it. `None` before the region is built.
    camera_cell: Option<Rc<std::cell::Cell<MapCamera>>>,
    /// The theme the pins' views are laid out under, taken from the last build.
    theme: Option<Theme>,
    /// What the map is, for assistive technology.
    name: Option<String>,
    /// The pins the map holds, or the views they show, changed since the region was built: it
    /// is built again around them.
    structure: bool,

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
            heights: Rc::default(),
            resize: Rc::default(),
            camera_cell: None,
            theme: None,
            name: None,
            structure: false,
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
            remember(self.drop_key(), camera);
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

    /// [`Self::reveal`] holding `keep` (canvas pixels) as well: what a canvas does when a place
    /// arrives beside another, so what the person was looking at stays where they were looking
    /// rather than being pushed out of the viewport by the pan to what arrived (`shell.md` §3,
    /// "a place next to where it came from"). `keep` is every place standing wholly in the
    /// viewport: they and what arrived all end up in view — panning the least that does, and
    /// zooming out only as far as they need, since none of them may be lost. With nothing to keep
    /// this is [`Self::reveal`].
    pub fn reveal_with(&mut self, area: Rect, margin: f64, keep: &[Rect]) {
        let Some(together) = keep
            .iter()
            .fold(None, |held: Option<Rect>, rect| {
                let rect = rect.inflate(margin, margin);
                Some(held.map_or(rect, |held| held.union(rect)))
            })
            .map(|kept| kept.union(area.inflate(margin, margin)))
        else {
            self.reveal(area, margin);
            return;
        };
        let (width, height) = (self.rect.width(), self.rect.height());
        if width <= 0.0 || height <= 0.0 {
            return;
        }
        let origin = self.camera.origin;
        let zoom = self.camera.zoom;
        // All of them already stand in sight: the person's view stays their own.
        let fits = |zoom: f64| {
            let span = Vec2::new(width, height) / zoom.max(f64::EPSILON);
            span.x >= together.width() && span.y >= together.height()
        };
        // Zoom out only when they cannot all stand in the viewport at the person's zoom, and no
        // further than they need: the canvas keeps the zoom it can.
        let zoom = if fits(zoom) {
            zoom
        } else {
            (width / together.width().max(1.0))
                .min(height / together.height().max(1.0))
                .clamp(MAP_ZOOM_MIN, MAP_ZOOM_MAX)
        };
        let span = Vec2::new(width, height) / zoom.max(f64::EPSILON);
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
        self.set_camera(MapCamera {
            origin: Point::new(x, y),
            zoom,
        });
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

    /// Build the region that places the pins: each pin's view is laid out at its own place
    /// size (canvas pixels, unzoomed) by its host, and the region only places each host's box
    /// where the camera draws the pin. Panning and zooming change where the boxes stand and lay
    /// nothing out again; a view laid out again is one whose place's size or view changed.
    fn rebuild_region(&mut self, theme: &Theme) {
        self.theme = Some(theme.clone());
        // The pins' keys and places, in draw order: the region places one box per pin.
        let keys: Vec<String> = self.pins.iter().map(|(key, _, _)| key.clone()).collect();
        let places: Vec<PinPlace> = self.pins.iter().map(|(_, place, _)| *place).collect();
        // The hosts the last region held, taken back out: a pin the map still holds keeps its
        // host, so its view, what the person is doing with it and its layout all stay; the
        // hosts of pins the map no longer holds go with them.
        let mut built: HashMap<String, Element> = HashMap::new();
        if let Some(region) = self.region_mut() {
            for child in region.take_children() {
                let Some(key) = child
                    .as_any()
                    .and_then(|any| any.downcast_ref::<PinHost>())
                    .map(|host| host.key.clone())
                else {
                    continue;
                };
                built.insert(key, child);
            }
        }
        *self.heights.borrow_mut() = places
            .iter()
            .map(|place| place.height.unwrap_or(0.0))
            .collect();
        let mut hosts: Vec<Element> = Vec::with_capacity(keys.len());
        for (index, (key, place, view)) in self.pins.iter_mut().enumerate() {
            if let Some(mut host) = built.remove(key)
                && let Some(pin) = host
                    .as_any_mut()
                    .and_then(|any| any.downcast_mut::<PinHost>())
            {
                // The place it stood at may have changed in the props taken in place.
                pin.place = *place;
                pin.index = index;
                hosts.push(host);
                continue;
            }
            let taken = std::mem::replace(
                view,
                Box::new(crate::widget::Spacer::token(crate::tokens::SpacingStep::S0)),
            );
            hosts.push(Box::new(PinHost::new(
                key.clone(),
                taken,
                *place,
                Rc::clone(&self.heights),
                index,
                Rc::clone(&self.resize),
            )));
        }
        // The region's policy is built once and re-solved whenever the camera moves, so it reads
        // the camera from this shared cell rather than from a copy taken here: a copy would lay
        // every pin out at the camera the region was built with, and panning, zooming and the
        // arrow keys would change the lens the map draws from and nothing where its pins stand.
        let camera: Rc<std::cell::Cell<MapCamera>> = Rc::new(std::cell::Cell::new(self.camera));
        self.camera_cell = Some(Rc::clone(&camera));
        // How tall each pin stands, which a view that came to another height than the region
        // placed it at updates: the region places its box there.
        let heights: Rc<std::cell::RefCell<Vec<f64>>> = Rc::clone(&self.heights);
        let mut region = Manual::new(move |cx| -> Result<Size<f32>, WidgetLayoutError> {
            let available = cx.available();
            let camera = camera.get();
            let heights = heights.borrow();
            for index in 0..cx.len() {
                let Some(place) = places.get(index) else {
                    continue;
                };
                // A pin's box is its place's size at the camera's zoom: it is where the pin is
                // drawn, never how its view is laid out (that is its host's own, unzoomed).
                let height = place
                    .height
                    .unwrap_or_else(|| heights.get(index).copied().unwrap_or(0.0));
                let rect = Rect::from_origin_size(
                    (
                        (place.x - camera.origin.x) * camera.zoom,
                        (place.y - camera.origin.y) * camera.zoom,
                    ),
                    (
                        (place.width * camera.zoom).max(1.0),
                        (height * camera.zoom).max(0.0),
                    ),
                );
                cx.place(index, rect)?;
            }
            // The region is its viewport: a canvas does not grow with what it holds.
            Ok(Size {
                width: available.width.max(0.0),
                height: available.height.max(0.0),
            })
        });
        region = region.filling();
        for host in hosts {
            region = region.child(host);
        }
        self.invalidation = region.invalidation();
        self.parts.children = vec![Box::new(region)];
    }

    /// The host of the pin at `index` in draw order, while the region holds it.
    fn host_mut(&mut self, index: usize) -> Option<&mut PinHost> {
        let place = self.pins.get(index)?.1;
        let host = self
            .region_mut()
            .and_then(|region| region.child_mut(index))
            .and_then(|child| child.as_any_mut())
            .and_then(|any| any.downcast_mut::<PinHost>())?;
        host.place = place;
        host.index = index;
        Some(host)
    }

    /// The host of the pin keyed `key`, while the region holds it.
    fn host_of(&self, key: &str) -> Option<&PinHost> {
        let region = self
            .parts
            .children
            .first()?
            .as_any()?
            .downcast_ref::<Manual>()?;
        let index = self.pins.iter().position(|(held, _, _)| held == key)?;
        region.child_at(index)?.as_any()?.downcast_ref::<PinHost>()
    }

    /// Lay every pin's view out at its own place size, in the view's own space (canvas pixels,
    /// unzoomed): the region then places each pin's box at the height its view stands. Before the
    /// region solves, so a place with no height of its own still places its box where its view
    /// reaches; a camera move never reaches here.
    fn lay_out_views(&mut self, theme: &Theme, text: &mut TextRenderer) {
        let count = self.pins.len();
        for index in 0..count {
            if let Some(host) = self.host_mut(index) {
                host.layout_view(theme, text);
            }
        }
    }

    /// The pins the map holds changed: the region is built again around them (arranged, given
    /// or swapped views), so each has its box where the camera draws it.
    fn structure_changed(&mut self) {
        self.dirty = true;
        // A change in which pins the map holds, or the views they show, builds the region again
        // around them.
        self.structure = true;
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
        } = props;
        let mut map = Self::new();
        map.name = name;
        map.key = format!("map\u{1f}{key}");
        // The lens the person left this map looking through on this device: a map built again
        // around a place opened, closed or moved looks where they were looking, so the place they
        // were in stays in sight (`set_camera` remembers it, and its own `build_layout` recalls it
        // where a map is built raw).
        map.camera = recalled(&map.key).unwrap_or_default();
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
            name,
            ground,
            pins,
            drop,
        } = props;
        self.drop = drop;
        self.name = name;
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

/// The pins of a map: one view each. Each view is hosted by the map's region (`PinHost`), laid
/// out at its place's own size; a change here reaches the pin's host or rebuilds the region
/// around the pins it holds. The pins' keys and places stay in `self.pins`.
impl SlotViews for Map {
    fn len(&self) -> usize {
        self.pins.len()
    }

    fn view_mut(&mut self, index: usize) -> Option<&mut Element> {
        self.host_mut(index).map(PinHost::view_mut)
    }

    fn insert(&mut self, index: usize, view: Element) {
        let at = index.min(self.pins.len());
        let key = format!("pin-{at}");
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
                view,
            ),
        );
        // A pin that arrived takes its host of its own: the region built around the pins it now
        // holds gives it a box.
        self.structure_changed();
    }

    fn remove(&mut self, index: usize) {
        if index < self.pins.len() {
            self.pins.remove(index);
            self.structure_changed();
        }
    }

    fn move_view(&mut self, from: usize, to: usize) {
        if from < self.pins.len() && to < self.pins.len() {
            let pin = self.pins.remove(from);
            self.pins.insert(to, pin);
            self.structure_changed();
        }
    }

    fn replace(&mut self, index: usize, view: Element) {
        if let Some(slot) = self.view_mut(index) {
            *slot = view;
        } else if let Some((_, _, slot)) = self.pins.get_mut(index) {
            *slot = view;
        }
    }
}

impl Map {
    /// Draw the map's pins, each posed by its own [`crate::motion::Placed`]: a pin that just
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
        // Each pin's box on screen, from its host (which the region placed and the camera
        // transformed), and the keys the map holds now.
        let boxes: Vec<(String, Rect)> = self
            .pins
            .iter()
            .filter_map(|(key, _, _)| Some((key.clone(), self.host_of(key)?.screen)))
            .collect();
        let live: std::collections::HashSet<String> =
            boxes.iter().map(|(key, _)| key.clone()).collect();
        // Bring every pin's motion up to this frame, from where the camera draws its box.
        for (key, rect) in &boxes {
            let placed = self
                .placed
                .entry(key.clone())
                .or_insert_with(crate::motion::Placed::closed);
            placed.follow(*rect, true, motion, source, now);
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
        // Draw each pin the map holds, at its box, under its own pose. The view is drawn through
        // the camera's transform: laid out at its place's own size, it reads at any zoom.
        for (key, rect) in boxes {
            let Some(placed) = self.placed.get(&key) else {
                continue;
            };
            if !placed.is_visible(now) {
                continue;
            }
            let pose = placed.pose(rect, now);
            let bounds = rect.union(pose.drawn_rect(rect));
            let Some(index) = self.pins.iter().position(|(held, _, _)| *held == key) else {
                continue;
            };
            let Some(host) = self.host_mut(index) else {
                continue;
            };
            pose.paint(scene, transform, bounds, |scene, transform| {
                host.draw(scene, transform, text, theme, presence);
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
        // A map the view built raw (`Map::new`, not through its props) picks up this thread's lens
        // for its key here, where the key is known.
        if let Some(camera) = recalled(&self.key) {
            self.camera = camera;
        }
        self.rebuild_region(theme);
        // The region was just built around the pins the map holds now.
        self.structure = false;
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

    /// Lay every pin's view out at its own place size before the region places its box: the
    /// region's own solve reads the heights the views stand, and a pin laid out again is one
    /// whose place's size or view changed, never one the camera moved.
    fn adjust_layout_style(
        &mut self,
        _taffy: &mut TaffyTree<()>,
        _node: NodeId,
        text: &mut TextRenderer,
    ) -> bool {
        let Some(theme) = self.theme.clone() else {
            return false;
        };
        self.lay_out_views(&theme, text);
        // A view that came to another height than the region placed its box at re-solves the
        // region this frame (the flag lives with the hosts that set it).
        self.resize.replace(false)
    }

    fn apply_layout(&mut self, tree: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.parts.apply_layout(tree, node, origin);
        self.rect = crate::widget::layout_rect(tree, node, origin);
        // A place that arrived off screen comes into view: the canvas pans to it once the map
        // built around it is laid out (`shell.md` §3, "a place next to where it came from"; the
        // session's `open` brings the canvas to what it stood). Only the last pin — the place
        // just put on the canvas — and only when it stands wholly outside the viewport, so a
        // place already in sight moves nothing. Every place that stood in the viewport stays
        // there: a place opened beside a conversation never pushes the conversation out of the
        // viewport, however many were opened before it (`reveal_with`).
        let arriving = self
            .pins
            .last()
            .map(|(key, place, _)| (key.clone(), *place));
        // What the person was looking at: every pin that stood wholly in the viewport before the
        // place arrived, as canvas rectangles.
        let prior = self.rect;
        let kept: Vec<Rect> = self
            .pins
            .iter()
            .rev()
            .skip(1)
            .filter(|(_, place, _)| {
                let area = Rect::from_origin_size(
                    Point::new(place.x, place.y),
                    (place.width.max(1.0), place.height.unwrap_or(0.0)),
                );
                prior.width() > 0.0
                    && prior.height() > 0.0
                    && self.camera.to_screen(prior.origin(), area.origin()).x >= prior.x0
                    && self
                        .camera
                        .to_screen(prior.origin(), Point::new(area.x1, area.y0))
                        .x
                        <= prior.x1
                    && self.camera.to_screen(prior.origin(), area.origin()).y >= prior.y0
                    && self
                        .camera
                        .to_screen(prior.origin(), Point::new(area.x0, area.y1))
                        .y
                        <= prior.y1
            })
            .map(|(_, place, _)| {
                Rect::from_origin_size(
                    Point::new(place.x, place.y),
                    (place.width.max(1.0), place.height.unwrap_or(0.0)),
                )
            })
            .collect();
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
            self.reveal_with(
                area,
                f64::from(crate::tokens::RhythmScale::BASE.get(crate::tokens::Rhythm::Group)),
                &kept,
            );
        }
    }

    fn layout_needs_measure(&self) -> bool {
        // The region answers for its hosts: a pin whose view came to another height, or needs a
        // measure, asks the region to solve again.
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

    /// The canvas as a group named for what it is; its pins' views are its children, walked
    /// through the region that places them.
    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        Some(
            crate::a11y::AccessNode::new(crate::a11y::Role::Group)
                .with_optional_name(self.name.clone()),
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
        let mut dirty = std::mem::take(&mut self.structure);
        for child in &mut self.parts.children {
            dirty |= child.take_structure_dirty();
        }
        dirty
    }

    fn take_structure_change(&mut self) -> crate::widget::StructureChange {
        // A pin added, removed or moved changes which widgets the map holds, so the host builds
        // the region again: every node under the map may be gone.
        if std::mem::take(&mut self.structure) {
            for child in &mut self.parts.children {
                let _ = child.take_structure_change();
            }
            return crate::widget::StructureChange::Rebuild;
        }
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

#[cfg(test)]
#[path = "map_motion_tests.rs"]
mod motion_tests;

#[cfg(test)]
#[path = "map_scale_tests.rs"]
mod scale_tests;
