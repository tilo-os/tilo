//! The canvas: a workspace's windows anywhere on one 2D surface, and its one
//! gesture, point at something and start typing
//! (`docs/designs/app-language/canvas.md`).
//!
//! The arrangement (whether a workspace shows its canvas, and where each
//! window stands on it) is the workspace's, synced and undoable
//! ([`tilo_desktop_model::arrangement`]). What is here is this device's:
//! how far each person has panned and zoomed (their lens), the caret, a
//! selection being dragged out, and the answers on their way to a window.
//!
//! - **Free space:** a click on empty canvas puts the caret there; typing
//!   goes to it at once, and Enter asks the workspace's agent, with where it
//!   was asked and the windows near it.
//! - **Right-click:** the menu of what is under the pointer takes typing as
//!   every menu does; a sentence it does not match asks about that thing
//!   ([`ShellOverlayHost::open_ask_agent`] hands it here).
//! - **Drag to select:** a band over windows selects them; typing then asks
//!   about the selection.
//!
//! Every ask becomes a Tasks issue the agent works on, and its page opens as
//! a window placed at the place it was asked, or beside the thing it is
//! about, in free space: nothing already there moves. That pairing (the
//! window an answer opens and the anchor it belongs at) is
//! [`CanvasAnswers`], the one interface answers are placed through.

use std::collections::HashMap;
use std::time::Instant;

use tilo_desktop_model::{Arrangement, CANVAS_GAP, CanvasPlace, TileContent};
use ui::query_text::{QueryComposition, QueryInk, QueryView};
use ui::text_selection::Selection;
use ui::widget::CanvasView;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Vec2};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use super::ShellOverlayHost;
use crate::shell::selection_agent::SelectionSeed;
use crate::shell::state::{GroupId, WorkspaceId};

/// The size a window takes on the canvas when nothing said otherwise, in
/// canvas pixels.
pub(super) const WINDOW_SIZE: (u32, u32) = (720, 520);

/// How far from where it was asked the canvas looks for the windows an ask
/// is near, in canvas pixels.
const NEARBY: u32 = 900;

/// How much one wheel detent zooms.
const ZOOM_PER_DETENT: f64 = 1.1;

/// Pixels of a touchpad's scroll that make one wheel detent.
const PIXELS_PER_DETENT: f64 = ui::tokens::Layout::BASE.jump_px_per_detent as f64;

/// One person's lens on a workspace's canvas: the canvas point at the
/// deck's top-left corner and how many screen pixels a canvas pixel takes.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Lens {
    origin: Point,
    zoom: f64,
}

impl Default for Lens {
    fn default() -> Self {
        Self {
            origin: Point::ORIGIN,
            zoom: 1.0,
        }
    }
}

impl Lens {
    fn view(self, places: HashMap<u64, Rect>) -> CanvasView {
        CanvasView {
            origin: self.origin,
            zoom: self.zoom,
            places,
        }
    }
}

/// Where an answer belongs on the canvas.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum CanvasAnchor {
    /// At a point of free space, in canvas pixels: the window's corner goes
    /// there, or into the nearest free space.
    Point(Point),
    /// Beside a thing (a window, a card, a selection), in canvas pixels.
    Beside(Rect),
}

/// Answers on their way: each the app whose window will show it, and where
/// that window belongs. The next window of that app to open in the
/// workspace takes the oldest anchor waiting for it.
#[derive(Default)]
pub(crate) struct CanvasAnswers {
    waiting: Vec<(String, CanvasAnchor)>,
}

impl CanvasAnswers {
    /// An answer will open as a window of `app_id`; it belongs at `anchor`.
    pub(crate) fn expect(&mut self, app_id: &str, anchor: CanvasAnchor) {
        self.waiting.push((app_id.to_owned(), anchor));
    }

    /// A window of `app_id` opened: the anchor its answer belongs at, if one
    /// waits for it.
    pub(crate) fn claim(&mut self, app_id: &str) -> Option<CanvasAnchor> {
        let index = self.waiting.iter().position(|(app, _)| app == app_id)?;
        Some(self.waiting.remove(index).1)
    }

    /// Whether an answer waits for a window of `app_id`.
    pub(crate) fn waits_for(&self, app_id: &str) -> bool {
        self.waiting.iter().any(|(app, _)| app == app_id)
    }

    /// How many answers wait for their window.
    pub(crate) fn waiting(&self) -> usize {
        self.waiting.len()
    }
}

/// Why what was typed on the canvas did not reach an agent. Every ask
/// either opens its answer's window or comes back as one of these, which
/// the caret says where the person typed ([`Caret::refused`]): nothing
/// typed on the canvas goes unanswered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CanvasRefusal {
    /// Enter with nothing typed.
    NothingTyped,
    /// The workspace has no agent to ask.
    NoAgent,
    /// The workspace's agent, or the door to it, is not reachable yet: no
    /// action broker, no identity for the room, no workspace on screen.
    AgentUnreachable,
    /// The composer's path did not take the question.
    NotHanded,
}

impl CanvasRefusal {
    fn message_key(self) -> &'static str {
        match self {
            Self::NothingTyped => "canvas-ask-nothing-typed",
            Self::NoAgent => "canvas-ask-no-agent",
            Self::AgentUnreachable => "canvas-ask-agent-unreachable",
            Self::NotHanded => "canvas-ask-not-handed",
        }
    }

    /// What the caret says.
    fn message(self) -> String {
        crate::shell::i18n::t(self.message_key())
    }
}

/// The caret on the canvas and what is typed at it.
struct Caret {
    /// Where it stands, in canvas pixels.
    at: Point,
    text: String,
    selection: Selection,
    composition: QueryComposition,
    /// Why the last Enter did not reach an agent, said under the text until
    /// the person types again.
    refused: Option<CanvasRefusal>,
}

impl Caret {
    fn at(at: Point) -> Self {
        Self {
            at,
            text: String::new(),
            selection: Selection::collapsed(0),
            composition: QueryComposition::new(),
            refused: None,
        }
    }

    /// A caret at `at` holding `text`, which did not reach an agent for
    /// `why`.
    fn refused_at(at: Point, text: &str, why: CanvasRefusal) -> Self {
        Self {
            text: text.to_owned(),
            selection: Selection::collapsed(text.len()),
            refused: Some(why),
            ..Self::at(at)
        }
    }
}

