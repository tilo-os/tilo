//! The desk's widgets as one layer: it lays them out where the person keeps
//! them, decides the form each is in ([`DeskBoard`]) and paints it there.
//!
//! Widget content is what the session already holds: the people in this
//! workspace come from the tree the desk shows, the date from the clock.
//! Nothing is polled: the layer reads the tree it is mounted over, and the
//! person's choices ([`DeskWidgetPrefs`]) arrive from the settings document
//! the session watches.

#![allow(
    clippy::arithmetic_side_effects,
    reason = "widget geometry is finite logical-pixel kurbo arithmetic"
)]

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

use taffy::{NodeId, Style, TaffyTree};
use tilo_desktop_surface::notifications::{
    CLEAR_UNDO_WINDOW, Notification, NotificationIconKind, NotificationSeverity,
};
use tilo_desktop_surface::{DesktopHandle, DesktopNotifications, NotificationOutcome};
use ui::desk_widgets::{DeskWidgetPrefs, ShowMode, WidgetId, WidgetPrefs};
use ui::motion::{Ambient, Presence, Zoom};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::time::{Duration, Instant};
use ui::tokens::{EffectiveMotion, MotionSource};
use ui::widget::{AppModel, EventCtx, ModelPath, TaffyTreeExt as _, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Size, Vec2};
use winit::event::{ElementState, MouseButton, WindowEvent};

use super::calendar::Month;
use super::music::{DeskMusic, MusicAction, NowPlaying};
use super::paint::{self, BAND};
use super::placement::{self, MARGIN};
use super::sizing;
use super::system_monitor::{self, Monitor, Reading, Service, Source};
use super::team::{self, TeamMember};
use super::{Context, DeskBoard, Form, Slot};
use crate::chrome::ChromeIntent;
use crate::desktop_host::DesktopAsk;

/// The widgets this layer lays out, bottom first: music rests in the
/// bottom-right corner, the people and the date down the right edge and
/// the machine's load in the bottom-left, so they never overlap.
const SHOWN: [WidgetId; 4] = [
    WidgetId::Music,
    WidgetId::Team,
    WidgetId::Calendar,
    WidgetId::SystemMonitor,
];

/// What the desk's controls are named to assistive technology: the widget's
/// own control, then `.pin` or `.remove` for its card's.
const DESK_TARGET: &str = "desk.widget:";

/// One of the desk's controls, as a screen reader reads it.
fn control(target: String, name: String, bounds: Rect) -> ui::a11y::AccessNode {
    ui::a11y::AccessNode::new(ui::a11y::Role::Button)
        .with_name(name)
        .with_bounds(bounds)
        .with_key(ui::a11y::hash_key(&target))
        .with_action_target(target)
}

/// Which part of a widget a press landed on.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Part {
    /// The form itself: a tap opens a glance, and on an open card folds it.
    Body,
    Collapse,
    Pin,
    Remove,
    /// The card's resize handle, which follows the hand from the press.
    Resize,
    /// The row of the person with this roster id.
    Person(String),
    /// One of the music widget's transport controls.
    Music(MusicAction),
}

/// A press being held on a widget: which part it took, where, and what the
/// pointer did since. A press that moves past [`DRAG_SLOP`] is a drag, and
/// only a press let go where it was pressed is a tap.
#[derive(Clone, Debug)]
struct Grab {
    id: WidgetId,
    part: Part,
    at: Point,
    started: Instant,
    dragging: bool,
    /// The card's width at the press, for a resize.
    width: f64,
    /// The pointer's recent places, for a throw's velocity.
    samples: Vec<(f64, Point)>,
}

/// A press that moves this far is a drag, never a tap.
const DRAG_SLOP: f64 = ui::tokens::Metrics::BASE.icon_label_gap as f64;

/// How much larger a widget is drawn while lifted under the hand.
const LIFT_SCALE: f64 = 0.03;

/// The desk's widgets, drawn over the desk and under the windows.
pub struct DeskWidgets {
    handle: DesktopHandle,
    /// This person, as the roster names them.
    user: String,
    /// What the desk asks of the host (a person's row opens who they are).
    asks: Rc<RefCell<Vec<DesktopAsk>>>,
    /// The person's choices as this layer has them.
    prefs: DeskWidgetPrefs,
    /// The same choices as the session reads them from the settings
    /// document: the document is watched there, and read once a change.
    changes: std::sync::Arc<std::sync::Mutex<DeskWidgetPrefs>>,
    /// What this layer last laid out from.
    laid_from: DeskWidgetPrefs,
    /// Write a choice back to the settings document. A test keeps its own.
    persist: bool,
    board: DeskBoard,
    slots: Vec<Slot>,
    /// Each widget coming and going, and its card expanding from its glance.
    shown: BTreeMap<WidgetId, Presence>,
    opened: BTreeMap<WidgetId, Presence>,
    pointer: Option<Point>,
    grab: Option<Grab>,
    /// The width a resize under way gives its card.
    resizing: Option<(WidgetId, f64)>,
    /// Each widget's travel from its laid-out place into the one it was
    /// just dropped on: a set-down settles the last pixels, a throw coasts.
    offsets: BTreeMap<WidgetId, ui::motion::MotionValue<Vec2>>,
    /// How far each widget is lifted under the hand, growing with its
    /// shadow while a press carries it.
    lifts: BTreeMap<WidgetId, ui::motion::MotionValue<f64>>,
    /// The system monitor's readings, and where they come from.
    monitor: Monitor,
    source: Box<dyn Source>,
    /// The people the team card lists, top row first: what a press on a row
    /// opens is the row as laid out.
    team_ids: Vec<String>,
    motion: EffectiveMotion,
    /// What the last frame painted, to know that the next one differs even
    /// when nothing the person did changed it: the date turned, someone
    /// joined, a choice arrived from Settings.
    painted: Option<Painted>,
    dirty: bool,
    rect: Rect,
    /// Whether a card is open, for the keys that fold it, and their ask.
    fold: super::DeskFold,
    /// Whether the overview is up over the desk: a card the person opened
    /// goes with it.
    overview: Rc<Cell<bool>>,
    /// The desk's notifications: a widget taken off the desk offers Undo
    /// there for a while.
    notices: DesktopNotifications,
    /// The Undo toasts on offer, by notification: each puts its widget back.
    undo: Rc<RefCell<BTreeMap<u64, WidgetId>>>,
    /// The widgets an Undo asked back, taken up on the next pass.
    restores: Rc<RefCell<Vec<WidgetId>>>,
    /// What plays, as the session reads it: the music widget shows it and
    /// its controls ask for the transport.
    music: DeskMusic,
}

/// One widget as laid out: where it is, and which display that is.
#[derive(Clone, Debug, PartialEq)]
struct Laid {
    slot: Slot,
    display: usize,
    monitor: String,
    area: Rect,
}

/// What the widgets fill themselves with in a frame: the day the calendar
/// shows, the people the team card lists, and where the pointer is.
struct Content<'a> {
    month: &'a Month,
    members: &'a [TeamMember],
    reading: Reading,
    /// What plays, while something does.
    playing: Option<&'a NowPlaying>,
    cursor: Option<Point>,
}

/// What the desk showed in a frame.
#[derive(Clone, Debug, PartialEq)]
struct Painted {
    forms: Vec<(WidgetId, Form)>,
    day: chrono::NaiveDate,
    people: Vec<String>,
    reading: Reading,
    playing: Option<NowPlaying>,
    prefs: DeskWidgetPrefs,
}

impl DeskWidgets {
    /// The desk's widgets, showing what `prefs` says.
    #[must_use]
    pub fn new(
        handle: DesktopHandle,
        user: String,
        asks: Rc<RefCell<Vec<DesktopAsk>>>,
        changes: std::sync::Arc<std::sync::Mutex<DeskWidgetPrefs>>,
    ) -> Self {
        let prefs = shared(&changes).clone();
        Self {
            handle,
            user,
            asks,
            laid_from: prefs.clone(),
            prefs,
            changes,
            persist: true,
            board: DeskBoard::default(),
            slots: Vec::new(),
            shown: BTreeMap::new(),
            opened: BTreeMap::new(),
            pointer: None,
            grab: None,
            resizing: None,
            offsets: BTreeMap::new(),
            lifts: BTreeMap::new(),
            monitor: Monitor::default(),
            source: Box::new(Service::default()),
            team_ids: Vec::new(),
            motion: EffectiveMotion::default(),
            painted: None,
            dirty: true,
            rect: Rect::ZERO,
            fold: super::DeskFold::default(),
            overview: Rc::default(),
            notices: DesktopNotifications::default(),
            undo: Rc::default(),
            restores: Rc::default(),
            music: DeskMusic::default(),
        }
    }

    /// The same layer sharing `fold` with the session's keys, which fold
    /// its open card on Escape.
    #[must_use]
    pub fn with_fold(mut self, fold: super::DeskFold) -> Self {
        self.fold = fold;
        self
    }

    /// The same layer folding what is open while the overview `up` is over
    /// the desk.
    #[must_use]
    pub fn with_overview(mut self, up: Rc<Cell<bool>>) -> Self {
        self.overview = up;
        self
    }

    /// The same layer showing what plays from `music`, the reading the
    /// session keeps in step with the player Quick Settings shows.
    #[must_use]
    pub fn with_music(mut self, music: DeskMusic) -> Self {
        self.music = music;
        self.dirty = true;
        self
    }

    /// The same layer posting to `notices`, the desk's own: what it takes off
    /// the desk offers Undo there.
    #[must_use]
    pub fn with_notices(mut self, notices: DesktopNotifications) -> Self {
        let undo = Rc::clone(&self.undo);
        let restores = Rc::clone(&self.restores);
        // The person's answer comes from wherever the toast was shown, not
        // from this layer: the desk takes it up on its next pass.
        notices.on_outcome(move |outcome| match outcome {
            NotificationOutcome::Activated { notification, .. } => {
                if let Some(widget) = undo.borrow_mut().remove(&notification.id) {
                    restores.borrow_mut().push(widget);
                }
            }
            NotificationOutcome::Dismissed { id } => {
                undo.borrow_mut().remove(id);
            }
        });
        self.notices = notices;
        self
    }

    /// The same layer showing `prefs`, keeping what it is asked in memory:
    /// for a desk that stands on its own.
    #[must_use]
    pub fn with_prefs(mut self, prefs: DeskWidgetPrefs) -> Self {
        *shared(&self.changes) = prefs.clone();
        self.laid_from = prefs.clone();
        self.prefs = prefs;
        self.persist = false;
        self
    }

    /// The same layer reading the machine through `source`: for a desk
    /// whose readings are known.
    #[cfg(test)]
    #[must_use]
    pub fn with_source(mut self, source: Box<dyn Source>) -> Self {
        self.source = source;
        self
    }

    /// The form `id` is in now.
    #[must_use]
    pub fn form(&self, id: WidgetId) -> Form {
        self.board.form(id)
    }

    /// Where `id` rests and where its card opens this frame.
    #[must_use]
    pub fn slot(&self, id: WidgetId) -> Option<Slot> {
        self.slots.iter().find(|slot| slot.id == id).copied()
    }

