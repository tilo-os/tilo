//! The desk's widgets as one layer: it lays them out where the person keeps
//! them, decides the form each is in ([`DeskBoard`]) and paints it there.
//!
//! Widget content is what the session already holds: the people in this
//! workspace come from the tree the desk shows, the date from the clock.
//! Nothing is polled: the layer reads the tree it is mounted over, and the
//! person's choices ([`DeskWidgetPrefs`]) arrive from the settings document
//! the session watches.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use taffy::{NodeId, Style, TaffyTree};
use tilo_desktop_surface::DesktopHandle;
use ui::desk_widgets::{DeskWidgetPrefs, ShowMode, WidgetId, WidgetPrefs};
use ui::motion::{Ambient, Presence, Zoom};
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::time::{Duration, Instant};
use ui::tokens::{EffectiveMotion, MotionSource};
use ui::widget::{AppModel, EventCtx, ModelPath, TaffyTreeExt as _, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Size};
use winit::event::{ElementState, MouseButton, WindowEvent};

use super::calendar::Month;
use super::paint::{self, BAND};
use super::placement::{self, MARGIN};
use super::sizing;
use super::team::{self, TeamMember};
use super::{Context, DeskBoard, Form, Slot};
use crate::chrome::ChromeIntent;
use crate::desktop_host::DesktopAsk;

/// The widgets this layer shows. What is playing and how busy the machine
/// is join them with the readings they show.
const SHOWN: [WidgetId; 2] = [WidgetId::Team, WidgetId::Calendar];

/// Which part of a widget a press landed on.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Part {
    /// The form itself: a tap opens a glance, and on an open card folds it.
    Body,
    Collapse,
    Pin,
    Remove,
    /// The row of the person with this roster id.
    Person(String),
}

/// A press being held on a widget.
#[derive(Clone, Debug)]
struct Press {
    id: WidgetId,
    part: Part,
    at: Point,
}

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
    press: Option<Press>,
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
}

/// What the widgets fill themselves with in a frame: the day the calendar
/// shows, the people the team card lists, and where the pointer is.
struct Content<'a> {
    month: &'a Month,
    members: &'a [TeamMember],
    cursor: Option<Point>,
}

/// What the desk showed in a frame.
#[derive(Clone, Debug, PartialEq)]
struct Painted {
    forms: Vec<(WidgetId, Form)>,
    day: chrono::NaiveDate,
    people: Vec<String>,
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
            press: None,
            team_ids: Vec::new(),
            motion: EffectiveMotion::default(),
            painted: None,
            dirty: true,
            rect: Rect::ZERO,
        }
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

    /// Where `id`'s resting form and open card are, and whether it has
    /// anything to show.
    fn lay(&self, id: WidgetId, people: usize) -> (Slot, f64) {
        let prefs = self.prefs.get(id).clone();
        let at = prefs
            .places
            .first()
            .map_or_else(|| id.default_place(), |place| (place.x, place.y));
        let area = self.rect;
        let glance = match id {
            WidgetId::Team => {
                // Against a side edge (not in a bottom corner) the faces
                // stack down it; anywhere else they line up across.
                let vertical = (at.0 <= 0.01 || at.0 >= 0.99) && at.1 < 0.99;
                placement::rect_at(area, paint::team_glance_size(vertical, people), at)
            }
            WidgetId::Calendar => placement::rect_at(area, paint::CALENDAR_GLANCE, at),
            WidgetId::Music | WidgetId::SystemMonitor => Rect::ZERO,
        };
        let room = (area.width() - 2.0 * MARGIN).max(1.0);
        let sizing = sizing::sizing(id);
        let width = sizing
            .clamp(prefs.width.unwrap_or(sizing.default))
            .min(room);
        let height = sizing::height(id, width, |stage, width| match id {
            WidgetId::Team => paint::team_card_height(stage, people),
            WidgetId::Calendar => paint::calendar_card_height(stage, width),
            WidgetId::Music | WidgetId::SystemMonitor => 0.0,
        });
        let open = Size::new(width, height + BAND);
        let has_content = match id {
            WidgetId::Team => people > 0,
            WidgetId::Calendar => true,
            WidgetId::Music | WidgetId::SystemMonitor => false,
        };
        let slot = Slot {
            id,
            glance,
            open: placement::open_rect(area, glance, open),
            reach: placement::reach_zone(area, glance),
            has_content,
        };
        (slot, width)
    }

    /// Lay every widget out where the person put it. Returns whether
    /// anything moved.
    fn relayout(&mut self) -> bool {
        if self.rect.width() <= 0.0 || self.rect.height() <= 0.0 {
            return false;
        }
        let people = self.team_rows();
        let mut slots: Vec<Slot> = SHOWN
            .into_iter()
            .map(|id| self.lay(id, people.len()).0)
            .collect();
        // Two widgets resting on each other step apart from the display's
        // middle.
        let mut rects: Vec<Rect> = slots.iter().map(|slot| slot.glance).collect();
        placement::resolve(self.rect, &mut rects, &[]);
        for (slot, rect) in slots.iter_mut().zip(rects) {
            if slot.has_content {
                slot.glance = rect;
                slot.reach = placement::reach_zone(self.rect, rect);
                let open = slot.open.size();
                slot.open = placement::open_rect(self.rect, rect, open);
            }
        }
        let changed = slots != self.slots;
        self.slots = slots;
        // What a press on a row opens is the row as drawn.
        self.team_ids = people;
        changed
    }

    /// The widget form under `p`, if one is showing there: the topmost, as
    /// painted. A glance's remove badge is part of it.
    fn hit_at(&self, p: Point) -> Option<(WidgetId, Form, Rect)> {
        self.slots.iter().rev().find_map(|slot| {
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
                    Part::Remove
                } else {
                    Part::Body
                }
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
                Part::Body
            }
        }
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
            Part::Remove => {
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
            }
            Part::Collapse => self.board.close(),
            Part::Person(person) => self
                .asks
                .borrow_mut()
                .push(DesktopAsk::Intent(ChromeIntent::ShowPerson(person))),
            Part::Body if form == Form::Glance => self.board.open(id),
            Part::Body => {}
        }
        self.dirty = true;
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
        let fresh = shared(&self.changes).clone();
        if fresh != self.prefs {
            self.prefs = fresh;
        }
        if self.prefs != self.laid_from {
            self.laid_from = self.prefs.clone();
            changed = true;
        }
        changed |= self.relayout();
        let covered = false;
        let engaged = self
            .press
            .as_ref()
            .map(|press| press.id)
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
        if changed {
            self.dirty = true;
        }
        changed
    }

    /// Whether a widget is still coming, going, or changing shape.
    fn animating(&self, now: Instant) -> bool {
        self.shown
            .values()
            .chain(self.opened.values())
            .any(|presence| presence.is_moving(now))
    }

    fn paint_glance(
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        slot: &Slot,
        content: &Content<'_>,
    ) {
        let Content { month, members, .. } = *content;
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
            WidgetId::Music | WidgetId::SystemMonitor => {}
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
            |rect: Rect| cursor.is_some_and(|p| rect.contains(p)) && self.press.is_none();
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
                        cursor.filter(|_| self.press.is_none()),
                    ),
                    WidgetId::Calendar => {
                        paint::paint_calendar_card(
                            scene, transform, text, theme, card, month, stage,
                        );
                    }
                    WidgetId::Music | WidgetId::SystemMonitor => {}
                }
                // The content stays inside the card's shape.
                let _ = radius;
            });
        }
    }
}

