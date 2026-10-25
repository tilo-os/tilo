//! How the lock answers an attempt: what the status row says, and how a change
//! of answer arrives.
//!
//! The controller owns whether a credential was accepted, refused or is still
//! being checked; this module owns the presentation of that fact. It composes
//! the named motions the toolkit already has — a cross-fade for the answer
//! replacing the last one, the refusal's shake from `Attention` — rather than
//! timing anything here. Both runs are `fast` and finite: the lock is mostly
//! unattended, so nothing loops or keeps animating after an answer.
//!
//! The painter receives a plain snapshot ([`LockStatusPaint`]), so scene
//! painting stays pure and a still (a capture, a review fixture) draws the
//! resting pose. Only [`LockFeedback::observe`] keeps timing state, and the
//! retained lock — the one live producer — holds it.

use std::time::Instant;

use ui::motion::{Attention, CrossFade};
use ui::theme::Theme;
use ui::tokens::{EffectiveMotion, MotionSource};
use vello::peniko::Color;

use super::{LockView, lock_error_status};
use crate::shell::i18n::t;

/// What the status row is answering. The tone decides both the row's ink and
/// the credential field's ring, so a refusal and its field read as one answer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum LockStatus {
    /// Nothing is being checked: the resting hint, or a quiet session.
    #[default]
    Idle,
    /// The password is being verified; the field is frozen.
    Verifying,
    /// The attempt was refused, or the verifier could not answer.
    Failed,
}

impl LockStatus {
    /// The tone of `view`. A verification in flight outranks an older error
    /// (the controller clears the error when it submits, so the two never
    /// normally coexist).
    pub(crate) fn of(view: &LockView<'_>) -> Self {
        if view.verifying {
            Self::Verifying
        } else if view.error.is_some() {
            Self::Failed
        } else {
            Self::Idle
        }
    }

    /// Whether a change into this tone is quiet. Typing after a refusal
    /// restores the hint: that is an edit, not an answer, so it lands at once.
    pub(crate) const fn is_quiet(self) -> bool {
        matches!(self, Self::Idle)
    }

    /// The row's line, in the person's words.
    pub(crate) fn copy(self, view: &LockView<'_>) -> String {
        match self {
            Self::Idle => t("lock-screen-press-enter"),
            Self::Verifying => t("lock-screen-verifying"),
            Self::Failed => lock_error_status(view.error.unwrap_or_default()),
        }
    }

    /// The row's ink: a hint is quiet, a check in flight is informational, and
    /// a refusal is the danger role.
    pub(crate) fn ink(self, theme: &Theme) -> Color {
        match self {
            Self::Idle => theme.text_dim,
            Self::Verifying => theme.tokens.colors.semantic.info,
            Self::Failed => theme.tokens.colors.semantic.danger,
        }
    }

    /// The credential field's ring. The field keeps the person's focus colour
    /// while it can take input, takes the informational role while a check
    /// runs, and the danger role while it hands the person back a retry.
    pub(crate) fn field_ring(self, theme: &Theme) -> Color {
        match self {
            Self::Idle => theme.border_focus,
            Self::Verifying => theme.tokens.colors.semantic.info,
            Self::Failed => theme.tokens.colors.semantic.danger,
        }
    }
}

/// The status row at one instant: the line shown, the line it is replacing
/// while that fades out, and the refusal's sideways nudge.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct LockStatusPaint {
    /// The tone the row answers with now.
    pub tone: LockStatus,
    /// The line the row answers with now.
    pub shown: String,
    /// Its opacity: 0 as a fade begins, 1 at rest.
    pub shown_alpha: f32,
    /// The tone and line it replaces, and that line's fading opacity.
    pub replaced: Option<(LockStatus, String, f32)>,
    /// The refusal's horizontal offset, in the lock chrome's pixels. Zero at
    /// rest and under reduced motion.
    pub dx: f64,
}

