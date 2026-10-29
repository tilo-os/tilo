//! Quick Settings' popover motion, composed from the standard motions: the
//! panel appears from the tray and goes back to it ([`ui::motion::Appear`]),
//! and its height follows what it shows ([`ui::motion::Extent`]). What
//! moves inside the panel is the app's own.
//!
//! The now-playing card is the panel's companion: it stands under the panel
//! or beside it ([`CardSide`]), placed against the panel as drawn, so it
//! follows the height's motion; when it has to change sides it fades out
//! where it was and in where it goes ([`ui::motion::Appear`]), and it
//! leaves with the panel, never on its own.

use ui::motion::{Appear, Extent, Pose, Presence};
use ui::time::Instant;
use ui::tokens::{EffectiveMotion, MotionSource};
use vello::kurbo::{Point, Rect};

/// Where the now-playing card stands against the panel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CardSide {
    Below,
    Beside,
}

/// One frame's input to [`QsMotion::follow`].
pub(super) struct QsFrame {
    pub open: bool,
    /// The panel's resting height for what it shows.
    pub height: f64,
    pub motion: EffectiveMotion,
    pub source: MotionSource,
    pub now: Instant,
}

#[derive(Debug)]
pub(super) struct QsMotion {
    presence: Presence,
    height: Extent,
    /// Whether a frame has been followed since this was made: the first
    /// one takes the height as it is (a panel settled for a capture shows
    /// as it is, it does not grow into it).
    synced: bool,
    /// The now-playing card under the panel and beside it.
    below: Presence,
    beside: Presence,
    /// The card takes its side as it is on the next
    /// [`Self::follow_card`]: the panel has just appeared.
    card_synced: bool,
}

impl QsMotion {
    pub(super) fn closed() -> Self {
        Self::with(Presence::closed())
    }

    /// Open and at rest: a host that starts with the panel shown.
    pub(super) fn opened() -> Self {
        Self::with(Presence::opened())
    }

    /// Open, held at `permille` of its entrance, for a deterministic capture.
    pub(super) fn held_at(permille: u16) -> Self {
        Self::with(Presence::held_at(permille))
    }

    fn with(presence: Presence) -> Self {
        Self {
            presence,
            height: Extent::default(),
            synced: false,
            below: Presence::closed(),
            beside: Presence::closed(),
            card_synced: false,
        }
    }

    /// Follow this frame's chrome state.
    pub(super) fn follow(&mut self, frame: &QsFrame) {
        let now = frame.now;
        let was_hidden = !self.presence.is_visible(now);
        self.presence
            .show(frame.open, Appear::SPEC, frame.motion, frame.source, now);
        if was_hidden || !self.synced || !self.presence.is_visible(now) {
            self.synced = true;
            self.card_synced = false;
            // A panel that opens shows as it is: nothing grows on the way
            // in.
            self.height.snap(frame.height);
            return;
        }
        if frame.open {
            self.height
                .follow(frame.height, frame.motion, frame.source, now);
        }
    }

    /// Follow where the now-playing card goes this frame: `side`, or
    /// nowhere. While the panel closes the card stays as it is and leaves
    /// with the panel's own pose.
    pub(super) fn follow_card(
        &mut self,
        open: bool,
        side: Option<CardSide>,
        motion: EffectiveMotion,
        source: MotionSource,
        now: Instant,
    ) {
        if !open {
            return;
        }
        let below = side == Some(CardSide::Below);
        let beside = side == Some(CardSide::Beside);
        if !self.card_synced {
            // The card comes in with the panel, on the panel's pose.
            self.card_synced = true;
            self.below.snap(below);
            self.beside.snap(beside);
            return;
        }
        self.below.show(below, Appear::SPEC, motion, source, now);
        self.beside.show(beside, Appear::SPEC, motion, source, now);
    }

    /// The sides the card is drawn on at `now`, each with its opacity.
    pub(super) fn card_sides(&self, now: Instant) -> Vec<(CardSide, f32)> {
        [
            (CardSide::Below, &self.below),
            (CardSide::Beside, &self.beside),
        ]
        .into_iter()
        .filter(|(_, presence)| presence.is_visible(now))
        .map(|(side, presence)| (side, ui::motion::unit_opacity(presence.progress(now))))
        .collect()
    }

    /// Whether any of the panel draws at `now` (open, opening or closing).
    pub(super) fn is_visible(&self, now: Instant) -> bool {
        self.presence.is_visible(now)
    }

    /// Whether anything about the panel still moves at `now`.
    pub(super) fn is_animating(&self, now: Instant) -> bool {
        self.presence.is_moving(now)
            || self.height.is_moving(now)
            || self.below.is_moving(now)
            || self.beside.is_moving(now)
    }

    /// Where the panel `rect` appears from: the tray it hangs from sits over
    /// its trailing quarter; a panel placed at a caret (`centred`) appears
    /// from its top centre.
    pub(super) fn origin(rect: Rect, centred: bool) -> Point {
        let x = if centred {
            rect.center().x
        } else {
            rect.x1 - rect.width() * 0.25
        };
        Appear::origin(rect, Point::new(x, rect.y0))
    }

