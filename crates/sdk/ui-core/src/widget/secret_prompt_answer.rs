//! How the line under a password prompt's fields answers.
//!
//! The asker owns whether an answer was accepted, refused or is still being
//! checked; this is how the prompt shows a change of answer. It composes the
//! toolkit's named motions rather than timing anything itself: the new line
//! cross-fades in over the one it replaces, and a refusal also shakes the
//! line sideways (`Attention::refuse`). Both are finite, so nothing keeps
//! asking for frames once an answer has landed, and under reduced motion
//! only the fade remains. An edit (typing takes an error down, Caps Lock)
//! is not an answer: its line lands at once.

use crate::time::Instant;

use crate::motion::{Attention, CrossFade};
use crate::tokens::{EffectiveMotion, MotionSource};

use super::NoticeTone;

/// The line under the fields as shown: what it says and in which tone.
pub(super) type Line = Option<(String, NoticeTone)>;

/// The line at one instant: what it says now and at which opacity, the line
/// it is replacing and that line's fading opacity, and the refusal's
/// sideways offset in the prompt's units.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct AnswerPaint {
    pub shown: Line,
    pub shown_alpha: f32,
    pub replaced: Option<((String, NoticeTone), f32)>,
    pub dx: f64,
}

/// The answers one prompt has shown, timestamped as it paints.
#[derive(Default)]
pub(super) struct Answer {
    fade: Option<CrossFade<Line>>,
    attention: Attention,
    /// When the runs in flight come to rest; cleared once the resting frame
    /// has been painted, so a settled prompt asks for no frames.
    end: Option<Instant>,
    movement_allowed: bool,
}

impl Answer {
    /// Record the line at `now`. The first line a prompt shows rests; a
    /// later answer (a refusal, the asker checking) fades in, a refusal
    /// also shakes, and any other change lands at once.
    pub(super) fn observe(&mut self, line: Line, now: Instant, motion: EffectiveMotion) {
        let movement_allowed = motion.moves();
        if self.movement_allowed && !movement_allowed {
            // Reduced motion turned on mid-refusal: keep only its fade.
            self.attention = Attention::default();
            self.end = self
                .fade
                .as_ref()
                .and_then(CrossFade::settles_at)
                .filter(|end| *end > now);
        }
        self.movement_allowed = movement_allowed;
        let Some(fade) = self.fade.as_mut() else {
            self.fade = Some(CrossFade::new(line));
            return;
        };
        if *fade.shown() == line {
            return;
        }
        let tone = line.as_ref().map(|(_, tone)| *tone);
        self.attention = Attention::default();
        match tone {
            Some(NoticeTone::Error) => {
                fade.arrive(line, motion, now);
                if motion.moves() {
                    self.attention.refuse(motion, now);
                }
            }
            Some(NoticeTone::Busy) => fade.arrive(line, motion, now),
            _ => fade.show(line, motion, MotionSource::Keyboard, now),
        }
        self.end = [fade.settles_at(), self.attention.settles_at()]
            .into_iter()
            .flatten()
            .max();
    }

    /// The line to paint at `now`.
    pub(super) fn paint(&self, now: Instant) -> AnswerPaint {
        let Some(fade) = self.fade.as_ref() else {
            return AnswerPaint::default();
        };
        AnswerPaint {
            shown: fade.shown().clone(),
            shown_alpha: fade.incoming(now),
            replaced: fade
                .outgoing(now)
                .and_then(|(line, alpha)| line.clone().map(|line| (line, alpha))),
            dx: self.attention.offset(now).x,
        }
    }

    /// Drop the deadline once the resting frame is painted.
    pub(super) fn rest_if_settled(&mut self, now: Instant) {
        if self.end.is_some_and(|end| now >= end) {
            self.end = None;
        }
    }

    /// When the answer in flight comes to rest; `None` once its resting
    /// frame has been painted.
    pub(super) fn deadline(&self) -> Option<Instant> {
        self.end
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;
    use crate::time::Duration;

    fn at(base: Instant, millis: u64) -> Instant {
        base.checked_add(Duration::from_millis(millis))
            .expect("bounded test instant")
    }

    fn line(text: &str, tone: NoticeTone) -> Line {
        Some((text.into(), tone))
    }

    #[test]
    fn only_an_answer_arrives_and_the_first_line_rests() {
        let motion = Theme::dark().motion();
        let base = crate::time::now();
        let mut answer = Answer::default();

        answer.observe(line("Press Enter", NoticeTone::Quiet), base, motion);
        assert_eq!(answer.paint(base).shown_alpha, 1.0, "the first line rests");
        assert_eq!(answer.deadline(), None);

        answer.observe(line("Checking", NoticeTone::Busy), base, motion);
        let arriving = answer.paint(base);
        assert!(arriving.shown_alpha < 1.0, "an answer fades in");
        assert!(arriving.replaced.is_some(), "over the line it replaces");
        assert_eq!(arriving.dx, 0.0, "checking does not shake");
        let end = answer.deadline().expect("the fade asks for frames");
        assert!(answer.paint(end).replaced.is_none());
        assert_eq!(answer.paint(end).shown_alpha, 1.0);
        answer.rest_if_settled(end);
        assert_eq!(answer.deadline(), None, "a settled answer asks for nothing");

        answer.observe(line("Wrong", NoticeTone::Error), at(end, 10), motion);
        let shaking = (0..=40)
            .map(|step| answer.paint(at(end, 10 + step * 5)).dx)
            .any(|dx| dx.abs() > 0.5);
        assert!(shaking, "a refusal shakes");

        let later = at(end, 5_000);
        answer.observe(line("Press Enter", NoticeTone::Quiet), later, motion);
        let edit = answer.paint(later);
        assert!(
            edit.replaced.is_none() || edit.shown_alpha >= 1.0,
            "an edit lands at once"
        );
        assert_eq!(edit.dx, 0.0);
    }

    #[test]
    fn reduced_motion_keeps_the_fade_and_never_shakes() {
        let theme = Theme::dark().with_reduce_motion(true);
        let motion = theme.motion();
        let base = crate::time::now();
        let mut answer = Answer::default();
        answer.observe(line("Press Enter", NoticeTone::Quiet), base, motion);
        answer.observe(line("Wrong", NoticeTone::Error), base, motion);
        let end = answer.deadline().unwrap_or(base);
        for step in 0..=40 {
            let now = at(base, step * 5);
            if now > end {
                break;
            }
            assert_eq!(
                answer.paint(now).dx,
                0.0,
                "no movement under reduced motion"
            );
        }
    }
}