impl Widget for DeskWidgets {
    fn update(&mut self, _state: &AppModel, _path: &ModelPath) -> bool {
        // Who is here can change with the tree alone: lay the widget out
        // again for it.
        let people = self.team_rows();
        let team = self.slot(WidgetId::Team);
        let changed = team.is_some_and(|slot| {
            let (laid, _) = self.lay(WidgetId::Team, people.len());
            laid.has_content != slot.has_content
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
        let painted = Painted {
            forms: self
                .slots
                .iter()
                .map(|slot| (slot.id, self.board.form(slot.id)))
                .collect(),
            day: today,
            people: self.team_ids.clone(),
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
            cursor: self.pointer,
        };
        for slot in self.slots.clone() {
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
                if morph >= 0.5 {
                    paint::paint_card_ground(scene, transform, theme, ground);
                } else {
                    paint::paint_glance_ground(scene, transform, theme, slot.id, ground);
                }
                let _ = radius;
                if glance_alpha > f32::EPSILON {
                    paint::with_alpha(scene, transform, bounds, glance_alpha, |scene| {
                        paint::paint_glance_ground(scene, transform, theme, slot.id, slot.glance);
                        Self::paint_glance(scene, transform, text, theme, &slot, &content);
                        if hovered && card_alpha <= f32::EPSILON {
                            let badge = paint::badge_rect(slot.glance, self.anchor(slot.id));
                            let on_badge = self.pointer.is_some_and(|p| badge.contains(p));
                            paint::paint_badge(scene, transform, theme, badge, on_badge);
                        }
                    });
                }
                if card_alpha > f32::EPSILON {
                    paint::with_alpha(scene, transform, bounds, card_alpha, |scene| {
                        self.paint_card(scene, transform, text, theme, &slot, &content);
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
            self.board.on_pointer(p, now, &self.slots);
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
                self.press = Some(Press { id, part, at: p });
                self.dirty = true;
                self.tick(now);
                *ctx.handled = true;
            }
            ElementState::Released => {
                let Some(press) = self.press.take() else {
                    return;
                };
                let form = self.board.form(press.id);
                self.tap(press.id, form, press.part);
                self.tick(now);
                *ctx.handled = true;
                let _ = press.at;
            }
        }
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
            || self.animating(ui::time::now())
            // A choice from Settings arrives without an event: the frame it
            // asks for is what takes it up.
            || *shared(&self.changes) != self.prefs
    }

    fn is_layout_dirty(&self) -> bool {
        false
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
        self.board
            .deadline(now)
            .into_iter()
            .chain(day)
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

    use tilo_desktop_model::{Collaborator, IdMinter, Tab, Workspace};
    use tilo_desktop_surface::{DesktopSurface, TileSource, seed_tree};
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
    }

    /// A desk with `people` in its workspace, the calendar added, and the
    /// layer laid out over a 1600×900 display.
    fn rig(people: &[(&str, &str)]) -> Rig {
        let mut minter = IdMinter::new(7);
        let mut tree = seed_tree(&mut minter, "Desk", Vec::new());
        tree.collaborators = people
            .iter()
            .map(|(id, name)| Collaborator::new(*id, *name))
            .collect();
        let handle = DesktopSurface::new(tree, 7, Box::new(Tiles)).1;
        let asks = Rc::new(RefCell::new(Vec::new()));
        let mut prefs = DeskWidgetPrefs::default();
        prefs.get_mut(WidgetId::Calendar).enabled = true;
        let changes = std::sync::Arc::new(std::sync::Mutex::new(prefs.clone()));
        let mut widgets = DeskWidgets::new(handle, "me".into(), Rc::clone(&asks), changes.clone());
        widgets = widgets.with_prefs(prefs);
        lay_out(&mut widgets, AREA);
        Rig {
            widgets,
            asks,
            changes,
        }
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

    /// A desk surface that shows no window: what stands between the
    /// widgets and the person in these tests.
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