    /// The panel's pose at `now`, appearing from `origin`.
    pub(super) fn pose(&self, now: Instant, origin: Point, motion: EffectiveMotion) -> Pose {
        Appear::pose(self.presence.progress(now), origin, motion)
    }

    /// The panel as drawn at `now`: `rest` with the height the page change
    /// has reached.
    pub(super) fn drawn_rect(&self, rest: Rect, now: Instant) -> Rect {
        if !self.height.is_moving(now) {
            return rest;
        }
        Rect::new(
            rest.x0,
            rest.y0,
            rest.x1,
            rest.y0 + self.height.get(now).max(1.0),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ui::time::Duration;
    use ui::tokens::Motion;

    fn frame(open: bool, height: f64, now: Instant) -> QsFrame {
        QsFrame {
            open,
            height,
            motion: EffectiveMotion::new(Motion::default(), false),
            source: MotionSource::Pointer,
            now,
        }
    }

    #[test]
    fn quick_settings_appears_from_the_tray_and_its_height_follows_what_it_shows() {
        let now = ui::time::now();
        let motion = EffectiveMotion::new(Motion::default(), false);
        let mut qs = QsMotion::closed();
        qs.follow(&frame(true, 420.0, now));
        assert!(qs.is_visible(now) && qs.is_animating(now), "it appears");
        let rect = Rect::new(600.0, 40.0, 960.0, 460.0);
        let origin = QsMotion::origin(rect, false);
        let pose = qs.pose(now + Duration::from_millis(30), origin, motion);
        assert!(pose.opacity > 0.0 && pose.opacity < 1.0);
        assert_eq!(pose.to_drawn(origin), origin, "it grows out of the tray");
        let opened = qs.presence.settles_at().expect("appearing");
        assert!(qs.pose(opened, origin, motion).is_rest(), "and settles");

        // A section closing: the height follows the content down.
        qs.follow(&frame(true, 260.0, opened));
        let mid = opened + Duration::from_millis(40);
        let h = qs.drawn_rect(rect, mid).height();
        assert!(h < 420.0 && h > 260.0, "the height is on its way: {h}");
        let settled = qs.height.settles_at().expect("moving");
        qs.follow(&frame(true, 260.0, settled));
        assert!(!qs.is_animating(settled), "settles and asks for no frames");
    }

    #[test]
    fn the_card_comes_with_the_panel_changes_sides_by_fading_and_leaves_with_it() {
        let now = ui::time::now();
        let motion = EffectiveMotion::new(Motion::default(), false);
        let mut qs = QsMotion::closed();
        qs.follow(&frame(true, 420.0, now));
        qs.follow_card(
            true,
            Some(CardSide::Below),
            motion,
            MotionSource::Pointer,
            now,
        );
        assert_eq!(
            qs.card_sides(now),
            [(CardSide::Below, 1.0)],
            "it appears on the panel's pose, not on its own"
        );
        let opened = qs.presence.settles_at().expect("appearing");
        qs.follow(&frame(true, 700.0, opened));
        qs.follow_card(
            true,
            Some(CardSide::Beside),
            motion,
            MotionSource::Pointer,
            opened,
        );
        let mid = opened + Duration::from_millis(30);
        let sides = qs.card_sides(mid);
        assert_eq!(
            sides.len(),
            2,
            "it fades out under and in beside: {sides:?}"
        );
        assert!(
            sides
                .iter()
                .all(|(_, opacity)| *opacity > 0.0 && *opacity < 1.0)
        );
        let moved = [qs.below.settles_at(), qs.beside.settles_at()]
            .into_iter()
            .flatten()
            .max()
            .expect("moving");
        assert_eq!(qs.card_sides(moved), [(CardSide::Beside, 1.0)]);
        qs.follow(&frame(false, 700.0, moved));
        qs.follow_card(false, None, motion, MotionSource::Pointer, moved);
        assert_eq!(
            qs.card_sides(moved),
            [(CardSide::Beside, 1.0)],
            "closing leaves the card to the panel's pose"
        );
    }

    #[test]
    fn a_shortcut_opens_quick_settings_at_once() {
        let now = ui::time::now();
        let mut qs = QsMotion::closed();
        let mut f = frame(true, 300.0, now);
        f.source = MotionSource::Keyboard;
        qs.follow(&f);
        assert!(!qs.is_animating(now));
        let motion = EffectiveMotion::new(Motion::default(), false);
        assert!(qs.pose(now, Point::ZERO, motion).is_rest());
    }

    #[test]
    fn reduced_motion_fades_quick_settings_without_scaling_or_sliding() {
        let now = ui::time::now();
        let reduced = EffectiveMotion::new(Motion::default(), true);
        let mut qs = QsMotion::closed();
        let mut f = frame(true, 300.0, now);
        f.motion = reduced;
        qs.follow(&f);
        let pose = qs.pose(
            now + Duration::from_millis(40),
            Point::new(5.0, 5.0),
            reduced,
        );
        assert_eq!(pose.transform, vello::kurbo::Affine::IDENTITY);
        assert!(pose.opacity > 0.0 && pose.opacity < 1.0);
    }
}
