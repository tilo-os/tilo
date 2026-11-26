//! The capture bar's own motion: its motions and the receipt's fade.

use crate::time::{Duration, Instant};

pub(super) fn studio_progress(spec: tilo_ui_base::motion::MotionSpec, elapsed: Duration) -> f64 {
    spec.progress_after(
        tilo_ui_base::motion::Direction::Enter,
        tilo_ui_base::tokens::EffectiveMotion::default(),
        tilo_ui_base::tokens::MotionSource::Pointer,
        elapsed,
    )
}

pub(super) fn studio_motion_time(spec: tilo_ui_base::motion::MotionSpec) -> Duration {
    spec.settle_time(
        tilo_ui_base::motion::Direction::Enter,
        tilo_ui_base::tokens::EffectiveMotion::default(),
        tilo_ui_base::tokens::MotionSource::Pointer,
    )
}

pub(super) fn leaving_opacity(shown: Instant, lasts: Duration) -> f32 {
    let now = crate::time::now();
    let fade_from = tilo_ui_base::motion::Transient::exit_starts(
        tilo_ui_base::motion::Transient::READOUT,
        shown + lasts,
        tilo_ui_base::tokens::EffectiveMotion::default(),
    );
    if now < fade_from {
        return 1.0;
    }
    tilo_ui_base::motion::unit_opacity(f64::from(tilo_ui_base::motion::Transient::opacity(
        tilo_ui_base::motion::Transient::READOUT,
        (shown, shown + lasts),
        now,
        tilo_ui_base::tokens::EffectiveMotion::default(),
    )))
}
