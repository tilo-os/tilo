//! Slots arriving, leaving and changing places in one tab's row, on the
//! standard list change ([`ListChange`]): a slot the app adds fades in where
//! it stands, one it takes away fades out where it stood, and the slots that
//! stay glide from their old places to their new ones. Reduced motion: the
//! slots land, and arrivals and departures still fade. A key the person just
//! pressed: everything lands. A slot is hit where it is drawn while it
//! glides, and one that arrives takes a press at once.

use std::collections::HashMap;
use std::rc::Rc;

use vello::kurbo::{Rect, Vec2};

use super::{Slot, Slots, WindowToolbar};
use crate::motion::{Direction, ListChange, MotionValue};
use crate::time::Instant;
use crate::tokens::{EffectiveMotion, MotionSource};

/// Who a slot is across two toolbars of one tab: an action by its id, every
/// other slot by its place in the row.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum SlotKey {
    Sidebar,
    Back,
    Forward,
    Leading(String),
    /// The view switcher, as one strip.
    Views,
    ViewsMenu,
    Location,
    Action(String),
    /// One of a notice's buttons in the row, by its id.
    Notice(String),
    Overflow,
    /// The window's reactions, which move as the reaction strip moves them.
    Reactions,
}

impl SlotKey {
    /// Who `slot` of `toolbar` is.
    pub(super) fn of(toolbar: &WindowToolbar, slot: &Slot) -> Self {
        match slot {
            Slot::Sidebar => Self::Sidebar,
            Slot::Back => Self::Back,
            Slot::Forward => Self::Forward,
            Slot::Leading(i) => Self::Leading(
                toolbar
                    .leading
                    .get(*i)
                    .map_or_else(String::new, |action| action.id.clone()),
            ),
            Slot::Location => Self::Location,
            Slot::View(_) => Self::Views,
            Slot::ViewsMenu => Self::ViewsMenu,
            Slot::Action(i) => Self::Action(
                toolbar
                    .actions
                    .get(*i)
                    .map_or_else(String::new, |action| action.id.clone()),
            ),
            Slot::Notice(i) => Self::Notice(
                toolbar
                    .notice
                    .as_ref()
                    .and_then(|notice| notice.actions.get(*i))
                    .map_or_else(String::new, |action| action.id.clone()),
            ),
            Slot::Overflow => Self::Overflow,
            Slot::Reaction(_) | Slot::ReactionAdd => Self::Reactions,
        }
    }
}

/// Each slot `toolbar` shows in the row as `slots` lays it out, by who it is.
pub(super) fn placed(toolbar: &WindowToolbar, slots: &Slots) -> Vec<(SlotKey, Rect)> {
    let shown = |rect: &Rect| rect.area() > 0.0;
    let mut out = Vec::new();
    let mut put = |key: SlotKey, rect: Rect| {
        if shown(&rect) {
            out.push((key, rect));
        }
    };
    if toolbar.sidebar.is_some() {
        put(SlotKey::Sidebar, slots.sidebar);
    }
    if toolbar.navigation.is_some() {
        put(SlotKey::Back, slots.back);
        put(SlotKey::Forward, slots.forward);
    }
    for (action, rect) in toolbar.leading.iter().zip(&slots.leading) {
        put(SlotKey::Leading(action.id.clone()), *rect);
    }
    if let (Some(first), Some(last)) = (slots.views.first(), slots.views.last()) {
        put(SlotKey::Views, first.union(*last));
    }
    put(SlotKey::ViewsMenu, slots.views_menu);
    put(SlotKey::Location, slots.location);
    for (action, rect) in toolbar.actions.iter().zip(&slots.actions) {
        put(SlotKey::Action(action.id.clone()), *rect);
    }
    if let Some(notice) = toolbar.notice.as_ref() {
        for (action, rect) in notice.actions.iter().zip(&slots.notice) {
            put(SlotKey::Notice(action.id.clone()), *rect);
        }
    }
    put(SlotKey::Overflow, slots.overflow);
    out
}

/// A slot the app took away, fading out where it stood: painted from the
/// toolbar it stood in, cut to its place.
pub(super) struct Leaving {
    key: SlotKey,
    pub(super) from: Rc<(WindowToolbar, Slots)>,
    pub(super) rect: Rect,
    /// Where it was drawn from its laid-out place when it began to leave.
    pub(super) offset: Vec2,
    opacity: MotionValue<f64>,
}

impl Leaving {
    pub(super) fn opacity(&self, now: Instant) -> f32 {
        crate::num::f32_of_f64(self.opacity.get(now).clamp(0.0, 1.0))
    }
}

