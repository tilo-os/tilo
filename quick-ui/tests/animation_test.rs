use quick_ui::animation::{Spring, Tween, Ease};
use quick_ui::tokens::SpringParams;

#[test]
fn spring_converges_to_target() {
    let mut s = Spring::new(0.0, SpringParams { stiffness: 260.0, damping: 28.0, mass: 1.0 });
    s.set_target(100.0);
    for _ in 0..600 { s.step(1.0 / 120.0); }
    assert!((s.value() - 100.0).abs() < 0.5);
    assert!(s.velocity().abs() < 0.5);
}

#[test]
fn spring_settled_after_convergence() {
    let mut s = Spring::new(0.0, SpringParams { stiffness: 260.0, damping: 28.0, mass: 1.0 });
    s.set_target(50.0);
    for _ in 0..500 { s.step(1.0 / 120.0); }
    assert!(s.is_settled());
}

#[test]
fn tween_interpolates_linearly() {
    let mut t = Tween::new(0.0, 100.0, 1.0, Ease::Linear);
    t.advance(0.5);
    assert!((t.value() - 50.0).abs() < 0.01);
    t.advance(0.5);
    assert!(t.done());
    assert!((t.value() - 100.0).abs() < 0.01);
}