    /// The choices in force.
    #[must_use]
    pub fn prefs(&self) -> DeskWidgetPrefs {
        self.prefs.clone()
    }

    /// How far `id` is lifted under the hand now, from 0 (set down) to 1
    /// (carried): it is drawn a little larger and its shadow deeper.
    #[must_use]
    pub fn lift(&self, id: WidgetId) -> f64 {
        self.lifts
            .get(&id)
            .map_or(0.0, |lift| lift.get(ui::time::now()).clamp(0.0, 1.0))
    }

    /// Lift `id` under the hand, or set it down.
    fn set_lift(&mut self, id: WidgetId, up: bool) {
        let timing = ui::motion::ControlChange::MOVE.timing(
            ui::motion::Direction::of(up),
            self.motion,
            MotionSource::Gesture,
        );
        self.lifts
            .entry(id)
            .or_insert_with(|| ui::motion::MotionValue::new(0.0))
            .set(if up { 1.0 } else { 0.0 }, timing, ui::time::now());
        self.dirty = true;
    }

    /// The people the team card lists, top row first.
    fn team_rows(&self) -> Vec<String> {
        let user = self.user.as_str();
        self.handle
            .read(|tree| team::rows(tree, user, user))
            .into_iter()
            .map(|row| row.id)
            .collect()
    }

    /// The people here as the team widget lists them.
    fn members(&self, presence: &PresenceCtx) -> Vec<TeamMember> {
        let user = self.user.as_str();
        self.handle
            .read(|tree| team::members(tree, user, user, presence, std::time::SystemTime::now()))
    }

    /// The displays the desk covers, each by its monitor's stable identity
    /// and its rect in the desk's own coordinates.
    fn displays(&self) -> Vec<(String, Rect)> {
        let displays = self.handle.read(|tree| tree.outputs.displays.clone());
        let Some(x0) = displays.iter().map(|display| display.x).reduce(f64::min) else {
            return vec![(String::new(), self.rect)];
        };
        let y0 = displays
            .iter()
            .map(|display| display.y)
            .fold(f64::INFINITY, f64::min);
        displays
            .into_iter()
            .map(|display| {
                let x = self.rect.x0 + display.x - x0;
                let y = self.rect.y0 + display.y - y0;
                (
                    display.monitor,
                    Rect::new(x, y, x + display.width, y + display.height),
                )
            })
            .collect()
    }

    /// The display `id` lives on and the place it rests there: the first
    /// connected display it has a place on, else the primary at the
    /// widget's own default place.
    fn home(&self, displays: &[(String, Rect)], id: WidgetId) -> (usize, (f64, f64)) {
        let prefs = self.prefs.get(id);
        prefs
            .places
            .iter()
            .find_map(|place| {
                displays
                    .iter()
                    .position(|(monitor, _)| *monitor == place.display)
                    .map(|index| (index, (place.x, place.y)))
            })
            .unwrap_or_else(|| (0, id.default_place()))
    }

    /// The width `id`'s card is drawn at: the resize under way, else the
    /// person's, else the widget's own.
    fn card_width(&self, id: WidgetId, area: Rect) -> f64 {
        let sizing = sizing::sizing(id);
        let room = (area.width() - 2.0 * MARGIN).max(1.0);
        let width = match self.resizing {
            Some((resized, width)) if resized == id => width,
            _ => self.prefs.get(id).width.unwrap_or(sizing.default),
        };
        sizing.clamp(width).min(room)
    }

    /// Whether `id` has anything to show right now.
    fn has_content(id: WidgetId, people: usize, playing: Option<&NowPlaying>) -> bool {
        match id {
            WidgetId::Team => people > 0,
            // The date is always there to show; the machine always offers
            // memory.
            WidgetId::Calendar | WidgetId::SystemMonitor => true,
            // Music is there while something plays, as its person would find
            // it, and gone when nothing does.
            WidgetId::Music => playing.is_some(),
        }
    }

    /// Where `id`'s resting form and open card are on the display it lives
    /// on, and whether it has anything to show.
    fn lay(&self, id: WidgetId, people: usize, playing: Option<&NowPlaying>) -> Laid {
        let displays = self.displays();
        let (display, at) = self.home(&displays, id);
        let (monitor, area) = displays
            .get(display)
            .cloned()
            .unwrap_or_else(|| (String::new(), self.rect));
        let glance = match id {
            WidgetId::Team => {
                // Against a side edge (not in a bottom corner) the faces
                // stack down it; anywhere else they line up across.
                let vertical = (at.0 <= 0.01 || at.0 >= 0.99) && at.1 < 0.99;
                placement::rect_at(area, paint::team_glance_size(vertical, people), at)
            }
            WidgetId::Calendar => placement::rect_at(area, paint::CALENDAR_GLANCE, at),
            WidgetId::SystemMonitor => placement::rect_at(area, paint::MONITOR_GLANCE, at),
            WidgetId::Music => match playing {
                Some(_) => placement::rect_at(area, paint::MUSIC_GLANCE, at),
                None => Rect::ZERO,
            },
        };
        let width = self.card_width(id, area);
        let height = sizing::height(id, width, |stage, width| match id {
            WidgetId::Team => paint::team_card_height(stage, people),
            WidgetId::Calendar => paint::calendar_card_height(stage, width),
            WidgetId::SystemMonitor => paint::monitor_card_height(stage),
            WidgetId::Music => paint::music_card_height(stage, width),
        });
        let open = Size::new(width, height + BAND);
        let slot = Slot {
            id,
            glance,
            open: placement::open_rect(area, glance, open),
            reach: placement::reach_zone(area, glance),
            has_content: Self::has_content(id, people, playing),
        };
        Laid {
            slot,
            display,
            monitor,
            area,
        }
    }

    /// Lay every widget out where the person put it, each on its own
    /// display, with neighbours there stepping out of each other's way.
    /// Returns whether anything moved.
    fn relayout(&mut self) -> bool {
        if self.rect.width() <= 0.0 || self.rect.height() <= 0.0 {
            return false;
        }
        let people = self.team_rows();
        let playing = self.music.get();
        let laid: Vec<Laid> = SHOWN
            .into_iter()
            .map(|id| self.lay(id, people.len(), playing.as_ref()))
            .collect();
        let mut slots: Vec<Slot> = laid.iter().map(|laid| laid.slot).collect();
        // On each display, widgets resting on each other step apart from
        // that display's middle.
        let displays = laid.iter().map(|laid| (laid.display, laid.area)).fold(
            Vec::new(),
            |mut seen: Vec<(usize, Rect)>, one| {
                if !seen.contains(&one) {
                    seen.push(one);
                }
                seen
            },
        );
        for (display, area) in displays {
            let on: Vec<usize> = (0..slots.len())
                .filter(|k| laid.get(*k).is_some_and(|one| one.display == display))
                .collect();
            let mut rects: Vec<Rect> = on
                .iter()
                .filter_map(|k| slots.get(*k).map(|slot| slot.glance))
                .collect();
            placement::resolve(area, &mut rects, &[]);
            for (k, rect) in on.into_iter().zip(rects) {
                let Some(slot) = slots.get_mut(k) else {
                    continue;
                };
                if !slot.has_content {
                    continue;
                }
                slot.glance = rect;
                slot.reach = placement::reach_zone(area, rect);
                slot.open = placement::open_rect(area, rect, slot.open.size());
            }
        }
        let changed = slots != self.slots;
        self.slots = slots;
        // What a press on a row opens is the row as drawn.
        self.team_ids = people;
        changed
    }

    /// Whether a window of the shown strip covers the open card: the
    /// widgets rest under the windows, so a card a window covers is out of
    /// sight and folds, as it does when the person clicks the desk beside
    /// it.
    fn window_covers_card(&self) -> bool {
        let Some(opened) = self.board.opened() else {
            return false;
        };
        let Some(slot) = self.slot(opened) else {
            return false;
        };
        let card = self.drawn(&slot).open;
        self.handle
            .shown_tabs()
            .iter()
            .any(|(_, window)| window.intersect(card).area() > 0.0)
    }

    /// The widget form under `p`, if one is showing there: the topmost, as
    /// painted. A glance's remove badge is part of it.
    fn hit_at(&self, p: Point) -> Option<(WidgetId, Form, Rect)> {
        let drawn: Vec<Slot> = self.slots.iter().map(|slot| self.drawn(slot)).collect();
        drawn.iter().rev().find_map(|slot| {
            let form = self.board.form(slot.id);
            match form {
                Form::Away => None,
                Form::Open => slot.open.contains(p).then_some((slot.id, form, slot.open)),
                Form::Glance => (slot.glance.contains(p)
                    || paint::badge_rect(slot.glance, self.anchor(slot.id)).contains(p))
                .then_some((slot.id, form, slot.glance)),
            }
        })
    }

    /// Which way `id`'s card grows from its glance: toward the middle of
    /// the desk.
    fn anchor(&self, id: WidgetId) -> (bool, bool) {
        self.slot(id).map_or((true, true), |slot| {
            placement::anchor(placement::fractions(self.rect, slot.glance))
        })
    }

    /// The part of `id`, showing in `form` at `rect`, under `p`.
    fn part_at(&self, id: WidgetId, form: Form, rect: Rect, p: Point) -> Part {
        match form {
            Form::Away => Part::Body,
            Form::Glance => {
                if paint::badge_rect(rect, self.anchor(id)).contains(p) {
                    return Part::Remove;
                }
                // The music widget's play or pause acts where the song rests;
                // the rest of it opens the card.
                if id == WidgetId::Music && paint::music_glance_play(rect).contains(p) {
                    return Part::Music(MusicAction::PlayPause);
                }
                Part::Body
            }
            Form::Open => {
                if paint::remove_rect(rect).contains(p) {
                    return Part::Remove;
                }
                if paint::pin_rect(rect).contains(p) {
                    return Part::Pin;
                }
                if paint::collapse_rect(rect).contains(p) {
                    return Part::Collapse;
                }
                let card = paint::card_rect(rect);
                let anchor = self.anchor(id);
                if paint::grip_rect(card, anchor).contains(p)
                    || paint::grip_edge(card, anchor).contains(p)
                {
                    return Part::Resize;
                }
                if !card.contains(p) {
                    return Part::Body;
                }
                if id == WidgetId::Team {
                    let stage = sizing::dominant(id, card.width());
                    if let Some(person) = paint::team_rows(card, stage, self.team_ids.len())
                        .iter()
                        .zip(&self.team_ids)
                        .find(|(row, _)| row.contains(p))
                        .map(|(_, person)| person.clone())
                    {
                        return Part::Person(person);
                    }
                }
                if id == WidgetId::Music {
                    let stage = sizing::dominant(id, card.width());
                    if let Some((action, _)) = paint::music_transport(card, stage)
                        .into_iter()
                        .find(|(_, target)| target.contains(p))
                    {
                        return Part::Music(action);
                    }
                }
                Part::Body
            }
        }
    }

    /// How far `id` is drawn from its laid-out place: with the hand while
    /// one carries it.
    fn offset(&self, id: WidgetId) -> Vec2 {
        match &self.grab {
            Some(grab) if grab.id == id && grab.dragging && grab.part != Part::Resize => {
                self.pointer.map_or(Vec2::ZERO, |pointer| pointer - grab.at)
            }
            _ => Vec2::ZERO,
        }
    }

