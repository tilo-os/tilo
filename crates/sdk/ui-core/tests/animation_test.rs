#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::time::{Duration, Instant};

use tilo_ui::animation::{Animated, Curve, Spring};
use tilo_ui::tokens::SpringParams;

#[test]
fn springs_and_tweens_reach_and_settle_at_their_targets() {
    // Case `spring_converges_to_target`.
    {
        let mut s = Spring::new(
            0.0,
            SpringParams {
                stiffness: 260.0,
                damping: 28.0,
                mass: 1.0,
            },
        );
        s.set_target(100.0);
        for _ in 0..600 {
            s.step(1.0 / 120.0);
        }
        assert!((s.value() - 100.0).abs() < 0.5);
        assert!(s.velocity().abs() < 0.5);
    }

    // Case `spring_settled_after_convergence`.
    {
        let mut s = Spring::new(
            0.0,
            SpringParams {
                stiffness: 260.0,
                damping: 28.0,
                mass: 1.0,
            },
        );
        s.set_target(50.0);
        for _ in 0..500 {
            s.step(1.0 / 120.0);
        }
        assert!(s.is_settled());
    }

    // Case `tween_interpolates_linearly`.
    {
        let start = Instant::now();
        let t = Animated::starting_at(
            0.0f32,
            100.0f32,
            start,
            Duration::from_secs(1),
            Curve::Linear,
        );
        assert!((t.current(start + Duration::from_millis(500)) - 50.0).abs() < 0.01);
        assert!(t.done(start + Duration::from_secs(1)));
        assert!((t.current(start + Duration::from_secs(1)) - 100.0).abs() < 0.01);
    }
}