/// A press on empty canvas, not yet a click or a drag.
#[derive(Clone, Copy, Debug)]
struct Press {
    /// Where it went down and where it is now, on screen.
    start: Point,
    now: Point,
    /// The middle button pans; the primary one selects.
    pans: bool,
    /// It travelled past the click slop: a drag.
    dragging: bool,
    /// The finger making it, for a touch; `None` for the pointer.
    touch: Option<u64>,
    /// When it went down: a finger held still this long is a right-click.
    since: Instant,
}

/// The canvas's state on this device.
#[derive(Default)]
pub(crate) struct CanvasLayer {
    lenses: HashMap<WorkspaceId, Lens>,
    /// The workspace whose canvas is on screen.
    shown: Option<WorkspaceId>,
    caret: Option<Caret>,
    press: Option<Press>,
    /// The windows a band selected, the subject of what is typed next.
    selection: Vec<GroupId>,
    pub(crate) answers: CanvasAnswers,
    /// The view the rows were given last.
    given: Option<CanvasView>,
    /// A pinch's scale at its last update, to zoom by what changed.
    pinch: Option<f64>,
}

/// The app a window shows, as its tile names it.
fn tile_app_id(tile: &TileContent) -> Option<&str> {
    match tile {
        TileContent::Foreign { app_id, .. }
        | TileContent::LocalApp { app_id, .. }
        | TileContent::PeerWindow { app_id, .. }
        | TileContent::HostedWindow { app_id, .. } => Some(app_id),
        TileContent::AppPlaceholder { .. } | TileContent::Stub { .. } => None,
    }
}

/// A canvas place as a rect, in canvas pixels.
fn place_rect(place: CanvasPlace) -> Rect {
    Rect::new(
        f64::from(place.x),
        f64::from(place.y),
        f64::from(place.x) + f64::from(place.width),
        f64::from(place.y) + f64::from(place.height),
    )
}

/// A rect in canvas pixels as a place, rounded to whole pixels.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "canvas coordinates are clamped into i32 and sizes into u32 first"
)]
fn rect_place(rect: Rect) -> CanvasPlace {
    let clamp = |v: f64| v.round().clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32;
    let side = |v: f64| v.round().clamp(1.0, f64::from(u32::MAX)) as u32;
    CanvasPlace::new(
        clamp(rect.x0),
        clamp(rect.y0),
        side(rect.width()),
        side(rect.height()),
    )
}

#[allow(
    clippy::cast_possible_truncation,
    reason = "canvas coordinates are clamped into i32 first"
)]
fn canvas_point(p: Point) -> (i32, i32) {
    let clamp = |v: f64| v.round().clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32;
    (clamp(p.x), clamp(p.y))
}

/// `read` of the active workspace, holding the tree only while it reads.
fn read_workspace<T>(
    chrome: &crate::shell::chrome_state::Chrome,
    read: impl FnOnce(&crate::shell::state::Workspace) -> T,
) -> Option<T> {
    let state = chrome.borrow();
    let tree = state.tree();
    let value = tree.desktop.active_workspace().map(read);
    drop(tree);
    drop(state);
    value
}

/// Client windows of the active workspace keep their canvas size and are
/// drawn at `zoom` ([`crate::shell::foreign::ForeignFrameStore::set_canvas_zoom`]).
fn zoom_canvas_windows(chrome: &crate::shell::chrome_state::Chrome, zoom: f64) {
    let surfaces = read_workspace(chrome, |workspace| {
        workspace
            .strips
            .iter()
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .filter_map(|tab| match tab.tile {
                TileContent::Foreign { surface_id, .. } => Some(surface_id),
                _ => None,
            })
            .collect::<Vec<_>>()
    })
    .unwrap_or_default();
    let store = crate::shell::foreign::store();
    for surface in surfaces {
        store.set_canvas_zoom(surface, zoom);
    }
}

/// One window on the canvas as the agent is told of it.
struct NamedWindow {
    group: GroupId,
    app_id: Option<String>,
    title: String,
}

impl ShellOverlayHost {
    /// Whether the active workspace shows its canvas.
    pub fn canvas_shown(&self) -> bool {
        self.canvas.shown.is_some()
    }

    /// Whether the caret on the canvas has the keyboard.
    pub fn canvas_takes_keys(&self) -> bool {
        self.canvas.shown.is_some() && self.canvas.caret.is_some()
    }

    /// The caret as the input method serves it: a text field where the
    /// caret is drawn, so a finger's tap raises the on-screen keyboard.
    pub(super) fn canvas_ime_field(&self) -> Option<ui::ime::ImeWindowState> {
        if !self.canvas_takes_keys() {
            return None;
        }
        let caret = self.canvas.caret.as_ref()?;
        let rect = caret.composition.ime_cursor_rect().or_else(|| {
            self.to_screen(caret.at)
                .map(|p| Rect::from_center_size(p, (2.0, 20.0)))
        })?;
        Some(ui::ime::ImeWindowState::from_rect(
            ui::ime::InputContent::TEXT,
            rect,
        ))
    }

    /// What the input method may compare the caret's text by.
    pub(super) fn canvas_ime_edit_fingerprint(&self, key: &[u8; 32]) -> Option<[u8; 32]> {
        let caret = self.canvas.caret.as_ref()?;
        caret
            .composition
            .ime_edit_fingerprint(&caret.text, &caret.selection, key)
    }

    /// The deck's corner and size on screen: where the canvas is drawn.
    fn canvas_frame(&self) -> Option<Rect> {
        let rect = self.desktop_surface()?.strip_deck()?.rect();
        (rect.width() > 0.0 && rect.height() > 0.0).then_some(rect)
    }

    fn canvas_lens(&self) -> Lens {
        self.canvas
            .shown
            .and_then(|workspace| self.canvas.lenses.get(&workspace).copied())
            .unwrap_or_default()
    }

    /// The canvas point under screen point `p`.
    fn to_canvas(&self, p: Point) -> Option<Point> {
        let frame = self.canvas_frame()?;
        let lens = self.canvas_lens();
        Some(lens.view(HashMap::new()).to_canvas(frame.origin(), p))
    }

    /// Canvas point `p` on screen.
    fn to_screen(&self, p: Point) -> Option<Point> {
        let frame = self.canvas_frame()?;
        let lens = self.canvas_lens();
        Some(lens.view(HashMap::new()).to_screen(frame.origin(), p))
    }