    /// `slot` where it is drawn now.
    fn drawn(&self, slot: &Slot) -> Slot {
        let offset = self.offset(slot.id);
        if offset == Vec2::ZERO {
            return *slot;
        }
        Slot {
            glance: slot.glance + offset,
            open: slot.open + offset,
            ..*slot
        }
    }

    /// The pointer moved to `p` with a press held on a widget: carry it or
    /// resize it, as the press began.
    fn grab_moved(&mut self, p: Point, now: Instant) {
        let Some(mut grab) = self.grab.take() else {
            return;
        };
        let elapsed = now.saturating_duration_since(grab.started).as_secs_f64();
        grab.samples.push((elapsed, p));
        if grab.samples.len() > 24 {
            grab.samples.remove(0);
        }
        if grab.part == Part::Resize {
            // The card grows from the corner it is anchored by: dragging
            // the handle away from that corner widens it.
            let (right, _) = self.anchor(grab.id);
            let toward = if right { -1.0 } else { 1.0 };
            let displays = self.displays();
            let area = displays
                .get(self.home(&displays, grab.id).0)
                .map_or(self.rect, |(_, area)| *area);
            let room = (area.width() - 2.0 * MARGIN).max(1.0);
            let width = sizing::sizing(grab.id)
                .clamp(grab.width + toward * (p.x - grab.at.x))
                .min(room);
            self.resizing = Some((grab.id, width));
            self.dirty = true;
            self.relayout();
        } else if !grab.dragging && (p - grab.at).hypot() > DRAG_SLOP {
            grab.dragging = true;
            // Under the hand it comes up off the desk: bigger, and its
            // shadow further away.
            self.set_lift(grab.id, true);
            self.dirty = true;
        }
        self.grab = Some(grab);
    }

    /// The press `grab` let go at `p`: a resize keeps its width, a drag
    /// lands the widget, anything else is a tap.
    fn release(&mut self, grab: Grab, p: Point) {
        if grab.part == Part::Resize {
            if let Some((id, width)) = self.resizing.take() {
                let mut prefs = self.prefs.get(id).clone();
                prefs.width = Some(width.round());
                self.store(id, prefs);
            }
            return;
        }
        if grab.dragging {
            self.set_lift(grab.id, false);
            self.drop(&grab, p);
            return;
        }
        let form = self.board.form(grab.id);
        self.tap(grab.id, form, grab.part);
    }

    /// `grab`'s widget let go at `p`: thrown off the screen it is removed;
    /// otherwise it coasts on with the hand's momentum to where the throw
    /// projects, is drawn onto a nearby edge, corner or neighbour on the
    /// display under it, and keeps that place there.
    fn drop(&mut self, grab: &Grab, p: Point) {
        let id = grab.id;
        let Some(slot) = self.slot(id) else {
            return;
        };
        let motion = self.motion;
        let velocity = placement::release_velocity(&grab.samples);
        let travel = p - grab.at;
        let open = self.board.form(id) == Form::Open;
        let before = self.drawn(&slot);
        let drawn = if open { before.open } else { before.glance } + travel;
        let displays = self.displays();
        let areas: Vec<Rect> = displays.iter().map(|(_, area)| *area).collect();
        if placement::thrown_off(&areas, drawn, velocity, motion) {
            self.remove(id);
            return;
        }
        let landed = placement::landing(drawn, velocity, motion);
        let Some(index) = placement::display_at(&areas, landed.center()) else {
            return;
        };
        let Some((monitor, area)) = displays.get(index).cloned() else {
            return;
        };
        let neighbours: Vec<Rect> = self
            .slots
            .iter()
            .filter(|other| other.id != id)
            .filter(|other| self.home(&displays, other.id).0 == index)
            .filter_map(|other| match self.board.form(other.id) {
                Form::Away => None,
                Form::Glance => Some(other.glance),
                Form::Open => Some(other.open),
            })
            .collect();
        let snapped = placement::magnet(area, landed, &neighbours);
        let glance = if open {
            placement::glance_in(area, snapped, slot.glance.size())
        } else {
            snapped
        };
        let at = placement::fractions(area, glance);
        let mut prefs = self.prefs.get(id).clone();
        prefs.drop_on(&monitor, at.0, at.1);
        self.store(id, prefs);
        self.relayout();
        // From where the hand let go to its new place, carrying the hand's
        // velocity: a throw coasts on and slows into its place, a set-down
        // settles the last pixels.
        let rest = self
            .slot(id)
            .map_or(snapped, |slot| if open { slot.open } else { slot.glance });
        let timing = ui::motion::Snap::COAST.timing(
            ui::motion::Direction::Enter,
            motion,
            MotionSource::Gesture,
        );
        let offset = self.offsets.entry(id).or_default();
        offset.snap(drawn.origin() - rest.origin());
        offset.release(Vec2::ZERO, velocity, timing, ui::time::now());
    }

    /// Whether the pointer is on `id` as it is drawn.
    fn hovered(&self, id: WidgetId) -> bool {
        self.pointer
            .and_then(|p| self.hit_at(p))
            .is_some_and(|(hit, ..)| hit == id)
    }

    /// What a press on `part` of `id`, showing in `form`, does.
    fn tap(&mut self, id: WidgetId, form: Form, part: Part) {
        match part {
            Part::Pin => self.toggle_pin(id),
            Part::Remove => self.remove(id),
            Part::Collapse => self.board.close(),
            Part::Person(person) => self
                .asks
                .borrow_mut()
                .push(DesktopAsk::Intent(ChromeIntent::ShowPerson(person))),
            Part::Music(action) => self.asks.borrow_mut().push(DesktopAsk::Music(action)),
            Part::Body if form == Form::Glance => self.board.open(id),
            // A resize ends where the hand lets go; a click on the handle
            // does nothing, as does a tap on an open card.
            Part::Resize | Part::Body => {}
        }
        self.dirty = true;
    }

    /// Take `id` off the desk: its remove control, or thrown off the screen.
    /// Its place and size are kept, switched off, and a toast offers Undo,
    /// which puts it back where it was.
    fn remove(&mut self, id: WidgetId) {
        let mut prefs = self.prefs.get(id).clone();
        prefs.enabled = false;
        self.store(id, prefs);
        if self.board.opened() == Some(id) {
            self.board.close();
        }
        for presence in [self.shown.get_mut(&id), self.opened.get_mut(&id)]
            .into_iter()
            .flatten()
        {
            presence.snap(false);
        }
        if let Some(offset) = self.offsets.get_mut(&id) {
            offset.snap(Vec2::ZERO);
        }
        if let Some(lift) = self.lifts.get_mut(&id) {
            lift.snap(0.0);
        }
        self.offer_undo(id);
    }

    /// Offer Undo for `id`, just taken off the desk: the toast names it, and
    /// choosing it before it lapses puts the widget back.
    fn offer_undo(&self, id: WidgetId) {
        let name = paint::desk_widget_title(id);
        let mut toast = Notification::new(
            crate::i18n::t_args("desk-widget-removed", &[("widget", name.as_str().into())]),
            String::new(),
        )
        .with_severity(NotificationSeverity::Info)
        .with_icon_kind(NotificationIconKind::Generic);
        toast.dismiss_after = CLEAR_UNDO_WINDOW;
        toast.actions = vec![("default".into(), crate::i18n::t("desk-widget-undo"))];
        toast.bypass_dnd = true;
        toast.show_on_lock_screen = false;
        let posted = self.notices.post(toast);
        self.undo.borrow_mut().insert(posted, id);
    }

    /// Keep `id` on this desk even while the person works, or stop.
    fn toggle_pin(&mut self, id: WidgetId) {
        let mut prefs = self.prefs.get(id).clone();
        if prefs.always_on(None) {
            prefs.always_on_strips.clear();
            prefs.show = ShowMode::Pause;
        } else {
            prefs.show = ShowMode::Always;
            prefs.always_on_strips.clear();
        }
        self.store(id, prefs);
    }

    /// Put `prefs` in force for `id`, in the settings document too.
    fn store(&mut self, id: WidgetId, prefs: WidgetPrefs) {
        if self.persist
            && let Err(error) = ui::desk_widgets::prefs_key(id).set(prefs.clone())
        {
            tracing::warn!(target: "tilo::desk", %error, "desk widget choice not saved");
        }
        *self.prefs.get_mut(id) = prefs;
        *shared(&self.changes) = self.prefs.clone();
        self.laid_from = self.prefs.clone();
        self.dirty = true;
        self.relayout();
    }

    /// Take up any change the person made in Settings, and decide every
    /// widget's form at `now`. Returns whether anything changed.
    fn tick(&mut self, now: Instant) -> bool {
        let mut changed = false;
        // Undo puts a widget the person removed back where it was.
        let restores: Vec<WidgetId> = std::mem::take(&mut *self.restores.borrow_mut());
        for id in restores {
            let mut prefs = self.prefs.get(id).clone();
            prefs.enabled = true;
            self.store(id, prefs);
            changed = true;
        }
        let fresh = shared(&self.changes).clone();
        if fresh != self.prefs {
            self.prefs = fresh;
        }
        if self.prefs != self.laid_from {
            self.laid_from = self.prefs.clone();
            changed = true;
        }
        changed |= self.relayout();
        // An open card folds under whatever takes the desk from the person:
        // the overview over it, a window covering it, or Escape, which the
        // session's keys send here because the desk is under the windows. An
        // ask that finds no card open is spent here too, so it cannot fold
        // the next card opened.
        let asked = self.fold.take_asked();
        if self.board.opened().is_some()
            && (asked || self.overview.get() || self.window_covers_card())
        {
            self.board.close();
        }
        let covered = false;
        let engaged = self
            .grab
            .as_ref()
            .map(|grab| grab.id)
            .or_else(|| self.pointer.and_then(|p| self.hit_at(p)).map(|(id, ..)| id));
        let ctx = Context {
            now,
            prefs: &self.prefs,
            strip: None,
            super_surface: false,
            super_glance: false,
            covered,
            engaged,
            slots: &self.slots,
        };
        changed |= self.board.update(&ctx);
        // The system monitor reads the machine only while it shows: away,
        // it holds no subscription, so nothing is read and nothing wakes
        // for it.
        if self.board.form(WidgetId::SystemMonitor) == Form::Away {
            self.monitor.pause(&mut *self.source);
        } else if self.monitor.show(&mut *self.source) {
            changed = true;
            changed |= self.relayout();
        }
        let asked =
            |id: WidgetId| self.board.reached() == Some(id) || self.board.opened() == Some(id);
        for slot in &self.slots {
            let form = self.board.form(slot.id);
            let asked = asked(slot.id);
            self.shown
                .entry(slot.id)
                .or_insert_with(Presence::closed)
                .show(
                    form != Form::Away,
                    Ambient::spec(asked),
                    self.motion,
                    MotionSource::Pointer,
                    now,
                );
            // A card expands from its glance and folds back into it in
            // place: one shape between the two.
            self.opened
                .entry(slot.id)
                .or_insert_with(Presence::closed)
                .show(
                    form == Form::Open,
                    Zoom::SPEC,
                    self.motion,
                    MotionSource::Pointer,
                    now,
                );
        }
        // Escape is the desk's while a card is open, and not a moment
        // before or after.
        self.fold.set_open(self.board.opened().is_some());
        if changed {
            self.dirty = true;
        }
        changed
    }