/// The slots of one tab's row in motion after the app changed them. At rest
/// it holds nothing and asks for no frames.
#[derive(Default)]
pub(super) struct SlotChanges {
    /// Slots fading in, by who they are: their opacity.
    arriving: HashMap<SlotKey, MotionValue<f64>>,
    /// Slots gliding to their new place: where they are drawn from it.
    moving: HashMap<SlotKey, MotionValue<Vec2>>,
    leaving: Vec<Leaving>,
}

/// How a slot in motion is drawn now: its opacity and where it is drawn
/// from its laid-out place.
pub(super) type Pose = (f32, Vec2);

impl SlotChanges {
    /// The row went from `before` to `toolbar` laid out as `slots`, both in
    /// one tab: start what arrived, left and moved on its way.
    pub(super) fn start(
        &mut self,
        before: Rc<(WindowToolbar, Slots)>,
        (toolbar, slots): (&WindowToolbar, &Slots),
        motion: EffectiveMotion,
        now: Instant,
    ) {
        // The app changed the row: it moves unless a key just asked for the
        // change, which lands.
        let source = crate::motion::recent_input(now).unwrap_or(MotionSource::Pointer);
        let fade_in = ListChange::FADE.timing(Direction::Enter, motion, source);
        let fade_out = ListChange::FADE.timing(Direction::Exit, motion, source);
        let glide = ListChange::MOVE.timing(Direction::Enter, motion, source);
        let old = placed(&before.0, &before.1);
        let new = placed(toolbar, slots);
        for (key, rect) in &new {
            if let Some((_, was)) = old.iter().find(|(was, _)| was == key) {
                let drawn = was.origin() + self.offset(key, now);
                let offset = drawn - rect.origin();
                if offset.hypot() <= 0.5 {
                    self.moving.remove(key);
                    continue;
                }
                let mut value = MotionValue::new(offset).with_tolerance(0.25);
                value.set(Vec2::ZERO, glide, now);
                self.moving.insert(key.clone(), value);
            } else {
                // A slot coming back while it left fades in from where its
                // fade out stood.
                let from = self
                    .leaving
                    .iter()
                    .position(|leaving| leaving.key == *key)
                    .map_or(0.0, |i| self.leaving.remove(i).opacity.get(now));
                let mut value = MotionValue::new(from);
                value.set(1.0, fade_in, now);
                self.moving.remove(key);
                self.arriving.insert(key.clone(), value);
            }
        }
        for (key, rect) in old {
            if new.iter().any(|(stays, _)| *stays == key) {
                continue;
            }
            let from = self
                .arriving
                .remove(&key)
                .map_or(1.0, |value| value.get(now));
            let offset = self.offset(&key, now);
            self.moving.remove(&key);
            let mut opacity = MotionValue::new(from);
            opacity.set(0.0, fade_out, now);
            self.leaving.push(Leaving {
                key,
                from: Rc::clone(&before),
                rect,
                offset,
                opacity,
            });
        }
        self.prune(now);
    }

    /// Where `key` is drawn from its laid-out place now.
    pub(super) fn offset(&self, key: &SlotKey, now: Instant) -> Vec2 {
        self.moving
            .get(key)
            .map_or(Vec2::ZERO, |value| value.get(now))
    }

    /// How each slot in motion is drawn now; empty at rest.
    pub(super) fn poses(&self, now: Instant) -> HashMap<SlotKey, Pose> {
        let mut poses: HashMap<SlotKey, Pose> = HashMap::new();
        for (key, value) in &self.moving {
            poses.insert(key.clone(), (1.0, value.get(now)));
        }
        for (key, value) in &self.arriving {
            let opacity = crate::num::f32_of_f64(value.get(now).clamp(0.0, 1.0));
            poses.entry(key.clone()).or_insert((1.0, Vec2::ZERO)).0 = opacity;
        }
        poses
    }

    /// The slots fading out where they stood.
    pub(super) fn leaving(&self) -> &[Leaving] {
        &self.leaving
    }

    /// Whether anything still moves at `now`.
    pub(super) fn is_moving(&self, now: Instant) -> bool {
        self.moving.values().any(|value| value.is_moving(now))
            || self.arriving.values().any(|value| value.is_moving(now))
            || self
                .leaving
                .iter()
                .any(|leaving| leaving.opacity.is_moving(now))
    }

    /// Whether a slot is drawn away from its laid-out place at `now`.
    pub(super) fn glides(&self, now: Instant) -> bool {
        self.moving.values().any(|value| value.is_moving(now))
    }

    /// Drop what has arrived.
    pub(super) fn prune(&mut self, now: Instant) {
        self.moving.retain(|_, value| value.is_moving(now));
        self.arriving.retain(|_, value| value.is_moving(now));
        self.leaving
            .retain(|leaving| leaving.opacity.is_moving(now));
    }

    /// Another tab shows: its row cross-fades in whole.
    pub(super) fn clear(&mut self) {
        self.moving.clear();
        self.arriving.clear();
        self.leaving.clear();
    }
}