impl LockStatusPaint {
    /// The row at rest: what a still, a capture or an untouched lock shows.
    pub(crate) fn settled(view: &LockView<'_>) -> Self {
        let tone = LockStatus::of(view);
        Self {
            tone,
            shown: tone.copy(view),
            shown_alpha: 1.0,
            replaced: None,
            dx: 0.0,
        }
    }
}

/// The status transitions of one lock session, timestamped as the live
/// producer paints. Both lock edges call [`LockFeedback::reset`], so a fresh
/// lock never animates the state it engaged in.
#[derive(Default)]
pub(crate) struct LockFeedback {
    status: LockStatus,
    previous: Option<LockStatus>,
    fades: Option<CrossFade<String>>,
    attention: Attention,
    /// When the runs in flight come to rest; cleared once the resting frame
    /// has been painted, so nothing keeps asking for frames.
    end: Option<Instant>,
    seen: bool,
    movement_allowed: bool,
}

impl LockFeedback {
    /// Forget the session's transitions: the next lock edge starts at rest.
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }

    /// Record what `view` answers at `now`. The first observation of a session
    /// is its resting state; a later answer cross-fades in (a refusal also
    /// shakes), and a quiet change (typing) lands at once.
    pub(crate) fn observe(&mut self, view: &LockView<'_>, now: Instant, motion: EffectiveMotion) {
        let movement_allowed = motion.moves();
        if self.movement_allowed && !movement_allowed {
            // A live accessibility preference change takes effect even when
            // this is still the same refusal. Keep only its permitted fade.
            self.attention = Attention::default();
            self.end = self
                .fades
                .as_ref()
                .and_then(CrossFade::settles_at)
                .filter(|end| *end > now);
        }
        self.movement_allowed = movement_allowed;
        let status = LockStatus::of(view);
        let copy = status.copy(view);
        let Some(fades) = self.fades.as_mut() else {
            self.seen = true;
            self.status = status;
            self.fades = Some(CrossFade::new(copy));
            return;
        };
        if self.seen && self.status == status && *fades.shown() == copy {
            return;
        }
        self.seen = true;
        self.previous = Some(self.status);
        self.status = status;
        if status.is_quiet() {
            fades.show(copy, motion, MotionSource::Keyboard, now);
            self.attention = Attention::default();
        } else {
            fades.arrive(copy, motion, now);
            self.attention = Attention::default();
            if status == LockStatus::Failed && motion.moves() {
                // The library's refusal: a sideways shake, never a loop. Under
                // reduced motion the cross-fade and the danger role carry the
                // answer with no movement at all.
                self.attention.refuse(motion, now);
            }
        }
        self.end = [fades.settles_at(), self.attention.settles_at()]
            .into_iter()
            .flatten()
            .max();
    }

    /// The row to paint at `now`.
    pub(crate) fn paint(&self, now: Instant) -> LockStatusPaint {
        let Some(fades) = self.fades.as_ref() else {
            return LockStatusPaint::default();
        };
        LockStatusPaint {
            tone: self.status,
            shown: fades.shown().clone(),
            shown_alpha: fades.incoming(now),
            replaced: fades
                .outgoing(now)
                .map(|(text, alpha)| (self.previous.unwrap_or(self.status), text.clone(), alpha)),
            dx: self.attention.offset(now).x,
        }
    }

    /// Drop the deadline once its resting frame is painted, so a settled lock
    /// never asks for another frame.
    pub(crate) fn rest_if_settled(&mut self, now: Instant) {
        if self.end.is_some_and(|end| now >= end) {
            self.end = None;
        }
    }

    /// Whether a run still needs frames at `now`.
    pub(crate) fn moving(&self, now: Instant) -> bool {
        self.end.is_some_and(|end| now < end)
    }

    /// When the run in flight comes to rest.
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.end
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use ui::theme::Theme;

    fn at(base: Instant, millis: u64) -> Instant {
        base.checked_add(Duration::from_millis(millis))
            .expect("bounded test instant")
    }

    fn view(status: LockStatus) -> LockView<'static> {
        let error = match status {
            LockStatus::Idle | LockStatus::Verifying => None,
            LockStatus::Failed => Some("Incorrect username or password"),
        };
        LockView {
            clock: "13:37",
            date: "Friday, 11 July",
            verifying: status == LockStatus::Verifying,
            error,
            ..LockView::default()
        }
    }

    #[test]
    fn only_an_answer_arrives_and_the_first_locked_frame_rests() {
        let theme = Theme::dark();
        let motion = theme.motion();
        let base = Instant::now();
        let mut feedback = LockFeedback::default();

        // The state the session locked into never animates.
        feedback.observe(&view(LockStatus::Idle), base, motion);
        let rest = feedback.paint(base);
        assert_eq!(rest.shown, "Press Enter to unlock");
        assert!((rest.shown_alpha - 1.0).abs() < 1e-6);
        assert_eq!(rest.replaced, None);
        assert!(rest.dx.abs() < 1e-6);
        assert!(!feedback.moving(base));
        assert_eq!(feedback.next_deadline(), None);

        // Beginning a verification is an answer: it cross-fades in over `fast`.
        let start = at(base, 10);
        feedback.observe(&view(LockStatus::Verifying), start, motion);
        assert!(feedback.moving(start));
        let arriving = feedback.paint(start);
        assert_eq!(arriving.shown, "Verifying…");
        assert!(
            arriving.shown_alpha.abs() < 1e-6,
            "it fades in from nothing"
        );
        assert_eq!(
            arriving.replaced.map(|(_, line, _)| line).as_deref(),
            Some("Press Enter to unlock")
        );
        let deadline = feedback.next_deadline().expect("the run has an end");
        assert!(deadline > start);
        let settled = feedback.paint(deadline);
        assert!((settled.shown_alpha - 1.0).abs() < 1e-6);
        assert_eq!(settled.replaced, None);
        assert!(settled.dx.abs() < 1e-6);
        assert!(!feedback.moving(deadline));
        feedback.rest_if_settled(deadline);
        assert_eq!(feedback.next_deadline(), None);

        // Typing after a refusal is an edit: the hint lands at once.
        feedback.observe(&view(LockStatus::Failed), at(base, 20), motion);
        assert!(feedback.moving(at(base, 20)));
        feedback.observe(&view(LockStatus::Idle), at(base, 30), motion);
        assert!(!feedback.moving(at(base, 30)));
        assert_eq!(feedback.next_deadline(), None);
        let typed = feedback.paint(at(base, 30));
        assert_eq!(typed.shown, "Press Enter to unlock");
        assert!((typed.shown_alpha - 1.0).abs() < 1e-6);
        assert_eq!(typed.replaced, None);
    }

    #[test]
    fn a_refusal_shakes_once_and_a_retry_starts_the_fade_again() {
        let theme = Theme::dark();
        let motion = theme.motion();
        let base = Instant::now();
        let mut feedback = LockFeedback::default();
        feedback.observe(&view(LockStatus::Idle), base, motion);

        feedback.observe(&view(LockStatus::Failed), at(base, 10), motion);
        let refused = feedback.paint(at(base, 40));
        assert!(
            refused.dx.abs() > 0.5,
            "the refusal moves sideways: {}",
            refused.dx
        );
        assert!(feedback.moving(at(base, 40)));
        assert!(refused.shown_alpha > 0.0 && refused.shown_alpha < 1.0);
        feedback.rest_if_settled(at(base, 2000));
        assert!(feedback.paint(at(base, 2000)).dx.abs() < 1e-6);

        // Retrying cross-fades the refusal out instead of inheriting a phase.
        feedback.observe(&view(LockStatus::Verifying), at(base, 2000), motion);
        let retry = feedback.paint(at(base, 2000));
        assert_eq!(retry.shown, "Verifying…");
        assert!(retry.shown_alpha.abs() < 1e-6);
        assert!(retry.dx.abs() < 1e-6, "a retry does not inherit the shake");
        assert_eq!(
            retry.replaced.map(|(_, line, _)| line).as_deref(),
            Some("Sign-in failed. Enter your password again.")
        );
    }

    #[test]
    fn reduced_motion_keeps_the_answer_without_moving_it() {
        let theme = Theme::dark().with_reduce_motion(true);
        let motion = theme.motion();
        let base = Instant::now();
        let mut feedback = LockFeedback::default();
        feedback.observe(&view(LockStatus::Idle), base, motion);
        feedback.observe(&view(LockStatus::Failed), at(base, 10), motion);
        // The answer still fades in, and its field still takes the danger
        // role, but nothing moves.
        let mid = feedback.paint(at(base, 40));
        assert!(mid.shown_alpha > 0.0 && mid.shown_alpha < 1.0);
        assert!(mid.dx.abs() < 1e-6);
        assert!(feedback.moving(at(base, 40)));
        let end = feedback.next_deadline().expect("a short fade still ticks");
        assert!(
            end <= at(base, 10) + motion.tokens().durations.fast() + Duration::from_millis(20),
            "reduced motion still runs one short fade"
        );
        assert!(feedback.paint(end).dx.abs() < 1e-6);
    }

    #[test]
    fn enabling_reduced_motion_stops_an_inflight_refusal_immediately() {
        let base = Instant::now();
        let mut feedback = LockFeedback::default();
        let normal = Theme::dark().motion();
        feedback.observe(&view(LockStatus::Idle), base, normal);
        feedback.observe(&view(LockStatus::Failed), at(base, 10), normal);
        assert!(feedback.paint(at(base, 40)).dx.abs() > 0.5);
        feedback.observe(
            &view(LockStatus::Failed),
            at(base, 40),
            Theme::dark().with_reduce_motion(true).motion(),
        );
        let reduced = feedback.paint(at(base, 40));
        assert!(reduced.dx.abs() < 1e-6);
        assert!(reduced.shown_alpha > 0.0 && reduced.shown_alpha < 1.0);
        let end = feedback.next_deadline().unwrap();
        feedback.rest_if_settled(end);
        feedback.observe(
            &view(LockStatus::Failed),
            at(base, 2000),
            Theme::dark().with_reduce_motion(true).motion(),
        );
        assert_eq!(feedback.next_deadline(), None);
        assert!(feedback.paint(at(base, 2000)).dx.abs() < 1e-6);
    }

    #[test]
    fn the_tone_names_the_row_and_the_field_for_every_state() {
        let theme = Theme::dark();
        for (status, row, ink, ring) in [
            (
                LockStatus::Idle,
                "Press Enter to unlock",
                theme.text_dim,
                theme.border_focus,
            ),
            (
                LockStatus::Verifying,
                "Verifying…",
                theme.tokens.colors.semantic.info,
                theme.tokens.colors.semantic.info,
            ),
            (
                LockStatus::Failed,
                "Sign-in failed. Enter your password again.",
                theme.tokens.colors.semantic.danger,
                theme.tokens.colors.semantic.danger,
            ),
        ] {
            let view = view(status);
            assert_eq!(LockStatus::of(&view), status);
            assert_eq!(status.copy(&view), row);
            assert_eq!(status.ink(&theme), ink);
            assert_eq!(status.field_ring(&theme), ring);
        }
        // An unavailable verifier is a refusal with its own next step.
        let unavailable = LockView {
            error: Some(crate::compositor::lock_auth::UNAVAILABLE_MSG),
            ..LockView::default()
        };
        assert_eq!(
            LockStatus::Failed.copy(&unavailable),
            "Can't verify your password. Enter it again."
        );
    }
}