    /// Whether a widget is still coming, going, or changing shape.
    fn animating(&self, now: Instant) -> bool {
        self.offsets
            .values()
            .any(|offset| offset.settles_at().is_some_and(|at| at > now))
            || self.lifts.values().any(|lift| lift.is_moving(now))
            || self
                .shown
                .values()
                .chain(self.opened.values())
                .any(|presence| presence.is_moving(now))
    }

    fn paint_glance(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        slot: &Slot,
        content: &Content<'_>,
    ) {
        let Content {
            month,
            members,
            reading,
            playing,
            ..
        } = *content;
        paint::paint_glance_ground(scene, transform, theme, slot.id, slot.glance);
        match slot.id {
            WidgetId::Team => paint::paint_team_glance(
                scene,
                transform,
                text,
                theme,
                slot.glance,
                slot.glance.height() > slot.glance.width(),
                members,
            ),
            WidgetId::Calendar => {
                paint::paint_calendar_glance(scene, transform, text, theme, slot.glance, month);
            }
            WidgetId::SystemMonitor => {
                paint::paint_monitor_glance(scene, transform, text, theme, slot.glance, reading);
            }
            WidgetId::Music => {
                if let Some(playing) = playing {
                    paint::paint_music_glance(
                        scene,
                        transform,
                        text,
                        theme,
                        slot.glance,
                        playing,
                        content.cursor.filter(|_| self.grab.is_none()),
                    );
                }
            }
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one card's ground, band controls and content at its stages, from what the frame holds"
    )]
    fn paint_card(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        slot: &Slot,
        content: &Content<'_>,
    ) {
        let Content {
            month,
            members,
            reading,
            playing,
            cursor,
        } = *content;
        let open = slot.open;
        let card = paint::card_rect(open);
        paint::paint_card_ground(scene, transform, theme, open);
        let pinned = self.prefs.get(slot.id).always_on(None);
        let title = paint::desk_widget_title(slot.id);
        let collapse_label =
            crate::i18n::t_args("desk-widget-collapse", &[("widget", title.as_str().into())]);
        let remove_label =
            crate::i18n::t_args("desk-widget-remove", &[("widget", title.as_str().into())]);
        let pin_label = crate::i18n::t(if pinned {
            "desk-widget-unpin"
        } else {
            "desk-widget-pin"
        });
        let cursor_on =
            |rect: Rect| cursor.is_some_and(|p| rect.contains(p)) && self.grab.is_none();
        for (rect, icon, label, toggled) in [
            (
                paint::collapse_rect(open),
                ui::icons::Icon::ChevronDown,
                collapse_label.as_str(),
                false,
            ),
            (
                paint::pin_rect(open),
                ui::icons::Icon::Pin,
                pin_label.as_str(),
                pinned,
            ),
            (
                paint::remove_rect(open),
                ui::icons::Icon::X,
                remove_label.as_str(),
                false,
            ),
        ] {
            paint::paint_band_control(
                scene,
                transform,
                text,
                theme,
                rect,
                (icon, label),
                (toggled, cursor_on(rect)),
            );
        }
        let width = card.width();
        let (lo, hi, k) = sizing::stages(slot.id, width);
        let stages: Vec<(sizing::Stage, f32)> = if lo == hi {
            vec![(lo, 1.0)]
        } else {
            #[allow(clippy::cast_possible_truncation, reason = "a unit fraction")]
            let k = k as f32;
            vec![(lo, 1.0 - k), (hi, k)]
        };
        let radius = ui::widget::floating_surface::card_radius(card, theme);
        for (stage, alpha) in stages {
            if alpha <= f32::EPSILON {
                continue;
            }
            paint::with_alpha(scene, transform, card.inflate(2.0, 2.0), alpha, |scene| {
                match slot.id {
                    WidgetId::Team => paint::paint_team_card(
                        scene,
                        transform,
                        text,
                        theme,
                        card,
                        members,
                        stage,
                        cursor.filter(|_| self.grab.is_none()),
                    ),
                    WidgetId::Calendar => {
                        paint::paint_calendar_card(
                            scene, transform, text, theme, card, month, stage,
                        );
                    }
                    WidgetId::SystemMonitor => {
                        paint::paint_monitor_card(
                            scene, transform, text, theme, card, reading, stage,
                        );
                    }
                    WidgetId::Music => {
                        if let Some(playing) = playing {
                            paint::paint_music_card(
                                scene,
                                transform,
                                text,
                                theme,
                                card,
                                playing,
                                stage,
                                cursor.filter(|_| self.grab.is_none()),
                            );
                        }
                    }
                }
                // The content stays inside the card's shape.
                let _ = radius;
            });
        }
        // The resize handle sits in the card's free corner, brighter under
        // the pointer or while a resize follows it.
        let anchor = self.anchor(slot.id);
        let grip = paint::grip_rect(card, anchor);
        let resizing = self.resizing.is_some_and(|(resized, _)| resized == slot.id);
        paint::paint_grip(
            scene,
            transform,
            theme,
            grip,
            anchor,
            resizing || cursor_on(grip),
        );
    }
}

impl Widget for DeskWidgets {
    fn update(&mut self, _state: &AppModel, _path: &ModelPath) -> bool {
        // Who is here can change with the tree alone: lay the widget out
        // again for it.
        let people = self.team_rows();
        let playing = self.music.get();
        let team = self.slot(WidgetId::Team);
        let changed = team.is_some_and(|slot| {
            let laid = self.lay(WidgetId::Team, people.len(), playing.as_ref());
            laid.slot.has_content != slot.has_content
        });
        // What plays came or went: its widget is there, or is not.
        let music = self.slot(WidgetId::Music);
        let changed = changed
            | music.is_some_and(|slot| {
                let laid = self.lay(WidgetId::Music, people.len(), playing.as_ref());
                laid.slot.has_content != slot.has_content
            });
        changed | self.relayout()
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = Style {
            size: taffy::Size::from_percent(1.0, 1.0),
            ..Default::default()
        };
        taffy.widget_leaf(style, map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        self.relayout();
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        let now = ui::time::now();
        self.motion = theme.motion();
        self.tick(now);
        let today = ui::clock::local_now().date_naive();
        let playing = self.music.get();
        let painted = Painted {
            forms: self
                .slots
                .iter()
                .map(|slot| (slot.id, self.board.form(slot.id)))
                .collect(),
            day: today,
            people: self.team_ids.clone(),
            reading: self.monitor.reading(),
            playing: playing.clone(),
            prefs: self.prefs.clone(),
        };
        self.dirty = self.painted.as_ref() != Some(&painted);
        self.painted = Some(painted);
        let members = if self.board.form(WidgetId::Team) == Form::Away {
            Vec::new()
        } else {
            self.members(presence)
        };
        let month = Month::around(today, ui::format::first_weekday());
        let content = Content {
            month: &month,
            members: &members,
            reading: self.monitor.reading(),
            playing: playing.as_ref(),
            cursor: self.pointer,
        };
        let drawn: Vec<Slot> = self.slots.iter().map(|slot| self.drawn(slot)).collect();
        for slot in drawn {
            let shown = self
                .shown
                .get(&slot.id)
                .map_or(0.0, |presence| presence.progress(now));
            if shown <= f64::EPSILON {
                continue;
            }
            let open = self
                .opened
                .get(&slot.id)
                .map_or(0.0, |presence| presence.progress(now).clamp(0.0, 1.0));
            let reduced = self.motion.reduced();
            // Under reduced motion nothing changes shape: the two forms
            // fade.
            let morph = if reduced { open.round() } else { open };
            let glance_radius = paint::glance_radius(slot.id, theme, slot.glance);
            let card_radius = ui::widget::floating_surface::card_radius(slot.open, theme);
            let radius = glance_radius + (card_radius - glance_radius) * morph;
            let ground = lerp_rect(slot.glance, slot.open, morph);
            let bounds = slot.glance.union(slot.open).inflate(40.0, 48.0);
            let hovered = self.hovered(slot.id);
            // A widget carried by the hand comes up off the desk: it grows
            // a little and its shadow falls further, both from the middle
            // it would grow from.
            let lift = self
                .lifts
                .get(&slot.id)
                .map_or(0.0, |lift| lift.get(now).clamp(0.0, 1.0));
            let scale = if reduced {
                1.0
            } else {
                1.0 + LIFT_SCALE * lift
            };
            let at = transform * Affine::scale_about(scale, ground.center());
            #[allow(clippy::cast_possible_truncation, reason = "a unit fraction")]
            let (glance_alpha, card_alpha) = if reduced {
                (1.0 - open as f32, open as f32)
            } else {
                (
                    (1.0 - 2.0 * morph).clamp(0.0, 1.0) as f32,
                    (2.0 * morph - 1.0).clamp(0.0, 1.0) as f32,
                )
            };
            #[allow(clippy::cast_possible_truncation, reason = "a unit fraction")]
            let alpha = shown.clamp(0.0, 1.0) as f32;
            paint::with_alpha(scene, transform, bounds, alpha, |scene| {
                paint::paint_lift(scene, at, theme, ground, radius, lift);
                if morph >= 0.5 {
                    paint::paint_card_ground(scene, at, theme, ground);
                } else {
                    paint::paint_glance_ground(scene, at, theme, slot.id, ground);
                }
                if glance_alpha > f32::EPSILON {
                    paint::with_alpha(scene, at, bounds, glance_alpha, |scene| {
                        paint::paint_glance_ground(scene, at, theme, slot.id, slot.glance);
                        self.paint_glance(scene, at, text, theme, &slot, &content);
                        if hovered && card_alpha <= f32::EPSILON {
                            let badge = paint::badge_rect(slot.glance, self.anchor(slot.id));
                            let on_badge = self.pointer.is_some_and(|p| badge.contains(p));
                            paint::paint_badge(scene, at, theme, badge, on_badge);
                        }
                    });
                }
                if card_alpha > f32::EPSILON {
                    paint::with_alpha(scene, at, bounds, card_alpha, |scene| {
                        self.paint_card(scene, at, text, theme, &slot, &content);
                    });
                }
            });
        }
        self.dirty = self.animating(now);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        let now = ui::time::now();
        if let WindowEvent::CursorMoved { position, .. } = event {
            let p = ctx.logical(position);
            self.pointer = Some(p);
            if self.grab.is_some() {
                self.grab_moved(p, now);
                self.tick(now);
                self.dirty = true;
                *ctx.handled = true;
                return;
            }
            let drawn: Vec<Slot> = self.slots.iter().map(|slot| self.drawn(slot)).collect();
            self.board.on_pointer(p, now, &drawn);
            self.tick(now);
            if self.hit_at(p).is_some() {
                *ctx.handled = true;
                self.dirty = true;
            }
            return;
        }
        if matches!(event, WindowEvent::CursorLeft { .. }) {
            self.pointer = None;
            self.board.pointer_left();
            // A press whose release will never come: what it carried goes
            // back where it was.
            if let Some(grab) = self.grab.take() {
                self.set_lift(grab.id, false);
            }
            self.resizing = None;
            self.dirty = true;
            self.tick(now);
            return;
        }
        let WindowEvent::MouseInput {
            state,
            button: MouseButton::Left,
            ..
        } = event
        else {
            return;
        };
        let Some(p) = self.pointer else {
            return;
        };
        match state {
            ElementState::Pressed => {
                let Some((id, form, rect)) = self.hit_at(p) else {
                    // A press anywhere else folds the open card.
                    if self.board.opened().is_some() {
                        self.board.close();
                        self.dirty = true;
                        self.tick(now);
                    }
                    return;
                };
                let part = self.part_at(id, form, rect, p);
                // A throw still settling is caught where it is drawn.
                let drawn_offset = self.offset(id);
                if let Some(settling) = self.offsets.get_mut(&id) {
                    settling.snap(drawn_offset);
                }
                self.grab = Some(Grab {
                    id,
                    part,
                    at: p,
                    started: now,
                    dragging: false,
                    width: self
                        .slot(id)
                        .map_or_else(|| rect.width(), |slot| paint::card_rect(slot.open).width()),
                    samples: vec![(0.0, p)],
                });
                self.dirty = true;
                self.tick(now);
                *ctx.handled = true;
            }
            ElementState::Released => {
                let Some(mut grab) = self.grab.take() else {
                    return;
                };
                if grab.samples.last().is_none_or(|(_, last)| *last != p) {
                    let elapsed = now.saturating_duration_since(grab.started).as_secs_f64();
                    grab.samples.push((elapsed, p));
                }
                self.release(grab, p);
                self.tick(now);
                *ctx.handled = true;
            }
        }
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
            || self.animating(ui::time::now())
            // A choice from Settings arrives without an event: the frame it
            // asks for is what takes it up.
            || *shared(&self.changes) != self.prefs
            // So does Escape, and the overview coming over an open card:
            // both arrive without the desk being told anything itself.
            || self.fold.asked()
            || (self.overview.get() && self.board.opened().is_some())
            // Undo, answered on the toast, reaches the desk this way.
            || !self.restores.borrow().is_empty()
            // So does what plays: the session writes the reading every frame
            // it pumps, and the next frame draws it.
            || self.music.get() != self.painted.as_ref().and_then(|painted| painted.playing.clone())
    }

    fn is_layout_dirty(&self) -> bool {
        false
    }

    /// The widgets on the desk as a screen reader reads them: each showing
    /// one is a control that opens or folds it, and its card's own controls
    /// beside it — the pin, and the remove off the desk. Each keeps its
    /// identity as the others come and go.
    fn accessibility(&self) -> Option<ui::a11y::AccessNode> {
        // The overview is over the desk: the widgets are behind it.
        if self.overview.get() {
            return None;
        }
        let mut children = Vec::new();
        for slot in &self.slots {
            let form = self.board.form(slot.id);
            if form == Form::Away || !slot.has_content {
                continue;
            }
            let title = paint::desk_widget_title(slot.id);
            let target = format!("{DESK_TARGET}{}", slot.id.key());
            let (bounds, name) = if form == Form::Open {
                (
                    slot.open,
                    crate::i18n::t_args(
                        "desk-widget-collapse",
                        &[("widget", title.as_str().into())],
                    ),
                )
            } else {
                (
                    slot.glance,
                    crate::i18n::t_args("desk-widget-open", &[("widget", title.as_str().into())]),
                )
            };
            children.push(control(target.clone(), name, bounds));
            let remove = control(
                format!("{target}.remove"),
                crate::i18n::t_args("desk-widget-remove", &[("widget", title.as_str().into())]),
                if form == Form::Open {
                    paint::remove_rect(slot.open)
                } else {
                    paint::badge_rect(slot.glance, self.anchor(slot.id))
                },
            );
            match form {
                Form::Open => {
                    let pinned = self.prefs.get(slot.id).always_on(None);
                    let pin = crate::i18n::t(if pinned {
                        "desk-widget-unpin"
                    } else {
                        "desk-widget-pin"
                    });
                    children.push(control(
                        format!("{target}.pin"),
                        pin,
                        paint::pin_rect(slot.open),
                    ));
                    children.push(remove);
                }
                _ => children.push(remove),
            }
        }
        (!children.is_empty())
            .then(|| ui::a11y::AccessNode::new(ui::a11y::Role::Group).with_children(children))
    }

    /// Carry out what assistive technology asked of a widget's control: the
    /// same open, fold, pin or remove its press performs.
    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &ui::a11y::Action,
        _state: &mut ui::widget::Edits<'_>,
        _path: &ModelPath,
    ) -> bool {
        if !matches!(action, ui::a11y::Action::Activate) {
            return false;
        }
        let Some(rest) = target.strip_prefix(DESK_TARGET) else {
            return false;
        };
        let (key, control) = match rest.rsplit_once('.') {
            Some((key, control @ ("pin" | "remove"))) => (key, Some(control)),
            _ => (rest, None),
        };
        let Some(id) = WidgetId::ALL.into_iter().find(|id| id.key() == key) else {
            return false;
        };
        let form = self.board.form(id);
        let part = match (form, control) {
            (Form::Open, Some("pin")) => Part::Pin,
            (_, Some(_)) => Part::Remove,
            (Form::Open, None) => Part::Collapse,
            (_, None) => Part::Body,
        };
        self.tap(id, form, part);
        self.tick(ui::time::now());
        self.dirty = true;
        true
    }

