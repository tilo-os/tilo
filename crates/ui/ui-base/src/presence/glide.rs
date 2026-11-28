//! Where a peer's mark on a document surface glides to their new place: the caret and the
//! selection move on the shared motion primitives, and land at once where motion is reduced.

use std::collections::{HashMap, HashSet};

use vello::kurbo::{Point, Rect};

use crate::motion::{Direction, Follow, MotionValue, Timing};
use crate::permissions::MemberId;
use crate::theme::Theme;
use crate::time::Instant;
use crate::tokens::MotionSource;

/// The timing a peer's mark moves with: the shared fast pacing a reading settles on, with the
/// pointer as its source. Where motion is reduced the spec lands it at once.
#[must_use]
pub fn peer_glide_timing(theme: &Theme) -> Timing {
    Follow::READING.timing(Direction::Enter, theme.motion(), MotionSource::Pointer)
}

/// One member's mark as it glides: where they point and what they selected.
#[derive(Debug)]
struct Held {
    at: MotionValue<Point>,
    selection: Option<MotionValue<Rect>>,
}

/// Every peer's mark on one surface, and where it is drawn between what it showed and where its
/// person is now.
#[derive(Debug, Default)]
pub struct PeerGlide {
    held: HashMap<MemberId, Held>,
}

impl PeerGlide {
    /// Send each member's mark to the place they say, with `timing` (`None` lands it at once),
    /// and drop the marks of members no longer there.
    pub fn retarget(
        &mut self,
        peers: impl IntoIterator<Item = (MemberId, Point, Option<Rect>)>,
        timing: Timing,
        now: Instant,
    ) {
        let mut seen: HashSet<MemberId> = HashSet::new();
        for (user, at, selection) in peers {
            seen.insert(user.clone());
            let held = self.held.entry(user).or_insert_with(|| Held {
                at: MotionValue::new(at),
                selection: selection.map(MotionValue::new),
            });
            held.at.set(at, timing, now);
            match (held.selection.as_mut(), selection) {
                (Some(shown), Some(rect)) => shown.set(rect, timing, now),
                (None, Some(rect)) => held.selection = Some(MotionValue::new(rect)),
                (Some(_), None) => held.selection = None,
                (None, None) => {}
            }
        }
        self.held.retain(|user, _| seen.contains(user));
    }

    /// Where `user`'s mark is drawn at `now`: their point, and what they selected.
    #[must_use]
    pub fn shown(&self, user: &MemberId, now: Instant) -> Option<(Point, Option<Rect>)> {
        let held = self.held.get(user)?;
        Some((
            held.at.get(now),
            held.selection.as_ref().map(|selection| selection.get(now)),
        ))
    }

    /// Whether any mark is still on its way at `now`: a reason to draw the next frame.
    #[must_use]
    pub fn moving(&self, now: Instant) -> bool {
        self.held.values().any(|held| {
            held.at.is_moving(now)
                || held
                    .selection
                    .as_ref()
                    .is_some_and(|selection| selection.is_moving(now))
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::float_cmp,
        reason = "a motion's frames at its own instants, read back as the points it was given"
    )]
    use super::*;
    use crate::time::Duration;

    fn user() -> MemberId {
        MemberId::person("ada")
    }

    fn timing() -> Timing {
        Follow::READING.timing(
            Direction::Enter,
            crate::tokens::EffectiveMotion::default(),
            MotionSource::Pointer,
        )
    }

    /// A mark glides to a new place over frames, and a move under reduced motion lands at once.
    #[test]
    fn a_peers_mark_glides_to_their_new_place_and_lands_at_once_reduced() {
        let start = crate::motion::frame_now();
        let mut glide = PeerGlide::default();
        let from = Point::new(10.0, 20.0);
        let to = Point::new(110.0, 20.0);
        glide.retarget([(user(), from, None)], Timing::Instant, start);
        assert_eq!(glide.shown(&user(), start), Some((from, None)));
        assert!(!glide.moving(start), "a mark seen first is where it is");

        glide.retarget([(user(), to, None)], timing(), start);
        assert!(glide.moving(start), "it set out");
        let at_start = glide.shown(&user(), start).expect("shown").0;
        assert!(
            (at_start.x - from.x).abs() < 1.0,
            "the mark starts where it was drawn: {at_start:?}"
        );
        let mut mid = None;
        for step in 1..=20u32 {
            let now = start
                .checked_add(Duration::from_millis(u64::from(step) * 16))
                .expect("a frame");
            let at = glide.shown(&user(), now).expect("shown").0;
            if at.x > from.x + 1.0 && at.x < to.x - 1.0 {
                mid = Some(at);
            }
        }
        assert!(
            mid.is_some(),
            "a frame between the two places draws it between them"
        );
        let late = start
            .checked_add(Duration::from_secs(1))
            .expect("a second on");
        assert_eq!(glide.shown(&user(), late).map(|(at, _)| at), Some(to));

        // Reduced motion: a mark put somewhere new is drawn there in the same frame.
        let mut still = PeerGlide::default();
        still.retarget([(user(), from, None)], Timing::Instant, start);
        still.retarget([(user(), to, None)], Timing::Instant, start);
        assert_eq!(still.shown(&user(), start).map(|(at, _)| at), Some(to));
        assert!(!still.moving(start));
    }

    /// A selection glides with its person, and a member gone takes the mark with them.
    #[test]
    fn a_selection_glides_with_its_person_and_a_member_gone_leaves_nothing() {
        let start = crate::motion::frame_now();
        let mut glide = PeerGlide::default();
        let first = Rect::new(0.0, 0.0, 10.0, 10.0);
        let moved = Rect::new(50.0, 0.0, 60.0, 10.0);
        glide.retarget([(user(), Point::ZERO, Some(first))], Timing::Instant, start);
        glide.retarget([(user(), Point::ZERO, Some(moved))], timing(), start);
        assert!(glide.moving(start));
        let late = start
            .checked_add(Duration::from_secs(1))
            .expect("a second on");
        assert_eq!(
            glide.shown(&user(), late).and_then(|(_, rect)| rect),
            Some(moved)
        );
        glide.retarget([], Timing::Instant, late);
        assert_eq!(glide.shown(&user(), late), None, "gone with them");
    }
}
