//! `Stage` — a page of a fixed size, shown at any size: a slide on its canvas, the same slide as
//! a thumbnail in the rail, and again filling the screen while it is presented.
//!
//! A stage is placed content, as a canvas's places are: each child stands at a point of the page
//! in the page's own coordinates (a slide is 1280 × 720 whatever the window or the rail row is
//! wide). What differs from a canvas is what the coordinates mean: a stage lays its children out
//! at the page's size and draws them through one scale, so a text box set at 32 points on the
//! slide is that size on the slide and in the thumbnail, and the words break at the same places
//! at every size. A canvas keeps every place's own size on screen and scales only the ground
//! under it.
//!
//! The person presses a place to choose it, drags it to move it and drags its edge or corner to
//! size it; a press that travels no further than the click slop is the place's own, so a click in
//! a text box still puts the caret in it. A place being moved lines up with the page's frame, the
//! margins a page keeps from it and the places around it, and the lines it lined up with show
//! while it moves. The stage changes nothing itself: the view commits one change (one undo step,
//! shared with everyone looking) and hands the new places back.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use taffy::{NodeId, Size, Style, TaffyTree};
use vello::kurbo::{Affine, Line, Point, Rect, RoundedRect, Vec2};
use vello::peniko::{BlendMode, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::component::{Component, Described, Event, PartType, PartViews, Source, Value};
use crate::theme::Theme;
use crate::widget::composition_parts::Parts;
use crate::widget::context_menu::{ContextMenu, MenuItem};
use crate::widget::{
    DrawChild, Element, Manual, MeasureConstraint, MenuPart, Widget, WidgetLayoutError,
};

/// How close to a place's trailing or bottom edge a press grabs it, in logical pixels: the same
/// reach a canvas's places keep, so the pips of a slide are as easy to hit as a window's edge.
const EDGE_GRAB: f64 = 8.0;

/// How large a background colour's page is drawn at least, so a stage of no size still lays out.
const LEAST_SIDE: f64 = 1.0;

/// How close to a line a dragged place comes, in logical pixels, before it lines up with it.
const SNAP_REACH: f64 = 8.0;

/// How much of the page one line of a wheel moves it, in window pixels.
const WHEEL_STEP: f64 = 40.0;

/// How much one wheel notch or a pinch zooms the page, as a factor.
const ZOOM_PER_NOTCH: f64 = 1.1;

/// What stands on the page: a text box, a shape, a picture — each a view of its own, at a place
/// in the page's coordinates.
#[derive(Component, Default)]
#[component(name = "StagePin", role = "Group", label = "name")]
pub struct StagePinProps {
    /// The place's stable key.
    #[component(key)]
    pub key: String,
    /// What it is, for assistive technology.
    pub name: Option<String>,
    /// The page point of its top-leading corner.
    pub x: Option<i64>,
    /// The page's y of its top-leading corner.
    pub y: Option<i64>,
    /// How wide it stands, in page coordinates.
    pub width: Option<i64>,
    /// How tall it stands, in page coordinates.
    pub height: Option<i64>,
    /// The person drags it to move it: a press that travels moves the place with the hand, and a
    /// press that stays is the place's own (a text box puts the caret where it was clicked).
    pub moves: bool,
    /// What the place stands for, as the view names it: what choosing it, moving it, sizing it
    /// and pressing it with the right button report. Without it, the place's key.
    #[component(data)]
    pub item: Option<String>,
    /// What the place offers on its context menu: a right press, the Menu key, a long press of a
    /// finger, or assistive technology opens it, and the place is chosen first. Its commands run
    /// as the view bound them (`on choose`), one undo step each.
    #[component(part)]
    pub menu: Vec<MenuPart>,
    /// What it shows.
    #[component(part(default))]
    pub body: Vec<Element>,
}

/// A page of a fixed size, with what stands on it placed in the page's own coordinates.
#[derive(Component, Default)]
#[component(name = "Stage", role = "Group", label = "name")]
pub struct StageProps {
    /// What the stage is, for assistive technology.
    pub name: Option<String>,
    /// The page's own size, its width then its height (a slide is 1280, 720).
    #[component(required)]
    pub canvas: Vec<i64>,
    /// How many logical pixels one page coordinate takes, as the person chose it: the page is
    /// fitted to the stage's box, whole and centred, unless said. The stage's wheel, pinch and
    /// keys report the zoom they took (`zoomed`), so a view that keeps it here finds the zoom
    /// where the person left it after a rebuild.
    pub zoom: Option<f64>,
    /// How far the page is moved from centred, in logical pixels, its x then its y: the stage
    /// keeps it within the travel the page's edges allow and reports the pan it took (`panned`).
    pub pan: Option<Vec<f64>>,
    /// The colour the page itself is painted in, `#rrggbb` or `#rrggbbaa`; none leaves it clear.
    pub background: Option<String>,
    /// The place the view chose, by its item or key: drawn with its frame, so the person sees
    /// what they are working on.
    pub chosen: Option<String>,
    /// What stands on the page, back to front.
    #[component(part(default))]
    pub pins: Vec<StagePinProps>,
    /// The person pressed the page's own empty surface and let go there, travelling no further
    /// than a click: carries the page point, its x then its y.
    #[component(event(value = Vec<i64>))]
    pub press: Event,
    /// The person pressed a place: carries what it stands for and the page point.
    #[component(event(value = String, at = Vec<i64>))]
    pub select: Event,
    /// The person dragged a place to a new place on the page: carries what it stands for and its
    /// new page point. Reported as the hand moves, as one gesture, so everyone looking sees it
    /// move and one undo takes the whole move back.
    #[component(event(value = String, at = Vec<i64>))]
    pub moved: Event,
    /// The person dragged a place's trailing edge, bottom edge or corner: carries what it stands
    /// for and its new width and height in page coordinates. Its top-leading corner stays.
    #[component(event(value = String, at = Vec<i64>))]
    pub resized: Event,
    /// The person pressed a place with the right button: carries what it stands for and the page
    /// point.
    #[component(event(value = String, at = Vec<i64>))]
    pub context: Event,
    /// The person zoomed the page, by a wheel with the command modifier, a pinch or a key:
    /// carries the zoom they chose, in logical pixels a page coordinate, or 0 for the page fitted
    /// whole. A view that keeps it and gives it back as `zoom` finds the zoom where the person
    /// left it after a rebuild.
    #[component(event(value = f64))]
    pub zoomed: Event,
    /// The person moved the page: carries its new pan, its x then its y in window pixels. A view
    /// that keeps it and gives it back as `pan` finds the page where the person left it.
    #[component(event(value = Vec<f64>))]
    pub panned: Event,
    /// The person pressed Enter on the chosen place: carries what it stands for. The view opens
    /// it — a text box takes the typing.
    #[component(event(value = String))]
    pub open: Event,
    /// The person pressed Delete on the chosen place: carries what it stands for. The view
    /// removes it.
    #[component(event(value = String))]
    pub delete: Event,
    /// The stage stands the page where the view puts it and takes nothing of its own: the wheel
    /// and a pinch do not zoom it, no pan moves it, the keys that walk and move its places do
    /// not reach it, and a dragged place does not line up with the page's margins. A stage laid
    /// over a page another view draws sets this, so the page under it keeps its own zoom and
    /// its own keys and its places move only where the hand puts them.
    pub still: bool,
    /// How the stage plays another page swapped for the one it shows: `.cross_fade` keeps the
    /// page that was shown laid out while it fades out and fades the new one in over it, for a
    /// stage that presents one page at a time. A change of the same page (a moved place, a zoom)
    /// is reconciled in place and never swaps.
    pub transition: Option<crate::motion::Swap>,
    /// The pace a page swap plays at: `.standard` (the theme's, and what the stage does anyway),
    /// `.brisk`, `.calm`, or `.still`, whose swaps land at once.
    pub motion: Option<crate::motion::Feel>,
}

impl PartType for Vec<StagePinProps> {
    const MANY: bool = true;

    fn accepts() -> crate::component::Accepts {
        crate::component::Accepts::Component(StagePinProps::NAME.into())
    }

    fn fill(&mut self, source: &mut dyn Source, part: &str) -> Result<(), String> {
        let mut out = Vec::new();
        let mut failed: Option<String> = None;
        source.components(part, &mut |child| match StagePinProps::from_source(child) {
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

/// A place on the page, in page coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Place {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    moves: bool,
}

impl Place {
    fn rect(self) -> Rect {
        Rect::from_origin_size((self.x, self.y), (self.w, self.h))
    }
}

/// A press on a place that can still become a drag: where it went down on the page, and the place
/// it holds.
#[derive(Clone, Copy, Debug)]
struct Grab {
    index: usize,
    /// Where the pointer went down, on the page.
    from: Point,
    place: Place,
    /// The press travelled further than a click: the place moves with the hand.
    moving: bool,
}

/// A drag of a place's trailing edge, bottom edge or corner, on the page.
#[derive(Clone, Copy, Debug)]
struct Edge {
    index: usize,
    from: Rect,
    start: Point,
    now: Point,
    trailing: bool,
    bottom: bool,
}

impl Edge {
    fn rect(self) -> Rect {
        let mut rect = self.from;
        if self.trailing {
            rect.x1 = (rect.x1 + (self.now.x - self.start.x)).max(rect.x0 + LEAST_SIDE);
        }
        if self.bottom {
            rect.y1 = (rect.y1 + (self.now.y - self.start.y)).max(rect.y0 + LEAST_SIDE);
        }
        rect
    }
}

pub struct Stage {
    parts: Parts,
    /// Each place: its key, its place on the page, and the view it shows.
    pins: Vec<(String, Place, Element)>,
    /// What each place stands for, by its key, as its view named it.
    items: HashMap<String, String>,
    /// The menu each place offers, by its key, as its view placed it: a text box's words, an
    /// element's own commands. Empty for a place that offers none.
    menus: HashMap<String, Vec<MenuPart>>,
    /// The one menu a place offered that is open now, and the place it is about.
    menu: ContextMenu,
    menu_pin: Option<usize>,
    /// What the page is, for assistive technology.
    name: Option<String>,
    canvas: (f64, f64),
    /// The zoom the person chose, in logical pixels a page coordinate: `None` fits the page.
    chosen_zoom: Option<f64>,
    /// How far the page is moved from centred, in window pixels, kept within the travel its
    /// edges allow.
    chosen_pan: Vec2,
    /// The platform whose command modifier zooms: the Ctrl of a PC, the ⌘ of a Mac.
    platform: crate::platform::PlatformConventions,
    /// The keyboard is in the page, for the keys that zoom it.
    focused: bool,
    background: Option<vello::peniko::Color>,
    chosen: Option<String>,
    /// Where each place was laid out last, in page coordinates, by its key.
    laid: Rc<RefCell<Vec<Rect>>>,
    /// Where the places stand, read by the region at each solve.
    places: Rc<RefCell<Vec<Place>>>,
    invalidation: crate::widget::ManualInvalidation,
    /// The stage's own box, in the window's coordinates.
    rect: Rect,
    /// What one page coordinate takes on screen, and where the page's origin stands inside the
    /// stage's box: the one scale the page and everything on it is drawn through.
    scale: f64,
    offset: Vec2,
    layout_dirty: bool,
    /// Something of the stage changed since the last frame it drew.
    dirty: bool,
    /// The press on a place, until it is let go.
    grab: Option<Grab>,
    /// The place's edge or corner being dragged, until it is let go.
    edge: Option<Edge>,
    /// The one gesture a drag is, so its reports are one step of the person's.
    gesture: Option<tilo_actions::ActionId>,
    cursor: Point,
    slop: f64,
    press: Event,
    select: Event,
    moved: Event,
    resized: Event,
    context: Event,
    zoomed: Event,
    panned: Event,
    open: Event,
    delete: Event,
    /// The pin whose view a change inside it is patching, by its index.
    editing: Option<usize>,
    /// What a place being dragged lines up with, while it moves: drawn across the page.
    guides: RefCell<Vec<crate::snap::Guide>>,
    /// The page stands where the view puts it, taking no zoom, pan or keys of its own.
    still: bool,
    /// The clicks on a place, so a second one on the same place opens it: a double click's, or a
    /// finger's double tap, which the window's touch emulation makes a click too.
    clicks: super::rows::Clicks<usize>,
}

impl Stage {
    fn no_body() -> Element {
        Box::new(crate::widget::Spacer::new())
    }

    fn item_of(&self, index: usize) -> String {
        let Some((key, _, _)) = self.pins.get(index) else {
            return String::new();
        };
        self.items.get(key).cloned().unwrap_or_else(|| key.clone())
    }

    /// The design point under `local` (this stage's own coordinates).
    fn page_point(&self, local: Point) -> Point {
        let inside = local - self.rect.origin().to_vec2() - self.offset;
        let scale = self.scale.max(f64::EPSILON);
        Point::new(inside.x / scale, inside.y / scale)
    }

    /// `event` as a place reads it: a place is laid out on the page and drawn through the
    /// stage's own scale, so the hand's window position reaches it as the page point it stands
    /// on. Events that carry no position reach the places as they are.
    fn place_event(
        &self,
        event: &WindowEvent,
        ctx: &crate::widget::EventCtx<'_>,
    ) -> Option<WindowEvent> {
        let on_page = |at: winit::dpi::PhysicalPosition<f64>| {
            let at = self.page_point(ctx.logical(at));
            winit::dpi::PhysicalPosition::new(at.x * ctx.scale_factor, at.y * ctx.scale_factor)
        };
        match event {
            WindowEvent::CursorMoved {
                device_id,
                position,
            } => Some(WindowEvent::CursorMoved {
                device_id: *device_id,
                position: on_page(*position),
            }),
            WindowEvent::Touch(touch) => Some(WindowEvent::Touch(winit::event::Touch {
                location: on_page(touch.location),
                ..touch.clone()
            })),
            _ => None,
        }
    }

    /// What one page coordinate takes on screen: the fit the person zoomed to.
    #[must_use]
    pub fn scale(&self) -> f64 {
        self.scale
    }

    /// How far the page is moved from centred, in window pixels.
    #[must_use]
    pub fn pan(&self) -> Vec2 {
        self.clamped_pan(self.rect, self.scale)
    }

    /// The place the view chose, by its item or key.
    fn chosen_index(&self) -> Option<usize> {
        let chosen = self.chosen.as_deref()?;
        self.pins.iter().position(|(key, _, _)| {
            key == chosen || self.items.get(key).map(String::as_str) == Some(chosen)
        })
    }

    /// The place at `index`, and its item as the view names it.
    fn item_at(&self, index: usize) -> String {
        self.item_of(index)
    }

    /// Choose the place `step` places along from the one the view chose, wrapping: Tab walks the
    /// page's places, so the person reaches everything on it from the keyboard.
    fn choose_along(&mut self, step: isize) {
        if self.pins.is_empty() {
            return;
        }
        let Ok(count) = isize::try_from(self.pins.len()) else {
            return;
        };
        let at = match self.chosen_index() {
            Some(index) => isize::try_from(index)
                .unwrap_or(0)
                .saturating_add(step)
                .rem_euclid(count),
            None if step < 0 => count - 1,
            None => 0,
        };
        let Ok(index) = usize::try_from(at) else {
            return;
        };
        let item = self.item_at(index);
        let place = self.pins[index].1;
        self.chosen = Some(item.clone());
        self.dirty = true;
        self.select.fire(Some(Value::List(vec![
            Value::Text(item),
            Self::point_value(Point::new(place.x, place.y)),
        ])));
    }

    /// Move the chosen place by `(dx, dy)` page units, as an arrow key asks.
    fn nudge(&mut self, dx: f64, dy: f64) {
        let Some(index) = self.chosen_index() else {
            return;
        };
        let (key, place, _) = &mut self.pins[index];
        let item = self.items.get(key).cloned().unwrap_or_else(|| key.clone());
        let x = (place.x + dx).max(0.0);
        let y = (place.y + dy).max(0.0);
        place.x = x;
        place.y = y;
        self.stand_places();
        self.invalidate();
        self.moved.fire(Some(Value::List(vec![
            Value::Text(item),
            Self::point_value(Point::new(x, y)),
        ])));
    }

    /// Where the page point `at` stands in this stage's own coordinates.
    fn screen_point(&self, at: Point) -> Point {
        self.rect.origin() + self.offset + at.to_vec2() * self.scale
    }

    /// The page rectangle `rect` takes on screen, in this stage's own coordinates.
    fn screen_rect(&self, rect: Rect) -> Rect {
        Rect::from_points(
            self.screen_point(rect.origin()),
            self.screen_point(Point::new(rect.x1, rect.y1)),
        )
    }

    /// The scale the page is drawn at in its box: fitted whole and centred unless the view gave
    /// one or the person zoomed, times the zoom they chose.
    fn fit(&self, box_rect: Rect) -> (f64, Vec2) {
        let scale = self.effective_scale(box_rect);
        (
            scale,
            self.centred(box_rect, scale) + self.clamped_pan(box_rect, scale),
        )
    }

    /// What one page coordinate takes: the zoom the person chose, or the size that fits the
    /// page whole in its box.
    fn effective_scale(&self, box_rect: Rect) -> f64 {
        let fitted = self.fitted_scale(box_rect);
        self.chosen_zoom.map_or(fitted, |zoom| {
            zoom.clamp(
                f64::from(crate::content_zoom::MIN),
                f64::from(crate::content_zoom::MAX),
            )
        })
    }

    /// The scale the page takes with no zoom of its own: as large as its box holds it whole.
    fn fitted_scale(&self, box_rect: Rect) -> f64 {
        let (width, height) = (self.canvas.0.max(1.0), self.canvas.1.max(1.0));
        (box_rect.width() / width)
            .min(box_rect.height() / height)
            .max(0.0001)
    }

    /// Where the page stands in its box, centred, at `scale`.
    fn centred(&self, box_rect: Rect, scale: f64) -> Vec2 {
        Vec2::new(
            (box_rect.width() - self.canvas.0.max(1.0) * scale) / 2.0,
            (box_rect.height() - self.canvas.1.max(1.0) * scale) / 2.0,
        )
    }

    /// How far the page may travel from centred at `scale` before one of its edges passes the
    /// stage's own: the page's edges stop at the stage's, so a person never loses it.
    fn reach(&self, box_rect: Rect, scale: f64) -> Vec2 {
        Vec2::new(
            ((self.canvas.0.max(1.0) * scale - box_rect.width()) / 2.0).max(0.0),
            ((self.canvas.1.max(1.0) * scale - box_rect.height()) / 2.0).max(0.0),
        )
    }

    /// The pan the person asked for, kept within what the page's edges allow.
    fn clamped_pan(&self, box_rect: Rect, scale: f64) -> Vec2 {
        let reach = self.reach(box_rect, scale);
        Vec2::new(
            self.chosen_pan.x.clamp(-reach.x, reach.x),
            self.chosen_pan.y.clamp(-reach.y, reach.y),
        )
    }

    /// Draw the page at the zoom and pan the stage holds now, and report both to the view.
    fn show_view(&mut self) {
        let (scale, offset) = self.fit(self.rect);
        self.chosen_pan = self.clamped_pan(self.rect, scale);
        self.scale = scale;
        self.offset = offset;
        self.invalidate();
        let zoom = if self.chosen_zoom.is_some() {
            scale
        } else {
            0.0
        };
        self.zoomed.fire(Some(Value::Float(zoom)));
        self.panned.fire(Some(Value::List(vec![
            Value::Float(self.chosen_pan.x),
            Value::Float(self.chosen_pan.y),
        ])));
    }

    /// Zoom by `factor` about the pointer, keeping the page point under it where the page's
    /// edges allow.
    fn zoom_about(&mut self, factor: f64) {
        let now = self.effective_scale(self.rect);
        let scale = (now * factor).clamp(
            f64::from(crate::content_zoom::MIN),
            f64::from(crate::content_zoom::MAX),
        );
        if (scale - now).abs() <= f64::EPSILON {
            return;
        }
        let local = self.cursor - self.rect.origin().to_vec2();
        let at = self.page_point(local);
        self.chosen_zoom = Some(scale);
        let want = local.to_vec2() - at.to_vec2() * scale;
        self.chosen_pan = want - self.centred(self.rect, scale);
        self.show_view();
    }

    /// Move the page by `by` window pixels, reporting whether it moved: a fitted page has
    /// nowhere to go, so the wheel is the view's then.
    fn pan_view(&mut self, by: Vec2) -> bool {
        let before = self.clamped_pan(self.rect, self.scale);
        self.chosen_pan += by;
        let after = self.clamped_pan(self.rect, self.scale);
        if (after - before).hypot() <= f64::EPSILON {
            self.chosen_pan = before;
            return false;
        }
        self.show_view();
        true
    }

    /// The place under `local` (this stage's own coordinates): the topmost first.
    fn place_under(&self, local: Point) -> Option<usize> {
        let at = self.page_point(local);
        self.pins
            .iter()
            .enumerate()
            .rev()
            .find(|(_, (_, place, _))| place.rect().contains(at))
            .map(|(index, _)| index)
    }

    /// The place whose trailing edge, bottom edge or corner stands at `local`, and which edges
    /// the point grabs.
    fn edge_at(&self, local: Point) -> Option<(usize, bool, bool)> {
        let laid = self.laid.borrow();
        let at = self.page_point(local);
        self.pins.iter().enumerate().rev().find_map(|(index, _)| {
            let rect = self.screen_rect(*laid.get(index)?);
            let reach = EDGE_GRAB;
            let trailing = (local.x - rect.x1).abs() <= reach
                && local.y >= rect.y0 - reach
                && local.y <= rect.y1 + reach;
            let bottom = (local.y - rect.y1).abs() <= reach
                && local.x >= rect.x0 - reach
                && local.x <= rect.x1 + reach;
            let _ = at;
            (trailing || bottom).then_some((index, trailing, bottom))
        })
    }

    /// Say where every place stands to the region that lays them out.
    fn stand_places(&self) {
        *self.places.borrow_mut() = self.pins.iter().map(|(_, place, _)| *place).collect();
    }

    /// Stand `view` where the place at `index` shows it: in the region once it is built, else in
    /// the place itself.
    fn put_view(&mut self, index: usize, view: Element) {
        if let Some(part) = self.region_mut().and_then(|region| region.child_mut(index)) {
            *part = view;
        } else if let Some((_, _, part)) = self.pins.get_mut(index) {
            *part = view;
        }
    }

    fn region_mut(&mut self) -> Option<&mut Manual> {
        self.parts
            .children
            .first_mut()
            .and_then(|child| child.as_any_mut())
            .and_then(|any| any.downcast_mut::<Manual>())
    }

    /// Build the region that places the page's children: each at its own place on the page, laid
    /// out at the page's size, ready to be drawn through the stage's one scale.
    fn rebuild_region(&mut self) {
        let mut views: Vec<Element> = match self.region_mut() {
            Some(region) => region.take_children(),
            None => Vec::new(),
        };
        while views.len() < self.pins.len() {
            let index = views.len();
            let view = std::mem::replace(&mut self.pins[index].2, Self::no_body());
            views.push(view);
        }
        views.truncate(self.pins.len());
        self.stand_places();
        let places = Rc::clone(&self.places);
        let laid: Rc<RefCell<Vec<Rect>>> = Rc::clone(&self.laid);
        let mut region = Manual::new(move |cx| -> Result<Size<f32>, WidgetLayoutError> {
            let available = cx.available();
            let places = places.borrow();
            let mut out: Vec<Rect> = Vec::with_capacity(cx.len());
            for index in 0..cx.len() {
                let Some(place) = places.get(index) else {
                    continue;
                };
                // Words that wrap to more lines than the place was given grow it: the place is
                // laid out at the height its view asks for, never less than the height the
                // element keeps on the slide. Anything that fits stands at its stored height, as
                // it stands on the page.
                let natural = cx
                    .measure_child(
                        index,
                        Size {
                            width: MeasureConstraint::Exact(f32_of(place.w)),
                            height: MeasureConstraint::Intrinsic,
                        },
                    )?
                    .size;
                let wants = f64::from(natural.height);
                let height = if wants > place.h + 0.5 {
                    cx.measure_child(
                        index,
                        Size {
                            width: MeasureConstraint::Exact(f32_of(place.w)),
                            height: MeasureConstraint::Exact(f32_of(wants)),
                        },
                    )?;
                    wants
                } else {
                    cx.measure_child(
                        index,
                        Size {
                            width: MeasureConstraint::Exact(f32_of(place.w)),
                            height: MeasureConstraint::Exact(f32_of(place.h)),
                        },
                    )?;
                    place.h
                };
                let rect =
                    Rect::from_origin_size((place.x, place.y), (place.w, height.max(LEAST_SIDE)));
                cx.place(index, rect)?;
                out.push(rect);
            }
            *laid.borrow_mut() = out;
            // The region is the stage's own box: the page's coordinates are placed inside it,
            // and the stage draws them through its scale.
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

    /// The boxes a dragged place lines up with: the page's frame and the margins a slide's words
    /// keep from it, and every other place's box.
    fn snap_targets(&self, moving: usize) -> Vec<Rect> {
        let (w, h) = self.canvas;
        let mut targets = vec![
            Rect::new(0.0, 0.0, w, h),
            Rect::new(96.0, 0.0, 96.0, h),
            Rect::new(w - 96.0, 0.0, w - 96.0, h),
            Rect::new(0.0, 70.0, w, 70.0),
            Rect::new(0.0, h - 60.0, w, h - 60.0),
        ];
        let laid = self.laid.borrow();
        for (index, rect) in laid.iter().enumerate() {
            if index != moving {
                targets.push(*rect);
            }
        }
        targets
    }

    /// Report a place being moved to the page point under the hand, lined up with the page and
    /// the places around it.
    fn report_move(&self) {
        let Some(grab) = self.grab else { return };
        let at = self.page_point(self.cursor);
        let dx = at.x - grab.from.x;
        let dy = at.y - grab.from.y;
        let moving = Rect::from_origin_size(
            (grab.place.x + dx, grab.place.y + dy),
            (grab.place.w, grab.place.h),
        );
        // A stage that takes nothing of its own moves the place exactly where the hand goes: a
        // page another view draws has no slide's margins for a place to line up with.
        let (x, y) = if self.still {
            (moving.x0.round(), moving.y0.round())
        } else {
            let reach = SNAP_REACH / self.scale.max(f64::EPSILON);
            let snap = crate::snap::snap_rect(moving, &self.snap_targets(grab.index), reach);
            *self.guides.borrow_mut() = snap.guides;
            (
                (moving.x0 + snap.offset.x).round(),
                (moving.y0 + snap.offset.y).round(),
            )
        };
        let item = self.item_of(grab.index);
        self.moved.fire_in_gesture(
            Some(Value::List(vec![
                Value::Text(item),
                Value::List(vec![Value::Int(x as i64), Value::Int(y as i64)]),
            ])),
            self.gesture,
        );
    }

    /// Mark the stage as needing a frame and another lay-out.
    fn invalidate(&mut self) {
        self.layout_dirty = true;
        self.dirty = true;
        self.invalidation.invalidate();
    }

    /// The page point `at` as a report's list of whole page coordinates.
    fn point_value(at: Point) -> Value {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "a page coordinate, whole on the slide"
        )]
        let (x, y) = (at.x.round() as i64, at.y.round() as i64);
        Value::List(vec![Value::Int(x), Value::Int(y)])
    }

    /// Open the menu the place at `index` offers, built anew from what the view placed there,
    /// with its corner at the window point `at`. Whether one opened.
    fn open_menu(&mut self, index: usize, at: Point) -> bool {
        let Some((key, _, _)) = self.pins.get(index) else {
            return false;
        };
        let Some(parts) = self.menus.get(key) else {
            return false;
        };
        let mut menu = ContextMenu::new();
        for part in parts {
            match part.again() {
                MenuPart::Item(item) => menu.push(item.into_item()),
                MenuPart::Separator => menu.push(MenuItem::separator()),
                // An emoji bar reacts to a message; a page's menu holds commands.
                MenuPart::Emoji(_) => {}
            }
        }
        if menu.items().is_empty() {
            return false;
        }
        menu.open_at([crate::num::f32_of_f64(at.x), crate::num::f32_of_f64(at.y)]);
        self.menu = menu;
        self.menu_pin = Some(index);
        self.dirty = true;
        true
    }

    /// Route `event` to the open menu. Returns whether it consumed it; a press outside closes
    /// the menu and falls through to the places.
    fn route_menu(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) -> bool {
        if !self.menu.is_open() {
            return false;
        }
        let mut consumed = false;
        self.menu.handle_event(
            event,
            &mut crate::widget::EventCtx {
                handled: &mut consumed,
                ..ctx.reborrow(ctx.path, ctx.node_id)
            },
        );
        self.drain_menu_selection();
        if !self.menu.is_open() {
            self.menu_pin = None;
            self.dirty = true;
        }
        if consumed {
            *ctx.handled = true;
        }
        consumed
    }

    /// Note the row the person took: it ran the view's command as it went, and the menu is no
    /// longer about a place.
    fn drain_menu_selection(&mut self) {
        if self.menu.take_selection().is_some() {
            self.menu_pin = None;
        }
    }

    /// Whether the open menu needs a frame: it arrives, leaves, a key moved its highlight, or
    /// the box beside it changed.
    fn menu_needs_redraw(&self) -> bool {
        self.menu.is_open() && self.menu.needs_redraw()
    }

    /// The semantics of the open menu, its targets namespaced into the stage's.
    fn menu_node(&self) -> Option<crate::a11y::AccessNode> {
        if !self.menu.is_open() {
            return None;
        }
        let mut node = Widget::accessibility(&self.menu)?;
        namespace_menu_targets(&mut node);
        Some(node)
    }
}

impl Described for Stage {
    type Props = StageProps;

    fn from_props(key: &str, props: StageProps) -> Self {
        let _ = key;
        let StageProps {
            name,
            canvas,
            zoom,
            background,
            chosen,
            pins,
            press,
            select,
            moved,
            resized,
            context,
            pan,
            zoomed,
            panned,
            open,
            delete,
            still,
            transition: _,
            motion: _,
        } = props;
        let mut stage = Self {
            parts: Parts::default(),
            pins: Vec::new(),
            items: HashMap::new(),
            menus: HashMap::new(),
            menu: ContextMenu::new(),
            menu_pin: None,
            name: name.filter(|name| !name.trim().is_empty()),
            canvas: (
                canvas.first().copied().unwrap_or(1280) as f64,
                canvas.get(1).copied().unwrap_or(720) as f64,
            ),
            chosen_zoom: zoom,
            chosen_pan: pan.map_or(Vec2::ZERO, |pan| {
                Vec2::new(
                    pan.first().copied().unwrap_or(0.0),
                    pan.get(1).copied().unwrap_or(0.0),
                )
            }),
            platform: crate::platform::PlatformConventions::default(),
            focused: false,
            background: background.as_deref().and_then(crate::color::parse_hex),
            chosen: chosen.filter(|chosen| !chosen.is_empty()),
            laid: Rc::default(),
            places: Rc::default(),
            invalidation: crate::widget::ManualInvalidation::default(),
            rect: Rect::ZERO,
            scale: 1.0,
            offset: Vec2::ZERO,
            layout_dirty: true,
            dirty: true,
            grab: None,
            edge: None,
            gesture: None,
            cursor: Point::ORIGIN,
            slop: 6.0,
            press,
            select,
            moved,
            resized,
            context,
            zoomed,
            panned,
            open,
            delete,
            editing: None,
            guides: RefCell::new(Vec::new()),
            still,
            clicks: super::rows::Clicks::default(),
        };
        for pin in pins {
            let place = Place {
                x: pin.x.unwrap_or(0) as f64,
                y: pin.y.unwrap_or(0) as f64,
                w: (pin.width.unwrap_or(320) as f64).max(LEAST_SIDE),
                h: (pin.height.unwrap_or(120) as f64).max(LEAST_SIDE),
                moves: pin.moves,
            };
            if let Some(item) = pin.item.filter(|item| !item.is_empty()) {
                stage.items.insert(pin.key.clone(), item);
            }
            if !pin.menu.is_empty() {
                stage.menus.insert(pin.key.clone(), pin.menu);
            }
            let body: Element = match pin.body.into_iter().next() {
                Some(body) => body,
                None => Self::no_body(),
            };
            stage.pins.push((pin.key, place, body));
        }
        stage
    }

    fn set_props(&mut self, props: StageProps) -> bool {
        let StageProps {
            name,
            canvas,
            zoom,
            background,
            chosen,
            pins,
            press,
            select,
            moved,
            resized,
            context,
            pan,
            zoomed,
            panned,
            open,
            delete,
            still,
            transition: _,
            motion: _,
        } = props;
        if pins.len() != self.pins.len()
            || pins
                .iter()
                .zip(&self.pins)
                .any(|(given, (key, _, _))| given.key != *key)
        {
            return false;
        }
        let name = name.filter(|name| !name.trim().is_empty());
        if name != self.name {
            self.name = name;
            self.layout_dirty = true;
        }
        let (width, height) = (
            canvas.first().copied().unwrap_or(1280) as f64,
            canvas.get(1).copied().unwrap_or(720) as f64,
        );
        if (width, height) != self.canvas {
            self.canvas = (width, height);
            self.layout_dirty = true;
        }
        if zoom != self.chosen_zoom {
            self.chosen_zoom = zoom;
            self.layout_dirty = true;
        }
        let pan = pan.map_or(Vec2::ZERO, |pan| {
            Vec2::new(
                pan.first().copied().unwrap_or(0.0),
                pan.get(1).copied().unwrap_or(0.0),
            )
        });
        if pan != self.chosen_pan {
            self.chosen_pan = pan;
            self.layout_dirty = true;
        }
        self.background = background.as_deref().and_then(crate::color::parse_hex);
        let chosen = chosen.filter(|chosen| !chosen.is_empty());
        if chosen != self.chosen {
            self.chosen = chosen;
            self.layout_dirty = true;
        }
        self.press = press;
        self.select = select;
        self.moved = moved;
        self.resized = resized;
        self.context = context;
        self.zoomed = zoomed;
        self.panned = panned;
        self.open = open;
        self.delete = delete;
        self.still = still;
        self.items.clear();
        self.menus.clear();
        for (pin, (key, held, _)) in pins.into_iter().zip(self.pins.iter_mut()) {
            if let Some(item) = pin.item.filter(|item| !item.is_empty()) {
                self.items.insert(key.clone(), item);
            }
            if !pin.menu.is_empty() {
                self.menus.insert(key.clone(), pin.menu);
            }
            *held = Place {
                x: pin.x.unwrap_or(0) as f64,
                y: pin.y.unwrap_or(0) as f64,
                w: (pin.width.unwrap_or(320) as f64).max(LEAST_SIDE),
                h: (pin.height.unwrap_or(120) as f64).max(LEAST_SIDE),
                moves: pin.moves,
            };
        }
        self.stand_places();
        self.layout_dirty = true;
        self.invalidation.invalidate();
        true
    }

    fn part_views(&mut self, part: &str) -> Option<&mut dyn PartViews> {
        self.editing = None;
        (part == "pins").then_some(self as &mut dyn PartViews)
    }

    fn item_views(&mut self, part: &str, key: &str, of: &str) -> Option<&mut dyn PartViews> {
        if part != "pins" || of != "body" {
            return None;
        }
        self.editing = Some(self.pins.iter().position(|(held, _, _)| held == key)?);
        Some(self as &mut dyn PartViews)
    }
}

impl PartViews for Stage {
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
        if self.parts.children.is_empty() {
            return self.pins.get_mut(index).map(|(_, _, view)| view);
        }
        self.region_mut()?.child_mut(index)
    }

    fn insert(&mut self, index: usize, view: Element) {
        if let Some(pin) = self.editing {
            if let Some((_, _, held)) = self.pins.get_mut(pin) {
                *held = view;
            }
            return;
        }
        let at = index.min(self.pins.len());
        let key = format!("pin-{at}");
        self.pins.insert(
            at,
            (
                key,
                Place {
                    x: 0.0,
                    y: 0.0,
                    w: 320.0,
                    h: 120.0,
                    moves: false,
                },
                Self::no_body(),
            ),
        );
        if let Some(region) = self.region_mut() {
            region.insert_child(at, view);
        } else if let Some((_, _, held)) = self.pins.get_mut(at) {
            *held = view;
        }
        self.stand_places();
        self.invalidate();
    }

    fn remove(&mut self, index: usize) {
        if let Some(pin) = self.editing {
            if let Some((_, _, held)) = self.pins.get_mut(pin) {
                *held = Self::no_body();
            }
            return;
        }
        if index < self.pins.len() {
            self.pins.remove(index);
            if let Some(region) = self.region_mut() {
                region.remove_child(index);
            }
            self.stand_places();
            self.layout_dirty = true;
        }
    }

    fn move_view(&mut self, from: usize, to: usize) {
        if self.editing.is_some() || from >= self.pins.len() || to >= self.pins.len() {
            return;
        }
        let pin = self.pins.remove(from);
        self.pins.insert(to, pin);
        if let Some(region) = self.region_mut() {
            region.move_child(from, to);
        }
        self.stand_places();
        self.layout_dirty = true;
    }

    fn replace(&mut self, index: usize, view: Element) {
        let index = self.editing.unwrap_or(index);
        self.put_view(index, view);
    }
}

impl Widget for Stage {
    crate::described!();

    fn name(&self) -> &'static str {
        "Stage"
    }

    fn test_id(&self) -> Option<&str> {
        self.name.as_deref()
    }

    fn update(&mut self, state: &crate::widget::AppModel, path: &crate::widget::ModelPath) -> bool {
        let mut changed = false;
        for child in &mut self.parts.children {
            changed |= child.update(state, path);
        }
        changed
    }

    fn sync(&mut self, state: &crate::widget::AppModel, path: &crate::widget::ModelPath) {
        for child in &mut self.parts.children {
            child.sync(state, path);
        }
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

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        self.parts
            .children
            .iter_mut()
            .map(|child| &mut **child as &mut (dyn Widget + '_))
            .collect()
    }

    fn build_layout(
        &mut self,
        tree: &mut TaffyTree<()>,
        theme: &Theme,
        map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.slop = f64::from(theme.tokens.metrics.click_slop);
        self.rebuild_region();
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
        let rect = crate::widget::layout_rect(tree, node, origin);
        if rect != self.rect {
            self.rect = rect;
        }
        let (scale, offset) = self.fit(rect);
        if (scale - self.scale).abs() > f64::EPSILON || offset != self.offset {
            self.scale = scale;
            self.offset = offset;
        }
        self.layout_dirty = false;
    }

    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
            || self.layout_dirty
            || self.invalidation.is_pending()
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
        crate::scene_parts::picture();
        let viewport = self.rect;
        let clips = viewport.width() > 0.0 && viewport.height() > 0.0;
        if clips {
            scene.push_layer(
                Fill::NonZero,
                BlendMode::default(),
                1.0,
                transform,
                &viewport,
            );
        }
        // The page itself, in the colour the view gave it.
        let page = self.screen_rect(Rect::from_origin_size(
            Point::ORIGIN,
            (self.canvas.0, self.canvas.1),
        ));
        if let Some(colour) = self.background {
            scene.fill(Fill::NonZero, transform, colour, None, &page);
        }
        // Everything on the page, drawn through the one scale the page is shown at: a text box
        // laid out at the slide's size, drawn small in a thumbnail and large on a projector.
        let page_transform = transform * Affine::translate(self.offset) * Affine::scale(self.scale);
        // What the place being dragged lines up with, across the page.
        let guides = self.guides.borrow();
        if !guides.is_empty() {
            let hairline = 1.0 / self.scale.max(f64::EPSILON);
            for guide in guides.iter() {
                let (from, to) = if guide.vertical {
                    (
                        Point::new(guide.at, guide.from),
                        Point::new(guide.at, guide.to),
                    )
                } else {
                    (
                        Point::new(guide.from, guide.at),
                        Point::new(guide.to, guide.at),
                    )
                };
                scene.stroke(
                    &vello::kurbo::Stroke::new(hairline),
                    page_transform,
                    theme.tokens.colors.intent.selection_tint,
                    None,
                    &Line::new(from, to),
                );
            }
        }
        for index in 0..self.pins.len() {
            let Some(region) = self.parts.children.first_mut() else {
                break;
            };
            let Some(region) = region
                .as_any_mut()
                .and_then(|any| any.downcast_mut::<Manual>())
            else {
                break;
            };
            let Some(child) = region.child_mut(index) else {
                break;
            };
            child.draw_child(scene, page_transform, text, theme, &env);
        }
        // The frame of a place being sized, on screen, as the size it is taking.
        if let Some(drag) = self.edge {
            let frame = self.screen_rect(drag.rect());
            scene.stroke(
                &vello::kurbo::Stroke::new(1.5),
                transform,
                theme.tokens.colors.intent.selection_tint,
                None,
                &RoundedRect::from_rect(frame, 2.0),
            );
        }
        // The frame of what the view chose, drawn in the window's pixels so it stays a hairline
        // however small the page is shown.
        if let Some(chosen) = self.chosen.clone() {
            if let Some(index) = self
                .pins
                .iter()
                .position(|(key, _, _)| *key == chosen || self.items.get(key) == Some(&chosen))
                && let Some(rect) = self.laid.borrow().get(index).copied()
            {
                let frame = self.screen_rect(rect);
                let radius = (4.0 * self.scale)
                    .min(frame.width() / 2.0)
                    .min(frame.height() / 2.0);
                scene.stroke(
                    &vello::kurbo::Stroke::new(1.5),
                    transform,
                    theme.tokens.colors.intent.selection_tint,
                    None,
                    &RoundedRect::from_rect(frame, radius.max(0.0)),
                );
            }
        }
        if clips {
            scene.pop_layer();
        }
    }

    fn needs_redraw(&self) -> bool {
        crate::widget::redraw_or_children(self.dirty || self.menu_needs_redraw(), self)
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        Widget::mark_drawn(&mut self.menu);
        for child in &mut self.parts.children {
            child.mark_drawn();
        }
    }

    /// The menu a place opened paints in the top layer, above every place and the page itself.
    fn draw_overlay(&mut self, ctx: &mut crate::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text, theme, env) = ctx.parts();
        // A place's own menu or popover stands in the window's pixels over the page: the place
        // is laid out and hit in page coordinates and drawn through the page's one scale, so
        // the menu's corner moves to the page point the place put it at and the menu keeps the
        // window's size, as one placed by hand does (`Embedded`).
        let page = Affine::translate(self.offset) * Affine::scale(self.scale);
        let mut child_env = env.clone();
        if page.determinant().abs() > f64::EPSILON
            && let Some(viewport) = env.viewport
        {
            child_env.viewport = Some(page.inverse().transform_rect_bbox(viewport));
        }
        if let Some(region) = self
            .parts
            .children
            .first_mut()
            .and_then(|child| child.as_any_mut())
            .and_then(|any| any.downcast_mut::<Manual>())
        {
            for index in 0..self.pins.len() {
                let Some(child) = region.child_mut(index) else {
                    break;
                };
                let placed = child.modal_capture().map_or(page, |panel| {
                    let corner = panel.origin();
                    let offset = (page * corner) - corner;
                    let placed = panel + offset;
                    let nudge = env.viewport.map_or(Vec2::ZERO, |viewport| {
                        Vec2::new(
                            in_viewport(placed.x0, placed.x1, viewport.x0, viewport.x1),
                            in_viewport(placed.y0, placed.y1, viewport.y0, viewport.y1),
                        )
                    });
                    Affine::translate(offset + nudge)
                });
                child.draw_overlay(&mut crate::draw::DrawCtx::new(
                    scene,
                    transform * placed,
                    text,
                    theme,
                    child_env.clone(),
                ));
            }
        }
        // The stage's own open menu stands above the places, in the window's own pixels.
        if self.menu.is_open() {
            self.menu.draw_overlay(&mut crate::draw::DrawCtx::new(
                scene, transform, text, theme, env,
            ));
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut crate::widget::EventCtx<'_>) {
        // The menu a place opened hears the event first, above the places and the page.
        if self.route_menu(event, ctx) {
            return;
        }
        match event {
            // The hand's place is read in page coordinates: a drag keeps moving the place it
            // holds past the stage's edge, as it would past the page's.
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor =
                    Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                if let Some(mut edge) = self.edge {
                    edge.now = self.page_point(self.cursor);
                    self.edge = Some(edge);
                    self.invalidate();
                    *ctx.handled = true;
                    return;
                }
                if let Some(mut grab) = self.grab {
                    if !grab.moving
                        && (self.page_point(self.cursor) - grab.from).hypot() * self.scale
                            >= self.slop
                    {
                        grab.moving = true;
                        self.grab = Some(grab);
                        self.gesture = Some(tilo_actions::ActionId::new());
                    }
                    if grab.moving {
                        self.grab = Some(grab);
                        self.report_move();
                        self.invalidate();
                        *ctx.handled = true;
                        return;
                    }
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } if self.edge.is_some() => {
                if let Some(drag) = self.edge.take() {
                    let rect = drag.rect();
                    let from = drag.from;
                    if rect != from {
                        let item = self.item_of(drag.index);
                        #[expect(
                            clippy::cast_possible_truncation,
                            reason = "a size on the page, whole"
                        )]
                        let (w, h) = (rect.width().round() as i64, rect.height().round() as i64);
                        self.resized.fire(Some(Value::List(vec![
                            Value::Text(item),
                            Value::List(vec![Value::Int(w), Value::Int(h)]),
                        ])));
                    }
                }
                self.invalidate();
                *ctx.handled = true;
                return;
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } if self.grab.is_some_and(|grab| grab.moving) => {
                self.grab = None;
                self.gesture = None;
                self.guides.borrow_mut().clear();
                self.invalidate();
                *ctx.handled = true;
                return;
            }
            _ => {}
        }
        // The places' own views take what is theirs: a press in a text box puts the caret in it,
        // a button in a picture's well presses. A place is laid out on the page and drawn
        // through the stage's one scale, so the hand's window position reaches it as the page
        // point it stands on.
        let on_page = self.place_event(event, ctx);
        for (child, node) in self.parts.children.iter_mut().zip(self.parts.nodes.clone()) {
            let path = ctx.path;
            let event = on_page.as_ref().unwrap_or(event);
            child.handle_event(event, &mut ctx.reborrow(path, node));
            if *ctx.handled {
                break;
            }
        }
        if *ctx.handled {
            return;
        }
        // The Menu key and Shift+F10 open the chosen place's menu where it stands.
        let keyboard_menu = self.focused
            && crate::widget::context_menu::opens_from_keyboard(event, ctx.modifiers.state());
        // The page's own view: a wheel moves it, the command modifier and a wheel (or a
        // pinch) zoom it about the pointer, and the keys that zoom it reach it while it holds
        // the keyboard. A fitted page has nowhere to move, so its wheel is the view's.
        match event {
            WindowEvent::MouseWheel { delta, .. }
                if !self.still && self.rect.contains(self.cursor) =>
            {
                let notches = match delta {
                    winit::event::MouseScrollDelta::LineDelta(x, y) => {
                        Vec2::new(f64::from(*x), f64::from(*y))
                    }
                    winit::event::MouseScrollDelta::PixelDelta(p) => {
                        Vec2::new(p.x / 50.0, p.y / 50.0)
                    }
                };
                if ctx.modifiers.state().control_key() || ctx.modifiers.state().super_key() {
                    if notches.y != 0.0 {
                        self.zoom_about(ZOOM_PER_NOTCH.powf(notches.y));
                        *ctx.handled = true;
                    }
                } else {
                    let by = match delta {
                        winit::event::MouseScrollDelta::LineDelta(x, y) => {
                            Vec2::new(f64::from(*x), f64::from(*y)) * WHEEL_STEP
                        }
                        winit::event::MouseScrollDelta::PixelDelta(p) => Vec2::new(p.x, p.y),
                    };
                    if by != Vec2::ZERO && self.pan_view(by) {
                        *ctx.handled = true;
                    }
                }
                if *ctx.handled {
                    return;
                }
            }
            WindowEvent::PinchGesture { delta, .. }
                if !self.still && self.rect.contains(self.cursor) =>
            {
                if *delta != 0.0 {
                    self.zoom_about(1.0 + *delta);
                    *ctx.handled = true;
                    return;
                }
            }
            WindowEvent::KeyboardInput { event, .. }
                if !self.still && self.focused && event.state == ElementState::Pressed =>
            {
                // The Menu key and Shift+F10 open the chosen place's menu where it stands.
                if keyboard_menu && let Some(index) = self.chosen_index() {
                    let at = self
                        .laid
                        .borrow()
                        .get(index)
                        .map_or(self.cursor, |rect| self.screen_rect(*rect).center());
                    if self.open_menu(index, at) {
                        *ctx.handled = true;
                        return;
                    }
                }
                let plain = !ctx.modifiers.state().control_key()
                    && !ctx.modifiers.state().super_key()
                    && !ctx.modifiers.state().alt_key();
                if plain {
                    match &event.logical_key {
                        Key::Named(NamedKey::Tab) => {
                            let step = if ctx.modifiers.state().shift_key() {
                                -1
                            } else {
                                1
                            };
                            self.choose_along(step);
                            *ctx.handled = true;
                            return;
                        }
                        Key::Named(NamedKey::Enter) => {
                            if let Some(index) = self.chosen_index() {
                                self.open.fire(Some(Value::Text(self.item_at(index))));
                                *ctx.handled = true;
                                return;
                            }
                        }
                        Key::Named(NamedKey::Delete | NamedKey::Backspace) => {
                            if let Some(index) = self.chosen_index() {
                                self.delete.fire(Some(Value::Text(self.item_at(index))));
                                *ctx.handled = true;
                                return;
                            }
                        }
                        Key::Named(
                            NamedKey::ArrowLeft
                            | NamedKey::ArrowRight
                            | NamedKey::ArrowUp
                            | NamedKey::ArrowDown,
                        ) => {
                            let by = if ctx.modifiers.state().shift_key() {
                                1.0
                            } else {
                                10.0
                            };
                            let (dx, dy) = match &event.logical_key {
                                Key::Named(NamedKey::ArrowLeft) => (-by, 0.0),
                                Key::Named(NamedKey::ArrowRight) => (by, 0.0),
                                Key::Named(NamedKey::ArrowUp) => (0.0, -by),
                                _ => (0.0, by),
                            };
                            self.nudge(dx, dy);
                            *ctx.handled = true;
                            return;
                        }
                        _ => {}
                    }
                }
                let step = crate::content_zoom::ZoomStep::from_key(
                    self.platform,
                    ctx.modifiers.state(),
                    &event.logical_key,
                );
                if let Some(step) = step {
                    match step {
                        crate::content_zoom::ZoomStep::Reset => {
                            self.chosen_zoom = None;
                            self.chosen_pan = Vec2::ZERO;
                        }
                        step => {
                            let fitted = self.fitted_scale(self.rect);
                            let scale = f64::from(step.apply_in(
                                &crate::content_zoom::LEVELS,
                                fitted as f32,
                                self.effective_scale(self.rect) as f32,
                            ));
                            self.chosen_zoom = Some(scale);
                            // A key steps the zoom about the page's centre.
                            self.chosen_pan = Vec2::ZERO;
                        }
                    }
                    self.show_view();
                    *ctx.handled = true;
                    return;
                }
            }
            _ => {}
        }
        let local = self.cursor - self.rect.origin().to_vec2();
        match event {
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if self.rect.contains(self.cursor) => {
                // A press in the page puts the keyboard in it, so the keys that zoom it reach
                // it without the person looking for a field to take them. A stage that takes
                // nothing of its own leaves the keyboard where it was.
                if !self.still {
                    *ctx.focus_target = Some(ctx.node_id);
                }
                // A place's edge or corner first: dragging it sizes the place. A stage that
                // takes nothing of its own keeps every place at the size its view gave it.
                if !self.still
                    && let Some((index, trailing, bottom)) = self.edge_at(local)
                {
                    let laid = self.laid.borrow();
                    let from = laid.get(index).copied().unwrap_or_default();
                    self.edge = Some(Edge {
                        index,
                        from,
                        start: self.page_point(local),
                        now: self.page_point(local),
                        trailing,
                        bottom,
                    });
                    self.grab = None;
                    *ctx.handled = true;
                    return;
                }
                if let Some(index) = self.place_under(local) {
                    let at = self.page_point(local);
                    let item = self.item_of(index);
                    self.select.fire(Some(Value::List(vec![
                        Value::Text(item),
                        Self::point_value(at),
                    ])));
                    let place = self.pins[index].1;
                    self.grab = place.moves.then_some(Grab {
                        index,
                        from: at,
                        place,
                        moving: false,
                    });
                    return;
                }
                self.grab = None;
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } if self.rect.contains(self.cursor) => {
                // A click on the page's own surface points there; a press that became a drag of
                // a place was a drag, not a click.
                self.grab = None;
                match self.place_under(local) {
                    None => {
                        self.press
                            .fire(Some(Self::point_value(self.page_point(local))));
                    }
                    // A second click on the same place opens it: the view puts the typing in it
                    // (a text box) or opens where it stands. A finger's double tap is two
                    // clicks too, as the window makes a tap a click.
                    Some(index) => {
                        if self.clicks.register(index) {
                            self.open.fire(Some(Value::Text(self.item_of(index))));
                        }
                    }
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } if self.rect.contains(self.cursor) => {
                let at = self.page_point(local);
                let under = self.place_under(local);
                let item = under.map_or_else(String::new, |index| self.item_of(index));
                self.context.fire(Some(Value::List(vec![
                    Value::Text(item),
                    Self::point_value(at),
                ])));
                // A place that names commands opens them where the press landed; the press is
                // the menu's, so it reaches no place behind it.
                if let Some(index) = under
                    && self.open_menu(index, self.cursor)
                {
                    *ctx.handled = true;
                }
            }
            _ => {}
        }
    }

    fn is_focusable(&self) -> bool {
        !self.still
    }

    /// A finger on a place that moves drags it, as a pointer does: a tap clicks, a held finger is
    /// the right press that opens the place's menu, and a drag moves the place past the tap's
    /// slop. Elsewhere the page is ordinary content: a tap clicks and a drag scrolls.
    fn touch_claim(&self, point: Point) -> crate::interaction::TouchClaim {
        use crate::interaction::TouchClaim;
        if !self.rect.contains(point) {
            return TouchClaim::Pointer;
        }
        let local = point - self.rect.origin().to_vec2();
        if self.edge_at(local).is_some() {
            return TouchClaim::Drag;
        }
        match self.place_under(local) {
            Some(index) if self.pins[index].1.moves => TouchClaim::Drag,
            _ => TouchClaim::Pointer,
        }
    }

    fn traps_focus(&self) -> bool {
        self.menu.is_open()
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &crate::a11y::Action,
        state: &mut crate::widget::Edits<'_>,
        path: &crate::widget::ModelPath,
    ) -> bool {
        let Some(target) = target.strip_prefix(MENU_TARGET) else {
            return false;
        };
        let performed = self
            .menu
            .perform_semantic_action(target, action, state, path);
        if performed {
            self.drain_menu_selection();
        }
        performed
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

    fn set_platform_conventions(&mut self, conventions: crate::platform::PlatformConventions) {
        self.platform = conventions;
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        use winit::window::CursorIcon;
        if !self.rect.contains(point) {
            return None;
        }
        let local = point - self.rect.origin().to_vec2();
        if !self.still
            && let Some((_, trailing, bottom)) = self.edge_at(local)
        {
            return Some(match (trailing, bottom) {
                (true, true) => CursorIcon::SeResize,
                (true, false) => CursorIcon::EResize,
                _ => CursorIcon::SResize,
            });
        }
        if self.grab.is_some_and(|grab| grab.moving) {
            return Some(CursorIcon::Grabbing);
        }
        if self
            .place_under(local)
            .is_some_and(|index| self.pins[index].1.moves)
        {
            return Some(CursorIcon::Grab);
        }
        None
    }

    fn accessibility(&self) -> Option<crate::a11y::AccessNode> {
        let mut node = crate::a11y::AccessNode::new(crate::a11y::Role::Group)
            .with_optional_name(self.name.clone());
        if let Some(menu) = self.menu_node() {
            node.children.push(menu);
        }
        Some(node)
    }
}

/// The prefix a place's menu targets carry, namespacing them into the stage's own semantics.
const MENU_TARGET: &str = "pin-menu:";

fn namespace_menu_targets(node: &mut crate::a11y::AccessNode) {
    if let Some(target) = node.action_target.take() {
        let target = format!("{MENU_TARGET}{target}");
        node.semantic_key = Some(target.clone());
        node.action_target = Some(target);
    }
    for child in &mut node.children {
        namespace_menu_targets(child);
    }
}

/// How far `low..high` must move to stand inside `min..max`: `0` when it already does.
fn in_viewport(low: f64, high: f64, min: f64, max: f64) -> f64 {
    if high - low > max - min || low < min {
        min - low
    } else if high > max {
        max - high
    } else {
        0.0
    }
}

/// A page coordinate as `f32`, for the layout solver.
#[expect(
    clippy::cast_possible_truncation,
    reason = "a page coordinate of a view, far inside f32"
)]
fn f32_of(v: f64) -> f32 {
    v as f32
}

#[cfg(test)]
#[path = "stage_tests.rs"]
mod tests;