    fn collect_damage(&self, regions: &mut Vec<Rect>) {
        if !self.needs_redraw() {
            return;
        }
        if self.slots.is_empty() {
            regions.push(self.rect);
            return;
        }
        let bounds = self
            .slots
            .iter()
            .fold(Rect::ZERO, |bounds, slot| bounds.union(slot.open))
            .union(self.rect);
        regions.push(bounds.inflate(44.0, 52.0));
    }

    fn next_wake_deadline(&self) -> Option<Instant> {
        let now = ui::time::now();
        let fades = self
            .shown
            .values()
            .chain(self.opened.values())
            .filter_map(Presence::settles_at)
            .filter(|at| *at > now);
        // A showing calendar turns the page at midnight.
        let day = (self.board.form(WidgetId::Calendar) != Form::Away).then(|| {
            use chrono::Timelike as _;
            let secs = ui::clock::local_now().num_seconds_from_midnight();
            let left = u64::from(86_400_u32.saturating_sub(secs).max(1));
            now.checked_add(Duration::from_secs(left)).unwrap_or(now)
        });
        // A showing system monitor is sent a reading every
        // [`system_monitor::SAMPLE_EVERY`]: the loop wakes for it.
        let sample = self
            .monitor
            .is_showing()
            .then(|| now.checked_add(system_monitor::SAMPLE_EVERY))
            .flatten();
        // A song that plays moves its playhead: the frame that shows it
        // comes once a second, from the reading the session keeps current.
        let playhead = self.music.get().filter(|playing| {
            playing.playing
                && playing.progress().is_some()
                && self.slots.iter().any(|slot| {
                    slot.id == WidgetId::Music && self.board.form(slot.id) != Form::Away
                })
        });
        let playhead = playhead.and_then(|_| now.checked_add(Duration::from_secs(1)));
        let settling = self
            .offsets
            .values()
            .filter_map(ui::motion::MotionValue::settles_at);
        let lifting = self
            .lifts
            .values()
            .filter_map(ui::motion::MotionValue::settles_at);
        self.board
            .deadline(now)
            .into_iter()
            .chain(settling)
            .chain(lifting)
            .chain(day)
            .chain(sample)
            .chain(playhead)
            .chain(fades)
            .filter(|at| *at > now)
            .min()
    }

