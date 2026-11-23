//! Parts arriving, leaving and changing places in one tab's row, on the standard list change
//! ([`ListChange`]): a part the app adds fades in where it stands, one it takes away fades out
//! where it stood, and the parts that stay glide from their old places to their new ones. Reduced
//! motion: the parts land, and arrivals and departures still fade.
use std::collections::HashMap;
use std::rc::Rc;

use vello::kurbo::{Rect, Vec2};

use super::{Part, Parts, WindowToolbar};
use crate::motion::{Direction, ListChange, MotionValue};
use crate::time::Instant;
use crate::tokens::{EffectiveMotion, MotionSource};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum PartKey {
    Sidebar,
    Back,
    Forward,
    Leading(String),
    Views,
    ViewsMenu,
    Location,
    Action(String),
    Notice(String),
    Overflow,
    Reactions,
}

impl PartKey {
    pub(super) fn of(toolbar: &WindowToolbar, part: &Part) -> Self {
        match part {
            Part::Sidebar => Self::Sidebar,
            Part::Back => Self::Back,
            Part::Forward => Self::Forward,
            Part::Leading(i) => Self::Leading(
                toolbar
                    .leading
                    .get(*i)
                    .map_or_else(String::new, |action| action.id.clone()),
            ),
            Part::Location => Self::Location,
            Part::View(_) => Self::Views,
            Part::ViewsMenu => Self::ViewsMenu,
            Part::Action(i) => Self::Action(
                toolbar
                    .actions
                    .get(*i)
                    .map_or_else(String::new, |action| action.id.clone()),
            ),
            Part::Notice(i) => Self::Notice(
                toolbar
                    .notice
                    .as_ref()
                    .and_then(|notice| notice.actions.get(*i))
                    .map_or_else(String::new, |action| action.id.clone()),
            ),
            Part::Overflow => Self::Overflow,
            Part::Reaction(_) | Part::ReactionAdd => Self::Reactions,
        }
    }
}

pub(super) fn placed(toolbar: &WindowToolbar, parts: &Parts) -> Vec<(PartKey, Rect)> {
    let shown = |rect: &Rect| rect.area() > 0.0;
    let mut out = Vec::new();
    let mut put = |key: PartKey, rect: Rect| {
        if shown(&rect) {
            out.push((key, rect));
        }
    };
    if toolbar.sidebar.is_some() {
        put(PartKey::Sidebar, parts.sidebar);
    }
    if toolbar.navigation.is_some() {
        put(PartKey::Back, parts.back);
        put(PartKey::Forward, parts.forward);
    }
    for (action, rect) in toolbar.leading.iter().zip(&parts.leading) {
        put(PartKey::Leading(action.id.clone()), *rect);
    }
    if let (Some(first), Some(last)) = (parts.views.first(), parts.views.last()) {
        put(PartKey::Views, first.union(*last));
    }
    put(PartKey::ViewsMenu, parts.views_menu);
    put(PartKey::Location, parts.location);
    for (action, rect) in toolbar.actions.iter().zip(&parts.actions) {
        put(PartKey::Action(action.id.clone()), *rect);
    }
    if let Some(notice) = toolbar.notice.as_ref() {
        for (action, rect) in notice.actions.iter().zip(&parts.notice) {
            put(PartKey::Notice(action.id.clone()), *rect);
        }
    }
    put(PartKey::Overflow, parts.overflow);
    out
}

pub(super) struct Leaving {
    key: PartKey,
    pub(super) from: Rc<(WindowToolbar, Parts)>,
    pub(super) rect: Rect,
    pub(super) offset: Vec2,
    opacity: MotionValue<f64>,
}

impl Leaving {
    pub(super) fn opacity(&self, now: Instant) -> f32 {
        crate::num::f32_of_f64(self.opacity.get(now).clamp(0.0, 1.0))
    }
}

#[derive(Default)]
pub(super) struct PartChanges {
    arriving: HashMap<PartKey, MotionValue<f64>>,
    moving: HashMap<PartKey, MotionValue<Vec2>>,
    leaving: Vec<Leaving>,
}

pub(super) type Pose = (f32, Vec2);

impl PartChanges {
    pub(super) fn start(
        &mut self,
        before: Rc<(WindowToolbar, Parts)>,
        (toolbar, parts): (&WindowToolbar, &Parts),
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
        let new = placed(toolbar, parts);
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

    pub(super) fn offset(&self, key: &PartKey, now: Instant) -> Vec2 {
        self.moving
            .get(key)
            .map_or(Vec2::ZERO, |value| value.get(now))
    }

    pub(super) fn poses(&self, now: Instant) -> HashMap<PartKey, Pose> {
        let mut poses: HashMap<PartKey, Pose> = HashMap::new();
        for (key, value) in &self.moving {
            poses.insert(key.clone(), (1.0, value.get(now)));
        }
        for (key, value) in &self.arriving {
            let opacity = crate::num::f32_of_f64(value.get(now).clamp(0.0, 1.0));
            poses.entry(key.clone()).or_insert((1.0, Vec2::ZERO)).0 = opacity;
        }
        poses
    }

    pub(super) fn leaving(&self) -> &[Leaving] {
        &self.leaving
    }

    pub(super) fn is_moving(&self, now: Instant) -> bool {
        self.moving.values().any(|value| value.is_moving(now))
            || self.arriving.values().any(|value| value.is_moving(now))
            || self
                .leaving
                .iter()
                .any(|leaving| leaving.opacity.is_moving(now))
    }

    pub(super) fn glides(&self, now: Instant) -> bool {
        self.moving.values().any(|value| value.is_moving(now))
    }

    pub(super) fn prune(&mut self, now: Instant) {
        self.moving.retain(|_, value| value.is_moving(now));
        self.arriving.retain(|_, value| value.is_moving(now));
        self.leaving
            .retain(|leaving| leaving.opacity.is_moving(now));
    }

    pub(super) fn clear(&mut self) {
        self.moving.clear();
        self.arriving.clear();
        self.leaving.clear();
    }
}