    /// The window drawn at screen point `p` on the canvas.
    fn canvas_window_at(&self, p: Point) -> Option<GroupId> {
        self.group_row_ptrs.values().find_map(|&ptr| {
            // SAFETY: read-only use under the field's stable-row contract.
            unsafe { &*ptr }.canvas_group_at(p)
        })
    }

    /// Bring the rows to the active workspace's arrangement: on its canvas,
    /// each window where it stands (a window with no place yet is given
    /// one, an answer's at its anchor), through this device's lens; or back
    /// in their strips. Whether anything changed.
    pub(super) fn sync_canvas(&mut self) -> bool {
        let Some(chrome) = self.chrome.clone() else {
            return false;
        };
        let Some((workspace, arrangement, windows)) = read_workspace(&chrome, |workspace| {
            let windows: Vec<(GroupId, Option<String>)> = workspace
                .strips
                .iter()
                .flat_map(|strip| &strip.window_groups)
                .map(|group| {
                    let app = group
                        .tabs
                        .iter()
                        .find(|tab| tab.id == group.active_tab)
                        .or_else(|| group.tabs.first())
                        .and_then(|tab| tile_app_id(&tab.tile))
                        .map(str::to_owned);
                    (group.id, app)
                })
                .collect();
            (workspace.id, workspace.arrangement.clone(), windows)
        }) else {
            return false;
        };
        if !arrangement.is_canvas() {
            return self.leave_canvas();
        }
        let entered = self.canvas.shown != Some(workspace);
        if entered {
            self.canvas.caret = None;
            self.canvas.press = None;
            self.canvas.selection.clear();
            // What hung from the windows where they stood (a bar's menu, a
            // flyout) would hang over nothing now.
            self.dismiss_popovers();
        }
        self.canvas.shown = Some(workspace);
        let mut arrangement = arrangement;
        let newly_placed = self.place_new_windows(&chrome, &mut arrangement, &windows);
        let lens = self.canvas_lens();
        let places: HashMap<u64, Rect> = arrangement
            .places
            .iter()
            .map(|(group, place)| (*group, place_rect(*place)))
            .collect();
        let view = lens.view(places);
        let changed = entered || newly_placed || self.canvas.given.as_ref() != Some(&view);
        if let Some(deck) = self
            .desktop_surface_mut()
            .and_then(tilo_desktop::composition::DesktopSurface::strip_deck_mut)
        {
            deck.set_canvas(true);
        }
        for &ptr in self.group_row_ptrs.values() {
            // SAFETY: rows are visited one at a time under the field's
            // stable-pointer contract; `&mut self` makes it exclusive.
            unsafe { &mut *ptr }.set_canvas(Some(view.clone()));
        }
        zoom_canvas_windows(&chrome, lens.zoom);
        self.canvas.given = Some(view);
        if changed {
            self.bump_chrome();
        }
        changed
    }

    /// The active workspace shows its strips: the rows go back to them.
    fn leave_canvas(&mut self) -> bool {
        if self.canvas.shown.take().is_none() {
            return false;
        }
        self.canvas.caret = None;
        self.canvas.press = None;
        self.canvas.selection.clear();
        self.canvas.given = None;
        self.dismiss_popovers();
        if let Some(deck) = self
            .desktop_surface_mut()
            .and_then(tilo_desktop::composition::DesktopSurface::strip_deck_mut)
        {
            deck.set_canvas(false);
        }
        for &ptr in self.group_row_ptrs.values() {
            // SAFETY: as in `sync_canvas`.
            unsafe { &mut *ptr }.set_canvas(None);
        }
        if let Some(chrome) = self.chrome.clone() {
            zoom_canvas_windows(&chrome, 1.0);
        }
        self.bump_chrome();
        true
    }

    /// Give every window with no place on the canvas one: an answer's at
    /// the anchor it was asked at, any other beside the window in front,
    /// else in the middle of what is on screen. Nothing placed moves.
    /// Whether any was placed.
    fn place_new_windows(
        &mut self,
        chrome: &crate::shell::chrome_state::Chrome,
        arrangement: &mut Arrangement,
        windows: &[(GroupId, Option<String>)],
    ) -> bool {
        let (width, height) = WINDOW_SIZE;
        let mut placed = false;
        // A workspace that just came to its canvas: its windows, in strip
        // order, share what is on screen, so all of them show at once.
        if arrangement.places.is_empty() {
            let unanchored: Vec<GroupId> = windows
                .iter()
                .filter(|(_, app)| {
                    app.as_deref()
                        .is_none_or(|app| !self.canvas.answers.waits_for(app))
                })
                .map(|(group, _)| *group)
                .collect();
            let Some(grid) = self.on_screen_grid(unanchored.len()) else {
                // Not laid out yet: placed once the canvas has a size.
                return false;
            };
            for (group, place) in unanchored.into_iter().zip(grid) {
                if chrome.borrow_mut().place_on_canvas(group, place).is_ok() {
                    arrangement.places.insert(group, place);
                    placed = true;
                }
            }
        }
        for (group, app) in windows {
            if arrangement.places.contains_key(group) {
                continue;
            }
            let anchor = app
                .as_deref()
                .and_then(|app| self.canvas.answers.claim(app));
            let place = match anchor {
                Some(CanvasAnchor::Point(at)) => {
                    arrangement.free_place_near(canvas_point(at), width, height, None)
                }
                Some(CanvasAnchor::Beside(subject)) => {
                    arrangement.free_place_beside(rect_place(subject), width, height, None)
                }
                None => self.default_place(chrome, arrangement, width, height),
            };
            if let Err(error) = chrome.borrow_mut().place_on_canvas(*group, place) {
                tracing::warn!(group, %error, "a window could not be placed on the canvas");
                continue;
            }
            arrangement.places.insert(*group, place);
            placed = true;
        }
        placed
    }

