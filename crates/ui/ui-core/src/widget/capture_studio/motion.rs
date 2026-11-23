//! The capture bar's own motion: its motions and the receipt's fade.

use crate::time::{Duration, Instant};

pub(super) fn studio_progress(spec: crate::motion::MotionSpec, elapsed: Duration) -> f64 {
    spec.progress_after(
        crate::motion::Direction::Enter,
        crate::tokens::EffectiveMotion::default(),
        crate::tokens::MotionSource::Pointer,
        elapsed,
    )
}

pub(super) fn studio_motion_time(spec: crate::motion::MotionSpec) -> Duration {
    spec.settle_time(
        crate::motion::Direction::Enter,
        crate::tokens::EffectiveMotion::default(),
        crate::tokens::MotionSource::Pointer,
    )
}

pub(super) fn leaving_opacity(shown: Instant, lasts: Duration) -> f32 {
    let now = crate::time::now();
    let fade_from = crate::motion::Transient::exit_starts(
        crate::motion::Transient::READOUT,
        shown + lasts,
        crate::tokens::EffectiveMotion::default(),
    );
    if now < fade_from {
        return 1.0;
    }
    crate::motion::unit_opacity(f64::from(crate::motion::Transient::opacity(
        crate::motion::Transient::READOUT,
        (shown, shown + lasts),
        now,
        crate::tokens::EffectiveMotion::default(),
    )))
}
