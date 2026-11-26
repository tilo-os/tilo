//! Parts arriving, leaving and changing places in one tab's row, on the standard list change
//! ([`RowMotions`], as a list's rows and a stack's parts move): a part the app adds fades in where it
//! stands, one it takes away fades out where it stood, and the parts that stay glide from their old
//! places to their new ones. Reduced motion: the parts land, and arrivals and departures still fade.
use std::collections::HashMap;
use std::rc::Rc;

use vello::kurbo::{Rect, Vec2};

use super::{Part, Parts, WindowToolbar};
use tilo_ui_base::collection_transition::RowMotions;
use crate::time::Instant;
use tilo_ui_base::{tokens::EffectiveMotion, tokens::MotionSource};

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

/// A part the app took away, fading out where it was drawn: the row it was painted in, its box
/// there, how far from that box it was drawn when it left, and its opacity.
pub(super) struct Leaving {
    pub(super) from: Rc<(WindowToolbar, Parts)>,
    pub(super) rect: Rect,
    pub(super) offset: Vec2,
    pub(super) opacity: f32,
}

pub(super) struct PartChanges {
    rows: RowMotions<PartKey, Vec2>,
    /// What each part fading out was painted in, and its box there.
    left: HashMap<PartKey, (Rc<(WindowToolbar, Parts)>, Rect)>,
}

impl Default for PartChanges {
    fn default() -> Self {
        Self {
            rows: RowMotions::default().fading_out_leavers(),
            left: HashMap::new(),
        }
    }
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
        let source = tilo_ui_base::motion::recent_input(now).unwrap_or(MotionSource::Pointer);
        let old = placed(&before.0, &before.1);
        let new = placed(toolbar, parts);
        let at = |placed: &[(PartKey, Rect)]| {
            placed
                .iter()
                .map(|(key, rect)| (key.clone(), rect.origin().to_vec2()))
                .collect::<Vec<_>>()
        };
        self.rows.capture(at(&old), true, Some(source));
        self.rows.start(at(&new), motion, now);
        for (key, rect) in old {
            if !new.iter().any(|(stays, _)| *stays == key) {
                self.left.insert(key, (Rc::clone(&before), rect));
            }
        }
        self.prune(now);
    }

    pub(super) fn offset(&self, key: &PartKey, now: Instant) -> Vec2 {
        self.rows.pose(key, now).0
    }

    pub(super) fn poses(&self, now: Instant) -> HashMap<PartKey, Pose> {
        self.rows
            .moving(now)
            .map(|(key, offset, opacity)| (key.clone(), (opacity, offset)))
            .collect()
    }

    pub(super) fn leaving(&self, now: Instant) -> Vec<Leaving> {
        self.rows
            .leaving(now)
            .filter_map(|(key, at, opacity)| {
                let (from, rect) = self.left.get(key)?;
                Some(Leaving {
                    from: Rc::clone(from),
                    rect: *rect,
                    offset: at - rect.origin().to_vec2(),
                    opacity,
                })
            })
            .collect()
    }

    pub(super) fn is_moving(&self, now: Instant) -> bool {
        self.rows.is_moving(now)
    }

    pub(super) fn glides(&self, now: Instant) -> bool {
        self.rows.glides(now)
    }

    pub(super) fn prune(&mut self, now: Instant) {
        self.rows.settle(now);
        let leaving: Vec<PartKey> = self
            .rows
            .leaving(now)
            .map(|(key, ..)| key.clone())
            .collect();
        self.left.retain(|key, _| leaving.contains(key));
    }

    pub(super) fn clear(&mut self) {
        self.rows.clear();
        self.left.clear();
    }
}