    /// `count` places in rows and columns over what is on screen, a gap
    /// apart and at most [`WINDOW_SIZE`] each, in reading order; `None`
    /// while the canvas has no size yet.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        reason = "a handful of windows, and sizes clamped to the screen before the casts"
    )]
    fn on_screen_grid(&self, count: usize) -> Option<Vec<CanvasPlace>> {
        if count == 0 {
            return Some(Vec::new());
        }
        let frame = self.canvas_frame()?;
        let lens = self.canvas_lens();
        let zoom = lens.zoom.max(f64::EPSILON);
        let (width, height) = (frame.width() / zoom, frame.height() / zoom);
        let gap = f64::from(CANVAS_GAP);
        let columns = (count as f64).sqrt().ceil().max(1.0);
        let rows = (count as f64 / columns).ceil().max(1.0);
        let cell = |space: f64, n: f64, most: u32, least: f64| {
            ((space - gap * (n + 1.0)) / n).clamp(least, f64::from(most))
        };
        let cell_w = cell(width, columns, WINDOW_SIZE.0, 320.0);
        let cell_h = cell(height, rows, WINDOW_SIZE.1, 240.0);
        Some(
            (0..count)
                .map(|i| {
                    let (column, row) = ((i as f64) % columns, ((i as f64) / columns).floor());
                    let x = lens.origin.x + gap + column * (cell_w + gap);
                    let y = lens.origin.y + gap + row * (cell_h + gap);
                    CanvasPlace::new(
                        canvas_point(Point::new(x, y)).0,
                        canvas_point(Point::new(x, y)).1,
                        cell_w.round() as u32,
                        cell_h.round() as u32,
                    )
                })
                .collect(),
        )
    }

    /// Where a window nobody placed goes: beside the window in front, else
    /// centred in what is on screen.
    fn default_place(
        &self,
        chrome: &crate::shell::chrome_state::Chrome,
        arrangement: &Arrangement,
        width: u32,
        height: u32,
    ) -> CanvasPlace {
        let front = {
            let state = chrome.borrow();
            let tree = state.tree();
            tree.desktop
                .active_workspace()
                .and_then(|workspace| {
                    let strip = tree.desktop.active_strip.get(&workspace.id)?;
                    tree.desktop.active_group.get(strip).copied()
                })
                .and_then(|group| arrangement.place_of(group))
        };
        if let Some(front) = front {
            return arrangement.free_place_beside(front, width, height, None);
        }
        let middle = self
            .canvas_frame()
            .and_then(|frame| self.to_canvas(frame.center()))
            .unwrap_or_default();
        let corner = Point::new(
            middle.x - f64::from(width) / 2.0,
            middle.y - f64::from(height) / 2.0,
        );
        arrangement.free_place_near(canvas_point(corner), width, height, None)
    }

    /// The windows the person carried to new places on the canvas, kept.
    pub(super) fn harvest_pending_canvas_moves(&self) {
        let moves: Vec<(u64, Rect)> = self
            .group_row_ptrs
            .values()
            .filter_map(|&ptr| {
                // SAFETY: as in `sync_canvas`.
                unsafe { &mut *ptr }.take_pending_canvas_move()
            })
            .collect();
        let Some(chrome) = self.chrome.clone() else {
            return;
        };
        for (group, rect) in moves {
            if let Err(error) = chrome.borrow_mut().place_on_canvas(group, rect_place(rect)) {
                tracing::warn!(group, %error, "a window carried on the canvas could not be kept there");
            }
        }
    }

    /// Pan the canvas by `delta` screen pixels.
    fn pan_canvas(&mut self, delta: Vec2) {
        let Some(workspace) = self.canvas.shown else {
            return;
        };
        let lens = self.canvas.lenses.entry(workspace).or_default();
        let mut view = lens.view(HashMap::new());
        view.pan_by(delta);
        lens.origin = view.origin;
        self.bump_chrome();
    }

    /// Zoom the canvas by `factor` about screen point `about`.
    fn zoom_canvas(&mut self, about: Point, factor: f64) {
        let (Some(workspace), Some(frame)) = (self.canvas.shown, self.canvas_frame()) else {
            return;
        };
        let lens = self.canvas.lenses.entry(workspace).or_default();
        let mut view = lens.view(HashMap::new());
        view.zoom_about(frame.origin(), about, factor);
        lens.origin = view.origin;
        lens.zoom = view.zoom;
        self.bump_chrome();
    }

    /// Two fingers on the canvas pan it, or pinch it to zoom: `scale` is
    /// the pinch's since the fingers landed. Whether the canvas took it.
    pub(super) fn canvas_touch_gesture(
        &mut self,
        gesture: &ui::gesture::TouchGestureEvent,
    ) -> bool {
        use ui::gesture::TouchGestureEvent;
        if !self.canvas_shown() {
            return false;
        }
        match *gesture {
            TouchGestureEvent::Pan { fingers: 2, delta } => {
                self.canvas.pinch = None;
                self.pan_canvas(Vec2::new(delta.0, delta.1));
                true
            }
            TouchGestureEvent::Pinch {
                fingers: 2,
                center,
                scale,
            } => {
                let last = self.canvas.pinch.replace(scale).unwrap_or(1.0);
                if last > 0.0 {
                    self.zoom_canvas(center, scale / last);
                }
                true
            }
            _ => false,
        }
    }

    /// Input on the canvas that no window and no chrome took: the caret, the
    /// band, panning and zooming, and typing at the caret. Whether the
    /// canvas took it.
    pub(super) fn route_canvas_event(
        &mut self,
        event: &WindowEvent,
        modifiers: winit::event::Modifiers,
        scale: f64,
    ) -> bool {
        if !self.canvas_shown() {
            return false;
        }
        match event {
            WindowEvent::Touch(touch) => self.canvas_touch(touch, scale),
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button,
                ..
            } => self.canvas_press(*button),
            WindowEvent::CursorMoved { .. } => self.canvas_pointer_moved(),
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left | MouseButton::Middle,
                ..
            } => self.canvas_release(),
            WindowEvent::MouseWheel { delta, .. } => self.canvas_wheel(*delta, modifiers),
            WindowEvent::KeyboardInput { .. } | WindowEvent::Ime(_) => {
                self.canvas_type(event, modifiers)
            }
            _ => false,
        }
    }

    /// Whether screen point `p` is empty canvas: on the canvas, on no window.
    pub(super) fn canvas_on_empty_space(&self, p: Point) -> bool {
        self.canvas_shown()
            && self.canvas_frame().is_some_and(|frame| frame.contains(p))
            && self.canvas_window_at(p).is_none()
    }

    /// A press landed at `p`, whoever takes it: on a window, the caret and
    /// the selection give way to it.
    pub(super) fn canvas_press_on_window(&mut self, p: Point) {
        if !self.canvas_shown() || self.canvas_window_at(p).is_none() {
            return;
        }
        let had = self.canvas.caret.take().is_some();
        let selected = !self.canvas.selection.is_empty();
        self.canvas.selection.clear();
        if had || selected {
            self.bump_chrome();
        }
    }

    fn canvas_press(&mut self, button: MouseButton) -> bool {
        let Some(at) = self.last_cursor else {
            return false;
        };
        let inside = self.canvas_frame().is_some_and(|frame| frame.contains(at));
        if !inside {
            return false;
        }
        if self.canvas_window_at(at).is_some() {
            // A window takes its own clicks; the caret and the selection
            // give way to it.
            if self.canvas.caret.take().is_some() || !self.canvas.selection.is_empty() {
                self.canvas.selection.clear();
                self.bump_chrome();
            }
            return false;
        }
        match button {
            MouseButton::Left | MouseButton::Middle => {
                self.canvas.press = Some(Press {
                    start: at,
                    now: at,
                    pans: button == MouseButton::Middle,
                    dragging: false,
                    touch: None,
                    since: Instant::now(),
                });
                true
            }
            _ => false,
        }
    }

    /// A finger on empty canvas: a tap is the caret, a drag a band, a still
    /// hold the menu a right-click opens. Two fingers are a pan or a pinch
    /// ([`Self::canvas_touch_gesture`]).
    fn canvas_touch(&mut self, touch: &winit::event::Touch, scale: f64) -> bool {
        use winit::event::TouchPhase;
        let at = Point::new(touch.location.x / scale, touch.location.y / scale);
        match touch.phase {
            TouchPhase::Started => {
                if self.canvas.press.is_some() {
                    // A second finger: a pan or a pinch, never a band.
                    self.canvas.press = None;
                    self.bump_chrome();
                    return true;
                }
                if !self.canvas_on_empty_space(at) {
                    return false;
                }
                self.canvas.pinch = None;
                self.canvas.press = Some(Press {
                    start: at,
                    now: at,
                    pans: false,
                    dragging: false,
                    touch: Some(touch.id),
                    since: Instant::now(),
                });
                true
            }
            TouchPhase::Moved => {
                if self
                    .canvas
                    .press
                    .is_none_or(|press| press.touch != Some(touch.id))
                {
                    return false;
                }
                self.canvas_moved_to(at, ui::gesture::TouchGestureArena::TAP_SLOP)
            }
            TouchPhase::Ended => {
                if self
                    .canvas
                    .press
                    .is_none_or(|press| press.touch != Some(touch.id))
                {
                    return false;
                }
                self.canvas_release()
            }
            TouchPhase::Cancelled => self.canvas.press.take().is_some(),
        }
    }

    /// When a finger held still on empty canvas becomes a right-click.
    pub(super) fn canvas_deadline(&self) -> Option<Instant> {
        let press = self
            .canvas
            .press
            .filter(|p| p.touch.is_some() && !p.dragging)?;
        press
            .since
            .checked_add(ui::interaction::TouchPointerEmulator::LONG_PRESS)
    }

    /// A finger held still on empty canvas long enough opens its menu, as
    /// a right-click there does. Whether it did.
    pub(super) fn fire_canvas_long_press(&mut self, now: Instant) -> bool {
        if self.canvas_deadline().is_none_or(|due| now < due) {
            return false;
        }
        let Some(press) = self.canvas.press.take() else {
            return false;
        };
        match self.thing_at(press.start) {
            Some(seed) => self.open_thing_menu(seed, press.start),
            None => false,
        }
    }

    fn canvas_pointer_moved(&mut self) -> bool {
        let Some(at) = self.last_cursor else {
            return false;
        };
        if self.canvas.press.is_none_or(|press| press.touch.is_some()) {
            return false;
        }
        self.canvas_moved_to(at, 4.0)
    }

    /// The press on empty canvas moved to `at`: past `slop` it is a drag
    /// that pans or draws a band.
    fn canvas_moved_to(&mut self, at: Point, slop: f64) -> bool {
        let Some(mut press) = self.canvas.press else {
            return false;
        };
        let travelled = Vec2::new(at.x - press.now.x, at.y - press.now.y);
        press.now = at;
        if !press.dragging && at.distance(press.start) > slop {
            press.dragging = true;
            if !press.pans {
                self.canvas.caret = None;
            }
        }
        self.canvas.press = Some(press);
        if press.dragging {
            if press.pans {
                self.pan_canvas(travelled);
            } else {
                self.bump_chrome();
            }
        }
        true
    }

    fn canvas_release(&mut self) -> bool {
        let Some(press) = self.canvas.press.take() else {
            return false;
        };
        if press.pans {
            return true;
        }
        if press.dragging {
            let band = Rect::from_points(press.start, press.now);
            self.select_band(band);
        } else if let Some(at) = self.to_canvas(press.start) {
            self.canvas.selection.clear();
            self.canvas.caret = Some(Caret::at(at));
        }
        self.bump_chrome();
        true
    }

    /// Select the windows a band drawn over `band` (on screen) touches, and
    /// put the caret beside them for what is typed about them.
    fn select_band(&mut self, band: Rect) {
        let (Some(a), Some(b)) = (
            self.to_canvas(band.origin()),
            self.to_canvas(Point::new(band.x1, band.y1)),
        ) else {
            return;
        };
        let area = rect_place(Rect::from_points(a, b));
        let selected = self
            .chrome
            .as_ref()
            .and_then(|chrome| {
                let state = chrome.borrow();
                let tree = state.tree();
                tree.desktop
                    .active_workspace()
                    .map(|workspace| workspace.arrangement.within(area))
            })
            .unwrap_or_default();
        self.canvas.selection = selected;
        let bounds = self.selection_bounds();
        // Typing about the selection starts under it.
        let caret = bounds.map_or(b, |bounds| {
            Point::new(bounds.x0, bounds.y1 + f64::from(CANVAS_GAP))
        });
        self.canvas.caret = Some(Caret::at(caret));
    }

    /// The selection's bounds, in canvas pixels.
    fn selection_bounds(&self) -> Option<Rect> {
        let chrome = self.chrome.as_ref()?;
        let state = chrome.borrow();
        let tree = state.tree();
        tree.desktop
            .active_workspace()?
            .arrangement
            .bounds_of(&self.canvas.selection)
            .map(place_rect)
    }

    fn canvas_wheel(
        &mut self,
        delta: MouseScrollDelta,
        modifiers: winit::event::Modifiers,
    ) -> bool {
        let Some(at) = self.last_cursor else {
            return false;
        };
        if !self.canvas_frame().is_some_and(|frame| frame.contains(at))
            || self.canvas_window_at(at).is_some()
        {
            return false;
        }
        let (dx, dy) = match delta {
            MouseScrollDelta::LineDelta(x, y) => (
                f64::from(x) * PIXELS_PER_DETENT,
                f64::from(y) * PIXELS_PER_DETENT,
            ),
            MouseScrollDelta::PixelDelta(p) => (p.x, p.y),
        };
        if modifiers.state().control_key() {
            let detents = dy / PIXELS_PER_DETENT;
            self.zoom_canvas(at, ZOOM_PER_DETENT.powf(detents));
        } else {
            self.pan_canvas(Vec2::new(dx, dy));
        }
        true
    }

    /// A key or an input method's text at the caret. Enter asks, Escape
    /// puts the caret away. Whether the caret took it.
    fn canvas_type(&mut self, event: &WindowEvent, modifiers: winit::event::Modifiers) -> bool {
        if self.canvas.caret.is_none() {
            return false;
        }
        if let WindowEvent::KeyboardInput {
            event: key_event, ..
        } = event
            && key_event.state == ElementState::Pressed
        {
            match &key_event.logical_key {
                Key::Named(NamedKey::Escape) => {
                    self.canvas.caret = None;
                    self.canvas.selection.clear();
                    self.bump_chrome();
                    return true;
                }
                Key::Named(NamedKey::Enter) => {
                    self.ask_at_caret();
                    return true;
                }
                _ => {}
            }
        }
        let command = modifiers.state().control_key() || modifiers.state().super_key();
        let shift = modifiers.state().shift_key();
        let Some(caret) = self.canvas.caret.as_mut() else {
            return false;
        };
        let change = caret.composition.handle_event(
            event,
            &mut caret.text,
            &mut caret.selection,
            shift,
            command,
        );
        if change == ui::query_text::QueryEventChange::Ignored {
            // Keys a caret has no use for (a shortcut) still are not the
            // window's that lost the keyboard to the caret.
            return matches!(event, WindowEvent::KeyboardInput { .. });
        }
        // What the caret said about the last Enter was about other words.
        caret.refused = None;
        self.bump_chrome();
        true
    }

    /// Ask what is typed at the caret: about the selection when there is
    /// one, else about the place it was typed at. The caret goes once the
    /// question is with the agent (its answer opens where it was); else it
    /// stays, and says why.
    fn ask_at_caret(&mut self) {
        let Some(caret) = self.canvas.caret.as_ref() else {
            return;
        };
        let text = caret.text.trim().to_owned();
        let at = caret.at;
        let outcome = if text.is_empty() {
            Err(CanvasRefusal::NothingTyped)
        } else {
            self.ask_typed_at_caret(at, &text)
        };
        match outcome {
            Ok(()) => self.canvas.caret = None,
            Err(why) => {
                tracing::info!(
                    ?why,
                    "a question typed on the canvas did not reach an agent"
                );
                if let Some(caret) = self.canvas.caret.as_mut() {
                    caret.refused = Some(why);
                }
            }
        }
        self.bump_chrome();
    }

    /// Hand `text`, typed at the caret at `at`, to the agent.
    fn ask_typed_at_caret(&mut self, at: Point, text: &str) -> Result<(), CanvasRefusal> {
        let selection = std::mem::take(&mut self.canvas.selection);
        let label = self.app_label(tilo_desktop_model::commands::APP_ID);
        let mut seed = SelectionSeed::new(tilo_desktop_model::commands::APP_ID, label, None);
        let anchor = if selection.is_empty() {
            seed = seed.drawn_at(
                self.to_screen(at)
                    .map(|p| Rect::from_center_size(p, (1.0, 1.0))),
            );
            CanvasAnchor::Point(at)
        } else {
            let bounds = {
                let chrome = self.chrome.as_ref();
                chrome.and_then(|chrome| {
                    let state = chrome.borrow();
                    let tree = state.tree();
                    tree.desktop
                        .active_workspace()?
                        .arrangement
                        .bounds_of(&selection)
                        .map(place_rect)
                })
            };
            match bounds {
                Some(bounds) => CanvasAnchor::Beside(bounds),
                None => CanvasAnchor::Point(at),
            }
        };
        let context = self.canvas_context(anchor, &selection);
        let asked = self.ask_on_canvas(&seed.asking(text), anchor, &context);
        if asked.is_err() {
            // The selection stays the subject of the next try.
            self.canvas.selection = selection;
        }
        asked
    }

    /// What the agent is told of where it was asked: the windows near the
    /// place, or the windows selected.
    fn canvas_context(&self, anchor: CanvasAnchor, selection: &[GroupId]) -> String {
        let windows = self.named_windows();
        let named = |group: &GroupId| {
            windows
                .iter()
                .find(|w| w.group == *group)
                .map(|w| match &w.app_id {
                    Some(app) => format!("- {} ({}, {app})", w.title, self.app_label(app)),
                    None => format!("- {}", w.title),
                })
        };
        let arrangement = self
            .chrome
            .as_ref()
            .and_then(|chrome| {
                let state = chrome.borrow();
                let tree = state.tree();
                tree.desktop
                    .active_workspace()
                    .map(|workspace| workspace.arrangement.clone())
            })
            .unwrap_or_default();
        if !selection.is_empty() {
            let lines: Vec<String> = selection.iter().filter_map(named).collect();
            return format!(
                "Asked on the workspace's canvas about these windows, selected there:\n{}",
                lines.join("\n")
            );
        }
        let (point, beside) = match anchor {
            CanvasAnchor::Point(at) => (canvas_point(at), None),
            CanvasAnchor::Beside(rect) => (canvas_point(rect.origin()), Some(rect)),
        };
        let near: Vec<String> = arrangement
            .near(point, NEARBY)
            .iter()
            .filter_map(named)
            .collect();
        let place = match beside {
            Some(rect) => format!("beside something at x {:.0}, y {:.0}", rect.x0, rect.y0),
            None => format!("in free space at x {}, y {}", point.0, point.1),
        };
        if near.is_empty() {
            format!(
                "Asked on the workspace's canvas, {place}. No windows are near it. The answer \
                 opens there as a window."
            )
        } else {
            format!(
                "Asked on the workspace's canvas, {place}. The answer opens there as a window. \
                 Windows near it, nearest first:\n{}",
                near.join("\n")
            )
        }
    }

    /// The windows of the active workspace, each by the tab in front.
    fn named_windows(&self) -> Vec<NamedWindow> {
        let Some(chrome) = self.chrome.as_ref() else {
            return Vec::new();
        };
        read_workspace(chrome, |workspace| {
            workspace
                .strips
                .iter()
                .flat_map(|strip| &strip.window_groups)
                .filter_map(|group| {
                    let tab = group
                        .tabs
                        .iter()
                        .find(|tab| tab.id == group.active_tab)
                        .or_else(|| group.tabs.first())?;
                    Some(NamedWindow {
                        group: group.id,
                        app_id: tile_app_id(&tab.tile).map(str::to_owned),
                        title: tab.title.clone(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
    }

    /// An ask from the canvas, or from a menu on something on it, while the
    /// canvas shows: the anchor it belongs at is where the person pointed,
    /// or beside what they pointed at. Whether it was handed to the agent.
    pub(super) fn ask_about_on_canvas(&mut self, seed: &SelectionSeed) -> bool {
        let anchor = match seed.area {
            Some(area) if area.width() > 2.0 || area.height() > 2.0 => {
                let (Some(a), Some(b)) = (
                    self.to_canvas(area.origin()),
                    self.to_canvas(Point::new(area.x1, area.y1)),
                ) else {
                    return false;
                };
                CanvasAnchor::Beside(Rect::from_points(a, b))
            }
            Some(area) => match self.to_canvas(area.center()) {
                Some(at) => self
                    .beside_window_at(area.center())
                    .unwrap_or(CanvasAnchor::Point(at)),
                None => return false,
            },
            None => match self.last_cursor.and_then(|p| self.beside_window_at(p)) {
                Some(anchor) => anchor,
                None => match self.last_cursor.and_then(|p| self.to_canvas(p)) {
                    Some(at) => CanvasAnchor::Point(at),
                    None => return false,
                },
            },
        };
        let context = self.canvas_context(anchor, &[]);
        match self.ask_on_canvas(seed, anchor, &context) {
            Ok(()) => true,
            Err(why) => {
                // Said where the person pointed: a caret there holds what
                // they typed, and why it did not reach an agent.
                tracing::info!(
                    ?why,
                    "a question asked from a menu on the canvas did not reach an agent"
                );
                let at = match anchor {
                    CanvasAnchor::Point(at) => at,
                    CanvasAnchor::Beside(rect) => {
                        Point::new(rect.x0, rect.y1 + f64::from(CANVAS_GAP))
                    }
                };
                let text = seed.ask.clone().unwrap_or_default();
                self.canvas.selection.clear();
                self.canvas.caret = Some(Caret::refused_at(at, &text, why));
                self.bump_chrome();
                true
            }
        }
    }

    /// Beside the window drawn at screen point `p`, when there is one.
    fn beside_window_at(&self, p: Point) -> Option<CanvasAnchor> {
        let group = self.canvas_window_at(p)?;
        let chrome = self.chrome.as_ref()?;
        let place = read_workspace(chrome, |workspace| workspace.arrangement.place_of(group))??;
        Some(CanvasAnchor::Beside(place_rect(place)))
    }

    /// Hand `seed`'s question to the workspace's agent as a Tasks issue
    /// whose page opens as a window at `anchor`, through the composer's
    /// path. An agent not running is started; the issue waits for it.
    /// When it could not be asked, why.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the session source and the composer's actions are handed on at their last use; \
                  the lint reads the move into `open_and_ask` as a held guard"
    )]
    fn ask_on_canvas(
        &mut self,
        seed: &SelectionSeed,
        anchor: CanvasAnchor,
        context: &str,
    ) -> Result<(), CanvasRefusal> {
        use crate::shell::state::WorkspaceAuthority as _;
        let Some(text) = seed.ask.clone().filter(|text| !text.trim().is_empty()) else {
            return Err(CanvasRefusal::NothingTyped);
        };
        let Some(chrome) = self.chrome.clone() else {
            return Err(CanvasRefusal::AgentUnreachable);
        };
        let Some((room_id, person, members)) = ({
            let state = chrome.borrow();
            let tree = state.tree();
            tree.desktop.active_workspace().map(|workspace| {
                (
                    workspace.room(),
                    state.active_local_user_in(&tree),
                    workspace
                        .agent_members()
                        .into_iter()
                        .map(|(agent, _)| agent)
                        .collect::<Vec<_>>(),
                )
            })
        }) else {
            return Err(CanvasRefusal::AgentUnreachable);
        };
        let room = room_id.to_string();
        let Some(agent) = self.selection_requests.choose_agent(&room, &members) else {
            return Err(CanvasRefusal::NoAgent);
        };
        let Some(broker) = self.shortcut_triggers.broker() else {
            return Err(CanvasRefusal::AgentUnreachable);
        };
        let live = super::ask_agent::askable(&broker, &room, &agent);
        let source = crate::shell::certified_action_session::ActionSessionSource::new(broker);
        let Some(identity_revision) = source.identity_revision(&room) else {
            return Err(CanvasRefusal::AgentUnreachable);
        };
        let owner = crate::shell::agent_quick_composer::ComposerOwner {
            room: room.clone(),
            agent: agent.clone(),
            person,
            identity_revision,
        };
        let body = format!(
            "{}{}\n<<<\n{}\n>>>",
            seed.instruction(&text),
            runtime_collab::INSTRUCTION_CONTEXT,
            context.trim()
        );
        let (start, asked) = {
            let actions = self.agent_actions.entry(owner.clone()).or_default();
            let start = !live && actions.start_once(Instant::now());
            let asked =
                actions.open_and_ask(source, &owner, (&text, &body), Some(seed), true, true);
            (start, asked)
        };
        if !asked {
            tracing::warn!(%agent, "the canvas's question could not be handed to the agent");
            return Err(CanvasRefusal::NotHanded);
        }
        self.selection_requests.asked(&room, &agent);
        self.canvas
            .answers
            .expect(tilo_action_broker::services::TRACKER_APP, anchor);
        if start {
            self.start_agent_for(&room_id, &agent);
        }
        tracing::info!(%agent, ?anchor, "asked from the canvas");
        Ok(())
    }

    /// The canvas's marks over its windows: the band being drawn, the
    /// selection, and the caret with what is typed at it.
    pub(super) fn draw_canvas_marks(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut ui::text_render::TextRenderer,
        theme: &ui::Theme,
    ) {
        if !self.canvas_shown() {
            return;
        }
        if let Some(press) = self.canvas.press.filter(|p| p.dragging && !p.pans) {
            let band = Rect::from_points(press.start, press.now);
            ui::chrome::paint_selection_band(scene, transform, band, theme);
        }
        let radius = ui::widget::primitives::SurfaceLevel::Window.radius(theme);
        let selected: Vec<Rect> = self
            .group_row_ptrs
            .values()
            .flat_map(|&ptr| {
                // SAFETY: read-only use under the field's stable-row contract.
                let row = unsafe { &*ptr };
                row.groups
                    .iter()
                    .filter(|group| self.canvas.selection.contains(&group.id))
                    .map(ui::Widget::get_rect)
                    .collect::<Vec<_>>()
            })
            .collect();
        for rect in selected {
            ui::chrome::paint_selected_ring(scene, transform, rect, radius, theme);
        }
        let Some(at) = self
            .canvas
            .caret
            .as_ref()
            .and_then(|caret| self.to_screen(caret.at))
        else {
            return;
        };
        let frame = self.canvas_frame().unwrap_or(self.rect);
        let row = f64::from(theme.tokens.metrics.menu_row_height);
        let inset = ui::widget::primitives::FloatingPanelFace::slot_inset(theme);
        let gap = f64::from(CANVAS_GAP);
        // As wide as a sheet, but never past the canvas's trailing edge: the
        // text starts where the person pointed and the panel fits on screen.
        let width = f64::from(theme.tokens.metrics.panel_width_sheet)
            .min(frame.x1 - gap - inset - at.x)
            .max(row);
        // The text starts where the person pointed: the field is centred on
        // the line pointed at, and the panel grows around it.
        let field = Rect::new(at.x, at.y - row / 2.0, at.x + width, at.y + row / 2.0);
        let Some(caret) = self.canvas.caret.as_mut() else {
            return;
        };
        let refused = caret.refused.map(CanvasRefusal::message);
        let theme =
            if caret.text.is_empty() && !caret.composition.is_composing() && refused.is_none() {
                theme.clone()
            } else {
                let mut face = ui::widget::primitives::FloatingPanelFace::new()
                    .header(row)
                    .body_empty(true);
                if refused.is_some() {
                    face = face.footer(row);
                }
                let panel = Rect::new(
                    field.x0 - inset,
                    field.y0 - inset,
                    field.x1 + inset,
                    field.y0 - inset + face.height_for(0.0, theme),
                );
                let (layout, on_panel) = face.paint(scene, transform, panel, 1.0, theme);
                if let Some(message) = refused.as_deref() {
                    // Why the last Enter did not reach an agent, under the text.
                    let style = on_panel.text(ui::tokens::TextRole::Label);
                    text_renderer.draw_style(
                        scene,
                        message,
                        on_panel.tokens.colors.text_dim,
                        transform.pre_translate((layout.footer.x0, layout.footer.y0).into()),
                        Some(ui::num::f32_of_f64(layout.footer.width())),
                        &style,
                    );
                }
                on_panel
            };
        caret.composition.draw(
            &mut ui::paint::PaintCtx::new(scene, transform, text_renderer, &theme),
            QueryView {
                text: &caret.text,
                selection: &caret.selection,
                placeholder: "",
                show_caret: true,
            },
            field,
            &theme.text(ui::tokens::TextRole::Body),
            QueryInk::of(&theme.tokens.colors),
        );
    }

    /// What the canvas shows now, for the test control: whether it shows,
    /// the lens, the caret, what is typed at it and why it did not reach an
    /// agent, the selection, every
    /// window's place on the canvas and its rect on screen, and how many
    /// answers wait for their window.
    pub(crate) fn canvas_state(&self) -> serde_json::Value {
        let lens = self.canvas_lens();
        let strip_frame = self.strip_frame();
        let strip_indicators = strip_frame.ribbon_offered || strip_frame.rail_offered;
        let windows: Vec<serde_json::Value> = self
            .group_row_ptrs
            .values()
            .flat_map(|&ptr| {
                // SAFETY: read-only use under the field's stable-row contract.
                let row = unsafe { &*ptr };
                let view = row.canvas().cloned();
                row.groups
                    .iter()
                    .map(|group| {
                        let rect = ui::Widget::get_rect(group);
                        let place = view.as_ref().and_then(|v| v.places.get(&group.id).copied());
                        serde_json::json!({
                            "group": group.id,
                            "place": place.map(|p| [p.x0, p.y0, p.width(), p.height()]),
                            "screen": [rect.x0, rect.y0, rect.width(), rect.height()],
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        serde_json::json!({
            "shown": self.canvas_shown(),
            "origin": [lens.origin.x, lens.origin.y],
            "zoom": lens.zoom,
            "caret": self.canvas.caret.as_ref().map(|caret| serde_json::json!({
                "at": [caret.at.x, caret.at.y],
                "text": caret.text,
                "refused": caret.refused.map(CanvasRefusal::message_key),
            })),
            "selection": self.canvas.selection,
            "windows": windows,
            "answers_waiting": self.canvas.answers.waiting(),
            // Whether the strips' ribbon and rail may show: never over a
            // canvas, which has no strip to be on.
            "strip_indicators": strip_indicators,
            "frame": self.canvas_frame().map(|f| [f.x0, f.y0, f.width(), f.height()]),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_answer_takes_the_oldest_anchor_waiting_for_its_app() {
        let mut answers = CanvasAnswers::default();
        answers.expect("so.tilo.tasks", CanvasAnchor::Point(Point::new(1.0, 2.0)));
        answers.expect("so.tilo.tasks", CanvasAnchor::Point(Point::new(3.0, 4.0)));
        assert_eq!(answers.claim("so.tilo.notes"), None);
        assert_eq!(
            answers.claim("so.tilo.tasks"),
            Some(CanvasAnchor::Point(Point::new(1.0, 2.0)))
        );
        assert_eq!(answers.waiting(), 1);
    }

    #[test]
    fn places_and_rects_round_trip_in_whole_pixels() {
        let place = CanvasPlace::new(-40, 12, 640, 480);
        assert_eq!(rect_place(place_rect(place)), place);
        assert_eq!(
            rect_place(Rect::new(0.4, 0.6, 10.2, 20.7)),
            CanvasPlace::new(0, 1, 10, 20)
        );
    }
}