    fn name(&self) -> &'static str {
        "DeskWidgets"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

/// The choices the session keeps in step with the settings document.
fn shared(
    changes: &std::sync::Mutex<DeskWidgetPrefs>,
) -> std::sync::MutexGuard<'_, DeskWidgetPrefs> {
    changes
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn lerp_rect(a: Rect, b: Rect, t: f64) -> Rect {
    let at = |from: f64, to: f64| from + (to - from) * t;
    Rect::new(
        at(a.x0, b.x0),
        at(a.y0, b.y0),
        at(a.x1, b.x1),
        at(a.y1, b.y1),
    )
}

#[cfg(test)]
#[allow(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    reason = "test desks add margins to logical pixels and lay out in f32"
)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    use std::sync::atomic::{AtomicUsize, Ordering};

    use tilo_desktop_model::{Collaborator, IdMinter, Tab, WindowWidth, Workspace};
    use tilo_desktop_surface::{DesktopHandle, DesktopSurface, TileSource, seed_tree};
    use tilo_system_stats::protocol::{CpuStats, MemoryStats, SystemStats, ThermalZone};
    use ui::synthetic_input::{cursor_moved, mouse_press, mouse_release};
    use ui::widget::{TileContent, TileHost};

    /// The desk of the tests: no window is placed, so what a tab stands for
    /// is never asked.
    struct Tiles;

    impl TileSource for Tiles {
        fn tile(&mut self, _: &Workspace, tab: &Tab) -> TileHost {
            TileHost::new(TileContent::Notice {
                title: tab.title.clone(),
                detail: String::new(),
            })
        }
    }

    const AREA: Size = Size::new(1600.0, 900.0);
    /// A point on the desk no widget rests on.
    const BARE: Point = Point::new(800.0, 450.0);

    struct Rig {
        widgets: DeskWidgets,
        asks: Rc<RefCell<Vec<DesktopAsk>>>,
        changes: std::sync::Arc<std::sync::Mutex<DeskWidgetPrefs>>,
        /// The desk's open card as the session's keys see it, which asks it
        /// to fold.
        fold: super::super::DeskFold,
        /// Whether the overview is over the desk.
        overview: Rc<Cell<bool>>,
        /// The desk's notifications, where a removed widget's Undo is
        /// offered.
        notices: tilo_desktop_surface::DesktopNotifications,
        /// What plays, as the session keeps it beside its Quick Settings
        /// reading.
        music: DeskMusic,
    }

    /// A desk with `people` in its workspace, the calendar added, and the
    /// layer laid out over a 1600×900 display.
    fn rig(people: &[(&str, &str)]) -> Rig {
        let mut prefs = DeskWidgetPrefs::default();
        prefs.get_mut(WidgetId::Calendar).enabled = true;
        rig_with(people, prefs, Box::new(Service::default()))
    }

    /// A desk as `rig` with the choices `prefs` hold, the monitor's readings
    /// coming from `source`.
    fn rig_with(people: &[(&str, &str)], prefs: DeskWidgetPrefs, source: Box<dyn Source>) -> Rig {
        let mut minter = IdMinter::new(7);
        let mut tree = seed_tree(&mut minter, "Desk", Vec::new());
        tree.collaborators = people
            .iter()
            .map(|(id, name)| Collaborator::new(*id, *name))
            .collect();
        let handle = DesktopSurface::new(tree, 7, Box::new(Tiles)).1;
        rig_over(handle, prefs, source)
    }

    /// The layer of the tests over `handle`, with the desk's choices `prefs`
    /// and the monitor's readings from `source`.
    fn rig_over(handle: DesktopHandle, prefs: DeskWidgetPrefs, source: Box<dyn Source>) -> Rig {
        let asks = Rc::new(RefCell::new(Vec::new()));
        let changes = std::sync::Arc::new(std::sync::Mutex::new(prefs.clone()));
        let fold = super::super::DeskFold::default();
        let overview: Rc<Cell<bool>> = Rc::default();
        let notices = tilo_desktop_surface::DesktopNotifications::default();
        let music = DeskMusic::default();
        let mut widgets = DeskWidgets::new(handle, "me".into(), Rc::clone(&asks), changes.clone())
            .with_fold(fold.clone())
            .with_overview(Rc::clone(&overview))
            .with_notices(notices.clone())
            .with_music(music.clone());
        widgets = widgets.with_source(source).with_prefs(prefs);
        lay_out(&mut widgets, AREA);
        Rig {
            widgets,
            asks,
            changes,
            fold,
            overview,
            notices,
            music,
        }
    }

    /// The desk with no window on it yet, the layer laid out over it, and
    /// the frame laying the desk's windows out: what the desk is before the
    /// person opens a window.
    fn rig_before_a_window_arrives() -> (Rig, ui::headless::FrameHarness) {
        let mut minter = IdMinter::new(7);
        let tree = seed_tree(&mut minter, "Desk", Vec::new());
        let (surface, handle) = DesktopSurface::new(tree, 7, Box::new(Tiles));
        handle.set_viewport_width(AREA.width);
        let mut frame = ui::headless::FrameHarness::new(
            ui::state::State::new(),
            Box::new(surface),
            (1600, 900),
        );
        frame.relayout();
        let mut prefs = DeskWidgetPrefs::default();
        prefs.get_mut(WidgetId::Calendar).enabled = true;
        let rig = rig_over(handle, prefs, Box::new(Service::default()));
        (rig, frame)
    }

    /// A window spanning the whole strip showing on the desk `handle` draws,
    /// laid out by `frame`. Returns the window's rect.
    fn show_a_window(handle: &DesktopHandle, frame: &mut ui::headless::FrameHarness) -> Rect {
        let tree = handle.read(|tree| {
            let mut tree = tree.clone();
            let strip = tree
                .active_workspace_mut()
                .and_then(|workspace| workspace.strips.first_mut())
                .expect("the desk has a strip");
            strip.window_groups.push(tilo_desktop_model::WindowGroup {
                id: 4242,
                active_tab: 4343,
                tabs: vec![Tab {
                    id: 4343,
                    title: "Notes".into(),
                    tile: tilo_desktop_model::TileContent::AppPlaceholder {
                        kind: tilo_desktop_model::AppKind::Other("so.tilo.notes".into()),
                        label: "Notes".into(),
                    },
                    app_title: None,
                }],
                width: WindowWidth::Fraction(1000),
                maximized: false,
                focus_history: Vec::new(),
            });
            tree
        });
        handle.replace_tree(tree);
        frame.relayout();
        handle
            .shown_tabs()
            .into_iter()
            .next()
            .expect("the window is shown")
            .1
    }

    /// Lay the layer out over `area`, as the desk around it does.
    fn lay_out(widgets: &mut DeskWidgets, area: Size) {
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = widgets.build_layout(&mut taffy, &Theme::default(), &mut map);
        let available = taffy::Size {
            width: taffy::AvailableSpace::Definite(area.width as f32),
            height: taffy::AvailableSpace::Definite(area.height as f32),
        };
        taffy
            .compute_layout_with_measure(node, available, |_, _, _, _, _| taffy::Size::ZERO)
            .expect("a layout");
        widgets.apply_layout(&taffy, node, Point::ZERO);
        widgets.tick(Instant::now());
    }

    impl Rig {
        fn send(&mut self, event: &WindowEvent) -> bool {
            ui::testing::Dispatch::new()
                .send(&mut self.widgets, event)
                .handled
        }

        /// The person clicks at `at`: the pointer is there, the button goes
        /// down and comes up again. Returns whether the desk took the press.
        fn click(&mut self, at: Point) -> bool {
            self.send(&cursor_moved(at.x, at.y));
            let pressed = self.send(&mouse_press());
            self.send(&mouse_release());
            pressed
        }

        fn form(&self, id: WidgetId) -> Form {
            self.widgets.form(id)
        }

        fn slot(&self, id: WidgetId) -> Slot {
            self.widgets.slot(id).expect("the widget is laid out")
        }

        fn prefs(&self, id: WidgetId) -> WidgetPrefs {
            self.widgets.prefs().get(id).clone()
        }

        /// The pointer moves to `at`, without pressing anything.
        fn point_at(&mut self, at: Point) {
            self.send(&cursor_moved(at.x, at.y));
        }

        /// The newest toast on the desk's notifications.
        fn toast(&self) -> tilo_desktop_surface::notifications::Notification {
            self.notices
                .feed()
                .banners
                .last()
                .map(|banner| banner.notification.clone())
                .expect("a toast is up")
        }
    }

    fn centre(rect: Rect) -> Point {
        Point::new(rect.x0 + rect.width() / 2.0, rect.y0 + rect.height() / 2.0)
    }

    #[test]
    fn the_desk_shows_who_is_here_and_today_and_a_tap_opens_the_card() {
        let mut rig = rig(&[("ada", "Ada"), ("bo", "Bo")]);
        rig.point_at(BARE);
        assert_eq!(
            rig.form(WidgetId::Team),
            Form::Glance,
            "the people here show"
        );
        assert_eq!(rig.form(WidgetId::Calendar), Form::Glance, "today shows");
        let members = rig.widgets.members(&PresenceCtx::default());
        assert_eq!(
            members
                .iter()
                .map(|member| member.row.label.as_str())
                .collect::<Vec<_>>(),
            vec!["Ada", "Bo"],
            "the card lists the people in this workspace"
        );

        let glance = rig.slot(WidgetId::Calendar).glance;
        assert!(rig.click(centre(glance)), "the tap lands on the widget");
        assert_eq!(rig.form(WidgetId::Calendar), Form::Open, "a tap opens it");
        let open = rig.slot(WidgetId::Calendar).open;
        assert!(
            open.width() > glance.width() && open.height() > glance.height(),
            "the card is bigger than the form it grew from: {open:?}"
        );

        // The card folds from its own control, and from a click on the desk
        // beside it.
        let collapse = centre(paint::collapse_rect(open));
        assert!(rig.click(collapse));
        assert_eq!(rig.form(WidgetId::Calendar), Form::Glance, "folded");
        assert!(rig.click(centre(glance)));
        assert_eq!(rig.form(WidgetId::Calendar), Form::Open);
        assert!(!rig.click(BARE));
        assert_eq!(
            rig.form(WidgetId::Calendar),
            Form::Glance,
            "a click on the desk folds the card"
        );
    }

    #[test]
    fn an_open_card_folds_when_escape_asks_or_the_overview_comes_over_it() {
        let mut rig = rig(&[("ada", "Ada")]);
        rig.point_at(BARE);
        let glance = rig.slot(WidgetId::Team).glance;
        assert!(rig.click(centre(glance)));
        assert_eq!(rig.form(WidgetId::Team), Form::Open);
        assert!(rig.fold.is_open(), "the keys know a card is open");

        // Escape, from the session's keys, folds the card.
        rig.fold.ask();
        rig.widgets.tick(Instant::now());
        assert_eq!(rig.form(WidgetId::Team), Form::Glance, "Escape folds it");
        assert!(!rig.fold.is_open());

        // The ask is spent: the next card opened stays open.
        assert!(rig.click(centre(glance)));
        rig.widgets.tick(Instant::now());
        assert_eq!(rig.form(WidgetId::Team), Form::Open);

        // The overview comes over the desk: the card folds under it.
        rig.overview.set(true);
        rig.widgets.tick(Instant::now());
        assert_eq!(
            rig.form(WidgetId::Team),
            Form::Glance,
            "the overview covers the open card"
        );
    }

    #[test]
    fn a_widget_taken_off_the_desk_offers_undo_which_puts_it_back() {
        let mut rig = rig(&[("ada", "Ada")]);
        rig.point_at(BARE);
        let glance = rig.slot(WidgetId::Calendar).glance;
        assert!(rig.click(centre(glance)));
        let open = rig.slot(WidgetId::Calendar).open;

        // The person removes the widget from its open card.
        assert!(rig.click(centre(paint::remove_rect(open))));
        assert_eq!(
            rig.form(WidgetId::Calendar),
            Form::Away,
            "taken off the desk"
        );
        let toast = rig.toast();
        assert_eq!(toast.title, "Calendar removed from the desktop");
        assert_eq!(
            toast.actions,
            [("default".to_owned(), "Undo".to_owned())],
            "the toast offers Undo"
        );

        // Undo, chosen on the toast, puts it back where it was.
        rig.notices
            .answer(tilo_desktop_surface::notifications::BannerAsk::Activate(
                toast.id,
            ));
        rig.widgets.tick(Instant::now());
        assert_eq!(
            rig.form(WidgetId::Calendar),
            Form::Glance,
            "back on the desk"
        );
        assert!(
            rig.prefs(WidgetId::Calendar).enabled,
            "and switched on again"
        );

        // Thrown off the screen, the widget offers the same way back.
        let glance = rig.slot(WidgetId::Calendar).glance;
        let from = centre(glance);
        let t0 = Instant::now();
        ui::time::testing::with_monotonic_time(t0, || {
            rig.send(&cursor_moved(from.x, from.y));
            assert!(rig.send(&mouse_press()));
        });
        ui::time::testing::with_monotonic_time(t0 + Duration::from_millis(16), || {
            rig.send(&cursor_moved(600.0, from.y));
        });
        ui::time::testing::with_monotonic_time(t0 + Duration::from_millis(32), || {
            rig.send(&cursor_moved(20.0, from.y));
            rig.send(&mouse_release());
        });
        assert_eq!(
            rig.form(WidgetId::Calendar),
            Form::Away,
            "thrown off the desk"
        );
        let toast = rig.toast();
        assert_eq!(toast.title, "Calendar removed from the desktop");
        rig.notices
            .answer(tilo_desktop_surface::notifications::BannerAsk::Activate(
                toast.id,
            ));
        rig.widgets.tick(Instant::now());
        assert_eq!(
            rig.form(WidgetId::Calendar),
            Form::Glance,
            "and back from the throw"
        );
    }

    #[test]
    fn a_window_coming_over_the_desk_folds_the_open_card() {
        let (mut rig, mut frame) = rig_before_a_window_arrives();
        rig.point_at(BARE);
        let glance = rig.slot(WidgetId::Calendar).glance;
        assert!(rig.click(centre(glance)), "the person opened the card");
        assert_eq!(rig.form(WidgetId::Calendar), Form::Open);
        assert!(
            rig.widgets.handle.shown_tabs().is_empty(),
            "nothing is over the desk yet"
        );

        let window = show_a_window(&rig.widgets.handle, &mut frame);
        assert!(
            window.intersect(rig.slot(WidgetId::Calendar).open).area() > 0.0,
            "the window stands over the open card: {window:?}"
        );
        rig.widgets.tick(Instant::now());
        assert_eq!(
            rig.form(WidgetId::Calendar),
            Form::Glance,
            "a window over the desk folds the open card"
        );
    }

    #[test]
    fn a_desk_with_nobody_else_on_it_shows_no_team() {
        let mut rig = rig(&[]);
        rig.point_at(BARE);
        assert_eq!(
            rig.form(WidgetId::Team),
            Form::Away,
            "nobody else here: nothing to list"
        );
        assert!(
            !rig.click(centre(rig.slot(WidgetId::Team).glance)),
            "and nothing takes the click"
        );
        assert_eq!(rig.form(WidgetId::Calendar), Form::Glance);
    }

    #[test]
    fn pinning_keeps_the_widget_on_the_desk_and_remove_puts_it_away() {
        let mut rig = rig(&[("ada", "Ada")]);
        rig.point_at(BARE);
        let glance = rig.slot(WidgetId::Calendar).glance;
        assert!(rig.click(centre(glance)));
        let open = rig.slot(WidgetId::Calendar).open;
        assert_eq!(rig.form(WidgetId::Calendar), Form::Open);

        assert!(rig.click(centre(paint::pin_rect(open))));
        assert_eq!(
            rig.prefs(WidgetId::Calendar).show,
            ShowMode::Always,
            "pinned: it stays while the person works"
        );
        assert_eq!(rig.form(WidgetId::Calendar), Form::Open, "and stays open");

        // The pin again lets it go quiet as before.
        assert!(rig.click(centre(paint::pin_rect(open))));
        assert_eq!(rig.prefs(WidgetId::Calendar).show, ShowMode::Pause);

        // Removing it from the card takes it off the desk.
        assert!(rig.click(centre(paint::remove_rect(open))));
        assert_eq!(rig.form(WidgetId::Calendar), Form::Away);
        assert!(!rig.prefs(WidgetId::Calendar).enabled);

        // A widget away keeps its resting form's badge: the person takes it
        // off the desk from there.
        assert_eq!(rig.form(WidgetId::Team), Form::Glance);
        let team = rig.slot(WidgetId::Team);
        rig.point_at(centre(team.glance));
        let badge = centre(paint::badge_rect(
            team.glance,
            rig.widgets.anchor(WidgetId::Team),
        ));
        assert!(rig.click(badge), "the badge is the widget's");
        assert!(!rig.prefs(WidgetId::Team).enabled);
        assert_eq!(rig.form(WidgetId::Team), Form::Away);
    }

    #[test]
    fn a_row_on_the_team_card_opens_that_person() {
        let mut rig = rig(&[("ada", "Ada"), ("bo", "Bo")]);
        rig.point_at(BARE);
        assert!(rig.click(centre(rig.slot(WidgetId::Team).glance)));
        assert_eq!(rig.form(WidgetId::Team), Form::Open);
        let card = paint::card_rect(rig.slot(WidgetId::Team).open);
        let stage = sizing::dominant(WidgetId::Team, card.width());
        let rows = paint::team_rows(card, stage, 2);
        assert_eq!(rows.len(), 2, "both are listed");
        assert!(rig.click(centre(rows[1])));
        let asks = rig.asks.borrow();
        assert!(
            matches!(
                asks.as_slice(),
                [DesktopAsk::Intent(ChromeIntent::ShowPerson(id))] if id == "bo"
            ),
            "the row opens that person: {asks:?}"
        );
    }

    #[test]
    fn the_music_widget_shows_what_plays_and_steers_the_player_it_shows() {
        let mut rig = rig(&[]);
        rig.point_at(BARE);
        assert_eq!(
            rig.form(WidgetId::Music),
            Form::Away,
            "nothing plays: nothing to show"
        );

        // What the player reports is what the widget shows.
        rig.music.set(Some(NowPlaying {
            title: "Clair de lune".into(),
            subtitle: "Claude Debussy".into(),
            player: "Music".into(),
            playing: true,
            can_toggle: true,
            can_previous: true,
            can_next: true,
            position_ms: 60_000.0,
            duration_ms: 240_000.0,
            ..NowPlaying::default()
        }));
        rig.widgets.tick(Instant::now());
        assert_eq!(
            rig.form(WidgetId::Music),
            Form::Glance,
            "the song is on the desk"
        );
        let glance = rig.slot(WidgetId::Music).glance;

        // The play control on the glancing widget pauses what plays.
        assert!(rig.click(centre(paint::music_glance_play(glance))));
        assert_eq!(
            rig.asks.borrow().as_slice(),
            [DesktopAsk::Music(MusicAction::PlayPause)],
            "the widget asks the session to pause it"
        );

        // The card it opens carries the whole transport.
        rig.asks.borrow_mut().clear();
        assert!(rig.click(centre(glance)));
        assert_eq!(rig.form(WidgetId::Music), Form::Open);
        let card = paint::card_rect(rig.slot(WidgetId::Music).open);
        let stage = sizing::dominant(WidgetId::Music, card.width());
        let next = paint::music_transport(card, stage)
            .into_iter()
            .find(|(action, _)| *action == MusicAction::Next)
            .map(|(_, rect)| rect)
            .expect("the open card offers the next song");
        assert!(rig.click(centre(next)));
        assert_eq!(
            rig.asks.borrow().as_slice(),
            [DesktopAsk::Music(MusicAction::Next)],
            "skip asks the session for the next song"
        );

        // The song ends: the widget goes with it.
        rig.music.set(None);
        rig.widgets.tick(Instant::now());
        assert_eq!(rig.form(WidgetId::Music), Form::Away, "nothing plays again");
    }

    #[test]
    fn a_reader_finds_the_widgets_and_can_open_and_pin_one() {
        // What a screen reader hears on the desk, in order.
        fn heard(rig: &Rig) -> Vec<(String, String, Rect)> {
            rig.widgets
                .accessibility()
                .map(|node| {
                    node.children
                        .into_iter()
                        .filter_map(|child| {
                            Some((child.action_target?, child.name?, child.bounds?.to_rect()))
                        })
                        .collect()
                })
                .unwrap_or_default()
        }
        let mut rig = rig(&[("ada", "Ada")]);
        rig.point_at(BARE);
        let resting = heard(&rig);
        let names: Vec<&str> = resting.iter().map(|(_, name, _)| name.as_str()).collect();
        assert!(
            names.contains(&"Open People here"),
            "each showing widget is a control: {names:?}"
        );
        assert!(names.contains(&"Open Calendar"));
        assert!(
            !resting
                .iter()
                .any(|(target, ..)| target.rsplit('.').next() == Some("pin")),
            "a folded widget has no pin to reach: {resting:?}"
        );

        // Activating the calendar's control opens it, as a press does.
        let calendar = resting
            .iter()
            .find(|(target, ..)| target == "desk.widget:calendar")
            .map(|(target, ..)| target.clone())
            .expect("the calendar is a control");
        let mut doc = automerge::AutoCommit::new();
        assert!(rig.widgets.perform_semantic_action(
            &calendar,
            &ui::a11y::Action::Activate,
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
        assert_eq!(
            rig.form(WidgetId::Calendar),
            Form::Open,
            "activating it opens the card"
        );

        // The open card offers folding it back, its pin and its remove, where
        // they are drawn.
        let open = rig.slot(WidgetId::Calendar).open;
        let card = heard(&rig);
        let where_it_is = |rect: Rect, controls: &[(String, String, Rect)], name: &str| {
            controls
                .iter()
                .any(|(_, heard, bounds)| heard == name && bounds.contains(centre(rect)))
        };
        assert!(
            where_it_is(
                paint::pin_rect(open),
                &card,
                "Always show this widget on this workspace"
            ),
            "the pin stands on the card's control: {card:?}"
        );
        assert!(where_it_is(
            paint::remove_rect(open),
            &card,
            "Remove Calendar"
        ));
        assert!(
            card.iter()
                .any(|(_, name, bounds)| name == "Fold Calendar back" && *bounds == open),
            "folding is the card itself: {card:?}"
        );

        // Its pin keeps the widget on the desk, as pressing it does.
        let pin = card
            .iter()
            .find(|(target, ..)| target == "desk.widget:calendar.pin")
            .map(|(target, ..)| target.clone())
            .expect("the open card offers its pin");
        assert!(rig.widgets.perform_semantic_action(
            &pin,
            &ui::a11y::Action::Activate,
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT,
        ));
        assert_eq!(
            rig.prefs(WidgetId::Calendar).show,
            ShowMode::Always,
            "activating the pin pins it"
        );

        // With the overview over the desk, the widgets are behind it.
        rig.overview.set(true);
        assert!(
            rig.widgets.accessibility().is_none(),
            "the reader meets the overview, not the desk behind it"
        );
    }

    #[test]
    fn a_change_in_settings_reaches_the_desk_without_a_restart() {
        let mut rig = rig(&[("ada", "Ada")]);
        rig.point_at(BARE);
        assert_eq!(rig.form(WidgetId::Team), Form::Glance);

        let mut prefs = rig.widgets.prefs();
        prefs.get_mut(WidgetId::Team).enabled = false;
        prefs.get_mut(WidgetId::Calendar).width = Some(420.0);
        *shared(&rig.changes) = prefs;
        rig.point_at(BARE);
        assert_eq!(rig.form(WidgetId::Team), Form::Away, "taken off the desk");
        let open = rig.slot(WidgetId::Calendar).open;
        assert!(
            (open.width() - 420.0).abs() < 0.5,
            "the card takes the width the person chose: {open:?}"
        );
    }

    #[test]
    fn the_desk_keeps_the_clicks_beside_the_widgets() {
        let mut rig = rig(&[("ada", "Ada")]);
        assert!(!rig.send(&cursor_moved(BARE.x, BARE.y)), "the desk's own");
        assert!(!rig.send(&mouse_press()), "the desk's own");
        assert!(
            !rig.send(&mouse_release()),
            "a release it never pressed for"
        );
        let glance = centre(rig.slot(WidgetId::Calendar).glance);
        assert!(rig.send(&cursor_moved(glance.x, glance.y)), "on the widget");
        assert!(rig.send(&mouse_press()));
        assert!(rig.send(&mouse_release()));
    }

    #[test]
    fn a_widget_carried_by_the_hand_comes_up_off_the_desk() {
        let mut rig = rig(&[("ada", "Ada")]);
        rig.point_at(BARE);
        let glance = rig.slot(WidgetId::Calendar).glance;
        let from = centre(glance);
        let t0 = Instant::now();
        ui::time::testing::with_monotonic_time(t0, || {
            rig.send(&cursor_moved(from.x, from.y));
            assert!(rig.send(&mouse_press()));
        });
        assert!(
            rig.widgets.lift(WidgetId::Calendar) <= f64::EPSILON,
            "resting on the desk while the press is still a press"
        );
        ui::time::testing::with_monotonic_time(t0 + Duration::from_millis(40), || {
            rig.send(&cursor_moved(from.x + 30.0, from.y));
        });
        // The hand carries it: under it the widget has come up, its shadow
        // further from the desk than any resting one.
        let lifted =
            ui::time::testing::with_monotonic_time(t0 + Duration::from_millis(160), || {
                rig.widgets.lift(WidgetId::Calendar)
            });
        assert!(lifted > 0.5, "carried by the hand: lifted {lifted}");

        // Let go: it settles back onto the desk.
        ui::time::testing::with_monotonic_time(t0 + Duration::from_millis(200), || {
            rig.send(&mouse_release());
        });
        let down = ui::time::testing::with_monotonic_time(t0 + Duration::from_millis(900), || {
            rig.widgets.lift(WidgetId::Calendar)
        });
        assert!(down <= f64::EPSILON, "set down: lifted {down}");
    }

    /// The machine's readings as the tests give them, and how often the
    /// monitor asked for one.
    #[derive(Default, Clone)]
    struct Fake {
        queued: std::sync::Arc<std::sync::Mutex<Option<SystemStats>>>,
        started: std::sync::Arc<AtomicUsize>,
        stopped: std::sync::Arc<AtomicUsize>,
    }

    impl Fake {
        fn send(&self, stats: SystemStats) {
            *self.queued.lock().expect("a reading slot") = Some(stats);
        }

        fn started(&self) -> usize {
            self.started.load(Ordering::SeqCst)
        }

        fn stopped(&self) -> usize {
            self.stopped.load(Ordering::SeqCst)
        }
    }

    impl Source for Fake {
        fn start(&mut self) {
            self.started.fetch_add(1, Ordering::SeqCst);
        }

        fn stop(&mut self) {
            self.stopped.fetch_add(1, Ordering::SeqCst);
        }

        fn latest(&mut self) -> Option<SystemStats> {
            self.queued.lock().expect("a reading slot").take()
        }
    }

    /// The machine's answer: `cpu` (none until a second reading), memory
    /// three quarters full, and two sensors of which one is nonsense.
    fn stats(cpu: Option<f32>) -> SystemStats {
        SystemStats {
            cpu: cpu.map(|total| CpuStats {
                total,
                per_core: vec![total],
            }),
            memory: MemoryStats {
                total: 16_000,
                available: 4_000,
                ..MemoryStats::default()
            },
            thermal: vec![
                ThermalZone {
                    name: "cpu".into(),
                    celsius: 47.25,
                },
                ThermalZone {
                    name: "gone".into(),
                    celsius: -273.0,
                },
            ],
            ..SystemStats::default()
        }
    }

    #[test]
    fn the_monitor_shows_the_machines_readings_only_while_it_is_up() {
        let source = Fake::default();
        let probes = source.clone();
        let mut prefs = DeskWidgetPrefs::default();
        prefs.get_mut(WidgetId::Calendar).enabled = true;
        let mut monitor = prefs.get_mut(WidgetId::SystemMonitor).clone();
        monitor.enabled = true;
        monitor.show = ShowMode::Always;
        *prefs.get_mut(WidgetId::SystemMonitor) = monitor;
        let mut rig = rig_with(&[("ada", "Ada")], prefs, Box::new(source));

        assert_eq!(
            rig.form(WidgetId::SystemMonitor),
            Form::Glance,
            "pinned: it is on the desk"
        );
        assert_eq!(probes.started(), 1, "showing asks the machine");
        probes.send(stats(None));
        rig.widgets.tick(Instant::now());
        assert_eq!(
            rig.widgets.monitor.reading(),
            Reading {
                cpu_percent: None,
                memory_percent: Some(75),
                temperature_celsius: Some(47),
            },
            "one reading: memory and the warmest sensor"
        );
        probes.send(stats(Some(41.4)));
        rig.widgets.tick(Instant::now());
        assert_eq!(
            rig.widgets.monitor.reading().cpu_percent,
            Some(41),
            "and load once there are two"
        );
        assert_eq!(probes.started(), 1, "one subscription while it shows");

        // Taken off the desk, it stops reading the machine.
        let mut prefs = rig.widgets.prefs();
        prefs.get_mut(WidgetId::SystemMonitor).enabled = false;
        *shared(&rig.changes) = prefs;
        rig.widgets.tick(Instant::now());
        assert_eq!(rig.form(WidgetId::SystemMonitor), Form::Away);
        assert_eq!(probes.stopped(), 1, "away: no subscription");
    }

    #[test]
    fn a_widget_dropped_where_the_hand_let_go_keeps_that_place_there() {
        let mut rig = rig(&[("ada", "Ada")]);
        rig.point_at(BARE);
        let glance = rig.slot(WidgetId::Calendar).glance;
        let from = centre(glance);
        let to = Point::new(500.0, 300.0);

        rig.send(&cursor_moved(from.x, from.y));
        assert!(rig.send(&mouse_press()));
        rig.send(&cursor_moved(to.x, to.y));
        // Let go where the hand stopped: a still hand throws nothing.
        rig.send(&cursor_moved(to.x, to.y));
        rig.send(&mouse_release());

        let places = rig.prefs(WidgetId::Calendar).places;
        assert_eq!(places.len(), 1, "it remembers one place: {places:?}");
        let dropped = rig.slot(WidgetId::Calendar).glance;
        let let_go = glance + (to - from);
        assert!(
            (dropped.x0 - let_go.x0).abs() < 1.0 && (dropped.y0 - let_go.y0).abs() < 1.0,
            "it rests where it was let go: {dropped:?} from {let_go:?}"
        );

        // The place is the person's, not this desk's: the next desk with
        // the same choices puts it there.
        let prefs = rig.widgets.prefs();
        let mut again = rig_with(&[("ada", "Ada")], prefs, Box::new(Service::default()));
        again.point_at(BARE);
        let back = again.slot(WidgetId::Calendar).glance;
        assert!(
            (back.x0 - dropped.x0).abs() < 1.0 && (back.y0 - dropped.y0).abs() < 1.0,
            "and is there again next time: {back:?} against {dropped:?}"
        );
    }

    #[test]
    fn a_card_resized_from_its_handle_keeps_the_width_it_was_given() {
        let mut rig = rig(&[("ada", "Ada")]);
        rig.point_at(BARE);
        assert!(rig.click(centre(rig.slot(WidgetId::Calendar).glance)));
        let open = rig.slot(WidgetId::Calendar).open;
        assert_eq!(rig.form(WidgetId::Calendar), Form::Open);
        let anchor = rig.widgets.anchor(WidgetId::Calendar);
        let handle = paint::grip_rect(paint::card_rect(open), anchor);
        let grip = centre(handle);

        // The handle at the card's free corner follows the hand: away from
        // that corner widens the card.
        let toward = if anchor.0 { -1.0 } else { 1.0 };
        rig.send(&cursor_moved(grip.x, grip.y));
        assert!(rig.send(&mouse_press()), "the handle takes the press");
        rig.send(&cursor_moved(grip.x + toward * 60.0, grip.y));
        rig.send(&mouse_release());

        let width = rig.prefs(WidgetId::Calendar).width.expect("a chosen width");
        assert!(
            (width - (open.width() + 60.0)).abs() < 1.0,
            "the card is 60 wider: {width} from {}",
            open.width()
        );

        // The width is the person's: the next desk with the same choices
        // opens the card just as wide.
        let prefs = rig.widgets.prefs();
        let mut again = rig_with(&[("ada", "Ada")], prefs, Box::new(Service::default()));
        again.point_at(BARE);
        assert!(again.click(centre(again.slot(WidgetId::Calendar).glance)));
        let reopened = again.slot(WidgetId::Calendar).open;
        assert!(
            (reopened.width() - width).abs() < 1.0,
            "as wide as it was left: {} against {width}",
            reopened.width()
        );
    }

    struct StubSurface;

    impl Widget for StubSurface {
        fn build_layout(
            &mut self,
            taffy: &mut TaffyTree<()>,
            _theme: &Theme,
            map: &mut HashMap<NodeId, *mut dyn Widget>,
        ) -> NodeId {
            let style = Style {
                size: taffy::Size::from_percent(1.0, 1.0),
                ..Default::default()
            };
            taffy.widget_leaf(style, map, self)
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

        fn get_rect(&self) -> Rect {
            Rect::ZERO
        }

        fn name(&self) -> &'static str {
            "StubSurface"
        }
    }

    #[test]
    fn the_desk_lays_the_widgets_under_its_windows_and_hands_them_the_pointer() {
        let mut minter = IdMinter::new(7);
        let tree = seed_tree(&mut minter, "Desk", Vec::new());
        let handle = DesktopSurface::new(tree, 7, Box::new(Tiles)).1;
        let mut prefs = DeskWidgetPrefs::default();
        prefs.get_mut(WidgetId::Calendar).enabled = true;
        let changes = std::sync::Arc::new(std::sync::Mutex::new(prefs.clone()));
        let widgets = DeskWidgets::new(
            handle.clone(),
            "me".into(),
            Rc::new(RefCell::new(Vec::new())),
            std::sync::Arc::clone(&changes),
        )
        .with_prefs(prefs);
        let mut layer = crate::desktop_host::DeskLayer::new(
            Box::new(StubSurface),
            handle,
            crate::launchpad::CollaborationLaunchpad::new(),
            vec![Box::new(widgets)],
        );
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let mut map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
        let node = layer.build_layout(&mut taffy, &Theme::default(), &mut map);
        let available = taffy::Size {
            width: taffy::AvailableSpace::Definite(AREA.width as f32),
            height: taffy::AvailableSpace::Definite(AREA.height as f32),
        };
        taffy
            .compute_layout_with_measure(node, available, |_, _, _, _, _| taffy::Size::ZERO)
            .expect("a layout");
        layer.apply_layout(&taffy, node, Point::ZERO);

        // The calendar rests down the right edge of the desk's area, which
        // starts under the bar.
        let area = layer.area().get_rect();
        let glance = placement::rect_at(area, paint::CALENDAR_GLANCE, (1.0, 0.25));
        let badge = centre(paint::badge_rect(glance, placement::anchor((1.0, 0.25))));
        let mut dispatch = ui::testing::Dispatch::new().layout(&taffy, node);
        // The pointer comes to the widget from the desk beside it.
        dispatch.send(&mut layer, &cursor_moved(800.0, 600.0));
        assert!(
            dispatch
                .send(&mut layer, &cursor_moved(badge.x, badge.y))
                .handled,
            "the widget takes the pointer over the desk"
        );
        assert!(dispatch.send(&mut layer, &mouse_press()).handled);
        dispatch.send(&mut layer, &mouse_release());
        assert!(
            !shared(&changes).get(WidgetId::Calendar).enabled,
            "the badge took the widget off the desk"
        );
    }

    #[test]
    fn the_widgets_wake_the_loop_when_they_next_change() {
        let rig = rig(&[("ada", "Ada")]);
        let wake = rig.widgets.next_wake_deadline().expect("a deadline");
        assert!(wake > Instant::now(), "a deadline in the future");
        assert!(
            rig.widgets.needs_redraw(),
            "a desk just laid out paints once"
        );
    }
}
